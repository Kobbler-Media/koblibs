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
    let _serial = crate::rig::serial().await;
    let rig = Loaded::new(Vec::new()).await;
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
    rig.start_download(&engine).await;
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
}
