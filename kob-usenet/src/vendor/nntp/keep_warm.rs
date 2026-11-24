//! A provider restarts TCP slow start on a connection left idle, which at a 210 ms round trip
//! turns a 0.23 s article into 1.25 s. Measured on the primary provider, a STAT every 250 ms
//! keeps the window open through 30 s of idling and one every second does not. So while
//! someone watches, a few idle connections that have just moved a body get one.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

use crate::vendor::nntp::connection::Link;
use crate::vendor::nntp::stats::Stats;
use crate::vendor::nntp::viewers::Viewers;

pub(crate) const KEEP_WARM_CONNECTIONS: usize = 4;
pub(crate) const KEEP_WARM_EVERY: Duration = Duration::from_millis(250);
/// Longest since its last body a connection can join: by the one-second mark the provider
/// has already collapsed its window, and a STAT would keep nothing.
const JOIN_WITHIN: Duration = Duration::from_millis(500);

pub(crate) struct KeepWarm {
    viewers: Arc<Viewers>,
    target: usize,
    members: AtomicUsize,
    primary: AtomicBool,
    id: Mutex<Option<Arc<str>>>,
    stats: Mutex<Arc<Stats>>,
    changed: Arc<Notify>,
}

impl KeepWarm {
    pub(crate) fn new(viewers: Arc<Viewers>, target: usize, changed: Arc<Notify>) -> Self {
        Self {
            viewers,
            target,
            members: AtomicUsize::new(0),
            primary: AtomicBool::new(false),
            id: Mutex::new(None),
            stats: Mutex::default(),
            changed,
        }
    }

    fn lock_id(&self) -> MutexGuard<'_, Option<Arc<str>>> {
        self.id.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_primary(&self, primary: bool) {
        self.primary.store(primary, Ordering::Relaxed);
    }

    pub(crate) fn set_stats(&self, stats: Arc<Stats>) {
        *self.stats.lock().unwrap_or_else(PoisonError::into_inner) = stats;
    }

    /// Remembers an article a viewer is reading, whose id the keep-warm STATs ask about.
    pub(crate) fn note(&self, message_id: &str) {
        if !self.primary.load(Ordering::Relaxed) {
            return;
        }
        let mut id = self.lock_id();
        if id.as_deref() != Some(message_id) {
            *id = Some(message_id.into());
        }
    }

    pub(crate) fn id(&self) -> Option<Arc<str>> {
        self.lock_id().clone()
    }

    pub(crate) fn active(&self) -> bool {
        self.target > 0 && self.primary.load(Ordering::Relaxed) && self.viewers.watching()
    }

    /// Whether `link` moved a body recently enough to be worth keeping.
    pub(crate) fn eligible(&self, link: &Link) -> bool {
        self.active() && link.since_body().is_some_and(|age| age <= JOIN_WITHIN)
    }

    pub(crate) fn join(self: &Arc<Self>, link: &Arc<Link>) -> Option<KeptSlot> {
        if !self.eligible(link) {
            return None;
        }
        self.members
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < self.target).then_some(n + 1)
            })
            .ok()?;
        link.set_kept(true);
        Some(KeptSlot {
            keep: Arc::clone(self),
            link: Arc::clone(link),
        })
    }

    pub(crate) fn sent(&self) {
        self.stats
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keep_warm();
    }
}

/// One connection's place in the kept set, given up the moment it has real work.
pub(crate) struct KeptSlot {
    keep: Arc<KeepWarm>,
    link: Arc<Link>,
}

impl Drop for KeptSlot {
    fn drop(&mut self) {
        self.link.set_kept(false);
        self.keep.members.fetch_sub(1, Ordering::Relaxed);
        self.keep.changed.notify_waiters();
    }
}
