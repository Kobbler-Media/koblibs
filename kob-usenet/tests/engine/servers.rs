use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, Error, ImportMode, Purpose, Release};
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, quoted, server};

async fn read_all(engine: &Engine, release: &Release, fixture: &Fixture) {
    for out in &release.outputs {
        let mut all = Vec::new();
        engine
            .open(&release.key, out.index, Purpose::Play)
            .unwrap()
            .read_to_end(&mut all)
            .await
            .unwrap();
        assert!(all == fixture.data(&out.path), "{}", out.path);
    }
}

#[tokio::test]
async fn an_expired_account_is_never_connected() {
    let fixture = Fixture::season();
    let expired = LoopbackServer::start(fixture.articles()).await.unwrap();
    let current = LoopbackServer::start(fixture.articles()).await.unwrap();
    let mut lapsed = server("expired", &expired, 0);
    lapsed.expires_on = Some("2001-01-01".to_owned());
    let mut valid = server("current", &current, 1);
    valid.expires_on = Some("2999-12-31".to_owned());
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![lapsed, valid]).await;

    let release = import(&engine, &fixture.standard_nzb(), None).await;
    read_all(&engine, &release, &fixture).await;
    assert_eq!(expired.peak_connections(), 0);
    assert!(current.body_commands() > 0);
    assert!(engine.stats().iter().all(|s| s.key != "expired"));
}

#[tokio::test]
async fn a_spent_quota_survives_a_restart_and_skips_the_account() {
    let fixture = Fixture::season();
    let block = LoopbackServer::start(fixture.articles()).await.unwrap();
    let primary = LoopbackServer::start(fixture.articles()).await.unwrap();
    let servers = || {
        let mut capped = server("block", &block, 0);
        capped.quota_bytes = Some(1);
        vec![capped, server("primary", &primary, 1)]
    };
    let dir = tempfile::tempdir().unwrap();
    let release = {
        let engine = engine(dir.path(), servers()).await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        read_all(&engine, &release, &fixture).await;
        release
    };
    let (bodies, served) = (block.body_commands(), primary.body_commands());

    let engine = engine(dir.path(), servers()).await;
    read_all(&engine, &release, &fixture).await;
    assert_eq!(
        block.body_commands(),
        bodies,
        "the spent block account stays untouched"
    );
    assert!(primary.body_commands() > served);
}

#[tokio::test]
async fn a_release_older_than_every_retention_fails_without_requests() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let old = fixture.nzb(&[0, 1, 2], quoted, 1_000_000_000);
    let mut short = server("short", &nntp, 0);
    short.retention_days = Some(365);

    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![short.clone()]).await;
    let result = engine
        .import(Some(&old), None, "Show.S01", None, ImportMode::Stream)
        .await;
    assert!(matches!(result, Err(Error::Retention)), "{result:?}");
    assert_eq!(nntp.peak_connections(), 0);

    let mut long = short;
    long.retention_days = Some(20_000);
    engine.reconfigure(vec![long]).unwrap();
    engine
        .import(Some(&old), None, "Show.S01", None, ImportMode::Stream)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_server_whose_retention_the_post_outlived_is_never_asked() {
    let fixture = Fixture::season();
    let short = LoopbackServer::start(fixture.articles()).await.unwrap();
    let long = LoopbackServer::start(fixture.articles()).await.unwrap();
    let mut recent = server("short", &short, 0);
    recent.retention_days = Some(30);
    let mut deep = server("long", &long, 99);
    deep.retention_days = Some(4000);
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![recent, deep]).await;

    let release = import(&engine, &fixture.standard_nzb(), None).await;
    read_all(&engine, &release, &fixture).await;
    assert_eq!(short.body_commands(), 0);
    assert!(long.body_commands() > 0);
}

#[tokio::test]
async fn credentials_no_server_could_accept_are_refused_when_providers_are_built() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut spaced = server("a", &nntp, 0);
    spaced.username = "bad user".into();
    let mut broken = server("a", &nntp, 0);
    broken.password = "pass\r\nQUIT".into();
    for bad in [spaced, broken] {
        let built = kob_usenet::Engine::new(kob_usenet::EngineConfig::new(
            dir.path().to_path_buf(),
            vec![bad.clone()],
        ))
        .await;
        assert!(matches!(built, Err(Error::Server { .. })), "{bad:?}");
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        assert!(matches!(
            engine.reconfigure(vec![bad.clone()]),
            Err(Error::Server { .. })
        ));
    }
}
