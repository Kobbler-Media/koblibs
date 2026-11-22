use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(500);

pub trait FreeSpace: Send + Sync {
    fn available(&self, path: &Path) -> io::Result<u64>;
}

pub struct Statvfs;

impl FreeSpace for Statvfs {
    fn available(&self, path: &Path) -> io::Result<u64> {
        let stat = nix::sys::statvfs::statvfs(path).map_err(io::Error::from)?;
        Ok(stat.blocks_available().saturating_mul(stat.fragment_size()))
    }
}

pub struct SpaceGate {
    source: Arc<dyn FreeSpace>,
    floor: u64,
    paused: AtomicBool,
}

impl SpaceGate {
    pub fn new(source: Arc<dyn FreeSpace>, floor: u64) -> Self {
        Self {
            source,
            floor,
            paused: AtomicBool::new(false),
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub async fn wait(&self, path: &Path) {
        loop {
            match self.source.available(path) {
                Ok(free) if free < self.floor => {
                    if !self.paused.swap(true, Ordering::Relaxed) {
                        tracing::warn!(free, floor = self.floor, path = %path.display(), "pausing downloads: low disk space");
                    }
                    tokio::time::sleep(POLL).await;
                }
                Ok(_) => break,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "cannot read free space");
                    break;
                }
            }
        }
        if self.paused.swap(false, Ordering::Relaxed) {
            tracing::info!("resuming downloads: disk space returned");
        }
    }
}
