pub mod capabilities;
pub mod download;
pub mod event;
pub mod file_classification;
pub mod http;
pub mod ingest;
pub mod job;
pub mod media;
pub mod parsed;
pub mod scoring;
pub mod search;
pub mod stream;
pub mod want;
pub mod watch;
pub mod watch_responses;

pub use capabilities::{DownloadClientCapabilities, IndexerCapabilities};
pub use download::{DownloadCompletedEvent, DownloadFileInfo, FileGroupInfo, ReleaseInfo};
pub use event::Event;
pub use file_classification::{
    AudioMetadata, FileClassification, FileMetadata, FileRole, MetadataExtractionStatus,
    SubtitleMetadata, VideoMetadata,
};
pub use http::{
    CreateDownloadRequest, CreateDownloadResponse, SearchAndDownloadRequest,
    SearchAndDownloadResponse, WatchSyncItem, WatchSyncRequest, WatchSyncResponse,
};
pub use ingest::{IngestResultEvent, IngestResultStatus};
pub use job::{Job, JobContext, JobStatus, JobStatusUpdate};
pub use media::{
    BookIdentifiers, EpisodeRef, MediaKind, MediaWant, MovieIdentifiers, QualityOverrides,
    TvIdentifiers,
};
pub use parsed::{
    AudiobookParsed, BookParsed, GameParsed, MovieParsed, MusicParsed, ParsedMedia, ParsedQuality,
    ParsedRelease, ReleaseFlags, SoftwareParsed, SpecialType, SportParsed, TvParsed, XxxParsed,
};
pub use scoring::{
    ContextFlag, MatchedTarget, ProcessedReleaseDto, QualityRejectReason, RejectReason,
    ScoringDecision, StructuralRejectReason,
};
pub use search::{SearchRequest, SearchResponse, SearchResult};
pub use stream::{
    StreamFile, StreamOption, StreamOutcomeReport, StreamPlayback, StreamPreference,
    StreamResolveRequest, StreamResolveResponse, StreamSearchRequest, StreamSearchResponse,
    StreamSourceType,
};
pub use want::{ExcludedRelease, WantRequest, WantResponse, WantStatus};
pub use watch::{
    GrabHeldMatch, MatchTarget, SearchPatternNow, WatchMatchedEvent, WatchPatternDelete,
    WatchPatternDeletedEvent, WatchPatternUpsert,
};
pub use watch_responses::{
    EpisodeProgress, ListWatchMatchesResponse, ListWatchPatternsResponse, WatchMatchResponse,
    WatchPatternDetailResponse, WatchPatternResponse,
};
