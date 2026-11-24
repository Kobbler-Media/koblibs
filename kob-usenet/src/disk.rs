use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::Path;

use tokio::runtime::{Handle, RuntimeFlavor};

pub trait JobDisk: Send + Sync {
    fn write_at(&self, file: &File, data: &[u8], at: u64) -> io::Result<()> {
        file.write_all_at(data, at)?;
        start_writeback(file, at, data.len());
        Ok(())
    }

    fn sync(&self, file: &File) -> io::Result<()> {
        file.sync_data()
    }

    fn sync_path(&self, path: &Path) -> io::Result<()> {
        open_to_sync(path)?.sync_all()
    }
}

// An archive can set a member's mode to unreadable, and fsync needs an open descriptor.
fn open_to_sync(path: &Path) -> io::Result<File> {
    match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let mode = fs::symlink_metadata(path)?.permissions().mode();
            fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o400))?;
            let opened = File::open(path);
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            opened
        }
        opened => opened,
    }
}

pub(crate) fn sync_tree(disk: &dyn JobDisk, root: &Path) -> io::Result<()> {
    let kind = fs::symlink_metadata(root)?.file_type();
    if kind.is_dir() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() || kind.is_file() {
                sync_tree(disk, &entry.path())?;
            }
        }
    } else if !kind.is_file() {
        return Ok(());
    }
    disk.sync_path(root)
}

pub(crate) fn sync_parent(disk: &dyn JobDisk, path: &Path) -> io::Result<()> {
    disk.sync_path(path.parent().unwrap_or_else(|| Path::new(".")))
}

pub struct DirectDisk;

impl JobDisk for DirectDisk {}

// Only a hint that spreads device work across the download, so the periodic fsync finds little
// left to do. It promises no durability and a filesystem without support loses nothing.
#[allow(unsafe_code)]
fn start_writeback(file: &File, at: u64, len: usize) {
    let (Ok(offset), Ok(bytes)) = (libc::off64_t::try_from(at), libc::off64_t::try_from(len))
    else {
        return;
    };
    // SAFETY: the descriptor is borrowed from a live `File` and no memory is passed.
    unsafe {
        libc::sync_file_range(file.as_raw_fd(), offset, bytes, libc::SYNC_FILE_RANGE_WRITE);
    }
}

pub(crate) fn hand_off_worker<T>(work: impl FnOnce() -> T) -> T {
    match Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{DirectDisk, hand_off_worker, sync_tree};

    #[test]
    fn a_member_unpacked_without_read_permission_is_still_synced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked.bin");
        std::fs::write(&locked, b"data").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        sync_tree(&DirectDisk, dir.path()).unwrap();
        let mode = std::fs::metadata(&locked).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0, "the member keeps its mode");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocking_work_leaves_the_runtime_serving_other_tasks() {
        let served = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&served);
        let blocker = tokio::spawn(async move {
            hand_off_worker(|| {
                let started = std::time::Instant::now();
                while !flag.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                flag.load(Ordering::SeqCst)
            })
        });
        std::thread::sleep(Duration::from_millis(50));
        let flag = Arc::clone(&served);
        tokio::spawn(async move { flag.store(true, Ordering::SeqCst) });
        assert!(blocker.await.unwrap(), "the worker stayed blocked");
    }
}
