use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::post;
use kob_usenet::{Engine, ImportMode, JobState, Purpose};
use tokio::io::AsyncReadExt;

use crate::rig::{DRAIN, Loaded, media, single_nzb};
use crate::support::ARTICLE;

const CONNECTIONS: usize = 8;
const STREAM_ARTICLES: usize = 600;
// About 5 Mbit/s, read ahead by 2 MB: playback in its steady state.
const PACE: Duration = Duration::from_millis(100);
const READ_AHEAD: u64 = 2 << 20;
const HELD_FOR: Duration = Duration::from_millis(1500);
const FINISH_WITHIN: Duration = Duration::from_secs(30);

async fn finishes(engine: &Engine) -> Duration {
    let started = Instant::now();
    loop {
        let status = engine.job("library").unwrap();
        match status.state {
            JobState::Completed => return started.elapsed(),
            JobState::Failed => panic!("{status:?}"),
            _ if started.elapsed() > FINISH_WITHIN => panic!("{status:?}"),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_continuous_playback_leaves_the_spare_line_to_the_download() {
    let _serial = crate::rig::serial().await;
    let rig = Loaded::new(post(
        "film",
        "Film.2026.mkv",
        &media(STREAM_ARTICLES * ARTICLE, 239),
        ARTICLE,
    ))
    .await;
    let engine = Arc::new(rig.engine_reading_ahead(CONNECTIONS, READ_AHEAD).await);
    let release = engine
        .import(
            Some(&single_nzb(
                "film",
                "Film.2026.mkv",
                STREAM_ARTICLES,
                ARTICLE,
            )),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    rig.start_download(&engine).await;
    let done = Arc::new(AtomicBool::new(false));
    let player = {
        let (engine, done) = (Arc::clone(&engine), Arc::clone(&done));
        tokio::spawn(async move {
            let mut reader = engine.open(&release.key, 0, Purpose::Play).unwrap();
            let mut buf = vec![0; 64 * 1024];
            while !done.load(Ordering::Relaxed) {
                assert!(reader.read(&mut buf).await.unwrap() > 0, "the film ran out");
                tokio::time::sleep(PACE).await;
            }
        })
    };
    tokio::time::sleep(DRAIN).await;
    let before = engine.job("library").unwrap().bytes_done;
    tokio::time::sleep(HELD_FOR).await;
    let during = engine.job("library").unwrap().bytes_done - before;
    done.store(true, Ordering::Relaxed);
    player.await.unwrap();
    assert!(
        during > (CONNECTIONS * ARTICLE) as u64,
        "the download moved only {during} bytes beside a 5 Mbit/s player"
    );
    finishes(&engine).await;
}
