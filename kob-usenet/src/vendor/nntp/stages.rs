//! Where playback and cache-ahead requests spend their time, cheap enough to keep on in
//! production. A delivered request's additive stages partition its `total` exactly;
//! `connect`, `tls` and `auth` are nested inside `connection_wait`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::vendor::nntp::stats::Workload;

pub const STAGE_BUCKETS: [f64; 17] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.35, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0,
];
const STAGE_BOUNDS_NS: [u64; 17] = [
    10_000_000,
    20_000_000,
    50_000_000,
    100_000_000,
    200_000_000,
    350_000_000,
    500_000_000,
    750_000_000,
    1_000_000_000,
    1_500_000_000,
    2_000_000_000,
    3_000_000_000,
    4_000_000_000,
    6_000_000_000,
    8_000_000_000,
    12_000_000_000,
    16_000_000_000,
];
pub const LOAD_BUCKETS: [u64; 12] = [0, 1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024];

pub const TIMED_WORKLOADS: [Workload; 2] = [Workload::Stream, Workload::Cache];

const fn slot(workload: Workload) -> Option<usize> {
    match workload {
        Workload::Stream => Some(0),
        Workload::Cache => Some(1),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStage {
    Retry,
    Admission,
    ConnectionWait,
    Connect,
    Tls,
    Auth,
    SendQueue,
    PipelineWait,
    ProviderFirstByte,
    BodyTransfer,
    Decode,
    Total,
    Service,
    ServiceFirstRead,
}

impl StreamStage {
    pub const ALL: [Self; 14] = [
        Self::Retry,
        Self::Admission,
        Self::ConnectionWait,
        Self::Connect,
        Self::Tls,
        Self::Auth,
        Self::SendQueue,
        Self::PipelineWait,
        Self::ProviderFirstByte,
        Self::BodyTransfer,
        Self::Decode,
        Self::Total,
        Self::Service,
        Self::ServiceFirstRead,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Admission => "admission",
            Self::ConnectionWait => "connection_wait",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Auth => "auth",
            Self::SendQueue => "send_queue",
            Self::PipelineWait => "pipeline_wait",
            Self::ProviderFirstByte => "provider_first_byte",
            Self::BodyTransfer => "body_transfer",
            Self::Decode => "decode",
            Self::Total => "total",
            Self::Service => "service",
            Self::ServiceFirstRead => "service_first_read",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionRoute {
    Idle,
    Busy,
    New,
}

impl ConnectionRoute {
    pub const ALL: [Self; 3] = [Self::Idle, Self::Busy, Self::New];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::New => "new",
        }
    }
}

/// Whether the connection a request took had moved body data within the last second, so
/// its congestion window is still open on the provider's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPath {
    Warm,
    Cold,
}

impl ConnectionPath {
    pub const ALL: [Self; 2] = [Self::Warm, Self::Cold];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignLoad {
    RequestsAhead,
    BusyConnections,
    RequestsInFlight,
    PaceInFlightMib,
    PaceBoundMib,
}

impl AssignLoad {
    pub const ALL: [Self; 5] = [
        Self::RequestsAhead,
        Self::BusyConnections,
        Self::RequestsInFlight,
        Self::PaceInFlightMib,
        Self::PaceBoundMib,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::RequestsAhead => "requests_ahead",
            Self::BusyConnections => "busy_connections",
            Self::RequestsInFlight => "requests_in_flight",
            Self::PaceInFlightMib => "pace_in_flight_mib",
            Self::PaceBoundMib => "pace_bound_mib",
        }
    }
}

struct Histogram<const N: usize> {
    buckets: [AtomicU64; N],
    sum: AtomicU64,
}

impl<const N: usize> Default for Histogram<N> {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum: AtomicU64::new(0),
        }
    }
}

impl<const N: usize> Histogram<N> {
    fn observe(&self, bounds: &[u64], value: u64) {
        let bucket = bounds.partition_point(|&bound| bound < value);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
    }

    fn snapshot(&self) -> HistogramSnapshot {
        let mut count = 0;
        let cumulative = self
            .buckets
            .iter()
            .map(|bucket| {
                count += bucket.load(Ordering::Relaxed);
                count
            })
            .collect();
        HistogramSnapshot {
            cumulative,
            sum: self.sum.load(Ordering::Relaxed),
        }
    }
}

/// Cumulative counts per bucket bound, `+Inf` last. `sum` is in the observed unit.
#[derive(Clone, Debug, Default)]
pub struct HistogramSnapshot {
    pub cumulative: Vec<u64>,
    pub sum: u64,
}

impl HistogramSnapshot {
    #[must_use]
    pub fn count(&self) -> u64 {
        self.cumulative.last().copied().unwrap_or(0)
    }

    #[must_use]
    pub fn sum_seconds(&self) -> f64 {
        self.sum as f64 / 1e9
    }
}

#[derive(Default)]
struct WorkloadStages {
    stages: [Histogram<18>; StreamStage::ALL.len()],
    routes: [AtomicU64; ConnectionRoute::ALL.len()],
    paths: [AtomicU64; ConnectionPath::ALL.len()],
    load: [Histogram<13>; AssignLoad::ALL.len()],
    body_bytes: AtomicU64,
    abandoned: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct WorkloadStageSnapshot {
    pub workload: Workload,
    pub stages: Vec<(StreamStage, HistogramSnapshot)>,
    pub routes: Vec<(ConnectionRoute, u64)>,
    pub paths: Vec<(ConnectionPath, u64)>,
    pub load: Vec<(AssignLoad, HistogramSnapshot)>,
    pub body_bytes: u64,
    /// Assigned a connection but never delivered: cancelled by a seek, or failed.
    pub abandoned: u64,
}

impl WorkloadStageSnapshot {
    #[must_use]
    pub fn stage(&self, stage: StreamStage) -> &HistogramSnapshot {
        &self.stages[stage as usize].1
    }

    #[must_use]
    pub fn route(&self, route: ConnectionRoute) -> u64 {
        self.routes[route as usize].1
    }

    #[must_use]
    pub fn path(&self, path: ConnectionPath) -> u64 {
        self.paths[path as usize].1
    }

    #[must_use]
    pub fn load(&self, load: AssignLoad) -> &HistogramSnapshot {
        &self.load[load as usize].1
    }
}

#[derive(Default)]
pub struct StageMetrics {
    workloads: [WorkloadStages; TIMED_WORKLOADS.len()],
    trace: AtomicBool,
}

impl StageMetrics {
    /// Logs one `info` line per playback request while on.
    pub fn set_trace(&self, on: bool) {
        self.trace.store(on, Ordering::Relaxed);
    }

    #[must_use]
    pub fn tracing(&self) -> bool {
        self.trace.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<WorkloadStageSnapshot> {
        TIMED_WORKLOADS
            .iter()
            .zip(&self.workloads)
            .map(|(&workload, stages)| WorkloadStageSnapshot {
                workload,
                stages: StreamStage::ALL
                    .iter()
                    .map(|&stage| (stage, stages.stages[stage as usize].snapshot()))
                    .collect(),
                routes: ConnectionRoute::ALL
                    .iter()
                    .map(|&route| (route, stages.routes[route as usize].load(Ordering::Relaxed)))
                    .collect(),
                paths: ConnectionPath::ALL
                    .iter()
                    .map(|&path| (path, stages.paths[path as usize].load(Ordering::Relaxed)))
                    .collect(),
                load: AssignLoad::ALL
                    .iter()
                    .map(|&load| (load, stages.load[load as usize].snapshot()))
                    .collect(),
                body_bytes: stages.body_bytes.load(Ordering::Relaxed),
                abandoned: stages.abandoned.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// One HTTP read: `first_read` is when its first bytes came back from the engine,
    /// `ready` when the response carrying them was handed over.
    pub fn record_service(&self, workload: Workload, first_read: Duration, ready: Duration) {
        let Some(stages) = slot(workload).map(|slot| &self.workloads[slot]) else {
            return;
        };
        stages.observe(StreamStage::ServiceFirstRead, first_read);
        stages.observe(StreamStage::Service, ready);
        if workload == Workload::Stream && self.tracing() {
            tracing::info!(
                target: TRACE_TARGET,
                first_read_ms = first_read.as_millis() as u64,
                service_ms = ready.as_millis() as u64,
                "stream service"
            );
        }
    }
}

pub const TRACE_TARGET: &str = "usenet_stream_trace";

impl WorkloadStages {
    fn observe(&self, stage: StreamStage, took: Duration) {
        let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        self.stages[stage as usize].observe(&STAGE_BOUNDS_NS, nanos);
    }
}

/// When one request entered the client, carried across its attempts.
#[derive(Clone)]
pub(crate) struct StageClock {
    metrics: Arc<StageMetrics>,
    entered: Instant,
    resumed: Option<Instant>,
}

impl StageClock {
    pub(crate) fn start(metrics: &Arc<StageMetrics>) -> Self {
        Self {
            metrics: Arc::clone(metrics),
            entered: Instant::now(),
            resumed: None,
        }
    }

    /// The last attempt failed; time until the next one starts counts as `retry`.
    pub(crate) fn resume(&mut self) {
        self.resumed = Some(Instant::now());
    }

    pub(crate) fn attempt_from(&self) -> Instant {
        self.resumed.unwrap_or(self.entered)
    }
}

/// What the pool looked like when a playback request took its connection.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LoadAtAssign {
    pub(crate) busy_connections: usize,
    pub(crate) requests_in_flight: usize,
    pub(crate) pace_in_flight: u64,
    pub(crate) pace_bound: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ConnectTimes {
    pub(crate) connect: Duration,
    pub(crate) tls: Duration,
    pub(crate) auth: Duration,
}

pub(crate) struct Assigned {
    pub(crate) attempt_from: Instant,
    pub(crate) permitted: Instant,
    pub(crate) ahead: usize,
    pub(crate) connection: u32,
    pub(crate) provider: u16,
    pub(crate) opened: Option<ConnectTimes>,
    pub(crate) warm: bool,
    pub(crate) load: Option<LoadAtAssign>,
}

/// The reply's timestamps, read on the connection task.
#[derive(Clone, Copy)]
pub(crate) struct ReplyTimes {
    pub(crate) head: Instant,
    pub(crate) first_byte: Instant,
    pub(crate) read_end: Instant,
    pub(crate) end: Instant,
    pub(crate) decode: Duration,
}

/// One body attempt from the moment it holds a connection.
pub(crate) struct RequestTrace {
    clock: StageClock,
    workload: Workload,
    assigned_at: Instant,
    assigned: Assigned,
    route: ConnectionRoute,
    path: ConnectionPath,
    written: Option<Instant>,
    reply: Option<ReplyTimes>,
}

impl RequestTrace {
    pub(crate) fn assign(
        clock: &StageClock,
        workload: Workload,
        assigned: Assigned,
    ) -> Option<Box<Self>> {
        let stages = &clock.metrics.workloads[slot(workload)?];
        let route = match (assigned.opened, assigned.ahead) {
            (Some(_), _) => ConnectionRoute::New,
            (None, 0) => ConnectionRoute::Idle,
            (None, _) => ConnectionRoute::Busy,
        };
        stages.routes[route as usize].fetch_add(1, Ordering::Relaxed);
        let path = if assigned.warm {
            ConnectionPath::Warm
        } else {
            ConnectionPath::Cold
        };
        stages.paths[path as usize].fetch_add(1, Ordering::Relaxed);
        let load = |which: AssignLoad, value: u64| {
            stages.load[which as usize].observe(&LOAD_BUCKETS, value);
        };
        load(AssignLoad::RequestsAhead, assigned.ahead as u64);
        if let Some(at) = assigned.load {
            load(AssignLoad::BusyConnections, at.busy_connections as u64);
            load(AssignLoad::RequestsInFlight, at.requests_in_flight as u64);
            load(AssignLoad::PaceInFlightMib, at.pace_in_flight >> 20);
            load(AssignLoad::PaceBoundMib, at.pace_bound >> 20);
        }
        if let Some(opened) = assigned.opened {
            stages.observe(StreamStage::Connect, opened.connect);
            stages.observe(StreamStage::Tls, opened.tls);
            stages.observe(StreamStage::Auth, opened.auth);
        }
        Some(Box::new(Self {
            clock: clock.clone(),
            workload,
            assigned_at: Instant::now(),
            assigned,
            route,
            path,
            written: None,
            reply: None,
        }))
    }

    pub(crate) fn written(&mut self, at: Instant) {
        self.written.get_or_insert(at);
    }

    pub(crate) const fn replied(&mut self, reply: ReplyTimes) {
        self.reply = Some(reply);
    }

    pub(crate) fn abandoned(self) {
        if let Some(slot) = slot(self.workload) {
            self.clock.metrics.workloads[slot]
                .abandoned
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn delivered(self, bytes: u32) {
        let (Some(slot), Some(reply)) = (slot(self.workload), self.reply) else {
            return self.abandoned();
        };
        let metrics = &self.clock.metrics;
        let stages = &metrics.workloads[slot];
        let assigned = &self.assigned;
        let written = self.written.unwrap_or(reply.head);
        let read = reply.read_end.saturating_duration_since(reply.first_byte);
        let timings = [
            (
                StreamStage::Retry,
                assigned
                    .attempt_from
                    .saturating_duration_since(self.clock.entered),
            ),
            (
                StreamStage::Admission,
                assigned
                    .permitted
                    .saturating_duration_since(assigned.attempt_from),
            ),
            (
                StreamStage::ConnectionWait,
                self.assigned_at
                    .saturating_duration_since(assigned.permitted),
            ),
            (
                StreamStage::SendQueue,
                written.saturating_duration_since(self.assigned_at),
            ),
            (
                StreamStage::PipelineWait,
                reply.head.saturating_duration_since(written),
            ),
            (
                StreamStage::ProviderFirstByte,
                reply.first_byte.saturating_duration_since(reply.head),
            ),
            (StreamStage::BodyTransfer, read.saturating_sub(reply.decode)),
            (
                StreamStage::Decode,
                reply.decode + reply.end.saturating_duration_since(reply.read_end),
            ),
            (
                StreamStage::Total,
                reply.end.saturating_duration_since(self.clock.entered),
            ),
        ];
        for (stage, took) in timings {
            stages.observe(stage, took);
        }
        stages
            .body_bytes
            .fetch_add(u64::from(bytes), Ordering::Relaxed);
        if self.workload == Workload::Stream && metrics.tracing() {
            let ms = |stage: StreamStage| {
                timings
                    .iter()
                    .find(|(s, _)| *s == stage)
                    .map_or(0, |(_, took)| took.as_millis() as u64)
            };
            let load = assigned.load.unwrap_or_default();
            tracing::info!(
                target: TRACE_TARGET,
                route = self.route.label(),
                path = self.path.label(),
                provider = assigned.provider,
                conn = assigned.connection,
                ahead = assigned.ahead,
                busy_conns = load.busy_connections,
                in_flight = load.requests_in_flight,
                pace_in_flight = load.pace_in_flight,
                pace_bound = load.pace_bound,
                bytes,
                retry_ms = ms(StreamStage::Retry),
                admission_ms = ms(StreamStage::Admission),
                conn_wait_ms = ms(StreamStage::ConnectionWait),
                connect_ms = assigned.opened.map(|o| o.connect.as_millis() as u64),
                tls_ms = assigned.opened.map(|o| o.tls.as_millis() as u64),
                auth_ms = assigned.opened.map(|o| o.auth.as_millis() as u64),
                send_queue_ms = ms(StreamStage::SendQueue),
                pipeline_ms = ms(StreamStage::PipelineWait),
                first_byte_ms = ms(StreamStage::ProviderFirstByte),
                body_ms = ms(StreamStage::BodyTransfer),
                decode_ms = ms(StreamStage::Decode),
                total_ms = ms(StreamStage::Total),
                "stream stages"
            );
        }
    }
}
