//! Mono 16-bit PCM in and out of memory: decoding a clip of any container to PCM, and
//! encoding PCM as it arrives into ADTS AAC, a stream a player can start on without a
//! container header or a known length.
//!
//! ADTS is framed by hand rather than through the `adts` muxer: every encoder packet is
//! one access unit, so its 7-byte header is all the muxer would add, and handing frames
//! back one at a time lets a caller index them by timestamp to serve a resume.

use crate::codec::encoder::{AudioEncoderConfig, EncodeResult, Encoder};
use crate::error::FfmpegError;
use crate::format::input::InputContext;
use crate::format::io_context::CustomIoContext;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::types::{ChannelLayout, SampleFormat, TimeBase};
use crate::{Result, ffi};

/// Decode a whole in-memory clip to s16le mono at `sample_rate`.
pub fn decode_to_pcm(data: Vec<u8>, sample_rate: i32) -> Result<Vec<u8>> {
    let mut io = CustomIoContext::from_buffer(data)?;
    let mut out = Vec::new();
    crate::audio::extract_pcm_from(
        InputContext::open_custom(&mut io)?,
        sample_rate,
        0.0,
        f64::INFINITY,
        None,
        &mut |pcm| {
            out.extend_from_slice(pcm);
            true
        },
    )?;
    Ok(out)
}

/// One AAC access unit with its ADTS header. `pts` is in samples and starts negative:
/// the encoder's priming frame precedes the first input sample.
pub struct AdtsFrame {
    pub pts: i64,
    pub bytes: Vec<u8>,
}

pub struct AdtsAacEncoder {
    encoder: Encoder,
    packet: Packet,
    frame_size: usize,
    sample_rate: i32,
    frequency_index: u8,
    pending: Vec<f32>,
    odd_byte: Option<u8>,
    next_pts: i64,
}

impl AdtsAacEncoder {
    pub fn new(sample_rate: i32, bit_rate: i64) -> Result<Self> {
        let frequency_index = adts_frequency_index(sample_rate).ok_or_else(|| {
            FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!("ADTS has no index for {sample_rate} Hz"),
            )
        })?;
        let encoder = Encoder::audio(AudioEncoderConfig {
            codec_name: "aac".to_string(),
            sample_rate,
            channel_layout: ChannelLayout::mono(),
            sample_format: SampleFormat::FLTP,
            bit_rate: Some(bit_rate),
            time_base: TimeBase::new(1, sample_rate),
            global_header: false,
        })?;
        let frame_size = encoder.frame_size().max(1) as usize;
        Ok(Self {
            encoder,
            packet: Packet::new()?,
            frame_size,
            sample_rate,
            frequency_index,
            pending: Vec::with_capacity(frame_size * 2),
            odd_byte: None,
            next_pts: 0,
        })
    }

    pub fn push_s16le(&mut self, pcm: &[u8]) -> Result<Vec<AdtsFrame>> {
        let mut bytes = pcm;
        if let Some(lo) = self.odd_byte.take() {
            let Some((&hi, rest)) = bytes.split_first() else {
                self.odd_byte = Some(lo);
                return Ok(Vec::new());
            };
            self.pending.push(sample(lo, hi));
            bytes = rest;
        }
        let (pairs, rest) = bytes.as_chunks::<2>();
        self.odd_byte = rest.first().copied();
        self.pending
            .extend(pairs.iter().map(|&[lo, hi]| sample(lo, hi)));

        let mut out = Vec::new();
        while self.pending.len() >= self.frame_size {
            let chunk: Vec<f32> = self.pending.drain(..self.frame_size).collect();
            self.send(&chunk, &mut out)?;
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Result<Vec<AdtsFrame>> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let chunk = std::mem::take(&mut self.pending);
            self.send(&chunk, &mut out)?;
        }
        self.encoder.send_eof()?;
        self.drain(&mut out)?;
        Ok(out)
    }

    fn send(&mut self, samples: &[f32], out: &mut Vec<AdtsFrame>) -> Result<()> {
        let mut frame = Frame::new()?;
        unsafe {
            let f = frame.as_mut_ptr();
            (*f).format = SampleFormat::FLTP.raw();
            (*f).sample_rate = self.sample_rate;
            (*f).nb_samples = samples.len() as i32;
            (*f).pts = self.next_pts;
            ffi::av_channel_layout_default(&mut (*f).ch_layout, 1);
            crate::error::ff_check(ffi::av_frame_get_buffer(f, 0), "allocating AAC input frame")?;
            std::ptr::copy_nonoverlapping(
                samples.as_ptr(),
                (*f).data[0] as *mut f32,
                samples.len(),
            );
        }
        self.next_pts += samples.len() as i64;
        self.encoder.send_frame(&frame)?;
        self.drain(out)
    }

    fn drain(&mut self, out: &mut Vec<AdtsFrame>) -> Result<()> {
        while let EncodeResult::Packet = self.encoder.receive_packet(&mut self.packet)? {
            let payload = self.packet.data();
            let mut bytes = adts_header(self.frequency_index, payload.len()).to_vec();
            bytes.extend_from_slice(payload);
            out.push(AdtsFrame {
                pts: self.packet.pts(),
                bytes,
            });
        }
        Ok(())
    }
}

fn sample(lo: u8, hi: u8) -> f32 {
    i16::from_le_bytes([lo, hi]) as f32 / 32768.0
}

fn adts_frequency_index(sample_rate: i32) -> Option<u8> {
    const RATES: [i32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    RATES
        .iter()
        .position(|&r| r == sample_rate)
        .map(|i| i as u8)
}

fn adts_header(frequency_index: u8, payload_len: usize) -> [u8; 7] {
    const AAC_LC: u8 = 1;
    const MONO: u8 = 1;
    let len = payload_len + 7;
    [
        0xFF,
        0xF1,
        (AAC_LC << 6) | (frequency_index << 2) | (MONO >> 2),
        ((MONO & 3) << 6) | ((len >> 11) & 0x03) as u8,
        ((len >> 3) & 0xFF) as u8,
        (((len & 0x07) << 5) as u8) | 0x1F,
        0xFC,
    ]
}
