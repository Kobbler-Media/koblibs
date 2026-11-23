use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::{LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, JobSpec, JobState};

use crate::link::Link;
use crate::measure::{LIBRARY_BYTES, Provider, article_size};
use crate::rig::{media, single_nzb};
use crate::support::server;

const SAMPLE: Duration = Duration::from_millis(100);

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "measurement: run alone, with --release"]
async fn job_alone() {
    let article = article_size();
    let provider = Provider::spawn();
    let (dir, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut account = server("a", &LoopbackServer::start(Vec::new()).await.unwrap(), 0);
    account.port = provider.port;
    account.connections = 49;
    let mut config = EngineConfig::new(dir.path().to_path_buf(), vec![account]);
    config.download.disk = crate::model_disk::from_env();
    let engine = Engine::new(config).await.unwrap();
    let added = Instant::now();
    engine
        .add_job(JobSpec {
            id: "library".into(),
            name: "Library.2026".into(),
            category: "movies".into(),
            nzb: single_nzb(
                "lib",
                "Library.2026.mkv",
                LIBRARY_BYTES.div_ceil(article),
                article,
            ),
            password: None,
            dest_dir: dest.path().to_path_buf(),
        })
        .await
        .unwrap();
    let (mut seen, mut moved, mut first, mut last) = (0, added, None, added);
    let mut longest = Duration::ZERO;
    let status = std::thread::scope(|_| {
        loop {
            let status = engine.job("library").unwrap();
            let now = Instant::now();
            if status.bytes_done > seen {
                if first.is_some() {
                    longest = longest.max(now - moved);
                } else {
                    first = Some(now);
                }
                seen = status.bytes_done;
                moved = now;
                last = now;
            }
            if matches!(status.state, JobState::Completed | JobState::Failed) {
                break status;
            }
            std::thread::sleep(SAMPLE);
        }
    });
    assert_eq!(status.state, JobState::Completed, "{status:?}");
    let ended = Instant::now();
    let first = first.unwrap();
    println!(
        "RESULT job_mbs={:.1} data_mbs={:.1} first_byte_s={:.2} longest_stall_s={:.2} tail_s={:.2} total_s={:.2}",
        status.bytes_total as f64 / (ended - added).as_secs_f64() / 1e6,
        status.bytes_total as f64 / (last - first).as_secs_f64() / 1e6,
        (first - added).as_secs_f64(),
        longest.as_secs_f64(),
        (ended - last).as_secs_f64(),
        (ended - added).as_secs_f64(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_availability_probe_spreads_over_the_whole_pool() {
    const PARTS: usize = 256;
    const SIZE: usize = 2_000;
    const POOL: usize = 40;
    const ROUND_TRIP: Duration = Duration::from_millis(200);
    let _serial = crate::rig::serial().await;
    let articles: Vec<_> = post("gap", "Gap.2026.mkv", &media(PARTS * SIZE, 241), SIZE)
        .into_iter()
        .enumerate()
        .filter(|(part, _)| part % 8 != 3)
        .map(|(_, article)| article)
        .collect();
    let nntp = LoopbackServer::start(articles).await.unwrap();
    let front = Link::new(100_000_000, ROUND_TRIP).front(nntp.addr()).await;
    let (dir, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut account = server("a", &nntp, 0);
    account.port = front.port();
    account.connections = POOL;
    let config = EngineConfig::new(dir.path().to_path_buf(), vec![account]);
    let engine = Engine::new(config).await.unwrap();
    let added = Instant::now();
    engine
        .add_job(JobSpec {
            id: "gap".into(),
            name: "Gap.2026".into(),
            category: "movies".into(),
            nzb: single_nzb("gap", "Gap.2026.mkv", PARTS, SIZE),
            password: None,
            dest_dir: dest.path().to_path_buf(),
        })
        .await
        .unwrap();
    while engine.job("gap").unwrap().state != JobState::Failed {
        assert!(added.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let took = added.elapsed();
    let sixteen_wide = ROUND_TRIP * (PARTS / 16) as u32;
    assert!(
        took < sixteen_wide,
        "planning took {took:?}; sixteen probes at a time need {sixteen_wide:?} alone"
    );
}
