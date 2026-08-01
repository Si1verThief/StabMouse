//! The hot loop: read a report, run it through the active mode's pipeline, emit.

use crate::control::{Command, Listener};
use crate::modes::{Action, Applied, Modes, Switch};
use crate::notify;
use crate::signals::Signals;
use crate::wait::{self, Wake};
use stabmouse_config::Output;
use stabmouse_core::{Quantizer, Sample};
use stabmouse_input::{Capture, Report};
use stabmouse_output::{MouseSink, SurfaceMapper};
use std::time::{Duration, Instant};

/// Interval between synthesised zero-motion ticks while anything is outstanding.
///
/// Measured (docs/stages.md): sample gaps are p50 1ms / p90 3ms / p99 22ms, and against a
/// 60ms attack a 2ms tick advances the envelope in 0.033 steps. At 4ms the steps were visible
/// in the metrics. Most real gaps are already under 2ms, so this rarely fires.
const TICK: Duration = Duration::from_micros(2_000);

/// Fallback interval for checking the config directory's mtime.
///
/// Only used when a watch could not be established — watches are a finite kernel resource and
/// can be refused. With one in place, edits arrive as events and this never runs.
const CONFIG_POLL: Duration = Duration::from_millis(400);

/// Indices into the poll set.
const DEVICE: usize = 0;
const CONTROL: usize = 1;
const CONFIG: usize = 2;

/// Time floor before silence is even considered worth reporting.
const STALL_REPORT: Duration = Duration::from_millis(600);

/// Net hand travel, in millimetres, that no legitimate filter can swallow.
///
/// **Time alone cannot tell a stall from a filter working**, which is what the first version
/// got wrong: it fired on every slow stroke with aggressive smoothing. A pulled string rests up
/// to a full radius behind the cursor permanently and by design — that is the filter, not a
/// backlog — so at 5mm/s a 9mm radius is nearly two seconds of entirely correct silence, and no
/// duration distinguishes it from a fault.
///
/// Net displacement does. Whatever a filter holds back is bounded by its own parameters, and
/// the heaviest shipped preset rests 9mm behind; travel this far in one direction with nothing
/// emitted is beyond anything a filter would explain. Net rather than path length, because
/// circling inside a stabiliser's radius covers distance while going nowhere, and that is
/// exactly the case that must not report.
const STALL_TRAVEL_MM: f64 = 25.0;

/// How long the hand must be still before a compositor cursor report is adopted into the
/// shared position. Our own emissions echo back through the report a few pixels stale, and
/// adopting an echo mid-motion would drag the position backwards; motion from a device we do
/// not manage keeps reporting after our emissions stop, so it survives this gate.
const ADOPT_IDLE: Duration = Duration::from_millis(150);

/// How often to look for a source device that has gone away.
///
/// A second is imperceptible against unplugging and replugging a mouse, and enumeration is
/// not free — it opens every node under `/dev/input` — so this is deliberately not eager.
const RECONNECT_POLL: Duration = Duration::from_millis(1000);

/// How long the daemon watches for a button after a frontend asks it to.
///
/// Long enough to reach for an awkward thumb button, short enough that a window closed
/// mid-bind does not leave the mouse swallowing a click a minute later.
const CAPTURE_WINDOW: Duration = Duration::from_secs(8);

pub struct Runtime {
    /// The source device, absent while it is disconnected.
    ///
    /// `None` is a normal running state, not a failure: a wireless mouse that goes to sleep
    /// takes its event node with it, and the daemon waits rather than dying.
    ///
    /// Dying would take the virtual tablets with it, and an application *started* while they
    /// are absent gets no pressure for its entire lifetime (D13) — Krita initialises tablet
    /// support once and never retries. Applications already running are unharmed and re-hook
    /// when the tablet returns (P1b), so the cost is a window in which launching a drawing
    /// application silently produces a session with no pressure.
    pub capture: Option<Capture>,
    pub control: Listener,
    pub mouse: MouseSink,
    /// The fallback transport: absolute, fed from the mapper, so the cursor cannot diverge
    /// between the tablet and the fallback (P6/D22). `None` when the screen layout is unknown
    /// or creation failed — the relative fallback below still works, it merely teleports.
    pub pointer: Option<stabmouse_output::PointerSink>,
    pub tablets: crate::tablets::Tablets,
    pub modes: Modes,
    pub quantizer: Quantizer,
    /// Fallback for when the screen layout is unknown, so one surface covers the desktop.
    pub mapper: SurfaceMapper,
    /// Which source button counts as pen-down in tablet mode.
    pub pen_button: u16,
    pub verbose: bool,
    /// Set by `handle` whenever a non-zero movement actually reached a sink.
    pub emitted_motion: bool,
    /// Net source movement, in device counts, since anything last reached a sink. Compared
    /// against [`STALL_TRAVEL_MM`] to tell a stalled daemon from a filter doing its job.
    pub stall_dx: f64,
    pub stall_dy: f64,
    /// When a sink last carried motion, gating cursor-report adoption — see [`ADOPT_IDLE`].
    pub last_emit: Instant,
    /// Proof of life for the watchdog. Marked around each unit of work, never around the
    /// blocking wait — an idle daemon is not a wedged one.
    pub beat: crate::watchdog::Heartbeat,
    /// Keyboards watched for a constrain modifier, when a mode binds a keyboard key.
    ///
    /// `None` unless some mode asks for one — a mouse-button binding needs no keyboard opened
    /// at all, and an unopened device is the strongest privacy guarantee available.
    pub listener: Option<stabmouse_input::Listener>,
    /// The codes the listener was last opened for, so a reload only reopens on a real change.
    pub watched_keys: Vec<u16>,
    /// Whether the current mode's modifier is a mouse button that is currently held.
    ///
    /// Tracked here rather than read from the report, because a button's state persists
    /// between reports while `report.keys` carries only transitions.
    pub buttons_held: Vec<u16>,
    /// Whether to freeze in an application neither named nor on the built-in list.
    pub freeze_while_scrolling: bool,
    /// User entries naming applications that do or do not need the freeze.
    pub scroll_freeze_overrides: Vec<(String, bool)>,
    /// How long the freeze outlasts the last wheel event.
    pub scroll_freeze: Duration,
    /// When the current freeze ends, if one is running.
    pub frozen_until: Option<Instant>,
    /// A profile switch waiting for the next reload to pick it up.
    pub requested_profile: Option<String>,
    /// While set, the next button press is reported rather than acted on.
    pub capture_until: Option<Instant>,
    /// The captured button's name, waiting for a frontend to collect it.
    pub captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Buttons whose releases must be swallowed, their presses having been swallowed.
    pub swallow_release: Vec<u16>,
    /// Names captured so far in the current chord.
    pub capture_chord: Vec<String>,
    /// How many captured buttons are still held, so the chord knows when it is finished.
    pub capture_down: usize,
    /// Fractional scroll owed to each wheel axis, kept separately per resolution.
    ///
    /// The `scroll` stage produces fractional notches, and both wheel axes are integers with
    /// *different* scales — whole notches and 120ths. Truncating each sample independently is
    /// the accumulation hazard from modules.md: at 1000Hz every step is far below a notch, so
    /// a slow drag would scroll nothing at all.
    pub scroll_carry: ScrollCarry,
    /// When a scroll event last left, so a gesture emits at a rate an application can follow.
    pub last_scroll_emit: Instant,
    pub last_tablet_xy: (i32, i32),
    /// State the D-Bus service reads. Written here, never read by the loop itself.
    pub published: crate::service::Published,
    /// The focused application, as reported by the compositor.
    pub focus: crate::focus::Focus,
    /// The transport in use last time a sample was routed, for reporting changes.
    pub last_transport: Option<Output>,
    /// User entries overriding the built-in tablet-support table, from config.
    pub tablet_overrides: Vec<(String, bool)>,
    /// Opt-in `auto_activate` rules from the profile. Empty means no auto-switching, which is
    /// the default and the safe one.
    pub auto: crate::auto::AutoSwitch,
    /// In tablet mode, also emit ordinary mouse buttons alongside the pen.
    ///
    /// KWin only turns a pen tip into a click for clients that speak `tablet_v2`. Everything
    /// else — browsers, file managers, this project's own settings window — receives nothing,
    /// so without this tablet mode can move the cursor but not press anything.
    ///
    /// Safe to pair with the pen because the compositor moves the *global* cursor for tablet
    /// motion, so a relative button press lands exactly where the pen is. Verified by probe:
    /// hovering with a tablet and clicking with a mouse hits the hovered target.
    pub tablet_clicks: bool,
    /// Whether filtering is stopped and the grab released.
    ///
    /// A field rather than a local passed between methods. It used to be the latter, and
    /// `announce` — which has no way to know it — passed a literal `false` when publishing a
    /// mode switch. Switching mode while panicked therefore told every client that filtering
    /// had resumed, and the enable button then asked for a state the daemon was already in and
    /// did nothing, leaving no way back. One home for the value makes that unrepresentable.
    pub inert: bool,
}

/// The parts of a reloaded config the running daemon has to adopt.
///
/// More than the modes, because tablet teardown is a profile-level choice and editing it
/// should take effect the same way editing a preset does.
pub struct Reloaded {
    pub modes: Modes,
    pub destroy_tablet_on_leave: bool,
    pub tablet_emits_mouse_clicks: bool,
    pub freeze_position_while_scrolling: bool,
    pub scroll_freeze_ms: u64,
    pub scroll_freeze: Vec<(String, bool)>,
    /// What the reload settled on, so the published state names the live profile rather than
    /// the one the daemon happened to start with.
    pub profile_slug: String,
    pub profile_name: String,
}

/// Fractional scroll owed, per axis and per resolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollCarry {
    notch_x: f64,
    notch_y: f64,
    hi_x: f64,
    hi_y: f64,
}

/// Whole-wheel and hi-res values to emit for one report.
#[derive(Debug, Clone, Copy, Default)]
struct ScrollOut {
    notch_x: i32,
    notch_y: i32,
    hi_x: i32,
    hi_y: i32,
}

impl ScrollOut {
    fn any(&self) -> bool {
        self.notch_x != 0 || self.notch_y != 0 || self.hi_x != 0 || self.hi_y != 0
    }
}

/// Hi-res wheel units per notch, fixed by the kernel's `REL_WHEEL_HI_RES` contract.
const HI_RES_PER_NOTCH: f64 = 120.0;

/// Shortest interval between scroll events a gesture will emit.
///
/// **A gesture can produce a scroll on every sample, and a mouse never does.** A real wheel
/// emits a handful of events a second — one per notch, or a few dozen hi-res steps — while a
/// swipe at 1000Hz would emit a thousand, each carrying a sliver. Pages that do real work per
/// wheel event cannot keep up: measured in use, Google Search and Maps stayed smooth while
/// YouTube and WhatsApp stuttered, which is the signature of the *handler* being the
/// bottleneck rather than the scrolling.
///
/// So the gesture accumulates and emits fewer, larger events. 8ms is faster than any display
/// refreshes, so nothing is lost visually, and it cuts the event rate by roughly eight.
/// Nothing is dropped: whatever accrues between emissions is carried into the next one.
const SCROLL_INTERVAL: Duration = Duration::from_millis(8);

/// Which signal a state change should announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Mode,
    Enabled,
    Config,
}

#[derive(Default)]
pub struct Stats {
    pub reports: u64,
    pub ticks: u64,
    pub switches: u64,
    pub worst_process: Duration,
}

impl Runtime {
    pub fn run(
        &mut self,
        signals: &Signals,
        mut reload: impl FnMut(Option<&str>) -> Option<Reloaded>,
        mut reopen: impl FnMut() -> Option<Capture>,
        config_dir: Option<std::path::PathBuf>,
    ) -> anyhow::Result<Stats> {
        // Device and control socket in one wait, so a command wakes the loop as promptly as a
        // mouse report does. This is why control is not on signals: glibc installs handlers
        // with `SA_RESTART`, so a signal restarts `poll` instead of interrupting it and a
        // command would sit unseen until the timeout expired.
        // A watch on the config directory turns editing a preset into an event instead of
        // something to poll for. Failure is not fatal: the mtime poll below still works, and a
        // slower reload beats a daemon that will not start.
        let mut watcher = config_dir
            .as_deref()
            .and_then(|dir| match crate::watch::Watcher::new(dir) {
                Ok(w) => Some(w),
                Err(e) => {
                    eprintln!("watching {} failed ({e}); falling back to polling", dir.display());
                    None
                }
            });

        // A negative descriptor is skipped by `poll(2)` with `revents` cleared, which is how
        // the device slot stays at a fixed index while the device itself comes and goes. The
        // alternative — rebuilding the set — would shift every other index under it.
        let mut fds = vec![self.capture.as_ref().map_or(-1, |c| c.raw_fd()), self.control.raw_fd()];
        if let Some(w) = &watcher {
            fds.push(w.raw_fd());
        }
        // Watched keyboards occupy the tail of the poll set, so their indices are known only
        // once the optional config watcher has or has not claimed slot 2.
        let keyboards_from = fds.len();
        if let Some(l) = &self.listener {
            fds.extend(l.raw_fds());
        }

        // Announced once at startup rather than left for a client to discover. Tablets that
        // were not confined to their screens still work, but they cover the whole desktop —
        // which looks like a precision problem rather than a placement one, so it has to be
        // said out loud.
        if self.tablets.per_screen_active() && !self.tablets.placed() {
            let reason = "tablets are not confined to their screens; \
                          they cover the whole desktop until mapped in display settings";
            self.published.publish(|s| {
                s.degraded = stabmouse_ipc::Degraded {
                    degraded: true,
                    reason: reason.to_string(),
                };
            });
            self.published.announce(crate::service::Announce::Degraded);
        }

        let mut report = Report::default();
        let mut stats = Stats::default();
        let mut last_config_check = Instant::now();
        let mut last_mtime = config_dir.as_deref().and_then(newest_mtime);
        // Set by an explicit `Reload`, which must work whether or not the files changed —
        // asking for a reload and being told nothing happened is not a useful answer.
        let mut reload_requested = false;
        // Tracked so the transition is acted on once, rather than on every pass.
        let mut was_falling_back = false;
        let mut stroke_active = false;
        // Stall detection: input arriving with nothing emitted means something is wrong that
        // the filters cannot explain.
        let mut input_since: Option<Instant> = None;
        let mut stall_reported = false;
        // Continues the source's own timestamp sequence, so synthesised ticks never make time
        // appear to jump.
        let mut last_t_us: u64 = 0;

        while !signals.should_shutdown() {
            let outstanding =
                stroke_active || self.modes.current().is_some_and(|m| !m.pipeline.settled());
            // Indefinite when nothing is outstanding *and* a watch is doing the noticing:
            // there is then no reason to ever wake a daemon nobody is using, which is what
            // makes idle cost genuinely zero rather than 2.5 wakeups a second.
            let deadline = if self.capture.is_none() {
                // Something has to wake the loop to look for the device, since the thing that
                // would normally wake it is the thing that is missing.
                Some(RECONNECT_POLL)
            } else if outstanding {
                Some(TICK)
            } else if watcher.is_some() {
                None
            } else {
                Some(CONFIG_POLL)
            };

            let wake = wait::wait_any(&fds, deadline);
            if wake == Wake::Interrupted {
                continue;
            }

            // Everything from here to the end of the iteration is work the watchdog holds to a
            // deadline, and the guard ends it however this iteration ends. The blocking wait
            // above deliberately sits outside it: an idle daemon may stay in `poll`
            // indefinitely and that is the correct resting state, not a wedge. See watchdog.rs.
            let _work = self.beat.work();

            // Look for the device while it is away. Enumeration opens every node under
            // /dev/input, so it is not free — but it only runs while disconnected, and once
            // per interval rather than per wakeup.
            if self.capture.is_none() {
                if let Some(capture) = reopen() {
                    self.regain_device(capture, &mut fds);
                }
            }

            // Modifier state before anything is filtered, so a constraint engages on the same
            // report the user's press arrived in rather than one late.
            if let Some(listener) = &mut self.listener {
                for i in 0..listener.raw_fds().len() {
                    if wake.has(keyboards_from + i) {
                        listener.drain(i);
                    }
                }
            }

            // Control first: a queued switch should take effect before the next batch of motion
            // is filtered, not after it.
            if wake.has(CONTROL) {
                for command in self.control.drain() {
                    match command {
                        Command::Quit => return Ok(stats),
                        Command::Panic => self.set_inert(!self.inert),
                        Command::SetEnabled(on) => self.set_inert(!on),
                        Command::Status => self.print_status(),
                        // Forcing the mtime to differ is what makes the existing poll pick it
                        // up on its next pass, so reload has exactly one implementation.
                        Command::Reload => reload_requested = true,
                        Command::Cycle => self.act(Action::Cycle, &mut stats, stroke_active),
                        Command::CyclePrev => {
                            self.act(Action::CyclePrev, &mut stats, stroke_active)
                        }
                        Command::Flip => self.act(Action::Flip, &mut stats, stroke_active),
                        Command::Mode(n) => {
                            self.act(Action::Select(n), &mut stats, stroke_active)
                        }
                        // Handled through the same reload the config watch uses, so there is
                        // one path that rebuilds modes and swaps them in rather than two that
                        // could disagree about what a rebuild involves.
                        Command::Profile(name) => {
                            self.requested_profile = Some(name);
                            reload_requested = true;
                        }
                        Command::CaptureBinding => {
                            self.end_capture();
                            self.capture_until = Some(Instant::now() + CAPTURE_WINDOW);
                        }
                        // A capture the frontend finished by other means — a keyboard-only
                        // chord, or Escape. Without this the daemon went on swallowing every
                        // mouse event until the window expired, which reads as the mouse
                        // being stuck in binding mode.
                        Command::CancelCapture => self.end_capture(),
                    }
                }
            }

            // Only read the device when the device is what woke us. Reading it otherwise blocks
            // until the user happens to move the mouse, stranding every queued command behind
            // it — which is exactly how one switch landed out of seven.
            if wake.has(DEVICE) {
                // A read failure means the device is gone — unplugged, or a wireless mouse
                // asleep. Losing it is not a reason to exit: the process dying would take the
                // virtual tablets with it, and every application holding one loses pressure
                // until it restarts (D13). So the source is dropped and waited for.
                // Collected, and any failure reduced to a message, before the loop touches
                // `self` again: the iterator borrows the capture that `lose_device` replaces.
                let read: std::result::Result<Vec<_>, String> = match self.capture.as_mut() {
                    Some(capture) => match capture.read() {
                        Ok(events) => Ok(events.collect()),
                        Err(e) => Err(e.to_string()),
                    },
                    None => Ok(Vec::new()),
                };
                let events = match read {
                    Ok(events) => events,
                    Err(why) => {
                        self.lose_device(&why, &mut fds, &mut stroke_active);
                        continue;
                    }
                };
                for event in &events {
                    if !report.accumulate(event) {
                        continue;
                    }
                    let t0 = Instant::now();
                    last_t_us = report.t_us.max(last_t_us.saturating_add(1));

                    if self.inert {
                        // Track the button anyway, so a deferred switch can still resolve.
                        self.track_button(&report, &mut stroke_active);
                    } else {
                        self.handle(&report, last_t_us, &mut stroke_active)?;
                    }

                    let took = t0.elapsed();
                    if took > stats.worst_process {
                        stats.worst_process = took;
                    }
                    stats.reports += 1;
                    report.clear();

                    if !stroke_active {
                        self.flush_deferred_switch(&mut stats);
                    }
                }

                // Track input-in versus motion-out so a genuine freeze is visible in the log.
                if events.iter().any(|e| e.event_type() == evdev::EventType::RELATIVE) {
                    let now = Instant::now();
                    let since = *input_since.get_or_insert(now);
                    let travelled_mm = {
                        let per_mm = self.quantizer.counts_per_mm().max(f64::MIN_POSITIVE);
                        self.stall_dx.hypot(self.stall_dy) / per_mm
                    };
                    if self.emitted_motion {
                        input_since = None;
                        stall_reported = false;
                        self.emitted_motion = false;
                    } else if !stall_reported
                        && now.duration_since(since) > STALL_REPORT
                        && travelled_mm > STALL_TRAVEL_MM
                    {
                        stall_reported = true;
                        let mode = self
                            .modes
                            .current()
                            .map(|m| format!("{} ({:?})", m.name, m.output))
                            .unwrap_or_else(|| "none".into());
                        eprintln!(
                            "STALL: {travelled_mm:.0}mm of hand movement over {:.0}ms with no \
                             motion emitted. mode {} = {mode}, \
                             inert={}, grabbed={}, stages=[{}]",
                            now.duration_since(since).as_millis(),
                            self.modes.current_index() + 1,
                            self.inert,
                            self.capture.as_ref().is_some_and(|c| c.is_grabbed()),
                            self.modes
                                .current()
                                .map(|m| m.pipeline.stage_names().collect::<Vec<_>>().join(", "))
                                .unwrap_or_default()
                        );
                    }
                }
            }

            // A compositor cursor report re-syncs the shared position when something we do
            // not manage moved it — a second mouse, a touchpad, or a relative mode's raw
            // deltas (D23). Gated on the hand being still, and never while a pen is in
            // proximity: the pen owns the cursor then, and the report describes the pointer's
            // cursor, which the pen does not move (P6).
            if let Some((x, y)) = self.focus.take_external_cursor() {
                if !self.tablets.any_in_proximity() && self.last_emit.elapsed() >= ADOPT_IDLE {
                    self.tablets.adopt_position_px(x, y);
                }
            }

            // Opt-in, and a no-op for a profile with no rules. Outside the stroke because a
            // line crossing a window boundary is not a request to change mode, and while
            // filtering because a panicked daemon should change nothing at all.
            if !self.inert && !stroke_active {
                self.auto_switch(&mut stats);
            }

            // Entering a fallback must lift the pen, exactly as leaving tablet output does.
            // A tool left in proximity over an application that cannot see it keeps the
            // compositor believing a pen is present.
            let falling_back = self.falling_back();
            if falling_back != was_falling_back {
                was_falling_back = falling_back;
                if falling_back {
                    self.tablets.leave_all();
                }
                self.publish(Change::Mode);
            }

            if wake == Wake::Timeout && outstanding && !self.inert {
                last_t_us = last_t_us.saturating_add(TICK.as_micros() as u64);
                let tick = Report {
                    t_us: last_t_us,
                    complete: true,
                    ..Default::default()
                };
                self.handle(&tick, last_t_us, &mut stroke_active)?;
                stats.ticks += 1;
            }

            // Two ways to learn of an edit, and exactly one of them is active. The watch
            // reports the moment a save completes; the poll is the fallback for when no watch
            // could be established.
            let edited = match &mut watcher {
                Some(w) => wake.has(CONFIG) && w.drain(),
                None if last_config_check.elapsed() >= CONFIG_POLL => {
                    last_config_check = Instant::now();
                    let now = config_dir.as_deref().and_then(newest_mtime);
                    let changed = now != last_mtime;
                    last_mtime = now;
                    changed
                }
                None => false,
            };

            if (edited || reload_requested) && config_dir.is_some() {
                reload_requested = false;
                let wanted = self.requested_profile.take();
                // Kept current even on the watch path, so switching to the poll fallback after
                // a failure does not immediately re-fire on a change already handled.
                last_mtime = config_dir.as_deref().and_then(newest_mtime);

                // A failed reload leaves the running config alone rather than dropping to
                // passthrough, so a typo mid-edit cannot take the pointer away.
                if let Some(fresh) = reload(wanted.as_deref()) {
                    self.modes = fresh.modes;
                    self.tablets
                        .set_destroy_on_leave(fresh.destroy_tablet_on_leave);
                    self.tablet_clicks = fresh.tablet_emits_mouse_clicks;
                    self.freeze_while_scrolling = fresh.freeze_position_while_scrolling;
                    self.scroll_freeze_overrides = fresh.scroll_freeze;
                    self.scroll_freeze = Duration::from_millis(fresh.scroll_freeze_ms);
                    let name = self
                        .modes
                        .current()
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| "none".into());

                    // Republish what the reload actually produced. Without this the dashboard
                    // kept naming the profile the daemon started with, however many times it
                    // was switched — the modes changed underneath a readout that did not.
                    let modes = self.modes.infos();
                    let slot = (self.modes.current_index() + 1) as u32;
                    let mode_name = name.clone();
                    let profile_name = fresh.profile_name.clone();
                    let profile_slug = fresh.profile_slug.clone();
                    self.published.publish(move |s| {
                        s.profile = profile_name;
                        s.profile_slug = profile_slug;
                        s.modes = modes;
                        s.mode_slot = slot;
                        s.mode_name = mode_name;
                    });

                    println!(
                        "config reloaded — profile '{}', mode {} ({name})",
                        fresh.profile_slug,
                        self.modes.current_index() + 1
                    );
                    // **Rebuild the keyboard watch.** The listener only ever opens keyboards
                    // for codes some mode actually binds, so a key bound *after* startup was
                    // being watched by nothing at all — the binding was in the file, the
                    // daemon had resolved it, and no device was reporting it.
                    self.rewatch_keyboards(&mut fds, keyboards_from);

                    notify::mode("config reloaded", &name);
                    self.publish(Change::Config);
                }
            }

        }

        Ok(stats)
    }

    /// Panic releases the grab and stops filtering, but **keeps the sinks alive**.
    ///
    /// Releasing the grab is what actually returns the cursor. Tearing the virtual devices down
    /// would additionally cost every running application its tablet pressure until it restarted
    /// (D13) — a worse outcome than the problem panic is solving.
    /// Move to a requested filtering state, returning the state actually reached.
    ///
    /// Takes the target rather than flipping, so a caller that knows what it wants cannot race
    /// itself. Already being in the requested state is success and does nothing — which is what
    /// makes repeated commands safe.
    fn set_inert(&mut self, want_inert: bool) {
        if want_inert == self.inert {
            return;
        }

        if want_inert {
            if let Some(c) = self.capture.as_mut() {
                c.ungrab();
            }
            self.tablets.leave_all();
            self.inert = true;
            eprintln!("PANIC: grab released, filtering stopped. Send panic again to resume.");
            notify::mode("PANIC", "grab released — send panic again to resume");
            self.publish(Change::Enabled);
            return;
        }

        // Resuming can fail — another remapper may have taken the device while we were inert.
        // The published state has to reflect what actually happened, not what was asked for,
        // or a client shows "active" over a mouse nobody is filtering.
        match self.capture.as_mut() {
            // Resuming with no device is not a failure to report — the reconnect path grabs
            // it when it returns, and staying inert would silently ignore the user's request.
            None => {
                self.inert = false;
                eprintln!("resumed; waiting for the device to come back");
                notify::mode("resumed", "waiting for the device");
            }
            Some(capture) => match capture.grab() {
                Ok(()) => {
                    self.inert = false;
                    eprintln!("resumed");
                    notify::mode("resumed", "filtering again");
                }
                Err(e) => {
                    eprintln!("could not re-grab, staying inert: {e}");
                    notify::mode("could not resume", "the device could not be grabbed");
                }
            },
        }
        self.publish(Change::Enabled);
    }

    /// Drop the source device and start waiting for it to return.
    ///
    /// Everything downstream is deliberately left standing: the virtual mouse, the pointer and
    /// the tablets all survive. Applications already holding a tablet would re-hook if it went
    /// and came back (P1b), but one *started* while it is absent gets no pressure at all for
    /// its lifetime (D13) — so the tablets outliving a disconnect is what keeps a badly-timed
    /// application launch from quietly costing a whole session's pressure.
    ///
    /// Only the pen is lifted, since a tool left in proximity while nothing drives it is the
    /// two-cursor hazard from D23.
    fn lose_device(&mut self, why: &str, fds: &mut [std::os::fd::RawFd], stroke_active: &mut bool) {
        // Dropping the capture closes the fd, which is also what releases the grab — the same
        // property the watchdog rests on (P8), here arriving by an ordinary route.
        self.capture = None;
        // A negative descriptor is ignored by `poll`, so the slot stays put while the device
        // is away. Leaving the dead fd in place would wake the loop on POLLERR forever.
        if let Some(slot) = fds.get_mut(DEVICE) {
            *slot = -1;
        }
        *stroke_active = false;
        self.tablets.leave_all();
        self.frozen_until = None;

        eprintln!("source device lost ({why}); waiting for it to come back");
        notify::mode("device disconnected", "waiting for it to return");
        let reason = "the source device is disconnected".to_string();
        self.published.publish(|s| {
            s.degraded = stabmouse_ipc::Degraded {
                degraded: true,
                reason: reason.clone(),
            };
        });
        self.published.announce(crate::service::Announce::Degraded);
    }

    /// Adopt a device that has come back.
    fn regain_device(&mut self, capture: Capture, fds: &mut [std::os::fd::RawFd]) {
        let name = capture.name().to_string();
        let path = capture.path().display().to_string();
        if let Some(slot) = fds.get_mut(DEVICE) {
            *slot = capture.raw_fd();
        }
        self.capture = Some(capture);

        // The gap while it was away is longer than any real one, so filter state describes a
        // world that no longer exists. The pipeline would reach the same conclusion from the
        // timestamp jump, but saying it here does not depend on the returning device's clock
        // agreeing with the old one — which, for a device that has just been re-enumerated,
        // is not something to assume.
        if let Some(mode) = self.modes.current_mut() {
            mode.pipeline.reset();
        }
        self.quantizer.reset();

        eprintln!("source device back: {name} ({path})");
        notify::mode("device reconnected", &name);
        self.published.publish(|s| {
            s.degraded = stabmouse_ipc::Degraded::default();
        });
        self.published.announce(crate::service::Announce::Degraded);
    }

    /// Turn the pipeline's fractional scroll into whole wheel and hi-res values.
    ///
    /// Both resolutions carry their own remainder because they are different quantities: a
    /// notch's worth of hi-res motion has already been reported by the time a whole notch is
    /// owed, and sharing one accumulator would make each starve the other.
    ///
    /// Applications read whichever they understand. Emitting both is what every real mouse
    /// with a hi-res wheel does, and omitting the coarse one loses scrolling entirely in
    /// anything that has not been updated for hi-res.
    fn take_scroll(&mut self, sample: &Sample) -> ScrollOut {
        let (sx, sy) = (sample.scroll_x, sample.scroll_y);
        if sx.is_finite() && sy.is_finite() {
            let c = &mut self.scroll_carry;
            c.notch_x += sx;
            c.notch_y += sy;
            c.hi_x += sx * HI_RES_PER_NOTCH;
            c.hi_y += sy * HI_RES_PER_NOTCH;
        }

        // Accumulate freely, emit rarely. Everything owed stays in the carry until it is sent.
        if self.last_scroll_emit.elapsed() < SCROLL_INTERVAL {
            return ScrollOut::default();
        }
        let c = &mut self.scroll_carry;
        if c.notch_x == 0.0 && c.notch_y == 0.0 && c.hi_x == 0.0 && c.hi_y == 0.0 {
            return ScrollOut::default();
        }
        self.last_scroll_emit = Instant::now();
        let c = &mut self.scroll_carry;

        fn take(carry: &mut f64) -> i32 {
            let whole = carry.trunc();
            if !whole.is_finite() {
                *carry = 0.0;
                return 0;
            }
            *carry -= whole;
            whole.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        }

        ScrollOut {
            notch_x: take(&mut c.notch_x),
            notch_y: take(&mut c.notch_y),
            hi_x: take(&mut c.hi_x),
            hi_y: take(&mut c.hi_y),
        }
    }

    /// A switch the user asked for. Only ever called from command handling, which is what
    /// makes it the right place to record that a rule has been overruled.
    fn act(&mut self, action: Action, stats: &mut Stats, stroke_active: bool) {
        // Whatever the rules think, the user has now said otherwise for this window.
        if !self.auto.is_empty() {
            let class = self.under_position().class;
            self.auto.overrule(&class);
        }
        self.switch(action, stats, stroke_active);
    }

    /// Apply the profile's `auto_activate` rules to whatever is under the position.
    ///
    /// Evaluated on entering a window rather than continuously: a rule is about arriving
    /// somewhere, and re-asserting it every sample would make manual switching impossible.
    ///
    /// Never during a stroke. Drawing a line that happens to cross a window boundary is not a
    /// request to change mode, and the deferral machinery would otherwise queue one for the
    /// moment the stroke ended.
    fn auto_switch(&mut self, stats: &mut Stats) {
        if self.auto.is_empty() {
            return;
        }
        let class = self.under_position().class;
        let Some(slot) = self.auto.entering(&class) else {
            return;
        };
        // Already there — nothing to announce and nothing to do.
        if self.modes.target_for(Action::Select(slot)).is_none() {
            return;
        }

        // Announced unconditionally, because software that changes its own behaviour silently
        // is indistinguishable from software that is malfunctioning.
        println!("auto-switch: {class} → mode {slot}");
        self.switch(Action::Select(slot), stats, false);
    }

    fn switch(&mut self, action: Action, stats: &mut Stats, stroke_active: bool) {
        match self.modes.request(action, stroke_active) {
            Some(Switch::Applied(a)) => self.announce(a, stats),
            Some(Switch::Deferred(target)) => {
                // Announce the intent immediately even though it lands later, so a press never
                // looks as though it was missed.
                notify::mode(
                    &format!("mode {} queued", target + 1),
                    "applies when the stroke ends",
                );
                if self.verbose {
                    eprintln!("mode {} queued until the stroke ends", target + 1);
                }
            }
            None => {
                if self.verbose {
                    eprintln!("nothing to switch to for {action:?}");
                }
            }
        }
    }

    fn flush_deferred_switch(&mut self, stats: &mut Stats) {
        if let Some(a) = self.modes.take_pending() {
            self.announce(a, stats);
        }
    }

    /// Apply the side effects of a switch and make it visible.
    ///
    /// A switch the user cannot see is indistinguishable from one that did not happen, which is
    /// what turns an instant action into "is it frozen, should I press again". The daemon
    /// usually runs in the background, so stdout is not enough.
    fn announce(&mut self, a: Applied, stats: &mut Stats) {
        if a.left_tablet {
            self.tablets.leave_all();
        }
        stats.switches += 1;
        self.publish(Change::Mode);
        let total = self.modes.names().len();
        println!(
            "mode {} -> {} — {} ({:?})",
            a.from + 1,
            a.to + 1,
            a.name,
            a.output
        );
        notify::mode(
            &format!("{}  ({:?})", a.name, a.output),
            &format!("mode {} of {total}", a.to + 1),
        );
    }

    /// Republish everything the bus exposes, and announce what changed.
    ///
    /// Called after any state change rather than on a timer: a client that has to poll to
    /// notice a mode switch cannot show one promptly, and polling is the thing signals exist
    /// to avoid.
    fn publish(&self, changed: Change) {
        let inert = self.inert;
        let slot = (self.modes.current_index() + 1) as u32;
        let name = self
            .modes
            .current()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let infos = self.modes.infos();

        self.published.publish(|s| {
            s.mode_slot = slot;
            s.mode_name = name.clone();
            s.modes = infos;
            s.panicked = inert;
            s.enabled = !inert;
            s.tablets = self.tablets.len() as u32;
            s.tablets_placed = self.tablets.placed();
        });

        // Queued, never emitted here. A D-Bus call from this thread can block on the executor
        // while the executor blocks on the control socket this thread drains — a deadlock that
        // froze the cursor and could only be cleared by killing the process.
        self.published.announce(match changed {
            Change::Mode => crate::service::Announce::Mode,
            Change::Enabled => crate::service::Announce::Enabled,
            Change::Config => crate::service::Announce::Config,
        });
    }

    /// Where this mode's output should actually go right now.
    ///
    /// A tablet mode delivers through the mouse when the focused application cannot receive a
    /// pen, because otherwise its cursor moves and nothing it clicks responds (D18). Pressure
    /// is the only loss, and that application could never have received it.
    fn transport(&self) -> Output {
        self.transport_for(&self.under_position())
    }

    /// The transport for a window already looked up, so a caller that needs the window for
    /// something else does not pay for a second hit-test.
    fn transport_for(&self, under: &crate::focus::Under) -> Output {
        let Some(mode) = self.modes.current() else {
            return Output::Mouse;
        };
        if mode.output != Output::Tablet {
            return mode.output;
        }
        if crate::focus::supports_tablet(under, &self.tablet_overrides) {
            Output::Tablet
        } else {
            Output::Mouse
        }
    }

    /// What is under the daemon's own position — the hover question, answered locally.
    ///
    /// The layout hit-test works on every transport, the pen included, which is what the
    /// compositor's under-the-cursor report could never do (P6). That report remains the
    /// degraded source for when no layout has arrived (D23).
    fn under_position(&self) -> crate::focus::Under {
        if let Some((x, y)) = self.tablets.position_px() {
            if let Some(under) = self.focus.window_at(x, y) {
                return under;
            }
        }
        self.focus.get()
    }

    /// Whether the active mode is currently delivering through its second choice.
    fn falling_back(&self) -> bool {
        self.modes
            .current()
            .is_some_and(|m| m.output == Output::Tablet)
            && self.transport() == Output::Mouse
    }

    fn print_status(&self) {
        let inert = self.inert;
        println!(
            "mode {} — {} ({})",
            self.modes.current_index() + 1,
            self.modes
                .current()
                .map(|m| m.name.as_str())
                .unwrap_or("none"),
            if inert { "PANICKED" } else { "active" }
        );
        for line in self.modes.names() {
            println!("  {line}");
        }
        // Worth stating outright: with teardown enabled the tablet is legitimately absent
        // between strokes, and "no such device" is otherwise indistinguishable from a fault.
        if self.tablets.per_screen_active() {
            println!(
                "  tablets: {} — one per screen, pen on {}",
                self.tablets.len(),
                self.tablets.describe_active()
            );
            // Whether placement took is the difference between per-screen precision and every
            // tablet silently covering the whole desktop, and it is not visible any other way.
            if !self.tablets.placed() {
                println!("           NOT confined to their screens — map them in display settings");
            }
        } else {
            println!("  tablet: {} — one surface over the desktop", self.tablets.describe_active());
        }
    }

    /// While capturing, take the first button press for the frontend and swallow it.
    ///
    /// Swallowed on purpose: the press that names a binding must not also *do* whatever that
    /// button normally does, or binding the middle button would paste while you bound it.
    /// Returns whether this report was consumed.
    fn capture_button(&mut self, report: &Report) -> bool {
        let Some(until) = self.capture_until else {
            return false;
        };
        if Instant::now() >= until {
            // A capture that timed out with a button still held would otherwise leave its
            // release queued for swallowing indefinitely, and the next ordinary release of
            // that button would be eaten instead.
            self.end_capture();
            return false;
        }
        if report.keys.is_empty() {
            // Motion during a capture is swallowed too. The hand is reaching for a button,
            // not pointing at anything, and letting the cursor wander mid-bind is noise.
            return true;
        }

        for (code, pressed) in &report.keys {
            if *pressed {
                // **Accumulated, not taken on the first press**: `Ctrl+A+Middle` is one
                // binding, so the chord is whatever was held together, and the mouse's share
                // of it is however many of its buttons were down at once.
                //
                // `Debug` renders the evdev name — `BTN_SIDE` — which is exactly what the
                // config takes, so a capture round-trips without a translation table.
                let name = format!("{:?}", evdev::KeyCode::new(*code));
                if !self.capture_chord.contains(&name) {
                    self.capture_chord.push(name);
                }
                self.capture_down += 1;
                self.swallow_release.push(*code);
            } else {
                self.capture_down = self.capture_down.saturating_sub(1);
            }
        }

        // Published when the hand lets go of everything, which is how a chord announces that
        // it is finished. Waiting for a timeout instead would make every bind feel slow.
        if self.capture_down == 0 && !self.capture_chord.is_empty() {
            let chord = self.capture_chord.join("+");
            if let Ok(mut slot) = self.captured.lock() {
                *slot = Some(chord.clone());
            }
            println!("captured binding: {chord}");
            self.capture_until = None;
            self.capture_chord.clear();
        }
        true
    }

    fn track_button(&mut self, report: &Report, stroke_active: &mut bool) {
        for (code, pressed) in &report.keys {
            if *code == self.pen_button {
                *stroke_active = *pressed;
            }
            // Held state for every button, since a modifier bound to one is asked about on
            // samples that carry no transition of its own.
            match (pressed, self.buttons_held.iter().position(|c| c == code)) {
                (true, None) => self.buttons_held.push(*code),
                (false, Some(i)) => {
                    self.buttons_held.swap_remove(i);
                }
                _ => {}
            }
        }
    }

    /// Whether the current mode's constrain modifier is held.
    ///
    /// A mouse button is answered from the grabbed device's own state; a keyboard key from the
    /// listener. A mode that binds nothing is never constrained, which is what makes `snap`
    /// with `activation = "modifier"` and no binding inert rather than stuck on.
    fn constrain_held(&self) -> bool {
        match self.modes.current() {
            Some(mode) => self.any_held(&mode.modifier),
            None => false,
        }
    }

    /// Whether a whole scroll chord is held.
    fn scroll_held(&self) -> bool {
        match self.modes.current() {
            Some(mode) => self.any_held(&mode.scroll_button),
            None => false,
        }
    }

    /// Whether *any* part of a scroll binding is held.
    ///
    /// The halfway state of a chord: enough for a page to keep coasting, not enough to be
    /// steering it. Equal to `scroll_held` for a single-button binding, which is why the
    /// option that reads it is offered only for chords.
    fn scroll_partly_held(&self) -> bool {
        self.modes.current().is_some_and(|m| {
            m.scroll_button.iter().flatten().any(|c| self.code_held(*c))
        })
    }

    /// Reopen the keyboards, so a binding added since startup is actually watched.
    ///
    /// Keyboards occupy the tail of the poll set, which is what lets them be replaced without
    /// disturbing the indices of everything before them. Rebuilt only when the wanted set
    /// changes: reopening on every config edit would drop and reacquire devices for a change
    /// that had nothing to do with bindings.
    fn rewatch_keyboards(&mut self, fds: &mut Vec<std::os::fd::RawFd>, from: usize) {
        let mut wanted: Vec<u16> = self
            .modes
            .slots()
            .iter()
            .flat_map(|m| m.modifier.iter().chain(m.scroll_button.iter()))
            .flatten()
            .filter(|c| !stabmouse_input::is_mouse_button(**c))
            .copied()
            .collect();
        wanted.sort_unstable();
        wanted.dedup();

        if wanted == self.watched_keys {
            return;
        }
        self.watched_keys = wanted.clone();

        self.listener = if wanted.is_empty() {
            None
        } else {
            match stabmouse_input::Listener::watching(&wanted) {
                Ok(l) => {
                    println!(
                        "  constrain modifier: watching {} keyboard(s) for {} key(s)",
                        l.raw_fds().len(),
                        wanted.len()
                    );
                    Some(l)
                }
                Err(e) => {
                    eprintln!("  keyboard bindings unavailable: {e}");
                    None
                }
            }
        };

        fds.truncate(from);
        if let Some(l) = &self.listener {
            fds.extend(l.raw_fds());
        }
    }

    /// Stop capturing and forget anything half-collected.
    fn end_capture(&mut self) {
        self.capture_until = None;
        self.capture_chord.clear();
        self.capture_down = 0;
        self.swallow_release.clear();
    }

    /// Whether a bound mouse button should be withheld from the application.
    ///
    /// Only mouse buttons: a keyboard key is never emitted by this daemon, so there is
    /// nothing to withhold, and treating one as consumed would be a claim about a device we
    /// deliberately do not control.
    fn bound_to_gesture(&self, code: u16) -> bool {
        use stabmouse_config::Passthrough;

        if !stabmouse_input::is_mouse_button(code) {
            return false;
        }
        // Never the pen button: it is what starts a stroke, and a preset that also bound it
        // to a gesture would otherwise silently lose the ability to draw.
        if code == self.pen_button {
            return false;
        }
        let Some(mode) = self.modes.current() else {
            return false;
        };
        let chords = || mode.scroll_button.iter().chain(mode.modifier.iter());

        match mode.passthrough {
            Passthrough::Always => false,
            Passthrough::Reserved => chords().flatten().any(|c| *c == code),
            // **Taken only when the rest of its chord is already held.** With `alt+middle`,
            // middle alone still pastes and middle with alt held does not — so a binding
            // costs the button nothing outside the exact combination that was chosen. For a
            // single-button binding there is no "rest", so it is taken whenever bound.
            Passthrough::UnlessActive => chords().any(|chord| {
                chord.contains(&code)
                    && chord
                        .iter()
                        .filter(|c| **c != code)
                        .all(|c| self.code_held(*c))
            }),
        }
    }

    /// Whether any chord is fully held.
    ///
    /// A mouse button is answered from the grabbed device's own state; a keyboard key from the
    /// listener. An empty list is never held, which is what makes a stage that waits for a
    /// binding inert rather than stuck on when nothing is bound — and an empty *chord* would
    /// otherwise be vacuously true, which would leave it permanently engaged.
    fn any_held(&self, chords: &[Vec<u16>]) -> bool {
        chords
            .iter()
            .any(|chord| !chord.is_empty() && chord.iter().all(|c| self.code_held(*c)))
    }

    fn code_held(&self, code: u16) -> bool {
        if stabmouse_input::is_mouse_button(code) {
            self.buttons_held.contains(&code)
        } else {
            self.listener.as_ref().is_some_and(|l| l.is_held(code))
        }
    }

    fn handle(
        &mut self,
        report: &Report,
        t_us: u64,
        stroke_active: &mut bool,
    ) -> anyhow::Result<()> {
        if self.capture_button(report) {
            return Ok(());
        }
        // The other half of swallowing a captured press: those releases belong to presses no
        // application ever saw, so passing them on would leave a button-up with no button-down.
        //
        // **Filtered out of the emission, never skipped as a report.** Skipping meant
        // `track_button` never saw the release either, so the button stayed in `buttons_held`
        // and the gesture it was bound to stayed engaged — which is exactly the "first use
        // after binding behaves as though latch were on" that was reported. The state must
        // always be tracked; only the *output* is suppressed.
        let mut swallowed: Vec<u16> = Vec::new();
        if !self.swallow_release.is_empty() {
            for (code, pressed) in &report.keys {
                if !*pressed {
                    if let Some(i) = self.swallow_release.iter().position(|c| c == code) {
                        self.swallow_release.swap_remove(i);
                        swallowed.push(*code);
                    }
                }
            }
        }

        self.track_button(report, stroke_active);

        // Net source movement since a sink last carried anything, for the stall check. Raw
        // counts, because the question is what the *hand* did, independent of whatever the
        // filters decided to do with it.
        self.stall_dx += f64::from(report.dx);
        self.stall_dy += f64::from(report.dy);

        // The window under the position, looked up once: it decides both the transport and
        // whether this application needs the pen held still to receive a scroll.
        let under = self.under_position();

        // The transport, which is not always the mode's preference. The mode, the preset and
        // every filter above are untouched — see D20: a mode is the user's intent, and where
        // the result is delivered is a detail of the application in front of them.
        let output = self.transport_for(&under);
        let mode_wants_tablet = self
            .modes
            .current()
            .is_some_and(|m| m.output == Output::Tablet);
        let px_per_mm = self.tablets.px_per_mm();

        // One line per change, naming what the pointer was over and why the decision went that
        // way. Printed unconditionally: this is the only trustworthy way to see which transport
        // is live — behaviour alone is ambiguous, and the scroll wheel in particular is not a
        // reliable indicator.
        if self.last_transport != Some(output) {
            let previous = self.last_transport;
            self.last_transport = Some(output);
            let under = self.under_position();
            let (_, why) = crate::focus::explain(&under, &self.tablet_overrides);
            let app = if under.class.is_empty() {
                "(nothing under the pointer)".to_string()
            } else {
                under.class.clone()
            };
            println!(
                "transport: {} -> {:?} over {app} — {why}",
                previous
                    .map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "start".into()),
                output
            );
        }

        // **A button bound to a gesture is consumed by it.** Otherwise binding the middle
        // button to autoscroll also pastes, and binding a side button also goes Back — the
        // gesture appears to "activate when it shouldn't", when what actually fired was the
        // button's ordinary job happening alongside it.
        let keys: Vec<(u16, bool)> = report
            .keys
            .iter()
            .filter(|(code, pressed)| {
                !self.bound_to_gesture(*code) && !(!*pressed && swallowed.contains(code))
            })
            .copied()
            .collect();

        // Read before the mutable borrow of the mode below.
        let constrain = self.constrain_held();
        let scrolling = self.scroll_held();
        let scroll_partial = self.scroll_partly_held();
        let scroll_bound = self
            .modes
            .current()
            .is_some_and(|m| !m.scroll_button.is_empty());

        let Some(mode) = self.modes.current_mut() else {
            return Ok(());
        };

        let mut sample = Sample::new(
            f64::from(report.dx),
            f64::from(report.dy),
            t_us,
            *stroke_active,
        );
        sample.constrain = constrain;
        sample.scrolling = scrolling;
        sample.scroll_partial = scroll_partial;

        // The physical wheel enters the pipeline so a stage may act on it. **Whatever comes
        // back out is emitted verbatim**, so a wheel nothing claims is untouched — the whole
        // point of routing it through is momentum on a wheel you already own, not a new way
        // to lose scrolling.
        use evdev::RelativeAxisCode as Rel;
        for (code, value) in &report.other_relative {
            let v = f64::from(*value);
            if *code == Rel::REL_WHEEL.0 {
                sample.wheel_v += v;
            } else if *code == Rel::REL_HWHEEL.0 {
                sample.wheel_h += v;
            } else if *code == Rel::REL_WHEEL_HI_RES.0 {
                sample.wheel_v += v / HI_RES_PER_NOTCH;
            } else if *code == Rel::REL_HWHEEL_HI_RES.0 {
                sample.wheel_h += v / HI_RES_PER_NOTCH;
            }
        }
        sample.scroll_bound = scroll_bound;
        let wheel_in = (sample.wheel_v, sample.wheel_h);
        mode.pipeline.process(&mut sample);

        // The `scroll` stage turns held motion into scroll and consumes the motion, so a
        // gesture reaches the sinks as wheel events rather than as cursor movement.
        // Anything a stage did not take leaves as it arrived. Compared against what went in,
        // so a stage that only *changed* the value still cannot make it vanish unnoticed.
        let wheel_kept = sample.wheel_v == wheel_in.0 && sample.wheel_h == wheel_in.1;
        let passthrough_wheel: Vec<(u16, i32)> = if wheel_kept {
            report.other_relative.clone()
        } else {
            // Partly consumed: re-emit only what is left, at the resolution it came in.
            let mut out = Vec::new();
            if sample.wheel_v != 0.0 {
                out.push((Rel::REL_WHEEL_HI_RES.0, (sample.wheel_v * HI_RES_PER_NOTCH) as i32));
            }
            if sample.wheel_h != 0.0 {
                out.push((Rel::REL_HWHEEL_HI_RES.0, (sample.wheel_h * HI_RES_PER_NOTCH) as i32));
            }
            // Axes that are not the wheel — pan, or anything unusual a device carries — were
            // never candidates and must survive regardless.
            for (code, value) in &report.other_relative {
                if ![
                    Rel::REL_WHEEL.0,
                    Rel::REL_HWHEEL.0,
                    Rel::REL_WHEEL_HI_RES.0,
                    Rel::REL_HWHEEL_HI_RES.0,
                ]
                .contains(code)
                {
                    out.push((*code, *value));
                }
            }
            out
        };

        let gesture = self.take_scroll(&sample);

        match output {
            Output::Mouse => {
                // Checked every time rather than on the transition into this state. A tool left
                // in proximity while the pointer is being driven gives the user two cursors,
                // both live — observed drawing in two places in Blender at once. The check is a
                // bool per tablet and the lift only emits when something is actually down.
                if self.tablets.any_in_proximity() {
                    self.tablets.lift_all();
                }

                // One shared position, advanced at this mode's own scale: the tablet's span
                // for a fallback, the source device's counts-per-millimetre for a mouse mode
                // (D23). One position is what makes every mode switch continue from where the
                // cursor actually is.
                let fallback = mode_wants_tablet;
                if fallback {
                    self.tablets.track(sample.dx, sample.dy, *stroke_active);
                } else {
                    let scale = self.quantizer.counts_per_mm();
                    self.tablets.track_px(sample.dx * scale, sample.dy * scale);
                }

                // Absolute delivery: state the position outright and the cursor is wherever
                // the shared position says. No divergence between transports means no teleport
                // on a transport or mode change — the whole reason this sink exists (P6/D22).
                if let Some(pointer) = self.pointer.as_mut() {
                    if let Some((px, py)) = self.tablets.position_px() {
                        if pointer.position(px, py) {
                            self.emitted_motion = true;
                            self.last_emit = Instant::now();
                            self.stall_dx = 0.0;
                            self.stall_dy = 0.0;
                        }
                        // Wheel and buttons bypass the filters: they are not pointer
                        // motion. Through this sink they land under the visible cursor,
                        // which the relative sink could not guarantee.
                        for (code, value) in &passthrough_wheel {
                            pointer.relative(*code, *value);
                        }
                        emit_scroll(gesture, |code, value| pointer.relative(code, value));
                        for (code, pressed) in &keys {
                            pointer.key(*code, *pressed);
                        }
                        pointer.flush()?;
                        return Ok(());
                    }
                }

                // Relative delivery: mouse-intent modes, and the degraded fallback for when
                // the screen layout is unknown.
                //
                // A tablet mode delivering through the relative pointer must keep the tablet's
                // scale. The two paths are otherwise unrelated — millimetres across a screen
                // versus counts at the source DPI — so the sensitivity changed with the
                // transport, which is precisely what the fallback exists to hide. Measured as
                // roughly double on this host.
                let (dx, dy) = if fallback {
                    // Pixels, because StabMouse's own pointer is set to one pixel per count.
                    self.quantizer.quantize_at(sample.dx, sample.dy, px_per_mm)
                } else {
                    self.quantizer.quantize(sample.dx, sample.dy)
                };
                if dx != 0 || dy != 0 {
                    self.emitted_motion = true;
                    self.last_emit = Instant::now();
                    self.stall_dx = 0.0;
                    self.stall_dy = 0.0;
                }
                self.mouse.motion(dx, dy);
                // Wheel and buttons bypass the filters: they are not pointer motion, and
                // reinterpreting them as such would be wrong. The `scroll` stage's output is
                // the exception — it *is* filtered, being motion the pipeline reinterpreted.
                for (code, value) in &passthrough_wheel {
                    self.mouse.relative(*code, *value);
                }
                emit_scroll(gesture, |code, value| self.mouse.relative(code, value));
                for (code, pressed) in &keys {
                    self.mouse.key(*code, *pressed);
                }
                self.mouse.flush()?;
            }
            Output::Relative => {
                if self.tablets.any_in_proximity() {
                    self.tablets.lift_all();
                }
                // Raw deltas for pointer-lock and raw-input consumers — games. The shared
                // position is deliberately *not* advanced: the compositor decides where a
                // locked pointer goes, not us. Its cursor reports re-sync the position when
                // the hand pauses (D23), which is also what makes leaving one of these modes
                // jump-free.
                let (dx, dy) = self.quantizer.quantize(sample.dx, sample.dy);
                if dx != 0 || dy != 0 {
                    self.emitted_motion = true;
                    self.last_emit = Instant::now();
                    self.stall_dx = 0.0;
                    self.stall_dy = 0.0;
                }
                self.mouse.motion(dx, dy);
                for (code, value) in &passthrough_wheel {
                    self.mouse.relative(*code, *value);
                }
                emit_scroll(gesture, |code, value| self.mouse.relative(code, value));
                for (code, pressed) in &keys {
                    self.mouse.key(*code, *pressed);
                }
                self.mouse.flush()?;
            }
            Output::Tablet => {
                // **A scroll stops the pen — in the applications that need it.** Krita
                // discards mouse input while a pen is in proximity, on a timer that every
                // tablet event resets, so a moving pen suppresses the wheel indefinitely;
                // scrolling worked only when the hand held still, which is how the mechanism
                // was identified. Sending the wheel from the tablet instead is not possible:
                // libinput will not give a tablet the pointer capability scroll requires (P9).
                //
                // Blender does not filter this way and scrolls perfectly well mid-movement, so
                // this is a property of the **application**, not of the mode — freezing
                // everything would take from Blender to give to Krita.
                //
                // The hand's movement during a freeze is **discarded rather than banked**.
                // Banking it would fling the cursor when the freeze lifted, and moving the
                // cursor was not what the hand was asking for while it was scrolling.
                if !*stroke_active
                    && !report.other_relative.is_empty()
                    && crate::focus::needs_scroll_freeze(
                        &under.class,
                        &self.scroll_freeze_overrides,
                        self.freeze_while_scrolling,
                    )
                {
                    self.frozen_until = Some(Instant::now() + self.scroll_freeze);
                }
                let frozen = self
                    .frozen_until
                    .is_some_and(|until| Instant::now() < until);
                if !frozen {
                    self.frozen_until = None;
                }

                // Per-screen when the layout is known, which also performs the handover as the
                // pen crosses a boundary. Otherwise one surface over the whole desktop.
                let (dx, dy) = if frozen { (0.0, 0.0) } else { (sample.dx, sample.dy) };
                let (x, y) = match self.tablets.advance(dx, dy, *stroke_active) {
                    Some(p) => p,
                    None => self.mapper.advance(dx, dy),
                };
                if (x, y) != self.last_tablet_xy {
                    self.emitted_motion = true;
                    self.last_emit = Instant::now();
                    self.stall_dx = 0.0;
                    self.stall_dy = 0.0;
                    self.last_tablet_xy = (x, y);
                }
                // Recreated here rather than on the switch when a profile opts into teardown,
                // so the ~50ms is spent while the hand is still moving rather than inside the
                // switch the user is waiting on.
                // Same invariant from the other side: only the tablet receiving input may be
                // live. A screen crossing hands over, and any path that misses it leaves a
                // second tool in proximity on another screen.
                self.tablets.lift_inactive();

                let tablet = self.tablets.active().ensure()?;
                // Emitted even while frozen: the position is unchanged, so `pen` states only
                // what actually differs and a held pen costs nothing. Skipping it entirely
                // would take the tool out of proximity, which is the one thing this must not
                // do — proximity churn is what applications handle worst (D13).
                tablet.pen(x, y, sample.pressure.unwrap_or(0.0), *stroke_active);

                for (code, pressed) in &keys {
                    // Right and middle also become barrel buttons, for applications that read
                    // them as such.
                    if *code == evdev::KeyCode::BTN_RIGHT.code() {
                        tablet.stylus(false, *pressed);
                    } else if *code == evdev::KeyCode::BTN_MIDDLE.code() {
                        tablet.stylus(true, *pressed);
                    }
                }
                tablet.flush()?;

                // Neither the wheel nor a button reaches an application through the pen: KWin
                // turns a tip into a click only behind a deprecated environment variable
                // (D18), and a tablet cannot carry scroll at all (P9). Both leave by the
                // absolute pointer, placed on the pen's position first — the pixel the cursor
                // already occupies, so nothing visibly moves and both land where the user is
                // pointing.
                //
                // The wheel works from here *because* of the freeze above: with the pen held
                // still, the proximity filter that was swallowing it lapses.
                //
                // Hover only. With the pen down the wheel is the input to `pressure.manual`
                // (stages.md), which is specified as active during a stroke — reserving it now
                // means building that term cannot make one notch do two things.
                //
                // Clicks stay behind `tablet_emits_mouse_clicks` because an application
                // reading both tablet and mouse buttons would see one press twice. The wheel
                // has no such hazard, the pen having no wheel to double with.
                let wheel: &[(u16, i32)] = if *stroke_active {
                    &[]
                } else {
                    &passthrough_wheel
                };
                let clicks: &[(u16, bool)] = if self.tablet_clicks { &keys } else { &[] };
                if !wheel.is_empty() || !clicks.is_empty() || gesture.any() {
                    let at = self.tablets.position_px();
                    match (self.pointer.as_mut(), at) {
                        (Some(pointer), Some((px, py))) => {
                            pointer.position(px, py);
                            for (code, value) in wheel {
                                pointer.relative(*code, *value);
                            }
                            emit_scroll(gesture, |code, value| pointer.relative(code, value));
                            for (code, pressed) in clicks {
                                pointer.key(*code, *pressed);
                            }
                            pointer.flush()?;
                        }
                        _ => {
                            // No absolute pointer. The wheel still works — scrolling needs
                            // focus, not position — while clicks keep the old wrong-position
                            // behaviour, since a press somewhere beats no press anywhere.
                            for (code, value) in wheel {
                                self.mouse.relative(*code, *value);
                            }
                            emit_scroll(gesture, |code, value| self.mouse.relative(code, value));
                            for (code, pressed) in clicks {
                                self.mouse.key(*code, *pressed);
                            }
                            self.mouse.flush()?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Hand the wheel values to whichever sink is carrying this mode.
///
/// A closure rather than a sink reference, because the three sinks share this shape without
/// sharing a trait — and inventing one for four lines would be the larger change.
fn emit_scroll(out: ScrollOut, mut relative: impl FnMut(u16, i32)) {
    use evdev::RelativeAxisCode as Rel;
    if out.notch_y != 0 {
        relative(Rel::REL_WHEEL.0, out.notch_y);
    }
    if out.hi_y != 0 {
        relative(Rel::REL_WHEEL_HI_RES.0, out.hi_y);
    }
    if out.notch_x != 0 {
        relative(Rel::REL_HWHEEL.0, out.notch_x);
    }
    if out.hi_x != 0 {
        relative(Rel::REL_HWHEEL_HI_RES.0, out.hi_x);
    }
}

/// Newest mtime anywhere under `dir`, so editing a preset counts as a change even though the
/// directory itself is untouched.
fn newest_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    fn walk(dir: &std::path::Path, best: &mut Option<std::time::SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, best);
            } else if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if best.map_or(true, |b| m > b) {
                    *best = Some(m);
                }
            }
        }
    }
    let mut best = None;
    if dir.is_dir() {
        walk(dir, &mut best);
    }
    best
}
