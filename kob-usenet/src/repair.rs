mod check;
mod control;
mod field;
pub(crate) mod recovery;
mod sets;
mod solve;
mod spool;

use std::collections::HashSet;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::jobs::fetch::Fetcher;
use crate::vendor::nntp;
use crate::vendor::par2::{self, Id};
use crate::vendor::queue::nzb::Nzb;
pub use check::{damaged_slices, proof_damage};
pub use control::Control;
use control::Ticket;
use recovery::{Collected, Par2s};
pub use sets::described;
use sets::{covered, covering, position, sources, usable};
use solve::solve;
pub use spool::Spool;

const DEPENDENT_ROWS: usize = 3;

pub struct OnDisk {
    pub id: Option<Id>,
    pub hash: Option<Id>,
    pub name: String,
    pub path: PathBuf,
    pub damaged: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("no PAR2 recovery set covers the damaged files")]
    NoPar2,
    #[error("the PAR2 set protects files that were not downloaded whole: {0}")]
    Uncovered(String),
    #[error("{need} PAR2 blocks are missing but only {have} recovery blocks could be fetched")]
    Insufficient { need: usize, have: usize },
    #[error("the fetched recovery blocks are not independent enough to repair")]
    Dependent,
    #[error("PAR2 repair failed: {0}")]
    Failed(String),
    #[error(
        "repairing {need} slices needs {required} bytes of memory, over the {limit} byte budget"
    )]
    Memory {
        need: usize,
        required: usize,
        limit: usize,
    },
    #[error("the repair was cancelled")]
    Cancelled,
    #[error("PAR2 repair: {0}")]
    Io(#[from] io::Error),
    #[error("providers unavailable: {0}")]
    Providers(#[from] nntp::Error),
}

impl From<par2::Error> for RepairError {
    fn from(error: par2::Error) -> Self {
        match error {
            par2::Error::InsufficientRecovery => Self::Dependent,
            other => Self::Failed(other.to_string()),
        }
    }
}

#[derive(Clone)]
struct Source {
    file: par2::File,
    slices: Range<usize>,
    path: PathBuf,
    present: bool,
    trusted: bool,
}

#[derive(Debug, Default)]
pub struct Repaired {
    pub slices: usize,
    pub by_other_set: HashSet<PathBuf>,
}

enum SetOutcome {
    Agrees,
    Solved(usize),
}

#[derive(Clone, Copy)]
pub struct Budget {
    pub memory: usize,
    pub absent_ok: bool,
}

pub async fn repair(
    fetcher: &Fetcher,
    nzb: &Nzb,
    par2: &Par2s,
    files: &[OnDisk],
    staging: &Path,
    budget: Budget,
    control: &Control,
) -> Result<Repaired, RepairError> {
    let mut collected = par2
        .index(fetcher, nzb, Spool::on_disk(staging)?, |reader| {
            covered(reader, files)
        })
        .await?
        .ok_or(RepairError::NoPar2)?;
    let order = covering(collected.spool().critical(), files);
    if order.is_empty() {
        return Err(RepairError::NoPar2);
    }
    let mut open: HashSet<usize> = (0..files.len()).filter(|&i| files[i].damaged).collect();
    let (mut agreed, mut failed) = (HashSet::new(), HashSet::new());
    let mut first_error = None;
    let mut repaired = Repaired::default();
    for set in order {
        let Some(plan) = usable(collected.spool().critical(), set) else {
            continue;
        };
        let targets: Vec<(usize, Id)> = plan
            .sources()
            .iter()
            .filter_map(|s| {
                let at = position(&s.file, files)?;
                open.contains(&at).then_some((at, s.file.hash))
            })
            .collect();
        if targets.is_empty() {
            continue;
        }
        let slice_size = plan.slice_size();
        let sources = sources(&plan, files, staging, budget.absent_ok);
        drop(plan);
        let outcome = match sources {
            Ok(sources) => {
                repair_set(
                    fetcher,
                    nzb,
                    par2,
                    &mut collected,
                    (set, slice_size),
                    sources,
                    budget.memory,
                    control,
                )
                .await
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(SetOutcome::Agrees) => agreed.extend(targets.iter().map(|&(at, _)| at)),
            Ok(SetOutcome::Solved(slices)) => {
                repaired.slices += slices;
                for (at, hash) in targets {
                    open.remove(&at);
                    failed.remove(&at);
                    if files[at].hash != Some(hash) {
                        repaired.by_other_set.insert(files[at].path.clone());
                    }
                }
            }
            Err(
                error @ (RepairError::Uncovered(_)
                | RepairError::Insufficient { .. }
                | RepairError::Dependent
                | RepairError::Failed(_)),
            ) => {
                failed.extend(targets.iter().map(|&(at, _)| at));
                first_error.get_or_insert(error);
            }
            Err(error) => return Err(error),
        }
    }
    if open
        .iter()
        .any(|at| failed.contains(at) || !agreed.contains(at))
    {
        return Err(first_error.unwrap_or(RepairError::NoPar2));
    }
    Ok(repaired)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one set's repair needs the whole job context"
)]
async fn repair_set(
    fetcher: &Fetcher,
    nzb: &Nzb,
    par2: &Par2s,
    collected: &mut Collected,
    (set, slice_size): (Id, usize),
    sources: Vec<Source>,
    memory: usize,
    control: &Control,
) -> Result<SetOutcome, RepairError> {
    let ticket = control.ticket();
    let (missing, sources) =
        blocking(move || missing_slices(slice_size, sources, &ticket)).await??;
    if missing.is_empty() {
        return Ok(SetOutcome::Agrees);
    }
    let volumes = par2.volumes(nzb, slice_size);
    let mut need = missing.len();
    loop {
        let have = collected
            .fetch(fetcher, &volumes, set, slice_size, need)
            .await?;
        if have < need {
            return Err(RepairError::Insufficient {
                need: missing.len(),
                have,
            });
        }
        let rows = collected.spool().rows(set, slice_size, need);
        let (back, solved) = {
            let (missing, sources) = (missing.clone(), sources.clone());
            let held = std::mem::take(collected);
            let ticket = control.ticket();
            blocking(move || {
                let spool = held.spool();
                let solved = solve(
                    spool.critical(),
                    set,
                    spool,
                    &rows,
                    &missing,
                    &sources,
                    memory,
                    &|| ticket.cancelled(),
                );
                drop(ticket);
                (held, solved)
            })
            .await?
        };
        *collected = back;
        match solved {
            Err(RepairError::Dependent) if need < missing.len() + DEPENDENT_ROWS => {
                need += 1;
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(SetOutcome::Solved(missing.len())),
        }
    }
}

fn missing_slices(
    slice_size: usize,
    sources: Vec<Source>,
    ticket: &Ticket,
) -> Result<(Vec<usize>, Vec<Source>), RepairError> {
    let mut missing = Vec::new();
    for source in &sources {
        if source.present {
            let bad = damaged_slices(
                &source.path,
                source.file.length,
                slice_size as u64,
                &source.file.slices,
                &source.file.hash,
                &|| ticket.cancelled(),
            )
            .map_err(|e| match e.kind() {
                io::ErrorKind::Interrupted => RepairError::Cancelled,
                _ => RepairError::Io(e),
            })?;
            if source.trusted && !bad.is_empty() {
                return Err(RepairError::Failed(format!(
                    "a PAR2 set disagrees with the verified file {}",
                    source.file.name
                )));
            }
            missing.extend(bad.into_iter().map(|i| source.slices.start + i));
        } else {
            missing.extend(source.slices.clone());
        }
    }
    Ok((missing, sources))
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)
}
