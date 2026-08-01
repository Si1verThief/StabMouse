//! `stabilize` — pulled string / lazy rope.

use crate::sample::Sample;
use crate::stage::Stage;

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
    /// **Off by default, and it should usually stay off.** See the note on button state
    /// below: catching up moves the stroke past where the user meant it to end, and it
    /// empties the leash so the next movement has a dead zone a full radius wide.
    /// Retained only because a consumer drawing with a very large radius might prefer it.
    pub snap_on_stroke_end: bool,

    cursor_x: f64,
    cursor_y: f64,
    anchor_x: f64,
    anchor_y: f64,
    primed: bool,
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

        // Button state deliberately does NOT touch the leash.
        //
        // Two earlier versions moved the anchor at a stroke boundary — snapping to the
        // cursor on press, and converging to it on release — on the reasoning that ink
        // should begin and end at the hand's true position. Both were backwards for the
        // same reason: **the anchor is what the user sees**, so they aim with it, press
        // when it is on target, and release when it is where the stroke should end.
        // Snapping on press teleported the cursor by a full radius; converging on release
        // pushed the stroke past its intended end *and* emptied the leash, so the next
        // movement had a dead zone one radius wide before anything moved at all.
        //
        // A stabiliser is a motion filter. Clicking is orthogonal to motion.
        if s.stroke_end && self.snap_on_stroke_end {
            self.anchor_x = self.cursor_x;
            self.anchor_y = self.cursor_y;
        }

        let dx = self.cursor_x - self.anchor_x;
        let dy = self.cursor_y - self.anchor_y;
        let dist = dx.hypot(dy);

        // The anchor only advances along the RADIAL direction. Tangential cursor motion
        // barely moves it even at full hand speed — which is why the pressure stage cannot
        // naively derive velocity from this output. See stages.md.
        if dist > radius && dist > 0.0 {
            let target_x = self.cursor_x - dx / dist * radius;
            let target_y = self.cursor_y - dy / dist * radius;
            self.anchor_x += (target_x - self.anchor_x) * catch_up;
            self.anchor_y += (target_y - self.anchor_y) * catch_up;
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
    }

    /// Always settled: this stage never holds anything back for later.
    ///
    /// A pulled string rests up to a full radius behind the cursor, permanently and by
    /// design. That lag is the filter working, not output awaiting a flush — so asking for
    /// coincidence would make this forever false and any tick loop infinite.
    fn settled(&self) -> bool {
        true
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

    /// The lag persists after a stroke ends, and that is correct.
    ///
    /// The stroke ends where the *visible* cursor was when the button came up, because that
    /// is what the user aimed with. Catching up afterwards would push the line past its
    /// intended end.
    #[test]
    fn the_leash_survives_a_stroke_ending() {
        let mut st = Stabilize::new(5.0, 1.0);
        let mut total = 0.0;
        let mut t = 0u64;

        for i in 0..40 {
            t += 1_000;
            let mut s = Sample::new(1.0, 0.0, t, true);
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

        assert!(st.settled(), "nothing should be held back for a later flush");
        assert!(
            (total - 35.0).abs() < 1e-6,
            "40mm in with a 5mm leash should give 35mm out, got {total}"
        );
    }

    /// Regression guard for the dead zone after a click.
    ///
    /// An earlier version converged the anchor onto the cursor when a stroke ended, which
    /// emptied the leash — so the next movement had to travel a full radius before the
    /// cursor moved at all. Reported from use as having to "feed a bunch of inputs to be
    /// allowed to continue moving".
    #[test]
    fn clicking_does_not_create_a_dead_zone() {
        let mut st = Stabilize::new(5.0, 1.0);
        let mut t = 0u64;

        // Establish steady motion so the leash is taut.
        for _ in 0..40 {
            t += 1_000;
            let mut s = Sample::new(1.0, 0.0, t, false);
            s.dt = 0.001;
            st.process(&mut s);
        }

        // A full click: press then release, without moving.
        for (i, down) in [true, false].into_iter().enumerate() {
            t += 1_000;
            let mut s = Sample::new(0.0, 0.0, t, down);
            s.dt = 0.001;
            s.stroke_start = i == 0;
            s.stroke_end = i == 1;
            st.process(&mut s);
        }

        // The very next movement must produce output immediately.
        t += 1_000;
        let mut s = Sample::new(1.0, 0.0, t, false);
        s.dt = 0.001;
        st.process(&mut s);

        assert!(
            (s.dx - 1.0).abs() < 1e-6,
            "after a click, 1mm of movement should still yield 1mm of output; got {} \
             (the leash was emptied, so the cursor is stuck)",
            s.dx
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
