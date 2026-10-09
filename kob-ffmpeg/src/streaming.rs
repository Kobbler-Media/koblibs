//! Streaming operations for debrid and live media.
//!
//! Provides the "probe then remux" pattern: open a URL or custom I/O source,
//! probe it for stream info, then remux packets to an output — all on the
//! same connection. This eliminates the dual-connection problem in debrid
//! streaming where you'd otherwise need to open the URL twice.

use std::path::Path;

use crate::format::input::InputContext;
use crate::format::output::OutputContext;
use crate::packet::Packet;
use crate::probe::{ProbeOptions, ProbeResult};
use crate::{Result, ffi};

/// Result of probing a stream — contains both the probe result and the
/// still-open input context, ready for further packet reading.
pub struct ProbedStream {
    pub probe_result: ProbeResult,
    pub input: InputContext,
}

/// Open and probe a URL, keeping the connection alive for subsequent operations.
///
/// Returns the probe result and the open InputContext. The caller can then
/// read packets from the same context without reconnecting.
pub fn probe_and_hold(url: &str) -> Result<ProbedStream> {
    probe_and_hold_with_options(
        url,
        ProbeOptions {
            probesize: 131072,
            max_analyze_duration: 2_000_000,
        },
    )
}

/// Probe with custom options, keeping the connection alive.
pub fn probe_and_hold_with_options(url: &str, opts: ProbeOptions) -> Result<ProbedStream> {
    let mut ctx = InputContext::open(url)?;
    ctx.set_probesize(opts.probesize);
    ctx.set_max_analyze_duration(opts.max_analyze_duration);
    ctx.find_stream_info()?;

    let probe_result = crate::probe::extract_probe_result(&ctx);

    Ok(ProbedStream {
        probe_result,
        input: ctx,
    })
}

/// Open a custom I/O source, probe it, and keep it alive.
pub fn probe_and_hold_custom(
    io: &mut crate::format::io_context::CustomIoContext,
) -> Result<ProbedStream> {
    let mut ctx = InputContext::open_custom(io)?;
    ctx.find_stream_info()?;

    let probe_result = crate::probe::extract_probe_result(&ctx);

    Ok(ProbedStream {
        probe_result,
        input: ctx,
    })
}

/// Remux from a probed stream to an output file.
///
/// Copies all streams without re-encoding. The input context is consumed.
pub fn remux_probed_to_file(
    probed: ProbedStream,
    output: &Path,
    run: Option<&crate::progress::RunContext>,
) -> Result<u64> {
    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create(output_str)?;

    remux_probed_to_output(probed, &mut out_ctx, run)
}

/// Remux from a probed stream to a fragmented MP4 output file.
///
/// Ideal for debrid streaming — produces fMP4 that can be streamed progressively.
pub fn remux_probed_to_fragmented_mp4(
    probed: ProbedStream,
    output: &Path,
    run: Option<&crate::progress::RunContext>,
) -> Result<u64> {
    let output_str = output.to_str().ok_or_else(|| {
        crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "output path not valid UTF-8")
    })?;
    let mut out_ctx = OutputContext::create_with_format(output_str, "mp4")?;
    out_ctx.set_option("movflags", "frag_keyframe+empty_moov")?;

    remux_probed_to_output(probed, &mut out_ctx, run)
}

/// Core remux logic from a probed stream to any output context.
fn remux_probed_to_output(
    probed: ProbedStream,
    out_ctx: &mut OutputContext,
    run: Option<&crate::progress::RunContext>,
) -> Result<u64> {
    let mut in_ctx = probed.input;

    let mut stream_map = Vec::new();
    for i in 0..in_ctx.nb_streams() {
        let stream = in_ctx.stream(i).unwrap();
        let out_idx = out_ctx.add_stream_copy(&stream)?;
        stream_map.push(out_idx);
    }

    out_ctx.write_header()?;

    let mut packet = Packet::new()?;
    let mut packets = 0u64;

    while in_ctx.read_packet(&mut packet)? {
        crate::progress::check_cancel(run)?;

        let idx = packet.stream_index();
        if idx >= 0 && (idx as usize) < stream_map.len() {
            let in_tb = in_ctx.stream(idx as u32).unwrap().time_base();
            out_ctx.write_packet_rescaled(&mut packet, stream_map[idx as usize], in_tb)?;
            packets += 1;
        }
    }

    out_ctx.write_trailer()?;
    Ok(packets)
}
