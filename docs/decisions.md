# Decision log

Short records of choices made, with the reasoning that drove them, so they can be
revisited when circumstances change rather than re-litigated from scratch.

---

## D1 — Rust, not Python

**Decision:** the daemon and core are Rust.

**Why:** most code here is LLM-generated, which inverts the usual "Python is more
forgiving" argument — forgiving means deferring an LLM's type and control-flow
mistakes to runtime. A strict compiler substitutes for review that is no longer
happening line by line. The failure mode is also severe: a runtime panic in a
daemon holding `EVIOCGRAB` leaves the user unable to move their cursor to fix it.

Ownership here is simple (single-threaded event loop over a config struct), which
avoids the borrow-checker thrash that makes LLM-generated Rust painful.

**Kept from Python:** an offline stroke-replay harness using numpy/matplotlib,
calling into the core via PyO3 so it tests the exact production code.

---

## D2 — Live tunability over recompilation

**Decision:** every filter parameter is runtime-adjustable via hot-reloaded TOML
and GUI sliders. No tuning constant is baked in.

**Why:** the real iteration cost of this project is *feel*, not code. Hundreds of
adjustments to pulled-string radius, one-euro beta/mincutoff, pressure envelope
shape. Once tuning never requires a recompile, the language choice stops
affecting iteration speed at all.

---

## D3 — Slint over Qt/QML

**Decision:** Slint for the GUI.

**Why:** one language and one binary — QML would mean shipping a Qt6 runtime
alongside a Rust daemon, which is the single biggest packaging burden available.
Slint also targets Windows/macOS from the same source, has live preview in
VS Code, and GPLv3 covers open-source use.

**Accepted cost:** thinner widget library, more design hand-rolled, less LLM
training data than QML. Minor here, since a tuning UI is sliders plus a curve
editor plus a preview canvas, and the interesting parts would be hand-rolled in
QML too.

**Rejected:** egui/imgui (immediate mode repaints every frame — constant CPU for a
static settings window), Tauri (~150MB WebKitGTK for a settings dialog).

---

## D4 — Separate GUI process over D-Bus

**Decision:** headless daemon; GUI is a separate optional binary speaking D-Bus.

**Why:** idle CPU becomes zero because the GUI is not running. The toolkit choice
stops touching the hot path. Multiple frontends become possible. This is also how
ratbagd/Piper is structured, and it is why a Flatpak GUI talking to a system
daemon stays possible later.

---

## D5 — evdev grab, not a kernel module

**Decision:** userspace `evdev` + `EVIOCGRAB` + `uinput`.

**Why:** it is the mainstream approach (interception-tools, input-remapper,
evsieve, keyd, Steam Input, Steam Deck). Kernel modules mean DKMS rebuilds on
every kernel bump — a maintenance tax paid forever, and a bad experience for
anyone installing this.

**Open question:** the added round trip is ~0.2–0.5ms. Must be benchmarked against
yeetmouse before that module is retired. If it proves perceptible, HID-BPF for
the accel stage is the escape hatch, so keep accel filters separable.

---

## D6 — Linux only

**Decision:** no Windows port.

**Why:** Windows is already served by Lazy Nezumi; Linux has nothing comparable.
Capture and output are full rewrites there, real tablet pressure requires a signed
virtual HID driver, and the anti-cheat picture inverts — kernel anti-cheat is real
on Windows and input filter drivers (Interception especially) are actively
detected, so the accel half would carry genuine ban risk it does not carry here.

**Hedge:** keep platform I/O behind traits so a port remains possible at ~60-70%
code reuse.

---

## D7 — Device configuration is out of scope

**Decision:** StabMouse does not configure mouse hardware (DPI stages, RGB,
onboard profiles). That work goes upstream to libratbag as a separate driver.

**Why:** different direction (host→device), different cadence (occasional, not
1000Hz), different privileges (hidraw vs uinput), different audience. Coupling
them means people installing a vendor config tool to get stroke smoothing, and an
issue tracker full of "please support my mouse."

**Seam:** StabMouse **never speaks hidraw at all** — it listens. It subscribes to
`ratbagd` over D-Bus (which already publishes resolution changes for supported
devices) and additionally exposes a generic `SetDeviceResolution(device, dpi)`
method that any external tool or script may call. A future Mad Catz driver is
then just one more caller, and nothing device-specific enters this codebase.

Knowing the active DPI lets StabMouse adjust its own multiplier so cursor ratio
stays constant across hardware DPI switches — the user gets finer sampling
resolution without their pointer speed changing.

---

## D8 — Presets are global; devices carry overlays

**Decision:** a filter preset is a single global entity. Devices do not own presets;
they own a thin overlay of overridden leaf values. Optional user-defined groups sit
between global defaults and individual devices.

**Why:** it has to serve both ends of the range at once. Someone swapping in a new
mouse should have everything stay the same — an unmatched device inherits global
defaults and needs no setup. Someone running two mice, two CAD pucks and an
occasional tablet should be able to fix a mode once and have it improve
everywhere, while still overriding a radius on one device.

Forked per-device copies fail the second case: the artist edits `inking` five times
and they drift apart.

**Consequences for the config schema:** it needs a cascade with per-key overrides
rather than whole-section copies, identity matching that degrades (serial →
VID:PID → default), and persistence for devices that are not currently connected.

**Groups are user-defined, not an imposed taxonomy.** Most users never create one.

---

## D9 — Preset / mode / profile are three separate things

**Decision:**

- **Preset** — a named filter pipeline plus parameters. Global, reusable, referenced
  by name rather than embedded.
- **Mode** — a *slot* holding an output type (mouse or tablet) and a preset
  reference. Lives inside a profile. Positional and per-profile.
- **Profile** — a named set of mode slots for one activity, plus which slot is
  default.

**Why:** these change on completely different rhythms, and collapsing any two of
them breaks a real use case.

Mode switching is the **headline interaction** — hotkey, mid-task, dozens of times
a session (click in Gartic Phone → draw → click). Profile switching is deliberate
and rare. Preset editing is a long tuning session. One concept cannot serve all
three.

Modes being *slots* rather than device types is what allows two mouse modes in one
profile — a fine sensitivity curve for precision and a flat one for tracking — which a
mouse/tablet axis could not express.

Presets being referenced rather than embedded means tuning `raw` improves every
mode in every profile using it. Same reasoning as D8.

**Consequences:**

- Config needs a preset library with name-based references, plus reference
  integrity (renaming or deleting a preset must not silently break profiles).
- The UI must warn at the point of edit that a preset is shared, since surprising
  people with action-at-a-distance is the main risk of the reuse model.
- Mode switching is on the hot path and must be allocation-free and instant —
  it is not a config reload.
- **No cap on slot count** (per the standing preference against arbitrary limits).
  The UI is designed around two to four; direct-select bindings cover the first
  four; cycling handles the rest.
