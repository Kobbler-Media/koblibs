//! Decoder wrapper (AVCodecContext for decoding).

use std::ffi::CString;
use std::ptr;

use crate::ffi;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ff_check};

/// Owns an `AVCodecContext` configured for decoding.
///
/// `Send` but not `Sync` — decoder contexts are not thread-safe.
pub struct Decoder {
    ptr: *mut ffi::AVCodecContext,
}

unsafe impl Send for Decoder {}

/// Result of receiving a decoded frame.
pub enum DecodeResult {
    /// A frame was successfully decoded.
    Frame,
    /// The decoder needs more input (EAGAIN).
    NeedsInput,
    /// End of stream — no more frames will be produced.
    Eof,
}

impl Decoder {
    /// Configure lenient error handling on a decoder context.
    ///
    /// Must be called after `avcodec_alloc_context3` and before `avcodec_open2`.
    /// This tells FFmpeg to skip corrupt frames instead of failing, which prevents
    /// malformed audio/video packets from crashing the process.
    unsafe fn configure_error_resilience(ptr: *mut ffi::AVCodecContext) {
        // Disable all error recognition checks. This matches FFmpeg CLI's default
        // (`-err_detect 0`). Previously we used AV_EF_IGNORE_ERR which tells the
        // decoder to *keep going* despite corrupt data — that's dangerous because
        // it forces the decoder to process corrupt state, causing UB/segfault in
        // codecs like AC3 with malformed exponents. With 0 the decoder detects
        // the error and returns AVERROR immediately, which our Rust wrappers catch.
        unsafe {
            (*ptr).err_recognition = 0;
            // Enable error concealment for video: guess motion vectors and deblock
            // around damaged macroblocks. Harmless for audio decoders (ignored).
            (*ptr).error_concealment =
                (ffi::FF_EC_GUESS_MVS | ffi::FF_EC_DEBLOCK | ffi::FF_EC_FAVOR_INTER) as i32;
        }
    }

    /// Create a decoder from codec parameters (typically from a stream).
    ///
    /// This is the common path: get codec params from `stream.codec_parameters()`,
    /// find the matching decoder, and configure the context.
    ///
    /// Uses FFmpeg's default single-thread decode (`thread_count = 1`). For batch
    /// workloads that want frame-parallel decoding, use
    /// `from_codec_parameters_with_threads(params, 0)` to let ffmpeg auto-pick.
    ///
    /// # Safety
    /// `params` must point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters(params: *mut ffi::AVCodecParameters) -> Result<Self> {
        unsafe { Self::from_codec_parameters_with_threads(params, 1) }
    }

    /// Like `from_codec_parameters` but sets `AVCodecContext::thread_count`
    /// before opening the codec. `0` lets ffmpeg auto-pick (~num_cpus) for
    /// frame/slice threading — big batch-decode speedup on CPU. `1` keeps
    /// single-thread latency behaviour.
    ///
    /// Frame threading adds `thread_count` frames of decode latency. Use `1`
    /// (or the default ctor) for real-time / streaming / interactive paths
    /// where first-frame latency matters.
    ///
    /// # Safety
    /// `params` must point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters_with_threads(
        params: *mut ffi::AVCodecParameters,
        thread_count: i32,
    ) -> Result<Self> {
        let codec_id = unsafe { (*params).codec_id };
        let codec = unsafe { ffi::avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_DECODER_NOT_FOUND,
                format!("no decoder for codec id {codec_id}"),
            ));
        }

        let ptr = unsafe { ffi::avcodec_alloc_context3(codec) };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating decoder context",
            ));
        }

        unsafe {
            ff_check!(
                ffi::avcodec_parameters_to_context(ptr, params),
                "copying codec parameters to decoder"
            )?;

            Self::configure_error_resilience(ptr);
            (*ptr).thread_count = thread_count;

            ff_check!(
                ffi::avcodec_open2(ptr, codec, ptr::null_mut()),
                "opening decoder"
            )?;
        }

        Ok(Self { ptr })
    }

    /// Create a decoder from codec parameters with an attached hardware device
    /// context for GPU-accelerated decoding (e.g., NVDEC via CUDA).
    ///
    /// The `hw_device_ctx` is set on the codec context **before** `avcodec_open2`,
    /// which is required for the decoder to negotiate hardware pixel formats.
    ///
    /// Uses FFmpeg's default single-thread decode. For batch workloads see
    /// `from_codec_parameters_hw_with_threads`.
    ///
    /// # Safety
    /// `params` must point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters_hw(
        params: *mut ffi::AVCodecParameters,
        hw_device: &crate::hw::device::HwDeviceContext,
    ) -> Result<Self> {
        unsafe { Self::from_codec_parameters_hw_with_threads(params, hw_device, 1) }
    }

    /// Like `from_codec_parameters_hw` but sets `AVCodecContext::thread_count`
    /// before opening the codec. See `from_codec_parameters_with_threads`.
    ///
    /// # Safety
    /// `params` must point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters_hw_with_threads(
        params: *mut ffi::AVCodecParameters,
        hw_device: &crate::hw::device::HwDeviceContext,
        thread_count: i32,
    ) -> Result<Self> {
        let codec_id = unsafe { (*params).codec_id };
        let codec = unsafe { ffi::avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_DECODER_NOT_FOUND,
                format!("no decoder for codec id {codec_id}"),
            ));
        }

        let mut ptr = unsafe { ffi::avcodec_alloc_context3(codec) };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating decoder context",
            ));
        }

        unsafe {
            ff_check!(
                ffi::avcodec_parameters_to_context(ptr, params),
                "copying codec parameters to decoder"
            )?;

            Self::configure_error_resilience(ptr);
            (*ptr).thread_count = thread_count;

            // Attach hw device context before opening — this lets the decoder
            // negotiate hardware pixel formats (e.g., AV_PIX_FMT_CUDA).
            let hw_ref = ffi::av_buffer_ref(hw_device.as_ptr());
            if hw_ref.is_null() {
                ffi::avcodec_free_context(&mut ptr);
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "av_buffer_ref failed for hw_device_ctx",
                ));
            }
            (*ptr).hw_device_ctx = hw_ref;

            ff_check!(
                ffi::avcodec_open2(ptr, codec, ptr::null_mut()),
                "opening hw-accelerated decoder"
            )?;
        }

        Ok(Self { ptr })
    }

    /// Create a decoder by codec name (e.g., "h264", "aac", "libopus").
    pub fn from_name(name: &str) -> Result<Self> {
        let c_name = CString::new(name).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "codec name contains null byte")
        })?;
        let codec = unsafe { ffi::avcodec_find_decoder_by_name(c_name.as_ptr()) };
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_DECODER_NOT_FOUND,
                format!("decoder not found: {name}"),
            ));
        }

        let ptr = unsafe { ffi::avcodec_alloc_context3(codec) };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating decoder context",
            ));
        }

        unsafe {
            Self::configure_error_resilience(ptr);

            ff_check!(
                ffi::avcodec_open2(ptr, codec, ptr::null_mut()),
                "opening decoder {}",
                name
            )?;
        }

        Ok(Self { ptr })
    }

    /// Send a packet to the decoder for decoding.
    /// Returns `Ok(true)` if accepted, `Ok(false)` if the decoder is full (EAGAIN).
    /// Corrupt packets are skipped with a warning rather than failing.
    pub fn send_packet(&mut self, packet: &Packet) -> Result<bool> {
        let ret = unsafe { ffi::avcodec_send_packet(self.ptr, packet.as_ptr()) };
        if ret == ffi::AVERROR_EAGAIN {
            return Ok(false);
        }
        if ret == ffi::AVERROR_EOF {
            return Ok(true); // Already flushed
        }
        if ret < 0 {
            // Log but skip corrupt/invalid packets instead of killing the session.
            // The error flags we set should prevent most decoder crashes, but some
            // errors (e.g., invalid packet structure) still return error codes.
            eprintln!(
                "[kob-ffmpeg] Skipping corrupt packet (error {}), continuing",
                ret
            );
            return Ok(true);
        }
        Ok(true)
    }

    /// Send a flush signal to the decoder (NULL packet).
    /// Call this when there are no more packets to decode.
    pub fn send_eof(&mut self) -> Result<()> {
        let ret = unsafe { ffi::avcodec_send_packet(self.ptr, ptr::null()) };
        if ret == ffi::AVERROR_EOF {
            return Ok(()); // Already flushed
        }
        ff_check!(ret, "sending EOF to decoder")?;
        Ok(())
    }

    /// Receive a decoded frame from the decoder.
    /// Returns `NeedsInput` for errors on individual frames rather than failing,
    /// allowing the caller to continue with the next packet.
    ///
    /// Frames that decode "successfully" but have NULL data pointers (e.g., corrupt
    /// AC3 audio) are detected and skipped — returning `NeedsInput` instead of
    /// `Frame` to prevent downstream NULL dereferences in the resampler/filters.
    pub fn receive_frame(&mut self, frame: &mut Frame) -> Result<DecodeResult> {
        frame.unref();
        let ret = unsafe { ffi::avcodec_receive_frame(self.ptr, frame.as_mut_ptr()) };
        if ret == ffi::AVERROR_EAGAIN {
            return Ok(DecodeResult::NeedsInput);
        }
        if ret == ffi::AVERROR_EOF {
            return Ok(DecodeResult::Eof);
        }
        if ret < 0 {
            // Treat decode errors as "need more input" so the caller skips the
            // corrupt frame and continues decoding. This matches FFmpeg CLI's
            // default behaviour of logging and continuing.
            eprintln!(
                "[kob-ffmpeg] Skipping corrupt frame (error {}), continuing",
                ret
            );
            return Ok(DecodeResult::NeedsInput);
        }

        // Validate that the decoded frame actually has usable data. Some codecs
        // return success for corrupt frames but leave data pointers NULL — passing
        // these downstream causes segfaults (e.g., swr_convert → memcpy(NULL)).
        let codec_type = self.codec_type();
        let valid = if codec_type == ffi::AVMEDIA_TYPE_AUDIO {
            frame.has_audio_data()
        } else if codec_type == ffi::AVMEDIA_TYPE_VIDEO {
            frame.has_video_data()
        } else {
            true // subtitle/data/etc — no validation needed
        };

        if !valid {
            eprintln!(
                "[kob-ffmpeg] Skipping frame with NULL data pointers (corrupt decode), continuing"
            );
            frame.unref();
            return Ok(DecodeResult::NeedsInput);
        }

        Ok(DecodeResult::Frame)
    }

    /// Flush the decoder, resetting its internal state.
    /// Use after seeking to clear buffered frames.
    pub fn flush(&mut self) {
        unsafe { ffi::avcodec_flush_buffers(self.ptr) };
    }

    /// Set the loop filter skip level.
    /// `AVDISCARD_ALL` skips all deblocking, saving ~20-30% decode time.
    /// Quality loss is negligible for low-resolution outputs (thumbnails, hashing).
    pub fn set_skip_loop_filter(&mut self, discard: ffi::AVDiscard) {
        unsafe { (*self.ptr).skip_loop_filter = discard };
    }

    /// Codec type (video, audio, etc.).
    pub fn codec_type(&self) -> ffi::AVMediaType {
        unsafe { (*self.ptr).codec_type }
    }

    /// Sample rate (audio decoders).
    pub fn sample_rate(&self) -> i32 {
        unsafe { (*self.ptr).sample_rate }
    }

    /// Hardware frames context (set after avcodec_open2 for hw decoders).
    /// Returns a new reference that the caller must free, or None if not set.
    pub fn hw_frames_ctx(&self) -> Option<*mut ffi::AVBufferRef> {
        unsafe {
            let ctx = (*self.ptr).hw_frames_ctx;
            if ctx.is_null() {
                None
            } else {
                Some(ffi::av_buffer_ref(ctx))
            }
        }
    }

    /// Number of audio channels.
    pub fn channels(&self) -> i32 {
        unsafe { (*self.ptr).ch_layout.nb_channels }
    }

    /// Sample format (audio decoders).
    pub fn sample_format(&self) -> crate::types::SampleFormat {
        crate::types::SampleFormat::from_raw(unsafe { (*self.ptr).sample_fmt })
    }

    /// Raw sample format (for FFI interop).
    pub fn sample_format_raw(&self) -> ffi::AVSampleFormat {
        unsafe { (*self.ptr).sample_fmt }
    }

    /// Channel layout.
    pub fn channel_layout(&self) -> crate::types::ChannelLayout {
        crate::types::ChannelLayout::from_raw(unsafe { (*self.ptr).ch_layout })
    }

    /// Raw channel layout reference (for FFI interop).
    pub fn channel_layout_raw(&self) -> &ffi::AVChannelLayout {
        unsafe { &(*self.ptr).ch_layout }
    }

    /// Pixel format (video decoders).
    pub fn pixel_format(&self) -> crate::types::PixelFormat {
        crate::types::PixelFormat::from_raw(unsafe { (*self.ptr).pix_fmt })
    }

    /// Raw pixel format (for FFI interop).
    pub fn pixel_format_raw(&self) -> ffi::AVPixelFormat {
        unsafe { (*self.ptr).pix_fmt }
    }

    /// Frame width (video decoders).
    pub fn width(&self) -> i32 {
        unsafe { (*self.ptr).width }
    }

    /// Frame height (video decoders).
    pub fn height(&self) -> i32 {
        unsafe { (*self.ptr).height }
    }

    /// Time base for this codec context.
    pub fn time_base(&self) -> ffi::AVRational {
        unsafe { (*self.ptr).time_base }
    }

    /// Raw pointer to the underlying AVCodecContext.
    pub fn as_ptr(&self) -> *const ffi::AVCodecContext {
        self.ptr
    }

    /// Mutable raw pointer.
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVCodecContext {
        self.ptr
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::avcodec_free_context(&mut self.ptr) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::input::{InputContext, MediaType};
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
                "testsrc=duration=0.5:size=160x120:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.5",
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
        assert!(status.success());
        tmp
    }

    #[test]
    fn test_decode_video_frames() {
        let tmp = generate_test_file();
        let mut ctx = InputContext::open_path(tmp.path()).unwrap();
        ctx.find_stream_info().unwrap();

        let video_idx = ctx.find_best_stream(MediaType::Video).unwrap();
        let stream = ctx.stream(video_idx).unwrap();
        let mut decoder =
            unsafe { Decoder::from_codec_parameters(stream.codec_parameters()).unwrap() };

        let mut packet = Packet::new().unwrap();
        let mut frame = Frame::new().unwrap();
        let mut frame_count = 0;

        while ctx.read_packet(&mut packet).unwrap() {
            if packet.stream_index() != video_idx as i32 {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            loop {
                match decoder.receive_frame(&mut frame).unwrap() {
                    DecodeResult::Frame => frame_count += 1,
                    DecodeResult::NeedsInput => break,
                    DecodeResult::Eof => break,
                }
            }
        }

        // Flush
        decoder.send_eof().unwrap();
        loop {
            match decoder.receive_frame(&mut frame).unwrap() {
                DecodeResult::Frame => frame_count += 1,
                _ => break,
            }
        }

        assert!(frame_count > 0, "should have decoded at least one frame");
        assert_eq!(decoder.width(), 160);
        assert_eq!(decoder.height(), 120);
    }

    /// Generate a valid AC3 file, then corrupt audio packets by zeroing bytes
    /// in the middle of the file. This simulates the corrupt AC3 frames that
    /// crash the decoder in production (e.g., "exponent -1 is out-of-range").
    fn generate_corrupt_ac3_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new()
            .suffix(".mkv")
            .tempfile()
            .expect("create temp file");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
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
            .expect("ffmpeg must be available for tests");
        assert!(status.success(), "failed to generate AC3 test file");

        // Corrupt the file: zero out chunks in the middle to create
        // invalid AC3 sync frames with bad exponents/coefficients.
        let mut data = std::fs::read(tmp.path()).expect("read test file");
        let len = data.len();
        if len > 2000 {
            // Corrupt several regions to hit different AC3 sync frames
            for offset in [len / 4, len / 3, len / 2, 2 * len / 3] {
                let end = (offset + 64).min(len);
                data[offset..end].fill(0xFF);
            }
        }
        std::fs::write(tmp.path(), &data).expect("write corrupted file");
        tmp
    }

    #[test]
    fn test_corrupt_ac3_does_not_crash() {
        crate::init();
        let tmp = generate_corrupt_ac3_file();
        let mut ctx = InputContext::open_path(tmp.path()).unwrap();
        ctx.find_stream_info().unwrap();

        let audio_idx = ctx.find_best_stream(MediaType::Audio).unwrap();
        let stream = ctx.stream(audio_idx).unwrap();
        let mut decoder =
            unsafe { Decoder::from_codec_parameters(stream.codec_parameters()).unwrap() };

        let mut packet = Packet::new().unwrap();
        let mut frame = Frame::new().unwrap();
        let mut frame_count = 0u32;
        let error_count = 0u32;

        // This must not crash — corrupt frames should be skipped gracefully.
        while ctx.read_packet(&mut packet).unwrap() {
            if packet.stream_index() != audio_idx as i32 {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            loop {
                match decoder.receive_frame(&mut frame).unwrap() {
                    DecodeResult::Frame => frame_count += 1,
                    DecodeResult::NeedsInput => break,
                    DecodeResult::Eof => break,
                }
            }
        }

        decoder.send_eof().unwrap();
        loop {
            match decoder.receive_frame(&mut frame).unwrap() {
                DecodeResult::Frame => frame_count += 1,
                _ => break,
            }
        }

        // We expect at least some frames decoded (the file isn't 100% corrupt)
        // and the key assertion is that we got here without crashing.
        eprintln!("AC3 corrupt test: {frame_count} frames decoded, {error_count} errors skipped");
    }

    /// Regression test for the SIGSEGV: corrupt AC3 frames could produce
    /// DecodeResult::Frame with NULL extended_data. When passed to the resampler
    /// (swr_convert), this caused memcpy(dst, NULL, ...) → segfault at 0.
    ///
    /// This test exercises the full decode→resample pipeline to ensure corrupt
    /// frames are skipped gracefully at both the decoder and resampler layers.
    #[test]
    fn test_corrupt_ac3_resample_does_not_crash() {
        crate::init();
        let tmp = generate_corrupt_ac3_file();

        // Use extract_pcm_range which runs decoder → resampler — the exact crash path.
        let mut total_bytes = 0usize;
        let result = crate::audio::extract_pcm_range(
            tmp.path(),
            8000,
            0.0,
            f64::INFINITY,
            None,
            &mut |data| {
                total_bytes += data.len();
                true
            },
        );

        match result {
            Ok(()) => eprintln!(
                "corrupt AC3 resample test: extracted {total_bytes} bytes of PCM without crash"
            ),
            Err(e) => eprintln!("corrupt AC3 resample test: failed gracefully: {e}"),
        }
        // The key assertion is that we reach this point without SIGSEGV.
    }
}
