//! HTTP API contract types shared between services.
//!
//! These types define the request/response shapes for HTTP REST API calls
//! between services. They have no NATS equivalent.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::media::MediaWant;
use super::scoring::ProcessedReleaseDto;
use super::watch::MatchTarget;

// ============================================================================
// Download Management
// ============================================================================

/// Request to create a download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateDownloadRequest {
    pub source: String,
    pub protocol: String,
    pub name: String,
    pub download_url: String,
    pub callback_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<serde_json::Value>,
}

/// Response from creating a download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateDownloadResponse {
    pub id: Uuid,
    pub status: String,
}

// ============================================================================
// Search and Download
// ============================================================================

/// Request for search-and-download (cascade search with auto-download).
///
/// Patterns are tried in order. After each search, results are scored
/// against match_targets. If any result meets the early_exit_score threshold,
/// the cascade stops early to minimize API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchAndDownloadRequest {
    /// Search cascade — patterns tried in order, stops on good match.
    pub patterns: Vec<String>,
    /// Structured media request (preferred for normal flows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_want: Option<MediaWant>,
    /// Targets to score results against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_targets: Vec<MatchTarget>,
    /// Stop cascade if any result scores >= this threshold (default 0.85).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early_exit_score: Option<f64>,
    /// Max results per search query (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Media type key for category filtering (e.g., "ebooks", "tv").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub auto_download: bool,
    #[serde(default)]
    pub min_score: f64,
    /// Metadata to include in the download request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Callback URL for download completion (required if auto_download is true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    /// Auth token for callback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

/// Response from search-and-download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchAndDownloadResponse {
    /// Download ID if auto_download was true and a match was found.
    pub download_id: Option<Uuid>,
    /// The best matching release (if any above threshold).
    pub selected_release: Option<ProcessedReleaseDto>,
    /// All search results with their match scores.
    pub results: Vec<ProcessedReleaseDto>,
}
