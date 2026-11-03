//! HTTP client for calling a koblem instance.

use crate::embed::{EmbeddingsRequest, EmbeddingsResponse};
use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, HealthResponse, ReviewRequest, ReviewResponse,
    TextEmbeddingsRequest, TextEmbeddingsResponse, VisionClassifyFromEmbedRequest,
    VisionClassifyFromEmbedResponse, VisionTextEmbedRequest, VisionTextEmbedResponse,
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

    /// Embed prompts verbatim (the legacy `{prompts}` shape of
    /// `/v1/text_embeddings`). Prefer [`Self::embed_texts`], which applies the
    /// task prefix and reports the model.
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

    /// Embed texts under a task (`/v1/text_embeddings`), served by koblem's
    /// always-on text instance. A 413 (too long, no `chunk`) arrives as
    /// `Error::Upstream(413, body)` with a `TextTooLongError` body.
    pub async fn embed_texts(
        &self,
        request: &TextEmbeddingsRequest,
    ) -> Result<TextEmbeddingsResponse, Error> {
        self.post_json("/v1/text_embeddings", request).await
    }

    /// OpenAI-shape `/v1/embeddings`: strings and/or image content parts. Images
    /// go through koblem's GPU gate; text-only requests do not.
    pub async fn embeddings(
        &self,
        request: &EmbeddingsRequest,
    ) -> Result<EmbeddingsResponse, Error> {
        self.post_json("/v1/embeddings", request).await
    }

    async fn post_json<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        request: &Req,
    ) -> Result<Resp, Error> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base_url))
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

    /// Score pre-computed image embeddings against text prompts
    /// (`/v1/classify_from_embeddings`): cosine similarity, no GPU.
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
