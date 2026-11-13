//! Worker abstraction for processing jobs from NATS JetStream.

use crate::cancel::CancelRegistry;
use crate::client::{NatsClient, NatsError};
use crate::consumer::ensure_consumer;
use crate::db::JobStatusDb;
use crate::types::{Job, JobStatus};
use async_nats::jetstream::consumer::AckPolicy;
use async_nats::jetstream::consumer::pull::Config as ConsumerConfig;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Error type for job execution.
#[derive(Error, Debug)]
pub enum JobError {
    #[error("Job failed: {message}")]
    Failed {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
        retryable: bool,
    },
    #[error("Job cancelled")]
    Cancelled,
}

impl JobError {
    /// Create a non-retryable error.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            source: None,
            retryable: false,
        }
    }

    /// Create a retryable error.
    pub fn retryable(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            source: None,
            retryable: true,
        }
    }

    /// Create an error with a source.
    pub fn with_source<E: std::error::Error + Send + Sync + 'static>(
        message: impl Into<String>,
        source: E,
        retryable: bool,
    ) -> Self {
        Self::Failed {
            message: message.into(),
            source: Some(Box::new(source)),
            retryable,
        }
    }

    /// Check if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Failed { retryable, .. } => *retryable,
            Self::Cancelled => false,
        }
    }
}

/// Trait for job handlers.
///
/// Implement this trait for each job type you want to process.
#[async_trait]
pub trait JobHandler: Send + Sync {
    /// The job type this handler processes.
    fn job_type(&self) -> &'static str;

    /// Maximum concurrent jobs for this handler. Default is 3.
    fn max_concurrent(&self) -> usize {
        3
    }

    /// Execute the job.
    ///
    /// The `cancel` token is triggered when the job is cancelled externally
    /// (e.g. via a cancel API endpoint). Handlers should call
    /// [`check_cancelled`](crate::check_cancelled) at natural checkpoints.
    ///
    /// Returns a JSON result on success, or a JobError on failure.
    async fn execute(
        &self,
        job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError>;

    /// Optional delay after a successful job completion, before the NATS
    /// message is acked. Useful for throttling bursts (e.g. 30s cooldown
    /// between audiobook transcriptions). The payload is passed so handlers
    /// can decide per-invocation (e.g. skip delay for manual triggers).
    fn post_job_delay(&self, _payload: &Value) -> Option<Duration> {
        None
    }

    /// Optional per-handler ack_wait override. When set, this handler's NATS
    /// consumer uses this timeout instead of the WorkerConfig default.
    /// Useful for long-running jobs (e.g. transcription) that need more time
    /// before NATS considers the message unacknowledged.
    fn ack_wait(&self) -> Option<Duration> {
        None
    }

    /// Optional per-handler max_deliver override. When set, this handler's
    /// NATS consumer uses this limit instead of the WorkerConfig default.
    /// Useful for jobs that sit in the queue a long time (e.g. enrichment)
    /// where queue-pressure redeliveries shouldn't exhaust the retry budget.
    fn max_deliver(&self) -> Option<i64> {
        None
    }
}

/// Configuration for the job worker.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Service name (e.g., "kobbler", "kobload").
    pub service: String,
    /// Ack wait timeout (how long before message is redelivered).
    pub ack_wait: Duration,
    /// Maximum number of delivery attempts.
    pub max_deliver: i64,
}

impl WorkerConfig {
    /// Redelivery deadline for a message with no per-handler override. Short because
    /// a running job heartbeats a Progress ack at half this, so the deadline only
    /// ever fires on a worker that has actually stopped talking.
    pub const DEFAULT_ACK_WAIT: Duration = Duration::from_secs(60);
    /// Original + 5 retries (must be >= backoff.len() + 1).
    pub const DEFAULT_MAX_DELIVER: i64 = 6;

    /// Create a new worker config for a specific service.
    pub fn for_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
            ack_wait: Self::DEFAULT_ACK_WAIT,
            max_deliver: Self::DEFAULT_MAX_DELIVER,
        }
    }
}

/// Status change notification sent when a job transitions state.
#[derive(Debug, Clone)]
pub struct JobStatusChange {
    pub job_id: Uuid,
    pub job_type: String,
    pub status: JobStatus,
    pub progress: Option<i32>,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    pub description: Option<String>,
    pub display: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
    pub scheduled: bool,
}

/// Callback type for job status changes.
pub type StatusCallback = Arc<dyn Fn(JobStatusChange) + Send + Sync>;

/// Registered handler with its configuration.
struct RegisteredHandler {
    handler: Arc<dyn JobHandler>,
    job_type: String,
}

/// Job worker that processes jobs from NATS JetStream.
///
/// Creates one consumer per registered handler, allowing independent
/// concurrency control per job type.
pub struct JobWorker {
    nats: NatsClient,
    config: WorkerConfig,
    handlers: Vec<RegisteredHandler>,
    job_db: Option<JobStatusDb>,
    status_callback: Option<StatusCallback>,
    cancel_registry: CancelRegistry,
}

impl JobWorker {
    /// Create a new job worker.
    pub fn new(nats: NatsClient, config: WorkerConfig) -> Self {
        Self {
            nats,
            config,
            handlers: Vec::new(),
            job_db: None,
            status_callback: None,
            cancel_registry: CancelRegistry::new(),
        }
    }

    /// Use an externally-created cancel registry.
    ///
    /// Call this before `run()` if you need to share the registry with other
    /// components (e.g. API routes that cancel running jobs).
    pub fn with_cancel_registry(mut self, registry: CancelRegistry) -> Self {
        self.cancel_registry = registry;
        self
    }

    /// Get a handle to the cancel registry.
    ///
    /// Share this with API routes so they can cancel running jobs.
    pub fn cancel_registry(&self) -> CancelRegistry {
        self.cancel_registry.clone()
    }

    /// Set up local job status tracking.
    pub fn with_job_db(mut self, db: JobStatusDb) -> Self {
        self.job_db = Some(db);
        self
    }

    /// Set a callback that fires on every job status change (running, completed, failed, retrying).
    pub fn with_status_callback(mut self, cb: StatusCallback) -> Self {
        self.status_callback = Some(cb);
        self
    }

    /// Register a job handler.
    pub fn register_handler<H: JobHandler + 'static>(&mut self, handler: H) {
        let job_type = handler.job_type().to_string();
        info!(
            "Registering handler for job type: {} (max_concurrent: {})",
            job_type,
            handler.max_concurrent()
        );
        self.handlers.push(RegisteredHandler {
            handler: Arc::new(handler),
            job_type,
        });
    }

    /// Run the worker until shutdown is signaled.
    ///
    /// Creates one consumer per registered handler, each with its own
    /// concurrency limit based on the handler's `max_concurrent()`.
    ///
    /// Handles NATS reconnections automatically - when NATS reconnects,
    /// consumers are recreated to handle the case where ephemeral consumer
    /// state was lost on the NATS server.
    pub async fn run(self, shutdown_rx: watch::Receiver<bool>) -> Result<(), NatsError> {
        let mut tasks = Vec::new();

        for registered in self.handlers {
            let job_type = registered.job_type.clone();
            let handler = registered.handler.clone();
            let max_concurrent = handler.max_concurrent();

            // Consumer name: {service}-{job_type}
            let consumer_name = format!("{}-{}", self.config.service, job_type);
            // Filter subject: jobs.{service}.{job_type}
            let filter_subject = format!("jobs.{}.{}", self.config.service, job_type);

            // Per-handler ack_wait / max_deliver overrides, falling back to the global default.
            let ack_wait = handler.ack_wait().unwrap_or(self.config.ack_wait);
            let max_deliver = handler.max_deliver().unwrap_or(self.config.max_deliver);

            let consumer_config = ConsumerConfig {
                durable_name: Some(consumer_name.clone()),
                filter_subject: filter_subject.clone(),
                ack_policy: AckPolicy::Explicit,
                ack_wait,
                max_deliver,
                // Limit in-flight messages to max_concurrent. Without this,
                // the async-nats pull consumer pre-fetches extra messages that
                // sit in a buffer burning ack_wait without being processed,
                // causing spurious redeliveries and max_deliver exhaustion.
                max_ack_pending: max_concurrent as i64,
                // No backoff array — ack_wait controls the redelivery deadline.
                // NOTE: Setting backoff causes NATS to override ack_wait with
                // backoff[0], which led to stale consumer loops and premature
                // redelivery. The heartbeat mechanism (Progress acks at ack_wait/2)
                // keeps long-running jobs alive.
                ..Default::default()
            };

            let job_db = self.job_db.clone();
            let nats = self.nats.clone();
            let shutdown_rx = shutdown_rx.clone();
            let status_callback = self.status_callback.clone();
            let cancel_registry = self.cancel_registry.clone();

            info!(
                "Starting consumer '{}' on '{}' with {} concurrent jobs",
                consumer_name, filter_subject, max_concurrent
            );

            // Spawn a task for this handler's consumer with reconnection support
            let task = tokio::spawn(consumer_task(
                nats,
                handler,
                job_db,
                consumer_name,
                consumer_config,
                max_concurrent,
                shutdown_rx,
                status_callback,
                cancel_registry,
            ));

            tasks.push(task);
        }

        // Wait for all consumer tasks to complete
        for task in tasks {
            let _ = task.await;
        }

        Ok(())
    }
}

/// Per-handler consumer task with reconnection support.
///
/// This task maintains a consumer for a single job type and handles:
/// - Initial consumer creation
/// - Reconnection when NATS connection is restored
/// - Stream errors that require consumer recreation
#[allow(clippy::too_many_arguments)]
async fn consumer_task(
    nats: NatsClient,
    handler: Arc<dyn JobHandler>,
    job_db: Option<JobStatusDb>,
    consumer_name: String,
    consumer_config: ConsumerConfig,
    max_concurrent: usize,
    mut shutdown_rx: watch::Receiver<bool>,
    status_callback: Option<StatusCallback>,
    cancel_registry: CancelRegistry,
) {
    let job_type = handler.job_type();
    let mut reconnect_rx = nats.subscribe_reconnect();
    let mut backoff = Duration::from_secs(1);
    // Track the advisory subscription task so we can abort it on reconnect.
    let mut advisory_handle: Option<tokio::task::JoinHandle<()>> = None;

    'reconnect: loop {
        // Abort previous advisory subscription on reconnect — core NATS
        // subscriptions aren't durable, so we need a fresh one.
        if let Some(handle) = advisory_handle.take() {
            handle.abort();
        }

        // Check for shutdown before trying to connect
        if *shutdown_rx.borrow() {
            info!("Consumer {} shutting down before connect", job_type);
            return;
        }

        // Get fresh stream reference
        let mut stream = match nats.jetstream().get_stream("JOBS").await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Failed to get JOBS stream for {}: {}, retrying in {:?}",
                    job_type, e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        let consumer = match ensure_consumer(&mut stream, &consumer_name, &consumer_config).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Failed to create consumer '{}': {}, retrying in {:?}",
                    consumer_name, e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        // Get messages stream
        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    "Failed to get messages for '{}': {}, retrying in {:?}",
                    consumer_name, e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        info!(
            "Consumer '{}' ready, processing messages for {}",
            consumer_name, job_type
        );
        backoff = Duration::from_secs(1); // Reset backoff on successful setup

        // Subscribe to max-deliveries advisory for this consumer.
        // When NATS exhausts max_deliver, this fires and we mark the job as failed
        // so it doesn't stay stuck in pending/retrying forever.
        let advisory_subject = format!(
            "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.JOBS.{}",
            consumer_name
        );
        advisory_handle = match nats.subscribe(&advisory_subject).await {
            Ok(advisory_sub) => {
                let db = job_db.clone();
                let cb = status_callback.clone();
                let nats_clone = nats.clone();
                let mut shutdown = shutdown_rx.clone();
                Some(tokio::spawn(async move {
                    handle_max_delivery_advisories(advisory_sub, nats_clone, db, cb, &mut shutdown)
                        .await;
                }))
            }
            Err(e) => {
                warn!(
                    "Failed to subscribe to advisory for {}: {}, max-delivery events won't update DB",
                    consumer_name, e
                );
                None
            }
        };

        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Consumer {} shutting down", job_type);
                        return;
                    }
                }

                // Proactive: Reconnect signal from NatsClient
                _ = reconnect_rx.recv() => {
                    info!("Consumer {} received reconnect signal, recreating consumer", job_type);
                    continue 'reconnect;
                }

                msg_result = messages.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            debug!("Received message for {}", job_type);
                            let permit = match semaphore.clone().acquire_owned().await {
                                Ok(p) => p,
                                Err(_) => {
                                    error!("Semaphore closed for {}", job_type);
                                    return;
                                }
                            };
                            let handler = handler.clone();
                            let job_db = job_db.clone();
                            let status_callback = status_callback.clone();
                            let cancel_registry = cancel_registry.clone();

                            // When backoff is set, the server uses backoff[0] as the effective
                            // ack deadline for the first delivery, overriding ack_wait.
                            let ack_wait = consumer_config.backoff.first()
                                .copied()
                                .unwrap_or(consumer_config.ack_wait);
                            let max_deliver = consumer_config.max_deliver;
                            tokio::spawn(async move {
                                let _permit = permit;
                                process_message(msg, handler.as_ref(), job_db.as_ref(), status_callback.as_ref(), ack_wait, max_deliver, &cancel_registry).await;
                            });
                        }
                        Some(Err(e)) => {
                            // Fallback: Handle stream errors by reconnecting
                            error!(
                                "Stream error for consumer {}: {}, reconnecting",
                                job_type, e
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                            continue 'reconnect;
                        }
                        None => {
                            // Stream ended unexpectedly
                            warn!(
                                "Message stream ended for consumer {}, reconnecting",
                                job_type
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                            continue 'reconnect;
                        }
                    }
                }
            }
        }
    }
}

/// Advisory payload from NATS when a message exceeds max_deliver.
#[derive(Deserialize)]
struct MaxDeliveriesAdvisory {
    stream_seq: u64,
    deliveries: i64,
}

/// Listens for max-deliveries advisories and marks the affected jobs as failed.
///
/// When NATS exhausts all delivery attempts for a message, it publishes an
/// advisory but doesn't notify the application. Without this handler, the
/// job_status row stays pending/retrying forever, blocking re-enqueue.
async fn handle_max_delivery_advisories(
    mut sub: async_nats::Subscriber,
    nats: NatsClient,
    job_db: Option<JobStatusDb>,
    status_callback: Option<StatusCallback>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return;
                }
            }
            msg = sub.next() => {
                let Some(msg) = msg else { return };

                let advisory: MaxDeliveriesAdvisory = match serde_json::from_slice(&msg.payload) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!("Failed to parse max-deliveries advisory: {}", e);
                        continue;
                    }
                };

                // Fetch the original message and deserialize the full Job
                // so we have job_type, context, etc. for the status callback.
                let stream = match nats.jetstream().get_stream("JOBS").await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            "Failed to get JOBS stream for advisory (seq {}): {}",
                            advisory.stream_seq, e
                        );
                        continue;
                    }
                };

                let (job, subject) = match stream.direct_get(advisory.stream_seq).await {
                    Ok(stream_msg) => {
                        let subject = stream_msg.subject.to_string();
                        match serde_json::from_slice::<Job>(&stream_msg.payload) {
                            Ok(job) => (job, subject),
                            Err(e) => {
                                warn!(
                                    "Failed to deserialize job from stream seq {}: {}",
                                    advisory.stream_seq, e
                                );
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch message at seq {} for max-deliveries advisory: {} \
                             (ghost reconciler will catch it)",
                            advisory.stream_seq, e
                        );
                        continue;
                    }
                };

                // Check DB status: if the job is still pending, it was never
                // picked up — queue pressure caused the redeliveries, not handler
                // failures. Re-publish as a fresh message instead of failing.
                if let Some(ref db) = job_db
                    && let Ok(Some(JobStatus::Pending)) = db.get_job_status(job.id).await
                {
                    info!(
                            "Job {} ({}) hit max_deliver ({} deliveries) but DB status \
                             is still pending — re-publishing as fresh message",
                            job.id, job.job_type, advisory.deliveries
                        );
                        let bytes = match serde_json::to_vec(&job) {
                            Ok(b) => b,
                            Err(e) => {
                                warn!("Failed to serialize job {} for re-publish: {}", job.id, e);
                                continue;
                            }
                        };
                        // Use a new message ID so NATS dedup doesn't reject it.
                        let new_msg_id = format!("{}-re{}", job.id, advisory.deliveries);
                        let publish_msg = async_nats::jetstream::message::PublishMessage::build()
                            .payload(bytes.into())
                            .message_id(new_msg_id);
                        match nats.jetstream().send_publish(subject, publish_msg).await {
                            Ok(_) => {
                                debug!("Re-published job {} with fresh delivery count", job.id);
                            }
                            Err(e) => {
                                warn!("Failed to re-publish job {}: {}", job.id, e);
                                // Fall through to mark failed below
                            }
                        }
                        continue;
                    }

                let error_msg = format!(
                    "NATS delivery limit exhausted after {} attempts",
                    advisory.deliveries
                );
                warn!("Job {} ({}) failed: {}", job.id, job.job_type, error_msg);

                if let Some(ref db) = job_db {
                    let _ = db
                        .update_status(job.id, JobStatus::Failed, None, Some(&error_msg))
                        .await;
                }

                if let Some(ref cb) = status_callback {
                    let ctx = job.context.as_ref();
                    cb(JobStatusChange {
                        job_id: job.id,
                        job_type: job.job_type,
                        status: JobStatus::Failed,
                        progress: None,
                        error: Some(error_msg),
                        result: None,
                        entity_type: ctx.and_then(|c| c.entity_type.clone()),
                        entity_id: ctx.and_then(|c| c.entity_id),
                        description: ctx.map(|c| c.description.clone()),
                        display: ctx.and_then(|c| c.display.clone()),
                        created_at: job.created_at,
                        scheduled: job.scheduled,
                    });
                }
            }
        }
    }
}

/// How long to hold a NAK'd job before redelivering it.
///
/// Doubles from one second and caps at thirty: 1s, 2s, 4s, 8s, 16s, 30s. Long
/// enough that a failing dependency is not hammered six times in a second,
/// short enough that a transient blip clears quickly and that the consumer's
/// ack-pending slot is not tied up for long.
fn retry_backoff(retry_count: u32) -> Duration {
    const CAP_SECS: u64 = 30;
    Duration::from_secs((1u64 << retry_count.min(6)).min(CAP_SECS))
}

/// How many deliveries this job can actually get.
///
/// The job payload carries its own retry budget, but the consumer's
/// `max_deliver` is the one NATS enforces — past it the server stops
/// redelivering no matter what the payload says. The lower of the two is the
/// real budget, so a handler that narrows `max_deliver` is not told it has
/// retries left that it will never receive.
///
/// JetStream reads a non-positive `max_deliver` as unlimited, so only a positive
/// value narrows anything; the job's own budget stands otherwise.
fn effective_max_attempts(job_max_retries: u32, max_deliver: i64) -> i64 {
    let job_budget = job_max_retries as i64 + 1;
    if max_deliver > 0 {
        job_budget.min(max_deliver)
    } else {
        job_budget
    }
}

async fn process_message(
    msg: async_nats::jetstream::message::Message,
    handler: &dyn JobHandler,
    job_db: Option<&JobStatusDb>,
    status_callback: Option<&StatusCallback>,
    ack_wait: Duration,
    max_deliver: i64,
    cancel_registry: &CancelRegistry,
) {
    let job: Job = match serde_json::from_slice(&msg.payload) {
        Ok(job) => job,
        Err(e) => {
            error!("Failed to deserialize job: {}", e);
            // Ack to prevent infinite retry of bad message
            let _ = msg.ack().await;
            return;
        }
    };

    let _running = crate::running::track_running(&job.job_type, job.id);

    // Get delivery count from NATS message info (this is the actual retry count)
    // num_delivered starts at 1 for first delivery, so subtract 1 to get retry count
    let delivery_count = msg.info().map(|info| info.delivered).unwrap_or(1);
    let retry_count = delivery_count.saturating_sub(1) as u32;

    debug!(
        "Processing job {} of type {} (delivery #{}, retry #{})",
        job.id, job.job_type, delivery_count, retry_count
    );

    // --- Duplicate execution guard ---
    // NATS may redeliver a message if a heartbeat ack fails (network blip,
    // executor stall). Without this check, the same job executes twice,
    // producing duplicate side effects (double imports, double notifications).
    if let Some(db) = job_db {
        match db.get_job_status_with_updated(job.id).await {
            Ok(Some((JobStatus::Completed, _))) => {
                debug!("Job {} already completed, acking redelivery", job.id);
                let _ = msg.ack().await;
                return;
            }
            Ok(Some((JobStatus::Running, Some(updated_at)))) => {
                let staleness = OffsetDateTime::now_utc() - updated_at;
                // If heartbeat touched updated_at recently, the original is still alive
                if staleness < (ack_wait * 2) {
                    // Loud, not debug. This arm destroys a delivery, and when it
                    // fires on a message that was meant to be retried the job
                    // simply stops existing — no error, no advisory, nothing to
                    // grep for. That failure mode hid a dropped-retry bug for
                    // months; if it ever recurs it should be visible in prod
                    // logs at default level.
                    warn!(
                        "Job {} already running (updated {:?} ago, delivery #{}), acking redelivery",
                        job.id, staleness, delivery_count
                    );
                    let _ = msg.ack().await;
                    return;
                }
                // Otherwise the original is likely dead — proceed with this delivery
                info!(
                    "Job {} marked running but stale (updated {:?} ago), taking over",
                    job.id, staleness
                );
            }
            _ => {} // Pending, Failed, Retrying, or not found — proceed normally
        }
    }

    // Helper to build and fire status change
    let notify = |status: JobStatus,
                  progress: Option<i32>,
                  error: Option<String>,
                  result: Option<serde_json::Value>| {
        if let Some(cb) = status_callback {
            let ctx = job.context.as_ref();
            cb(JobStatusChange {
                job_id: job.id,
                job_type: job.job_type.clone(),
                status,
                progress,
                error,
                result,
                entity_type: ctx.and_then(|c| c.entity_type.clone()),
                entity_id: ctx.and_then(|c| c.entity_id),
                description: ctx.map(|c| c.description.clone()),
                display: ctx.and_then(|c| c.display.clone()),
                created_at: job.created_at,
                scheduled: job.scheduled,
            });
        }
    };

    // Update local DB to "running"
    if let Some(db) = job_db {
        let _ = db
            .upsert_job_status(&job, JobStatus::Running, None, None)
            .await;
    }
    notify(JobStatus::Running, None, None, None);

    // Immediately reset the ack_wait deadline so slow-starting handlers
    // don't burn a delivery attempt before the first heartbeat tick.
    if let Err(e) = msg.ack_with(async_nats::jetstream::AckKind::Progress).await {
        warn!(
            "Failed to send initial progress ack for job {}: {}",
            job.id, e
        );
    }

    // Execute with periodic in-progress acks to prevent NATS redelivery.
    // Sends AckKind::Progress at half the ack_wait interval, resetting the
    // server-side deadline so long-running jobs aren't treated as stuck.
    let cancel = cancel_registry.insert(job.id);
    let result = {
        let execute_fut = handler.execute(job.id, job.payload.clone(), cancel.clone());
        tokio::pin!(execute_fut);

        let heartbeat_interval = ack_wait / 2;
        let mut ticker = tokio::time::interval(heartbeat_interval);
        ticker.tick().await; // skip the immediate first tick

        loop {
            tokio::select! {
                result = &mut execute_fut => break result,
                _ = cancel.cancelled() => break Err(JobError::Cancelled),
                _ = ticker.tick() => {
                    debug!("Sending in-progress ack for job {} (heartbeat every {:?})", job.id, heartbeat_interval);
                    if let Err(e) = msg.ack_with(async_nats::jetstream::AckKind::Progress).await {
                        warn!("Failed to send in-progress ack for job {}: {}", job.id, e);
                    }
                    // Touch updated_at so the orphan detector knows this job is alive
                    if let Some(db) = job_db {
                        let _ = db.heartbeat(job.id).await;
                    }
                }
            }
        }
    };
    cancel_registry.remove(&job.id);

    match result {
        Ok(result) => {
            // Sleep before acking if the handler requests a post-job delay.
            // This keeps the message in-flight during the cooldown so no other
            // instance grabs the next message prematurely.
            if let Some(delay) = handler.post_job_delay(&job.payload) {
                debug!("Job {} completed, sleeping {:?} before ack", job.id, delay);
                tokio::time::sleep(delay).await;
            }
            debug!("Job {} completed successfully", job.id);
            let _ = msg.ack().await;
            if let Some(db) = job_db {
                let _ = db
                    .update_status(job.id, JobStatus::Completed, Some(&result), None)
                    .await;
            }
            notify(JobStatus::Completed, None, None, Some(result));
        }
        Err(e) => {
            let error_msg = e.to_string();

            let max_attempts = effective_max_attempts(job.max_retries, max_deliver);

            if e.is_retryable() && (retry_count as i64) < max_attempts - 1 {
                info!(
                    "Job {} failed with retryable error (attempt {}/{}), will retry: {}",
                    job.id,
                    retry_count + 1,
                    max_attempts,
                    error_msg
                );
                // Record Retrying BEFORE the NAK, not after.
                //
                // `Nak(None)` asks for immediate redelivery, and the redelivered
                // message reaches the duplicate-execution guard above within a
                // millisecond or two. If the row still reads `Running` at that
                // moment — which it does while this update is still in flight,
                // with `updated_at` freshly heartbeated by the run that just
                // failed — the guard concludes another worker is alive, acks the
                // redelivery and returns. The message is consumed without ever
                // executing, the row is stranded in `retrying`, and the retry
                // this branch just promised silently never happens.
                //
                // `Retrying` hits the guard's "proceed normally" arm, so writing
                // it first makes the redelivery run instead of being swallowed.
                if let Some(db) = job_db
                    && let Err(e) = db
                        .update_status(job.id, JobStatus::Retrying, None, Some(&error_msg))
                        .await
                {
                    // Not fatal — the redelivery delay below is long enough that
                    // the guard is very unlikely to still read Running — but this
                    // is the one thing standing between a retry and silence, so
                    // never let it fail quietly.
                    error!(
                        "Job {} could not be marked Retrying before its NAK ({}); \
                         the redelivery may be dropped by the duplicate guard",
                        job.id, e
                    );
                }
                // NAK with a delay that grows per attempt.
                //
                // This used to be Nak(None) — immediate redelivery. That was
                // survivable only while retries were being silently dropped;
                // with them working, a handler failing fast against a sick
                // dependency would retry as fast as the loop can spin (IPTV
                // provider timeouts burn all six attempts in well under a
                // second). The delay also gives the Retrying write above room to
                // land before the redelivery races it.
                //
                // Kept modest deliberately: a NAK'd message can hold its
                // ack-pending slot until it comes back, and max_ack_pending is
                // only max_concurrent, so a long delay would idle the consumer.
                let delay = retry_backoff(retry_count);
                match msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(delay)))
                    .await
                {
                    Ok(_) => {}
                    Err(e) => error!("Failed to NAK message: {}", e),
                }
                notify(JobStatus::Retrying, None, Some(error_msg), None);
            } else {
                error!(
                    "Job {} failed permanently after {} attempts: {}",
                    job.id,
                    retry_count + 1,
                    error_msg
                );
                let _ = msg.ack().await; // Ack to prevent more retries
                if let Some(db) = job_db {
                    let _ = db
                        .update_status(job.id, JobStatus::Failed, None, Some(&error_msg))
                        .await;
                }
                notify(JobStatus::Failed, None, Some(error_msg), None);
            }
        }
    }
}

/// Progress reporter for long-running jobs.
pub struct ProgressReporter {
    job_id: Uuid,
    job_db: Option<JobStatusDb>,
}

impl ProgressReporter {
    pub fn new(job_id: Uuid, job_db: Option<JobStatusDb>) -> Self {
        Self { job_id, job_db }
    }

    /// Update progress (0-100).
    pub async fn update(&self, progress: u8, intermediate_result: Option<&Value>) {
        if let Some(db) = &self.job_db {
            let _ = db
                .update_progress(self.job_id, progress, intermediate_result)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal handler for testing trait defaults.
    struct TestHandler;

    #[async_trait]
    impl JobHandler for TestHandler {
        fn job_type(&self) -> &'static str {
            "test"
        }
        async fn execute(
            &self,
            _job_id: Uuid,
            _payload: Value,
            cancel: CancellationToken,
        ) -> Result<Value, JobError> {
            crate::check_cancelled(&cancel)?;
            Ok(Value::Null)
        }
    }

    /// Handler that overrides max_deliver.
    struct CustomMaxDeliverHandler;

    #[async_trait]
    impl JobHandler for CustomMaxDeliverHandler {
        fn job_type(&self) -> &'static str {
            "custom_max_deliver"
        }
        fn max_deliver(&self) -> Option<i64> {
            Some(20)
        }
        async fn execute(
            &self,
            _job_id: Uuid,
            _payload: Value,
            cancel: CancellationToken,
        ) -> Result<Value, JobError> {
            crate::check_cancelled(&cancel)?;
            Ok(Value::Null)
        }
    }

    #[test]
    fn default_max_deliver_is_none() {
        let handler = TestHandler;
        assert_eq!(handler.max_deliver(), None);
    }

    #[test]
    fn handler_can_override_max_deliver() {
        let handler = CustomMaxDeliverHandler;
        assert_eq!(handler.max_deliver(), Some(20));
    }

    #[test]
    fn default_max_concurrent_is_3() {
        let handler = TestHandler;
        assert_eq!(handler.max_concurrent(), 3);
    }

    #[test]
    fn default_ack_wait_is_none() {
        let handler = TestHandler;
        assert_eq!(handler.ack_wait(), None);
    }

    #[test]
    fn worker_config_defaults() {
        let config = WorkerConfig::for_service("test");
        assert_eq!(config.max_deliver, WorkerConfig::DEFAULT_MAX_DELIVER);
        assert_eq!(config.ack_wait, WorkerConfig::DEFAULT_ACK_WAIT);
    }

    #[test]
    fn max_deliver_override_takes_precedence() {
        let config = WorkerConfig::for_service("test");
        let handler = CustomMaxDeliverHandler;
        let effective = handler.max_deliver().unwrap_or(config.max_deliver);
        assert_eq!(effective, 20);
    }

    #[test]
    fn max_deliver_default_falls_back_to_config() {
        let config = WorkerConfig::for_service("test");
        let handler = TestHandler;
        let effective = handler.max_deliver().unwrap_or(config.max_deliver);
        assert_eq!(effective, 6);
    }

    #[test]
    fn a_narrow_max_deliver_caps_the_jobs_own_retry_budget() {
        // media_analyze's shape: job says 5 retries, consumer allows 2 deliveries.
        assert_eq!(effective_max_attempts(5, 2), 2);
    }

    #[test]
    fn a_wide_max_deliver_leaves_the_jobs_budget_alone() {
        assert_eq!(effective_max_attempts(5, 20), 6);
        assert_eq!(effective_max_attempts(5, 6), 6);
    }

    #[test]
    fn an_unlimited_max_deliver_does_not_narrow_the_budget() {
        // JetStream reads <= 0 as unlimited; it must not read as "one attempt".
        assert_eq!(effective_max_attempts(5, -1), 6);
        assert_eq!(effective_max_attempts(5, 0), 6);
    }

    #[test]
    fn a_job_with_no_retries_gets_exactly_one_attempt() {
        assert_eq!(effective_max_attempts(0, 6), 1);
    }

    #[test]
    fn retry_backoff_grows_and_then_holds_at_the_cap() {
        assert_eq!(retry_backoff(0), Duration::from_secs(1));
        assert_eq!(retry_backoff(1), Duration::from_secs(2));
        assert_eq!(retry_backoff(4), Duration::from_secs(16));
        assert_eq!(retry_backoff(5), Duration::from_secs(30));
        // Must not overflow or wrap for an absurd delivery count.
        assert_eq!(retry_backoff(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn the_first_retry_is_not_immediate() {
        // The bug this replaced: Nak(None) redelivered instantly, racing the
        // Retrying write and hammering failing dependencies.
        assert!(retry_backoff(0) >= Duration::from_secs(1));
    }
}
