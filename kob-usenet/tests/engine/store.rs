use std::time::Duration;

use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, EngineConfig, Error, ImportMode, ServerConfig};

use crate::support::{Fixture, POSTED, engine, import, quoted, server};

fn release_file(dir: &std::path::Path, key: &kob_usenet::ReleaseKey) -> std::path::PathBuf {
    dir.join("release").join(key.as_str())
}

#[tokio::test]
async fn an_unreadable_stored_layout_is_dropped_and_the_release_imports_again() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    for damage in ["truncated", "garbage", "empty"] {
        let dir = tempfile::tempdir().unwrap();
        let release = {
            let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
            import(&engine, &fixture.standard_nzb(), Some("idx:guid")).await
        };
        let path = release_file(dir.path(), &release.key);
        let bytes = std::fs::read(&path).unwrap();
        let damaged = match damage {
            "truncated" => bytes[..bytes.len() / 2].to_vec(),
            "garbage" => vec![0xFF; 64],
            _ => Vec::new(),
        };
        std::fs::write(&path, damaged).unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let again = engine
            .import(None, Some("idx:guid"), "Show.S01", None, ImportMode::Stream)
            .await
            .unwrap_or_else(|e| panic!("{damage}: {e}"));
        assert_eq!(again, release, "{damage}");
        assert_eq!(engine.release(&release.key).unwrap(), release, "{damage}");
    }
}

#[tokio::test]
async fn concurrent_imports_of_one_release_fetch_it_once() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let single = {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let before = nntp.body_commands();
        import(&engine, &fixture.standard_nzb(), None).await;
        nntp.body_commands() - before
    };
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let before = nntp.body_commands();
    let nzb = fixture.standard_nzb();
    let (a, b, c) = tokio::join!(
        import(&engine, &nzb, Some("one")),
        import(&engine, &nzb, Some("two")),
        import(&engine, &nzb, None),
    );
    assert!(a == b && b == c);
    assert_eq!(nntp.body_commands() - before, single);
}

fn config(dir: &std::path::Path, servers: Vec<ServerConfig>) -> EngineConfig {
    let mut config = EngineConfig::new(dir.to_path_buf(), servers);
    config.maintenance_interval = Duration::from_millis(50);
    config
}

#[tokio::test]
async fn provider_usage_is_saved_while_running_and_a_corrupt_file_is_not_silently_reset() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut block = server("a", &nntp, 0);
    block.quota_bytes = Some(1 << 40);
    let engine = Engine::new(config(dir.path(), vec![block.clone()]))
        .await
        .unwrap();
    import(&engine, &fixture.standard_nzb(), None).await;
    let usage = dir.path().join("usage");
    let saved = std::fs::read(&usage).unwrap();
    crate::support::read_all(&engine, &fixture).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_ne!(
        std::fs::read(&usage).unwrap(),
        saved,
        "stream reads were saved"
    );
    std::mem::forget(engine);

    std::fs::write(&usage, b"\xff\xff\xff").unwrap();
    let restored = Engine::new(config(dir.path(), vec![block.clone()])).await;
    assert!(restored.is_ok(), "the backup stands in for a corrupt file");
    drop(restored);
    std::fs::write(&usage, b"\xff\xff\xff").unwrap();
    std::fs::write(dir.path().join("usage.bak"), b"\xff").unwrap();
    let refused = Engine::new(config(dir.path(), vec![block])).await;
    assert!(
        matches!(refused, Err(Error::Stored(_))),
        "{:?}",
        refused.err()
    );
}

#[tokio::test]
async fn leftover_temporary_files_are_removed_and_never_evicted_as_cache() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    for sub in ["nzb", "release", "alias"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        std::fs::write(dir.path().join(sub).join(".tmp-crashed"), b"partial").unwrap();
    }
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    for sub in ["nzb", "release", "alias"] {
        assert!(!dir.path().join(sub).join(".tmp-crashed").exists(), "{sub}");
    }
    import(&engine, &fixture.standard_nzb(), None).await;
    let names: Vec<_> = std::fs::read_dir(dir.path().join("nzb"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| n.starts_with("nzb-")), "{names:?}");
}

#[tokio::test]
async fn the_release_cache_keeps_the_newest_layouts_within_its_cap() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path(), vec![server("a", &nntp, 0)]);
    config.release_cache_bytes = 1;
    let engine = Engine::new(config).await.unwrap();
    let first = import(&engine, &fixture.nzb(&[0], quoted, POSTED), None).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = import(&engine, &fixture.nzb(&[1], quoted, POSTED), None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!release_file(dir.path(), &first.key).exists());
    assert!(release_file(dir.path(), &second.key).exists());
}

#[tokio::test]
async fn warm_connections_are_kept_on_the_first_level_only() {
    let fixture = Fixture::season();
    let primary = LoopbackServer::start(fixture.articles()).await.unwrap();
    let backup = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(
        dir.path(),
        vec![
            server("primary", &primary, 0),
            server("backup", &backup, 99),
        ],
    );
    config.min_connections = 2;
    let engine = Engine::new(config).await.unwrap();
    engine.prewarm().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(primary.open_connections(), 2);
    assert_eq!(backup.peak_connections(), 0);
}

#[tokio::test]
async fn a_block_account_that_runs_out_is_dropped_from_the_providers() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let block = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut limited = server("block", &block, 0);
    limited.quota_bytes = Some(1);
    let engine = Engine::new(config(dir.path(), vec![limited, server("a", &nntp, 0)]))
        .await
        .unwrap();
    assert_eq!(engine.stats().len(), 2);
    import(&engine, &fixture.standard_nzb(), None).await;
    crate::support::read_all(&engine, &fixture).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let keys: Vec<_> = engine.stats().into_iter().map(|s| s.key).collect();
    assert_eq!(keys, ["a"]);
}
