use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::OwnedSemaphorePermit;

use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::finish::{finish, make_durable, moved};
use crate::jobs::plan::{self, Next};
use crate::jobs::settle::{Settled, settle};
use crate::jobs::store::{Phase, Plan, StoredSpec};
use crate::jobs::{JobFailure, JobState, Shared, probe, raw};
use crate::repair::recovery::Par2s;
use crate::unpack::Unpack;
use crate::vendor::queue::nzb::{self, Nzb};
use crate::writer::{Tally, Target, WriteError, Writer};

const NESTED: u32 = 3;

pub enum Ended {
    Finished,
    Retry,
}

pub struct Job<'a> {
    pub shared: &'a Shared,
    pub spec: &'a StoredSpec,
    pub fetcher: &'a Fetcher,
    pub nzb: &'a Nzb,
    pub par2: &'a Par2s,
    pub staging: &'a Path,
    pub slot: &'a Arc<OwnedSemaphorePermit>,
}

pub async fn run(
    shared: &Shared,
    spec: &StoredSpec,
    last_try: bool,
    slot: &Arc<OwnedSemaphorePermit>,
) -> Ended {
    let outcome = execute(shared, spec, slot).await;
    let ended = match &outcome {
        Err(Failure::Infrastructure(_)) if !last_try => Ended::Retry,
        _ => Ended::Finished,
    };
    shared.set_state(&spec.id, |status| match outcome {
        Ok(path) => {
            status.state = JobState::Completed;
            status.path = path;
            status.error = None;
            status.failure = None;
        }
        Err(Failure::Infrastructure(reason)) if !last_try => {
            tracing::info!(job = %spec.id, %reason, "download waits to retry");
            status.state = JobState::Queued;
            status.error = Some(reason);
            status.failure = None;
        }
        Err(Failure::Infrastructure(reason)) => {
            tracing::warn!(job = %spec.id, %reason, "download gave up retrying");
            status.state = JobState::Failed;
            status.error = Some(reason);
            status.failure = Some(JobFailure::Infrastructure);
        }
        Err(Failure::Content(reason)) => {
            tracing::warn!(job = %spec.id, %reason, "download failed");
            status.state = JobState::Failed;
            status.error = Some(reason);
            status.failure = Some(JobFailure::Content);
        }
    });
    ended
}

async fn execute(
    shared: &Shared,
    spec: &StoredSpec,
    slot: &Arc<OwnedSemaphorePermit>,
) -> Result<PathBuf, Failure> {
    let staging = spec.staging();
    let phase = stored(shared.store.phase(&spec.id))?;
    if let Phase::Moving(target) = &phase {
        return moved(shared, spec, &staging, target).await;
    }
    let mut nested = 0;
    let (nzb, mut plan, par2) = loop {
        let nzb = stored_nzb(shared, spec)?;
        let fetcher = fetcher(shared, &nzb);
        let par2 = if phase == Phase::Downloading {
            shared.set_state(&spec.id, |s| s.state = JobState::Downloading);
            Par2s::discover(&fetcher, &nzb)
                .await
                .map_err(|e| Failure::providers(&e))?
        } else {
            Par2s::default()
        };
        let job = Job {
            shared,
            spec,
            fetcher: &fetcher,
            nzb: &nzb,
            par2: &par2,
            staging: &staging,
            slot,
        };
        let plan = match stored(shared.store.plan(&spec.id))? {
            Some(plan) => plan,
            None => match plan::direct(&fetcher, &nzb, &par2, &spec.name, &passwords(spec, &nzb))
                .await
            {
                Next::Plan(plan) => save(shared, spec, plan)?,
                Next::Raw => go_raw(&job).await?,
                Next::Nested(_) if nested >= NESTED => {
                    return Err(Failure::Content(
                        "The download holds NZBs nested too deep.".to_owned(),
                    ));
                }
                Next::Nested(inner) => {
                    nested += 1;
                    shared
                        .store
                        .put_nzb(&spec.id, &inner)
                        .map_err(|e| Failure::io("saving the inner NZB", e))?;
                    continue;
                }
                Next::Fail(failure) => return Err(failure),
            },
        };
        break (nzb, plan, par2);
    };
    let passwords = passwords(spec, &nzb);
    let fetcher = fetcher(shared, &nzb);
    let job = Job {
        shared,
        spec,
        fetcher: &fetcher,
        nzb: &nzb,
        par2: &par2,
        staging: &staging,
        slot,
    };
    let extracted = match phase {
        Phase::Downloading => {
            download(&job, &mut plan).await?;
            Vec::new()
        }
        Phase::Unpacking { extracted } => extracted,
        Phase::Downloaded | Phase::Moving(_) => Vec::new(),
    };
    shared.set_state(&spec.id, |s| s.state = JobState::Unpacking);
    let unpack = Unpack {
        unrar: &shared.config.unrar,
        sevenzip: &shared.config.sevenzip,
        passwords: &passwords,
        space: &shared.gate,
        stall: shared.config.unpack_stall,
        disk: &shared.config.disk,
    };
    let record = |extracted: &[String]| {
        stored(shared.store.put_phase(
            &spec.id,
            &Phase::Unpacking {
                extracted: extracted.to_vec(),
            },
        ))
    };
    record(&extracted)?;
    unpack.run(&staging, extracted, &record).await?;
    finish(shared, spec, &staging).await
}

async fn download(job: &Job<'_>, plan: &mut Plan) -> Result<(), Failure> {
    let (shared, spec) = (job.shared, job.spec);
    loop {
        if !plan.probed {
            let missed = probe::hopeless(job.fetcher, job.nzb, job.par2, plan).await?;
            plan.probed = true;
            if missed.members && !job.par2.is_empty() {
                tracing::info!(job = %spec.id, "archive members miss articles; downloading whole volumes to repair");
                *plan = go_raw(job).await?;
                continue;
            }
            *plan = save(shared, spec, plan.clone())?;
        }
        shared.set_state(&spec.id, |s| s.state = JobState::Downloading);
        let damage = write(job, plan).await?;
        shared.set_state(&spec.id, |s| s.state = JobState::Verifying);
        match settle(job, plan, damage).await? {
            Settled::Clean => break,
            Settled::Raw => *plan = go_raw(job).await?,
        }
    }
    std::fs::create_dir_all(job.staging)
        .map_err(|e| Failure::io(&format!("creating {}", job.staging.display()), e))?;
    make_durable(shared, job.staging).await?;
    stored(shared.store.put_phase(&spec.id, &Phase::Downloaded))
}

fn stored_nzb(shared: &Shared, spec: &StoredSpec) -> Result<Nzb, Failure> {
    let raw = shared
        .store
        .nzb(&spec.id)
        .map_err(|e| Failure::io("reading the stored NZB", e))?;
    let invalid = |e: String| Failure::Content(format!("invalid NZB: {e}"));
    let xml = nzb::decompress(&raw).map_err(|e| invalid(e.to_string()))?;
    nzb::parse(&xml).map_err(|e| invalid(e.to_string()))
}

fn passwords(spec: &StoredSpec, nzb: &Nzb) -> Vec<String> {
    let mut all: Vec<String> = nzb.meta("password").map(str::to_owned).collect();
    all.extend(spec.password.clone());
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert(p.clone()));
    all
}

fn fetcher(shared: &Shared, nzb: &Nzb) -> Fetcher {
    Fetcher::new(
        &shared.client,
        nzb.files.iter().filter_map(|f| f.date).max(),
    )
}

async fn go_raw(job: &Job<'_>) -> Result<Plan, Failure> {
    let plan = raw::raw(job.fetcher, job.nzb, job.par2).await?;
    job.shared
        .store
        .clear_maps(&job.spec.id)
        .map_err(|e| Failure::io("resetting the job", e))?;
    match std::fs::remove_dir_all(job.staging) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(Failure::io(
                &format!("resetting {}", job.staging.display()),
                e,
            ));
        }
        _ => {}
    }
    save(job.shared, job.spec, plan)
}

fn save(shared: &Shared, spec: &StoredSpec, plan: Plan) -> Result<Plan, Failure> {
    stored(shared.store.put_plan(&spec.id, &plan))?;
    Ok(plan)
}

pub fn stored<T>(result: Result<T, crate::Error>) -> Result<T, Failure> {
    result.map_err(|e| Failure::io("job state", e))
}

async fn write(job: &Job<'_>, plan: &Plan) -> Result<Vec<Vec<Range<u64>>>, Failure> {
    let shared = job.shared;
    let targets = plan
        .outputs
        .iter()
        .enumerate()
        .map(|(i, o)| {
            Ok(Target {
                file: o.under(job.staging)?,
                meta: &o.meta,
                map: shared.store.map(&job.spec.id, i),
            })
        })
        .collect::<Result<Vec<Target<'_>>, Failure>>()?;
    let connections = shared.client.max_connections().max(1);
    let writer = Writer {
        fetcher: job.fetcher,
        gate: &shared.gate,
        disk: &shared.config.disk,
        window: connections * shared.pipeline_depth.max(1),
        tail: connections,
        straggler_after: shared.config.straggler_after,
        numbered: plan.raw,
    };
    let progress = |tally: Tally| {
        shared.update(&job.spec.id, |s| {
            s.bytes_total = tally.bytes_total;
            s.bytes_done = tally.bytes_done;
            s.segments_total = tally.segments_total;
            s.segments_done = tally.segments_done;
        });
    };
    writer
        .write(&targets, &progress)
        .await
        .map_err(|e| match e {
            WriteError::Providers(error) => Failure::providers(&error),
            WriteError::Io(error) => Failure::io("writing to disk", error),
        })
}
