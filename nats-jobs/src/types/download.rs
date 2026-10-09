use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::file_classification::{FileClassification, FileMetadata};
use super::parsed::ParsedRelease;
use super::scoring::MatchedTarget;

/// Download completion event payload.
///
/// Unified type for both NATS events (`download.completed`, `ingest.ready`)
/// and HTTP callbacks. NATS publishers populate available fields; optional
/// fields default when not available.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DownloadCompletedEvent {
    pub download_id: Uuid,
    pub status: String,
    /// Release title / download name (e.g. "Show.S01E02.1080p.WEB-DL-GROUP").
    /// Always populated — use this for episode parsing when filenames are obfuscated.
    #[serde(default)]
    pub name: String,
    pub metadata: serde_json::Value,
    pub files: Vec<DownloadFileInfo>,
    /// File groups (same content, different formats) from scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_groups: Option<Vec<FileGroupInfo>>,
    /// Parsed release metadata (title, season/episode, quality, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_interpretation: Option<ParsedRelease>,
    pub error: Option<String>,
    /// Recoverable error on download service side (e.g., can't access file storage).
    /// Consumer should show this to user; service will auto-retry and send another event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable_error: Option<String>,
    /// Idempotency key for deduplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<Uuid>,
    /// Whether this download is a quality upgrade replacing a previous download.
    #[serde(default)]
    pub is_upgrade: bool,
    /// Quality score of this release (from quality scoring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    /// Download ID this release replaces (for upgrades).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_download_id: Option<Uuid>,
    /// Structured release information for provenance and ingest routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_info: Option<ReleaseInfo>,
}

/// Typed release information for provenance and ingest routing.
///
/// Provides structured fields that consumers can use directly for provenance
/// tracking, ingest routing, and display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseInfo {
    /// Release title (e.g., "Show.S01E02.1080p.WEB-DL-GROUP").
    pub title: String,
    /// Release GUID from the indexer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    /// Indexer name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer: Option<String>,
    /// What this release matched against (episode, season, book, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_target: Option<MatchedTarget>,
    /// Quality score from release scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    /// Combined total score (match + quality + bonuses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_score: Option<f64>,
    /// Release size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Protocol used (usenet/torrent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Info hash (torrents only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
}

/// File info for download events and callbacks.
///
/// Unified type used by both NATS events and HTTP callbacks.
/// NATS publishers populate basic fields; HTTP callbacks include
/// scoring, classification, and technical metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = NatsDownloadFileInfo))]
pub struct DownloadFileInfo {
    /// Unique identifier for this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Relative path within the download, or "archive.zip!/inner/path" for archived files.
    pub path: String,
    /// Absolute path on the download service's filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_path: Option<String>,
    /// URL to download this file (relative to service base URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// File size in bytes.
    pub size: i64,
    /// File extension (lowercase, without dot).
    pub extension: String,
    /// MIME type (detected from magic bytes or extension).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// File relevance score (higher is better match for the request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    /// File classification (role within the release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<FileClassification>,
    /// Technical metadata extracted from file (video/audio/subtitle info).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<FileMetadata>,
    /// Episode identifier this file belongs to (e.g., "4" for S01E04).
    /// Set by download service when per-file episode identification succeeds.
    /// Consumers should use this instead of parsing filenames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_episode: Option<String>,
}

/// A group of files representing the same content in different formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileGroupInfo {
    pub name: String,
    pub formats: Vec<String>,
    pub best_path: String,
}
