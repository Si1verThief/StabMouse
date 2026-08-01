//! `average` — weighted moving average over a time window.

use crate::sample::Sample;
use crate::stage::Stage;
use std::collections::VecDeque;

/// How much a sample counts for as it ages out of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    /// Falls off straight to nothing at the window edge. The classic WMA, and the most
    /// predictable to tune against — halving the window halves the lag.
    Linear,
    /// Falls off fast at first and lingers, so recent motion dominates. Feels the closest
    /// to `smooth` while still being a true average.
    Exponential,
    /// Falls off slowly near the newest sample, then quickly. The roundest of the three,
    /// and the one that most suppresses a single jittery sample.
    Gaussian,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    x: f64,
    y: f64,
    t_us: u64,
}

/// Averages *position* over the last `window_ms`, and emits the change in that average.
///
/// # Milliseconds, not samples
///
/// A window counted in samples means different things on different hardware: 16 samples is
/// 128ms at 125Hz and 2ms at 8000Hz, so a shared preset would feel like a different filter on
/// someone else's mouse. Time is the portable unit (D10's reasoning, applied to the time axis
/// instead of the distance one).
///
/// # Cost, and why the buffer is bounded
///
/// The weights depend on each sample's age, so the average cannot be maintained incrementally
/// the way a fixed-weight sum could — every sample in the window is visited per report. That is
/// tens of entries at a normal 1000Hz and a 50ms window. The buffer is sized for the window at
/// a high report rate and hard-capped, so a very long window on a very fast mouse loses the
/// oldest samples rather than growing without limit — and the contract forbidding allocation in
/// `process` is kept structurally, not by hoping the window stays small.
#[derive(Debug, Clone)]
pub struct Average {
    enabled: bool,
    pub window_ms: f64,
    pub weighting: Weighting,

    /// Bound on `history.len()`, so pushing can never reallocate.
    capacity: usize,
    history: VecDeque<Entry>,
    position_x: f64,
    position_y: f64,
    last_out_x: f64,
    last_out_y: f64,
    primed: bool,
}

/// Report rate the buffer is sized against. Well above the 1000Hz of ordinary mice and at the
/// top of what gaming mice claim, so the window is honoured in full on any real hardware.
const ASSUMED_MAX_HZ: f64 = 8000.0;

/// Hard ceiling on buffered samples, so an absurd window cannot cost unbounded memory or time.
const MAX_ENTRIES: usize = 4096;

impl Default for Average {
    fn default() -> Self {
        // Identity window, so a default-constructed stage changes nothing. The weighting is
        // the measured best of the three — see the table in stages.md.
        Self::new(0.0, Weighting::Exponential)
    }
}

impl Average {
    pub fn new(window_ms: f64, weighting: Weighting) -> Self {
        let capacity = Self::capacity_for(window_ms);
        Self {
            enabled: true,
            window_ms,
            weighting,
            capacity,
            history: VecDeque::with_capacity(capacity),
            position_x: 0.0,
            position_y: 0.0,
            last_out_x: 0.0,
            last_out_y: 0.0,
            primed: false,
        }
    }

    fn capacity_for(window_ms: f64) -> usize {
        if !window_ms.is_finite() || window_ms <= 0.0 {
            return 1;
        }
        let wanted = (window_ms / 1000.0 * ASSUMED_MAX_HZ).ceil();
        // The `as` cast saturates, but the clamp makes the intent explicit rather than relying
        // on that; +1 so the window's own edge sample fits.
        (wanted.clamp(1.0, MAX_ENTRIES as f64) as usize).saturating_add(1)
    }

    /// Whether the stage is configured to do anything. Identity must be pass-through.
    fn active(&self) -> bool {
        self.window_ms.is_finite() && self.window_ms > 0.0
    }

    /// Weight for a sample of the given age, in `0.0..=1.0`.
    fn weight(&self, age_ms: f64) -> f64 {
        let t = (age_ms / self.window_ms).clamp(0.0, 1.0);
        match self.weighting {
            Weighting::Linear => 1.0 - t,
            // -3 at the edge, so the oldest sample still counts for ~5% rather than nothing:
            // an exponential that reached zero would just be a linear window with extra steps.
            Weighting::Exponential => (-3.0 * t).exp(),
            // sigma = window/3, so the edge is three sigma out and contributes ~1%.
            Weighting::Gaussian => (-4.5 * t * t).exp(),
        }
    }

    fn clear(&mut self) {
        self.history.clear();
        self.position_x = 0.0;
        self.position_y = 0.0;
        self.last_out_x = 0.0;
        self.last_out_y = 0.0;
        self.primed = false;
    }
}

impl Stage for Average {
    fn name(&self) -> &'static str {
        "average"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }
        // Identity is byte-identical pass-through, per the Stage contract — not a window of
        // one, which would still round-trip the value through the arithmetic below.
        if !self.active() {
            return;
        }
        if s.discontinuity {
            self.clear();
        }
        if !self.primed {
            self.primed = true;
        }

        self.position_x += s.dx;
        self.position_y += s.dy;

        if self.history.len() >= self.capacity {
            // Full only when the window is longer than the buffer can hold at this report
            // rate. Dropping the oldest shortens the effective window, which is a graceful
            // degradation; growing would allocate on the hot path, which is forbidden.
            self.history.pop_front();
        }
        self.history.push_back(Entry {
            x: self.position_x,
            y: self.position_y,
            t_us: s.t_us,
        });

        // Age out anything past the window. `saturating_sub` because a source whose timestamps
        // step backwards must not produce an enormous age and empty the buffer.
        let window_us = (self.window_ms * 1000.0) as u64;
        while let Some(front) = self.history.front() {
            if s.t_us.saturating_sub(front.t_us) > window_us && self.history.len() > 1 {
                self.history.pop_front();
            } else {
                break;
            }
        }

        let mut sum_w = 0.0;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        for e in &self.history {
            let age_ms = s.t_us.saturating_sub(e.t_us) as f64 / 1000.0;
            let w = self.weight(age_ms);
            sum_w += w;
            sum_x += w * e.x;
            sum_y += w * e.y;
        }

        // Every weight can legitimately reach zero — a linear window whose only entry sits
        // exactly on the edge — and dividing by that would poison the position with NaN.
        let (avg_x, avg_y) = if sum_w > 0.0 {
            (sum_x / sum_w, sum_y / sum_w)
        } else {
            (self.position_x, self.position_y)
        };

        s.dx = avg_x - self.last_out_x;
        s.dy = avg_y - self.last_out_y;
        self.last_out_x = avg_x;
        self.last_out_y = avg_y;
    }

    fn reset(&mut self) {
        self.clear();
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn settled(&self) -> bool {
        // The average lags the true position, so after the hand stops there is still motion
        // owed to the output — it arrives as old samples age out of the window. Reporting
        // settled here would end the daemon's settle phase early and leave the stroke short of
        // where the hand actually finished.
        if !self.active() || !self.primed {
            return true;
        }
        const CLOSE_ENOUGH_MM: f64 = 0.001;
        (self.position_x - self.last_out_x).abs() < CLOSE_ENOUGH_MM
            && (self.position_y - self.last_out_y).abs() < CLOSE_ENOUGH_MM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples at a steady 1kHz.
    fn feed(stage: &mut Average, steps: &[(f64, f64)]) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for (i, (dx, dy)) in steps.iter().enumerate() {
            let mut s = Sample::new(*dx, *dy, (i as u64 + 1) * 1000, true);
            s.dt = 0.001;
            stage.process(&mut s);
            out.push((s.dx, s.dy));
        }
        out
    }

    #[test]
    fn a_zero_window_is_byte_identical_pass_through() {
        let mut stage = Average::new(0.0, Weighting::Linear);
        let mut s = Sample::new(1.25, -0.75, 1000, true);
        let before = s;
        stage.process(&mut s);
        assert_eq!(s, before, "identity settings must not touch the sample");
    }

    #[test]
    fn motion_is_conserved_across_a_stroke() {
        // The averaged path may lag, but it must arrive: a filter that loses total distance
        // makes every stroke short of where the hand went.
        let mut stage = Average::new(20.0, Weighting::Linear);
        let steps: Vec<(f64, f64)> = (0..400).map(|_| (0.1, 0.05)).collect();
        let out = feed(&mut stage, &steps);
        let sum_x: f64 = out.iter().map(|o| o.0).sum();
        let sum_y: f64 = out.iter().map(|o| o.1).sum();
        // Within one window's worth of lag at the end of the run.
        assert!((sum_x - 40.0).abs() < 2.0, "x total drifted: {sum_x}");
        assert!((sum_y - 20.0).abs() < 1.0, "y total drifted: {sum_y}");
    }

    #[test]
    fn a_single_jittery_sample_is_damped() {
        let mut stage = Average::new(20.0, Weighting::Gaussian);
        let mut steps: Vec<(f64, f64)> = (0..40).map(|_| (0.1, 0.0)).collect();
        steps[20] = (5.0, 0.0);
        let out = feed(&mut stage, &steps);
        assert!(
            out[20].0 < 1.0,
            "a spike must be spread across the window, got {}",
            out[20].0
        );
    }

    #[test]
    fn every_weighting_prefers_the_newest_sample() {
        for w in [Weighting::Linear, Weighting::Exponential, Weighting::Gaussian] {
            let stage = Average::new(10.0, w);
            assert!(
                stage.weight(0.0) > stage.weight(5.0),
                "{w:?} must weight recent motion more heavily"
            );
            assert!(stage.weight(5.0) > stage.weight(10.0), "{w:?} must fall off");
            assert!((0.0..=1.0).contains(&stage.weight(0.0)), "{w:?} out of range");
        }
    }

    #[test]
    fn the_buffer_never_grows_past_its_capacity() {
        // The no-allocation contract, checked structurally rather than trusted.
        let mut stage = Average::new(1000.0, Weighting::Linear);
        let cap = stage.history.capacity();
        let steps: Vec<(f64, f64)> = (0..5000).map(|_| (0.01, 0.0)).collect();
        feed(&mut stage, &steps);
        assert!(stage.history.len() <= stage.capacity);
        assert_eq!(stage.history.capacity(), cap, "process must not reallocate");
    }

    #[test]
    fn it_survives_time_moving_backwards() {
        let mut stage = Average::new(20.0, Weighting::Linear);
        let mut a = Sample::new(1.0, 1.0, 100_000, true);
        a.dt = 0.001;
        stage.process(&mut a);
        // A source whose clock steps back must not empty the window or produce NaN.
        let mut b = Sample::new(1.0, 1.0, 1_000, true);
        b.dt = 0.001;
        stage.process(&mut b);
        assert!(b.dx.is_finite() && b.dy.is_finite());
    }

    #[test]
    fn a_discontinuity_drops_stale_state() {
        let mut stage = Average::new(20.0, Weighting::Linear);
        feed(&mut stage, &[(1.0, 0.0); 10]);
        let mut s = Sample::new(0.0, 0.0, 9_000_000, true);
        s.dt = 0.001;
        s.discontinuity = true;
        stage.process(&mut s);
        assert!(stage.history.len() <= 1, "a resumed device must not average across the gap");
    }

    #[test]
    fn it_reports_unsettled_while_it_still_owes_motion() {
        let mut stage = Average::new(20.0, Weighting::Linear);
        feed(&mut stage, &[(1.0, 0.0); 10]);
        assert!(!stage.settled(), "lag not yet delivered must keep the settle phase alive");

        // Zero-motion ticks are how the daemon flushes it.
        let mut t = 11_000;
        for _ in 0..200 {
            let mut s = Sample::new(0.0, 0.0, t, false);
            s.dt = 0.001;
            stage.process(&mut s);
            t += 1000;
        }
        assert!(stage.settled(), "it must converge once the input stops");
    }

    #[test]
    fn nothing_panics_on_pathological_input() {
        for window in [0.0, -5.0, f64::NAN, f64::INFINITY, 1e12] {
            let mut stage = Average::new(window, Weighting::Exponential);
            for (dx, dy) in [(0.0, 0.0), (f64::NAN, 1.0), (1e300, -1e300), (0.1, 0.1)] {
                let mut s = Sample::new(dx, dy, 1000, true);
                s.dt = 0.001;
                stage.process(&mut s);
            }
        }
    }
}
