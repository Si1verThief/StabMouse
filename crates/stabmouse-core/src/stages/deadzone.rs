//! `deadzone` — suppress motion below a threshold.

use crate::sample::Sample;
use crate::stage::Stage;

/// Gates sensor noise by discarding samples below `threshold_mm`.
///
/// **Defaults to 0 (off), which measurement supports.** A direction-reversal rate of
/// 0.2–0.4% while drawing on a 1600-dpi PMW3389 means there is essentially no noise to
/// gate, and a genuinely stationary mouse emits no reports at all. The stage exists
/// because that will not hold at 20,000 dpi.
///
/// The suppressed distance is **banked, not discarded**: without that, motion slower than
/// the threshold would be lost entirely rather than merely delayed — the accumulation
/// hazard documented in docs/modules.md, which has caused three separate bugs in this
/// codebase.
#[derive(Debug, Clone)]
pub struct Deadzone {
    enabled: bool,
    pub threshold_mm: f64,
    banked_x: f64,
    banked_y: f64,
}

impl Default for Deadzone {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl Deadzone {
    pub fn new(threshold_mm: f64) -> Self {
        Self {
            enabled: true,
            threshold_mm,
            banked_x: 0.0,
            banked_y: 0.0,
        }
    }
}

impl Stage for Deadzone {
    fn name(&self) -> &'static str {
        "deadzone"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }
        let threshold = if self.threshold_mm.is_finite() && self.threshold_mm > 0.0 {
            self.threshold_mm
        } else {
            // Exact pass-through when off, including the banked state staying empty.
            return;
        };

        self.banked_x += s.dx;
        self.banked_y += s.dy;

        if self.banked_x.hypot(self.banked_y) >= threshold {
            s.dx = self.banked_x;
            s.dy = self.banked_y;
            self.banked_x = 0.0;
            self.banked_y = 0.0;
        } else {
            s.dx = 0.0;
            s.dy = 0.0;
        }
    }

    fn reset(&mut self) {
        self.banked_x = 0.0;
        self.banked_y = 0.0;
    }

    fn settled(&self) -> bool {
        self.banked_x == 0.0 && self.banked_y == 0.0
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

    fn feed(d: &mut Deadzone, steps: &[f64]) -> f64 {
        steps
            .iter()
            .enumerate()
            .map(|(i, &dx)| {
                let mut s = Sample::new(dx, 0.0, (i as u64 + 1) * 1_000, false);
                d.process(&mut s);
                s.dx
            })
            .sum()
    }

    #[test]
    fn zero_threshold_is_exact_pass_through() {
        let mut d = Deadzone::new(0.0);
        let before = Sample::new(0.001, -0.002, 1_000, false);
        let mut after = before;
        d.process(&mut after);
        assert_eq!(before, after);
    }

    #[test]
    fn slow_motion_is_delayed_but_never_lost() {
        // Each step is far below the threshold; the total is far above it.
        let mut d = Deadzone::new(0.5);
        let total = feed(&mut d, &[0.01; 1000]);
        assert!(
            (total - 10.0).abs() < 0.5,
            "expected ~10mm through, got {total} — motion was discarded, not delayed"
        );
    }

    #[test]
    fn motion_below_the_threshold_is_held_back_for_now() {
        let mut d = Deadzone::new(1.0);
        let mut s = Sample::new(0.1, 0.0, 1_000, false);
        d.process(&mut s);
        assert_eq!(s.dx, 0.0);
        assert!(!d.settled(), "held motion means the stage is not settled");
    }

    #[test]
    fn crossing_the_threshold_releases_the_whole_bank_at_once() {
        let mut d = Deadzone::new(1.0);

        // Feed until it fires rather than asserting on a specific sample count: ten
        // additions of 0.1 sum to 0.9999999999999999, so an exact-boundary test measures
        // float representation rather than the behaviour under test.
        let mut fed = 0.0;
        let mut released = None;
        for i in 0..20 {
            let mut s = Sample::new(0.1, 0.0, (i + 1) * 1_000, false);
            fed += 0.1;
            d.process(&mut s);
            if s.dx != 0.0 {
                released = Some(s.dx);
                break;
            }
        }

        let released = released.expect("should have released within 20 samples");
        assert!(
            (released - fed).abs() < 1e-9,
            "released {released} but had been fed {fed}; the bank must emerge intact"
        );
        assert!(d.settled());
    }

    #[test]
    fn reset_drops_the_bank() {
        let mut d = Deadzone::new(1.0);
        let mut s = Sample::new(0.5, 0.0, 1_000, false);
        d.process(&mut s);
        assert!(!d.settled());
        d.reset();
        assert!(d.settled());
    }

    #[test]
    fn non_finite_input_is_swallowed() {
        let mut d = Deadzone::new(0.5);
        let mut s = Sample::new(f64::NAN, 1.0, 1_000, false);
        d.process(&mut s);
        assert_eq!((s.dx, s.dy), (0.0, 0.0));
    }
}
