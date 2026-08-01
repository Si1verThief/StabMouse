//! Which application is **under the daemon's own position**, and whether it can take a pen.
//!
//! Feeds D20: a mode stays as the user chose it, and only the transport changes when the
//! application receiving the pen cannot accept one.
//!
//! # The layout comes to us; the hit-test is ours
//!
//! Tablet input is delivered by *position*, so the surface under the tool is what has to
//! accept a pen. Asking the compositor "what is under the cursor" cannot answer that for a
//! pen: `workspace.cursorPos` follows only the *pointer's* cursor, and a tablet tool drives
//! its own — measured in P6 after being observed in use (2026-08-01) as hover switching going
//! blind whenever the pen was the thing moving. (The theory had been floated once before,
//! untested, to explain a silence that was really a frozen script and a bad D-Bus signature.)
//!
//! So the question is inverted (D23). The KWin script ships the **window layout** — class,
//! pid, geometry, stacking order — whenever it changes, and the daemon hit-tests its own
//! shared position against it. The daemon always knows where it is, on every transport; the
//! layout tells it what lives there. Hover switching therefore works identically whether the
//! pen, the absolute pointer, or nothing at all is moving the cursor.
//!
//! # Why a KWin script
//!
//! There is no protocol route. Measured against a live session: of 67 advertised Wayland
//! globals, none of `org_kde_plasma_window_management`, `zwlr_foreign_toplevel_manager_v1`,
//! `ext_foreign_toplevel_list_v1` or `zcosmic_toplevel_info_v1` is offered to an ordinary
//! client — KWin reserves window management for privileged ones. See D19.
//!
//! # KWin registers a script under its **path**, not the name you give it
//!
//! `loadScript(path, name)` accepts a plugin name and ignores it. Measured:
//!
//! ```text
//! loadScript("/tmp/stabmouse-focus.js", "stabmouse-focus")  -> 15
//! isScriptLoaded("stabmouse-focus")                          -> false
//! isScriptLoaded("/tmp/stabmouse-focus.js")                  -> true
//! ```
//!
//! This cost days. Unloading by the chosen name matched nothing, so the script was never
//! removed — and `loadScript` on an already-registered path returns the existing id **without
//! re-reading the file**. Every daemon start therefore kept running the *first* script ever
//! loaded from that path. When the D-Bus method it called was later renamed, the frozen copy
//! went on calling the old name, silently, and per-application switching simply stopped. It
//! looked like a logic bug and was not one.
//!
//! So everything here refers to the script by its **path**. Unloading on start as well as on
//! exit then genuinely works, which also makes a crash self-healing.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const SCRIPT_NAME: &str = "stabmouse-focus";

/// Where the script lives, which is also the identity KWin knows it by.
fn script_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{SCRIPT_NAME}.js"))
}

/// The KWin script: it ships the **window layout**, not "what is under the cursor".
///
/// The cursor question cannot be answered for a pen — `cursorPos` only tracks the pointer's
/// cursor, and a tablet tool drives its own (P6). But the daemon always knows where its own
/// position is; what it lacks is what lives there. So the script maintains a picture of the
/// layout — class, pid, geometry, in stacking order — and re-sends it whenever it changes.
/// The daemon hit-tests locally, which works identically on every transport (D23).
///
/// `cursorPosChanged` is still reported, rate-limited, for a different job: it is how motion
/// from devices StabMouse does not manage — a second mouse, a touchpad — re-syncs the shared
/// position. All values cross as strings: a JavaScript number marshals as int32, which
/// silently fails to match any other signature.
const SCRIPT: &str = r#"
var SERVICE = "io.github.si1verthief.StabMouse";
var PATH = "/io/github/si1verthief/StabMouse";
var IFACE = "io.github.si1verthief.StabMouse.Daemon";

function onCurrentDesktop(w) {
    if (w.onAllDesktops) { return true; }
    var ds = w.desktops;
    if (!ds || !ds.length) { return true; }
    // Compared by id, never by object identity: the script engine may hand out a fresh
    // wrapper per property read, and `===` between wrappers of the same desktop can be
    // false — which would silently drop every non-sticky window from the layout.
    var current = workspace.currentDesktop ? workspace.currentDesktop.id : null;
    if (current === null) { return true; }
    for (var i = 0; i < ds.length; i++) {
        if (ds[i] && ds[i].id === current) { return true; }
    }
    return false;
}

var lastLayout = 0;
function sendLayout() {
    lastLayout = Date.now();
    var rows = [];
    var stack = workspace.stackingOrder;
    for (var i = 0; i < stack.length; i++) {
        var w = stack[i];
        if (!w || w.minimized || !onCurrentDesktop(w)) { continue; }
        var g = w.frameGeometry;
        if (!g) { continue; }
        rows.push([String(w.resourceClass || ""), String(w.pid || 0),
                   String(g.x), String(g.y),
                   String(g.width), String(g.height)].join("\t"));
    }
    callDBus(SERVICE, PATH, IFACE, "WindowLayout", rows.join("\n"));
}

// A raise triggered by a click lands in the stacking order *after* windowActivated has
// already fired (alt-tab raises first — measured, which is why alt-tab looked fine while
// clicks stayed stale). There is no restack signal to connect to on this KWin, so a short
// settle timer re-snapshots once the raise has landed. QTimer is available to scripts even
// though setTimeout is not — measured.
var settle = (typeof QTimer === "function") ? new QTimer() : null;
if (settle) {
    settle.singleShot = true;
    settle.interval = 80;
    settle.timeout.connect(sendLayout);
}

function hook(w) {
    if (!w) { return; }
    w.frameGeometryChanged.connect(sendLayout);
    w.minimizedChanged.connect(sendLayout);
}

var existing = workspace.stackingOrder;
for (var i = 0; i < existing.length; i++) { hook(existing[i]); }
workspace.windowAdded.connect(function (w) { hook(w); sendLayout(); });
workspace.windowRemoved.connect(sendLayout);
workspace.currentDesktopChanged.connect(sendLayout);

// The restack signal itself is absent from this KWin's scripting API (measured: 6.7.3
// exposes no workspace.stackingOrderChanged). Connected anyway, guarded, so a KWin that
// gains it starts using it with no change here.
if (workspace.stackingOrderChanged) {
    workspace.stackingOrderChanged.connect(sendLayout);
}

// Activation also reports the window directly. This is the belt-and-braces degraded path:
// if anything above ever breaks — a renamed signal, an API change — clicking a window still
// drives the transport, which is precisely what rescued earlier versions of this script.
workspace.windowActivated.connect(function (w) {
    sendLayout();
    if (settle) { settle.start(); }
    callDBus(SERVICE, PATH, IFACE, "PointerOverWindow",
             String((w && w.resourceClass) ? w.resourceClass : ""),
             String((w && w.pid) ? w.pid : 0));
});

var lastCursorSent = 0;
workspace.cursorPosChanged.connect(function () {
    var now = Date.now();
    if (now - lastCursorSent < 16) { return; }
    lastCursorSent = now;
    var p = workspace.cursorPos;
    callDBus(SERVICE, PATH, IFACE, "CursorMoved", String(p.x), String(p.y));
    // Self-heal: while the pointer is moving, keep the layout fresh even for restacks that
    // announced themselves with no signal at all — a lower, a wheel-raise, a script. Stale
    // regions route to the pointer transport, which moves the cursor, which lands here — so
    // a wrong region corrects itself within ~150ms of being hovered.
    if (now - lastLayout > 150) { sendLayout(); }
});

sendLayout();
"#;

/// What is under the pointer: its window class, and what inspection established about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Under {
    pub class: String,
    /// `None` when the process could not be inspected at all, which is different from "no".
    pub signal: Option<Signal>,
}

/// One window of the compositor's layout, with its inspection verdict precomputed.
///
/// The verdict is computed when the layout arrives, on the D-Bus thread, so a hit-test on the
/// input loop is rectangle arithmetic and nothing else — no `/proc` reads between a mouse
/// report and the cursor moving.
#[derive(Clone, Debug)]
pub struct Win {
    pub class: String,
    pub signal: Option<Signal>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Win {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Default)]
struct State {
    /// The window under the *pointer cursor*, as the compositor reported it. The degraded
    /// hover source: `cursorPos` never tracks a tablet tool (P6), so during tablet transport
    /// this is stale — the layout hit-test below is what works everywhere.
    under: Under,
    /// The compositor's window layout, bottom to top of the stacking order.
    windows: Vec<Win>,
    /// The compositor's cursor position, when it moved without us moving it. Consumed by the
    /// input loop to re-sync the shared position (D23).
    external_cursor: Option<(f64, f64)>,
}

/// Window knowledge shared between the D-Bus thread and the input loop.
#[derive(Clone, Default)]
pub struct Focus(Arc<Mutex<State>>);

impl Focus {
    /// Record what is under the pointer, inspecting the process to decide.
    ///
    /// Runs on the D-Bus thread, never the input loop: the first look at a large executable
    /// reads it from disk, and that must not sit between a mouse report and the cursor moving.
    pub fn set(&self, class: &str, pid: u32, cache: &mut std::collections::HashMap<String, bool>) {
        let signal = if pid == 0 {
            None
        } else {
            detect_tablet_support(pid, cache)
        };
        if let Ok(mut guard) = self.0.lock() {
            guard.under = Under {
                class: class.to_string(),
                signal,
            };
        }
    }

    pub fn get(&self) -> Under {
        self.0
            .lock()
            .map(|g| g.under.clone())
            .unwrap_or_default()
    }

    /// Replace the layout. Verdicts are the caller's to have precomputed — see [`Win`].
    pub fn set_layout(&self, windows: Vec<Win>) {
        if let Ok(mut guard) = self.0.lock() {
            guard.windows = windows;
        }
    }

    /// The topmost window containing the point, by the daemon's own position.
    ///
    /// This is what makes hover detection work on *every* transport: the compositor will not
    /// say what is under a pen (P6), but the daemon always knows where its own position is,
    /// and the layout says what lives there.
    pub fn window_at(&self, x: f64, y: f64) -> Option<Under> {
        let guard = self.0.lock().ok()?;
        guard
            .windows
            .iter()
            .rev()
            .find(|w| w.contains(x, y))
            .map(|w| Under {
                class: w.class.clone(),
                signal: w.signal,
            })
    }

    /// Record the compositor's cursor position, from its own report.
    pub fn report_cursor(&self, x: f64, y: f64) {
        if let Ok(mut guard) = self.0.lock() {
            guard.external_cursor = Some((x, y));
        }
    }

    /// Take the most recent reported cursor position, if any arrived since the last take.
    pub fn take_external_cursor(&self) -> Option<(f64, f64)> {
        self.0.lock().ok()?.external_cursor.take()
    }
}

/// Parse the layout the KWin script sends: one window per line, bottom to top,
/// `class \t pid \t x \t y \t width \t height`.
///
/// A plain format instead of JSON so nothing new is depended on and a malformed line costs
/// that line, not the message.
pub fn parse_layout(text: &str) -> Vec<(String, u32, f64, f64, f64, f64)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let class = f.next()?.trim().to_string();
            let pid = f.next()?.trim().parse().unwrap_or(0);
            let x = f.next()?.trim().parse().ok()?;
            let y = f.next()?.trim().parse().ok()?;
            let width: f64 = f.next()?.trim().parse().ok()?;
            let height: f64 = f.next()?.trim().parse().ok()?;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some((class, pid, x, y, width, height))
        })
        .collect()
}

/// Call a one-argument scripting method.
fn scripting(method: &str, arg: &str) -> bool {
    Command::new("busctl")
        .args([
            "--user", "call", "org.kde.KWin", "/Scripting",
            "org.kde.kwin.Scripting", method, "s", arg,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install the tracking script. Returns whether it is available.
///
/// Best-effort: a desktop with no such source loses per-application transport selection and
/// nothing else. Tablet and mouse modes keep working, switched by hand.
pub fn install() -> bool {
    let path = script_path();
    let Some(path) = path.to_str().map(str::to_string) else {
        return false;
    };

    // Unload *before* writing, and by path. Loading a path KWin already knows returns the old
    // id and does not re-read the file, so without this the daemon runs whatever was loaded
    // first and no edit to this script ever takes effect.
    let _ = scripting("unloadScript", &path);

    if std::fs::write(&path, SCRIPT).is_err() {
        return false;
    }

    // Whether KWin *accepted* it, not merely whether busctl ran. A negative id means refused,
    // and reporting that as success is how "per-application transport: on" came to be printed
    // by a daemon whose script was doing nothing.
    let Ok(out) = Command::new("busctl")
        .args([
            "--user", "call", "org.kde.KWin", "/Scripting",
            "org.kde.kwin.Scripting", "loadScript", "s", &path,
        ])
        .output()
    else {
        return false;
    };
    let id: i32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(-1);
    if id < 0 {
        return false;
    }
    Command::new("busctl")
        .args([
            "--user", "call", "org.kde.KWin", "/Scripting",
            "org.kde.kwin.Scripting", "start",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
    true
}

/// Detach the script from the compositor.
pub fn remove() {
    let path = script_path();
    if let Some(path) = path.to_str() {
        let _ = scripting("unloadScript", path);
    }
    let _ = std::fs::remove_file(&path);
}

/// Whether an application can receive tablet input.
///
/// **Keyed on toolkit, not application** — see [`Signal`] and [`explain`]. For the major
/// toolkits binding is not an application decision at all: Qt's and GDK's Wayland backends bind
/// the tablet manager whenever the compositor offers it, for every application. So detecting
/// *which toolkit the process loaded* answers the question for most of the desktop, and the
/// per-application entries are the exceptions: applications that implement the protocol
/// themselves, and the user's own overrides.
///
/// **Unknown means the pointer.** The costs are not symmetric: sending the pen to a window that
/// cannot take one loses *clicking entirely*, and nothing on screen says why (D18). Sending the
/// pointer to a window that could have taken a pen loses pressure in that one application —
/// visible, and correctable with a one-line `[tablet_support]` override. So unknowns get the
/// transport that always works, and the job of this module is to make accurate positives broad
/// enough that "unknown" is rare.
pub fn supports_tablet(under: &Under, overrides: &[(String, bool)]) -> bool {
    explain(under, overrides).0
}

/// The same verdict, with the reason that produced it.
///
/// # Capability is not use, and on Wayland nothing bridges the gap
///
/// This once granted the pen to any application whose *toolkit* binds `tablet_v2`, which is
/// every Qt and GTK application there is. That was wrong, and wrong in the worst direction.
///
/// **X11 and Wayland differ in a way that decides this question.** Under XWayland a tablet
/// arrives as an XInput device, and the X server emulates core pointer events from it — so
/// every X11 application, whatever it knows about tablets, receives working motion, hover and
/// clicks. Wayland has no equivalent: `wl_pointer` and `zwp_tablet_tool_v2` are separate
/// protocols with separate focus, and **nothing converts one into the other**. A Wayland
/// application that does not implement tablet handling itself receives *nothing at all* from a
/// pen — no hover, no clicks — while its cursor moves normally, which reads as the application
/// being frozen rather than as an input mismatch.
///
/// Observed exactly that way in use (2026-08-01): in a drawing mode, KDE's own panel and every
/// Qt window stopped highlighting or accepting clicks, because plasmashell is Qt and was
/// therefore being sent a pen it has no code to receive.
///
/// So the toolkit tier is **demoted to a hint**: it says an application *could* take a pen if
/// the user says it does, and nothing more. The ladder, strongest claim first:
///
/// 1. **The user's `[tablet_support]` entry.** Always wins; the correction mechanism for
///    everything below, and the way to promote an application this list has not heard of.
/// 2. **The built-in list of applications that actually handle a pen.** Not toolkits —
///    programs, verified by the fact that pen support is a feature they advertise.
/// 3. **X11 under XWayland**, where core pointer emulation makes a pen safe for *any*
///    application, including Wine and Proton.
/// 4. **Everything else takes the pointer**, which always works.
///
/// The earlier allow-list of two was closer to right than the toolkit tier that replaced it.
/// What survives from that change is the part that was genuinely broken: GTK3 was being
/// condemned by a scan of `libgtk-3` when its Wayland code lives in `libgdk-3`, and X11 was
/// being computed and then ignored.
pub fn explain(under: &Under, overrides: &[(String, bool)]) -> (bool, &'static str) {
    let class = under.class.trim();

    // A user entry always wins, and is the way to correct anything below.
    if !class.is_empty() {
        for (name, supported) in overrides {
            if name.eq_ignore_ascii_case(class) {
                return (*supported, "your config's [tablet_support]");
            }
        }
    }

    if draws_with_a_pen(class) {
        return (true, "a known drawing application");
    }

    match under.signal {
        // The one tier where the *platform* guarantees it, rather than the application. X's
        // core pointer emulation turns tablet input into ordinary clicks and motion for every
        // client, which is also what makes Wine and Proton drawing software work.
        Some(Signal::X11) => (true, "an X11 window — X emulates pointer events from a pen"),

        // Capability, not use. Named individually so the reason a window is on the pointer is
        // legible, and so the message can say what would change it.
        Some(Signal::QtWayland) => (
            false,
            "Qt on Wayland: able to receive a pen, but only if the application handles one — \
             add it to [tablet_support] if it does",
        ),
        Some(Signal::Gtk) => (
            false,
            "GTK on Wayland: able to receive a pen, but only if the application handles one — \
             add it to [tablet_support] if it does",
        ),
        Some(Signal::Sdl3) => (
            false,
            "SDL3 on Wayland: able to receive a pen, but only if the application handles one — \
             add it to [tablet_support] if it does",
        ),
        Some(Signal::Chromium) => (
            false,
            "Chromium-based; pen support varies by build, so the pointer is used — \
             [tablet_support] overrides this",
        ),
        Some(Signal::CannotBind) => {
            (false, "contains no tablet protocol code, so the pointer is used")
        }
        Some(Signal::Unproven) | None => (
            false,
            "not a known pen application, so the pointer is used — [tablet_support] overrides this",
        ),
    }
}

/// Whether an application needs the pen held still before a scroll will reach it.
///
/// **Keyed on the application, not on the mode.** Krita ignores mouse input while a pen is in
/// proximity — a time-based filter that every tablet event resets — so the wheel only arrives
/// once the pen has been quiet. Blender does not filter that way and scrolls perfectly well
/// while the pen moves, so freezing it there would remove something that worked.
///
/// **Unlisted applications are not frozen**, for the same asymmetry that governs the pen tier:
/// doing nothing to an application that turned out to need it shows up as a scroll that does
/// not work, which is a thing the user can see and name, while freezing one that did not want
/// it silently removes the ability to move and scroll at once. `default_freeze` lets a profile
/// flip that for everything unnamed, and `[scroll_freeze]` names exceptions in either
/// direction.
pub fn needs_scroll_freeze(
    class: &str,
    overrides: &[(String, bool)],
    default_freeze: bool,
) -> bool {
    let class = class.trim();
    if !class.is_empty() {
        for (name, freeze) in overrides {
            if name.eq_ignore_ascii_case(class) {
                return *freeze;
            }
        }
    }

    // Applications measured to filter mouse input during pen proximity. Short on purpose: an
    // entry here costs its user the ability to move while scrolling, so a name belongs on it
    // only once the behaviour has actually been seen.
    const FILTERS_MOUSE_DURING_PROXIMITY: &[&str] = &["krita"];

    let lower = class.to_ascii_lowercase();
    if FILTERS_MOUSE_DURING_PROXIMITY
        .iter()
        .any(|k| lower == *k || lower.starts_with(&format!("{k}-")))
    {
        return true;
    }
    default_freeze
}

/// Applications known to implement pen input on Wayland.
///
/// **Programs, not toolkits** — see [`explain`]. Membership means the application handles
/// `tablet_v2` itself, which is a feature its authors chose and advertise, not something a
/// linked library can confer. Kept short and honest: a name that does not belong here costs
/// its user every click in that window, and `[tablet_support]` is one line for anything missing.
///
/// Matched against the compositor's `resourceClass`, which often carries a version suffix —
/// `gimp-2.10` — so a trailing `-…` still matches. `stabmouse-probe focus` prints the exact
/// strings a compositor reports.
fn draws_with_a_pen(class: &str) -> bool {
    const KNOWN: &[&str] = &[
        "krita",
        "blender",
        "gimp",
        "inkscape",
        "mypaint",
        "drawpile",
        "xournalpp",
        "aseprite",
        "kolourpaint",
        "openboard",
    ];
    let lower = class.to_ascii_lowercase();
    KNOWN
        .iter()
        .any(|k| lower == *k || lower.starts_with(&format!("{k}-")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn under(class: &str, signal: Option<Signal>) -> Under {
        Under { class: class.into(), signal }
    }

    #[test]
    fn a_pen_capable_toolkit_is_not_enough_on_its_own() {
        // The regression this exists to prevent: every Qt and GTK window was being handed a
        // pen, and a Wayland application with no tablet handling receives *nothing* from one —
        // no hover, no clicks — because nothing bridges wl_pointer and tablet_v2. KDE's own
        // panel is Qt, so in a drawing mode the whole desktop stopped responding.
        assert!(!supports_tablet(&under("org.kde.dolphin", Some(Signal::QtWayland)), &[]));
        assert!(!supports_tablet(&under("plasmashell", Some(Signal::QtWayland)), &[]));
        assert!(!supports_tablet(&under("org.gnome.Nautilus", Some(Signal::Gtk)), &[]));
        assert!(!supports_tablet(&under("some-sdl3-game", Some(Signal::Sdl3)), &[]));
    }

    #[test]
    fn known_drawing_applications_get_the_pen_whatever_their_toolkit() {
        for class in ["krita", "blender", "gimp", "inkscape", "mypaint", "xournalpp"] {
            assert!(supports_tablet(&under(class, Some(Signal::QtWayland)), &[]), "{class}");
            assert!(supports_tablet(&under(class, None), &[]), "{class} without inspection");
        }
    }

    #[test]
    fn a_versioned_class_still_matches_its_application() {
        // Compositors report `gimp-2.10`, not `gimp`.
        assert!(supports_tablet(&under("gimp-2.10", Some(Signal::Gtk)), &[]));
        assert!(supports_tablet(&under("Krita", None), &[]), "and case does not matter");
        // But a different program that merely starts with the same letters must not match.
        assert!(!supports_tablet(&under("gimpshop-clone", None), &[]));
    }

    #[test]
    fn an_x11_window_always_gets_the_pen() {
        // The one tier where the platform guarantees it rather than the application: X's core
        // pointer emulation gives every client working clicks and hover from tablet input, so
        // unlike the Wayland toolkits this is safe without knowing the program. It is also the
        // route Wine and Proton drawing applications take.
        assert!(supports_tablet(&under("clipstudiopaint.exe", Some(Signal::X11)), &[]));
        assert!(supports_tablet(&under("some-old-x11-app", Some(Signal::X11)), &[]));
    }

    #[test]
    fn unproven_and_absent_both_take_the_pointer() {
        // The costs are asymmetric: a wrong pen loses clicking entirely and silently; a wrong
        // pointer loses pressure visibly. Unknowns therefore get the transport that always works.
        assert!(!supports_tablet(&under("firefox", Some(Signal::CannotBind)), &[]));
        assert!(!supports_tablet(&under("vivaldi-stable", Some(Signal::Chromium)), &[]));
        assert!(!supports_tablet(&under("discord", None), &[]));
    }

    #[test]
    fn chromium_is_not_mistaken_for_the_toolkits_it_maps() {
        // Measured live: Vivaldi maps Qt's Wayland client and GTK's gdk for *theming*, while
        // its windows belong to its own Ozone code. The toolkit tier must not answer for it.
        let vivaldi = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                       7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libQt6WaylandClient.so.6\n\
                       7f04-7f05 r-xp 00000000 08:02 3 /usr/lib/libgdk-3.so.0\n\
                       7f06-7f07 r--p 00000000 08:02 4 /opt/vivaldi/resources.pak\n";
        let c = classify(vivaldi);
        assert!(c.chromium);
        // An Electron shell maps gdk plus the v8 snapshot, and no Qt.
        let codium = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                      7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libgdk-3.so.0\n\
                      7f04-7f05 r--p 00000000 08:02 3 /usr/share/codium/v8_context_snapshot.bin\n";
        assert!(classify(codium).chromium);
        // And a plain Qt application must not trip the marker.
        let konsole = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                       7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libQt6WaylandClient.so.6\n";
        assert!(!classify(konsole).chromium);
    }

    #[test]
    fn the_builtin_list_outranks_inspection() {
        // Krita in a Flatpak: its libraries are unreadable from outside the sandbox, so
        // inspection can be arbitrarily wrong about it. The named list keeps its pen anyway.
        assert!(supports_tablet(&under("krita", Some(Signal::CannotBind)), &[]));
        assert!(supports_tablet(&under("blender", Some(Signal::Unproven)), &[]));
    }

    #[test]
    fn nothing_under_the_pointer_uses_the_pointer() {
        assert!(!supports_tablet(&under("", None), &[]));
    }

    #[test]
    fn only_applications_that_filter_the_mouse_freeze_the_pen() {
        // The distinction that makes this per-application: Krita needs the pen quiet before a
        // wheel reaches it, Blender scrolls fine mid-movement and freezing it would take away
        // something that worked.
        assert!(needs_scroll_freeze("krita", &[], false));
        assert!(!needs_scroll_freeze("blender", &[], false));
    }

    #[test]
    fn an_unlisted_application_is_left_alone_by_default() {
        // Same asymmetry as the pen tier: doing nothing is visible and correctable, doing
        // something unwanted quietly removes a capability.
        assert!(!needs_scroll_freeze("inkscape", &[], false));
        // ...unless the profile asks for the opposite.
        assert!(needs_scroll_freeze("inkscape", &[], true));
    }

    #[test]
    fn a_scroll_freeze_entry_wins_in_either_direction() {
        let off = vec![("krita".to_string(), false)];
        assert!(!needs_scroll_freeze("krita", &off, false), "a user may switch it off");
        let on = vec![("blender".to_string(), true)];
        assert!(needs_scroll_freeze("blender", &on, false), "and on");
        // An override also beats the profile default.
        let no = vec![("gimp".to_string(), false)];
        assert!(!needs_scroll_freeze("gimp", &no, true));
    }

    #[test]
    fn a_versioned_class_still_matches_the_freeze_list() {
        assert!(needs_scroll_freeze("krita-5.2", &[], false));
    }

    #[test]
    fn the_topmost_window_wins_the_hit_test() {
        let f = Focus::default();
        f.set_layout(vec![
            Win { class: "below".into(), signal: None, x: 0.0, y: 0.0, width: 1000.0, height: 1000.0 },
            Win { class: "above".into(), signal: Some(Signal::QtWayland), x: 100.0, y: 100.0, width: 200.0, height: 200.0 },
        ]);
        // The layout arrives bottom-to-top, so the last containing window is the visible one.
        assert_eq!(f.window_at(150.0, 150.0).unwrap().class, "above");
        assert_eq!(f.window_at(50.0, 50.0).unwrap().class, "below");
        assert!(f.window_at(2000.0, 50.0).is_none(), "a miss must fall to the degraded source");
    }

    #[test]
    fn a_bad_layout_line_costs_only_itself() {
        let text = "krita\t123\t0\t0\t1920\t1080\nnot a line\ngimp\t456\t1920\t0\t1280\t1024";
        let rows = parse_layout(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "krita");
        assert_eq!(rows[1], ("gimp".to_string(), 456, 1920.0, 0.0, 1280.0, 1024.0));
    }

    #[test]
    fn a_zero_sized_window_is_dropped() {
        // A window mid-construction can report empty geometry; hit-testing it would be noise.
        assert!(parse_layout("x\t1\t0\t0\t0\t100").is_empty());
    }

    #[test]
    fn the_external_cursor_is_taken_once() {
        // Consumed, not read: adopting the same report every loop iteration would fight the
        // very motion it is meant to reconcile.
        let f = Focus::default();
        assert!(f.take_external_cursor().is_none());
        f.report_cursor(100.0, 200.0);
        assert_eq!(f.take_external_cursor(), Some((100.0, 200.0)));
        assert!(f.take_external_cursor().is_none());
    }

    #[test]
    fn a_user_entry_overrules_everything() {
        let overrides = vec![("firefox".to_string(), true), ("krita".to_string(), false)];
        assert!(supports_tablet(&under("firefox", Some(Signal::CannotBind)), &overrides));
        assert!(!supports_tablet(&under("krita", Some(Signal::QtWayland)), &overrides));
    }

    #[test]
    fn overrides_match_case_insensitively() {
        let overrides = vec![("Firefox".to_string(), true)];
        assert!(supports_tablet(&under("firefox", None), &overrides));
    }

    #[test]
    fn qt_is_recognised_from_a_map() {
        let maps = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libQt6WaylandClient.so.6.9.1\n";
        let c = classify(maps);
        assert!(c.uses_wayland);
        assert_eq!(c.toolkit, Some(Signal::QtWayland));
    }

    #[test]
    fn gtk3_is_recognised_by_gdk_not_gtk() {
        // libgtk-3 does not contain the Wayland code; libgdk-3 does. Recognising GTK3 by the
        // wrong library is the bug that condemned GIMP.
        let gtk3 = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libgdk-3.so.0.2405.32\n\
                    7f04-7f05 r-xp 00000000 08:02 3 /usr/lib/libgtk-3.so.0.2405.32\n";
        assert_eq!(classify(gtk3).toolkit, Some(Signal::Gtk));

        let gtk4 = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libgtk-4.so.1.1800.4\n";
        assert_eq!(classify(gtk4).toolkit, Some(Signal::Gtk));
    }

    #[test]
    fn sdl2_is_not_sdl3() {
        // SDL2 has no tablet support; only SDL3's Wayland driver binds the tablet manager.
        let maps = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libSDL2-2.0.so.0\n";
        assert_eq!(classify(maps).toolkit, None);
    }

    #[test]
    fn a_sandboxed_toolkit_is_recognised_by_name_alone() {
        // The Flatpak prefix makes the file unopenable from outside, but the basename in the
        // map still names the toolkit, and presence is all the toolkit tier needs.
        let maps = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /app/lib/libgdk-3.so.0\n";
        assert_eq!(classify(maps).toolkit, Some(Signal::Gtk));
    }

    #[test]
    fn no_wayland_library_means_x11() {
        let maps = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libX11.so.6\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/libc.so.6\n";
        assert!(!classify(maps).uses_wayland);
    }

    #[test]
    fn only_libxul_is_scanned_from_the_map() {
        // The executable is added from /proc/<pid>/exe, not matched here. An earlier version
        // scanned every non-.so mapping, which swept up locale archives and resource packs.
        let maps = "7f00-7f01 r-xp 00000000 08:02 1 /usr/lib/libwayland-client.so.0\n\
                    7f02-7f03 r-xp 00000000 08:02 2 /usr/lib/firefox/libxul.so\n\
                    7f04-7f05 r-xp 00000000 08:02 3 /usr/lib/locale/locale-archive\n\
                    7f06-7f07 r-xp 00000000 08:02 4 /usr/lib/libcrypto.so.3\n";
        assert_eq!(classify(maps).scan, vec!["/usr/lib/firefox/libxul.so"]);
    }

    #[test]
    fn an_override_keyed_on_nothing_matches_nothing() {
        let overrides = vec![("".to_string(), true)];
        assert!(!supports_tablet(&under("", None), &overrides));
    }

    #[test]
    fn a_path_containing_spaces_survives_parsing() {
        // Blender lives under a Steam library path with a space in it, and truncating there
        // made a drawing application look incapable of accepting a pen.
        let line = "7f00-7f01 r-xp 00000000 08:02 1234    /mnt/c730/Programs/Games etc/Steam/blender";
        assert_eq!(
            field_six(line),
            Some("/mnt/c730/Programs/Games etc/Steam/blender")
        );
    }

    #[test]
    fn an_anonymous_mapping_has_no_path() {
        assert_eq!(field_six("7f00-7f01 rw-p 00000000 00:00 0"), None);
    }

    #[test]
    fn this_very_process_is_inspectable() {
        let mut cache = std::collections::HashMap::new();
        let me = std::process::id();
        // The daemon maps no Wayland client library, so it classifies as X11.
        assert_eq!(detect_tablet_support(me, &mut cache), Some(Signal::X11));
    }

    #[test]
    fn a_process_that_does_not_exist_yields_no_answer() {
        let mut cache = std::collections::HashMap::new();
        assert_eq!(detect_tablet_support(u32::MAX, &mut cache), None);
    }
}

// ---------------------------------------------------------------- runtime detection

/// The interface a client must bind to receive a pen at all.
const TABLET_INTERFACE: &[u8] = b"zwp_tablet_manager_v2";

/// What inspecting the process established, strongest claim first.
///
/// All but the last two are properties of *how the process talks to the display*, and each
/// decides the answer by itself. The last two are the honest names for what a string search
/// can prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// No Wayland client library is mapped, so the window reaches the compositor through
    /// XWayland — which emulates a wacom stylus for exactly this case (since 21.1). Pressure
    /// arrives as XI2 valuators, clicks as core-pointer emulation, so a pen is always safe
    /// here. This is also the path Wine and Proton applications take.
    X11,
    /// Qt's Wayland platform plugin is loaded. QtWayland binds the tablet manager whenever the
    /// compositor offers it, for every application — the toolkit answers, not the app.
    QtWayland,
    /// GDK's Wayland backend is loaded. GDK also binds the tablet manager unconditionally.
    /// GTK3 keeps it in `libgdk-3`; GTK4 merged GDK into `libgtk-4` — scanning `libgtk-3` for
    /// it was the wrong-file bug that once condemned every GTK3 application.
    Gtk,
    /// SDL3 is loaded, whose Wayland video driver binds the tablet manager for its pen API and
    /// synthesizes mouse events from pen input by default. SDL2 has no tablet support and
    /// deliberately does not match.
    Sdl3,
    /// Chromium-family (browsers, Electron, CEF), recognised by the data files every build maps
    /// (`resources.pak`, `v8_context_snapshot`, `icudtl.dat`). **Checked before the toolkits,
    /// because it defeats them:** Chromium loads Qt and GTK as theming satellites while its
    /// windowing is its own Ozone code — measured live, Vivaldi maps `libQt6WaylandClient` and
    /// `libgdk-3` and uses neither for its windows. Whether its Ozone build binds the tablet
    /// protocol is version-dependent and unprovable from outside, so this is a named flavour of
    /// unproven rather than a verdict.
    Chromium,
    /// Nothing the process loaded contains the interface string, and everything worth checking
    /// was readable. It cannot bind what it does not contain.
    CannotBind,
    /// The string is present somewhere, or a candidate could not be read (a Flatpak's libraries
    /// are outside our mount namespace). Binding is observable only inside the compositor, so
    /// presence proves nothing — this project's own window embeds the string three times via
    /// `wayland-protocols` and never binds it.
    Unproven,
}

/// A library whose presence in the map decides the answer on its own — see [`Signal`].
///
/// Matched on the basename, not the path, so a Flatpak or Snap prefix does not hide the
/// toolkit: the *name* of the mapped library is visible in `/proc/<pid>/maps` even when the
/// file itself is not openable from outside the sandbox.
fn toolkit_signal(name: &str) -> Option<Signal> {
    if name.contains("WaylandClient") {
        // libQt5WaylandClient / libQt6WaylandClient; only loaded when Qt runs its Wayland
        // platform, so it cannot fire for a Qt application on xcb.
        return Some(Signal::QtWayland);
    }
    if name.starts_with("libgdk-3") || name.starts_with("libgtk-4") {
        return Some(Signal::Gtk);
    }
    if name.starts_with("libSDL3") {
        return Some(Signal::Sdl3);
    }
    None
}

/// Files still worth a string search once no toolkit answered.
///
/// Deliberately short: the point of the search is the applications that carry their own
/// Wayland code — Firefox in `libxul`, and Chromium-family code in the executable itself,
/// which is added from `/proc/<pid>/exe` rather than matched here. An earlier version searched
/// every non-`.so` mapping, which swept up locale archives and resource packs measured in the
/// hundreds of megabytes.
fn scan_worthy(name: &str) -> bool {
    name.starts_with("libxul")
}

/// The pathname field of a `/proc/<pid>/maps` line, which may contain spaces.
///
/// Five fields precede it — address, perms, offset, dev, inode — and the rest of the line is
/// the path verbatim.
fn field_six(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for _ in 0..5 {
        let end = rest.find(char::is_whitespace)?;
        rest = rest[end..].trim_start();
    }
    let path = rest.trim_end();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Whether a file contains the tablet interface string, or `None` when it could not be read.
///
/// Streamed with an overlap rather than read whole: an executable can be hundreds of megabytes,
/// and the string could straddle any chunk boundary.
///
/// Unreadable is **not** the same as absent. A sandboxed application's files are not openable
/// from outside its mount namespace, and reporting that as "contains nothing" would conclude
/// `CannotBind` about applications this says nothing about.
fn file_binds_tablet(path: &std::path::Path) -> Option<bool> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let overlap = TABLET_INTERFACE.len();
    let mut buffer = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = Vec::with_capacity(overlap);

    loop {
        let read = file.read(&mut buffer[..]).ok()?;
        if read == 0 {
            return Some(false);
        }
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&buffer[..read]);
        if window
            .windows(TABLET_INTERFACE.len())
            .any(|w| w == TABLET_INTERFACE)
        {
            return Some(true);
        }
        let keep = window.len().saturating_sub(overlap);
        carry = window[keep..].to_vec();
    }
}

/// A mapped file that marks the process as Chromium-family.
///
/// Data files rather than libraries, because they are what every Chromium build maps under its
/// own name — a browser, an Electron shell and a CEF embed all carry them, and a Flatpak prefix
/// does not hide a basename. The `.pak` resource format is Chromium's own. Measured live:
/// Vivaldi's main process maps only `resources.pak` of these; Codium maps all three.
fn chromium_marker(name: &str) -> bool {
    name.ends_with(".pak")
        || name.starts_with("v8_context_snapshot")
        || name.starts_with("snapshot_blob")
        || name == "icudtl.dat"
        || name == "libcef.so"
}

/// What one pass over `/proc/<pid>/maps` established.
struct Classified<'a> {
    /// Whether `libwayland-client` is mapped at all. Its absence is conclusive: the process
    /// cannot be speaking Wayland, so its window comes through XWayland. Its presence proves
    /// little on its own — Mesa's EGL links it, so plenty of X11 processes map it too.
    uses_wayland: bool,
    /// Whether a Chromium-family marker is mapped. Must be consulted **before** `toolkit`:
    /// Chromium loads the toolkits for theming without windowing through them.
    chromium: bool,
    /// The strongest toolkit tier found, Qt preferred over GTK over SDL3 when several are
    /// mapped, so the verdict does not depend on map order.
    toolkit: Option<Signal>,
    /// Libraries still worth a string search if no toolkit answered.
    scan: Vec<&'a str>,
}

fn classify(maps: &str) -> Classified<'_> {
    let mut c =
        Classified { uses_wayland: false, chromium: false, toolkit: None, scan: Vec::new() };
    let (mut qt, mut gtk, mut sdl) = (false, false, false);
    for line in maps.lines() {
        // The pathname is the sixth field and **may contain spaces**, so it is everything after
        // the fifth, not the sixth whitespace-separated token. Splitting on whitespace truncated
        // Blender's path at "Games etc", opened a file that does not exist, found no tablet
        // string, and reported a drawing application as unable to accept a pen.
        let Some(path) = field_six(line) else {
            continue;
        };
        if !path.starts_with('/') {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.starts_with("libwayland-client") {
            c.uses_wayland = true;
        }
        if chromium_marker(name) {
            c.chromium = true;
        }
        match toolkit_signal(name) {
            Some(Signal::QtWayland) => qt = true,
            Some(Signal::Gtk) => gtk = true,
            Some(Signal::Sdl3) => sdl = true,
            _ => {}
        }
        if scan_worthy(name) && !c.scan.contains(&path) {
            c.scan.push(path);
        }
    }
    c.toolkit = if qt {
        Some(Signal::QtWayland)
    } else if gtk {
        Some(Signal::Gtk)
    } else if sdl {
        Some(Signal::Sdl3)
    } else {
        None
    };
    c
}

/// Ask the process itself. See [`Signal`] for what each answer means and why it holds.
///
/// The tiers, in order:
///
/// 1. **No `libwayland-client` mapped** → [`Signal::X11`]. Conclusive and safe — XWayland
///    translates a pen into exactly what X11 clients expect. The reverse is *not* checked
///    symmetrically: plenty of X11 processes map `libwayland-client` through Mesa, so its
///    presence merely disqualifies this tier rather than proving Wayland.
/// 2. **A Chromium-family marker** → [`Signal::Chromium`]. Before the toolkits, because
///    Chromium maps them without windowing through them and would impersonate them here.
/// 3. **A toolkit whose Wayland backend binds the tablet manager unconditionally** →
///    [`Signal::QtWayland`], [`Signal::Gtk`], [`Signal::Sdl3`]. Presence of the *library*
///    answers for the application, which is what makes the defaults broad without naming apps.
/// 4. **A string search of the few files that carry their own Wayland code** — `libxul` and
///    the executable. Absence everywhere readable is conclusive ([`Signal::CannotBind`]);
///    anything else is honestly [`Signal::Unproven`], because presence cannot prove binding.
///
/// A toolkit application running its X11 backend under XWayland lands in tier 3, not tier 1 —
/// and that is still correct, because both routes deliver a working pen; they merely name
/// different mechanisms.
pub fn detect_tablet_support(pid: u32, cache: &mut std::collections::HashMap<String, bool>) -> Option<Signal> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).ok()?;
    let c = classify(&maps);

    if !c.uses_wayland {
        return Some(Signal::X11);
    }
    if c.chromium {
        return Some(Signal::Chromium);
    }
    if let Some(toolkit) = c.toolkit {
        return Some(toolkit);
    }

    let mut scan: Vec<std::path::PathBuf> =
        c.scan.iter().map(std::path::PathBuf::from).collect();
    // Chromium-family applications carry their Wayland code in the executable itself. Read from
    // the pid rather than guessed from maps, so a wrapper script or a renamed binary cannot
    // hide it.
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        if !scan.contains(&exe) {
            scan.push(exe);
        }
    }

    let mut proven_absent = true;
    for path in scan {
        let key = path.to_string_lossy().into_owned();
        let verdict = match cache.get(&key) {
            Some(known) => Some(*known),
            None => {
                // Cached by path: a library does not change under a running system, so each
                // file is read at most once however many applications map it. Failures are not
                // cached — a file unreadable now may be readable after a permission fix.
                let found = file_binds_tablet(&path);
                if let Some(found) = found {
                    cache.insert(key, found);
                }
                found
            }
        };
        match verdict {
            // Present means "might bind it", which is not an answer — see `Signal::Unproven`.
            Some(true) => return Some(Signal::Unproven),
            Some(false) => {}
            None => proven_absent = false,
        }
    }
    Some(if proven_absent {
        Signal::CannotBind
    } else {
        Signal::Unproven
    })
}
