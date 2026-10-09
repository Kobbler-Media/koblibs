//! Simple scheduler for periodic job enqueueing.

use crate::publisher::Publisher;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info};

/// A scheduled job definition.
#[derive(Debug, Clone)]
pub struct ScheduledJob {
    /// Job name for logging.
    pub name: String,
    /// Target service ("kobbler", "kobload", etc.).
    pub target: String,
    /// Job type.
    pub job_type: String,
    /// Interval between runs.
    pub interval: Duration,
    /// Function to generate payload (called each run).
    pub payload_fn: fn() -> Value,
    /// Whether to run immediately on startup.
    pub run_immediately: bool,
}

impl ScheduledJob {
    pub fn new(
        name: impl Into<String>,
        target: impl Into<String>,
        job_type: impl Into<String>,
        interval: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            job_type: job_type.into(),
            interval,
            payload_fn: || Value::Object(serde_json::Map::new()),
            run_immediately: false,
        }
    }

    pub fn with_payload(mut self, payload_fn: fn() -> Value) -> Self {
        self.payload_fn = payload_fn;
        self
    }

    pub fn run_immediately(mut self) -> Self {
        self.run_immediately = true;
        self
    }
}

/// Simple scheduler that periodically enqueues jobs.
pub struct Scheduler {
    publisher: Publisher,
    jobs: Vec<ScheduledJob>,
}

impl Scheduler {
    pub fn new(publisher: Publisher) -> Self {
        Self {
            publisher,
            jobs: Vec::new(),
        }
    }

    /// Register a scheduled job.
    pub fn register(&mut self, job: ScheduledJob) {
        info!(
            "Registered scheduled job '{}' (every {:?})",
            job.name, job.interval
        );
        self.jobs.push(job);
    }

    /// Run the scheduler until shutdown.
    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) {
        info!("Scheduler started with {} jobs", self.jobs.len());

        // Track next run time for each job
        let mut next_runs: HashMap<String, tokio::time::Instant> = HashMap::new();
        let now = tokio::time::Instant::now();

        for job in &self.jobs {
            let next = if job.run_immediately {
                now
            } else {
                now + job.interval
            };
            next_runs.insert(job.name.clone(), next);
        }

        loop {
            // Find the next job to run
            let now = tokio::time::Instant::now();
            let mut next_job: Option<(&ScheduledJob, tokio::time::Instant)> = None;

            for job in &self.jobs {
                if let Some(&next_time) = next_runs.get(&job.name)
                    && (next_job.is_none() || next_time < next_job.unwrap().1)
                {
                    next_job = Some((job, next_time));
                }
            }

            let Some((job, next_time)) = next_job else {
                // No jobs, just wait for shutdown
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Scheduler shutting down");
                            break;
                        }
                    }
                }
                continue;
            };

            // Wait until next run time or shutdown
            let delay = next_time.saturating_duration_since(now);
            if delay > Duration::ZERO {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Scheduler shutting down");
                            break;
                        }
                    }
                }
            }

            // Check if we were signaled to shut down during sleep
            if *shutdown_rx.borrow() {
                break;
            }

            // Run the job
            let payload = (job.payload_fn)();
            debug!("Running scheduled job '{}'", job.name);

            match self
                .publisher
                .enqueue_scheduled_job(&job.target, &job.job_type, payload)
                .await
            {
                Ok(job_id) => {
                    debug!("Enqueued scheduled job '{}' as {}", job.name, job_id);
                }
                Err(e) => {
                    error!("Failed to enqueue scheduled job '{}': {}", job.name, e);
                }
            }

            // Schedule next run
            next_runs.insert(job.name.clone(), tokio::time::Instant::now() + job.interval);
        }
    }
}

/// Builder for common scheduled job patterns.
pub mod patterns {
    use super::*;

    /// Create a cleanup job that runs daily at the specified hour.
    pub fn daily_cleanup(name: &str, target: &str, job_type: &str) -> ScheduledJob {
        ScheduledJob::new(name, target, job_type, Duration::from_secs(24 * 60 * 60))
    }

    /// Create a sync job that runs every N minutes.
    pub fn periodic_sync(
        name: &str,
        target: &str,
        job_type: &str,
        interval_minutes: u64,
    ) -> ScheduledJob {
        ScheduledJob::new(
            name,
            target,
            job_type,
            Duration::from_secs(interval_minutes * 60),
        )
    }

    /// Create a quick check job that runs frequently.
    pub fn frequent_check(
        name: &str,
        target: &str,
        job_type: &str,
        interval_seconds: u64,
    ) -> ScheduledJob {
        ScheduledJob::new(
            name,
            target,
            job_type,
            Duration::from_secs(interval_seconds),
        )
    }
}
