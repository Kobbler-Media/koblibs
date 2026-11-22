use std::ops::Range;
use std::path::Path;

use crate::jobs::JobState;
use crate::jobs::failure::Failure;
use crate::jobs::run::Job;
use crate::jobs::store::Plan;
use crate::repair::{self, OnDisk, RepairError, proof_damage};
use crate::vendor::core::FileMeta;
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
    let checks = {
        let (plan, staging) = (plan.clone(), staging.to_path_buf());
        tokio::task::spawn_blocking(move || check(&plan, &damage, &staging))
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
        job.fetcher,
        job.nzb,
        job.par2,
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
