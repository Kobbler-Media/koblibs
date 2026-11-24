//! How many bytes a cache-ahead keeps in flight. Connections on one link split it evenly, so
//! whatever is in flight beyond what fills the link only delays a seek. The bound starts
//! small and grows while more in flight buys more throughput, as it does on a provider that
//! caps each connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, OwnedMutexGuard};

const INITIAL: u64 = 20 << 20;
/// Quarters of the bound a cache-ahead keeps while a seek runs.
const SEEKING_SHARE: u64 = 3;
const PROBE: Duration = Duration::from_secs(2);
const IDLE_RESET: Duration = Duration::from_secs(10);
/// Growth that counts as the link not being full yet.
const GAIN: f64 = 1.15;
/// Beyond this much queued on the link, more in flight would only delay the running seek.
const SEEK_DRAIN_SECS: f64 = 0.5;

#[derive(Debug)]
struct State {
    limit: u64,
    in_flight: u64,
    readers: HashMap<u64, u64>,
    tried: Option<(u64, f64)>,
    settled: bool,
    window: Instant,
    landed: u64,
    bound: bool,
    seeking: bool,
    last: Instant,
}

impl State {
    fn fresh(now: Instant) -> Self {
        Self {
            limit: INITIAL,
            in_flight: 0,
            readers: HashMap::new(),
            tried: None,
            settled: false,
            window: now,
            landed: 0,
            bound: false,
            seeking: false,
            last: now,
        }
    }

    fn release(&mut self, bytes: u64, reader: u64) {
        self.in_flight = self.in_flight.saturating_sub(bytes);
        if let Some(held) = self.readers.get_mut(&reader) {
            *held = held.saturating_sub(bytes);
            if *held == 0 {
                self.readers.remove(&reader);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Pace {
    state: Mutex<State>,
    changed: Arc<Notify>,
}

impl Default for Pace {
    fn default() -> Self {
        Self::waking(Arc::default())
    }
}

impl Pace {
    /// `changed` hears of bytes given back without ever having landed.
    pub(crate) fn waking(changed: Arc<Notify>) -> Self {
        Self {
            state: Mutex::new(State::fresh(Instant::now())),
            changed,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `reader`'s next request must wait. While a seek runs the bound shrinks; a lone
    /// request always goes, so an article larger than the bound still moves. A reader under
    /// its even share of the bound goes too: one that just started would otherwise wait
    /// for the whole backlog of a reader already filling it.
    pub(crate) fn full(&self, seeking: bool, reader: u64) -> bool {
        let mut state = self.lock();
        state.seeking |= seeking;
        let limit = if seeking {
            state.limit * SEEKING_SHARE / 4
        } else {
            state.limit
        };
        let own = state.readers.get(&reader).copied().unwrap_or(0);
        let others = state.readers.len() - usize::from(own > 0);
        let share = limit / (others as u64 + 1);
        let full = state.in_flight > 0 && state.in_flight >= limit && own >= share;
        state.bound |= full;
        full
    }

    /// Bytes in flight, and the bound they would face while a seek runs.
    pub(crate) fn during_seek(&self) -> (u64, u64) {
        let state = self.lock();
        (state.in_flight, state.limit * SEEKING_SHARE / 4)
    }

    #[cfg(test)]
    pub(crate) fn landed_in_window(&self) -> u64 {
        self.lock().landed
    }

    fn refund(&self, bytes: u64, reader: u64) {
        self.lock().release(bytes, reader);
        self.changed.notify_waiters();
    }

    fn landed(&self, bytes: u64, reader: u64, now: Instant) {
        let mut state = self.lock();
        state.release(bytes, reader);
        state.landed += bytes;
        state.last = now;
        let elapsed = now.duration_since(state.window);
        if elapsed < PROBE {
            return;
        }
        let rate = state.landed as f64 / elapsed.as_secs_f64();
        let seek_limit = state.limit * SEEKING_SHARE / 4;
        let queued = state.seeking && seek_limit as f64 > rate * SEEK_DRAIN_SECS;
        if state.bound && !state.settled && !queued {
            match state.tried {
                Some((limit, best)) if rate < best * GAIN => {
                    state.limit = limit;
                    state.settled = true;
                }
                _ => {
                    state.tried = Some((state.limit, rate));
                    state.limit += state.limit / 2;
                }
            }
        }
        state.window = now;
        state.landed = 0;
        state.bound = false;
        state.seeking = false;
    }

    fn charge(&self, bytes: u64, reader: u64, now: Instant) {
        let mut state = self.lock();
        if state.in_flight == 0 && now.duration_since(state.last) >= IDLE_RESET {
            *state = State::fresh(now);
        }
        state.in_flight += bytes;
        if bytes > 0 {
            *state.readers.entry(reader).or_default() += bytes;
        }
    }
}

/// One cache request's bytes, counted in flight from when it takes a connection, or reserves
/// one it is opening. A request that never got one gives them back as not having landed.
#[derive(Debug, Default)]
pub(crate) struct Flight {
    pace: Option<Arc<Pace>>,
    bytes: u64,
    reader: u64,
    charged: bool,
    assigned: bool,
}

impl Flight {
    pub(crate) fn of(pace: &Arc<Pace>, bytes: u64, reader: u64) -> Self {
        Self {
            pace: Some(Arc::clone(pace)),
            bytes,
            reader,
            charged: false,
            assigned: false,
        }
    }

    pub(crate) fn charge(&mut self) {
        if let (Some(pace), false) = (&self.pace, self.charged) {
            pace.charge(self.bytes, self.reader, Instant::now());
            self.charged = true;
        }
    }

    pub(crate) const fn assign(&mut self) {
        self.assigned = true;
    }

    pub(crate) fn refund(&mut self) {
        if let (Some(pace), true) = (&self.pace, std::mem::take(&mut self.charged)) {
            pace.refund(self.bytes, self.reader);
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if !self.assigned {
            self.refund();
        } else if let (Some(pace), true) = (&self.pace, self.charged) {
            pace.landed(self.bytes, self.reader, Instant::now());
        }
    }
}

/// Each cache reader chooses connections in its own request order. Readers do not queue
/// behind each other: a new one would wait out everything the other has parked.
#[derive(Debug, Default)]
pub(crate) struct CacheTurns(Mutex<HashMap<u64, Arc<tokio::sync::Mutex<()>>>>);

impl CacheTurns {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Arc<tokio::sync::Mutex<()>>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) async fn take(self: &Arc<Self>, reader: u64) -> CacheTurn {
        let queue = Arc::clone(self.lock().entry(reader).or_default());
        let mut turn = CacheTurn {
            turns: Arc::clone(self),
            reader,
            queue: Some(Arc::clone(&queue)),
            guard: None,
        };
        turn.guard = Some(queue.lock_owned().await);
        turn
    }
}

pub(crate) struct CacheTurn {
    turns: Arc<CacheTurns>,
    reader: u64,
    queue: Option<Arc<tokio::sync::Mutex<()>>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for CacheTurn {
    fn drop(&mut self) {
        self.guard.take();
        self.queue.take();
        let mut queues = self.turns.lock();
        if queues
            .get(&self.reader)
            .is_some_and(|queue| Arc::strong_count(queue) == 1)
        {
            queues.remove(&self.reader);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: u64 = 1 << 20;
    const READER: u64 = 7;

    fn window(pace: &Pace, at: &mut Instant, rate: u64) -> u64 {
        pace.charge(ARTICLE, READER, *at);
        assert!(!pace.full(false, READER) || pace.lock().limit <= ARTICLE);
        pace.lock().bound = true;
        pace.lock().landed = rate * PROBE.as_secs() - ARTICLE;
        *at += PROBE;
        pace.landed(ARTICLE, READER, *at);
        pace.lock().limit
    }

    #[test]
    fn the_bound_grows_while_it_buys_throughput_and_returns_when_it_stops() {
        let pace = Pace::default();
        let mut at = pace.lock().window;
        let grown = window(&pace, &mut at, 10 << 20);
        assert_eq!(grown, INITIAL * 3 / 2);
        let grown = window(&pace, &mut at, 15 << 20);
        assert_eq!(grown, INITIAL * 9 / 4);
        assert_eq!(window(&pace, &mut at, 15 << 20), INITIAL * 3 / 2);
        assert_eq!(window(&pace, &mut at, 40 << 20), INITIAL * 3 / 2, "settled");
    }

    #[test]
    fn a_seek_grows_the_bound_only_while_it_drains_quickly() {
        for (rate, grown) in [
            (100 << 20, INITIAL * 3 / 2),
            (35 << 20, INITIAL * 3 / 2),
            (25 << 20, INITIAL),
        ] {
            let pace = Pace::default();
            let mut at = pace.lock().window;
            assert!(!pace.full(true, READER));
            assert_eq!(window(&pace, &mut at, rate), grown, "{rate} B/s");
        }
    }

    #[test]
    fn a_lone_request_always_goes_and_a_seek_halves_the_bound() {
        let pace = Pace::default();
        let now = Instant::now();
        assert!(!pace.full(true, READER));
        pace.charge(INITIAL * SEEKING_SHARE / 4, READER, now);
        assert!(!pace.full(false, READER));
        assert!(pace.full(true, READER));
    }
}
