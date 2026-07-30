//! The ordered stage list, and the timing sanitisation every stage relies on.

use crate::sample::Sample;
use crate::stage::Stage;

/// Floor for `dt`, in seconds. Guards every division by elapsed time without needing
/// each stage to defend itself.
///
/// Set to a *physically possible* interval rather than a bare epsilon. 100us is 10kHz,
/// above any real input device, so a genuine sample can never be clamped — while a
/// duplicate or near-duplicate timestamp inflates derived rates by at most ~10x instead
/// of the ~100x an epsilon floor allowed.
const MIN_DT: f64 = 1.0e-4;

/// Ceiling for `dt`. Longer gaps are reported as a discontinuity instead.
const MAX_DT: f64 = 0.1;

/// A gap beyond this means filter state is stale — suspend/resume, or the device
/// went away and came back.
const DISCONTINUITY_S: f64 = 0.25;

/// An ordered list of stages.
///
/// Allocation happens here, at construction. `process` never allocates.
pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
    last_t_us: Option<u64>,
    was_down: bool,
}

impl Pipeline {
    pub fn new(stages: Vec<Box<dyn Stage>>) -> Self {
        Self {
            stages,
            last_t_us: None,
            was_down: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn stage_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.stages.iter().map(|s| s.name())
    }

    /// True when no stage holds un-emitted state.
    ///
    /// After a stroke ends, the stabiliser still holds its accumulated lag and the
    /// pressure envelope has yet to ramp down. Neither can finish without further
    /// samples, so the consumer must feed zero-motion ticks until this returns true.
    /// See the tick requirement in docs/modules.md.
    pub fn settled(&self) -> bool {
        self.stages.iter().all(|s| !s.enabled() || s.settled())
    }

    /// Clear all filter state. Called on mode switch and after a discontinuity.
    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
        self.last_t_us = None;
        self.was_down = false;
    }

    /// Run one sample through every enabled stage, in order.
    ///
    /// `sample.dt`, `stroke_start`, `stroke_end` and `discontinuity` are derived here
    /// so that stages never have to, and so the sanitisation is applied once rather
    /// than being reimplemented inconsistently.
    pub fn process(&mut self, sample: &mut Sample) {
        let (dt, discontinuity) = self.timing(sample.t_us);
        sample.dt = dt;
        sample.discontinuity = discontinuity;

        sample.stroke_start = sample.down && !self.was_down;
        sample.stroke_end = !sample.down && self.was_down;
        self.was_down = sample.down;

        if discontinuity {
            for stage in &mut self.stages {
                stage.reset();
            }
        }

        for stage in &mut self.stages {
            if stage.enabled() {
                stage.process(sample);
            }
        }
    }

    /// Derive a usable `dt` from two source timestamps.
    ///
    /// Handles the three pathological cases the acceptance criteria require:
    /// duplicate timestamps (dt would be zero), time moving backwards (a monotonic
    /// clock can still appear to go backwards across a device reconnect), and
    /// multi-hour gaps from suspend.
    fn timing(&mut self, t_us: u64) -> (f64, bool) {
        let previous = self.last_t_us.replace(t_us);

        let Some(previous) = previous else {
            // First sample: no interval exists yet. Treat as a discontinuity so
            // stages start from a clean state.
            return (MIN_DT, true);
        };

        if t_us < previous {
            // Time went backwards. Nothing sensible can be computed from this
            // interval, so start over rather than producing a negative or huge dt.
            return (MIN_DT, true);
        }

        let raw = (t_us - previous) as f64 * 1.0e-6;

        if raw > DISCONTINUITY_S {
            (MAX_DT, true)
        } else {
            (raw.clamp(MIN_DT, MAX_DT), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(times: &[u64]) -> Vec<(f64, bool)> {
        let mut p = Pipeline::new(vec![]);
        let mut out = Vec::new();
        for &t in times {
            let mut s = Sample::new(1.0, 0.0, t, false);
            p.process(&mut s);
            out.push((s.dt, s.discontinuity));
        }
        out
    }

    /// Counts resets so discontinuity propagation can be asserted.
    struct ResetSpy {
        resets: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Stage for ResetSpy {
        fn name(&self) -> &'static str {
            "reset-spy"
        }
        fn process(&mut self, _s: &mut Sample) {}
        fn reset(&mut self) {
            self.resets.set(self.resets.get() + 1);
        }
    }

    #[test]
    fn first_sample_is_a_discontinuity() {
        let out = run(&[1_000]);
        assert!(out[0].1, "first sample must report a discontinuity");
        assert_eq!(out[0].0, MIN_DT);
    }

    #[test]
    fn duplicate_timestamps_never_yield_zero_dt() {
        let out = run(&[1_000, 1_000, 1_000]);
        for (dt, _) in out {
            assert!(dt >= MIN_DT, "dt must never be below MIN_DT, got {dt}");
        }
    }

    #[test]
    fn time_going_backwards_is_a_discontinuity() {
        let out = run(&[10_000, 5_000]);
        assert!(out[1].1, "backwards time must report a discontinuity");
        assert!(out[1].0 > 0.0);
    }

    #[test]
    fn long_gap_is_a_discontinuity_and_dt_is_clamped() {
        // Ten seconds, as if resuming from suspend.
        let out = run(&[1_000, 10_001_000]);
        assert!(out[1].1);
        assert_eq!(out[1].0, MAX_DT);
    }

    #[test]
    fn normal_interval_passes_through() {
        // 1kHz.
        let out = run(&[1_000, 2_000, 3_000]);
        assert!(!out[1].1);
        assert!((out[1].0 - 0.001).abs() < 1e-12);
    }

    #[test]
    fn discontinuity_resets_every_stage() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0));
        let spy = Box::new(ResetSpy {
            resets: counter.clone(),
        });
        let mut p = Pipeline::new(vec![spy]);

        // First sample counts as a discontinuity.
        let mut s = Sample::new(0.0, 0.0, 1_000, false);
        p.process(&mut s);
        assert_eq!(counter.get(), 1);

        // Normal interval: no reset.
        let mut s = Sample::new(0.0, 0.0, 2_000, false);
        p.process(&mut s);
        assert_eq!(counter.get(), 1);

        // Ten-second gap: reset.
        let mut s = Sample::new(0.0, 0.0, 10_002_000, false);
        p.process(&mut s);
        assert_eq!(counter.get(), 2);
    }

    #[test]
    fn stroke_edges_are_reported_once() {
        let mut p = Pipeline::new(vec![]);
        let states = [false, true, true, false, false];
        let mut starts = 0;
        let mut ends = 0;
        for (i, &down) in states.iter().enumerate() {
            let mut s = Sample::new(0.0, 0.0, (i as u64 + 1) * 1_000, down);
            p.process(&mut s);
            starts += usize::from(s.stroke_start);
            ends += usize::from(s.stroke_end);
        }
        assert_eq!(starts, 1);
        assert_eq!(ends, 1);
    }
}
