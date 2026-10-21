//! NATS JetStream-based job system for Kobbler and Kobload.
//!
//! This crate provides a unified job queue and event system using NATS JetStream,
//! enabling cross-service communication and reliable job processing.
//!
//! # Architecture
//!
//! The system uses three NATS stream patterns:
//!
//! - **JOBS stream** (work queue): Each job is processed by exactly one worker.
//!   Jobs are published to `jobs.{target}.{job_type}` (e.g., `jobs.kobbler.directory_scan`).
//!
//! - **EVENTS stream** (pub/sub): Events can be consumed by multiple subscribers.
//!   Events are published to `events.{event_type}` (e.g., `events.download.completed`).
//!
//! - **Request/Reply**: For synchronous operations like manual search.
//!   Uses `requests.{service}.{operation}` subjects.
//!
//! # Usage
//!
//! ```rust,no_run
//! use nats_jobs::{NatsClient, NatsConfig, Publisher, JobWorker, WorkerConfig, JobHandler};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to NATS
//! let config = NatsConfig::from_env("my-service");
//! let nats = NatsClient::connect(&config).await?;
//!
//! // Create a publisher
//! let publisher = Publisher::new(nats.clone(), "kobbler");
//!
//! // Enqueue a job
//! publisher.enqueue_job("kobbler", "directory_scan", serde_json::json!({
//!     "watch_directory_id": "some-uuid"
//! })).await?;
//!
//! // Publish an event
//! publisher.publish_event("library.book_added", serde_json::json!({
//!     "book_id": "some-uuid"
//! })).await?;
//! # Ok(())
//! # }
//! ```

pub mod cancel;
pub mod client;
pub mod db;
pub mod publisher;
pub mod scheduler;
pub mod test_fixtures;
pub mod types;
pub mod worker;

// Re-export main types
pub use cancel::{CancelRegistry, check_cancelled, purge_nats_job};
pub use client::{NatsClient, NatsConfig, NatsError};
pub use db::{JobCounts, JobStatusDb, JobStatusRow};
pub use publisher::{OptionalPublisher, Publisher};
pub use scheduler::{ScheduledJob, Scheduler};
pub use types::{
    AudioMetadata, AudiobookParsed, BookIdentifiers, BookParsed, ContextFlag,
    CreateDownloadRequest, CreateDownloadResponse, DownloadClientCapabilities,
    DownloadCompletedEvent, DownloadFileInfo, EpisodeProgress, EpisodeRef, Event, ExcludedRelease,
    FileClassification, FileGroupInfo, FileMetadata, FileRole, GameParsed, GrabHeldMatch,
    IndexerCapabilities, IngestResultEvent, IngestResultStatus, Job, JobContext, JobStatus,
    JobStatusUpdate, ListWatchMatchesResponse, ListWatchPatternsResponse, MatchTarget,
    MatchedTarget, MediaKind, MediaWant, MetadataExtractionStatus, MovieIdentifiers, MovieParsed,
    MusicParsed, ParsedMedia, ParsedQuality, ParsedRelease, ProcessedReleaseDto, QualityOverrides,
    QualityRejectReason, RejectReason, ReleaseFlags, ReleaseInfo, ScoringDecision,
    SearchAndDownloadRequest, SearchAndDownloadResponse, SearchPatternNow, SearchRequest,
    SearchResponse, SearchResult, SoftwareParsed, SpecialType, SportEventMeta, SportIdentifiers,
    SportParsed, SportTeam, StreamAvailability, StreamFile, StreamOption, StreamOutcomeReport,
    StreamPlayback, StreamPreference, StreamPriority, StreamResolveRequest, StreamResolveResponse,
    StreamSearchRequest, StreamSearchResponse, StreamSourceType, StructuralRejectReason,
    SubtitleMetadata, TvIdentifiers, TvParsed, VideoMetadata, WantRequest, WantResponse,
    WantStatus, WatchMatchResponse, WatchMatchedEvent, WatchPatternDelete,
    WatchPatternDeletedEvent, WatchPatternDetailResponse, WatchPatternResponse, WatchPatternUpsert,
    WatchSyncItem, WatchSyncRequest, WatchSyncResponse, XxxParsed,
};
pub use worker::{
    JobError, JobHandler, JobStatusChange, JobWorker, ProgressReporter, StatusCallback,
    WorkerConfig,
};
