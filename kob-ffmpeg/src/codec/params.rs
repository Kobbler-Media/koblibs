//! Codec parameters wrapper (AVCodecParameters).
//!
//! AVCodecParameters stores codec configuration without opening a codec context.
//! Used for copying codec info between streams, muxers, and demuxers.

use crate::ffi;
use crate::{Result, ff_check};

/// Owns an `AVCodecParameters`. Automatically freed on drop.
pub struct CodecParameters {
    ptr: *mut ffi::AVCodecParameters,
}

unsafe impl Send for CodecParameters {}

impl CodecParameters {
    /// Allocate new empty codec parameters.
    pub fn new() -> Result<Self> {
        let ptr = unsafe { ffi::avcodec_parameters_alloc() };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating codec parameters",
            ));
        }
        Ok(Self { ptr })
    }

    /// Copy parameters from an encoder/decoder context.
    ///
    /// # Safety
    /// `ctx` must point to a valid `AVCodecContext`.
    pub unsafe fn from_context(ctx: *const ffi::AVCodecContext) -> Result<Self> {
        let params = Self::new()?;
        unsafe {
            ff_check!(
                ffi::avcodec_parameters_from_context(params.ptr, ctx),
                "copying parameters from codec context"
            )?;
        }
        Ok(params)
    }

    /// Copy parameters from another AVCodecParameters (e.g., from a stream).
    ///
    /// # Safety
    /// `src` must point to a valid `AVCodecParameters`.
    pub unsafe fn from_raw(src: *const ffi::AVCodecParameters) -> Result<Self> {
        let params = Self::new()?;
        unsafe {
            ff_check!(
                ffi::avcodec_parameters_copy(params.ptr, src),
                "copying codec parameters"
            )?;
        }
        Ok(params)
    }

    /// Codec ID.
    pub fn codec_id(&self) -> ffi::AVCodecID {
        unsafe { (*self.ptr).codec_id }
    }

    /// Codec type (video, audio, etc.).
    pub fn codec_type(&self) -> ffi::AVMediaType {
        unsafe { (*self.ptr).codec_type }
    }

    pub fn as_ptr(&self) -> *const ffi::AVCodecParameters {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVCodecParameters {
        self.ptr
    }
}

impl Drop for CodecParameters {
    fn drop(&mut self) {
        unsafe { ffi::avcodec_parameters_free(&mut self.ptr) };
    }
}
