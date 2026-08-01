//! `rotate` — angle offset applied to the motion vector.

use crate::sample::Sample;
use crate::stage::Stage;

/// Rotates motion by a fixed angle.
///
/// Included primarily as an **accessibility feature**, for users who cannot hold a mouse
/// square to the desk, and secondarily for artists working at an angle. Cheap to provide
/// and there is no substitute for it elsewhere in the pipeline.
#[derive(Debug, Clone)]
pub struct Rotate {
    enabled: bool,
    angle_deg: f64,
    sin: f64,
    cos: f64,
}

impl Default for Rotate {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl Rotate {
    pub fn new(angle_deg: f64) -> Self {
        let mut r = Self {
            enabled: true,
            angle_deg: 0.0,
            sin: 0.0,
            cos: 1.0,
        };
        r.set_angle(angle_deg);
        r
    }

    pub fn set_angle(&mut self, angle_deg: f64) {
        // Precomputed once rather than per sample: this runs on the hot path.
        self.angle_deg = if angle_deg.is_finite() { angle_deg } else { 0.0 };
        let rad = self.angle_deg.to_radians();
        self.sin = rad.sin();
        self.cos = rad.cos();
    }

    pub fn angle_deg(&self) -> f64 {
        self.angle_deg
    }
}

impl Stage for Rotate {
    fn name(&self) -> &'static str {
        "rotate"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }
        let (dx, dy) = (s.dx, s.dy);
        s.dx = dx * self.cos - dy * self.sin;
        s.dy = dx * self.sin + dy * self.cos;
        // `raw_*` is deliberately rotated too: the pressure stage's `cursor` speed source
        // reads it, and a speed magnitude must not depend on which stage rotated first.
        let (rx, ry) = (s.raw_dx, s.raw_dy);
        s.raw_dx = rx * self.cos - ry * self.sin;
        s.raw_dy = rx * self.sin + ry * self.cos;
    }

    fn reset(&mut self) {}

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

    #[test]
    fn zero_degrees_is_exact_pass_through() {
        let mut r = Rotate::new(0.0);
        let before = Sample::new(3.0, -4.0, 1_000, false);
        let mut after = before;
        r.process(&mut after);
        assert!((after.dx - 3.0).abs() < 1e-12);
        assert!((after.dy + 4.0).abs() < 1e-12);
    }

    #[test]
    fn ninety_degrees_maps_x_onto_y() {
        let mut r = Rotate::new(90.0);
        let mut s = Sample::new(1.0, 0.0, 1_000, false);
        r.process(&mut s);
        assert!(s.dx.abs() < 1e-12, "dx should vanish, got {}", s.dx);
        assert!((s.dy - 1.0).abs() < 1e-12, "dy should be 1, got {}", s.dy);
    }

    #[test]
    fn rotation_preserves_magnitude() {
        for angle in [0.0, 13.0, 45.0, 90.0, 180.0, -37.5, 359.0] {
            let mut r = Rotate::new(angle);
            let mut s = Sample::new(3.0, 4.0, 1_000, false);
            r.process(&mut s);
            assert!(
                (s.magnitude() - 5.0).abs() < 1e-9,
                "angle {angle} changed magnitude to {}",
                s.magnitude()
            );
        }
    }

    #[test]
    fn raw_is_rotated_alongside_so_speed_is_angle_independent() {
        let mut r = Rotate::new(37.0);
        let mut s = Sample::new(3.0, 4.0, 1_000, false);
        r.process(&mut s);
        assert!((s.raw_magnitude() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_non_finite_angle_falls_back_to_no_rotation() {
        for bad in [f64::NAN, f64::INFINITY] {
            let mut r = Rotate::new(bad);
            let mut s = Sample::new(1.0, 2.0, 1_000, false);
            r.process(&mut s);
            assert!((s.dx - 1.0).abs() < 1e-12 && (s.dy - 2.0).abs() < 1e-12);
        }
    }
}
