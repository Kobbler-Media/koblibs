mod fixtures;

use kob_ffmpeg::hw::device::{HwDeviceContext, SharedHwDevice};
use kob_ffmpeg::session::{SessionConfig, TranscodeSession};
use kob_ffmpeg::single_pass::{
    EmbedSrcParams, SinglePassRequest, TrickplayParams, VhashParams, run_single_pass,
};
use kob_ffmpeg::transcode::{HlsConfig, transcode_to_hls};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROBE_W: u32 = 32;
const PROBE_H: u32 = 18;
const SDR_OUT: &str =
    "libplacebo=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv:format=yuv420p";

/// Where libplacebo cannot import FFmpeg's Vulkan device it opens one per graph,
/// and some NVIDIA drivers refuse a new device while another test in this binary
/// holds CUDA contexts and devices of its own. Production shares one device.
fn one_gpu_user() -> std::sync::MutexGuard<'static, ()> {
    static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GPU.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn rgb_frame(input: &Path, filter_prefix: Option<&str>) -> Vec<u8> {
    let mut vf = filter_prefix.map(|f| format!("{f},")).unwrap_or_default();
    vf.push_str(&format!("scale={PROBE_W}:{PROBE_H},format=rgb24"));
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-vf", &vf, "-frames:v", "1", "-f", "rawvideo", "-"])
        .output()
        .expect("ffmpeg must be available");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout.len(), (PROBE_W * PROBE_H * 3) as usize);
    out.stdout
}

fn jpeg_rgb(jpeg: &[u8]) -> Vec<u8> {
    let tmp = tempfile::Builder::new().suffix(".jpg").tempfile().unwrap();
    std::fs::write(tmp.path(), jpeg).unwrap();
    rgb_frame(tmp.path(), None)
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() as f64)
        .sum::<f64>()
        / a.len() as f64
}

fn jpeg_of(source: &Path, filter: &str) -> Vec<u8> {
    let tmp = tempfile::Builder::new().suffix(".jpg").tempfile().unwrap();
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(source)
        .args(["-vf", &format!("{filter}format=yuvj420p"), "-frames:v", "1"])
        .args(["-q:v", "2", "-update", "1", "-y"])
        .arg(tmp.path())
        .status()
        .expect("ffmpeg must be available");
    assert!(status.success());
    std::fs::read(tmp.path()).unwrap()
}

fn assert_tone_mapped(source: &Path, actual: &[u8]) {
    let mapped = rgb_frame(source, Some(SDR_OUT));
    let untouched = rgb_frame(source, None);
    assert_closer_to_mapped(actual, &mapped, &untouched);
}

fn assert_jpeg_tone_mapped(source: &Path, jpeg: &[u8]) {
    let mapped = jpeg_rgb(&jpeg_of(source, &format!("{SDR_OUT},")));
    let untouched = jpeg_rgb(&jpeg_of(source, ""));
    assert_closer_to_mapped(&jpeg_rgb(jpeg), &mapped, &untouched);
}

fn assert_closer_to_mapped(actual: &[u8], mapped: &[u8], untouched: &[u8]) {
    let separation = mean_abs_diff(mapped, untouched);
    assert!(
        separation > 12.0,
        "fixture does not separate a mapped picture from an untouched one"
    );
    let to_mapped = mean_abs_diff(actual, mapped);
    let to_untouched = mean_abs_diff(actual, untouched);
    assert!(
        to_mapped < separation / 3.0 && to_mapped < to_untouched,
        "picture was not tone mapped: {to_mapped:.1} from the mapped reference, \
         {to_untouched:.1} from the untouched decode"
    );
}

fn joined_first_segment(dir: &Path) -> PathBuf {
    let joined = dir.join("first.mp4");
    let mut bytes = std::fs::read(dir.join("init.mp4")).unwrap();
    bytes.extend(std::fs::read(dir.join("seg_00000.m4s")).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    joined
}

fn color_tags(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=color_primaries,color_transfer"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe must be available");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_session(input: &Path, dir: &Path, encoder: &str, hw: Option<SharedHwDevice>) -> PathBuf {
    let mut config = SessionConfig::new(dir, 1.0)
        .segment_duration(1.0)
        .video_encoder(encoder);
    if let Some(hw) = hw {
        config = config.hw_device(hw);
    }
    let session = TranscodeSession::start(input, config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while session.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(session.error(), None);
    joined_first_segment(&session.gen_dir(0))
}

fn cuda() -> Option<SharedHwDevice> {
    match HwDeviceContext::cuda() {
        Ok(hw) if kob_ffmpeg::detect::encoder_available("h264_nvenc") => Some(Arc::new(hw)),
        _ => {
            eprintln!("no CUDA device with NVENC; skipping the GPU leg");
            None
        }
    }
}

#[test]
fn probe_reads_the_dolby_vision_configuration_record() {
    let probe = kob_ffmpeg::probe::probe(&fixtures::dolby_vision_p5()).unwrap();
    let video = &probe.video_streams[0];
    let dv = video.dolby_vision.expect("dolby vision configuration");
    assert_eq!(dv.profile, 5);
    assert_eq!(dv.bl_signal_compatibility_id, 0);
    assert!(!dv.has_compatible_base_layer());
    assert!(video.is_hdr);
    assert_eq!(video.hdr_format.as_deref(), Some("Dolby Vision P5"));
}

#[test]
fn probe_leaves_plain_hdr10_without_dolby_vision() {
    let probe = kob_ffmpeg::probe::probe(&fixtures::hdr10_video()).unwrap();
    let video = &probe.video_streams[0];
    assert_eq!(video.dolby_vision, None);
    assert_eq!(video.hdr_format.as_deref(), Some("HDR10"));
}

#[test]
fn thumbnails_tone_map_hdr_and_dolby_vision_frames() {
    let _gpu = one_gpu_user();
    for source in [fixtures::dolby_vision_p5(), fixtures::hdr10_video()] {
        let frames = kob_ffmpeg::thumbnails::extract_frames_sequential(
            &source, 0.0, 0.5, 2.0, None, 2, false,
        )
        .unwrap();
        assert_eq!(frames[0].timestamp_secs, 0.0);
        assert_jpeg_tone_mapped(&source, &frames[0].jpeg_data);
    }
}

#[test]
fn gpu_decoded_thumbnails_keep_the_dolby_vision_metadata() {
    let _gpu = one_gpu_user();
    let Some(hw) = cuda() else { return };
    let source = fixtures::dolby_vision_p5();
    let frames = kob_ffmpeg::thumbnails::extract_frames_sequential_with_hw(
        &source,
        0.0,
        0.5,
        2.0,
        Some(128),
        2,
        false,
        Some(&hw),
    )
    .unwrap();
    assert_eq!(frames[0].width, 128);
    assert_jpeg_tone_mapped(&source, &frames[0].jpeg_data);
}

#[test]
fn single_pass_tone_maps_trickplay_embeddings_and_vhash() {
    let _gpu = one_gpu_user();
    let source = fixtures::dolby_vision_p5();
    let mut req = SinglePassRequest::new(source.clone());
    req.trickplay = Some(TrickplayParams {
        width: 128,
        fps: 2.0,
        jpeg_quality: 2,
    });
    req.embed_src = Some(EmbedSrcParams {
        width: 128,
        fps: 2.0,
        jpeg_quality: 2,
        windows: Vec::new(),
    });
    req.vhash = Some(VhashParams {
        intermediate_width: 64,
        fps: 2.0,
        windows: Vec::new(),
    });
    let result = run_single_pass(req, None).unwrap();
    assert!(
        result.per_branch_errors.is_empty(),
        "{:?}",
        result.per_branch_errors
    );
    let trickplay = result.trickplay.unwrap();
    assert_eq!(trickplay[0].timestamp_secs, 0.0);
    assert_jpeg_tone_mapped(&source, &trickplay[0].jpeg_data);
    assert_jpeg_tone_mapped(&source, &result.embed_src.unwrap()[0].jpeg_data);

    let luma = |rgb: &[u8]| {
        rgb.chunks(3)
            .map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64)
            .sum::<f64>()
            / (rgb.len() / 3) as f64
    };
    let descriptor = &result.vhash.unwrap()[0].data;
    let mean = descriptor.iter().map(|&v| v as f64).sum::<f64>() / descriptor.len() as f64;
    let mapped = luma(&rgb_frame(&source, Some(SDR_OUT)));
    let untouched = luma(&rgb_frame(&source, None));
    assert!(
        (mean - mapped).abs() < (mean - untouched).abs() && (mean - mapped).abs() < 8.0,
        "vhash mean {mean:.1}: mapped {mapped:.1}, untouched {untouched:.1}"
    );
}

#[test]
fn a_cpu_session_tone_maps_dolby_vision_to_bt709() {
    let _gpu = one_gpu_user();
    let source = fixtures::dolby_vision_p5();
    let dir = tempfile::tempdir().unwrap();
    let first = run_session(&source, dir.path(), "libx264", None);
    assert_eq!(color_tags(&first), "bt709,bt709");
    assert_tone_mapped(&source, &rgb_frame(&first, None));
}

#[test]
fn a_gpu_session_tone_maps_dolby_vision_to_bt709() {
    let _gpu = one_gpu_user();
    let Some(hw) = cuda() else { return };
    let source = fixtures::dolby_vision_p5();
    let dir = tempfile::tempdir().unwrap();
    let first = run_session(&source, dir.path(), "h264_nvenc", Some(hw));
    assert_eq!(color_tags(&first), "bt709,bt709");
    assert_tone_mapped(&source, &rgb_frame(&first, None));
}

#[test]
fn an_hls_transcode_tone_maps_dolby_vision_to_bt709() {
    let _gpu = one_gpu_user();
    let source = fixtures::dolby_vision_p5();
    let mut legs: Vec<(&str, Option<SharedHwDevice>)> = vec![("libx264", None)];
    if let Some(hw) = cuda() {
        legs.push(("h264_nvenc", Some(hw)));
    }
    for (encoder, hw) in legs {
        let dir = tempfile::tempdir().unwrap();
        let mut config = HlsConfig::new(dir.path())
            .segment_duration(1.0)
            .video_encoder(encoder);
        if let Some(hw) = hw {
            config = config.hw_device(hw);
        }
        transcode_to_hls(&source, &config).unwrap();
        let first = joined_first_segment(dir.path());
        assert_eq!(color_tags(&first), "bt709,bt709", "{encoder}");
        assert_tone_mapped(&source, &rgb_frame(&first, None));
    }
}
