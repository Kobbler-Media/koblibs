use crate::ffi;
use crate::filter::graph::{FilterGraph, VideoColorInfo, VideoFilterParams};
use crate::frame::Frame;

/// A CPU filter chain whose source format is only known once a frame arrives.
pub(crate) struct CpuFilterPlan {
    pub(crate) chain: String,
    pub(crate) time_base: ffi::AVRational,
    pub(crate) color: VideoColorInfo,
    /// What the encoder was opened with. The encoder has to be opened before a
    /// frame exists, so its format is a prediction; the graph is what makes the
    /// prediction true.
    pub(crate) target: crate::types::PixelFormat,
    /// Source pixel aspect. `scale` derives its output SAR from this, so a
    /// square-pixel default would make it "correct" a 720x576 DVB frame that was
    /// never square to begin with.
    pub(crate) sample_aspect_ratio: ffi::AVRational,
    /// Set when `chain` tone-maps through libplacebo: the same chain without it,
    /// so a Vulkan device that will not open costs colour, not playback.
    pub(crate) untoned_chain: Option<String>,
}

impl CpuFilterPlan {
    /// `frame` is the decoded frame as the encoder will see it — already
    /// downloaded from the GPU when the decode really was hardware.
    ///
    /// The chain always ends in a `format` filter. Letting the frame's own
    /// layout through whenever it was "close enough" is what produced correct
    /// greyscale under green and magenta blocks: attaching a hw_device_ctx does
    /// not mean the decoder took it, so a codec the GPU cannot decode (MPEG-2 on
    /// current NVDEC) falls back to software and emits planar YUV into an encoder
    /// opened for semi-planar NV12, which reads the U and V planes as one
    /// interleaved plane. Pinning costs nothing when the guess was right —
    /// `format` is a no-op on a frame that already matches.
    pub(crate) fn build(&self, frame: &Frame) -> crate::Result<FilterGraph> {
        let fmt = frame.format();
        let chain = self.finished(&self.chain);
        let params = VideoFilterParams {
            width: frame.width(),
            height: frame.height(),
            pixel_format: fmt,
            time_base: self.time_base,
            sample_aspect_ratio: self.sample_aspect_ratio,
            hw_frames_ctx: None,
            color: self.color,
            side_data_frame: std::ptr::null(),
        };
        let Some(untoned) = &self.untoned_chain else {
            return FilterGraph::new_video(&params, &chain);
        };
        crate::sdr::tone_map_graph(&params, &chain).or_else(|e| {
            tracing::warn!(error = %e, "libplacebo failed to open; encoding without tone mapping");
            FilterGraph::new_video(&params, &self.finished(untoned))
        })
    }

    fn finished(&self, chain: &str) -> String {
        let format = format!("format=pix_fmts={}", self.target.name());
        if chain.is_empty() {
            format
        } else {
            format!("{chain},{format}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_frame(fmt: ffi::AVPixelFormat, w: i32, h: i32) -> Frame {
        let mut f = Frame::new().unwrap();
        unsafe {
            let p = f.as_mut_ptr();
            (*p).format = fmt;
            (*p).width = w;
            (*p).height = h;
            assert_eq!(ffi::av_frame_get_buffer(p, 0), 0);
            for plane in 0..3 {
                let d = (*p).data[plane];
                if !d.is_null() {
                    let ls = (*p).linesize[plane] as usize;
                    let rows = if plane == 0 {
                        h as usize
                    } else {
                        (h / 2) as usize
                    };
                    std::ptr::write_bytes(d, 128u8, ls * rows);
                }
            }
            (*p).pts = 0;
        }
        f
    }

    fn plan(target: crate::types::PixelFormat) -> CpuFilterPlan {
        CpuFilterPlan {
            chain: "scale=1024:576".to_string(),
            time_base: ffi::AVRational { num: 1, den: 25 },
            color: VideoColorInfo::default(),
            target,
            sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
            untoned_chain: None,
        }
    }

    /// DVB SD is 720x576 at 64:45 — 16:9 on screen, and the decoder sets that on
    /// every frame. The graph must carry it through: a chain declared square
    /// makes `scale` rewrite the SAR to "correct" an aspect that was never wrong,
    /// and the picture reaches the browser squashed to 5:4.
    ///
    /// The encoder half is separate and cannot be seen from here — `avctx
    /// ->sample_aspect_ratio` is what the muxer turns into the `pasp` box, and
    /// leaving it unset is what produced 5:4 output no matter what the frames
    /// said. That is set from the source stream in `transcode_to_hls_inner`.
    #[test]
    fn a_non_square_source_keeps_its_display_aspect_through_the_graph() {
        // bwdif is deliberately absent: it needs two frames before it emits one,
        // and the scales are the filters that actually rewrite SAR. 600x480 is
        // `transcode_dimensions` at a 480 cap — proportional, so the pixel aspect
        // is unchanged, which is why handing the encoder the *source* SAR is right.
        for chain in ["", "scale=720:576", "scale=600:480"] {
            let mut p = plan(crate::types::PixelFormat::YUV420P);
            p.chain = chain.to_string();
            p.sample_aspect_ratio = ffi::AVRational { num: 64, den: 45 };

            let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 720, 576);
            unsafe { (*f.as_mut_ptr()).sample_aspect_ratio = ffi::AVRational { num: 64, den: 45 } };

            let mut g = p.build(&f).unwrap();
            g.push_frame(&mut f).unwrap();
            let mut out = Frame::new().unwrap();
            assert!(g.pull_frame(&mut out).unwrap(), "chain {chain:?}");
            let sar = unsafe { (*out.as_ptr()).sample_aspect_ratio };
            assert_eq!(
                (sar.num, sar.den),
                (64, 45),
                "chain {chain:?} flattened the pixel aspect to square"
            );
        }
    }

    fn filter_once(graph: &mut FilterGraph, frame: &mut Frame) -> ffi::AVPixelFormat {
        graph.push_frame(frame).unwrap();
        let mut out = Frame::new().unwrap();
        assert!(
            graph.pull_frame(&mut out).unwrap(),
            "graph produced no frame"
        );
        out.format()
    }

    /// Attaching a hw device does not mean the decoder took it: a codec the GPU
    /// cannot decode falls back to software and emits planar YUV. The plan has to
    /// read the frame, not the config, or the graph is built for the wrong layout.
    /// It also has to *end* on the encoder's format, whichever way the guess went:
    /// the encoder is already open by the time the first frame exists, so the
    /// graph is the only place the two can be reconciled.
    #[test]
    fn the_graph_reads_the_frames_format_and_ends_on_the_encoders() {
        for target in [
            crate::types::PixelFormat::YUV420P,
            crate::types::PixelFormat::NV12,
        ] {
            for source in [ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_NV12] {
                let mut f = grey_frame(source, 704, 576);
                let mut g = plan(target).build(&f).unwrap();
                assert_eq!(
                    filter_once(&mut g, &mut f),
                    target.raw(),
                    "source {source} was not converted to the encoder's {}",
                    target.name()
                );
            }
        }
    }

    /// A 10-bit source still lands on the encoder's 8-bit format — browsers band
    /// badly on High 10, and the encoder was never opened for it.
    #[test]
    fn a_ten_bit_source_is_brought_down_to_the_encoders_format() {
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P10LE, 704, 576);
        let mut g = plan(crate::types::PixelFormat::YUV420P).build(&f).unwrap();
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_YUV420P);
    }

    /// The chain is empty whenever there is nothing to deinterlace, rotate, scale
    /// or burn in — which is exactly when the frames used to reach the encoder
    /// unconverted. The format filter has to stand on its own there.
    #[test]
    fn an_otherwise_empty_chain_still_converts() {
        let mut plan = plan(crate::types::PixelFormat::NV12);
        plan.chain = String::new();
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 704, 576);
        let mut g = plan.build(&f).unwrap();
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_NV12);
    }

    /// Why the above matters: libavfilter does not reject a source whose declared
    /// format is wrong. It logs and reinterprets, so YUV420P pushed into a graph
    /// told to expect NV12 comes back out as NV12 — the two chroma planes read as
    /// one interleaved plane. Luma is untouched, so it reaches the screen as a
    /// correct greyscale picture under green and magenta chroma, not as an error.
    #[test]
    fn a_wrongly_declared_source_format_is_reinterpreted_not_rejected() {
        let params = VideoFilterParams {
            width: 704,
            height: 576,
            pixel_format: ffi::AV_PIX_FMT_NV12,
            time_base: ffi::AVRational { num: 1, den: 25 },
            sample_aspect_ratio: ffi::AVRational { num: 1, den: 1 },
            hw_frames_ctx: None,
            color: VideoColorInfo::default(),
            side_data_frame: std::ptr::null(),
        };
        let mut g = FilterGraph::new_video(&params, "scale=1024:576").unwrap();
        let mut f = grey_frame(ffi::AV_PIX_FMT_YUV420P, 704, 576);
        assert_eq!(filter_once(&mut g, &mut f), ffi::AV_PIX_FMT_NV12);
    }
}
