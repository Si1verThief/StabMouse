//! `smooth` — one-euro adaptive low pass.

use crate::sample::Sample;
use crate::stage::Stage;

/// Adaptive low pass: heavy smoothing when slow, low lag when fast.
///
/// Applied to integrated position rather than to deltas, because the one-euro filter
/// is defined over a signal and its adaptive term is driven by that signal's
/// derivative. Filtering deltas directly would make the adaptation respond to
/// acceleration instead of velocity.
#[derive(Debug, Clone)]
pub struct Smooth {
    enabled: bool,
    pub min_cutoff_hz: f64,
    pub beta: f64,
    pub d_cutoff_hz: f64,

    position_x: f64,
    position_y: f64,
    filtered_x: f64,
    filtered_y: f64,
    derivative_x: f64,
    derivative_y: f64,
    primed: bool,
}

impl Default for Smooth {
    fn default() -> Self {
        Self {
            enabled: true,
            // Measured general default (docs/stages.md): 0.30mm lag on deliberate
            // strokes while removing ~12% of path wobble. `mc2 / b0.2` is the tremor
            // variant. Beta must be in this range, not the ~0.007 seen in one-euro
            // literature -- it multiplies velocity, and velocity here is mm/s.
            min_cutoff_hz: 5.0,
            beta: 0.05,
            d_cutoff_hz: 1.0,
            position_x: 0.0,
            position_y: 0.0,
            filtered_x: 0.0,
            filtered_y: 0.0,
            derivative_x: 0.0,
            derivative_y: 0.0,
            primed: false,
        }
    }
}

impl Smooth {
    pub fn new(min_cutoff_hz: f64, beta: f64, d_cutoff_hz: f64) -> Self {
        Self {
            min_cutoff_hz,
            beta,
            d_cutoff_hz,
            ..Default::default()
        }
    }
}

/// Smoothing factor for a given cutoff frequency and interval.
fn alpha(cutoff_hz: f64, dt: f64) -> f64 {
    if !(cutoff_hz.is_finite() && cutoff_hz > 0.0) || !(dt.is_finite() && dt > 0.0) {
        return 1.0;
    }
    let tau = 1.0 / (2.0 * core::f64::consts::PI * cutoff_hz);
    let a = 1.0 / (1.0 + tau / dt);
    if a.is_finite() {
        a.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

impl Stage for Smooth {
    fn name(&self) -> &'static str {
        "smooth"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }

        if !self.primed {
            self.position_x = 0.0;
            self.position_y = 0.0;
            self.filtered_x = 0.0;
            self.filtered_y = 0.0;
            self.derivative_x = 0.0;
            self.derivative_y = 0.0;
            self.primed = true;
        }

        let prev_filtered_x = self.filtered_x;
        let prev_filtered_y = self.filtered_y;

        self.position_x += s.dx;
        self.position_y += s.dy;

        // Filtered derivative, used to widen the cutoff when moving quickly.
        let ad = alpha(self.d_cutoff_hz, s.dt);
        let raw_dx = s.dx / s.dt;
        let raw_dy = s.dy / s.dt;
        if raw_dx.is_finite() {
            self.derivative_x += ad * (raw_dx - self.derivative_x);
        }
        if raw_dy.is_finite() {
            self.derivative_y += ad * (raw_dy - self.derivative_y);
        }

        let beta = if self.beta.is_finite() { self.beta } else { 0.0 };
        let cutoff_x = self.min_cutoff_hz + beta * self.derivative_x.abs();
        let cutoff_y = self.min_cutoff_hz + beta * self.derivative_y.abs();

        self.filtered_x += alpha(cutoff_x, s.dt) * (self.position_x - self.filtered_x);
        self.filtered_y += alpha(cutoff_y, s.dt) * (self.position_y - self.filtered_y);

        s.dx = self.filtered_x - prev_filtered_x;
        s.dy = self.filtered_y - prev_filtered_y;
    }

    fn reset(&mut self) {
        self.primed = false;
    }

    fn settled(&self) -> bool {
        !self.primed
            || (self.position_x - self.filtered_x).hypot(self.position_y - self.filtered_y)
                < 1.0e-6
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

    fn feed(st: &mut Smooth, steps: &[f64]) -> Vec<f64> {
        steps
            .iter()
            .enumerate()
            .map(|(i, &dx)| {
                let mut s = Sample::new(dx, 0.0, (i as u64 + 1) * 1_000, false);
                s.dt = 0.001;
                st.process(&mut s);
                s.dx
            })
            .collect()
    }

    #[test]
    fn very_high_cutoff_is_near_transparent() {
        let mut st = Smooth::new(1000.0, 0.0, 1.0);
        let out = feed(&mut st, &[1.0; 200]);
        let total: f64 = out.iter().sum();
        assert!(
            (total - 200.0).abs() < 1.0,
            "expected ~200mm through, got {total}"
        );
    }

    #[test]
    fn smoothing_attenuates_a_single_spike() {
        let mut st = Smooth::new(2.0, 0.0, 1.0);
        let mut steps = vec![0.0; 20];
        steps.push(10.0);
        steps.extend(vec![0.0; 20]);
        let out = feed(&mut st, &steps);
        let peak = out.iter().cloned().fold(0.0f64, f64::max);
        assert!(peak < 10.0, "spike should be attenuated, peak was {peak}");
    }

    #[test]
    fn no_motion_is_permanently_lost_once_settled() {
        let mut st = Smooth::new(5.0, 0.01, 1.0);

        // A low pass lags a continuously-moving signal *permanently*, so the totals
        // only match once the input stops and the filter settles. Asserting
        // convergence mid-motion would be asserting that the filter does nothing.
        let mut steps = vec![0.5; 4000];
        steps.extend(vec![0.0; 4000]);

        let total: f64 = feed(&mut st, &steps).iter().sum();
        assert!(
            (total - 2000.0).abs() < 1.0,
            "expected ~2000mm once settled, got {total}"
        );
    }

    #[test]
    fn lag_during_continuous_motion_is_bounded() {
        // The flip side: it must lag, but by a bounded amount rather than unboundedly.
        let mut st = Smooth::new(5.0, 0.0, 1.0);
        let total: f64 = feed(&mut st, &[0.5; 4000]).iter().sum();
        let lag = 2000.0 - total;
        assert!(lag > 0.0, "a low pass should lag; got {lag}");
        assert!(lag < 50.0, "lag should stay bounded; got {lag}");
    }

    #[test]
    fn non_finite_input_is_swallowed() {
        let mut st = Smooth::new(5.0, 0.01, 1.0);
        let mut s = Sample::new(f64::NAN, 1.0, 1_000, false);
        s.dt = 0.001;
        st.process(&mut s);
        assert_eq!((s.dx, s.dy), (0.0, 0.0));
    }
}
