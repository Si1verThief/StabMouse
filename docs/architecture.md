# StabMouse Architecture

## What this is

A userspace input daemon that transforms mouse input before applications see it.
Two headline capabilities:

1. **Acceleration / sensitivity curves** — replacing kernel modules like yeetmouse
   and maccel with a userspace equivalent.
2. **Tablet emulation** — presenting a virtual pen with inferred pressure,
   stroke smoothing, and constraint modes, so a mouse becomes usable for 2D art.

Both are the same pipeline with different filters loaded. That is the core insight
the whole design rests on.

## Shape

```
        ┌──────────────────────────┐
        │  core crate (pure, no    │
        │  I/O — the filter pipeline) │
        └────────────┬─────────────┘
             ┌───────┼───────┐
             ▼       ▼       ▼
        ┌────────┐ ┌────┐ ┌──────┐
        │ daemon │ │PyO3│ │ WASM │
        │(evdev  │ │mod │ │      │
        │ uinput)│ │    │ │      │
        └────────┘ └────┘ └──────┘
             │       │        │
             │   numpy stroke  live curve
             │   replay        preview in
             │   harness       the GUI
             ▼
        ┌────────┐   D-Bus   ┌──────────┐
        │ daemon │◄─────────►│ GUI      │
        │        │           │ (Slint)  │
        └────────┘           └──────────┘
```

The **core crate is pure** — deltas in, deltas out, no I/O. That lets the same
filter code run in the daemon, in an offline Python research harness (via PyO3),
and in the GUI as a live preview (via WASM). One algorithm, three consumers,
zero drift between prototype and production.

The **GUI is a separate process**. Consequences: idle CPU is zero because it
isn't running; multiple frontends are possible (full tuning app, Plasma applet,
CLI); and it can never affect the hot path.

## Layers

### L0 — Capture

`evdev` read + `EVIOCGRAB` on the physical device.

This is the mainstream choice: interception-tools, input-remapper, evsieve, keyd,
Steam Input and the Steam Deck's whole input stack all work this way. yeetmouse
and maccel are the outliers, having gone in-kernel specifically for accel latency.

Cost: a kernel→userspace→kernel round trip, **measured at ~13µs median** (p99
0.2ms, p99.9 0.7ms) — see D5. That is well inside the 1ms polling interval of a
1000 Hz mouse, so there is no latency case for going in-kernel.

The remaining latency work is the **tail**, not the median: the outliers are
scheduler jitter rather than compute, so the lever is the hot thread's scheduling
priority. HID-BPF survives as a theoretical escape hatch only; sensitivity filters
stay separable because it is nearly free, not because we expect to need it.

### L1 — Transform

Ordered, config-driven stages over a shared per-event context:

1. **`normalize`** — raw counts → physical units using known DPI. Everything
   downstream becomes DPI-independent, which is what makes "change DPI, keep
   cursor ratio" fall out for free.
2. **`sensitivity`** — a flat multiplier, with curve shapes (linear / power /
   natural / jump / lookup) behind advanced disclosure. Named for the common case,
   not the specialist one — see [vocabulary.md](vocabulary.md).
3. **`smooth` / `stabilize` / `average`** — one-euro, pulled-string, weighted.
4. **`snap`** — line / ellipse / angle constraints, perspective guides.
5. **`pressure`** — tablet mode only. See below.
6. **Subpixel accumulator** — carry the fractional remainder between events.
   Always on, never exposed. **Omitting this silently loses slow motion**, and it
   is the most common bug in this class of software.

Stage order and membership come from config, not code. Different modes load
different presets.

### L2 — Output

- **uinput relative mouse** — universal; works in games, browsers, everything.
- **uinput absolute tablet** — `ABS_X`/`ABS_Y`/`ABS_PRESSURE` plus `BTN_TOOL_PEN`
  and `BTN_TOUCH`. KWin exposes this to clients via `tablet_v2`.

Two output modes matter because browser tablet-pressure support on Wayland is
unreliable, and most simple web canvases ignore pressure anyway. Smoothing-only
output through a plain virtual mouse works *everywhere*.

### L3 — Control plane

D-Bus service (`zbus`), TOML config with hot reload (`notify`), profiles, and
mode switching. Hotkeys should be registered as KDE global shortcuts that call a
D-Bus method — cleaner than grabbing a keyboard device, and it cannot strand your
keyboard if the daemon dies.

### L4 — Presentation

Slint GUI in a separate process. Chosen over Qt/QML for single-language,
single-binary packaging and cross-platform reach; retained-mode so idle cost is
nil; GPLv3 covers open-source use.

## Pressure inference

Three terms, individually weightable:

1. **Time envelope** — pressure ramps in over ~40–80ms after `BTN_TOUCH` and
   ramps out on release. Gives tapered stroke ends, which is most of what reads
   as "hand-drawn."
2. **Speed inverse** — `p ∝ clamp(1 − v/v_max)^γ`. Fast strokes thin, slow
   deliberate ones heavy.
3. **Manual modulation** — scroll wheel as live pressure during a stroke. This is
   the only term carrying actual *intent* rather than inference, and it is the
   closest a mouse gets to a real pen.

`p = envelope(t) × speed_term(v) × manual(wheel)`

## Safety

**The daemon holds an exclusive grab on the user's only pointing device.** If it
dies while grabbing, the user has no cursor. Mitigations, in order of importance:

- Ungrab in a `Drop` impl; install a panic hook that ungrabs before unwinding.
- `systemd --user` unit with `Restart=on-failure`.
- Watchdog heartbeat — a supervisor ungrabs if the daemon stops responding.
- **Never grab the keyboard.** There must always be a way out.
- During development, test against a synthetic playback device rather than the
  real mouse (see below), and keep a second mouse plugged in.

## Development strategy

**Test against a virtual mouse, not your real one.** Create a uinput device that
replays recorded event streams, and grab *that*. A crash then costs nothing. This
is the real sandbox for this project — containers do not isolate input, since an
`EVIOCGRAB` from inside a container still affects the host device.

**Record real strokes early.** The hard part of this project is the *feel* of the
filters, and that is tuned by replaying identical input through candidate
algorithms and comparing — not by drawing a new line each time and guessing.

## Non-goals

- **Windows port.** The core crate and Slint GUI port for free; capture and output
  are full rewrites, and real tablet pressure needs a signed virtual HID driver.
  Meanwhile Lazy Nezumi already serves Windows well and Linux has nothing.
  Architect behind I/O traits so a port stays possible; do not build it.
- **Device configuration** (DPI stages, RGB, onboard profiles). Separate concern,
  separate project — belongs upstream in libratbag. StabMouse never speaks hidraw;
  it *listens* for resolution changes on D-Bus (from `ratbagd`, or from any tool
  calling its generic `SetDeviceResolution` method) and adjusts its multiplier so
  cursor ratio stays constant. See decision D7.
- **Macro playback.** Different risk class. If ever added, it must be a separate
  opt-in feature, not part of the always-on path.
