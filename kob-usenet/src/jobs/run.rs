use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::plan::{self, Next, has_par2};
use crate::jobs::store::{Plan, StoredSpec};
use crate::jobs::{JobFailure, JobState, Shared, probe, raw};
use crate::repair::{self, OnDisk, RepairError, proof_damage};
use crate::unpack::{Tools, unpack};
use crate::vendor::core::FileMeta;
use crate::vendor::queue::nzb::{self, Nzb};
use crate::vendor::yenc::crc32;
use crate::writer::{Tally, Target, WriteError, Writer};

const NESTED: u32 = 3;

enum Settled {
    Clean,
    Raw,
}

pub enum Ended {
    Finished,
    Retry,
}

pub async fn run(shared: &Shared, spec: &StoredSpec, last_try: bool) -> Ended {
    let outcome = execute(shared, spec).await;
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

async fn execute(shared: &Shared, spec: &StoredSpec) -> Result<PathBuf, Failure> {
    let mut nested = 0;
    let (nzb, mut plan) = loop {
        let nzb = stored_nzb(shared, spec)?;
        let fetcher = fetcher(shared, &nzb);
        shared.set_state(&spec.id, |s| s.state = JobState::Downloading);
        let plan = match stored(shared.store.plan(&spec.id))? {
            Some(plan) => plan,
            None => match plan::direct(&fetcher, &nzb, &spec.name, password(spec, &nzb).as_deref())
                .await
            {
                Next::Plan(plan) => save(shared, spec, plan)?,
                Next::Raw => go_raw(shared, spec, &fetcher, &nzb, &spec.staging()).await?,
                Next::Nested(_) if nested >= NESTED => {
                    return Err("The download holds NZBs nested too deep.".to_owned().into());
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
        break (nzb, plan);
    };
    let password = password(spec, &nzb);
    let fetcher = fetcher(shared, &nzb);
    let staging = spec.staging();
    loop {
        if !plan.probed {
            probe::hopeless(&fetcher, &nzb, &plan).await?;
            plan.probed = true;
            plan = save(shared, spec, plan)?;
        }
        shared.set_state(&spec.id, |s| s.state = JobState::Downloading);
        let damage = write(shared, spec, &fetcher, &plan, &staging).await?;
        shared.set_state(&spec.id, |s| s.state = JobState::Verifying);
        match settle(shared, spec, &fetcher, &nzb, &plan, damage, &staging).await? {
            Settled::Clean => break,
            Settled::Raw => plan = go_raw(shared, spec, &fetcher, &nzb, &staging).await?,
        }
    }
    if plan.raw {
        shared.set_state(&spec.id, |s| s.state = JobState::Unpacking);
        let tools = Tools {
            unrar: &shared.config.unrar,
            sevenzip: &shared.config.sevenzip,
        };
        unpack(&tools, &staging, password.as_deref()).await?;
    }
    finish(spec, &staging)
}

fn stored_nzb(shared: &Shared, spec: &StoredSpec) -> Result<Nzb, Failure> {
    let raw = shared
        .store
        .nzb(&spec.id)
        .map_err(|e| Failure::io("reading the stored NZB", e))?;
    let xml = nzb::decompress(&raw).map_err(|e| format!("invalid NZB: {e}"))?;
    Ok(nzb::parse(&xml).map_err(|e| format!("invalid NZB: {e}"))?)
}

fn password(spec: &StoredSpec, nzb: &Nzb) -> Option<String> {
    nzb.meta("password")
        .next()
        .map(str::to_owned)
        .or_else(|| spec.password.clone())
}

fn fetcher(shared: &Shared, nzb: &Nzb) -> Fetcher {
    Fetcher::new(
        &shared.client,
        nzb.files.iter().filter_map(|f| f.date).max(),
    )
}

async fn go_raw(
    shared: &Shared,
    spec: &StoredSpec,
    fetcher: &Fetcher,
    nzb: &Nzb,
    staging: &Path,
) -> Result<Plan, Failure> {
    let plan = raw::raw(fetcher, nzb).await?;
    shared
        .store
        .clear_maps(&spec.id)
        .map_err(|e| Failure::io("resetting the job", e))?;
    match std::fs::remove_dir_all(staging) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("resetting {}: {e}", staging.display()).into());
        }
        _ => {}
    }
    save(shared, spec, plan)
}

fn save(shared: &Shared, spec: &StoredSpec, plan: Plan) -> Result<Plan, Failure> {
    stored(shared.store.put_plan(&spec.id, &plan))?;
    Ok(plan)
}

fn stored<T>(result: Result<T, crate::Error>) -> Result<T, Failure> {
    result.map_err(|e| Failure::io("job state", e))
}

async fn write(
    shared: &Shared,
    spec: &StoredSpec,
    fetcher: &Fetcher,
    plan: &Plan,
    staging: &Path,
) -> Result<Vec<Vec<Range<u64>>>, Failure> {
    let targets: Vec<Target<'_>> = plan
        .outputs
        .iter()
        .enumerate()
        .map(|(i, o)| Target {
            file: staging.join(&o.path),
            meta: &o.meta,
            map: shared.store.map(&spec.id, i),
        })
        .collect();
    let connections = shared.client.max_connections().max(1);
    let writer = Writer {
        fetcher,
        gate: &shared.gate,
        window: connections * shared.pipeline_depth.max(1),
        tail: connections,
        straggler_after: shared.config.straggler_after,
    };
    let progress = |tally: Tally| {
        shared.update(&spec.id, |s| {
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

enum Check {
    Clean,
    Member(String),
    Unprotected(String),
    Repairable,
}

async fn settle(
    shared: &Shared,
    spec: &StoredSpec,
    fetcher: &Fetcher,
    nzb: &Nzb,
    plan: &Plan,
    damage: Vec<Vec<Range<u64>>>,
    staging: &Path,
) -> Result<Settled, Failure> {
    let checks = {
        let (plan, staging) = (plan.clone(), staging.to_path_buf());
        tokio::task::spawn_blocking(move || check(&plan, &damage, &staging))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| Failure::io("verifying", e))?
    };
    let fallback = !plan.raw && has_par2(nzb);
    for check in &checks {
        match check {
            Check::Member(reason) | Check::Unprotected(reason) if fallback => {
                tracing::info!(job = %spec.id, %reason, "downloading whole volumes to repair");
                return Ok(Settled::Raw);
            }
            Check::Member(reason) | Check::Unprotected(reason) => {
                return Err(format!("{reason}, and no PAR2 recovery can repair it").into());
            }
            Check::Clean | Check::Repairable => {}
        }
    }
    if !checks.iter().any(|c| matches!(c, Check::Repairable)) {
        return Ok(Settled::Clean);
    }
    shared.set_state(&spec.id, |s| s.state = JobState::Repairing);
    let files: Vec<OnDisk> = plan
        .outputs
        .iter()
        .filter_map(|o| match &o.meta {
            FileMeta::Nzb(f) => Some(OnDisk {
                id: f.proof.as_ref().map(|p| p.file_id),
                name: o.path.rsplit('/').next().unwrap_or(&o.path).to_owned(),
                path: staging.join(&o.path),
            }),
            FileMeta::Multipart(_) => None,
        })
        .collect();
    let repaired = repair::repair(
        fetcher,
        nzb,
        &files,
        staging,
        plan.raw,
        shared.config.repair_memory_bytes,
    )
    .await;
    match repaired {
        Ok(slices) => {
            tracing::info!(job = %spec.id, slices, "repaired with PAR2");
            Ok(Settled::Clean)
        }
        Err(RepairError::Uncovered(reason)) if fallback => {
            tracing::info!(job = %spec.id, %reason, "downloading whole volumes to repair");
            Ok(Settled::Raw)
        }
        Err(RepairError::Providers(error)) => Err(Failure::providers(&error)),
        Err(error) => Err(error.to_string().into()),
    }
}

fn check(plan: &Plan, damage: &[Vec<Range<u64>>], staging: &Path) -> std::io::Result<Vec<Check>> {
    plan.outputs
        .iter()
        .zip(damage)
        .map(|(output, damaged)| {
            let path = staging.join(&output.path);
            let proof = match &output.meta {
                FileMeta::Nzb(f) => f.proof.as_ref(),
                FileMeta::Multipart(_) => None,
            };
            if output.member {
                if !damaged.is_empty() {
                    return Ok(Check::Member(format!(
                        "{}: {} byte ranges are missing",
                        output.path,
                        damaged.len()
                    )));
                }
                if let Some(expected) = output.crc32
                    && crc_of(&path)? != expected
                {
                    return Ok(Check::Member(format!(
                        "{}: CRC mismatch against the archive header",
                        output.path
                    )));
                }
                return Ok(Check::Clean);
            }
            if let Some(proof) = proof {
                return Ok(if proof_damage(&path, proof)?.is_empty() {
                    Check::Clean
                } else {
                    Check::Repairable
                });
            }
            Ok(if damaged.is_empty() {
                Check::Clean
            } else {
                Check::Unprotected(format!(
                    "{}: {} byte ranges are missing",
                    output.path,
                    damaged.len()
                ))
            })
        })
        .collect()
}

fn crc_of(path: &Path) -> std::io::Result<u32> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0; 1 << 20];
    let mut crc = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(crc);
        }
        crc = crc32(crc, &buf[..n]);
    }
}

fn finish(spec: &StoredSpec, staging: &Path) -> Result<PathBuf, Failure> {
    std::fs::create_dir_all(staging)
        .map_err(|e| Failure::io(&format!("creating {}", staging.display()), e))?;
    let mut target = spec.dest_dir.join(&spec.folder);
    let mut n = 1;
    while target.exists() {
        target = spec.dest_dir.join(format!("{}.{n}", spec.folder));
        n += 1;
    }
    std::fs::rename(staging, &target)
        .map_err(|e| Failure::io(&format!("moving to {}", target.display()), e))?;
    Ok(target)
}
