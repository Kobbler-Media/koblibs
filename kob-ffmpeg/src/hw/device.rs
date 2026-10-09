//! Hardware device context wrapper.
//!
//! Manages GPU device contexts for hardware-accelerated encoding/decoding.

use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ffi;
use crate::{Result, ff_check};

/// Hardware acceleration type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwAccelType {
    Vaapi,
    Cuda,
    None,
}

impl HwAccelType {
    fn to_av(self) -> ffi::AVHWDeviceType {
        match self {
            Self::Vaapi => ffi::AV_HWDEVICE_TYPE_VAAPI,
            Self::Cuda => ffi::AV_HWDEVICE_TYPE_CUDA,
            Self::None => ffi::AV_HWDEVICE_TYPE_NONE,
        }
    }

    pub fn from_av(av: ffi::AVHWDeviceType) -> Self {
        match av {
            ffi::AV_HWDEVICE_TYPE_VAAPI => Self::Vaapi,
            ffi::AV_HWDEVICE_TYPE_CUDA => Self::Cuda,
            _ => Self::None,
        }
    }
}

/// Owns a hardware device context (`AVBufferRef*` pointing to `AVHWDeviceContext`).
///
/// Thread-safe: `Send + Sync` because `AVBufferRef` is internally reference-counted.
/// Designed to be wrapped in `Arc` and shared across transcode sessions.
pub struct HwDeviceContext {
    buf_ref: *mut ffi::AVBufferRef,
    device_type: HwAccelType,
}

impl std::fmt::Debug for HwDeviceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwDeviceContext")
            .field("device_type", &self.device_type)
            .finish()
    }
}

// SAFETY: AVBufferRef is reference-counted and the operations we use
// (av_buffer_ref/av_buffer_unref) are thread-safe.
unsafe impl Send for HwDeviceContext {}
unsafe impl Sync for HwDeviceContext {}

impl HwDeviceContext {
    /// Create a hardware device context.
    ///
    /// `device` is the device path (e.g., "/dev/dri/renderD128" for VAAPI)
    /// or None for the default device.
    pub fn new(hw_type: HwAccelType, device: Option<&str>) -> Result<Self> {
        let mut buf_ref: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let device_cstr = match device {
            Some(d) => Some(CString::new(d).map_err(|_| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    "device path contains null byte",
                )
            })?),
            None => None,
        };
        let device_ptr = device_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        unsafe {
            ff_check!(
                ffi::av_hwdevice_ctx_create(
                    &mut buf_ref,
                    hw_type.to_av(),
                    device_ptr,
                    std::ptr::null_mut(),
                    0,
                ),
                "creating {:?} device context",
                hw_type
            )?;
        }

        Ok(Self {
            buf_ref,
            device_type: hw_type,
        })
    }

    /// Create a VAAPI device context with the default render device.
    pub fn vaapi() -> Result<Self> {
        Self::new(HwAccelType::Vaapi, None)
    }

    /// Create a VAAPI device context with a specific device path.
    pub fn vaapi_device(path: &str) -> Result<Self> {
        Self::new(HwAccelType::Vaapi, Some(path))
    }

    /// Create a CUDA device context.
    pub fn cuda() -> Result<Self> {
        Self::new(HwAccelType::Cuda, None)
    }

    /// Get a new reference to the underlying buffer.
    /// This increments the reference count.
    pub fn buffer_ref(&self) -> *mut ffi::AVBufferRef {
        unsafe { ffi::av_buffer_ref(self.buf_ref) }
    }

    /// Device type.
    pub fn device_type(&self) -> HwAccelType {
        self.device_type
    }

    /// Raw pointer to the AVBufferRef.
    pub fn as_ptr(&self) -> *mut ffi::AVBufferRef {
        self.buf_ref
    }

    /// Create a hardware frames context for the given dimensions.
    ///
    /// Returns an initialized `AVBufferRef*` pointing to an `AVHWFramesContext`
    /// configured for CUDA/NV12. Used for both encoder `hw_frames_ctx` (target
    /// dimensions) and filter graph buffersrc (source dimensions) in the
    /// zero-copy GPU transcode pipeline.
    pub fn create_frames_ctx(
        &self,
        width: i32,
        height: i32,
    ) -> crate::Result<*mut ffi::AVBufferRef> {
        unsafe {
            let frames_ref = ffi::av_hwframe_ctx_alloc(self.buf_ref);
            if frames_ref.is_null() {
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "allocating hw frames context",
                ));
            }
            let frames = (*frames_ref).data as *mut ffi::AVHWFramesContext;
            (*frames).format = match self.device_type {
                HwAccelType::Cuda => ffi::AV_PIX_FMT_CUDA,
                HwAccelType::Vaapi => ffi::AV_PIX_FMT_VAAPI,
                HwAccelType::None => ffi::AV_PIX_FMT_NV12,
            };
            (*frames).sw_format = ffi::AV_PIX_FMT_NV12;
            (*frames).width = width;
            (*frames).height = height;
            (*frames).initial_pool_size = 20;

            let ret = ffi::av_hwframe_ctx_init(frames_ref);
            if ret < 0 {
                let mut fr = frames_ref;
                ffi::av_buffer_unref(&mut fr);
                return Err(crate::error::FfmpegError::new(
                    ret,
                    "initializing hw frames context",
                ));
            }
            Ok(frames_ref)
        }
    }
}

impl Clone for HwDeviceContext {
    fn clone(&self) -> Self {
        let buf_ref = unsafe { ffi::av_buffer_ref(self.buf_ref) };
        assert!(
            !buf_ref.is_null(),
            "out of memory cloning HW device context"
        );
        Self {
            buf_ref,
            device_type: self.device_type,
        }
    }
}

impl Drop for HwDeviceContext {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.buf_ref) };
    }
}

/// Shared hardware device context for use across multiple transcode sessions.
pub type SharedHwDevice = Arc<HwDeviceContext>;

/// How to build a hardware device context on demand.
#[derive(Debug, Clone)]
pub struct HwDeviceSpec {
    pub hw_type: HwAccelType,
    pub device: Option<String>,
}

impl HwDeviceSpec {
    pub fn cuda() -> Self {
        Self {
            hw_type: HwAccelType::Cuda,
            device: None,
        }
    }

    pub fn vaapi(device: Option<String>) -> Self {
        Self {
            hw_type: HwAccelType::Vaapi,
            device,
        }
    }

    fn build(&self) -> Result<HwDeviceContext> {
        HwDeviceContext::new(self.hw_type, self.device.as_deref())
    }
}

struct LazyState {
    device: Option<SharedHwDevice>,
    last_used: Option<Instant>,
}

/// Lazily creates and caches a hardware device context, releasing the GPU
/// context after an idle grace period.
///
/// A GPU context (notably a CUDA primary context, ~100MB VRAM) is only built on
/// the first `get()` and kept while transcode sessions are active. `reap_if_idle`
/// drops the cached strong reference once `get()` hasn't been called within the
/// grace window; the underlying context survives until the last `Arc`/FFI clone
/// handed to running sessions is dropped, so VRAM frees only when truly idle.
///
/// A `None` spec means no hardware acceleration: `get()` always returns `None`.
pub struct LazyHwDevice {
    spec: Option<HwDeviceSpec>,
    state: Mutex<LazyState>,
}

impl LazyHwDevice {
    pub fn new(spec: Option<HwDeviceSpec>) -> Self {
        Self {
            spec,
            state: Mutex::new(LazyState {
                device: None,
                last_used: None,
            }),
        }
    }

    pub fn disabled() -> Self {
        Self::new(None)
    }

    /// Get the device context, building it on first use. Returns `None` when no
    /// hardware acceleration is configured or context creation fails.
    ///
    /// The build holds an internal lock, so concurrent first-hits create exactly
    /// one context rather than racing `av_hwdevice_ctx_create`.
    pub fn get(&self) -> Option<SharedHwDevice> {
        let spec = self.spec.as_ref()?;
        let mut state = self.state.lock().unwrap();
        state.last_used = Some(Instant::now());
        if let Some(dev) = &state.device {
            return Some(dev.clone());
        }
        match spec.build() {
            Ok(ctx) => {
                tracing::info!(
                    device_type = ?spec.hw_type,
                    "Lazily created hardware device context"
                );
                let arc = Arc::new(ctx);
                state.device = Some(arc.clone());
                Some(arc)
            }
            Err(e) => {
                tracing::warn!("Failed to create {:?} device context: {e}", spec.hw_type);
                None
            }
        }
    }

    /// Release the cached context when no session still holds it and it has been
    /// idle longer than `grace`. Returns `true` if a context was released.
    ///
    /// While any transcode session holds an `Arc` (strong count > 1) the context
    /// is in active use: the grace timer is refreshed and nothing is released, so
    /// every concurrent session shares the single context. Once the last session
    /// drops its `Arc` (strong count back to 1, the provider's own reference),
    /// the context is released `grace` later — long enough to stay warm across
    /// brief gaps between sessions.
    pub fn reap_if_idle(&self, grace: Duration) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(device) = &state.device else {
            return false;
        };
        if Arc::strong_count(device) > 1 {
            state.last_used = Some(Instant::now());
            return false;
        }
        let idle = state
            .last_used
            .map(|t| t.elapsed() >= grace)
            .unwrap_or(true);
        if idle {
            state.device = None;
            state.last_used = None;
            tracing::info!("Released idle hardware device context");
            return true;
        }
        false
    }
}
