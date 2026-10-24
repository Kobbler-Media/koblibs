//! High-level media file probing — direct replacement for ffprobe CLI invocations.

use std::path::Path;

use crate::Result;
use crate::ffi;
use crate::format::input::{InputContext, MediaType};

// --- Result types ---

/// Complete probe result for a media file.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub format: FormatInfo,
    pub video_streams: Vec<VideoStream>,
    pub audio_streams: Vec<AudioStream>,
    pub subtitle_streams: Vec<SubtitleStream>,
    pub chapters: Vec<ChapterInfo>,
}

/// Container format information.
#[derive(Debug, Clone)]
pub struct FormatInfo {
    pub format_name: String,
    pub duration_seconds: Option<f64>,
    pub bit_rate_kbps: Option<i32>,
    pub size_bytes: Option<i64>,
}

/// Video stream information.
#[derive(Debug, Clone)]
pub struct VideoStream {
    pub index: i32,
    pub codec: String,
    pub profile: Option<String>,
    pub width: i32,
    pub height: i32,
    pub bitrate_kbps: Option<i32>,
    pub framerate: Option<f64>,
    pub bit_depth: Option<i16>,
    pub is_hdr: bool,
    pub hdr_format: Option<String>,
    pub is_interlaced: bool,
}

/// Audio stream information.
#[derive(Debug, Clone)]
pub struct AudioStream {
    pub index: i32,
    pub codec: String,
    pub channels: i32,
    pub sample_rate: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
}

/// Subtitle stream information.
#[derive(Debug, Clone)]
pub struct SubtitleStream {
    pub index: i32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
}

/// Chapter information.
#[derive(Debug, Clone)]
pub struct ChapterInfo {
    pub index: i32,
    pub title: Option<String>,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

// --- Probe functions ---

/// Probe a media file and return format, stream, and chapter information.
///
/// This is a direct replacement for the ffprobe CLI invocation:
/// `ffprobe -v quiet -print_format json -show_format -show_streams -show_chapters`
pub fn probe(path: &Path) -> Result<ProbeResult> {
    probe_with_options(path, ProbeOptions::default())
}

/// Options for customizing probe behavior.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Maximum bytes to read for format detection (default: 100MB).
    pub probesize: i64,
    /// Maximum duration to analyze in microseconds (default: 100s).
    pub max_analyze_duration: i64,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            probesize: 100_000_000,
            max_analyze_duration: 100_000_000,
        }
    }
}

/// Probe with custom options.
pub fn probe_with_options(path: &Path, opts: ProbeOptions) -> Result<ProbeResult> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.set_probesize(opts.probesize);
    ctx.set_max_analyze_duration(opts.max_analyze_duration);
    ctx.find_stream_info()?;

    Ok(extract_probe_result(&ctx))
}

/// Extract probe information from an already-open InputContext.
///
/// Used by the streaming module to probe and then continue reading
/// from the same connection.
pub fn extract_probe_result(ctx: &InputContext) -> ProbeResult {
    let format = extract_format_info(ctx);
    let mut video_streams = Vec::new();
    let mut audio_streams = Vec::new();
    let mut subtitle_streams = Vec::new();

    for stream in ctx.streams() {
        match stream.codec_type() {
            MediaType::Video => {
                if let Some(vs) = extract_video_stream(&stream) {
                    video_streams.push(vs);
                }
            }
            MediaType::Audio => {
                audio_streams.push(extract_audio_stream(&stream));
            }
            MediaType::Subtitle => {
                subtitle_streams.push(extract_subtitle_stream(&stream));
            }
            _ => {}
        }
    }

    let chapters = ctx
        .chapters()
        .enumerate()
        .map(|(i, ch)| ChapterInfo {
            index: i as i32,
            title: ch.title(),
            start_seconds: ch.start_secs(),
            end_seconds: ch.end_secs(),
        })
        .collect();

    ProbeResult {
        format,
        video_streams,
        audio_streams,
        subtitle_streams,
        chapters,
    }
}

/// Probe a media file for URL/network source.
pub fn probe_url(url: &str) -> Result<ProbeResult> {
    probe_url_with_options(url, ProbeOptions::default())
}

/// Probe a URL with custom options.
pub fn probe_url_with_options(url: &str, opts: ProbeOptions) -> Result<ProbeResult> {
    let mut ctx = InputContext::open(url)?;
    ctx.set_probesize(opts.probesize);
    ctx.set_max_analyze_duration(opts.max_analyze_duration);
    ctx.find_stream_info()?;

    Ok(extract_probe_result(&ctx))
}

/// Extract keyframe timestamps (in seconds) from the first video stream.
///
/// Replaces: `ffprobe -select_streams v:0 -show_entries packet=pts_time,dts_time,flags -of csv`
pub fn probe_keyframes(path: &Path) -> Result<Vec<f64>> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let video_idx = ctx.find_best_stream(MediaType::Video).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no video stream")
    })?;

    let time_base = ctx
        .stream(video_idx)
        .map(|s| s.time_base())
        .unwrap_or(ffi::AVRational { num: 1, den: 90000 });

    let tb_f64 = time_base.num as f64 / time_base.den as f64;

    let mut keyframes = Vec::new();
    let mut packet = crate::packet::Packet::new()?;

    while ctx.read_packet(&mut packet)? {
        if packet.stream_index() == video_idx as i32 && packet.is_keyframe() {
            let pts = packet.pts();
            if pts != ffi::AV_NOPTS_VALUE {
                keyframes.push(pts as f64 * tb_f64);
            } else {
                let dts = packet.dts();
                if dts != ffi::AV_NOPTS_VALUE {
                    keyframes.push(dts as f64 * tb_f64);
                }
            }
        }
    }

    Ok(keyframes)
}

// --- Helpers ---

fn extract_format_info(ctx: &InputContext) -> FormatInfo {
    let format_name = unsafe {
        let iformat = ffi::kff_fmt_iformat(ctx.as_ptr());
        if iformat.is_null() {
            "unknown".to_string()
        } else {
            let name = (*iformat).name;
            if name.is_null() {
                "unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    };

    // Prefer the first video stream's duration over the format-level duration.
    // For MKV, format duration comes from the Segment Info header which muxers
    // often set inaccurately (rounded, estimated, or stale from remux). The
    // stream-level duration comes from the track header and is based on actual
    // packet timestamps — typically more accurate.
    let video_stream_duration = ctx
        .streams()
        .find(|s| s.codec_type() == MediaType::Video)
        .and_then(|s| s.duration_secs());
    let format_duration = ctx.duration_secs();
    let duration_seconds = video_stream_duration.or(format_duration);

    FormatInfo {
        format_name,
        duration_seconds,
        bit_rate_kbps: ctx.bit_rate().map(|br| (br / 1000) as i32),
        size_bytes: None, // Not available from AVFormatContext directly — caller can stat the file
    }
}

fn extract_video_stream(stream: &crate::format::input::Stream<'_>) -> Option<VideoStream> {
    let params = stream.codec_parameters();
    if params.is_null() {
        return None;
    }

    let (
        codec_id,
        width,
        height,
        bit_rate,
        profile_id,
        pix_fmt,
        bits_per_raw,
        color_trc,
        color_pri,
    ) = unsafe {
        let p = &*params;
        (
            p.codec_id,
            p.width,
            p.height,
            p.bit_rate,
            p.profile,
            p.format,
            p.bits_per_raw_sample,
            p.color_trc,
            p.color_primaries,
        )
    };

    // Skip attached picture streams (album art)
    if stream.disposition() & ffi::AV_DISPOSITION_ATTACHED_PIC as i32 != 0 {
        return None;
    }

    let codec = unsafe {
        let name = ffi::avcodec_get_name(codec_id);
        if name.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        }
    };

    let profile = unsafe {
        let name = ffi::avcodec_profile_name(codec_id, profile_id);
        if name.is_null() {
            None
        } else {
            Some(
                std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    };

    let pix_fmt_name = unsafe {
        let name = ffi::av_get_pix_fmt_name(pix_fmt);
        if name.is_null() {
            None
        } else {
            Some(
                std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    };

    let bit_depth = if bits_per_raw > 0 {
        Some(bits_per_raw as i16)
    } else {
        pix_fmt_name
            .as_deref()
            .and_then(infer_bit_depth_from_pix_fmt)
    };

    let framerate = {
        let fr = stream.avg_frame_rate();
        if fr.den > 0 && fr.num > 0 {
            Some(fr.num as f64 / fr.den as f64)
        } else {
            let rfr = stream.r_frame_rate();
            if rfr.den > 0 && rfr.num > 0 {
                Some(rfr.num as f64 / rfr.den as f64)
            } else {
                None
            }
        }
    };

    let is_hdr = detect_hdr(color_trc, color_pri, bit_depth);
    let hdr_format = if is_hdr {
        detect_hdr_format(color_trc)
    } else {
        None
    };

    let is_interlaced = unsafe { crate::interlace::codec_parameters_interlaced(params) };

    Some(VideoStream {
        index: stream.index(),
        codec,
        profile,
        width,
        height,
        bitrate_kbps: if bit_rate > 0 {
            Some((bit_rate / 1000) as i32)
        } else {
            None
        },
        framerate,
        bit_depth,
        is_hdr,
        hdr_format,
        is_interlaced,
    })
}

fn extract_audio_stream(stream: &crate::format::input::Stream<'_>) -> AudioStream {
    let params = stream.codec_parameters();
    let (codec_id, channels, sample_rate, bit_rate) = if params.is_null() {
        (0, 0, 0, 0i64)
    } else {
        unsafe {
            let p = &*params;
            (
                p.codec_id,
                p.ch_layout.nb_channels,
                p.sample_rate,
                p.bit_rate,
            )
        }
    };

    let codec = unsafe {
        let name = ffi::avcodec_get_name(codec_id);
        if name.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        }
    };

    let meta = stream.metadata();
    let disposition = stream.disposition();

    AudioStream {
        index: stream.index(),
        codec,
        channels,
        sample_rate: if sample_rate > 0 {
            Some(sample_rate)
        } else {
            None
        },
        bitrate_kbps: if bit_rate > 0 {
            Some((bit_rate / 1000) as i32)
        } else {
            None
        },
        language: meta.get("language").filter(|l| l != "und"),
        title: meta.get("title"),
        is_default: disposition & ffi::AV_DISPOSITION_DEFAULT as i32 != 0,
    }
}

fn extract_subtitle_stream(stream: &crate::format::input::Stream<'_>) -> SubtitleStream {
    let params = stream.codec_parameters();
    let codec_id = if params.is_null() {
        0
    } else {
        unsafe { (*params).codec_id }
    };

    let codec = unsafe {
        let name = ffi::avcodec_get_name(codec_id);
        if name.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        }
    };

    let meta = stream.metadata();
    let disposition = stream.disposition();

    SubtitleStream {
        index: stream.index(),
        codec,
        language: meta.get("language").filter(|l| l != "und"),
        title: meta.get("title"),
        is_default: disposition & ffi::AV_DISPOSITION_DEFAULT as i32 != 0,
        is_forced: disposition & ffi::AV_DISPOSITION_FORCED as i32 != 0,
        is_hearing_impaired: disposition & ffi::AV_DISPOSITION_HEARING_IMPAIRED as i32 != 0,
    }
}

fn detect_hdr(
    color_trc: ffi::AVColorTransferCharacteristic,
    color_pri: ffi::AVColorPrimaries,
    bit_depth: Option<i16>,
) -> bool {
    let has_hdr_transfer =
        color_trc == ffi::AVCOL_TRC_SMPTE2084 || color_trc == ffi::AVCOL_TRC_ARIB_STD_B67;
    let has_wide_gamut = color_pri == ffi::AVCOL_PRI_BT2020;
    let high_bit_depth = bit_depth.is_some_and(|b| b > 8);

    has_hdr_transfer || (has_wide_gamut && high_bit_depth)
}

fn detect_hdr_format(color_trc: ffi::AVColorTransferCharacteristic) -> Option<String> {
    // Side data for DV/HDR10+ would require packet-level inspection.
    // For the common case, transfer characteristic is sufficient.
    match color_trc {
        ffi::AVCOL_TRC_ARIB_STD_B67 => Some("HLG".to_string()),
        ffi::AVCOL_TRC_SMPTE2084 => Some("HDR10".to_string()),
        _ => Some("HDR".to_string()),
    }
}

fn infer_bit_depth_from_pix_fmt(pix_fmt: &str) -> Option<i16> {
    if pix_fmt.contains("12le") || pix_fmt.contains("12be") {
        Some(12)
    } else if pix_fmt.contains("10le") || pix_fmt.contains("10be") || pix_fmt.contains("p10") {
        Some(10)
    } else if pix_fmt.contains("p8")
        || pix_fmt == "yuv420p"
        || pix_fmt == "yuv422p"
        || pix_fmt == "yuv444p"
        || pix_fmt == "yuvj420p"
    {
        Some(8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn generate_test_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new()
            .suffix(".mp4")
            .tempfile()
            .expect("create temp file");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be available for tests");
        assert!(status.success(), "ffmpeg failed to generate test file");
        tmp
    }

    #[test]
    fn test_probe_basic() {
        let tmp = generate_test_file();
        let result = probe(tmp.path()).expect("probe should succeed");

        assert_eq!(result.video_streams.len(), 1);
        assert_eq!(result.audio_streams.len(), 1);
        assert!(result.subtitle_streams.is_empty());

        let video = &result.video_streams[0];
        assert_eq!(video.width, 320);
        assert_eq!(video.height, 240);
        assert_eq!(video.codec, "h264");
        assert!(!video.is_hdr);
        assert!(!video.is_interlaced);
        assert!(video.framerate.is_some());

        let audio = &result.audio_streams[0];
        assert_eq!(audio.codec, "aac");
        assert!(audio.channels > 0);

        assert!(result.format.duration_seconds.is_some());
        let dur = result.format.duration_seconds.unwrap();
        assert!(dur > 0.5 && dur < 2.0, "duration was {dur}");
    }

    #[test]
    fn test_probe_keyframes() {
        let tmp = generate_test_file();
        let kfs = probe_keyframes(tmp.path()).expect("keyframe probe should succeed");
        assert!(!kfs.is_empty(), "should have at least one keyframe");
        // First keyframe should be at or near 0
        assert!(
            kfs[0] < 0.1,
            "first keyframe at {} should be near 0",
            kfs[0]
        );
    }

    #[test]
    fn test_probe_nonexistent_file() {
        let result = probe(Path::new("/nonexistent/file.mp4"));
        assert!(result.is_err());
    }
}
