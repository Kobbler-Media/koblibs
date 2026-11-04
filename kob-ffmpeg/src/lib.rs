// FFmpeg decode/encode loops use loop{match} with ? which clippy wants as while-let,
// but the explicit match is clearer for error handling in these patterns.
#![allow(clippy::while_let_loop)]

//! Safe Rust wrappers and high-level operations for FFmpeg libraries.
//!
//! # Quick start
//!
//! ```ignore
//! use kob_ffmpeg::{probe, ProbeResult};
//!
//! let result = probe::probe(path)?;
//! println!("{} video streams", result.video_streams.len());
//! ```
//!
//! # Architecture
//!
//! - **Types** (`types`): Rust-native wrappers for FFmpeg pixel formats, sample formats,
//!   channel layouts, and time bases. Use these instead of raw FFI types.
//! - **Low-level wrappers** (`format`, `codec`, `filter`, `hw`, `frame`, `packet`):
//!   Safe RAII types around FFmpeg C structures.
//! - **High-level modules** (`probe`, `transcode`, `audio`, `remux`, etc.):
//!   Task-oriented APIs for common media operations.

// Core types, errors, progress
pub mod error;
pub mod progress;
pub mod types;

// Low-level wrappers
pub mod bsf;
pub mod codec;
pub mod filter;
pub mod format;
pub mod frame;
pub mod hw;
pub mod packet;
pub mod resample;
pub mod scale;

// Internal helpers shared across modules (capture scheduling, etc.)
pub(crate) mod fps_grid;

// High-level operations
pub mod audio;
pub mod chromaprint;
pub mod detect;
#[cfg(feature = "hybrid-io")]
pub mod hybrid_io;
pub mod interlace;
pub mod iptv;
pub mod pcm_stream;
pub mod probe;
pub mod remux;
pub mod session;
pub mod single_pass;
pub mod streaming;
pub mod subtitles;
pub mod thumbnails;
pub mod transcode;

/// Re-export the sys crate for advanced usage requiring direct FFI access.
pub use kob_ffmpeg_sys as ffi;

// --- Convenience re-exports ---

// Error types
pub use error::{ErrorKind, FfmpegError};

// Result alias
pub type Result<T> = std::result::Result<T, FfmpegError>;

// Common types
pub use types::{ChannelLayout, PixelFormat, SampleFormat, TimeBase};

// Probe
pub use probe::{ProbeOptions, ProbeResult};

// Transcode
pub use transcode::{HlsConfig, HlsWindow};

// Format I/O
pub use format::input::MediaType;

// Progress/cancellation
pub use progress::{Progress, RunContext};

use std::sync::Once;

static FFMPEG_INIT: Once = Once::new();

/// Custom av_log callback that routes FFmpeg's C-level logging through Rust's
/// tracing framework. This replaces the default callback which writes to stderr.
///
/// Crucially, this prevents FFmpeg from triggering abort() through its default
/// fatal/panic log path, which can crash the process when decoders (especially
/// AC3) hit corrupt data.
unsafe extern "C" fn ffmpeg_log_callback(
    ptr: *mut std::os::raw::c_void,
    level: std::os::raw::c_int,
    fmt: *const std::os::raw::c_char,
    vl: *mut ffi::__va_list_tag,
) {
    // Filter by the globally configured log level (same as default callback).
    if level > unsafe { ffi::av_log_get_level() } {
        return;
    }

    // Format the message using FFmpeg's own formatter.
    let mut buf = [0u8; 1024];
    let mut prefix: std::os::raw::c_int = 1;
    unsafe {
        ffi::av_log_format_line2(
            ptr,
            level,
            fmt,
            vl,
            buf.as_mut_ptr() as *mut std::os::raw::c_char,
            buf.len() as std::os::raw::c_int,
            &mut prefix,
        );
    }

    // Convert to a Rust string (lossy — FFmpeg messages are almost always ASCII).
    let msg = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const _) }.to_string_lossy();
    let msg = msg.trim();
    if msg.is_empty() {
        return;
    }

    // Route through tracing at the appropriate level. AV_LOG_PANIC and
    // AV_LOG_FATAL are logged as errors — we never abort.
    match level as u32 {
        l if l <= ffi::AV_LOG_ERROR => tracing::warn!(target: "ffmpeg", "{msg}"),
        _ => tracing::debug!(target: "ffmpeg", "{msg}"),
    }
}

/// Initialize FFmpeg global state. Safe to call multiple times — only executes once.
pub fn init() {
    FFMPEG_INIT.call_once(|| {
        unsafe {
            ffi::avformat_network_init();
            // ERROR level suppresses harmless warnings like "Timestamps are unset"
            // (handled by write_packet_rescaled) and "deprecated pixel format"
            // (handled by using yuv420p + JPEG range). Real errors still surface.
            ffi::av_log_set_level(ffi::AV_LOG_ERROR as i32);
            // Install our custom callback to route through tracing and prevent
            // the default callback from triggering abort on fatal messages.
            ffi::av_log_set_callback(Some(ffmpeg_log_callback));
        }
        tracing::debug!("FFmpeg initialized");
    });
}
