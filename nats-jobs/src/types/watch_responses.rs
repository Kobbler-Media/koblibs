//! Shared HTTP API response types for watch patterns and matches.
//!
//! These types define the wire format between kobload (serializer) and its
//! consumers like kobbler (deserializer). Keeping them in koblibs prevents
//! drift between the two projects.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::media::MediaWant;
use super::watch::MatchTarget;

/// Episode progress for TvSeason patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeProgress {
    pub total: usize,
    pub imported: usize,
    pub downloading: usize,
}

/// API response for a single watch pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchPatternResponse {
    pub id: Uuid,
    pub source_system: String,
    pub source_id: String,
    pub callback_url: String,
    pub patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_want: Option<MediaWant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_targets: Option<Vec<MatchTarget>>,
    pub match_mode: String,
    #[serde(default)]
    pub min_score: Option<f64>,
    #[serde(default)]
    pub media_type_id: Option<Uuid>,
    #[serde(default)]
    pub auto_download: bool,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub upgrade_allowed: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub upgrade_until: Option<OffsetDateTime>,
    /// Computed status: "searching", "downloading", "upgrading", "satisfied", "paused", "held"
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_progress: Option<EpisodeProgress>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Watch match API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchMatchResponse {
    pub id: Uuid,
    #[serde(default)]
    pub watch_pattern_id: Option<Uuid>,
    #[serde(default)]
    pub indexer_source_id: Option<Uuid>,
    pub release_guid: String,
    pub release_title: String,
    #[serde(default)]
    pub release_data: serde_json::Value,
    #[serde(default)]
    pub match_score: f64,
    #[serde(default)]
    pub matched_pattern: Option<String>,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub download_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub callback_sent_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub callback_error: Option<String>,
    #[serde(default)]
    pub quality_score: Option<f64>,
    #[serde(default)]
    pub rejected: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub rejected_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    #[serde(default)]
    pub held: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub hold_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub import_confirmed_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_key: Option<String>,
}

/// Combined pattern + matches response for detail endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchPatternDetailResponse {
    pub pattern: WatchPatternResponse,
    pub matches: Vec<WatchMatchResponse>,
}

/// Paginated list of watch patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListWatchPatternsResponse {
    pub items: Vec<WatchPatternResponse>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Paginated list of watch matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListWatchMatchesResponse {
    pub items: Vec<WatchMatchResponse>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}
