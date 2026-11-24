//! Whether someone is watching: work below playback and cache-ahead starts nothing while
//! one of their requests is outstanding, held by admission or the pacer included. A reader
//! limited by its consumer waits on its budget instead and has none, so it holds no job.

use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::vendor::nntp::pool::STREAM_LINGER;

#[derive(Debug, Default)]
struct Presence {
    requests: usize,
    busy_since: Option<Instant>,
    until: Option<Instant>,
}

/// A viewer that never goes quiet still cannot hold work below it for longer than this.
const MAX_HOLD: Duration = Duration::from_hours(1);

#[derive(Debug)]
pub(crate) struct Viewers {
    presence: Mutex<Presence>,
    changed: Notify,
    max_hold: Duration,
}

impl Default for Viewers {
    fn default() -> Self {
        Self {
            presence: Mutex::default(),
            changed: Notify::new(),
            max_hold: MAX_HOLD,
        }
    }
}

impl Viewers {
    fn lock(&self) -> MutexGuard<'_, Presence> {
        self.presence.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn enter(self: &Arc<Self>) -> Viewing {
        let now = Instant::now();
        let mut presence = self.lock();
        if presence.requests == 0 && presence.until.is_none_or(|until| now >= until) {
            presence.busy_since = Some(now);
        }
        presence.requests += 1;
        drop(presence);
        self.changed.notify_waiters();
        Viewing {
            viewers: Arc::clone(self),
        }
    }

    /// Until when work below waits, unless a change comes first; `None` when it may start.
    fn hold(&self, now: Instant) -> Option<Instant> {
        let presence = self.lock();
        let longest = presence.busy_since? + self.max_hold;
        if now >= longest {
            return None;
        }
        if presence.requests > 0 {
            return Some(longest);
        }
        presence
            .until
            .filter(|&until| now < until)
            .map(|until| until.min(longest))
    }

    /// Until when outstanding viewer requests hold work below; `None` with none.
    fn demand(&self, now: Instant) -> Option<Instant> {
        let presence = self.lock();
        let longest = presence.busy_since? + self.max_hold;
        (presence.requests > 0 && now < longest).then_some(longest)
    }

    pub(crate) fn demanded(&self) -> bool {
        self.demand(Instant::now()).is_some()
    }

    pub(crate) fn held_until(&self, now: Instant) -> Option<Instant> {
        self.hold(now)
    }

    pub(crate) fn holding(&self) -> bool {
        self.hold(Instant::now()).is_some()
    }

    /// A viewer request is outstanding or just ended. Unlike the hold on work below, this
    /// has no ceiling: a long film stays watched.
    pub(crate) fn watching(&self) -> bool {
        let presence = self.lock();
        presence.requests > 0 || presence.until.is_some_and(|until| Instant::now() < until)
    }

    /// A future that resolves on the next change, armed before the caller's own check.
    pub(crate) fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }

    pub(crate) async fn clear(&self) {
        loop {
            let mut changed = pin!(self.changed());
            changed.as_mut().enable();
            let Some(until) = self.demand(Instant::now()) else {
                return;
            };
            let _ = tokio::time::timeout_at(until.into(), changed).await;
        }
    }
}

/// One viewer request. The hold after it is at most as long as the burst it ended, so a
/// reader trickling a few articles leaves the link to downloads between them.
pub(crate) struct Viewing {
    viewers: Arc<Viewers>,
}

impl Drop for Viewing {
    fn drop(&mut self) {
        let now = Instant::now();
        let mut presence = self.viewers.lock();
        presence.requests -= 1;
        if presence.requests == 0 {
            let burst = presence
                .busy_since
                .map_or(Duration::ZERO, |since| now.duration_since(since));
            presence.until = Some(now + burst.min(STREAM_LINGER));
        }
        drop(presence);
        self.viewers.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_hold_ends_after_a_burst_as_long_as_it() {
        let viewers = Arc::new(Viewers::default());
        assert!(!viewers.holding());
        let request = viewers.enter();
        assert!(viewers.holding());
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(request);
        assert!(viewers.holding());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!viewers.holding());
    }

    #[tokio::test]
    async fn work_below_waits_only_while_a_viewer_request_is_outstanding() {
        let viewers = Arc::new(Viewers::default());
        let request = viewers.enter();
        assert!(viewers.demanded());
        drop(request);
        assert!(!viewers.demanded() && viewers.holding());
        tokio::time::timeout(Duration::from_millis(50), viewers.clear())
            .await
            .expect("no outstanding request holds work below");
    }

    #[tokio::test]
    async fn a_viewer_that_never_stops_cannot_hold_work_below_forever() {
        let viewers = Arc::new(Viewers {
            max_hold: Duration::from_millis(60),
            ..Viewers::default()
        });
        let _request = viewers.enter();
        assert!(viewers.holding());
        tokio::time::timeout(Duration::from_secs(1), viewers.clear())
            .await
            .expect("the hold has a ceiling");
        assert!(!viewers.holding());
    }
}
