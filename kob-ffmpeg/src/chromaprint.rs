//! Audio fingerprinting via libchromaprint (FFI, no CLI).
//!
//! Decodes audio to PCM, feeds it to chromaprint's C library, and returns
//! the raw fingerprint. No child process spawning — runs entirely in-process.

use std::path::Path;

use crate::codec::decoder::DecodeResult;
use crate::format::input::{InputContext, MediaType};
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

const SAMPLE_RATE: i32 = 8000;

/// A chromaprint audio fingerprint.
pub struct Fingerprint {
    /// Raw sub-fingerprint values.
    pub raw: Vec<u32>,
    /// Duration of the fingerprinted audio in seconds.
    pub duration_secs: f64,
}

/// Extract a chromaprint fingerprint from an audio file.
///
/// Decodes audio, resamples to 8kHz mono s16le, feeds to libchromaprint.
/// If `offset_secs` is Some, seeks to that position first.
pub fn extract_fingerprint(
    path: &Path,
    offset_secs: Option<f64>,
    length_secs: f64,
) -> Result<Fingerprint> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let audio_idx = ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    // Set AVDISCARD_ALL on non-audio streams. This skips DECODE of video and
    // subtitle packets but does NOT reduce wire bytes — lavf still walks the
    // byte stream to find audio packet boundaries. The savings are CPU only.
    // Measured 2026-04-13: byte-identical CIFS read with vs without discard.
    ctx.discard_all_except(audio_idx);

    if let Some(offset) = offset_secs {
        let seek_ts = (offset * ffi::AV_TIME_BASE as f64) as i64;
        ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
    }

    let stream = ctx.stream(audio_idx).unwrap();
    let tb = stream.time_base();
    let tb_f64 = tb.num as f64 / tb.den as f64;
    let start_time = offset_secs.unwrap_or(0.0);
    let end_time = start_time + length_secs;

    let mut decoder = stream.decoder()?;
    if offset_secs.is_some() {
        decoder.flush();
    }

    // Resampler is created lazily from the first decoded frame's actual properties,
    // not from the decoder context. AC3 streams commonly declare 5.1 in the container
    // while initial decoded frames are stereo — using decoder.channel_layout() here
    // would configure the resampler for 6 channels, then swr_convert reads NULL
    // extended_data pointers for channels 3-6 → segfault.
    let mut resampler: Option<crate::resample::Resampler> = None;
    let mut resampler_state: Option<crate::audio::AudioGraphState> = None;

    // Initialize chromaprint
    let cp_ctx = unsafe { ffi::chromaprint_new(ffi::CHROMAPRINT_ALGORITHM_DEFAULT as i32) };
    if cp_ctx.is_null() {
        return Err(crate::error::FfmpegError::new(
            ffi::AVERROR_ENOMEM,
            "failed to create chromaprint context",
        ));
    }

    // RAII guard for chromaprint context
    struct ChromaprintGuard(*mut ffi::ChromaprintContext);
    impl Drop for ChromaprintGuard {
        fn drop(&mut self) {
            unsafe { ffi::chromaprint_free(self.0) };
        }
    }
    let _guard = ChromaprintGuard(cp_ctx);

    unsafe {
        if ffi::chromaprint_start(cp_ctx, SAMPLE_RATE, 1) == 0 {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EXTERNAL,
                "chromaprint_start failed",
            ));
        }
    }

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut total_samples = 0u64;

    while ctx.read_packet(&mut packet)? {
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }

        // Stop if past the requested time range
        if packet.pts() != ffi::AV_NOPTS_VALUE {
            let pkt_time = packet.pts() as f64 * tb_f64;
            if pkt_time > end_time {
                break;
            }
        }

        decoder.send_packet(&packet)?;
        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    // Skip frames before the offset
                    if let Some(offset) = offset_secs
                        && frame.pts() != ffi::AV_NOPTS_VALUE
                    {
                        let frame_time = frame.pts() as f64 * tb_f64;
                        if frame_time < offset {
                            continue;
                        }
                    }

                    // Rebuild resampler if frame properties changed (AC3 layout mismatch)
                    crate::audio::AudioGraphState::ensure_resampler(
                        &mut resampler,
                        &mut resampler_state,
                        &frame,
                        SAMPLE_RATE,
                        crate::types::SampleFormat::S16,
                        crate::types::ChannelLayout::mono(),
                    )?;

                    let pcm = resampler.as_mut().unwrap().convert_frame_to_bytes(&frame)?;
                    if !pcm.is_empty() {
                        let samples = pcm.len() / 2;
                        unsafe {
                            ffi::chromaprint_feed(
                                cp_ctx,
                                pcm.as_ptr() as *const i16,
                                samples as i32,
                            );
                        }
                        total_samples += samples as u64;
                    }
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break,
            }
        }
    }

    // Flush decoder
    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame)? {
            DecodeResult::Frame => {
                crate::audio::AudioGraphState::ensure_resampler(
                    &mut resampler,
                    &mut resampler_state,
                    &frame,
                    SAMPLE_RATE,
                    crate::types::SampleFormat::S16,
                    crate::types::ChannelLayout::mono(),
                )?;
                let pcm = resampler.as_mut().unwrap().convert_frame_to_bytes(&frame)?;
                if !pcm.is_empty() {
                    let samples = pcm.len() / 2;
                    unsafe {
                        ffi::chromaprint_feed(cp_ctx, pcm.as_ptr() as *const i16, samples as i32);
                    }
                    total_samples += samples as u64;
                }
            }
            _ => break,
        }
    }

    unsafe {
        if ffi::chromaprint_finish(cp_ctx) == 0 {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EXTERNAL,
                "chromaprint_finish failed",
            ));
        }
    }

    // Extract raw fingerprint
    let mut fp_ptr: *mut u32 = std::ptr::null_mut();
    let mut fp_size: i32 = 0;
    unsafe {
        if ffi::chromaprint_get_raw_fingerprint(cp_ctx, &mut fp_ptr, &mut fp_size) == 0 {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EXTERNAL,
                "chromaprint_get_raw_fingerprint failed",
            ));
        }
    }

    let raw = if fp_ptr.is_null() || fp_size <= 0 {
        Vec::new()
    } else {
        let slice = unsafe { std::slice::from_raw_parts(fp_ptr, fp_size as usize) };
        let result = slice.to_vec();
        unsafe { ffi::chromaprint_dealloc(fp_ptr as *mut std::ffi::c_void) };
        result
    };

    let duration_secs = total_samples as f64 / SAMPLE_RATE as f64;

    Ok(Fingerprint { raw, duration_secs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_fingerprint_corrupt_ac3_does_not_crash() {
        crate::init();
        let tmp = tempfile::Builder::new()
            .suffix(".mkv")
            .tempfile()
            .expect("create temp file");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
                "-c:a",
                "ac3",
                "-b:a",
                "192k",
                "-y",
            ])
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be available");
        assert!(status.success());

        // Corrupt the file
        let mut data = std::fs::read(tmp.path()).expect("read");
        let len = data.len();
        if len > 2000 {
            for offset in [len / 4, len / 3, len / 2, 2 * len / 3] {
                let end = (offset + 64).min(len);
                data[offset..end].fill(0xFF);
            }
        }
        std::fs::write(tmp.path(), &data).expect("write corrupted");

        // Must not crash — should either succeed with partial data or return an error.
        let result = extract_fingerprint(tmp.path(), None, 3.0);
        match result {
            Ok(fp) => eprintln!("fingerprint extracted: {} sub-fingerprints", fp.raw.len()),
            Err(e) => eprintln!("fingerprint failed gracefully: {e}"),
        }
    }

    /// Regression test for AC3 channel layout mismatch segfault.
    ///
    /// AC3 containers declare 5.1 (6ch) but initial decoded frames can be stereo (2ch).
    /// The old code created the resampler from decoder context (6ch), then swr_convert
    /// read extended_data[2..5] which were NULL → memcpy(dst, NULL) → segfault.
    ///
    /// This test generates a 5.1 AC3 file to exercise the ensure_resampler path.
    #[test]
    fn test_fingerprint_ac3_51_does_not_crash() {
        crate::init();
        let tmp = tempfile::Builder::new()
            .suffix(".mkv")
            .tempfile()
            .expect("create temp file");
        // Generate a 5.1 AC3 file — the channel layout in the container will be 5.1
        // but the decoder may produce frames with fewer channels initially.
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
                "-af",
                "aformat=channel_layouts=5.1",
                "-c:a",
                "ac3",
                "-b:a",
                "384k",
                "-y",
            ])
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg must be available");
        assert!(status.success(), "failed to generate 5.1 AC3 test file");

        // Must not crash — the resampler must adapt to actual frame channel layout.
        let result = extract_fingerprint(tmp.path(), None, 3.0);
        match result {
            Ok(fp) => eprintln!(
                "5.1 AC3 fingerprint: {} sub-fingerprints, {:.1}s",
                fp.raw.len(),
                fp.duration_secs
            ),
            Err(e) => eprintln!("5.1 AC3 fingerprint failed gracefully: {e}"),
        }
    }
}
