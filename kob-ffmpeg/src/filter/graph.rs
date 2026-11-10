//! Filter graph wrapper (AVFilterGraph).
//!
//! Provides a builder for constructing FFmpeg filter graphs and an executor
//! for pushing frames through them.

use std::ffi::CString;
use std::ptr;

use crate::ffi;
use crate::frame::Frame;
use crate::{Result, ff_check};

/// Owns an `AVFilterGraph` with buffersrc input and buffersink output.
///
/// `Send` but not `Sync`.
pub struct FilterGraph {
    graph: *mut ffi::AVFilterGraph,
    src_ctx: *mut ffi::AVFilterContext,
    sink_ctx: *mut ffi::AVFilterContext,
}

unsafe impl Send for FilterGraph {}

/// Side data types that CLI ffmpeg propagates to filter buffer sources.
/// These mirror `AV_SIDE_DATA_PROP_GLOBAL` types that affect downstream filter
/// or encoder configuration. DISPLAYMATRIX is intentionally excluded — CLI
/// stores it separately and applies it via an auto-rotate filter, not via the
/// buffer source. DOWNMIX_INFO is added because aresample needs it at config
/// time even though it isn't formally tagged GLOBAL.
const PROPAGATED_SIDE_DATA_TYPES: &[ffi::AVFrameSideDataType] = &[
    ffi::AV_FRAME_DATA_DOWNMIX_INFO,
    ffi::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
    ffi::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
    ffi::AV_FRAME_DATA_AMBIENT_VIEWING_ENVIRONMENT,
    ffi::AV_FRAME_DATA_SPHERICAL,
    ffi::AV_FRAME_DATA_ICC_PROFILE,
    ffi::AV_FRAME_DATA_STEREO3D,
    ffi::AV_FRAME_DATA_AUDIO_SERVICE_TYPE,
];

/// Walk `frame->side_data` and copy pointers for each global-prop side data
/// type into `out`, returning how many were collected. Returns 0 (and writes
/// nothing) when `frame` is null or has no matching side data — callers can
/// then skip setting `nb_side_data` so `av_buffersrc_parameters_set` no-ops
/// the side-data clone path.
unsafe fn collect_global_side_data(
    frame: *const ffi::AVFrame,
    out: &mut [*mut ffi::AVFrameSideData],
) -> usize {
    if frame.is_null() {
        return 0;
    }
    let mut n = 0;
    for &ty in PROPAGATED_SIDE_DATA_TYPES {
        if n >= out.len() {
            break;
        }
        let sd = unsafe { ffi::av_frame_get_side_data(frame, ty) };
        if !sd.is_null() {
            out[n] = sd;
            n += 1;
        }
    }
    n
}

/// Color metadata for video streams. Default is "unspecified" for all fields,
/// which matches a freshly-zeroed AVCodecContext / AVCodecParameters.
#[derive(Debug, Clone, Copy)]
pub struct VideoColorInfo {
    pub color_space: ffi::AVColorSpace,
    pub color_range: ffi::AVColorRange,
    pub color_primaries: ffi::AVColorPrimaries,
    pub color_trc: ffi::AVColorTransferCharacteristic,
    pub chroma_location: ffi::AVChromaLocation,
}

impl Default for VideoColorInfo {
    fn default() -> Self {
        Self {
            color_space: ffi::AVCOL_SPC_UNSPECIFIED,
            color_range: ffi::AVCOL_RANGE_UNSPECIFIED,
            color_primaries: ffi::AVCOL_PRI_UNSPECIFIED,
            color_trc: ffi::AVCOL_TRC_UNSPECIFIED,
            chroma_location: ffi::AVCHROMA_LOC_UNSPECIFIED,
        }
    }
}

impl VideoColorInfo {
    /// Read color metadata from an `AVCodecParameters` (typically from an input stream).
    /// Returns `Default` (all-unspecified) when `params` is null, so callers can
    /// pass a stream's codecpar directly without a separate null check.
    /// # Safety
    /// If `params` is non-null, it must point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters(params: *const ffi::AVCodecParameters) -> Self {
        if params.is_null() {
            return Self::default();
        }
        unsafe {
            Self {
                color_space: (*params).color_space,
                color_range: (*params).color_range,
                color_primaries: (*params).color_primaries,
                color_trc: (*params).color_trc,
                chroma_location: (*params).chroma_location,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Deinterlace {
    #[default]
    None,
    /// Apply only when the source stream is field-coded.
    Auto,
    Force,
}

impl Deinterlace {
    pub fn filter_for(self, source_interlaced: bool) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Auto if !source_interlaced => None,
            // send_field emits one frame per field, so 1080i25 becomes 1080p50 —
            // any gop_size derived from the source frame rate must be doubled.
            _ => Some("bwdif=mode=send_field"),
        }
    }
}

/// Auto-rotation derived from an `AV_PKT_DATA_DISPLAYMATRIX` side data on a
/// stream — describes the filter chain that needs to be inserted *before* the
/// encoder so output pixels are in the intended landscape/portrait orientation.
///
/// Mirrors what CLI ffmpeg does in `fftools/ffmpeg_filter.c::configure_input_video_filter`
/// when the `autorotate` flag is on (default).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AutoRotate {
    /// 90° clockwise. Use `transpose=clock` (or `cclock_flip` if matrix is mirrored).
    Clock,
    ClockFlip,
    /// 90° counter-clockwise.
    Cclock,
    CclockFlip,
    /// 180° rotation, with optional axis flips.
    Hflip,
    Vflip,
    HVflip,
    /// Arbitrary angle in degrees (used as `rotate=<theta>*PI/180`).
    Arbitrary(f64),
}

impl AutoRotate {
    /// Filter chain fragment to prepend to the existing filter description.
    /// Returned string does NOT include a trailing comma — caller composes.
    pub fn as_filter_chain(self) -> String {
        match self {
            Self::Clock => "transpose=clock".into(),
            Self::ClockFlip => "transpose=clock_flip".into(),
            Self::Cclock => "transpose=cclock".into(),
            Self::CclockFlip => "transpose=cclock_flip".into(),
            Self::Hflip => "hflip".into(),
            Self::Vflip => "vflip".into(),
            Self::HVflip => "hflip,vflip".into(),
            Self::Arbitrary(theta) => format!("rotate={theta}*PI/180"),
        }
    }

    /// Detect rotation from a stream's coded side data. Returns `None` when
    /// there is no `AV_PKT_DATA_DISPLAYMATRIX` (the common case) or when the
    /// matrix encodes the identity (no rotation, no flip). Mirrors CLI's
    /// `cmdutils.c::get_rotation` + `configure_input_video_filter` angle
    /// classification: theta is normalised to `[0, 360)` clockwise-positive,
    /// then matched against the four right-angle cases.
    ///
    /// # Safety
    /// `params` must be a valid (or null) `AVCodecParameters` pointer.
    pub unsafe fn from_codec_parameters(params: *const ffi::AVCodecParameters) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        unsafe {
            let sd = ffi::av_packet_side_data_get(
                (*params).coded_side_data,
                (*params).nb_coded_side_data,
                ffi::AV_PKT_DATA_DISPLAYMATRIX,
            );
            if sd.is_null() || (*sd).size < 9 * std::mem::size_of::<i32>() {
                return None;
            }
            let m = (*sd).data as *const i32;
            let theta_raw = ffi::av_display_rotation_get(m);
            if !theta_raw.is_finite() {
                return None;
            }
            // Match CLI: negate (CCW → CW), round, then normalise to [0, 360).
            let mut theta = -theta_raw.round();
            theta -= 360.0 * (theta / 360.0 + 0.9 / 360.0).floor();

            // m[3] is the sign-relevant entry CLI inspects when picking
            // between transpose variants on 90°/270°.
            let m0 = *m.add(0);
            let m3 = *m.add(3);
            let m4 = *m.add(4);

            if (theta - 90.0).abs() < 1.0 {
                Some(if m3 > 0 {
                    Self::CclockFlip
                } else {
                    Self::Clock
                })
            } else if (theta - 180.0).abs() < 1.0 {
                match (m0 < 0, m4 < 0) {
                    (true, true) => Some(Self::HVflip),
                    (true, false) => Some(Self::Hflip),
                    (false, true) => Some(Self::Vflip),
                    (false, false) => None, // 180° but no flip: shouldn't happen
                }
            } else if (theta - 270.0).abs() < 1.0 {
                Some(if m3 < 0 {
                    Self::ClockFlip
                } else {
                    Self::Cclock
                })
            } else if theta.abs() > 1.0 && (theta - 360.0).abs() > 1.0 {
                Some(Self::Arbitrary(theta))
            } else if m4 < 0 {
                // theta ≈ 0 but vertical flip baked in.
                Some(Self::Vflip)
            } else {
                None
            }
        }
    }
}

/// Parameters for creating a video filter graph.
pub struct VideoFilterParams {
    pub width: i32,
    pub height: i32,
    pub pixel_format: ffi::AVPixelFormat,
    pub time_base: ffi::AVRational,
    pub sample_aspect_ratio: ffi::AVRational,
    /// Hardware frames context for GPU-resident filter chains (e.g., `scale_cuda`).
    /// When set, the buffersrc is configured with this hw_frames_ctx so GPU
    /// filters receive hardware frames directly.
    pub hw_frames_ctx: Option<*mut ffi::AVBufferRef>,
    /// Color metadata propagated from input to encoder. Default is "unspecified"
    /// — matches CLI behaviour when a stream lacks explicit tagging.
    pub color: VideoColorInfo,
    /// Optional source frame whose global side data (HDR mastering display,
    /// content light level, etc.) should be propagated to the buffer source
    /// so that downstream filters and encoders see the same metadata CLI ffmpeg
    /// would. Pass `ptr::null()` for non-video or when no source frame is
    /// available yet.
    pub side_data_frame: *const ffi::AVFrame,
}

/// Parameters for creating an audio filter graph.
pub struct AudioFilterParams {
    pub sample_rate: i32,
    pub sample_format: ffi::AVSampleFormat,
    pub channel_layout: ffi::AVChannelLayout,
    pub time_base: ffi::AVRational,
    /// Optional source frame whose `AV_FRAME_DATA_DOWNMIX_INFO` side data
    /// (set by AC3/E-AC3 decoders) should be propagated to the buffer source
    /// so the auto-inserted aresample uses the bitstream's mix levels rather
    /// than swresample defaults. Required for AC3/E-AC3 sources that downmix
    /// to stereo — without it, output is ~+1.5 dB hotter than `ffmpeg` CLI
    /// and can clip on loud content.
    pub side_data_frame: *const ffi::AVFrame,
}

impl FilterGraph {
    /// Create a video filter graph from a filter description string.
    ///
    /// The filter string uses FFmpeg's filter graph syntax, e.g.:
    /// - `"scale=640:480"` — scale video
    /// - `"blackdetect=d=0.08:pix_th=0.10"` — detect black frames
    /// - `"overlay"` — overlay subtitles
    pub fn new_video(params: &VideoFilterParams, filter_descr: &str) -> Result<Self> {
        Self::new_video_on_device(params, filter_descr, None)
    }

    /// [`new_video`](Self::new_video) with `hw_device_ctx` handed to every filter,
    /// as CLI `-filter_hw_device` does, so device-backed filters share one device
    /// instead of each opening its own.
    pub fn new_video_on_device(
        params: &VideoFilterParams,
        filter_descr: &str,
        hw_device_ctx: Option<*mut ffi::AVBufferRef>,
    ) -> Result<Self> {
        let graph = unsafe { ffi::avfilter_graph_alloc() };
        if graph.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating filter graph",
            ));
        }

        // Wrap early so Drop cleans up graph on early return via `?`
        let mut fg = FilterGraph {
            graph,
            src_ctx: ptr::null_mut(),
            sink_ctx: ptr::null_mut(),
        };

        let buffersrc = unsafe { ffi::avfilter_get_by_name(c"buffer".as_ptr()) };
        let buffersink = unsafe { ffi::avfilter_get_by_name(c"buffersink".as_ptr()) };

        if buffersrc.is_null() || buffersink.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_FILTER_NOT_FOUND,
                "buffer/buffersink filters not found",
            ));
        }

        unsafe {
            if let Some(hw_frames) = params.hw_frames_ctx {
                // HW path: use alloc_filter + set hw_frames_ctx + init_str.
                // avfilter_graph_create_filter does alloc+init in one step,
                // but we need to set hw_frames_ctx BETWEEN alloc and init
                // (FFmpeg rejects HW pix_fmt during init without it).
                fg.src_ctx = ffi::avfilter_graph_alloc_filter(fg.graph, buffersrc, c"in".as_ptr());
                if fg.src_ctx.is_null() {
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "allocating buffersrc filter",
                    ));
                }
                // Set hw_frames_ctx on the allocated (but not yet initialized) context
                (*fg.src_ctx).hw_device_ctx = std::ptr::null_mut();
                let par = ffi::av_buffersrc_parameters_alloc();
                if par.is_null() {
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "allocating buffersrc parameters",
                    ));
                }
                std::ptr::write_bytes(par, 0, 1);
                (*par).format = params.pixel_format; // AV_PIX_FMT_CUDA
                let hw_ref = ffi::av_buffer_ref(hw_frames);
                if hw_ref.is_null() {
                    ffi::av_free(par as *mut std::ffi::c_void);
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "av_buffer_ref failed for hw_frames_ctx",
                    ));
                }
                (*par).hw_frames_ctx = hw_ref;
                (*par).width = params.width;
                (*par).height = params.height;
                (*par).time_base = params.time_base;
                (*par).sample_aspect_ratio = params.sample_aspect_ratio;
                (*par).color_space = params.color.color_space;
                (*par).color_range = params.color.color_range;
                let mut sd_array: [*mut ffi::AVFrameSideData; 16] = [ptr::null_mut(); 16];
                let nb_sd = collect_global_side_data(params.side_data_frame, &mut sd_array);
                if nb_sd > 0 {
                    (*par).side_data = sd_array.as_mut_ptr();
                    (*par).nb_side_data = nb_sd as i32;
                }
                ff_check!(
                    ffi::av_buffersrc_parameters_set(fg.src_ctx, par),
                    "setting hw buffersrc parameters"
                )?;
                ffi::av_free(par as *mut std::ffi::c_void);
                // Now initialize with remaining args (video_size, time_base)
                let args = format!(
                    "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}",
                    params.width,
                    params.height,
                    params.pixel_format,
                    params.time_base.num,
                    params.time_base.den,
                    params.sample_aspect_ratio.num.max(1),
                    params.sample_aspect_ratio.den.max(1),
                );
                let c_args = CString::new(args).map_err(|_| {
                    crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "filter args null byte")
                })?;
                ff_check!(
                    ffi::avfilter_init_str(fg.src_ctx, c_args.as_ptr()),
                    "initializing buffersrc with hw_frames_ctx"
                )?;
            } else {
                // Software path: single-step create_filter
                let args = format!(
                    "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}",
                    params.width,
                    params.height,
                    params.pixel_format,
                    params.time_base.num,
                    params.time_base.den,
                    params.sample_aspect_ratio.num.max(1),
                    params.sample_aspect_ratio.den.max(1),
                );
                let c_args = CString::new(args).map_err(|_| {
                    crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "filter args null byte")
                })?;
                ff_check!(
                    ffi::avfilter_graph_create_filter(
                        &mut fg.src_ctx,
                        buffersrc,
                        c"in".as_ptr(),
                        c_args.as_ptr(),
                        ptr::null_mut(),
                        fg.graph,
                    ),
                    "creating buffersrc filter"
                )?;

                // Color metadata + global side data (HDR, etc.) need to flow
                // to the buffer source so the auto-conversion filters and the
                // encoder downstream see the same metadata CLI ffmpeg would.
                let par = ffi::av_buffersrc_parameters_alloc();
                if par.is_null() {
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "allocating buffersrc parameters",
                    ));
                }
                (*par).color_space = params.color.color_space;
                (*par).color_range = params.color.color_range;
                let mut sd_array: [*mut ffi::AVFrameSideData; 16] = [ptr::null_mut(); 16];
                let nb_sd = collect_global_side_data(params.side_data_frame, &mut sd_array);
                if nb_sd > 0 {
                    (*par).side_data = sd_array.as_mut_ptr();
                    (*par).nb_side_data = nb_sd as i32;
                }
                let ret = ffi::av_buffersrc_parameters_set(fg.src_ctx, par);
                ffi::av_free(par as *mut std::ffi::c_void);
                ff_check!(ret, "setting sw buffersrc color/side_data")?;
            }

            ff_check!(
                ffi::avfilter_graph_create_filter(
                    &mut fg.sink_ctx,
                    buffersink,
                    c"out".as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    fg.graph,
                ),
                "creating buffersink filter"
            )?;

            let c_descr = CString::new(filter_descr).map_err(|_| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    "filter description contains null byte",
                )
            })?;
            match hw_device_ctx {
                Some(device) => fg.link_chain_on_device(&c_descr, device, filter_descr)?,
                None => fg.link_chain(&c_descr, filter_descr)?,
            }

            ff_check!(
                ffi::avfilter_graph_config(fg.graph, ptr::null_mut()),
                "configuring filter graph"
            )?;
        }

        Ok(fg)
    }

    unsafe fn link_chain(&mut self, c_descr: &CString, filter_descr: &str) -> Result<()> {
        unsafe {
            let mut inputs = ffi::avfilter_inout_alloc();
            let mut outputs = ffi::avfilter_inout_alloc();
            if inputs.is_null() || outputs.is_null() {
                ffi::avfilter_inout_free(&mut inputs);
                ffi::avfilter_inout_free(&mut outputs);
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "allocating filter inout",
                ));
            }

            (*outputs).name = ffi::av_strdup(c"in".as_ptr());
            (*outputs).filter_ctx = self.src_ctx;
            (*outputs).pad_idx = 0;
            (*outputs).next = ptr::null_mut();

            (*inputs).name = ffi::av_strdup(c"out".as_ptr());
            (*inputs).filter_ctx = self.sink_ctx;
            (*inputs).pad_idx = 0;
            (*inputs).next = ptr::null_mut();

            let ret = ffi::avfilter_graph_parse_ptr(
                self.graph,
                c_descr.as_ptr(),
                &mut inputs,
                &mut outputs,
                ptr::null_mut(),
            );
            ffi::avfilter_inout_free(&mut inputs);
            ffi::avfilter_inout_free(&mut outputs);
            ff_check!(ret, "parsing filter graph: {}", filter_descr)?;
        }
        Ok(())
    }

    /// `avfilter_graph_parse_ptr` initialises each filter as it creates it, and
    /// libplacebo picks its Vulkan device during init — so the device has to be
    /// attached between creation and init, which only the segment API allows.
    unsafe fn link_chain_on_device(
        &mut self,
        c_descr: &CString,
        device: *mut ffi::AVBufferRef,
        filter_descr: &str,
    ) -> Result<()> {
        unsafe {
            let mut seg = ptr::null_mut();
            ff_check!(
                ffi::avfilter_graph_segment_parse(self.graph, c_descr.as_ptr(), 0, &mut seg),
                "parsing filter graph: {}",
                filter_descr
            )?;
            let mut inputs = ptr::null_mut();
            let mut outputs = ptr::null_mut();
            let linked = self.apply_segment(seg, device, &mut inputs, &mut outputs, filter_descr);
            ffi::avfilter_graph_segment_free(&mut seg);
            ffi::avfilter_inout_free(&mut inputs);
            ffi::avfilter_inout_free(&mut outputs);
            linked
        }
    }

    unsafe fn apply_segment(
        &mut self,
        seg: *mut ffi::AVFilterGraphSegment,
        device: *mut ffi::AVBufferRef,
        inputs: &mut *mut ffi::AVFilterInOut,
        outputs: &mut *mut ffi::AVFilterInOut,
        filter_descr: &str,
    ) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::avfilter_graph_segment_create_filters(seg, 0),
                "creating filters: {}",
                filter_descr
            )?;
            for c in 0..(*seg).nb_chains {
                let chain = *(*seg).chains.add(c);
                for f in 0..(*chain).nb_filters {
                    let filter = (*(*(*chain).filters.add(f))).filter;
                    if !filter.is_null() {
                        ffi::av_buffer_unref(&mut (*filter).hw_device_ctx);
                        (*filter).hw_device_ctx = ffi::av_buffer_ref(device);
                    }
                }
            }
            ff_check!(
                ffi::avfilter_graph_segment_apply(seg, 0, inputs, outputs),
                "applying filter graph: {}",
                filter_descr
            )?;
            let (input, output) = (*inputs, *outputs);
            if input.is_null()
                || output.is_null()
                || !(*input).next.is_null()
                || !(*output).next.is_null()
            {
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    format!("filter graph is not a single chain: {filter_descr}"),
                ));
            }
            ff_check!(
                ffi::avfilter_link(
                    self.src_ctx,
                    0,
                    (*input).filter_ctx,
                    (*input).pad_idx as u32
                ),
                "linking buffersrc"
            )?;
            ff_check!(
                ffi::avfilter_link(
                    (*output).filter_ctx,
                    (*output).pad_idx as u32,
                    self.sink_ctx,
                    0
                ),
                "linking buffersink"
            )?;
        }
        Ok(())
    }

    /// Create an audio filter graph from a filter description string.
    ///
    /// Example filter strings:
    /// - `"silencedetect=noise=-30dB:d=2"` — detect silence
    /// - `"ebur128=peak=none"` — LUFS measurement
    pub fn new_audio(params: &AudioFilterParams, filter_descr: &str) -> Result<Self> {
        let graph = unsafe { ffi::avfilter_graph_alloc() };
        if graph.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_ENOMEM,
                "allocating filter graph",
            ));
        }

        // Wrap early so Drop cleans up graph on early return via `?`
        let mut fg = FilterGraph {
            graph,
            src_ctx: ptr::null_mut(),
            sink_ctx: ptr::null_mut(),
        };

        let buffersrc = unsafe { ffi::avfilter_get_by_name(c"abuffer".as_ptr()) };
        let buffersink = unsafe { ffi::avfilter_get_by_name(c"abuffersink".as_ptr()) };

        if buffersrc.is_null() || buffersink.is_null() {
            return Err(crate::error::FfmpegError::new(
                ffi::AVERROR_FILTER_NOT_FOUND,
                "abuffer/abuffersink filters not found",
            ));
        }

        // Build channel layout string
        let mut ch_buf = [0u8; 128];
        unsafe {
            ffi::av_channel_layout_describe(
                &params.channel_layout,
                ch_buf.as_mut_ptr() as *mut std::os::raw::c_char,
                ch_buf.len(),
            );
        }
        let ch_str = std::ffi::CStr::from_bytes_until_nul(&ch_buf)
            .unwrap_or(c"stereo")
            .to_string_lossy();

        let args = format!(
            "sample_rate={}:sample_fmt={}:channel_layout={}:time_base={}/{}",
            params.sample_rate,
            params.sample_format,
            ch_str,
            params.time_base.num,
            params.time_base.den,
        );
        let c_args = CString::new(args).map_err(|_| {
            crate::error::FfmpegError::new(ffi::AVERROR_EINVAL, "filter args contain null byte")
        })?;

        unsafe {
            ff_check!(
                ffi::avfilter_graph_create_filter(
                    &mut fg.src_ctx,
                    buffersrc,
                    c"in".as_ptr(),
                    c_args.as_ptr(),
                    ptr::null_mut(),
                    fg.graph,
                ),
                "creating abuffersrc filter"
            )?;

            // Propagate global-prop frame side data (most importantly
            // AC3/E-AC3 downmix info) to the buffer source so the auto-inserted
            // aresample picks up the bitstream's mix levels at config time.
            // CLI ffmpeg does this in ffmpeg_filter.c::ifilter_parameters_from_frame.
            // Without it, 5.1 → stereo downmix uses swr defaults (clev=slev=0.707)
            // instead of e.g. AC3 metadata (0.594/0.500), producing output
            // ~+1.5 dB hotter that clips on loud peaks.
            let mut sd_array: [*mut ffi::AVFrameSideData; 16] = [ptr::null_mut(); 16];
            let nb_sd = collect_global_side_data(params.side_data_frame, &mut sd_array);
            if nb_sd > 0 {
                let par = ffi::av_buffersrc_parameters_alloc();
                if par.is_null() {
                    return Err(crate::error::FfmpegError::new(
                        ffi::AVERROR_ENOMEM,
                        "allocating buffersrc parameters",
                    ));
                }
                (*par).side_data = sd_array.as_mut_ptr();
                (*par).nb_side_data = nb_sd as i32;
                let ret = ffi::av_buffersrc_parameters_set(fg.src_ctx, par);
                ffi::av_free(par as *mut std::ffi::c_void);
                ff_check!(ret, "setting abuffersrc side data")?;
            }

            ff_check!(
                ffi::avfilter_graph_create_filter(
                    &mut fg.sink_ctx,
                    buffersink,
                    c"out".as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    fg.graph,
                ),
                "creating abuffersink filter"
            )?;

            let c_descr = CString::new(filter_descr).map_err(|_| {
                crate::error::FfmpegError::new(
                    ffi::AVERROR_EINVAL,
                    "filter description contains null byte",
                )
            })?;
            let mut inputs = ffi::avfilter_inout_alloc();
            let mut outputs = ffi::avfilter_inout_alloc();

            if inputs.is_null() || outputs.is_null() {
                if !inputs.is_null() {
                    ffi::avfilter_inout_free(&mut inputs);
                }
                if !outputs.is_null() {
                    ffi::avfilter_inout_free(&mut outputs);
                }
                return Err(crate::error::FfmpegError::new(
                    ffi::AVERROR_ENOMEM,
                    "allocating filter inout",
                ));
            }

            (*outputs).name = ffi::av_strdup(c"in".as_ptr());
            (*outputs).filter_ctx = fg.src_ctx;
            (*outputs).pad_idx = 0;
            (*outputs).next = ptr::null_mut();

            (*inputs).name = ffi::av_strdup(c"out".as_ptr());
            (*inputs).filter_ctx = fg.sink_ctx;
            (*inputs).pad_idx = 0;
            (*inputs).next = ptr::null_mut();

            ff_check!(
                ffi::avfilter_graph_parse_ptr(
                    fg.graph,
                    c_descr.as_ptr(),
                    &mut inputs,
                    &mut outputs,
                    ptr::null_mut(),
                ),
                "parsing audio filter graph: {}",
                filter_descr
            )?;

            ff_check!(
                ffi::avfilter_graph_config(fg.graph, ptr::null_mut()),
                "configuring audio filter graph"
            )?;

            ffi::avfilter_inout_free(&mut inputs);
            ffi::avfilter_inout_free(&mut outputs);
        }

        Ok(fg)
    }

    /// Push a frame into the filter graph.
    /// Takes `&mut Frame` because `av_buffersrc_add_frame` may ref the frame internally.
    pub fn push_frame(&mut self, frame: &mut Frame) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_buffersrc_add_frame(self.src_ctx, frame.as_mut_ptr()),
                "pushing frame to filter graph"
            )?;
        }
        Ok(())
    }

    /// Push a frame into the filter graph while keeping the caller's ref intact.
    ///
    /// `av_buffersrc_add_frame` (used by [`push_frame`](Self::push_frame)) takes
    /// ownership of the frame's reference and resets the input frame, so any
    /// subsequent reader sees empty data. When the same decoded frame must be
    /// fanned out to multiple sinks (multi-branch single-pass extraction),
    /// use this method instead — it sets `AV_BUFFERSRC_FLAG_KEEP_REF` so the
    /// graph creates its own ref and the input frame remains usable.
    pub fn push_frame_keep_ref(&mut self, frame: &mut Frame) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_buffersrc_add_frame_flags(
                    self.src_ctx,
                    frame.as_mut_ptr(),
                    ffi::AV_BUFFERSRC_FLAG_KEEP_REF as i32,
                ),
                "pushing frame to filter graph (keep_ref)"
            )?;
        }
        Ok(())
    }

    /// Signal end of input to the filter graph.
    pub fn push_eof(&mut self) -> Result<()> {
        unsafe {
            ff_check!(
                ffi::av_buffersrc_add_frame(self.src_ctx, ptr::null_mut()),
                "pushing EOF to filter graph"
            )?;
        }
        Ok(())
    }

    /// Get the output timebase of the filter graph's sink.
    /// Filtered frames have PTS in this timebase.
    pub fn output_time_base(&self) -> ffi::AVRational {
        unsafe { ffi::av_buffersink_get_time_base(self.sink_ctx) }
    }

    /// Pull a filtered frame from the output.
    /// Returns `Ok(true)` if a frame was retrieved, `Ok(false)` if EAGAIN or EOF.
    pub fn pull_frame(&mut self, frame: &mut Frame) -> Result<bool> {
        frame.unref();
        let ret = unsafe { ffi::av_buffersink_get_frame(self.sink_ctx, frame.as_mut_ptr()) };
        if ret == ffi::AVERROR_EAGAIN || ret == ffi::AVERROR_EOF {
            return Ok(false);
        }
        ff_check!(ret, "pulling frame from filter graph")?;
        Ok(true)
    }
}

impl Drop for FilterGraph {
    fn drop(&mut self) {
        unsafe { ffi::avfilter_graph_free(&mut self.graph) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an AVCodecParameters whose `coded_side_data` carries a
    /// `DISPLAYMATRIX` for `theta_degrees` (clockwise positive). The matrix is
    /// generated by `av_display_rotation_set`, then split into header + payload
    /// the way libavcodec stores side data on params.
    ///
    /// Returns the params pointer plus the heap-allocations that back it; the
    /// caller drops the box to free everything in the right order.
    #[allow(clippy::type_complexity)]
    fn make_params_with_rotation(
        theta_degrees: f64,
        flip_h: bool,
    ) -> (
        *mut ffi::AVCodecParameters,
        Box<(Vec<i32>, Vec<ffi::AVPacketSideData>)>,
    ) {
        unsafe {
            let par = ffi::avcodec_parameters_alloc();
            assert!(!par.is_null());

            let mut matrix: Vec<i32> = vec![0; 9];
            ffi::av_display_rotation_set(matrix.as_mut_ptr(), theta_degrees);
            if flip_h {
                ffi::av_display_matrix_flip(matrix.as_mut_ptr(), 1, 0);
            }

            let sd = vec![ffi::AVPacketSideData {
                data: matrix.as_mut_ptr() as *mut u8,
                size: matrix.len() * std::mem::size_of::<i32>(),
                type_: ffi::AV_PKT_DATA_DISPLAYMATRIX,
            }];

            let mut backing = Box::new((matrix, sd));
            (*par).coded_side_data = backing.1.as_mut_ptr();
            (*par).nb_coded_side_data = backing.1.len() as i32;
            (par, backing)
        }
    }

    #[test]
    fn auto_rotate_handles_null_params() {
        let result = unsafe { AutoRotate::from_codec_parameters(std::ptr::null()) };
        assert_eq!(result, None);
    }

    #[test]
    fn auto_rotate_returns_none_when_no_displaymatrix() {
        unsafe {
            let par = ffi::avcodec_parameters_alloc();
            let result = AutoRotate::from_codec_parameters(par);
            ffi::avcodec_parameters_free(&mut (par as *mut _));
            assert_eq!(result, None);
        }
    }

    #[test]
    fn auto_rotate_clock_for_iphone_portrait_clip() {
        // An iPhone-portrait clip stores a matrix where `av_display_rotation_get`
        // returns -90 (raw); CLI's `get_rotation` then negates+normalises that
        // to +90, picks the 90°-CW branch, sees m[3] < 0, and inserts
        // `transpose=clock`. We construct the same matrix via
        // `av_display_rotation_set(m, 90)`.
        let (par, _backing) = make_params_with_rotation(90.0, false);
        let result = unsafe { AutoRotate::from_codec_parameters(par) };
        assert_eq!(result, Some(AutoRotate::Clock));
        unsafe {
            ffi::av_freep(&par as *const _ as *mut std::ffi::c_void);
        }
    }

    #[test]
    fn auto_rotate_180_picks_hflip_or_vflip() {
        let (par, _backing) = make_params_with_rotation(180.0, false);
        let result = unsafe { AutoRotate::from_codec_parameters(par) };
        assert!(matches!(
            result,
            Some(AutoRotate::Hflip | AutoRotate::Vflip | AutoRotate::HVflip)
        ));
        unsafe {
            ffi::av_freep(&par as *const _ as *mut std::ffi::c_void);
        }
    }

    #[test]
    fn auto_rotate_cclock_for_270_degree_clip() {
        // 270° CW display rotation: `av_display_rotation_set(m, -90)` produces
        // a matrix whose CLI-normalised theta is 270, with m[3] > 0 → Cclock.
        let (par, _backing) = make_params_with_rotation(-90.0, false);
        let result = unsafe { AutoRotate::from_codec_parameters(par) };
        assert_eq!(result, Some(AutoRotate::Cclock));
        unsafe {
            ffi::av_freep(&par as *const _ as *mut std::ffi::c_void);
        }
    }

    #[test]
    fn auto_rotate_skips_identity() {
        let (par, _backing) = make_params_with_rotation(0.0, false);
        let result = unsafe { AutoRotate::from_codec_parameters(par) };
        assert_eq!(result, None);
        unsafe {
            ffi::av_freep(&par as *const _ as *mut std::ffi::c_void);
        }
    }

    #[test]
    fn auto_rotate_filter_chain_strings_compile() {
        // Each variant must produce a non-empty filter expression.
        for variant in [
            AutoRotate::Clock,
            AutoRotate::ClockFlip,
            AutoRotate::Cclock,
            AutoRotate::CclockFlip,
            AutoRotate::Hflip,
            AutoRotate::Vflip,
            AutoRotate::HVflip,
            AutoRotate::Arbitrary(33.5),
        ] {
            assert!(!variant.as_filter_chain().is_empty());
        }
    }
}
