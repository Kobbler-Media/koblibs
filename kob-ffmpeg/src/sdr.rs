use std::sync::OnceLock;

use crate::filter::graph::{FilterGraph, VideoColorInfo, VideoFilterParams};
use crate::frame::Frame;
use crate::{Result, ffi};

/// libplacebo is the only FFmpeg filter that applies a Dolby Vision RPU; it reads
/// the per-frame `DOVI_METADATA` side data the decoder attaches, so the frames
/// must reach it with their side data intact.
pub fn sdr_filter(width: i32, height: i32, pix_fmt: &str) -> String {
    format!(
        "libplacebo=w={width}:h={height}:apply_dolbyvision=1:colorspace=bt709:\
         color_primaries=bt709:color_trc=bt709:range=tv:format={pix_fmt}"
    )
}

pub fn sdr_color() -> VideoColorInfo {
    VideoColorInfo {
        color_space: ffi::AVCOL_SPC_BT709,
        color_range: ffi::AVCOL_RANGE_MPEG,
        color_primaries: ffi::AVCOL_PRI_BT709,
        color_trc: ffi::AVCOL_TRC_BT709,
        chroma_location: ffi::AVCHROMA_LOC_LEFT,
    }
}

struct VulkanDevice(*mut ffi::AVBufferRef);

unsafe impl Send for VulkanDevice {}
unsafe impl Sync for VulkanDevice {}

fn probe_params() -> VideoFilterParams {
    VideoFilterParams {
        width: 64,
        height: 64,
        pixel_format: ffi::AV_PIX_FMT_YUV420P10LE,
        time_base: ffi::AVRational { num: 1, den: 90000 },
        sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
        hw_frames_ctx: None,
        color: VideoColorInfo::default(),
        side_data_frame: std::ptr::null(),
    }
}

/// One Vulkan device for every tone-mapping graph in the process. Left to
/// itself libplacebo opens a fresh instance and device per graph: seconds of
/// setup each, and on the NVIDIA driver `vkCreateDevice` fails outright when
/// several graphs open at once. `None` when libplacebo cannot import an FFmpeg
/// device, which some distro builds of the pair refuse.
fn shared_vulkan_device() -> Option<*mut ffi::AVBufferRef> {
    static DEVICE: OnceLock<Option<VulkanDevice>> = OnceLock::new();
    DEVICE
        .get_or_init(|| {
            crate::init();
            let mut device = std::ptr::null_mut();
            let ret = unsafe {
                ffi::av_hwdevice_ctx_create(
                    &mut device,
                    ffi::AV_HWDEVICE_TYPE_VULKAN,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if ret < 0 {
                tracing::debug!(
                    error = %crate::FfmpegError::new(ret, "creating Vulkan device"),
                    "no shared Vulkan device for libplacebo"
                );
                return None;
            }
            let probe = FilterGraph::new_video_on_device(
                &probe_params(),
                &sdr_filter(64, 64, "yuv420p"),
                Some(device),
            );
            if let Err(e) = probe {
                tracing::debug!(error = %e, "libplacebo cannot import the shared Vulkan device");
                unsafe { ffi::av_buffer_unref(&mut device) };
                return None;
            }
            Some(VulkanDevice(device))
        })
        .as_ref()
        .map(|d| d.0)
}

pub(crate) fn tone_map_graph(
    params: &VideoFilterParams,
    filter_descr: &str,
) -> Result<FilterGraph> {
    if let Some(device) = shared_vulkan_device() {
        return FilterGraph::new_video_on_device(params, filter_descr, Some(device));
    }
    static OWN_DEVICE_OPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_at_a_time = OWN_DEVICE_OPEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    FilterGraph::new_video(params, filter_descr)
}

/// Whether libplacebo exists in the linked FFmpeg and can open a Vulkan device
/// (a GPU, or lavapipe on the CPU). Checked once per process.
pub fn libplacebo_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        crate::init();
        match tone_map_graph(&probe_params(), &sdr_filter(64, 64, "yuv420p")) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "libplacebo unavailable: HDR and Dolby Vision video will not be tone mapped"
                );
                false
            }
        }
    })
}

/// A stream whose base layer only displays correctly after RPU reshaping, and
/// a libplacebo that can do it.
///
/// # Safety
/// `params` must be null or point to a valid `AVCodecParameters`.
pub unsafe fn stream_needs_reshaping(params: *const ffi::AVCodecParameters) -> bool {
    let dv = unsafe { crate::dovi::DolbyVision::from_codec_parameters(params) };
    dv.is_some_and(|dv| !dv.has_compatible_base_layer()) && libplacebo_available()
}

pub fn frame_is_hdr(frame: &Frame) -> bool {
    unsafe {
        let f = frame.as_ptr();
        !ffi::av_frame_get_side_data(f, ffi::AV_FRAME_DATA_DOVI_METADATA).is_null()
            || (*f).color_trc == ffi::AVCOL_TRC_SMPTE2084
            || (*f).color_trc == ffi::AVCOL_TRC_ARIB_STD_B67
    }
}

#[derive(PartialEq)]
struct GraphKey {
    width: i32,
    height: i32,
    format: i32,
    color: (ffi::AVColorSpace, ffi::AVColorRange),
}

/// Turns HDR and Dolby Vision frames into BT.709 `yuv420p` for stills and
/// analysis. SDR frames, and every frame when libplacebo is missing, pass
/// through untouched.
#[derive(Default)]
pub struct SdrConverter {
    graph: Option<(GraphKey, FilterGraph)>,
    unopenable: bool,
}

impl SdrConverter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn convert(&mut self, frame: &Frame) -> Result<Option<Frame>> {
        if self.unopenable || !frame_is_hdr(frame) || !libplacebo_available() {
            return Ok(None);
        }
        let color = unsafe { ((*frame.as_ptr()).colorspace, (*frame.as_ptr()).color_range) };
        let key = GraphKey {
            width: frame.width(),
            height: frame.height(),
            format: frame.format(),
            color,
        };
        if self.graph.as_ref().is_none_or(|(k, _)| *k != key) {
            let params = VideoFilterParams {
                width: key.width,
                height: key.height,
                pixel_format: key.format,
                time_base: ffi::AVRational { num: 1, den: 90000 },
                sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
                hw_frames_ctx: None,
                color: VideoColorInfo {
                    color_space: color.0,
                    color_range: color.1,
                    ..VideoColorInfo::default()
                },
                side_data_frame: std::ptr::null(),
            };
            match tone_map_graph(&params, &sdr_filter(key.width, key.height, "yuv420p")) {
                Ok(graph) => self.graph = Some((key, graph)),
                Err(e) => {
                    tracing::warn!(error = %e, "libplacebo failed to open; stills stay untouched");
                    self.unopenable = true;
                    return Ok(None);
                }
            }
        }
        let (_, graph) = self.graph.as_mut().expect("graph built above");
        // libplacebo emits a frame only once the next one has arrived, and stills
        // are picked sparsely; a one-tick echo releases the real frame now, and is
        // itself dropped when the next real frame flushes it out.
        let pts = frame.pts();
        let mut input = frame.clone_ref()?;
        graph.push_frame(&mut input)?;
        let mut echo = frame.clone_ref()?;
        echo.set_pts(pts.saturating_add(1));
        graph.push_frame(&mut echo)?;
        let mut converted = None;
        let mut out = Frame::new()?;
        while graph.pull_frame(&mut out)? {
            if out.pts() == pts {
                converted = Some(std::mem::replace(&mut out, Frame::new()?));
            }
        }
        converted.map(Some).ok_or_else(|| {
            crate::FfmpegError::new(ffi::AVERROR_EXTERNAL, "libplacebo produced no frame")
        })
    }
}
