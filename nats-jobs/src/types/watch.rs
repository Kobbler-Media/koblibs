use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::scoring::MatchedTarget;

/// A target to score search results against.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MatchTarget {
    /// Caller's identifier (e.g., book_id)
    pub id: String,
    /// Full string for fuzzy scoring
    pub target: String,
    /// Per-target minimum score threshold override
    #[serde(default)]
    pub min_score: Option<f64>,
    /// Structured identification of what this target represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_target: Option<MatchedTarget>,
}

/// Watch pattern deletion request (Kobbler -> Kobload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchPatternDelete {
    pub source_system: String,
    pub source_id: String,
}

/// Payload for "search this pattern right now" job.
/// Handler runs backlog_search_pattern and publishes want.response event with result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchPatternNow {
    pub source_system: String,
    pub source_id: String,
}

/// Payload for "grab the held match for this pattern" job.
/// Handler calls grab_held_release, result flows through existing download/callback channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GrabHeldMatch {
    pub source_system: String,
    pub source_id: String,
}

/// Watch pattern deleted notification (Kobload -> Kobbler via NATS).
/// Published when a watch pattern is deleted so subscribers can reset entity statuses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchPatternDeletedEvent {
    pub source_system: String,
    pub source_id: String,
    pub metadata: Option<serde_json::Value>,
}

/// Watch match notification (Kobload -> Kobbler).
///
/// Unified type for both NATS events and HTTP callbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WatchMatchedEvent {
    pub watch_pattern_id: Uuid,
    pub source_system: String,
    pub source_id: String,
    pub match_score: f64,
    pub matched_pattern: String,
    pub release_title: String,
    pub release_guid: String,
    pub download_id: Option<Uuid>,
    pub action: String,
    pub metadata: Option<serde_json::Value>,
    /// Whether this download is a quality upgrade replacing a previous download.
    #[serde(default)]
    pub is_upgrade: bool,
    /// Quality score of this release (from quality scoring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    /// Download ID this release replaces (for upgrades).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_download_id: Option<Uuid>,
    /// Episode key for TvSeason patterns (e.g. "S01E05").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_key: Option<String>,
}
