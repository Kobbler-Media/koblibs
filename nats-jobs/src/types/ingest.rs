use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Result of an ingest operation, published by the ingesting service
/// back to the download orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IngestResultEvent {
    pub download_id: Uuid,
    pub status: IngestResultStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Status of an ingest result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum IngestResultStatus {
    Imported,
    Failed,
    /// The ingesting service cannot process this now (e.g. target storage
    /// temporarily unavailable) but will retry automatically. The download
    /// orchestrator should NOT re-search or treat this as a permanent failure.
    Deferred,
}
