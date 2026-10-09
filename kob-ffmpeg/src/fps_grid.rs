//! Target-fps capture grid shared by `single_pass` and `thumbnails`.
//!
//! Given a stream of decoded video frame timestamps, decide which ones to
//! emit so the output rate matches a target fps. Keyframe sparseness can push
//! decoded frame_time past the next grid slot; we advance the clock past
//! `frame_time` rather than multiple times per frame — that was the vhash-fps
//! bug (`+ 0.5` slack halved the effective sample rate for any fps > 2).

/// Decides which decoded frames to capture against a target-fps grid.
///
/// After construction, call [`take_if_due`] once per decoded frame in timestamp
/// order. Returns `true` for the frames that should be captured; the caller
/// does the capture work. The clock advances only on capture.
pub(crate) struct TargetFpsGrid {
    interval: f64,
    next_grab_time: f64,
}

impl TargetFpsGrid {
    /// `next_grab_time_initial` is the first slot the grid will emit at.
    /// Typically 0.0 for whole-file extraction, or `start_secs` when seeking.
    pub(crate) fn new(fps: f64, next_grab_time_initial: f64) -> Self {
        Self {
            interval: 1.0 / fps,
            next_grab_time: next_grab_time_initial,
        }
    }

    pub(crate) fn take_if_due(&mut self, frame_time: f64) -> bool {
        if frame_time < self.next_grab_time - 0.1 {
            return false;
        }
        self.next_grab_time += self.interval;
        while self.next_grab_time <= frame_time {
            self.next_grab_time += self.interval;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_fps3_yields_expected_captures() {
        // The original bug: `while next_grab_time < frame_time + 0.5` advanced
        // the clock more than one slot per capture for fps > 2, halving the
        // effective rate. At fps=3 over 10s on a dense ~24fps source the bug
        // produced ~15 captures; the fix produces ~30. Accept the 30-or-31
        // window that arises from the 0.1s forward-slack: slot 30 at t=10.0
        // can fire as early as t=9.9.
        let mut grid = TargetFpsGrid::new(3.0, 0.0);
        let mut captures = 0;
        for i in 0..240 {
            let t = i as f64 / 24.0;
            if grid.take_if_due(t) {
                captures += 1;
            }
        }
        assert!(
            (28..=31).contains(&captures),
            "fps=3 over ~10s must capture ~30 frames (was {captures})"
        );
        assert!(captures > 20, "regression: bug would halve the rate to ~15");
    }

    #[test]
    fn sparse_keyframes_catch_up_past_frame_time() {
        // If decoded frame_time jumps forward (sparse keyframes), we catch up
        // past the gap — next grid slot must be strictly after frame_time.
        let mut grid = TargetFpsGrid::new(3.0, 0.0);
        assert!(grid.take_if_due(0.0));
        // Jump 10 seconds — past slots 0.33, 0.67, ..., 9.67.
        assert!(grid.take_if_due(10.0));
        // Next slot should be 10.33, not something in the gap.
        assert!(!grid.take_if_due(10.1));
        assert!(grid.take_if_due(10.4));
    }

    #[test]
    fn start_offset_first_slot_is_start_secs() {
        // thumbnails starts mid-file — first emitted slot is start_secs.
        let mut grid = TargetFpsGrid::new(1.0, 5.0);
        assert!(!grid.take_if_due(4.0));
        assert!(grid.take_if_due(5.0));
        assert!(!grid.take_if_due(5.5));
        assert!(grid.take_if_due(6.0));
    }
}
