use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use kob_usenet::{DirectDisk, JobDisk};

const STEP: Duration = Duration::from_millis(10);
const DIRTY_LIMIT: u64 = 2 << 30;

// Writes land in the page cache until the dirty limit. A sync pays a journal commit and waits
// for what was written when it began. Without early writeback the device only starts on that
// data once a sync asks; with it, the device drains the cache continuously.
pub struct ModelDisk {
    step: u64,
    commit: Duration,
    early: bool,
    written: AtomicU64,
    drained: AtomicU64,
}

impl ModelDisk {
    fn drain_step(&self, upto: u64) {
        let _ = self
            .drained
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |drained| {
                Some(upto.min(drained + self.step).max(drained))
            });
    }
}

impl JobDisk for ModelDisk {
    fn write_at(&self, file: &File, data: &[u8], at: u64) -> io::Result<()> {
        while self.written.load(Ordering::Relaxed) - self.drained.load(Ordering::Relaxed)
            > DIRTY_LIMIT
        {
            std::thread::sleep(STEP);
        }
        file.write_all_at(data, at)?;
        self.written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn sync(&self, _: &File) -> io::Result<()> {
        let target = self.written.load(Ordering::Relaxed);
        std::thread::sleep(self.commit);
        while self.drained.load(Ordering::Relaxed) < target {
            std::thread::sleep(STEP);
            if !self.early {
                self.drain_step(target);
            }
        }
        Ok(())
    }
}

fn writeback(disk: Weak<ModelDisk>) {
    while let Some(disk) = disk.upgrade() {
        disk.drain_step(disk.written.load(Ordering::Relaxed));
        drop(disk);
        std::thread::sleep(STEP);
    }
}

pub fn from_env() -> Arc<dyn JobDisk> {
    let Ok(spec) = std::env::var("KOB_MEASURE_DISK") else {
        return Arc::new(DirectDisk);
    };
    let mut fields = spec.split(':');
    let rate = fields.next().unwrap().parse::<u64>().unwrap() * 1_000_000;
    let commit = Duration::from_millis(fields.next().unwrap().parse().unwrap());
    let early = fields.next() == Some("early");
    let disk = Arc::new(ModelDisk {
        step: (rate as f64 * STEP.as_secs_f64()) as u64,
        commit,
        early,
        written: AtomicU64::new(0),
        drained: AtomicU64::new(0),
    });
    if early {
        let weak = Arc::downgrade(&disk);
        std::thread::spawn(move || writeback(weak));
    }
    disk
}
