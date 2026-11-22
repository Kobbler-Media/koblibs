use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

/// Request slots shared by every provider pool. A freed slot goes to a waiting playback
/// request before any background (import, health) request, so imports never make a
/// stream wait behind them; requests already admitted run to completion. Among playback
/// waiters it goes to the reader holding the fewest slots, oldest first, so one stream's
/// deep read-ahead cannot queue another stream's reads behind it.
#[derive(Debug)]
pub struct Downloads {
    lanes: Mutex<Lanes>,
}

/// Slots wanted, the reader asking (`0`: none of its own), and where to send the permit.
type Waiter = (usize, u64, oneshot::Sender<Permit>);

#[derive(Debug)]
struct Lanes {
    total: usize,
    free: usize,
    urgent: VecDeque<Waiter>,
    background: VecDeque<Waiter>,
    /// Slots each reader's admitted playback requests hold.
    held: HashMap<u64, usize>,
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
                background: VecDeque::new(),
                held: HashMap::new(),
            }),
        })
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
            .chain(&lanes.background)
            .any(|(_, _, tx)| !tx.is_closed())
    }

    /// Whether [`Self::acquire`] would admit this request right now. A snapshot, not a
    /// reservation.
    pub(crate) fn can_acquire(&self, urgent: bool, weight: usize) -> bool {
        let lanes = self.lock();
        let weight = weight.clamp(1, lanes.total.max(1));
        let queued = !lanes.urgent.is_empty() || (!urgent && !lanes.background.is_empty());
        !queued && lanes.free >= weight
    }

    /// Waits for `weight` slots (capped at the total); `urgent` waiters are served
    /// before every background one, each lane strictly in order.
    /// Cancel-safe: slots granted to a waiter that left are handed on.
    #[cfg(test)]
    pub(crate) async fn acquire(self: &Arc<Self>, urgent: bool, weight: usize) -> Option<Permit> {
        self.acquire_for(urgent, weight, 0).await
    }

    /// [`acquire`](Self::acquire) for playback `reader` (`0`: none), which shares the
    /// playback slots fairly with other readers.
    pub(crate) async fn acquire_for(
        self: &Arc<Self>,
        urgent: bool,
        weight: usize,
        reader: u64,
    ) -> Option<Permit> {
        let reader = if urgent { reader } else { 0 };
        let rx = {
            let mut lanes = self.lock();
            let weight = weight.clamp(1, lanes.total.max(1));
            let queued = !lanes.urgent.is_empty() || (!urgent && !lanes.background.is_empty());
            if !queued && lanes.free >= weight {
                lanes.free -= weight;
                lanes.hold(reader, weight);
                return Some(Permit(Some(Arc::clone(self)), weight, reader));
            }
            let (tx, rx) = oneshot::channel();
            let lane = if urgent {
                &mut lanes.urgent
            } else {
                &mut lanes.background
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
            lanes.free += returned.0;
            lanes.unhold(returned.1, returned.0);
            for lane in [&mut lanes.urgent, &mut lanes.background] {
                while lane.front().is_some_and(|(_, _, w)| w.is_closed()) {
                    lane.pop_front();
                }
            }
            let (lane, at) = match lanes.next_urgent() {
                Some(at) => (&mut lanes.urgent, at),
                None => (&mut lanes.background, 0),
            };
            match lane.get(at) {
                Some(&(next, _, _)) if next <= lanes.free => lanes.free -= next,
                _ => return,
            }
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
        let first = downloads.acquire_for(true, 1, 1).await.unwrap();
        let _second = downloads.acquire_for(true, 1, 1).await.unwrap();
        let waiter = |reader| {
            let d = Arc::clone(&downloads);
            tokio::spawn(async move { d.acquire_for(true, 1, reader).await })
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
        assert!(downloads.can_acquire(true, 1));
        assert!(!downloads.can_acquire(true, 4));
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
            !downloads.can_acquire(true, 1),
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
        assert!(downloads.can_acquire(true, 9));
    }
}
