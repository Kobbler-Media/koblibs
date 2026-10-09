use serde::{Deserialize, Serialize};

/// Compact show summary for disambiguation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowSummary {
    pub id: String,
    pub title: String,
    pub year: Option<i32>,
    pub status: Option<String>,
    pub episode_count: Option<i32>,
}

/// Full show detail for playground — seasons, episodes, quality profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowDetail {
    pub id: String,
    pub title: String,
    pub year: Option<i32>,
    pub status: Option<String>,
    pub network: Option<String>,
    pub content_category: String,
    pub quality_profile_id: Option<String>,
    pub seasons: Vec<SeasonDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeasonDetail {
    pub id: String,
    pub season_identifier: String,
    pub episodes: Vec<EpisodeDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeDetail {
    pub id: String,
    pub episode_identifier: String,
    pub title: Option<String>,
    pub air_date: Option<String>,
    pub want_status: Option<String>,
}

/// Request to build a MediaWant from library data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMediaWantRequest {
    pub show_id: String,
    pub season_id: Option<String>,
    /// Specific episode IDs. If empty, includes all wanted episodes in the season.
    #[serde(default)]
    pub episode_ids: Vec<String>,
}

// ── Audiobook browsing ─────────────────────────────────────────────────

/// Compact audiobook summary for koblem's library pickers (TTS voice clone,
/// STT source selection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub narrator: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
}

/// Chapter with the server-side file path visible to the Python containers
/// (via the same bind-mount as the audiobooks volume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookChapter {
    pub index: u32,
    pub title: String,
    pub file_path: String,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
}

/// Full audiobook detail for koblem's slice-picker UIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookDetail {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub narrator: Option<String>,
    #[serde(default)]
    pub chapters: Vec<AudiobookChapter>,
}
