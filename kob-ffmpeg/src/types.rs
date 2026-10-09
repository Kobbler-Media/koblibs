//! Rust-native types wrapping FFmpeg constants.
//!
//! These eliminate the need for consumers to import `kob_ffmpeg::ffi` directly.

use crate::ffi;

// --- PixelFormat ---

/// Video pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelFormat(pub(crate) ffi::AVPixelFormat);

impl PixelFormat {
    pub const YUV420P: Self = Self(ffi::AV_PIX_FMT_YUV420P);
    pub const YUV444P: Self = Self(ffi::AV_PIX_FMT_YUV444P);
    pub const NV12: Self = Self(ffi::AV_PIX_FMT_NV12);
    pub const RGB24: Self = Self(ffi::AV_PIX_FMT_RGB24);
    pub const VAAPI: Self = Self(ffi::AV_PIX_FMT_VAAPI);

    pub fn from_raw(raw: ffi::AVPixelFormat) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> ffi::AVPixelFormat {
        self.0
    }

    /// Human-readable name (e.g. "yuv420p").
    pub fn name(self) -> String {
        unsafe {
            let ptr = ffi::av_get_pix_fmt_name(self.0);
            if ptr.is_null() {
                format!("unknown({})", self.0)
            } else {
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }
}

// --- SampleFormat ---

/// Audio sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleFormat(pub(crate) ffi::AVSampleFormat);

impl SampleFormat {
    pub const S16: Self = Self(ffi::AV_SAMPLE_FMT_S16);
    pub const S32: Self = Self(ffi::AV_SAMPLE_FMT_S32);
    pub const FLT: Self = Self(ffi::AV_SAMPLE_FMT_FLT);
    pub const FLTP: Self = Self(ffi::AV_SAMPLE_FMT_FLTP);
    pub const S16P: Self = Self(ffi::AV_SAMPLE_FMT_S16P);

    pub fn from_raw(raw: ffi::AVSampleFormat) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> ffi::AVSampleFormat {
        self.0
    }

    /// Bytes per sample for this format.
    pub fn bytes_per_sample(self) -> usize {
        unsafe { ffi::av_get_bytes_per_sample(self.0) as usize }
    }

    /// Whether this is a planar format.
    pub fn is_planar(self) -> bool {
        unsafe { ffi::av_sample_fmt_is_planar(self.0) != 0 }
    }

    /// Filter-graph string name for this sample format (`"fltp"`, `"s16"`,
    /// etc.). Used to build `aformat=sample_fmts=...` strings where the
    /// numeric AVSampleFormat enum value isn't accepted.
    pub fn name(self) -> &'static str {
        unsafe {
            let p = ffi::av_get_sample_fmt_name(self.0);
            if p.is_null() {
                return "none";
            }
            // av_get_sample_fmt_name returns pointers to static strings, safe
            // to convert to &'static str.
            std::ffi::CStr::from_ptr(p).to_str().unwrap_or("none")
        }
    }
}

// --- ChannelLayout ---

/// Audio channel layout.
pub struct ChannelLayout(pub(crate) ffi::AVChannelLayout);

impl std::fmt::Debug for ChannelLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelLayout")
            .field("channels", &self.0.nb_channels)
            .finish()
    }
}

impl ChannelLayout {
    /// Mono (1 channel).
    pub fn mono() -> Self {
        let mut layout = ffi::AVChannelLayout::default();
        unsafe { ffi::av_channel_layout_default(&mut layout, 1) };
        Self(layout)
    }

    /// Stereo (2 channels).
    pub fn stereo() -> Self {
        let mut layout = ffi::AVChannelLayout::default();
        unsafe { ffi::av_channel_layout_default(&mut layout, 2) };
        Self(layout)
    }

    /// Default layout for a given number of channels.
    pub fn default_for(channels: i32) -> Self {
        let mut layout = ffi::AVChannelLayout::default();
        unsafe { ffi::av_channel_layout_default(&mut layout, channels) };
        Self(layout)
    }

    /// Wrap a raw AVChannelLayout by making a proper copy.
    pub fn from_raw(raw: ffi::AVChannelLayout) -> Self {
        let mut layout = ffi::AVChannelLayout::default();
        unsafe { ffi::av_channel_layout_copy(&mut layout, &raw) };
        Self(layout)
    }

    /// Number of channels.
    pub fn channels(&self) -> i32 {
        self.0.nb_channels
    }

    pub(crate) fn raw(&self) -> &ffi::AVChannelLayout {
        &self.0
    }
}

impl Clone for ChannelLayout {
    fn clone(&self) -> Self {
        let mut layout = ffi::AVChannelLayout::default();
        unsafe { ffi::av_channel_layout_copy(&mut layout, &self.0) };
        Self(layout)
    }
}

impl Drop for ChannelLayout {
    fn drop(&mut self) {
        unsafe { ffi::av_channel_layout_uninit(&mut self.0) };
    }
}

// --- TimeBase ---

/// Rational time base (num/den).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBase {
    pub num: i32,
    pub den: i32,
}

impl TimeBase {
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Standard microsecond time base (1/1000000).
    pub const MICROSECOND: Self = Self {
        num: 1,
        den: 1_000_000,
    };

    /// Convert a timestamp in this time base to seconds.
    pub fn to_seconds(self, timestamp: i64) -> f64 {
        timestamp as f64 * self.num as f64 / self.den as f64
    }

    /// Convert seconds to a timestamp in this time base.
    pub fn from_seconds(self, seconds: f64) -> i64 {
        (seconds * self.den as f64 / self.num as f64) as i64
    }

    pub(crate) fn to_av(self) -> ffi::AVRational {
        ffi::AVRational {
            num: self.num,
            den: self.den,
        }
    }

    pub(crate) fn from_av(r: ffi::AVRational) -> Self {
        Self {
            num: r.num,
            den: r.den,
        }
    }
}

impl From<ffi::AVRational> for TimeBase {
    fn from(r: ffi::AVRational) -> Self {
        Self::from_av(r)
    }
}
