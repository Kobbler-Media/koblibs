use std::sync::Arc;
use std::time::Duration;

use kob_usenet::vendor::testkit::post;
use kob_usenet::{Engine, ImportMode, JobState, Purpose, Release};
use tokio::io::AsyncReadExt;

use crate::rig::{Loaded, media, single_nzb};

const CONNECTIONS: usize = 8;
const ARTICLE: usize = 64 * 1024;
const LIBRARY_ARTICLES: usize = 240;
const FILM_ARTICLES: usize = 96;
const READ_AHEAD: u64 = 4 << 20;

async fn read_for(engine: Arc<Engine>, release: Release, purpose: Purpose, for_: Duration) {
    let mut reader = engine.open(&release.key, 0, purpose).unwrap();
    let mut buf = vec![0; 64 * 1024];
    let _ = tokio::time::timeout(for_, async {
        loop {
            if reader.read(&mut buf).await.unwrap() == 0 {
                break;
            }
        }
    })
    .await;
}

async fn import(engine: &Engine, prefix: &str, name: &str) -> Release {
    engine
        .import(
            Some(&single_nzb(prefix, name, FILM_ARTICLES, ARTICLE)),
            None,
            name,
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap()
}

async fn completes(engine: &Engine) -> bool {
    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = engine.job("library").unwrap();
            match status.state {
                JobState::Completed => return,
                JobState::Failed => panic!("{status:?}"),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await;
    finished.is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_finishes_after_playback_and_a_cache_reader_stop() {
    let _serial = crate::rig::serial().await;
    let mut extra = post(
        "film",
        "Film.2026.mkv",
        &media(FILM_ARTICLES * ARTICLE, 239),
        ARTICLE,
    );
    extra.extend(post(
        "clip",
        "Clip.2026.mkv",
        &media(FILM_ARTICLES * ARTICLE, 241),
        ARTICLE,
    ));
    let rig = Loaded::sized(extra, ARTICLE, LIBRARY_ARTICLES).await;
    let engine = Arc::new(rig.engine_reading_ahead(CONNECTIONS, READ_AHEAD).await);
    let film = import(&engine, "film", "Film.2026.mkv").await;
    let clip = import(&engine, "clip", "Clip.2026.mkv").await;
    rig.start_download(&engine).await;
    let brief = Duration::from_millis(400);
    let play = tokio::spawn(read_for(Arc::clone(&engine), film, Purpose::Play, brief));
    let cache = tokio::spawn(read_for(Arc::clone(&engine), clip, Purpose::Cache, brief));
    play.await.unwrap();
    cache.await.unwrap();
    assert!(
        completes(&engine).await,
        "the download stalled after playback and caching stopped: {:?}",
        engine.job("library")
    );
}
