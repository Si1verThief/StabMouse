//! Millimetres back to integer device counts, without losing motion.

/// Converts continuous millimetre motion into the integer counts a virtual device
/// must emit, carrying the fractional remainder between calls.
///
/// **Omitting the carry silently loses slow motion.** With any multiplier below 1.0,
/// truncation alone discards every sub-count movement, so a slowly-moved mouse simply
/// does not move the cursor. It is the most common bug in this class of software,
/// which is why this is not optional and has no toggle (see stages.md).
#[derive(Debug, Clone)]
pub struct Quantizer {
    counts_per_mm: f64,
    carry_x: f64,
    carry_y: f64,
}

impl Quantizer {
    /// `dpi` is the resolution of the *output* device.
    pub fn new(dpi: f64) -> Self {
        Self {
            counts_per_mm: sanitise_dpi(dpi) / 25.4,
            carry_x: 0.0,
            carry_y: 0.0,
        }
    }

    pub fn set_dpi(&mut self, dpi: f64) {
        self.counts_per_mm = sanitise_dpi(dpi) / 25.4;
    }

    /// The scale itself, for callers that advance a continuous position instead of
    /// quantizing deltas — counts and pixels are 1:1 on StabMouse's own devices.
    pub fn counts_per_mm(&self) -> f64 {
        self.counts_per_mm
    }

    /// Drop the pending remainder. Used on discontinuity, where the carried fraction
    /// belongs to motion that is no longer relevant.
    pub fn reset(&mut self) {
        self.carry_x = 0.0;
        self.carry_y = 0.0;
    }

    /// Returns integer counts, banking the remainder for the next call.
    pub fn quantize(&mut self, dx_mm: f64, dy_mm: f64) -> (i32, i32) {
        (
            Self::axis(dx_mm, self.counts_per_mm, &mut self.carry_x),
            Self::axis(dy_mm, self.counts_per_mm, &mut self.carry_y),
        )
    }

    /// Quantize at a caller-supplied scale, sharing the same subpixel carry.
    ///
    /// Used when a tablet mode falls back to the pointer: the scale is then the tablet's
    /// pixels-per-millimetre rather than the source device's counts-per-millimetre, so the
    /// sensitivity does not change with the transport.
    ///
    /// The carry is deliberately the *same* accumulator. Motion conservation is the point of
    /// it, and a second carry would drop the remainder every time the transport changed —
    /// exactly the accumulation hazard recorded in modules.md.
    pub fn quantize_at(&mut self, dx_mm: f64, dy_mm: f64, scale: f64) -> (i32, i32) {
        if !scale.is_finite() || scale <= 0.0 {
            return self.quantize(dx_mm, dy_mm);
        }
        (
            Self::axis(dx_mm, scale, &mut self.carry_x),
            Self::axis(dy_mm, scale, &mut self.carry_y),
        )
    }

    fn axis(delta_mm: f64, counts_per_mm: f64, carry: &mut f64) -> i32 {
        // Non-finite input must not poison the accumulator or panic on cast.
        if !delta_mm.is_finite() {
            return 0;
        }

        let wanted = delta_mm * counts_per_mm + *carry;
        if !wanted.is_finite() {
            *carry = 0.0;
            return 0;
        }

        let whole = wanted.trunc();
        *carry = wanted - whole;

        // Saturating cast: an absurd delta must clamp rather than wrap or panic.
        if whole >= i32::MAX as f64 {
            *carry = 0.0;
            i32::MAX
        } else if whole <= i32::MIN as f64 {
            *carry = 0.0;
            i32::MIN
        } else {
            whole as i32
        }
    }
}

fn sanitise_dpi(dpi: f64) -> f64 {
    if dpi.is_finite() && dpi > 0.0 {
        dpi
    } else {
        // docs/modules.md: an unknown DPI is assumed to be 1000 and surfaced in the
        // UI. Here we only need to avoid producing a nonsense scale.
        1000.0
    }
}

#[cfg(test)]
mod tests_scale {
    use super::*;

    #[test]
    fn an_explicit_scale_is_used_instead_of_the_device_one() {
        let mut q = Quantizer::new(1600.0);
        // 10mm at 9.6 px/mm is 96 pixels, regardless of the source DPI.
        let (dx, _) = q.quantize_at(10.0, 0.0, 9.6);
        assert_eq!(dx, 96);
    }

    #[test]
    fn a_bad_scale_falls_back_rather_than_emitting_nothing() {
        let mut q = Quantizer::new(1600.0);
        let (a, _) = q.quantize_at(5.0, 0.0, f64::NAN);
        let mut q2 = Quantizer::new(1600.0);
        let (b, _) = q2.quantize(5.0, 0.0);
        assert_eq!(a, b, "an unusable scale must not silently stop motion");
        assert_eq!(q.quantize_at(1.0, 0.0, 0.0).0, q2.quantize(1.0, 0.0).0);
    }

    #[test]
    fn the_carry_is_shared_across_scales() {
        // The accumulation hazard: a separate carry per scale would drop the remainder every
        // time the transport changed.
        let mut q = Quantizer::new(1600.0);
        let mut total = 0;
        for _ in 0..10 {
            total += q.quantize_at(0.05, 0.0, 9.6).0;
        }
        // 0.5mm at 9.6 px/mm is 4.8 pixels; conserved motion means 4, not 0.
        assert_eq!(total, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conserves_motion_across_many_small_deltas() {
        let mut q = Quantizer::new(1000.0);
        let counts_per_mm = 1000.0 / 25.4;

        // Motion far below one count per sample: without the carry this yields zero.
        let step = 0.2 / counts_per_mm;
        let n = 10_000;

        let mut total = 0i64;
        for _ in 0..n {
            let (dx, _) = q.quantize(step, 0.0);
            total += i64::from(dx);
        }

        let expected = (step * counts_per_mm * n as f64).round() as i64;
        assert!(
            (total - expected).abs() <= 1,
            "expected ~{expected} counts, got {total}"
        );
    }

    #[test]
    fn sub_count_motion_is_not_discarded() {
        let mut q = Quantizer::new(1000.0);
        let tiny = 0.001; // mm — well under one count
        let mut moved = 0i64;
        for _ in 0..1000 {
            let (dx, _) = q.quantize(tiny, 0.0);
            moved += i64::from(dx);
        }
        assert!(moved > 0, "slow motion was lost entirely");
    }

    #[test]
    fn non_finite_input_cannot_panic_or_poison() {
        let mut q = Quantizer::new(1000.0);
        assert_eq!(q.quantize(f64::NAN, f64::INFINITY), (0, 0));
        assert_eq!(q.quantize(f64::NEG_INFINITY, f64::NAN), (0, 0));
        // Still functional afterwards.
        let (dx, _) = q.quantize(25.4, 0.0);
        assert_eq!(dx, 1000);
    }

    #[test]
    fn absurd_delta_saturates_rather_than_wrapping() {
        let mut q = Quantizer::new(1000.0);
        let (dx, dy) = q.quantize(1.0e30, -1.0e30);
        assert_eq!(dx, i32::MAX);
        assert_eq!(dy, i32::MIN);
    }

    #[test]
    fn invalid_dpi_falls_back_instead_of_dividing_by_zero() {
        for bad in [0.0, -100.0, f64::NAN, f64::INFINITY] {
            let mut q = Quantizer::new(bad);
            let (dx, _) = q.quantize(25.4, 0.0);
            assert_eq!(dx, 1000, "dpi {bad} should fall back to 1000");
        }
    }
}
