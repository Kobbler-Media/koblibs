//! AVFrame wrapper with RAII resource management.

use crate::ffi;

/// Owns an `AVFrame`. Automatically freed on drop.
///
/// `Send` but not `Sync` — an AVFrame is not thread-safe for concurrent access,
/// but can be moved between threads.
pub struct Frame {
    ptr: *mut ffi::AVFrame,
}

// SAFETY: AVFrame can be sent between threads. Not Sync because concurrent access is unsafe.
unsafe impl Send for Frame {}

impl Frame {
    /// Allocate a new empty frame.
    pub fn new() -> crate::Result<Self> {
        let ptr = unsafe { ffi::av_frame_alloc() };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating frame",
            ));
        }
        Ok(Self { ptr })
    }

    /// Cheap refcount clone via `av_frame_ref`. Returns a new `Frame` whose
    /// buffers refer to the same `AVBufferRef` as `self` — O(1); no pixel copy.
    ///
    /// Used to hand independent `Frame` handles to worker threads, since
    /// `Frame` is `Send` but `!Sync`.
    pub fn clone_ref(&self) -> crate::Result<Self> {
        let mut dst = unsafe { ffi::av_frame_alloc() };
        if dst.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating frame for clone_ref",
            ));
        }
        let rc = unsafe { ffi::av_frame_ref(dst, self.ptr) };
        if rc < 0 {
            unsafe { ffi::av_frame_free(&mut dst) };
            return Err(crate::error::FfmpegError::new(rc, "av_frame_ref"));
        }
        Ok(Self { ptr: dst })
    }

    /// Returns a raw pointer to the underlying AVFrame.
    #[inline]
    pub fn as_ptr(&self) -> *const ffi::AVFrame {
        self.ptr
    }

    /// Returns a mutable raw pointer to the underlying AVFrame.
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVFrame {
        self.ptr
    }

    /// Unreference the frame data, resetting it to an empty state.
    pub fn unref(&mut self) {
        unsafe { ffi::av_frame_unref(self.ptr) };
    }

    /// Presentation timestamp in time_base units.
    #[inline]
    pub fn pts(&self) -> i64 {
        unsafe { (*self.ptr).pts }
    }

    /// Set the presentation timestamp (in time_base units).
    #[inline]
    pub fn set_pts(&mut self, pts: i64) {
        unsafe { (*self.ptr).pts = pts };
    }

    /// Frame width (video only).
    #[inline]
    pub fn width(&self) -> i32 {
        unsafe { (*self.ptr).width }
    }

    /// Frame height (video only).
    #[inline]
    pub fn height(&self) -> i32 {
        unsafe { (*self.ptr).height }
    }

    /// Pixel format (video) or sample format (audio).
    #[inline]
    pub fn format(&self) -> i32 {
        unsafe { (*self.ptr).format }
    }

    /// Number of audio samples per channel in this frame.
    #[inline]
    pub fn nb_samples(&self) -> i32 {
        unsafe { (*self.ptr).nb_samples }
    }

    /// Sample rate for audio frames.
    #[inline]
    pub fn sample_rate(&self) -> i32 {
        unsafe { (*self.ptr).sample_rate }
    }

    /// Whether this frame is a keyframe.
    #[inline]
    pub fn is_keyframe(&self) -> bool {
        unsafe { (*self.ptr).flags & ffi::AV_FRAME_FLAG_KEY as i32 != 0 }
    }

    /// Whether this frame is in a hardware pixel format (e.g., CUDA, VAAPI).
    /// Hardware frames must be downloaded to CPU before software filters or
    /// encoders without hw_frames_ctx can process them.
    #[inline]
    pub fn is_hw_format(&self) -> bool {
        let fmt = self.format();
        fmt == ffi::AV_PIX_FMT_CUDA || fmt == ffi::AV_PIX_FMT_VAAPI
    }

    /// Line size (stride) for the given plane index.
    #[inline]
    pub fn linesize(&self, plane: usize) -> i32 {
        unsafe { (*self.ptr).linesize[plane] }
    }

    /// Raw data pointer for the given plane index. Returns None if null.
    pub fn data_ptr(&self, plane: usize) -> Option<*const u8> {
        unsafe {
            let ptr = (*self.ptr).data[plane];
            if ptr.is_null() { None } else { Some(ptr) }
        }
    }

    /// Whether this audio frame has valid data pointers.
    ///
    /// A corrupt decode can produce a "successful" frame with NULL `extended_data`
    /// (e.g., AC3 with malformed exponents). Passing such a frame to `swr_convert`
    /// causes a NULL-pointer dereference in libc `memcpy`. Callers should check
    /// this before resampling or filtering.
    #[inline]
    pub fn has_audio_data(&self) -> bool {
        unsafe {
            let f = &*self.ptr;
            f.nb_samples > 0 && !f.extended_data.is_null() && !(*f.extended_data).is_null()
        }
    }

    /// Whether this video frame has valid data pointers.
    ///
    /// For software frames, `data[0]` must be non-NULL. Hardware frames
    /// (CUDA/VAAPI) use opaque handles — `data[3]` holds the surface, so
    /// we check the format instead.
    #[inline]
    pub fn has_video_data(&self) -> bool {
        unsafe {
            let f = &*self.ptr;
            if f.width <= 0 || f.height <= 0 {
                return false;
            }
            self.is_hw_format() || !f.data[0].is_null()
        }
    }

    /// Audio data for the given plane as a byte slice.
    /// For planar audio, each plane is one channel. For interleaved, plane 0 has all data.
    pub fn audio_data(&self, plane: usize) -> Option<&[u8]> {
        unsafe {
            let frame = &*self.ptr;
            let ptr = frame.data[plane];
            if ptr.is_null() || frame.nb_samples <= 0 {
                return None;
            }
            let bytes_per_sample = ffi::av_get_bytes_per_sample(frame.format);
            if bytes_per_sample <= 0 {
                return None;
            }
            let is_planar = ffi::av_sample_fmt_is_planar(frame.format) != 0;
            let channels = if is_planar {
                1
            } else {
                frame.ch_layout.nb_channels.max(1)
            };
            let len = (frame.nb_samples as usize)
                .checked_mul(bytes_per_sample as usize)
                .and_then(|n| n.checked_mul(channels as usize))?;
            Some(std::slice::from_raw_parts(ptr, len))
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { ffi::av_frame_free(&mut self.ptr) };
    }
}
