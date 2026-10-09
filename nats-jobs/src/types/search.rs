use serde::{Deserialize, Serialize};

use super::media::MediaWant;

/// Search request (Kobbler -> Kobload via request/reply).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchRequest {
    /// Free-text query (for manual searches). Prefer media_want for structured requests.
    pub query: String,
    /// Media type key filter (e.g. "ebooks", "tv"). Resolved from media_want if not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Optional protocol filter (torrent/usenet)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Optional result limit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Optional offset for pagination (skip first N results)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// Structured media request (preferred for normal flows)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_want: Option<MediaWant>,
}

/// Search response (Kobload -> Kobbler).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SearchResult {
    pub guid: String,
    pub title: String,
    pub indexer: String,
    pub size: Option<i64>,
    pub publish_date: Option<String>,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub protocol: Option<String>,
    pub info_url: Option<String>,
    pub info_hash: Option<String>,
    pub indexer_id: Option<String>,
    pub indexer_type: Option<String>,
    pub seeders: Option<i32>,
    pub leechers: Option<i32>,
    pub grabs: Option<i32>,
}
