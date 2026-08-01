//! `scroll` — divert hand movement into scroll events while a button is held.

use crate::sample::Sample;
use crate::stage::Stage;

/// How held movement becomes scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Hand movement scrolls directly, one-to-one with distance.
    ///
    /// With `freeze_cursor` this is a finger-on-glass swipe; without it, a hand tool holding
    /// a point on the page so it follows the pointer. That was two modes until the only
    /// difference between them turned out to be the one flag, and a mode that is a synonym
    /// for a setting is a mode nobody can predict.
    Drag,
    /// Your own wheel, routed through this stage so it picks up speed and momentum.
    ///
    /// The binding here is a **modifier**, not a grip: `alt+wheel` coasts while a plain wheel
    /// does not. Acting with nothing bound is `always_active`, off by default — a stage that
    /// started rewriting the wheel the moment the mode was picked would be changing the one
    /// input the user had not asked it to touch.
    Wheel,
    /// Middle-click autoscroll: displacement from where the button went down sets a scroll
    /// *velocity*, so a small sustained offset scrolls without more hand travel.
    Joystick,
}

/// Diverts hand movement into scrolling while a button is held.
///
/// # Which gestures freeze the cursor, and why it is not all of them
///
/// `drag` freezes it: a finger on glass has no cursor, and a swipe that also dragged one
/// would select text while scrolling. `grab` and `joystick` do **not**.
///
/// Freezing the cursor during `joystick` was the original reading of the spec and it made the
/// gesture unusable in practice: displacement from the press origin is the control, so with
/// no cursor to see, there is no way to judge how fast you are scrolling or how far back to
/// come to stop. It reads as the scroll locking up. Every autoscroll worth copying — browsers,
/// file managers — leaves the cursor free for exactly this reason, and stages.md has been
/// corrected to match.
///
/// # Momentum
///
/// A flick carries on and decays, which is what makes a long page feel like a surface rather
/// than a crank. The rate is measured as the gesture ends and decays exponentially; any new
/// press cancels it, because a hand returning to the mouse means the user wants control back.
///
/// # Keeping the pipeline breathing
///
/// A velocity that persists while the hand is still — a joystick offset, or a glide — has no
/// input to drive it, so this reports itself **unsettled** while either is running. That is
/// what keeps the daemon feeding zero-motion ticks, the same mechanism the pressure envelope
/// and the stabiliser's lag already rely on.
#[derive(Debug, Clone)]
pub struct Scroll {
    enabled: bool,
    pub mode: Mode,
    /// Scroll notches per millimetre of hand travel. Larger is faster.
    ///
    /// The inverse of the distance-per-notch this used to take. Notches mean little to
    /// anyone who has not read the kernel's input docs, and a number that got *smaller* as
    /// the thing it controlled got *faster* was one more thing to hold backwards.
    pub speed: f64,
    pub invert: bool,
    /// Displacement before `joystick` starts moving, so resting a hand does not creep.
    pub deadzone_mm: f64,
    /// Notches per second per millimetre of displacement beyond the deadzone.
    pub gain: f64,
    /// Click to start and click to stop, rather than holding throughout. Applies to every
    /// mode: a swipe you do not have to keep holding is as reasonable as an autoscroll you
    /// do not.
    pub latch: bool,
    /// Hold the cursor still while the gesture runs.
    ///
    /// True is a swipe — a finger on glass has no cursor to move. False is a hand tool: the
    /// page follows the pointer, so the point under it stays under it.
    ///
    /// **Honoured by every mode, including `joystick`.** A frozen joystick is harder to steer,
    /// since displacement is the control and there is then nothing on screen showing it — but
    /// that is the user's trade to make, not this stage's to refuse.
    pub freeze_cursor: bool,
    /// Keep scrolling after a swipe is released, decaying from the speed it ended at.
    pub momentum: bool,
    /// Seconds for a flick to decay to about a third of its release speed.
    pub momentum_decay_s: f64,
    /// Notches of output per notch of wheel, in `wheel` mode.
    pub wheel_gain: f64,
    /// Act with nothing held, in `wheel` mode.
    ///
    /// Without it an unbound wheel mode is **inert**, which is the safe default: the wheel is
    /// an input the user already had, and a mode that started rewriting it the moment it was
    /// selected would be taking something without being asked. Turning this on is how you say
    /// you want every scroll to go through the stage.
    pub always_active: bool,
    /// Letting go of *everything* ends the glide.
    ///
    /// Releasing one part of a chord leaves the page coasting; releasing the rest stops it
    /// dead. That gives a flick a brake without needing a second binding for it.
    ///
    /// It means something for any binding in `wheel` mode, where the binding is a modifier and
    /// releasing it is the natural "stop" — and only for a chord in the other modes, where a
    /// single button has no halfway state to coast in.
    pub full_release_stops_momentum: bool,

    /// Displacement from the press origin, millimetres.
    offset_x: f64,
    offset_y: f64,
    /// Remainder below one hi-res step, carried so slow drags are not truncated away.
    carry_x: f64,
    carry_y: f64,
    was_held: bool,
    latched: bool,
    /// True while a velocity is being produced with no motion to drive it.
    coasting: bool,
    /// Recent scroll rate in notches per second, for a flick to carry on from.
    rate_x: f64,
    rate_y: f64,
    /// Notches per second still owed to momentum after a release.
    glide_x: f64,
    glide_y: f64,
    /// Whether the gesture was engaged on the previous sample, so a release is noticed once.
    was_active: bool,
}

impl Default for Scroll {
    fn default() -> Self {
        // Drag, not an inert mode. **There is no `off`**: every stage already has `enabled`,
        // which turns it off without losing its tuning, and a second way to mean the same
        // thing reads as a setting that does something the user has not been told about.
        Self::new(Mode::Drag)
    }
}

impl Scroll {
    pub fn new(mode: Mode) -> Self {
        Self {
            enabled: true,
            mode,
            // A notch every 4mm of travel — about a finger's width, close to how far a
            // touchscreen swipe moves content for a given hand movement.
            speed: 0.25,
            invert: false,
            // Below this a resting hand's tremor would creep the page.
            deadzone_mm: 2.0,
            gain: 1.5,
            latch: false,
            freeze_cursor: true,
            momentum: false,
            // A flick that dies in a third of a second reads as a surface with weight rather
            // than as one that keeps going after the hand has moved on.
            momentum_decay_s: 0.35,
            wheel_gain: 1.0,
            always_active: false,
            full_release_stops_momentum: false,
            offset_x: 0.0,
            offset_y: 0.0,
            carry_x: 0.0,
            carry_y: 0.0,
            was_held: false,
            latched: false,
            coasting: false,
            rate_x: 0.0,
            rate_y: 0.0,
            glide_x: 0.0,
            glide_y: 0.0,
            was_active: false,
        }
    }

    /// Whether the gesture is engaged, resolving `latch` against the raw button state.
    fn engaged(&mut self, held: bool) -> bool {
        let pressed = held && !self.was_held;
        self.was_held = held;

        if self.latch {
            // A press toggles. Releasing does nothing, which is what "click to start, click
            // to stop" means — and what lets the hand leave the button entirely mid-scroll.
            if pressed {
                self.latched = !self.latched;
            }
            self.latched
        } else {
            held
        }
    }

    fn release(&mut self) {
        self.offset_x = 0.0;
        self.offset_y = 0.0;
        self.carry_x = 0.0;
        self.carry_y = 0.0;
        self.coasting = false;
        self.rate_x = 0.0;
        self.rate_y = 0.0;
        self.glide_x = 0.0;
        self.glide_y = 0.0;
        self.was_active = false;
    }

    /// Carry a released flick onward, decaying.
    ///
    /// Stops at a rate below which the page would be crawling: a glide that never quite ends
    /// leaves the pipeline unsettled forever, which would hold the daemon's tick loop open for
    /// motion nobody can see.
    fn glide(&mut self, s: &mut Sample, dt: f64) {
        const STOP_NOTCHES_PER_S: f64 = 0.25;
        if self.glide_x.hypot(self.glide_y) < STOP_NOTCHES_PER_S || dt <= 0.0 {
            self.glide_x = 0.0;
            self.glide_y = 0.0;
            self.coasting = false;
            return;
        }
        s.scroll_x += self.glide_x * dt;
        s.scroll_y += self.glide_y * dt;

        let tau = if self.momentum_decay_s.is_finite() && self.momentum_decay_s > 0.0 {
            self.momentum_decay_s
        } else {
            0.35
        };
        let decay = (-dt / tau).exp();
        self.glide_x *= decay;
        self.glide_y *= decay;
        self.coasting = true;
    }
}

impl Stage for Scroll {
    fn name(&self) -> &'static str {
        "scroll"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }
        if s.discontinuity {
            self.was_held = false;
            self.latched = false;
            self.release();
        }

        let dt = if s.dt.is_finite() && s.dt > 0.0 { s.dt } else { 0.0 };
        let sign = if self.invert { -1.0 } else { 1.0 };
        let active = self.engaged(s.scrolling);

        // `wheel` has its own shape and takes it here, rather than being threaded through a
        // body written for a gesture that diverts hand movement. There is no press origin, no
        // motion to eat, and — the part that matters — **a held binding is a modifier, not a
        // hand returning to the mouse**. The shared path cancels the glide the moment the
        // gesture engages, which is right for a grab and is what made momentum unreachable for
        // anyone who bound this mode at all.
        if self.mode == Mode::Wheel {
            let acting = self.always_active || active;
            if acting && (s.wheel_v != 0.0 || s.wheel_h != 0.0) {
                // Taken *and cleared*, so nothing downstream passes the original through as
                // well and doubles it. Every other mode leaves both axes alone, which is what
                // makes routing the wheel through the pipeline safe at all.
                let gain = if self.wheel_gain.is_finite() && self.wheel_gain > 0.0 {
                    self.wheel_gain
                } else {
                    1.0
                };
                let (v, h) = (s.wheel_v * gain, s.wheel_h * gain);
                s.wheel_v = 0.0;
                s.wheel_h = 0.0;
                s.scroll_y += v;
                s.scroll_x += h;
                // A notch is an impulse rather than a rate, so the glide is seeded from the
                // notch itself: one flick of the wheel, one length of coast.
                if self.momentum && dt > 0.0 {
                    self.glide_y += v / dt.max(0.008);
                    self.glide_x += h / dt.max(0.008);
                    self.coasting = true;
                }
            }
            // Letting go of the modifier is the brake, on the same binding rather than a
            // second one to find. A chord still coasts while any part is held.
            if !acting && self.full_release_stops_momentum && !s.scroll_partial {
                self.glide_x = 0.0;
                self.glide_y = 0.0;
            }
            self.glide(s, dt);
            return;
        }

        if !active {
            // Letting go of the whole chord is a brake. Checked before the glide runs, so a
            // full release stops the page on the same sample rather than a frame later.
            if self.full_release_stops_momentum && !s.scroll_partial {
                self.glide_x = 0.0;
                self.glide_y = 0.0;
            }
            if self.was_active {
                self.was_active = false;
                // A flick hands its speed to the glide; a slow finish hands over nothing,
                // which is what stops a careful drag from drifting after the hand stops.
                if self.momentum
                    && self.mode != Mode::Joystick
                    && !(self.full_release_stops_momentum && !s.scroll_partial)
                {
                    self.glide_x = self.rate_x;
                    self.glide_y = self.rate_y;
                } else {
                    self.glide_x = 0.0;
                    self.glide_y = 0.0;
                }
                self.offset_x = 0.0;
                self.offset_y = 0.0;
                self.rate_x = 0.0;
                self.rate_y = 0.0;
            }
            self.glide(s, dt);
            return;
        }

        // The hand is back, so whatever the page was still doing is no longer wanted.
        self.glide_x = 0.0;
        self.glide_y = 0.0;
        self.was_active = true;

        let mut produced_x = 0.0;
        let mut produced_y = 0.0;

        match self.mode {
            // Handled and returned above; the wheel is the whole input, so there is no hand
            // movement here for it to divert.
            Mode::Wheel => {}
            Mode::Drag => {
                let speed = if self.speed.is_finite() && self.speed > 0.0 {
                    self.speed
                } else {
                    0.25
                };
                // Scrolling *content*: pulling down brings the page up, which is why the
                // vertical term is negated before `invert` is applied.
                produced_x = s.dx * speed * sign;
                produced_y = -s.dy * speed * sign;
                self.coasting = false;
            }
            Mode::Joystick => {
                self.offset_x += s.dx;
                self.offset_y += s.dy;

                let deadzone = if self.deadzone_mm.is_finite() && self.deadzone_mm > 0.0 {
                    self.deadzone_mm
                } else {
                    0.0
                };
                let gain = if self.gain.is_finite() { self.gain.max(0.0) } else { 0.0 };
                let distance = self.offset_x.hypot(self.offset_y);

                if distance > deadzone && distance > 0.0 && dt > 0.0 {
                    // Speed grows with displacement *past* the deadzone, so the gesture starts
                    // from a standstill rather than jumping to a rate at the edge.
                    let beyond = distance - deadzone;
                    let rate = beyond * gain;
                    produced_x = self.offset_x / distance * rate * dt * sign;
                    produced_y = -self.offset_y / distance * rate * dt * sign;
                    self.coasting = true;
                } else {
                    self.coasting = false;
                }
            }
        }

        self.carry_x += produced_x;
        self.carry_y += produced_y;

        // Rate for a flick to carry on from, smoothed so the last twitch before release does
        // not decide the whole glide.
        if dt > 0.0 {
            let alpha = (dt / 0.05).clamp(0.0, 1.0);
            self.rate_x += ((produced_x / dt) - self.rate_x) * alpha;
            self.rate_y += ((produced_y / dt) - self.rate_y) * alpha;
        }

        s.scroll_x += self.carry_x;
        s.scroll_y += self.carry_y;
        self.carry_x = 0.0;
        self.carry_y = 0.0;

        if self.freeze_cursor {
            s.dx = 0.0;
            s.dy = 0.0;
        }
    }

    fn reset(&mut self) {
        self.was_held = false;
        self.latched = false;
        self.release();
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn settled(&self) -> bool {
        // Unsettled only while a velocity is running with nothing driving it — a joystick
        // offset, or a glide after a flick. A drag is settled between samples, since it
        // produces nothing without hand movement, and claiming otherwise would keep the
        // daemon ticking for a gesture with nothing to add.
        !self.coasting && self.glide_x == 0.0 && self.glide_y == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(stage: &mut Scroll, steps: &[(f64, f64, bool)]) -> (f64, f64, f64, f64) {
        let (mut sx, mut sy, mut mx, mut my) = (0.0, 0.0, 0.0, 0.0);
        for (i, (dx, dy, held)) in steps.iter().enumerate() {
            let mut s = Sample::new(*dx, *dy, (i as u64 + 1) * 1000, false);
            s.dt = 0.001;
            s.scrolling = *held;
            stage.process(&mut s);
            sx += s.scroll_x;
            sy += s.scroll_y;
            mx += s.dx;
            my += s.dy;
        }
        (sx, sy, mx, my)
    }

    #[test]
    fn an_unheld_gesture_is_byte_identical_pass_through() {
        // The identity is "the button is not held", not a mode that means nothing — `enabled`
        // is what turns a stage off, and one way to mean that is enough.
        let mut stage = Scroll::new(Mode::Drag);
        let mut s = Sample::new(1.5, -2.5, 1000, true);
        let before = s;
        stage.process(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn an_unheld_button_leaves_motion_alone() {
        let mut stage = Scroll::new(Mode::Drag);
        let (sx, sy, mx, my) = feed(&mut stage, &[(2.0, 3.0, false); 5]);
        assert_eq!((sx, sy), (0.0, 0.0), "no gesture, no scroll");
        assert_eq!((mx, my), (10.0, 15.0), "and motion passes through untouched");
    }

    #[test]
    fn dragging_scrolls_by_distance_and_eats_the_motion() {
        let mut stage = Scroll::new(Mode::Drag);
        stage.speed = 0.25;
        // 16mm downward: four notches, and the page goes the other way.
        let (_, sy, mx, my) = feed(&mut stage, &[(0.0, 4.0, true); 4]);
        assert!((sy + 4.0).abs() < 1e-9, "dragging down scrolls content up: {sy}");
        assert_eq!((mx, my), (0.0, 0.0), "the cursor must not move while swiping");
    }

    #[test]
    fn a_slow_drag_is_not_truncated_away() {
        // The accumulation hazard from modules.md: thresholding a per-sample value when the
        // quantity accumulates. Sub-notch samples must bank, not vanish.
        let mut stage = Scroll::new(Mode::Drag);
        stage.speed = 0.25;
        let (_, sy, _, _) = feed(&mut stage, &[(0.0, 0.05, true); 80]);
        assert!((sy + 1.0).abs() < 1e-9, "80 x 0.05mm is one notch: {sy}");
    }

    #[test]
    fn invert_flips_the_direction() {
        let mut stage = Scroll::new(Mode::Drag);
        stage.invert = true;
        let (_, sy, _, _) = feed(&mut stage, &[(0.0, 4.0, true); 4]);
        assert!(sy > 0.0, "inverted, dragging down scrolls content down: {sy}");
    }

    #[test]
    fn joystick_keeps_scrolling_while_the_hand_holds_still() {
        let mut stage = Scroll::new(Mode::Joystick);
        stage.deadzone_mm = 2.0;
        stage.gain = 1.5;
        // Push 10mm past centre, then stop moving entirely.
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 1.0, true); 10];
        steps.extend(vec![(0.0, 0.0, true); 500]);
        let (_, sy, _, _) = feed(&mut stage, &steps);
        assert!(sy < -1.0, "displacement alone must keep it scrolling: {sy}");
        assert!(!stage.settled(), "and it must ask for ticks to do so");
    }

    #[test]
    fn joystick_rests_inside_the_deadzone() {
        let mut stage = Scroll::new(Mode::Joystick);
        stage.deadzone_mm = 2.0;
        let (_, sy, _, _) = feed(&mut stage, &[(0.0, 0.02, true); 50]);
        assert_eq!(sy, 0.0, "1mm of drift is not a scroll request");
        assert!(stage.settled(), "and it must not hold the tick loop open");
    }

    #[test]
    fn joystick_speed_grows_with_displacement() {
        let far = {
            let mut stage = Scroll::new(Mode::Joystick);
            let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 2.0, true); 10];
            steps.extend(vec![(0.0, 0.0, true); 100]);
            feed(&mut stage, &steps).1.abs()
        };
        let near = {
            let mut stage = Scroll::new(Mode::Joystick);
            let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 0.5, true); 10];
            steps.extend(vec![(0.0, 0.0, true); 100]);
            feed(&mut stage, &steps).1.abs()
        };
        assert!(far > near * 2.0, "further from centre must scroll faster: {far} vs {near}");
    }

    #[test]
    fn latching_survives_the_button_being_released() {
        let mut stage = Scroll::new(Mode::Joystick);
        stage.latch = true;
        // Press once to start, release the button, then keep displacing with it up.
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 1.0, true); 5];
        steps.extend(vec![(0.0, 1.0, false); 5]);
        steps.extend(vec![(0.0, 0.0, false); 100]);
        let (_, sy, _, _) = feed(&mut stage, &steps);
        assert!(sy < 0.0, "a latched scroll continues after release: {sy}");
    }

    #[test]
    fn a_second_click_ends_a_latched_scroll() {
        let mut stage = Scroll::new(Mode::Joystick);
        stage.latch = true;
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 1.0, true); 5];
        steps.extend(vec![(0.0, 0.0, false); 5]);
        steps.extend(vec![(0.0, 0.0, true); 2]); // the stopping click
        steps.extend(vec![(0.0, 0.0, false); 50]);
        feed(&mut stage, &steps);
        assert!(stage.settled(), "clicking again must stop it");
        // And motion flows again afterwards.
        let mut s = Sample::new(3.0, 4.0, 99_000, false);
        s.dt = 0.001;
        stage.process(&mut s);
        assert_eq!((s.dx, s.dy), (3.0, 4.0));
    }

    #[test]
    fn releasing_returns_the_cursor_and_forgets_the_origin() {
        let mut stage = Scroll::new(Mode::Joystick);
        feed(&mut stage, &[(0.0, 5.0, true); 5]);
        let (_, sy, mx, my) = feed(&mut stage, &[(2.0, 2.0, false); 3]);
        assert_eq!(sy, 0.0, "no scrolling once released");
        assert_eq!((mx, my), (6.0, 6.0), "and the cursor moves again");
        assert!(stage.settled());
    }

    #[test]
    fn an_unfrozen_drag_keeps_the_cursor_moving() {
        // The hand-tool feel: the page moves *with* the pointer, so what is under it stays
        // under it. One flag, where there used to be a whole second mode.
        let mut stage = Scroll::new(Mode::Drag);
        stage.freeze_cursor = false;
        let (_, sy, mx, my) = feed(&mut stage, &[(1.0, 2.0, true); 8]);
        assert!(sy != 0.0, "it must still scroll");
        assert!((mx - 8.0).abs() < 1e-9 && (my - 16.0).abs() < 1e-9, "cursor kept moving");
    }

    #[test]
    fn joystick_honours_the_cursor_setting_in_both_directions() {
        // It used to override the setting, on the reasoning that a frozen joystick cannot be
        // steered. That is a real cost but it is the user's to weigh — a mode that quietly
        // ignores a switch is worse than one that lets you make a hard choice.
        let mut free = Scroll::new(Mode::Joystick);
        free.freeze_cursor = false;
        let (_, _, mx, my) = feed(&mut free, &[(0.5, 0.5, true); 10]);
        assert!((mx - 5.0).abs() < 1e-9 && (my - 5.0).abs() < 1e-9, "free: ({mx}, {my})");

        let mut frozen = Scroll::new(Mode::Joystick);
        frozen.freeze_cursor = true;
        let (_, _, mx, my) = feed(&mut frozen, &[(0.5, 0.5, true); 10]);
        assert_eq!((mx, my), (0.0, 0.0), "frozen must hold the cursor");
    }

    #[test]
    fn a_chord_can_coast_on_a_partial_release_and_stop_on_a_full_one() {
        // The brake, on the same binding rather than a second one to find.
        fn run(full_release: bool, still_partly_held: bool) -> f64 {
            let mut stage = Scroll::new(Mode::Drag);
            stage.momentum = true;
            stage.full_release_stops_momentum = full_release;

            let mut scrolled = 0.0;
            // A flick, with the whole chord down.
            for i in 0..12 {
                let mut s = Sample::new(0.0, 3.0, (i + 1) * 1000, false);
                s.dt = 0.001;
                s.scrolling = true;
                s.scroll_partial = true;
                stage.process(&mut s);
            }
            // Then let go of the gesture, keeping (or not) a part of the chord held.
            for i in 0..40 {
                let mut s = Sample::new(0.0, 0.0, (i + 100) * 1000, false);
                s.dt = 0.001;
                s.scrolling = false;
                s.scroll_partial = still_partly_held;
                stage.process(&mut s);
                scrolled += s.scroll_y;
            }
            scrolled.abs()
        }

        assert!(run(true, true) > 0.0, "one part still held must keep it coasting");
        assert_eq!(run(true, false), 0.0, "letting go of everything must stop it dead");
        // With the option off, a release coasts either way — the old behaviour is intact.
        assert!(run(false, false) > 0.0);
    }

    #[test]
    fn a_flick_carries_on_and_stops_by_itself() {
        let mut stage = Scroll::new(Mode::Drag);
        stage.momentum = true;
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 3.0, true); 12];
        steps.push((0.0, 0.0, false));
        let during = feed(&mut stage, &steps).1;

        // Released: it must keep going...
        let after = feed(&mut stage, &[(0.0, 0.0, false); 20]).1;
        assert!(after.abs() > 0.0, "a flick must carry on: {after}");
        assert!(after.abs() < during.abs(), "and carry less than the swipe itself");

        // ...and then stop, rather than holding the tick loop open forever.
        feed(&mut stage, &[(0.0, 0.0, false); 3000]);
        assert!(stage.settled(), "a glide must end");
    }

    #[test]
    fn momentum_is_off_unless_asked_for() {
        let mut stage = Scroll::new(Mode::Drag);
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 3.0, true); 12];
        steps.push((0.0, 0.0, false));
        feed(&mut stage, &steps);
        let after = feed(&mut stage, &[(0.0, 0.0, false); 20]).1;
        assert_eq!(after, 0.0, "a released drag must stop dead unless momentum is on");
    }

    #[test]
    fn taking_hold_again_cancels_a_glide() {
        // A hand back on the mouse wants control, not to fight the page's leftover speed.
        let mut stage = Scroll::new(Mode::Drag);
        stage.momentum = true;
        let mut steps: Vec<(f64, f64, bool)> = vec![(0.0, 3.0, true); 12];
        steps.push((0.0, 0.0, false));
        feed(&mut stage, &steps);
        feed(&mut stage, &[(0.0, 0.0, true); 2]);
        assert!(stage.glide_x == 0.0 && stage.glide_y == 0.0);
    }

    fn wheel_sample(held: bool) -> Sample {
        let mut s = Sample::new(0.0, 0.0, 1000, false);
        s.dt = 0.001;
        s.wheel_v = 3.0;
        s.wheel_h = -2.0;
        s.scrolling = held;
        s.scroll_partial = held;
        s
    }

    #[test]
    fn a_mode_that_is_not_wheel_never_touches_the_wheel() {
        // The guarantee that makes routing the wheel through the pipeline safe at all: a
        // stage not interested in it must leave it exactly as it arrived, on every axis.
        for mode in [Mode::Drag, Mode::Joystick] {
            let mut stage = Scroll::new(mode);
            let mut s = wheel_sample(true);
            stage.process(&mut s);
            assert_eq!((s.wheel_v, s.wheel_h), (3.0, -2.0), "{mode:?} took the wheel");
        }
    }

    #[test]
    fn wheel_mode_takes_both_axes_and_clears_them() {
        let mut stage = Scroll::new(Mode::Wheel);
        stage.always_active = true;
        let mut s = wheel_sample(false);
        stage.process(&mut s);
        assert_eq!((s.wheel_v, s.wheel_h), (0.0, 0.0), "taken means cleared, or it doubles");
        assert_eq!(s.scroll_y, 3.0);
        assert_eq!(s.scroll_x, -2.0, "the horizontal wheel is not an afterthought");
    }

    #[test]
    fn wheel_mode_is_inert_until_something_asks_for_it() {
        // Selecting the mode is not consent to rewrite the one input the user already had.
        // Off by default, it must pass the wheel through untouched — the same guarantee the
        // other modes give — until either a binding is held or `always_active` says so.
        let mut stage = Scroll::new(Mode::Wheel);
        let mut idle = wheel_sample(false);
        stage.process(&mut idle);
        assert_eq!((idle.wheel_v, idle.wheel_h), (3.0, -2.0), "inert must pass through");
        assert_eq!((idle.scroll_x, idle.scroll_y), (0.0, 0.0), "and produce nothing");
    }

    #[test]
    fn a_bound_wheel_mode_waits_for_its_binding() {
        // `alt+wheel` must leave a plain wheel alone, which is the whole reason to bind it.
        let mut stage = Scroll::new(Mode::Wheel);
        let mut idle = wheel_sample(false);
        stage.process(&mut idle);
        assert_eq!((idle.wheel_v, idle.wheel_h), (3.0, -2.0), "unheld must pass through");

        let mut held = wheel_sample(true);
        stage.process(&mut held);
        assert_eq!((held.wheel_v, held.wheel_h), (0.0, 0.0), "held must take it");
    }

    #[test]
    fn always_active_overrides_a_binding_rather_than_arguing_with_it() {
        let mut stage = Scroll::new(Mode::Wheel);
        stage.always_active = true;
        let mut s = wheel_sample(false);
        stage.process(&mut s);
        assert_eq!((s.wheel_v, s.wheel_h), (0.0, 0.0), "always means always");
    }

    #[test]
    fn a_held_modifier_does_not_kill_the_momentum_it_is_there_to_produce() {
        // The bug this exists to prevent: the shared path cancels the glide whenever the
        // gesture engages, on the reasoning that a hand back on the mouse wants control. In
        // `wheel` mode the binding is a *modifier* and is held throughout, so that rule made
        // momentum reachable only by users who had bound nothing at all.
        let mut stage = Scroll::new(Mode::Wheel);
        stage.momentum = true;

        for i in 0..3 {
            let mut s = Sample::new(0.0, 0.0, (i + 1) * 1000, false);
            s.dt = 0.008;
            s.wheel_v = 1.0;
            s.scrolling = true;
            s.scroll_partial = true;
            stage.process(&mut s);
        }
        // Modifier still down, wheel now still: the page must keep moving.
        let mut coasted = 0.0;
        for i in 0..20 {
            let mut s = Sample::new(0.0, 0.0, (i + 100) * 1000, false);
            s.dt = 0.008;
            s.scrolling = true;
            s.scroll_partial = true;
            stage.process(&mut s);
            coasted += s.scroll_y;
        }
        assert!(coasted > 0.0, "a flick under a held modifier must coast: {coasted}");
    }

    #[test]
    fn releasing_the_modifier_can_brake_a_wheel_glide() {
        // Single-button in the other modes has no halfway state, so the brake means nothing
        // there. Here the binding is a modifier, so releasing it is exactly the "stop".
        fn run(brake: bool) -> f64 {
            let mut stage = Scroll::new(Mode::Wheel);
            stage.momentum = true;
            stage.full_release_stops_momentum = brake;
            for i in 0..3 {
                let mut s = Sample::new(0.0, 0.0, (i + 1) * 1000, false);
                s.dt = 0.008;
                s.wheel_v = 1.0;
                s.scrolling = true;
                s.scroll_partial = true;
                stage.process(&mut s);
            }
            let mut coasted = 0.0;
            for i in 0..20 {
                let mut s = Sample::new(0.0, 0.0, (i + 100) * 1000, false);
                s.dt = 0.008;
                stage.process(&mut s);
                coasted += s.scroll_y;
            }
            coasted
        }
        assert_eq!(run(true), 0.0, "letting go must stop it dead");
        assert!(run(false) > 0.0, "and with the brake off it coasts as before");
    }

    #[test]
    fn nothing_panics_on_pathological_input() {
        for mode in [Mode::Drag, Mode::Wheel, Mode::Joystick] {
            let mut stage = Scroll::new(mode);
            stage.speed = f64::NAN;
            stage.gain = f64::INFINITY;
            stage.deadzone_mm = -1.0;
            for (dx, dy) in [(0.0, 0.0), (f64::NAN, 1.0), (1e300, -1e300), (0.1, 0.1)] {
                let mut s = Sample::new(dx, dy, 1000, false);
                s.dt = 0.001;
                s.scrolling = true;
                stage.process(&mut s);
                assert!(s.scroll_x.is_finite() && s.scroll_y.is_finite());
            }
        }
    }
}
