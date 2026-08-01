//! StabMouse filter core.
//!
//! Pure computation: no I/O, no clock, no platform dependency. The same code runs in
//! the daemon, in the offline research harness via PyO3, and in the GUI preview via
//! WASM — which is what stops the prototype and the product from drifting apart.
//!
//! # Contract
//!
//! From docs/modules.md, and enforced by tests:
//!
//! - **Deterministic.** Identical input and parameters produce identical output.
//! - **No clock.** Time arrives on the sample, taken from the source event. Any
//!   `Instant::now()` in here would make replay diverge from live.
//! - **No allocation** after construction.
//! - **Motion is conserved** — the subpixel remainder is carried, so slow movement is
//!   never silently discarded.
//! - **Identity settings are pass-through.**
//! - **Cannot panic** on any input or parameter combination, including non-finite
//!   values, duplicate timestamps, time moving backwards, and multi-hour gaps.
//!
//! # Units
//!
//! Millimetres and seconds. `Sample::dx`/`dy` carry device counts only until
//! `normalize` runs, and millimetres thereafter. See D10 for why physical units:
//! preset sharing is a first-class feature, and a shared preset must mean the same
//! thing on someone else's hardware.

#![forbid(unsafe_code)]

mod pipeline;
mod quantize;
mod sample;
mod stage;
pub mod stages;

pub use pipeline::Pipeline;
pub use quantize::Quantizer;
pub use sample::{Sample, MAX_GESTURES};
pub use stage::Stage;

#[cfg(test)]
mod integration {
    use super::stages::*;
    use super::*;

    /// A pipeline resembling a real drawing preset.
    fn drawing_pipeline() -> Pipeline {
        Pipeline::new(vec![
            Box::new(Normalize::new(1600.0)),
            Box::new(Stabilize::new(0.5, 0.35)),
            Box::new(Sensitivity::flat(1.0)),
            Box::new(Pressure::default()),
        ])
    }

    /// Deterministic pseudo-random motion. Not `rand`, because the core crate stays
    /// dependency-free and tests must be reproducible.
    fn motion(seed: u64, n: usize) -> Vec<(f64, f64, bool)> {
        let mut state = seed;
        (0..n)
            .map(|i| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let dx = ((state >> 33) % 21) as f64 - 10.0;
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let dy = ((state >> 33) % 21) as f64 - 10.0;
                (dx, dy, i % 400 < 300)
            })
            .collect()
    }

    fn run(pipeline: &mut Pipeline, input: &[(f64, f64, bool)]) -> Vec<(f64, f64, f64)> {
        input
            .iter()
            .enumerate()
            .map(|(i, &(dx, dy, down))| {
                let mut s = Sample::new(dx, dy, (i as u64 + 1) * 1_000, down);
                pipeline.process(&mut s);
                (s.dx, s.dy, s.pressure.unwrap_or(0.0))
            })
            .collect()
    }

    #[test]
    fn identical_input_produces_identical_output() {
        let input = motion(0xC0FFEE, 5_000);
        let a = run(&mut drawing_pipeline(), &input);
        let b = run(&mut drawing_pipeline(), &input);
        assert_eq!(a, b, "pipeline must be bit-for-bit deterministic");
    }

    #[test]
    fn reset_restores_a_pipeline_to_its_initial_behaviour() {
        let input = motion(0xBEEF, 2_000);
        let mut p = drawing_pipeline();

        let first = run(&mut p, &input);
        p.reset();
        let second = run(&mut p, &input);

        assert_eq!(
            first, second,
            "reset must fully clear state, or mode switching is not reproducible"
        );
    }

    #[test]
    fn output_never_contains_non_finite_values() {
        let mut p = drawing_pipeline();
        let nasty = [
            (0.0, 0.0, true),
            (f64::NAN, 1.0, true),
            (1.0, f64::INFINITY, true),
            (1e300, -1e300, true),
            (f64::NEG_INFINITY, f64::NAN, false),
            (1.0, 1.0, true),
        ];
        for (i, &(dx, dy, down)) in nasty.iter().enumerate() {
            let mut s = Sample::new(dx, dy, (i as u64 + 1) * 1_000, down);
            p.process(&mut s);
            assert!(s.dx.is_finite(), "dx became {}", s.dx);
            assert!(s.dy.is_finite(), "dy became {}", s.dy);
            if let Some(pr) = s.pressure {
                assert!((0.0..=1.0).contains(&pr), "pressure became {pr}");
            }
        }
    }

    #[test]
    fn pathological_timestamps_are_survivable() {
        let mut p = drawing_pipeline();
        // Duplicate, backwards, far future, then normal.
        for t in [1_000u64, 1_000, 500, u64::MAX / 2, 2_000, 3_000] {
            let mut s = Sample::new(1.0, 1.0, t, true);
            p.process(&mut s);
            assert!(s.dx.is_finite() && s.dy.is_finite());
        }
    }

    #[test]
    fn end_to_end_conserves_motion_through_the_quantizer() {
        // No stabiliser lag and unit sensitivity: everything in must come out.
        let mut p = Pipeline::new(vec![
            Box::new(Normalize::new(1600.0)),
            Box::new(Sensitivity::flat(0.37)),
        ]);
        let mut q = Quantizer::new(1600.0);

        let mut emitted = 0i64;
        let count = 20_000;
        for i in 0..count {
            let mut s = Sample::new(1.0, 0.0, (i as u64 + 1) * 1_000, false);
            p.process(&mut s);
            let (dx, _) = q.quantize(s.dx, s.dy);
            emitted += i64::from(dx);
        }

        let expected = (count as f64 * 0.37).round() as i64;
        assert!(
            (emitted - expected).abs() <= 1,
            "expected ~{expected} counts out, got {emitted}"
        );
    }

    /// Regression guard for the accumulation hazard documented in modules.md.
    ///
    /// Three separate bugs have come from thresholding a per-sample value when the
    /// quantity accumulates. This drives the whole pipeline with motion so slow that
    /// no individual sample could clear any plausible threshold, and asserts nothing
    /// is lost. Fast motion passes trivially, which is exactly why these bugs survive
    /// casual testing.
    #[test]
    fn motion_too_slow_to_clear_any_per_sample_threshold_is_not_lost() {
        let mut p = Pipeline::new(vec![
            Box::new(Normalize::new(1600.0)),
            // A deliberately punishing stabiliser: large leash, very low catch-up.
            // This is the configuration that exposed the canvas ink-gating bug.
            Box::new(Stabilize::new(6.0, 0.02)),
            Box::new(Sensitivity::flat(1.0)),
            Box::new(Pressure::default()),
        ]);
        let mut q = Quantizer::new(1600.0);

        // One count per sample at 1600 dpi is ~0.016mm — far below any threshold.
        let moving = 40_000;
        let settling = 20_000;
        let mut emitted = 0i64;

        for i in 0..moving {
            let mut s = Sample::new(1.0, 0.0, (i as u64 + 1) * 1_000, true);
            p.process(&mut s);
            let (dx, _) = q.quantize(s.dx, s.dy);
            emitted += i64::from(dx);
        }

        // Release and keep ticking. The stabiliser holds a permanent leash-length of lag
        // by design, so the invariant is not "everything came out" but "nothing was lost
        // beyond the leash".
        let mut t = moving as u64;
        for _ in 0..settling {
            t += 1;
            let mut s = Sample::new(0.0, 0.0, t * 1_000, false);
            p.process(&mut s);
            let (dx, _) = q.quantize(s.dx, s.dy);
            emitted += i64::from(dx);
        }
        assert!(p.settled());

        // 6mm of leash at 1600 dpi is ~378 counts. A low catch-up trails further behind
        // during continuous motion, so allow generous headroom -- the point is that the
        // shortfall is bounded by the leash rather than growing with distance travelled.
        let leash_counts = (6.0 / 25.4 * 1600.0) as i64;
        let shortfall = moving as i64 - emitted;
        assert!(
            shortfall >= 0 && shortfall <= leash_counts * 2,
            "put in {moving} counts, got {emitted} out: a shortfall of {shortfall} exceeds \
             twice the {leash_counts}-count leash, so motion is being lost rather than lagged"
        );
    }

    #[test]
    fn a_fully_identity_pipeline_is_transparent() {
        // Explicitly transparent settings, not `Default`: the defaults now carry the
        // measured recommendations, which smooth and therefore lag on purpose.
        let mut p = Pipeline::new(vec![
            Box::new(Sensitivity::flat(1.0)),
            Box::new(Stabilize::new(0.0, 1.0)),
            Box::new(Smooth::new(1000.0, 0.0, 1.0)),
        ]);

        let mut total = 0.0;
        for i in 0..500 {
            let mut s = Sample::new(1.0, 0.0, (i as u64 + 1) * 1_000, false);
            p.process(&mut s);
            total += s.dx;
        }
        assert!(
            (total - 500.0).abs() < 1.0,
            "identity pipeline lost motion: {total}"
        );
    }
}
