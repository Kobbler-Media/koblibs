mod fixtures;

use fixtures::UNEVEN_KEYFRAMES;
use kob_ffmpeg::session::{OverlayConfig, PlaylistSegment, SessionConfig, TranscodeSession};
use std::path::Path;
use std::time::{Duration, Instant};

const DURATION: f64 = 60.0;
const RESUME_AT: f64 = 31.0;
const FIRST_KEYFRAME_BEFORE_RESUME: f64 = 25.5;

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn written_durations(dir: &Path, generation: u32) -> Vec<f64> {
    std::fs::read_to_string(dir.join(format!("gen_{generation}/stream.m3u8")))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("#EXTINF:"))
        .map(|d| d.trim_end_matches(',').parse().unwrap())
        .collect()
}

fn finished(dir: &Path, generation: u32) -> bool {
    std::fs::read_to_string(dir.join(format!("gen_{generation}/stream.m3u8")))
        .is_ok_and(|p| p.contains("#EXT-X-ENDLIST"))
}

fn overlay() -> OverlayConfig {
    OverlayConfig {
        video_encoder: "libx264".into(),
        video_bitrate: None,
        audio_encoder: Some("aac".into()),
        audio_bitrate: Some(32_000),
        hw_device: None,
    }
}

fn copy_session(dir: &Path) -> TranscodeSession {
    let mut config = SessionConfig::new(dir, DURATION)
        .segment_duration(6.0)
        .audio_encoder("aac");
    config.start_time = Some(RESUME_AT);
    TranscodeSession::start(&fixtures::uneven_gop_video(), config).unwrap()
}

fn start_times(dir: &Path, segs: &[PlaylistSegment], from: f64) -> Vec<(u32, u32, f64)> {
    let mut t = from;
    segs.iter()
        .map(|s| {
            let at = (s.generation, s.local_index, t);
            t += written_durations(dir, s.generation)[s.local_index as usize];
            at
        })
        .collect()
}

fn assert_splices_cleanly(dir: &Path, segs: &[PlaylistSegment], overlay_gen: u32) {
    for s in segs {
        let written = written_durations(dir, s.generation);
        assert_eq!(
            Some(&s.duration),
            written.get(s.local_index as usize),
            "gen {} seg {} listed with a duration it was not written with",
            s.generation,
            s.local_index
        );
    }
    assert!(
        segs.iter().any(|s| s.generation == overlay_gen),
        "repair not spliced in"
    );

    let primary: Vec<f64> = written_durations(dir, 0);
    let primary_start =
        |local: u32| FIRST_KEYFRAME_BEFORE_RESUME + primary[..local as usize].iter().sum::<f64>();
    let listed = start_times(dir, segs, FIRST_KEYFRAME_BEFORE_RESUME);
    for &(generation, local, at) in &listed {
        if generation == 0 {
            assert!(
                (at - primary_start(local)).abs() < 0.05,
                "copy seg {local} lands at {at:.3}s in the playlist but starts at {:.3}s",
                primary_start(local)
            );
        }
    }
    let first_repair = listed.iter().find(|s| s.0 == overlay_gen).unwrap().2;
    assert!(
        UNEVEN_KEYFRAMES
            .iter()
            .any(|k| (k - first_repair).abs() < 0.05),
        "repair starts at {first_repair:.3}s, not on a copy segment boundary"
    );
    let total: f64 = segs.iter().map(|s| s.duration).sum();
    assert!(
        (FIRST_KEYFRAME_BEFORE_RESUME + total - DURATION).abs() < 0.2,
        "playlist ends at {:.3}s",
        FIRST_KEYFRAME_BEFORE_RESUME + total
    );
}

#[test]
fn a_repair_over_the_first_segments_of_a_resumed_copy_splices_in_by_time() {
    let dir = tempfile::tempdir().unwrap();
    let session = copy_session(dir.path());
    wait_for("the copy to finish", || !session.is_running());
    let start = session.time_to_segment(RESUME_AT);

    let overlay_gen = session
        .start_overlay(RESUME_AT..RESUME_AT + 10.0, overlay())
        .unwrap();
    wait_for("the repair to finish", || finished(dir.path(), overlay_gen));
    session.update_segments();

    assert_splices_cleanly(
        dir.path(),
        &session.playlist_segments_from(start),
        overlay_gen,
    );
}

#[test]
fn a_repair_while_the_copy_is_still_running_keeps_the_copy_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let session = copy_session(dir.path());
    wait_for("the first copy segments", || {
        written_durations(dir.path(), 0).len() >= 2
    });
    let start = session.time_to_segment(RESUME_AT);

    let overlay_gen = session
        .start_overlay(RESUME_AT..RESUME_AT + 10.0, overlay())
        .unwrap();
    wait_for("the copy to finish", || !session.is_running());
    wait_for("the repair to finish", || finished(dir.path(), overlay_gen));
    session.update_segments();

    assert_splices_cleanly(
        dir.path(),
        &session.playlist_segments_from(start),
        overlay_gen,
    );
}
