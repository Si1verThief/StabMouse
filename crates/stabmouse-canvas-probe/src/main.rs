//! Slint scratch-canvas probe (P3).
//!
//! Answers: can Slint host a responsive drawing canvas, does it look acceptable,
//! and what input rate does a GUI toolkit actually observe?
//!
//! Also a first taste of pulled-string stabilisation with both parameters live.
//! Filter maths is inline and throwaway — the real implementation belongs in
//! `stabmouse-core`.
//!
//! Two things learned here and reflected in the code:
//!
//! - **Pressure velocity is measured on the drawn point, not the cursor.** Taking it
//!   from the cursor produces blobs on direction changes: the cursor decelerates so
//!   pressure peaks, while the stabiliser anchor is still travelling fast. This is
//!   exactly why `pressure` is pinned last in the pipeline (see stages.md).
//! - **Do not restamp a point that has not moved.** The same lesson as P1c on the
//!   output side: redundant output is not merely wasteful, it is visibly wrong.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

slint::include_modules!();

const W: usize = 1180;
const H: usize = 780;

/// Probe-grade pressure shaping, matching the tablet probe.
const ATTACK_MS: f64 = 60.0;
const V_MAX_PX_S: f64 = 3000.0;
const MIN_PRESSURE: f64 = 0.05;

/// Below this, the drawn point has not meaningfully moved and stamping again would
/// only deposit ink in place.
const MIN_STAMP_PX: f64 = 0.35;

/// Output travel required before the velocity estimate is updated. Below this the
/// point counts as stalled — which happens legitimately whenever the cursor moves
/// *inside* the stabiliser radius.
const STALL_PX: f64 = 1.5;

/// A stall lasting longer than this discards its accumulated time rather than
/// eventually dividing by it, which would reproduce the hotspot on resumption.
const STALL_TIMEOUT_S: f64 = 0.12;

struct State {
    buf: Vec<u8>,
    dirty: bool,

    down: bool,
    stroke_started: Instant,

    raw: Option<(f64, f64)>,
    anchor: Option<(f64, f64)>,
    /// Last point ink was actually laid from. Displacement accumulates against this,
    /// NOT against the previous sample -- see the comment at the draw site.
    ink_from: Option<(f64, f64)>,
    ghost_from: Option<(f64, f64)>,
    last_move: Instant,
    /// Low-passed speed of the *drawn* point, in px/s.
    speed: f64,
    /// Distance and time banked since the velocity estimate last updated.
    accum_dist: f64,
    accum_time: f64,
    pressure: f64,

    stroke_ending: bool,
    motion_events: u64,
    events_window: u64,
    window_start: Instant,
    in_rate: f64,
    frames: u64,
    frame_rate: f64,
    worst_process_us: f64,
}

impl State {
    fn new() -> Self {
        Self {
            buf: vec![0xff; W * H * 4],
            dirty: true,
            down: false,
            stroke_started: Instant::now(),
            raw: None,
            anchor: None,
            ink_from: None,
            ghost_from: None,
            last_move: Instant::now(),
            speed: 0.0,
            accum_dist: 0.0,
            accum_time: 0.0,
            pressure: 0.0,
            stroke_ending: false,
            motion_events: 0,
            events_window: 0,
            window_start: Instant::now(),
            in_rate: 0.0,
            frames: 0,
            frame_rate: 0.0,
            worst_process_us: 0.0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0xff);
        self.raw = None;
        self.anchor = None;
        self.ink_from = None;
        self.ghost_from = None;
        self.dirty = true;
        self.worst_process_us = 0.0;
    }

    fn stamp(&mut self, x: f64, y: f64, r: f64, colour: [u8; 3]) {
        let r = r.max(0.5);
        let x0 = ((x - r).floor() as isize).max(0) as usize;
        let x1 = ((x + r).ceil() as isize).min(W as isize - 1).max(0) as usize;
        let y0 = ((y - r).floor() as isize).max(0) as usize;
        let y1 = ((y + r).ceil() as isize).min(H as isize - 1).max(0) as usize;

        for py in y0..=y1 {
            for px in x0..=x1 {
                let dx = px as f64 + 0.5 - x;
                let dy = py as f64 + 0.5 - y;
                let d = (dx * dx + dy * dy).sqrt();
                if d > r {
                    continue;
                }
                let a = ((r - d).min(1.0)).max(0.0);
                let i = (py * W + px) * 4;
                for c in 0..3 {
                    let dst = self.buf[i + c] as f64;
                    self.buf[i + c] = (dst * (1.0 - a) + colour[c] as f64 * a) as u8;
                }
            }
        }
        self.dirty = true;
    }

    fn stroke(&mut self, from: (f64, f64), to: (f64, f64), r: f64, colour: [u8; 3]) {
        let dist = (to.0 - from.0).hypot(to.1 - from.1);
        let steps = (dist / (r * 0.4).max(0.6)).ceil().max(1.0) as usize;
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            self.stamp(
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t,
                r,
                colour,
            );
        }
    }
}

/// `count_rate` is false for button events, so the input-rate figure reflects
/// genuine motion only.
fn handle_input(
    state: &Rc<RefCell<State>>,
    win: &MainWindow,
    x: f32,
    y: f32,
    pressed: bool,
    count_rate: bool,
) {
    let t_enter = Instant::now();
    let radius = win.get_radius() as f64;
    let catch_up = win.get_catch_up() as f64;
    let ghost = win.get_ghost();
    let hold_stalled = win.get_hold_stalled();
    let vel_tau = (win.get_vel_smoothing() as f64 / 1000.0).max(1e-4);
    let vel_from_cursor = win.get_vel_from_cursor();

    let mut s = state.borrow_mut();
    if count_rate {
        s.motion_events += 1;
        s.events_window += 1;
    }

    let x = x as f64;
    let y = y as f64;

    let now = Instant::now();
    let dt = now.duration_since(s.last_move).as_secs_f64().clamp(1e-4, 0.25);
    s.last_move = now;

    // Stroke edges.
    if pressed && !s.down {
        s.stroke_started = now;
        s.anchor = Some((x, y));
        s.ink_from = Some((x, y));
        s.speed = 0.0;
        s.accum_dist = 0.0;
        s.accum_time = 0.0;
    }
    if !pressed && s.down {
        // Anchor snaps to the cursor on stroke end, per stages.md.
        s.anchor = Some((x, y));
        s.stroke_ending = true;
    }
    s.down = pressed;

    // Pulled string: the anchor is dragged only once the cursor exceeds `radius`,
    // then lerped toward the boundary by `catch_up`.
    let prev_anchor = s.anchor;
    if let Some((ax, ay)) = s.anchor {
        let (dx, dy) = (x - ax, y - ay);
        let dist = dx.hypot(dy);
        if dist > radius && dist > 1e-9 {
            let tx = x - dx / dist * radius;
            let ty = y - dy / dist * radius;
            s.anchor = Some((ax + (tx - ax) * catch_up, ay + (ty - ay) * catch_up));
        }
    } else {
        s.anchor = Some((x, y));
    }

    // Speed of the DRAWN point, not the cursor. Using the cursor here produces
    // blobs on direction changes.
    let anchor_delta = match (prev_anchor, s.anchor) {
        (Some(a), Some(b)) => (b.0 - a.0).hypot(b.1 - a.1),
        _ => 0.0,
    };

    // The anchor only advances along the RADIAL direction, so tangential cursor
    // motion barely moves it even at full hand speed. That is why `cursor` is a
    // live alternative here rather than a discarded idea -- see stages.md.
    let cursor_delta = match s.raw {
        Some((px, py)) => (x - px).hypot(y - py),
        None => 0.0,
    };
    let vel_delta = if vel_from_cursor { cursor_delta } else { anchor_delta };

    if hold_stalled {
        // Accumulate distance and time, updating only once the point has genuinely
        // travelled. Slow-but-continuous motion still reports a low speed; a true
        // stall never reaches the threshold, so the estimate holds rather than being
        // dragged to zero and read as "maximum pressure".
        s.accum_dist += vel_delta;
        s.accum_time += dt;
        if s.accum_dist >= STALL_PX {
            let raw_speed = s.accum_dist / s.accum_time;
            let alpha = 1.0 - (-s.accum_time / vel_tau).exp();
            s.speed += alpha * (raw_speed - s.speed);
            s.accum_dist = 0.0;
            s.accum_time = 0.0;
        } else if s.accum_time > STALL_TIMEOUT_S {
            s.accum_dist = 0.0;
            s.accum_time = 0.0;
        }
    } else {
        // `decay`: the naive form, for comparison.
        let raw_speed = vel_delta / dt;
        let alpha = 1.0 - (-dt / vel_tau).exp();
        s.speed += alpha * (raw_speed - s.speed);
        s.accum_dist = 0.0;
        s.accum_time = 0.0;
    }

    let pressure = if pressed {
        let elapsed_ms = now.duration_since(s.stroke_started).as_secs_f64() * 1000.0;
        let envelope = (elapsed_ms / ATTACK_MS).min(1.0);
        let speed_term = (1.0 - s.speed / V_MAX_PX_S).clamp(0.0, 1.0);
        (envelope * speed_term).max(MIN_PRESSURE)
    } else {
        0.0
    };
    s.pressure = pressure;

    // Accumulate against the last point we actually DREW FROM, not against the
    // previous sample.
    //
    // A per-sample threshold silently loses slow-but-continuous motion: with a low
    // catch-up the anchor advances a fraction of a pixel per sample, so every
    // individual step fails the threshold while the total displacement is large. The
    // result is an output point that visibly travels without drawing anything.
    //
    // This is the same hazard as subpixel truncation in the quantizer and as banked
    // time in the pressure stall handler. Threshold the ACCUMULATED quantity.
    if ghost {
        let from = s.ghost_from.or(s.raw);
        if let Some(from) = from {
            if (x - from.0).hypot(y - from.1) >= MIN_STAMP_PX {
                s.stroke(from, (x, y), 1.0, [200, 200, 210]);
                s.ghost_from = Some((x, y));
            }
        } else {
            s.ghost_from = Some((x, y));
        }
    }

    if pressed {
        if let (Some(from), Some(to)) = (s.ink_from.or(prev_anchor), s.anchor) {
            let travelled = (to.0 - from.0).hypot(to.1 - from.1);
            if travelled >= MIN_STAMP_PX {
                let width = 1.0 + pressure * 11.0;
                s.stroke(from, to, width, [24, 24, 32]);
                s.ink_from = Some(to);
            }
            // Otherwise keep `ink_from` where it is so the displacement keeps banking.
        }
    }

    // Flush the un-drawn tail on release, or the end of every stroke is lost.
    if s.stroke_ending {
        if let (Some(from), Some(to)) = (s.ink_from, s.anchor) {
            if (to.0 - from.0).hypot(to.1 - from.1) > 0.0 {
                let width = 1.0 + pressure.max(MIN_PRESSURE) * 11.0;
                s.stroke(from, to, width, [24, 24, 32]);
            }
        }
        s.ink_from = None;
        s.stroke_ending = false;
    }

    s.raw = Some((x, y));

    let us = t_enter.elapsed().as_secs_f64() * 1e6;
    if us > s.worst_process_us {
        s.worst_process_us = us;
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    let state = Rc::new(RefCell::new(State::new()));

    {
        let state = state.clone();
        let handle = window.as_weak();
        window.on_moved(move |x, y, pressed| {
            if let Some(win) = handle.upgrade() {
                handle_input(&state, &win, x, y, pressed, true);
            }
        });
    }

    {
        let state = state.clone();
        let handle = window.as_weak();
        window.on_button(move |x, y, pressed| {
            if let Some(win) = handle.upgrade() {
                handle_input(&state, &win, x, y, pressed, false);
            }
        });
    }

    {
        let state = state.clone();
        window.on_clear(move || state.borrow_mut().clear());
    }

    // Render on a timer, decoupling draw rate from input rate so the two can be
    // measured separately.
    let timer = Timer::default();
    {
        let state = state.clone();
        let handle = window.as_weak();
        timer.start(
            TimerMode::Repeated,
            std::time::Duration::from_millis(16),
            move || {
                let Some(win) = handle.upgrade() else { return };
                let mut s = state.borrow_mut();

                s.frames += 1;
                let elapsed = s.window_start.elapsed().as_secs_f64();
                if elapsed >= 1.0 {
                    s.in_rate = s.events_window as f64 / elapsed;
                    s.frame_rate = s.frames as f64 / elapsed;
                    s.events_window = 0;
                    s.frames = 0;
                    s.window_start = Instant::now();
                }

                win.set_stats(
                    format!(
                        "motion {:>5.0}/s   draw {:>5.1}fps   total {:<8}  worst handler {:>6.1}us",
                        s.in_rate, s.frame_rate, s.motion_events, s.worst_process_us
                    )
                    .into(),
                );
                win.set_pressure(s.pressure as f32);

                if s.dirty {
                    let pb = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                        &s.buf,
                        W as u32,
                        H as u32,
                    );
                    win.set_canvas(Image::from_rgba8(pb));
                    s.dirty = false;
                }
            },
        );
    }

    window.run()
}
