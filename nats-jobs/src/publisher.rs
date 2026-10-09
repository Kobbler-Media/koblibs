//! Publishing utilities for jobs and events.

use crate::client::{NatsClient, NatsError};
use crate::types::{Event, Job, JobContext};
use serde::Serialize;
use std::time::Duration;
use tracing::{debug, instrument};
use uuid::Uuid;

/// Publisher for NATS jobs and events.
#[derive(Clone)]
pub struct Publisher {
    nats: NatsClient,
    source: String,
}

impl Publisher {
    /// Create a new publisher.
    pub fn new(nats: NatsClient, source: impl Into<String>) -> Self {
        Self {
            nats,
            source: source.into(),
        }
    }

    /// Enqueue a job for processing.
    ///
    /// The job will be published to `jobs.{target}.{job_type}` where target
    /// is the service that should process it (e.g., "kobbler", "kobload", "shared").
    #[instrument(skip(self, payload), fields(job_type = %job_type, target = %target))]
    pub async fn enqueue_job(
        &self,
        target: &str,
        job_type: &str,
        payload: serde_json::Value,
    ) -> Result<Uuid, NatsError> {
        let job = Job::new(job_type, payload, &self.source);
        let job_id = job.id;
        let subject = format!("jobs.{}.{}", target, job_type);

        self.publish_job_internal(subject, job).await?;

        Ok(job_id)
    }

    /// Enqueue a job with context for UI display.
    #[instrument(skip(self, payload, context), fields(job_type = %job_type, target = %target))]
    pub async fn enqueue_job_with_context(
        &self,
        target: &str,
        job_type: &str,
        payload: serde_json::Value,
        context: JobContext,
    ) -> Result<Uuid, NatsError> {
        self.enqueue_job_with_context_and_options(target, job_type, payload, context, false)
            .await
    }

    /// Enqueue a job with context and additional options.
    ///
    /// Use `scheduled=true` for routine/periodic jobs that should be hidden in the UI by default.
    #[instrument(skip(self, payload, context), fields(job_type = %job_type, target = %target, scheduled = %scheduled))]
    pub async fn enqueue_job_with_context_and_options(
        &self,
        target: &str,
        job_type: &str,
        payload: serde_json::Value,
        context: JobContext,
        scheduled: bool,
    ) -> Result<Uuid, NatsError> {
        let mut job = Job::new(job_type, payload, &self.source).with_context(context);
        if scheduled {
            job = job.as_scheduled();
        }
        let job_id = job.id;
        let subject = format!("jobs.{}.{}", target, job_type);

        self.publish_job_internal(subject, job).await?;

        Ok(job_id)
    }

    /// Enqueue a scheduled/routine job (marked so the UI can filter it).
    #[instrument(skip(self, payload), fields(job_type = %job_type, target = %target))]
    pub async fn enqueue_scheduled_job(
        &self,
        target: &str,
        job_type: &str,
        payload: serde_json::Value,
    ) -> Result<Uuid, NatsError> {
        let job = Job::new(job_type, payload, &self.source).as_scheduled();
        let job_id = job.id;
        let subject = format!("jobs.{}.{}", target, job_type);

        self.publish_job_internal(subject, job).await?;

        Ok(job_id)
    }

    /// Enqueue a full Job struct (for more control over job parameters).
    pub async fn enqueue(&self, target: &str, job: Job) -> Result<Uuid, NatsError> {
        let job_id = job.id;
        let subject = format!("jobs.{}.{}", target, job.job_type);

        self.publish_job_internal(subject, job).await?;

        Ok(job_id)
    }

    async fn publish_job_internal(&self, subject: String, job: Job) -> Result<(), NatsError> {
        let bytes = serde_json::to_vec(&job)?;
        debug!("Publishing job {} to {}", job.id, subject);

        // Set Nats-Msg-Id header for NATS-level dedup and per-message lookup
        // in ghost reconciliation.
        let msg = async_nats::jetstream::message::PublishMessage::build()
            .payload(bytes.into())
            .message_id(job.id.to_string());

        self.nats
            .jetstream()
            .send_publish(subject, msg)
            .await
            .map_err(|e| NatsError::JetStream(e.to_string()))?;

        Ok(())
    }

    /// Publish an event for pub/sub consumption.
    ///
    /// Events are published to `events.{event_type}`.
    #[instrument(skip(self, payload), fields(event_type = %event_type))]
    pub async fn publish_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Uuid, NatsError> {
        let event = Event::new(event_type, payload, &self.source);
        let event_id = event.id;
        let subject = format!("events.{}", event_type);

        let bytes = serde_json::to_vec(&event)?;
        debug!("Publishing event {} to {}", event_id, subject);

        self.nats
            .jetstream()
            .publish(subject, bytes.into())
            .await
            .map_err(|e| NatsError::JetStream(e.to_string()))?;

        Ok(event_id)
    }

    /// Publish a typed event (serializes the payload).
    pub async fn publish_event_typed<T: Serialize>(
        &self,
        event_type: &str,
        payload: &T,
    ) -> Result<Uuid, NatsError> {
        let payload_json = serde_json::to_value(payload)?;
        self.publish_event(event_type, payload_json).await
    }

    /// Make a request and wait for a reply (request/reply pattern).
    ///
    /// Used for synchronous-ish operations like manual search.
    #[instrument(skip(self, payload), fields(subject = %subject))]
    pub async fn request<T: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        subject: &str,
        payload: &T,
        timeout: Duration,
    ) -> Result<R, NatsError> {
        let bytes = serde_json::to_vec(payload)?;

        let response = tokio::time::timeout(
            timeout,
            self.nats
                .client()
                .request(subject.to_string(), bytes.into()),
        )
        .await
        .map_err(|_| NatsError::JetStream("Request timed out".to_string()))?
        .map_err(NatsError::Request)?;

        let result: R = serde_json::from_slice(&response.payload)?;
        Ok(result)
    }
}

/// Optional publisher that gracefully handles missing NATS connection.
#[derive(Clone)]
pub struct OptionalPublisher {
    publisher: Option<Publisher>,
}

impl OptionalPublisher {
    /// Create a new optional publisher.
    pub fn new(nats: Option<NatsClient>, source: impl Into<String>) -> Self {
        Self {
            publisher: nats.map(|n| Publisher::new(n, source)),
        }
    }

    /// Check if NATS is available.
    pub fn is_available(&self) -> bool {
        self.publisher.is_some()
    }

    /// Enqueue a job, logging a warning if NATS is unavailable.
    pub async fn enqueue_job(
        &self,
        target: &str,
        job_type: &str,
        payload: serde_json::Value,
    ) -> Option<Uuid> {
        match &self.publisher {
            Some(p) => match p.enqueue_job(target, job_type, payload).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!("Failed to enqueue job {}: {}", job_type, e);
                    None
                }
            },
            None => {
                tracing::debug!("NATS unavailable, skipping job enqueue: {}", job_type);
                None
            }
        }
    }

    /// Publish an event, logging a warning if NATS is unavailable.
    pub async fn publish_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Option<Uuid> {
        match &self.publisher {
            Some(p) => match p.publish_event(event_type, payload).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!("Failed to publish event {}: {}", event_type, e);
                    None
                }
            },
            None => {
                tracing::debug!("NATS unavailable, skipping event publish: {}", event_type);
                None
            }
        }
    }

    /// Get the underlying publisher if available.
    pub fn get(&self) -> Option<&Publisher> {
        self.publisher.as_ref()
    }
}
