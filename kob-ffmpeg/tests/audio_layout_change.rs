mod fixtures;

use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
use std::time::{Duration, Instant};

/// Prod encodes with libfdk_aac, which a stock FFmpeg build may not carry.
fn audio_encoder() -> &'static str {
    let encoders = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("ffmpeg must be available");
    match String::from_utf8_lossy(&encoders.stdout).contains("libfdk_aac") {
        true => "libfdk_aac",
        false => "aac",
    }
}

/// A stream whose audio layout changes mid-way rebuilds the audio filter graph. Flushing the
/// old graph hands the encoder a short final frame, and the next frame from the new graph then
/// fails with EINVAL ("frame_size was not respected for a non-last frame").
#[test]
fn audio_transcode_survives_a_mid_stream_channel_layout_change() {
    let dir = tempfile::tempdir().unwrap();
    let config = SessionConfig::new(dir.path(), 2.0)
        .segment_duration(1.0)
        .audio_encoder(audio_encoder())
        .audio_bitrate(128_000);
    let session =
        TranscodeSession::start(&fixtures::ac3_channel_change_with_video_mkv(), config).unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    while session.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(session.error(), None);
    session.update_segments();
    assert!(!session.playlist_segments_from(0).is_empty());
}
