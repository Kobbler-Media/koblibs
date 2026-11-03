//! Embedding wire types and EmbeddingGemma-2 prompt rules, shared by koblem (which
//! serves embeddings) and its callers (kobbler, which may also talk to a
//! llama-server directly).
//!
//! The wire shape is the OpenAI `/v1/embeddings` one that llama-server speaks:
//! `input` is a list whose items are either plain strings or `{content: [...]}`
//! arrays mixing text and images. Text and image vectors share one space.
//!
//! EmbeddingGemma-2 expects a task prefix on text, written by the caller — the
//! server embeds exactly the string it is given. [`eg2_text`] is the one place
//! those prefixes live. Images take no prefix.

use serde::{Deserialize, Serialize};

/// Model id recorded next to stored vectors, so a model change reads as stale.
pub const EG2_MODEL: &str = "google/embeddinggemma-2";
/// Native output width (Matryoshka-truncatable to 512/256/128, then renormalise).
pub const EG2_DIM: usize = 768;

/// What a text is for, which selects its prompt prefix. Queries and documents
/// must be embedded with matching conventions or retrieval quietly degrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedTask {
    /// A search query, matched against [`EmbedTask::Document`] texts.
    Query,
    /// A stored text (summary, email, …). Takes an optional title.
    Document,
    QuestionAnswering,
    FactChecking,
    Classification,
    Clustering,
    Similarity,
    CodeRetrieval,
    /// Embed the text exactly as given.
    Raw,
}

/// The string to embed for `text` under `task`. `title` is used by
/// [`EmbedTask::Document`] only; `None` becomes the model's `title: none`.
pub fn eg2_text(task: EmbedTask, title: Option<&str>, text: &str) -> String {
    let query = |name: &str| format!("task: {name} | query: {text}");
    match task {
        EmbedTask::Query => query("search result"),
        EmbedTask::Document => {
            let title = title
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("none");
            format!("title: {title} | text: {text}")
        }
        EmbedTask::QuestionAnswering => query("question answering"),
        EmbedTask::FactChecking => query("fact checking"),
        EmbedTask::Classification => query("classification"),
        EmbedTask::Clustering => query("clustering"),
        EmbedTask::Similarity => query("sentence similarity"),
        EmbedTask::CodeRetrieval => query("code retrieval"),
        EmbedTask::Raw => text.to_string(),
    }
}

/// `POST /v1/embeddings` body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingsRequest {
    pub input: Vec<EmbeddingInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Text(String),
    Content { content: Vec<ContentPart> },
}

impl EmbeddingInput {
    /// A single image, given as a `data:` (or http) URL. See [`image_data_url`].
    pub fn image(url: impl Into<String>) -> Self {
        Self::Content {
            content: vec![ContentPart::ImageUrl {
                image_url: ImageUrl { url: url.into() },
            }],
        }
    }

    pub fn has_image(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Content { content } => content
                .iter()
                .any(|p| !matches!(p, ContentPart::Text { .. })),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

/// `data:<mime>;base64,<b64>` — callers base64-encode with their own crate.
pub fn image_data_url(mime: &str, base64: &str) -> String {
    format!("data:{mime};base64,{base64}")
}

/// `POST /v1/embeddings` response (OpenAI shape).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingsResponse {
    #[serde(default)]
    pub object: Option<String>,
    pub data: Vec<EmbeddingData>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<EmbeddingUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingData {
    #[serde(default)]
    pub object: Option<String>,
    pub embedding: Vec<f32>,
    pub index: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl EmbeddingsResponse {
    /// Vectors in request order (servers may answer out of order).
    pub fn into_vectors(mut self) -> Vec<Vec<f32>> {
        self.data.sort_by_key(|d| d.index);
        self.data.into_iter().map(|d| d.embedding).collect()
    }
}

/// Cosine similarity. EG2 output is already L2-normalised, but stored or
/// truncated vectors may not be.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_match_the_model_card() {
        assert_eq!(
            eg2_text(EmbedTask::Query, None, "heist movie"),
            "task: search result | query: heist movie"
        );
        assert_eq!(
            eg2_text(
                EmbedTask::Document,
                Some("Alien"),
                "A crew meets a lifeform."
            ),
            "title: Alien | text: A crew meets a lifeform."
        );
        assert_eq!(
            eg2_text(EmbedTask::Document, Some("  "), "x"),
            "title: none | text: x"
        );
        assert_eq!(eg2_text(EmbedTask::Raw, None, "as is"), "as is");
    }

    #[test]
    fn input_round_trips_llama_server_shape() {
        let req = EmbeddingsRequest {
            input: vec![
                EmbeddingInput::Text("hello".into()),
                EmbeddingInput::image(image_data_url("image/jpeg", "AAAA")),
            ],
            model: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"input": ["hello", {"content": [
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AAAA"}}
            ]}]})
        );
        let back: EmbeddingsRequest = serde_json::from_value(v).unwrap();
        assert!(!back.input[0].has_image());
        assert!(back.input[1].has_image());
    }

    #[test]
    fn response_sorts_by_index() {
        let r: EmbeddingsResponse = serde_json::from_value(serde_json::json!({
            "object": "list",
            "data": [{"embedding": [2.0], "index": 1}, {"embedding": [1.0], "index": 0}],
            "usage": {"prompt_tokens": 3, "total_tokens": 3}
        }))
        .unwrap();
        assert_eq!(r.into_vectors(), vec![vec![1.0], vec![2.0]]);
    }
}
