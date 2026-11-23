use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::post;
use kob_usenet::{Engine, ImportMode, Purpose, Release};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::rig::{Loaded, article_time, downloading, media, single_nzb, within};
use crate::support::{POSTED, output, quoted};

const CONNECTIONS: usize = 16;
const DEPTH: usize = 8;
const ARTICLE: usize = 64 * 1024;
const LIBRARY_ARTICLES: usize = 1500;
const FILM_ARTICLES: usize = 384;
const READ_AHEAD: u64 = 4 << 20;
const PLAY_EVERY: Duration = Duration::from_millis(300);

struct Cached {
    first_byte: Duration,
    longest_gap: Duration,
    rate: f64,
}

async fn cache_read(engine: &Engine, release: &Release, flowing: &AtomicBool) -> Cached {
    let mut reader = engine.open(&release.key, 0, Purpose::Cache).unwrap();
    let mut buf = vec![0; 64 * 1024];
    let started = Instant::now();
    let (mut last, mut first_byte, mut longest_gap, mut read) = (None, None, Duration::ZERO, 0);
    loop {
        let n = reader.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        let now = Instant::now();
        match last {
            Some(last) => longest_gap = longest_gap.max(now - last),
            None => {
                first_byte = Some(now - started);
                flowing.store(true, Ordering::Relaxed);
            }
        }
        last = Some(now);
        read += n;
    }
    let rate = read as f64 / started.elapsed().as_secs_f64();
    Cached {
        first_byte: first_byte.unwrap(),
        longest_gap,
        rate,
    }
}

async fn play_bursts(
    engine: Arc<Engine>,
    release: Release,
    flowing: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
) -> usize {
    let episode = output(&release, "Show.S01E01.mkv");
    while !flowing.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut bursts = 0;
    while !done.load(Ordering::Relaxed) {
        let mut reader = engine.open(&release.key, episode, Purpose::Play).unwrap();
        let at = (bursts as u64 * 37_000) % (reader.len() - 4096);
        reader.seek(std::io::SeekFrom::Start(at)).await.unwrap();
        let mut buf = vec![0; 4096];
        reader.read_exact(&mut buf).await.unwrap();
        bursts += 1;
        tokio::time::sleep(PLAY_EVERY).await;
    }
    bursts
}

async fn cache_under_download(playing: bool) {
    let _serial = crate::rig::serial().await;
    let film = post(
        "film",
        "Film.2026.mkv",
        &media(FILM_ARTICLES * ARTICLE, 239),
        ARTICLE,
    );
    let rig = Loaded::sized(film, ARTICLE, LIBRARY_ARTICLES).await;
    let engine = Arc::new(
        rig.engine_configured(CONNECTIONS, |config| {
            config.pipeline_depth = DEPTH;
            config.read_ahead_bytes = READ_AHEAD;
        })
        .await,
    );
    let film = engine
        .import(
            Some(&single_nzb("film", "Film.2026.mkv", FILM_ARTICLES, ARTICLE)),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    let episode = engine
        .import(
            Some(&rig.fixture.nzb(&[1], quoted, POSTED)),
            None,
            "Show.S01",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    rig.start_download(&engine).await;
    tokio::time::sleep(article_time(ARTICLE) * 2 * DEPTH as u32).await;

    let (flowing, done) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let player = playing.then(|| {
        tokio::spawn(play_bursts(
            Arc::clone(&engine),
            episode,
            Arc::clone(&flowing),
            Arc::clone(&done),
        ))
    });
    let queued_before = engine.job("library").unwrap().bytes_done;
    let started = Instant::now();
    let cached = within("cache read", cache_read(&engine, &film, &flowing)).await;
    let queue_rate = (engine.job("library").unwrap().bytes_done - queued_before) as f64
        / started.elapsed().as_secs_f64();
    done.store(true, Ordering::Relaxed);
    let bursts = match player {
        Some(player) => within("play bursts", player).await.unwrap(),
        None => 0,
    };
    let still = downloading(&engine);
    // One connection's pipeline draining, then the article itself.
    let drain = article_time(ARTICLE) * (DEPTH as u32 + 2);
    eprintln!(
        "drain {drain:?}; cache first byte {:?}, longest gap {:?}, {:.1} MB/s; queue {:.1} MB/s; play bursts {bursts}; download still running: {still}",
        cached.first_byte,
        cached.longest_gap,
        cached.rate / 1e6,
        queue_rate / 1e6,
    );
    // Its own read-ahead may pipeline a full depth ahead of the segment it waits for.
    assert!(
        cached.longest_gap < drain * 2,
        "cache reader went {:?} without bytes; a drain takes {drain:?}",
        cached.longest_gap
    );
    assert!(
        cached.first_byte < drain,
        "cache reader waited {:?} for its first byte; a drain takes {drain:?}",
        cached.first_byte
    );
    assert!(
        cached.rate * 3.0 >= queue_rate * 2.0,
        "cache read {:.1} MB/s, under two thirds of the download's {:.1} MB/s",
        cached.rate / 1e6,
        queue_rate / 1e6
    );
    assert!(still, "the download must saturate the pool throughout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cache_reader_keeps_flowing_under_a_saturating_download() {
    cache_under_download(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cache_reader_keeps_flowing_under_a_download_and_short_playback_reads() {
    cache_under_download(true).await;
}
