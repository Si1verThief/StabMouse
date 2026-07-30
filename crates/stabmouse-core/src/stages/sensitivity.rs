//! `sensitivity` — flat multiplier, with an optional curve inside it.

use crate::sample::Sample;
use crate::stage::Stage;

/// How the multiplier varies with speed.
///
/// The variants are deliberately open-ended: `Natural` and `Custom` are named in
/// stages.md and will be added without touching this stage, per D12.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    /// Constant multiplier. The common case, and the reason this stage is called
    /// `sensitivity` rather than `accel`.
    Flat,
    /// `factor = (speed / reference)^exponent`, clamped to `[min, max]`.
    Power {
        reference_mm_s: f64,
        exponent: f64,
        min: f64,
        max: f64,
    },
}

/// Scales motion, optionally as a function of speed.
///
/// Named for the common case rather than the specialist one: a flat multiplier is
/// what most users need, and burying it under a heading called "acceleration" beside
/// a curve editor stops the people who need it most from touching it. The curve is
/// progressive disclosure *inside* this stage — see D11 and vocabulary.md.
#[derive(Debug, Clone)]
pub struct Sensitivity {
    enabled: bool,
    pub multiplier: f64,
    pub y_ratio: f64,
    pub max_multiplier: Option<f64>,
    pub curve: Curve,
    /// Low-passed speed in mm/s, so the curve does not chase per-event noise.
    speed: f64,
    pub speed_smoothing_s: f64,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self {
            enabled: true,
            multiplier: 1.0,
            y_ratio: 1.0,
            max_multiplier: None,
            curve: Curve::Flat,
            speed: 0.0,
            speed_smoothing_s: 0.02,
        }
    }
}

impl Sensitivity {
    pub fn flat(multiplier: f64) -> Self {
        Self {
            multiplier,
            ..Default::default()
        }
    }

    fn curve_factor(&self) -> f64 {
        match self.curve {
            Curve::Flat => 1.0,
            Curve::Power {
                reference_mm_s,
                exponent,
                min,
                max,
            } => {
                if !(reference_mm_s.is_finite() && reference_mm_s > 0.0) {
                    return 1.0;
                }
                let ratio = self.speed / reference_mm_s;
                let f = if ratio <= 0.0 {
                    min
                } else {
                    ratio.powf(exponent)
                };
                if f.is_finite() {
                    f.clamp(min.min(max), max.max(min))
                } else {
                    1.0
                }
            }
        }
    }
}

impl Stage for Sensitivity {
    fn name(&self) -> &'static str {
        "sensitivity"
    }

    fn process(&mut self, s: &mut Sample) {
        // Track speed only when a curve actually needs it, so `Flat` stays exactly
        // pass-through with multiplier 1.0.
        if self.curve != Curve::Flat {
            let instantaneous = s.magnitude() / s.dt;
            let tau = self.speed_smoothing_s;
            if tau > 0.0 && instantaneous.is_finite() {
                let alpha = 1.0 - (-s.dt / tau).exp();
                self.speed += alpha * (instantaneous - self.speed);
            } else if instantaneous.is_finite() {
                self.speed = instantaneous;
            }
        }

        let mut m = self.multiplier * self.curve_factor();
        if let Some(cap) = self.max_multiplier {
            if cap.is_finite() {
                m = m.min(cap);
            }
        }
        if !m.is_finite() {
            return;
        }

        s.dx *= m;
        s.dy *= m * if self.y_ratio.is_finite() { self.y_ratio } else { 1.0 };
    }

    fn reset(&mut self) {
        self.speed = 0.0;
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

    fn sample(dx: f64) -> Sample {
        let mut s = Sample::new(dx, 0.0, 1_000, true);
        s.dt = 0.001;
        s
    }

    #[test]
    fn identity_settings_are_exact_pass_through() {
        let mut st = Sensitivity::default();
        let before = sample(3.7);
        let mut after = before;
        st.process(&mut after);
        assert_eq!(before, after, "identity settings must not alter the sample");
    }

    #[test]
    fn flat_multiplier_scales_both_axes() {
        let mut st = Sensitivity::flat(0.5);
        let mut s = Sample::new(4.0, 8.0, 1_000, true);
        s.dt = 0.001;
        st.process(&mut s);
        assert!((s.dx - 2.0).abs() < 1e-12);
        assert!((s.dy - 4.0).abs() < 1e-12);
    }

    #[test]
    fn y_ratio_only_affects_y() {
        let mut st = Sensitivity {
            y_ratio: 2.0,
            ..Default::default()
        };
        let mut s = Sample::new(1.0, 1.0, 1_000, true);
        s.dt = 0.001;
        st.process(&mut s);
        assert!((s.dx - 1.0).abs() < 1e-12);
        assert!((s.dy - 2.0).abs() < 1e-12);
    }

    #[test]
    fn nonsense_parameters_leave_the_sample_finite() {
        for bad in [f64::NAN, f64::INFINITY, -f64::INFINITY] {
            let mut st = Sensitivity::flat(bad);
            let mut s = sample(1.0);
            st.process(&mut s);
            assert!(s.dx.is_finite(), "multiplier {bad} produced {}", s.dx);
        }
    }

    #[test]
    fn power_curve_is_bounded_by_its_clamps() {
        let mut st = Sensitivity {
            curve: Curve::Power {
                reference_mm_s: 100.0,
                exponent: 2.0,
                min: 0.5,
                max: 3.0,
            },
            ..Default::default()
        };
        // Drive it hard for a while, then check the factor never escaped.
        for i in 0..500 {
            let mut s = Sample::new(50.0, 0.0, i * 1_000, true);
            s.dt = 0.001;
            st.process(&mut s);
            assert!(s.dx.is_finite());
            assert!(s.dx.abs() <= 50.0 * 3.0 + 1e-9, "exceeded max clamp");
        }
    }
}
