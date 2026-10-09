use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::library::ProviderError;

// ── Request/Response types ──────────────────────────────────────────

/// Input for batch release interpretation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretInput {
    /// Raw release name to parse.
    pub name: String,
    /// Media kind: "tv", "movies", "ebooks", "audiobooks", "music", "games", "software".
    pub media_kind: String,
}

/// Parsed release from interpretation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretedRelease {
    /// Original input name.
    pub input: String,
    /// Cleaned/parsed title (show name, movie title, etc.).
    pub title: String,
    /// Media-specific parsed data.
    pub media: Value,
    /// Parsed quality attributes.
    pub quality: InterpretedQuality,
    /// Release group if detected.
    pub release_group: Option<String>,
    /// Parse confidence (0.0-1.0).
    pub confidence: f64,
}

/// Key quality fields extracted from a release name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterpretedQuality {
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub codec: Option<String>,
    pub audio: Option<String>,
    pub streaming_service: Option<String>,
    pub is_proper: bool,
    pub is_repack: bool,
    pub is_remux: bool,
    pub is_hdr: bool,
    pub is_dolby_vision: bool,
    pub is_hardcoded_subs: bool,
}

/// Request for a dry-run release search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSearchRequest {
    /// Pre-built MediaWant JSON — the preferred way to search.
    /// Built by the library provider's `build_media_want()` method.
    /// Passed opaquely to the release provider.
    pub media_want: serde_json::Value,
    /// Quality profile ID to score against (overrides what's in MediaWant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_profile_id: Option<String>,
    /// Minimum match score threshold (default: 0.0).
    #[serde(default)]
    pub min_score: f64,
    /// Max results per search query (default: 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Scored release candidate from a dry-run search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredRelease {
    /// Unique release GUID from the indexer.
    pub guid: String,
    /// Release name.
    pub name: String,
    /// Total score (match + quality + structural).
    pub total_score: f64,
    /// Match score against the search pattern.
    pub match_score: f64,
    /// Quality score from profile evaluation.
    pub quality_score: f64,
    /// Whether this candidate was accepted by scoring.
    pub accepted: bool,
    /// Rejection reason if rejected.
    pub reject_reason: Option<String>,
    /// Parsed quality fields.
    pub quality: InterpretedQuality,
    /// Release group.
    pub release_group: Option<String>,
    /// Indexer that found this release.
    pub indexer: Option<String>,
    /// Release size in bytes.
    pub size: Option<i64>,
    /// Matched season.
    pub matched_season: Option<String>,
    /// Matched episode.
    pub matched_episode: Option<String>,
    /// When this release was published.
    pub publish_date: Option<String>,
    /// Torrent seeders (trust signal).
    pub seeders: Option<i32>,
    /// Community votes (negative = bad).
    pub votes: Option<i32>,
    /// Usenet grabs count.
    pub grabs: Option<i32>,
    /// Protocol: "torrent" or "usenet".
    pub protocol: Option<String>,
}

/// Results from a dry-run release search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSearchResults {
    /// Accepted candidates (passed scoring).
    pub candidates: Vec<ScoredRelease>,
    /// Rejected candidates (failed scoring, with reasons).
    pub rejected: Vec<ScoredRelease>,
    /// The best release that would have been selected.
    pub best_pick: Option<ScoredRelease>,
    /// Total releases found before scoring.
    pub total_found: usize,
    /// Total accepted after scoring.
    pub total_accepted: usize,
}

// ── Trait ────────────────────────────────────────────────────────────

/// Provider for release search and interpretation.
///
/// Implementations wrap a download orchestration service (e.g. kobload)
/// to search indexers, parse release names, and score candidates.
#[async_trait]
pub trait ReleaseProvider: Send + Sync {
    /// Dry-run search: find and score releases for a want, without downloading.
    async fn search_releases(
        &self,
        request: &ReleaseSearchRequest,
    ) -> Result<ReleaseSearchResults, ProviderError>;

    /// Parse release names into structured quality/media fields (batch).
    async fn interpret_releases(
        &self,
        releases: &[InterpretInput],
    ) -> Result<Vec<InterpretedRelease>, ProviderError>;
}
