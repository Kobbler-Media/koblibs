use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

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

    use super::hand_off_worker;

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
