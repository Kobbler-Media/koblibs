//! Controllable transcode session with segment-level caching.
//!
//! `TranscodeSession` is the single source of truth for:
//! - Which generations exist and what time ranges they cover
//! - Which segments are on disk in each generation
//! - Whether a seek target can be served from cache
//! - Cache eviction (deleting old generations atomically)
//!
//! The consumer (kobbler) only handles HLS playlist formatting — it asks the
//! session "what segments exist for this time range?" and renders them into
//! `#EXTINF` lines.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::codec::decoder::{DecodeResult, Decoder};
use crate::codec::encoder::{AudioEncoderConfig, EncodeResult, Encoder, VideoEncoderConfig};
use crate::filter::graph::FilterGraph;
use crate::filter::plan::CpuFilterPlan;
use crate::format::input::{InputContext, MediaType};
use crate::format::output::OutputContext;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

// ============================================================================
// Public types
// ============================================================================

/// Configuration for a transcode session.
pub struct SessionConfig {
    pub output_dir: PathBuf,
    pub segment_duration: f64,
    pub duration_seconds: f64,
    pub video_encoder: Option<String>,
    pub audio_encoder: Option<String>,
    pub video_bitrate: Option<i64>,
    pub audio_bitrate: Option<i64>,
    pub scale_width: Option<i32>,
    pub scale_height: Option<i32>,
    pub start_time: Option<f64>,
    pub video_stream_index: Option<u32>,
    pub audio_stream_index: Option<u32>,
    pub subtitle_burn_in: Option<u32>,
    /// Maximum bytes across all generations before eviction kicks in.
    pub max_cache_bytes: u64,
    /// Starting generation number (default 0). Use when restarting a session
    /// mid-seek to continue from the parent session's generation counter.
    pub initial_generation: u32,
    /// Hardware device context for GPU-accelerated decoding (NVDEC/VAAPI).
    /// When set, the video decoder outputs hardware frames (e.g., CUDA format)
    /// which are downloaded to CPU before filtering/encoding.
    ///
    /// Held as a shared `Arc` for the whole session lifetime (it lives in the
    /// transcode thread's frame), so a lazy provider can keep one context shared
    /// across all active sessions and release it only once none remain.
    pub hw_device: Option<crate::hw::device::SharedHwDevice>,
    /// When set, `run_generation` breaks the read-packet loop once a packet's
    /// presentation timestamp reaches this value. Used by overlay generations
    /// (corruption transcode windows) to encode only `[start_time, end_secs]`
    /// of the source rather than all the way to EOF.
    pub end_secs: Option<f64>,
    /// When set, the auto-registered run owns only these timeline slots instead of
    /// every slot from its start to end-of-file. Used together with `end_secs` for
    /// bounded overlay generations so the primary run's segments outside them stay.
    pub overlay_segments: Option<Range<u32>>,
    /// Field-order removal. `Auto` applies `bwdif` only to interlaced sources.
    pub deinterlace: crate::filter::graph::Deinterlace,
    /// Encoder speed preset (`veryfast`, `medium`, …). `None` leaves the
    /// encoder's own default.
    pub preset: Option<String>,
    /// Encoder tuning (`zerolatency`, `film`, …).
    pub tune: Option<String>,
    /// Runs an earlier session left in `output_dir`, from [`TranscodeSession::runs`].
    /// Their segments are served as they are, and `start_time` moves forward past
    /// whatever of them is contiguous from it, so only what was never produced is
    /// transcoded again.
    pub prior_runs: Vec<PriorRun>,
    /// Tone-maps HDR10, HLG and Dolby Vision sources to BT.709 SDR. Dolby Vision
    /// without a compatible base layer is reshaped regardless.
    pub tone_map_hdr: bool,
}

/// A run of segments one generation produced, as [`TranscodeSession::runs`] reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriorRun {
    pub generation: u32,
    pub run_start: u32,
    pub start_secs: f64,
    /// Set for an overlay run, which owns only `[run_start, end_segment)`.
    pub end_segment: Option<u32>,
}

impl SessionConfig {
    pub fn new(output_dir: impl Into<PathBuf>, duration_seconds: f64) -> Self {
        Self {
            output_dir: output_dir.into(),
            segment_duration: 6.0,
            duration_seconds,
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
            max_cache_bytes: 2 * 1024 * 1024 * 1024, // 2 GB default
            initial_generation: 0,
            hw_device: None,
            end_secs: None,
            overlay_segments: None,
            deinterlace: crate::filter::graph::Deinterlace::None,
            preset: None,
            tune: None,
            prior_runs: Vec::new(),
            tone_map_hdr: false,
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
    pub fn subtitle_burn_in(mut self, stream_index: u32) -> Self {
        self.subtitle_burn_in = Some(stream_index);
        self
    }
    #[must_use]
    pub fn max_cache_bytes(mut self, bytes: u64) -> Self {
        self.max_cache_bytes = bytes;
        self
    }
    #[must_use]
    pub fn initial_generation(mut self, generation: u32) -> Self {
        self.initial_generation = generation;
        self
    }
    #[must_use]
    pub fn prior_runs(mut self, runs: Vec<PriorRun>) -> Self {
        self.prior_runs = runs;
        self
    }
    #[must_use]
    pub fn hw_device(mut self, device: crate::hw::device::SharedHwDevice) -> Self {
        self.hw_device = Some(device);
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
    pub fn deinterlace(mut self, mode: crate::filter::graph::Deinterlace) -> Self {
        self.deinterlace = mode;
        self
    }

    #[must_use]
    pub fn tone_map_hdr(mut self, tone_map_hdr: bool) -> Self {
        self.tone_map_hdr = tone_map_hdr;
        self
    }
}

/// A segment on disk, resolved to its file path.
#[derive(Debug, Clone)]
pub struct ResolvedSegment {
    pub generation: u32,
    pub local_index: u32,
    pub seg_path: PathBuf,
    pub init_path: PathBuf,
    pub duration: f64,
}

/// A segment entry for building playlists.
#[derive(Debug, Clone)]
pub struct PlaylistSegment {
    pub generation: u32,
    pub local_index: u32,
    pub duration: f64,
    pub is_generation_boundary: bool,
}

/// Overlay-generation configuration: re-encode a bounded `[start, end)` time
/// range with a different video encoder so the resulting segments paper over
/// source bitstream corruption that crashes browser HW decoders. The primary
/// (copy-mode) generation continues producing segments before and after the
/// overlay window — the overlay's segments are placed into the timeline
/// without disturbing the rest.
///
/// `audio_encoder` should match the primary session's audio mode: if the
/// primary is transcoding audio, the overlay must transcode with the same
/// codec or the codec switch at the discontinuity will be audible. `None`
/// here means "copy audio passthrough", which only matches a primary that's
/// also in audio-copy mode.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Encoder name (e.g. `"libx264"`, `"h264_nvenc"`, `"h264_vaapi"`).
    pub video_encoder: String,
    /// Optional encoder bitrate. `None` lets the encoder pick.
    pub video_bitrate: Option<i64>,
    /// Audio encoder name when the primary is transcoding audio. `None`
    /// passes audio through unchanged (matches primary copy-mode).
    pub audio_encoder: Option<String>,
    /// Optional audio encoder bitrate (bits per second).
    pub audio_bitrate: Option<i64>,
    /// Optional hardware device for accelerated encode/decode in this overlay.
    /// Independent of the parent session's hw_device.
    pub hw_device: Option<crate::hw::device::SharedHwDevice>,
}

/// Result of a seek request.
#[derive(Debug)]
pub enum SeekResult {
    /// Target time is already cached. Serve from existing segments.
    /// `active_generation` is the gen to continue transcoding from
    /// (end of contiguous cached range).
    Cached {
        active_generation: u32,
        restart_from_secs: f64,
    },
    /// New generation created. Segments will appear as transcode progresses.
    NewGeneration { generation: u32 },
}

// ============================================================================
// TranscodeSession
// ============================================================================

/// A running, seekable transcode session with segment-level cache management.
pub struct TranscodeSession {
    cmd_tx: std::sync::mpsc::Sender<SessionCommand>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
    state: Arc<Mutex<SessionState>>,
    output_dir: PathBuf,
    generation: Arc<AtomicU32>,
    segment_duration: f64,
    total_segments: u32,
    duration_seconds: f64,
    /// Pre-computed keyframe timestamps for instant playlist prediction.
    keyframes: Mutex<Option<Vec<f64>>>,
    /// Error message from the transcode thread, if it failed.
    error: Arc<Mutex<Option<String>>>,
    /// Source path, when known. Required by `start_overlay` so an overlay
    /// generation can open a fresh `InputContext` against the same source.
    /// `None` for custom-IO (debrid live) sessions where overlays would need
    /// a separate byte source — those fall back to full-session restart.
    input_path: Option<PathBuf>,
    /// Spawned overlay generations, kept alive so dropping the
    /// `TranscodeSession` waits for any in-flight overlay encodes to exit
    /// — and can signal Cancel through the per-overlay `cmd_tx` so they
    /// stop encoding promptly instead of running to natural EOF.
    overlay_handles: Mutex<Vec<OverlayHandle>>,
}

struct OverlayHandle {
    cmd_tx: std::sync::mpsc::Sender<SessionCommand>,
    join: std::thread::JoinHandle<Result<()>>,
}

enum SessionCommand {
    Seek { position_secs: f64 },
    Cancel,
}

/// Shared state between the transcode thread and the caller.
struct SessionState {
    /// Transcode runs keyed by generation.
    runs: BTreeMap<u32, TranscodeRun>,
    /// Flat segment index: abs_segment → generation + run_start. O(1) lookup.
    segment_index: Vec<Option<SegmentEntry>>,
    /// Max cache size for eviction.
    max_cache_bytes: u64,
    /// Base output directory.
    base_dir: PathBuf,
    /// Generations spawned by `start_overlay` — never evicted, since their
    /// re-encoded segments are the *only* thing covering a known-corrupt
    /// region. If we evicted one and the user seeked back into it, the copy
    /// segments that would replace it would crash the browser HW decoder
    /// again. Memory cost is bounded by the number of corrupt ranges per file.
    overlay_generations: std::collections::HashSet<u32>,
}

#[derive(Debug, Clone)]
struct TranscodeRun {
    generation: u32,
    run_start: u32,
    start_time_secs: f64,
    segment_count: u32,
    estimated_bytes: u64,
    segment_durations: Vec<f64>,
    end_segment: Option<u32>,
    /// A bounded run that has written its whole range. Its claimed slots past the
    /// segments it wrote are then filler: an overlay cut longer than the copy
    /// segments it replaces covers their time in fewer segments.
    complete: bool,
}

impl TranscodeRun {
    fn claims(&self, abs: u32) -> bool {
        self.end_segment
            .is_some_and(|end| abs >= self.run_start && abs < end)
    }
}

impl SessionState {
    fn is_filler(&self, abs: u32) -> bool {
        self.segment_index
            .get(abs as usize)
            .is_some_and(Option::is_none)
            && self.runs.values().any(|r| r.complete && r.claims(abs))
    }
}

#[derive(Debug, Clone, Copy)]
struct SegmentEntry {
    generation: u32,
    run_start: u32,
}

impl TranscodeSession {
    /// Start a new transcode session from a file path.
    pub fn start(input: &Path, config: SessionConfig) -> Result<Self> {
        let mut in_ctx = InputContext::open_path(input)?;
        in_ctx.find_stream_info()?;
        Self::start_from_ctx(in_ctx, Some(input), config)
    }

    /// Start a new transcode session from an already-open InputContext.
    ///
    /// Use this for custom I/O sources (HybridIo, channel-backed, etc.)
    /// where the input isn't a simple file path. Pass `input_path` if
    /// subtitle burn-in is needed (the subtitles filter reads the file directly).
    pub fn start_from_ctx(
        in_ctx: InputContext,
        input_path: Option<&Path>,
        config: SessionConfig,
    ) -> Result<Self> {
        Self::start_from_ctx_with_io(in_ctx, None, input_path, config)
    }

    /// Start from a custom I/O context, taking ownership to prevent use-after-free.
    ///
    /// The `CustomIoContext` must outlive the `InputContext` — this method ensures
    /// that by moving both into the session thread together.
    pub fn start_from_custom_io(
        io: crate::format::io_context::CustomIoContext,
        in_ctx: InputContext,
        input_path: Option<&Path>,
        config: SessionConfig,
    ) -> Result<Self> {
        Self::start_from_ctx_with_io(in_ctx, Some(io), input_path, config)
    }

    fn start_from_ctx_with_io(
        in_ctx: InputContext,
        io: Option<crate::format::io_context::CustomIoContext>,
        input_path: Option<&Path>,
        mut config: SessionConfig,
    ) -> Result<Self> {
        let segment_duration = config.segment_duration;
        let duration_seconds = config.duration_seconds;
        let total_segments =
            ((duration_seconds / segment_duration).ceil() as u32).min(24 * 3600 / 2);
        let output_dir = config.output_dir.clone();

        let state = Arc::new(Mutex::new(SessionState {
            runs: BTreeMap::new(),
            segment_index: vec![None; total_segments as usize],
            max_cache_bytes: config.max_cache_bytes,
            base_dir: output_dir.clone(),
            overlay_generations: std::collections::HashSet::new(),
        }));

        let fully_cached = if config.prior_runs.is_empty() {
            false
        } else {
            let mut s = state.lock().unwrap();
            let finished = adopt_prior_runs(&mut s, &output_dir, &config.prior_runs);
            let from = ((config.start_time.unwrap_or(0.0) / segment_duration).floor() as u32)
                .min(total_segments.saturating_sub(1));
            let end = contiguous_range_end(&s, from);
            if end > from {
                config.start_time = Some((f64::from(end) * segment_duration).min(duration_seconds));
            }
            let ends_on_eof = end
                .checked_sub(1)
                .and_then(|last| s.segment_index.get(last as usize).copied().flatten())
                .is_some_and(|e| {
                    finished.contains(&e.generation)
                        && s.runs
                            .get(&e.generation)
                            .is_some_and(|r| e.run_start + r.segment_count == end)
                });
            end >= total_segments || ends_on_eof
        };

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let generation = Arc::new(AtomicU32::new(config.initial_generation));

        let gen_clone = generation.clone();
        let state_clone = state.clone();
        let path_owned = input_path.map(|p| p.to_owned());
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_clone = error.clone();

        let handle = std::thread::spawn(move || {
            // Keep the CustomIoContext alive for the duration of the session.
            // It MUST outlive in_ctx which references its AVIOContext via raw pointer.
            let _io_guard = io;
            if fully_cached {
                return Ok(());
            }
            let result =
                session_loop_from_ctx(in_ctx, path_owned, config, cmd_rx, gen_clone, state_clone);
            if let Err(ref e) = result {
                eprintln!("[kob-ffmpeg] Transcode session thread error: {e}");
                if let Ok(mut err) = error_clone.lock() {
                    *err = Some(e.to_string());
                }
            }
            result
        });

        Ok(Self {
            cmd_tx,
            handle: Some(handle),
            state,
            output_dir,
            generation,
            segment_duration,
            error,
            total_segments,
            duration_seconds,
            keyframes: Mutex::new(None),
            input_path: input_path.map(|p| p.to_owned()),
            overlay_handles: Mutex::new(Vec::new()),
        })
    }

    /// Seek to a position. Returns immediately with cache info or after
    /// the new generation starts.
    pub fn seek(&self, position_secs: f64) -> Option<SeekResult> {
        // Update segment index from disk before checking cache.
        // This ensures all previously-produced segments are visible.
        self.update_segments();

        // Check if the target time is covered by any existing run.
        // Uses run data directly (not the linear time_to_segment approximation)
        // because variable-length segments can cause the linear method to map
        // to the wrong slot in the segment index, falsely triggering a
        // non-cached seek that creates a new generation and fragments the index.
        if let Ok(state) = self.state.lock() {
            for run in state.runs.values() {
                if run.segment_durations.is_empty() {
                    continue;
                }
                let run_start = run.run_start;
                let run_end_time: f64 =
                    run.start_time_secs + run.segment_durations.iter().sum::<f64>();
                if position_secs >= run.start_time_secs && position_secs < run_end_time {
                    // Target is within this run — find the exact segment
                    let mut t = run.start_time_secs;
                    for (i, &dur) in run.segment_durations.iter().enumerate() {
                        if t + dur > position_secs {
                            let abs_seg = run_start + i as u32;
                            let contiguous_end = contiguous_range_end(&state, abs_seg);
                            let restart_secs = (contiguous_end as f64 * self.segment_duration)
                                .min(self.duration_seconds);
                            return Some(SeekResult::Cached {
                                active_generation: run.generation,
                                restart_from_secs: restart_secs,
                            });
                        }
                        t += dur;
                    }
                }
            }
        }

        // Not cached — bump generation immediately so playlist requests see the
        // new generation right away (not the old one while av_seek_frame is blocking).
        // The session thread will see this generation when it processes the seek.
        let next_gen = self.generation.fetch_add(1, Ordering::Release) + 1;
        if self
            .cmd_tx
            .send(SessionCommand::Seek { position_secs })
            .is_err()
        {
            return None;
        }
        Some(SeekResult::NewGeneration {
            generation: next_gen,
        })
    }

    /// Redirect the transcode thread to a new position unconditionally.
    /// Unlike `seek()`, this always creates a new generation and sends a Seek
    /// command, even if the target position is cached. Use after a cached seek
    /// to redirect the thread to the cache boundary so it fills forward.
    pub fn redirect(&self, position_secs: f64) -> Option<u32> {
        let next_gen = self.generation.fetch_add(1, Ordering::Release) + 1;
        if self
            .cmd_tx
            .send(SessionCommand::Seek { position_secs })
            .is_err()
        {
            return None;
        }
        Some(next_gen)
    }

    /// Start an overlay generation that re-encodes `[time_range.start,
    /// time_range.end)` with a different encoder, leaving copy-mode segments
    /// outside that range intact.
    ///
    /// The window is widened to the boundaries of the primary segments it touches,
    /// found by time from what the primary has written, so the overlay replaces whole
    /// segments: a stream copy cuts them on source keyframes, not on the fixed grid.
    /// Returns the new generation number. The encode runs on a background thread;
    /// segments appear on disk progressively and are picked up by `update_segments`.
    ///
    /// Fails when the session was started from a custom I/O source (e.g.
    /// debrid live) — those don't have a reopenable input path. Caller should
    /// fall back to full-session restart in that case.
    pub fn start_overlay(
        &self,
        time_range: std::ops::Range<f64>,
        overlay: OverlayConfig,
    ) -> Result<u32> {
        let input_path = self.input_path.clone().ok_or_else(|| {
            crate::error::FfmpegError::new(
                ffi::AVERROR_EXTERNAL,
                "start_overlay: session has no reopenable input path (custom I/O source)",
            )
        })?;

        self.update_segments();
        let abs_start = self.time_to_segment_accurate(time_range.start);
        let last = self.time_to_segment_accurate(time_range.end);
        let abs_end_excl = if self.segment_to_time_accurate(last) >= time_range.end {
            last
        } else {
            last + 1
        }
        .max(abs_start + 1);
        let window_start = self.segment_to_time_accurate(abs_start);
        let window_end = self
            .segment_to_time_accurate(abs_end_excl)
            .min(self.duration_seconds);
        if window_end <= window_start {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EXTERNAL,
                "start_overlay: empty time range",
            ));
        }
        let overlay_segment_duration = (abs_start..abs_end_excl)
            .map(|abs| self.segment_to_time_accurate(abs + 1) - self.segment_to_time_accurate(abs))
            .fold(0.0_f64, f64::max);

        let new_gen = self.generation.fetch_add(1, Ordering::Release) + 1;
        let output_dir = self.output_dir.clone();
        let duration_seconds = self.duration_seconds;
        let state = self.state.clone();

        // Mark this generation as overlay-protected so `evict_if_needed`
        // never deletes its gen_dir while it's still referenced by the
        // playlist (re-encoded segments are the only thing covering the
        // corrupt window — losing them re-exposes the bug).
        if let Ok(mut s) = state.lock() {
            s.overlay_generations.insert(new_gen);
        }

        // Per-overlay command channel so Drop can send Cancel to stop the
        // encode promptly (vs running to its natural end_secs).
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

        let join = std::thread::spawn(move || -> Result<()> {
            let mut in_ctx = InputContext::open_path(&input_path)?;
            in_ctx.find_stream_info()?;

            let mut config = SessionConfig::new(output_dir, duration_seconds)
                .segment_duration(overlay_segment_duration)
                .video_encoder(&overlay.video_encoder)
                .initial_generation(new_gen);
            if let Some(br) = overlay.video_bitrate {
                config = config.video_bitrate(br);
            }
            if let Some(ref ae) = overlay.audio_encoder {
                config = config.audio_encoder(ae);
            }
            if let Some(ab) = overlay.audio_bitrate {
                config = config.audio_bitrate(ab);
            }
            if let Some(hw) = overlay.hw_device {
                config = config.hw_device(hw);
            }
            config.start_time = Some(window_start);
            config.end_secs = Some(window_end);
            config.overlay_segments = Some(abs_start..abs_end_excl);

            let local_gen = Arc::new(AtomicU32::new(new_gen));

            let result = session_loop_from_ctx(
                in_ctx,
                Some(input_path.clone()),
                config,
                cmd_rx,
                local_gen,
                state,
            );
            if let Err(ref e) = result {
                eprintln!("[kob-ffmpeg] overlay gen {new_gen} thread error: {e}");
            }
            result
        });

        if let Ok(mut handles) = self.overlay_handles.lock() {
            handles.push(OverlayHandle { cmd_tx, join });
        }
        Ok(new_gen)
    }

    /// Resolve an absolute segment index to its file path.
    pub fn resolve_segment(&self, abs_index: u32) -> Option<ResolvedSegment> {
        let state = self.state.lock().ok()?;
        let entry = (*state.segment_index.get(abs_index as usize)?)?;
        let run = state.runs.get(&entry.generation)?;
        let local_index = abs_index - entry.run_start;
        let gen_dir = state.base_dir.join(format!("gen_{}", entry.generation));
        let duration = run
            .segment_durations
            .get(local_index as usize)
            .copied()
            .unwrap_or(self.segment_duration);
        Some(ResolvedSegment {
            generation: entry.generation,
            local_index,
            seg_path: gen_dir.join(format!("seg_{local_index:05}.m4s")),
            init_path: gen_dir.join("init.mp4"),
            duration,
        })
    }

    /// Get all segments from a starting segment index for playlist building.
    pub fn playlist_segments_from(&self, start_segment: u32) -> Vec<PlaylistSegment> {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut segments = Vec::new();
        let mut last_gen: Option<u32> = None;

        for abs in start_segment..state.segment_index.len() as u32 {
            let Some(entry) = state.segment_index.get(abs as usize).and_then(|e| *e) else {
                if state.is_filler(abs) {
                    continue;
                }
                break;
            };
            let local_index = abs - entry.run_start;
            let duration = state
                .runs
                .get(&entry.generation)
                .and_then(|r| r.segment_durations.get(local_index as usize).copied())
                .unwrap_or(self.segment_duration);
            let is_boundary = last_gen != Some(entry.generation);
            last_gen = Some(entry.generation);

            segments.push(PlaylistSegment {
                generation: entry.generation,
                local_index,
                duration,
                is_generation_boundary: is_boundary,
            });
        }
        segments
    }

    /// Update segment info from disk for ALL generations with runs.
    /// Call this periodically (e.g., on each playlist request) to pick up
    /// newly written segments.
    pub fn update_segments(&self) {
        let generations: Vec<u32> = match self.state.lock() {
            Ok(s) => s.runs.keys().copied().collect(),
            Err(_) => return,
        };

        for generation in generations {
            let Some((durations, bytes)) = read_generation(&self.output_dir, generation) else {
                continue;
            };
            let wrote_range = self
                .output_dir
                .join(format!("gen_{generation}"))
                .join(EOF_MARKER)
                .exists();
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            fill_run(&mut state, generation, durations, bytes, wrote_range);
        }
    }

    /// Evict old generations to stay within cache budget.
    /// Deletes the generation furthest from the current playback position.
    pub fn evict_if_needed(&self, playback_segment: u32) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let active_gen = self.generation.load(Ordering::Acquire);

        loop {
            let total_bytes: u64 = state.runs.values().map(|r| r.estimated_bytes).sum();
            if total_bytes <= state.max_cache_bytes || state.runs.len() <= 1 {
                break;
            }

            // Find run furthest from playback, excluding active and overlay
            // generations (overlays are the only thing covering corrupt
            // windows; evicting one would re-expose the bug on seek-back).
            let to_evict = state
                .runs
                .iter()
                .filter(|(_, r)| r.generation != active_gen)
                .filter(|(_, r)| !state.overlay_generations.contains(&r.generation))
                .filter(|(_, r)| {
                    let end = r.run_start + r.segment_count;
                    !(r.run_start <= playback_segment && end > playback_segment)
                })
                .max_by_key(|(_, r)| {
                    let mid = r.run_start + r.segment_count / 2;
                    mid.abs_diff(playback_segment)
                })
                .map(|(_, r)| (r.run_start, r.generation, r.segment_count));

            let Some((run_start, evict_gen, seg_count)) = to_evict else {
                break;
            };

            // Clear from flat index
            for i in 0..seg_count {
                let abs = run_start + i;
                if let Some(slot) = state.segment_index.get_mut(abs as usize)
                    && slot.is_some_and(|e| e.generation == evict_gen)
                {
                    *slot = None;
                }
            }

            state.runs.remove(&evict_gen);

            // Delete files (non-blocking — best effort)
            let dir = state.base_dir.join(format!("gen_{evict_gen}"));
            let _ = std::fs::remove_dir_all(&dir);

            tracing::info!(
                generation = evict_gen,
                segments = seg_count,
                "Evicted cached generation"
            );
        }
    }

    /// Convert time to segment index (linear approximation).
    pub fn time_to_segment(&self, secs: f64) -> u32 {
        let seg = (secs / self.segment_duration).floor() as u32;
        seg.min(self.total_segments.saturating_sub(1))
    }

    /// Convert segment index to time (linear approximation).
    pub fn segment_to_time(&self, idx: u32) -> f64 {
        idx as f64 * self.segment_duration
    }

    /// Find the segment that contains the given time using actual durations
    /// from FFmpeg output. Uses the runs directly (not the segment index) to
    /// handle sessions that start from a resume position where early segments
    /// are absent. Falls back to linear approximation if no run covers the time.
    pub fn time_to_segment_accurate(&self, secs: f64) -> u32 {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return self.time_to_segment(secs),
        };
        for run in state.runs.values().filter(|r| r.end_segment.is_none()) {
            let mut t = run.start_time_secs;
            if secs < t {
                continue;
            }
            for (i, &dur) in run.segment_durations.iter().enumerate() {
                if t + dur > secs {
                    return run.run_start + i as u32;
                }
                t += dur;
            }
        }
        self.time_to_segment(secs)
    }

    /// Actual start time of a given segment, using real durations from FFmpeg.
    /// Falls back to linear approximation if the segment isn't in any run.
    pub fn segment_to_time_accurate(&self, target: u32) -> f64 {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return self.segment_to_time(target),
        };
        let primaries = || state.runs.values().filter(|r| r.end_segment.is_none());
        let start_of = |r: &TranscodeRun| {
            let local = (target - r.run_start) as usize;
            r.start_time_secs + r.segment_durations.iter().take(local).sum::<f64>()
        };
        if let Some(run) =
            primaries().find(|r| target >= r.run_start && target < r.run_start + r.segment_count)
        {
            return start_of(run);
        }
        if let Some(run) =
            primaries().find(|r| r.segment_count > 0 && target == r.run_start + r.segment_count)
        {
            return start_of(run);
        }
        self.segment_to_time(target)
    }

    /// Store pre-computed keyframe timestamps for playlist prediction.
    pub fn set_keyframes(&self, keyframes: Vec<f64>) {
        if let Ok(mut kf) = self.keyframes.lock() {
            *kf = Some(keyframes);
        }
    }

    /// Generate predicted segments from cached keyframes, if available.
    ///
    /// Uses keyframe positions to predict HLS segment boundaries before the
    /// transcode has actually produced them. Returns playlist entries with
    /// predicted durations for immediate playlist generation.
    pub fn predicted_segments(
        &self,
        current_generation: u32,
        run_start_time: f64,
    ) -> Vec<PlaylistSegment> {
        let keyframes = match self.keyframes.lock() {
            Ok(kf) => kf,
            Err(_) => return Vec::new(),
        };
        let keyframes = match keyframes.as_ref() {
            Some(kf) if kf.len() >= 2 => kf,
            _ => return Vec::new(),
        };

        let relevant: Vec<f64> = keyframes
            .iter()
            .copied()
            .filter(|&kf| kf >= run_start_time)
            .collect();

        if relevant.len() < 2 {
            return Vec::new();
        }

        let boundaries = compute_segment_boundaries(&relevant, self.segment_duration);

        boundaries
            .iter()
            .enumerate()
            .map(|(i, &(_start, duration))| PlaylistSegment {
                generation: current_generation,
                local_index: i as u32,
                is_generation_boundary: false,
                duration,
            })
            .collect()
    }

    /// Contiguous cached segment range from a starting index.
    pub fn contiguous_range_from(&self, start: u32) -> Range<u32> {
        let state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return start..start,
        };
        start..contiguous_range_end(&state, start)
    }

    /// Every run whose segments are still indexed, oldest generation first — what a
    /// later session over the same `output_dir` passes back as
    /// [`SessionConfig::prior_runs`].
    pub fn runs(&self) -> Vec<PriorRun> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let mut runs: Vec<PriorRun> = state
            .runs
            .values()
            .map(|r| PriorRun {
                generation: r.generation,
                run_start: r.run_start,
                start_secs: r.start_time_secs,
                end_segment: r.end_segment,
            })
            .collect();
        runs.sort_by_key(|r| r.generation);
        runs
    }

    /// Current generation number.
    pub fn current_generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// Generation directory path.
    pub fn gen_dir(&self, generation: u32) -> PathBuf {
        self.output_dir.join(format!("gen_{generation}"))
    }

    pub fn cancel(&self) {
        let _ = self.cmd_tx.send(SessionCommand::Cancel);
    }

    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Returns the error message if the transcode thread failed.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    pub fn join(mut self) -> Result<()> {
        if let Some(h) = self.handle.take() {
            h.join().map_err(|_| {
                crate::error::FfmpegError::new(ffi::AVERROR_EXTERNAL, "session panicked")
            })??;
        }
        Ok(())
    }
}

impl Drop for TranscodeSession {
    fn drop(&mut self) {
        // Signal the primary loop first.
        let _ = self.cmd_tx.send(SessionCommand::Cancel);

        // Signal all in-flight overlays before joining anything — Cancel
        // costs nothing if the thread already exited, and parallel signaling
        // means overlays start winding down before we sit blocked on join().
        let overlays = self
            .overlay_handles
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        for ov in &overlays {
            let _ = ov.cmd_tx.send(SessionCommand::Cancel);
        }

        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        for ov in overlays {
            let _ = ov.join.join();
        }
    }
}

/// Left in a generation's directory once it has written the last segment of the file, or
/// of its range for a bounded run. The trailer alone does not say so: a seek or a cancel
/// writes one too.
const EOF_MARKER: &str = "eof";

/// The EXTINF durations FFmpeg has written for a generation, and its bytes on disk.
fn read_generation(output_dir: &Path, generation: u32) -> Option<(Vec<f64>, u64)> {
    let gen_dir = output_dir.join(format!("gen_{generation}"));
    let contents = std::fs::read_to_string(gen_dir.join("stream.m3u8")).ok()?;
    let durations = contents
        .lines()
        .filter_map(|l| {
            l.strip_prefix("#EXTINF:")
                .and_then(|rest| rest.trim_end_matches(',').parse::<f64>().ok())
        })
        .collect();
    let bytes = std::fs::read_dir(&gen_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    Some((durations, bytes))
}

// Replayed oldest first, each run filled to its full count before the next registers:
// that is the order the original session saw them in, so a later run displaces exactly
// what it displaced then.
/// Returns the generations that ran to the end of the file.
fn adopt_prior_runs(
    state: &mut SessionState,
    output_dir: &Path,
    prior: &[PriorRun],
) -> std::collections::HashSet<u32> {
    let mut finished = std::collections::HashSet::new();
    let mut prior = prior.to_vec();
    prior.sort_by_key(|r| r.generation);
    for run in prior {
        let Some((durations, bytes)) = read_generation(output_dir, run.generation) else {
            continue;
        };
        let wrote_range = output_dir
            .join(format!("gen_{}", run.generation))
            .join(EOF_MARKER)
            .exists();
        register_run(
            state,
            run.generation,
            run.run_start,
            run.end_segment,
            run.start_secs,
        );
        if run.end_segment.is_some() {
            state.overlay_generations.insert(run.generation);
        } else if wrote_range {
            finished.insert(run.generation);
        }
        fill_run(state, run.generation, durations, bytes, wrote_range);
    }
    finished
}

fn fill_run(
    state: &mut SessionState,
    generation: u32,
    durations: Vec<f64>,
    bytes: u64,
    wrote_range: bool,
) {
    let Some(run) = state.runs.get_mut(&generation) else {
        return;
    };
    let old_count = run.segment_count;
    let count = durations.len() as u32;
    run.segment_count = count;
    run.estimated_bytes = bytes;
    run.segment_durations = durations;
    run.complete = run.end_segment.is_some() && wrote_range;
    let run_start = run.run_start;
    let bounded = run.end_segment.is_some();
    let limit = run.end_segment.unwrap_or(u32::MAX);
    for abs in (run_start + old_count)..(run_start + count).min(limit) {
        let displaced = state.runs.values().any(|other| {
            other.generation != generation
                && other.claims(abs)
                && (!bounded || other.generation > generation)
        });
        if displaced {
            continue;
        }
        // A copy run cuts on source keyframes, so it can need more slots than the
        // fixed grid sized the index for.
        if abs as usize >= state.segment_index.len() {
            state.segment_index.resize(abs as usize + 1, None);
        }
        state.segment_index[abs as usize] = Some(SegmentEntry {
            generation,
            run_start,
        });
    }
}

fn contiguous_range_end(state: &SessionState, start: u32) -> u32 {
    let mut end = start;
    while state
        .segment_index
        .get(end as usize)
        .is_some_and(|e| e.is_some() || state.is_filler(end))
    {
        end += 1;
    }
    end
}

/// Compute predicted segment boundaries from keyframes.
///
/// Mimics FFmpeg's HLS muxer: splits at the first keyframe at or beyond
/// `current_start + hls_time`. Returns `(start_time, duration)` pairs.
pub fn compute_segment_boundaries(keyframes: &[f64], hls_time: f64) -> Vec<(f64, f64)> {
    if keyframes.is_empty() {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut seg_start = keyframes[0];

    for &kf in &keyframes[1..] {
        if kf >= seg_start + hls_time {
            segments.push((seg_start, kf - seg_start));
            seg_start = kf;
        }
    }

    // Final segment: from last split to the last keyframe
    if let Some(&last) = keyframes.last()
        && last > seg_start
    {
        segments.push((seg_start, last - seg_start));
    }

    segments
}

// ============================================================================
// Session loop + generation runner (transcode internals)
// ============================================================================

fn session_loop_from_ctx(
    mut in_ctx: InputContext,
    input_path: Option<PathBuf>,
    config: SessionConfig,
    cmd_rx: std::sync::mpsc::Receiver<SessionCommand>,
    generation: Arc<AtomicU32>,
    state: Arc<Mutex<SessionState>>,
) -> Result<()> {
    crate::init();

    let video_idx = in_ctx
        .validate_stream_type(config.video_stream_index, MediaType::Video)
        .or_else(|| in_ctx.find_best_stream(MediaType::Video));
    let audio_idx = in_ctx
        .validate_stream_type(config.audio_stream_index, MediaType::Audio)
        .or_else(|| in_ctx.find_best_stream(MediaType::Audio));
    if video_idx.is_none() && audio_idx.is_none() {
        return Err(crate::error::FfmpegError::new(
            ffi::AVERROR_STREAM_NOT_FOUND,
            "no streams",
        ));
    }

    let video_copy = config.video_encoder.is_none();
    let audio_copy = config.audio_encoder.is_none();
    // GENPTS, DISCARD_CORRUPT, SORT_DTS are set globally by InputContext::configure_lenient_demuxing

    let mut video_decoder = None;
    let mut audio_decoder = None;
    if let Some(v) = video_idx
        && !video_copy
    {
        let stream = in_ctx.stream(v).unwrap();
        let dec = if let Some(ref hw) = config.hw_device {
            stream.decoder_hw(hw)?
        } else {
            stream.decoder()?
        };
        video_decoder = Some(dec);
    }
    if let Some(a) = audio_idx
        && !audio_copy
    {
        audio_decoder = Some(in_ctx.stream(a).unwrap().decoder()?);
    }

    let start_secs = config.start_time.unwrap_or(0.0);
    if start_secs > 0.0 {
        let ts = (start_secs * ffi::AV_TIME_BASE as f64) as i64;
        in_ctx.seek(-1, ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
        if let Some(ref mut d) = video_decoder {
            d.flush();
        }
        if let Some(ref mut d) = audio_decoder {
            d.flush();
        }
    }

    let initial_gen = config.initial_generation;
    generation.store(initial_gen, Ordering::Release);
    let seg_dur = config.segment_duration;
    {
        let mut s = state.lock().unwrap();
        match config.overlay_segments.clone() {
            Some(slots) => register_run(
                &mut s,
                initial_gen,
                slots.start,
                Some(slots.end),
                start_secs,
            ),
            None => {
                let abs_start = ((start_secs / seg_dur).floor() as u32).min(24 * 3600 / 2 - 1);
                register_run(&mut s, initial_gen, abs_start, None, start_secs);
            }
        }
    }
    // A run starts on the keyframe the seek landed on, before the time it asked for;
    // mapping times to its segments needs where it really starts.
    let mut record_run_start = |secs: f64| {
        let current = generation.load(Ordering::Acquire);
        if let Ok(mut s) = state.lock()
            && let Some(run) = s.runs.get_mut(&current)
        {
            run.start_time_secs = secs;
        }
    };

    loop {
        let current_gen = generation.load(Ordering::Acquire);
        let gen_dir = config.output_dir.join(format!("gen_{current_gen}"));

        match run_generation(
            &mut in_ctx,
            input_path.as_deref(),
            &config,
            &gen_dir,
            video_idx,
            audio_idx,
            video_copy,
            audio_copy,
            &mut video_decoder,
            &mut audio_decoder,
            &cmd_rx,
            &mut record_run_start,
        )? {
            GenResult::Completed | GenResult::Cancelled => break,
            GenResult::SeekTo(pos) => {
                let ts = (pos * ffi::AV_TIME_BASE as f64) as i64;
                in_ctx.seek(-1, ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
                if let Some(ref mut d) = video_decoder {
                    d.flush();
                }
                if let Some(ref mut d) = audio_decoder {
                    d.flush();
                }

                // Generation was already bumped by seek() on the caller side
                // so that playlist requests see the new gen immediately.
                let new_gen = generation.load(Ordering::Acquire);

                let abs_start = ((pos / seg_dur).floor() as u32).min(24 * 3600 / 2 - 1);
                let mut s = state.lock().unwrap();
                register_run(&mut s, new_gen, abs_start, None, pos);
            }
        }
    }
    Ok(())
}

/// Register a new run owning a slice of the absolute segment timeline.
///
/// `end_exclusive`:
/// - `None` — primary-run / seek behaviour: the new run takes ownership of all
///   segments from `abs_start` to end-of-file. Existing entries from
///   `abs_start` forward are wiped, and any runs starting at or after
///   `abs_start` are removed.
/// - `Some(end)` — overlay-run behaviour: the new run claims only the slice
///   `[abs_start, end)`. Index entries inside that slice are cleared and any
///   overlay wholly inside it is replaced, but no primary run is removed: one
///   that starts inside the slice is still writing the segments after it.
fn register_run(
    state: &mut SessionState,
    generation: u32,
    abs_start: u32,
    end_exclusive: Option<u32>,
    start_time: f64,
) {
    let end = end_exclusive
        .map(|e| (e as usize).min(state.segment_index.len()))
        .unwrap_or(state.segment_index.len());
    let start = (abs_start as usize).min(end);

    for slot in state.segment_index[start..end].iter_mut() {
        *slot = None;
    }

    state.runs.retain(|_, r| match end_exclusive {
        None => r.run_start < abs_start,
        Some(end_seg) => !(r.end_segment.is_some_and(|e| e <= end_seg) && r.run_start >= abs_start),
    });

    state.runs.insert(
        generation,
        TranscodeRun {
            generation,
            run_start: abs_start,
            start_time_secs: start_time,
            segment_count: 0,
            estimated_bytes: 0,
            segment_durations: Vec::new(),
            end_segment: end_exclusive,
            complete: false,
        },
    );
}

enum GenResult {
    Completed,
    Cancelled,
    SeekTo(f64),
}

// Deep video+audio encode/filter pipeline — collapsing the outer `if let`
// chains here would need a risky reflow of every match arm body.
#[allow(clippy::too_many_arguments, clippy::collapsible_if)]
fn run_generation(
    in_ctx: &mut InputContext,
    input_path: Option<&Path>,
    config: &SessionConfig,
    gen_dir: &Path,
    video_idx: Option<u32>,
    audio_idx: Option<u32>,
    video_copy: bool,
    audio_copy: bool,
    video_decoder: &mut Option<Decoder>,
    audio_decoder: &mut Option<Decoder>,
    cmd_rx: &std::sync::mpsc::Receiver<SessionCommand>,
    record_run_start: &mut dyn FnMut(f64),
) -> Result<GenResult> {
    std::fs::create_dir_all(gen_dir).map_err(|e| {
        crate::error::FfmpegError::new(ffi::AVERROR_EXTERNAL, format!("mkdir: {e}"))
    })?;

    let src_interlaced = video_idx.is_some_and(|v| unsafe {
        crate::interlace::codec_parameters_interlaced(in_ctx.stream(v).unwrap().codec_parameters())
    });
    let deint_filter = config.deinterlace.filter_for(src_interlaced);
    let reshape = video_idx.is_some_and(|v| unsafe {
        crate::sdr::stream_needs_reshaping(
            in_ctx.stream(v).unwrap().codec_parameters(),
            config.tone_map_hdr,
        )
    });

    let playlist_path = gen_dir.join("stream.m3u8");
    let seg_pattern = gen_dir.join("seg_%05d.m4s");
    let mut out_ctx = OutputContext::create_with_format(playlist_path.to_str().unwrap(), "hls")?;
    out_ctx.set_option("hls_time", &config.segment_duration.to_string())?;
    out_ctx.set_option("hls_segment_type", "fmp4")?;
    out_ctx.set_option("hls_fmp4_init_filename", "init.mp4")?;
    out_ctx.set_option("hls_segment_filename", seg_pattern.to_str().unwrap())?;
    out_ctx.set_option("hls_flags", "independent_segments+append_list")?;
    out_ctx.set_option("hls_list_size", "0")?;
    out_ctx.set_option("hls_segment_options", "movflags=+frag_discont")?;

    let mut buffered_packets: std::collections::VecDeque<Packet> =
        std::collections::VecDeque::new();
    let mut video_encoder = None;
    let mut encoder_pixel_format = crate::types::PixelFormat::YUV420P;
    if let Some(v_idx) = video_idx {
        let stream = in_ctx.stream(v_idx).unwrap();
        if video_copy {
            let osi = out_ctx.add_stream_copy(&stream)?;
            crate::hevc_config::prepare_copied_hevc(
                in_ctx,
                v_idx,
                &mut out_ctx,
                osi,
                &mut buffered_packets,
            )?;
        } else if let Some(dec) = video_decoder.as_ref() {
            let cn = config.video_encoder.as_deref().unwrap();

            // Zero-copy GPU path: requires hw_device AND no subtitles.
            // For variants with scaling, scale_cuda is created lazily after the
            // first frame when decoder's hw_frames_ctx becomes available.
            let needs_subtitle = config.subtitle_burn_in.is_some();
            // Deinterlace is a CPU-only filter here; the deferred GPU path emits
            // scale_* and nothing else, so zero-copy has to be given up for it.
            let use_zero_copy =
                config.hw_device.is_some() && !needs_subtitle && deint_filter.is_none() && !reshape;

            // Auto-rotate is SW-only — CLI also skips it on HW paths because
            // there's no good transpose_cuda/vaapi without an extra hwdownload
            // round-trip. Phone clips on a HW pipeline stay in sensor orientation
            // (matching CLI behaviour); revisit if a real use case appears.
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
            let target_w = config.scale_width.unwrap_or(src_post_rot_w);
            let target_h = config.scale_height.unwrap_or(src_post_rot_h);

            let pix_fmt = if config.hw_device.is_some() {
                crate::types::PixelFormat::NV12
            } else {
                // Always output 8-bit YUV420P on CPU path — browsers can't
                // decode 10-bit H.264 High 10 profile properly (banding in darks).
                // A format filter handles the actual pixel conversion with dithering.
                crate::types::PixelFormat::YUV420P
            };
            encoder_pixel_format = pix_fmt;
            let enc_hw_frames = if use_zero_copy {
                let hw = config.hw_device.as_ref().unwrap();
                Some(hw.create_frames_ctx(target_w, target_h)?)
            } else {
                None
            };
            let enc = Encoder::video(VideoEncoderConfig {
                codec_name: cn.to_string(),
                width: target_w,
                height: target_h,
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
                sample_aspect_ratio: None,
                frame_rate: crate::codec::encoder::output_frame_rate(
                    stream.r_frame_rate(),
                    deint_filter.is_some(),
                ),
                color: if reshape {
                    crate::sdr::sdr_color()
                } else {
                    unsafe {
                        crate::filter::graph::VideoColorInfo::from_codec_parameters(
                            stream.codec_parameters(),
                        )
                    }
                },
                options: config.encoder_options(),
            })?;
            // Encoder took its own ref — free ours
            if let Some(mut hf) = enc_hw_frames {
                unsafe { ffi::av_buffer_unref(&mut hf) };
            }

            let osi = out_ctx.add_stream_from_encoder(&enc)?;
            unsafe {
                let s = *ffi::kff_fmt_streams(out_ctx.as_ptr()).add(osi as usize);
                let _out_tb = ffi::kff_stream_time_base(s);
            }
            if cn.contains("hevc") || cn.contains("x265") {
                unsafe {
                    let s = *ffi::kff_fmt_streams(out_ctx.as_ptr()).add(osi as usize);
                    (*ffi::kff_stream_codecpar(s)).codec_tag = ffi::mktag(b'h', b'v', b'c', b'1');
                }
            }
            video_encoder = Some(enc);
        }
    }

    // Zero-copy flag matches encoder config: hw_device + no subtitles.
    let needs_subtitle = config.subtitle_burn_in.is_some();
    let zero_copy =
        config.hw_device.is_some() && !needs_subtitle && deint_filter.is_none() && !reshape;

    let mut video_filter: Option<FilterGraph> = None;
    let mut cpu_filter_plan: Option<CpuFilterPlan> = None;
    // Deferred HW scale filter: (src_w, src_h, dst_w, dst_h, time_base, hw_type)
    // Created lazily after first frame when decoder's hw_frames_ctx is available.
    let hw_type = config
        .hw_device
        .as_ref()
        .map(|d| d.device_type())
        .unwrap_or(crate::hw::device::HwAccelType::None);
    let mut deferred_hw_scale: Option<(i32, i32, i32, i32, ffi::AVRational)> = None;
    if let (Some(dec), Some(v_idx)) = (video_decoder.as_ref(), video_idx) {
        // Re-derive rotation here (HW path keeps it None — same rule as the encoder
        // setup block above). Filter chain dimensions are post-rotation.
        let rotation = if config.hw_device.is_none() {
            unsafe {
                crate::filter::graph::AutoRotate::from_codec_parameters(
                    in_ctx.stream(v_idx).unwrap().codec_parameters(),
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
            // Always defer a GPU filter in zero-copy mode: scale_cuda handles
            // both resolution changes AND pixel format conversion (e.g. P010→NV12
            // for 10-bit HEVC sources). Without this, 10-bit decoded frames go
            // directly to the encoder whose hw_frames_ctx expects NV12, causing
            // "Could not register an input HW frame" on NVENC.
            {
                deferred_hw_scale = Some((
                    dec.width(),
                    dec.height(),
                    target_w,
                    target_h,
                    in_ctx.stream(v_idx).unwrap().time_base(),
                ));
            }
        } else {
            // CPU filter path (Phase 1 hwdownload or software)
            let mut filters: Vec<String> = Vec::new();
            // Auto-rotate first so subtitle overlay and scale operate on the
            // intended display orientation. CLI does the same in
            // configure_input_video_filter (autorotate before user filters).
            if let Some(r) = rotation {
                filters.push(r.as_filter_chain());
            }
            if let Some(d) = deint_filter {
                filters.push(d.to_string());
            }
            let resize_at = filters.len();
            if let Some(sub_idx) = config.subtitle_burn_in
                && let Some(ps) = input_path.and_then(|p| p.to_str())
            {
                let escaped = ps.replace('\\', "\\\\").replace('\'', "'\\''");
                let si = in_ctx
                    .streams()
                    .filter(|s| s.codec_type() == MediaType::Subtitle)
                    .position(|s| s.index() == sub_idx as i32)
                    .unwrap_or(0);
                filters.push(format!("subtitles='{escaped}':si={si}"));
            }
            let scale = needs_scale.then(|| format!("scale={target_w}:{target_h}"));
            let mut untoned_chain = None;
            if reshape {
                let mut untoned = filters.clone();
                untoned.splice(resize_at..resize_at, scale);
                untoned_chain = Some(untoned.join(","));
                filters.insert(
                    resize_at,
                    crate::sdr::sdr_filter(target_w, target_h, &encoder_pixel_format.name()),
                );
            } else if let Some(scale) = scale {
                filters.insert(resize_at, scale);
            }
            let stream = in_ctx.stream(v_idx).unwrap();
            cpu_filter_plan = Some(CpuFilterPlan {
                chain: filters.join(","),
                time_base: stream.time_base(),
                color: unsafe {
                    crate::filter::graph::VideoColorInfo::from_codec_parameters(
                        stream.codec_parameters(),
                    )
                },
                target: encoder_pixel_format,
                sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
                untoned_chain,
            });
        }
    }

    let mut audio_encoder = None;
    let mut audio_filter: Option<FilterGraph> = None;
    let mut audio_filter_state: Option<crate::audio::AudioGraphState> = None;
    let mut audio_filter_str: Option<String> = None;
    let mut audio_filter_tb = ffi::AVRational { num: 0, den: 1 };
    let mut buffered_audio_frames: std::collections::VecDeque<Frame> =
        std::collections::VecDeque::new();
    let mut reached_eof = false;
    let mut reached_end = false;
    let mut run_start_recorded = false;
    if let Some(a_idx) = audio_idx {
        if audio_copy {
            let stream = in_ctx.stream(a_idx).unwrap();
            out_ctx.add_stream_copy(&stream)?;
        } else if let Some(dec) = audio_decoder.as_mut() {
            let stream_tb = in_ctx.stream(a_idx).unwrap().time_base();
            let cn = config.audio_encoder.as_deref().unwrap();
            let effective_sample_rate = crate::audio::probe_audio_sample_rate(
                in_ctx,
                a_idx,
                dec,
                &mut buffered_packets,
                &mut buffered_audio_frames,
            )?;
            // Each audio encoder accepts a specific sample format (e.g. native
            // `aac` → fltp, `libfdk_aac` → s16, `libmp3lame` → s16p). Query the
            // codec's preferred format and use it both for the encoder context
            // and the upstream aformat filter — mismatch makes avcodec_open2
            // return EINVAL ("Invalid argument").
            let sample_format = crate::codec::encoder::default_audio_sample_format(cn)?;
            let enc = Encoder::audio(AudioEncoderConfig {
                codec_name: cn.to_string(),
                sample_rate: effective_sample_rate,
                channel_layout: crate::types::ChannelLayout::stereo(),
                sample_format,
                bit_rate: config.audio_bitrate,
                time_base: crate::types::TimeBase::new(1, effective_sample_rate),
                global_header: out_ctx.requires_global_header(),
            })?;
            // Audio filter is created lazily from the first decoded frame's actual
            // properties, not from the decoder context (AC3 channel layout mismatch).
            audio_filter_str = Some(format!(
                "aformat=sample_fmts={}:sample_rates={}:channel_layouts=stereo,asetnsamples=n={}:p=0",
                sample_format.name(),
                effective_sample_rate,
                enc.frame_size().max(1024),
            ));
            audio_filter_tb = stream_tb;
            out_ctx.add_stream_from_encoder(&enc)?;
            audio_encoder = Some(enc);
        }
    }

    out_ctx.write_header()?;
    // Get the ACTUAL stream timebase after write_header — the muxer may override it.
    // Encoded packets must be rescaled from encoder timebase to this.
    let video_out_tb = unsafe {
        let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
        ffi::kff_stream_time_base(*streams.add(0))
    };
    let audio_out_stream_idx = if video_idx.is_some() { 1 } else { 0 };
    let audio_writer = audio_encoder.as_ref().map(|e| {
        let out_tb = unsafe {
            let streams = ffi::kff_fmt_streams(out_ctx.as_ptr());
            ffi::kff_stream_time_base(*streams.add(audio_out_stream_idx as usize))
        };
        crate::audio::EncodedAudioWriter::new(audio_out_stream_idx, e.time_base(), out_tb)
    });
    let mut pkt = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut filtered = Frame::new()?;
    let mut out_pkt = Packet::new()?;

    // Drain frames captured during sample-rate pre-roll into filter+encoder.
    if let (Some(enc), Some(aw)) = (audio_encoder.as_mut(), audio_writer.as_ref()) {
        while let Some(mut buf_frame) = buffered_audio_frames.pop_front() {
            if let Some(ref fs) = audio_filter_str
                && crate::audio::AudioGraphState::needs_rebuild(&audio_filter_state, &buf_frame)
            {
                if let Some(ref mut af) = audio_filter {
                    af.push_eof()?;
                    aw.encode_filtered(enc, af, &mut filtered, &mut out_ctx, &mut out_pkt)?;
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
                aw.encode_filtered(enc, af, &mut filtered, &mut out_ctx, &mut out_pkt)?;
            } else {
                aw.encode(
                    enc,
                    &mut buf_frame,
                    audio_filter_tb,
                    &mut out_ctx,
                    &mut out_pkt,
                )?;
            }
        }
    }

    loop {
        match cmd_rx.try_recv() {
            Ok(SessionCommand::Cancel) => {
                flush_all(
                    &mut out_ctx,
                    video_decoder,
                    &mut video_encoder,
                    &mut video_filter,
                    cpu_filter_plan.as_ref(),
                    audio_decoder,
                    &mut audio_encoder,
                    &mut audio_filter,
                    &mut audio_filter_state,
                    audio_filter_str.as_deref(),
                    audio_filter_tb,
                    audio_writer.as_ref(),
                    &mut frame,
                    &mut filtered,
                    &mut out_pkt,
                    zero_copy,
                    video_out_tb,
                )?;
                return Ok(GenResult::Cancelled);
            }
            Ok(SessionCommand::Seek { position_secs }) => {
                flush_all(
                    &mut out_ctx,
                    video_decoder,
                    &mut video_encoder,
                    &mut video_filter,
                    cpu_filter_plan.as_ref(),
                    audio_decoder,
                    &mut audio_encoder,
                    &mut audio_filter,
                    &mut audio_filter_state,
                    audio_filter_str.as_deref(),
                    audio_filter_tb,
                    audio_writer.as_ref(),
                    &mut frame,
                    &mut filtered,
                    &mut out_pkt,
                    zero_copy,
                    video_out_tb,
                )?;
                return Ok(GenResult::SeekTo(position_secs));
            }
            Err(_) => {}
        }

        let have_packet = if let Some(buffered) = buffered_packets.pop_front() {
            pkt = buffered;
            true
        } else {
            in_ctx.read_packet(&mut pkt)?
        };
        if !have_packet {
            reached_eof = true;
            break;
        }
        let ps = pkt.stream_index();

        if ps >= 0 && Some(ps as u32) == video_idx && pkt.pts() != ffi::AV_NOPTS_VALUE {
            let tb = in_ctx.stream(ps as u32).unwrap().time_base();
            let secs = pkt.pts() as f64 * tb.num as f64 / tb.den as f64;
            if !run_start_recorded {
                record_run_start(secs);
                run_start_recorded = true;
            }
            // Audio is allowed to lag slightly behind so the encoder has paired
            // samples to flush.
            if config.end_secs.is_some_and(|end| secs >= end) {
                reached_end = true;
                break;
            }
        }

        if let Some(v) = video_idx {
            if ps == v as i32 {
                if video_copy {
                    let tb = in_ctx.stream(v).unwrap().time_base();
                    out_ctx.write_packet_rescaled(&mut pkt, 0, tb)?;
                } else if let (Some(dec), Some(enc)) = (video_decoder.as_mut(), &mut video_encoder)
                {
                    dec.send_packet(&pkt)?;
                    // Lazy scale_cuda filter creation: after the first packet,
                    // the decoder's hw_frames_ctx should be populated by NVDEC.
                    if let Some((src_w, src_h, dst_w, dst_h, tb)) = deferred_hw_scale.take() {
                        if let Some(dec_hw_frames) = dec.hw_frames_ctx() {
                            let hw_pix_fmt = match hw_type {
                                crate::hw::device::HwAccelType::Cuda => ffi::AV_PIX_FMT_CUDA,
                                crate::hw::device::HwAccelType::Vaapi => ffi::AV_PIX_FMT_VAAPI,
                                _ => ffi::AV_PIX_FMT_NV12,
                            };
                            let params = crate::filter::graph::VideoFilterParams {
                                width: src_w,
                                height: src_h,
                                pixel_format: hw_pix_fmt,
                                time_base: tb,
                                sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
                                hw_frames_ctx: Some(dec_hw_frames),
                                color: unsafe {
                                    crate::filter::graph::VideoColorInfo::from_codec_parameters(
                                        in_ctx.stream(v).unwrap().codec_parameters(),
                                    )
                                },
                                side_data_frame: std::ptr::null(),
                            };
                            let filter_str = match hw_type {
                                crate::hw::device::HwAccelType::Cuda => {
                                    // format=nv12 ensures 10-bit P010 sources get
                                    // converted to 8-bit NV12 for the encoder
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
                            // Put it back to retry on next packet
                            deferred_hw_scale = Some((src_w, src_h, dst_w, dst_h, tb));
                        }
                    }
                    drain_video(
                        dec,
                        enc,
                        &mut video_filter,
                        cpu_filter_plan.as_ref(),
                        &mut out_ctx,
                        &mut frame,
                        &mut filtered,
                        &mut out_pkt,
                        zero_copy,
                        video_out_tb,
                    )?;
                }
            }
        }
        if let Some(a) = audio_idx {
            if ps == a as i32 {
                let ai = if video_idx.is_some() { 1 } else { 0 };
                if audio_copy {
                    let tb = in_ctx.stream(a).unwrap().time_base();
                    out_ctx.write_packet_rescaled(&mut pkt, ai, tb)?;
                } else if let (Some(dec), Some(enc), Some(aw)) = (
                    audio_decoder.as_mut(),
                    &mut audio_encoder,
                    audio_writer.as_ref(),
                ) {
                    dec.send_packet(&pkt)?;
                    drain_audio(
                        dec,
                        enc,
                        &mut audio_filter,
                        &mut audio_filter_state,
                        audio_filter_str.as_deref(),
                        audio_filter_tb,
                        aw,
                        &mut out_ctx,
                        &mut frame,
                        &mut filtered,
                        &mut out_pkt,
                    )?;
                }
            }
        }
    }

    flush_all(
        &mut out_ctx,
        video_decoder,
        &mut video_encoder,
        &mut video_filter,
        cpu_filter_plan.as_ref(),
        audio_decoder,
        &mut audio_encoder,
        &mut audio_filter,
        &mut audio_filter_state,
        audio_filter_str.as_deref(),
        audio_filter_tb,
        audio_writer.as_ref(),
        &mut frame,
        &mut filtered,
        &mut out_pkt,
        zero_copy,
        video_out_tb,
    )?;
    if reached_eof || (reached_end && config.overlay_segments.is_some()) {
        std::fs::write(gen_dir.join(EOF_MARKER), b"").map_err(|e| {
            crate::error::FfmpegError::new(ffi::AVERROR_EXTERNAL, format!("eof marker: {e}"))
        })?;
    }
    Ok(GenResult::Completed)
}

#[allow(clippy::too_many_arguments)]
fn drain_video(
    dec: &mut Decoder,
    enc: &mut Encoder,
    filter: &mut Option<FilterGraph>,
    plan: Option<&CpuFilterPlan>,
    out: &mut OutputContext,
    frame: &mut Frame,
    filt: &mut Frame,
    pkt: &mut Packet,
    zero_copy: bool,
    out_tb: ffi::AVRational,
) -> Result<()> {
    let enc_tb = enc.time_base();
    let mut sw_frame = Frame::new()?;
    loop {
        match dec.receive_frame(frame)? {
            DecodeResult::Frame => {
                let src: &mut Frame = if !zero_copy && frame.is_hw_format() {
                    crate::hw::frames::HwFramesContext::download(frame, &mut sw_frame)?;
                    sw_frame.set_pts(frame.pts());
                    &mut sw_frame
                } else {
                    &mut *frame
                };
                if filter.is_none()
                    && let Some(plan) = plan
                {
                    *filter = Some(plan.build(src)?);
                }
                if let Some(f) = filter.as_mut() {
                    f.push_frame(src)?;
                    let filter_tb = f.output_time_base();
                    while f.pull_frame(filt)? {
                        unsafe {
                            let ptr = filt.as_mut_ptr();
                            (*ptr).pts = ffi::av_rescale_q((*ptr).pts, filter_tb, enc_tb);
                        }
                        enc.send_frame(filt)?;
                        while let EncodeResult::Packet = enc.receive_packet(pkt)? {
                            unsafe {
                                (*pkt.as_mut_ptr()).stream_index = 0;
                                ffi::av_packet_rescale_ts(pkt.as_mut_ptr(), enc_tb, out_tb);
                            }
                            out.write_packet_interleaved(pkt)?;
                        }
                        filt.unref();
                    }
                } else {
                    enc.send_frame(src)?;
                    while let EncodeResult::Packet = enc.receive_packet(pkt)? {
                        unsafe {
                            (*pkt.as_mut_ptr()).stream_index = 0;
                            ffi::av_packet_rescale_ts(pkt.as_mut_ptr(), enc_tb, out_tb);
                        }
                        out.write_packet_interleaved(pkt)?;
                    }
                }
                sw_frame.unref();
            }
            DecodeResult::NeedsInput | DecodeResult::Eof => break,
        }
    }
    Ok(())
}

// Audio filter-rebuild inside a decode loop — collapsing the outer `if let`
// chains here would need a risky reflow of the inner while/for bodies.
#[allow(clippy::too_many_arguments, clippy::collapsible_if)]
fn drain_audio(
    dec: &mut Decoder,
    enc: &mut Encoder,
    filter: &mut Option<FilterGraph>,
    filter_state: &mut Option<crate::audio::AudioGraphState>,
    filter_str: Option<&str>,
    filter_tb: ffi::AVRational,
    aw: &crate::audio::EncodedAudioWriter,
    out: &mut OutputContext,
    frame: &mut Frame,
    filt: &mut Frame,
    pkt: &mut Packet,
) -> Result<()> {
    loop {
        match dec.receive_frame(frame)? {
            DecodeResult::Frame => {
                // Rebuild audio filter if frame properties changed (AC3 layout mismatch)
                if let Some(fs) = filter_str {
                    if crate::audio::AudioGraphState::needs_rebuild(filter_state, frame) {
                        if let Some(f) = filter {
                            f.push_eof()?;
                            aw.encode_filtered(enc, f, filt, out, pkt)?;
                        }
                        crate::audio::AudioGraphState::rebuild_filter_graph(
                            filter,
                            filter_state,
                            frame,
                            filter_tb,
                            fs,
                        )?;
                    }
                }
                if let Some(f) = filter.as_mut() {
                    f.push_frame(frame)?;
                    aw.encode_filtered(enc, f, filt, out, pkt)?;
                } else {
                    aw.encode(enc, frame, filter_tb, out, pkt)?;
                }
            }
            DecodeResult::NeedsInput | DecodeResult::Eof => break,
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn flush_all(
    out: &mut OutputContext,
    vd: &mut Option<Decoder>,
    ve: &mut Option<Encoder>,
    vf: &mut Option<FilterGraph>,
    vf_plan: Option<&CpuFilterPlan>,
    ad: &mut Option<Decoder>,
    ae: &mut Option<Encoder>,
    af: &mut Option<FilterGraph>,
    af_state: &mut Option<crate::audio::AudioGraphState>,
    af_str: Option<&str>,
    af_tb: ffi::AVRational,
    audio_writer: Option<&crate::audio::EncodedAudioWriter>,
    frame: &mut Frame,
    filt: &mut Frame,
    pkt: &mut Packet,
    zero_copy: bool,
    video_out_tb: ffi::AVRational,
) -> Result<()> {
    if let (Some(d), Some(e)) = (vd.as_mut(), ve.as_mut()) {
        let enc_tb = e.time_base();
        d.send_eof()?;
        drain_video(
            d,
            e,
            vf,
            vf_plan,
            out,
            frame,
            filt,
            pkt,
            zero_copy,
            video_out_tb,
        )?;
        if let Some(f) = vf.as_mut() {
            f.push_eof()?;
            let ftb = f.output_time_base();
            while f.pull_frame(filt)? {
                unsafe {
                    let p = filt.as_mut_ptr();
                    (*p).pts = ffi::av_rescale_q((*p).pts, ftb, enc_tb);
                }
                e.send_frame(filt)?;
                while let EncodeResult::Packet = e.receive_packet(pkt)? {
                    unsafe {
                        (*pkt.as_mut_ptr()).stream_index = 0;
                        ffi::av_packet_rescale_ts(pkt.as_mut_ptr(), enc_tb, video_out_tb);
                    }
                    out.write_packet_interleaved(pkt)?;
                }
                filt.unref();
            }
        }
        e.send_eof()?;
        while let EncodeResult::Packet = e.receive_packet(pkt)? {
            unsafe {
                (*pkt.as_mut_ptr()).stream_index = 0;
                ffi::av_packet_rescale_ts(pkt.as_mut_ptr(), enc_tb, video_out_tb);
            }
            out.write_packet_interleaved(pkt)?;
        }
    }
    if let (Some(d), Some(e), Some(aw)) = (ad.as_mut(), ae.as_mut(), audio_writer) {
        d.send_eof()?;
        drain_audio(d, e, af, af_state, af_str, af_tb, aw, out, frame, filt, pkt)?;
        if let Some(f) = af.as_mut() {
            f.push_eof()?;
            aw.encode_filtered(e, f, filt, out, pkt)?;
        }
        e.send_eof()?;
        aw.drain(e, out, pkt)?;
    }
    out.write_trailer()?;
    Ok(())
}

#[cfg(test)]
mod register_run_test {
    use super::*;

    fn fresh_state(total: usize) -> SessionState {
        SessionState {
            runs: BTreeMap::new(),
            segment_index: vec![None; total],
            max_cache_bytes: 0,
            base_dir: PathBuf::new(),
            overlay_generations: std::collections::HashSet::new(),
        }
    }

    fn populate_run(
        state: &mut SessionState,
        run_start: u32,
        generation: u32,
        count: u32,
        seg_dur: f64,
    ) {
        populate(state, run_start, generation, count, seg_dur, None);
    }

    fn populate(
        state: &mut SessionState,
        run_start: u32,
        generation: u32,
        count: u32,
        seg_dur: f64,
        end_segment: Option<u32>,
    ) {
        state.runs.insert(
            generation,
            TranscodeRun {
                generation,
                run_start,
                start_time_secs: run_start as f64 * seg_dur,
                segment_count: count,
                estimated_bytes: 0,
                segment_durations: vec![seg_dur; count as usize],
                end_segment,
                complete: false,
            },
        );
        for i in 0..count {
            let abs = run_start + i;
            state.segment_index[abs as usize] = Some(SegmentEntry {
                generation,
                run_start,
            });
        }
    }

    #[test]
    fn unbounded_register_clears_everything_from_abs_start() {
        let mut state = fresh_state(20);
        populate_run(&mut state, 0, 0, 20, 6.0); // primary run gen0 covers [0,20)

        // Simulate a seek that creates a fresh primary run starting at segment 5.
        register_run(&mut state, 1, 5, None, 30.0);

        // Seg 0..5 retained (gen0), seg 5..20 wiped (the fresh run will populate as it produces).
        assert!(state.segment_index[0].is_some());
        assert!(state.segment_index[4].is_some());
        assert!(state.segment_index[5].is_none());
        assert!(state.segment_index[19].is_none());
        assert!(state.runs.contains_key(&0));
        assert_eq!(state.runs[&1].run_start, 5);
    }

    #[test]
    fn bounded_register_only_clears_its_slice() {
        let mut state = fresh_state(20);
        populate_run(&mut state, 0, 0, 20, 6.0); // primary copy run gen0

        // Overlay generation 1 claims segments [5, 10) — i.e. 5..=9.
        register_run(&mut state, 1, 5, Some(10), 30.0);

        // Pre-overlay segments untouched.
        assert!(state.segment_index[0].is_some());
        assert!(state.segment_index[4].is_some());
        // Overlay slice cleared (waiting for the overlay encoder to repopulate).
        assert!(state.segment_index[5].is_none());
        assert!(state.segment_index[9].is_none());
        // Post-overlay copy segments must remain — this is the entire point.
        assert!(
            state.segment_index[10].is_some(),
            "post-overlay copy segments wiped"
        );
        assert!(state.segment_index[19].is_some());

        assert!(state.runs.contains_key(&0));
        assert_eq!(state.runs[&1].run_start, 5);
    }

    #[test]
    fn bounded_register_replaces_overlapping_overlay() {
        let mut state = fresh_state(20);
        populate_run(&mut state, 0, 0, 20, 6.0);
        populate(&mut state, 5, 1, 5, 6.0, Some(10));

        register_run(&mut state, 2, 5, Some(10), 30.0);

        assert!(!state.runs.contains_key(&1));
        assert_eq!(state.runs[&2].run_start, 5);
        // Post-overlay copy segments still intact.
        assert!(state.segment_index[10].is_some());
    }

    /// Inline copy of `evict_if_needed`'s eviction-candidate selection so we
    /// can unit-test it without spinning up a full TranscodeSession (which
    /// requires real ffmpeg I/O).
    fn pick_eviction_candidate(
        state: &SessionState,
        active_gen: u32,
        playback_segment: u32,
    ) -> Option<u32> {
        state
            .runs
            .iter()
            .filter(|(_, r)| r.generation != active_gen)
            .filter(|(_, r)| !state.overlay_generations.contains(&r.generation))
            .filter(|(_, r)| {
                let end = r.run_start + r.segment_count;
                !(r.run_start <= playback_segment && end > playback_segment)
            })
            .max_by_key(|(_, r)| {
                let mid = r.run_start + r.segment_count / 2;
                mid.abs_diff(playback_segment)
            })
            .map(|(_, r)| r.generation)
    }

    #[test]
    fn eviction_skips_overlay_generations() {
        // Three runs: gen0 (primary, far from playhead), gen1 (overlay,
        // far from playhead too), gen2 (active). Without overlay protection,
        // either gen0 or gen1 would be eligible. With protection, only gen0.
        let mut state = fresh_state(50);
        populate_run(&mut state, 0, 0, 5, 6.0); // primary, segs [0,5)
        populate_run(&mut state, 5, 1, 3, 6.0); // overlay, segs [5,8)
        populate_run(&mut state, 30, 2, 5, 6.0); // active near playhead
        state.overlay_generations.insert(1);

        // Playback at seg 32 (inside gen2's range). active_gen = 2.
        let pick = pick_eviction_candidate(&state, 2, 32);
        assert_eq!(pick, Some(0), "must evict primary gen0, not overlay gen1");

        // Without overlay protection, the further-from-playhead run wins.
        // gen1 mid is segment 6, gen0 mid is segment 2 — gen0 still further.
        // Add a closer-to-playhead primary run to make gen1 the natural pick:
        populate_run(&mut state, 28, 3, 1, 6.0); // very close to playhead
        let pick_with_close_primary = pick_eviction_candidate(&state, 2, 32);
        // gen0 (mid 2) is further than gen1 (mid 6) and gen3 (mid 28). Still
        // picks gen0 because it's furthest. The point is gen1 (overlay) is
        // never picked even when it's a candidate by distance.
        assert_eq!(pick_with_close_primary, Some(0));
        // Now drop overlay protection and confirm gen1 becomes evictable.
        state.overlay_generations.clear();
        // After clearing, gen1 is eligible — but gen0 is still further away,
        // so it'd still be chosen. The protection check is what matters: re-insert
        // overlay set with gen0 instead and confirm gen1 wins.
        state.overlay_generations.insert(0);
        let pick_swap = pick_eviction_candidate(&state, 2, 32);
        assert_eq!(pick_swap, Some(1), "with gen0 protected, gen1 should win");
    }
}

#[cfg(test)]
mod hls_test {
    use super::*;
    use std::path::Path;

    #[test]
    #[ignore]
    fn test_hls_transcode_segments() {
        let input_path = match std::env::var("KOB_TEST_MKV") {
            Ok(p) if Path::new(&p).exists() => p,
            _ => {
                eprintln!("Set KOB_TEST_MKV to a real MKV file to run this test");
                return;
            }
        };
        let input = Path::new(&input_path);
        let output_dir = Path::new("/tmp/test_hls_transcode");
        let _ = std::fs::remove_dir_all(output_dir);
        std::fs::create_dir_all(output_dir).unwrap();

        let hw_device = crate::hw::device::HwDeviceContext::vaapi()
            .ok()
            .map(std::sync::Arc::new);
        let encoder = if hw_device.is_some() {
            "h264_vaapi"
        } else {
            "libx264"
        };
        eprintln!("Using encoder: {encoder}, hw: {}", hw_device.is_some());

        let mut config = SessionConfig::new(output_dir, 60.0)
            .segment_duration(6.0)
            .video_encoder(encoder);
        if let Some(hw) = hw_device {
            config = config.hw_device(hw);
        }

        let session = TranscodeSession::start(input, config).unwrap();
        for i in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            session.update_segments();
            let segs = session.playlist_segments_from(0);
            eprintln!("[{i}] segments: {}", segs.len());
            if segs.len() >= 5 {
                break;
            }
        }

        let m3u8 = output_dir.join("gen_0/stream.m3u8");
        if let Ok(c) = std::fs::read_to_string(&m3u8) {
            eprintln!("stream.m3u8:\n{c}");
        }
        let segs = session.playlist_segments_from(0);
        assert!(segs.len() >= 2, "Expected >=2 segments, got {}", segs.len());
        session.cancel();
    }

    /// Real-fixture overlay test, off by default. Run with:
    ///   KOB_TEST_OVERLAY_MKV=/path/to/clean.mkv cargo test --offline -p kob-ffmpeg \
    ///     -- --ignored test_overlay_inserts_transcoded_window
    ///
    /// Spins up a primary copy session, calls `start_overlay` over a 12s
    /// window, and verifies the resulting playlist has copy → discontinuity →
    /// overlay segments laid out correctly.
    #[test]
    #[ignore]
    fn test_overlay_inserts_transcoded_window() {
        let input_path = match std::env::var("KOB_TEST_OVERLAY_MKV") {
            Ok(p) if Path::new(&p).exists() => p,
            _ => {
                eprintln!("Set KOB_TEST_OVERLAY_MKV to a real MKV file to run this test");
                return;
            }
        };
        let input = Path::new(&input_path);
        let output_dir = Path::new("/tmp/test_overlay_session");
        let _ = std::fs::remove_dir_all(output_dir);
        std::fs::create_dir_all(output_dir).unwrap();

        // Primary: copy mode (no video_encoder set).
        let config = SessionConfig::new(output_dir, 600.0).segment_duration(6.0);
        let session = TranscodeSession::start(input, config).expect("primary session start");

        // Wait for some primary segments to land before overlaying.
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            session.update_segments();
            let segs = session.playlist_segments_from(0);
            if segs.len() >= 5 {
                break;
            }
        }

        // Overlay [12s, 24s) — should claim a couple of segments.
        let overlay_gen = session
            .start_overlay(
                12.0..24.0,
                OverlayConfig {
                    video_encoder: "libx264".to_string(),
                    video_bitrate: None,
                    audio_encoder: None,
                    audio_bitrate: None,
                    hw_device: None,
                },
            )
            .expect("start_overlay");
        eprintln!("overlay generation: {overlay_gen}");

        // Wait for overlay to produce its segments.
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            session.update_segments();
            let segs = session.playlist_segments_from(0);
            let any_overlay = segs.iter().any(|s| s.generation == overlay_gen);
            if any_overlay {
                break;
            }
        }

        let segs = session.playlist_segments_from(0);
        eprintln!(
            "playlist segments: {}",
            segs.iter()
                .map(|s| format!("[gen{}]", s.generation))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let overlay_count = segs.iter().filter(|s| s.generation == overlay_gen).count();
        let copy_count = segs.iter().filter(|s| s.generation == 0).count();
        assert!(
            overlay_count >= 1,
            "expected ≥1 overlay segment in playlist, got {overlay_count}"
        );
        assert!(
            copy_count >= 1,
            "expected primary copy segments to coexist with overlay, got {copy_count}"
        );
        let boundaries = segs.iter().filter(|s| s.is_generation_boundary).count();
        assert!(
            boundaries >= 2,
            "expected ≥2 generation boundaries (copy→overlay→copy), got {boundaries}"
        );

        session.cancel();
    }
}
