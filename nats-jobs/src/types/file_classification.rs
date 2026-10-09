//! Shared file classification and metadata types.
//!
//! These types are produced by kobload's file classifier and included in
//! `DownloadFileInfo` payloads. Both kobload and kobbler import them from
//! here so the wire format can never go out of sync.

use serde::{Deserialize, Serialize};

use super::parsed::ParsedMedia;

/// Classification of a file's role within a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileClassification {
    /// The file's role in the release
    pub role: FileRole,
    /// Parsed metadata for this specific file (if different from release)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedMedia>,
    /// Confidence in classification (0.0-1.0)
    pub confidence: f64,
    /// Group key for files that belong together (same content, different formats)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
}

/// Role of a file within a release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    /// Primary movie/episode/book content
    MainContent,
    /// Same content, different format (epub vs mobi)
    AlternateFormat,
    /// Preview sample
    Sample,
    /// Subtitle file
    Subtitle,
    /// Behind the scenes, deleted scenes
    Extras,
    /// Movie/show trailer
    Trailer,
    /// Cover, poster, booklet
    Artwork,
    /// NFO, SFV, proof
    Metadata,
    /// Proof images/files
    Proof,
    /// Unable to determine
    Unknown,
}

/// Status of metadata extraction for a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MetadataExtractionStatus {
    Extracted,
    SkippedArchiveNested,
    ExtractionFailed,
    UnsupportedFormat,
    ToolNotAvailable,
}

/// Extracted metadata from actual file analysis (via mediainfo).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<MetadataExtractionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<SubtitleMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
}

/// Video stream metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VideoMetadata {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framerate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(default)]
    pub is_hdr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_format: Option<String>,
    #[serde(default)]
    pub is_interlaced: bool,
}

/// Audio track metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AudioMetadata {
    pub codec: String,
    pub channels: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Absolute stream index from the container (e.g., ffprobe/mediainfo StreamOrder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_index: Option<u32>,
}

/// Subtitle track metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubtitleMetadata {
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub is_forced: bool,
    #[serde(default)]
    pub is_default: bool,
    /// Absolute stream index from the container (e.g., ffprobe/mediainfo StreamOrder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_index: Option<u32>,
}
