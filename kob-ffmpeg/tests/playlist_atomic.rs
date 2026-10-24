//! A playlist is read while it is being written: the server serves the live one straight
//! off disk, and a session re-reads each generation's for segment durations. A rewrite in
//! place has a moment where the file is empty or cut short, so every rewrite has to land
//! whole — written aside and renamed over, which gives each version its own inode.

mod fixtures;

use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
use kob_ffmpeg::transcode::{HlsConfig, HlsWindow, transcode_to_hls};
use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn watch_inodes(playlist: PathBuf, done: Arc<AtomicBool>) -> std::thread::JoinHandle<HashSet<u64>> {
    std::thread::spawn(move || {
        let mut seen = HashSet::new();
        while !done.load(Ordering::Acquire) {
            if let Ok(meta) = std::fs::metadata(&playlist) {
                seen.insert(meta.ino());
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        seen
    })
}

fn assert_replaced_whole(seen: &HashSet<u64>, what: &str) {
    assert!(
        seen.len() > 1,
        "{what}: the playlist kept one inode across its rewrites, so it is rewritten in place"
    );
}

fn hls(dir: &Path, window: HlsWindow) -> HlsConfig {
    HlsConfig::new(dir)
        .video_encoder("libx264")
        .segment_duration(1.0)
        .hls_window(window)
}

#[test]
fn every_transcode_playlist_is_replaced_whole() {
    for (what, window) in [
        ("sliding", HlsWindow::Sliding { segments: 6 }),
        ("dvr", HlsWindow::Dvr),
        ("vod", HlsWindow::Vod),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let watcher = watch_inodes(dir.path().join("stream.m3u8"), done.clone());
        transcode_to_hls(&fixtures::minute_video(), &hls(dir.path(), window)).unwrap();
        done.store(true, Ordering::Release);
        assert_replaced_whole(&watcher.join().unwrap(), what);
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")),
            "{what}: a temporary file was left behind"
        );
    }
}

#[test]
fn a_session_generation_playlist_is_replaced_whole() {
    let dir = tempfile::tempdir().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let watcher = watch_inodes(dir.path().join("gen_0/stream.m3u8"), done.clone());
    let config = SessionConfig::new(dir.path(), 60.0)
        .segment_duration(1.0)
        .video_encoder("libx264");
    let session = TranscodeSession::start(&fixtures::minute_video(), config).unwrap();
    while session.is_running() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    done.store(true, Ordering::Release);
    assert_replaced_whole(&watcher.join().unwrap(), "session");

    session.update_segments();
    let covered: f64 = session
        .playlist_segments_from(0)
        .iter()
        .map(|s| s.duration)
        .sum();
    assert!(
        covered > 55.0,
        "segment durations still read back: {covered:.1}s"
    );
}
