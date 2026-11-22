use std::path::{Path, PathBuf};

use crate::jobs::Shared;
use crate::jobs::failure::Failure;
use crate::jobs::run::stored;
use crate::jobs::store::{Phase, StoredSpec};

pub fn finish(shared: &Shared, spec: &StoredSpec, staging: &Path) -> Result<PathBuf, Failure> {
    if !staging.is_dir() {
        return lost(shared, spec, staging);
    }
    let mut target = spec.dest_dir.join(&spec.folder);
    let mut n = 1;
    while target.exists() {
        target = spec.dest_dir.join(format!("{}.{n}", spec.folder));
        n += 1;
    }
    stored(
        shared
            .store
            .put_phase(&spec.id, &Phase::Moving(target.clone())),
    )?;
    std::fs::rename(staging, &target)
        .map_err(|e| Failure::io(&format!("moving to {}", target.display()), e))?;
    Ok(target)
}

pub fn moved(
    shared: &Shared,
    spec: &StoredSpec,
    staging: &Path,
    target: &Path,
) -> Result<PathBuf, Failure> {
    match (staging.is_dir(), target.exists()) {
        (true, false) => std::fs::rename(staging, target)
            .map(|()| target.to_path_buf())
            .map_err(|e| Failure::io(&format!("moving to {}", target.display()), e)),
        (true, true) => finish(shared, spec, staging),
        (false, true) => Ok(target.to_path_buf()),
        (false, false) => lost(shared, spec, staging),
    }
}

fn lost(shared: &Shared, spec: &StoredSpec, staging: &Path) -> Result<PathBuf, Failure> {
    stored(shared.store.put_phase(&spec.id, &Phase::Downloading))?;
    Err(Failure::Infrastructure(format!(
        "{} disappeared before the download was moved into place; downloading it again",
        staging.display()
    )))
}
