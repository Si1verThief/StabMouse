//! `stabilize` — pulled string / lazy rope.

use crate::sample::Sample;
use crate::stage::Stage;

/// Below this remaining gap, closing is declared complete.
const CLOSE_EPSILON_MM: f64 = 1.0e-6;

/// The cursor drags an anchor on a leash. The anchor is what downstream sees.
///
/// This produces the characteristic confident sweeping arc and is the single most
/// important parameter for drawing feel.
///
/// The radius is **millimetres of hand movement**, never screen pixels. Pixels are not
/// computable here — the mm-to-pixel mapping depends on compositor pointer gain and
/// per-monitor scale, neither of which this crate can see — and millimetres are what
/// make a shared preset behave the same on someone else's mouse and screen. See the
/// units section of docs/stages.md.
///
/// Measured on real recordings: 0.2mm removes visible tremor with shape detail intact,
/// 0.5mm is smooth with the shape preserved, 2mm distorts, 4mm destroys a drawing that
/// spans ~8mm of hand movement.
#[derive(Debug, Clone)]
pub struct Stabilize {
    enabled: bool,
    pub radius_mm: f64,
    /// How quickly the anchor closes on the leash boundary, `0..=1`.
    pub catch_up: f64,
    /// Jump the anchor straight to the cursor when a stroke ends.
    ///
    /// **Off by default.** It recovers the right distance but draws a straight line to
    /// get there — measured at 4000-9000 mm/s in one sample. The correct mechanism is
    /// for the consumer to feed zero-motion ticks until `settled()`, letting the anchor
    /// converge along its actual path. This remains available for consumers that cannot
    /// tick, where falling short would be worse.
    pub snap_on_stroke_end: bool,

    cursor_x: f64,
    cursor_y: f64,
    anchor_x: f64,
    anchor_y: f64,
    primed: bool,
    /// True between stroke end and full convergence.
    closing: bool,
}

impl Default for Stabilize {
    fn default() -> Self {
        Self {
            enabled: true,
            radius_mm: 0.0,
            catch_up: 1.0,
            snap_on_stroke_end: false,
            cursor_x: 0.0,
            cursor_y: 0.0,
            anchor_x: 0.0,
            anchor_y: 0.0,
            primed: false,
            closing: false,
        }
    }
}

impl Stabilize {
    pub fn new(radius_mm: f64, catch_up: f64) -> Self {
        Self {
            radius_mm,
            catch_up,
            ..Default::default()
        }
    }
}

impl Stage for Stabilize {
    fn name(&self) -> &'static str {
        "stabilize"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }

        if !self.primed {
            self.cursor_x = 0.0;
            self.cursor_y = 0.0;
            self.anchor_x = 0.0;
            self.anchor_y = 0.0;
            self.primed = true;
        }

        self.cursor_x += s.dx;
        self.cursor_y += s.dy;

        let prev_x = self.anchor_x;
        let prev_y = self.anchor_y;

        let radius = if self.radius_mm.is_finite() && self.radius_mm > 0.0 {
            self.radius_mm
        } else {
            0.0
        };
        let catch_up = if self.catch_up.is_finite() {
            self.catch_up.clamp(0.0, 1.0)
        } else {
            1.0
        };

        if s.stroke_end {
            if self.snap_on_stroke_end {
                self.anchor_x = self.cursor_x;
                self.anchor_y = self.cursor_y;
                self.closing = false;
            } else {
                self.closing = true;
            }
        }

        if s.stroke_start {
            // Begin the stroke with no accumulated lag.
            self.anchor_x = self.cursor_x;
            self.anchor_y = self.cursor_y;
            self.closing = false;
        } else {
            // While closing, the leash collapses so the anchor converges all the way
            // to the cursor instead of resting a radius behind it.
            //
            // The resting lag is not a defect — it is what pulled-string *is*, and it
            // is why `settled()` cannot simply ask whether anchor equals cursor. But at
            // stroke end the ink has to reach where the hand actually stopped, so the
            // gap must be closed deliberately, over ticks rather than in one jump.
            let effective_radius = if self.closing { 0.0 } else { radius };

            let dx = self.cursor_x - self.anchor_x;
            let dy = self.cursor_y - self.anchor_y;
            let dist = dx.hypot(dy);

            // The anchor only advances along the RADIAL direction. Tangential cursor
            // motion barely moves it even at full hand speed — which is why the
            // pressure stage cannot naively derive velocity from this output. See
            // stages.md.
            if dist > effective_radius && dist > 0.0 {
                let target_x = self.cursor_x - dx / dist * effective_radius;
                let target_y = self.cursor_y - dy / dist * effective_radius;
                self.anchor_x += (target_x - self.anchor_x) * catch_up;
                self.anchor_y += (target_y - self.anchor_y) * catch_up;
            }
        }

        // A geometric approach never reaches its target exactly. Terminate it rather
        // than leaving an endless tail of vanishing deltas.
        if self.closing {
            let gap = (self.cursor_x - self.anchor_x).hypot(self.cursor_y - self.anchor_y);
            if gap < CLOSE_EPSILON_MM {
                self.anchor_x = self.cursor_x;
                self.anchor_y = self.cursor_y;
                self.closing = false;
            }
        }

        s.dx = self.anchor_x - prev_x;
        s.dy = self.anchor_y - prev_y;
    }

    fn reset(&mut self) {
        self.primed = false;
        self.cursor_x = 0.0;
        self.cursor_y = 0.0;
        self.anchor_x = 0.0;
        self.anchor_y = 0.0;
        self.closing = false;
    }

    /// Settled means "nothing outstanding", not "anchor equals cursor".
    ///
    /// A pulled string legitimately rests a full radius behind the cursor, so demanding
    /// coincidence would make this permanently false and any tick loop infinite.
    fn settled(&self) -> bool {
        !self.closing
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(st: &mut Stabilize, steps: &[(f64, f64)], down: bool) -> (f64, f64) {
        let mut out = (0.0, 0.0);
        for (i, &(dx, dy)) in steps.iter().enumerate() {
            let mut s = Sample::new(dx, dy, (i as u64 + 1) * 1_000, down);
            s.dt = 0.001;
            s.stroke_start = down && i == 0;
            st.process(&mut s);
            out.0 += s.dx;
            out.1 += s.dy;
        }
        out
    }

    #[test]
    fn zero_radius_is_exact_pass_through() {
        let mut st = Stabilize::new(0.0, 1.0);
        let before = {
            let mut s = Sample::new(2.5, -1.5, 1_000, false);
            s.dt = 0.001;
            s
        };
        let mut after = before;
        st.process(&mut after);
        assert!((after.dx - before.dx).abs() < 1e-12);
        assert!((after.dy - before.dy).abs() < 1e-12);
    }

    #[test]
    fn output_lags_by_at_most_the_radius() {
        let mut st = Stabilize::new(5.0, 1.0);
        let steps: Vec<(f64, f64)> = (0..200).map(|_| (0.5, 0.0)).collect();
        let (out_x, _) = feed(&mut st, &steps, false);
        let input_total = 100.0;
        let lag = input_total - out_x;
        assert!(
            lag >= 0.0 && lag <= 5.0 + 1e-6,
            "lag {lag} should settle at the radius"
        );
    }

    #[test]
    fn motion_within_the_radius_produces_no_output() {
        let mut st = Stabilize::new(10.0, 1.0);
        // Total travel of 4mm, well inside a 10mm leash.
        let (out_x, out_y) = feed(&mut st, &[(1.0, 0.0), (1.0, 0.0), (1.0, 0.0), (1.0, 0.0)], false);
        assert!(out_x.abs() < 1e-12 && out_y.abs() < 1e-12);
    }

    /// Ticking to convergence is the supported way to recover the lag; snapping is the
    /// fallback for consumers that cannot tick. Both must end up in the same place.
    #[test]
    fn ticking_to_settled_recovers_the_full_distance() {
        let mut st = Stabilize::new(5.0, 0.35);
        let mut total = 0.0;
        let mut t = 0u64;

        for i in 0..2 {
            t += 1_000;
            let mut s = Sample::new(20.0, 0.0, t, true);
            s.dt = 0.001;
            s.stroke_start = i == 0;
            st.process(&mut s);
            total += s.dx;
        }

        t += 1_000;
        let mut s = Sample::new(0.0, 0.0, t, false);
        s.dt = 0.001;
        s.stroke_end = true;
        st.process(&mut s);
        total += s.dx;

        assert!(!st.settled(), "lag should still be outstanding at stroke end");

        // Zero-motion ticks, as the daemon supplies.
        let mut ticks = 0;
        while !st.settled() && ticks < 10_000 {
            t += 1_000;
            let mut s = Sample::new(0.0, 0.0, t, false);
            s.dt = 0.001;
            st.process(&mut s);
            total += s.dx;
            ticks += 1;
        }

        assert!(st.settled(), "should have converged within {ticks} ticks");
        assert!(
            (total - 40.0).abs() < 1e-4,
            "ticking should recover all 40mm, got {total} after {ticks} ticks"
        );
    }

    #[test]
    fn snap_remains_available_for_consumers_that_cannot_tick() {
        let mut st = Stabilize::new(5.0, 1.0);
        st.snap_on_stroke_end = true;
        let mut total = 0.0;

        for (i, t) in [1_000u64, 2_000].into_iter().enumerate() {
            let mut s = Sample::new(20.0, 0.0, t, true);
            s.dt = 0.001;
            s.stroke_start = i == 0;
            st.process(&mut s);
            total += s.dx;
        }

        let mut s = Sample::new(0.0, 0.0, 3_000, false);
        s.dt = 0.001;
        s.stroke_end = true;
        st.process(&mut s);
        total += s.dx;

        assert!((total - 40.0).abs() < 1e-9, "snap should recover 40mm, got {total}");
    }

    #[test]
    fn non_finite_input_is_swallowed_not_propagated() {
        let mut st = Stabilize::new(5.0, 0.5);
        let mut s = Sample::new(f64::NAN, f64::INFINITY, 1_000, true);
        s.dt = 0.001;
        st.process(&mut s);
        assert_eq!((s.dx, s.dy), (0.0, 0.0));

        let mut s = Sample::new(1.0, 0.0, 2_000, true);
        s.dt = 0.001;
        st.process(&mut s);
        assert!(s.dx.is_finite());
    }
}
