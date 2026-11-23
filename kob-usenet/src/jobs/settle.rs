use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::jobs::JobState;
use crate::jobs::failure::Failure;
use crate::jobs::run::Job;
use crate::jobs::store::Plan;
use crate::repair::{self, Budget, Control, OnDisk, RepairError, Repaired, proof_damage};
use crate::vendor::core::{FileMeta, NzbFile};
use crate::vendor::yenc::crc32;

pub enum Settled {
    Clean,
    Raw,
}

enum Check {
    Clean,
    Member(String),
    Unprotected(String),
    Repairable,
}

pub async fn settle(
    job: &Job<'_>,
    plan: &Plan,
    damage: Vec<Vec<Range<u64>>>,
) -> Result<Settled, Failure> {
    let (shared, spec, staging) = (job.shared, job.spec, job.staging);
    let paths = plan
        .outputs
        .iter()
        .map(|o| o.under(staging))
        .collect::<Result<Vec<PathBuf>, Failure>>()?;
    let checks = {
        let (plan, paths) = (plan.clone(), paths.clone());
        tokio::task::spawn_blocking(move || check(&plan, &damage, &paths))
            .await
            .map_err(|e| Failure::io("verifying", e))?
            .map_err(|e| Failure::io("verifying", e))?
    };
    let fallback = !plan.raw && !job.par2.is_empty();
    for check in &checks {
        match check {
            Check::Member(reason) | Check::Unprotected(reason) if fallback => {
                tracing::info!(job = %spec.id, %reason, "downloading whole volumes to repair");
                return Ok(Settled::Raw);
            }
            Check::Member(reason) | Check::Unprotected(reason) => {
                return Err(Failure::Content(format!(
                    "{reason}, and no PAR2 recovery can repair it"
                )));
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
        .zip(&paths)
        .zip(&checks)
        .filter_map(|((o, path), check)| match &o.meta {
            FileMeta::Nzb(f) => Some(OnDisk {
                id: f.proof.as_ref().map(|p| p.file_id),
                hash: f.proof.as_ref().map(|p| p.file_md5),
                name: o.path.rsplit('/').next().unwrap_or(&o.path).to_owned(),
                path: path.clone(),
                damaged: matches!(check, Check::Repairable),
            }),
            FileMeta::Multipart(_) => None,
        })
        .collect();
    let control = Control::new(Arc::clone(&shared.solving), Arc::clone(job.slot));
    let repaired = repair::repair(
        job.fetcher,
        job.nzb,
        job.par2,
        &files,
        staging,
        Budget {
            memory: shared.config.repair_memory_bytes,
            absent_ok: plan.raw,
        },
        &control,
    )
    .await;
    match repaired {
        Ok(repaired) => {
            let slices = repaired.slices;
            let (plan, paths) = (plan.clone(), paths.clone());
            tokio::task::spawn_blocking(move || still_damaged(&plan, &paths, &repaired))
                .await
                .map_err(|e| Failure::io("verifying", e))?
                .map_err(|e| Failure::io("verifying", e))?
                .map_or(Ok(()), |name| {
                    Err(Failure::Content(format!(
                        "{name} is still damaged after PAR2 repair"
                    )))
                })?;
            tracing::info!(job = %spec.id, slices, "repaired with PAR2");
            Ok(Settled::Clean)
        }
        Err(RepairError::Uncovered(reason)) if fallback => {
            tracing::info!(job = %spec.id, %reason, "downloading whole volumes to repair");
            Ok(Settled::Raw)
        }
        Err(error) => Err(repair_failure(error)),
    }
}

fn still_damaged(
    plan: &Plan,
    paths: &[PathBuf],
    repaired: &Repaired,
) -> std::io::Result<Option<String>> {
    for (output, path) in plan.outputs.iter().zip(paths) {
        let FileMeta::Nzb(NzbFile {
            proof: Some(proof), ..
        }) = &output.meta
        else {
            continue;
        };
        if !repaired.by_other_set.contains(path) && !proof_damage(path, proof)?.is_empty() {
            return Ok(Some(output.path.clone()));
        }
    }
    Ok(None)
}

fn repair_failure(error: RepairError) -> Failure {
    match error {
        RepairError::Providers(error) => Failure::providers(&error),
        error @ (RepairError::Io(_) | RepairError::Memory { .. } | RepairError::Cancelled) => {
            Failure::Infrastructure(error.to_string())
        }
        error @ (RepairError::NoPar2
        | RepairError::Uncovered(_)
        | RepairError::Insufficient { .. }
        | RepairError::Dependent
        | RepairError::Failed(_)) => Failure::Content(error.to_string()),
    }
}

fn check(
    plan: &Plan,
    damage: &[Vec<Range<u64>>],
    paths: &[PathBuf],
) -> std::io::Result<Vec<Check>> {
    plan.outputs
        .iter()
        .zip(damage)
        .zip(paths)
        .map(|((output, damaged), path)| {
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
                    && crc_of(path)? != expected
                {
                    return Ok(Check::Member(format!(
                        "{}: CRC mismatch against the archive header",
                        output.path
                    )));
                }
                return Ok(Check::Clean);
            }
            if let Some(proof) = proof {
                return Ok(
                    if damaged.is_empty() && proof_damage(path, proof)?.is_empty() {
                        Check::Clean
                    } else {
                        Check::Repairable
                    },
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_failures_of_the_machine_are_infrastructure() {
        let full = || std::io::Error::from(std::io::ErrorKind::StorageFull);
        for (error, machine) in [
            (RepairError::Io(full()), true),
            (
                RepairError::Memory {
                    need: 9,
                    required: 1 << 30,
                    limit: 1 << 20,
                },
                true,
            ),
            (RepairError::Insufficient { need: 9, have: 3 }, false),
            (RepairError::Dependent, false),
            (RepairError::NoPar2, false),
        ] {
            let said = error.to_string();
            let failure = repair_failure(error);
            assert_eq!(
                matches!(failure, Failure::Infrastructure(_)),
                machine,
                "{said}"
            );
        }
    }
}
