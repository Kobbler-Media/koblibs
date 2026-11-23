use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::OwnedSemaphorePermit;

pub struct Control {
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
    slot: Arc<OwnedSemaphorePermit>,
}

// Dropped with the job's future, so aborting the job tells detached blocking work to stop.
impl Drop for Control {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Control {
    pub fn new(running: Arc<AtomicUsize>, slot: Arc<OwnedSemaphorePermit>) -> Self {
        Self {
            cancel: Arc::default(),
            running,
            slot,
        }
    }

    pub fn ticket(&self) -> Ticket {
        self.running.fetch_add(1, Ordering::AcqRel);
        Ticket {
            cancel: Arc::clone(&self.cancel),
            running: Arc::clone(&self.running),
            _slot: Arc::clone(&self.slot),
        }
    }
}

// Blocking work outlives an aborted task; holding the job's slot keeps the concurrency limit true.
pub struct Ticket {
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
    _slot: Arc<OwnedSemaphorePermit>,
}

impl Ticket {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::AcqRel);
    }
}
