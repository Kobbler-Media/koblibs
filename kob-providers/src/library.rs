use async_trait::async_trait;

use crate::types::{
    AudiobookDetail, AudiobookSummary, BuildMediaWantRequest, ShowDetail, ShowSummary,
};

/// Error type for provider operations.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("provider unavailable: {0}")]
    Unavailable(String),

    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}

/// Library provider for disambiguation lookups.
///
/// Implementations search a media library for shows/movies by title,
/// enabling disambiguation ("are there other shows with this name?").
#[async_trait]
pub trait LibraryProvider: Send + Sync {
    /// Search for shows matching a title query.
    async fn search_shows(&self, query: &str) -> Result<Vec<ShowSummary>, ProviderError>;

    /// Get full show detail including seasons, episodes, and quality profile.
    async fn get_show_detail(&self, show_id: &str) -> Result<Option<ShowDetail>, ProviderError>;

    /// Get the episode title for a specific show/season/episode.
    async fn get_episode_title(
        &self,
        show_id: &str,
        season: &str,
        episode: &str,
    ) -> Result<Option<String>, ProviderError>;

    /// Build a MediaWant for a show/season/episodes — identical to what would
    /// be sent to the download service when actually wanting episodes.
    /// Returns the MediaWant as opaque JSON (caller passes it straight through).
    async fn build_media_want(
        &self,
        request: &BuildMediaWantRequest,
    ) -> Result<serde_json::Value, ProviderError>;

    /// Search for audiobooks by title/narrator.
    ///
    /// Default implementation returns an empty list so existing providers that
    /// don't expose audiobooks don't have to implement this. The HTTP provider
    /// overrides it.
    async fn search_audiobooks(
        &self,
        _query: &str,
    ) -> Result<Vec<AudiobookSummary>, ProviderError> {
        Ok(Vec::new())
    }

    /// Get an audiobook's full detail (chapters + file paths).
    ///
    /// Default implementation returns `None` — same rationale as search_audiobooks.
    async fn get_audiobook(&self, _id: &str) -> Result<Option<AudiobookDetail>, ProviderError> {
        Ok(None)
    }

    /// Cut a window from a chapter and return the WAV bytes.
    ///
    /// Used by koblem to obtain a reference clip for TTS voice cloning
    /// without needing local ffmpeg deps. Default implementation returns
    /// `Unavailable` — providers that don't support audiobook clipping
    /// (everything except the kobbler HTTP impl) inherit it.
    async fn fetch_audiobook_clip(
        &self,
        _id: &str,
        _chapter_index: u32,
        _offset_seconds: f64,
        _duration_seconds: f64,
    ) -> Result<bytes::Bytes, ProviderError> {
        Err(ProviderError::Unavailable(
            "audiobook clip slicing not supported by this provider".into(),
        ))
    }
}
