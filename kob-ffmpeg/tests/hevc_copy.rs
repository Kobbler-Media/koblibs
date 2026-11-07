mod fixtures;

use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
use kob_ffmpeg::transcode::{HlsConfig, transcode_to_hls};
use std::path::Path;
use std::time::{Duration, Instant};

fn hvcc_payload(init: &Path) -> Vec<u8> {
    let data = std::fs::read(init).unwrap();
    let at = data
        .windows(4)
        .position(|w| w == b"hvcC")
        .expect("no hvcC box")
        - 4;
    let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    data[at + 8..at + size].to_vec()
}

fn decoded_frames(init: &Path, segment: &Path) -> u64 {
    let joined = segment.with_extension("joined.mp4");
    let mut bytes = std::fs::read(init).unwrap();
    bytes.extend(std::fs::read(segment).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(&joined)
        .output()
        .expect("ffprobe must be available");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn assert_playable_hvc1(dir: &Path) {
    let init = dir.join("init.mp4");
    let hvcc = hvcc_payload(&init);
    assert!(
        hvcc.len() > 23,
        "hvcC carries no parameter sets: {} bytes",
        hvcc.len()
    );
    assert!(hvcc[22] >= 3, "hvcC declares {} NAL arrays", hvcc[22]);
    let init_bytes = std::fs::read(&init).unwrap();
    assert!(
        init_bytes.windows(4).any(|w| w == b"hvc1"),
        "not tagged hvc1"
    );
    assert!(decoded_frames(&init, &dir.join("seg_00000.m4s")) > 0);
}

#[test]
fn a_session_copying_hevc_with_only_in_band_parameter_sets_writes_a_decodable_hvc1_init() {
    let dir = tempfile::tempdir().unwrap();
    let session = TranscodeSession::start(
        &fixtures::hevc_inband_params(),
        SessionConfig::new(dir.path(), 4.0).segment_duration(1.0),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(session.error(), None);
    assert_playable_hvc1(&session.gen_dir(0));
}

#[test]
fn an_hls_copy_of_hevc_with_only_in_band_parameter_sets_writes_a_decodable_hvc1_init() {
    let dir = tempfile::tempdir().unwrap();
    transcode_to_hls(
        &fixtures::hevc_inband_params(),
        &HlsConfig::new(dir.path()).segment_duration(1.0),
    )
    .unwrap();
    assert_playable_hvc1(dir.path());
}
