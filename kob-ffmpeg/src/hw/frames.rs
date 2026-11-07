//! Hardware frames context wrapper.
//!
//! Manages GPU frame pools for hardware-accelerated encoding/decoding.

use crate::ffi;
use crate::frame::Frame;
use crate::hw::device::HwDeviceContext;
use crate::{Result, ff_check};

/// Owns a hardware frames context (`AVBufferRef*` pointing to `AVHWFramesContext`).
pub struct HwFramesContext {
    buf_ref: *mut ffi::AVBufferRef,
}

unsafe impl Send for HwFramesContext {}

impl HwFramesContext {
    /// Create a hardware frames context.
    pub fn new(
        device: &HwDeviceContext,
        sw_format: ffi::AVPixelFormat,
        hw_format: ffi::AVPixelFormat,
        width: i32,
        height: i32,
        initial_pool_size: i32,
    ) -> Result<Self> {
        let buf_ref = unsafe { ffi::av_hwframe_ctx_alloc(device.as_ptr()) };
        if buf_ref.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating hw frames context",
            ));
        }

        unsafe {
            let frames_ctx = (*buf_ref).data as *mut ffi::AVHWFramesContext;
            (*frames_ctx).format = hw_format;
            (*frames_ctx).sw_format = sw_format;
            (*frames_ctx).width = width;
            (*frames_ctx).height = height;
            (*frames_ctx).initial_pool_size = initial_pool_size;

            ff_check!(
                ffi::av_hwframe_ctx_init(buf_ref),
                "initializing hw frames context"
            )?;
        }

        Ok(Self { buf_ref })
    }

    /// Get a reference to the underlying buffer (increments refcount).
    pub fn buffer_ref(&self) -> *mut ffi::AVBufferRef {
        unsafe { ffi::av_buffer_ref(self.buf_ref) }
    }

    /// Transfer a frame from GPU to CPU memory, keeping its timestamps, colour
    /// tags and side data (Dolby Vision RPU included).
    pub fn download(hw_frame: &Frame, sw_frame: &mut Frame) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_hwframe_transfer_data(sw_frame.as_mut_ptr(), hw_frame.as_ptr(), 0,),
                "downloading frame from GPU"
            )?;
            ff_check!(
                ffi::av_frame_copy_props(sw_frame.as_mut_ptr(), hw_frame.as_ptr()),
                "copying frame properties from GPU frame"
            )?;
        }
        Ok(())
    }

    /// Transfer a frame from CPU to GPU memory.
    pub fn upload(sw_frame: &Frame, hw_frame: &mut Frame) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_hwframe_transfer_data(hw_frame.as_mut_ptr(), sw_frame.as_ptr(), 0,),
                "uploading frame to GPU"
            )?;
        }
        Ok(())
    }

    pub fn as_ptr(&self) -> *mut ffi::AVBufferRef {
        self.buf_ref
    }
}

impl Drop for HwFramesContext {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.buf_ref) };
    }
}
