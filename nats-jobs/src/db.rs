//! Local database tracking for job status (for UI queries).

use crate::types::{Job, JobStatus};
use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::debug;
use uuid::Uuid;

/// Database wrapper for job status tracking.
#[derive(Clone)]
pub struct JobStatusDb {
    pool: PgPool,
}

impl JobStatusDb {
    /// Create a new job status database wrapper.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert or update job status.
    pub async fn upsert_job_status(
        &self,
        job: &Job,
        status: JobStatus,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = OffsetDateTime::now_utc();

        let (entity_type, entity_id, description, display) = match &job.context {
            Some(ctx) => (
                ctx.entity_type.as_deref(),
                ctx.entity_id,
                Some(ctx.description.as_str()),
                ctx.display.as_ref(),
            ),
            None => (None, None, None, None),
        };

        let started_at = if status == JobStatus::Running {
            Some(now)
        } else {
            None
        };
        let completed_at = if matches!(status, JobStatus::Completed | JobStatus::Failed) {
            Some(now)
        } else {
            None
        };

        sqlx::query!(
            r#"
            INSERT INTO job_status (
                id, job_type, status, payload, result, error, progress,
                entity_type, entity_id, description, display, scheduled,
                created_at, started_at, completed_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            ON CONFLICT (id) DO UPDATE SET
                status = EXCLUDED.status,
                updated_at = NOW(),
                -- When transitioning to running, clear stale fields from
                -- previous failure (e.g. NATS redelivery after orphan cleanup)
                result = CASE
                    WHEN EXCLUDED.status = 'running' THEN NULL
                    ELSE COALESCE(EXCLUDED.result, job_status.result)
                END,
                error = CASE
                    WHEN EXCLUDED.status = 'running' THEN NULL
                    ELSE COALESCE(EXCLUDED.error, job_status.error)
                END,
                progress = CASE
                    WHEN EXCLUDED.status = 'running' THEN 0
                    ELSE EXCLUDED.progress
                END,
                started_at = CASE
                    WHEN EXCLUDED.status = 'running' THEN EXCLUDED.started_at
                    ELSE job_status.started_at
                END,
                completed_at = CASE
                    WHEN EXCLUDED.status = 'running' THEN NULL
                    WHEN EXCLUDED.status IN ('completed', 'failed')
                    THEN EXCLUDED.completed_at
                    ELSE job_status.completed_at
                END
            "#,
            job.id,
            &job.job_type,
            status as JobStatus,
            &job.payload,
            result as Option<&Value>,
            error as Option<&str>,
            0i16,
            entity_type as Option<&str>,
            entity_id as Option<Uuid>,
            description as Option<&str>,
            display as Option<&Value>,
            job.scheduled,
            job.created_at,
            started_at as Option<OffsetDateTime>,
            completed_at as Option<OffsetDateTime>,
        )
        .execute(&self.pool)
        .await?;

        debug!("Upserted job status {} -> {}", job.id, status);
        Ok(())
    }

    /// Update job status by ID.
    pub async fn update_status(
        &self,
        job_id: Uuid,
        status: JobStatus,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = OffsetDateTime::now_utc();

        sqlx::query!(
            r#"
            UPDATE job_status SET
                status = $2,
                updated_at = NOW(),
                -- When transitioning to running, clear stale fields from
                -- previous failure (e.g. NATS redelivery after orphan cleanup)
                result = CASE
                    WHEN $2 = 'running' THEN NULL
                    ELSE COALESCE($3, result)
                END,
                error = CASE
                    WHEN $2 = 'running' THEN NULL
                    ELSE COALESCE($4, error)
                END,
                progress = CASE
                    WHEN $2 = 'running' THEN 0
                    ELSE progress
                END,
                started_at = CASE
                    WHEN $2 = 'running' THEN $5
                    ELSE started_at
                END,
                completed_at = CASE
                    WHEN $2 = 'running' THEN NULL
                    WHEN $2 IN ('completed', 'failed') THEN $5
                    ELSE completed_at
                END
            WHERE id = $1
            "#,
            job_id,
            status as JobStatus,
            result as Option<&Value>,
            error as Option<&str>,
            now,
        )
        .execute(&self.pool)
        .await?;

        debug!("Updated job status {} -> {}", job_id, status);
        Ok(())
    }

    /// Update job progress.
    pub async fn update_progress(
        &self,
        job_id: Uuid,
        progress: u8,
        intermediate_result: Option<&Value>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"
            UPDATE job_status SET
                progress = $2,
                result = COALESCE($3, result),
                updated_at = NOW()
            WHERE id = $1
            "#,
            job_id,
            progress as i16,
            intermediate_result as Option<&Value>,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Query jobs for a specific entity.
    pub async fn jobs_for_entity(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> Result<Vec<JobStatusRow>, sqlx::Error> {
        let rows = sqlx::query_as!(
            JobStatusRow,
            r#"
            SELECT
                id, job_type, status, payload, result, error, progress,
                entity_type, entity_id, description, display, scheduled,
                created_at, started_at, completed_at
            FROM job_status
            WHERE entity_type = $1 AND entity_id = $2
            ORDER BY
                (status IN ('running', 'retrying'))::int DESC,
                CASE WHEN status IN ('running', 'retrying') THEN started_at END ASC NULLS LAST,
                COALESCE(started_at, created_at) DESC
            LIMIT 50
            "#,
            entity_type,
            entity_id,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// Query active jobs (pending or running) for an entity.
    pub async fn active_jobs_for_entity(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> Result<Vec<JobStatusRow>, sqlx::Error> {
        let rows = sqlx::query_as!(
            JobStatusRow,
            r#"
            SELECT
                id, job_type, status, payload, result, error, progress,
                entity_type, entity_id, description, display, scheduled,
                created_at, started_at, completed_at
            FROM job_status
            WHERE entity_type = $1 AND entity_id = $2
              AND status IN ('pending', 'running', 'retrying')
            ORDER BY
                (status IN ('running', 'retrying'))::int DESC,
                CASE WHEN status IN ('running', 'retrying') THEN started_at END ASC NULLS LAST,
                COALESCE(started_at, created_at) DESC
            "#,
            entity_type,
            entity_id,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// List recent jobs.
    ///
    /// When `exclude_routine` is true, hides completed/pending routine jobs
    /// (routine noise). A job is routine if `scheduled=true` or its `job_type`
    /// is in `routine_job_types`. Failed and running routine jobs still appear.
    pub async fn list_recent(
        &self,
        limit: i64,
        offset: i64,
        status_filter: Option<&str>,
        job_type_filter: Option<&str>,
        exclude_routine: bool,
        routine_job_types: &[&str],
    ) -> Result<Vec<JobStatusRow>, sqlx::Error> {
        let rows = sqlx::query_as!(
            JobStatusRow,
            r#"
            SELECT
                id, job_type, status, payload, result, error, progress,
                entity_type, entity_id, description, display, scheduled,
                created_at, started_at, completed_at
            FROM job_status
            WHERE ($3::text IS NULL OR status = $3)
              AND ($4::text IS NULL OR job_type = $4)
              AND (NOT $5 OR NOT ((scheduled = true OR job_type = ANY($6)) AND status IN ('completed', 'pending')))
            ORDER BY
                (status IN ('running', 'retrying'))::int DESC,
                CASE WHEN status IN ('running', 'retrying') THEN started_at END ASC NULLS LAST,
                COALESCE(started_at, created_at) DESC
            LIMIT $1 OFFSET $2
            "#,
            limit,
            offset,
            status_filter as Option<&str>,
            job_type_filter as Option<&str>,
            exclude_routine,
            routine_job_types as &[&str],
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    /// Get job counts by status.
    ///
    /// When `exclude_routine` is true, excludes completed/pending routine jobs.
    /// A job is routine if `scheduled=true` or its `job_type` is in `routine_job_types`.
    pub async fn job_counts(
        &self,
        exclude_routine: bool,
        routine_job_types: &[&str],
    ) -> Result<JobCounts, sqlx::Error> {
        let row = sqlx::query_as!(
            JobCounts,
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'pending') as "pending!",
                COUNT(*) FILTER (WHERE status = 'running') as "running!",
                COUNT(*) FILTER (WHERE status = 'completed') as "completed!",
                COUNT(*) FILTER (WHERE status = 'failed') as "failed!",
                COUNT(*) FILTER (WHERE status = 'retrying') as "retrying!"
            FROM job_status
            WHERE created_at > NOW() - INTERVAL '24 hours'
              AND (NOT $1 OR NOT ((scheduled = true OR job_type = ANY($2)) AND status IN ('completed', 'pending')))
            "#,
            exclude_routine,
            routine_job_types as &[&str],
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row)
    }

    /// Clean up old completed jobs.
    pub async fn cleanup_old_jobs(&self, retention_days: i64) -> Result<u64, sqlx::Error> {
        let result = sqlx::query!(
            r#"
            DELETE FROM job_status
            WHERE status IN ('completed', 'failed')
              AND completed_at < NOW() - make_interval(days => $1::int)
            "#,
            retention_days as i32,
        )
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            debug!("Cleaned up {} old job status records", deleted);
        }
        Ok(deleted)
    }

    /// Cancel pending jobs of a given type matching one payload field, but only
    /// when a second payload field is present (non-null). Returns the number of
    /// jobs cancelled.
    ///
    /// Useful for superseding granular jobs (e.g. single-file scans) when a
    /// broader job (e.g. full directory scan) is being enqueued.
    pub async fn cancel_pending_jobs_with_field(
        &self,
        job_type: &str,
        match_field: &str,
        match_value: &str,
        required_field: &str,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE job_status SET status = 'cancelled'
            WHERE job_type = $1
              AND status = 'pending'
              AND payload->>$2::text = $3
              AND payload->>$4::text IS NOT NULL
            "#,
        )
        .bind(job_type)
        .bind(match_field)
        .bind(match_value)
        .bind(required_field)
        .execute(&self.pool)
        .await?;

        let cancelled = result.rows_affected();
        if cancelled > 0 {
            debug!(
                "Cancelled {} pending {} jobs for {}={}",
                cancelled, job_type, match_field, match_value
            );
        }
        Ok(cancelled)
    }

    /// Touch a running job's `updated_at` to signal it is still alive.
    ///
    /// Called by the worker heartbeat loop alongside the NATS progress ack.
    pub async fn heartbeat(&self, job_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE job_status SET updated_at = NOW() WHERE id = $1",
            job_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reset orphaned running jobs that haven't been updated within `minutes`.
    ///
    /// A running job whose `updated_at` hasn't advanced in a long time is almost
    /// certainly orphaned (worker crashed, NATS message lost). Mark it failed so
    /// `has_active_job` no longer blocks new enqueues for that entity.
    pub async fn reset_orphaned_running_jobs(&self, minutes: i64) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE job_status
            SET status = 'failed',
                error = 'Orphaned — running job not updated within timeout',
                completed_at = NOW()
            WHERE status = 'running'
              AND updated_at < NOW() - make_interval(mins => $1::int)
            "#,
        )
        .bind(minutes as i32)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Check if there's an active job of a given type with matching payload field.
    pub async fn has_active_job(
        &self,
        job_type: &str,
        payload_field: &str,
        payload_value: &str,
    ) -> Result<bool, sqlx::Error> {
        // `retrying` is bounded by recency; `pending` and `running` are not.
        //
        // A retrying row is only genuinely active while a redelivery is still
        // coming, and redelivery is at most tens of seconds out (see
        // retry_backoff). A row that has not moved in ten minutes is stranded,
        // not working — and because this predicate is what callers dedup on, a
        // stranded row silently blocks the same work from ever being enqueued
        // again. That turned one dropped retry into a media file that no
        // re-scan could pick up until the ghost reaper cleared the row.
        //
        // Ten minutes matches the ghost reaper's own threshold, and is far
        // longer than any real retry cycle, so a job that is actually retrying
        // still dedups correctly.
        let exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM job_status
                WHERE job_type = $1
                  AND payload->>$2::text = $3
                  AND (
                        status IN ('pending', 'running')
                     OR (status = 'retrying' AND updated_at > NOW() - INTERVAL '10 minutes')
                  )
            )
            "#,
        )
        .bind(job_type)
        .bind(payload_field)
        .bind(payload_value)
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Get the current DB status of a job by ID, if it exists.
    pub async fn get_job_status(&self, job_id: Uuid) -> Result<Option<JobStatus>, sqlx::Error> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM job_status WHERE id = $1")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(status.and_then(|s| match s.as_str() {
            "pending" => Some(JobStatus::Pending),
            "running" => Some(JobStatus::Running),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            "retrying" => Some(JobStatus::Retrying),
            _ => None,
        }))
    }

    /// Get the current DB status and `updated_at` for a job by ID.
    ///
    /// Used by the duplicate-execution guard to decide whether a redelivered
    /// NATS message should be processed or silently acked.
    pub async fn get_job_status_with_updated(
        &self,
        job_id: Uuid,
    ) -> Result<Option<(JobStatus, Option<OffsetDateTime>)>, sqlx::Error> {
        let row: Option<(String, Option<OffsetDateTime>)> =
            sqlx::query_as("SELECT status, updated_at FROM job_status WHERE id = $1")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.and_then(|(s, updated_at)| {
            let status = match s.as_str() {
                "pending" => JobStatus::Pending,
                "running" => JobStatus::Running,
                "completed" => JobStatus::Completed,
                "failed" => JobStatus::Failed,
                "retrying" => JobStatus::Retrying,
                _ => return None,
            };
            Some((status, updated_at))
        }))
    }
}

/// Job status row from database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JobStatusRow {
    pub id: Uuid,
    pub job_type: String,
    pub status: String,
    pub payload: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub progress: Option<i16>,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    pub description: Option<String>,
    pub display: Option<Value>,
    pub scheduled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Job counts by status.
#[derive(Debug, Clone)]
pub struct JobCounts {
    pub pending: i64,
    pub running: i64,
    pub completed: i64,
    pub failed: i64,
    pub retrying: i64,
}
