//! Encoder wrapper (AVCodecContext for encoding).

use std::ffi::CString;
use std::ptr;

use crate::ffi;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ff_check};

/// Default sample format an audio encoder accepts. Different encoders demand
/// different formats: `aac` (native FFmpeg) takes `fltp`, `libfdk_aac` takes
/// `s16` only, `libmp3lame` takes `s16p` or `s32p`. Caller MUST match the
/// encoder context's `sample_fmt` AND any upstream filter graph's output to
/// what the encoder accepts, or `avcodec_open2` returns `EINVAL`.
///
/// Returns the encoder's first/preferred supported sample format. Falls back to
/// `FLTP` if the encoder doesn't advertise supported formats.
pub fn default_audio_sample_format(codec_name: &str) -> Result<crate::types::SampleFormat> {
    let c_name = CString::new(codec_name).map_err(|_| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "codec name contains null byte")
    })?;
    unsafe {
        let codec = ffi::avcodec_find_encoder_by_name(c_name.as_ptr());
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENCODER_NOT_FOUND,
                format!("audio encoder not found: {codec_name}"),
            ));
        }
        // AVCodec.sample_fmts was removed in libavcodec 63 (FFmpeg 8.0);
        // avcodec_get_supported_config (since 7.1) replaces it. A NULL
        // out_num_configs yields an AV_SAMPLE_FMT_NONE-terminated list, matching
        // the old field's semantics.
        let mut fmts: *const ffi::AVSampleFormat = ptr::null();
        let ret = ffi::avcodec_get_supported_config(
            ptr::null(),
            codec,
            ffi::AV_CODEC_CONFIG_SAMPLE_FORMAT,
            0,
            &mut fmts as *mut *const ffi::AVSampleFormat as *mut *const std::ffi::c_void,
            ptr::null_mut(),
        );
        if ret < 0 || fmts.is_null() {
            return Ok(crate::types::SampleFormat::FLTP);
        }
        let first = *fmts;
        if first == ffi::AV_SAMPLE_FMT_NONE {
            return Ok(crate::types::SampleFormat::FLTP);
        }
        Ok(crate::types::SampleFormat::from_raw(first))
    }
}

/// Owns an `AVCodecContext` configured for encoding.
///
/// `Send` but not `Sync` — encoder contexts are not thread-safe.
pub struct Encoder {
    ptr: *mut ffi::AVCodecContext,
}

unsafe impl Send for Encoder {}

/// Result of receiving an encoded packet.
pub enum EncodeResult {
    /// A packet was successfully encoded.
    Packet,
    /// The encoder needs more input (EAGAIN).
    NeedsInput,
    /// End of stream — no more packets will be produced.
    Eof,
}

/// Configuration for creating an audio encoder.
pub struct AudioEncoderConfig {
    pub codec_name: String,
    pub sample_rate: i32,
    pub channel_layout: crate::types::ChannelLayout,
    pub sample_format: crate::types::SampleFormat,
    pub bit_rate: Option<i64>,
    pub time_base: crate::types::TimeBase,
    /// Set to true if the output format requires global headers (MKV, MP4, etc.).
    /// Use `OutputContext::requires_global_header()` to check.
    pub global_header: bool,
}

/// Configuration for creating a video encoder.
pub struct VideoEncoderConfig {
    pub codec_name: String,
    pub width: i32,
    pub height: i32,
    pub pixel_format: crate::types::PixelFormat,
    pub time_base: crate::types::TimeBase,
    pub bit_rate: Option<i64>,
    pub gop_size: Option<i32>,
    /// Set to true if the output format requires global headers (MKV, MP4, etc.).
    pub global_header: bool,
    /// Quantizer quality (qmin/qmax). Used by constant-quality codecs like mjpeg.
    /// For mjpeg: 1-31, lower is better quality.
    pub quality: Option<i32>,
    /// Hardware frames context for zero-copy GPU encoding (NVENC).
    /// When set, `pix_fmt` is overridden to CUDA. NVENC accepts frames from
    /// any CUDA pool (decoder, scale_cuda) — if the pool differs from this
    /// one, NVENC does a fast GPU-to-GPU copy internally.
    pub hw_frames_ctx: Option<*mut ffi::AVBufferRef>,
    /// Color metadata (BT.709, BT.2020, HDR transfer, etc.). Written into the
    /// encoder's AVCodecContext and from there into the output bitstream's VUI.
    /// Without this, transcoded streams are tagged "unspecified" and HDR/BT.2020
    /// content displays with the wrong colours in browsers.
    pub color: crate::filter::graph::VideoColorInfo,
    /// Source pixel aspect. `None` leaves the encoder's default (unset), which
    /// is right for every square-pixel source. DVB SD is 720x576 at 64:45, so
    /// dropping it renders a 16:9 broadcast as 5:4.
    pub sample_aspect_ratio: Option<(i32, i32)>,
    /// Frames per second, as a rational. Required whenever `time_base` is a
    /// container's clock rather than a frame rate: MPEG-TS is 1/90000, and an
    /// encoder left to infer its rate from that budgets bits as if 90000
    /// frames shared each second, which leaves ~55 bits a frame.
    pub frame_rate: Option<(i32, i32)>,
    /// Codec-private options (`preset`, `tune`, `crf`, …) applied to the
    /// encoder's `priv_data` at open time. Applied last, so a caller entry
    /// overrides the built-in HW-encoder defaults below.
    pub options: Vec<(String, String)>,
}

/// The output frame rate for a stream's `r_frame_rate`, doubled when
/// deinterlacing emits one frame per field. `None` when the stream has none.
pub fn output_frame_rate(
    r_frame_rate: ffi::AVRational,
    frame_per_field: bool,
) -> Option<(i32, i32)> {
    if r_frame_rate.num <= 0 || r_frame_rate.den <= 0 {
        return None;
    }
    let num = if frame_per_field {
        r_frame_rate.num * 2
    } else {
        r_frame_rate.num
    };
    Some((num, r_frame_rate.den))
}

impl Encoder {
    /// Create an audio encoder from configuration.
    pub fn audio(config: AudioEncoderConfig) -> Result<Self> {
        let c_name = CString::new(config.codec_name.as_str()).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "codec name contains null byte")
        })?;
        let codec = unsafe { ffi::avcodec_find_encoder_by_name(c_name.as_ptr()) };
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENCODER_NOT_FOUND,
                format!("encoder not found: {}", config.codec_name),
            ));
        }

        let ptr = unsafe { ffi::avcodec_alloc_context3(codec) };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating encoder context",
            ));
        }

        unsafe {
            (*ptr).sample_rate = config.sample_rate;
            (*ptr).sample_fmt = config.sample_format.raw();
            (*ptr).time_base = config.time_base.to_av();

            if config.global_header {
                (*ptr).flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
            }

            // Copy channel layout
            ff_check!(
                ffi::av_channel_layout_copy(&mut (*ptr).ch_layout, config.channel_layout.raw(),),
                "copying channel layout"
            )?;

            if let Some(br) = config.bit_rate {
                (*ptr).bit_rate = br;
            }

            ff_check!(
                ffi::avcodec_open2(ptr, codec, ptr::null_mut()),
                "opening audio encoder {}",
                config.codec_name
            )?;
        }

        Ok(Self { ptr })
    }

    /// Create a video encoder from configuration.
    pub fn video(config: VideoEncoderConfig) -> Result<Self> {
        let c_name = CString::new(config.codec_name.as_str()).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "codec name contains null byte")
        })?;
        let codec = unsafe { ffi::avcodec_find_encoder_by_name(c_name.as_ptr()) };
        if codec.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENCODER_NOT_FOUND,
                format!("encoder not found: {}", config.codec_name),
            ));
        }

        let mut ptr = unsafe { ffi::avcodec_alloc_context3(codec) };
        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating encoder context",
            ));
        }

        unsafe {
            (*ptr).width = config.width;
            (*ptr).height = config.height;
            (*ptr).pix_fmt = config.pixel_format.raw();
            (*ptr).time_base = config.time_base.to_av();
            (*ptr).colorspace = config.color.color_space;
            (*ptr).color_range = config.color.color_range;
            (*ptr).color_primaries = config.color.color_primaries;
            (*ptr).color_trc = config.color.color_trc;
            (*ptr).chroma_sample_location = config.color.chroma_location;

            if config.global_header {
                (*ptr).flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
            }

            if let Some(br) = config.bit_rate {
                (*ptr).bit_rate = br;
            }
            if let Some(gop) = config.gop_size {
                (*ptr).gop_size = gop;
            }
            if let Some(q) = config.quality {
                (*ptr).qmin = q;
                (*ptr).qmax = q;
            }
            if let Some((num, den)) = config.frame_rate {
                (*ptr).framerate = ffi::AVRational { num, den };
            }
            if let Some((num, den)) = config.sample_aspect_ratio {
                (*ptr).sample_aspect_ratio = ffi::AVRational { num, den };
            }

            // Zero-copy GPU path: set hw_frames_ctx so the encoder accepts
            // hardware frames directly. Read the HW pixel format from the
            // frames context (CUDA for NVENC, VAAPI for VAAPI encoders).
            if let Some(hw_frames) = config.hw_frames_ctx {
                let ref_ptr = ffi::av_buffer_ref(hw_frames);
                if ref_ptr.is_null() {
                    ffi::avcodec_free_context(&mut ptr);
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "av_buffer_ref failed for hw_frames_ctx",
                    ));
                }
                (*ptr).hw_frames_ctx = ref_ptr;
                let frames = (*ref_ptr).data as *const ffi::AVHWFramesContext;
                (*ptr).pix_fmt = (*frames).format;
            }

            // Build encoder options. HW encoders need explicit quality settings
            // (CQ mode for NVENC, quality level for VAAPI). Without these,
            // the output is very low quality.
            let mut opts: *mut ffi::AVDictionary = ptr::null_mut();
            if config.bit_rate.is_none() && config.quality.is_none() {
                let is_nvenc = config.codec_name.contains("nvenc");
                let is_vaapi = config.codec_name.contains("vaapi");
                if is_nvenc {
                    // NVENC: VBR with CQ 20 — allocates more bits to dark/complex
                    // scenes than constqp, preventing banding in gradients.
                    let _ = ffi::av_dict_set(&mut opts, c"rc".as_ptr(), c"vbr".as_ptr(), 0);
                    let _ = ffi::av_dict_set(&mut opts, c"cq".as_ptr(), c"20".as_ptr(), 0);
                    let _ = ffi::av_dict_set(&mut opts, c"preset".as_ptr(), c"p4".as_ptr(), 0);
                } else if is_vaapi {
                    // VAAPI: quality level (lower = better, 0-51 range)
                    (*ptr).global_quality = 20;
                }
            }

            for (key, value) in &config.options {
                if let (Ok(k), Ok(v)) = (CString::new(key.as_str()), CString::new(value.as_str())) {
                    let _ = ffi::av_dict_set(&mut opts, k.as_ptr(), v.as_ptr(), 0);
                }
            }

            ff_check!(
                ffi::avcodec_open2(ptr, codec, &mut opts),
                "opening video encoder {}",
                config.codec_name
            )?;
            if !opts.is_null() {
                ffi::av_dict_free(&mut opts);
            }
        }

        Ok(Self { ptr })
    }

    /// Send a frame to the encoder.
    /// Returns `Ok(true)` if accepted, `Ok(false)` if the encoder is full (EAGAIN).
    pub fn send_frame(&mut self, frame: &Frame) -> Result<bool> {
        let ret = unsafe { ffi::avcodec_send_frame(self.ptr, frame.as_ptr()) };
        if ret == ffi::AVERROR_EAGAIN {
            return Ok(false);
        }
        ff_check!(ret, "sending frame to encoder")?;
        Ok(true)
    }

    /// Send a flush signal to the encoder (NULL frame).
    /// Call this when there are no more frames to encode.
    pub fn send_eof(&mut self) -> Result<()> {
        let ret = unsafe { ffi::avcodec_send_frame(self.ptr, ptr::null()) };
        if ret == ffi::AVERROR_EOF {
            return Ok(());
        }
        ff_check!(ret, "sending EOF to encoder")?;
        Ok(())
    }

    /// Receive an encoded packet from the encoder.
    pub fn receive_packet(&mut self, packet: &mut Packet) -> Result<EncodeResult> {
        packet.unref();
        let ret = unsafe { ffi::avcodec_receive_packet(self.ptr, packet.as_mut_ptr()) };
        if ret == ffi::AVERROR_EAGAIN {
            return Ok(EncodeResult::NeedsInput);
        }
        if ret == ffi::AVERROR_EOF {
            return Ok(EncodeResult::Eof);
        }
        ff_check!(ret, "receiving packet from encoder")?;
        // Encoders leave a video packet's duration unset, and the HLS muxer then ends
        // a run's last segment a frame early.
        unsafe {
            let fr = (*self.ptr).framerate;
            let pkt = packet.as_mut_ptr();
            if (*pkt).duration == 0 && fr.num > 0 && fr.den > 0 {
                (*pkt).duration = ffi::av_rescale_q(
                    1,
                    ffi::AVRational {
                        num: fr.den,
                        den: fr.num,
                    },
                    (*self.ptr).time_base,
                );
            }
        }
        Ok(EncodeResult::Packet)
    }

    /// Frame size for audio encoders (samples per frame).
    pub fn frame_size(&self) -> i32 {
        unsafe { (*self.ptr).frame_size }
    }

    /// Time base for this codec context.
    pub fn time_base(&self) -> ffi::AVRational {
        unsafe { (*self.ptr).time_base }
    }

    /// Codec parameters — useful for copying to output stream.
    pub fn codec_parameters(&self) -> *const ffi::AVCodecContext {
        self.ptr
    }

    /// Raw pointer to the underlying AVCodecContext.
    pub fn as_ptr(&self) -> *const ffi::AVCodecContext {
        self.ptr
    }

    /// Mutable raw pointer.
    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVCodecContext {
        self.ptr
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ffi::avcodec_free_context(&mut self.ptr) };
    }
}
