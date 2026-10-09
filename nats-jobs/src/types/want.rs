use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::media::MediaWant;
use super::scoring::ProcessedReleaseDto;
use super::watch::MatchTarget;

fn default_want_match_mode() -> String {
    "fuzzy".to_string()
}

fn default_want_min_score() -> f64 {
    0.75
}

fn default_want_early_exit_score() -> f64 {
    0.95
}

fn default_want_auto_download() -> bool {
    true
}

fn default_want_limit() -> u32 {
    50
}

/// Want request payload (Kobbler -> Kobload).
/// Combines watch pattern creation with immediate search.
///
/// Unified type for both NATS jobs and HTTP API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WantRequest {
    /// Source system identifier (e.g., "kobbler")
    pub source_system: String,
    /// Source-specific identifier (e.g., book_id)
    pub source_id: String,
    /// Where to POST match notifications
    pub callback_url: String,
    /// Auth token for callbacks (optional)
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Simple search query (converted to single-element patterns if patterns is empty).
    /// Convenience field for HTTP API; prefer patterns for NATS jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Search patterns (manual/advanced). Prefer media_want for normal flows.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Match targets for scoring (optional, uses patterns if empty)
    #[serde(default)]
    pub match_targets: Vec<MatchTarget>,
    /// Match mode: "any", "all", or "fuzzy" (default: "fuzzy")
    #[serde(default = "default_want_match_mode")]
    pub match_mode: String,
    /// Minimum score threshold (default: 0.75)
    #[serde(default = "default_want_min_score")]
    pub min_score: f64,
    /// Early exit score - stop cascade search if score >= this (default: 0.95)
    #[serde(default = "default_want_early_exit_score")]
    pub early_exit_score: f64,
    /// Media type key for category filtering (e.g., "ebooks", "tv").
    /// Convenience field; resolved from media_want if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Whether to auto-download matches (default: true)
    #[serde(default = "default_want_auto_download")]
    pub auto_download: bool,
    /// Max results per search pattern
    #[serde(default = "default_want_limit")]
    pub limit: u32,
    /// Opaque metadata passed back in callbacks
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Structured media request (preferred for normal flows).
    /// Download service resolves media type and categories from this.
    #[serde(default)]
    pub media_want: Option<MediaWant>,
    /// Whether quality upgrades are allowed after initial download (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_allowed: Option<bool>,
    /// A person asked for this specific thing right now, so the upgrade-eligibility
    /// gate does not apply: an already-imported item is a candidate again. Quality
    /// profile, trash filters and exclusions still decide which release wins.
    /// Schedulers, RSS and backlog runs leave this false.
    #[serde(default)]
    pub manual: bool,
    /// Releases to exclude from matching (previously rejected/replaced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_releases: Option<Vec<ExcludedRelease>>,
    /// Don't backlog-search before this date (ISO 8601). NULL means search immediately.
    /// RSS matching is unaffected — only backlog search is gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_not_before: Option<String>,
}

/// A release to exclude from matching (previously rejected or replaced by user).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExcludedRelease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
}

/// Want response payload (Kobload -> Kobbler).
///
/// Unified type for both NATS events and HTTP responses.
/// NATS sets source_system/source_id for routing; HTTP callers already know them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WantResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub watch_pattern_id: Uuid,
    pub status: WantStatus,
    pub download_id: Option<Uuid>,
    /// The selected release (if any). Included in HTTP responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_release: Option<ProcessedReleaseDto>,
    pub results_found: usize,
    pub best_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_target: Option<super::scoring::MatchedTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// For TvSeason: which episodes actually started downloading.
    /// Episode identifiers (e.g., "1", "2") that got downloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes_downloaded: Option<Vec<String>>,
}

/// Status of a want request
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum WantStatus {
    Downloading,
    Watching,
    Failed,
}

impl std::fmt::Display for WantStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WantStatus::Downloading => write!(f, "downloading"),
            WantStatus::Watching => write!(f, "watching"),
            WantStatus::Failed => write!(f, "failed"),
        }
    }
}
