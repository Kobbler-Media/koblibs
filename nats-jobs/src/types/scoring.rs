use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Scoring decision enums ──────────────────────────────────────────────────

/// Top-level scoring decision for a processed release.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScoringDecision {
    /// Release passed all gates and is eligible for download/selection.
    Candidate,
    /// Release was rejected by one or more scoring gates.
    Rejected,
    /// Release did not match any pattern/target above the minimum score.
    NoMatch,
    /// Release was in the pattern's excluded list (user-curated blocklist).
    Excluded,
}

impl std::fmt::Display for ScoringDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Candidate => write!(f, "candidate"),
            Self::Rejected => write!(f, "rejected"),
            Self::NoMatch => write!(f, "no_match"),
            Self::Excluded => write!(f, "excluded"),
        }
    }
}

/// Quality-specific rejection reasons from `score_quality()`.
///
/// A release can have multiple quality rejections simultaneously
/// (e.g., rejected_source AND nuked AND rejected_language).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum QualityRejectReason {
    BelowMinResolution,
    ExceedsMaxResolution,
    RejectedSource,
    RejectedHardcodedSubs,
    RejectedRemux,
    LanguageNotPreferred,
    RejectedLanguage,
    RejectedTerm,
    Nuked,
    TrashLq,
    TrashUpscaled,
    TrashBrDisk,
    TrashBadDual,
    TrashLineMic,
    TrashObfuscated,
    TrashRetags,
    TrashX265Hd,
    TrashExtras,
    TrashNoGroup,
    TrashAnimeLq,
    /// Catch-all for unrecognized trash filter categories.
    TrashFilter,
}

impl QualityRejectReason {
    /// Whether this is a "hard" reject that koblem should skip reviewing.
    ///
    /// Hard rejects are obviously bad (CAM, TS, nuked) where LLM review
    /// adds no value. Soft rejects (below_min_resolution, language) may
    /// warrant a second look.
    pub fn is_hard_reject(self) -> bool {
        matches!(
            self,
            Self::RejectedSource | Self::Nuked | Self::RejectedRemux
        )
    }

    /// Whether this is a TRaSH guide filter rejection.
    pub fn is_trash_filter(self) -> bool {
        matches!(
            self,
            Self::TrashLq
                | Self::TrashUpscaled
                | Self::TrashBrDisk
                | Self::TrashBadDual
                | Self::TrashLineMic
                | Self::TrashObfuscated
                | Self::TrashRetags
                | Self::TrashX265Hd
                | Self::TrashExtras
                | Self::TrashNoGroup
                | Self::TrashAnimeLq
                | Self::TrashFilter
        )
    }
}

impl std::fmt::Display for QualityRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BelowMinResolution => write!(f, "below_min_resolution"),
            Self::ExceedsMaxResolution => write!(f, "exceeds_max_resolution"),
            Self::RejectedSource => write!(f, "rejected_source"),
            Self::RejectedHardcodedSubs => write!(f, "rejected_hardcoded_subs"),
            Self::RejectedRemux => write!(f, "rejected_remux"),
            Self::LanguageNotPreferred => write!(f, "language_not_preferred"),
            Self::RejectedLanguage => write!(f, "rejected_language"),
            Self::RejectedTerm => write!(f, "rejected_term"),
            Self::Nuked => write!(f, "nuked"),
            Self::TrashLq => write!(f, "trash_lq"),
            Self::TrashUpscaled => write!(f, "trash_upscaled"),
            Self::TrashBrDisk => write!(f, "trash_br_disk"),
            Self::TrashBadDual => write!(f, "trash_bad_dual"),
            Self::TrashLineMic => write!(f, "trash_line_mic"),
            Self::TrashObfuscated => write!(f, "trash_obfuscated"),
            Self::TrashRetags => write!(f, "trash_retags"),
            Self::TrashX265Hd => write!(f, "trash_x265_hd"),
            Self::TrashExtras => write!(f, "trash_extras"),
            Self::TrashNoGroup => write!(f, "trash_no_group"),
            Self::TrashAnimeLq => write!(f, "trash_anime_lq"),
            Self::TrashFilter => write!(f, "trash_filter"),
        }
    }
}

/// Structural rejection reasons from `structural_reject()`.
///
/// These check whether a release is structurally compatible with what was
/// requested (right season, right show, right year, etc.).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum StructuralRejectReason {
    TitleMismatch,
    CrossMediaTypeTv,
    UnairedEpisode,
    PublishDateTooOld,
    PublishDateBeforeShowYear,
    PublishDateBeforeMovieRelease,
    PublishDateBeforeMovieYear,
    YearMismatch,
    AuthorMismatch,
    PackRejectedLowEpisodeRatio,
    PackRejectedByPreference,
    SeasonMismatch,
    EpisodeWantPackRejected,
    EpisodeMismatch,
    NoEpisodeEvidence,
    RoundMismatch,
    LocationMismatch,
    SessionMismatch,
    SessionMissing,
    EventNotYetHeld,
    /// A movie still new enough to be in cinemas, from a release with no
    /// source tag: in that window an unlabelled copy is a CAM/TS.
    UnsourcedNewMovie,
}

impl StructuralRejectReason {
    /// Whether this rejection is strong enough that koblem should skip reviewing.
    ///
    /// Season/episode mismatches are definitive — it's the wrong content entirely.
    /// Title mismatches are softer — naming variance could mean the fuzzy check
    /// was wrong and koblem should have a look.
    pub fn is_hard_reject(self) -> bool {
        matches!(
            self,
            Self::SeasonMismatch
                | Self::EpisodeMismatch
                | Self::NoEpisodeEvidence
                | Self::CrossMediaTypeTv
                | Self::EpisodeWantPackRejected
                | Self::YearMismatch
                | Self::EventNotYetHeld
        )
    }
}

impl std::fmt::Display for StructuralRejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TitleMismatch => write!(f, "title_mismatch"),
            Self::CrossMediaTypeTv => write!(f, "cross_media_type_tv"),
            Self::UnairedEpisode => write!(f, "unaired_episode"),
            Self::PublishDateTooOld => write!(f, "publish_date_too_old"),
            Self::PublishDateBeforeShowYear => write!(f, "publish_date_before_show_year"),
            Self::PublishDateBeforeMovieRelease => write!(f, "publish_date_before_movie_release"),
            Self::PublishDateBeforeMovieYear => write!(f, "publish_date_before_movie_year"),
            Self::YearMismatch => write!(f, "year_mismatch"),
            Self::AuthorMismatch => write!(f, "author_mismatch"),
            Self::PackRejectedLowEpisodeRatio => write!(f, "pack_rejected_low_episode_ratio"),
            Self::PackRejectedByPreference => write!(f, "pack_rejected_by_preference"),
            Self::SeasonMismatch => write!(f, "season_mismatch"),
            Self::EpisodeWantPackRejected => write!(f, "episode_want_pack_rejected"),
            Self::EpisodeMismatch => write!(f, "episode_mismatch"),
            Self::NoEpisodeEvidence => write!(f, "no_episode_evidence"),
            Self::RoundMismatch => write!(f, "round_mismatch"),
            Self::LocationMismatch => write!(f, "location_mismatch"),
            Self::SessionMismatch => write!(f, "session_mismatch"),
            Self::SessionMissing => write!(f, "session_missing"),
            Self::EventNotYetHeld => write!(f, "event_not_yet_held"),
            Self::UnsourcedNewMovie => write!(f, "unsourced_new_movie"),
        }
    }
}

/// Contextual flags from cross-release batch analysis.
///
/// These are set by `contextual_analysis()` after individual scoring,
/// examining the batch as a whole for patterns invisible to per-release scoring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContextFlag {
    /// High quality outlier in a batch dominated by garbage quality.
    /// e.g., lone WEB-DL.1080p among 10 CAM releases = likely fake.
    SuspiciousQualityOutlier,
}

impl std::fmt::Display for ContextFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SuspiciousQualityOutlier => write!(f, "suspicious_quality_outlier"),
        }
    }
}

/// Parent rejection reason wrapping all category-specific reasons.
///
/// Used as the primary `reject_reasons` field on `ProcessedRelease`.
/// Koblem can match on the outer variant to decide review priority:
/// `Quality(_)` vs `Structural(_)` vs standalone reasons.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "category", content = "reason", rename_all = "snake_case")]
pub enum RejectReason {
    Quality(QualityRejectReason),
    Structural(StructuralRejectReason),
    NegativeVotes,
    CategoryMismatch,
    EpisodeNotNeeded,
    Context(ContextFlag),
}

impl RejectReason {
    pub fn is_quality(self) -> bool {
        matches!(self, Self::Quality(_))
    }

    pub fn is_structural(self) -> bool {
        matches!(self, Self::Structural(_))
    }

    /// Whether this rejection is strong enough that koblem should skip reviewing.
    pub fn is_hard_reject(self) -> bool {
        match self {
            Self::Quality(q) => q.is_hard_reject(),
            Self::Structural(s) => s.is_hard_reject(),
            Self::NegativeVotes | Self::CategoryMismatch => true,
            Self::EpisodeNotNeeded => true,
            Self::Context(_) => false,
        }
    }
}

impl From<QualityRejectReason> for RejectReason {
    fn from(r: QualityRejectReason) -> Self {
        Self::Quality(r)
    }
}

impl From<StructuralRejectReason> for RejectReason {
    fn from(r: StructuralRejectReason) -> Self {
        Self::Structural(r)
    }
}

impl From<ContextFlag> for RejectReason {
    fn from(f: ContextFlag) -> Self {
        Self::Context(f)
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quality(q) => write!(f, "{q}"),
            Self::Structural(s) => write!(f, "{s}"),
            Self::NegativeVotes => write!(f, "negative_votes"),
            Self::CategoryMismatch => write!(f, "category_mismatch"),
            Self::EpisodeNotNeeded => write!(f, "episode_not_needed"),
            Self::Context(c) => write!(f, "{c}"),
        }
    }
}

// ── Existing types ──────────────────────────────────────────────────────────

/// Structured identification of what a release matched against.
///
/// Replaces the opaque `matched_target_id` string (e.g., `"episode_4"`)
/// with a typed enum that consumers can use directly without parsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MatchedTarget {
    /// Single TV episode (from per-episode search or season episode fallback).
    Episode { season: String, episode: String },
    /// XEM scene-numbered episode (maps to a different TVDB episode).
    SceneEpisode {
        scene_season: u32,
        scene_episode: u32,
        tvdb_season: u32,
        tvdb_episode: u32,
    },
    /// Season pack.
    Season { season: String },
    /// Book or audiobook.
    Book,
    /// Movie.
    Movie,
    /// Sport event.
    SportEvent,
    /// Generic match (games, software, comics, custom patterns).
    Media,
}

/// Wire-serializable processed release with full scoring data.
///
/// Canonical output type for the unified scoring pipeline. Replaces
/// `ScoredSearchResult` with richer data (decision, reject reasons,
/// contextual flags) that downstream consumers need.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProcessedReleaseDto {
    // ── Release identity ──
    pub guid: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    pub size: i64,
    pub indexer: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer_source_id: Option<Uuid>,

    // ── Scoring decision ──
    pub decision: ScoringDecision,
    /// All rejection reasons (empty for candidates).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_reasons: Vec<RejectReason>,

    // ── Match scoring ──
    /// Pattern/target match score (0.0-1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_score: Option<f64>,
    /// The pattern/target string this was scored against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_pattern: Option<String>,
    /// Structured identification of what was matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_target: Option<MatchedTarget>,

    // ── Quality scoring ──
    /// Quality profile score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    /// Combined score (match + quality + bonuses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_score: Option<f64>,
    /// Parsed quality fields (ParsedQuality serialized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<serde_json::Value>,
    pub quality_rejected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality_reject_reasons: Vec<QualityRejectReason>,

    // ── Structural scoring ──
    pub structural_rejected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_reject_reason: Option<StructuralRejectReason>,

    // ── Episode/selection ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_key: Option<String>,

    // ── Contextual analysis ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_flags: Vec<ContextFlag>,
}
