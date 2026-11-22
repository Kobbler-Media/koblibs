use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

    pub fn free(&self, path: &Path) -> Option<u64> {
        match self.source.available(path) {
            Ok(free) => Some(free),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot read free space");
                None
            }
        }
    }

    pub fn can_reserve(&self, path: &Path, size: u64) -> bool {
        self.free(path)
            .is_some_and(|free| free.checked_sub(size).is_some_and(|left| left > self.floor))
    }

    pub fn has_room(&self, path: &Path, need: u64) -> bool {
        self.free(path)
            .is_none_or(|free| free >= need.saturating_add(self.floor))
    }

    pub fn admits(&self, path: &Path, credit: u64) -> bool {
        let Some(free) = self.free(path) else {
            return true;
        };
        let admitted = free.saturating_add(credit) >= self.floor;
        if admitted {
            if self.paused.swap(false, Ordering::Relaxed) {
                tracing::info!("resuming downloads: disk space returned");
            }
        } else if !self.paused.swap(true, Ordering::Relaxed) {
            tracing::warn!(free, floor = self.floor, path = %path.display(), "pausing downloads: low disk space");
        }
        admitted
    }
}
