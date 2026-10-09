use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;

use crate::library::{LibraryProvider, ProviderError};
use crate::releases::{
    InterpretInput, InterpretedRelease, ReleaseProvider, ReleaseSearchRequest, ReleaseSearchResults,
};
use crate::types::{
    AudiobookDetail, AudiobookSummary, BuildMediaWantRequest, ShowDetail, ShowSummary,
};

const CACHE_TTL: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP-based library provider that calls a remote service.
pub struct HttpLibraryProvider {
    http: reqwest::Client,
    base_url: String,
    token: String,
    show_cache: Mutex<HashMap<String, (Instant, Vec<ShowSummary>)>>,
    episode_cache: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl HttpLibraryProvider {
    pub fn new(base_url: String, token: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("failed to build HTTP client");

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            show_cache: Mutex::new(HashMap::new()),
            episode_cache: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Deserialize)]
struct EpisodeTitleResponse {
    title: Option<String>,
}

#[async_trait]
impl LibraryProvider for HttpLibraryProvider {
    async fn search_shows(&self, query: &str) -> Result<Vec<ShowSummary>, ProviderError> {
        let cache_key = query.to_lowercase();

        // Check cache
        if let Some((at, cached)) = self.show_cache.lock().unwrap().get(&cache_key)
            && at.elapsed() < CACHE_TTL
        {
            return Ok(cached.clone());
        }

        let url = format!("{}/api/v1/providers/library/shows/search", self.base_url);

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .query(&[("q", query)])
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        let shows: Vec<ShowSummary> = resp.json().await?;

        self.show_cache
            .lock()
            .unwrap()
            .insert(cache_key, (Instant::now(), shows.clone()));

        Ok(shows)
    }

    async fn get_show_detail(&self, show_id: &str) -> Result<Option<ShowDetail>, ProviderError> {
        let url = format!(
            "{}/api/v1/providers/library/shows/{}",
            self.base_url, show_id
        );

        let resp = self.http.get(&url).bearer_auth(&self.token).send().await?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(Some(resp.json().await?))
    }

    async fn get_episode_title(
        &self,
        show_id: &str,
        season: &str,
        episode: &str,
    ) -> Result<Option<String>, ProviderError> {
        let cache_key = format!("{show_id}:{season}:{episode}");

        // Check cache
        if let Some((at, cached)) = self.episode_cache.lock().unwrap().get(&cache_key)
            && at.elapsed() < CACHE_TTL
        {
            return Ok(cached.clone());
        }

        let url = format!(
            "{}/api/v1/providers/library/shows/{}/episodes/{}/{}/title",
            self.base_url, show_id, season, episode
        );

        let resp = self.http.get(&url).bearer_auth(&self.token).send().await?;

        if resp.status().as_u16() == 404 {
            self.episode_cache
                .lock()
                .unwrap()
                .insert(cache_key, (Instant::now(), None));
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        let body: EpisodeTitleResponse = resp.json().await?;

        self.episode_cache
            .lock()
            .unwrap()
            .insert(cache_key, (Instant::now(), body.title.clone()));

        Ok(body.title)
    }

    async fn build_media_want(
        &self,
        request: &BuildMediaWantRequest,
    ) -> Result<serde_json::Value, ProviderError> {
        let url = format!(
            "{}/api/v1/providers/library/build-media-want",
            self.base_url
        );

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(resp.json().await?)
    }

    async fn search_audiobooks(&self, query: &str) -> Result<Vec<AudiobookSummary>, ProviderError> {
        let url = format!("{}/api/v1/library/audiobooks", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .query(&[("q", query)])
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(resp.json().await?)
    }

    async fn get_audiobook(&self, id: &str) -> Result<Option<AudiobookDetail>, ProviderError> {
        let url = format!("{}/api/v1/library/audiobooks/{}", self.base_url, id);
        let resp = self.http.get(&url).bearer_auth(&self.token).send().await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(Some(resp.json().await?))
    }

    async fn fetch_audiobook_clip(
        &self,
        id: &str,
        chapter_index: u32,
        offset_seconds: f64,
        duration_seconds: f64,
    ) -> Result<bytes::Bytes, ProviderError> {
        let url = format!("{}/api/v1/library/audiobooks/{}/clip", self.base_url, id);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .query(&[
                ("chapter", chapter_index.to_string()),
                ("offset", offset_seconds.to_string()),
                ("duration", duration_seconds.to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {status}: {body}"
            )));
        }
        Ok(resp.bytes().await?)
    }
}

// ── HttpReleaseProvider ─────────────────────────────────────────────

const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTP-based release provider that calls a remote service (e.g. kobload).
pub struct HttpReleaseProvider {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl HttpReleaseProvider {
    pub fn new(base_url: String, token: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(SEARCH_TIMEOUT)
            .build()
            .expect("failed to build HTTP client");

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }
}

#[async_trait]
impl ReleaseProvider for HttpReleaseProvider {
    async fn search_releases(
        &self,
        request: &ReleaseSearchRequest,
    ) -> Result<ReleaseSearchResults, ProviderError> {
        let url = format!("{}/api/v1/providers/releases/search", self.base_url);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(resp.json().await?)
    }

    async fn interpret_releases(
        &self,
        releases: &[InterpretInput],
    ) -> Result<Vec<InterpretedRelease>, ProviderError> {
        let url = format!("{}/api/v1/providers/releases/interpret", self.base_url);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(releases)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::UnexpectedResponse(format!(
                "status {}",
                resp.status()
            )));
        }

        Ok(resp.json().await?)
    }
}
