use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use crate::vendor::core::trace::{Outcome, Probe, Reason, Step};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::time::timeout;

use crate::vendor::nntp::article::Body;
use crate::vendor::nntp::client::{BodySink, Lane};
use crate::vendor::nntp::connection::{ConnectionStats, Link, Phase, Timing, validate_message_id};
use crate::vendor::nntp::downloads::{Downloads, Permit};
use crate::vendor::nntp::pace::{Flight, Pace};
use crate::vendor::nntp::stages::{
    Assigned, ConnectTimes, LoadAtAssign, ReplyTimes, RequestTrace, StageClock,
};
use crate::vendor::nntp::stats::{Stats, Workload, outcome};
use crate::vendor::nntp::{Connection, Endpoint, Error, Progress, Usage};

/// Retries after this request's own read failed at the connection level (stale idle
/// socket, stall) before giving up.
const MAX_RETRIES: usize = 2;
/// Re-dispatches of requests lost unanswered when their connection died under another
/// request; bounds a provider that drops every connection.
const MAX_UNANSWERED: usize = 16;
/// After a connect fails with no other connection live or connecting, requests fail
/// fast for this long instead of each waiting out its own connect.
const CONNECT_BACKOFF: Duration = Duration::from_secs(1);
/// A connect that never reached the provider while others are open holds back new
/// connects for [`CONNECT_BACKOFF`], doubling up to this while they keep failing.
const MAX_CONNECT_BACKOFF: Duration = Duration::from_secs(30);
/// A refused connect shrinks the limit to the connections we have; it then regrows by
/// one per step.
const THROTTLE_STEP: Duration = Duration::from_secs(5);
/// Playback gives up waiting for a download slot and connection after this long.
const STREAM_WAIT: Duration = Duration::from_secs(30);
/// Playback waits at most this long (or half the endpoint timeout) for a new connection
/// while another is live, then pipelines on that one.
const STREAM_CONNECT_WAIT: Duration = Duration::from_secs(3);
/// From this effective limit up, background work leaves one connection slot for playback.
const STREAM_HEADROOM_MIN_CONNECTIONS: usize = 4;
/// From this effective limit up, work below cache leaves another slot for a cache reader,
/// whose first read would otherwise wait for a full pipeline to drain.
const CACHE_HEADROOM_MIN_CONNECTIONS: usize = 16;
/// Background work stays bounded this long after the last playback request: reads and
/// seeks come in bursts.
pub(crate) const STREAM_LINGER: Duration = Duration::from_secs(2);
/// While bounded, background work leaves playback the connections it holds plus this
/// many, a seek's first reads, up to a quarter of the limit.
const STREAM_SPARE: usize = 4;
/// While a cache reader is active or lingering, work below it leaves it the connections
/// it holds plus this many, up to three quarters of the limit: lane isolation keeps cache
/// reads from pipelining behind downloads, so without idle connections they wait for
/// whole pipelines to drain.
const CACHE_SPARE: usize = 4;
/// Connections a seek may take from a running cache-ahead, which keeps the rest.
const SEEK_CONNECTIONS: usize = 2;
/// Cache requests in flight across the pool, at least one per connection: what is pipelined
/// on a shared link is what a seek has to wait for.
const CACHE_FLIGHT: usize = 16;
/// How often a waiting quick check looks again, for changes no connection announces.
const LIGHT_RECHECK: Duration = Duration::from_millis(50);

const fn cache_depth(limit: usize, depth: usize) -> usize {
    let wanted = CACHE_FLIGHT.div_ceil(if limit == 0 { 1 } else { limit });
    if wanted < depth { wanted } else { depth }
}

/// Connections kept for seeks beside a cache-ahead on a pool of `limit`.
const fn seek_connections(limit: usize) -> usize {
    if limit < STREAM_HEADROOM_MIN_CONNECTIONS {
        return 0;
    }
    let quarter = limit / 4;
    if quarter < SEEK_CONNECTIONS {
        quarter
    } else {
        SEEK_CONNECTIONS
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Provider connection limit; never exceeded.
    pub max_connections: usize,
    /// BODY requests in flight per connection.
    pub pipeline_depth: usize,
    /// Connections with nothing to do for this long are closed, down to
    /// `min_connections`; those are kept alive with keepalives instead.
    pub idle_timeout: Duration,
    /// Warm floor: idle connections are not closed below this many while the quota
    /// lasts. [`Client::warm`](crate::vendor::nntp::Client::warm) reopens the ones that die.
    pub min_connections: usize,
}

/// Connections to one provider. Each connection is owned by its own task, which
/// pipelines requests and reads every reply to completion, so a caller that stops
/// waiting never desynchronises the connection. Clones are handles to the same pool.
#[derive(Clone)]
pub struct Pool {
    endpoint: Endpoint,
    config: PoolConfig,
    downloads: Arc<Downloads>,
    /// Requests holding a ticket.
    busy: Arc<AtomicUsize>,
    warm_busy: Arc<AtomicUsize>,
    /// Playback requests waiting for global admission or a provider connection.
    stream_waiting: Arc<AtomicUsize>,
    /// Playback requests holding a download slot and waiting for a connection.
    stream_parked: Arc<AtomicUsize>,
    /// [`Workload::Cache`] requests holding a download slot and waiting for a
    /// connection; lower-ranked background work takes none meanwhile.
    cache_parked: Arc<AtomicUsize>,
    /// Taken in request order by [`Workload::Cache`] requests choosing a connection, so a
    /// reader's next segment is not outrun by the read-ahead parked behind it.
    cache_turn: Arc<tokio::sync::Mutex<()>>,
    pace: Arc<Pace>,
    /// Connection tasks running.
    live: Arc<AtomicUsize>,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    /// Counts every failed connect, background ones included.
    stats: Arc<Stats>,
    stream_wait: Duration,
    /// `weight-large` experiment: articles of [`LARGE_ARTICLE`] or more weigh a full depth.
    weight_large: bool,
    /// How long the provider keeps articles; `None` for as long as anyone.
    retention: Option<Duration>,
    /// One permit per open connection, shared by every pool on the same account, so a
    /// pool replacing another opens connections only as the old one closes them.
    account: Arc<Semaphore>,
}

/// Articles this large are never pipelined under `weight-large`.
const LARGE_ARTICLE: u32 = 2_097_152;

#[derive(Default)]
struct State {
    workers: Vec<Worker>,
    connecting: usize,
    /// `(limit, since)` after the provider turned a connect away while others were up.
    throttle: Option<(usize, Instant)>,
    /// Set when a connect could not reach the provider while others were up.
    backoff: Option<Backoff>,
    /// Last connect failure on a pool with nothing else live.
    failed_at: Option<Instant>,
    /// Why that connect failed; cleared once one succeeds.
    connect_error: Option<Arc<str>>,
    connected: bool,
    /// Playback requests in [`Pool::fetch`]; background work is bounded meanwhile.
    streams: Presence,
    /// [`Workload::Cache`] requests in [`Pool::fetch`]; work below them is bounded meanwhile.
    caches: Presence,
    /// Replaced: takes no new requests and closes each connection once it drains.
    retired: bool,
}

#[derive(Clone, Copy)]
struct Backoff {
    until: Instant,
    delay: Duration,
    failures: u32,
}

impl State {
    const fn presence(&mut self, cache: bool) -> &mut Presence {
        if cache {
            &mut self.caches
        } else {
            &mut self.streams
        }
    }

    /// Until when new connects wait; never while nothing is open, so an outage still
    /// gets noticed.
    fn held(&self, open: usize) -> Option<Instant> {
        self.backoff
            .map(|backoff| backoff.until)
            .filter(|&until| open > 0 && Instant::now() < until)
    }

    /// Effective connection limit, regrowing one per [`THROTTLE_STEP`] after a throttle.
    fn limit(&self, max: usize) -> usize {
        self.throttle.map_or(max, |(limit, since)| {
            let steps = since.elapsed().as_secs() / THROTTLE_STEP.as_secs();
            limit
                .saturating_add(usize::try_from(steps).unwrap_or(usize::MAX))
                .min(max)
        })
    }

    /// Holds back new connects after one that `started` failed to reach the provider;
    /// `true` when this starts a new run of failures.
    fn back_off(&mut self, started: Instant, now: Instant) -> bool {
        match &mut self.backoff {
            // Judged by start time: a slow failure is still a retry, not a new run.
            Some(backoff) if started < backoff.until + backoff.delay => {
                backoff.failures += 1;
                // A burst started before the hold fails within it; only a retry after it lengthens it.
                if started >= backoff.until {
                    backoff.delay = (backoff.delay * 2).min(MAX_CONNECT_BACKOFF);
                    backoff.until = now + backoff.delay;
                }
                false
            }
            _ => {
                self.backoff = Some(Backoff {
                    until: now + CONNECT_BACKOFF,
                    delay: CONNECT_BACKOFF,
                    failures: 1,
                });
                true
            }
        }
    }
}

/// Why [`Pool::fetch`] failed.
pub(crate) enum Failure {
    /// No connection could be opened and none was live: the provider is unreachable.
    Connect(Error),
    Request(Error),
}

struct Worker {
    lane: AtomicU64,
    /// The lane belongs to a [`Workload::Cache`] reader.
    cached: AtomicBool,
    tx: mpsc::Sender<Request>,
    load: Arc<AtomicUsize>,
    task: tokio::task::AbortHandle,
    link: Arc<Link>,
}

/// A connection's sender, load, requests ahead, id, and how long it took if newly opened.
type Picked = (
    mpsc::Sender<Request>,
    Load,
    usize,
    u32,
    Option<ConnectTimes>,
);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Body,
    Stat,
}

/// Who a request is for, so its completion lands in the right counters and trace.
#[derive(Clone)]
pub(crate) struct Tag {
    pub(crate) workload: Workload,
    pub(crate) provider: u16,
    pub(crate) stats: Option<Arc<Stats>>,
    pub(crate) probe: Option<Probe>,
    pub(crate) progress: Option<Progress>,
    /// Geometry the body must have; see [`Connection::read_body_with`].
    pub(crate) expected: crate::vendor::nntp::BodyExpectation,
    /// Gets bodies whose caller left before they arrived.
    pub(crate) sink: Option<Arc<OnceLock<BodySink>>>,
    /// Waits for a connection with nothing in flight rather than pipeline behind
    /// another request: a hedge queued behind a slow article is as slow as it.
    pub(crate) fresh: bool,
    /// A provider's miss is remembered, so later requests skip that provider.
    pub(crate) remember: bool,
    pub(crate) lane: Option<Lane>,
    /// Skips admission and the hold for viewers; see [`Pool::light_worker`].
    pub(crate) light: bool,
    /// Set on playback and cache-ahead bodies.
    pub(crate) stages: Option<StageClock>,
}

impl Tag {
    pub(crate) const UNTRACKED: Self = Self {
        workload: Workload::Stream,
        provider: 0,
        stats: None,
        probe: None,
        progress: None,
        expected: crate::vendor::nntp::BodyExpectation::NONE,
        sink: None,
        fresh: false,
        remember: true,
        lane: None,
        light: false,
        stages: None,
    };

    fn step(&self, step: Step) {
        if let Some(probe) = &self.probe {
            probe.step(step);
        }
    }
}

type Reply = Result<Option<Body>, Error>;

/// One attempt on one connection. Records its outcome exactly once: when answered,
/// before the caller sees the reply, or else when dropped. The ticket then releases
/// the permit and connection load.
struct Request {
    command: Command,
    message_id: String,
    /// `None` answers a STAT. Taken by [`answer`](Self::answer).
    reply: Option<oneshot::Sender<Reply>>,
    tag: Tag,
    started: Instant,
    recorded: bool,
    trace: Option<Box<RequestTrace>>,
    _ticket: Ticket,
}

impl Request {
    fn answer(mut self, result: Reply, decode: Duration) {
        let reply = self.reply.take();
        let bytes = match &result {
            Ok(Some(body)) => u32::try_from(body.data.len()).unwrap_or(u32::MAX),
            _ => 0,
        };
        let ended = match &result {
            // The caller stopped waiting; the reply was drained anyway.
            _ if reply.as_ref().is_none_or(oneshot::Sender::is_closed) => Outcome::Cancelled,
            Ok(_) => Outcome::Retrieved,
            Err(error) => outcome(error),
        };
        if matches!((&ended, &result), (Outcome::Retrieved, Ok(Some(_))))
            && let Some(trace) = self.trace.take()
        {
            trace.delivered(bytes);
        }
        self.record(ended, bytes, decode);
        if let Some(reply) = reply.filter(|r| !r.is_closed()) {
            let _ = reply.send(result);
        } else if let (Ok(Some(body)), Some(sink)) =
            (&result, self.tag.sink.as_ref().and_then(|s| s.get()))
        {
            sink(&self.message_id, self.tag.workload, body, true);
        }
    }

    fn record(&mut self, outcome: Outcome, bytes: u32, decode: Duration) {
        self.recorded = true;
        if let Some(trace) = self.trace.take() {
            trace.abandoned();
        }
        if let Some(stats) = &self.tag.stats {
            stats.record(self.tag.workload, outcome, bytes, self.started.elapsed());
        }
        self.tag.step(Step::Done {
            provider: self.tag.provider,
            outcome,
            bytes,
            decode_ns: u32::try_from(decode.as_nanos()).unwrap_or(u32::MAX),
        });
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if self.recorded {
            return;
        }
        // Unanswered: the connection died, or the caller left before it was sent.
        if self.reply.as_ref().is_none_or(oneshot::Sender::is_closed) {
            self.record(Outcome::Cancelled, 0, Duration::ZERO);
        } else {
            // Re-dispatched, so not a provider failure; the error that ended the
            // connection is counted on the request it hit.
            self.tag.stats = None;
            self.record(Outcome::NotRetrieved(Reason::Network), 0, Duration::ZERO);
        }
    }
}

/// Held for exactly the life of one request; dropping it is the completion.
struct Ticket {
    // First, so a waiter woken by the connection's load sees these bytes gone.
    _flight: Flight,
    _download: Permit,
    _load: Load,
    _busy: Load,
    _warm: Option<Load>,
}

pub(crate) struct Load(Arc<AtomicUsize>, Option<Arc<Notify>>);

impl Load {
    pub(crate) fn acquire(load: &Arc<AtomicUsize>) -> Self {
        load.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(load), None)
    }

    /// A connection's load; its release wakes requests waiting for an idle one.
    fn watched(load: &Arc<AtomicUsize>, changed: &Arc<Notify>) -> Self {
        load.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(load), Some(Arc::clone(changed)))
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        if let Some(changed) = &self.1 {
            changed.notify_waiters();
        }
    }
}

impl Pool {
    /// `downloads` caps in-flight requests across every provider sharing it.
    pub fn new(
        endpoint: Endpoint,
        config: PoolConfig,
        downloads: Arc<Downloads>,
    ) -> Result<Self, Error> {
        if config.max_connections == 0 || config.pipeline_depth == 0 {
            return Err(Error::InvalidArgument("pool config"));
        }
        Ok(Self {
            endpoint,
            config,
            downloads,
            busy: Arc::default(),
            warm_busy: Arc::default(),
            stream_waiting: Arc::default(),
            stream_parked: Arc::default(),
            cache_parked: Arc::default(),
            cache_turn: Arc::default(),
            pace: Arc::default(),
            live: Arc::default(),
            state: Arc::default(),
            changed: Arc::default(),
            stats: Arc::default(),
            stream_wait: STREAM_WAIT,
            weight_large: false,
            retention: None,
            account: Arc::new(Semaphore::new(config.max_connections)),
        })
    }

    /// Shares the account's connection limit with other pools on it: `account` holds one
    /// permit per connection the account allows.
    #[must_use]
    pub fn with_account(mut self, account: Arc<Semaphore>) -> Self {
        self.account = account;
        self
    }

    /// Whether `other` is a handle to this same pool.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Stops taking requests: idle connections close now, busy ones once their requests
    /// are answered, which frees their account permits for the pool replacing this one.
    pub fn retire(&self) {
        let workers = {
            let mut state = self.lock();
            state.retired = true;
            std::mem::take(&mut state.workers)
        };
        drop(workers);
        self.changed.notify_waiters();
    }

    pub(crate) fn set_stats(&mut self, stats: Arc<Stats>) {
        self.stats = stats;
    }

    #[cfg(test)]
    pub(crate) const fn with_stream_wait(mut self, wait: Duration) -> Self {
        self.stream_wait = wait;
        self
    }

    /// Articles older than `retention` are not asked of this provider.
    #[must_use]
    pub const fn with_retention(mut self, retention: Duration) -> Self {
        self.retention = Some(retention);
        self
    }

    /// Whether an article `age` old is past this provider's retention.
    pub(crate) fn outlives(&self, age: Option<Duration>) -> bool {
        self.retention
            .zip(age)
            .is_some_and(|(kept, age)| age > kept)
    }

    /// The `weight-large` experiment: articles of 2,097,152 bytes or more take a whole
    /// connection's permits whatever the depth.
    #[must_use]
    pub const fn with_weight_large(mut self, on: bool) -> Self {
        self.weight_large = on;
        self
    }

    /// Fetches and decodes one body (message-id without angle brackets).
    pub async fn body(&self, message_id: &str) -> Result<Body, Error> {
        validate_message_id(message_id)?;
        match self.fetch(Command::Body, message_id, &Tag::UNTRACKED).await {
            Ok(body) => body.ok_or(Error::Protocol),
            Err(Failure::Connect(error) | Failure::Request(error)) => Err(error),
        }
    }

    /// Whether the provider has the article: `Ok` if so, [`Error::NotFound`] if not.
    pub async fn stat(&self, message_id: &str) -> Result<(), Error> {
        validate_message_id(message_id)?;
        match self.fetch(Command::Stat, message_id, &Tag::UNTRACKED).await {
            Ok(_) => Ok(()),
            Err(Failure::Connect(error) | Failure::Request(error)) => Err(error),
        }
    }

    /// Opens connections concurrently until `connections` (capped at the limit) are open
    /// or connecting. Best effort: failures are left to the next request.
    pub async fn prewarm(&self, connections: usize) {
        let reserved = {
            let mut state = self.lock();
            state.workers.retain(|w| !w.tx.is_closed());
            let open = state.workers.len() + state.connecting;
            let backoff = state.retired
                || open == 0
                    && state
                        .failed_at
                        .is_some_and(|at| at.elapsed() < CONNECT_BACKOFF);
            let reserved = if backoff || state.held(open).is_some() {
                0
            } else {
                connections.min(self.limit(&state)).saturating_sub(open)
            };
            state.connecting += reserved;
            reserved
        };
        // Dropping a new connection's load leaves it idle in the pool.
        join_all((0..reserved).map(|_| self.open(self.connecting(), None, None, false))).await;
    }

    /// Ends every connection now, abandoning the requests on them.
    pub fn abort(&self) {
        let workers = std::mem::take(&mut self.lock().workers);
        for worker in workers {
            worker.task.abort();
        }
    }

    /// Closes connections with no request on them.
    pub(crate) fn close_idle(&self) {
        self.lock()
            .workers
            .retain(|w| !w.tx.is_closed() && w.load.load(Ordering::Relaxed) > 0);
    }

    pub(crate) const fn max_connections(&self) -> usize {
        self.config.max_connections
    }

    pub(crate) const fn min_connections(&self) -> usize {
        self.config.min_connections
    }

    /// Whether the endpoint's [`Usage`](crate::vendor::nntp::Usage) reached its cap.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.endpoint.usage().is_some_and(|u| u.exhausted())
    }

    pub(crate) const fn pipeline_depth(&self) -> usize {
        self.config.pipeline_depth
    }

    /// Download slots one request holds: one per started MB of article, up to the pipeline
    /// depth, so large articles keep about one connection's worth of bytes in flight each
    /// (a seek then waits behind less) while small ones still pipeline.
    fn weight(&self, size: Option<u32>) -> usize {
        const UNIT: u32 = 1_000_000;
        size.map_or(1, |size| {
            if self.weight_large && size >= LARGE_ARTICLE {
                return self.config.pipeline_depth;
            }
            (size.div_ceil(UNIT) as usize).clamp(1, self.config.pipeline_depth)
        })
    }

    /// Whether a playback hedge of `size` bytes would get a permit and an idle or new
    /// connection right now. A snapshot, not a reservation.
    pub(crate) fn can_hedge(&self, size: u32) -> bool {
        if !self
            .downloads
            .can_acquire(Workload::Stream, self.weight(Some(size)))
        {
            return false;
        }
        let state = self.lock();
        let live = state.workers.iter().filter(|w| !w.tx.is_closed());
        let idle = live.clone().any(|w| w.load.load(Ordering::Relaxed) == 0);
        let open = live.count() + state.connecting;
        let backed_off = open == 0
            && state
                .failed_at
                .is_some_and(|at| at.elapsed() < CONNECT_BACKOFF);
        let spare = open < self.limit(&state) && !backed_off && state.held(open).is_none();
        drop(state);
        idle || spare
    }

    /// Download permits not in use, across every pool sharing them.
    pub(crate) fn free_permits(&self) -> usize {
        self.downloads.available_permits()
    }

    /// Requests per mille of capacity. Playback includes requests still awaiting
    /// admission and uses the throttled limit.
    pub(crate) fn busy_permille(&self, warming: bool) -> usize {
        let (capacity, pending) = if warming {
            (self.config.max_connections * self.config.pipeline_depth, 0)
        } else {
            let state = self.lock();
            (
                self.limit(&state) * self.config.pipeline_depth,
                self.stream_waiting.load(Ordering::Relaxed),
            )
        };
        self.busy().saturating_add(pending).saturating_mul(1000) / capacity
    }

    /// Effective connection capacity, occupied request slots and physical warming
    /// requests. Tickets keep abandoned drains counted until transport completion.
    pub(crate) fn warming_capacity(&self) -> (usize, usize, usize, bool) {
        let state = self.lock();
        let limit = self.limit(&state);
        drop(state);
        (
            limit,
            self.busy.load(Ordering::Relaxed),
            self.warm_busy.load(Ordering::Relaxed),
            self.downloads.pressured(),
        )
    }

    /// Requests in flight or queued on a connection.
    pub(crate) fn busy(&self) -> usize {
        self.busy.load(Ordering::Relaxed)
    }

    /// Open connections.
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// Connection tasks taking requests, and how many of them have none.
    pub(crate) fn connections(&self) -> (usize, usize) {
        let state = self.lock();
        let open = state.workers.iter().filter(|w| !w.tx.is_closed());
        let idle = open
            .clone()
            .filter(|w| w.load.load(Ordering::Relaxed) == 0)
            .count();
        let open = open.count();
        drop(state);
        (open, idle)
    }

    /// Whether a connection ever opened, and why the last connect failed while none was
    /// open (cleared when one opens).
    pub(crate) fn connect_status(&self) -> (bool, Option<Arc<str>>) {
        let state = self.lock();
        (state.connected, state.connect_error.clone())
    }

    /// Open connections, and connects in progress.
    pub(crate) fn links(&self) -> (Vec<ConnectionStats>, usize) {
        let state = self.lock();
        let links = state
            .workers
            .iter()
            .filter(|w| !w.tx.is_closed())
            .map(|w| w.link.view(w.load.load(Ordering::Relaxed)))
            .collect();
        let connecting = state.connecting;
        drop(state);
        (links, connecting)
    }

    /// [`body`](Self::body) or [`stat`](Self::stat) for a validated message-id.
    #[allow(clippy::significant_drop_tightening)] // False positive: the permit moves into the ticket.
    pub(crate) async fn fetch(
        &self,
        command: Command,
        message_id: &str,
        tag: &Tag,
    ) -> Result<Option<Body>, Failure> {
        let (mut failed, mut unanswered) = (0, 0);
        let mut attempt_from = tag.stages.as_ref().map(StageClock::attempt_from);
        let provider = tag.provider;
        let stream = tag.workload == Workload::Stream;
        // Shared by retries; only admission waits spend it, not transfers.
        let mut budget = self.stream_wait;
        let _present = match tag.workload {
            Workload::Stream => Some(Present::start(self, false)),
            Workload::Cache | Workload::Import => Some(Present::start(self, true)),
            _ => None,
        };
        let viewers = self.downloads.viewers();
        let _viewing = matches!(tag.workload, Workload::Stream | Workload::Cache)
            .then(|| viewers.enter(tag.workload == Workload::Cache));
        let held_back = !tag.light && tag.workload.rank().is_some_and(|rank| rank > 0);
        let lane = if stream {
            tag.lane.or(Some(Lane::STREAM))
        } else {
            tag.lane
        };
        loop {
            // Waits holding no download slot: a slot parked here is one a cache reader,
            // the very work being waited for, could never get.
            if held_back {
                viewers.clear().await;
            }
            let stream_waiting = stream.then(|| Load::acquire(&self.stream_waiting));
            let admitted = tokio::time::Instant::now();
            let deadline = stream.then(|| admitted + budget);
            let acquire = self.downloads.acquire_for(
                tag.workload,
                self.weight(tag.probe.as_ref().map(|probe| probe.size).or_else(|| {
                    tag.expected
                        .decoded_len
                        .map(|len| u32::try_from(len).unwrap_or(u32::MAX))
                })),
                tag.probe.as_ref().map_or(0, |probe| probe.reader),
            );
            let download = if tag.light {
                drop(acquire);
                Permit::none()
            } else {
                match deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, acquire)
                        .await
                        .map_err(|_| Failure::Request(Error::Unavailable))?,
                    None => acquire.await,
                }
                .ok_or(Failure::Request(Error::Closed))?
            };
            let permitted = Instant::now();
            tag.step(Step::Permitted { provider });
            let parked = match tag.workload {
                Workload::Stream => Some(Load::watched(&self.stream_parked, &self.changed)),
                Workload::Cache | Workload::Import => {
                    Some(Load::watched(&self.cache_parked, &self.changed))
                }
                _ => None,
            };
            let mut flight = if tag.workload == Workload::Cache {
                let size = tag.probe.as_ref().map(|probe| u64::from(probe.size));
                Flight::of(&self.pace, size.or(tag.expected.decoded_len).unwrap_or(0))
            } else {
                Flight::default()
            };
            let picked = if tag.light {
                let (tx, load, ahead, connection) = self.light_worker().await?;
                Some((tx, load, ahead, connection, None))
            } else {
                self.worker(tag.workload, tag.fresh, deadline, lane, &mut flight)
                    .await?
            };
            drop(parked);
            let Some((tx, load, ahead, connection, opened)) = picked else {
                continue;
            };
            // Withdrawn while it waited: someone else asks for the article instead.
            if tag
                .probe
                .as_ref()
                .and_then(|probe| probe.wait.as_ref())
                .is_some_and(|wait| !wait.send())
            {
                return Err(Failure::Request(Error::Unavailable));
            }
            budget = budget.saturating_sub(admitted.elapsed());
            tag.step(Step::Connected {
                provider,
                ahead: u16::try_from(ahead).unwrap_or(u16::MAX),
                connection,
            });
            let trace = tag
                .stages
                .as_ref()
                .zip(attempt_from)
                .and_then(|(clock, from)| {
                    RequestTrace::assign(
                        clock,
                        tag.workload,
                        Assigned {
                            attempt_from: from,
                            permitted,
                            ahead,
                            connection,
                            provider,
                            opened,
                            load: stream.then(|| self.load_at_assign()),
                        },
                    )
                });
            let ticket = Ticket {
                _flight: flight,
                _download: download,
                _load: load,
                _busy: Load::acquire(&self.busy),
                _warm: (tag.workload == Workload::Warm).then(|| Load::acquire(&self.warm_busy)),
            };
            drop(stream_waiting);
            if tag.fresh
                && let Some(stats) = &tag.stats
            {
                stats.hedged();
            }
            let request = Request {
                command,
                message_id: message_id.to_owned(),
                reply: None,
                tag: tag.clone(),
                started: Instant::now(),
                recorded: false,
                trace,
                _ticket: ticket,
            };
            match dispatch(&tx, request).await {
                // Playback fails over to the next provider rather than wait out another stall.
                Some(Err(Error::Timeout)) if stream => {
                    return Err(Failure::Request(Error::Timeout));
                }
                // A 400 ends the session (RFC 3977 §3.2.1); the server closes after it.
                Some(Err(
                    Error::Closed | Error::Io(_) | Error::Timeout | Error::Status { code: 400, .. },
                )) if failed < MAX_RETRIES => {
                    failed += 1;
                }
                Some(result) => return result.map_err(Failure::Request),
                None if unanswered < MAX_UNANSWERED => unanswered += 1,
                None => return Err(Failure::Request(Error::Closed)),
            }
            attempt_from = tag.stages.is_some().then(Instant::now);
        }
    }

    fn load_at_assign(&self) -> LoadAtAssign {
        let busy_connections = self
            .lock()
            .workers
            .iter()
            .filter(|w| w.load.load(Ordering::Relaxed) > 0)
            .count();
        let (pace_in_flight, pace_bound) = self.pace.during_seek();
        LoadAtAssign {
            busy_connections,
            requests_in_flight: self.busy(),
            pace_in_flight,
            pace_bound,
        }
    }

    /// An idle connection, else a new one while the limit allows, else the least loaded;
    /// with the requests already on it and its id. A `stream` request that tires of a
    /// connect leaves it running in its own task and pipelines on a live connection
    /// instead. A `fresh` request never pipelines: it waits for an idle connection instead.
    /// For limits of four or more, background work leaves one idle or unopened slot for
    /// playback and waits for pending connections before pipelining. While playback is
    /// active or lingering, background work leaves playback the connections it holds plus
    /// [`STREAM_SPARE`], never pipelines behind playback, and takes no connection at all
    /// while playback waits for one. Work below cache is bounded the same way by an active
    /// cache reader, with [`CACHE_SPARE`], and leaves it a headroom slot of its own.
    /// Past the admission `deadline` it fails as [`Error::Unavailable`], except that a
    /// cold connect it started itself runs on, so a hung provider is blamed for it.
    async fn worker(
        &self,
        workload: Workload,
        fresh: bool,
        deadline: Option<tokio::time::Instant>,
        lane: Option<Lane>,
        flight: &mut Flight,
    ) -> Result<Option<Picked>, Failure> {
        let stream = workload == Workload::Stream;
        let outranked = workload.rank().is_some_and(|rank| rank > 0);
        let cached = workload.rank() == Some(0) && lane.is_some();
        let viewers = self.downloads.viewers();
        let (mut impatient, mut first) = (false, true);
        let mut turn = if workload == Workload::Cache {
            Some(self.cache_turn.lock().await)
        } else {
            None
        };
        loop {
            let viewer_arrived = viewers.changed();
            let mut viewer_arrived = pin!(viewer_arrived);
            viewer_arrived.as_mut().enable();
            if outranked && viewers.holding() {
                return Ok(None);
            }
            // A spent budget still takes a connection that is free right away.
            if !std::mem::take(&mut first)
                && deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                return Err(Failure::Request(Error::Unavailable));
            }
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            let (reserved, live, held) = {
                let mut state = self.lock();
                if state.retired {
                    return Err(Failure::Request(Error::Unavailable));
                }
                state.workers.retain(|w| !w.tx.is_closed());
                let open = state.workers.len() + state.connecting;
                let limit = self.limit(&state);
                let held = state.held(open);
                let headroom = match workload.rank() {
                    None => 0,
                    Some(_) if limit < STREAM_HEADROOM_MIN_CONNECTIONS => 0,
                    Some(rank) if rank > 0 && limit >= CACHE_HEADROOM_MIN_CONNECTIONS => 2,
                    // A reader's first request may take all but one free slot, so several
                    // readers starting together still leave a seek a connection.
                    Some(_) if workload == Workload::Cache => {
                        // A connect in progress counts: a burst of read-ahead would otherwise
                        // see nothing busy yet and open every slot.
                        let reading = state.connecting > 0
                            || state.workers.iter().any(|w| {
                                w.load.load(Ordering::Relaxed) > 0
                                    && lane.is_none_or(|l| {
                                        w.cached.load(Ordering::Relaxed)
                                            && w.lane.load(Ordering::Relaxed) == l.0
                                    })
                            });
                        let most = seek_connections(limit);
                        if reading { most } else { most.min(1) }
                    }
                    Some(_) => 1,
                };
                let hold_headroom = headroom > 0 && {
                    let idle = state
                        .workers
                        .iter()
                        .filter(|worker| worker.load.load(Ordering::Relaxed) == 0)
                        .count();
                    idle + limit.saturating_sub(open) <= headroom
                };
                let now = Instant::now();
                let streaming = !stream && state.streams.active(now);
                let caching = outranked && state.caches.active(now);
                let linger = [
                    state.streams.lingering(now).filter(|_| !stream),
                    state.caches.lingering(now).filter(|_| outranked),
                ]
                .into_iter()
                .flatten()
                .min();
                let bounded = streaming || caching;
                let busy = |worker: &Worker| worker.load.load(Ordering::Relaxed) > 0;
                let playing =
                    |worker: &Worker| worker.lane.load(Ordering::Relaxed) == Lane::STREAM.0;
                let caches = |worker: &Worker| worker.cached.load(Ordering::Relaxed);
                let background = |worker: &&Worker| {
                    busy(worker) && !playing(worker) && !(caching && caches(worker))
                };
                // Connections this work holds, and may hold besides the reserves above it.
                let share = bounded.then(|| {
                    let held = |class: &dyn Fn(&Worker) -> bool| {
                        state.workers.iter().filter(|w| busy(w) && class(w)).count()
                    };
                    let stream_reserve = if streaming {
                        let most = if workload == Workload::Cache {
                            seek_connections(limit)
                        } else {
                            limit / 4
                        };
                        (held(&playing) + STREAM_SPARE).min(most)
                    } else {
                        0
                    };
                    let cache_reserve = if caching {
                        (held(&caches) + CACHE_SPARE).min(limit * 3 / 4)
                    } else {
                        usize::from(outranked && limit >= CACHE_HEADROOM_MIN_CONNECTIONS)
                    };
                    let busy = state.workers.iter().filter(background).count();
                    (
                        busy + state.connecting,
                        limit.saturating_sub(stream_reserve + cache_reserve),
                    )
                });
                // Over its share, background work lets connections drain for playback.
                let starved = streaming && self.stream_parked.load(Ordering::Relaxed) > 0
                    || share.is_some_and(|(busy, room)| busy > room);
                // Below cache work, background work lets connections drain for a waiting
                // cache request, which never pipelines behind it.
                let yielding = outranked && self.cache_parked.load(Ordering::Relaxed) > 0;
                let capped = starved || share.is_some_and(|(busy, room)| busy >= room);
                let spare =
                    open < limit && !hold_headroom && !capped && !yielding && held.is_none();
                let live = !state.workers.is_empty();
                // Beside a cache-ahead a seek keeps to its few connections, pipelining on
                // them, so the file's sequential fetch never loses more than those.
                let seek_capped = stream && viewers.caching() && {
                    let most = seek_connections(limit).max(1);
                    state
                        .workers
                        .iter()
                        .filter(|w| busy(w) && playing(w))
                        .count()
                        >= most
                };
                let cache_room = cache_depth(limit, self.config.pipeline_depth);
                let paced = workload == Workload::Cache && self.pace.full(streaming);
                let spare = spare && !seek_capped && !paced;
                let best = state
                    .workers
                    .iter()
                    .filter(|worker| !seek_capped || (busy(worker) && playing(worker)))
                    .filter(|_| !paced)
                    .filter(|worker| {
                        !seek_capped
                            || worker.load.load(Ordering::Relaxed) < self.config.pipeline_depth
                    })
                    .filter(|worker| {
                        let load = worker.load.load(Ordering::Relaxed);
                        !bounded
                            || (load == 0 && !capped)
                            || (!starved && load < self.config.pipeline_depth && background(worker))
                    })
                    .filter(|_| !yielding)
                    // A cache reader past a full pipeline parks instead, so work below it
                    // yields connections and its read-ahead spreads over them.
                    .filter(|worker| {
                        workload != Workload::Cache
                            || worker.load.load(Ordering::Relaxed) < cache_room
                    })
                    .filter(|worker| !hold_headroom || worker.load.load(Ordering::Relaxed) > 0)
                    .filter(|worker| {
                        lane.is_none_or(|lane| {
                            worker.load.load(Ordering::Relaxed) == 0
                                || worker.lane.load(Ordering::Relaxed) == lane.0
                        })
                    })
                    .min_by_key(|w| w.load.load(Ordering::Relaxed));
                let reserved = match best {
                    // Spread before pipelining: providers cap bandwidth per connection,
                    // and a slot still connecting is idle soon (a cold start's prewarm).
                    Some(w)
                        if (!hold_headroom && w.load.load(Ordering::Relaxed) == 0)
                            || (!fresh && (impatient || (!spare && state.connecting == 0))) =>
                    {
                        flight.charge();
                        let ahead = w.load.load(Ordering::Relaxed);
                        // An unscoped caller makes the connection mixed until idle;
                        // scoped callers may not pipeline behind that unknown work.
                        if ahead == 0 || lane.is_none() {
                            w.lane
                                .store(lane.map_or(0, |lane| lane.0), Ordering::Relaxed);
                            w.cached.store(cached, Ordering::Relaxed);
                        }
                        return Ok(Some((
                            w.tx.clone(),
                            Load::watched(&w.load, &self.changed),
                            ahead,
                            w.link.id,
                            None,
                        )));
                    }
                    _ if deadline
                        .is_some_and(|deadline| tokio::time::Instant::now() >= deadline) =>
                    {
                        return Err(Failure::Request(Error::Unavailable));
                    }
                    _ if open == 0
                        && state
                            .failed_at
                            .is_some_and(|at| at.elapsed() < CONNECT_BACKOFF) =>
                    {
                        return Err(Failure::Connect(Error::Unavailable));
                    }
                    _ if spare => {
                        flight.charge();
                        state.connecting += 1;
                        true
                    }
                    // Every slot is still connecting.
                    _ => false,
                };
                drop(state);
                (reserved, live, held.into_iter().chain(linger).min())
            };
            if reserved {
                // Connects run side by side; only choosing a connection is in order.
                drop(turn.take());
                let now = tokio::time::Instant::now();
                // A detached connect keeps only the endpoint's own deadline.
                let until = deadline
                    .filter(|_| !(stream && live))
                    .map(|deadline| deadline.max(now + STREAM_CONNECT_WAIT));
                let mut open = Box::pin(self.open(self.connecting(), until, lane, cached));
                let opened = if stream && live {
                    let mut patience = now + STREAM_CONNECT_WAIT.min(self.endpoint.timeout() / 2);
                    if let Some(deadline) = deadline {
                        patience = patience.min(deadline);
                    }
                    if let Ok(opened) = tokio::time::timeout_at(patience, &mut open).await {
                        opened
                    } else {
                        // Joins the pool idle if it ever connects.
                        tokio::spawn(open);
                        impatient = true;
                        continue;
                    }
                } else {
                    open.await
                };
                if let Some((tx, load, id, times)) = opened? {
                    return Ok(Some((tx, load, 0, id, Some(times))));
                }
                continue;
            }
            let wake = match (held.map(tokio::time::Instant::from), deadline) {
                (Some(held), Some(deadline)) => Some(held.min(deadline)),
                (held, deadline) => held.or(deadline),
            };
            let either = poll_fn(|cx| {
                if changed.as_mut().poll(cx).is_ready()
                    || (outranked && viewer_arrived.as_mut().poll(cx).is_ready())
                {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            });
            match wake {
                Some(until) => {
                    let _ = tokio::time::timeout_at(until, either).await;
                }
                None => either.await,
            }
        }
    }

    /// A connection for a STAT that must answer quickly whatever else is running: the
    /// least loaded one that is not serving playback. While someone watches it opens
    /// none of its own and leaves idle connections to a waiting seek.
    async fn light_worker(&self) -> Result<(mpsc::Sender<Request>, Load, usize, u32), Failure> {
        let viewers = self.downloads.viewers();
        loop {
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            let reserved = {
                let mut state = self.lock();
                if state.retired {
                    return Err(Failure::Request(Error::Unavailable));
                }
                state.workers.retain(|w| !w.tx.is_closed());
                let open = state.workers.len() + state.connecting;
                let limit = self.limit(&state);
                let watching = viewers.holding();
                let load = |w: &Worker| w.load.load(Ordering::Relaxed);
                // STATs answer within a round trip, so they may use the connections kept
                // for seeks until a seek actually waits.
                let free_idle = !watching || self.stream_waiting.load(Ordering::Relaxed) == 0;
                let best = state
                    .workers
                    .iter()
                    .filter(|w| load(w) < self.config.pipeline_depth)
                    .filter(|w| load(w) == 0 || w.lane.load(Ordering::Relaxed) != Lane::STREAM.0)
                    .filter(|w| free_idle || load(w) > 0)
                    .min_by_key(|w| load(w));
                if let Some(w) = best {
                    let ahead = load(w);
                    if ahead == 0 {
                        w.lane.store(0, Ordering::Relaxed);
                        w.cached.store(false, Ordering::Relaxed);
                    }
                    return Ok((
                        w.tx.clone(),
                        Load::watched(&w.load, &self.changed),
                        ahead,
                        w.link.id,
                    ));
                }
                if open == 0
                    && state
                        .failed_at
                        .is_some_and(|at| at.elapsed() < CONNECT_BACKOFF)
                {
                    return Err(Failure::Connect(Error::Unavailable));
                }
                let reserved =
                    open < limit && (!watching || open == 0) && state.held(open).is_none();
                if reserved {
                    state.connecting += 1;
                }
                reserved
            };
            if reserved {
                if let Some((tx, load, id, _)) =
                    self.open(self.connecting(), None, None, false).await?
                {
                    return Ok((tx, load, 0, id));
                }
                continue;
            }
            let _ = timeout(LIGHT_RECHECK, changed).await;
        }
    }

    fn limit(&self, state: &State) -> usize {
        state.limit(self.config.max_connections)
    }

    /// `None` when the connect was refused but other connections can take the request.
    /// The connect ends by `until` or the endpoint timeout, whichever comes first.
    fn open(
        &self,
        connecting: Connecting,
        until: Option<tokio::time::Instant>,
        lane: Option<Lane>,
        cached: bool,
    ) -> impl Future<
        Output = Result<Option<(mpsc::Sender<Request>, Load, u32, ConnectTimes)>, Failure>,
    > + Send
    + 'static {
        let (endpoint, config) = (self.endpoint.clone(), self.config);
        let usage = endpoint.usage().cloned();
        let live = Arc::clone(&self.live);
        let stats = Arc::clone(&self.stats);
        let account = Arc::clone(&self.account);
        async move {
            let started = Instant::now();
            let limit = tokio::time::Instant::now() + endpoint.timeout();
            let until = until.map_or(limit, |until| until.min(limit));
            let Ok(Ok(permit)) = tokio::time::timeout_at(until, account.acquire_owned()).await
            else {
                let others = connecting.lock().workers.iter().any(|w| !w.tx.is_closed());
                drop(connecting);
                return if others {
                    Ok(None)
                } else {
                    Err(Failure::Request(Error::Unavailable))
                };
            };
            let mut conn = match Connection::connect_staged_until(&endpoint, until).await {
                Ok(conn) => conn,
                Err((stage, error)) => {
                    stats.connect_failed();
                    let mut state = connecting.lock();
                    let live = state.workers.iter().filter(|w| !w.tx.is_closed()).count();
                    // Excludes this attempt, whose slot `connecting` still holds.
                    let others = live + state.connecting - 1;
                    let now = Instant::now();
                    let (host, port) = (endpoint.host(), endpoint.port());
                    let reason = error.connect_problem();
                    let stage_label = stage.label();
                    let result = if others == 0 {
                        // Warn once per outage; retries while it lasts are debug.
                        if state.failed_at.replace(now).is_none() {
                            tracing::warn!(
                                host,
                                port,
                                stage = stage_label,
                                reason,
                                "cannot connect to provider"
                            );
                        } else {
                            tracing::debug!(
                                host,
                                port,
                                stage = stage_label,
                                reason,
                                "provider connect retry failed"
                            );
                        }
                        state.connect_error = Some(reason.into());
                        Err(Failure::Connect(error))
                    } else {
                        if stage.reached() {
                            // Turned away after accepting: likely the provider's connection
                            // limit, which the connections we hold do not exceed.
                            let limit = live.max(1);
                            if state
                                .throttle
                                .replace((limit, now))
                                .is_none_or(|(old, _)| old != limit)
                            {
                                tracing::warn!(
                                    host,
                                    port,
                                    connections = limit,
                                    stage = stage_label,
                                    reason,
                                    "provider refused more connections; limiting pool"
                                );
                            } else {
                                tracing::debug!(
                                    host,
                                    port,
                                    connections = limit,
                                    stage = stage_label,
                                    reason,
                                    "provider still refuses more connections"
                                );
                            }
                        } else if state.back_off(started, now) {
                            tracing::warn!(
                                host,
                                port,
                                connections = live,
                                stage = stage_label,
                                reason,
                                "cannot open more connections to provider; retrying later"
                            );
                        } else {
                            tracing::debug!(
                                host,
                                port,
                                connections = live,
                                stage = stage_label,
                                reason,
                                "provider connect retry failed"
                            );
                        }
                        // A hung handshake is a provider fault the breaker must hear of.
                        match error {
                            Error::Timeout if stage.reached() => Err(Failure::Request(error)),
                            _ => Ok(None),
                        }
                    };
                    drop(state);
                    drop(connecting);
                    return result;
                }
            };
            conn.hold(permit);
            let times = conn.connect_times();
            let (tx, rx) = mpsc::channel(config.pipeline_depth);
            let load = Arc::new(AtomicUsize::new(0));
            let acquired = Load::watched(&load, &connecting.changed);
            let link = Arc::clone(conn.link());
            let id = link.id;
            let task = tokio::spawn(run(
                conn,
                rx,
                config,
                Live::acquire(&live),
                usage,
                endpoint.host().to_owned(),
            ));
            let mut state = connecting.lock();
            if let Some(backoff) = state.backoff.take() {
                tracing::info!(
                    host = endpoint.host(),
                    port = endpoint.port(),
                    failures = backoff.failures,
                    "provider connects work again"
                );
            }
            state.failed_at = None;
            state.connect_error = None;
            state.connected = true;
            // Retired meanwhile: it serves the request that opened it, then closes.
            if !state.retired {
                state.workers.push(Worker {
                    lane: AtomicU64::new(lane.map_or(0, |lane| lane.0)),
                    cached: AtomicBool::new(cached),
                    tx: tx.clone(),
                    load,
                    task: task.abort_handle(),
                    link,
                });
            }
            // A throttle set while these connects were pending must not hold below them.
            let open = state.workers.iter().filter(|w| !w.tx.is_closed()).count();
            if state.throttle.is_some() && open > state.limit(config.max_connections) {
                state.throttle = Some((open, Instant::now()));
            }
            drop(state);
            drop(connecting);
            Ok(Some((tx, acquired, id, times)))
        }
    }

    /// Reserves nothing itself: the caller already counted the slot in `connecting`.
    fn connecting(&self) -> Connecting {
        Connecting {
            state: Arc::clone(&self.state),
            changed: Arc::clone(&self.changed),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Drives every future to completion concurrently, discarding outputs.
pub(crate) async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) {
    let mut pending: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    poll_fn(|cx| {
        pending.retain_mut(|future| future.as_mut().poll(cx).is_pending());
        if pending.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}

/// `None` when the connection went away before this request was answered.
async fn dispatch(tx: &mpsc::Sender<Request>, mut request: Request) -> Option<Reply> {
    let (reply, response) = oneshot::channel();
    request.reply = Some(reply);
    tx.send(request).await.ok()?;
    response.await.ok()
}

/// Requests of one ranked workload in [`Pool::fetch`]; the bound on work below them
/// lingers [`STREAM_LINGER`] after the last.
#[derive(Default)]
struct Presence {
    requests: usize,
    until: Option<Instant>,
}

impl Presence {
    fn lingering(&self, now: Instant) -> Option<Instant> {
        self.until.filter(|&until| now < until)
    }

    fn active(&self, now: Instant) -> bool {
        self.requests > 0 || self.lingering(now).is_some()
    }

    const fn leave(&mut self, until: Instant) {
        self.requests -= 1;
        self.until = Some(until);
    }
}

/// Counts one playback (or cache) request in its [`Presence`].
struct Present {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    cache: bool,
}

impl Present {
    fn start(pool: &Pool, cache: bool) -> Self {
        pool.lock().presence(cache).requests += 1;
        Self {
            state: Arc::clone(&pool.state),
            changed: Arc::clone(&pool.changed),
            cache,
        }
    }
}

impl Drop for Present {
    fn drop(&mut self) {
        let until = Instant::now() + STREAM_LINGER;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .presence(self.cache)
            .leave(until);
        // Work below parked while this request was present; from now on it waits out the
        // linger on a timer, but only if it wakes to see it.
        self.changed.notify_waiters();
    }
}

/// Releases a reserved connection slot, also when `connect` fails or is cancelled.
struct Connecting {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

impl Connecting {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Connecting {
    fn drop(&mut self) {
        self.lock().connecting -= 1;
        self.changed.notify_waiters();
    }
}

/// One running connection task; counted until it exits or retires itself.
struct Live(Option<Arc<AtomicUsize>>);

impl Live {
    fn acquire(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::Relaxed);
        Self(Some(Arc::clone(live)))
    }

    /// Leaves the count when more than `floor` remain; `false` keeps this one alive.
    fn retire(&mut self, floor: usize) -> bool {
        let Some(live) = &self.0 else { return true };
        let retired = live
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n > floor).then(|| n - 1)
            })
            .is_ok();
        if retired {
            self.0 = None;
        }
        retired
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(live) = &self.0 {
            live.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Owns one connection: keeps up to `pipeline_depth` requests in flight and answers
/// them in order. Exits on idle timeout above the warm floor, pool drop or a
/// connection-level error; any requests still queued are dropped, which callers see
/// as unanswered and retry.
async fn run(
    mut conn: Connection,
    mut rx: mpsc::Receiver<Request>,
    config: PoolConfig,
    mut live: Live,
    usage: Option<Arc<Usage>>,
    host: String,
) {
    let mut in_flight = VecDeque::with_capacity(config.pipeline_depth);
    loop {
        if in_flight.is_empty() {
            conn.link().set(Phase::Idle);
            match timeout(config.idle_timeout, rx.recv()).await {
                Ok(Some(request)) => enqueue(&mut conn, &mut in_flight, request),
                // Retired or dropped: the account permit stays held until the provider
                // has let the connection go.
                Ok(None) => {
                    conn.quit().await;
                    return;
                }
                Err(_)
                    if live.retire(if usage.as_ref().is_some_and(|u| u.exhausted()) {
                        0
                    } else {
                        config.min_connections
                    }) =>
                {
                    drop(rx);
                    conn.quit().await;
                    return;
                }
                Err(_) => {
                    if conn.keepalive().await.is_err() {
                        return;
                    }
                    continue;
                }
            }
        }
        while in_flight.len() < config.pipeline_depth {
            match rx.try_recv() {
                Ok(request) => enqueue(&mut conn, &mut in_flight, request),
                Err(_) => break,
            }
        }
        if let Err(error) = conn.flush().await {
            let head = in_flight.pop_front();
            dropped(&host, &error, in_flight.len() + rx.len());
            if let Some(request) = head {
                request.answer(Err(error), Duration::ZERO);
            }
            return;
        }
        let written = Instant::now();
        for request in &mut in_flight {
            if let Some(trace) = &mut request.trace {
                trace.written(written);
            }
        }
        let Some(mut request) = in_flight.pop_front() else {
            continue;
        };
        let head = Instant::now();
        let mut first_byte = None;
        let mut read_end = None;
        let provider = request.tag.provider;
        request.tag.step(Step::Reading { provider });
        let abandoned = request
            .reply
            .as_ref()
            .is_none_or(oneshot::Sender::is_closed);
        conn.link().set(if abandoned {
            Phase::Draining
        } else {
            Phase::Reply
        });
        let result = match request.command {
            Command::Body => {
                let read = conn
                    .read_body_with(request.tag.progress.as_ref(), request.tag.expected, || {
                        first_byte = Some(Instant::now());
                        request.tag.step(Step::Receiving { provider });
                    })
                    .await;
                read_end = Some(Instant::now());
                read.and_then(|body| {
                    request.tag.expected.validate(&body)?;
                    Ok(Some(body))
                })
            }
            Command::Stat => conn.read_stat().await.map(|()| None),
        };
        let timing = match request.command {
            Command::Body => conn.timing(),
            Command::Stat => Timing::default(),
        };
        let reusable = result
            .as_ref()
            .map_or_else(Error::is_connection_reusable, |_| true);
        if let Err(error) = &result
            && !reusable
        {
            dropped(&host, error, in_flight.len() + rx.len());
        }
        if let (Some(trace), Some(first_byte), Some(read_end)) =
            (&mut request.trace, first_byte, read_end)
        {
            trace.replied(ReplyTimes {
                head,
                first_byte,
                read_end,
                end: Instant::now(),
                decode: timing.decode,
            });
        }
        // A dropped receiver means the caller was cancelled; the reply is drained anyway.
        request.answer(result, timing.decode);
        if !reusable {
            return;
        }
    }
}

/// Expected now and then (provider resets, stalls); the requests are re-dispatched.
fn dropped(host: &str, error: &Error, requeued: usize) {
    tracing::debug!(host, reason = %error, requeued, "provider connection dropped");
}

fn enqueue(conn: &mut Connection, in_flight: &mut VecDeque<Request>, request: Request) {
    // The caller left while it was queued: sending it would only delay the requests
    // behind it. Dropping records it cancelled.
    if request
        .reply
        .as_ref()
        .is_none_or(oneshot::Sender::is_closed)
    {
        return;
    }
    let sent = match request.command {
        Command::Body => conn.send_body(&request.message_id),
        Command::Stat => conn.send_stat(&request.message_id),
    };
    match sent {
        Ok(()) => in_flight.push_back(request),
        Err(error) => request.answer(Err(error), Duration::ZERO),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn playback_load_counts_admission_waits_and_releases_cancelled_waiters() {
        let endpoint = Endpoint::new(
            "127.0.0.1",
            1,
            crate::vendor::nntp::TlsMode::Plain,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let downloads = Downloads::new(1);
        let held = downloads.acquire(true, 1).await.unwrap();
        let pool = Pool::new(
            endpoint,
            PoolConfig {
                max_connections: 4,
                pipeline_depth: 2,
                idle_timeout: Duration::from_secs(60),
                min_connections: 0,
            },
            Arc::clone(&downloads),
        )
        .unwrap();
        let mut request = Box::pin(pool.body("poc@loopback.invalid"));
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert_eq!(pool.busy(), 0);
        assert_eq!(pool.busy_permille(false), 125);
        // Playback ranks this pool against its current capacity, rather than the
        // configured ceiling the server has just refused.
        pool.lock().throttle = Some((2, Instant::now()));
        assert_eq!(pool.busy_permille(false), 250);
        drop(request);
        assert_eq!(pool.busy_permille(false), 0);
        assert_eq!(pool.lock().connecting, 0);
        assert_eq!(downloads.available_permits(), 0);
        drop(held);
        assert_eq!(downloads.available_permits(), 1);
    }

    #[tokio::test]
    async fn expired_admission_reuses_an_idle_connection_but_never_opens_one() {
        let server = crate::vendor::testkit::LoopbackServer::start(Vec::new())
            .await
            .unwrap();
        let endpoint = Endpoint::new(
            "127.0.0.1",
            server.addr().port(),
            crate::vendor::nntp::TlsMode::Plain,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let config = PoolConfig {
            max_connections: 2,
            pipeline_depth: 2,
            idle_timeout: Duration::from_secs(60),
            min_connections: 0,
        };
        let pool = Pool::new(endpoint, config, Downloads::new(4)).unwrap();
        let deadline = Some(tokio::time::Instant::now());
        assert!(matches!(
            pool.worker(
                Workload::Stream,
                false,
                deadline,
                None,
                &mut Flight::default()
            )
            .await,
            Err(Failure::Request(Error::Unavailable))
        ));
        assert_eq!(server.peak_connections(), 0);
        assert_eq!(pool.lock().connecting, 0);

        let Ok(Some((_, load, _, connection, _))) = pool
            .worker(Workload::Stream, false, None, None, &mut Flight::default())
            .await
        else {
            panic!("a connection opens before admission expires");
        };
        drop(load);
        let Ok(Some((_, load, _, reused, _))) = pool
            .worker(
                Workload::Stream,
                false,
                deadline,
                None,
                &mut Flight::default(),
            )
            .await
        else {
            panic!("an idle connection is reusable after admission expires");
        };
        assert_eq!(reused, connection);
        assert_eq!(server.peak_connections(), 1);
        drop(load);
        pool.abort();
    }

    struct ReleaseWatch {
        ports: (u16, u16),
        open_at_release: OnceLock<bool>,
    }

    impl std::task::Wake for ReleaseWatch {
        fn wake(self: Arc<Self>) {
            let _ = self.open_at_release.set(established(self.ports));
        }
    }

    fn established((local, remote): (u16, u16)) -> bool {
        let table = std::fs::read_to_string("/proc/self/net/tcp").unwrap();
        table.lines().skip(1).any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields[1].ends_with(&format!(":{local:04X}"))
                && fields[2].ends_with(&format!(":{remote:04X}"))
                && fields[3] == "01"
        })
    }

    struct Scripted {
        pool: Pool,
        account: Arc<Semaphore>,
        server: tokio::io::BufReader<tokio::net::TcpStream>,
        ports: (u16, u16),
    }

    async fn scripted(idle_timeout: Duration) -> Scripted {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = Endpoint::new(
            "127.0.0.1",
            port,
            crate::vendor::nntp::TlsMode::Plain,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let config = PoolConfig {
            max_connections: 1,
            pipeline_depth: 1,
            idle_timeout,
            min_connections: 0,
        };
        let account = Arc::new(Semaphore::new(1));
        let pool = Pool::new(endpoint, config, Downloads::new(4))
            .unwrap()
            .with_account(Arc::clone(&account));
        let accept = async {
            let (mut stream, peer) = listener.accept().await.unwrap();
            stream.write_all(b"200 ready\r\n").await.unwrap();
            (stream, peer.port())
        };
        let mut flight = Flight::default();
        let ((stream, client), opened) = tokio::join!(
            accept,
            pool.worker(Workload::Stream, false, None, None, &mut flight)
        );
        let Ok(Some((_, load, _, _, _))) = opened else {
            panic!("the connection opens");
        };
        drop(load);
        Scripted {
            pool,
            account,
            server: tokio::io::BufReader::new(stream),
            ports: (client, port),
        }
    }

    #[tokio::test]
    async fn an_account_permit_outlives_a_socket_closed_without_quit() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        for broken in [false, true] {
            let Scripted {
                pool,
                account,
                mut server,
                ports,
            } = scripted(Duration::from_secs(60)).await;
            let watch = Arc::new(ReleaseWatch {
                ports,
                open_at_release: OnceLock::new(),
            });
            let waker = std::task::Waker::from(Arc::clone(&watch));
            let mut waiter = Box::pin(Arc::clone(&account).acquire_owned());
            assert!(
                waiter
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(&waker))
                    .is_pending()
            );
            let request = broken.then(|| {
                let pool = pool.clone();
                tokio::spawn(async move { pool.body("gone@test").await.map(|_| ()) })
            });
            if broken {
                let mut line = String::new();
                server.read_line(&mut line).await.unwrap();
                server.get_mut().write_all(b"400 bye\r\n").await.unwrap();
            } else {
                pool.abort();
            }
            timeout(Duration::from_secs(5), async {
                while watch.open_at_release.get().is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the permit comes back");
            assert_eq!(
                watch.open_at_release.get(),
                Some(&false),
                "broken={broken}: the permit was released while the socket was open"
            );
            drop(waiter);
            if let Some(request) = request {
                request.abort();
            }
            pool.abort();
        }
    }

    #[tokio::test]
    async fn an_account_permit_is_held_until_the_provider_closes_a_retired_or_idle_connection() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        for (idle_timeout, retire) in [
            (Duration::from_secs(60), true),
            (Duration::from_millis(20), false),
        ] {
            let mut scripted = scripted(idle_timeout).await;
            if retire {
                scripted.pool.retire();
            }
            let mut line = String::new();
            timeout(Duration::from_secs(5), scripted.server.read_line(&mut line))
                .await
                .expect("the connection says goodbye")
                .unwrap();
            assert_eq!(line.trim(), "QUIT", "retire={retire}");
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert_eq!(scripted.account.available_permits(), 0, "retire={retire}");
            scripted
                .server
                .get_mut()
                .write_all(b"205 bye\r\n")
                .await
                .unwrap();
            drop(scripted.server);
            timeout(Duration::from_secs(5), async {
                while scripted.account.available_permits() == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the permit comes back once the provider closed");
        }
    }

    async fn loopback_pool(
        max_connections: usize,
    ) -> (Pool, crate::vendor::testkit::LoopbackServer) {
        let server = crate::vendor::testkit::LoopbackServer::start(Vec::new())
            .await
            .unwrap();
        let endpoint = Endpoint::new(
            "127.0.0.1",
            server.addr().port(),
            crate::vendor::nntp::TlsMode::Plain,
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let config = PoolConfig {
            max_connections,
            pipeline_depth: 2,
            idle_timeout: Duration::from_secs(60),
            min_connections: 0,
        };
        let pool = Pool::new(endpoint, config, Downloads::new(16)).unwrap();
        (pool, server)
    }

    async fn pending<F: Future + Unpin>(future: &mut F) {
        assert!(
            poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx)))
                .await
                .is_pending()
        );
    }

    #[tokio::test]
    async fn work_below_playback_and_cache_resumes_once_both_leave() {
        let (pool, _server) = loopback_pool(4).await;
        let stream = Present::start(&pool, false);
        let cache = Present::start(&pool, true);
        let mut flight = Flight::default();
        let mut queued = Box::pin(pool.worker(Workload::Queue, false, None, None, &mut flight));
        pending(&mut queued).await;
        drop(stream);
        drop(cache);
        let resumed = timeout(STREAM_LINGER * 3, queued).await;
        assert!(
            matches!(resumed, Ok(Ok(_))),
            "background work stayed parked"
        );
        pool.abort();
    }

    #[tokio::test]
    async fn background_work_resumes_when_a_parked_playback_request_gives_up() {
        let (pool, _server) = loopback_pool(2).await;
        let mut held = Vec::new();
        for _ in 0..2 {
            let Ok(Some((_, load, _, _, _))) = pool
                .worker(Workload::Queue, false, None, None, &mut Flight::default())
                .await
            else {
                panic!("a connection opens");
            };
            held.push(load);
        }
        let mut playback = Box::pin(pool.body("parked@loopback.invalid"));
        pending(&mut playback).await;
        assert_eq!(pool.stream_parked.load(Ordering::Relaxed), 1);
        let mut queued = Box::pin(below_viewers(&pool));
        pending(&mut queued).await;
        drop(playback);
        let resumed = timeout(Duration::from_secs(1), queued).await;
        assert!(
            matches!(resumed, Ok(Ok(_))),
            "background work stayed parked"
        );
        drop(held);
        pool.abort();
    }

    async fn below_viewers(pool: &Pool) -> Result<Option<Picked>, Failure> {
        loop {
            pool.downloads.viewers().clear().await;
            match pool
                .worker(Workload::Queue, false, None, None, &mut Flight::default())
                .await
            {
                Ok(None) => {}
                picked => return picked,
            }
        }
    }

    async fn saturate(pool: &Pool, workload: Workload, lane: Option<Lane>, n: usize) -> Vec<Load> {
        let mut loads = Vec::new();
        for _ in 0..n {
            let Ok(Some((_, load, _, _, _))) = pool
                .worker(workload, false, None, lane, &mut Flight::default())
                .await
            else {
                panic!("a connection is available");
            };
            loads.push(load);
        }
        loads
    }

    #[tokio::test]
    async fn from_sixteen_connections_work_below_cache_leaves_a_second_idle_slot() {
        for (limit, workload, opened) in [
            (15, Workload::Queue, 14),
            (16, Workload::Queue, 14),
            (16, Workload::Cache, 14),
            (24, Workload::Queue, 22),
        ] {
            let (pool, _server) = loopback_pool(limit).await;
            let requests = if workload == Workload::Cache {
                opened
            } else {
                limit + 2
            };
            let held = saturate(&pool, workload, None, requests).await;
            assert_eq!(pool.lock().workers.len(), opened, "{limit} {workload:?}");
            if workload == Workload::Queue && limit >= CACHE_HEADROOM_MIN_CONNECTIONS {
                let lane = Some(Lane::reader(1));
                let Ok(Some((_, cache, ahead, _, _))) = pool
                    .worker(Workload::Cache, false, None, lane, &mut Flight::default())
                    .await
                else {
                    panic!("the cache reader gets the slot left for it");
                };
                assert_eq!(ahead, 0, "its first read waits behind nothing");
                drop(cache);
            }
            drop(held);
            pool.abort();
        }
    }

    #[tokio::test]
    async fn work_below_cache_yields_while_a_cache_request_waits() {
        let (pool, _server) = loopback_pool(16).await;
        let lane = Lane::reader(1);
        // Two download connections sit under the share a cache reader leaves them, with
        // pipeline room, so only the waiting cache request holds them back.
        let queue = saturate(&pool, Workload::Queue, None, 2).await;
        let cache = saturate(&pool, Workload::Cache, Some(lane), 12).await;
        let tag = Tag {
            workload: Workload::Cache,
            lane: Some(lane),
            ..Tag::UNTRACKED
        };
        let mut waiting = Box::pin(pool.fetch(Command::Body, "waiting@loopback.invalid", &tag));
        pending(&mut waiting).await;
        assert_eq!(pool.cache_parked.load(Ordering::Relaxed), 1);
        let mut below = Box::pin(below_viewers(&pool));
        pending(&mut below).await;
        drop(waiting);
        drop(cache);
        let resumed = timeout(STREAM_LINGER * 3, below).await;
        assert!(matches!(resumed, Ok(Ok(_))), "work below cache resumes");
        drop(queue);
        pool.abort();
    }

    #[test]
    fn slow_failing_retries_keep_doubling_the_backoff() {
        let start = Instant::now();
        let mut state = State::default();
        assert!(state.back_off(start, start));
        // Each retry starts as the hold ends and takes 6 s to fail.
        let mut retry = start + CONNECT_BACKOFF;
        for _ in 0..6 {
            assert!(!state.back_off(retry, retry + Duration::from_secs(6)));
            retry = state.backoff.map_or(retry, |b| b.until);
        }
        let backoff = state.backoff.map(|b| (b.delay, b.failures));
        assert_eq!(backoff, Some((MAX_CONNECT_BACKOFF, 7)));
    }
}
