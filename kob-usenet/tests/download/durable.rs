use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kob_usenet::JobDisk;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::repair::Protected;
use crate::support::{Rig, assert_file, completed, finish, listing};

const MAP_HEADER: usize = 16;

pub fn marked_done(rig: &Rig) -> usize {
    listing(rig.data.path())
        .iter()
        .filter(|path| path.ends_with("map-0"))
        .map(|path| std::fs::read(rig.data.path().join(path)).unwrap())
        .map(|map| {
            map[MAP_HEADER.min(map.len())..]
                .iter()
                .filter(|&&mark| mark == 1)
                .count()
        })
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segments_are_marked_done_on_disk_only_after_their_data_is_synced() {
    let post = Posted::new("ep", "Show.S01E03.mkv", video(30 * ARTICLE + 1, 63));
    let rig = Rig::new(post.articles.clone()).await;
    let (arrived, release) = rig.nntp.hold_article(post.id(15));
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&[&post]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    arrived.await.unwrap();
    while engine.job(&id).unwrap().segments_done < 10 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        marked_done(&rig),
        0,
        "no batch sync ran yet, so no segment may be marked done on disk"
    );
    drop(release);
    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &post.name, &post.data);
    assert_eq!(marked_done(&rig), 31);
}

// Each record starts with a one-byte version.
const PHASE_TAG: usize = 1;
const DOWNLOADING: u8 = 0;
const MOVING: u8 = 3;

struct Recorder {
    data: PathBuf,
    syncs: Mutex<Vec<(PathBuf, Option<u8>)>>,
    fail_once: Mutex<Option<PathBuf>>,
}

impl Recorder {
    fn new(rig: &Rig, fail_once: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            data: rig.data.path().to_path_buf(),
            syncs: Mutex::default(),
            fail_once: Mutex::new(fail_once),
        })
    }

    fn phase(&self) -> Option<u8> {
        let jobs = std::fs::read_dir(self.data.join("jobs")).ok()?;
        jobs.filter_map(Result::ok)
            .find_map(|job| std::fs::read(job.path().join("phase")).ok())
            .and_then(|phase| phase.get(PHASE_TAG).copied())
            .or(Some(DOWNLOADING))
    }

    fn phases_of(&self, wanted: &Path) -> Vec<Option<u8>> {
        self.syncs
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path == wanted)
            .map(|&(_, phase)| phase)
            .collect()
    }

    fn first_named(&self, name: &str) -> Option<Option<u8>> {
        self.syncs
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.file_name().is_some_and(|n| n == name))
            .map(|&(_, phase)| phase)
    }
}

impl JobDisk for Recorder {
    fn sync_path(&self, path: &Path) -> io::Result<()> {
        self.syncs
            .lock()
            .unwrap()
            .push((path.to_path_buf(), self.phase()));
        let mut failing = self.fail_once.lock().unwrap();
        if failing.as_deref() == Some(path) && self.phase() == Some(MOVING) {
            *failing = None;
            return Err(io::Error::other("sync failed"));
        }
        Ok(())
    }
}

async fn run_recorded(rig: &Rig, recorder: &Arc<Recorder>, posts: &[&Posted]) -> PathBuf {
    let disk = Arc::clone(recorder);
    let engine = rig
        .engine_with(|config| config.download.disk = disk as Arc<dyn JobDisk>)
        .await;
    let spec = rig.spec("Show", nzb::of(posts), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();
    completed(&finish(&engine, &id).await)
}

#[tokio::test]
async fn repaired_bytes_are_synced_before_the_job_is_marked_downloaded() {
    let release = Protected::new(
        vec![Posted::new(
            "e01",
            "Show.S01E01.mkv",
            video(12 * ARTICLE + 500, 71),
        )],
        "Show",
        &[(0, 2), (2, 8)],
    );
    let gone = release.files[0].id(4).to_owned();
    let rig = Rig::new(release.articles_without(&[&gone])).await;
    let recorder = Recorder::new(&rig, None);
    let dir = run_recorded(&rig, &recorder, &release.posts()).await;
    assert_file(&dir, "Show.S01E01.mkv", &release.files[0].data);
    assert_eq!(
        recorder.first_named("Show.S01E01.mkv"),
        Some(Some(DOWNLOADING)),
        "the repaired file is durable while the job still says Downloading"
    );
}

#[tokio::test]
async fn both_parents_are_synced_around_the_final_move() {
    let post = Posted::new("ep", "Show.S01E03.mkv", video(3 * ARTICLE + 1, 72));
    let rig = Rig::new(post.articles.clone()).await;
    let recorder = Recorder::new(&rig, None);
    let dir = run_recorded(&rig, &recorder, &[&post]).await;
    let dest = dir.parent().unwrap();
    let staging_parent = dest.join(".incomplete");
    assert!(recorder.phases_of(dest).contains(&Some(MOVING)));
    assert!(recorder.phases_of(&staging_parent).contains(&Some(MOVING)));
}

#[tokio::test]
async fn a_failed_parent_sync_after_the_move_is_retried_before_completing() {
    let post = Posted::new("ep", "Show.S01E04.mkv", video(3 * ARTICLE + 1, 73));
    let rig = Rig::new(post.articles.clone()).await;
    let dest = rig.dest.path().to_path_buf();
    let recorder = Recorder::new(&rig, Some(dest.clone()));
    let dir = run_recorded(&rig, &recorder, &[&post]).await;
    assert_file(&dir, &post.name, &post.data);
    let moving = recorder
        .phases_of(&dest)
        .into_iter()
        .filter(|&phase| phase == Some(MOVING))
        .count();
    assert!(moving >= 2, "the resumed move syncs the parent again");
}
