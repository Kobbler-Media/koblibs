//! IPTV stream operations: recording, live transcode.
//!
//! Handles network stream capture, single-pass record+encode to MP4,
//! segment concatenation, and live HLS transcoding from IPTV streams.

use std::collections::VecDeque;
use std::path::Path;

use crate::codec::decoder::DecodeResult;
use crate::codec::encoder::{AudioEncoderConfig, EncodeResult, Encoder, VideoEncoderConfig};
use crate::format::input::{InputContext, MediaType};
use crate::format::output::OutputContext;
use crate::frame::Frame;
use crate::packet::Packet;
use crate::probe::{self, ProbeOptions, ProbeResult};
use crate::{Result, ffi};

/// Probe an IPTV/network stream.
///
/// Uses shorter analysis duration and probe size suitable for live streams.
pub fn probe_stream(url: &str) -> Result<ProbeResult> {
    probe::probe_url_with_options(
        url,
        ProbeOptions {
            probesize: 131072,               // 128KB
            max_analyze_duration: 2_000_000, // 2 seconds
        },
    )
}

/// Result from a record+encode operation.
#[derive(Debug, Clone)]
pub struct RecordResult {
    /// Number of packets written to the output.
    pub packets_written: u64,
    /// Wall-clock seconds actually recorded.
    pub duration_recorded_secs: f64,
}

/// Record a network stream directly to an encoded file (single-pass).
///
/// Opens the network URL with reconnect options, decodes video+audio,
/// encodes with the specified codecs, and writes to the output file.
/// The output format is auto-detected from the file extension (e.g., `.mp4`).
/// For MP4 output, `movflags=+faststart` is set for streaming compatibility.
///
/// `max_duration_secs` limits recording by wall-clock time.
// Deep video+audio decode/filter pipeline — collapsing the outer `if let`
// chains here would need a risky reflow of every match arm body.
#[allow(clippy::collapsible_if)]
pub fn record_and_encode(
    url: &str,
    output: &Path,
    video_encoder: &str,
    audio_encoder: &str,
    audio_bitrate: i64,
    max_duration_secs: Option<f64>,
    run: Option<&crate::progress::RunContext>,
) -> Result<RecordResult> {
    let mut in_ctx = InputContext::open_with_options(
        url,
        &[
            ("reconnect", "1"),
            ("reconnect_streamed", "1"),
            ("reconnect_delay_max", "30"),
        ],
    )?;
    in_ctx.find_stream_info()?;

    let video_idx = in_ctx.find_best_stream(MediaType::Video);
    let audio_idx = in_ctx.find_best_stream(MediaType::Audio);

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create(output_str)?;

    // Enable faststart for MP4 (moov atom at beginning for streaming)
    let _ = out_ctx.set_option("movflags", "+faststart");

    // Set up video decoder + encoder
    let mut video_decoder = None;
    let mut video_encoder_ctx = None;
    if let Some(v_idx) = video_idx {
        let stream = in_ctx.stream(v_idx).unwrap();
        let dec = stream.decoder()?;
        let enc = Encoder::video(VideoEncoderConfig {
            codec_name: video_encoder.to_string(),
            width: dec.width(),
            height: dec.height(),
            pixel_format: dec.pixel_format(),
            time_base: stream.time_base_native(),
            bit_rate: None,
            gop_size: None,
            global_header: out_ctx.requires_global_header(),
            quality: None,
            hw_frames_ctx: None,
            sample_aspect_ratio: None,
            frame_rate: crate::codec::encoder::output_frame_rate(stream.r_frame_rate(), false),
            color: unsafe {
                crate::filter::graph::VideoColorInfo::from_codec_parameters(
                    stream.codec_parameters(),
                )
            },
            options: Vec::new(),
        })?;
        out_ctx.add_stream_from_encoder(&enc)?;
        video_decoder = Some(dec);
        video_encoder_ctx = Some(enc);
    }

    // Set up audio decoder + filter (stereo downmix + frame size) + encoder.
    // Audio filter is created lazily from the first decoded frame's actual properties,
    // not from the decoder context (AC3 channel layout mismatch).
    let mut audio_decoder = None;
    let mut audio_encoder_ctx = None;
    let mut audio_filter: Option<crate::filter::graph::FilterGraph> = None;
    let mut audio_filter_state: Option<crate::audio::AudioGraphState> = None;
    let mut audio_filter_str: Option<String> = None;
    let mut audio_filter_tb = ffi::AVRational { num: 0, den: 1 };
    let mut buffered_audio_frames: VecDeque<Frame> = VecDeque::new();
    let mut buffered_packets: VecDeque<Packet> = VecDeque::new();
    if let Some(a_idx) = audio_idx {
        let stream_tb = in_ctx.stream(a_idx).unwrap().time_base();
        let mut dec = in_ctx.stream(a_idx).unwrap().decoder()?;
        let effective_sample_rate = crate::audio::probe_audio_sample_rate(
            &mut in_ctx,
            a_idx,
            &mut dec,
            &mut buffered_packets,
            &mut buffered_audio_frames,
        )?;
        let sample_format = crate::codec::encoder::default_audio_sample_format(audio_encoder)?;
        let enc = Encoder::audio(AudioEncoderConfig {
            codec_name: audio_encoder.to_string(),
            sample_rate: effective_sample_rate,
            channel_layout: crate::types::ChannelLayout::stereo(),
            sample_format,
            bit_rate: Some(audio_bitrate),
            time_base: crate::types::TimeBase::new(1, effective_sample_rate),
            global_header: out_ctx.requires_global_header(),
        })?;

        audio_filter_str = Some(format!(
            "aformat=sample_fmts={}:sample_rates={}:channel_layouts=stereo,asetnsamples=n={}:p=0",
            sample_format.name(),
            effective_sample_rate,
            enc.frame_size().max(1024),
        ));
        audio_filter_tb = stream_tb;

        out_ctx.add_stream_from_encoder(&enc)?;
        audio_decoder = Some(dec);
        audio_encoder_ctx = Some(enc);
    }

    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut filtered_frame = Frame::new()?;
    let mut out_packet = Packet::new()?;
    let mut packets_written = 0u64;
    let start_time = std::time::Instant::now();

    // Track first PTS per stream so we can rebase to zero.
    // Broadcast streams carry wall-clock PTS that inflate duration and desync A/V.
    let mut video_pts_offset: Option<i64> = None;
    let mut audio_pts_offset: Option<i64> = None;

    // Drain frames captured during sample-rate pre-roll into filter+encoder.
    if let Some(enc) = audio_encoder_ctx.as_mut() {
        let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
        while let Some(mut buf_frame) = buffered_audio_frames.pop_front() {
            let pts = buf_frame.pts();
            let offset = *audio_pts_offset.get_or_insert(pts);
            buf_frame.set_pts(pts - offset);
            if let Some(ref fs) = audio_filter_str
                && crate::audio::AudioGraphState::needs_rebuild(&audio_filter_state, &buf_frame)
            {
                if let Some(ref mut af) = audio_filter {
                    af.push_eof()?;
                    while af.pull_frame(&mut filtered_frame)? {
                        enc.send_frame(&filtered_frame)?;
                        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                            unsafe {
                                (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                            }
                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                            packets_written += 1;
                        }
                        filtered_frame.unref();
                    }
                }
                crate::audio::AudioGraphState::rebuild_filter_graph(
                    &mut audio_filter,
                    &mut audio_filter_state,
                    &buf_frame,
                    audio_filter_tb,
                    fs,
                )?;
            }
            if let Some(ref mut af) = audio_filter {
                af.push_frame(&mut buf_frame)?;
                while af.pull_frame(&mut filtered_frame)? {
                    enc.send_frame(&filtered_frame)?;
                    while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                        unsafe {
                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                        }
                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                        packets_written += 1;
                    }
                    filtered_frame.unref();
                }
            } else {
                enc.send_frame(&buf_frame)?;
                while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                    unsafe {
                        (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                    }
                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                    packets_written += 1;
                }
            }
        }
    }

    // Main read → decode → encode → write loop.
    // Stream errors are caught so we can flush encoders and write the trailer,
    // producing a playable partial file instead of a corrupt one.
    let read_error = 'read_loop: {
        loop {
            if let Some(buffered) = buffered_packets.pop_front() {
                packet = buffered;
            } else {
                match in_ctx.read_packet(&mut packet) {
                    Ok(true) => {}
                    Ok(false) => break 'read_loop None, // EOF
                    Err(e) => {
                        if packets_written > 0 {
                            // Stream died after writing data — break out to flush/trailer
                            break 'read_loop Some(e);
                        }
                        // No data captured — nothing to salvage
                        return Err(e);
                    }
                }
            }

            if let Err(e) = crate::progress::check_cancel(run) {
                break 'read_loop Some(e);
            }

            if let Some(max_dur) = max_duration_secs
                && start_time.elapsed().as_secs_f64() >= max_dur
            {
                break;
            }

            let pkt_stream = packet.stream_index();

            if let Some(v_idx) = video_idx
                && pkt_stream == v_idx as i32
                && let (Some(dec), Some(enc)) = (&mut video_decoder, &mut video_encoder_ctx)
            {
                dec.send_packet(&packet)?;
                loop {
                    match dec.receive_frame(&mut frame)? {
                        DecodeResult::Frame => {
                            let pts = frame.pts();
                            let offset = *video_pts_offset.get_or_insert(pts);
                            frame.set_pts(pts - offset);
                            enc.send_frame(&frame)?;
                            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                                unsafe {
                                    (*out_packet.as_mut_ptr()).stream_index = 0;
                                }
                                out_ctx.write_packet_interleaved(&mut out_packet)?;
                                packets_written += 1;
                            }
                        }
                        DecodeResult::NeedsInput | DecodeResult::Eof => break,
                    }
                }
            }

            if let Some(a_idx) = audio_idx
                && pkt_stream == a_idx as i32
            {
                let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
                if let (Some(dec), Some(enc)) = (&mut audio_decoder, &mut audio_encoder_ctx) {
                    dec.send_packet(&packet)?;
                    loop {
                        match dec.receive_frame(&mut frame)? {
                            DecodeResult::Frame => {
                                let pts = frame.pts();
                                let offset = *audio_pts_offset.get_or_insert(pts);
                                frame.set_pts(pts - offset);
                                // Rebuild audio filter if frame properties changed
                                if let Some(ref fs) = audio_filter_str
                                    && crate::audio::AudioGraphState::needs_rebuild(
                                        &audio_filter_state,
                                        &frame,
                                    )
                                {
                                    if let Some(ref mut af) = audio_filter {
                                        af.push_eof()?;
                                        while af.pull_frame(&mut filtered_frame)? {
                                            enc.send_frame(&filtered_frame)?;
                                            while let EncodeResult::Packet =
                                                enc.receive_packet(&mut out_packet)?
                                            {
                                                unsafe {
                                                    (*out_packet.as_mut_ptr()).stream_index =
                                                        audio_out_idx;
                                                }
                                                out_ctx
                                                    .write_packet_interleaved(&mut out_packet)?;
                                                packets_written += 1;
                                            }
                                            filtered_frame.unref();
                                        }
                                    }
                                    crate::audio::AudioGraphState::rebuild_filter_graph(
                                        &mut audio_filter,
                                        &mut audio_filter_state,
                                        &frame,
                                        audio_filter_tb,
                                        fs,
                                    )?;
                                }
                                if let Some(ref mut af) = audio_filter {
                                    af.push_frame(&mut frame)?;
                                    while af.pull_frame(&mut filtered_frame)? {
                                        enc.send_frame(&filtered_frame)?;
                                        while let EncodeResult::Packet =
                                            enc.receive_packet(&mut out_packet)?
                                        {
                                            unsafe {
                                                (*out_packet.as_mut_ptr()).stream_index =
                                                    audio_out_idx;
                                            }
                                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                                            packets_written += 1;
                                        }
                                        filtered_frame.unref();
                                    }
                                } else {
                                    enc.send_frame(&frame)?;
                                    while let EncodeResult::Packet =
                                        enc.receive_packet(&mut out_packet)?
                                    {
                                        unsafe {
                                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                        }
                                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                                        packets_written += 1;
                                    }
                                }
                            }
                            DecodeResult::NeedsInput | DecodeResult::Eof => break,
                        }
                    }
                }
            }
        } // end loop
        None // normal completion
    }; // end 'read_loop block

    // Flush and finalize — best-effort when stream died mid-recording.
    // We must write the trailer to produce a playable MP4 (moov atom).
    let flush_result: Result<()> = (|| {
        // Flush video
        if let (Some(dec), Some(enc)) = (&mut video_decoder, &mut video_encoder_ctx) {
            dec.send_eof()?;
            loop {
                match dec.receive_frame(&mut frame)? {
                    DecodeResult::Frame => {
                        let pts = frame.pts();
                        let offset = *video_pts_offset.get_or_insert(pts);
                        frame.set_pts(pts - offset);
                        enc.send_frame(&frame)?;
                        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                            unsafe {
                                (*out_packet.as_mut_ptr()).stream_index = 0;
                            }
                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                            packets_written += 1;
                        }
                    }
                    _ => break,
                }
            }
            enc.send_eof()?;
            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                unsafe {
                    (*out_packet.as_mut_ptr()).stream_index = 0;
                }
                out_ctx.write_packet_interleaved(&mut out_packet)?;
                packets_written += 1;
            }
        }

        // Flush audio
        if let (Some(dec), Some(enc)) = (&mut audio_decoder, &mut audio_encoder_ctx) {
            let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
            dec.send_eof()?;
            loop {
                match dec.receive_frame(&mut frame)? {
                    DecodeResult::Frame => {
                        let pts = frame.pts();
                        let offset = *audio_pts_offset.get_or_insert(pts);
                        frame.set_pts(pts - offset);
                        if let Some(ref fs) = audio_filter_str {
                            if crate::audio::AudioGraphState::needs_rebuild(
                                &audio_filter_state,
                                &frame,
                            ) {
                                if let Some(ref mut af) = audio_filter {
                                    af.push_eof()?;
                                    while af.pull_frame(&mut filtered_frame)? {
                                        enc.send_frame(&filtered_frame)?;
                                        while let EncodeResult::Packet =
                                            enc.receive_packet(&mut out_packet)?
                                        {
                                            unsafe {
                                                (*out_packet.as_mut_ptr()).stream_index =
                                                    audio_out_idx;
                                            }
                                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                                            packets_written += 1;
                                        }
                                        filtered_frame.unref();
                                    }
                                }
                                crate::audio::AudioGraphState::rebuild_filter_graph(
                                    &mut audio_filter,
                                    &mut audio_filter_state,
                                    &frame,
                                    audio_filter_tb,
                                    fs,
                                )?;
                            }
                        }
                        if let Some(ref mut af) = audio_filter {
                            af.push_frame(&mut frame)?;
                            while af.pull_frame(&mut filtered_frame)? {
                                enc.send_frame(&filtered_frame)?;
                                while let EncodeResult::Packet =
                                    enc.receive_packet(&mut out_packet)?
                                {
                                    unsafe {
                                        (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                    }
                                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                                    packets_written += 1;
                                }
                                filtered_frame.unref();
                            }
                        } else {
                            enc.send_frame(&frame)?;
                            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                                unsafe {
                                    (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                }
                                out_ctx.write_packet_interleaved(&mut out_packet)?;
                                packets_written += 1;
                            }
                        }
                    }
                    _ => break,
                }
            }
            if let Some(ref mut af) = audio_filter {
                af.push_eof()?;
                while af.pull_frame(&mut filtered_frame)? {
                    enc.send_frame(&filtered_frame)?;
                    while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                        unsafe {
                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                        }
                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                        packets_written += 1;
                    }
                    filtered_frame.unref();
                }
            }
            enc.send_eof()?;
            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                unsafe {
                    (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                }
                out_ctx.write_packet_interleaved(&mut out_packet)?;
                packets_written += 1;
            }
        }

        out_ctx.write_trailer()?;
        Ok(())
    })();

    // If stream died but we successfully flushed and wrote the trailer,
    // return the partial result as Ok so the caller can use the segment.
    // If both the stream AND the flush failed, return the original read error.
    match (read_error, flush_result) {
        (None, Ok(())) => Ok(RecordResult {
            packets_written,
            duration_recorded_secs: start_time.elapsed().as_secs_f64(),
        }),
        (Some(_), Ok(())) => Ok(RecordResult {
            packets_written,
            duration_recorded_secs: start_time.elapsed().as_secs_f64(),
        }),
        (Some(read_err), Err(_)) => Err(read_err),
        (None, Err(flush_err)) => Err(flush_err),
    }
}

/// Concatenate multiple media segments into a single output file.
///
/// Uses the ffmpeg concat demuxer — writes a temporary file list, opens it
/// as a concat input, and copies all streams to the output (pure remux, no
/// re-encode). All segments must share the same codec configuration.
pub fn concat_segments(segments: &[&Path], output: &Path) -> Result<()> {
    if segments.is_empty() {
        return Err(crate::error::FfmpegError::new(
            ffi::AVERROR_EINVAL,
            "no segments to concatenate",
        ));
    }
    if segments.len() == 1 {
        // Single segment — just rename
        std::fs::rename(segments[0], output).map_err(|e| {
            crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!("renaming segment to output: {e}"),
            )
        })?;
        return Ok(());
    }

    // Write concat list to a temp file next to the output
    let list_path = output.with_extension("concat.txt");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&list_path).map_err(|e| {
            crate::error::FfmpegError::new(
                ffi::AVERROR_EINVAL,
                format!("creating concat list: {e}"),
            )
        })?;
        for seg in segments {
            let abs = seg.canonicalize().map_err(|e| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    format!("resolving segment path: {e}"),
                )
            })?;
            writeln!(f, "file '{}'", abs.display()).map_err(|e| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    format!("writing concat list: {e}"),
                )
            })?;
        }
    }

    let mut in_ctx = InputContext::open_concat(&list_path)?;
    in_ctx.find_stream_info()?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create(output_str)?;
    let _ = out_ctx.set_option("movflags", "+faststart");

    let nb_streams = in_ctx.nb_streams();
    for i in 0..nb_streams {
        let stream = in_ctx.stream(i).unwrap();
        out_ctx.add_stream_copy(&stream)?;
    }
    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    while in_ctx.read_packet(&mut packet)? {
        let pkt_idx = packet.stream_index();
        let in_tb = in_ctx.stream(pkt_idx as u32).unwrap().time_base();
        out_ctx.write_packet_rescaled(&mut packet, pkt_idx, in_tb)?;
    }

    out_ctx.write_trailer()?;

    // Clean up temp list
    let _ = std::fs::remove_file(&list_path);

    Ok(())
}

/// Record a network stream to a transport stream file.
///
/// Replaces: `ffmpeg -reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 30
///            -i <url> -c copy -t <duration> -y <output.ts>`
///
/// `on_progress` is called periodically with the number of packets written.
/// Return `false` from `on_progress` to stop recording early.
pub fn record_stream(
    url: &str,
    output: &Path,
    max_duration_secs: Option<f64>,
    run: Option<&crate::progress::RunContext>,
    on_progress: &mut dyn FnMut(u64) -> bool,
) -> Result<u64> {
    // Set HTTP reconnect options matching the old CLI flags:
    // -reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 30
    let mut in_ctx = InputContext::open_with_options(
        url,
        &[
            ("reconnect", "1"),
            ("reconnect_streamed", "1"),
            ("reconnect_delay_max", "30"),
        ],
    )?;
    in_ctx.find_stream_info()?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    // Explicitly specify mpegts format — the output path may use a temp extension
    // (e.g. ".ts.tmp") that ffmpeg can't auto-detect.
    let mut out_ctx = OutputContext::create_with_format(output_str, "mpegts")?;

    let nb_streams = in_ctx.nb_streams();
    for i in 0..nb_streams {
        let stream = in_ctx.stream(i).unwrap();
        out_ctx.add_stream_copy(&stream)?;
    }

    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut packets_written = 0u64;
    let start_time = std::time::Instant::now();

    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if let Some(max_dur) = max_duration_secs
            && start_time.elapsed().as_secs_f64() >= max_dur
        {
            break;
        }

        let pkt_idx = packet.stream_index();
        let in_tb = in_ctx.stream(pkt_idx as u32).unwrap().time_base();
        out_ctx.write_packet_rescaled(&mut packet, pkt_idx, in_tb)?;
        packets_written += 1;

        if packets_written.is_multiple_of(100) && !on_progress(packets_written) {
            break;
        }
    }

    out_ctx.write_trailer()?;
    Ok(packets_written)
}

/// Transcode a recorded TS file to MKV with optional hardware acceleration.
///
/// Replaces the three-way branch in iptv/recording.rs (VAAPI, NVENC, software).
// Deep video+audio transcode pipeline — collapsing the outer `if let` chains
// here would need a risky reflow of every match arm body.
#[allow(clippy::collapsible_if)]
pub fn transcode_to_file(
    input: &Path,
    output: &Path,
    video_encoder: &str,
    audio_encoder: &str,
    audio_bitrate: i64,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let video_idx = in_ctx.find_best_stream(MediaType::Video);
    let audio_idx = in_ctx.find_best_stream(MediaType::Audio);

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create(output_str)?;

    // Set up video
    let mut video_decoder = None;
    let mut video_encoder_ctx = None;
    if let Some(v_idx) = video_idx {
        let stream = in_ctx.stream(v_idx).unwrap();
        let dec = stream.decoder()?;
        let enc = Encoder::video(VideoEncoderConfig {
            codec_name: video_encoder.to_string(),
            width: dec.width(),
            height: dec.height(),
            pixel_format: dec.pixel_format(),
            time_base: stream.time_base_native(),
            bit_rate: None,
            gop_size: None,
            global_header: out_ctx.requires_global_header(),
            quality: None,
            hw_frames_ctx: None,
            sample_aspect_ratio: None,
            frame_rate: crate::codec::encoder::output_frame_rate(stream.r_frame_rate(), false),
            color: unsafe {
                crate::filter::graph::VideoColorInfo::from_codec_parameters(
                    stream.codec_parameters(),
                )
            },
            options: Vec::new(),
        })?;
        out_ctx.add_stream_from_encoder(&enc)?;
        video_decoder = Some(dec);
        video_encoder_ctx = Some(enc);
    }

    // Set up audio with filter for frame size adaptation + stereo downmix.
    // Audio filter is created lazily from the first decoded frame's actual properties.
    let mut audio_decoder = None;
    let mut audio_encoder_ctx = None;
    let mut audio_filter: Option<crate::filter::graph::FilterGraph> = None;
    let mut audio_filter_state: Option<crate::audio::AudioGraphState> = None;
    let mut audio_filter_str: Option<String> = None;
    let mut audio_filter_tb = ffi::AVRational { num: 0, den: 1 };
    let mut buffered_audio_frames: VecDeque<Frame> = VecDeque::new();
    let mut buffered_packets: VecDeque<Packet> = VecDeque::new();
    if let Some(a_idx) = audio_idx {
        let stream_tb = in_ctx.stream(a_idx).unwrap().time_base();
        let mut dec = in_ctx.stream(a_idx).unwrap().decoder()?;
        let effective_sample_rate = crate::audio::probe_audio_sample_rate(
            &mut in_ctx,
            a_idx,
            &mut dec,
            &mut buffered_packets,
            &mut buffered_audio_frames,
        )?;
        let sample_format = crate::codec::encoder::default_audio_sample_format(audio_encoder)?;
        let enc = Encoder::audio(AudioEncoderConfig {
            codec_name: audio_encoder.to_string(),
            sample_rate: effective_sample_rate,
            channel_layout: crate::types::ChannelLayout::stereo(),
            sample_format,
            bit_rate: Some(audio_bitrate),
            time_base: crate::types::TimeBase::new(1, effective_sample_rate),
            global_header: out_ctx.requires_global_header(),
        })?;

        audio_filter_str = Some(format!(
            "aformat=sample_fmts={}:sample_rates={}:channel_layouts=stereo,asetnsamples=n={}:p=0",
            sample_format.name(),
            effective_sample_rate,
            enc.frame_size().max(1024),
        ));
        audio_filter_tb = stream_tb;

        out_ctx.add_stream_from_encoder(&enc)?;
        audio_decoder = Some(dec);
        audio_encoder_ctx = Some(enc);
    }

    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut filtered_frame = Frame::new()?;
    let mut out_packet = Packet::new()?;

    // Track first PTS per stream so we can rebase to zero.
    let mut video_pts_offset: Option<i64> = None;
    let mut audio_pts_offset: Option<i64> = None;

    // Drain frames captured during sample-rate pre-roll into filter+encoder.
    if let Some(enc) = audio_encoder_ctx.as_mut() {
        let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
        while let Some(mut buf_frame) = buffered_audio_frames.pop_front() {
            let pts = buf_frame.pts();
            let offset = *audio_pts_offset.get_or_insert(pts);
            buf_frame.set_pts(pts - offset);
            if let Some(ref fs) = audio_filter_str
                && crate::audio::AudioGraphState::needs_rebuild(&audio_filter_state, &buf_frame)
            {
                if let Some(ref mut af) = audio_filter {
                    af.push_eof()?;
                    while af.pull_frame(&mut filtered_frame)? {
                        enc.send_frame(&filtered_frame)?;
                        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                            unsafe {
                                (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                            }
                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                        }
                        filtered_frame.unref();
                    }
                }
                crate::audio::AudioGraphState::rebuild_filter_graph(
                    &mut audio_filter,
                    &mut audio_filter_state,
                    &buf_frame,
                    audio_filter_tb,
                    fs,
                )?;
            }
            if let Some(ref mut af) = audio_filter {
                af.push_frame(&mut buf_frame)?;
                while af.pull_frame(&mut filtered_frame)? {
                    enc.send_frame(&filtered_frame)?;
                    while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                        unsafe {
                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                        }
                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                    }
                    filtered_frame.unref();
                }
            } else {
                enc.send_frame(&buf_frame)?;
                while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                    unsafe {
                        (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                    }
                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                }
            }
        }
    }

    loop {
        let have_packet = if let Some(buffered) = buffered_packets.pop_front() {
            packet = buffered;
            true
        } else {
            in_ctx.read_packet(&mut packet)?
        };
        if !have_packet {
            break;
        }
        crate::progress::check_cancel(run)?;
        let pkt_stream = packet.stream_index();

        if let Some(v_idx) = video_idx {
            if pkt_stream == v_idx as i32 {
                if let (Some(dec), Some(enc)) = (&mut video_decoder, &mut video_encoder_ctx) {
                    dec.send_packet(&packet)?;
                    loop {
                        match dec.receive_frame(&mut frame)? {
                            DecodeResult::Frame => {
                                let pts = frame.pts();
                                let offset = *video_pts_offset.get_or_insert(pts);
                                frame.set_pts(pts - offset);
                                enc.send_frame(&frame)?;
                                while let EncodeResult::Packet =
                                    enc.receive_packet(&mut out_packet)?
                                {
                                    unsafe {
                                        (*out_packet.as_mut_ptr()).stream_index = 0;
                                    }
                                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                                }
                            }
                            DecodeResult::NeedsInput => break,
                            DecodeResult::Eof => break,
                        }
                    }
                }
            }
        }

        if let Some(a_idx) = audio_idx {
            if pkt_stream == a_idx as i32 {
                let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
                if let (Some(dec), Some(enc)) = (&mut audio_decoder, &mut audio_encoder_ctx) {
                    dec.send_packet(&packet)?;
                    loop {
                        match dec.receive_frame(&mut frame)? {
                            DecodeResult::Frame => {
                                let pts = frame.pts();
                                let offset = *audio_pts_offset.get_or_insert(pts);
                                frame.set_pts(pts - offset);
                                // Rebuild audio filter if frame properties changed
                                if let Some(ref fs) = audio_filter_str {
                                    if crate::audio::AudioGraphState::needs_rebuild(
                                        &audio_filter_state,
                                        &frame,
                                    ) {
                                        if let Some(ref mut af) = audio_filter {
                                            af.push_eof()?;
                                            while af.pull_frame(&mut filtered_frame)? {
                                                enc.send_frame(&filtered_frame)?;
                                                while let EncodeResult::Packet =
                                                    enc.receive_packet(&mut out_packet)?
                                                {
                                                    unsafe {
                                                        (*out_packet.as_mut_ptr()).stream_index =
                                                            audio_out_idx;
                                                    }
                                                    out_ctx.write_packet_interleaved(
                                                        &mut out_packet,
                                                    )?;
                                                }
                                                filtered_frame.unref();
                                            }
                                        }
                                        crate::audio::AudioGraphState::rebuild_filter_graph(
                                            &mut audio_filter,
                                            &mut audio_filter_state,
                                            &frame,
                                            audio_filter_tb,
                                            fs,
                                        )?;
                                    }
                                }
                                if let Some(ref mut af) = audio_filter {
                                    af.push_frame(&mut frame)?;
                                    while af.pull_frame(&mut filtered_frame)? {
                                        enc.send_frame(&filtered_frame)?;
                                        while let EncodeResult::Packet =
                                            enc.receive_packet(&mut out_packet)?
                                        {
                                            unsafe {
                                                (*out_packet.as_mut_ptr()).stream_index =
                                                    audio_out_idx;
                                            }
                                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                                        }
                                        filtered_frame.unref();
                                    }
                                } else {
                                    enc.send_frame(&frame)?;
                                    while let EncodeResult::Packet =
                                        enc.receive_packet(&mut out_packet)?
                                    {
                                        unsafe {
                                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                        }
                                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                                    }
                                }
                            }
                            DecodeResult::NeedsInput => break,
                            DecodeResult::Eof => break,
                        }
                    }
                }
            }
        }
    }

    // Flush
    if let (Some(dec), Some(enc)) = (&mut video_decoder, &mut video_encoder_ctx) {
        dec.send_eof()?;
        loop {
            match dec.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    let pts = frame.pts();
                    let offset = *video_pts_offset.get_or_insert(pts);
                    frame.set_pts(pts - offset);
                    enc.send_frame(&frame)?;
                    while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                        unsafe {
                            (*out_packet.as_mut_ptr()).stream_index = 0;
                        }
                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                    }
                }
                _ => break,
            }
        }
        enc.send_eof()?;
        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
            unsafe {
                (*out_packet.as_mut_ptr()).stream_index = 0;
            }
            out_ctx.write_packet_interleaved(&mut out_packet)?;
        }
    }

    if let (Some(dec), Some(enc)) = (&mut audio_decoder, &mut audio_encoder_ctx) {
        let audio_out_idx = if video_idx.is_some() { 1 } else { 0 };
        dec.send_eof()?;
        loop {
            match dec.receive_frame(&mut frame)? {
                DecodeResult::Frame => {
                    let pts = frame.pts();
                    let offset = *audio_pts_offset.get_or_insert(pts);
                    frame.set_pts(pts - offset);
                    if let Some(ref fs) = audio_filter_str {
                        if crate::audio::AudioGraphState::needs_rebuild(&audio_filter_state, &frame)
                        {
                            if let Some(ref mut af) = audio_filter {
                                af.push_eof()?;
                                while af.pull_frame(&mut filtered_frame)? {
                                    enc.send_frame(&filtered_frame)?;
                                    while let EncodeResult::Packet =
                                        enc.receive_packet(&mut out_packet)?
                                    {
                                        unsafe {
                                            (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                        }
                                        out_ctx.write_packet_interleaved(&mut out_packet)?;
                                    }
                                    filtered_frame.unref();
                                }
                            }
                            crate::audio::AudioGraphState::rebuild_filter_graph(
                                &mut audio_filter,
                                &mut audio_filter_state,
                                &frame,
                                audio_filter_tb,
                                fs,
                            )?;
                        }
                    }
                    if let Some(ref mut af) = audio_filter {
                        af.push_frame(&mut frame)?;
                        while af.pull_frame(&mut filtered_frame)? {
                            enc.send_frame(&filtered_frame)?;
                            while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                                unsafe {
                                    (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                                }
                                out_ctx.write_packet_interleaved(&mut out_packet)?;
                            }
                            filtered_frame.unref();
                        }
                    } else {
                        enc.send_frame(&frame)?;
                        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                            unsafe {
                                (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                            }
                            out_ctx.write_packet_interleaved(&mut out_packet)?;
                        }
                    }
                }
                _ => break,
            }
        }
        if let Some(ref mut af) = audio_filter {
            af.push_eof()?;
            while af.pull_frame(&mut filtered_frame)? {
                enc.send_frame(&filtered_frame)?;
                while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
                    unsafe {
                        (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
                    }
                    out_ctx.write_packet_interleaved(&mut out_packet)?;
                }
                filtered_frame.unref();
            }
        }
        enc.send_eof()?;
        while let EncodeResult::Packet = enc.receive_packet(&mut out_packet)? {
            unsafe {
                (*out_packet.as_mut_ptr()).stream_index = audio_out_idx;
            }
            out_ctx.write_packet_interleaved(&mut out_packet)?;
        }
    }

    out_ctx.write_trailer()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_test_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new()
            .suffix(".mp4")
            .tempfile()
            .expect("create temp file");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.5:size=160x120:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.5",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg");
        assert!(status.success());
        tmp
    }

    #[test]
    fn test_record_from_file_url() {
        let src = generate_test_file();
        let dst = tempfile::Builder::new()
            .suffix(".ts")
            .tempfile()
            .expect("create temp file");

        let url = format!("file:{}", src.path().display());
        let mut progress_called = false;
        let packets = record_stream(&url, dst.path(), None, None, &mut |_| {
            progress_called = true;
            true
        })
        .unwrap();

        assert!(packets > 0, "should have recorded packets");
        assert!(
            dst.path().metadata().unwrap().len() > 0,
            "output file should be non-empty"
        );
    }

    #[test]
    fn test_mkv_output_basic() {
        use crate::codec::encoder::{Encoder, VideoEncoderConfig};

        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("basic.mkv");

        // Create output first to check global_header requirement
        let mut out_ctx = OutputContext::create(dst.to_str().unwrap()).unwrap();
        let needs_gh = out_ctx.requires_global_header();

        let enc = Encoder::video(VideoEncoderConfig {
            codec_name: "libx264".to_string(),
            width: 160,
            height: 120,
            pixel_format: crate::types::PixelFormat::YUV420P,
            time_base: crate::types::TimeBase::new(1, 25),
            bit_rate: None,
            gop_size: Some(12),
            global_header: needs_gh,
            quality: None,
            hw_frames_ctx: None,
            sample_aspect_ratio: None,
            frame_rate: None,
            color: Default::default(),
            options: Vec::new(),
        })
        .unwrap();

        out_ctx.add_stream_from_encoder(&enc).unwrap();
        out_ctx.write_header().unwrap();
        out_ctx.write_trailer().unwrap();
    }

    #[test]
    fn test_transcode_to_mkv() {
        let src = generate_test_file();
        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("output.mkv");

        transcode_to_file(src.path(), &dst, "libx264", "aac", 128000, None).unwrap();

        // Verify output
        let mut ctx = InputContext::open_path(&dst).unwrap();
        ctx.find_stream_info().unwrap();
        assert!(ctx.nb_streams() >= 2, "should have video + audio streams");
        assert!(ctx.duration_secs().is_some());
    }

    #[test]
    fn test_record_and_encode_to_mp4() {
        let src = generate_test_file();
        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("encoded.mp4");

        let url = format!("file:{}", src.path().display());
        let result = record_and_encode(&url, &dst, "libx264", "aac", 128_000, None, None).unwrap();

        assert!(result.packets_written > 0, "should have written packets");
        assert!(result.duration_recorded_secs >= 0.0);

        // Verify output is valid MP4 with video + audio
        let mut ctx = InputContext::open_path(&dst).unwrap();
        ctx.find_stream_info().unwrap();
        assert!(ctx.nb_streams() >= 2, "should have video + audio streams");
        assert!(
            dst.metadata().unwrap().len() > 0,
            "output file should be non-empty"
        );
    }

    #[test]
    fn test_concat_segments() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Create two small encoded segments
        let src = generate_test_file();
        let seg1 = dir.path().join("seg1.mp4");
        let seg2 = dir.path().join("seg2.mp4");

        let url = format!("file:{}", src.path().display());
        record_and_encode(&url, &seg1, "libx264", "aac", 128_000, None, None).unwrap();
        record_and_encode(&url, &seg2, "libx264", "aac", 128_000, None, None).unwrap();

        // Concatenate
        let output = dir.path().join("combined.mp4");
        concat_segments(&[seg1.as_path(), seg2.as_path()], &output).unwrap();

        // Verify combined output
        let mut ctx = InputContext::open_path(&output).unwrap();
        ctx.find_stream_info().unwrap();
        assert!(ctx.nb_streams() >= 2, "should have video + audio streams");
        assert!(
            output.metadata().unwrap().len() > 0,
            "output file should be non-empty"
        );

        // Segments should have been left in place (caller manages cleanup)
        assert!(seg1.exists());
        assert!(seg2.exists());
    }

    #[test]
    fn test_concat_single_segment_renames() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = generate_test_file();
        let seg = dir.path().join("only.mp4");

        let url = format!("file:{}", src.path().display());
        record_and_encode(&url, &seg, "libx264", "aac", 128_000, None, None).unwrap();

        let output = dir.path().join("final.mp4");
        concat_segments(&[seg.as_path()], &output).unwrap();

        // Single segment should have been renamed
        assert!(output.exists());
        assert!(!seg.exists(), "original segment should have been renamed");
    }

    /// Generate a test file whose PTS starts at a large offset (simulating broadcast PTS).
    fn generate_offset_test_file() -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new()
            .suffix(".ts")
            .tempfile()
            .expect("create temp file");
        // Use itsoffset to shift PTS by 5 hours (18000s), simulating broadcast timestamps.
        // Output as MPEG-TS which preserves the offset (MP4 would normalize it).
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-itsoffset",
                "18000",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.5:size=160x120:rate=10",
                "-itsoffset",
                "18000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.5",
                "-c:v",
                "mpeg2video",
                "-c:a",
                "mp2",
                "-f",
                "mpegts",
                "-y",
            ])
            .arg(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg");
        assert!(status.success());
        tmp
    }

    #[test]
    fn test_record_and_encode_rebases_pts_to_zero() {
        let src = generate_offset_test_file();
        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("rebased.mp4");

        let url = format!("file:{}", src.path().display());
        let result = record_and_encode(&url, &dst, "libx264", "aac", 128_000, None, None).unwrap();
        assert!(result.packets_written > 0);

        // Probe the output — start time should be near 0, not hours into a broadcast day
        let mut ctx = InputContext::open_path(&dst).unwrap();
        ctx.find_stream_info().unwrap();

        let start_us = ctx.start_time_us().unwrap_or(0);
        let start_secs = start_us as f64 / 1_000_000.0;
        assert!(
            start_secs.abs() < 5.0,
            "output start time should be near 0, got {start_secs}s"
        );

        let duration = ctx.duration_secs().unwrap_or(0.0);
        assert!(
            duration < 10.0,
            "output duration should reflect actual content (~0.5s), got {duration}s"
        );
    }

    #[test]
    fn test_transcode_rebases_pts_to_zero() {
        let src = generate_offset_test_file();
        let dir = tempfile::tempdir().expect("create temp dir");
        let dst = dir.path().join("rebased.mkv");

        transcode_to_file(src.path(), &dst, "libx264", "aac", 128_000, None).unwrap();

        let mut ctx = InputContext::open_path(&dst).unwrap();
        ctx.find_stream_info().unwrap();

        let start_us = ctx.start_time_us().unwrap_or(0);
        let start_secs = start_us as f64 / 1_000_000.0;
        assert!(
            start_secs.abs() < 5.0,
            "output start time should be near 0, got {start_secs}s"
        );

        // Duration should be far less than the 18000s offset we injected.
        // The audio filter graph may add some overhead beyond the ~0.5s source,
        // but it must not reflect the broadcast PTS origin.
        let duration = ctx.duration_secs().unwrap_or(0.0);
        assert!(
            duration < 120.0,
            "output duration should not reflect broadcast PTS offset, got {duration}s"
        );
    }
}
