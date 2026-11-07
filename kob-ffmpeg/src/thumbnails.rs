//! Thumbnail/frame extraction from video files.
//!
//! Single entry point: [`extract_frames_sequential_with_hw`] — opens the file
//! once, seeks to the start, and streams forward through the requested
//! region, emitting one JPEG per grid point. Per-frame seek variants used to
//! live here but were removed on 2026-04-13 after bench measurements showed
//! sequential streaming was strictly faster and much gentler on slow NAS.

use std::path::Path;

use crate::codec::decoder::DecodeResult;
use crate::format::input::{InputContext, MediaType};
use crate::fps_grid::TargetFpsGrid;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

/// A thumbnail as JPEG bytes at a specific timestamp.
pub struct JpegFrame {
    /// Timestamp in seconds where this frame was extracted.
    pub timestamp_secs: f64,
    /// JPEG-encoded image data.
    pub jpeg_data: Vec<u8>,
    /// Image width in pixels.
    pub width: i32,
    /// Image height in pixels.
    pub height: i32,
}

/// Extract frames as JPEG by sequential decode (CPU only).
///
/// For GPU-accelerated extraction, use [`extract_frames_sequential_with_hw`]
/// with a shared `HwDeviceContext`. This wrapper uses CPU decoding to avoid
/// creating per-call CUDA contexts that race when called concurrently.
pub fn extract_frames_sequential(
    path: &Path,
    start_secs: f64,
    duration_secs: f64,
    fps: f64,
    scale_width: Option<i32>,
    jpeg_quality: i32,
    fast_decode: bool,
) -> Result<Vec<JpegFrame>> {
    extract_frames_sequential_with_hw(
        path,
        start_secs,
        duration_secs,
        fps,
        scale_width,
        jpeg_quality,
        fast_decode,
        None,
    )
}

/// Extract frames as JPEG by sequential decode through a time region.
///
/// Opens the file once, seeks to `start_secs`, decodes sequentially, and grabs
/// one frame every `1/fps` seconds until `start_secs + duration_secs`. One
/// smooth streaming read — measured 2× faster wall-clock than per-frame seek
/// at dense spacing (5s interval) and byte-equivalent, and dramatically kinder
/// to NAS SMB credit windows under concurrent load.
///
/// When `hw_device` is `Some`, uses the provided GPU context for hardware-accelerated
/// decoding. When `None`, auto-detects (CUDA → VAAPI → CPU). Sharing a single device
/// context across concurrent calls avoids CUDA driver races that cause segfaults.
///
/// When `fast_decode` is true, skips the loop filter (deblocking) for ~20-30%
/// faster decode at the cost of slight quality loss (negligible for thumbnails/hashing).
#[allow(clippy::too_many_arguments)]
pub fn extract_frames_sequential_with_hw(
    path: &Path,
    start_secs: f64,
    duration_secs: f64,
    fps: f64,
    scale_width: Option<i32>,
    jpeg_quality: i32,
    fast_decode: bool,
    hw_device: Option<&crate::hw::device::HwDeviceContext>,
) -> Result<Vec<JpegFrame>> {
    crate::init();

    let end_secs = start_secs + duration_secs;
    let mut grid = TargetFpsGrid::new(fps, start_secs);

    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let video_idx = ctx.find_best_stream(MediaType::Video).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no video stream")
    })?;

    // Seek to start
    if start_secs > 0.5 {
        let seek_ts = (start_secs * ffi::AV_TIME_BASE as f64) as i64;
        ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
    }

    let stream = ctx.stream(video_idx).unwrap();
    let tb_f64 = {
        let tb = stream.time_base_native();
        tb.num as f64 / tb.den as f64
    };

    // Use provided GPU context for hardware-accelerated decoding.
    // When None, decode on CPU — creating per-call CUDA/VAAPI contexts is unsafe
    // for concurrent use (driver races → NULL pointers → segfault).
    let hw_ref = hw_device;

    // Thumbnails is a batch processing job (not user-facing interactive) —
    // frame-threaded decode is fine and materially faster.
    let mut decoder = if let Some(hw) = hw_ref {
        let mut dec = unsafe {
            crate::codec::decoder::Decoder::from_codec_parameters_hw_with_threads(
                stream.codec_parameters(),
                hw,
                0,
            )?
        };
        if fast_decode {
            dec.set_skip_loop_filter(ffi::AVDISCARD_ALL);
        }
        dec.flush();
        dec
    } else {
        let mut dec = stream.decoder_with_threads(0)?;
        if fast_decode {
            dec.set_skip_loop_filter(ffi::AVDISCARD_ALL);
        }
        dec.flush();
        dec
    };

    let mut jpeg = crate::jpeg::ScaledJpeg::new(scale_width, jpeg_quality);
    let mut results = Vec::new();
    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut sw_frame = Frame::new()?;

    let mut process_frame = |frame: &Frame,
                             sw_frame: &mut Frame,
                             grid: &mut TargetFpsGrid,
                             results: &mut Vec<JpegFrame>|
     -> Result<bool> {
        let pts = frame.pts();
        if pts == ffi::AV_NOPTS_VALUE {
            return Ok(false);
        }
        let frame_time = pts as f64 * tb_f64;
        if frame_time > end_secs + 0.5 {
            return Ok(true);
        }
        if !grid.take_if_due(frame_time) {
            return Ok(false);
        }

        let cpu_frame = if frame.is_hw_format() {
            sw_frame.unref();
            crate::hw::frames::HwFramesContext::download(frame, sw_frame)?;
            &*sw_frame
        } else {
            frame
        };

        match jpeg.encode(cpu_frame) {
            Ok((jpeg_data, w, h)) => {
                results.push(JpegFrame {
                    timestamp_secs: frame_time,
                    jpeg_data,
                    width: w,
                    height: h,
                });
            }
            Err(e) => {
                tracing::warn!("Failed to encode frame at {frame_time:.1}s: {e}");
            }
        }
        Ok(false)
    };

    'outer: while ctx.read_packet(&mut packet)? {
        if packet.stream_index() != video_idx as i32 {
            continue;
        }
        decoder.send_packet(&packet)?;

        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    if process_frame(&frame, &mut sw_frame, &mut grid, &mut results)? {
                        break 'outer;
                    }
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break 'outer,
            }
        }
    }

    // Flush any frames still buffered in the decoder. With frame-threaded
    // decode the reorder buffer holds several frames at EOF that the normal
    // send→receive loop never drains — skipping this drops trailing frames
    // from the output (and was a silent bug even with single-thread decode).
    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame)? {
            DecodeResult::Frame => {
                if process_frame(&frame, &mut sw_frame, &mut grid, &mut results)? {
                    break;
                }
            }
            _ => break,
        }
    }

    Ok(results)
}
