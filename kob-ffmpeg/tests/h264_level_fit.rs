mod fixtures;

use kob_ffmpeg::hw::device::HwDeviceContext;
use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
use kob_ffmpeg::transcode::{HlsConfig, transcode_to_hls};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// (level, r_frame_rate, height, frames) of the encode in `dir`'s init and segments.
fn encoded(dir: &Path) -> (u64, String, u64, u64) {
    let mut segments: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "m4s"))
        .collect();
    segments.sort();
    let mut bytes = std::fs::read(dir.join("init.mp4")).unwrap();
    for segment in segments {
        bytes.extend(std::fs::read(segment).unwrap());
    }
    let joined = dir.join("joined.mp4");
    std::fs::write(&joined, bytes).unwrap();
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args([
            "-show_entries",
            "stream=level,r_frame_rate,height,nb_read_frames",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&joined)
        .output()
        .expect("ffprobe must be available");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("no {key} in {stdout:?}"))
            .to_string()
    };
    (
        field("level").parse().unwrap(),
        field("r_frame_rate"),
        field("height").parse().unwrap(),
        field("nb_read_frames").parse().unwrap(),
    )
}

fn assert_fitted_to_4_1(dir: &Path, what: &str) {
    assert_eq!(
        encoded(dir),
        (41, "30/1".to_string(), 1080, 60),
        "{what}: 1080p60 at level 4.1 is 1080p30"
    );
}

fn session(encoder: &str, hw: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut config = SessionConfig::new(dir.path(), 2.0)
        .segment_duration(1.0)
        .video_encoder(encoder)
        .h264_level(41);
    if hw {
        config = config.hw_device(Arc::new(HwDeviceContext::cuda().unwrap()));
    }
    let session = TranscodeSession::start(&fixtures::full_hd_60(), config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while session.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(session.error(), None, "{encoder} hw={hw}");
    assert_fitted_to_4_1(&session.gen_dir(0), &format!("{encoder} hw={hw}"));
    dir
}

#[test]
fn a_session_capped_at_4_1_halves_1080p60() {
    session("libx264", false);
}

#[test]
fn an_hls_transcode_capped_at_4_1_halves_1080p60() {
    let dir = tempfile::tempdir().unwrap();
    transcode_to_hls(
        &fixtures::full_hd_60(),
        &HlsConfig::new(dir.path())
            .segment_duration(1.0)
            .video_encoder("libx264")
            .h264_level(41),
    )
    .unwrap();
    assert_fitted_to_4_1(dir.path(), "hls libx264");
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn nvenc_opens_for_1080p60_capped_at_4_1() {
    session("h264_nvenc", false);
    session("h264_nvenc", true);
}
