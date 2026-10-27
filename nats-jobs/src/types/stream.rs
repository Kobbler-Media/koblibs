use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::media::MediaWant;

/// Request to search for streamable content via debrid providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSearchRequest {
    /// Structured media want (title, year, identifiers, etc.).
    #[serde(default)]
    pub media_want: Option<MediaWant>,
    /// Free-text search query (used if media_want is not provided).
    #[serde(default)]
    pub query: String,
    /// Media type filter (e.g., "movie", "tv").
    #[serde(default)]
    pub media_type: Option<String>,
    /// Quality profile to score against.
    #[serde(default)]
    pub quality_profile_id: Option<Uuid>,
    /// Maximum number of results to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Whether to include uncached results (default: false).
    #[serde(default)]
    pub include_uncached: Option<bool>,
    /// A release the caller already played for this title, or for the previous episode of
    /// the same show. Ranking favours the same pack, then the same binge group, then the
    /// same release group, within each availability tier.
    #[serde(default)]
    pub prefer: Option<StreamPreference>,
    /// Preferred audio languages, most preferred first: English names ("italian") or
    /// ISO 639-1/639-2 codes.
    #[serde(default)]
    pub audio_languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPreference {
    pub info_hash: String,
    #[serde(default)]
    pub file_id: Option<i32>,
    #[serde(default)]
    pub release_title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamAvailability {
    Ready,
    Likely,
    Unknown,
    Uncached,
}

/// The section of the stream list an option belongs to, in list order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamGroup {
    Played,
    Account,
    Available,
    Uncached,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPlayback {
    Worked,
    Failed,
}

/// Source type of a stream option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamSourceType {
    Torrent,
    Usenet,
}

/// A file within a streamable torrent/NZB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamFile {
    /// Provider-specific file ID.
    pub id: i32,
    /// File path within the torrent/archive.
    pub path: String,
    /// File size in bytes.
    pub size: i64,
}

/// A single streamable option returned from stream search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOption {
    /// Unique ID for this result (used in resolve requests).
    pub id: String,
    /// Raw release title from the indexer.
    pub release_title: String,
    /// Total file size in bytes.
    pub size: i64,
    /// Indexer that provided this release.
    pub indexer: String,
    /// Torrent info hash (None for usenet).
    #[serde(default)]
    pub info_hash: Option<String>,
    /// NZB download URL (Some for usenet via TorBox).
    #[serde(default)]
    pub nzb_url: Option<String>,
    /// Magnet URL for the torrent.
    #[serde(default)]
    pub magnet_url: Option<String>,
    /// Parsed quality information.
    #[serde(default)]
    pub quality: Option<serde_json::Value>,
    /// Overall score (want scoring + debrid boost).
    pub total_score: f64,
    /// Match score component (title/year/identifiers).
    pub match_score: f64,
    /// Quality score component (resolution/codec/audio).
    pub quality_score: f64,
    /// Which debrid provider has this cached.
    pub debrid_provider: String,
    /// Debrid provider ID for resolve requests.
    pub debrid_provider_id: Uuid,
    /// Whether this is instantly available (cached) on the debrid provider.
    pub cached: bool,
    /// Torrent or Usenet source.
    pub source_type: StreamSourceType,
    /// Files within this release.
    #[serde(default)]
    pub files: Vec<StreamFile>,
    /// When this release was published.
    #[serde(default)]
    pub publish_date: Option<String>,
    /// Torrentio's `behaviorHints.bingeGroup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binge_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<StreamPlayback>,
    /// `ready`: we resolved it on this account recently. `likely`: a third party reports it
    /// cached. `uncached`: it would have to download first. `cached` is `availability == ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<StreamAvailability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<StreamGroup>,
    /// Audio languages the release name lists, in its order, as ISO 639-1 codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_languages: Vec<String>,
}

/// Response from stream search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSearchResponse {
    /// Ranked stream options, #1 = auto-play pick.
    pub options: Vec<StreamOption>,
    /// Non-fatal errors encountered during search (e.g., one indexer timed out).
    #[serde(default)]
    pub errors: Vec<String>,
}

/// What happened when a client played (or failed to play) a resolved release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOutcomeReport {
    /// Falls back to the first enabled provider when absent.
    #[serde(default)]
    pub debrid_provider_id: Option<Uuid>,
    pub info_hash: String,
    #[serde(default)]
    pub file_id: Option<i32>,
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub release_title: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
}

/// Request to resolve a stream option into a playable URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResolveRequest {
    /// Which debrid provider to use.
    pub debrid_provider_id: Uuid,
    /// Torrent info hash (for torrent sources).
    #[serde(default)]
    pub info_hash: Option<String>,
    /// Magnet URL (for torrent sources).
    #[serde(default)]
    pub magnet_url: Option<String>,
    /// NZB URL (for usenet sources via TorBox).
    #[serde(default)]
    pub nzb_url: Option<String>,
    /// Specific file to stream (for multi-file torrents).
    #[serde(default)]
    pub file_id: Option<i32>,
    /// `warm` resolves ahead of a tap and yields to `user` resolves. Absent means `user`.
    #[serde(default)]
    pub priority: Option<StreamPriority>,
    /// The episode or film being played; a pack's hash only has a title for the whole pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamPriority {
    #[default]
    User,
    Warm,
}

/// Response with a resolved direct stream URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResolveResponse {
    /// Direct HTTP URL for streaming (supports Range requests).
    pub stream_url: String,
    /// Filename of the stream.
    pub filename: String,
    /// File size in bytes.
    pub filesize: i64,
    /// MIME type if known.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// When this URL expires (ISO 8601).
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Whether the content was already cached on the debrid provider.
    /// When false, the provider is downloading the torrent/NZB and the URL
    /// may not be immediately streamable until the download completes.
    #[serde(default = "default_cached")]
    pub cached: bool,
    /// Provider-side torrent/download ID (useful for status polling when uncached).
    #[serde(default)]
    pub torrent_id: Option<String>,
}

fn default_cached() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_is_lowercase_and_optional() {
        for (value, json) in [
            (StreamAvailability::Ready, "\"ready\""),
            (StreamAvailability::Likely, "\"likely\""),
            (StreamAvailability::Unknown, "\"unknown\""),
            (StreamAvailability::Uncached, "\"uncached\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
        }
        let option: StreamOption = serde_json::from_value(serde_json::json!({
            "id": "x", "release_title": "t", "size": 1, "indexer": "i", "total_score": 0.0,
            "match_score": 0.0, "quality_score": 0.0, "debrid_provider": "real_debrid",
            "debrid_provider_id": Uuid::nil(), "cached": false, "source_type": "torrent"
        }))
        .unwrap();
        assert_eq!(option.availability, None);
        assert_eq!(option.group, None);
        assert!(
            !serde_json::to_string(&option)
                .unwrap()
                .contains("availability")
        );
        for (value, json) in [
            (StreamGroup::Played, "\"played\""),
            (StreamGroup::Account, "\"account\""),
            (StreamGroup::Available, "\"available\""),
            (StreamGroup::Uncached, "\"uncached\""),
            (StreamGroup::Failed, "\"failed\""),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
        }
        assert!(option.audio_languages.is_empty());
    }

    #[test]
    fn new_request_fields_default_when_absent() {
        let search: StreamSearchRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(search.audio_languages.is_empty());
        let resolve: StreamResolveRequest =
            serde_json::from_value(serde_json::json!({ "debrid_provider_id": Uuid::nil() }))
                .unwrap();
        assert_eq!(resolve.priority, None);
        assert_eq!(resolve.priority.unwrap_or_default(), StreamPriority::User);
        let warm: StreamResolveRequest = serde_json::from_value(
            serde_json::json!({ "debrid_provider_id": Uuid::nil(), "priority": "warm" }),
        )
        .unwrap();
        assert_eq!(warm.priority, Some(StreamPriority::Warm));
    }
}
