//! How many bytes a cache-ahead keeps in flight. Connections on one link split it evenly, so
//! whatever is in flight beyond what fills the link only delays a seek. The bound starts
//! small and grows while more in flight buys more throughput, as it does on a provider that
//! caps each connection.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const INITIAL: u64 = 20 << 20;
/// Quarters of the bound a cache-ahead keeps while a seek runs.
const SEEKING_SHARE: u64 = 3;
const PROBE: Duration = Duration::from_secs(2);
const IDLE_RESET: Duration = Duration::from_secs(10);
/// Growth that counts as the link not being full yet.
const GAIN: f64 = 1.15;

#[derive(Debug)]
struct State {
    limit: u64,
    in_flight: u64,
    tried: Option<(u64, f64)>,
    settled: bool,
    window: Instant,
    landed: u64,
    bound: bool,
    seeking: bool,
    last: Instant,
}

#[derive(Debug)]
pub(crate) struct Pace(Mutex<State>);

impl Default for Pace {
    fn default() -> Self {
        let now = Instant::now();
        Self(Mutex::new(State {
            limit: INITIAL,
            in_flight: 0,
            tried: None,
            settled: false,
            window: now,
            landed: 0,
            bound: false,
            seeking: false,
            last: now,
        }))
    }
}

impl Pace {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether another request must wait. While a seek runs the bound halves; a lone request
    /// always goes, so an article larger than the bound still moves.
    pub(crate) fn full(&self, seeking: bool) -> bool {
        let mut state = self.lock();
        state.seeking |= seeking;
        let limit = if seeking {
            state.limit * SEEKING_SHARE / 4
        } else {
            state.limit
        };
        let full = state.in_flight > 0 && state.in_flight >= limit;
        state.bound |= full;
        full
    }

    fn landed(&self, bytes: u64, now: Instant) {
        let mut state = self.lock();
        state.in_flight = state.in_flight.saturating_sub(bytes);
        state.landed += bytes;
        state.last = now;
        let elapsed = now.duration_since(state.window);
        if elapsed < PROBE {
            return;
        }
        if state.bound && !state.seeking && !state.settled {
            let rate = state.landed as f64 / elapsed.as_secs_f64();
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

    fn charge(&self, bytes: u64, now: Instant) {
        let mut state = self.lock();
        if state.in_flight == 0 && now.duration_since(state.last) >= IDLE_RESET {
            *state = State {
                in_flight: 0,
                window: now,
                last: now,
                ..Self::default()
                    .0
                    .into_inner()
                    .unwrap_or_else(PoisonError::into_inner)
            };
        }
        state.in_flight += bytes;
    }
}

/// One cache request's bytes, counted in flight from when it takes a connection.
#[derive(Debug, Default)]
pub(crate) struct Flight {
    pace: Option<Arc<Pace>>,
    bytes: u64,
    charged: bool,
}

impl Flight {
    pub(crate) fn of(pace: &Arc<Pace>, bytes: u64) -> Self {
        Self {
            pace: Some(Arc::clone(pace)),
            bytes,
            charged: false,
        }
    }

    pub(crate) fn charge(&mut self) {
        if let (Some(pace), false) = (&self.pace, self.charged) {
            pace.charge(self.bytes, Instant::now());
            self.charged = true;
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if let (Some(pace), true) = (&self.pace, self.charged) {
            pace.landed(self.bytes, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: u64 = 1 << 20;

    fn window(pace: &Pace, at: &mut Instant, rate: u64) -> u64 {
        pace.charge(ARTICLE, *at);
        assert!(!pace.full(false) || pace.lock().limit <= ARTICLE);
        pace.lock().bound = true;
        pace.lock().landed = rate * PROBE.as_secs() - ARTICLE;
        *at += PROBE;
        pace.landed(ARTICLE, *at);
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
    fn a_lone_request_always_goes_and_a_seek_halves_the_bound() {
        let pace = Pace::default();
        let now = Instant::now();
        assert!(!pace.full(true));
        pace.charge(INITIAL * SEEKING_SHARE / 4, now);
        assert!(!pace.full(false));
        assert!(pace.full(true));
    }
}
