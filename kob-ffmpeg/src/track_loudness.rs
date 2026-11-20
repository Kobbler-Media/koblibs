use crate::audio::{AudioGraphState, Loudness};
use crate::codec::decoder::{DecodeResult, Decoder};
use crate::downmix_peak::DownmixPeakMeter;
use crate::filter::graph::FilterGraph;
use crate::format::input::{InputContext, MediaType};
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{FfmpegError, Result, ffi};

const LUFS: &str = "ebur128=peak=none:metadata=1";

/// Loudness of one audio stream, keyed by its absolute index in the container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackLoudness {
    pub stream_index: u32,
    pub loudness: Loudness,
}

pub(crate) fn audio_stream_indices(ctx: &InputContext) -> Vec<u32> {
    ctx.streams()
        .filter(|s| s.codec_type() == MediaType::Audio)
        .map(|s| s.index() as u32)
        .collect()
}

pub(crate) struct LoudnessMeter {
    time_base: ffi::AVRational,
    graph: Option<FilterGraph>,
    state: Option<AudioGraphState>,
    out: Frame,
    last_lufs: Option<f64>,
    downmix: DownmixPeakMeter,
}

impl LoudnessMeter {
    pub(crate) fn new(time_base: ffi::AVRational) -> Result<Self> {
        Ok(Self {
            time_base,
            graph: None,
            state: None,
            out: Frame::new()?,
            last_lufs: None,
            downmix: DownmixPeakMeter::new(),
        })
    }

    pub(crate) fn feed(&mut self, frame: &mut Frame) -> Result<()> {
        if AudioGraphState::needs_rebuild(&self.state, frame) {
            self.flush()?;
            AudioGraphState::rebuild_filter_graph(
                &mut self.graph,
                &mut self.state,
                frame,
                self.time_base,
                LUFS,
            )?;
        }
        self.downmix.feed(frame, self.time_base)?;
        self.graph.as_mut().unwrap().push_frame_keep_ref(frame)?;
        self.drain()
    }

    pub(crate) fn finish(mut self) -> Result<Loudness> {
        self.flush()?;
        Ok(Loudness {
            integrated_lufs: self.last_lufs,
            downmix_peak_db: self.downmix.finish()?,
        })
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(g) = self.graph.as_mut() {
            g.push_eof()?;
            self.drain()?;
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let Some(graph) = self.graph.as_mut() else {
            return Ok(());
        };
        while graph.pull_frame(&mut self.out)? {
            if let Some(lufs) = unsafe { extract_lufs(&self.out) } {
                self.last_lufs = Some(lufs);
            }
            self.out.unref();
        }
        Ok(())
    }
}

// Undecodable packets are skipped: one damaged packet must not cost a whole track its level.
pub(crate) struct DecodingLoudnessMeter {
    stream_index: u32,
    decoder: Decoder,
    meter: LoudnessMeter,
    frame: Frame,
    frames_decoded: u64,
    decode_errors: u64,
}

impl DecodingLoudnessMeter {
    pub(crate) fn new(ctx: &InputContext, stream_index: u32) -> Result<Self> {
        let stream = ctx.stream(stream_index).ok_or_else(|| {
            FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no such audio stream")
        })?;
        Ok(Self {
            stream_index,
            decoder: stream.decoder()?,
            meter: LoudnessMeter::new(stream.time_base())?,
            frame: Frame::new()?,
            frames_decoded: 0,
            decode_errors: 0,
        })
    }

    pub(crate) fn stream_index(&self) -> u32 {
        self.stream_index
    }

    pub(crate) fn feed(&mut self, packet: &Packet) -> Result<()> {
        if let Err(e) = self.decoder.send_packet(packet) {
            tracing::debug!(
                stream = self.stream_index,
                "skipping malformed audio packet: {e}"
            );
            self.decode_errors += 1;
            return Ok(());
        }
        self.receive()
    }

    pub(crate) fn finish(mut self) -> Result<TrackLoudness> {
        self.decoder.send_eof()?;
        self.receive()?;
        if self.frames_decoded == 0 && self.decode_errors > 0 {
            return Err(FfmpegError::new(
                ffi::AVERROR_INVALIDDATA,
                format!(
                    "all {} packets of audio stream {} failed to decode",
                    self.decode_errors, self.stream_index
                ),
            ));
        }
        Ok(TrackLoudness {
            stream_index: self.stream_index,
            loudness: self.meter.finish()?,
        })
    }

    fn receive(&mut self) -> Result<()> {
        loop {
            match self.decoder.receive_frame(&mut self.frame) {
                Ok(DecodeResult::Frame) => {
                    self.frames_decoded += 1;
                    self.meter.feed(&mut self.frame)?;
                    self.frame.unref();
                }
                Ok(DecodeResult::NeedsInput | DecodeResult::Eof) => return Ok(()),
                Err(e) => {
                    tracing::debug!(
                        stream = self.stream_index,
                        "skipping undecodable audio frame: {e}"
                    );
                    self.decode_errors += 1;
                    return Ok(());
                }
            }
        }
    }
}

/// # Safety
/// `frame` must hold a valid AVFrame.
unsafe fn extract_lufs(frame: &Frame) -> Option<f64> {
    unsafe {
        let metadata = (*frame.as_ptr()).metadata;
        if metadata.is_null() {
            return None;
        }
        let entry = ffi::av_dict_get(metadata, c"lavfi.r128.I".as_ptr(), std::ptr::null(), 0);
        if entry.is_null() || (*entry).value.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*entry).value)
            .to_string_lossy()
            .parse::<f64>()
            .ok()
    }
}
