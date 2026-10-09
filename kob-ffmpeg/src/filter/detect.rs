//! Detection filter helpers (silence, black frames, LUFS).
//!
//! These use FFmpeg filter graphs that analyze media and produce metadata
//! instead of (or in addition to) output frames.

use std::path::Path;

use crate::codec::decoder::{DecodeResult, Decoder};
use crate::filter::graph::{FilterGraph, VideoFilterParams};
use crate::format::input::{InputContext, MediaType};
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

/// A detected black frame interval.
#[derive(Debug, Clone)]
pub struct BlackInterval {
    pub start_secs: f64,
    pub end_secs: f64,
    pub duration_secs: f64,
}

/// Parameters for `blackdetect`.
#[derive(Debug, Clone)]
pub struct BlackFramesParams {
    /// Minimum run-length (seconds) to emit as a black interval.
    pub min_duration_secs: f64,
    /// Per-pixel black threshold (`pix_th`, 0.0..=1.0, lower = stricter).
    pub pixel_threshold: f64,
    /// Fraction of a picture's pixels that must fall under `pixel_threshold`
    /// before the picture counts as black (`pic_th`, 0.0..=1.0, lower = looser).
    /// Sources that fade to *near* black rather than black need this well under
    /// libavfilter's 0.98 default.
    pub picture_threshold: f64,
}

impl Default for BlackFramesParams {
    fn default() -> Self {
        // Matches the boundary-refinement defaults used by the V1 detect path;
        // `picture_threshold` is libavfilter's own default.
        Self {
            min_duration_secs: 0.1,
            pixel_threshold: 0.10,
            picture_threshold: 0.98,
        }
    }
}

impl BlackFramesParams {
    #[must_use]
    pub fn min_duration_secs(mut self, secs: f64) -> Self {
        self.min_duration_secs = secs;
        self
    }

    #[must_use]
    pub fn pixel_threshold(mut self, threshold: f64) -> Self {
        self.pixel_threshold = threshold;
        self
    }

    #[must_use]
    pub fn picture_threshold(mut self, threshold: f64) -> Self {
        self.picture_threshold = threshold;
        self
    }

    pub(crate) fn filter_string(&self) -> String {
        format!(
            "blackdetect=d={}:pix_th={}:pic_th={}",
            self.min_duration_secs, self.pixel_threshold, self.picture_threshold
        )
    }
}

/// A detected silence interval.
#[derive(Debug, Clone)]
pub struct SilenceInterval {
    pub start_secs: f64,
    pub end_secs: f64,
    pub duration_secs: f64,
}

/// Detect silence intervals in an audio stream.
///
/// Replaces: `ffmpeg -i file -af silencedetect=noise=<noise>dB:d=<duration> -f null -`
pub fn detect_silence(
    path: &Path,
    noise_db: f64,
    min_duration: f64,
    run: Option<&crate::progress::RunContext>,
) -> Result<Vec<SilenceInterval>> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let audio_idx = ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    ctx.discard_all_except(audio_idx);

    let stream = ctx.stream(audio_idx).unwrap();
    let time_base = stream.time_base();
    let mut decoder = stream.decoder()?;

    let filter_str = format!("silencedetect=noise={noise_db}dB:d={min_duration}");
    let mut graph: Option<FilterGraph> = None;
    let mut graph_state: Option<crate::audio::AudioGraphState> = None;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut out_frame = Frame::new()?;
    let mut intervals = Vec::new();
    let mut current_start: Option<f64> = None;

    // Process all packets
    while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        decoder.send_packet(&packet)?;
        drain_decoder_to_filter_adaptive(
            &mut decoder,
            &mut graph,
            &mut graph_state,
            &filter_str,
            time_base,
            &mut frame,
            &mut out_frame,
            &mut intervals,
            &mut current_start,
        )?;
    }

    // Flush
    decoder.send_eof()?;
    drain_decoder_to_filter_adaptive(
        &mut decoder,
        &mut graph,
        &mut graph_state,
        &filter_str,
        time_base,
        &mut frame,
        &mut out_frame,
        &mut intervals,
        &mut current_start,
    )?;
    if let Some(ref mut g) = graph {
        g.push_eof()?;
        drain_filter_output(g, &mut out_frame, &mut intervals, &mut current_start)?;
    }

    Ok(intervals)
}

/// Detect silence intervals in a specific time range.
pub fn detect_silence_range(
    path: &Path,
    start_secs: f64,
    duration_secs: f64,
    noise_db: f64,
    min_duration: f64,
    run: Option<&crate::progress::RunContext>,
) -> Result<Vec<SilenceInterval>> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let audio_idx = ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    ctx.discard_all_except(audio_idx);

    // Seek to start
    let seek_ts = (start_secs * ffi::AV_TIME_BASE as f64) as i64;
    ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;

    let stream = ctx.stream(audio_idx).unwrap();
    let tb = stream.time_base_native();
    let tb_f64 = tb.num as f64 / tb.den as f64;
    let end_secs = start_secs + duration_secs;

    let time_base = stream.time_base();
    let mut decoder = stream.decoder()?;

    let filter_str = format!("silencedetect=noise={noise_db}dB:d={min_duration}");
    let mut graph: Option<FilterGraph> = None;
    let mut graph_state: Option<crate::audio::AudioGraphState> = None;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut out_frame = Frame::new()?;
    let mut intervals = Vec::new();
    let mut current_start_silence: Option<f64> = None;

    while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        // Stop if past end time
        let pkt_time = packet.pts() as f64 * tb_f64;
        if pkt_time > end_secs {
            break;
        }
        decoder.send_packet(&packet)?;
        drain_decoder_to_filter_adaptive(
            &mut decoder,
            &mut graph,
            &mut graph_state,
            &filter_str,
            time_base,
            &mut frame,
            &mut out_frame,
            &mut intervals,
            &mut current_start_silence,
        )?;
    }

    decoder.send_eof()?;
    drain_decoder_to_filter_adaptive(
        &mut decoder,
        &mut graph,
        &mut graph_state,
        &filter_str,
        time_base,
        &mut frame,
        &mut out_frame,
        &mut intervals,
        &mut current_start_silence,
    )?;
    if let Some(ref mut g) = graph {
        g.push_eof()?;
        drain_filter_output(
            g,
            &mut out_frame,
            &mut intervals,
            &mut current_start_silence,
        )?;
    }

    Ok(intervals)
}

#[allow(clippy::too_many_arguments)]
fn drain_decoder_to_filter_adaptive(
    decoder: &mut Decoder,
    graph: &mut Option<FilterGraph>,
    graph_state: &mut Option<crate::audio::AudioGraphState>,
    filter_str: &str,
    time_base: ffi::AVRational,
    frame: &mut Frame,
    out_frame: &mut Frame,
    intervals: &mut Vec<SilenceInterval>,
    current_start: &mut Option<f64>,
) -> Result<()> {
    loop {
        match decoder.receive_frame(frame)? {
            DecodeResult::Frame => {
                crate::audio::AudioGraphState::ensure_filter_graph(
                    graph,
                    graph_state,
                    frame,
                    time_base,
                    filter_str,
                )?;
                graph.as_mut().unwrap().push_frame(frame)?;
                drain_filter_output(graph.as_mut().unwrap(), out_frame, intervals, current_start)?;
            }
            DecodeResult::NeedsInput | DecodeResult::Eof => break,
        }
    }
    Ok(())
}

fn drain_filter_output(
    graph: &mut FilterGraph,
    frame: &mut Frame,
    intervals: &mut Vec<SilenceInterval>,
    current_start: &mut Option<f64>,
) -> Result<()> {
    while graph.pull_frame(frame)? {
        // The silencedetect filter adds metadata to output frames.
        // We need to check frame metadata for silence_start/silence_end.
        // For now, frames pass through — the detection happens via the
        // filter's internal logging. We'll parse metadata from the frame.
        parse_silence_metadata(frame, intervals, current_start);
    }
    Ok(())
}

/// Detect black frame intervals in a video stream.
///
/// Replaces: `ffmpeg -i file -vf blackdetect=d=<dur>:pix_th=<pix>:pic_th=<pic> -f null -`
pub fn detect_black_frames(
    path: &Path,
    params: &BlackFramesParams,
    run: Option<&crate::progress::RunContext>,
) -> Result<Vec<BlackInterval>> {
    detect_black_frames_range(path, None, None, params, run)
}

/// Detect black frames in a specific time range.
pub fn detect_black_frames_range(
    path: &Path,
    start_secs: Option<f64>,
    duration_secs: Option<f64>,
    params: &BlackFramesParams,
    run: Option<&crate::progress::RunContext>,
) -> Result<Vec<BlackInterval>> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let video_idx = ctx.find_best_stream(MediaType::Video).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no video stream")
    })?;

    if let Some(start) = start_secs {
        let seek_ts = (start * ffi::AV_TIME_BASE as f64) as i64;
        ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
    }

    let stream = ctx.stream(video_idx).unwrap();
    let tb = stream.time_base();
    let tb_f64 = tb.num as f64 / tb.den as f64;
    let end_secs = start_secs.zip(duration_secs).map(|(s, d)| s + d);

    // Batch detect pass — frame-threaded decode is a big win, first-frame
    // latency is irrelevant.
    let mut decoder = stream.decoder_with_threads(0)?;

    let filter_str = params.filter_string();
    let params = VideoFilterParams {
        width: decoder.width(),
        height: decoder.height(),
        pixel_format: decoder.pixel_format_raw(),
        time_base: tb,
        sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
        hw_frames_ctx: None,
        color: Default::default(),
        side_data_frame: std::ptr::null(),
    };
    let mut graph = FilterGraph::new_video(&params, &filter_str)?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut out_frame = Frame::new()?;
    let mut intervals = Vec::new();
    let mut current_start: Option<f64> = None;

    while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != video_idx as i32 {
            continue;
        }
        if let Some(end) = end_secs
            && packet.pts() as f64 * tb_f64 > end
        {
            break;
        }
        decoder.send_packet(&packet)?;
        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    graph.push_frame(&mut frame)?;
                    while graph.pull_frame(&mut out_frame)? {
                        parse_black_metadata(&out_frame, &mut intervals, &mut current_start);
                        out_frame.unref();
                    }
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break,
            }
        }
    }

    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame)? {
            DecodeResult::Frame => {
                graph.push_frame(&mut frame)?;
                while graph.pull_frame(&mut out_frame)? {
                    parse_black_metadata(&out_frame, &mut intervals, &mut current_start);
                    out_frame.unref();
                }
            }
            _ => break,
        }
    }
    graph.push_eof()?;
    while graph.pull_frame(&mut out_frame)? {
        parse_black_metadata(&out_frame, &mut intervals, &mut current_start);
        out_frame.unref();
    }

    Ok(intervals)
}

/// Combined silence + black frame detection in a time range (convenience wrapper
/// that calls `detect_silence_range` and `detect_black_frames_range` separately,
/// opening the file twice).
///
/// Replaces: `ffmpeg -ss <start> -t <dur> -i file -vf blackdetect=... -af silencedetect=... -f null -`
pub fn detect_boundaries(
    path: &Path,
    start_secs: f64,
    duration_secs: f64,
    silence_noise_db: f64,
    silence_min_duration: f64,
    black: &BlackFramesParams,
    run: Option<&crate::progress::RunContext>,
) -> Result<(Vec<SilenceInterval>, Vec<BlackInterval>)> {
    let silence = detect_silence_range(
        path,
        start_secs,
        duration_secs,
        silence_noise_db,
        silence_min_duration,
        run,
    )?;
    let black = detect_black_frames_range(path, Some(start_secs), Some(duration_secs), black, run)?;
    Ok((silence, black))
}

pub(crate) fn parse_black_metadata(
    frame: &Frame,
    intervals: &mut Vec<BlackInterval>,
    current_start: &mut Option<f64>,
) {
    unsafe {
        let metadata = (*frame.as_ptr()).metadata;
        if metadata.is_null() {
            return;
        }

        let start_entry =
            ffi::av_dict_get(metadata, c"lavfi.black_start".as_ptr(), std::ptr::null(), 0);
        if !start_entry.is_null() && !(*start_entry).value.is_null() {
            let val = std::ffi::CStr::from_ptr((*start_entry).value).to_string_lossy();
            if let Ok(t) = val.parse::<f64>() {
                *current_start = Some(t);
            }
        }

        let end_entry =
            ffi::av_dict_get(metadata, c"lavfi.black_end".as_ptr(), std::ptr::null(), 0);
        if !end_entry.is_null()
            && !(*end_entry).value.is_null()
            && let Some(start) = current_start.take()
        {
            let val = std::ffi::CStr::from_ptr((*end_entry).value).to_string_lossy();
            if let Ok(end) = val.parse::<f64>() {
                intervals.push(BlackInterval {
                    start_secs: start,
                    end_secs: end,
                    duration_secs: end - start,
                });
            }
        }
    }
}

pub(crate) fn parse_silence_metadata(
    frame: &Frame,
    intervals: &mut Vec<SilenceInterval>,
    current_start: &mut Option<f64>,
) {
    unsafe {
        let metadata = (*frame.as_ptr()).metadata;
        if metadata.is_null() {
            return;
        }

        // Check for lavfi.silence_start
        let start_entry = ffi::av_dict_get(
            metadata,
            c"lavfi.silence_start".as_ptr(),
            std::ptr::null(),
            0,
        );
        if !start_entry.is_null() && !(*start_entry).value.is_null() {
            let val = std::ffi::CStr::from_ptr((*start_entry).value).to_string_lossy();
            if let Ok(t) = val.parse::<f64>() {
                *current_start = Some(t);
            }
        }

        // Check for lavfi.silence_end
        let end_entry =
            ffi::av_dict_get(metadata, c"lavfi.silence_end".as_ptr(), std::ptr::null(), 0);
        if !end_entry.is_null()
            && !(*end_entry).value.is_null()
            && let Some(start) = current_start.take()
        {
            let val = std::ffi::CStr::from_ptr((*end_entry).value).to_string_lossy();
            if let Ok(end) = val.parse::<f64>() {
                intervals.push(SilenceInterval {
                    start_secs: start,
                    end_secs: end,
                    duration_secs: end - start,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BlackFramesParams;

    #[test]
    fn filter_string_carries_every_threshold() {
        assert_eq!(
            BlackFramesParams::default().filter_string(),
            "blackdetect=d=0.1:pix_th=0.1:pic_th=0.98"
        );
        assert_eq!(
            BlackFramesParams::default()
                .min_duration_secs(0.04)
                .pixel_threshold(0.15)
                .picture_threshold(0.85)
                .filter_string(),
            "blackdetect=d=0.04:pix_th=0.15:pic_th=0.85"
        );
    }
}
