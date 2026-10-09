//! Zero-copy remux operations.
//!
//! Remuxing copies packets between containers without re-encoding,
//! used for debrid streaming (fMP4 output) and format conversion.

use std::path::Path;

use crate::format::input::InputContext;
use crate::format::output::OutputContext;
use crate::packet::Packet;
use crate::{Result, ffi};

/// Remux a file to a different container format.
///
/// This copies all streams without re-encoding — only the container changes.
pub fn remux(input: &Path, output: &Path, run: Option<&crate::progress::RunContext>) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create(output_str)?;

    // Map all streams
    let mut stream_map = Vec::new();
    for i in 0..in_ctx.nb_streams() {
        let stream = in_ctx.stream(i).unwrap();
        let out_idx = out_ctx.add_stream_copy(&stream)?;
        stream_map.push(out_idx);
    }

    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        let in_stream_idx = packet.stream_index();
        if in_stream_idx >= 0 && (in_stream_idx as usize) < stream_map.len() {
            let out_stream_idx = stream_map[in_stream_idx as usize];
            let in_tb = in_ctx.stream(in_stream_idx as u32).unwrap().time_base();
            out_ctx.write_packet_rescaled(&mut packet, out_stream_idx, in_tb)?;
        }
    }

    out_ctx.write_trailer()?;
    Ok(())
}

/// Remux with fragmented MP4 output (for streaming).
///
/// Replaces: `ffmpeg -i pipe:0 -c:v copy -c:a aac -f mp4 -movflags frag_keyframe+empty_moov pipe:1`
pub fn remux_to_fragmented_mp4(
    input: &Path,
    output: &Path,
    run: Option<&crate::progress::RunContext>,
) -> Result<()> {
    let mut in_ctx = InputContext::open_path(input)?;
    in_ctx.find_stream_info()?;

    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create_with_format(output_str, "mp4")?;

    let mut stream_map = Vec::new();
    for i in 0..in_ctx.nb_streams() {
        let stream = in_ctx.stream(i).unwrap();
        let out_idx = out_ctx.add_stream_copy(&stream)?;
        stream_map.push(out_idx);
    }

    // Set fragmented MP4 flags
    out_ctx.set_option("movflags", "frag_keyframe+empty_moov")?;
    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;
        let idx = packet.stream_index();
        if idx >= 0 && (idx as usize) < stream_map.len() {
            let in_tb = in_ctx.stream(idx as u32).unwrap().time_base();
            out_ctx.write_packet_rescaled(&mut packet, stream_map[idx as usize], in_tb)?;
        }
    }

    out_ctx.write_trailer()?;
    Ok(())
}
