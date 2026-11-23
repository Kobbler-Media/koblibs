use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::post;
use kob_usenet::{Engine, ImportMode, JobState, Purpose};
use tokio::io::AsyncReadExt;

use crate::rig::{Loaded, media, single_nzb};
use crate::support::ARTICLE;

const CONNECTIONS: usize = 8;
const STREAM_ARTICLES: usize = 600;
// About 5 Mbit/s, read ahead by 2 MB: playback in its steady state.
const PACE: Duration = Duration::from_millis(100);
const READ_AHEAD: u64 = 2 << 20;

async fn download_time(rig: &Loaded, engine: &Engine) -> Duration {
    let started = Instant::now();
    rig.start_download(engine).await;
    loop {
        let status = engine.job("library").unwrap();
        match status.state {
            JobState::Completed => return started.elapsed(),
            JobState::Failed => panic!("{status:?}"),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

async fn queue_time(playing: bool) -> Duration {
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
    let done = Arc::new(AtomicBool::new(false));
    let player = playing.then(|| {
        let (engine, done) = (Arc::clone(&engine), Arc::clone(&done));
        tokio::spawn(async move {
            let mut reader = engine.open(&release.key, 0, Purpose::Play).unwrap();
            let mut buf = vec![0; 64 * 1024];
            let mut read = 0;
            while !done.load(Ordering::Relaxed) {
                let n = reader.read(&mut buf).await.unwrap();
                assert!(n > 0, "the film ran out before the download finished");
                read += n;
                tokio::time::sleep(PACE).await;
            }
            read
        })
    });
    let took = download_time(&rig, &engine).await;
    done.store(true, Ordering::Relaxed);
    if let Some(player) = player {
        let read = player.await.unwrap();
        eprintln!("player read {read} B");
    }
    took
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continuous_playback_keeps_most_of_the_download_throughput() {
    let _serial = crate::rig::serial().await;
    let alone = queue_time(false).await;
    let playing = queue_time(true).await;
    let share = alone.as_secs_f64() / playing.as_secs_f64();
    eprintln!(
        "download alone {alone:?}, with playback {playing:?}: {:.0}%",
        share * 100.0
    );
    // 8 connections leave playback a reserve of 2 (a quarter); seeks need it, so a steady
    // stream costs a download about a fifth here, less on larger pools.
    assert!(
        share >= 0.75,
        "download kept {:.0}% of its throughput",
        share * 100.0
    );
}
