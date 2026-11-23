use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::{LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, ImportMode, Purpose, Release};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::link::Link;
use crate::rig::{media, serial, single_nzb};
use crate::support::server;

const POOL: usize = 49;
const ARTICLE: usize = 700_000;
const FILM_ARTICLES: usize = 300;
const LINK: u64 = 25_000_000;
const ROUND_TRIP: Duration = Duration::from_millis(206);
const SEEKS: usize = 5;
const SETTLE: Duration = Duration::from_millis(1500);
const SEEK_WITHIN: Duration = Duration::from_millis(1500);

async fn shared() -> (LoopbackServer, tempfile::TempDir, Arc<Engine>, Release) {
    let film = post(
        "film",
        "Film.2026.mkv",
        &media(FILM_ARTICLES * ARTICLE, 239),
        ARTICLE,
    );
    let nntp = LoopbackServer::start(film).await.unwrap();
    let front = Link::new(LINK, ROUND_TRIP).front(nntp.addr()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut account = server("a", &nntp, 0);
    account.port = front.port();
    account.connections = POOL;
    let config = EngineConfig::new(dir.path().to_path_buf(), vec![account]);
    let engine = Arc::new(Engine::new(config).await.unwrap());
    let release = engine
        .import(
            Some(&single_nzb("film", "Film.2026.mkv", FILM_ARTICLES, ARTICLE)),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    (nntp, dir, engine, release)
}

async fn first_byte(engine: &Engine, release: &Release, part: usize) -> Duration {
    let mut reader = engine.open(&release.key, 0, Purpose::Play).unwrap();
    let at = (part * ARTICLE) as u64;
    reader.set_end(at + ARTICLE as u64);
    let started = Instant::now();
    reader.seek(std::io::SeekFrom::Start(at)).await.unwrap();
    let mut buf = vec![0; 4096];
    reader.read_exact(&mut buf).await.unwrap();
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seek_gets_a_large_share_of_a_link_the_cache_ahead_saturates() {
    let _serial = serial().await;
    let (_nntp, _dir, engine, film) = shared().await;
    let cache = {
        let mut reader = engine.open(&film.key, 0, Purpose::Cache).unwrap();
        tokio::spawn(async move {
            let (mut buf, mut read) = (vec![0; 64 * 1024], 0);
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    return read;
                }
                read += n;
            }
        })
    };
    tokio::time::sleep(SETTLE).await;
    let mut seeks = Vec::new();
    for n in 0..SEEKS {
        seeks.push(first_byte(&engine, &film, FILM_ARTICLES - 1 - n * 3).await);
    }
    seeks.sort_unstable();
    let median = seeks[SEEKS / 2];
    assert_eq!(cache.await.unwrap(), FILM_ARTICLES * ARTICLE);
    assert!(
        median < SEEK_WITHIN,
        "median seek {median:?} on a shared link: {seeks:?}"
    );
}
