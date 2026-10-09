//! Job cancellation registry.
//!
//! Provides a shared registry of cancellation tokens keyed by job ID.
//! The worker inserts a token when a job starts executing, and removes it
//! when the job completes. External callers (e.g. a cancel API endpoint)
//! can trigger the token to request cooperative cancellation.

use crate::client::NatsClient;
use crate::worker::JobError;
use dashmap::DashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

/// Shared registry of active job cancellation tokens.
///
/// Clone is cheap — all clones share the same inner map.
#[derive(Clone)]
pub struct CancelRegistry(Arc<DashMap<Uuid, CancellationToken>>);

impl CancelRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self(Arc::new(DashMap::new()))
    }

    /// Create and store a cancellation token for `job_id`.
    ///
    /// If a token already exists for this job (e.g. NATS redelivery that
    /// slipped past the DB guard), the previous token is cancelled before
    /// being replaced, so the earlier execution stops cooperatively.
    ///
    /// Returns the new token so the worker can pass it to the handler.
    pub fn insert(&self, job_id: Uuid) -> CancellationToken {
        if let Some(existing) = self.0.get(&job_id) {
            existing.cancel();
        }
        let token = CancellationToken::new();
        self.0.insert(job_id, token.clone());
        token
    }

    /// Cancel a running job by triggering its token.
    ///
    /// Returns `true` if the job was found (and is now cancelled),
    /// `false` if no such job is currently registered.
    pub fn cancel(&self, job_id: Uuid) -> bool {
        if let Some(entry) = self.0.get(&job_id) {
            entry.cancel();
            true
        } else {
            false
        }
    }

    /// Remove the token for a completed job.
    pub fn remove(&self, job_id: &Uuid) {
        self.0.remove(job_id);
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a cancellation has been requested.
///
/// Call this at natural checkpoints in job handlers:
/// - At the start of `execute()`
/// - Between loop iterations
/// - Between significant processing steps
///
/// Returns `Err(JobError::Cancelled)` if the token has been triggered.
pub fn check_cancelled(cancel: &CancellationToken) -> Result<(), JobError> {
    if cancel.is_cancelled() {
        Err(JobError::Cancelled)
    } else {
        Ok(())
    }
}

/// Purge a pending job's NATS message from the JOBS stream.
///
/// Walks the stream for the job's subject (`jobs.{service}.{job_type}`),
/// finds the message whose `Nats-Msg-Id` header matches `job_id`, and
/// deletes it by sequence number. This prevents the worker from picking
/// up a job that has already been cancelled in the database.
///
/// Returns `true` if the message was found and deleted, `false` if not
/// found (e.g. already delivered/acked).
pub async fn purge_nats_job(
    nats: &NatsClient,
    service: &str,
    job_type: &str,
    job_id: Uuid,
) -> bool {
    use async_nats::jetstream::stream::DirectGetErrorKind;

    let stream = match nats.jetstream().get_stream("JOBS").await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to get JOBS stream for job purge");
            return false;
        }
    };

    let subject = format!("jobs.{service}.{job_type}");
    let job_id_str = job_id.to_string();
    let mut seq = 1u64;

    loop {
        match stream
            .direct_get_next_for_subject(&subject, Some(seq))
            .await
        {
            Ok(msg) => {
                let msg_id = msg
                    .headers
                    .get(async_nats::header::NATS_MESSAGE_ID)
                    .map(|v| v.as_str().to_string());

                if msg_id.as_deref() == Some(&job_id_str) {
                    match stream.delete_message(msg.sequence).await {
                        Ok(true) => {
                            tracing::info!(
                                job_id = %job_id,
                                sequence = msg.sequence,
                                "Purged cancelled job message from NATS stream"
                            );
                            return true;
                        }
                        Ok(false) => {
                            warn!(
                                job_id = %job_id,
                                sequence = msg.sequence,
                                "NATS delete_message returned false"
                            );
                            return false;
                        }
                        Err(e) => {
                            warn!(
                                job_id = %job_id,
                                sequence = msg.sequence,
                                error = %e,
                                "Failed to delete NATS message for cancelled job"
                            );
                            return false;
                        }
                    }
                }

                seq = msg.sequence + 1;
            }
            Err(e) if matches!(e.kind(), DirectGetErrorKind::NotFound) => {
                // No more messages — job wasn't found (already delivered or acked)
                return false;
            }
            Err(e) => {
                warn!(
                    job_id = %job_id,
                    error = %e,
                    "Error walking NATS stream for job purge"
                );
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_cancel() {
        let registry = CancelRegistry::new();
        let job_id = Uuid::new_v4();
        let token = registry.insert(job_id);

        assert!(!token.is_cancelled());
        assert!(registry.cancel(job_id));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_unknown_returns_false() {
        let registry = CancelRegistry::new();
        assert!(!registry.cancel(Uuid::new_v4()));
    }

    #[test]
    fn remove_then_cancel_returns_false() {
        let registry = CancelRegistry::new();
        let job_id = Uuid::new_v4();
        let _token = registry.insert(job_id);
        registry.remove(&job_id);
        assert!(!registry.cancel(job_id));
    }

    #[test]
    fn check_cancelled_ok_when_not_cancelled() {
        let token = CancellationToken::new();
        assert!(check_cancelled(&token).is_ok());
    }

    #[test]
    fn check_cancelled_err_when_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(check_cancelled(&token), Err(JobError::Cancelled)));
    }

    #[test]
    fn insert_cancels_existing_token() {
        let registry = CancelRegistry::new();
        let job_id = Uuid::new_v4();

        let first_token = registry.insert(job_id);
        assert!(!first_token.is_cancelled());

        // Second insert for same job_id should cancel the first token
        let second_token = registry.insert(job_id);
        assert!(first_token.is_cancelled());
        assert!(!second_token.is_cancelled());
    }
}
