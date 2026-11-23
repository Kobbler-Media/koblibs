use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::post;
use kob_usenet::{Engine, EngineConfig, ImportMode, OutputReader, Purpose, Release};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::rig::{Loaded, downloading, media, serial, single_nzb, within};

const CONNECTIONS: usize = 8;
const ARTICLE: usize = 64 * 1024;
const LIBRARY_ARTICLES: usize = 1500;
const FILM_ARTICLES: usize = 400;
const FILM_SEED: usize = 239;
const DRAINED: Duration = Duration::from_millis(600);
const RESUME_WITHIN: Duration = Duration::from_secs(4);

struct Watching {
    rig: Loaded,
    engine: Arc<Engine>,
    film: Release,
}

impl Watching {
    async fn new(edit: impl FnOnce(&mut EngineConfig)) -> Self {
        Self::sized(ARTICLE, FILM_ARTICLES, LIBRARY_ARTICLES, CONNECTIONS, edit).await
    }

    async fn sized(
        article: usize,
        film_articles: usize,
        library_articles: usize,
        connections: usize,
        edit: impl FnOnce(&mut EngineConfig),
    ) -> Self {
        let posted = post(
            "film",
            "Film.2026.mkv",
            &media(film_articles * article, FILM_SEED),
            article,
        );
        let rig = Loaded::sized(posted, article, library_articles).await;
        let engine = Arc::new(rig.engine_configured(connections, edit).await);
        let film = engine
            .import(
                Some(&single_nzb("film", "Film.2026.mkv", film_articles, article)),
                None,
                "Film",
                None,
                ImportMode::Stream,
            )
            .await
            .unwrap();
        Self { rig, engine, film }
    }

    fn open(&self, purpose: Purpose) -> OutputReader {
        self.engine.open(&self.film.key, 0, purpose).unwrap()
    }

    fn queued(&self) -> u64 {
        self.engine.job("library").unwrap().bytes_done
    }

    async fn queue_advances(&self, by: u64, within: Duration) -> bool {
        let from = self.queued();
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.queued() >= from + by {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

async fn read_all(mut reader: OutputReader) -> u64 {
    let mut buf = vec![0; 64 * 1024];
    let mut read = 0;
    loop {
        let n = reader.read(&mut buf).await.unwrap();
        if n == 0 {
            return read;
        }
        read += n as u64;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cache_reader_stops_the_queue_and_it_resumes_after_the_file_is_cached() {
    let _serial = serial().await;
    let watching = Watching::new(|_| {}).await;
    watching.rig.start_download(&watching.engine).await;
    let reader = tokio::spawn(read_all(watching.open(Purpose::Cache)));
    tokio::time::sleep(DRAINED).await;
    let before = watching.queued();
    let read = within("cache read", reader).await.unwrap();
    assert_eq!(read, (FILM_ARTICLES * ARTICLE) as u64);
    let during = watching.queued() - before;
    assert!(
        during <= (CONNECTIONS * ARTICLE) as u64,
        "the queue moved {during} bytes while a cache reader was active"
    );
    assert!(
        watching
            .queue_advances(16 * ARTICLE as u64, RESUME_WITHIN)
            .await,
        "the queue did not resume after the file was cached"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_that_stops_without_closing_never_holds_the_queue() {
    let _serial = serial().await;
    for purpose in [Purpose::Cache, Purpose::Play] {
        let watching = Watching::new(|config| {
            config.read_ahead_bytes = 8 * ARTICLE as u64;
            config.cache_read_ahead_bytes = 8 * ARTICLE as u64;
        })
        .await;
        watching.rig.start_download(&watching.engine).await;
        let mut stalled = watching.open(purpose);
        let mut buf = vec![0; ARTICLE];
        stalled.read_exact(&mut buf).await.unwrap();
        assert!(
            watching
                .queue_advances(32 * ARTICLE as u64, RESUME_WITHIN)
                .await,
            "a stalled {purpose:?} reader held the queue"
        );
        assert!(downloading(&watching.engine));
        drop(stalled);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warm_reader_stays_below_the_queue() {
    let _serial = serial().await;
    let watching = Watching::new(|_| {}).await;
    watching.rig.start_download(&watching.engine).await;
    let warm = tokio::spawn(read_all(watching.open(Purpose::Warm)));
    tokio::time::sleep(DRAINED).await;
    assert!(
        watching
            .queue_advances(64 * ARTICLE as u64, Duration::from_secs(2))
            .await,
        "a warm reader held the queue back"
    );
    warm.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeks_leave_the_cache_ahead_position_and_finish_time_alone() {
    let _serial = serial().await;
    let alone = {
        let watching = Watching::new(|_| {}).await;
        let started = Instant::now();
        read_all(watching.open(Purpose::Cache)).await;
        started.elapsed()
    };
    let watching = Watching::new(|_| {}).await;
    let expected = media(FILM_ARTICLES * ARTICLE, FILM_SEED);
    let mut cache = watching.open(Purpose::Cache);
    let started = Instant::now();
    let seeker = {
        let engine = Arc::clone(&watching.engine);
        let key = watching.film.key.clone();
        tokio::spawn(async move {
            let mut seeks = 0_u64;
            loop {
                let mut reader = engine.open(&key, 0, Purpose::Play).unwrap();
                let at = (seeks * 7_919 * ARTICLE as u64) % (reader.len() - ARTICLE as u64);
                reader.set_end(at + ARTICLE as u64);
                reader.seek(std::io::SeekFrom::Start(at)).await.unwrap();
                let mut buf = vec![0; ARTICLE];
                reader.read_exact(&mut buf).await.unwrap();
                seeks += 1;
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        })
    };
    let (mut position, mut buf) = (0, vec![0; 64 * 1024]);
    loop {
        let n = cache.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        assert!(
            buf[..n] == expected[position..position + n],
            "the cache reader left its position at byte {position}"
        );
        position += n;
    }
    let seeking = started.elapsed();
    seeker.abort();
    assert_eq!(position, expected.len());
    assert!(
        seeking.as_secs_f64() <= alone.as_secs_f64() * 1.35,
        "cache-ahead took {seeking:?} under seeks, {alone:?} alone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_read_ahead_fills_the_pool_with_large_articles() {
    let _serial = serial().await;
    const LARGE: usize = 4_200_000;
    const POOL: usize = 16;
    let watching = Watching::sized(LARGE, 48, 1, POOL, |_| {}).await;
    let mut reader = watching.open(Purpose::Cache);
    let mut buf = vec![0; 256 * 1024];
    let mut read = 0;
    while read < 4 * LARGE {
        read += reader.read(&mut buf).await.unwrap();
    }
    let peak = watching.rig.nntp.peak_connections();
    assert!(
        peak >= POOL - 2,
        "read-ahead reached {peak} of {POOL} connections"
    );
    assert!(peak <= POOL, "the account cap was exceeded: {peak}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_read_ahead_is_bounded_by_bytes() {
    let _serial = serial().await;
    const WINDOW: usize = 12;
    let watching = Watching::new(|config| {
        config.cache_read_ahead_bytes = (WINDOW * ARTICLE) as u64;
    })
    .await;
    let before = watching.rig.nntp.body_commands();
    let mut stalled = watching.open(Purpose::Cache);
    let mut buf = vec![0; ARTICLE];
    stalled.read_exact(&mut buf).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let fetched = watching.rig.nntp.body_commands() - before;
    assert!(
        fetched <= WINDOW + 2,
        "a stalled reader fetched {fetched} articles with a window of {WINDOW}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quick_check_answers_during_a_download_and_a_cache_ahead() {
    let _serial = serial().await;
    let watching = Watching::new(|_| {}).await;
    watching.rig.start_download(&watching.engine).await;
    let bodies = tokio::spawn(read_all(watching.open(Purpose::Cache)));
    tokio::time::sleep(DRAINED).await;
    let started = Instant::now();
    let sample = watching
        .engine
        .quick_check(&watching.film.key, 64, Some(0))
        .await
        .unwrap();
    let took = started.elapsed();
    assert_eq!((sample.checked, sample.missing), (64, 0));
    assert!(took < Duration::from_secs(2), "the check took {took:?}");
    bodies.await.unwrap();
}
