mod fixtures;

use kob_ffmpeg::session::{PlaylistSegment, PriorRun, SessionConfig, TranscodeSession};
use std::path::Path;
use std::time::{Duration, Instant};

const SEG: f64 = 6.0;
const DURATION: f64 = 60.0;

fn config(dir: &Path) -> SessionConfig {
    SessionConfig::new(dir, DURATION).segment_duration(SEG)
}

fn settle(session: &TranscodeSession) -> Vec<PlaylistSegment> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    session.update_segments();
    session.playlist_segments_from(0)
}

fn key(segs: &[PlaylistSegment]) -> Vec<(u32, u32)> {
    segs.iter().map(|s| (s.generation, s.local_index)).collect()
}

fn cut_short(dir: &Path, end_secs: f64) -> (Vec<PriorRun>, Vec<PlaylistSegment>) {
    let mut first = config(dir);
    first.end_secs = Some(end_secs);
    let session = TranscodeSession::start(&fixtures::minute_video(), first).unwrap();
    let segs = settle(&session);
    assert!(
        !segs.is_empty() && segs.len() < 10,
        "{} segments",
        segs.len()
    );
    (session.runs(), segs)
}

#[test]
fn a_resumed_session_keeps_the_segments_already_on_disk_and_fills_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let (runs, before) = cut_short(dir.path(), 25.0);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].generation, 0);

    let mut resumed = config(dir.path()).initial_generation(1).prior_runs(runs);
    resumed.start_time = Some(0.0);
    let session = TranscodeSession::start(&fixtures::minute_video(), resumed).unwrap();

    session.update_segments();
    let at_once = session.playlist_segments_from(0);
    assert_eq!(
        key(&at_once[..before.len()]),
        key(&before),
        "the old generation must be served before the new one produces anything"
    );

    let after = settle(&session);
    assert_eq!(key(&after[..before.len()]), key(&before));
    assert!(
        after[before.len()..].iter().all(|s| s.generation == 1),
        "only the uncached tail is transcoded again"
    );
    assert_eq!(after[before.len()].local_index, 0);
    assert!(after[before.len()].is_generation_boundary);
    let covered: f64 = after.iter().map(|s| s.duration).sum();
    assert!(covered > DURATION - SEG, "covered {covered:.1}s");
}

#[test]
fn a_resume_from_the_playhead_starts_at_the_end_of_what_is_cached_there() {
    let dir = tempfile::tempdir().unwrap();
    let (runs, before) = cut_short(dir.path(), 25.0);

    let mut resumed = config(dir.path()).initial_generation(1).prior_runs(runs);
    resumed.start_time = Some(SEG);
    let session = TranscodeSession::start(&fixtures::minute_video(), resumed).unwrap();
    let after = settle(&session);
    assert_eq!(key(&after[..before.len()]), key(&before));
    assert_eq!(
        session
            .runs()
            .iter()
            .map(|r| r.generation)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(session.runs()[1].run_start, before.len() as u32);
}

#[test]
fn a_fully_cached_session_resumes_without_transcoding_anything() {
    let dir = tempfile::tempdir().unwrap();
    let session = TranscodeSession::start(&fixtures::minute_video(), config(dir.path())).unwrap();
    let before = settle(&session);
    let runs = session.runs();
    drop(session);

    let resumed = config(dir.path()).initial_generation(1).prior_runs(runs);
    let session = TranscodeSession::start(&fixtures::minute_video(), resumed).unwrap();
    let after = settle(&session);
    assert_eq!(key(&after), key(&before));
    assert!(!dir.path().join("gen_1").exists());
}

#[test]
fn a_prior_run_whose_generation_was_evicted_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (runs, _) = cut_short(dir.path(), 25.0);
    std::fs::remove_dir_all(dir.path().join("gen_0")).unwrap();

    let resumed = config(dir.path()).initial_generation(1).prior_runs(runs);
    let session = TranscodeSession::start(&fixtures::minute_video(), resumed).unwrap();
    let after = settle(&session);
    assert!(after.iter().all(|s| s.generation == 1));
    assert_eq!(after[0].local_index, 0);
}

// The container's duration runs a fraction past the last whole segment, so the segment
// grid has a slot after the final segment that no transcode will ever fill.
#[test]
fn a_session_that_ran_to_the_end_of_the_file_is_fully_cached() {
    let dir = tempfile::tempdir().unwrap();
    let padded = || SessionConfig::new(dir.path(), DURATION + 0.5).segment_duration(SEG);
    let session = TranscodeSession::start(&fixtures::minute_video(), padded()).unwrap();
    let before = settle(&session);
    let runs = session.runs();
    drop(session);

    let mut resumed = padded().initial_generation(1).prior_runs(runs);
    resumed.start_time = Some(SEG * 3.0);
    let session = TranscodeSession::start(&fixtures::minute_video(), resumed).unwrap();
    let after = settle(&session);
    assert_eq!(key(&after), key(&before));
    assert!(!dir.path().join("gen_1").exists());
}
