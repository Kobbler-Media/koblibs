use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, EngineConfig, Error, ImportMode};

use crate::support::{Fixture, POSTED, bare, engine, import, quoted, requests, server};

#[tokio::test]
async fn a_known_release_or_cache_key_imports_without_touching_the_network() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;

    let first = import(&engine, &fixture.standard_nzb(), Some("7/guid-1")).await;
    assert_eq!(first.outputs.len(), 3);
    let (asked, bodies) = (requests(&engine), nntp.body_commands());
    assert!(asked > 0 && bodies > 0);

    let reencoded = fixture.nzb(&[2, 0, 1], bare, POSTED + 60);
    for (nzb, key) in [
        (Some(fixture.standard_nzb()), None),
        (None, Some("7/guid-1")),
        (Some(reencoded), Some("9/guid-2")),
        (None, Some("9/guid-2")),
    ] {
        let again = engine
            .import(nzb.as_deref(), key, "Show.S01", None, ImportMode::Download)
            .await
            .unwrap();
        assert_eq!(again, first, "{key:?}");
    }
    assert_eq!(
        requests(&engine),
        asked,
        "no STAT or BODY for a known release"
    );
    assert_eq!(nntp.body_commands(), bodies);

    let unknown = engine
        .import(None, Some("7/never"), "x", None, ImportMode::Stream)
        .await;
    assert!(matches!(unknown, Err(Error::NotCached)), "{unknown:?}");
}

#[tokio::test]
async fn the_nzb_cache_keeps_the_most_recent_nzbs_within_its_byte_cap() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = EngineConfig::new(dir.path().to_path_buf(), vec![server("a", &nntp, 0)]);
    config.nzb_cache_bytes = 1;
    let engine = Engine::new(config).await.unwrap();
    let nzbs = || std::fs::read_dir(dir.path().join("nzb")).unwrap().count();

    let pack = import(&engine, &fixture.standard_nzb(), Some("1/pack")).await;
    let single = fixture.nzb(&[1], quoted, POSTED);
    let episode = import(&engine, &single, Some("1/single")).await;
    assert_ne!(pack.key, episode.key);
    assert_eq!(nzbs(), 1, "the older NZB is evicted past the cap");
    for key in ["1/pack", "1/single"] {
        engine
            .import(None, Some(key), "x", None, ImportMode::Stream)
            .await
            .unwrap();
    }
}
