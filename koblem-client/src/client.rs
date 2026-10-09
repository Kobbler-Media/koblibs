//! HTTP client for calling a koblem instance.

use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, HealthResponse, ReviewRequest, ReviewResponse,
    VisionClassifyFromEmbedRequest, VisionClassifyFromEmbedResponse, VisionTextEmbedRequest,
    VisionTextEmbedResponse,
};
use tracing::warn;

/// Client for communicating with a koblem instance.
#[derive(Clone)]
pub struct KoblemClient {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request to koblem failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("koblem returned {0}: {1}")]
    Upstream(u16, String),
}

impl KoblemClient {
    /// Create a new client pointing at the given koblem base URL.
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client");
        Self { http, base_url }
    }

    /// Get the base URL this client is configured to use.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Check if koblem is healthy and llama-server is connected.
    pub async fn health(&self) -> Result<HealthResponse, Error> {
        let resp = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream(status.as_u16(), body));
        }

        Ok(resp.json().await?)
    }

    /// Non-failing health check — returns true if koblem is up and healthy.
    pub async fn is_available(&self) -> bool {
        match self.health().await {
            Ok(h) => h.status == "ok",
            Err(e) => {
                warn!("koblem health check failed: {e}");
                false
            }
        }
    }

    /// Send a non-streaming chat completion request.
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, Error> {
        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream(status.as_u16(), body));
        }

        Ok(resp.json().await?)
    }

    /// Send a batch of candidates for LLM review.
    pub async fn review_candidates(
        &self,
        request: &ReviewRequest,
    ) -> Result<ReviewResponse, Error> {
        let resp = self
            .http
            .post(format!("{}/api/v1/review-candidates", self.base_url))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream(status.as_u16(), body));
        }

        Ok(resp.json().await?)
    }

    /// Encode a batch of text prompts into the SigLIP shared embedding space.
    /// Calls koblem's `/v1/text_embeddings` proxy (which holds the GPU vision guard).
    pub async fn text_embeddings(
        &self,
        request: &VisionTextEmbedRequest,
    ) -> Result<VisionTextEmbedResponse, Error> {
        let resp = self
            .http
            .post(format!("{}/v1/text_embeddings", self.base_url))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream(status.as_u16(), body));
        }

        Ok(resp.json().await?)
    }

    /// Classify pre-computed image embeddings against text prompts.
    /// Calls koblem's `/v1/classify_from_embeddings` proxy. Server-side this reproduces
    /// the full SigLIP `sigmoid(img @ txt.T * scale + bias)` forward, so output matches
    /// `/v1/classify` for the same images within fp16 noise.
    pub async fn classify_from_embeddings(
        &self,
        request: &VisionClassifyFromEmbedRequest,
    ) -> Result<VisionClassifyFromEmbedResponse, Error> {
        let resp = self
            .http
            .post(format!("{}/v1/classify_from_embeddings", self.base_url))
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Upstream(status.as_u16(), body));
        }

        Ok(resp.json().await?)
    }
}
