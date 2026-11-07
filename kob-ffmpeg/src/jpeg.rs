use crate::codec::encoder::{EncodeResult, Encoder, VideoEncoderConfig};
use crate::frame::Frame;
use crate::packet::Packet;
use crate::scale::Scaler;
use crate::sdr::SdrConverter;
use crate::types::{PixelFormat, TimeBase};
use crate::{FfmpegError, Result, ffi};

pub(crate) fn even_height_for(width: i32, src_width: i32, src_height: i32) -> i32 {
    let aspect = src_height as f64 / src_width as f64;
    ((width as f64 * aspect) as i32 + 1) & !1
}

/// Source frame → SDR → resized (keeping aspect) → JPEG. Scaler and tone-map
/// graph are rebuilt only when the incoming frame's geometry or format changes.
pub(crate) struct ScaledJpeg {
    width: Option<i32>,
    quality: i32,
    sdr: SdrConverter,
    scaler: Option<((i32, i32, ffi::AVPixelFormat), Scaler)>,
}

impl ScaledJpeg {
    pub(crate) fn new(width: Option<i32>, quality: i32) -> Self {
        Self {
            width,
            quality,
            sdr: SdrConverter::new(),
            scaler: None,
        }
    }

    pub(crate) fn encode(&mut self, frame: &Frame) -> Result<(Vec<u8>, i32, i32)> {
        let sdr = self.sdr.convert(frame)?;
        let frame = sdr.as_ref().unwrap_or(frame);
        let Some(width) = self.width else {
            return encode_jpeg(frame, self.quality);
        };
        let fmt = frame.format() as ffi::AVPixelFormat;
        let key = (frame.width(), frame.height(), fmt);
        if self.scaler.as_ref().is_none_or(|(k, _)| *k != key) {
            let height = even_height_for(width, key.0, key.1);
            self.scaler = Some((
                key,
                Scaler::bilinear(key.0, key.1, fmt, width, height, fmt)?,
            ));
        }
        let mut scaled = Frame::new()?;
        self.scaler
            .as_ref()
            .expect("scaler built above")
            .1
            .scale_frame(frame, &mut scaled)?;
        encode_jpeg(&scaled, self.quality)
    }
}

pub(crate) fn encode_jpeg(frame: &Frame, quality: i32) -> Result<(Vec<u8>, i32, i32)> {
    let src_w = frame.width();
    let src_h = frame.height();
    let src_fmt = frame.format() as ffi::AVPixelFormat;
    let target_fmt = ffi::AV_PIX_FMT_YUVJ420P;

    let converted_holder;
    let frame_ref: &Frame = if src_fmt == target_fmt {
        frame
    } else {
        let scaler = Scaler::bilinear(src_w, src_h, src_fmt, src_w, src_h, target_fmt)?;
        let mut converted = Frame::new()?;
        scaler.scale_frame(frame, &mut converted)?;
        converted_holder = converted;
        &converted_holder
    };

    let mut encoder = Encoder::video(VideoEncoderConfig {
        codec_name: "mjpeg".to_string(),
        width: src_w,
        height: src_h,
        pixel_format: PixelFormat::from_raw(target_fmt),
        time_base: TimeBase::new(1, 25),
        bit_rate: None,
        gop_size: None,
        global_header: false,
        quality: Some(quality.clamp(1, 31)),
        hw_frames_ctx: None,
        sample_aspect_ratio: None,
        frame_rate: None,
        color: Default::default(),
        options: Vec::new(),
    })?;
    encoder.send_frame(frame_ref)?;
    encoder.send_eof()?;
    let mut pkt = Packet::new()?;
    match encoder.receive_packet(&mut pkt)? {
        EncodeResult::Packet => Ok((pkt.data().to_vec(), src_w, src_h)),
        _ => Err(FfmpegError::new(
            ffi::AVERROR_EXTERNAL,
            "mjpeg encoder produced no packet",
        )),
    }
}
