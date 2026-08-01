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

/// Report a stall once input has been arriving this long with nothing coming out.
///
/// Measured on the shipped presets, the slowest legitimate delay before the cursor first moves
/// is ~364ms at a very gentle 5mm/s, and 24-119ms at normal speeds. Anything past this is not
/// the filters being slow, so it is worth saying out loud rather than leaving the user to
/// wonder whether the program is frozen.
const STALL_REPORT: Duration = Duration::from_millis(600);

/// How long the hand must be still before a compositor cursor report is adopted into the
/// shared position. Our own emissions echo back through the report a few pixels stale, and
/// adopting an echo mid-motion would drag the position backwards; motion from a device we do
/// not manage keeps reporting after our emissions stop, so it survives this gate.
const ADOPT_IDLE: Duration = Duration::from_millis(150);

pub struct Runtime {
    pub capture: Capture,
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
    /// Whether the current mode's modifier is a mouse button that is currently held.
    ///
    /// Tracked here rather than read from the report, because a button's state persists
    /// between reports while `report.keys` carries only transitions.
    pub buttons_held: Vec<u16>,
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
}

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
        mut reload: impl FnMut() -> Option<Reloaded>,
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

        let mut fds = vec![self.capture.raw_fd(), self.control.raw_fd()];
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
            let deadline = if outstanding {
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
                    }
                }
            }

            // Only read the device when the device is what woke us. Reading it otherwise blocks
            // until the user happens to move the mouse, stranding every queued command behind
            // it — which is exactly how one switch landed out of seven.
            if wake.has(DEVICE) {
                let events: Vec<_> = self.capture.read()?.collect();
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
                    if self.emitted_motion {
                        input_since = None;
                        stall_reported = false;
                        self.emitted_motion = false;
                    } else if !stall_reported && now.duration_since(since) > STALL_REPORT {
                        stall_reported = true;
                        let mode = self
                            .modes
                            .current()
                            .map(|m| format!("{} ({:?})", m.name, m.output))
                            .unwrap_or_else(|| "none".into());
                        eprintln!(
                            "STALL: {:.0}ms of input with no motion emitted. mode {} = {mode}, \
                             inert={}, grabbed={}, stages=[{}]",
                            now.duration_since(since).as_millis(),
                            self.modes.current_index() + 1,
                            self.inert,
                            self.capture.is_grabbed(),
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
                // Kept current even on the watch path, so switching to the poll fallback after
                // a failure does not immediately re-fire on a change already handled.
                last_mtime = config_dir.as_deref().and_then(newest_mtime);

                // A failed reload leaves the running config alone rather than dropping to
                // passthrough, so a typo mid-edit cannot take the pointer away.
                if let Some(fresh) = reload() {
                    self.modes = fresh.modes;
                    self.tablets
                        .set_destroy_on_leave(fresh.destroy_tablet_on_leave);
                    self.tablet_clicks = fresh.tablet_emits_mouse_clicks;
                    let name = self
                        .modes
                        .current()
                        .map(|m| m.name.clone())
                        .unwrap_or_else(|| "none".into());
                    println!(
                        "config reloaded — mode {} ({name})",
                        self.modes.current_index() + 1
                    );
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
            self.capture.ungrab();
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
        match self.capture.grab() {
            Ok(()) => {
                self.inert = false;
                eprintln!("resumed");
                notify::mode("resumed", "filtering again");
            }
            Err(e) => {
                eprintln!("could not re-grab, staying inert: {e}");
                notify::mode("could not resume", "the device could not be grabbed");
            }
        }
        self.publish(Change::Enabled);
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
        let Some(mode) = self.modes.current() else {
            return Output::Mouse;
        };
        if mode.output != Output::Tablet {
            return mode.output;
        }
        if crate::focus::supports_tablet(&self.under_position(), &self.tablet_overrides) {
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
        let Some(code) = self.modes.current().and_then(|m| m.modifier) else {
            return false;
        };
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
        self.track_button(report, stroke_active);

        // The transport, which is not always the mode's preference. The mode, the preset and
        // every filter above are untouched — see D20: a mode is the user's intent, and where
        // the result is delivered is a detail of the application in front of them.
        let output = self.transport();
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

        // Read before the mutable borrow of the mode below.
        let constrain = self.constrain_held();

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
        mode.pipeline.process(&mut sample);

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
                        }
                        // Wheel and buttons bypass the filters: they are not pointer
                        // motion. Through this sink they land under the visible cursor,
                        // which the relative sink could not guarantee.
                        for (code, value) in &report.other_relative {
                            pointer.relative(*code, *value);
                        }
                        for (code, pressed) in &report.keys {
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
                }
                self.mouse.motion(dx, dy);
                // Wheel and buttons bypass the filters: they are not pointer motion, and
                // reinterpreting them as such would be wrong.
                for (code, value) in &report.other_relative {
                    self.mouse.relative(*code, *value);
                }
                for (code, pressed) in &report.keys {
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
                }
                self.mouse.motion(dx, dy);
                for (code, value) in &report.other_relative {
                    self.mouse.relative(*code, *value);
                }
                for (code, pressed) in &report.keys {
                    self.mouse.key(*code, *pressed);
                }
                self.mouse.flush()?;
            }
            Output::Tablet => {
                // Per-screen when the layout is known, which also performs the handover as the
                // pen crosses a boundary. Otherwise one surface over the whole desktop.
                let (x, y) = match self
                    .tablets
                    .advance(sample.dx, sample.dy, *stroke_active)
                {
                    Some(p) => p,
                    None => self.mapper.advance(sample.dx, sample.dy),
                };
                if (x, y) != self.last_tablet_xy {
                    self.emitted_motion = true;
                    self.last_emit = Instant::now();
                    self.last_tablet_xy = (x, y);
                }
                // Recreated here rather than on the switch when a profile opts into teardown,
                // so the ~50ms is spent while the hand is still moving rather than inside the
                // switch the user is waiting on.
                // Same invariant from the other side: only the tablet receiving input may be
                // live. A screen crossing hands over, and any path that misses it leaves a
                // second tool in proximity on another screen.
                self.tablets.lift_inactive();

                // **Two devices must not claim an absolute position in the same instant.**
                // The wheel below is delivered through the absolute pointer, positioned on the
                // pen so the scroll lands where the user is pointing — and if the tablet also
                // emits motion on that sample, the compositor is handed two conflicting
                // absolute positions at once and the scroll is lost between them.
                //
                // Diagnosed from the exact symptom: scrolling worked while the pen was
                // *stationary* and not while it moved. A stationary pen emits nothing at all,
                // because `TabletSink::pen` states only axes that changed — so the quiet case
                // was the one with no conflict.
                //
                // Skipping the tablet's motion for that sample is invisible: the mapper still
                // tracks the position, so the pen resumes from the right place the moment
                // scrolling stops, and this path is only reachable while hovering, so no touch
                // or pressure state can be affected.
                let scrolling = !*stroke_active && !report.other_relative.is_empty();

                let tablet = self.tablets.active().ensure()?;
                if !scrolling {
                    tablet.pen(x, y, sample.pressure.unwrap_or(0.0), *stroke_active);
                }
                for (code, pressed) in &report.keys {
                    // Right and middle also become barrel buttons, for applications that read
                    // them as such.
                    if *code == evdev::KeyCode::BTN_RIGHT.code() {
                        tablet.stylus(false, *pressed);
                    } else if *code == evdev::KeyCode::BTN_MIDDLE.code() {
                        tablet.stylus(true, *pressed);
                    }
                }
                tablet.flush()?;

                // Neither the wheel nor a button can reach an application through the pen:
                // KWin turns a tip into a click only behind a deprecated environment variable
                // (D18), and a pen carries no wheel at all. Both therefore go through the
                // absolute pointer, placed on the pen's position first — the pixel the cursor
                // already occupies, so nothing visibly moves and the scroll lands under
                // whatever the user is pointing at.
                //
                // **Wheel passes through only while hovering.** With the pen down the wheel is
                // not scroll: it is the input to `pressure.manual` (stages.md), which is
                // specified as active only during a stroke. Reserving it now means building
                // that term cannot make one notch do two things at once — and scrolling
                // mid-stroke is meaningless anyway, since the hand is drawing.
                //
                // Clicks stay behind `tablet_emits_mouse_clicks` because an application that
                // reads both tablet and mouse buttons would see one press twice. The wheel has
                // no such hazard — the pen has no wheel to double with — so it is unconditional.
                let wheel: &[(u16, i32)] = if *stroke_active {
                    &[]
                } else {
                    &report.other_relative
                };
                let clicks: &[(u16, bool)] = if self.tablet_clicks { &report.keys } else { &[] };
                if !wheel.is_empty() || !clicks.is_empty() {
                    let at = self.tablets.position_px();
                    match (self.pointer.as_mut(), at) {
                        (Some(pointer), Some((px, py))) => {
                            pointer.position(px, py);
                            for (code, value) in wheel {
                                pointer.relative(*code, *value);
                            }
                            for (code, pressed) in clicks {
                                pointer.key(*code, *pressed);
                            }
                            pointer.flush()?;
                        }
                        _ => {
                            // No absolute pointer. The wheel still works — scrolling needs no
                            // position, only focus — while clicks keep the old wrong-position
                            // behaviour, since a press somewhere beats no press anywhere.
                            for (code, value) in wheel {
                                self.mouse.relative(*code, *value);
                            }
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
