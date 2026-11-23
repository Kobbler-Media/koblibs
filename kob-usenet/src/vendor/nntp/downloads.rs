use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

use crate::vendor::nntp::stats::{RANKS, Workload};

/// Request slots shared by every provider pool. A freed slot goes to a waiting playback
/// request before any background (import, health) request, so imports never make a
/// stream wait behind them; requests already admitted run to completion. Background
/// waiters are served by [`Workload::rank`], each rank in order. Among playback
/// waiters it goes to the reader holding the fewest slots, oldest first, so one stream's
/// deep read-ahead cannot queue another stream's reads behind it.
#[derive(Debug)]
pub struct Downloads {
    lanes: Mutex<Lanes>,
}

const BORROW_DIVISOR: usize = 4;

/// Slots wanted, the reader asking (`0`: none of its own), and where to send the permit.
type Waiter = (usize, u64, oneshot::Sender<Permit>);

#[derive(Debug)]
struct Lanes {
    total: usize,
    free: usize,
    urgent: VecDeque<Waiter>,
    background: [VecDeque<Waiter>; RANKS],
    /// Slots each reader's admitted playback requests hold.
    held: HashMap<u64, usize>,
    /// Slots held beyond the total, by a lowered total or playback borrowing; released
    /// slots pay this off first.
    owed: usize,
}

impl Lanes {
    fn hold(&mut self, reader: u64, weight: usize) {
        if reader != 0 {
            *self.held.entry(reader).or_default() += weight;
        }
    }

    fn unhold(&mut self, reader: u64, weight: usize) {
        if let Some(held) = self.held.get_mut(&reader) {
            *held = held.saturating_sub(weight);
            if *held == 0 {
                self.held.remove(&reader);
            }
        }
    }

    /// Whether a `workload` request must queue behind waiters ahead of it.
    fn queued(&self, workload: Workload) -> bool {
        !self.urgent.is_empty()
            || workload
                .rank()
                .is_some_and(|rank| self.background[..=rank].iter().any(|l| !l.is_empty()))
    }

    /// Background requests keep their slots while parked for a connection, so with every
    /// slot taken a seek would wait for a transfer to finish even with a connection idle.
    /// Playback may borrow up to a quarter of the total past it; connection limits still hold.
    const fn fits(&self, playback: bool, weight: usize) -> bool {
        self.free >= weight
            || playback && weight - self.free + self.owed <= self.total / BORROW_DIVISOR
    }

    const fn take(&mut self, weight: usize) {
        let paid = if weight < self.free {
            weight
        } else {
            self.free
        };
        self.free -= paid;
        self.owed += weight - paid;
    }

    /// The playback waiter to admit next: the oldest of the reader holding fewest slots.
    fn next_urgent(&self) -> Option<usize> {
        self.urgent
            .iter()
            .enumerate()
            .filter(|(_, (_, _, tx))| !tx.is_closed())
            .min_by_key(|&(i, &(_, reader, _))| {
                let held = if reader == 0 {
                    0
                } else {
                    self.held.get(&reader).copied().unwrap_or(0)
                };
                (held, i)
            })
            .map(|(i, _)| i)
    }
}

/// One admitted request holding `.1` slots for reader `.2`; dropping it hands them to
/// the next waiters.
#[derive(Debug)]
pub(crate) struct Permit(Option<Arc<Downloads>>, usize, u64);

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(owner) = self.0.take() {
            owner.release(self.1, self.2);
        }
    }
}

impl Downloads {
    #[must_use]
    pub fn new(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            lanes: Mutex::new(Lanes {
                total: permits,
                free: permits,
                urgent: VecDeque::new(),
                background: Default::default(),
                held: HashMap::new(),
                owed: 0,
            }),
        })
    }

    /// Changes the number of slots; requests holding slots keep them, and a lowered
    /// total takes effect as they finish.
    pub fn resize(self: &Arc<Self>, permits: usize) {
        let added = {
            let mut lanes = self.lock();
            let added = permits.saturating_sub(lanes.total);
            let removed = lanes.total.saturating_sub(permits);
            lanes.total = permits;
            let repaid = added.min(lanes.owed);
            lanes.owed -= repaid;
            let taken = removed.min(lanes.free);
            lanes.free -= taken;
            lanes.owed += removed - taken;
            added - repaid
        };
        if added > 0 {
            Arc::clone(self).release(added, 0);
        }
    }

    /// Slots nobody holds.
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.lock().free
    }

    /// Live transfers waiting for global admission; abandoned waiters do not count.
    pub(crate) fn pressured(&self) -> bool {
        let lanes = self.lock();
        lanes
            .urgent
            .iter()
            .chain(lanes.background.iter().flatten())
            .any(|(_, _, tx)| !tx.is_closed())
    }

    /// Whether [`Self::acquire`] would admit this request right now. A snapshot, not a
    /// reservation.
    pub(crate) fn can_acquire(&self, workload: Workload, weight: usize) -> bool {
        let lanes = self.lock();
        let weight = weight.clamp(1, lanes.total.max(1));
        !lanes.queued(workload) && lanes.fits(workload.rank().is_none(), weight)
    }

    /// Waits for `weight` slots (capped at the total); `urgent` waiters are served
    /// before every background one, each lane strictly in order.
    /// Cancel-safe: slots granted to a waiter that left are handed on.
    #[cfg(test)]
    pub(crate) async fn acquire(self: &Arc<Self>, urgent: bool, weight: usize) -> Option<Permit> {
        let workload = if urgent {
            Workload::Stream
        } else {
            Workload::Queue
        };
        self.acquire_for(workload, weight, 0).await
    }

    /// [`acquire`](Self::acquire) for playback `reader` (`0`: none), which shares the
    /// playback slots fairly with other readers.
    pub(crate) async fn acquire_for(
        self: &Arc<Self>,
        workload: Workload,
        weight: usize,
        reader: u64,
    ) -> Option<Permit> {
        let reader = if workload.rank().is_none() { reader } else { 0 };
        let rx = {
            let mut lanes = self.lock();
            let weight = weight.clamp(1, lanes.total.max(1));
            if !lanes.queued(workload) && lanes.fits(workload.rank().is_none(), weight) {
                lanes.take(weight);
                lanes.hold(reader, weight);
                return Some(Permit(Some(Arc::clone(self)), weight, reader));
            }
            let (tx, rx) = oneshot::channel();
            let lane = match workload.rank() {
                None => &mut lanes.urgent,
                Some(rank) => &mut lanes.background[rank],
            };
            // Amortised sweep of waiters that gave up.
            if lane.len() >= 64 && lane.len().is_power_of_two() {
                lane.retain(|(_, _, w)| !w.is_closed());
            }
            lane.push_back((weight, reader, tx));
            rx
        };
        rx.await.ok()
    }

    fn release(self: Arc<Self>, weight: usize, reader: u64) {
        let mut returned = (weight, reader);
        loop {
            let mut guard = self.lock();
            let lanes = &mut *guard;
            let repaid = returned.0.min(lanes.owed);
            lanes.owed -= repaid;
            lanes.free += returned.0 - repaid;
            lanes.unhold(returned.1, returned.0);
            for lane in std::iter::once(&mut lanes.urgent).chain(&mut lanes.background) {
                while lane.front().is_some_and(|(_, _, w)| w.is_closed()) {
                    lane.pop_front();
                }
            }
            let urgent = lanes.next_urgent();
            let rank = lanes.background.iter().position(|lane| !lane.is_empty());
            let (at, wanted) = match (urgent, rank) {
                (Some(at), _) => (at, lanes.urgent[at].0),
                (None, Some(rank)) => (0, lanes.background[rank][0].0),
                (None, None) => return,
            };
            if !lanes.fits(urgent.is_some(), wanted) {
                return;
            }
            lanes.take(wanted);
            let lane = match (urgent, rank) {
                (None, Some(rank)) => &mut lanes.background[rank],
                _ => &mut lanes.urgent,
            };
            let (next, waiter_reader, waiter) = lane.remove(at).expect("index was just checked");
            lanes.hold(waiter_reader, next);
            drop(guard);
            returned = match waiter.send(Permit(Some(Arc::clone(&self)), next, waiter_reader)) {
                Ok(()) => (0, 0),
                // That waiter left; disarm so its slots are handed on here, not recursively.
                Err(mut permit) => {
                    permit.0 = None;
                    (next, waiter_reader)
                }
            };
        }
    }

    fn lock(&self) -> MutexGuard<'_, Lanes> {
        self.lanes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn playback_borrows_a_quarter_past_a_full_total_and_repays_it_first() {
        let downloads = Downloads::new(8);
        let mut queue = Vec::new();
        for _ in 0..8 {
            queue.push(downloads.acquire(false, 1).await.unwrap());
        }
        let first = downloads.acquire(true, 1).await.unwrap();
        let second = downloads.acquire(true, 1).await.unwrap();
        assert!(!downloads.can_acquire(Workload::Stream, 1));
        assert!(!downloads.can_acquire(Workload::Cache, 1));
        drop((first, second));
        assert_eq!(downloads.available_permits(), 0);
        assert!(downloads.can_acquire(Workload::Stream, 2));
        assert!(!downloads.can_acquire(Workload::Queue, 1));
        queue.pop();
        assert_eq!(downloads.available_permits(), 1);
    }

    #[tokio::test]
    async fn background_waiters_are_served_by_rank() {
        let downloads = Downloads::new(1);
        let held = downloads.acquire(false, 1).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut waiters = Vec::new();
        for workload in [
            Workload::Health,
            Workload::Warm,
            Workload::Queue,
            Workload::Cache,
        ] {
            let (downloads, order) = (Arc::clone(&downloads), Arc::clone(&order));
            waiters.push(tokio::spawn(async move {
                let permit = downloads.acquire_for(workload, 1, 0).await;
                order.lock().unwrap().push(workload);
                drop(permit);
            }));
            tokio::task::yield_now().await;
        }
        drop(held);
        for waiter in waiters {
            waiter.await.unwrap();
        }
        assert_eq!(
            *order.lock().unwrap(),
            [
                Workload::Cache,
                Workload::Queue,
                Workload::Warm,
                Workload::Health
            ]
        );
    }

    #[tokio::test]
    async fn a_resize_keeps_held_slots_and_lowers_the_total_as_they_finish() {
        let downloads = Downloads::new(2);
        let a = downloads.acquire(false, 1).await;
        let b = downloads.acquire(false, 1).await;
        downloads.resize(1);
        assert_eq!(downloads.available_permits(), 0);
        drop(a);
        assert_eq!(downloads.available_permits(), 0);
        drop(b);
        assert_eq!(downloads.available_permits(), 1);
        let waiting = tokio::spawn({
            let downloads = Arc::clone(&downloads);
            let held = downloads.acquire(false, 1).await;
            async move {
                let next = downloads.acquire(false, 1).await;
                drop(held);
                next.is_some()
            }
        });
        tokio::task::yield_now().await;
        downloads.resize(3);
        assert!(waiting.await.unwrap());
        assert_eq!(downloads.available_permits(), 3);
    }

    #[tokio::test]
    async fn streams_overtake_waiting_background_requests() {
        let downloads = Downloads::new(1);
        let held = downloads.acquire(false, 1).await;
        let background = tokio::spawn({
            let d = Arc::clone(&downloads);
            async move { d.acquire(false, 1).await }
        });
        tokio::task::yield_now().await;
        let abandoned = {
            let d = Arc::clone(&downloads);
            tokio::spawn(async move { d.acquire(true, 1).await })
        };
        tokio::task::yield_now().await;
        abandoned.abort();
        let _ = abandoned.await;
        let stream = tokio::spawn({
            let d = Arc::clone(&downloads);
            async move { d.acquire(true, 1).await }
        });
        tokio::task::yield_now().await;

        drop(held);
        let stream = stream.await.unwrap();
        assert!(stream.is_some(), "the stream is served first");
        assert!(!background.is_finished());
        drop(stream);
        drop(background.await.unwrap());
        assert_eq!(downloads.available_permits(), 1, "no slot leaked");
    }

    #[tokio::test]
    async fn a_freed_slot_goes_to_the_stream_holding_fewest() {
        let downloads = Downloads::new(2);
        let first = downloads.acquire_for(Workload::Stream, 1, 1).await.unwrap();
        let _second = downloads.acquire_for(Workload::Stream, 1, 1).await.unwrap();
        let waiter = |reader| {
            let d = Arc::clone(&downloads);
            tokio::spawn(async move { d.acquire_for(Workload::Stream, 1, reader).await })
        };
        let deep = waiter(1);
        tokio::task::yield_now().await;
        let late = waiter(2);
        tokio::task::yield_now().await;

        drop(first);
        let late = late.await.unwrap();
        assert!(late.is_some(), "the reader holding nothing goes first");
        assert!(
            !deep.is_finished(),
            "the older waiter of the deep reader waits"
        );
        drop(late);
        drop(deep.await.unwrap());
    }

    #[tokio::test]
    async fn a_heavy_waiter_is_not_overtaken_by_lighter_ones() {
        let downloads = Downloads::new(4);
        let held = downloads.acquire(true, 2).await;
        assert!(downloads.can_acquire(Workload::Stream, 1));
        assert!(!downloads.can_acquire(Workload::Stream, 4));
        let heavy = tokio::spawn({
            let d = Arc::clone(&downloads);
            async move { d.acquire(true, 9).await }
        });
        tokio::task::yield_now().await;
        let light = tokio::spawn({
            let d = Arc::clone(&downloads);
            async move { d.acquire(true, 1).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !light.is_finished(),
            "free slots wait for the older heavy request"
        );
        assert!(
            !downloads.can_acquire(Workload::Stream, 1),
            "an older heavy waiter goes first"
        );

        drop(held);
        let heavy = heavy.await.unwrap();
        assert_eq!(
            downloads.available_permits(),
            0,
            "weight is capped at the total"
        );
        drop(heavy);
        drop(light.await.unwrap());
        assert_eq!(downloads.available_permits(), 4, "no slot leaked");
        assert!(downloads.can_acquire(Workload::Stream, 9));
    }
}
