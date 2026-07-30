//! `pressure` — synthesised pen pressure. Pinned last.

use crate::sample::Sample;
use crate::stage::Stage;

/// Which motion the speed term reads.
///
/// Unresolved by reasoning; both ship and the default follows feel testing. See
/// stages.md.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeedSource {
    /// Velocity of the drawn point. Physically consistent with where ink lands, but
    /// the stabiliser's anchor only advances radially, so tangential hand motion
    /// registers as slow even at full speed.
    Output,
    /// Velocity of the raw input. Represents hand *intent* and has no radial
    /// artefact, at the cost of leading the ink slightly.
    Cursor,
}

/// What to do when the measured point stops moving.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StallBehaviour {
    /// Freeze the velocity estimate. A fast → stalled → fast sequence collapses into
    /// one slightly-slower sample that smoothing absorbs.
    Hold,
    /// Let velocity fall toward zero, producing pressure hotspots. Someone may want
    /// this, so it is not removed.
    Decay,
}

/// Synthesises pressure from a time envelope and a speed term.
///
/// **Pinned last**, because it needs settled motion — and that pin is load-bearing
/// rather than tidiness. See stages.md for why.
#[derive(Debug, Clone)]
pub struct Pressure {
    enabled: bool,

    pub attack_s: f64,
    pub release_s: f64,
    pub envelope_enabled: bool,

    pub speed_enabled: bool,
    pub v_max_mm_s: f64,
    pub gamma: f64,
    pub source: SpeedSource,
    pub velocity_smoothing_s: f64,

    pub stall_threshold_mm: f64,
    pub stall_behaviour: StallBehaviour,
    pub stall_timeout_s: f64,

    /// May be zero. Warned about in the UI rather than clamped, because many
    /// applications treat zero pressure as pen-up and split the stroke in two.
    pub min_pressure: f64,

    speed: f64,
    accum_dist: f64,
    accum_time: f64,
    since_stroke_start_s: f64,
    envelope: f64,
    last_down: bool,
}

impl Default for Pressure {
    fn default() -> Self {
        Self {
            enabled: true,
            attack_s: 0.06,
            release_s: 0.06,
            envelope_enabled: true,
            speed_enabled: true,
            v_max_mm_s: 400.0,
            gamma: 1.0,
            source: SpeedSource::Cursor,
            velocity_smoothing_s: 0.04,
            stall_threshold_mm: 0.04,
            stall_behaviour: StallBehaviour::Hold,
            stall_timeout_s: 0.12,
            min_pressure: 0.05,
            speed: 0.0,
            accum_dist: 0.0,
            accum_time: 0.0,
            since_stroke_start_s: 0.0,
            envelope: 0.0,
            last_down: false,
        }
    }
}

impl Pressure {
    /// Update the low-passed velocity estimate.
    ///
    /// Instantaneous velocity is unusable: at 1kHz each sample carries a fraction of a
    /// millimetre over ~1ms with scheduler jitter, so `distance / dt` is dominated by
    /// quantisation noise and pressure comes out visibly gritty.
    ///
    /// Distance and time are *accumulated* and the estimate updated only once travel
    /// crosses the threshold. That reports genuinely low speeds for slow-but-continuous
    /// motion, while holding through a true stall. A long stall discards its banked
    /// time rather than eventually dividing by it — otherwise the hotspot returns on
    /// resumption, with interest.
    fn update_speed(&mut self, delta_mm: f64, dt: f64) {
        let tau = self.velocity_smoothing_s;

        if self.stall_behaviour == StallBehaviour::Decay {
            let instantaneous = delta_mm / dt;
            if !instantaneous.is_finite() {
                return;
            }
            self.speed = low_pass(self.speed, instantaneous, dt, tau);
            self.accum_dist = 0.0;
            self.accum_time = 0.0;
            return;
        }

        // Bank nothing at all for a sample that contributed no distance.
        //
        // Banking its time would inflate the divisor and make the *next* measurement
        // read as spuriously slow — reproducing the hotspot this exists to prevent.
        // The timeout below only catches long stalls; short ones have to be excluded
        // here or they poison the resumption.
        //
        // The stabiliser emits exactly zero while the cursor is inside the radius, so
        // for `Output` this is an exact discriminator. For `Cursor` an occasional zero
        // sample during slow motion is skipped too, slightly *over*-estimating speed —
        // which errs toward thinner strokes rather than blobs, the safer direction.
        if delta_mm <= 0.0 {
            return;
        }

        self.accum_dist += delta_mm;
        self.accum_time += dt;

        let threshold = if self.stall_threshold_mm.is_finite() && self.stall_threshold_mm > 0.0 {
            self.stall_threshold_mm
        } else {
            0.0
        };

        if self.accum_dist >= threshold && self.accum_time > 0.0 {
            let measured = self.accum_dist / self.accum_time;
            if measured.is_finite() {
                self.speed = low_pass(self.speed, measured, self.accum_time, tau);
            }
            self.accum_dist = 0.0;
            self.accum_time = 0.0;
        } else if self.accum_time > self.stall_timeout_s.max(0.0) {
            // Genuine stall: drop the accumulator, hold the estimate.
            self.accum_dist = 0.0;
            self.accum_time = 0.0;
        }
    }

    fn speed_term(&self) -> f64 {
        if !self.speed_enabled {
            return 1.0;
        }
        if !(self.v_max_mm_s.is_finite() && self.v_max_mm_s > 0.0) {
            return 1.0;
        }
        let linear = (1.0 - self.speed / self.v_max_mm_s).clamp(0.0, 1.0);
        let gamma = if self.gamma.is_finite() && self.gamma > 0.0 {
            self.gamma
        } else {
            1.0
        };
        let shaped = linear.powf(gamma);
        if shaped.is_finite() {
            shaped.clamp(0.0, 1.0)
        } else {
            linear
        }
    }
}

/// Time-correct exponential low pass. `tau <= 0` means no smoothing.
fn low_pass(current: f64, target: f64, dt: f64, tau: f64) -> f64 {
    if !(tau.is_finite() && tau > 0.0) {
        return target;
    }
    let alpha = 1.0 - (-dt / tau).exp();
    if !alpha.is_finite() {
        return target;
    }
    current + alpha * (target - current)
}

impl Stage for Pressure {
    fn name(&self) -> &'static str {
        "pressure"
    }

    fn process(&mut self, s: &mut Sample) {
        if s.stroke_start {
            self.since_stroke_start_s = 0.0;
            self.speed = 0.0;
            self.accum_dist = 0.0;
            self.accum_time = 0.0;
            self.envelope = 0.0;
        }

        let delta = match self.source {
            SpeedSource::Output => s.magnitude(),
            SpeedSource::Cursor => s.raw_magnitude(),
        };
        if delta.is_finite() {
            self.update_speed(delta, s.dt);
        }

        // Envelope: ramp in over `attack`, out over `release`. Gives strokes tapered
        // ends, which is most of what reads as hand-drawn.
        let target = if s.down { 1.0 } else { 0.0 };
        if self.envelope_enabled {
            self.since_stroke_start_s += s.dt;
            let ramp = if s.down { self.attack_s } else { self.release_s };
            if ramp > 0.0 {
                let step = s.dt / ramp;
                if step.is_finite() {
                    // Move toward the target by at most `step`, and snap when within
                    // reach.
                    //
                    // Branching on `target > envelope` and decrementing otherwise has
                    // no equality case: once the envelope reaches 1.0 while still held
                    // down, `1.0 > 1.0` is false and it decrements every sample. With a
                    // 1ms dt that is invisible jitter; with a 60ms dt the step is a full
                    // 1.0 and the envelope collapses to zero mid-stroke. Found on real
                    // recordings, 2026-07-30 — uniform synthetic timing cannot expose it.
                    let diff = target - self.envelope;
                    if diff.abs() <= step {
                        self.envelope = target;
                    } else {
                        self.envelope += step * diff.signum();
                    }
                    self.envelope = self.envelope.clamp(0.0, 1.0);
                }
            } else {
                self.envelope = target;
            }
        } else {
            self.envelope = target;
        }

        let floor = if self.min_pressure.is_finite() {
            self.min_pressure.clamp(0.0, 1.0)
        } else {
            0.0
        };

        let p = if s.down {
            (self.envelope * self.speed_term()).max(floor)
        } else {
            // Releasing: let the envelope carry it down, ignoring the floor so the
            // stroke can actually reach zero.
            self.envelope * self.speed_term()
        };

        self.last_down = s.down;
        s.speed_mm_s = Some(self.speed);
        s.pressure = Some(if p.is_finite() { p.clamp(0.0, 1.0) } else { floor });
    }

    fn reset(&mut self) {
        self.speed = 0.0;
        self.accum_dist = 0.0;
        self.accum_time = 0.0;
        self.since_stroke_start_s = 0.0;
        self.envelope = 0.0;
        self.last_down = false;
    }

    fn settled(&self) -> bool {
        let target = if self.last_down { 1.0 } else { 0.0 };
        (self.envelope - target).abs() < 1.0e-4
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

    fn stroke(st: &mut Pressure, samples: usize, delta_mm: f64, dt: f64) -> Vec<f64> {
        let mut out = Vec::new();
        for i in 0..samples {
            let mut s = Sample::new(delta_mm, 0.0, (i as u64 + 1) * 1_000, true);
            s.dt = dt;
            s.stroke_start = i == 0;
            st.process(&mut s);
            out.push(s.pressure.unwrap_or(-1.0));
        }
        out
    }

    #[test]
    fn pressure_is_always_within_range() {
        let mut st = Pressure::default();
        for p in stroke(&mut st, 500, 0.5, 0.001) {
            assert!((0.0..=1.0).contains(&p), "pressure out of range: {p}");
        }
    }

    #[test]
    fn envelope_ramps_in_rather_than_snapping() {
        let mut st = Pressure::default();
        let out = stroke(&mut st, 200, 0.0, 0.001);
        assert!(out[0] < out[50], "pressure should rise during attack");
        assert!(out[100] > 0.9, "should approach full by 100ms with 60ms attack");
    }

    #[test]
    fn fast_motion_reduces_pressure() {
        let mut slow = Pressure::default();
        let mut fast = Pressure::default();
        // Same duration, very different speeds.
        let slow_out = stroke(&mut slow, 300, 0.02, 0.001);
        let fast_out = stroke(&mut fast, 300, 1.0, 0.001);
        assert!(
            slow_out[299] > fast_out[299],
            "slow {} should exceed fast {}",
            slow_out[299],
            fast_out[299]
        );
    }

    #[test]
    fn hold_beats_decay_through_a_stall() {
        // Fast, then a stall, then fast again. `Hold` should not dip as deeply.
        fn run(behaviour: StallBehaviour) -> f64 {
            let mut st = Pressure {
                stall_behaviour: behaviour,
                ..Default::default()
            };
            let mut t = 0u64;
            let mut push = |st: &mut Pressure, delta: f64, n: usize, t: &mut u64| -> f64 {
                let mut last = 0.0;
                for _ in 0..n {
                    *t += 1_000;
                    let mut s = Sample::new(delta, 0.0, *t, true);
                    s.dt = 0.001;
                    s.stroke_start = *t == 1_000;
                    st.process(&mut s);
                    last = s.pressure.unwrap_or(0.0);
                }
                last
            };
            push(&mut st, 0.5, 200, &mut t); // moving fast
            push(&mut st, 0.0, 30, &mut t); // stalled
            push(&mut st, 0.5, 5, &mut t) // moving again
        }

        let held = run(StallBehaviour::Hold);
        let decayed = run(StallBehaviour::Decay);
        assert!(
            held < decayed,
            "hold ({held}) should show less of a pressure spike than decay ({decayed})"
        );
    }

    /// Regression guard: a long sample interval must not disturb a settled envelope.
    ///
    /// Real input has wildly variable dt — 1ms to 60ms within a single stroke — and the
    /// original code decremented the envelope on every sample once it reached its
    /// target, which a large dt turned into a full collapse to zero mid-stroke.
    #[test]
    fn a_settled_envelope_survives_a_long_sample_interval() {
        let mut st = Pressure::default();

        // Settle at full pressure with fast samples.
        let mut t = 0u64;
        for i in 0..300 {
            t += 1_000;
            let mut s = Sample::new(0.01, 0.0, t, true);
            s.dt = 0.001;
            s.stroke_start = i == 0;
            st.process(&mut s);
        }
        let settled = {
            t += 1_000;
            let mut s = Sample::new(0.01, 0.0, t, true);
            s.dt = 0.001;
            st.process(&mut s);
            s.pressure.unwrap()
        };
        assert!(settled > 0.9, "should have settled high, got {settled}");

        // A 60ms gap, still held down. Pressure must not fall off a cliff.
        t += 60_000;
        let mut s = Sample::new(0.01, 0.0, t, true);
        s.dt = 0.060;
        st.process(&mut s);
        let after = s.pressure.unwrap();

        assert!(
            (after - settled).abs() < 0.05,
            "a 60ms interval collapsed pressure from {settled:.4} to {after:.4}"
        );
    }

    #[test]
    fn envelope_still_releases_when_the_button_comes_up() {
        let mut st = Pressure::default();
        let mut t = 0u64;
        for i in 0..300 {
            t += 1_000;
            let mut s = Sample::new(0.01, 0.0, t, true);
            s.dt = 0.001;
            s.stroke_start = i == 0;
            st.process(&mut s);
        }
        // Release and let it ramp down.
        let mut last = 1.0;
        for i in 0..200 {
            t += 1_000;
            let mut s = Sample::new(0.0, 0.0, t, false);
            s.dt = 0.001;
            s.stroke_end = i == 0;
            st.process(&mut s);
            last = s.pressure.unwrap();
        }
        assert!(last < 0.01, "envelope should release to ~0, got {last}");
    }

    #[test]
    fn nonsense_parameters_still_produce_valid_pressure() {
        let mut st = Pressure {
            attack_s: f64::NAN,
            v_max_mm_s: 0.0,
            gamma: -1.0,
            velocity_smoothing_s: f64::INFINITY,
            stall_threshold_mm: f64::NAN,
            min_pressure: f64::NAN,
            ..Default::default()
        };
        for p in stroke(&mut st, 100, 0.5, 0.001) {
            assert!((0.0..=1.0).contains(&p), "got {p}");
        }
    }
}
