//! `normalize` — device counts to millimetres. Pinned first.

use crate::sample::Sample;
use crate::stage::Stage;

/// Converts raw device counts into millimetres.
///
/// **Pinned first in the pipeline.** Every downstream parameter is a physical
/// quantity (D10), so running anything before this leaves those parameters
/// meaningless.
///
/// This rewrites `raw_dx`/`raw_dy` as well as `dx`/`dy`, so that stages consulting
/// the pre-filter motion — the `pressure` stage's `cursor` speed source — also see
/// millimetres.
#[derive(Debug, Clone)]
pub struct Normalize {
    enabled: bool,
    mm_per_count: f64,
}

impl Normalize {
    pub fn new(dpi: f64) -> Self {
        Self {
            enabled: true,
            mm_per_count: mm_per_count(dpi),
        }
    }

    pub fn set_dpi(&mut self, dpi: f64) {
        self.mm_per_count = mm_per_count(dpi);
    }
}

fn mm_per_count(dpi: f64) -> f64 {
    if dpi.is_finite() && dpi > 0.0 {
        25.4 / dpi
    } else {
        // Unknown DPI is assumed to be 1000 and surfaced in the UI, per modules.md.
        25.4 / 1000.0
    }
}

impl Stage for Normalize {
    fn name(&self) -> &'static str {
        "normalize"
    }

    fn process(&mut self, s: &mut Sample) {
        s.dx *= self.mm_per_count;
        s.dy *= self.mm_per_count;
        s.raw_dx *= self.mm_per_count;
        s.raw_dy *= self.mm_per_count;
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
    fn one_inch_of_counts_is_one_inch_of_millimetres() {
        let mut n = Normalize::new(1000.0);
        let mut s = Sample::new(1000.0, 0.0, 0, false);
        n.process(&mut s);
        assert!((s.dx - 25.4).abs() < 1e-9);
    }

    #[test]
    fn raw_is_normalised_too() {
        let mut n = Normalize::new(1600.0);
        let mut s = Sample::new(160.0, 0.0, 0, false);
        n.process(&mut s);
        assert!((s.dx - s.raw_dx).abs() < 1e-12, "raw must track dx here");
        assert!(s.raw_dx > 0.0);
    }

    #[test]
    fn invalid_dpi_does_not_produce_nonsense() {
        for bad in [0.0, -1.0, f64::NAN] {
            let mut n = Normalize::new(bad);
            let mut s = Sample::new(1000.0, 0.0, 0, false);
            n.process(&mut s);
            assert!(s.dx.is_finite() && s.dx > 0.0, "dpi {bad}");
        }
    }
}
