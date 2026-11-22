use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

#[tokio::test]
async fn the_queue_pauses_below_the_free_space_floor_and_resumes_when_space_returns() {
    let post = Posted::new("ep", "Show.S01E01.mkv", video(8 * ARTICLE + 1, 51));
    let rig = Rig::new(post.articles.clone()).await;
    rig.space.0.store(1000, Ordering::SeqCst);
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(engine.paused_for_space());
    let paused = engine.job(&id).unwrap();
    assert_eq!(paused.segments_done, 0, "{paused:?}");

    rig.space.0.store(u64::MAX, Ordering::SeqCst);
    let dir = completed(&finish(&engine, &id).await);
    assert!(!engine.paused_for_space());
    assert_file(&dir, &post.name, &post.data);
}

struct Allocated {
    dir: std::path::PathBuf,
    budget: std::sync::atomic::AtomicU64,
}

impl kob_usenet::FreeSpace for Allocated {
    fn available(&self, _: &std::path::Path) -> std::io::Result<u64> {
        Ok(self
            .budget
            .load(Ordering::SeqCst)
            .saturating_sub(allocated(&self.dir)))
    }
}

fn allocated(dir: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => allocated(&e.path()),
            Ok(m) => m.blocks() * 512,
            Err(_) => 0,
        })
        .sum()
}

const FLOOR: u64 = 1 << 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_whose_reservation_would_cross_the_floor_downloads_sparse_instead_of_pausing_itself()
{
    let post = Posted::new("ep", "Show.S01E01.mkv", video(8 * ARTICLE, 52));
    let rig = Rig::new(post.articles.clone()).await;
    let space = std::sync::Arc::new(Allocated {
        dir: rig.dest.path().to_path_buf(),
        budget: (FLOOR + post.data.len() as u64 - 1).into(),
    });
    let engine = rig
        .engine_with(|c| {
            c.download.free_space = space.clone();
            c.download.free_space_floor = FLOOR;
        })
        .await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &post.name, &post.data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_never_pauses_on_its_own_reservations() {
    let one = Posted::new("one", "Show.S01E01.mkv", video(32 * ARTICLE, 53));
    let two = Posted::new("two", "Show.S01E02.mkv", video(32 * ARTICLE, 54));
    let rig = Rig::new(one.articles.iter().chain(&two.articles).cloned().collect()).await;
    let size = one.data.len() as u64;
    let space = std::sync::Arc::new(Allocated {
        dir: rig.dest.path().to_path_buf(),
        budget: (FLOOR + 2 * size - 1).into(),
    });
    let engine = rig
        .engine_with(|c| {
            c.download.free_space = space.clone();
            c.download.free_space_floor = FLOOR;
        })
        .await;
    let spec = rig.spec("Show", nzb::of(&[&one, &two]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &one.name, &one.data);
    assert_file(&dir, &two.name, &two.data);
}
