//! Single-pass multi-artefact extraction over one decode of a media file.
//!
//! Opens the file once, runs one decode loop, and fans each decoded video or
//! audio frame out to N independent per-branch sinks. Each branch is optional
//! and configured via [`SinglePassRequest`]; per-branch failures are isolated
//! into [`SinglePassResult::per_branch_errors`] without bailing the whole call.
//!
//! Available branches:
//! - `trickplay`: small JPEG frames at low fps (320w by default), for sprite tiling
//! - `vhash`: 16x16 grayscale descriptors at high fps, for visual hashing
//! - `embed_src`: wide JPEGs at moderate fps, returned in memory for SigLIP embedding
//! - `chromaprint`: full-file 8 kHz mono fingerprint via libchromaprint
//! - `silence`: silencedetect intervals
//! - `loudness`: integrated LUFS via ebur128
//! - `audio_peaks`: 1 kHz mono PCM bucketed into (min_i8, max_i8) pairs
//! - `corruption`: time ranges where the SW decoder hit error-concealment / missing
//!   references / invalid bitstream — used to drive per-segment transcode-on-demand
//!   so browser HW decoders don't trip over mid-stream corruption
//!
//! `vhash` and `embed_src` both support optional `windows` on their params —
//! a list of `[start_secs, end_secs)` ranges the sink will emit frames for.
//! Empty windows = whole file. Out-of-window frames skip the per-frame
//! scale + JPEG/descriptor work. When every requested branch is windowed the
//! run seeks to just ahead of the earliest window; otherwise the decoder runs
//! over the whole file because the other branches need it.
//!
//! CPU decode only — concurrent CUDA contexts race in the upstream sites.
//! Each branch keeps its own resampler / scaler / filter graph, so a corrupt
//! audio stream cannot break video branches and vice versa.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::audio::AudioGraphState;
use crate::codec::decoder::DecodeResult;
use crate::codec::encoder::{EncodeResult, Encoder, VideoEncoderConfig};
pub use crate::filter::detect::BlackFramesParams;
use crate::filter::detect::{
    BlackInterval, SilenceInterval, parse_black_metadata, parse_silence_metadata,
};
use crate::filter::graph::{FilterGraph, VideoFilterParams};
use crate::format::input::{InputContext, MediaType};
use crate::fps_grid::TargetFpsGrid;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::progress::RunContext;
use crate::scale::Scaler;
use crate::thumbnails::JpegFrame;
use crate::types::{ChannelLayout, PixelFormat, SampleFormat, TimeBase};
use crate::{FfmpegError, Result, ffi};

// ── Public types ────────────────────────────────────────────────────────────

/// Parameters for the trickplay (low-fps small JPEG) branch.
#[derive(Debug, Clone)]
pub struct TrickplayParams {
    /// Output JPEG width in pixels. Height preserves source aspect (rounded to even).
    pub width: i32,
    /// Frames per second to grab (e.g. `0.2` = one frame every 5 s).
    pub fps: f64,
    /// JPEG quality (1 = best, 31 = worst). Same scale as mjpeg encoder.
    pub jpeg_quality: i32,
}

/// Parameters for the vhash (16x16 grayscale descriptor) branch.
#[derive(Debug, Clone)]
pub struct VhashParams {
    /// Intermediate scale width (typically 64) before final 16x16 reduce.
    pub intermediate_width: i32,
    /// Frames per second to grab (typically 3.0).
    pub fps: f64,
    /// If non-empty, only produce descriptors whose timestamp falls inside one
    /// of these `[start_secs, end_secs)` windows. Empty = whole file.
    /// The decoder still runs over the whole file (other branches need it),
    /// but out-of-window frames skip the scale + descriptor work.
    pub windows: Vec<(f64, f64)>,
}

/// Parameters for the embed_src (wide JPEG) branch.
/// Produced JPEGs are returned in memory — no disk writes. The caller feeds
/// them straight to a vision service in `media_analyze`.
#[derive(Debug, Clone)]
pub struct EmbedSrcParams {
    /// Output JPEG width in pixels. Height preserves source aspect (rounded to even).
    pub width: i32,
    /// Frames per second to grab (typically 1.0).
    pub fps: f64,
    /// JPEG quality (1..=31).
    pub jpeg_quality: i32,
    /// If non-empty, only produce JPEGs whose timestamp falls inside one of
    /// these `[start_secs, end_secs)` windows. Empty = whole file.
    pub windows: Vec<(f64, f64)>,
}

/// Parameters for the chromaprint branch (full-file fingerprint).
#[derive(Debug, Clone, Default)]
pub struct ChromaprintParams {
    /// Optional offset to start fingerprinting from (seconds). `None` means start of file.
    pub offset_secs: Option<f64>,
    /// Optional max length to fingerprint (seconds). `None` means whole file.
    pub length_secs: Option<f64>,
}

/// Parameters for the silence detection branch.
#[derive(Debug, Clone)]
pub struct SilenceParams {
    pub noise_db: f64,
    pub min_duration_secs: f64,
}

impl Default for SilenceParams {
    fn default() -> Self {
        Self {
            noise_db: -30.0,
            min_duration_secs: 0.5,
        }
    }
}

/// Parameters for the audio_peaks (waveform bucketing) branch.
#[derive(Debug, Clone)]
pub struct AudioPeaksParams {
    /// Output sample rate (typically 1000) — one (min,max) pair per 1/sample_rate seconds.
    pub sample_rate: i32,
}

impl Default for AudioPeaksParams {
    fn default() -> Self {
        Self { sample_rate: 1000 }
    }
}

/// Parameters for the corruption-detection branch.
///
/// Reads `frame.flags & AV_FRAME_FLAG_CORRUPT` and `frame.decode_error_flags`
/// on every decoded video frame and coalesces consecutive corrupt frames into
/// `[start, end]` ranges. Cost is two field reads per frame — the decoder
/// already runs the whole file for the other video branches.
#[derive(Debug, Clone)]
pub struct CorruptionParams {
    /// Maximum gap (seconds) between consecutive corrupt frames before they
    /// are split into separate ranges. A single concealed GOP usually presents
    /// as 30–120 contiguous corrupt frames; setting this to ~0.25s keeps that
    /// as one range while still splitting unrelated corruption events.
    pub merge_gap_secs: f64,
}

impl Default for CorruptionParams {
    fn default() -> Self {
        Self {
            merge_gap_secs: 0.25,
        }
    }
}

/// One contiguous run of corrupt video frames.
///
/// `start_secs..=end_secs` is inclusive (both ends are timestamps of an
/// observed corrupt frame). For a single-frame corruption run, `start == end`.
#[derive(Debug, Clone, PartialEq)]
pub struct CorruptRange {
    pub start_secs: f64,
    pub end_secs: f64,
    pub frame_count: u32,
    /// Decoder fell back to motion-vector concealment for at least one frame.
    pub concealment: bool,
    /// Slice header referenced a PPS/SPS/ref-frame that wasn't present
    /// (the Paradise S2E6 PPS-23 case).
    pub missing_reference: bool,
    /// Bitstream itself is malformed.
    pub invalid_bitstream: bool,
    /// At least one slice failed to decode.
    pub decode_slices_failed: bool,
    /// `AV_FRAME_FLAG_CORRUPT` fired without a specific `decode_error_flags` bit.
    pub other: bool,
}

/// One vhash descriptor: a 16x16 grayscale frame at a given timestamp.
#[derive(Debug, Clone)]
pub struct VhashDescriptor {
    pub timestamp_secs: f64,
    /// Grayscale samples, length = width*height (typically 256 = 16*16).
    pub data: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

/// Result of the chromaprint branch.
#[derive(Debug, Clone)]
pub struct ChromaprintResult {
    pub raw: Vec<u32>,
    pub duration_secs: f64,
}

/// Result of the audio_peaks branch.
#[derive(Debug, Clone)]
pub struct AudioPeaksResult {
    /// Wire-format bytes: [version: u8 = 1, peaks_per_second: u8, (min_i8, max_i8) pairs ...]
    pub bytes: Vec<u8>,
    pub sample_rate: i32,
}

/// Result of the embed_src branch: a list of JPEGs held in memory so the
/// caller can send them straight to a vision service without a disk hop.
pub type EmbedSrcResult = Vec<JpegFrame>;

/// Custom streaming sink for the trickplay branch.
///
/// When supplied via [`CustomSinks::trickplay`] to [`run_single_pass_with_sinks`],
/// every trickplay JPEG is dispatched to [`Self::on_frame`] in capture order as
/// soon as it is encoded — frames are *not* accumulated in
/// [`SinglePassResult::trickplay`], which stays `None` for that run.
///
/// Use this for progressive on-disk emission (per-chunk sprite sheets, partial
/// VTT writes) so the player sees thumbnails before the full decode finishes
/// and so peak memory drops from "all JPEGs" to "one outgoing frame".
///
/// `on_frame` runs on whichever rayon worker is feeding the trickplay branch;
/// keep per-frame work cheap (a tile + async-write hand-off is fine, but a full
/// sprite encode here will stall the vhash + embed_src branches that share the
/// scope).
pub trait TrickplaySink: Send {
    /// Called once per encoded trickplay frame, in monotonically increasing
    /// `frame_idx`. Returning an error fails the trickplay branch and pushes
    /// the failure into [`SinglePassResult::per_branch_errors`] without
    /// affecting other branches.
    fn on_frame(&mut self, frame_idx: usize, time_secs: f64, jpeg: JpegFrame) -> Result<()>;

    /// Called once at the end of the decode after the last `on_frame`. Default
    /// is a no-op. Use this to flush any partial state (e.g. a trailing sprite
    /// chunk + final VTT rewrite).
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Default trickplay sink — buffers every frame in a `Vec<JpegFrame>`.
///
/// This is the behaviour used when no custom sink is supplied; it preserves
/// [`SinglePassResult::trickplay`] population for callers that want the full
/// in-memory list.
#[derive(Default)]
pub struct BufferingTrickplaySink {
    pub frames: Vec<JpegFrame>,
}

impl BufferingTrickplaySink {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TrickplaySink for BufferingTrickplaySink {
    fn on_frame(&mut self, _idx: usize, _time_secs: f64, jpeg: JpegFrame) -> Result<()> {
        self.frames.push(jpeg);
        Ok(())
    }
}

/// Optional per-branch streaming sinks for [`run_single_pass_with_sinks`].
/// Branches without a custom sink fall back to the in-memory results carried
/// by [`SinglePassResult`].
#[derive(Default)]
pub struct CustomSinks {
    pub trickplay: Option<Box<dyn TrickplaySink>>,
}

/// Identifier for a single sink. Used for per-branch error reporting and
/// per-branch wall-clock attribution in [`SinglePassResult::branch_durations`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchId {
    Trickplay,
    Vhash,
    EmbedSrc,
    Chromaprint,
    Silence,
    Loudness,
    AudioPeaks,
    BlackFrames,
    Corruption,
}

/// Single-pass extraction request. All branch fields are optional —
/// `None` omits that branch from the run entirely.
#[derive(Debug, Clone)]
pub struct SinglePassRequest {
    pub input_path: PathBuf,
    pub trickplay: Option<TrickplayParams>,
    pub vhash: Option<VhashParams>,
    pub embed_src: Option<EmbedSrcParams>,
    pub chromaprint: Option<ChromaprintParams>,
    pub silence: Option<SilenceParams>,
    pub loudness: bool,
    pub audio_peaks: Option<AudioPeaksParams>,
    pub black_frames: Option<BlackFramesParams>,
    pub corruption: Option<CorruptionParams>,
    /// `true` skips the loop filter on the video decoder for ~20–30% faster decode
    /// at negligible quality cost (matches `extract_frames_sequential_with_hw(fast_decode=true)`).
    /// Recommended `true` for all analysis-only runs.
    pub fast_decode: bool,
}

impl SinglePassRequest {
    pub fn new(input_path: impl Into<PathBuf>) -> Self {
        Self {
            input_path: input_path.into(),
            trickplay: None,
            vhash: None,
            embed_src: None,
            chromaprint: None,
            silence: None,
            loudness: false,
            audio_peaks: None,
            black_frames: None,
            corruption: None,
            fast_decode: true,
        }
    }
}

/// Result of a single-pass extraction. Each branch field is `Some` iff
/// (a) the branch was requested in [`SinglePassRequest`] AND (b) it didn't
/// fail mid-run. A failure is recorded in `per_branch_errors`.
///
/// `branch_durations` records cumulative wall time per branch — sum of every
/// `feed()` call across all frames plus the final `finalize()`. Useful for
/// "where did the 270s go" profiling. Branches that were never requested
/// (no slot allocated) won't appear in the map. The combined silence+LUFS
/// `audio_meta` sink is attributed to whichever branches were requested
/// (slight over-counting if both were on, but matches per-branch error
/// fan-out semantics).
#[derive(Default)]
pub struct SinglePassResult {
    pub input_duration_secs: f64,
    pub trickplay: Option<Vec<JpegFrame>>,
    pub vhash: Option<Vec<VhashDescriptor>>,
    pub embed_src: Option<EmbedSrcResult>,
    pub chromaprint: Option<ChromaprintResult>,
    pub silence: Option<Vec<SilenceInterval>>,
    pub loudness: Option<f64>,
    pub audio_peaks: Option<AudioPeaksResult>,
    pub black_frames: Option<Vec<BlackInterval>>,
    pub corruption: Option<Vec<CorruptRange>>,
    pub per_branch_errors: Vec<(BranchId, FfmpegError)>,
    pub branch_durations: HashMap<BranchId, Duration>,
    /// Where decoding started when every requested branch was windowed, so the run
    /// skipped the file ahead of the earliest window. `None` = decoded from the start.
    pub seeked_to_secs: Option<f64>,
}

/// Seek this far ahead of the earliest window: some demuxers (mpegts) land after the
/// target even with `AVSEEK_FLAG_BACKWARD`.
const WINDOW_SEEK_LEAD_SECS: f64 = 5.0;

/// The earliest window start when every requested branch is a windowed video branch
/// (vhash / embed_src with non-empty windows). `None` when any branch needs the whole
/// file, which is every audio branch plus trickplay, black frames and corruption.
fn windowed_only_start(req: &SinglePassRequest) -> Option<f64> {
    let whole_file = req.trickplay.is_some()
        || req.black_frames.is_some()
        || req.corruption.is_some()
        || req.chromaprint.is_some()
        || req.silence.is_some()
        || req.loudness
        || req.audio_peaks.is_some();
    if whole_file {
        return None;
    }
    let windows: Vec<&[(f64, f64)]> = req
        .vhash
        .iter()
        .map(|p| p.windows.as_slice())
        .chain(req.embed_src.iter().map(|p| p.windows.as_slice()))
        .collect();
    if windows.is_empty() || windows.iter().any(|w| w.is_empty()) {
        return None;
    }
    windows
        .iter()
        .flat_map(|w| w.iter().map(|(start, _)| *start))
        .reduce(f64::min)
}

// ── Internal sink state ──────────────────────────────────────────────────────

/// `true` if `time` falls inside any `[start, end)` window, or if `windows`
/// is empty (meaning "whole file — no filter").
fn time_in_windows(time: f64, windows: &[(f64, f64)]) -> bool {
    if windows.is_empty() {
        return true;
    }
    windows.iter().any(|(s, e)| time >= *s && time < *e)
}

/// Where a [`VideoJpegSink`] sends each freshly-encoded JPEG.
enum VideoJpegOutput {
    /// Accumulate every frame into an owned Vec; consumed at finalize.
    Buffered(Vec<JpegFrame>),
    /// Hand each frame to a [`TrickplaySink`] in capture order. `idx` is the
    /// monotonically-increasing index passed to `on_frame`.
    Streaming {
        sink: Box<dyn TrickplaySink>,
        idx: usize,
    },
}

/// Per-branch state for a video sink that produces JPEGs at a target fps with a target width.
struct VideoJpegSink {
    width: i32,
    quality: i32,
    grid: TargetFpsGrid,
    windows: Vec<(f64, f64)>,
    scaler: Option<Scaler>,
    // Cached source dims to know when to rebuild the scaler.
    src_w: i32,
    src_h: i32,
    src_fmt: ffi::AVPixelFormat,
    output: VideoJpegOutput,
}

impl VideoJpegSink {
    fn new_buffered(width: i32, fps: f64, quality: i32, windows: Vec<(f64, f64)>) -> Self {
        Self::with_output(
            width,
            fps,
            quality,
            windows,
            VideoJpegOutput::Buffered(Vec::new()),
        )
    }

    fn new_streaming(
        width: i32,
        fps: f64,
        quality: i32,
        windows: Vec<(f64, f64)>,
        sink: Box<dyn TrickplaySink>,
    ) -> Self {
        Self::with_output(
            width,
            fps,
            quality,
            windows,
            VideoJpegOutput::Streaming { sink, idx: 0 },
        )
    }

    fn with_output(
        width: i32,
        fps: f64,
        quality: i32,
        windows: Vec<(f64, f64)>,
        output: VideoJpegOutput,
    ) -> Self {
        Self {
            width,
            quality,
            grid: TargetFpsGrid::new(fps, 0.0),
            windows,
            scaler: None,
            src_w: 0,
            src_h: 0,
            src_fmt: ffi::AV_PIX_FMT_NONE,
            output,
        }
    }

    fn ensure_scaler(&mut self, frame: &Frame) -> Result<()> {
        let w = frame.width();
        let h = frame.height();
        let fmt = frame.format() as ffi::AVPixelFormat;
        if self.scaler.is_some() && w == self.src_w && h == self.src_h && fmt == self.src_fmt {
            return Ok(());
        }
        let aspect = h as f64 / w as f64;
        let dst_h = ((self.width as f64 * aspect) as i32 + 1) & !1;
        // Scale into the same source format — the JPEG encoder converts to YUVJ420P
        // afterwards, matching `extract_frames_sequential_with_hw`'s behaviour.
        self.scaler = Some(Scaler::bilinear(w, h, fmt, self.width, dst_h, fmt)?);
        self.src_w = w;
        self.src_h = h;
        self.src_fmt = fmt;
        Ok(())
    }

    fn feed(&mut self, frame: &Frame, frame_time: f64) -> Result<()> {
        if !self.grid.take_if_due(frame_time) {
            return Ok(());
        }
        if !time_in_windows(frame_time, &self.windows) {
            return Ok(());
        }
        self.ensure_scaler(frame)?;

        let mut scaled = Frame::new()?;
        self.scaler
            .as_ref()
            .unwrap()
            .scale_frame(frame, &mut scaled)?;
        let (jpeg, w, h) = encode_jpeg(&scaled, self.quality)?;
        let out = JpegFrame {
            timestamp_secs: frame_time,
            jpeg_data: jpeg,
            width: w,
            height: h,
        };
        match &mut self.output {
            VideoJpegOutput::Buffered(frames) => frames.push(out),
            VideoJpegOutput::Streaming { sink, idx } => {
                let i = *idx;
                *idx += 1;
                sink.on_frame(i, frame_time, out)?;
            }
        }
        Ok(())
    }

    /// Consume the sink at finalize. For [`VideoJpegOutput::Buffered`] returns
    /// the accumulated Vec; for [`VideoJpegOutput::Streaming`] flushes the sink
    /// via `finish()` and returns `None` (frames already left the building).
    fn finalize(self) -> Result<Option<Vec<JpegFrame>>> {
        match self.output {
            VideoJpegOutput::Buffered(frames) => Ok(Some(frames)),
            VideoJpegOutput::Streaming { mut sink, .. } => {
                sink.finish()?;
                Ok(None)
            }
        }
    }
}

/// Per-branch state for vhash: scale → 16x16 grayscale → store.
struct VhashSink {
    intermediate_width: i32,
    grid: TargetFpsGrid,
    windows: Vec<(f64, f64)>,
    intermediate_scaler: Option<Scaler>,
    final_scaler: Option<Scaler>,
    src_w: i32,
    src_h: i32,
    src_fmt: ffi::AVPixelFormat,
    descriptors: Vec<VhashDescriptor>,
}

impl VhashSink {
    fn new(intermediate_width: i32, fps: f64, windows: Vec<(f64, f64)>) -> Self {
        Self {
            intermediate_width,
            grid: TargetFpsGrid::new(fps, 0.0),
            windows,
            intermediate_scaler: None,
            final_scaler: None,
            src_w: 0,
            src_h: 0,
            src_fmt: ffi::AV_PIX_FMT_NONE,
            descriptors: Vec::new(),
        }
    }

    fn ensure_scalers(&mut self, frame: &Frame) -> Result<()> {
        let w = frame.width();
        let h = frame.height();
        let fmt = frame.format() as ffi::AVPixelFormat;
        if self.intermediate_scaler.is_some()
            && w == self.src_w
            && h == self.src_h
            && fmt == self.src_fmt
        {
            return Ok(());
        }
        let aspect = h as f64 / w as f64;
        let inter_h = ((self.intermediate_width as f64 * aspect) as i32 + 1) & !1;
        // Stage 1: scale to intermediate (e.g. 64-wide), keep source pixfmt.
        self.intermediate_scaler = Some(Scaler::bilinear(
            w,
            h,
            fmt,
            self.intermediate_width,
            inter_h,
            fmt,
        )?);
        // Stage 2: scale intermediate to 16x16 grayscale.
        self.final_scaler = Some(Scaler::bilinear(
            self.intermediate_width,
            inter_h,
            fmt,
            16,
            16,
            ffi::AV_PIX_FMT_GRAY8,
        )?);
        self.src_w = w;
        self.src_h = h;
        self.src_fmt = fmt;
        Ok(())
    }

    fn feed(&mut self, frame: &Frame, frame_time: f64) -> Result<()> {
        if !self.grid.take_if_due(frame_time) {
            return Ok(());
        }
        if !time_in_windows(frame_time, &self.windows) {
            return Ok(());
        }
        self.ensure_scalers(frame)?;

        let mut intermediate = Frame::new()?;
        self.intermediate_scaler
            .as_ref()
            .unwrap()
            .scale_frame(frame, &mut intermediate)?;
        let mut tiny = Frame::new()?;
        self.final_scaler
            .as_ref()
            .unwrap()
            .scale_frame(&intermediate, &mut tiny)?;

        let data = copy_gray8_frame(&tiny);
        self.descriptors.push(VhashDescriptor {
            timestamp_secs: frame_time,
            data,
            width: tiny.width(),
            height: tiny.height(),
        });
        Ok(())
    }
}

/// Per-branch state for chromaprint: feed every audio frame through a resampler
/// targeting 8 kHz mono s16, and into a libchromaprint context.
struct ChromaprintSink {
    cp_ctx: ChromaprintContext,
    resampler: Option<crate::resample::Resampler>,
    resampler_state: Option<AudioGraphState>,
    total_samples: u64,
    offset_secs: Option<f64>,
    end_time_secs: Option<f64>,
    started: bool,
    finished: bool,
}

const CHROMAPRINT_SAMPLE_RATE: i32 = 8000;

struct ChromaprintContext(*mut ffi::ChromaprintContext);

impl ChromaprintContext {
    fn new() -> Result<Self> {
        let ptr = unsafe { ffi::chromaprint_new(ffi::CHROMAPRINT_ALGORITHM_DEFAULT as i32) };
        if ptr.is_null() {
            return Err(FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "failed to create chromaprint context",
            ));
        }
        unsafe {
            if ffi::chromaprint_start(ptr, CHROMAPRINT_SAMPLE_RATE, 1) == 0 {
                ffi::chromaprint_free(ptr);
                return Err(FfmpegError::new(
                    ffi::AVERROR_EXTERNAL,
                    "chromaprint_start failed",
                ));
            }
        }
        Ok(Self(ptr))
    }
}

impl Drop for ChromaprintContext {
    fn drop(&mut self) {
        unsafe { ffi::chromaprint_free(self.0) };
    }
}

impl ChromaprintSink {
    fn new(params: &ChromaprintParams) -> Result<Self> {
        let cp_ctx = ChromaprintContext::new()?;
        let end_time_secs = match (params.offset_secs, params.length_secs) {
            (Some(o), Some(l)) => Some(o + l),
            (None, Some(l)) => Some(l),
            _ => None,
        };
        Ok(Self {
            cp_ctx,
            resampler: None,
            resampler_state: None,
            total_samples: 0,
            offset_secs: params.offset_secs,
            end_time_secs,
            started: false,
            finished: false,
        })
    }

    fn feed(&mut self, frame: &Frame, frame_time: f64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if let Some(offset) = self.offset_secs
            && frame_time < offset
        {
            return Ok(());
        }
        if let Some(end) = self.end_time_secs
            && frame_time >= end
        {
            self.finished = true;
            return Ok(());
        }
        AudioGraphState::ensure_resampler(
            &mut self.resampler,
            &mut self.resampler_state,
            frame,
            CHROMAPRINT_SAMPLE_RATE,
            SampleFormat::S16,
            ChannelLayout::mono(),
        )?;
        let pcm = self
            .resampler
            .as_mut()
            .unwrap()
            .convert_frame_to_bytes(frame)?;
        if !pcm.is_empty() {
            let samples = pcm.len() / 2;
            self.started = true;
            unsafe {
                ffi::chromaprint_feed(self.cp_ctx.0, pcm.as_ptr() as *const i16, samples as i32);
            }
            self.total_samples += samples as u64;
        }
        Ok(())
    }

    fn finalize(mut self) -> Result<ChromaprintResult> {
        if !self.started {
            return Ok(ChromaprintResult {
                raw: Vec::new(),
                duration_secs: 0.0,
            });
        }
        unsafe {
            if ffi::chromaprint_finish(self.cp_ctx.0) == 0 {
                return Err(FfmpegError::new(
                    ffi::AVERROR_EXTERNAL,
                    "chromaprint_finish failed",
                ));
            }
        }
        let mut fp_ptr: *mut u32 = std::ptr::null_mut();
        let mut fp_size: i32 = 0;
        unsafe {
            if ffi::chromaprint_get_raw_fingerprint(self.cp_ctx.0, &mut fp_ptr, &mut fp_size) == 0 {
                return Err(FfmpegError::new(
                    ffi::AVERROR_EXTERNAL,
                    "chromaprint_get_raw_fingerprint failed",
                ));
            }
        }
        let raw = if fp_ptr.is_null() || fp_size <= 0 {
            Vec::new()
        } else {
            let slice = unsafe { std::slice::from_raw_parts(fp_ptr, fp_size as usize) };
            let v = slice.to_vec();
            unsafe { ffi::chromaprint_dealloc(fp_ptr as *mut std::ffi::c_void) };
            v
        };
        let duration_secs = self.total_samples as f64 / CHROMAPRINT_SAMPLE_RATE as f64;
        // Stop the borrow checker from running drop early — we already called finish/get.
        let _ = std::mem::replace(&mut self.cp_ctx, ChromaprintContext(std::ptr::null_mut()));
        Ok(ChromaprintResult { raw, duration_secs })
    }
}

/// Combined silence + LUFS sink. Both filters are passthrough metadata-emitters
/// in lavfi, so chaining them as `silencedetect=…,ebur128=…` lets us run one
/// filter graph (one push, one drain per audio frame) instead of two parallel
/// graphs. Each lavfi.* metadata key is parsed independently from the same
/// output frames.
///
/// Result is kept split so the caller still gets `silence: Vec<SilenceInterval>`
/// and `loudness: Option<f64>` separately — the combination is purely an
/// implementation detail of how we run the graph.
struct AudioMetadataSink {
    filter_str: String,
    graph: Option<FilterGraph>,
    state: Option<AudioGraphState>,
    want_silence: bool,
    want_loudness: bool,
    intervals: Vec<SilenceInterval>,
    silence_current_start: Option<f64>,
    last_lufs: Option<f64>,
}

impl AudioMetadataSink {
    fn new(silence: Option<&SilenceParams>, loudness: bool) -> Self {
        let mut parts: Vec<String> = Vec::with_capacity(2);
        if let Some(p) = silence {
            parts.push(format!(
                "silencedetect=noise={}dB:d={}",
                p.noise_db, p.min_duration_secs
            ));
        }
        if loudness {
            parts.push("ebur128=peak=none:metadata=1".to_string());
        }
        Self {
            filter_str: parts.join(","),
            graph: None,
            state: None,
            want_silence: silence.is_some(),
            want_loudness: loudness,
            intervals: Vec::new(),
            silence_current_start: None,
            last_lufs: None,
        }
    }

    fn feed(&mut self, frame: &mut Frame, time_base: ffi::AVRational) -> Result<()> {
        AudioGraphState::ensure_filter_graph(
            &mut self.graph,
            &mut self.state,
            frame,
            time_base,
            &self.filter_str,
        )?;
        self.graph.as_mut().unwrap().push_frame_keep_ref(frame)?;
        self.drain()?;
        Ok(())
    }

    fn finalize(mut self) -> Result<(Option<Vec<SilenceInterval>>, Option<f64>)> {
        if self.graph.is_some() {
            self.graph.as_mut().unwrap().push_eof()?;
            self.drain()?;
        }
        let silence = if self.want_silence {
            Some(self.intervals)
        } else {
            None
        };
        let loudness = if self.want_loudness {
            self.last_lufs
        } else {
            None
        };
        Ok((silence, loudness))
    }

    /// Pull all available output frames out of the filter graph and parse the
    /// per-filter metadata into `self`. Splits graph borrow from `&mut self`
    /// borrow by destructuring up front.
    fn drain(&mut self) -> Result<()> {
        let Some(graph) = self.graph.as_mut() else {
            return Ok(());
        };
        let want_silence = self.want_silence;
        let want_loudness = self.want_loudness;
        let intervals = &mut self.intervals;
        let silence_current_start = &mut self.silence_current_start;
        let last_lufs = &mut self.last_lufs;
        let mut out = Frame::new()?;
        while graph.pull_frame(&mut out)? {
            if want_silence {
                parse_silence_metadata(&out, intervals, silence_current_start);
            }
            if want_loudness && let Some(lufs) = unsafe { extract_lufs(&out) } {
                *last_lufs = Some(lufs);
            }
            out.unref();
        }
        Ok(())
    }
}

/// Per-branch state for black-frame detection: run every decoded video frame
/// through a `blackdetect` FilterGraph and parse `lavfi.black_start` /
/// `lavfi.black_end` metadata from output frames.
///
/// Unlike audio sinks, the filter graph is built lazily on the first frame
/// (so we can read actual decoded pixel format / dimensions) and never
/// rebuilt — mid-stream video format changes are not seen in practice for
/// the kinds of inputs we analyse.
struct BlackFramesSink {
    filter_str: String,
    time_base: ffi::AVRational,
    graph: Option<FilterGraph>,
    intervals: Vec<BlackInterval>,
    current_start: Option<f64>,
}

impl BlackFramesSink {
    fn new(params: &BlackFramesParams, time_base: ffi::AVRational) -> Self {
        Self {
            filter_str: params.filter_string(),
            time_base,
            graph: None,
            intervals: Vec::new(),
            current_start: None,
        }
    }

    fn feed(&mut self, frame: &mut Frame) -> Result<()> {
        if self.graph.is_none() {
            let params = VideoFilterParams {
                width: frame.width(),
                height: frame.height(),
                pixel_format: frame.format() as ffi::AVPixelFormat,
                time_base: self.time_base,
                sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
                hw_frames_ctx: None,
                color: Default::default(),
                side_data_frame: frame.as_ptr(),
            };
            self.graph = Some(FilterGraph::new_video(&params, &self.filter_str)?);
        }
        let g = self.graph.as_mut().unwrap();
        g.push_frame_keep_ref(frame)?;
        let mut out = Frame::new()?;
        while g.pull_frame(&mut out)? {
            parse_black_metadata(&out, &mut self.intervals, &mut self.current_start);
            out.unref();
        }
        Ok(())
    }

    fn finalize(mut self) -> Result<Vec<BlackInterval>> {
        if let Some(ref mut g) = self.graph {
            g.push_eof()?;
            let mut out = Frame::new()?;
            while g.pull_frame(&mut out)? {
                parse_black_metadata(&out, &mut self.intervals, &mut self.current_start);
                out.unref();
            }
        }
        Ok(self.intervals)
    }
}

/// Per-branch state for corruption detection.
///
/// Reads `frame.flags & AV_FRAME_FLAG_CORRUPT` and `frame.decode_error_flags`
/// per frame and coalesces consecutive corrupt frames (within `merge_gap_secs`)
/// into one `CorruptRange`. Cost is O(1) per frame — no scaler, no allocation
/// in the common (clean) case.
struct CorruptionSink {
    merge_gap_secs: f64,
    ranges: Vec<CorruptRange>,
    /// Open range being extended by consecutive corrupt frames.
    /// Closed (pushed into `ranges`) when a clean frame appears or the gap exceeds tolerance.
    open: Option<CorruptRange>,
}

impl CorruptionSink {
    fn new(params: &CorruptionParams) -> Self {
        Self {
            merge_gap_secs: params.merge_gap_secs.max(0.0),
            ranges: Vec::new(),
            open: None,
        }
    }

    fn feed(&mut self, frame: &Frame, frame_time: f64) -> Result<()> {
        let (flags, derr) = unsafe {
            let p = frame.as_ptr();
            ((*p).flags as u32, (*p).decode_error_flags as u32)
        };
        let frame_corrupt = flags & ffi::AV_FRAME_FLAG_CORRUPT != 0;
        if !frame_corrupt && derr == 0 {
            self.flush_if_stale(frame_time);
            return Ok(());
        }

        let concealment = derr & ffi::FF_DECODE_ERROR_CONCEALMENT_ACTIVE != 0;
        let missing_reference = derr & ffi::FF_DECODE_ERROR_MISSING_REFERENCE != 0;
        let invalid_bitstream = derr & ffi::FF_DECODE_ERROR_INVALID_BITSTREAM != 0;
        let decode_slices_failed = derr & ffi::FF_DECODE_ERROR_DECODE_SLICES != 0;
        let other = frame_corrupt && derr == 0;

        match self.open.as_mut() {
            Some(open) if frame_time - open.end_secs <= self.merge_gap_secs => {
                open.end_secs = frame_time.max(open.end_secs);
                open.frame_count += 1;
                open.concealment |= concealment;
                open.missing_reference |= missing_reference;
                open.invalid_bitstream |= invalid_bitstream;
                open.decode_slices_failed |= decode_slices_failed;
                open.other |= other;
            }
            _ => {
                if let Some(prev) = self.open.take() {
                    self.ranges.push(prev);
                }
                self.open = Some(CorruptRange {
                    start_secs: frame_time,
                    end_secs: frame_time,
                    frame_count: 1,
                    concealment,
                    missing_reference,
                    invalid_bitstream,
                    decode_slices_failed,
                    other,
                });
            }
        }
        Ok(())
    }

    fn flush_if_stale(&mut self, frame_time: f64) {
        if let Some(open) = self.open.as_ref()
            && frame_time - open.end_secs > self.merge_gap_secs
            && let Some(prev) = self.open.take()
        {
            self.ranges.push(prev);
        }
    }

    fn finalize(mut self) -> Result<Vec<CorruptRange>> {
        if let Some(prev) = self.open.take() {
            self.ranges.push(prev);
        }
        Ok(self.ranges)
    }
}

/// Per-branch state for audio peaks: resample to (sample_rate) mono s16, then
/// bucket each (1/sample_rate)-second window into a (min_i8, max_i8) pair.
///
/// Wire format (version 2):
///   `[version: u8 = 2, peaks_per_second: u16_le (2 bytes), (min_i8, max_i8) pairs ...]`
///
/// Version 1 (legacy, used by `kobbler/api/src/audio/peaks.rs`) is:
///   `[version: u8 = 1, peaks_per_second: u8, (min_i8, max_i8) pairs ...]`
///
/// Bumped to v2 because v1's `peaks_per_second: u8` couldn't represent the
/// 1000 Hz rate the cache uses for `detect_post_credits_from_cache` RMS math —
/// it silently clamped to 255 in the header.
struct AudioPeaksSink {
    sample_rate: i32,
    resampler: Option<crate::resample::Resampler>,
    resampler_state: Option<AudioGraphState>,
    /// Carry-over PCM bytes that didn't fit into a full bucket on the previous frame.
    leftover: Vec<u8>,
    pairs: Vec<(i8, i8)>,
}

impl AudioPeaksSink {
    fn new(sample_rate: i32) -> Self {
        Self {
            sample_rate,
            resampler: None,
            resampler_state: None,
            leftover: Vec::new(),
            pairs: Vec::new(),
        }
    }

    fn feed(&mut self, frame: &Frame) -> Result<()> {
        AudioGraphState::ensure_resampler(
            &mut self.resampler,
            &mut self.resampler_state,
            frame,
            self.sample_rate,
            SampleFormat::S16,
            ChannelLayout::mono(),
        )?;
        let pcm = self
            .resampler
            .as_mut()
            .unwrap()
            .convert_frame_to_bytes(frame)?;
        if pcm.is_empty() {
            return Ok(());
        }
        // One pair per sample: each i16 sample maps to (min, max) where min == max == i8 reduction.
        // This matches the semantics of the existing kobbler peaks generator: at sample_rate Hz,
        // each output pair represents one sample window.
        let mut buf = std::mem::take(&mut self.leftover);
        buf.extend_from_slice(&pcm);
        let mut i = 0;
        while i + 2 <= buf.len() {
            let s = i16::from_le_bytes([buf[i], buf[i + 1]]);
            let scaled = (s >> 8) as i8;
            self.pairs.push((scaled, scaled));
            i += 2;
        }
        // Carry over any odd byte (shouldn't happen for s16, but defensive).
        self.leftover = buf[i..].to_vec();
        Ok(())
    }

    fn finalize(self) -> AudioPeaksResult {
        let mut bytes = Vec::with_capacity(3 + self.pairs.len() * 2);
        bytes.push(2u8); // version
        let pps = self.sample_rate.clamp(0, u16::MAX as i32) as u16;
        bytes.extend_from_slice(&pps.to_le_bytes());
        for (min, max) in &self.pairs {
            bytes.push(*min as u8);
            bytes.push(*max as u8);
        }
        AudioPeaksResult {
            bytes,
            sample_rate: self.sample_rate,
        }
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Run a single decode of `req.input_path`, fanning each frame into the
/// configured branch sinks, and return their results.
///
/// Equivalent to [`run_single_pass_with_sinks`] with no custom sinks — all
/// branches use their default in-memory accumulators and trickplay frames
/// land in [`SinglePassResult::trickplay`].
pub fn run_single_pass(
    req: SinglePassRequest,
    run: Option<&RunContext>,
) -> Result<SinglePassResult> {
    run_single_pass_with_sinks(req, run, CustomSinks::default())
}

/// Run a single decode like [`run_single_pass`], but routing the trickplay
/// branch (and any future streaming branch in [`CustomSinks`]) through a
/// caller-supplied sink for progressive emission. When `sinks.trickplay` is
/// `Some(_)`, [`SinglePassResult::trickplay`] stays `None` for that run — the
/// caller's sink owns the frames.
pub fn run_single_pass_with_sinks(
    req: SinglePassRequest,
    run: Option<&RunContext>,
    sinks: CustomSinks,
) -> Result<SinglePassResult> {
    crate::init();
    let mut ctx = InputContext::open_path(&req.input_path)?;
    ctx.find_stream_info()?;
    run_single_pass_from_ctx_with_sinks(ctx, req, run, sinks)
}

/// Like [`run_single_pass`], but over an already-open, already
/// `find_stream_info()`'d context — the seam for inputs that have no path
/// (custom I/O, a file still being written). `req.input_path` is ignored.
pub fn run_single_pass_from_ctx(
    ctx: InputContext,
    req: SinglePassRequest,
    run: Option<&RunContext>,
) -> Result<SinglePassResult> {
    run_single_pass_from_ctx_with_sinks(ctx, req, run, CustomSinks::default())
}

/// [`run_single_pass_from_ctx`] with caller-supplied streaming sinks.
pub fn run_single_pass_from_ctx_with_sinks(
    mut ctx: InputContext,
    req: SinglePassRequest,
    run: Option<&RunContext>,
    mut sinks: CustomSinks,
) -> Result<SinglePassResult> {
    crate::init();

    let video_idx = ctx.find_best_stream(MediaType::Video);
    let audio_idx = ctx.find_best_stream(MediaType::Audio);

    let want_video = req.trickplay.is_some()
        || req.vhash.is_some()
        || req.embed_src.is_some()
        || req.black_frames.is_some()
        || req.corruption.is_some();
    let want_audio = req.chromaprint.is_some()
        || req.silence.is_some()
        || req.loudness
        || req.audio_peaks.is_some();

    if want_video && video_idx.is_none() {
        return Err(FfmpegError::new(
            ffi::AVERROR_STREAM_NOT_FOUND,
            "no video stream but video branches were requested",
        ));
    }
    if want_audio && audio_idx.is_none() {
        return Err(FfmpegError::new(
            ffi::AVERROR_STREAM_NOT_FOUND,
            "no audio stream but audio branches were requested",
        ));
    }

    // Optionally discard streams we don't care about. We only do this when one of
    // (video, audio) is fully unused — leaving both enabled when both are needed
    // matches the existing extraction code.
    if want_video && !want_audio {
        if let Some(idx) = video_idx {
            ctx.discard_all_except(idx);
        }
    } else if want_audio
        && !want_video
        && let Some(idx) = audio_idx
    {
        ctx.discard_all_except(idx);
    }

    let mut input_duration_secs = 0.0_f64;

    // ── Set up video decoder + sinks ────────────────────────────────────────
    let mut video_decoder = None;
    let mut video_tb_f64 = 0.0_f64;
    let mut trickplay_sink: Option<std::result::Result<VideoJpegSink, FfmpegError>> = None;
    let mut vhash_sink: Option<std::result::Result<VhashSink, FfmpegError>> = None;
    let mut embed_src_sink: Option<std::result::Result<VideoJpegSink, FfmpegError>> = None;
    let mut black_frames_sink: Option<std::result::Result<BlackFramesSink, FfmpegError>> = None;
    let mut corruption_sink: Option<std::result::Result<CorruptionSink, FfmpegError>> = None;
    if let Some(idx) = video_idx
        && want_video
    {
        let stream = ctx.stream(idx).unwrap();
        let tb = stream.time_base_native();
        video_tb_f64 = tb.num as f64 / tb.den as f64;
        let video_tb_av = stream.time_base();
        // Frame-threaded decode: let ffmpeg auto-pick thread_count (~num_cpus).
        // This is a batch analytics path — added latency from frame threading
        // is irrelevant and wall-time drops ~5.5×.
        let mut dec = stream.decoder_with_threads(0)?;
        if req.fast_decode {
            dec.set_skip_loop_filter(ffi::AVDISCARD_ALL);
        }
        dec.flush();
        video_decoder = Some(dec);
        if let Some(p) = &req.trickplay {
            // Trickplay is whole-file — its sprite sheets drive the UI seek bar.
            // If the caller provided a streaming sink, route every frame through
            // it; otherwise keep the historical buffer-into-Vec behaviour.
            let sink = match sinks.trickplay.take() {
                Some(custom) => {
                    VideoJpegSink::new_streaming(p.width, p.fps, p.jpeg_quality, Vec::new(), custom)
                }
                None => VideoJpegSink::new_buffered(p.width, p.fps, p.jpeg_quality, Vec::new()),
            };
            trickplay_sink = Some(Ok(sink));
        }
        if let Some(p) = &req.vhash {
            vhash_sink = Some(Ok(VhashSink::new(
                p.intermediate_width,
                p.fps,
                p.windows.clone(),
            )));
        }
        if let Some(p) = &req.embed_src {
            // embed_src is always buffered — the caller sends the full Vec to
            // the vision service in one batch.
            embed_src_sink = Some(Ok(VideoJpegSink::new_buffered(
                p.width,
                p.fps,
                p.jpeg_quality,
                p.windows.clone(),
            )));
        }
        if let Some(p) = &req.black_frames {
            black_frames_sink = Some(Ok(BlackFramesSink::new(p, video_tb_av)));
        }
        if let Some(p) = &req.corruption {
            corruption_sink = Some(Ok(CorruptionSink::new(p)));
        }
    }

    // ── Set up audio decoder + sinks ────────────────────────────────────────
    let mut audio_decoder = None;
    let mut audio_tb = ffi::AVRational { num: 1, den: 1 };
    let mut audio_tb_f64 = 0.0_f64;
    let mut chromaprint_sink: Option<std::result::Result<ChromaprintSink, FfmpegError>> = None;
    // Combined silence + LUFS sink — one chained `silencedetect,ebur128` filter
    // graph, parsed metadata split into separate result fields.
    let mut audio_meta_sink: Option<std::result::Result<AudioMetadataSink, FfmpegError>> = None;
    let want_silence = req.silence.is_some();
    let want_loudness = req.loudness;
    let mut audio_peaks_sink: Option<std::result::Result<AudioPeaksSink, FfmpegError>> = None;
    if let Some(idx) = audio_idx
        && want_audio
    {
        let stream = ctx.stream(idx).unwrap();
        audio_tb = stream.time_base();
        audio_tb_f64 = audio_tb.num as f64 / audio_tb.den as f64;
        let dec = stream.decoder()?;
        audio_decoder = Some(dec);
        if let Some(p) = &req.chromaprint {
            chromaprint_sink = Some(ChromaprintSink::new(p));
        }
        if want_silence || want_loudness {
            audio_meta_sink = Some(Ok(AudioMetadataSink::new(
                req.silence.as_ref(),
                want_loudness,
            )));
        }
        if let Some(p) = &req.audio_peaks {
            audio_peaks_sink = Some(Ok(AudioPeaksSink::new(p.sample_rate)));
        }
    }

    // Only windowed branches: start at the earliest window rather than decoding (and
    // reading off the NAS) everything before it. The sinks' fps grids stay anchored at
    // 0 and advance past the pre-window frames, so they capture the same frames a
    // from-the-start decode would. A failed seek leaves the position at the start.
    let mut seeked_to_secs = None;
    if let Some(start) = windowed_only_start(&req) {
        let target = (start - WINDOW_SEEK_LEAD_SECS).max(0.0);
        if target > 0.0 {
            let ts = (target * ffi::AV_TIME_BASE as f64) as i64;
            match ctx.seek(-1, ts, ffi::AVSEEK_FLAG_BACKWARD as i32) {
                Ok(()) => {
                    if let Some(dec) = video_decoder.as_mut() {
                        dec.flush();
                    }
                    seeked_to_secs = Some(target);
                }
                Err(e) => {
                    tracing::warn!(error = %e, seek_secs = target, "window seek failed; decoding from the start");
                }
            }
        }
    }

    // ── Decode loop ─────────────────────────────────────────────────────────
    // Per-branch wall-clock accumulator, threaded through every feed/finalize
    // so callers can see where a long single_pass run actually spent its time.
    let mut branch_durations: HashMap<BranchId, Duration> = HashMap::new();
    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;

    while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;

        let stream_idx = packet.stream_index() as u32;
        if Some(stream_idx) == video_idx {
            if let Some(dec) = video_decoder.as_mut() {
                dec.send_packet(&packet)?;
                loop {
                    match dec.receive_frame(&mut frame)? {
                        DecodeResult::Frame => {
                            let pts = frame.pts();
                            if pts == ffi::AV_NOPTS_VALUE {
                                continue;
                            }
                            let frame_time = pts as f64 * video_tb_f64;
                            if frame_time > input_duration_secs {
                                input_duration_secs = frame_time;
                            }
                            feed_video_sinks(
                                &mut frame,
                                frame_time,
                                &mut trickplay_sink,
                                &mut vhash_sink,
                                &mut embed_src_sink,
                                &mut black_frames_sink,
                                &mut corruption_sink,
                                &mut branch_durations,
                            )?;
                        }
                        DecodeResult::NeedsInput => break,
                        DecodeResult::Eof => break,
                    }
                }
            }
        } else if Some(stream_idx) == audio_idx
            && let Some(dec) = audio_decoder.as_mut()
        {
            dec.send_packet(&packet)?;
            loop {
                match dec.receive_frame(&mut frame)? {
                    DecodeResult::Frame => {
                        let pts = frame.pts();
                        let frame_time = if pts == ffi::AV_NOPTS_VALUE {
                            0.0
                        } else {
                            pts as f64 * audio_tb_f64
                        };
                        if frame_time > input_duration_secs {
                            input_duration_secs = frame_time;
                        }
                        feed_audio_sinks(
                            &mut frame,
                            frame_time,
                            audio_tb,
                            &mut chromaprint_sink,
                            &mut audio_meta_sink,
                            &mut audio_peaks_sink,
                            &mut branch_durations,
                            want_silence,
                            want_loudness,
                        );
                    }
                    DecodeResult::NeedsInput => break,
                    DecodeResult::Eof => break,
                }
            }
        }
    }

    // ── Flush decoders ──────────────────────────────────────────────────────
    if let Some(dec) = video_decoder.as_mut() {
        dec.send_eof()?;
        loop {
            match dec.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    let pts = frame.pts();
                    if pts == ffi::AV_NOPTS_VALUE {
                        continue;
                    }
                    let frame_time = pts as f64 * video_tb_f64;
                    if frame_time > input_duration_secs {
                        input_duration_secs = frame_time;
                    }
                    feed_video_sinks(
                        &mut frame,
                        frame_time,
                        &mut trickplay_sink,
                        &mut vhash_sink,
                        &mut embed_src_sink,
                        &mut black_frames_sink,
                        &mut corruption_sink,
                        &mut branch_durations,
                    )?;
                }
                _ => break,
            }
        }
    }
    if let Some(dec) = audio_decoder.as_mut() {
        dec.send_eof()?;
        loop {
            match dec.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    let pts = frame.pts();
                    let frame_time = if pts == ffi::AV_NOPTS_VALUE {
                        0.0
                    } else {
                        pts as f64 * audio_tb_f64
                    };
                    if frame_time > input_duration_secs {
                        input_duration_secs = frame_time;
                    }
                    feed_audio_sinks(
                        &mut frame,
                        frame_time,
                        audio_tb,
                        &mut chromaprint_sink,
                        &mut audio_meta_sink,
                        &mut audio_peaks_sink,
                        &mut branch_durations,
                        want_silence,
                        want_loudness,
                    );
                }
                _ => break,
            }
        }
    }

    // ── Finalize sinks ─────────────────────────────────────────────────────
    // Carry the per-branch feed-loop accumulator into result so each
    // take_branch call below can add its finalize() time on top.
    let mut result = SinglePassResult {
        input_duration_secs,
        branch_durations,
        seeked_to_secs,
        ..Default::default()
    };

    take_branch(
        BranchId::Trickplay,
        trickplay_sink,
        &mut result,
        |sink, r| {
            // Buffered sink → populate result.trickplay; streaming sink →
            // sink.finish() and leave result.trickplay = None.
            r.trickplay = sink.finalize()?;
            Ok(())
        },
    );
    take_branch(BranchId::Vhash, vhash_sink, &mut result, |sink, r| {
        r.vhash = Some(sink.descriptors);
        Ok(())
    });
    take_branch(
        BranchId::EmbedSrc,
        embed_src_sink,
        &mut result,
        |sink, r| {
            // embed_src is constructed buffered, so finalize always returns Some.
            r.embed_src = sink.finalize()?;
            Ok(())
        },
    );
    take_branch(
        BranchId::Chromaprint,
        chromaprint_sink,
        &mut result,
        |sink, r| {
            let res = sink.finalize()?;
            r.chromaprint = Some(res);
            Ok(())
        },
    );
    // Combined silence + LUFS finalize. On error, fan the failure out under
    // each requested branch so per-branch error reporting stays unchanged.
    let push_audio_meta_err = |result: &mut SinglePassResult, e: FfmpegError| {
        let dup = || FfmpegError::new(e.code(), format!("{e}"));
        match (want_silence, want_loudness) {
            (true, true) => {
                result.per_branch_errors.push((BranchId::Silence, dup()));
                result.per_branch_errors.push((BranchId::Loudness, e));
            }
            (true, false) => result.per_branch_errors.push((BranchId::Silence, e)),
            (false, true) => result.per_branch_errors.push((BranchId::Loudness, e)),
            (false, false) => {}
        }
    };
    match audio_meta_sink {
        Some(Ok(sink)) => {
            let started = Instant::now();
            let outcome = sink.finalize();
            let elapsed = started.elapsed();
            // Same fan-out as feed_audio_sinks for the combined sink.
            if want_silence {
                *result
                    .branch_durations
                    .entry(BranchId::Silence)
                    .or_default() += elapsed;
            }
            if want_loudness {
                *result
                    .branch_durations
                    .entry(BranchId::Loudness)
                    .or_default() += elapsed;
            }
            match outcome {
                Ok((silence, lufs)) => {
                    if let Some(intervals) = silence {
                        result.silence = Some(intervals);
                    }
                    if want_loudness {
                        result.loudness = lufs;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "audio metadata finalize failed");
                    push_audio_meta_err(&mut result, e);
                }
            }
        }
        Some(Err(e)) => push_audio_meta_err(&mut result, e),
        None => {}
    }
    take_branch(
        BranchId::AudioPeaks,
        audio_peaks_sink,
        &mut result,
        |sink, r| {
            r.audio_peaks = Some(sink.finalize());
            Ok(())
        },
    );
    take_branch(
        BranchId::BlackFrames,
        black_frames_sink,
        &mut result,
        |sink, r| {
            let intervals = sink.finalize()?;
            r.black_frames = Some(intervals);
            Ok(())
        },
    );
    take_branch(
        BranchId::Corruption,
        corruption_sink,
        &mut result,
        |sink, r| {
            let ranges = sink.finalize()?;
            r.corruption = Some(ranges);
            Ok(())
        },
    );

    Ok(result)
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Run a feed callback against a single sink slot. If the sink errors, log the
/// branch name + frame time and demote the slot to `Err` so subsequent frames
/// short-circuit. Used by `feed_video_sinks` / `feed_audio_sinks` to avoid
/// repeating the same `if let Some(_)/Ok(_)/Err(e)` pattern per branch.
///
/// Wall-clock spent inside `feed` is added to `timings[branch_id]` so callers
/// can profile per-branch decode cost across the whole run. `Instant::now()`
/// adds ~10-100ns per call — sub-ms over a full-file run.
fn try_feed<S>(
    slot: &mut Option<std::result::Result<S, FfmpegError>>,
    branch_id: BranchId,
    frame_time: f64,
    timings: &mut HashMap<BranchId, Duration>,
    feed: impl FnOnce(&mut S) -> Result<()>,
) {
    let Some(slot_ref) = slot else { return };
    let Ok(sink) = slot_ref else { return };
    let started = Instant::now();
    let result = feed(sink);
    *timings.entry(branch_id).or_default() += started.elapsed();
    if let Err(e) = result {
        tracing::warn!(?branch_id, error = %e, "branch failed at frame time {frame_time:.1}s");
        *slot_ref = Err(e);
    }
}

/// Like `try_feed` but records elapsed wall-time into a caller-supplied
/// `&mut Duration` instead of the shared `timings` HashMap, so it can run
/// safely inside a rayon spawn (disjoint borrows across tasks). Caller
/// merges the duration back into `timings` after `rayon::scope` returns.
fn feed_one<S>(
    slot: &mut Option<std::result::Result<S, FfmpegError>>,
    branch_id: BranchId,
    frame_time: f64,
    duration: &mut Duration,
    feed: impl FnOnce(&mut S) -> Result<()>,
) {
    let Some(slot_ref) = slot else { return };
    let Ok(sink) = slot_ref else { return };
    let started = Instant::now();
    let result = feed(sink);
    *duration += started.elapsed();
    if let Err(e) = result {
        tracing::warn!(?branch_id, error = %e, "branch failed at frame time {frame_time:.1}s");
        *slot_ref = Err(e);
    }
}

#[allow(clippy::too_many_arguments)]
fn feed_video_sinks(
    frame: &mut Frame,
    frame_time: f64,
    trickplay: &mut Option<std::result::Result<VideoJpegSink, FfmpegError>>,
    vhash: &mut Option<std::result::Result<VhashSink, FfmpegError>>,
    embed_src: &mut Option<std::result::Result<VideoJpegSink, FfmpegError>>,
    black_frames: &mut Option<std::result::Result<BlackFramesSink, FfmpegError>>,
    corruption: &mut Option<std::result::Result<CorruptionSink, FfmpegError>>,
    timings: &mut HashMap<BranchId, Duration>,
) -> Result<()> {
    // Serial: black_frames uses a blackdetect filter graph that requires
    // frames in order; leave it on the main thread.
    try_feed(
        black_frames,
        BranchId::BlackFrames,
        frame_time,
        timings,
        |s| s.feed(frame),
    );

    // Serial: corruption is two field reads + coalesce — cheaper than the
    // rayon scope overhead, and order-dependent (gap-coalescing).
    try_feed(corruption, BranchId::Corruption, frame_time, timings, |s| {
        s.feed(frame, frame_time)
    });

    // Parallel: the three CPU-heavy video sinks (JPEG encode × 2, vhash
    // scaler × 1) run independently. Frame is Send but !Sync, so each
    // task owns a distinct handle via `av_frame_ref` (O(1); no pixel copy).
    let tp_frame = frame.clone_ref()?;
    let vh_frame = frame.clone_ref()?;
    let es_frame = frame.clone_ref()?;

    let mut tp_dur = Duration::ZERO;
    let mut vh_dur = Duration::ZERO;
    let mut es_dur = Duration::ZERO;
    // `move` closures take ownership of their cloned Frame + their disjoint
    // &mut references. Frame is Send but !Sync, so we can't borrow it into
    // worker threads — only move owned copies.
    let tp_dur_ref = &mut tp_dur;
    let vh_dur_ref = &mut vh_dur;
    let es_dur_ref = &mut es_dur;
    rayon::scope(|scope| {
        scope.spawn(move |_| {
            feed_one(
                trickplay,
                BranchId::Trickplay,
                frame_time,
                tp_dur_ref,
                |s| s.feed(&tp_frame, frame_time),
            );
        });
        scope.spawn(move |_| {
            feed_one(vhash, BranchId::Vhash, frame_time, vh_dur_ref, |s| {
                s.feed(&vh_frame, frame_time)
            });
        });
        scope.spawn(move |_| {
            feed_one(embed_src, BranchId::EmbedSrc, frame_time, es_dur_ref, |s| {
                s.feed(&es_frame, frame_time)
            });
        });
    });
    *timings.entry(BranchId::Trickplay).or_default() += tp_dur;
    *timings.entry(BranchId::Vhash).or_default() += vh_dur;
    *timings.entry(BranchId::EmbedSrc).or_default() += es_dur;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn feed_audio_sinks(
    frame: &mut Frame,
    frame_time: f64,
    time_base: ffi::AVRational,
    chromaprint: &mut Option<std::result::Result<ChromaprintSink, FfmpegError>>,
    audio_meta: &mut Option<std::result::Result<AudioMetadataSink, FfmpegError>>,
    audio_peaks: &mut Option<std::result::Result<AudioPeaksSink, FfmpegError>>,
    timings: &mut HashMap<BranchId, Duration>,
    want_silence: bool,
    want_loudness: bool,
) {
    try_feed(
        chromaprint,
        BranchId::Chromaprint,
        frame_time,
        timings,
        |s| s.feed(frame, frame_time),
    );
    // audio_meta is a combined silence+LUFS sink; attribute its feed time to
    // both branches if both were requested. Mirrors the per-branch error
    // fan-out done in run_single_pass for the same sink. Inlined rather than
    // going through `try_feed` because we need to fan the elapsed wall-time
    // across two BranchIds, which the generic helper doesn't model.
    if let Some(slot_ref) = audio_meta
        && let Ok(sink) = slot_ref
    {
        let started = Instant::now();
        let outcome = sink.feed(frame, time_base);
        let elapsed = started.elapsed();
        if want_silence {
            *timings.entry(BranchId::Silence).or_default() += elapsed;
        }
        if want_loudness {
            *timings.entry(BranchId::Loudness).or_default() += elapsed;
        }
        if let Err(e) = outcome {
            tracing::warn!(branch = "audio_meta", error = %e, "branch failed at frame time {frame_time:.1}s");
            *slot_ref = Err(e);
        }
    }
    try_feed(
        audio_peaks,
        BranchId::AudioPeaks,
        frame_time,
        timings,
        |s| s.feed(frame),
    );
}

fn take_branch<S, F>(
    id: BranchId,
    slot: Option<std::result::Result<S, FfmpegError>>,
    result: &mut SinglePassResult,
    on_ok: F,
) where
    F: FnOnce(S, &mut SinglePassResult) -> Result<()>,
{
    match slot {
        Some(Ok(sink)) => {
            let started = Instant::now();
            let outcome = on_ok(sink, result);
            *result.branch_durations.entry(id).or_default() += started.elapsed();
            if let Err(e) = outcome {
                tracing::warn!(?id, error = %e, "branch finalize failed");
                result.per_branch_errors.push((id, e));
            }
        }
        Some(Err(e)) => result.per_branch_errors.push((id, e)),
        None => {}
    }
}

fn encode_jpeg(frame: &Frame, quality: i32) -> Result<(Vec<u8>, i32, i32)> {
    let src_w = frame.width();
    let src_h = frame.height();
    let src_fmt = frame.format() as ffi::AVPixelFormat;
    let target_fmt = ffi::AV_PIX_FMT_YUVJ420P;

    let converted_holder;
    let frame_ref: &Frame = if src_fmt == target_fmt {
        frame
    } else {
        let scaler = Scaler::bilinear(src_w, src_h, src_fmt, src_w, src_h, target_fmt)?;
        let mut converted = Frame::new()?;
        scaler.scale_frame(frame, &mut converted)?;
        converted_holder = converted;
        &converted_holder
    };

    let mut encoder = Encoder::video(VideoEncoderConfig {
        codec_name: "mjpeg".to_string(),
        width: src_w,
        height: src_h,
        pixel_format: PixelFormat::from_raw(target_fmt),
        time_base: TimeBase::new(1, 25),
        bit_rate: None,
        gop_size: None,
        global_header: false,
        quality: Some(quality.clamp(1, 31)),
        hw_frames_ctx: None,
        sample_aspect_ratio: None,
        frame_rate: None,
        color: Default::default(),
        options: Vec::new(),
    })?;
    encoder.send_frame(frame_ref)?;
    encoder.send_eof()?;
    let mut pkt = Packet::new()?;
    match encoder.receive_packet(&mut pkt)? {
        EncodeResult::Packet => Ok((pkt.data().to_vec(), src_w, src_h)),
        _ => Err(FfmpegError::new(
            ffi::AVERROR_EXTERNAL,
            "mjpeg encoder produced no packet",
        )),
    }
}

fn copy_gray8_frame(frame: &Frame) -> Vec<u8> {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let stride = frame.linesize(0) as usize;
    let mut data = Vec::with_capacity(w * h);
    if let Some(ptr) = frame.data_ptr(0) {
        for row in 0..h {
            let row_ptr = unsafe { ptr.add(row * stride) };
            let row_slice = unsafe { std::slice::from_raw_parts(row_ptr, w) };
            data.extend_from_slice(row_slice);
        }
    }
    data
}

unsafe fn extract_lufs(frame: &Frame) -> Option<f64> {
    unsafe {
        let metadata = (*frame.as_ptr()).metadata;
        if metadata.is_null() {
            return None;
        }
        let entry = ffi::av_dict_get(metadata, c"lavfi.r128.I".as_ptr(), std::ptr::null(), 0);
        if entry.is_null() || (*entry).value.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*entry).value)
            .to_string_lossy()
            .parse::<f64>()
            .ok()
    }
}
