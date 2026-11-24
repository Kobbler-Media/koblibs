use std::path::{Path, PathBuf};

use crate::disk::{JobDisk, hand_off_worker, sync_parent, sync_tree};
use crate::jobs::Shared;
use crate::jobs::failure::Failure;
use crate::jobs::run::stored;
use crate::jobs::store::{Phase, StoredSpec};

pub async fn make_durable(shared: &Shared, staging: &Path) -> Result<(), Failure> {
    let disk = std::sync::Arc::clone(&shared.config.disk);
    let staging = staging.to_path_buf();
    tokio::task::spawn_blocking(move || {
        sync_tree(disk.as_ref(), &staging)?;
        sync_parent(disk.as_ref(), &staging)?;
        match staging.parent() {
            Some(holder) => sync_parent(disk.as_ref(), holder),
            None => Ok(()),
        }
    })
    .await
    .map_err(|e| Failure::io("syncing the download", e))?
    .map_err(|e| Failure::io("syncing the download", e))
}

pub async fn finish(
    shared: &Shared,
    spec: &StoredSpec,
    staging: &Path,
) -> Result<PathBuf, Failure> {
    if !staging.is_dir() {
        return lost(shared, spec, staging);
    }
    make_durable(shared, staging).await?;
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
    place(shared, staging, &target)?;
    Ok(target)
}

pub async fn moved(
    shared: &Shared,
    spec: &StoredSpec,
    staging: &Path,
    target: &Path,
) -> Result<PathBuf, Failure> {
    match (staging.is_dir(), target.exists()) {
        (true, false) => place(shared, staging, target).map(|()| target.to_path_buf()),
        (true, true) => finish(shared, spec, staging).await,
        (false, true) => {
            hand_off_worker(|| sync_parents(shared.config.disk.as_ref(), staging, target))
                .map(|()| target.to_path_buf())
                .map_err(|e| Failure::io(&format!("syncing {}", target.display()), e))
        }
        (false, false) => lost(shared, spec, staging),
    }
}

// On the worker rather than a blocking task: an aborted job must not leave a rename running
// behind the removal that follows the abort.
fn place(shared: &Shared, staging: &Path, target: &Path) -> Result<(), Failure> {
    hand_off_worker(|| {
        std::fs::rename(staging, target)?;
        sync_parents(shared.config.disk.as_ref(), staging, target)
    })
    .map_err(|e| Failure::io(&format!("moving to {}", target.display()), e))
}

fn sync_parents(disk: &dyn JobDisk, staging: &Path, target: &Path) -> std::io::Result<()> {
    sync_parent(disk, staging)?;
    sync_parent(disk, target)
}

fn lost(shared: &Shared, spec: &StoredSpec, staging: &Path) -> Result<PathBuf, Failure> {
    stored(shared.store.put_phase(&spec.id, &Phase::Downloading))?;
    Err(Failure::Infrastructure(format!(
        "{} disappeared before the download was moved into place; downloading it again",
        staging.display()
    )))
}
