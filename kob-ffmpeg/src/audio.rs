//! High-level audio operations: chapter splitting, WAV conversion, PCM extraction, LUFS.

use std::collections::VecDeque;
use std::path::Path;

use crate::codec::decoder::{DecodeResult, Decoder};
use crate::codec::encoder::{AudioEncoderConfig, EncodeResult, Encoder};
use crate::format::input::{InputContext, MediaType};
use crate::format::output::OutputContext;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::{Result, ffi};

/// Encodes audio frames and writes the packets to one output stream.
///
/// Timestamps take two hops that are easy to miss, and getting either wrong is
/// silent: filtered frames carry the *filter link's* time base, which equals
/// `1/sample_rate` only when libavfilter happened to insert a conversion filter.
/// An MPEG-TS source already matching the encoder's format/rate/layout inserts
/// none, so the frames still carry the container's 1/90000 and land 90000/48000
/// = 1.875x too late unless rescaled here.
pub(crate) struct EncodedAudioWriter {
    stream_index: i32,
    enc_tb: ffi::AVRational,
    out_tb: ffi::AVRational,
}

impl EncodedAudioWriter {
    pub(crate) fn new(stream_index: i32, enc_tb: ffi::AVRational, out_tb: ffi::AVRational) -> Self {
        Self {
            stream_index,
            enc_tb,
            out_tb,
        }
    }

    /// Rescale `frame` from `frame_tb` into the encoder's time base, encode it,
    /// and write every packet it produces.
    pub(crate) fn encode(
        &self,
        enc: &mut Encoder,
        frame: &mut Frame,
        frame_tb: ffi::AVRational,
        out: &mut OutputContext,
        pkt: &mut Packet,
    ) -> Result<()> {
        if frame_tb.num > 0 && frame_tb.den > 0 {
            unsafe {
                let p = frame.as_mut_ptr();
                if (*p).pts != ffi::AV_NOPTS_VALUE {
                    (*p).pts = ffi::av_rescale_q((*p).pts, frame_tb, self.enc_tb);
                }
            }
        }
        enc.send_frame(frame)?;
        self.drain(enc, out, pkt)
    }

    /// Write whatever the encoder has ready. Also the flush path after `send_eof`.
    pub(crate) fn drain(
        &self,
        enc: &mut Encoder,
        out: &mut OutputContext,
        pkt: &mut Packet,
    ) -> Result<()> {
        while let EncodeResult::Packet = enc.receive_packet(pkt)? {
            unsafe {
                (*pkt.as_mut_ptr()).stream_index = self.stream_index;
                ffi::av_packet_rescale_ts(pkt.as_mut_ptr(), self.enc_tb, self.out_tb);
            }
            out.write_packet_interleaved(pkt)?;
        }
        Ok(())
    }

    /// Drain a filter graph into the encoder, rescaling from the graph's own
    /// output time base.
    pub(crate) fn encode_filtered(
        &self,
        enc: &mut Encoder,
        filter: &mut crate::filter::graph::FilterGraph,
        filt: &mut Frame,
        out: &mut OutputContext,
        pkt: &mut Packet,
    ) -> Result<()> {
        let ftb = filter.output_time_base();
        while filter.pull_frame(filt)? {
            self.encode(enc, filt, ftb, out, pkt)?;
            filt.unref();
        }
        Ok(())
    }
}

/// Discover the audio sample rate before opening an encoder.
///
/// LATM-encapsulated AAC over MPEG-TS (and a few other live-stream containers)
/// reach `find_stream_info` with `codec_parameters.sample_rate == 0` because
/// the rate lives inside the bitstream, not the PMT. Opening an encoder with
/// rate 0 fails `avcodec_open2` ("Invalid sample rate: 0"), so callers must
/// pre-roll one decoded frame to extract the real rate.
///
/// If `dec.sample_rate() > 0`, returns immediately without consuming input.
/// Otherwise reads packets from `in_ctx`, sends audio packets through `dec`,
/// and drains decoded frames into `buffered_frames` until one reports a
/// non-zero sample rate. Non-audio packets read along the way are pushed
/// into `buffered_packets` so the caller can replay them in stream order
/// after `write_header`.
pub fn probe_audio_sample_rate(
    in_ctx: &mut InputContext,
    audio_stream_idx: u32,
    dec: &mut Decoder,
    buffered_packets: &mut VecDeque<Packet>,
    buffered_frames: &mut VecDeque<Frame>,
) -> Result<i32> {
    if dec.sample_rate() > 0 {
        return Ok(dec.sample_rate());
    }
    let mut found: Option<i32> = None;
    let mut probe_frame = Frame::new()?;
    let mut pkt = Packet::new()?;
    'preroll: while in_ctx.read_packet(&mut pkt)? {
        if pkt.stream_index() == audio_stream_idx as i32 {
            dec.send_packet(&pkt)?;
            // Packet consumed by the decoder; not replayed. Frames produced
            // are buffered for the caller to encode after building the encoder.
            pkt = Packet::new()?;
            loop {
                match dec.receive_frame(&mut probe_frame)? {
                    DecodeResult::Frame => {
                        if found.is_none() {
                            let sr = probe_frame.sample_rate();
                            if sr > 0 {
                                found = Some(sr);
                            }
                        }
                        let mut saved = Frame::new()?;
                        std::mem::swap(&mut saved, &mut probe_frame);
                        buffered_frames.push_back(saved);
                    }
                    _ => break,
                }
            }
            if found.is_some() {
                break 'preroll;
            }
        } else {
            buffered_packets.push_back(std::mem::replace(&mut pkt, Packet::new()?));
        }
    }
    found.ok_or_else(|| {
        crate::error::FfmpegError::new(
            ffi::AVERROR_INVALIDDATA,
            "could not probe audio sample rate before EOF",
        )
    })
}

/// Extract integrated LUFS from ebur128 filter output frame metadata.
///
/// # Safety
/// Caller must ensure `frame` contains a valid AVFrame pointer with valid metadata.
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

/// Extract raw PCM audio data (mono, specified sample rate, s16le) over a time range.
///
/// Replaces: `ffmpeg -ss <offset> -i input -t <duration> -vn -sn -ac 1 -ar <rate> -f s16le -acodec pcm_s16le pipe:1`
///
/// Returns raw 16-bit little-endian mono PCM samples.
///
/// `offset_secs = 0.0` and `duration_secs = f64::INFINITY` extracts the whole file.
/// When `offset_secs > 0`, seeks backward to the nearest keyframe and drops decoded
/// frames whose timestamp is earlier than `offset_secs`. Stops once a decoded frame
/// reaches `offset_secs + duration_secs`. Measured 4–10× wire-byte reduction over
/// CIFS for typical credits-region scans.
pub fn extract_pcm_range(
    path: &Path,
    sample_rate: i32,
    offset_secs: f64,
    duration_secs: f64,
    run: Option<&crate::progress::RunContext>,
    on_data: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<()> {
    extract_pcm_from(
        InputContext::open_path(path)?,
        sample_rate,
        offset_secs,
        duration_secs,
        run,
        on_data,
    )
}

pub(crate) fn extract_pcm_from(
    mut ctx: InputContext,
    sample_rate: i32,
    offset_secs: f64,
    duration_secs: f64,
    run: Option<&crate::progress::RunContext>,
    on_data: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<()> {
    ctx.find_stream_info()?;

    let audio_idx = ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    // Set AVDISCARD_ALL on non-audio streams. This skips DECODE of video and
    // subtitle packets but does NOT reduce wire bytes — lavf still walks the
    // byte stream to find audio packet boundaries. The savings are CPU only.
    // Measured 2026-04-13: byte-identical CIFS read with vs without discard.
    ctx.discard_all_except(audio_idx);

    if offset_secs > 0.0 {
        let seek_ts = (offset_secs * ffi::AV_TIME_BASE as f64) as i64;
        ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;
    }

    let stream = ctx.stream(audio_idx).unwrap();
    let tb = stream.time_base_native();
    let tb_f64 = tb.num as f64 / tb.den as f64;

    let mut decoder = stream.decoder()?;
    if offset_secs > 0.0 {
        decoder.flush();
    }

    let end_secs = offset_secs + duration_secs;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut resampler: Option<crate::resample::Resampler> = None;
    let mut resampler_state: Option<AudioGraphState> = None;

    let process_frame = |frame: &Frame,
                         resampler: &mut Option<crate::resample::Resampler>,
                         resampler_state: &mut Option<AudioGraphState>,
                         on_data: &mut dyn FnMut(&[u8]) -> bool|
     -> Result<FrameAction> {
        let pts = frame.pts();
        if pts != ffi::AV_NOPTS_VALUE {
            let frame_time = pts as f64 * tb_f64;
            if frame_time >= end_secs {
                return Ok(FrameAction::Stop);
            }
            if frame_time < offset_secs {
                return Ok(FrameAction::Skip);
            }
        }
        AudioGraphState::ensure_resampler(
            resampler,
            resampler_state,
            frame,
            sample_rate,
            crate::types::SampleFormat::S16,
            crate::types::ChannelLayout::mono(),
        )?;
        let data = resampler.as_mut().unwrap().convert_frame_to_bytes(frame)?;
        if !data.is_empty() && !on_data(&data) {
            return Ok(FrameAction::Stop);
        }
        Ok(FrameAction::Continue)
    };

    'outer: while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        decoder.send_packet(&packet)?;
        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    match process_frame(
                        &frame,
                        &mut resampler,
                        &mut resampler_state,
                        &mut *on_data,
                    )? {
                        FrameAction::Continue | FrameAction::Skip => {}
                        FrameAction::Stop => return Ok(()),
                    }
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break 'outer,
            }
        }
    }

    // Flush decoder
    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame)? {
            DecodeResult::Frame => {
                match process_frame(&frame, &mut resampler, &mut resampler_state, &mut *on_data)? {
                    FrameAction::Continue | FrameAction::Skip => {}
                    FrameAction::Stop => return Ok(()),
                }
            }
            _ => break,
        }
    }

    Ok(())
}

enum FrameAction {
    Continue,
    Skip,
    Stop,
}

/// Convert audio to WAV format (mono, 22050 Hz, 16-bit PCM).
///
/// Replaces: `ffmpeg -i input -ar 22050 -ac 1 -sample_fmt s16 -y output.wav`
pub fn convert_to_wav(
    input: &Path,
    output: &Path,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    convert_to_wav_with_options(input, output, 22050, 1, run)
}

/// Convert audio to WAV with custom sample rate and channels.
pub fn convert_to_wav_with_options(
    input: &Path,
    output: &Path,
    sample_rate: i32,
    channels: i32,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let audio_idx = in_ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    in_ctx.discard_all_except(audio_idx);

    let stream = in_ctx.stream(audio_idx).unwrap();
    let mut decoder = stream.decoder()?;

    let out_layout = crate::types::ChannelLayout::default_for(channels);

    let encoder = Encoder::audio(AudioEncoderConfig {
        codec_name: "pcm_s16le".to_string(),
        sample_rate,
        channel_layout: out_layout.clone(),
        sample_format: crate::types::SampleFormat::S16,
        bit_rate: None,
        time_base: crate::types::TimeBase::new(1, sample_rate),
        global_header: false,
    })?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path is not valid UTF-8")
    })?;

    let mut out_ctx = OutputContext::create(output_str)?;
    out_ctx.add_stream_from_encoder(&encoder)?;
    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut out_packet = Packet::new()?;
    let mut encoder = encoder;
    let mut resampler: Option<crate::resample::Resampler> = None;
    let mut resampler_state: Option<AudioGraphState> = None;

    let resample_and_encode = |resampler: &mut crate::resample::Resampler,
                               encoder: &mut Encoder,
                               out_ctx: &mut OutputContext,
                               in_frame: &Frame,
                               out_packet: &mut Packet|
     -> Result<()> {
        let pcm_data = resampler.convert_frame_to_bytes(in_frame)?;
        if pcm_data.is_empty() {
            return Ok(());
        }
        let mut out_frame = Frame::new()?;
        unsafe {
            let f = out_frame.as_mut_ptr();
            (*f).format = crate::types::SampleFormat::S16.raw();
            (*f).sample_rate = sample_rate;
            (*f).nb_samples = (pcm_data.len() / (2 * channels as usize)) as i32;
            ffi::av_channel_layout_default(&mut (*f).ch_layout, channels);
            crate::error::ff_check(
                ffi::av_frame_get_buffer(f, 0),
                "allocating audio frame buffer",
            )?;
            std::ptr::copy_nonoverlapping(pcm_data.as_ptr(), (*f).data[0], pcm_data.len());
        }
        encoder.send_frame(&out_frame)?;
        while let EncodeResult::Packet = encoder.receive_packet(out_packet)? {
            out_ctx.write_packet_interleaved(out_packet)?;
        }
        Ok(())
    };

    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        decoder.send_packet(&packet)?;
        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    AudioGraphState::ensure_resampler(
                        &mut resampler,
                        &mut resampler_state,
                        &frame,
                        sample_rate,
                        crate::types::SampleFormat::S16,
                        out_layout.clone(),
                    )?;
                    resample_and_encode(
                        resampler.as_mut().unwrap(),
                        &mut encoder,
                        &mut out_ctx,
                        &frame,
                        &mut out_packet,
                    )?;
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break,
            }
        }
    }

    // Flush
    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame)? {
            DecodeResult::Frame => {
                AudioGraphState::ensure_resampler(
                    &mut resampler,
                    &mut resampler_state,
                    &frame,
                    sample_rate,
                    crate::types::SampleFormat::S16,
                    out_layout.clone(),
                )?;
                resample_and_encode(
                    resampler.as_mut().unwrap(),
                    &mut encoder,
                    &mut out_ctx,
                    &frame,
                    &mut out_packet,
                )?;
            }
            _ => break,
        }
    }

    encoder.send_eof()?;
    while let EncodeResult::Packet = encoder.receive_packet(&mut out_packet)? {
        out_ctx.write_packet_interleaved(&mut out_packet)?;
    }

    out_ctx.write_trailer()?;
    Ok(())
}

/// Split an audio file into per-chapter MP3 files.
///
/// Replaces: `ffmpeg -ss <start> -to <end> -i <input> -codec:a libmp3lame -q:a 2 -y chapter_NN.mp3`
pub fn split_chapters_to_mp3(
    input: &Path,
    output_dir: &Path,
    quality: i32,
    run: Option<&crate::progress::RunContext>,
) -> Result<Vec<std::path::PathBuf>> {
    let probe = crate::probe::probe(input)?;
    if probe.chapters.is_empty() {
        return Ok(Vec::new());
    }

    let mut output_files = Vec::new();

    for (i, chapter) in probe.chapters.iter().enumerate() {
        let output_path = output_dir.join(format!("chapter_{:03}.mp3", i));
        split_audio_segment(
            input,
            &output_path,
            chapter.start_seconds,
            chapter.end_seconds,
            "libmp3lame",
            quality,
            run,
        )?;
        output_files.push(output_path);
    }

    Ok(output_files)
}

/// Extract a segment of audio to a specific codec.
pub fn split_audio_segment(
    input: &Path,
    output: &Path,
    start_secs: f64,
    end_secs: f64,
    codec_name: &str,
    quality: i32,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let audio_idx = in_ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    in_ctx.discard_all_except(audio_idx);

    // Seek to start
    let seek_ts = (start_secs * ffi::AV_TIME_BASE as f64) as i64;
    in_ctx.seek(-1, seek_ts, ffi::AVSEEK_FLAG_BACKWARD as i32)?;

    let stream = in_ctx.stream(audio_idx).unwrap();
    let tb = stream.time_base_native();
    let tb_f64 = tb.num as f64 / tb.den as f64;

    let mut decoder = stream.decoder()?;

    // Map LAME quality (0-9, lower=better) to approximate bitrate
    // quality 0 ≈ 245kbps, 2 ≈ 190kbps, 4 ≈ 165kbps, 6 ≈ 130kbps, 9 ≈ 65kbps
    let bit_rate = match quality.clamp(0, 9) {
        0 => 245_000,
        1 => 220_000,
        2 => 190_000,
        3 => 175_000,
        4 => 165_000,
        5 => 130_000,
        6 => 115_000,
        7 => 100_000,
        8 => 85_000,
        _ => 65_000,
    };

    // Encoder matches the audio filter output (stereo). Sample format is
    // encoder-specific — query it so encoder open + filter graph agree.
    let sample_format = crate::codec::encoder::default_audio_sample_format(codec_name)?;
    let encoder = Encoder::audio(AudioEncoderConfig {
        codec_name: codec_name.to_string(),
        sample_rate: decoder.sample_rate(),
        channel_layout: crate::types::ChannelLayout::stereo(),
        sample_format,
        bit_rate: Some(bit_rate),
        time_base: crate::types::TimeBase::new(1, decoder.sample_rate()),
        global_header: false,
    })?;

    // Filter string for format adaptation. The aformat filter normalizes to the
    // encoder's expected sample format while asetnsamples adapts frame sizes.
    // The abuffer configuration is deferred until the first decoded frame to handle
    // codecs (e.g., AC3) where initial frames have different properties than probed.
    let filter_str = format!(
        "aformat=sample_fmts={}:sample_rates={}:channel_layouts=stereo,asetnsamples=n={}:p=0",
        sample_format.name(),
        decoder.sample_rate(),
        encoder.frame_size().max(1024),
    );
    let time_base_av = tb.to_av();

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;

    let mut out_ctx = OutputContext::create(output_str)?;
    out_ctx.add_stream_from_encoder(&encoder)?;
    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut filtered_frame = Frame::new()?;
    let mut out_packet = Packet::new()?;
    let mut encoder = encoder;
    let mut audio_filter: Option<crate::filter::graph::FilterGraph> = None;
    let mut filter_state: Option<AudioGraphState> = None;

    let ensure_filter = |audio_filter: &mut Option<crate::filter::graph::FilterGraph>,
                         filter_state: &mut Option<AudioGraphState>,
                         frame: &Frame,
                         filtered_frame: &mut Frame,
                         encoder: &mut Encoder,
                         out_ctx: &mut OutputContext,
                         out_packet: &mut Packet|
     -> Result<()> {
        let needs_rebuild = match filter_state {
            None => true,
            Some(state) => !state.matches_frame(frame),
        };
        if needs_rebuild {
            if let Some(f) = audio_filter {
                f.push_eof()?;
                while f.pull_frame(filtered_frame)? {
                    encoder.send_frame(filtered_frame)?;
                    while let EncodeResult::Packet = encoder.receive_packet(out_packet)? {
                        out_ctx.write_packet_interleaved(out_packet)?;
                    }
                    filtered_frame.unref();
                }
            }
            let state = unsafe { AudioGraphState::from_frame(frame) };
            let params = crate::filter::graph::AudioFilterParams {
                sample_rate: state.sample_rate,
                sample_format: state.sample_format,
                channel_layout: state.channel_layout,
                time_base: time_base_av,
                side_data_frame: frame.as_ptr(),
            };
            *audio_filter = Some(crate::filter::graph::FilterGraph::new_audio(
                &params,
                &filter_str,
            )?);
            *filter_state = Some(unsafe { AudioGraphState::from_frame(frame) });
        }
        Ok(())
    };

    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        let pkt_time = packet.pts() as f64 * tb_f64;
        if pkt_time > end_secs {
            break;
        }
        decoder.send_packet(&packet)?;
        loop {
            match decoder.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    let frame_time = frame.pts() as f64 * tb_f64;
                    if frame_time < start_secs {
                        continue;
                    }
                    if frame_time > end_secs {
                        break;
                    }
                    ensure_filter(
                        &mut audio_filter,
                        &mut filter_state,
                        &frame,
                        &mut filtered_frame,
                        &mut encoder,
                        &mut out_ctx,
                        &mut out_packet,
                    )?;
                    audio_filter.as_mut().unwrap().push_frame(&mut frame)?;
                    while audio_filter
                        .as_mut()
                        .unwrap()
                        .pull_frame(&mut filtered_frame)?
                    {
                        encoder.send_frame(&filtered_frame)?;
                        while let EncodeResult::Packet = encoder.receive_packet(&mut out_packet)? {
                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                        }
                        filtered_frame.unref();
                    }
                }
                DecodeResult::NeedsInput => break,
                DecodeResult::Eof => break,
            }
        }
    }

    // Flush filter + encoder
    if let Some(ref mut f) = audio_filter {
        f.push_eof()?;
        while f.pull_frame(&mut filtered_frame)? {
            encoder.send_frame(&filtered_frame)?;
            while let EncodeResult::Packet = encoder.receive_packet(&mut out_packet)? {
                out_ctx.write_packet_interleaved(&mut out_packet)?;
            }
            filtered_frame.unref();
        }
    }
    encoder.send_eof()?;
    while let EncodeResult::Packet = encoder.receive_packet(&mut out_packet)? {
        out_ctx.write_packet_interleaved(&mut out_packet)?;
    }

    out_ctx.write_trailer()?;
    Ok(())
}

/// Audio properties tracked for resampler/filter graph (re)configuration.
///
/// Many codecs (notably AC3) declare one channel layout in the container (e.g. 5.1)
/// but produce initial decoded frames with a different layout (e.g. stereo). If the
/// resampler or filter graph is configured from the stream metadata instead of the
/// actual frame, `swr_convert` reads NULL channel pointers → segfault.
///
/// This struct snapshots frame properties and detects when they change, triggering
/// a rebuild. Used by `extract_pcm_range`, `convert_to_wav`, `measure_lufs`, `chromaprint`,
/// and `detect_silence`.
pub(crate) struct AudioGraphState {
    pub(crate) sample_rate: i32,
    pub(crate) sample_format: ffi::AVSampleFormat,
    pub(crate) channel_layout: ffi::AVChannelLayout,
}

impl AudioGraphState {
    /// Read audio properties from a decoded frame.
    ///
    /// # Safety
    /// `frame` must contain a valid AVFrame pointer.
    pub(crate) unsafe fn from_frame(frame: &Frame) -> Self {
        unsafe {
            let f = frame.as_ptr();
            Self {
                sample_rate: (*f).sample_rate,
                sample_format: (*f).format,
                channel_layout: (*f).ch_layout,
            }
        }
    }

    pub(crate) fn matches_frame(&self, frame: &Frame) -> bool {
        unsafe {
            let f = frame.as_ptr();
            self.sample_rate == (*f).sample_rate
                && self.sample_format == (*f).format
                && ffi::av_channel_layout_compare(&self.channel_layout, &(*f).ch_layout) == 0
        }
    }

    /// Rebuild the resampler if the current frame's properties differ from the
    /// last-seen state. Call this before every `convert_frame_to_bytes`.
    pub(crate) fn ensure_resampler(
        resampler: &mut Option<crate::resample::Resampler>,
        state: &mut Option<Self>,
        frame: &Frame,
        out_sample_rate: i32,
        out_sample_format: crate::types::SampleFormat,
        out_channel_layout: crate::types::ChannelLayout,
    ) -> crate::Result<()> {
        let needs_rebuild = match state {
            None => true,
            Some(s) => !s.matches_frame(frame),
        };
        if needs_rebuild {
            let new_state = unsafe { Self::from_frame(frame) };
            *resampler = Some(crate::resample::Resampler::new(
                crate::resample::ResampleConfig {
                    in_sample_rate: new_state.sample_rate,
                    in_sample_format: crate::types::SampleFormat::from_raw(new_state.sample_format),
                    in_channel_layout: crate::types::ChannelLayout::from_raw(
                        new_state.channel_layout,
                    ),
                    out_sample_rate,
                    out_sample_format,
                    out_channel_layout,
                },
            )?);
            *state = Some(unsafe { Self::from_frame(frame) });
        }
        Ok(())
    }

    /// Check whether the filter graph / resampler needs to be rebuilt for this frame.
    pub(crate) fn needs_rebuild(state: &Option<Self>, frame: &Frame) -> bool {
        match state {
            None => true,
            Some(s) => !s.matches_frame(frame),
        }
    }

    /// Create a new audio filter graph from the given frame's actual properties,
    /// and update the tracked state. The caller is responsible for flushing/draining
    /// the old graph before calling this.
    pub(crate) fn rebuild_filter_graph(
        graph: &mut Option<crate::filter::graph::FilterGraph>,
        state: &mut Option<Self>,
        frame: &Frame,
        time_base: ffi::AVRational,
        filter_str: &str,
    ) -> crate::Result<()> {
        let new_state = unsafe { Self::from_frame(frame) };
        let params = crate::filter::graph::AudioFilterParams {
            sample_rate: new_state.sample_rate,
            sample_format: new_state.sample_format,
            channel_layout: new_state.channel_layout,
            time_base,
            side_data_frame: frame.as_ptr(),
        };
        *graph = Some(crate::filter::graph::FilterGraph::new_audio(
            &params, filter_str,
        )?);
        *state = Some(unsafe { Self::from_frame(frame) });
        Ok(())
    }

    /// Rebuild an audio filter graph if the current frame's properties differ
    /// from the last-seen state. Flushes the old graph, discarding remaining frames.
    ///
    /// Use this for analysis pipelines (silence detection, LUFS) where flushed frames
    /// don't need to be encoded. For transcode pipelines where flushed frames must be
    /// encoded to output, use [`needs_rebuild`] + drain + [`rebuild_filter_graph`] instead.
    pub(crate) fn ensure_filter_graph(
        graph: &mut Option<crate::filter::graph::FilterGraph>,
        state: &mut Option<Self>,
        frame: &Frame,
        time_base: ffi::AVRational,
        filter_str: &str,
    ) -> crate::Result<()> {
        if Self::needs_rebuild(state, frame) {
            // Flush old graph, discarding remaining frames
            if let Some(g) = graph {
                let mut flush_frame = Frame::new()?;
                g.push_eof()?;
                while g.pull_frame(&mut flush_frame)? {
                    flush_frame.unref();
                }
            }
            Self::rebuild_filter_graph(graph, state, frame, time_base, filter_str)?;
        }
        Ok(())
    }
}

/// Measure integrated loudness (LUFS) of an audio file.
///
/// Replaces: `ffmpeg -i file -vn -sn -af ebur128=peak=none -f null -`
///
/// Like the ffmpeg CLI, this detects mid-stream audio property changes (channel
/// layout, sample rate, sample format) and reconfigures the filter graph. Some
/// codecs (notably AC3) can have initial frames with a different channel layout
/// than the stream's declared layout.
pub fn measure_lufs(path: &Path, run: Option<&crate::progress::RunContext>) -> Result<Option<f64>> {
    use crate::filter::graph::{AudioFilterParams, FilterGraph};

    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let audio_idx = ctx.find_best_stream(MediaType::Audio).ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_STREAM_NOT_FOUND, "no audio stream")
    })?;

    ctx.discard_all_except(audio_idx);

    let stream = ctx.stream(audio_idx).unwrap();
    let time_base = stream.time_base();
    let mut decoder = stream.decoder()?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut out_frame = Frame::new()?;
    let mut last_lufs: Option<f64> = None;
    let mut frames_decoded: u64 = 0;
    let mut decode_errors: u64 = 0;
    let mut graph: Option<FilterGraph> = None;
    let mut graph_state: Option<AudioGraphState> = None;

    let ensure_graph = |graph: &mut Option<FilterGraph>,
                        graph_state: &mut Option<AudioGraphState>,
                        frame: &Frame,
                        out_frame: &mut Frame,
                        last_lufs: &mut Option<f64>,
                        time_base: ffi::AVRational|
     -> Result<()> {
        let needs_rebuild = match graph_state {
            None => true,
            Some(state) => !state.matches_frame(frame),
        };
        if needs_rebuild {
            // Flush the old graph before replacing it
            if let Some(g) = graph {
                g.push_eof()?;
                while g.pull_frame(out_frame)? {
                    if let Some(lufs) = unsafe { extract_lufs(out_frame) } {
                        *last_lufs = Some(lufs);
                    }
                    out_frame.unref();
                }
            }
            let state = unsafe { AudioGraphState::from_frame(frame) };
            let params = AudioFilterParams {
                sample_rate: state.sample_rate,
                sample_format: state.sample_format,
                channel_layout: state.channel_layout,
                time_base,
                side_data_frame: frame.as_ptr(),
            };
            *graph = Some(FilterGraph::new_audio(
                &params,
                "ebur128=peak=none:metadata=1",
            )?);
            *graph_state = Some(unsafe { AudioGraphState::from_frame(frame) });
            tracing::debug!(
                sample_rate = state.sample_rate,
                "reconfigured LUFS filter graph for new audio properties"
            );
        }
        Ok(())
    };

    while ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() != audio_idx as i32 {
            continue;
        }
        if let Err(e) = decoder.send_packet(&packet) {
            tracing::debug!("skipping malformed audio packet: {e}");
            decode_errors += 1;
            continue;
        }
        loop {
            match decoder.receive_frame(&mut frame) {
                Ok(DecodeResult::Frame) => {
                    frames_decoded += 1;
                    ensure_graph(
                        &mut graph,
                        &mut graph_state,
                        &frame,
                        &mut out_frame,
                        &mut last_lufs,
                        time_base,
                    )?;
                    graph.as_mut().unwrap().push_frame(&mut frame)?;
                    while graph.as_mut().unwrap().pull_frame(&mut out_frame)? {
                        if let Some(lufs) = unsafe { extract_lufs(&out_frame) } {
                            last_lufs = Some(lufs);
                        }
                        out_frame.unref();
                    }
                }
                Ok(DecodeResult::NeedsInput) => break,
                Ok(DecodeResult::Eof) => break,
                Err(e) => {
                    tracing::debug!("skipping undecodable audio frame: {e}");
                    decode_errors += 1;
                    break;
                }
            }
        }
    }

    if frames_decoded == 0 && decode_errors > 0 {
        return Err(crate::error::FfmpegError::new(
            ffi::AVERROR_INVALIDDATA,
            format!("all {decode_errors} audio packets failed to decode"),
        ));
    }

    decoder.send_eof()?;
    loop {
        match decoder.receive_frame(&mut frame) {
            Ok(DecodeResult::Frame) => {
                ensure_graph(
                    &mut graph,
                    &mut graph_state,
                    &frame,
                    &mut out_frame,
                    &mut last_lufs,
                    time_base,
                )?;
                graph.as_mut().unwrap().push_frame(&mut frame)?;
                while graph.as_mut().unwrap().pull_frame(&mut out_frame)? {
                    if let Some(lufs) = unsafe { extract_lufs(&out_frame) } {
                        last_lufs = Some(lufs);
                    }
                    out_frame.unref();
                }
            }
            Err(e) => {
                tracing::debug!("error during flush: {e}");
                break;
            }
            _ => break,
        }
    }

    if let Some(ref mut g) = graph {
        g.push_eof()?;
        while g.pull_frame(&mut out_frame)? {
            if let Some(lufs) = unsafe { extract_lufs(&out_frame) } {
                last_lufs = Some(lufs);
            }
            out_frame.unref();
        }
    }

    Ok(last_lufs)
}
