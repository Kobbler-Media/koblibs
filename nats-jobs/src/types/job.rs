use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A job to be processed by a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique job identifier
    pub id: Uuid,
    /// Job type (e.g., "directory_scan", "metadata_write")
    pub job_type: String,
    /// Job-specific payload data
    pub payload: serde_json::Value,
    /// Source system that created the job ("kobbler", "kobload", "user")
    pub source: String,
    /// When the job was created
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Optional scheduled execution time
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub scheduled_for: Option<OffsetDateTime>,
    /// Maximum retry attempts (default: 3)
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Timeout in seconds (default: 300)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
    /// Current retry count
    #[serde(default)]
    pub retry_count: u32,
    /// Optional context for UI display
    pub context: Option<JobContext>,
    /// Whether this job was created by the scheduler (routine/periodic)
    #[serde(default)]
    pub scheduled: bool,
}

fn default_max_retries() -> u32 {
    5
}

fn default_timeout_secs() -> u32 {
    300
}

impl Job {
    /// Create a new job with the given type and payload.
    pub fn new(
        job_type: impl Into<String>,
        payload: serde_json::Value,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            job_type: job_type.into(),
            payload,
            source: source.into(),
            created_at: OffsetDateTime::now_utc(),
            scheduled_for: None,
            max_retries: default_max_retries(),
            timeout_secs: default_timeout_secs(),
            retry_count: 0,
            context: None,
            scheduled: false,
        }
    }

    /// Set the job context for UI display.
    pub fn with_context(mut self, context: JobContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Set custom retry settings.
    pub fn with_retries(mut self, max_retries: u32, timeout_secs: u32) -> Self {
        self.max_retries = max_retries;
        self.timeout_secs = timeout_secs;
        self
    }

    /// Schedule the job for a specific time.
    pub fn scheduled_at(mut self, time: OffsetDateTime) -> Self {
        self.scheduled_for = Some(time);
        self
    }

    /// Mark this job as a scheduled/routine job.
    pub fn as_scheduled(mut self) -> Self {
        self.scheduled = true;
        self
    }
}

/// Context information for UI display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobContext {
    /// Entity type (e.g., "author", "book", "download")
    pub entity_type: Option<String>,
    /// Entity ID for linking in UI
    pub entity_id: Option<Uuid>,
    /// Human-readable description (e.g., "Syncing author: Brandon Sanderson")
    pub description: String,
    /// Structured display metadata for rich UI rendering (title, subtitle, image_url)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<serde_json::Value>,
}

impl JobContext {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            entity_type: None,
            entity_id: None,
            description: description.into(),
            display: None,
        }
    }

    pub fn for_entity(
        entity_type: impl Into<String>,
        entity_id: Uuid,
        description: impl Into<String>,
    ) -> Self {
        Self {
            entity_type: Some(entity_type.into()),
            entity_id: Some(entity_id),
            description: description.into(),
            display: None,
        }
    }

    /// Attach structured display metadata.
    pub fn with_display(mut self, display: serde_json::Value) -> Self {
        self.display = Some(display);
        self
    }

    /// Attach optional structured display metadata.
    pub fn with_display_opt(mut self, display: Option<serde_json::Value>) -> Self {
        self.display = display;
        self
    }
}

/// Job status update message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatusUpdate {
    /// Job ID
    pub job_id: Uuid,
    /// Current status
    pub status: JobStatus,
    /// Progress percentage (0-100)
    pub progress: Option<u8>,
    /// Result data (on completion)
    pub result: Option<serde_json::Value>,
    /// Error message (on failure)
    pub error: Option<String>,
    /// When the job completed
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Job status enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Retrying,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Pending => write!(f, "pending"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
            JobStatus::Retrying => write!(f, "retrying"),
        }
    }
}
