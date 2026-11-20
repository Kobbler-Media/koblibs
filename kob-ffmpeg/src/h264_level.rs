//! Fits an H.264 encode inside a level's macroblock rate and frame size.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LevelFit {
    pub width: i32,
    pub height: i32,
    pub frame_step: u32,
}

/// (MaxMBPS, MaxFS) from Table A-1 of the H.264 spec.
fn limits(level_idc: u32) -> Option<(f64, i64)> {
    Some(match level_idc {
        30 => (40_500.0, 1_620),
        31 => (108_000.0, 3_600),
        32 => (216_000.0, 5_120),
        40 | 41 => (245_760.0, 8_192),
        42 => (522_240.0, 8_704),
        50 => (589_824.0, 22_080),
        51 => (983_040.0, 36_864),
        52 => (2_073_600.0, 36_864),
        _ => return None,
    })
}

const LADDER: [i32; 7] = [2160, 1440, 1080, 720, 576, 480, 360];
const MIN_FPS: f64 = 23.0;

fn macroblocks(width: i32, height: i32) -> i64 {
    i64::from((width + 15) / 16) * i64::from((height + 15) / 16)
}

fn even(x: f64) -> i32 {
    ((x / 2.0).round() as i32) * 2
}

/// Keeps the source size and drops frames by a whole divisor first, so cadence
/// stays even; only a size that fits at no allowed rate steps down the ladder.
pub(crate) fn fit(width: i32, height: i32, fps: f64, level_idc: u32) -> LevelFit {
    let unchanged = LevelFit {
        width,
        height,
        frame_step: 1,
    };
    let Some((max_mbps, max_fs)) = limits(level_idc) else {
        return unchanged;
    };
    let sizes = std::iter::once((width, height)).chain(
        LADDER
            .iter()
            .filter(|&&h| h < height)
            .map(|&h| (even(f64::from(width) * f64::from(h) / f64::from(height)), h)),
    );
    let mut last = unchanged;
    for (w, h) in sizes {
        let mbs = macroblocks(w, h);
        let mut step = 1;
        loop {
            last = LevelFit {
                width: w,
                height: h,
                frame_step: step,
            };
            if mbs <= max_fs && mbs as f64 * fps / f64::from(step) <= max_mbps {
                return last;
            }
            if fps / f64::from(step + 1) < MIN_FPS {
                break;
            }
            step += 1;
        }
    }
    last
}

/// What an encode with `encoder` at `frame_rate` comes out as: fitted to `level_idc` for an
/// H.264 encoder, otherwise as asked.
pub(crate) fn output_geometry(
    encoder: &str,
    level_idc: Option<u32>,
    width: i32,
    height: i32,
    frame_rate: Option<(i32, i32)>,
) -> LevelFit {
    match (level_idc, frame_rate) {
        (Some(level), Some((num, den)))
            if crate::codec::encoder::h264_level_option(encoder, level).is_some() =>
        {
            fit(width, height, f64::from(num) / f64::from(den), level)
        }
        _ => LevelFit {
            width,
            height,
            frame_step: 1,
        },
    }
}

pub(crate) fn frame_step_filter(frame_step: u32) -> Option<String> {
    (frame_step > 1).then(|| format!("framestep={frame_step}"))
}

pub(crate) fn with_frame_step(chain: String, frame_step: u32) -> String {
    match frame_step_filter(frame_step) {
        Some(step) => format!("{chain},{step}"),
        None => chain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encode_is_fitted_to_the_level_by_rate_first_then_size() {
        let cases = [
            ((1920, 1080, 60.0), 41, (1920, 1080, 2)),
            ((1920, 1080, 59.94), 41, (1920, 1080, 2)),
            ((1920, 1080, 50.0), 41, (1920, 1080, 2)),
            ((1920, 1080, 24.0), 41, (1920, 1080, 1)),
            ((1920, 1080, 30.0), 41, (1920, 1080, 1)),
            ((3840, 2160, 30.0), 41, (1920, 1080, 1)),
            ((3840, 2160, 60.0), 41, (1920, 1080, 2)),
            ((3840, 2160, 60.0), 42, (1920, 1080, 1)),
            ((1280, 720, 60.0), 41, (1280, 720, 1)),
            ((1920, 1080, 60.0), 42, (1920, 1080, 1)),
            ((1920, 1080, 60.0), 51, (1920, 1080, 1)),
            ((1920, 1080, 30.0), 31, (1280, 720, 1)),
            ((1920, 1080, 60.0), 31, (1280, 720, 2)),
            ((1920, 1080, 60.0), 99, (1920, 1080, 1)),
        ];
        for ((width, height, fps), level, (w, h, frame_step)) in cases {
            assert_eq!(
                fit(width, height, fps, level),
                LevelFit {
                    width: w,
                    height: h,
                    frame_step
                },
                "{width}x{height}@{fps} level {level}"
            );
        }
    }
}
