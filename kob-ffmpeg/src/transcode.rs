//! HLS transcode pipeline.
//!
//! Full pipeline: demux → decode → filter → encode → HLS fMP4 mux.
//! Supports software and hardware-accelerated encoding.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::codec::decoder::DecodeResult;
use crate::codec::encoder::{AudioEncoderConfig, EncodeResult, Encoder, VideoEncoderConfig};
use crate::filter::graph::{FilterGraph, VideoFilterParams};
use crate::format::input::{InputContext, MediaType};
use crate::format::output::OutputContext;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

/// How the HLS playlist treats segments once they are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsWindow {
    /// Sliding window: old segments deleted, MEDIA-SEQUENCE advances, no ENDLIST.
    Sliding { segments: u32 },
    /// Every segment retained and listed as an EXT-X-PLAYLIST-TYPE:EVENT
    /// playlist — the pause/timeshift buffer.
    Dvr,
    /// Every segment retained and listed, ENDLIST on completion.
    Vod,
}

/// Configuration for an HLS transcode session.
///
/// Use `HlsConfig::new(output_dir)` for defaults, then override fields:
/// ```ignore
/// let config = HlsConfig::new("/tmp/hls")
///     .video_encoder("libx264")
///     .audio_encoder("aac")
///     .audio_bitrate(128_000);
/// ```
/// A CPU filter chain whose source format is only known once a frame arrives.
struct CpuFilterPlan {
    chain: String,
    time_base: ffi::AVRational,
    color: crate::filter::graph::VideoColorInfo,
    /// What the encoder was opened with. The encoder has to be opened before a
    /// frame exists, so its format is a prediction; the graph is what makes the
    /// prediction true.
    target: crate::types::PixelFormat,
    /// Source pixel aspect. `scale` derives its output SAR from this, so a
    /// square-pixel default would make it "correct" a 720x576 DVB frame that was
    /// never square to begin with.
    sample_aspect_ratio: ffi::AVRational,
}

impl CpuFilterPlan {
    /// `frame` is the decoded frame as the encoder will see it — already
    /// downloaded from the GPU when the decode really was hardware.
    ///
    /// The chain always ends in a `format` filter. Letting the frame's own
    /// layout through whenever it was "close enough" is what produced correct
    /// greyscale under green and magenta blocks: attaching a hw_device_ctx does
    /// not mean the decoder took it, so a codec the GPU cannot decode (MPEG-2 on
    /// current NVDEC) falls back to software and emits planar YUV into an encoder
    /// opened for semi-planar NV12, which reads the U and V planes as one
    /// interleaved plane. Pinning costs nothing when the guess was right —
    /// `format` is a no-op on a frame that already matches.
    fn build(&self, frame: &Frame) -> crate::Result<FilterGraph> {
        let fmt = frame.format();
        let mut chain = self.chain.clone();
        if !chain.is_empty() {
            chain.push(',');
        }
        chain.push_str(&format!("format=pix_fmts={}", self.target.name()));
        let params = VideoFilterParams {
            width: frame.width(),
            height: frame.height(),
            pixel_format: fmt,
            time_base: self.time_base,
            sample_aspect_ratio: self.sample_aspect_ratio,
            hw_frames_ctx: None,
            color: self.color,
            side_data_frame: std::ptr::null(),
        };
        FilterGraph::new_video(&params, &chain)
    }
}

pub struct HlsConfig {
    /// Output directory for HLS segments and playlist.
    pub output_dir: PathBuf,
    /// Segment duration in seconds (default: 6.0).
    pub segment_duration: f64,
    /// Video encoder name or None for copy (default: None = copy).
    pub video_encoder: Option<String>,
    /// Audio encoder name or None for copy (default: None = copy).
    pub audio_encoder: Option<String>,
    /// Video bitrate in bits/s (default: None = CRF/quality-based).
    pub video_bitrate: Option<i64>,
    /// Audio bitrate in bits/s (default: None).
    pub audio_bitrate: Option<i64>,
    /// Target width for scaling (default: None = keep original).
    pub scale_width: Option<i32>,
    /// Target height for scaling (default: None = keep original).
    pub scale_height: Option<i32>,
    /// Start time offset in seconds (default: None = from beginning).
    pub start_time: Option<f64>,
    /// Video stream index (default: None = best).
    pub video_stream_index: Option<u32>,
    /// Audio stream index (default: None = best).
    pub audio_stream_index: Option<u32>,
    /// Subtitle stream index for burn-in (default: None = no burn-in).
    /// When set, subtitles are overlaid onto the video stream during transcoding.
    /// Only works when video_encoder is set (transcoding, not copy).
    pub subtitle_burn_in: Option<u32>,
    /// Run context for cancellation and progress — checked each packet loop iteration.
    pub run: Option<crate::progress::RunContext>,
    /// Enable DTS monotonicity correction for live/IPTV sources.
    /// When true, backward DTS jumps (from stream reconnects) are corrected
    /// so the HLS muxer always sees increasing timestamps.
    pub live_source: bool,
    /// Hardware device context for GPU-accelerated decoding (NVDEC/VAAPI).
    /// When set, the video decoder outputs hardware frames (e.g., CUDA format)
    /// which are downloaded to CPU before filtering/encoding.
    ///
    /// Shared `Arc` held by the caller for the whole blocking transcode, so a
    /// lazy provider keeps one context shared across active transcodes.
    pub hw_device: Option<crate::hw::device::SharedHwDevice>,
    /// `None` derives the legacy behaviour from `live_source`:
    /// `Sliding { segments: 6 }` when live, `Vod` otherwise.
    pub window: Option<HlsWindow>,
    /// Field-order removal. `Auto` applies `bwdif` only to interlaced sources.
    pub deinterlace: crate::filter::graph::Deinterlace,
    /// Encoder speed preset (`veryfast`, `medium`, …). `None` leaves the
    /// encoder's own default.
    pub preset: Option<String>,
    /// Encoder tuning (`zerolatency`, `film`, …). `zerolatency` disables the
    /// lookahead and frame threading that otherwise cost ~1s on a live path.
    pub tune: Option<String>,
}

impl HlsConfig {
    /// Create a config with sensible defaults. Only output_dir is required.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            segment_duration: 6.0,
            video_encoder: None,
            audio_encoder: None,
            video_bitrate: None,
            audio_bitrate: None,
            scale_width: None,
            scale_height: None,
            start_time: None,
            video_stream_index: None,
            audio_stream_index: None,
            subtitle_burn_in: None,
            run: None,
            live_source: false,
            hw_device: None,
            window: None,
            deinterlace: crate::filter::graph::Deinterlace::None,
            preset: None,
            tune: None,
        }
    }

    fn encoder_options(&self) -> Vec<(String, String)> {
        let mut opts = Vec::new();
        if let Some(ref p) = self.preset {
            opts.push(("preset".to_string(), p.clone()));
        }
        if let Some(ref t) = self.tune {
            opts.push(("tune".to_string(), t.clone()));
        }
        opts
    }

    fn resolved_window(&self) -> HlsWindow {
        self.window.unwrap_or(if self.live_source {
            HlsWindow::Sliding { segments: 6 }
        } else {
            HlsWindow::Vod
        })
    }

    #[must_use]
    pub fn video_encoder(mut self, name: &str) -> Self {
        self.video_encoder = Some(name.to_string());
        self
    }

    #[must_use]
    pub fn audio_encoder(mut self, name: &str) -> Self {
        self.audio_encoder = Some(name.to_string());
        self
    }

    #[must_use]
    pub fn audio_bitrate(mut self, bitrate: i64) -> Self {
        self.audio_bitrate = Some(bitrate);
        self
    }

    #[must_use]
    pub fn video_bitrate(mut self, bitrate: i64) -> Self {
        self.video_bitrate = Some(bitrate);
        self
    }

    #[must_use]
    pub fn segment_duration(mut self, secs: f64) -> Self {
        self.segment_duration = secs;
        self
    }

    #[must_use]
    pub fn preset(mut self, preset: &str) -> Self {
        self.preset = Some(preset.to_string());
        self
    }

    #[must_use]
    pub fn tune(mut self, tune: &str) -> Self {
        self.tune = Some(tune.to_string());
        self
    }

    #[must_use]
    pub fn hls_window(mut self, window: HlsWindow) -> Self {
        self.window = Some(window);
        self
    }

    #[must_use]
    pub fn deinterlace(mut self, mode: crate::filter::graph::Deinterlace) -> Self {
        self.deinterlace = mode;
        self
    }

    #[must_use]
    pub fn subtitle_burn_in(mut self, stream_index: u32) -> Self {
        self.subtitle_burn_in = Some(stream_index);
        self
    }

    #[must_use]
    pub fn run_context(mut self, ctx: crate::progress::RunContext) -> Self {
        self.run = Some(ctx);
        self
    }

    #[must_use]
    pub fn hw_device(mut self, device: crate::hw::device::SharedHwDevice) -> Self {
        self.hw_device = Some(device);
        self
    }
}

/// Result of an HLS transcode session.
pub struct HlsResult {
    pub playlist_path: PathBuf,
    pub segment_dir: PathBuf,
    pub video_packets_written: u32,
}

/// Run an HLS transcode from a file.
///
/// This is a blocking operation — designed to run inside `spawn_blocking`.
pub fn transcode_to_hls(input: &Path, config: &HlsConfig) -> Result<HlsResult> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;
    transcode_to_hls_inner(in_ctx, Some(input), config)
}

/// Run an HLS transcode from an already-open InputContext.
///
/// Used for live streaming where the input comes from a custom I/O context
/// (e.g., channel-backed from an HTTP stream).
pub fn transcode_to_hls_from_ctx(in_ctx: InputContext, config: &HlsConfig) -> Result<HlsResult> {
    transcode_to_hls_inner(in_ctx, None, config)
}

/// Core transcode implementation. `input_path` is needed for subtitle burn-in
/// (the `subtitles` filter references the file directly). Pass `None` for
/// channel-backed inputs where subtitle burn-in isn't available.
// Deep video+audio encode/filter pipeline — collapsing the outer `if let`
// chains here would need a risky reflow of every match arm body.
#[allow(clippy::collapsible_if)]
fn transcode_to_hls_inner(
    mut in_ctx: InputContext,
    input_path: Option<&Path>,
    config: &HlsConfig,
) -> Result<HlsResult> {
    std::fs::create_dir_all(&config.output_dir).map_err(|e| {
        crate::error::FfmpegError::new(ffi::AVERROR_EXTERNAL, format!("creating output dir: {e}"))
    })?;

    let video_idx = in_ctx
        .validate_stream_type(config.video_stream_index, MediaType::Video)
        .or_else(|| in_ctx.find_best_stream(MediaType::Video));
    let audio_idx = in_ctx
        .validate_stream_type(config.audio_stream_index, MediaType::Audio)
        .or_else(|| in_ctx.find_best_stream(MediaType::Audio));

    if video_idx.is_none() && audio_idx.is_none() {
        return Err(crate::error::FfmpegError::new(
            ffi::AVERROR_STREAM_NOT_FOUND,
            "no video or audio streams found",
        ));
    }

    let window = config.resolved_window();
    let src_interlaced = video_idx.is_some_and(|v| unsafe {
        crate::filter::graph::codec_parameters_interlaced(
            in_ctx.stream(v).unwrap().codec_parameters(),
        )
    });
    let deint_filter = config.deinterlace.filter_for(src_interlaced);
    // DVB SD is 720x576 with a 64:45 pixel aspect — 16:9 on screen. Dropping it
    // and letting the output default to square pixels renders it as 5:4.
    // MPEG-2's aspect lives in the sequence header, so `find_stream_info` lands it
    // on the codec parameters; AVStream.sample_aspect_ratio stays 0/1 for an
    // MPEG-TS input. Read both, codecpar first, or SD DVB reads as square.
    let src_sar = video_idx
        .and_then(|v| in_ctx.stream(v))
        .map(|s| {
            let par = unsafe { (*s.codec_parameters()).sample_aspect_ratio };
            if par.num > 0 && par.den > 0 {
                par
            } else {
                s.sample_aspect_ratio()
            }
        })
        .filter(|r| r.num > 0 && r.den > 0)
        .unwrap_or(ffi::AVRational { num: 1, den: 1 });

    // Seek if start time specified
    if let Some(start) = config.start_time {
        let seek_ts = (start * ffi::AV_TIME_BASE as f64) as i64;
        in_ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
    }

    let playlist_path = config.output_dir.join("stream.m3u8");
    let playlist_str = playlist_path.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "playlist path not valid UTF-8")
    })?;

    let seg_pattern = config.output_dir.join("seg_%05d.m4s");
    let init_filename = "init.mp4";

    let mut out_ctx = OutputContext::create_with_format(playlist_str, "hls")?;

    // Configure HLS output options.
    // These match the FFmpeg CLI flags that were proven to work for browser MSE playback.
    out_ctx.set_option("hls_time", &config.segment_duration.to_string())?;
    out_ctx.set_option("hls_segment_type", "fmp4")?;
    out_ctx.set_option("hls_fmp4_init_filename", init_filename)?;
    out_ctx.set_option("hls_segment_filename", seg_pattern.to_str().unwrap())?;
    match window {
        HlsWindow::Sliding { segments } => {
            out_ctx.set_option(
                "hls_flags",
                "independent_segments+delete_segments+omit_endlist",
            )?;
            out_ctx.set_option("hls_list_size", &segments.to_string())?;
        }
        HlsWindow::Dvr => {
            // EVENT is the standard "everything retained, no ENDLIST until the
            // trailer" playlist — omit_endlist would suppress the ENDLIST the
            // graceful stop below exists to write.
            out_ctx.set_option("hls_playlist_type", "event")?;
            out_ctx.set_option("hls_flags", "independent_segments+append_list")?;
            out_ctx.set_option("hls_list_size", "0")?;
        }
        HlsWindow::Vod => {
            out_ctx.set_option("hls_flags", "independent_segments+append_list")?;
            out_ctx.set_option("hls_list_size", "0")?;
        }
    }
    out_ctx.set_option("hls_segment_options", "movflags=+frag_discont")?;

    // Set up video path
    let mut video_decoder = None;
    let mut video_encoder = None;
    let mut encoder_pixel_format = crate::types::PixelFormat::YUV420P;
    let video_copy = config.video_encoder.is_none();

    if let Some(v_idx) = video_idx {
        let stream = in_ctx.stream(v_idx).unwrap();
        if video_copy {
            let out_stream_idx = out_ctx.add_stream_copy(&stream)?;
            // HEVC in HLS/fMP4 requires hvc1 codec tag for browser MSE compatibility.
            // Without this, FFmpeg writes hev1 which Chrome/Safari reject.
            let codec_id = unsafe { (*stream.codec_parameters()).codec_id };
            if codec_id == ffi::AV_CODEC_ID_HEVC {
                unsafe {
                    let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
                    let out_stream = *streams.add(out_stream_idx as usize);
                    (*ffi::kff_stream_codecpar(out_stream)).codec_tag =
                        ffi::mktag(b'h', b'v', b'c', b'1');
                }
            }
        } else {
            let codec_name = config.video_encoder.as_deref().unwrap();
            // Transcode is a batch job — frame-threaded decode is a big win
            // and first-frame latency is irrelevant here.
            let dec = if let Some(ref hw) = config.hw_device {
                stream.decoder_hw_with_threads(hw, 0)?
            } else {
                stream.decoder_with_threads(0)?
            };

            // Zero-copy GPU path: when hw_device is set AND no subtitle
            // burn-in, keep frames on GPU (CUDA) throughout the pipeline.
            // Subtitle burn-in requires CPU-side filters, so fall back to
            // Phase 1 (hwdownload) in that case.
            let needs_subtitle = config.subtitle_burn_in.is_some();
            // Deinterlace is a CPU-only filter here; the deferred GPU path emits
            // scale_* and nothing else, so zero-copy has to be given up for it.
            let zero_copy = config.hw_device.is_some() && !needs_subtitle && deint_filter.is_none();

            // Auto-rotate (DISPLAYMATRIX): SW path only, matching CLI.
            let rotation = if config.hw_device.is_none() {
                unsafe {
                    crate::filter::graph::AutoRotate::from_codec_parameters(
                        stream.codec_parameters(),
                    )
                }
            } else {
                None
            };
            let rotation_swaps_dims = matches!(
                rotation,
                Some(
                    crate::filter::graph::AutoRotate::Clock
                        | crate::filter::graph::AutoRotate::ClockFlip
                        | crate::filter::graph::AutoRotate::Cclock
                        | crate::filter::graph::AutoRotate::CclockFlip
                )
            );
            let (src_post_rot_w, src_post_rot_h) = if rotation_swaps_dims {
                (dec.height(), dec.width())
            } else {
                (dec.width(), dec.height())
            };
            let width = config.scale_width.unwrap_or(src_post_rot_w);
            let height = config.scale_height.unwrap_or(src_post_rot_h);

            let pix_fmt = if config.hw_device.is_some() {
                crate::types::PixelFormat::NV12
            } else {
                // Always output 8-bit YUV420P on CPU path — browsers can't
                // decode 10-bit H.264 High 10 profile properly (banding in darks).
                crate::types::PixelFormat::YUV420P
            };
            encoder_pixel_format = pix_fmt;
            let enc_hw_frames = if zero_copy {
                let hw = config.hw_device.as_ref().unwrap();
                Some(hw.create_frames_ctx(width, height)?)
            } else {
                None
            };

            let enc = Encoder::video(VideoEncoderConfig {
                codec_name: codec_name.to_string(),
                width,
                height,
                pixel_format: pix_fmt,
                time_base: stream.time_base_native(),
                bit_rate: config.video_bitrate,
                gop_size: Some({
                    let fr = stream.r_frame_rate();
                    let fps = if fr.den > 0 {
                        fr.num as f64 / fr.den as f64
                    } else {
                        25.0
                    };
                    let out_fps = if deint_filter.is_some() {
                        fps * 2.0
                    } else {
                        fps
                    };
                    (config.segment_duration * out_fps) as i32
                }),
                global_header: out_ctx.requires_global_header(),
                quality: None,
                hw_frames_ctx: enc_hw_frames,
                sample_aspect_ratio: Some((src_sar.num, src_sar.den)),
                color: unsafe {
                    crate::filter::graph::VideoColorInfo::from_codec_parameters(
                        stream.codec_parameters(),
                    )
                },
                options: config.encoder_options(),
            })?;
            if let Some(mut hf) = enc_hw_frames {
                unsafe { ffi::av_buffer_unref(&mut hf) };
            }

            let out_stream_idx = out_ctx.add_stream_from_encoder(&enc)?;

            // HEVC output needs hvc1 tag for browser MSE compatibility
            let is_hevc = codec_name.contains("hevc") || codec_name.contains("x265");
            if is_hevc {
                unsafe {
                    let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
                    let out_stream = *streams.add(out_stream_idx as usize);
                    (*ffi::kff_stream_codecpar(out_stream)).codec_tag =
                        ffi::mktag(b'h', b'v', b'c', b'1');
                }
            }
            video_decoder = Some(dec);
            video_encoder = Some(enc);
        }
    }

    // Set up video filter graph: scale (for ABR variants) + subtitles (burn-in)
    //
    // Zero-copy GPU path: use scale_cuda for GPU-resident scaling (no download).
    // Phase 1 fallback: subtitle burn-in requires CPU filters, so hwdownload first.
    let needs_subtitle = config.subtitle_burn_in.is_some();
    let zero_copy = config.hw_device.is_some() && !needs_subtitle && deint_filter.is_none();
    let hw_type = config
        .hw_device
        .as_ref()
        .map(|d| d.device_type())
        .unwrap_or(crate::hw::device::HwAccelType::None);
    let mut video_filter: Option<FilterGraph> = None;
    // Deferred HW scale filter for lazy creation
    let mut deferred_hw_scale: Option<(i32, i32, i32, i32, ffi::AVRational)> = None;
    let mut cpu_filter_plan: Option<CpuFilterPlan> = None;
    if let Some(dec) = video_decoder.as_ref() {
        // Re-derive rotation here (HW path keeps it None — same rule as
        // encoder setup above). Filter chain dimensions are post-rotation.
        let rotation = if config.hw_device.is_none() {
            unsafe {
                crate::filter::graph::AutoRotate::from_codec_parameters(
                    in_ctx
                        .stream(video_idx.unwrap())
                        .unwrap()
                        .codec_parameters(),
                )
            }
        } else {
            None
        };
        let rotation_swaps_dims = matches!(
            rotation,
            Some(
                crate::filter::graph::AutoRotate::Clock
                    | crate::filter::graph::AutoRotate::ClockFlip
                    | crate::filter::graph::AutoRotate::Cclock
                    | crate::filter::graph::AutoRotate::CclockFlip
            )
        );
        let (src_post_rot_w, src_post_rot_h) = if rotation_swaps_dims {
            (dec.height(), dec.width())
        } else {
            (dec.width(), dec.height())
        };
        let target_w = config.scale_width.unwrap_or(src_post_rot_w);
        let target_h = config.scale_height.unwrap_or(src_post_rot_h);
        let needs_scale = target_w != src_post_rot_w || target_h != src_post_rot_h;

        if zero_copy {
            // Always defer a GPU filter in zero-copy mode: handles both
            // resolution scaling AND pixel format conversion (P010→NV12
            // for 10-bit sources). Without this, 10-bit HEVC decoded
            // frames go directly to the encoder whose hw_frames_ctx
            // expects NV12, causing NVENC registration failures.
            {
                deferred_hw_scale = Some((
                    dec.width(),
                    dec.height(),
                    target_w,
                    target_h,
                    in_ctx.stream(video_idx.unwrap()).unwrap().time_base(),
                ));
            }
        } else {
            // CPU filter path (Phase 1 or software)
            let mut filters: Vec<String> = Vec::new();
            // Auto-rotate first so subtitle overlay and scale operate on the
            // intended display orientation.
            if let Some(r) = rotation {
                filters.push(r.as_filter_chain());
            }
            if let Some(d) = deint_filter {
                filters.push(d.to_string());
            }
            if needs_scale {
                filters.push(format!("scale={target_w}:{target_h}"));
            }
            if let (Some(sub_idx), Some(path)) = (config.subtitle_burn_in, input_path) {
                let input_path_str = path.to_str().ok_or_else(|| {
                    crate::error::FfmpegError::new(
                        ffi::AVERROR_EINVAL,
                        "input path not valid UTF-8",
                    )
                })?;
                let escaped = input_path_str.replace('\\', "\\\\").replace('\'', "'\\''");
                let sub_stream_idx = in_ctx
                    .streams()
                    .filter(|s| s.codec_type() == MediaType::Subtitle)
                    .position(|s| s.index() == sub_idx as i32)
                    .unwrap_or(0);
                filters.push(format!("subtitles='{escaped}':si={sub_stream_idx}"));
            }
            // Always planned, even with an empty chain: the plan is built on the
            // first decoded frame, and it is the only thing that reconciles the
            // frame's real layout with the format the encoder was already opened
            // for. Skipping it sent the decoder's own frames straight to the
            // encoder on that guess.
            cpu_filter_plan = Some(CpuFilterPlan {
                chain: filters.join(","),
                time_base: in_ctx.stream(video_idx.unwrap()).unwrap().time_base(),
                color: unsafe {
                    crate::filter::graph::VideoColorInfo::from_codec_parameters(
                        in_ctx
                            .stream(video_idx.unwrap())
                            .unwrap()
                            .codec_parameters(),
                    )
                },
                target: encoder_pixel_format,
                sample_aspect_ratio: src_sar,
            });
        }
    }

    // Set up audio path — with frame size adaptation filter for transcode mode.
    // Decoded audio frames may have different nb_samples than the encoder expects
    // (e.g., MKV sources can have 1536-sample frames, but AAC encoder needs 1024).
    let mut audio_decoder = None;
    let mut audio_encoder = None;
    let mut audio_filter: Option<FilterGraph> = None;
    let mut audio_filter_state: Option<crate::audio::AudioGraphState> = None;
    let mut audio_filter_str: Option<String> = None;
    let mut audio_filter_tb = ffi::AVRational { num: 0, den: 1 };
    let audio_copy = config.audio_encoder.is_none();

    // Filled when codec params lack sample_rate and we pre-roll to discover it
    // (e.g. LATM-AAC over MPEG-TS). Drained before the main read loop.
    let mut buffered_audio_frames: VecDeque<Frame> = VecDeque::new();
    let mut buffered_packets: VecDeque<Packet> = VecDeque::new();

    if let Some(a_idx) = audio_idx {
        if audio_copy {
            let stream = in_ctx.stream(a_idx).unwrap();
            out_ctx.add_stream_copy(&stream)?;
        } else {
            let codec_name = config.audio_encoder.as_deref().unwrap();
            let (mut dec, stream_tb) = {
                let stream = in_ctx.stream(a_idx).unwrap();
                (stream.decoder()?, stream.time_base())
            };
            let effective_sample_rate = crate::audio::probe_audio_sample_rate(
                &mut in_ctx,
                a_idx,
                &mut dec,
                &mut buffered_packets,
                &mut buffered_audio_frames,
            )?;

            // Encoder uses stereo output (browser playback standard).
            // The audio filter downmixes from source layout to stereo.
            // Sample format is encoder-specific (aac→fltp, libfdk_aac→s16,
            // libmp3lame→s16p). Query the codec's preferred format so encoder
            // open + filter graph agree — mismatch is EINVAL on avcodec_open2.
            let sample_format = crate::codec::encoder::default_audio_sample_format(codec_name)?;
            let enc = Encoder::audio(AudioEncoderConfig {
                codec_name: codec_name.to_string(),
                sample_rate: effective_sample_rate,
                channel_layout: crate::types::ChannelLayout::stereo(),
                sample_format,
                bit_rate: config.audio_bitrate,
                time_base: crate::types::TimeBase::new(1, effective_sample_rate),
                global_header: out_ctx.requires_global_header(),
            })?;

            // Audio filter is created lazily from the first decoded frame's actual
            // properties, not from the decoder context. AC3 streams commonly declare
            // 5.1 in the container while initial decoded frames are stereo.
            audio_filter_str = Some(format!(
                "aformat=sample_fmts={}:sample_rates={}:channel_layouts=stereo,asetnsamples=n={}:p=0",
                sample_format.name(),
                effective_sample_rate,
                enc.frame_size().max(1024),
            ));
            audio_filter_tb = stream_tb;

            out_ctx.add_stream_from_encoder(&enc)?;
            audio_decoder = Some(dec);
            audio_encoder = Some(enc);
        }
    }

    // Set genpts flag for video copy mode — generates missing PTS from DTS,
    // critical for MKV files with missing/broken timestamps.
    // GENPTS, DISCARD_CORRUPT, SORT_DTS are set globally by InputContext::configure_lenient_demuxing

    out_ctx.write_header()?;

    // Get post-header stream timebases — muxer may override them.
    // Video encoded packets must be rescaled from encoder timebase to this.
    let video_out_tb = if video_idx.is_some() {
        unsafe {
            let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
            ffi::kff_stream_time_base(*streams.add(0))
        }
    } else {
        ffi::AVRational { num: 1, den: 1000 }
    };
    let video_enc_tb = video_encoder
        .as_ref()
        .map(|e| e.time_base())
        .unwrap_or(ffi::AVRational { num: 1, den: 1000 });
    let audio_out_stream_idx = if video_idx.is_some() { 1 } else { 0 };
    let audio_writer = audio_encoder.as_ref().map(|e| {
        let out_tb = unsafe {
            let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
            ffi::kff_stream_time_base(*streams.add(audio_out_stream_idx as usize))
        };
        crate::audio::EncodedAudioWriter::new(audio_out_stream_idx, e.time_base(), out_tb)
    });

    // Processing loop
    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut sw_frame = Frame::new()?;
    let mut filtered_frame = Frame::new()?;
    let mut out_packet = Packet::new()?;
    let mut video_packets_written = 0u32;

    // Drain audio frames captured during sample-rate pre-roll. These frames
    // were already decoded; we feed them straight into filter+encoder.
    if let (Some(enc), Some(aw)) = (audio_encoder.as_mut(), audio_writer.as_ref()) {
        while let Some(mut buf_frame) = buffered_audio_frames.pop_front() {
            if let Some(ref fs) = audio_filter_str
                && crate::audio::AudioGraphState::needs_rebuild(&audio_filter_state, &buf_frame)
            {
                if let Some(ref mut af) = audio_filter {
                    af.push_eof()?;
                    aw.encode_filtered(
                        enc,
                        af,
                        &mut filtered_frame,
                        &mut out_ctx,
                        &mut out_packet,
                    )?;
                }
                crate::audio::AudioGraphState::rebuild_filter_graph(
                    &mut audio_filter,
                    &mut audio_filter_state,
                    &buf_frame,
                    audio_filter_tb,
                    fs,
                )?;
            }
            if let Some(ref mut af) = audio_filter {
                af.push_frame(&mut buf_frame)?;
                aw.encode_filtered(enc, af, &mut filtered_frame, &mut out_ctx, &mut out_packet)?;
            } else {
                aw.encode(
                    enc,
                    &mut buf_frame,
                    audio_filter_tb,
                    &mut out_ctx,
                    &mut out_packet,
                )?;
            }
        }
    }

    loop {
        let have_packet = if let Some(buffered) = buffered_packets.pop_front() {
            packet = buffered;
            true
        } else {
            in_ctx.read_packet(&mut packet)?
        };
        if !have_packet {
            break;
        }
        // A retained playlist has to be finished: break out so the encoders flush
        // and write_trailer() emits ENDLIST. A sliding live window has no ENDLIST
        // to write, so it keeps the cheaper abrupt exit.
        if let Err(e) = crate::progress::check_cancel(config.run.as_ref()) {
            if matches!(window, HlsWindow::Sliding { .. }) {
                return Err(e);
            }
            break;
        }

        let pkt_stream = packet.stream_index();

        // Video
        if let Some(v_idx) = video_idx {
            if pkt_stream == v_idx as i32 {
                if video_copy {
                    let in_tb = in_ctx.stream(v_idx).unwrap().time_base();
                    out_ctx.write_packet_rescaled(&mut packet, 0, in_tb)?;
                } else if let (Some(dec), Some(enc)) = (&mut video_decoder, &mut video_encoder) {
                    dec.send_packet(&packet)?;
                    // Lazy scale_cuda creation after first packet
                    if let Some((src_w, src_h, dst_w, dst_h, tb)) = deferred_hw_scale.take() {
                        if let Some(dec_hw_frames) = dec.hw_frames_ctx() {
                            let hw_pix_fmt = match hw_type {
                                crate::hw::device::HwAccelType::Cuda => ffi::AV_PIX_FMT_CUDA,
                                crate::hw::device::HwAccelType::Vaapi => ffi::AV_PIX_FMT_VAAPI,
                                _ => ffi::AV_PIX_FMT_NV12,
                            };
                            let params = VideoFilterParams {
                                width: src_w,
                                height: src_h,
                                pixel_format: hw_pix_fmt,
                                time_base: tb,
                                sample_aspect_ratio: src_sar,
                                hw_frames_ctx: Some(dec_hw_frames),
                                color: unsafe {
                                    crate::filter::graph::VideoColorInfo::from_codec_parameters(
                                        in_ctx.stream(v_idx).unwrap().codec_parameters(),
                                    )
                                },
                                side_data_frame: std::ptr::null(),
                            };
                            let filter_str = match hw_type {
                                crate::hw::device::HwAccelType::Cuda => {
                                    format!("scale_cuda={dst_w}:{dst_h}:format=nv12")
                                }
                                crate::hw::device::HwAccelType::Vaapi => {
                                    format!("scale_vaapi={dst_w}:{dst_h}:format=nv12")
                                }
                                _ => format!("scale={dst_w}:{dst_h}"),
                            };
                            video_filter = Some(FilterGraph::new_video(&params, &filter_str)?);
                            unsafe {
                                let mut p = dec_hw_frames;
                                ffi::av_buffer_unref(&mut p);
                            }
                        } else {
                            deferred_hw_scale = Some((src_w, src_h, dst_w, dst_h, tb));
                        }
                    }
                    loop {
                        match dec.receive_frame(&mut frame)? {
                            DecodeResult::Frame => {
                                // Zero-copy: CUDA frames pass directly to
                                // filter/encoder. Phase 1: download to CPU first.
                                let src = if !zero_copy && frame.is_hw_format() {
                                    crate::hw::frames::HwFramesContext::download(
                                        &frame,
                                        &mut sw_frame,
                                    )?;
                                    sw_frame.set_pts(frame.pts());
                                    &mut sw_frame
                                } else {
                                    &mut frame
                                };

                                if video_filter.is_none() {
                                    if let Some(plan) = cpu_filter_plan.as_ref() {
                                        video_filter = Some(plan.build(src)?);
                                    }
                                }
                                if let Some(ref mut filter) = video_filter {
                                    filter.push_frame(src)?;
                                    let filter_tb = filter.output_time_base();
                                    let enc_tb = enc.time_base();
                                    let mut filtered = Frame::new()?;
                                    while filter.pull_frame(&mut filtered)? {
                                        unsafe {
                                            let ptr = filtered.as_mut_ptr();
                                            (*ptr).pts =
                                                ffi::av_rescale_q((*ptr).pts, filter_tb, enc_tb);
                                        }
                                        enc.send_frame(&filtered)?;
                                        while let EncodeResult::Packet =
                                            enc.receive_packet(&mut out_packet)?
                                        {
                                            unsafe {
                                                (*out_packet.as_mut_ptr()).stream_index = 0;
                                                ffi::av_packet_rescale_ts(
                                                    out_packet.as_mut_ptr(),
                                                    video_enc_tb,
                                                    video_out_tb,
                                                );
                                            }
                                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                                            video_packets_written += 1;
                                        }
                                        filtered.unref();
                                    }
                                } else {
                                    enc.send_frame(src)?;
                                    while let EncodeResult::Packet =
                                        enc.receive_packet(&mut out_packet)?
                                    {
                                        unsafe {
                                            (*out_packet.as_mut_ptr()).stream_index = 0;
                                            ffi::av_packet_rescale_ts(
                                                out_packet.as_mut_ptr(),
                                                video_enc_tb,
                                                video_out_tb,
                                            );
                                        }
                                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                                        video_packets_written += 1;
                                    }
                                }
                                sw_frame.unref();
                            }
                            DecodeResult::NeedsInput => break,
                            DecodeResult::Eof => break,
                        }
                    }
                }
            }
        }

        // Audio
        if let Some(a_idx) = audio_idx {
            if pkt_stream == a_idx as i32 {
                let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
                if audio_copy {
                    let in_tb = in_ctx.stream(a_idx).unwrap().time_base();
                    out_ctx.write_packet_rescaled(&mut packet, audio_out_idx, in_tb)?;
                } else if let (Some(dec), Some(enc), Some(aw)) = (
                    &mut audio_decoder,
                    &mut audio_encoder,
                    audio_writer.as_ref(),
                ) {
                    dec.send_packet(&packet)?;
                    loop {
                        match dec.receive_frame(&mut frame)? {
                            DecodeResult::Frame => {
                                // Rebuild audio filter if frame properties changed (AC3 layout mismatch)
                                if let Some(ref fs) = audio_filter_str {
                                    if crate::audio::AudioGraphState::needs_rebuild(
                                        &audio_filter_state,
                                        &frame,
                                    ) {
                                        if let Some(ref mut af) = audio_filter {
                                            af.push_eof()?;
                                            aw.encode_filtered(
                                                enc,
                                                af,
                                                &mut filtered_frame,
                                                &mut out_ctx,
                                                &mut out_packet,
                                            )?;
                                        }
                                        crate::audio::AudioGraphState::rebuild_filter_graph(
                                            &mut audio_filter,
                                            &mut audio_filter_state,
                                            &frame,
                                            audio_filter_tb,
                                            fs,
                                        )?;
                                    }
                                }
                                // Push through audio filter for frame size adaptation
                                if let Some(ref mut af) = audio_filter {
                                    af.push_frame(&mut frame)?;
                                    aw.encode_filtered(
                                        enc,
                                        af,
                                        &mut filtered_frame,
                                        &mut out_ctx,
                                        &mut out_packet,
                                    )?;
                                } else {
                                    aw.encode(
                                        enc,
                                        &mut frame,
                                        audio_filter_tb,
                                        &mut out_ctx,
                                        &mut out_packet,
                                    )?;
                                }
                            }
                            DecodeResult::NeedsInput => break,
                            DecodeResult::Eof => break,
                        }
                    }
                }
            }
        }
    }

    // Flush decoders, filter, and encoders
    if let Some(dec) = &mut video_decoder {
        dec.send_eof()?;
        if let Some(enc) = &mut video_encoder {
            // Drain remaining decoder frames through filter
            loop {
                match dec.receive_frame(&mut frame)? {
                    DecodeResult::Frame => {
                        let src = if !zero_copy && frame.is_hw_format() {
                            crate::hw::frames::HwFramesContext::download(&frame, &mut sw_frame)?;
                            sw_frame.set_pts(frame.pts());
                            &mut sw_frame
                        } else {
                            &mut frame
                        };

                        if video_filter.is_none() {
                            if let Some(plan) = cpu_filter_plan.as_ref() {
                                video_filter = Some(plan.build(src)?);
                            }
                        }
                        if let Some(ref mut filter) = video_filter {
                            filter.push_frame(src)?;
                            let filter_tb = filter.output_time_base();
                            let mut filtered = Frame::new()?;
                            while filter.pull_frame(&mut filtered)? {
                                unsafe {
                                    let ptr = filtered.as_mut_ptr();
                                    (*ptr).pts =
                                        ffi::av_rescale_q((*ptr).pts, filter_tb, video_enc_tb);
                                }
                                enc.send_frame(&filtered)?;
                                while let EncodeResult::Packet =
                                    enc.receive_packet(&mut out_packet)?
                                {
                                    unsafe {
                                        (*out_packet.as_mut_ptr()).stream_index = 0;
                                        ffi::av_packet_rescale_ts(
                                            out_packet.as_mut_ptr(),
                                            video_enc_tb,
                                            video_out_tb,
                                        );
                                    }
                                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                                }
                                filtered.unref();
                            }
                        } else {
                            enc.send_frame(src)?;
                            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                                unsafe {
                                    (*out_packet.as_mut_ptr()).stream_index = 0;
                                    ffi::av_packet_rescale_ts(
                                        out_packet.as_mut_ptr(),
                                        video_enc_tb,
                                        video_out_tb,
                                    );
                                }
                                out_ctx.write_packet_interleaved(&mut out_packet)?;
                            }
                        }
                        sw_frame.unref();
                    }
                    _ => break,
                }
            }
            // Flush filter graph
            if let Some(ref mut filter) = video_filter {
                filter.push_eof()?;
                let filter_tb = filter.output_time_base();
                let mut filtered = Frame::new()?;
                while filter.pull_frame(&mut filtered)? {
                    unsafe {
                        let ptr = filtered.as_mut_ptr();
                        (*ptr).pts = ffi::av_rescale_q((*ptr).pts, filter_tb, video_enc_tb);
                    }
                    enc.send_frame(&filtered)?;
                    while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                        unsafe {
                            (*out_packet.as_mut_ptr()).stream_index = 0;
                            ffi::av_packet_rescale_ts(
                                out_packet.as_mut_ptr(),
                                video_enc_tb,
                                video_out_tb,
                            );
                        }
                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                    }
                    filtered.unref();
                }
            }
            enc.send_eof()?;
            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                unsafe {
                    (*out_packet.as_mut_ptr()).stream_index = 0;
                    ffi::av_packet_rescale_ts(out_packet.as_mut_ptr(), video_enc_tb, video_out_tb);
                }
                out_ctx.write_packet_interleaved(&mut out_packet)?;
            }
        }
    }

    if let Some(dec) = &mut audio_decoder {
        dec.send_eof()?;
        if let (Some(enc), Some(aw)) = (audio_encoder.as_mut(), audio_writer.as_ref()) {
            // Drain decoder through audio filter
            loop {
                match dec.receive_frame(&mut frame)? {
                    DecodeResult::Frame => {
                        // Rebuild audio filter if frame properties changed
                        if let Some(ref fs) = audio_filter_str {
                            if crate::audio::AudioGraphState::needs_rebuild(
                                &audio_filter_state,
                                &frame,
                            ) {
                                if let Some(ref mut af) = audio_filter {
                                    af.push_eof()?;
                                    aw.encode_filtered(
                                        enc,
                                        af,
                                        &mut filtered_frame,
                                        &mut out_ctx,
                                        &mut out_packet,
                                    )?;
                                }
                                crate::audio::AudioGraphState::rebuild_filter_graph(
                                    &mut audio_filter,
                                    &mut audio_filter_state,
                                    &frame,
                                    audio_filter_tb,
                                    fs,
                                )?;
                            }
                        }
                        if let Some(ref mut af) = audio_filter {
                            af.push_frame(&mut frame)?;
                            aw.encode_filtered(
                                enc,
                                af,
                                &mut filtered_frame,
                                &mut out_ctx,
                                &mut out_packet,
                            )?;
                        } else {
                            aw.encode(
                                enc,
                                &mut frame,
                                audio_filter_tb,
                                &mut out_ctx,
                                &mut out_packet,
                            )?;
                        }
                    }
                    _ => break,
                }
            }
            // Flush audio filter
            if let Some(ref mut af) = audio_filter {
                af.push_eof()?;
                aw.encode_filtered(enc, af, &mut filtered_frame, &mut out_ctx, &mut out_packet)?;
            }
            enc.send_eof()?;
            aw.drain(enc, &mut out_ctx, &mut out_packet)?;
        }
    }

    out_ctx.write_trailer()?;

    Ok(HlsResult {
        playlist_path,
        segment_dir: config.output_dir.clone(),
        video_packets_written,
    })
}

#[cfg(test)]
mod encoder_options_test {
    use super::*;

    #[test]
    fn default_config_passes_no_encoder_options() {
        let config = HlsConfig::new("/tmp/unused");
        assert_eq!(config.preset, None);
        assert_eq!(config.tune, None);
        assert!(
            config.encoder_options().is_empty(),
            "an empty option list is what keeps existing callers byte-identical"
        );
    }

    #[test]
    fn preset_and_tune_become_private_options() {
        let config = HlsConfig::new("/tmp/unused")
            .preset("veryfast")
            .tune("zerolatency");
        assert_eq!(
            config.encoder_options(),
            vec![
                ("preset".to_string(), "veryfast".to_string()),
                ("tune".to_string(), "zerolatency".to_string()),
            ]
        );
    }

    #[test]
    fn preset_alone_does_not_imply_a_tune() {
        let config = HlsConfig::new("/tmp/unused").preset("veryfast");
        assert_eq!(
            config.encoder_options(),
            vec![("preset".to_string(), "veryfast".to_string())]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::graph::VideoColorInfo;

    fn grey_frame(fmt: ffi::AVPixelFormat, w: i32, h: i32) -> Frame {
        let mut f = Frame::new().unwrap();
        unsafe {
            let p = f.as_mut_ptr();
            (*p).format = fmt;
            (*p).width = w;
            (*p).height = h;
            assert_eq!(ffi::av_frame_get_buffer(p, 0), 0);
            for plane in 0..3 {
                let d = (*p).data[plane];
                if !d.is_null() {
                    let ls = (*p).linesize[plane] as usize;
                    let rows = if plane == 0 {
                        h as usize
                    } else {
                        (h / 2) as usize
                    };
                    std::ptr::write_bytes(d, 128u8, ls * rows);
                }
            }
            (*p).pts = 0;
        }
        f
    }

    fn plan(target: crate::types::PixelFormat) -> CpuFilterPlan {
        CpuFilterPlan {
            chain: "scale=1024:576".to_string(),
            time_base: ffi::AVRational { num: 1, den: 25 },
            color: VideoColorInfo::default(),
            target,
            sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
        }
    }

    /// DVB SD is 720x576 at 64:45 — 16:9 on screen, and the decoder sets that on
    /// every frame. The graph must carry it through: a chain declared square
    /// makes `scale` rewrite the SAR to "correct" an aspect that was never wrong,
    /// and the picture reaches the browser squashed to 5:4.
    ///
    /// The encoder half is separate and cannot be seen from here — `avctx
    /// ->sample_aspect_ratio` is what the muxer turns into the `pasp` box, and
    /// leaving it unset is what produced 5:4 output no matter what the frames
    /// said. That is set from the source stream in `transcode_to_hls_inner`.
    #[test]
    fn a_non_square_source_keeps_its_display_aspect_through_the_graph() {
        // bwdif is deliberately absent: it needs two frames before it emits one,
        // and the scales are the filters that actually rewrite SAR. 600x480 is
        // `transcode_dimensions` at a 480 cap — proportional, so the pixel aspect
        // is unchanged, which is why handing the encoder the *source* SAR is right.
        for chain in ["", "scale=720:576", "scale=600:480"] {
            let mut p = plan(crate::types::PixelFormat::YUV420P);
            p.chain = chain.to_string();
            p.sample_aspect_ratio = ffi::AVRational { num: 64, den: 45 };

            let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 720, 576);
            unsafe { (*f.as_mut_ptr()).sample_aspect_ratio = ffi::AVRational { num: 64, den: 45 } };

            let mut g = p.build(&f).unwrap();
            g.push_frame(&mut f).unwrap();
            let mut out = Frame::new().unwrap();
            assert!(g.pull_frame(&mut out).unwrap(), "chain {chain:?}");
            let sar = unsafe { (*out.as_ptr()).sample_aspect_ratio };
            assert_eq!(
                (sar.num, sar.den),
                (64, 45),
                "chain {chain:?} flattened the pixel aspect to square"
            );
        }
    }

    fn filter_once(graph: &mut FilterGraph, frame: &mut Frame) -> ffi::AVPixelFormat {
        graph.push_frame(frame).unwrap();
        let mut out = Frame::new().unwrap();
        assert!(
            graph.pull_frame(&mut out).unwrap(),
            "graph produced no frame"
        );
        out.format()
    }

    /// Attaching a hw device does not mean the decoder took it: a codec the GPU
    /// cannot decode falls back to software and emits planar YUV. The plan has to
    /// read the frame, not the config, or the graph is built for the wrong layout.
    /// It also has to *end* on the encoder's format, whichever way the guess went:
    /// the encoder is already open by the time the first frame exists, so the
    /// graph is the only place the two can be reconciled.
    #[test]
    fn the_graph_reads_the_frames_format_and_ends_on_the_encoders() {
        for target in [
            crate::types::PixelFormat::YUV420P,
            crate::types::PixelFormat::NV12,
        ] {
            for source in [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_NV12] {
                let mut f = grey_frame(source, 704, 576);
                let mut g = plan(target).build(&f).unwrap();
                assert_eq!(
                    filter_once(&mut g, &mut f),
                    target.raw(),
                    "source {source} was not converted to the encoder's {}",
                    target.name()
                );
            }
        }
    }

    /// A 10-bit source still lands on the encoder's 8-bit format — browsers band
    /// badly on High 10, and the encoder was never opened for it.
    #[test]
    fn a_ten_bit_source_is_brought_down_to_the_encoders_format() {
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P10LE, 704, 576);
        let mut g = plan(crate::types::PixelFormat::YUV420P).build(&f).unwrap();
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_YUV420P);
    }

    /// The chain is empty whenever there is nothing to deinterlace, rotate, scale
    /// or burn in — which is exactly when the frames used to reach the encoder
    /// unconverted. The format filter has to stand on its own there.
    #[test]
    fn an_otherwise_empty_chain_still_converts() {
        let mut plan = plan(crate::types::PixelFormat::NV12);
        plan.chain = String::new();
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 704, 576);
        let mut g = plan.build(&f).unwrap();
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_NV12);
    }

    /// Why the above matters: libavfilter does not reject a source whose declared
    /// format is wrong. It logs and reinterprets, so YUV420P pushed into a graph
    /// told to expect NV12 comes back out as NV12 — the two chroma planes read as
    /// one interleaved plane. Luma is untouched, so it reaches the screen as a
    /// correct greyscale picture under green and magenta chroma, not as an error.
    #[test]
    fn a_wrongly_declared_source_format_is_reinterpreted_not_rejected() {
        let params = VideoFilterParams {
            width: 704,
            height: 576,
            pixel_format: ffi::AV_PIX_FMT_NV12,
            time_base: ffi::AVRational { num: 1, den: 25 },
            sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
            hw_frames_ctx: None,
            color: VideoColorInfo::default(),
            side_data_frame: std::ptr::null(),
        };
        let mut g = FilterGraph::new_video(&params, "scale=1024:576").unwrap();
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 704, 576);
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_NV12);
    }
}
