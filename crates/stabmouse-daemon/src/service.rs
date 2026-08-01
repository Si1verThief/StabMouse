//! The D-Bus server.
//!
//! # Why this does not run in the hot loop
//!
//! The hot loop is single-threaded and blocks in `poll(2)` on the input device. zbus brings its
//! own executor. Merging them would mean either the loop pumping a D-Bus queue between mouse
//! reports — adding jitter to the one path where jitter is the whole problem (D5: the tail is
//! what needs engineering, not the median) — or the loop losing its simple structure.
//!
//! So D-Bus runs on its own thread and the two halves meet at exactly two places:
//!
//! - **Commands out**: translated into the same [`crate::control::Command`] values the socket
//!   already carries, and sent on that socket. The hot loop needs no new wakeup source, because
//!   it already polls it. One code path handles both transports.
//! - **State in**: a [`Snapshot`] the hot loop publishes on change and the service reads.
//!
//! # Why a snapshot instead of request/response
//!
//! `GetStatus` could ask the hot loop and wait for an answer. That would couple a bus client's
//! latency to whether the user happens to be moving their mouse, and worse, it would let a
//! stalled or wedged loop hang every caller — the exact failure docs/api.md forbids.
//!
//! Publishing instead means a status call never touches the loop at all. The cost is that a
//! snapshot can be up to one state change stale, which for values that only change when the
//! user switches mode is not a cost at all.
//!
//! The lock is held only to clone a handful of small fields, never across I/O, so the hot
//! loop's write can never wait on a client.

use crate::control::{self, Command};
use stabmouse_ipc::{
    Degraded, DeviceInfo, ModeInfo, BUS_NAME, DAEMON_INTERFACE, INTERFACE_VERSION, OBJECT_PATH,
};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// What the daemon publishes about itself.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub profile: String,
    pub mode_slot: u32,
    pub mode_name: String,
    pub enabled: bool,
    pub panicked: bool,
    pub modes: Vec<ModeInfo>,
    pub devices: Vec<DeviceInfo>,
    pub degraded: Degraded,
    pub tablets: u32,
    pub tablets_placed: bool,
    /// Config's tablet-support overrides, so status agrees with the input loop.
    pub tablet_overrides: Vec<(String, bool)>,
}

/// What the hot loop announces. Emitting the matching D-Bus signal is somebody else's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Announce {
    Mode,
    Enabled,
    Config,
    Degraded,
}

/// Shared handle to the published state.
///
/// # Nothing here may block the input loop
///
/// An earlier version had the loop call `emit_signal` directly. That deadlocked the daemon and
/// left the user unable to move their mouse:
///
/// 1. the loop blocked inside `emit_signal`, waiting on the D-Bus executor;
/// 2. the executor was inside a method handler calling `control::send`;
/// 3. that datagram filled the control socket's buffer, because the loop that drains it was
///    stuck at step 1.
///
/// A circular wait, and `panic` could not get through either — killing the process was the only
/// way out, which is precisely the failure this project's first working rule forbids.
///
/// So announcements go onto an **unbounded channel**, whose send never blocks and never fails
/// in a way worth handling, and a separate thread turns them into signals. The loop's only
/// costs are a short mutex hold and a queue push.
#[derive(Clone)]
pub struct Published {
    state: Arc<Mutex<Snapshot>>,
    announce: Sender<Announce>,
}

impl Default for Published {
    fn default() -> Self {
        Self::new(Snapshot::default()).0
    }
}

impl Published {
    /// The handle, and the receiving end of the announcement queue.
    pub fn new(initial: Snapshot) -> (Self, Receiver<Announce>) {
        let (announce, rx) = std::sync::mpsc::channel();
        (
            Self {
                state: Arc::new(Mutex::new(initial)),
                announce,
            },
            rx,
        )
    }

    /// Queue an announcement. Never blocks; a dead receiver is ignored, since a daemon with no
    /// bus must keep filtering input.
    pub fn announce(&self, what: Announce) {
        let _ = self.announce.send(what);
    }

    /// Replace the published state. Called from the hot loop, so it must stay trivial.
    ///
    /// A poisoned lock is ignored rather than propagated: a panicking bus thread must not be
    /// able to take the input loop down with it, because the input loop is holding the user's
    /// mouse.
    pub fn publish(&self, f: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut guard) = self.state.lock() {
            f(&mut guard);
        }
    }

    pub fn read(&self) -> Snapshot {
        self.state.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Turn queued announcements into D-Bus signals, off the input loop's thread.
///
/// Owns the blocking calls that must never happen on the hot path. If the bus goes away this
/// thread simply stops emitting; nothing about input handling depends on it.
pub fn run_emitter(conn: zbus::blocking::Connection, state: Published, queue: Receiver<Announce>) {
    std::thread::spawn(move || {
        for what in queue {
            let s = state.read();
            match what {
                Announce::Mode => mode_changed(&conn, s.mode_slot, &s.mode_name),
                Announce::Enabled => enabled_changed(&conn, s.enabled),
                Announce::Config => config_reloaded(&conn),
                Announce::Degraded => output_degraded(&conn, &s.degraded.reason),
            }
        }
    });
}

struct Daemon {
    state: Published,
    /// What is under the pointer. Written here, read by the input loop.
    focus: crate::focus::Focus,
    /// Which files have already been searched for the tablet interface.
    ///
    /// Keyed by path because a library does not change under a running system, so each file is
    /// read at most once however many applications map it.
    scanned: Arc<Mutex<std::collections::HashMap<String, bool>>>,
    /// Inspection verdicts by window class, so a layout resend — which happens per frame
    /// while a window is being dragged — costs rectangle bookkeeping, not `/proc` reads.
    class_signals: Arc<Mutex<std::collections::HashMap<String, Option<crate::focus::Signal>>>>,
    /// Filled by the input loop when a binding capture completes.
    captured: Arc<Mutex<Option<String>>>,
}

struct Devices {
    state: Published,
}

struct Config {
    state: Published,
}

/// Send a command to the hot loop over the socket it already polls.
///
/// Failure here means the loop is gone, which is worth reporting to the caller rather than
/// silently accepting — a `SetMode` that returned success while doing nothing is worse than an
/// error.
fn dispatch(command: Command) -> zbus::fdo::Result<()> {
    control::send(&command)
        .map_err(|e| zbus::fdo::Error::Failed(format!("the input loop did not accept it: {e}")))
}

#[zbus::interface(name = "io.github.si1verthief.StabMouse.Daemon")]
impl Daemon {
    fn set_mode(&self, slot: u32) -> zbus::fdo::Result<()> {
        // Slots are 1-based everywhere the user can see them, so zero is a caller mistake
        // rather than something to silently coerce.
        if slot == 0 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "mode slots are 1-based".into(),
            ));
        }
        dispatch(Command::Mode(slot as usize))
    }

    fn set_mode_by_name(&self, name: String) -> zbus::fdo::Result<()> {
        let snapshot = self.state.read();
        let found = snapshot
            .modes
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| {
                // Listing what does exist, because the usual cause is a typo or a stale name
                // from another profile.
                let known: Vec<&str> = snapshot.modes.iter().map(|m| m.name.as_str()).collect();
                zbus::fdo::Error::InvalidArgs(format!(
                    "no mode named {name:?}; this profile has {}",
                    known.join(", ")
                ))
            })?;
        dispatch(Command::Mode(found.slot as usize))
    }

    fn toggle_mode(&self) -> zbus::fdo::Result<u32> {
        dispatch(Command::Cycle)?;
        // The slot the switch lands on, computed from the published order rather than waiting
        // for the loop to confirm — which would reintroduce the coupling a snapshot avoids.
        let s = self.state.read();
        let total = s.modes.len().max(1) as u32;
        Ok(s.mode_slot % total + 1)
    }

    fn set_profile(&self, name: String) -> zbus::fdo::Result<()> {
        if name.trim().is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs("a profile slug is required".into()));
        }
        dispatch(Command::Profile(name))
    }

    /// Ask the daemon to report the next button pressed rather than acting on it.
    ///
    /// A frontend cannot see these itself: the daemon holds an exclusive grab on the source
    /// device, so its buttons reach nothing else. Press-to-bind therefore has to be asked for.
    fn capture_binding(&self) -> zbus::fdo::Result<()> {
        dispatch(Command::CaptureBinding)
    }

    /// Collect a captured button name, if one has arrived. Empty while still waiting.
    ///
    /// Taken rather than read, so the same press cannot be bound twice by two callers or by
    /// one caller polling.
    fn take_captured_binding(&self) -> String {
        self.captured
            .lock()
            .ok()
            .and_then(|mut c| c.take())
            .unwrap_or_default()
    }

    /// Enabled maps onto panic's inert state: both mean "stop filtering, leave the devices
    /// alone". One mechanism, so they cannot disagree about what is happening.
    ///
    /// The desired state is sent, never a flip. An earlier version read the snapshot, compared,
    /// and returned success without acting when it already matched — which was wrong twice
    /// over: the snapshot lags the command that produced it, so a second click would compare
    /// against pre-command state and silently do nothing, and a flip computed from a stale read
    /// lands the wrong way round. Sending the target state is idempotent and cannot race.
    fn set_enabled(&self, enabled: bool) -> zbus::fdo::Result<()> {
        dispatch(Command::SetEnabled(enabled))
    }

    fn panic(&self) -> zbus::fdo::Result<()> {
        dispatch(Command::SetEnabled(false))
    }

    fn resume(&self) -> zbus::fdo::Result<()> {
        dispatch(Command::SetEnabled(true))
    }

    fn quit(&self) -> zbus::fdo::Result<()> {
        dispatch(Command::Quit)
    }

    /// Called by the KWin script when the window under the pointer changes.
    ///
    /// Under the pointer rather than focused: tablet events are delivered by position, so the
    /// surface receiving the pen is the one that has to be able to accept it. Focus is a
    /// different question and answering it here was the bug.
    ///
    /// Deliberately *not* routed through the control socket. It carries no instruction — it is
    /// a fact about the world that the input loop reads when it next needs it, so making the
    /// loop process a message per movement would be pure cost.
    ///
    /// The inspection happens here, on this thread, for the same reason: the first look at a
    /// large executable reads it from disk, and that must never sit between a mouse report and
    /// the cursor moving.
    /// The process id arrives as a **string**, not a number.
    ///
    /// KWin's `callDBus` marshals a JavaScript number as signed `int32`, so a `u32` parameter
    /// makes the signature `su`, which never matches and the call is dropped without an error
    /// on either side. The script fired, `callDBus` returned cleanly, and nothing arrived.
    ///
    /// Two strings is the shape that was measured working. A string also cannot be silently
    /// coerced into the wrong width later.
    fn pointer_over_window(&self, class: String, pid: String) {
        let pid: u32 = pid.trim().parse().unwrap_or(0);
        let mut cache = self.scanned.lock().unwrap_or_else(|e| e.into_inner());
        self.focus.set(&class, pid, &mut cache);
        // Ground truth for what the compositor actually sends. Every diagnosis of this so far
        // has been inference from behaviour, and most of them were wrong.
        let under = self.focus.get();
        eprintln!(
            "window: class={:?} pid={} inspected={}",
            class,
            pid,
            under
                .signal
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "unavailable".into())
        );
    }

    /// Called by the KWin script whenever the window layout changes — see the focus module.
    ///
    /// Inspection runs here, on this thread, exactly as `pointer_over_window` does: the first
    /// look at a process reads files, and the per-class cache is what keeps a drag's stream of
    /// resends from repeating it.
    fn window_layout(&self, layout: String) {
        // Announced exactly once. A KWin script that dies does so silently — that has cost
        // days here before — so the first arrival is worth a line saying the feed is alive.
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| {
            eprintln!("window layout feed: alive ({} windows)", layout.lines().count());
        });
        let mut scanned = self.scanned.lock().unwrap_or_else(|e| e.into_inner());
        let mut signals = self.class_signals.lock().unwrap_or_else(|e| e.into_inner());
        let windows: Vec<crate::focus::Win> = crate::focus::parse_layout(&layout)
            .into_iter()
            .map(|(class, pid, x, y, width, height)| {
                let signal = *signals.entry(class.clone()).or_insert_with(|| {
                    if pid == 0 {
                        None
                    } else {
                        crate::focus::detect_tablet_support(pid, &mut scanned)
                    }
                });
                crate::focus::Win { class, signal, x, y, width, height }
            })
            .collect();
        self.focus.set_layout(windows);
    }

    /// Called by the KWin script when the pointer cursor moves — rate-limited at the source.
    ///
    /// This is how motion from devices StabMouse does not manage re-syncs the shared position
    /// (D23). Strings for the same signature reason as everything else here.
    fn cursor_moved(&self, x: String, y: String) {
        if let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>()) {
            self.focus.report_cursor(x, y);
        }
    }

    fn list_modes(&self) -> Vec<ModeInfo> {
        self.state.read().modes
    }

    fn get_degraded(&self) -> Degraded {
        self.state.read().degraded
    }

    fn get_status(&self) -> std::collections::HashMap<String, zbus::zvariant::OwnedValue> {
        use zbus::zvariant::Value;
        let s = self.state.read();

        // Computed here rather than read from the snapshot. The input loop sleeps indefinitely
        // when nothing is happening — that is the point of the watch-based reload — so a focus
        // change does not wake it, and a value it published earlier would be stale exactly when
        // someone asks. One source of truth, evaluated on demand.
        let under = self.focus.get();
        let focused = under.class.clone();
        let fallback = s
            .modes
            .iter()
            .find(|m| m.slot == s.mode_slot)
            .is_some_and(|m| m.output == "tablet")
            && !crate::focus::supports_tablet(&under, &s.tablet_overrides);
        let mut map = std::collections::HashMap::new();
        let mut put = |k: &str, v: Value| {
            if let Ok(owned) = zbus::zvariant::OwnedValue::try_from(v) {
                map.insert(k.to_string(), owned);
            }
        };
        put("profile", Value::from(s.profile.clone()));
        put("mode_slot", Value::from(s.mode_slot));
        put("mode_name", Value::from(s.mode_name.clone()));
        put("enabled", Value::from(s.enabled));
        put("panicked", Value::from(s.panicked));
        put("degraded", Value::from(s.degraded.degraded));
        put("degraded_reason", Value::from(s.degraded.reason.clone()));
        put("tablets", Value::from(s.tablets));
        put("tablets_placed", Value::from(s.tablets_placed));
        put("fallback", Value::from(fallback));
        put("focused_app", Value::from(focused));
        put("version", Value::from(env!("CARGO_PKG_VERSION")));
        map
    }

    #[zbus(property)]
    fn interface_version(&self) -> u32 {
        INTERFACE_VERSION
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.state.read().profile
    }

    #[zbus(property)]
    fn active_mode(&self) -> u32 {
        self.state.read().mode_slot
    }

    #[zbus(property)]
    fn enabled(&self) -> bool {
        self.state.read().enabled
    }

}

#[zbus::interface(name = "io.github.si1verthief.StabMouse.Devices")]
impl Devices {
    fn list(&self) -> Vec<DeviceInfo> {
        self.state.read().devices
    }

    fn set_managed(&self, _id: String, _managed: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "editing managed devices over D-Bus is not implemented yet; edit config.toml".into(),
        ))
    }

    /// The D7 seam. StabMouse never speaks hidraw — it is told.
    fn set_resolution(&self, _id: String, _dpi: u32) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "resolution tracking is not implemented yet".into(),
        ))
    }
}

#[zbus::interface(name = "io.github.si1verthief.StabMouse.Config")]
impl Config {
    fn reload(&self) -> zbus::fdo::Result<()> {
        dispatch(Command::Reload)
    }

    fn list_profiles(&self) -> Vec<(String, String)> {
        let s = self.state.read();
        vec![(s.profile.clone(), s.profile)]
    }

    fn list_presets(&self) -> Vec<(String, String)> {
        let s = self.state.read();
        s.modes
            .iter()
            .map(|m| (m.preset.clone(), m.preset.clone()))
            .collect()
    }

    fn explain(&self, _device: String, _key: String) -> zbus::fdo::Result<(String, zbus::zvariant::OwnedValue)> {
        Err(zbus::fdo::Error::NotSupported(
            "cascade provenance over D-Bus is not implemented yet".into(),
        ))
    }
}

/// Emit one of the documented signals.
///
/// Emitted on the connection rather than declared inside the interface, which keeps the
/// signal list in one readable place and avoids the macro's bodyless-function form. The wire
/// result is identical — subscribers cannot tell the difference.
///
/// Failures are swallowed deliberately: a signal nobody is listening for, or a bus that has
/// gone away, must not disturb input handling. Signals are notifications, not commands.
fn emit(conn: &zbus::blocking::Connection, name: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) {
    let _ = conn.emit_signal(None::<&str>, OBJECT_PATH, DAEMON_INTERFACE, name, body);
}

pub fn mode_changed(conn: &zbus::blocking::Connection, slot: u32, name: &str) {
    emit(conn, "ModeChanged", &(slot, name));
}

pub fn enabled_changed(conn: &zbus::blocking::Connection, enabled: bool) {
    emit(conn, "EnabledChanged", &(enabled));
}

pub fn config_reloaded(conn: &zbus::blocking::Connection) {
    emit(conn, "ConfigReloaded", &());
}

/// The "Limited — no pressure" state from ux-requirements.md.
pub fn output_degraded(conn: &zbus::blocking::Connection, reason: &str) {
    emit(conn, "OutputDegraded", &(reason));
}

/// Start serving on the session bus.
///
/// Returns the connection, which must be kept alive — dropping it releases the bus name.
///
/// **A failure here is not fatal.** The daemon's job is filtering input, and it can do that
/// with no bus at all; a session without D-Bus should lose the CLI and GUI, not the mouse.
pub fn serve(
    state: Published,
    focus: crate::focus::Focus,
    captured: Arc<Mutex<Option<String>>>,
) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Daemon {
                state: state.clone(),
                focus,
                scanned: Arc::new(Mutex::new(std::collections::HashMap::new())),
                class_signals: Arc::new(Mutex::new(std::collections::HashMap::new())),
                captured,
            },
        )?
        .serve_at(OBJECT_PATH, Devices { state: state.clone() })?
        .serve_at(OBJECT_PATH, Config { state })?
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> Vec<ModeInfo> {
        vec![
            ModeInfo { slot: 1, name: "Mouse".into(), output: "mouse".into(), preset: "raw".into() },
            ModeInfo { slot: 2, name: "Draw".into(), output: "tablet".into(), preset: "inking".into() },
            ModeInfo { slot: 3, name: "Steady".into(), output: "mouse".into(), preset: "steady".into() },
        ]
    }

    fn published(slot: u32) -> Published {
        Published::new(Snapshot {
            profile: "default".into(),
            mode_slot: slot,
            mode_name: "Mouse".into(),
            enabled: true,
            modes: modes(),
            ..Default::default()
        })
        .0
    }

    #[test]
    fn a_snapshot_round_trips_through_the_lock() {
        let p = published(1);
        p.publish(|s| s.mode_slot = 3);
        assert_eq!(p.read().mode_slot, 3);
    }

    #[test]
    fn toggling_wraps_at_the_last_slot() {
        // Positional cycling, so the slot after the last is the first — not a stop.
        let d = Daemon {
            state: published(3),
            focus: Default::default(),
            scanned: Default::default(),
            class_signals: Default::default(),
            captured: Default::default(),
        };
        let s = d.state.read();
        let total = s.modes.len() as u32;
        assert_eq!(s.mode_slot % total + 1, 1);
    }

    #[test]
    fn toggling_advances_by_one_elsewhere() {
        let d = Daemon {
            state: published(1),
            focus: Default::default(),
            scanned: Default::default(),
            class_signals: Default::default(),
            captured: Default::default(),
        };
        let s = d.state.read();
        assert_eq!(s.mode_slot % s.modes.len() as u32 + 1, 2);
    }

    #[test]
    fn a_mode_name_is_matched_case_insensitively() {
        let s = published(1).read();
        assert!(s.modes.iter().any(|m| m.name.eq_ignore_ascii_case("draw")));
    }

    #[test]
    fn an_unknown_mode_name_matches_nothing() {
        let s = published(1).read();
        assert!(!s.modes.iter().any(|m| m.name.eq_ignore_ascii_case("nope")));
    }

    #[test]
    fn a_poisoned_lock_yields_a_default_rather_than_panicking() {
        // The input loop holds the user's mouse. A panicking bus thread must not be able to
        // take it down.
        let (p, _rx) = Published::new(Snapshot::default());
        let inner = p.state.clone();
        let _ = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("bus thread died");
        })
        .join();

        assert_eq!(p.read().mode_slot, 0, "read must still return");
        p.publish(|s| s.mode_slot = 9);
    }
}
