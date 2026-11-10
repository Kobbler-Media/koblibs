//! FFmpeg error types with pattern-matchable error kinds.

use std::ffi::CStr;
use std::fmt;

use kob_ffmpeg_sys as ffi;

/// Classifies FFmpeg errors for pattern matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// File or URL not found / could not be opened.
    NotFound,
    /// No matching stream found (video, audio, subtitle).
    NoStream,
    /// Decoder not found for the codec.
    DecoderNotFound,
    /// Encoder not found by name.
    EncoderNotFound,
    /// Invalid data in the stream or file.
    InvalidData,
    /// End of file / stream.
    Eof,
    /// Resource temporarily unavailable (try again).
    Again,
    /// Operation was cancelled via CancellationToken.
    Cancelled,
    /// Out of memory.
    OutOfMemory,
    /// Invalid argument (null byte in string, bad parameter).
    InvalidArgument,
    /// I/O error (disk full, permission denied, network timeout).
    Io,
    /// Muxer not found for output format.
    MuxerNotFound,
    /// Filter not found.
    FilterNotFound,
    /// Other FFmpeg error with raw AVERROR code.
    Other(i32),
}

impl ErrorKind {
    fn from_averror(code: i32) -> Self {
        match code {
            ffi::AVERROR_EOF => Self::Eof,
            ffi::AVERROR_EAGAIN => Self::Again,
            ffi::AVERROR_ENOMEM => Self::OutOfMemory,
            ffi::AVERROR_EINVAL => Self::InvalidArgument,
            ffi::AVERROR_DECODER_NOT_FOUND => Self::DecoderNotFound,
            ffi::AVERROR_ENCODER_NOT_FOUND => Self::EncoderNotFound,
            ffi::AVERROR_DEMUXER_NOT_FOUND | ffi::AVERROR_MUXER_NOT_FOUND => Self::MuxerNotFound,
            ffi::AVERROR_STREAM_NOT_FOUND => Self::NoStream,
            ffi::AVERROR_INVALIDDATA => Self::InvalidData,
            ffi::AVERROR_FILTER_NOT_FOUND => Self::FilterNotFound,
            _ => Self::Other(code),
        }
    }
}

/// FFmpeg error with a classifiable kind and context string.
#[derive(Debug)]
pub struct FfmpegError {
    kind: ErrorKind,
    code: i32,
    context: String,
}

impl FfmpegError {
    /// Create an error from an AVERROR code and context description.
    pub fn new(code: i32, context: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::from_averror(code),
            code,
            context: context.into(),
        }
    }

    /// Create a cancelled error.
    pub fn cancelled(context: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Cancelled,
            code: ffi::AVERROR_EXIT,
            context: context.into(),
        }
    }

    /// The error kind for pattern matching.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The raw AVERROR code.
    pub fn code(&self) -> i32 {
        self.code
    }

    /// The context string describing what operation failed.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// FFmpeg's description of the error code.
    pub fn av_description(&self) -> String {
        let mut buf = [0u8; 256];
        unsafe {
            ffi::av_strerror(self.code, buf.as_mut_ptr() as *mut std::os::raw::c_char, buf.len());
        }
        CStr::from_bytes_until_nul(&buf)
            .unwrap_or(c"Unknown error")
            .to_string_lossy()
            .into_owned()
    }

    pub fn is_eof(&self) -> bool {
        self.kind == ErrorKind::Eof
    }

    pub fn is_again(&self) -> bool {
        self.kind == ErrorKind::Again
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == ErrorKind::Cancelled
    }
}

impl fmt::Display for FfmpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.av_description())
    }
}

impl std::error::Error for FfmpegError {}

/// Check an FFmpeg return code. Returns `Ok(ret)` if >= 0, `Err(FfmpegError)` if negative.
#[inline]
pub fn ff_check(ret: i32, context: impl Into<String>) -> Result<i32, FfmpegError> {
    if ret < 0 {
        Err(FfmpegError::new(ret, context))
    } else {
        Ok(ret)
    }
}

/// Macro for checking FFmpeg return codes with lazy context string construction.
#[macro_export]
macro_rules! ff_check {
    ($expr:expr, $($arg:tt)*) => {{
        let ret = $expr;
        if ret < 0 {
            Err($crate::error::FfmpegError::new(ret, format!($($arg)*)))
        } else {
            Ok(ret)
        }
    }};
}
