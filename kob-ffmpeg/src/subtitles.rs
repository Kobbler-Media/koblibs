//! Subtitle extraction from containers.
//!
//! Replaces: `ffmpeg -i file -map 0:<idx> -c:s copy output.srt`

use std::path::Path;

use crate::format::input::{InputContext, MediaType};
use crate::format::output::OutputContext;
use crate::packet::Packet;
use crate::{Result, ff_check, ffi};

/// A subtitle stream available for extraction.
#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub stream_index: i32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
}

/// List all subtitle tracks in a media file.
pub fn list_subtitle_tracks(path: &Path) -> Result<Vec<SubtitleTrack>> {
    let mut ctx = InputContext::open_path(path)?;
    ctx.find_stream_info()?;

    let mut tracks = Vec::new();
    for stream in ctx.streams() {
        if stream.codec_type() != MediaType::Subtitle {
            continue;
        }
        let params = stream.codec_parameters();
        let codec = unsafe {
            let name = ffi::avcodec_get_name((*params).codec_id);
            if name.is_null() {
                "unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        let meta = stream.metadata();
        let disposition = stream.disposition();
        tracks.push(SubtitleTrack {
            stream_index: stream.index(),
            codec,
            language: meta.get("language").filter(|l| l != "und"),
            title: meta.get("title"),
            is_default: disposition & ffi::AV_DISPOSITION_DEFAULT as i32 != 0,
            is_forced: disposition & ffi::AV_DISPOSITION_FORCED as i32 != 0,
        });
    }
    Ok(tracks)
}

/// Extract a single subtitle stream to a file.
pub fn extract_subtitle(
    input: &Path,
    stream_index: i32,
    output: &Path,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let stream = in_ctx.stream(stream_index as u32).ok_or_else(|| {
        crate::error::FfmpegError::new(
            ffi::AVERROR_STREAM_NOT_FOUND,
            format!("stream {stream_index} not found"),
        )
    })?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;

    let mut out_ctx = OutputContext::create(output_str)?;
    let out_stream_idx = out_ctx.add_stream_copy(&stream)?;
    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        if packet.stream_index() == stream_index {
            let in_tb = in_ctx.stream(stream_index as u32).unwrap().time_base();
            out_ctx.write_packet_rescaled(&mut packet, out_stream_idx, in_tb)?;
        }
    }

    out_ctx.write_trailer()?;
    Ok(())
}

/// Extract multiple subtitle streams in a single pass.
///
/// Returns the number of files written.
pub fn extract_subtitles_batch(
    input: &Path,
    extractions: &[(i32, &Path)], // (stream_index, output_path)
    run: Option<&crate::progress::RunContext>,
) -> Result<usize> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let mut outputs: Vec<(i32, i32, OutputContext)> = Vec::new(); // (in_idx, out_idx, ctx)

    for (stream_idx, output_path) in extractions {
        let stream = in_ctx.stream(*stream_idx as u32).ok_or_else(|| {
            crate::error::FfmpegError::new(
                ffi::AVERROR_STREAM_NOT_FOUND,
                format!("stream {} not found", stream_idx),
            )
        })?;

        let output_str = output_path.to_str().ok_or_else(|| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
        })?;

        let mut out_ctx = OutputContext::create(output_str)?;
        let out_stream_idx = out_ctx.add_stream_copy(&stream)?;
        out_ctx.write_header()?;
        outputs.push((*stream_idx, out_stream_idx, out_ctx));
    }

    let mut packet = Packet::new()?;
    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        let pkt_idx = packet.stream_index();
        for (in_idx, out_idx, out_ctx) in &mut outputs {
            if pkt_idx == *in_idx {
                // Clone packet for each output (write_packet_interleaved may consume it)
                let mut clone_pkt = Packet::new()?;
                unsafe {
                    ff_check!(
                        ffi::av_packet_ref(clone_pkt.as_mut_ptr(), packet.as_ptr()),
                        "referencing subtitle packet"
                    )?;
                }
                let in_tb = in_ctx.stream(pkt_idx as u32).unwrap().time_base();
                out_ctx.write_packet_rescaled(&mut clone_pkt, *out_idx, in_tb)?;
            }
        }
    }

    for (_, _, mut out_ctx) in outputs {
        out_ctx.write_trailer()?;
    }

    Ok(extractions.len())
}

/// One subtitle line, its timing in seconds and its text with markup removed.
#[derive(Debug, Clone, PartialEq)]
pub struct TextCue {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct TextTrackCues {
    pub stream_index: i32,
    pub codec: String,
    pub cues: Vec<TextCue>,
}

pub const TEXT_CODECS: &[&str] = &["subrip", "srt", "ass", "ssa", "webvtt", "mov_text", "text"];

/// Every text subtitle stream's cues, read in one pass and only as far as [until_secs]:
/// the opening stretch of a file is enough to tell a track's language and density, and
/// on a file still downloading it is the only part there is.
pub fn read_text_cues(input: &Path, until_secs: Option<f64>) -> Result<Vec<TextTrackCues>> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let mut time_bases = Vec::new();
    let mut tracks: Vec<TextTrackCues> = Vec::new();
    for stream in in_ctx.streams() {
        let tb = stream.time_base();
        time_bases.push(f64::from(tb.num) / f64::from(tb.den.max(1)));
        if stream.codec_type() != MediaType::Subtitle {
            continue;
        }
        let codec = unsafe {
            let name = ffi::avcodec_get_name((*stream.codec_parameters()).codec_id);
            if name.is_null() {
                continue;
            }
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        };
        if TEXT_CODECS.contains(&codec.as_str()) {
            tracks.push(TextTrackCues {
                stream_index: stream.index(),
                codec,
                cues: Vec::new(),
            });
        }
    }
    if tracks.is_empty() {
        return Ok(tracks);
    }

    let mut packet = Packet::new()?;
    loop {
        // A file still downloading ends in bytes not yet written: what was read before
        // them is still a reading.
        match in_ctx.read_packet(&mut packet) {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) if tracks.iter().any(|t| !t.cues.is_empty()) => break,
            Err(e) => return Err(e),
        }
        let idx = packet.stream_index();
        let tb = time_bases.get(idx as usize).copied().unwrap_or(0.0);
        let ts = if packet.pts() != ffi::AV_NOPTS_VALUE {
            packet.pts()
        } else {
            packet.dts()
        };
        let start = ts as f64 * tb;
        // Packets are interleaved by time, so the first one past the limit ends the read.
        if ts != ffi::AV_NOPTS_VALUE && until_secs.is_some_and(|limit| start > limit) {
            break;
        }
        let Some(track) = tracks.iter_mut().find(|t| t.stream_index == idx) else {
            continue;
        };
        let text = cue_text(&track.codec, packet.data());
        if !text.is_empty() {
            track.cues.push(TextCue {
                start_secs: start,
                end_secs: start + packet.duration() as f64 * tb,
                text,
            });
        }
    }
    Ok(tracks)
}

fn cue_text(codec: &str, data: &[u8]) -> String {
    let raw = match codec {
        // A 16-bit length, then the text, then style boxes.
        "mov_text" if data.len() >= 2 => {
            let len = usize::from(u16::from_be_bytes([data[0], data[1]]));
            String::from_utf8_lossy(&data[2..(2 + len).min(data.len())]).into_owned()
        }
        // Matroska stores an event's fields up to and including Effect before its text.
        "ass" | "ssa" => {
            let line = String::from_utf8_lossy(data);
            let text = line.splitn(9, ',').nth(8).unwrap_or(&line).to_string();
            strip_between(&text, '{', '}')
                .replace("\\N", "\n")
                .replace("\\n", "\n")
                .replace("\\h", " ")
        }
        _ => String::from_utf8_lossy(data).into_owned(),
    };
    strip_between(&raw, '<', '>').trim().to_string()
}

fn strip_between(text: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        if c == open {
            depth += 1;
        } else if c == close && depth > 0 {
            depth -= 1;
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}
