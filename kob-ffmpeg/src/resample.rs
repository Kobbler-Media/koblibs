//! Audio resampling wrapper (SwrContext).
//!
//! Handles sample rate conversion, sample format conversion, and channel layout remixing.

use crate::ffi;
use crate::{Result, ff_check};

/// Owns a `SwrContext` for audio resampling/conversion.
///
/// `Send` but not `Sync`.
pub struct Resampler {
    ptr: *mut ffi::SwrContext,
    in_channels: i32,
    out_sample_format: ffi::AVSampleFormat,
    out_channels: i32,
}

unsafe impl Send for Resampler {}

/// Configuration for creating a resampler.
pub struct ResampleConfig {
    pub in_sample_rate: i32,
    pub in_sample_format: crate::types::SampleFormat,
    pub in_channel_layout: crate::types::ChannelLayout,
    pub out_sample_rate: i32,
    pub out_sample_format: crate::types::SampleFormat,
    pub out_channel_layout: crate::types::ChannelLayout,
}

impl Resampler {
    /// Create and initialize a resampler.
    pub fn new(config: ResampleConfig) -> Result<Self> {
        let mut ptr: *mut ffi::SwrContext = std::ptr::null_mut();
        unsafe {
            ff_check!(
                ffi::swr_alloc_set_opts2(
                    &mut ptr,
                    config.out_channel_layout.raw(),
                    config.out_sample_format.raw(),
                    config.out_sample_rate,
                    config.in_channel_layout.raw(),
                    config.in_sample_format.raw(),
                    config.in_sample_rate,
                    0,
                    std::ptr::null_mut(),
                ),
                "allocating resampler"
            )?;

            if ptr.is_null() {
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "swr_alloc_set_opts2 returned null",
                ));
            }

            ff_check!(ffi::swr_init(ptr), "initializing resampler")?;
        }

        Ok(Self {
            ptr,
            in_channels: config.in_channel_layout.channels(),
            out_sample_format: config.out_sample_format.raw(),
            out_channels: config.out_channel_layout.channels(),
        })
    }

    /// Convert audio samples.
    ///
    /// `input` is the raw input data (interleaved or planar depending on format).
    /// Returns the converted output data as a `Vec<u8>`.
    ///
    /// For simple cases (same channel count, known formats), this handles
    /// buffer allocation internally.
    /// # Safety
    /// `in_data` and `out_data` must point to valid sample buffers.
    pub unsafe fn convert_frame(
        &mut self,
        in_data: *const *const u8,
        in_samples: i32,
        out_data: *mut *mut u8,
        out_samples: i32,
    ) -> Result<i32> {
        let ret = unsafe { ffi::swr_convert(self.ptr, out_data, out_samples, in_data, in_samples) };
        ff_check!(ret, "resampling audio")?;
        Ok(ret)
    }

    /// Convert a complete AVFrame, writing resampled output to the output frame.
    /// Convert an input audio frame and return the resampled data as raw bytes (interleaved s16le).
    ///
    /// This uses `swr_convert` with the frame's raw data pointers, which is more
    /// reliable than `swr_convert_frame` across FFmpeg versions.
    pub fn convert_frame_to_bytes(&mut self, input: &crate::frame::Frame) -> Result<Vec<u8>> {
        let in_samples = input.nb_samples();
        if in_samples <= 0 {
            return Ok(Vec::new());
        }

        // Estimate output samples (may be more due to resampling ratio)
        let out_samples = unsafe { ffi::swr_get_out_samples(self.ptr, in_samples) };
        if out_samples <= 0 {
            return Ok(Vec::new());
        }

        // Allocate output buffer based on actual configured format
        let out_bytes_per_sample =
            unsafe { ffi::av_get_bytes_per_sample(self.out_sample_format) as usize };
        if out_bytes_per_sample == 0 {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                "unknown output sample format",
            ));
        }
        let out_channels = self.out_channels as usize;
        let mut out_buf = vec![0u8; out_samples as usize * out_bytes_per_sample * out_channels];
        let out_ptr = out_buf.as_mut_ptr();

        let in_data = unsafe { (*input.as_ptr()).extended_data };
        if in_data.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                "frame has NULL extended_data (corrupt decode) — skipping resample",
            ));
        }

        // Verify the frame's channel count matches what this resampler expects.
        // AC3 streams commonly declare 5.1 in the container but decode initial frames
        // as stereo — if the resampler is configured for 6ch but the frame has 2ch,
        // swr_convert reads extended_data[2..5] which are NULL → segfault.
        let frame_channels = unsafe { (*input.as_ptr()).ch_layout.nb_channels };
        if frame_channels != self.in_channels {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!(
                    "frame has {frame_channels} channels but resampler expects {} — \
                     rebuild resampler from actual frame properties",
                    self.in_channels
                ),
            ));
        }

        let converted = unsafe {
            ffi::swr_convert(
                self.ptr,
                &out_ptr,
                out_samples,
                in_data as *const *const u8,
                in_samples,
            )
        };

        if converted < 0 {
            return Err(crate::error::FfmpegError::new(
                converted,
                "resampling audio",
            ));
        }

        out_buf.truncate(converted as usize * out_bytes_per_sample * out_channels);
        Ok(out_buf)
    }

    /// Flush any remaining samples from the resampler.
    /// Pass NULL input to get buffered output.
    /// # Safety
    /// `out_data` must point to a valid output buffer.
    pub unsafe fn flush(&mut self, out_data: *mut *mut u8, out_samples: i32) -> Result<i32> {
        let ret =
            unsafe { ffi::swr_convert(self.ptr, out_data, out_samples, std::ptr::null_mut(), 0) };
        ff_check!(ret, "flushing resampler")?;
        Ok(ret)
    }

    pub fn as_ptr(&self) -> *const ffi::SwrContext {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut ffi::SwrContext {
        self.ptr
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe { ffi::swr_free(&mut self.ptr) };
    }
}
