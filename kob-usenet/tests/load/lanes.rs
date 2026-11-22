use std::time::{Duration, Instant};

use kob_usenet::{Engine, ImportMode, Purpose};
use tokio::io::AsyncReadExt;

use crate::rig::{DRAIN, Loaded, downloading, within};
use crate::support::{POSTED, output, quoted};

async fn timed_read(engine: &Engine, release: &kob_usenet::Release, purpose: Purpose) -> Duration {
    let started = Instant::now();
    let mut all = Vec::new();
    engine
        .open(&release.key, output(release, "Show.S01E01.mkv"), purpose)
        .unwrap()
        .read_to_end(&mut all)
        .await
        .unwrap();
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_reads_and_stream_imports_rank_above_a_saturating_download() {
    ranked(Loaded::new(Vec::new()).await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranking_holds_with_a_backup_tier_configured() {
    let rig = Loaded::new(Vec::new()).await.with_backup().await;
    ranked(rig).await;
}

async fn ranked(rig: Loaded) {
    let _serial = crate::rig::serial().await;
    let (episode, other) = (
        rig.fixture.nzb(&[1, 2], quoted, POSTED),
        rig.fixture.nzb(&[0], quoted, POSTED),
    );
    let (release, import_alone, cache_alone) = {
        let engine = rig.engine().await;
        let started = Instant::now();
        let release = engine
            .import(Some(&episode), None, "Show.S01", None, ImportMode::Stream)
            .await
            .unwrap();
        let import_alone = started.elapsed();
        let cache_alone = timed_read(&engine, &release, Purpose::Cache).await;
        (release, import_alone, cache_alone)
    };

    let engine = rig.engine().await;
    rig.start_downloads(&engine, 2).await;
    let cache_loaded = within("cache read", timed_read(&engine, &release, Purpose::Cache)).await;
    let started = Instant::now();
    within(
        "stream import",
        engine.import(Some(&other), None, "Show.S01", None, ImportMode::Stream),
    )
    .await
    .unwrap();
    let import_loaded = started.elapsed();
    let still = downloading(&engine);
    eprintln!(
        "import {import_alone:?} -> {import_loaded:?}, cache {cache_alone:?} -> {cache_loaded:?}, download still running: {still}"
    );
    assert!(still, "the download must still saturate the pool");
    assert!(
        import_loaded < import_alone * 3 + DRAIN,
        "stream import {import_alone:?} alone, {import_loaded:?} under a download"
    );
    assert!(
        cache_loaded < cache_alone * 3 + DRAIN,
        "cache read {cache_alone:?} alone, {cache_loaded:?} under a download"
    );
    assert_eq!(rig.backup_bodies(), 0, "the backup only serves misses");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturating_downloads_outrank_warm_and_health_work_with_a_backup_tier() {
    let _serial = crate::rig::serial().await;
    let rig = Loaded::new(Vec::new()).await.with_backup().await;
    let (episode, other) = (
        rig.fixture.nzb(&[1, 2], quoted, POSTED),
        rig.fixture.nzb(&[0], quoted, POSTED),
    );
    let engine = std::sync::Arc::new(rig.engine().await);
    let release = engine
        .import(Some(&episode), None, "Show.S01", None, ImportMode::Stream)
        .await
        .unwrap();
    rig.start_downloads(&engine, 2).await;

    let health = tokio::spawn({
        let engine = std::sync::Arc::clone(&engine);
        async move { engine.sample_health(&release.key, 100, None).await }
    });
    let prewarm = tokio::spawn({
        let engine = std::sync::Arc::clone(&engine);
        async move {
            engine
                .import(Some(&other), None, "Show.S01", None, ImportMode::Prewarm)
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        downloading(&engine),
        "the downloads must still saturate the pool"
    );
    assert!(
        !health.is_finished(),
        "a health sample ran ahead of queued downloads"
    );
    assert!(
        !prewarm.is_finished(),
        "a prewarm import ran ahead of queued downloads"
    );

    for id in ["library", "library-1"] {
        engine.remove_job(id, true).await.unwrap();
    }
    within("health sample", health).await.unwrap().unwrap();
    within("prewarm import", prewarm).await.unwrap().unwrap();
    assert_eq!(rig.backup_bodies(), 0, "the backup only serves misses");
}
