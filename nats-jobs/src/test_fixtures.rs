//! Contract test fixtures for cross-project type safety.
//!
//! These fixtures provide canonical JSON shapes for types shared between
//! Kobbler and Kobload. Both projects can deserialize these fixtures to
//! verify contract compatibility.

use crate::{
    BookIdentifiers, CreateDownloadRequest, CreateDownloadResponse, DownloadCompletedEvent,
    DownloadFileInfo, MatchTarget, MediaWant, ProcessedReleaseDto, SearchAndDownloadRequest,
    SearchAndDownloadResponse, SearchRequest, SearchResponse, SearchResult, WantRequest,
    WantResponse, WantStatus, WatchMatchedEvent, WatchPatternDelete, WatchPatternDeletedEvent,
    WatchPatternUpsert, WatchSyncItem, WatchSyncRequest, WatchSyncResponse,
};

/// Fixed UUIDs for deterministic test fixtures.
pub mod ids {
    use uuid::Uuid;

    pub fn book_id() -> Uuid {
        Uuid::parse_str("01234567-0123-0123-0123-0123456789ab").unwrap()
    }

    pub fn download_id() -> Uuid {
        Uuid::parse_str("aabbccdd-aabb-aabb-aabb-aabbccddeeff").unwrap()
    }

    pub fn watch_pattern_id() -> Uuid {
        Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap()
    }

    pub fn idempotency_key() -> Uuid {
        Uuid::parse_str("99999999-8888-7777-6666-555544443333").unwrap()
    }

    pub fn replaces_download_id() -> Uuid {
        Uuid::parse_str("eeeeeeee-dddd-cccc-bbbb-aaaaaaaaaaaa").unwrap()
    }

    pub fn indexer_source_id() -> Uuid {
        Uuid::parse_str("fedcba98-7654-3210-fedc-ba9876543210").unwrap()
    }
}

/// Sample WantRequest (Kobbler -> Kobload).
pub fn sample_want_request() -> WantRequest {
    WantRequest {
        source_system: "kobbler".to_string(),
        source_id: ids::book_id().to_string(),
        callback_url: "http://kobbler:6766/api/v1/admin/ingest/kobload/test-token".to_string(),
        auth_token: None,
        query: None,
        patterns: vec!["Brandon Sanderson Mistborn".to_string()],
        match_targets: vec![MatchTarget {
            id: ids::book_id().to_string(),
            target: "Brandon Sanderson Mistborn The Final Empire".to_string(),
            min_score: Some(0.8),
            semantic_target: None,
        }],
        match_mode: "fuzzy".to_string(),
        min_score: 0.75,
        early_exit_score: 0.95,
        media_type: Some("ebooks".to_string()),
        auto_download: true,
        limit: 50,
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id(),
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson"
        })),
        media_want: Some(MediaWant::Book {
            title: "Mistborn: The Final Empire".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: Some("9780765311788".to_string()),
            format_preferences: vec!["epub".to_string(), "kepub".to_string()],
            identifiers: Some(BookIdentifiers {
                isbn10: Some("0765311780".to_string()),
                isbn13: Some("9780765311788".to_string()),
                hardcover_id: None,
                goodreads_id: Some("68428".to_string()),
                openlibrary_id: None,
            }),
            series: Some("Mistborn".to_string()),
            series_position: Some("1".to_string()),
            release_date: None,
            alternate_titles: None,
        }),
        upgrade_allowed: None,
        manual: false,
        excluded_releases: None,
        search_not_before: None,
    }
}

/// Sample ProcessedReleaseDto.
pub fn sample_processed_release_dto() -> ProcessedReleaseDto {
    use crate::ScoringDecision;

    ProcessedReleaseDto {
        guid: "abc-123".to_string(),
        title: "Mistborn.The.Final.Empire.Brandon.Sanderson.EPUB".to_string(),
        file_name: Some("Mistborn.The.Final.Empire.epub".to_string()),
        size: 1_500_000,
        indexer: "TestIndexer".to_string(),
        protocol: "usenet".to_string(),
        download_url: Some("https://example.com/download/abc-123.nzb".to_string()),
        magnet_url: None,
        info_url: Some("https://example.com/details/abc-123".to_string()),
        publish_date: Some("2024-01-15T00:00:00Z".to_string()),
        indexer_source_id: Some(ids::indexer_source_id()),
        decision: ScoringDecision::Candidate,
        reject_reasons: vec![],
        match_score: Some(0.92),
        matched_pattern: Some("Brandon Sanderson Mistborn The Final Empire".to_string()),
        matched_target: None,
        quality_score: Some(0.85),
        total_score: Some(0.89),
        quality: Some(serde_json::json!({"source": "usenet", "format": "epub"})),
        quality_rejected: false,
        quality_reject_reasons: vec![],
        structural_rejected: false,
        structural_reject_reason: None,
        episode_key: None,
        context_flags: vec![],
    }
}

/// Sample WantResponse (Kobload -> Kobbler) — downloading status.
pub fn sample_want_response_downloading() -> WantResponse {
    WantResponse {
        source_system: Some("kobbler".to_string()),
        source_id: Some(ids::book_id().to_string()),
        watch_pattern_id: ids::watch_pattern_id(),
        status: WantStatus::Downloading,
        download_id: Some(ids::download_id()),
        selected_release: Some(sample_processed_release_dto()),
        results_found: 5,
        best_score: 0.92,
        matched_pattern: Some("Brandon Sanderson Mistborn The Final Empire".to_string()),
        matched_target: None,
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id(),
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson"
        })),
        episodes_downloaded: None,
    }
}

/// Sample WantResponse for watching status (no match found yet).
pub fn sample_want_response_watching() -> WantResponse {
    WantResponse {
        source_system: Some("kobbler".to_string()),
        source_id: Some(ids::book_id().to_string()),
        watch_pattern_id: ids::watch_pattern_id(),
        status: WantStatus::Watching,
        download_id: None,
        selected_release: None,
        results_found: 0,
        best_score: 0.0,
        matched_pattern: None,
        matched_target: None,
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id()
        })),
        episodes_downloaded: None,
    }
}

/// Sample SearchRequest (Kobbler -> Kobload).
pub fn sample_search_request() -> SearchRequest {
    SearchRequest {
        query: "Brandon Sanderson Mistborn".to_string(),
        media_type: Some("ebooks".to_string()),
        protocol: None,
        limit: Some(50),
        offset: None,
        media_want: Some(MediaWant::Book {
            title: "Mistborn: The Final Empire".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        }),
    }
}

/// Sample SearchResponse (Kobload -> Kobbler).
pub fn sample_search_response() -> SearchResponse {
    SearchResponse {
        results: vec![SearchResult {
            guid: "abc-123".to_string(),
            title: "Mistborn The Final Empire (epub)".to_string(),
            indexer: "TestIndexer".to_string(),
            size: Some(1_500_000),
            publish_date: Some("2024-01-15T00:00:00Z".to_string()),
            download_url: Some("https://example.com/download/abc-123.nzb".to_string()),
            magnet_url: None,
            protocol: Some("usenet".to_string()),
            info_url: Some("https://example.com/details/abc-123".to_string()),
            info_hash: None,
            indexer_id: Some("42".to_string()),
            indexer_type: Some("newznab".to_string()),
            seeders: None,
            leechers: None,
            grabs: Some(150),
        }],
        errors: vec![],
    }
}

/// Sample WatchPatternUpsert (Kobbler -> Kobload via NATS).
pub fn sample_watch_pattern_upsert() -> WatchPatternUpsert {
    WatchPatternUpsert {
        source_system: "kobbler".to_string(),
        source_id: ids::book_id().to_string(),
        callback_url: "http://kobbler:6766/api/v1/admin/ingest/kobload/test-token".to_string(),
        patterns: vec![],
        media_want: Some(MediaWant::Book {
            title: "Mistborn: The Final Empire".to_string(),
            author: Some("Brandon Sanderson".to_string()),
            isbn: Some("9780765311788".to_string()),
            format_preferences: vec!["epub".to_string()],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        }),
        match_targets: vec![],
        match_mode: "fuzzy".to_string(),
        min_score: Some(0.75),
        auto_download: true,
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id(),
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson"
        })),
        enabled: true,
    }
}

/// Sample WatchPatternDeletedEvent (Kobload -> Kobbler via NATS).
pub fn sample_watch_pattern_deleted_event() -> WatchPatternDeletedEvent {
    WatchPatternDeletedEvent {
        source_system: "kobbler".to_string(),
        source_id: ids::book_id().to_string(),
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id()
        })),
    }
}

/// Sample WatchPatternDelete (Kobbler -> Kobload via NATS).
pub fn sample_watch_pattern_delete() -> WatchPatternDelete {
    WatchPatternDelete {
        source_system: "kobbler".to_string(),
        source_id: ids::book_id().to_string(),
    }
}

/// Sample WatchMatchedEvent (Kobload -> Kobbler via NATS).
pub fn sample_watch_matched_event() -> WatchMatchedEvent {
    WatchMatchedEvent {
        watch_pattern_id: ids::watch_pattern_id(),
        source_system: "kobbler".to_string(),
        source_id: ids::book_id().to_string(),
        match_score: 0.92,
        matched_pattern: "Brandon Sanderson Mistborn The Final Empire".to_string(),
        release_title: "Mistborn.The.Final.Empire.Brandon.Sanderson.EPUB".to_string(),
        release_guid: "release-guid-abc".to_string(),
        download_id: Some(ids::download_id()),
        action: "downloaded".to_string(),
        metadata: Some(serde_json::json!({
            "book_id": ids::book_id()
        })),
        is_upgrade: false,
        quality_score: Some(0.85),
        replaces_download_id: None,
        episode_key: None,
    }
}

/// Sample DownloadCompletedEvent (Kobload -> Kobbler via NATS/HTTP callback).
pub fn sample_download_completed() -> DownloadCompletedEvent {
    DownloadCompletedEvent {
        download_id: ids::download_id(),
        status: "completed".to_string(),
        name: "Mistborn.The.Final.Empire-GROUP".to_string(),
        metadata: serde_json::json!({
            "book_id": ids::book_id(),
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson"
        }),
        files: vec![DownloadFileInfo {
            id: None,
            path: "Mistborn.The.Final.Empire.epub".to_string(),
            absolute_path: Some(
                "/downloads/complete/Mistborn.The.Final.Empire.epub".to_string(),
            ),
            url: Some("http://kobload:7080/api/v1/downloads/aabbccdd-aabb-aabb-aabb-aabbccddeeff/files/Mistborn.The.Final.Empire.epub".to_string()),
            size: 1_500_000,
            extension: "epub".to_string(),
            mime_type: Some("application/epub+zip".to_string()),
            score: Some(95),
            classification: None,
            file_metadata: None,
            matched_episode: None,
        }],
        file_groups: None,
        release_interpretation: None,
        error: None,
        recoverable_error: None,
        idempotency_key: Some(ids::idempotency_key()),
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info: None,
    }
}

/// Sample DownloadCompletedEvent with a failure status.
pub fn sample_download_failed() -> DownloadCompletedEvent {
    DownloadCompletedEvent {
        download_id: ids::download_id(),
        status: "failed".to_string(),
        name: String::new(),
        metadata: serde_json::json!({
            "book_id": ids::book_id()
        }),
        files: vec![],
        file_groups: None,
        release_interpretation: None,
        error: Some("Download client reported failure: torrent removed".to_string()),
        recoverable_error: None,
        idempotency_key: None,
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info: None,
    }
}

/// Sample callback JSON as consumers would receive it (DownloadCompletedEvent shape).
pub fn sample_ingest_callback_json() -> serde_json::Value {
    serde_json::json!({
        "download_id": ids::download_id(),
        "status": "completed",
        "metadata": {
            "book_id": ids::book_id(),
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson",
            "series": "Mistborn",
            "series_index": 1.0
        },
        "files": [
            {
                "path": "Mistborn.The.Final.Empire.epub",
                "size": 1500000,
                "extension": "epub",
                "mime_type": "application/epub+zip",
                "score": 95
            }
        ],
        "file_groups": null,
        "error": null,
        "recoverable_error": null,
        "idempotency_key": ids::idempotency_key(),
        "is_upgrade": false
    })
}

/// Sample CreateDownloadRequest.
pub fn sample_create_download_request() -> CreateDownloadRequest {
    CreateDownloadRequest {
        source: "kobbler".to_string(),
        protocol: "usenet".to_string(),
        name: "Mistborn.The.Final.Empire.epub".to_string(),
        download_url: "https://example.com/download/abc-123.nzb".to_string(),
        callback_url: "http://kobbler:6766/api/v1/admin/ingest/kobload/test-token".to_string(),
        auth_token: Some("test-token".to_string()),
        metadata: serde_json::json!({"book_id": ids::book_id()}),
        release: None,
    }
}

/// Sample CreateDownloadResponse.
pub fn sample_create_download_response() -> CreateDownloadResponse {
    CreateDownloadResponse {
        id: ids::download_id(),
        status: "pending".to_string(),
    }
}

/// Sample WatchSyncRequest.
pub fn sample_watch_sync_request() -> WatchSyncRequest {
    WatchSyncRequest {
        source_system: "kobbler".to_string(),
        items: vec![WatchSyncItem {
            source_id: ids::book_id().to_string(),
            callback_url: "http://kobbler:6766/api/v1/admin/ingest/kobload/test-token".to_string(),
            patterns: vec!["Brandon Sanderson Mistborn".to_string()],
            media_want: None,
            match_targets: vec![],
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.75),
            auto_download: true,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        }],
    }
}

/// Sample WatchSyncResponse.
pub fn sample_watch_sync_response() -> WatchSyncResponse {
    WatchSyncResponse {
        created: 1,
        updated: 0,
        deleted: 0,
    }
}

/// Sample SearchAndDownloadRequest.
pub fn sample_search_and_download_request() -> SearchAndDownloadRequest {
    SearchAndDownloadRequest {
        patterns: vec!["Brandon Sanderson Mistborn".to_string()],
        media_want: None,
        match_targets: vec![MatchTarget {
            id: ids::book_id().to_string(),
            target: "Brandon Sanderson Mistborn The Final Empire".to_string(),
            min_score: None,
            semantic_target: None,
        }],
        early_exit_score: Some(0.85),
        limit: Some(100),
        media_type: Some("ebooks".to_string()),
        auto_download: true,
        min_score: 0.75,
        metadata: None,
        callback_url: Some(
            "http://kobbler:6766/api/v1/admin/ingest/kobload/test-token".to_string(),
        ),
        auth_token: Some("test-token".to_string()),
    }
}

/// Sample SearchAndDownloadResponse.
pub fn sample_search_and_download_response() -> SearchAndDownloadResponse {
    SearchAndDownloadResponse {
        download_id: Some(ids::download_id()),
        selected_release: Some(sample_processed_release_dto()),
        results: vec![sample_processed_release_dto()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileGroupInfo;

    #[test]
    fn want_request_round_trip() {
        let req = sample_want_request();
        let json = serde_json::to_value(&req).unwrap();
        let deserialized: WantRequest = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.source_system, "kobbler");
        assert_eq!(deserialized.patterns.len(), 1);
        assert!(deserialized.media_want.is_some());
        assert_eq!(deserialized.media_type, Some("ebooks".to_string()));
    }

    #[test]
    fn want_response_round_trip() {
        let resp = sample_want_response_downloading();
        let json = serde_json::to_value(&resp).unwrap();
        let deserialized: WantResponse = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.status, WantStatus::Downloading);
        assert!(deserialized.download_id.is_some());
        assert!(deserialized.selected_release.is_some());
        assert_eq!(deserialized.source_system, Some("kobbler".to_string()));
    }

    #[test]
    fn want_response_without_source_fields() {
        // HTTP responses may omit source_system/source_id
        let json = serde_json::json!({
            "watch_pattern_id": ids::watch_pattern_id(),
            "status": "watching",
            "download_id": null,
            "results_found": 0,
            "best_score": 0.0
        });
        let deserialized: WantResponse = serde_json::from_value(json).unwrap();
        assert!(deserialized.source_system.is_none());
        assert!(deserialized.source_id.is_none());
        assert_eq!(deserialized.status, WantStatus::Watching);
    }

    #[test]
    fn search_request_round_trip() {
        let req = sample_search_request();
        let json = serde_json::to_value(&req).unwrap();
        let deserialized: SearchRequest = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.query, "Brandon Sanderson Mistborn");
    }

    #[test]
    fn search_response_round_trip() {
        let resp = sample_search_response();
        let json = serde_json::to_value(&resp).unwrap();
        let deserialized: SearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.results.len(), 1);
        assert_eq!(deserialized.results[0].guid, "abc-123");
    }

    #[test]
    fn watch_pattern_upsert_round_trip() {
        let upsert = sample_watch_pattern_upsert();
        let json = serde_json::to_value(&upsert).unwrap();
        let deserialized: WatchPatternUpsert = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.source_system, "kobbler");
        assert!(deserialized.media_want.is_some());
    }

    #[test]
    fn watch_pattern_deleted_event_round_trip() {
        let event = sample_watch_pattern_deleted_event();
        let json = serde_json::to_value(&event).unwrap();
        let deserialized: WatchPatternDeletedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.source_system, "kobbler");
        assert_eq!(deserialized.source_id, ids::book_id().to_string());
    }

    #[test]
    fn watch_pattern_delete_round_trip() {
        let delete = sample_watch_pattern_delete();
        let json = serde_json::to_value(&delete).unwrap();
        let deserialized: WatchPatternDelete = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.source_system, "kobbler");
    }

    #[test]
    fn watch_matched_event_round_trip() {
        let event = sample_watch_matched_event();
        let json = serde_json::to_value(&event).unwrap();
        let deserialized: WatchMatchedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.action, "downloaded");
        assert!(!deserialized.is_upgrade);
        assert_eq!(deserialized.quality_score, Some(0.85));
    }

    #[test]
    fn watch_matched_event_without_new_fields() {
        // Old NATS events won't have the new fields
        let json = serde_json::json!({
            "watch_pattern_id": ids::watch_pattern_id(),
            "source_system": "kobbler",
            "source_id": ids::book_id(),
            "match_score": 0.9,
            "matched_pattern": "test",
            "release_title": "test release",
            "release_guid": "guid-1",
            "download_id": null,
            "action": "matched",
            "metadata": null
        });
        let deserialized: WatchMatchedEvent = serde_json::from_value(json).unwrap();
        assert!(!deserialized.is_upgrade);
        assert!(deserialized.quality_score.is_none());
        assert!(deserialized.replaces_download_id.is_none());
        assert!(deserialized.episode_key.is_none());
    }

    #[test]
    fn download_completed_round_trip() {
        let event = sample_download_completed();
        let json = serde_json::to_value(&event).unwrap();
        let deserialized: DownloadCompletedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.status, "completed");
        assert_eq!(deserialized.files.len(), 1);
        assert_eq!(deserialized.files[0].path, "Mistborn.The.Final.Empire.epub");
        assert!(deserialized.idempotency_key.is_some());
    }

    #[test]
    fn download_completed_without_new_fields() {
        // Old NATS events won't have the new fields
        let json = serde_json::json!({
            "download_id": ids::download_id(),
            "status": "completed",
            "metadata": {},
            "files": [{
                "path": "test.epub",
                "size": 1000,
                "extension": "epub"
            }],
            "error": null,
            "recoverable_error": null
        });
        let deserialized: DownloadCompletedEvent = serde_json::from_value(json).unwrap();
        assert!(!deserialized.is_upgrade);
        assert!(deserialized.file_groups.is_none());
        assert!(deserialized.idempotency_key.is_none());
    }

    #[test]
    fn download_failed_round_trip() {
        let event = sample_download_failed();
        let json = serde_json::to_value(&event).unwrap();
        let deserialized: DownloadCompletedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.status, "failed");
        assert!(deserialized.error.is_some());
    }

    #[test]
    fn ingest_callback_has_expected_fields() {
        let json = sample_ingest_callback_json();
        assert!(json["download_id"].is_string());
        assert_eq!(json["status"], "completed");
        assert!(json["metadata"]["book_id"].is_string());
        assert!(json["files"].is_array());
        assert_eq!(json["files"][0]["extension"], "epub");
        // Can deserialize as DownloadCompletedEvent
        let event: DownloadCompletedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(event.files.len(), 1);
    }

    #[test]
    fn want_status_serializes_as_snake_case() {
        let json = serde_json::to_value(WantStatus::Downloading).unwrap();
        assert_eq!(json, "downloading");
        let json = serde_json::to_value(WantStatus::Watching).unwrap();
        assert_eq!(json, "watching");
        let json = serde_json::to_value(WantStatus::Failed).unwrap();
        assert_eq!(json, "failed");
    }

    #[test]
    fn media_want_book_serializes_with_kind_tag() {
        let want = MediaWant::Book {
            title: "Test".to_string(),
            author: None,
            isbn: None,
            format_preferences: vec![],
            identifiers: None,
            series: None,
            series_position: None,
            release_date: None,
            alternate_titles: None,
        };
        let json = serde_json::to_value(&want).unwrap();
        assert_eq!(json["kind"], "book");
        assert_eq!(json["title"], "Test");
    }

    #[test]
    fn processed_release_dto_round_trip() {
        let result = sample_processed_release_dto();
        let json = serde_json::to_value(&result).unwrap();
        let deserialized: ProcessedReleaseDto = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.guid, "abc-123");
        assert_eq!(deserialized.match_score, Some(0.92));
        assert!(deserialized.quality.is_some());
        assert_eq!(deserialized.decision, crate::ScoringDecision::Candidate);
        assert!(deserialized.reject_reasons.is_empty());
    }

    #[test]
    fn create_download_round_trip() {
        let req = sample_create_download_request();
        let json = serde_json::to_value(&req).unwrap();
        let deserialized: CreateDownloadRequest = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.protocol, "usenet");
    }

    #[test]
    fn watch_sync_round_trip() {
        let req = sample_watch_sync_request();
        let json = serde_json::to_value(&req).unwrap();
        let deserialized: WatchSyncRequest = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.items.len(), 1);

        let resp = sample_watch_sync_response();
        let json = serde_json::to_value(&resp).unwrap();
        let deserialized: WatchSyncResponse = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.created, 1);
    }

    #[test]
    fn search_and_download_round_trip() {
        let req = sample_search_and_download_request();
        let json = serde_json::to_value(&req).unwrap();
        let deserialized: SearchAndDownloadRequest = serde_json::from_value(json).unwrap();
        assert!(deserialized.auto_download);

        let resp = sample_search_and_download_response();
        let json = serde_json::to_value(&resp).unwrap();
        let deserialized: SearchAndDownloadResponse = serde_json::from_value(json).unwrap();
        assert!(deserialized.download_id.is_some());
        assert!(deserialized.selected_release.is_some());
    }

    #[test]
    fn file_group_info_round_trip() {
        let group = FileGroupInfo {
            name: "book".to_string(),
            formats: vec!["epub".to_string(), "pdf".to_string()],
            best_path: "book.epub".to_string(),
        };
        let json = serde_json::to_value(&group).unwrap();
        let deserialized: FileGroupInfo = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.formats.len(), 2);
    }
}
