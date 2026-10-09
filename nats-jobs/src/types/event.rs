use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// An event published to NATS for pub/sub consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event ID
    pub id: Uuid,
    /// Event type (e.g., "download.completed", "library.book_added")
    pub event_type: String,
    /// Event-specific payload
    pub payload: serde_json::Value,
    /// When the event occurred
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// Source system
    pub source: String,
}

impl Event {
    pub fn new(
        event_type: impl Into<String>,
        payload: serde_json::Value,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type: event_type.into(),
            payload,
            timestamp: OffsetDateTime::now_utc(),
            source: source.into(),
        }
    }
}
