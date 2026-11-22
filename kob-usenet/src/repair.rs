mod check;
pub(crate) mod recovery;
mod spool;

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::jobs::fetch::Fetcher;
use crate::vendor::nntp;
use crate::vendor::par2::{self, Id, Reader, Recovery, Repair};
use crate::vendor::queue::nzb::Nzb;
pub use check::{damaged_slices, proof_damage};
use recovery::Par2s;
pub use spool::Spool;

const DEPENDENT_ROWS: usize = 3;

pub struct OnDisk {
    pub id: Option<Id>,
    pub name: String,
    pub path: PathBuf,
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
    #[error("PAR2 repair: {0}")]
    Io(#[from] io::Error),
    #[error("providers unavailable: {0}")]
    Providers(#[from] nntp::Error),
}

impl From<par2::Error> for RepairError {
    fn from(error: par2::Error) -> Self {
        match error {
            par2::Error::InsufficientRecovery => Self::Dependent,
            par2::Error::Memory { required, limit } => Self::Memory {
                need: 0,
                required,
                limit,
            },
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
}

pub async fn repair(
    fetcher: &Fetcher,
    nzb: &Nzb,
    par2: &Par2s,
    files: &[OnDisk],
    staging: &Path,
    absent_ok: bool,
    memory: usize,
) -> Result<usize, RepairError> {
    let mut collected = par2
        .index(fetcher, nzb, Spool::on_disk(staging)?)
        .await?
        .ok_or(RepairError::NoPar2)?;
    let (set, slice_size, sources) =
        sources(collected.spool().critical(), files, staging, absent_ok)?;
    let (missing, sources) = blocking(move || missing_slices(slice_size, sources)).await??;
    if missing.is_empty() {
        return Ok(0);
    }
    let loaded = missing.len().saturating_mul(slice_size + spool::HEADER + 4);
    if loaded > memory / 2 {
        return Err(RepairError::Memory {
            need: missing.len(),
            required: loaded.saturating_mul(2),
            limit: memory,
        });
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
        let packets = collected.spool().load(set, slice_size, need)?;
        let workspace = memory.saturating_sub(loaded);
        let (back, solved) = {
            let (missing, sources) = (missing.clone(), sources.clone());
            blocking(move || {
                let solved = solve(
                    collected.spool().critical(),
                    set,
                    &packets,
                    &missing,
                    &sources,
                    workspace,
                );
                (collected, solved)
            })
            .await?
        };
        collected = back;
        match solved {
            Err(RepairError::Dependent) if need < missing.len() + DEPENDENT_ROWS => {
                need += 1;
            }
            Err(RepairError::Memory {
                required, limit, ..
            }) => {
                return Err(RepairError::Memory {
                    need: missing.len(),
                    required,
                    limit,
                });
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(missing.len()),
        }
    }
}

fn sources(
    reader: &Reader,
    files: &[OnDisk],
    staging: &Path,
    absent_ok: bool,
) -> Result<(Id, usize, Vec<Source>), RepairError> {
    for set in reader.sets() {
        let Ok(plan) = reader.verification_set(set) else {
            continue;
        };
        let ours = |file: &par2::File| {
            files
                .iter()
                .find(|d| d.id == Some(file.id))
                .or_else(|| files.iter().find(|d| d.name == file.name))
        };
        if !plan.sources().iter().any(|s| ours(&s.file).is_some()) {
            continue;
        }
        let mut sources = Vec::new();
        for source in plan.sources() {
            let found = ours(&source.file);
            let path = found.map_or_else(
                || {
                    staging.join(
                        crate::vendor::queue::names::sanitize(&source.file.name)
                            .unwrap_or_default(),
                    )
                },
                |d| d.path.clone(),
            );
            if found.is_none() && !absent_ok {
                return Err(RepairError::Uncovered(source.file.name.clone()));
            }
            sources.push(Source {
                file: source.file.clone(),
                slices: source.slices.clone(),
                present: found.is_some(),
                path,
            });
        }
        return Ok((set, plan.slice_size(), sources));
    }
    Err(RepairError::NoPar2)
}

fn missing_slices(
    slice_size: usize,
    sources: Vec<Source>,
) -> io::Result<(Vec<usize>, Vec<Source>)> {
    let mut missing = Vec::new();
    for source in &sources {
        if source.present {
            let bad = damaged_slices(
                &source.path,
                source.file.length,
                slice_size as u64,
                &source.file.slices,
                &source.file.hash,
            )?;
            missing.extend(bad.into_iter().map(|i| source.slices.start + i));
        } else {
            missing.extend(source.slices.clone());
        }
    }
    Ok((missing, sources))
}

fn solve(
    reader: &Reader,
    set: Id,
    packets: &[Vec<u8>],
    missing: &[usize],
    sources: &[Source],
    memory: usize,
) -> Result<(), RepairError> {
    let plan = reader.repair_set(set)?;
    let recovery = packets
        .iter()
        .map(|p| Recovery::read(p, set, plan.slice_size()))
        .collect::<Result<Vec<_>, _>>()?;
    let inputs = plan
        .inputs()
        .ok_or_else(|| RepairError::Failed("slice checksums are incomplete".to_owned()))?;
    let mut repair = Repair::new(
        plan.slice_size(),
        inputs,
        missing,
        &recovery,
        memory,
        || false,
    )?;
    let lost: BTreeSet<usize> = missing.iter().copied().collect();
    let mut buf = vec![0; plan.slice_size()];
    for (source, ours) in plan.sources().iter().zip(sources) {
        if !ours.present {
            continue;
        }
        let file = File::open(&ours.path)?;
        for index in source.slices.clone().filter(|i| !lost.contains(i)) {
            let (_, range) = plan.source(index).expect("index lies in its source");
            let data = &mut buf[..usize::try_from(range.end - range.start).unwrap_or(0)];
            file.read_exact_at(data, range.start)?;
            repair.feed(index, data, || false)?;
        }
    }
    let mut touched = BTreeSet::new();
    for recovered in repair.finish(|| false)? {
        let (position, range) = plan
            .source(recovered.index)
            .expect("recovered index lies in a source");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&sources[position].path)?;
        if file.metadata()?.len() != plan.sources()[position].file.length {
            file.set_len(plan.sources()[position].file.length)?;
        }
        file.write_all_at(&recovered.data, range.start)?;
        touched.insert(position);
    }
    for position in touched {
        let source = &sources[position];
        if !damaged_slices(&source.path, source.file.length, 0, &[], &source.file.hash)?.is_empty()
        {
            return Err(RepairError::Failed(format!(
                "{} still fails its checksum after repair",
                source.file.name
            )));
        }
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)
}
