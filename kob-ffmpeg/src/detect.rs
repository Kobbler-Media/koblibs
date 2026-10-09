//! Hardware capability detection.
//!
//! Tests encoder/decoder availability without spawning processes.
//! Replaces the CLI pattern of running `ffmpeg -f lavfi -i nullsrc ... -c:v <encoder> -f null -`.

use std::ffi::CString;

use crate::ffi;
use crate::hw::device::HwDeviceContext;

/// Test if a specific encoder is available.
pub fn encoder_available(name: &str) -> bool {
    let c_name = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let codec = unsafe { ffi::avcodec_find_encoder_by_name(c_name.as_ptr()) };
    !codec.is_null()
}

/// Test if a specific decoder is available.
pub fn decoder_available(name: &str) -> bool {
    let c_name = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let codec = unsafe { ffi::avcodec_find_decoder_by_name(c_name.as_ptr()) };
    !codec.is_null()
}

/// Result of hardware acceleration detection.
#[derive(Debug, Clone)]
pub struct HwCapabilities {
    pub vaapi_available: bool,
    pub vaapi_encoders: Vec<String>,
    pub cuda_available: bool,
    pub nvenc_encoders: Vec<String>,
    pub nvdec_available: bool,
}

/// Detect all hardware acceleration capabilities.
///
/// Replaces the CLI pattern of spawning test encodes with nullsrc.
pub fn detect_hw_capabilities() -> HwCapabilities {
    let vaapi_available = HwDeviceContext::vaapi().is_ok();
    let cuda_available = HwDeviceContext::cuda().is_ok();

    let vaapi_encoders = if vaapi_available {
        ["h264_vaapi", "hevc_vaapi", "av1_vaapi"]
            .iter()
            .filter(|name| test_vaapi_encoder(name))
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let nvenc_encoders = if cuda_available {
        ["h264_nvenc", "hevc_nvenc", "av1_nvenc"]
            .iter()
            .filter(|name| encoder_available(name))
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let nvdec_available = cuda_available; // If CUDA device works, NVDEC is available

    HwCapabilities {
        vaapi_available,
        vaapi_encoders,
        cuda_available,
        nvenc_encoders,
        nvdec_available,
    }
}

/// Test a VAAPI encoder by attempting to open it with a device context.
fn test_vaapi_encoder(name: &str) -> bool {
    let device = match HwDeviceContext::vaapi() {
        Ok(d) => d,
        Err(_) => return false,
    };

    let c_name = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return false,
    };

    unsafe {
        let codec = ffi::avcodec_find_encoder_by_name(c_name.as_ptr());
        if codec.is_null() {
            return false;
        }

        let ctx = ffi::avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return false;
        }

        // Configure minimal parameters for the test
        (*ctx).width = 256;
        (*ctx).height = 256;
        (*ctx).time_base = ffi::AVRational { num: 1, den: 25 };
        (*ctx).pix_fmt = ffi::AV_PIX_FMT_VAAPI;

        // Attach HW device context
        (*ctx).hw_device_ctx = device.buffer_ref();

        let result = ffi::avcodec_open2(ctx, codec, std::ptr::null_mut());
        ffi::avcodec_free_context(&mut { ctx });

        result >= 0
    }
}

/// Check if chromaprint is available (linked via libchromaprint).
pub fn chromaprint_available() -> bool {
    let ctx = unsafe { ffi::chromaprint_new(ffi::CHROMAPRINT_ALGORITHM_DEFAULT as i32) };
    if ctx.is_null() {
        return false;
    }
    unsafe { ffi::chromaprint_free(ctx) };
    true
}
