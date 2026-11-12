use crate::audio::AudioGraphState;
use crate::filter::graph::FilterGraph;
use crate::frame::Frame;
use crate::{Result, ffi};

const SURROUND_GAIN: f64 = std::f64::consts::FRAC_1_SQRT_2;
const SILENCE_FLOOR_DB: f64 = -120.0;
const TRUE_PEAK: &str = "ebur128=peak=true:metadata=1";

/// True peak (dBTP) of the stereo fold-down a player renders this audio as.
///
/// The fold-down is unnormalized — `L = FL + 0.707·(C + SL + BL…)`, LFE dropped —
/// because that is what VLC's channel mixer and Web Audio do with 5.1, and they
/// apply volume after it. Per-channel peaks of a surround track understate how
/// close that sum gets to full scale by several dB. Rarer layouts fold every
/// non-front channel in at −3 dB, which over-estimates VLC's mix for 7.1 and so
/// only ever costs headroom.
pub(crate) struct DownmixPeakMeter {
    graph: Option<FilterGraph>,
    state: Option<AudioGraphState>,
    peak: Option<f64>,
}

impl DownmixPeakMeter {
    pub(crate) fn new() -> Self {
        Self {
            graph: None,
            state: None,
            peak: None,
        }
    }

    pub(crate) fn feed(&mut self, frame: &mut Frame, time_base: ffi::AVRational) -> Result<()> {
        if AudioGraphState::needs_rebuild(&self.state, frame) {
            self.flush()?;
            AudioGraphState::rebuild_filter_graph(
                &mut self.graph,
                &mut self.state,
                frame,
                time_base,
                &filter_for(frame),
            )?;
        }
        self.graph.as_mut().unwrap().push_frame_keep_ref(frame)?;
        self.drain()
    }

    pub(crate) fn finish(mut self) -> Result<Option<f64>> {
        self.flush()?;
        Ok(self.peak.map(|p| (20.0 * p.log10()).max(SILENCE_FLOOR_DB)))
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
        let mut out = Frame::new()?;
        while graph.pull_frame(&mut out)? {
            if let Some(p) = unsafe { true_peak(&out) } {
                self.peak = Some(self.peak.map_or(p, |cur| cur.max(p)));
            }
            out.unref();
        }
        Ok(())
    }
}

fn filter_for(frame: &Frame) -> String {
    match stereo_fold_down(&channel_names(frame)) {
        Some(pan) => format!("aformat=sample_fmts=dbl,{pan},{TRUE_PEAK}"),
        None => TRUE_PEAK.to_string(),
    }
}

fn stereo_fold_down(channels: &[String]) -> Option<String> {
    if channels.len() <= 2 {
        return None;
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for (i, name) in channels.iter().enumerate() {
        let (l, r) = match name.as_str() {
            n if n.starts_with("LFE") => continue,
            "FL" => (1.0, 0.0),
            "FR" => (0.0, 1.0),
            "FLC" => (SURROUND_GAIN, 0.0),
            "FRC" => (0.0, SURROUND_GAIN),
            n if n.ends_with('L') => (SURROUND_GAIN, 0.0),
            n if n.ends_with('R') => (0.0, SURROUND_GAIN),
            _ => (SURROUND_GAIN, SURROUND_GAIN),
        };
        if l > 0.0 {
            left.push(format!("{l}*c{i}"));
        }
        if r > 0.0 {
            right.push(format!("{r}*c{i}"));
        }
    }
    Some(format!(
        "pan=stereo|c0={}|c1={}",
        or_silence(left),
        or_silence(right)
    ))
}

fn or_silence(terms: Vec<String>) -> String {
    if terms.is_empty() {
        "0*c0".to_string()
    } else {
        terms.join("+")
    }
}

fn channel_names(frame: &Frame) -> Vec<String> {
    unsafe {
        let src = &(*frame.as_ptr()).ch_layout;
        let mut layout = ffi::AVChannelLayout::default();
        if src.order == ffi::AV_CHANNEL_ORDER_UNSPEC {
            ffi::av_channel_layout_default(&mut layout, src.nb_channels);
        } else {
            ffi::av_channel_layout_copy(&mut layout, src);
        }
        let names = (0..layout.nb_channels.max(0) as u32)
            .map(|i| {
                let mut buf = [0 as std::ffi::c_char; 32];
                let ch = ffi::av_channel_layout_channel_from_index(&layout, i);
                ffi::av_channel_name(buf.as_mut_ptr(), buf.len(), ch);
                std::ffi::CStr::from_ptr(buf.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        ffi::av_channel_layout_uninit(&mut layout);
        names
    }
}

unsafe fn true_peak(frame: &Frame) -> Option<f64> {
    unsafe {
        let metadata = (*frame.as_ptr()).metadata;
        if metadata.is_null() {
            return None;
        }
        let entry = ffi::av_dict_get(
            metadata,
            c"lavfi.r128.true_peak".as_ptr(),
            std::ptr::null(),
            0,
        );
        if entry.is_null() || (*entry).value.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*entry).value)
            .to_string_lossy()
            .parse::<f64>()
            .ok()
    }
}
