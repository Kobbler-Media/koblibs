use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kob_usenet::{Engine, ImportMode, JobDisk, Purpose};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::durable::marked_done;
use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, video};
use crate::support::{Rig, assert_file, completed, finish};

const WAIT: Duration = Duration::from_secs(5);
const GATE_LIMIT: Duration = Duration::from_secs(12);
// The runtime's timers stall with a blocked worker, so waits here use the test's own thread.
const POLL: Duration = Duration::from_millis(5);
const SETTLE: Duration = Duration::from_millis(100);
const NO_HEDGE: Duration = Duration::from_mins(5);

#[derive(Default)]
struct GatedSync {
    entered: AtomicUsize,
    allowed: Mutex<usize>,
    opened: Condvar,
}

impl GatedSync {
    fn allow(&self, syncs: usize) {
        *self.allowed.lock().unwrap() = syncs;
        self.opened.notify_all();
    }

    fn entered(&self, syncs: usize) {
        let started = Instant::now();
        while self.entered.load(Ordering::SeqCst) < syncs {
            assert!(started.elapsed() < WAIT, "sync {syncs} never started");
            std::thread::sleep(POLL);
        }
    }
}

impl JobDisk for GatedSync {
    fn sync(&self, file: &File) -> io::Result<()> {
        let turn = self.entered.fetch_add(1, Ordering::SeqCst);
        let allowed = self.allowed.lock().unwrap();
        let (_allowed, _) = self
            .opened
            .wait_timeout_while(allowed, GATE_LIMIT, |allowed| *allowed <= turn)
            .unwrap();
        file.sync_data()
    }
}

async fn gated(rig: &Rig) -> (Engine, Arc<GatedSync>) {
    let disk = Arc::new(GatedSync::default());
    let shared = Arc::clone(&disk);
    let engine = rig
        .engine_with(|config| {
            config.download.disk = shared;
            config.download.straggler_after = NO_HEDGE;
        })
        .await;
    (engine, disk)
}

fn written(engine: &Engine, id: &str, segments: u64) {
    let started = Instant::now();
    loop {
        let done = engine.job(id).unwrap().segments_done;
        if done >= segments {
            return;
        }
        assert!(
            started.elapsed() < WAIT,
            "{done} of {segments} segments written while a sync was in progress"
        );
        std::thread::sleep(POLL);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_job_keeps_fetching_and_writing_while_a_sync_is_in_progress() {
    let post = Posted::new("ep", "Show.S01E04.mkv", video(40 * ARTICLE, 71));
    let rig = Rig::new(post.articles.clone()).await;
    let held: Vec<_> = (8..24)
        .map(|part| rig.nntp.hold_article(post.id(part)).1)
        .collect();
    let (engine, disk) = gated(&rig).await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    disk.entered(1);
    drop(held);

    written(&engine, &id, 40);

    disk.allow(usize::MAX);
    assert_file(
        &completed(&finish(&engine, &id).await),
        &post.name,
        &post.data,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_stream_read_is_served_while_a_job_sync_is_in_progress() {
    let job = Posted::new("ep", "Show.S01E05.mkv", video(12 * ARTICLE, 73));
    let film = Posted::new("film", "Film.2026.mkv", video(6 * ARTICLE, 79));
    let rig = Rig::new(articles([&job, &film])).await;
    let (_arrived, release) = rig.nntp.hold_article(job.id(6));
    let (engine, disk) = gated(&rig).await;
    let release_key = engine
        .import(
            Some(&nzb::of(&[&film])),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap()
        .key;
    let spec = rig.spec("Show", nzb::of(&[&job]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    disk.entered(1);

    let mut reader = engine.open(&release_key, 0, Purpose::Play).unwrap();
    let at = 3 * ARTICLE as u64;
    let mut buf = vec![0; 4096];
    let asked = Instant::now();
    reader.seek(io::SeekFrom::Start(at)).await.unwrap();
    reader.read_exact(&mut buf).await.unwrap();
    let took = asked.elapsed();

    drop(release);
    disk.allow(usize::MAX);
    assert!(
        took < WAIT,
        "the stream read waited {took:?} behind a job's sync"
    );
    assert_eq!(buf, film.data[at as usize..at as usize + 4096]);
    assert_file(
        &completed(&finish(&engine, &id).await),
        &job.name,
        &job.data,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_segment_written_during_a_sync_is_marked_only_after_a_later_sync() {
    let post = Posted::new("ep", "Show.S01E06.mkv", video(12 * ARTICLE, 83));
    let rig = Rig::new(post.articles.clone()).await;
    let (_arrived, release) = rig.nntp.hold_article(post.id(6));
    let (engine, disk) = gated(&rig).await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    disk.entered(1);
    std::thread::sleep(SETTLE);
    let before = engine.job(&id).unwrap().segments_done as usize;
    assert!(before < 12);
    assert_eq!(marked_done(&rig), 0, "marked before its sync finished");

    drop(release);
    written(&engine, &id, 12);
    assert_eq!(marked_done(&rig), 0, "marked before its sync finished");

    disk.allow(1);
    disk.entered(2);
    let started = Instant::now();
    while marked_done(&rig) < before && started.elapsed() < WAIT {
        std::thread::sleep(POLL);
    }
    assert_eq!(
        marked_done(&rig),
        before,
        "only the segments written before the finished sync began are done"
    );

    disk.allow(usize::MAX);
    assert_file(
        &completed(&finish(&engine, &id).await),
        &post.name,
        &post.data,
    );
    assert_eq!(marked_done(&rig), 12);
}

#[derive(Default)]
struct StuckWrites {
    open: Mutex<bool>,
    opened: Condvar,
}

impl StuckWrites {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

impl JobDisk for StuckWrites {
    fn write_at(&self, file: &File, data: &[u8], at: u64) -> io::Result<()> {
        let open = self.open.lock().unwrap();
        let (open, _) = self
            .opened
            .wait_timeout_while(open, GATE_LIMIT, |open| !*open)
            .unwrap();
        drop(open);
        file.write_all_at(data, at)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disk_slower_than_the_network_bounds_the_articles_held_in_memory() {
    const WINDOW: usize = 4 * 4;
    const PLANNING: usize = 2;
    let post = Posted::new("ep", "Show.S01E07.mkv", video(80 * ARTICLE, 89));
    let rig = Rig::new(post.articles.clone()).await;
    let disk = Arc::new(StuckWrites::default());
    let shared = Arc::clone(&disk);
    let engine = rig
        .engine_with(|config| {
            config.download.disk = shared;
            config.download.straggler_after = NO_HEDGE;
        })
        .await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    let started = Instant::now();
    while rig.nntp.body_commands() < WINDOW && started.elapsed() < WAIT {
        std::thread::sleep(POLL);
    }
    std::thread::sleep(Duration::from_millis(500));
    let fetched = rig.nntp.body_commands();
    disk.open();
    assert!(
        fetched <= WINDOW + PLANNING,
        "{fetched} articles fetched while no write could finish"
    );
    assert_file(
        &completed(&finish(&engine, &id).await),
        &post.name,
        &post.data,
    );
}
