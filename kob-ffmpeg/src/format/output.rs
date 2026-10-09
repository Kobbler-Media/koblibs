//! Output format context wrapper (AVFormatContext for writing).

use std::ffi::CString;
use std::ptr;

use crate::bsf::BitstreamFilter;
use crate::ffi;
use crate::packet::Packet;
use crate::{Result, ff_check};

/// Per-stream DTS tracking for monotonicity correction.
///
/// Some demuxer sources emit non-monotonic DTS: IPTV reconnects/feed switches
/// reset timestamps mid-stream, and MKV + B-frames + backward seeks surface
/// out-of-order packets as the reorder buffer refills. Newer FFmpeg rejects
/// non-monotonic DTS with EINVAL instead of warning, so we shift timestamps
/// forward here to keep the muxer happy. No-op for well-formed streams —
/// offset stays 0 and every packet is a literal passthrough.
#[derive(Debug, Default)]
struct DtsTracker {
    /// Last DTS written to the muxer (in output time_base units).
    last_dts: i64,
    /// Cumulative offset added to all timestamps for this stream.
    offset: i64,
    /// Whether we've seen at least one packet (to initialize last_dts).
    initialized: bool,
}

/// Owns an `AVFormatContext` opened for writing.
/// Automatically finalized and freed on drop.
///
/// `Send` but not `Sync`.
pub struct OutputContext {
    ptr: *mut ffi::AVFormatContext,
    header_written: bool,
    /// Custom AVIO context for channel-based output. Holds the avio pointer and the
    /// boxed opaque data (channel sender) to keep it alive.
    custom_io: Option<(*mut ffi::AVIOContext, Box<Box<dyn std::any::Any + Send>>)>,
    /// Per-stream bitstream filters (e.g., aac_adtstoasc for AAC in MP4).
    /// Indexed by output stream index. `None` entries mean no filtering needed.
    bsf_filters: Vec<Option<BitstreamFilter>>,
    /// Per-stream DTS monotonicity correction state. Always active — corrects
    /// backward DTS jumps (IPTV reconnects, MKV seek reorder-buffer refill) so
    /// the muxer never sees non-monotonic DTS. No-op for well-formed streams.
    dts_correction: Vec<DtsTracker>,
}

unsafe impl Send for OutputContext {}

impl OutputContext {
    /// Create an output context for a file path.
    /// The format is auto-detected from the filename extension.
    pub fn create(path: &str) -> Result<Self> {
        crate::init();
        let c_path = CString::new(path).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path contains null byte")
        })?;

        let mut ptr: *mut ffi::AVFormatContext = ptr::null_mut();
        unsafe {
            ff_check!(
                ffi::avformat_alloc_output_context2(
                    &mut ptr,
                    ptr::null(),
                    ptr::null(),
                    c_path.as_ptr(),
                ),
                "creating output context for {}",
                path
            )?;
        }

        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "avformat_alloc_output_context2 returned null",
            ));
        }

        // Open the output file (unless it's a special format like null)
        let flags = unsafe { ffi::kff_fmt_oformat(ptr) };
        let needs_file =
            unsafe { !flags.is_null() && (*flags).flags & ffi::AVFMT_NOFILE as i32 == 0 };

        if needs_file {
            unsafe {
                ff_check!(
                    ffi::avio_open(
                        ffi::kff_fmt_pb_ptr(ptr),
                        c_path.as_ptr(),
                        ffi::AVIO_FLAG_WRITE as i32,
                    ),
                    "opening output file {}",
                    path
                )?;
            }
        }

        Ok(Self {
            ptr,
            header_written: false,
            custom_io: None,
            bsf_filters: Vec::new(),
            dts_correction: Vec::new(),
        })
    }

    /// Create an output context with a specific format name (e.g., "hls", "mp4", "mpegts").
    pub fn create_with_format(path: &str, format_name: &str) -> Result<Self> {
        crate::init();
        let c_path = CString::new(path).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "path contains null byte")
        })?;
        let c_format = CString::new(format_name).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "format name contains null byte")
        })?;

        let mut ptr: *mut ffi::AVFormatContext = ptr::null_mut();
        unsafe {
            ff_check!(
                ffi::avformat_alloc_output_context2(
                    &mut ptr,
                    ptr::null(),
                    c_format.as_ptr(),
                    c_path.as_ptr(),
                ),
                "creating {} output context for {}",
                format_name,
                path
            )?;
        }

        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "avformat_alloc_output_context2 returned null",
            ));
        }

        let flags = unsafe { ffi::kff_fmt_oformat(ptr) };
        let needs_file =
            unsafe { !flags.is_null() && (*flags).flags & ffi::AVFMT_NOFILE as i32 == 0 };

        if needs_file {
            unsafe {
                ff_check!(
                    ffi::avio_open(
                        ffi::kff_fmt_pb_ptr(ptr),
                        c_path.as_ptr(),
                        ffi::AVIO_FLAG_WRITE as i32,
                    ),
                    "opening output file {}",
                    path
                )?;
            }
        }

        Ok(Self {
            ptr,
            header_written: false,
            custom_io: None,
            bsf_filters: Vec::new(),
            dts_correction: Vec::new(),
        })
    }

    /// Create an output context that writes muxed data to a channel sender.
    ///
    /// Instead of writing to a file, muxed output is sent as `Vec<u8>` chunks
    /// through the provided `mpsc::Sender`. This enables streaming remux pipelines.
    pub fn create_to_channel(
        format_name: &str,
        tx: std::sync::mpsc::Sender<Vec<u8>>,
    ) -> Result<Self> {
        crate::init();
        let c_format = CString::new(format_name).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "format name contains null byte")
        })?;

        let mut ptr: *mut ffi::AVFormatContext = ptr::null_mut();
        unsafe {
            ff_check!(
                ffi::avformat_alloc_output_context2(
                    &mut ptr,
                    ptr::null(),
                    c_format.as_ptr(),
                    ptr::null(),
                ),
                "creating {} output context for channel",
                format_name
            )?;
        }

        if ptr.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "avformat_alloc_output_context2 returned null",
            ));
        }

        // Allocate I/O buffer
        const AVIO_BUFFER_SIZE: usize = 32768;
        let buffer = unsafe { ffi::av_malloc(AVIO_BUFFER_SIZE) as *mut u8 };
        if buffer.is_null() {
            unsafe { ffi::avformat_free_context(ptr) };
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating AVIO buffer for channel output",
            ));
        }

        // Double-box the sender: Box<Box<dyn Any + Send>> gives us a stable thin pointer
        let opaque: Box<dyn std::any::Any + Send> = Box::new(tx);
        let opaque = Box::new(opaque);
        // SAFETY: opaque.as_ref() returns a reference into the heap-allocated outer Box,
        // which is stored in custom_io and lives for the lifetime of OutputContext.
        let opaque_ptr =
            opaque.as_ref() as *const Box<dyn std::any::Any + Send> as *mut std::ffi::c_void;

        let avio_ctx = unsafe {
            ffi::avio_alloc_context(
                buffer,
                AVIO_BUFFER_SIZE as i32,
                1, // write mode
                opaque_ptr,
                None,                         // no read
                Some(channel_write_callback), // write
                None,                         // no seek
            )
        };

        if avio_ctx.is_null() {
            unsafe {
                ffi::av_free(buffer as *mut std::ffi::c_void);
                ffi::avformat_free_context(ptr);
            }
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating AVIOContext for channel output",
            ));
        }

        // Attach the custom I/O context to the format context
        unsafe { ffi::kff_fmt_set_pb(ptr, avio_ctx) };

        Ok(Self {
            ptr,
            header_written: false,
            custom_io: Some((avio_ctx, opaque)),
            bsf_filters: Vec::new(),
            dts_correction: Vec::new(),
        })
    }

    /// Add a new stream to the output, copying parameters from source codec params.
    ///
    /// # Safety
    /// `codec_params` must point to valid `AVCodecParameters`.
    pub unsafe fn add_stream_from_params(
        &mut self,
        codec_params: *const ffi::AVCodecParameters,
    ) -> Result<i32> {
        unsafe {
            let stream = ffi::avformat_new_stream(self.ptr, ptr::null());
            if stream.is_null() {
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "adding output stream",
                ));
            }

            ff_check!(
                ffi::avcodec_parameters_copy(ffi::kff_stream_codecpar(stream), codec_params),
                "copying codec parameters to output stream"
            )?;

            // Reset codec tag — let the muxer choose
            (*ffi::kff_stream_codecpar(stream)).codec_tag = 0;

            let idx = ffi::kff_stream_index(stream);
            // Pad bsf_filters for index alignment (no BSF for raw params)
            while self.bsf_filters.len() <= idx as usize {
                self.bsf_filters.push(None);
            }

            Ok(idx)
        }
    }

    /// Add a stream by copying parameters from an input stream (safe wrapper).
    /// Used for remuxing — copies codec configuration and time_base without re-encoding.
    ///
    /// Automatically attaches the `aac_adtstoasc` bitstream filter when copying
    /// AAC audio into MP4-family containers (MP4, MOV, HLS with fMP4 segments).
    pub fn add_stream_copy(&mut self, stream: &crate::format::input::Stream<'_>) -> Result<i32> {
        let idx = unsafe { self.add_stream_from_params(stream.codec_parameters())? };
        // Copy time_base so packet timestamps are interpreted correctly by the muxer
        unsafe {
            let streams = ffi::kff_fmt_streams(self.ptr);
            let out_stream = *streams.add(idx as usize);
            ffi::kff_stream_set_time_base(out_stream, stream.time_base());
        }

        // Auto-attach aac_adtstoasc BSF for AAC streams going into MP4-family formats.
        // MPEG-TS delivers AAC with ADTS framing; MP4 expects raw AAC with
        // AudioSpecificConfig in extradata. The BSF strips ADTS headers.
        let codec_id = unsafe { (*stream.codec_parameters()).codec_id };
        let bsf = if codec_id == ffi::AV_CODEC_ID_AAC && self.needs_aac_adtstoasc() {
            Some(unsafe {
                BitstreamFilter::new(
                    "aac_adtstoasc",
                    stream.codec_parameters(),
                    stream.time_base(),
                )
            }?)
        } else {
            None
        };

        // If a BSF was attached, update the output stream's codec parameters
        // to match the BSF's output. For aac_adtstoasc, this replaces ADTS
        // extradata with raw AudioSpecificConfig so the muxer writes correct headers.
        if let Some(ref bsf) = bsf {
            unsafe {
                let streams = ffi::kff_fmt_streams(self.ptr);
                let out_stream = *streams.add(idx as usize);
                ff_check!(
                    ffi::avcodec_parameters_copy(
                        ffi::kff_stream_codecpar(out_stream),
                        bsf.output_codec_parameters(),
                    ),
                    "copying BSF output params to stream"
                )?;
            }
        }

        // Ensure bsf_filters vec is large enough for the stream index
        while self.bsf_filters.len() <= idx as usize {
            self.bsf_filters.push(None);
        }
        self.bsf_filters[idx as usize] = bsf;

        Ok(idx)
    }

    /// Whether this output format requires the aac_adtstoasc bitstream filter.
    fn needs_aac_adtstoasc(&self) -> bool {
        let oformat = unsafe { ffi::kff_fmt_oformat(self.ptr) };
        if oformat.is_null() {
            return false;
        }
        let name_ptr = unsafe { (*oformat).name };
        if name_ptr.is_null() {
            return false;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) };
        let name = name.to_str().unwrap_or("");
        // MP4, MOV, and HLS (which uses fMP4 segments) all need raw AAC
        matches!(name, "mp4" | "mov" | "hls" | "ipod")
    }

    /// Add a new stream from an encoder context.
    pub fn add_stream_from_encoder(
        &mut self,
        encoder: &crate::codec::encoder::Encoder,
    ) -> Result<i32> {
        let stream = unsafe { ffi::avformat_new_stream(self.ptr, ptr::null()) };
        if stream.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "adding output stream",
            ));
        }

        unsafe {
            ff_check!(
                ffi::avcodec_parameters_from_context(
                    ffi::kff_stream_codecpar(stream),
                    encoder.as_ptr(),
                ),
                "copying encoder parameters to output stream"
            )?;
            (*ffi::kff_stream_codecpar(stream)).codec_tag = 0;
            // Set stream time_base from encoder
            ffi::kff_stream_set_time_base(stream, (*encoder.as_ptr()).time_base);
            // `pasp` comes off the stream, not codecpar, and init_muxer refuses to
            // write a header when the two disagree. Copying it here is what makes
            // a 720x576 DVB frame reach the browser as 16:9 rather than 5:4.
            let sar = (*encoder.as_ptr()).sample_aspect_ratio;
            if sar.num > 0 && sar.den > 0 {
                ffi::kff_stream_set_sample_aspect_ratio(stream, sar);
            }
        }

        let idx = unsafe { ffi::kff_stream_index(stream) };
        // Pad bsf_filters for index alignment (no BSF for encoded streams)
        while self.bsf_filters.len() <= idx as usize {
            self.bsf_filters.push(None);
        }

        Ok(idx)
    }

    /// Write the file header. Must be called after adding all streams.
    pub fn write_header(&mut self) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::avformat_write_header(self.ptr, ptr::null_mut()),
                "writing output header"
            )?;
        }
        self.header_written = true;
        Ok(())
    }

    /// Write a packet to the output (interleaved).
    ///
    /// The packet must have `stream_index` set to the output stream index
    /// and timestamps in the output stream's time_base. For copy-mode remuxing,
    /// use `write_packet_rescaled` instead.
    ///
    /// If a bitstream filter is attached to this stream (e.g., `aac_adtstoasc`),
    /// the packet is filtered before writing.
    pub fn write_packet_interleaved(&mut self, packet: &mut Packet) -> Result<()> {
        let idx = packet.stream_index() as usize;
        if let Some(Some(bsf)) = self.bsf_filters.get_mut(idx) {
            bsf.filter_packet(packet)?;
        }
        unsafe {
            ff_check!(
                ffi::av_interleaved_write_frame(self.ptr, packet.as_mut_ptr()),
                "writing interleaved packet"
            )?;
        }
        Ok(())
    }

    /// Write a copy-mode packet, rescaling timestamps from input to output time_base.
    ///
    /// Use this for remuxing where packets are read from an input stream and written
    /// directly to an output stream without re-encoding.
    ///
    /// Packets with missing PTS get PTS set from DTS. Packets with neither
    /// PTS nor DTS are silently skipped (returns Ok).
    ///
    /// If a bitstream filter is attached to this stream (e.g., `aac_adtstoasc`),
    /// the packet is filtered before writing.
    pub fn write_packet_rescaled(
        &mut self,
        packet: &mut Packet,
        out_stream_idx: i32,
        in_time_base: ffi::AVRational,
    ) -> Result<()> {
        unsafe {
            let pkt = packet.as_mut_ptr();

            // Fix missing timestamps — MKV + backward seek commonly yields packets with
            // DTS=NOPTS (pts set, dts absent) on the first frames out of the reorder buffer.
            // Backfill the missing side from the other so the muxer never has to synthesize
            // timestamps (its synthesis conflicts with subsequent real DTS values, causing
            // non-monotonic errors on newer FFmpeg). Skip if both sides are missing.
            if (*pkt).pts == crate::ffi::AV_NOPTS_VALUE && (*pkt).dts == crate::ffi::AV_NOPTS_VALUE
            {
                return Ok(());
            }
            if (*pkt).pts == crate::ffi::AV_NOPTS_VALUE {
                (*pkt).pts = (*pkt).dts;
            }
            if (*pkt).dts == crate::ffi::AV_NOPTS_VALUE {
                (*pkt).dts = (*pkt).pts;
            }

            (*pkt).stream_index = out_stream_idx;
        }

        if let Some(Some(bsf)) = self.bsf_filters.get_mut(out_stream_idx as usize) {
            bsf.filter_packet(packet)?;
        }

        unsafe {
            let pkt = packet.as_mut_ptr();
            let out_streams = ffi::kff_fmt_streams(self.ptr);
            let out_tb = ffi::kff_stream_time_base(*out_streams.add(out_stream_idx as usize));
            ffi::av_packet_rescale_ts(pkt, in_time_base, out_tb);

            // Always correct DTS monotonicity. Demuxer-side backward jumps happen on
            // IPTV reconnects/feed switches and on MKV + B-frames after a backward seek
            // (reorder-buffer refill). Newer FFmpeg's check_packet rejects non-monotonic
            // DTS with EINVAL, so this runs unconditionally. No-op for well-formed
            // streams: offset stays 0 and every packet passes through unchanged.
            let idx = out_stream_idx as usize;
            while self.dts_correction.len() <= idx {
                self.dts_correction.push(DtsTracker::default());
            }
            let tracker = &mut self.dts_correction[idx];
            let dts = (*pkt).dts;

            if dts != crate::ffi::AV_NOPTS_VALUE {
                let corrected_dts = dts + tracker.offset;
                if tracker.initialized && corrected_dts <= tracker.last_dts {
                    // Backward jump — bump offset so next DTS is last_dts + 1
                    let gap = tracker.last_dts - corrected_dts + 1;
                    tracker.offset += gap;
                }
                let final_dts = dts + tracker.offset;
                let shift = tracker.offset;
                (*pkt).dts = final_dts;
                if (*pkt).pts != crate::ffi::AV_NOPTS_VALUE {
                    (*pkt).pts += shift;
                }
                tracker.last_dts = final_dts;
                tracker.initialized = true;
            }

            ff_check!(
                ffi::av_interleaved_write_frame(self.ptr, pkt),
                "writing rescaled packet"
            )?;
        }
        Ok(())
    }

    /// Write a packet to the output (non-interleaved).
    pub fn write_packet(&mut self, packet: &mut Packet) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_write_frame(self.ptr, packet.as_mut_ptr()),
                "writing packet"
            )?;
        }
        Ok(())
    }

    /// Write the file trailer. Must be called before closing.
    pub fn write_trailer(&mut self) -> Result<()> {
        if self.header_written {
            unsafe {
                ff_check!(ffi::av_write_trailer(self.ptr), "writing output trailer")?;
            }
            self.header_written = false;
        }
        Ok(())
    }

    /// Number of streams in the output.
    pub fn nb_streams(&self) -> u32 {
        unsafe { ffi::kff_fmt_nb_streams(self.ptr) }
    }

    /// Set a muxer option via av_opt_set.
    pub fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        let c_key = CString::new(key).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "option key contains null byte")
        })?;
        let c_value = CString::new(value).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "option value contains null byte")
        })?;
        unsafe {
            ff_check!(
                ffi::av_opt_set(
                    self.ptr as *mut std::ffi::c_void,
                    c_key.as_ptr(),
                    c_value.as_ptr(),
                    ffi::AV_OPT_SEARCH_CHILDREN as i32,
                ),
                "setting option {}={}",
                key,
                value
            )?;
        }
        Ok(())
    }

    /// Whether this output format requires global headers (e.g., MKV, MP4).
    /// If true, encoders must set `AV_CODEC_FLAG_GLOBAL_HEADER` before opening.
    pub fn requires_global_header(&self) -> bool {
        let oformat = unsafe { ffi::kff_fmt_oformat(self.ptr) };
        if oformat.is_null() {
            return false;
        }
        unsafe { (*oformat).flags & ffi::AVFMT_GLOBALHEADER as i32 != 0 }
    }

    pub fn as_ptr(&self) -> *const ffi::AVFormatContext {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut ffi::AVFormatContext {
        self.ptr
    }
}

impl Drop for OutputContext {
    fn drop(&mut self) {
        if self.header_written {
            unsafe { ffi::av_write_trailer(self.ptr) };
        }

        if let Some((mut avio_ctx, _opaque)) = self.custom_io.take() {
            // Detach the custom I/O before freeing the format context
            unsafe {
                ffi::kff_fmt_set_pb(self.ptr, ptr::null_mut());
                ffi::avio_context_free(&mut avio_ctx);
            }
            // _opaque (the boxed sender) is dropped here
        } else {
            // Close the I/O context if we opened a file
            let oformat = unsafe { ffi::kff_fmt_oformat(self.ptr) };
            let needs_close = unsafe {
                !oformat.is_null()
                    && (*oformat).flags & ffi::AVFMT_NOFILE as i32 == 0
                    && !ffi::kff_fmt_pb(self.ptr).is_null()
            };

            if needs_close {
                unsafe {
                    ffi::avio_closep(ffi::kff_fmt_pb_ptr(self.ptr));
                }
            }
        }

        unsafe { ffi::avformat_free_context(self.ptr) };
    }
}

/// C callback for writing muxed data to the channel sender.
unsafe extern "C" fn channel_write_callback(
    opaque: *mut std::ffi::c_void,
    buf: *const u8,
    buf_size: std::ffi::c_int,
) -> std::ffi::c_int {
    if buf.is_null() || buf_size <= 0 {
        return ffi::AVERROR_EINVAL;
    }
    let sender_box = unsafe { &*(opaque as *const Box<dyn std::any::Any + Send>) };
    let Some(tx) = sender_box.downcast_ref::<std::sync::mpsc::Sender<Vec<u8>>>() else {
        return ffi::AVERROR_EINVAL;
    };
    let data = unsafe { std::slice::from_raw_parts(buf, buf_size as usize) };
    match tx.send(data.to_vec()) {
        Ok(()) => buf_size,
        Err(_) => ffi::AVERROR_EOF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::input::InputContext;

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
    fn test_output_remux_roundtrip() {
        let src = generate_test_file();
        let dst = tempfile::Builder::new()
            .suffix(".mp4")
            .tempfile()
            .expect("create temp file");

        // Open input
        let mut in_ctx = InputContext::open_path(src.path()).unwrap();
        in_ctx.find_stream_info().unwrap();

        // Create output
        let mut out_ctx = OutputContext::create(dst.path().to_str().unwrap()).unwrap();

        // Copy all streams
        let nb = in_ctx.nb_streams();
        for i in 0..nb {
            let stream = in_ctx.stream(i).unwrap();
            out_ctx.add_stream_copy(&stream).unwrap();
        }
        out_ctx.write_header().unwrap();

        // Copy all packets
        let mut pkt = crate::packet::Packet::new().unwrap();
        let mut count = 0u64;
        while in_ctx.read_packet(&mut pkt).unwrap() {
            out_ctx.write_packet_interleaved(&mut pkt).unwrap();
            count += 1;
        }
        assert!(count > 0, "should have written packets");

        out_ctx.write_trailer().unwrap();
        drop(out_ctx);

        // Verify output is readable
        let mut verify = InputContext::open_path(dst.path()).unwrap();
        verify.find_stream_info().unwrap();
        assert_eq!(verify.nb_streams(), nb);
    }
}
