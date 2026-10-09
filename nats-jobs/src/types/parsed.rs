//! Shared parsed release metadata types.
//!
//! These types are produced by kobload's release parser and included in
//! `DownloadCompletedEvent` payloads. Both kobload and kobbler import
//! them from here so the wire format can never go out of sync.

use serde::{Deserialize, Serialize};

/// Complete parse result for a release name or path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ParsedRelease {
    /// The cleaned title (show name, movie title, etc.)
    pub title: String,
    /// Media-specific parsed data (depends on media_kind provided)
    pub media: ParsedMedia,
    /// Technical quality attributes
    pub quality: ParsedQuality,
    /// Release flags (proper, repack, internal, etc.)
    pub flags: ReleaseFlags,
    /// Release group if detected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_group: Option<String>,
    /// Confidence in the parse (0.0-1.0)
    pub confidence: f64,
}

/// Media-specific parsed details (tagged enum based on media_kind).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParsedMedia {
    Tv(TvParsed),
    Movie(MovieParsed),
    Book(BookParsed),
    Audiobook(AudiobookParsed),
    Music(MusicParsed),
    Game(GameParsed),
    Software(SoftwareParsed),
    Xxx(XxxParsed),
    Sport(SportParsed),
    Unknown { cleaned_title: String },
}

impl ParsedMedia {
    pub fn as_tv(&self) -> Option<&TvParsed> {
        match self {
            ParsedMedia::Tv(tv) => Some(tv),
            _ => None,
        }
    }

    pub fn as_movie(&self) -> Option<&MovieParsed> {
        match self {
            ParsedMedia::Movie(movie) => Some(movie),
            _ => None,
        }
    }

    pub fn as_book(&self) -> Option<&BookParsed> {
        match self {
            ParsedMedia::Book(book) => Some(book),
            _ => None,
        }
    }

    pub fn as_audiobook(&self) -> Option<&AudiobookParsed> {
        match self {
            ParsedMedia::Audiobook(audiobook) => Some(audiobook),
            _ => None,
        }
    }

    pub fn as_music(&self) -> Option<&MusicParsed> {
        match self {
            ParsedMedia::Music(music) => Some(music),
            _ => None,
        }
    }

    pub fn as_game(&self) -> Option<&GameParsed> {
        match self {
            ParsedMedia::Game(game) => Some(game),
            _ => None,
        }
    }

    pub fn as_software(&self) -> Option<&SoftwareParsed> {
        match self {
            ParsedMedia::Software(software) => Some(software),
            _ => None,
        }
    }

    pub fn as_sport(&self) -> Option<&SportParsed> {
        match self {
            ParsedMedia::Sport(sport) => Some(sport),
            _ => None,
        }
    }

    pub fn as_xxx(&self) -> Option<&XxxParsed> {
        match self {
            ParsedMedia::Xxx(xxx) => Some(xxx),
            _ => None,
        }
    }
}

// =============================================================================
// TV
// =============================================================================

/// Parsed TV episode/season information.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TvParsed {
    pub show_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute_episode: Option<u32>,
    #[serde(default)]
    pub is_pack: bool,
    #[serde(default)]
    pub is_daily: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u32>,
    #[serde(default)]
    pub is_special: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub special_type: Option<SpecialType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
}

/// Types of TV specials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SpecialType {
    Holiday { holiday: String },
    Pilot,
    BehindTheScenes,
    Recap,
    Movie,
    Ova,
    Ona,
    Crossover { other_show: Option<String> },
    Other,
}

// =============================================================================
// Movie
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u32>,
    #[serde(default)]
    pub is_3d: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<u32>,
}

// =============================================================================
// Book
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_position: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_hint: Option<String>,
    #[serde(default)]
    pub is_collection: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
}

// =============================================================================
// Audiobook
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AudiobookParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_position: Option<String>,
    #[serde(default)]
    pub is_unabridged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_parts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// Whether the title came from directory context rather than the filename
    /// (e.g., filename was "01.mp3" so title was derived from parent dir).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_generic_filename: bool,
}

// =============================================================================
// Music
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MusicParsed {
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<u32>,
}

// =============================================================================
// Games
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GameParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    #[serde(default)]
    pub is_cracked: bool,
    #[serde(default)]
    pub is_dlc: bool,
    #[serde(default)]
    pub is_update: bool,
}

// =============================================================================
// Software
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SoftwareParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default)]
    pub is_cracked: bool,
    #[serde(default)]
    pub is_portable: bool,
}

// =============================================================================
// XXX
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct XxxParsed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub performers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
}

// =============================================================================
// Sport
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SportParsed {
    pub league_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_title: Option<String>,
    #[serde(default)]
    pub is_pack: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
}

// =============================================================================
// Quality
// =============================================================================

/// Parsed quality/technical attributes from a release name.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ParsedQuality {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proper: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_repack: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_real: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_remux: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_internal: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_retail: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_limited: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_nfo_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_sample_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_sync_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proof: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dirfix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_nuked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subbed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dubbed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hardcoded_subs: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr10: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr10plus: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dolby_vision: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hlg: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_atmos: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dtsx: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_3d: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_3d: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_obfuscated: bool,
    /// Release group name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_group: Option<String>,
    /// Release version (v2, v3, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u8>,
}

// =============================================================================
// Release Flags
// =============================================================================

/// Release flags - important metadata about the release.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseFlags {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proper: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_repack: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_real: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_remux: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_internal: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_retail: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_limited: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_nfo_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_sample_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_sync_fix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_proof: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dirfix: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_nuked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subbed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dubbed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hardcoded_subs: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr10: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_hdr10plus: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dolby_vision: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_atmos: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dtsx: bool,
}
