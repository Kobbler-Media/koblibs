use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, Semaphore, SemaphorePermit};

use crate::ImportMode;

const PLAY: usize = 4;
const PREWARM: usize = 2;

pub struct Admission {
    play: Semaphore,
    prewarm: Semaphore,
    waiting: AtomicUsize,
    admitted: Notify,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            play: Semaphore::new(PLAY),
            prewarm: Semaphore::new(PREWARM),
            waiting: AtomicUsize::new(0),
            admitted: Notify::new(),
        }
    }
}

impl Admission {
    pub async fn enter(&self, mode: ImportMode) -> Option<SemaphorePermit<'_>> {
        if mode == ImportMode::Prewarm {
            let slot = self.prewarm.acquire().await.ok()?;
            loop {
                let admitted = self.admitted.notified();
                if self.waiting.load(Ordering::Acquire) == 0 {
                    return Some(slot);
                }
                admitted.await;
            }
        }
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let slot = self.play.acquire().await.ok();
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        self.admitted.notify_waiters();
        slot
    }
}
