//! Video scaling and pixel format conversion wrapper (SwsContext).

use crate::Result;
use crate::ffi;

/// Owns a `SwsContext` for video scaling and pixel format conversion.
///
/// `Send` but not `Sync`.
pub struct Scaler {
    ptr: *mut ffi::SwsContext,
    dst_width: i32,
    dst_height: i32,
}

unsafe impl Send for Scaler {}

impl Scaler {
    /// Create a scaler/converter.
    pub fn new(
        src_width: i32,
        src_height: i32,
        src_format: ffi::AVPixelFormat,
        dst_width: i32,
        dst_height: i32,
        dst_format: ffi::AVPixelFormat,
        flags: i32,
    ) -> Result<Self> {
        let ptr = unsafe {
            ffi::sws_getContext(
                src_width,
                src_height,
                src_format,
                dst_width,
                dst_height,
                dst_format,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!(
                    "creating scaler {}x{} -> {}x{}",
                    src_width, src_height, dst_width, dst_height
                ),
            ));
        }
        Ok(Self {
            ptr,
            dst_width,
            dst_height,
        })
    }

    /// Create a scaler with bilinear interpolation (common default).
    pub fn bilinear(
        src_width: i32,
        src_height: i32,
        src_format: ffi::AVPixelFormat,
        dst_width: i32,
        dst_height: i32,
        dst_format: ffi::AVPixelFormat,
    ) -> Result<Self> {
        Self::new(
            src_width,
            src_height,
            src_format,
            dst_width,
            dst_height,
            dst_format,
            ffi::SWS_BILINEAR as i32,
        )
    }

    /// Scale/convert a frame.
    ///
    /// `src_slice` and `src_stride` come from the source AVFrame's data/linesize arrays.
    /// `dst_slice` and `dst_stride` come from the destination AVFrame's data/linesize arrays.
    pub fn scale(
        &self,
        src_slice: &[*const u8],
        src_stride: &[i32],
        src_slice_y: i32,
        src_slice_height: i32,
        dst_slice: &[*mut u8],
        dst_stride: &[i32],
    ) -> i32 {
        unsafe {
            ffi::sws_scale(
                self.ptr,
                src_slice.as_ptr(),
                src_stride.as_ptr(),
                src_slice_y,
                src_slice_height,
                dst_slice.as_ptr(),
                dst_stride.as_ptr(),
            )
        }
    }

    /// Scale a complete AVFrame to another AVFrame.
    pub fn scale_frame(
        &self,
        src: &crate::frame::Frame,
        dst: &mut crate::frame::Frame,
    ) -> Result<()> {
        let ret = unsafe { ffi::sws_scale_frame(self.ptr, dst.as_mut_ptr(), src.as_ptr()) };
        if ret < 0 {
            return Err(crate::error::FfmpegError::new(ret, "scaling frame"));
        }
        Ok(())
    }

    pub fn dst_width(&self) -> i32 {
        self.dst_width
    }

    pub fn dst_height(&self) -> i32 {
        self.dst_height
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.ptr) };
    }
}
