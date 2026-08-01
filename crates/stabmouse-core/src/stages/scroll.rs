//! `scroll` — divert hand movement into scroll events while a button is held.

use crate::sample::Sample;
use crate::stage::Stage;

/// How held movement becomes scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Inert. The identity, and the default.
    Off,
    /// Touchscreen-style swipe: hand movement scrolls directly, one-to-one with distance.
    Drag,
    /// Middle-click autoscroll: displacement from where the button went down sets a scroll
    /// *velocity*, so a small sustained offset scrolls forever without more hand travel.
    Joystick,
}

/// Diverts motion into scroll while active, and **consumes it**.
///
/// # Why the cursor stops
///
/// Both gestures freeze the pointer: a swipe that also dragged the cursor would select text
/// or draw a line while scrolling, and an autoscroll whose origin kept moving has no fixed
/// point to measure displacement from. Consuming the motion is therefore the feature, not a
/// side effect — downstream position stages see nothing and drawing is suspended, exactly as
/// stages.md specifies.
///
/// # Joystick needs the pipeline to keep breathing
///
/// A velocity that persists while the hand is still has no input to be driven by, so this
/// reports itself **unsettled** whenever it is scrolling from displacement. That is what
/// keeps the daemon feeding zero-motion ticks, and it is the same mechanism the pressure
/// envelope and the stabiliser's lag already rely on.
#[derive(Debug, Clone)]
pub struct Scroll {
    enabled: bool,
    pub mode: Mode,
    /// Hand travel per notch in `drag`. Larger means slower scrolling.
    pub mm_per_unit: f64,
    pub invert: bool,
    /// Displacement before `joystick` starts moving, so resting a hand does not creep.
    pub deadzone_mm: f64,
    /// Notches per second per millimetre of displacement beyond the deadzone.
    pub gain: f64,
    /// `joystick`: click to start and click to stop, rather than holding throughout.
    pub latch: bool,

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
}

impl Default for Scroll {
    fn default() -> Self {
        Self::new(Mode::Off)
    }
}

impl Scroll {
    pub fn new(mode: Mode) -> Self {
        Self {
            enabled: true,
            mode,
            // A notch every 4mm: about a finger's width of travel, close to how far a
            // touchscreen swipe moves content per unit of hand movement.
            mm_per_unit: 4.0,
            invert: false,
            // Below this a resting hand's tremor would creep the page.
            deadzone_mm: 2.0,
            gain: 1.5,
            latch: false,
            offset_x: 0.0,
            offset_y: 0.0,
            carry_x: 0.0,
            carry_y: 0.0,
            was_held: false,
            latched: false,
            coasting: false,
        }
    }

    /// Whether the gesture is engaged, resolving `latch` against the raw button state.
    fn engaged(&mut self, held: bool) -> bool {
        let pressed = held && !self.was_held;
        self.was_held = held;

        if self.mode == Mode::Joystick && self.latch {
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
    }
}

impl Stage for Scroll {
    fn name(&self) -> &'static str {
        "scroll"
    }

    fn process(&mut self, s: &mut Sample) {
        if self.mode == Mode::Off {
            return;
        }
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

        let active = self.engaged(s.scrolling);
        if !active {
            if self.coasting || self.offset_x != 0.0 || self.offset_y != 0.0 {
                self.release();
            }
            return;
        }

        let sign = if self.invert { -1.0 } else { 1.0 };

        match self.mode {
            Mode::Off => {}
            Mode::Drag => {
                let per_unit = if self.mm_per_unit.is_finite() && self.mm_per_unit > 0.0 {
                    self.mm_per_unit
                } else {
                    4.0
                };
                // Scrolling *content*: dragging down moves the page up, which is why the
                // vertical term is negated before `invert` is applied to it.
                self.carry_x += s.dx / per_unit * sign;
                self.carry_y += -s.dy / per_unit * sign;
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

                if distance > deadzone && distance > 0.0 && s.dt.is_finite() && s.dt > 0.0 {
                    // Speed grows with displacement *past* the deadzone, so the gesture
                    // starts from a standstill rather than jumping to a rate the moment the
                    // edge is crossed.
                    let beyond = distance - deadzone;
                    let rate = beyond * gain * s.dt;
                    self.carry_x += self.offset_x / distance * rate * sign;
                    self.carry_y += -self.offset_y / distance * rate * sign;
                    self.coasting = true;
                } else {
                    self.coasting = false;
                }
            }
        }

        s.scroll_x += self.carry_x;
        s.scroll_y += self.carry_y;
        self.carry_x = 0.0;
        self.carry_y = 0.0;

        // The whole point: the hand was steering a page, not a cursor.
        s.dx = 0.0;
        s.dy = 0.0;
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
        // Unsettled only while a velocity is running with nothing driving it. A drag is
        // settled between samples — it produces nothing without hand movement — so claiming
        // otherwise would keep the daemon ticking for a gesture that has nothing to add.
        !self.coasting
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
    fn off_is_byte_identical_pass_through() {
        let mut stage = Scroll::new(Mode::Off);
        let mut s = Sample::new(1.5, -2.5, 1000, true);
        s.scrolling = true;
        let before = s;
        stage.process(&mut s);
        assert_eq!(s, before, "the default must not touch anything");
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
        stage.mm_per_unit = 4.0;
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
        stage.mm_per_unit = 4.0;
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
    fn nothing_panics_on_pathological_input() {
        for mode in [Mode::Drag, Mode::Joystick] {
            let mut stage = Scroll::new(mode);
            stage.mm_per_unit = f64::NAN;
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
