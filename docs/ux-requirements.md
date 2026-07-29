# UX requirements and user-facing structure

What the program has to feel like, and how a user moves through it. Written
before implementation so the architecture can be checked against it.

## Audiences

| | Wants | Cares most about |
|---|---|---|
| **Artist without a tablet** | Smoothing and pressure in Krita, Blender, browsers | Feel; works in the app they already use |
| **Gamer** | Custom accel curves, or provably nothing at all | Latency, and being able to *verify* it |
| **Accessibility user** | Tremor stabilisation for general cursor use | Set-and-forget; system-wide, not per-app |
| **Tinkerer** | Every parameter, reorderable, shareable | Config file parity, no GUI-only features |
| **Packager / contributor** | Build and ship it | One-command build, stable config schema |

The accessibility audience is a first-class path, not a preset buried inside a
drawing tool. For them smoothing is not a nicety — it is cursor usability, which
raises the stakes on fail-open and emergency-disable.

## 1. Trust — non-negotiable

- **Fail-open.** Daemon not running, crashed, or never started → the mouse behaves
  completely normally. There is no state where "StabMouse is broken" means "I have
  no pointer."
- **Emergency release.** Drops every grab, tears down virtual devices, daemon goes
  inert. Reachable from **at least three** places: tray, global hotkey, and
  `stabmouse disable`. Works when the GUI is unreachable, which is possible
  because the keyboard is never grabbed.
- **Revert-on-timeout for risky changes**, as display-resolution dialogs do:
  apply, then "keep these settings? reverting in 15s." A bad curve or extreme
  smoothing can make the mouse unusable, and the user has to fix it *with that
  mouse*.
- **CLI parity.** Everything the GUI can do, the CLI can do.
- **Uninstall fully reverts** — udev rules, units, group changes, config.

## 2. Install and first run

- One package, one command. AUR first, then `.deb`/`.rpm`.
- **No manual udev or group fiddling.** The package handles it, or `stabmouse
  setup` states exactly what it will change and does it.
- **Actionable errors.** Never bare `permission denied` — name the cause, give the
  exact command, offer a Copy button.
- First run: detect devices, choose a starting card, working in under a minute.

```
        What do you want to do?

  ┌──────────┐  ┌──────────┐  ┌──────────┐
  │    ✎     │  │    ⌖     │  │    ⊙     │
  │  Draw    │  │  Gaming  │  │  Steady  │
  │          │  │   feel   │  │ my cursor│
  │ smoothing│  │ accel    │  │ tremor & │
  │ +pressure│  │ curves,  │  │ precision│
  │ for art  │  │ or raw   │  │ help     │
  └──────────┘  └──────────┘  └──────────┘

           [ Skip — I'll configure manually ]
```

## 3. Mental model and conflict detection

Users must understand what layer this sits at, and what else is touching their
input. This is where comparable tools most often fail.

- **Detect and warn about conflicts**: yeetmouse/maccel loaded, libinput
  acceleration not flat, KDE pointer settings, Steam Input active. Two stacked
  accel curves produce baffling results and the user will blame StabMouse.
- **Show the effective pipeline** permanently, not buried in a log.
- **Say what it does not do.** It does not change hardware DPI — that is the mouse.

## 4. Configuration model

Presets are **global entities**. Devices carry a thin overlay of overridden leaf
values — never a forked copy.

```
Global default          ← every device starts here
   └─ Group (optional)  ← user-defined, e.g. "CAD pucks"
        └─ Device       ← specific overrides
```

This covers both ends of the range:

- **New mouse plugged in** → matches nothing → inherits global default →
  everything stays the same. Quiet notification, never a blocking wizard.
- **Artist with five devices** → tunes the `inking` preset once and it improves
  everywhere, while the CAD puck overrides `radius = 12` and the mouse keeps 24.
  Fixing the preset fixes it for all of them.

**Groups are user-defined, not an imposed device taxonomy.** Most users never
create one.

**Identity matching** falls back: serial → VID:PID → generic default. Devices are
user-labellable and can pin to a physical port, since two identical mice are
otherwise indistinguishable.

**Absent devices persist.** A tablet that is rarely plugged in keeps its config,
shown greyed under "remembered" — not cluttering the active view, not forgotten.

```toml
[defaults]
profile = "line-art"

[[group]]
name = "CAD pucks"
devices = ["1234:5678:A1", "1234:5678:B2"]
overrides = { "inking.stabilize.radius" = 12 }

[[device]]
match = "0738:0c08"
label = "R.A.T. 8+"
overrides = { "raw.normalize.dpi" = 1600 }
```

Overrides key into `<preset>.<stage>.<param>`, so a device can retune one stage of
one preset without forking anything.

### Presets, modes, profiles

Three entities, each doing one job. Names are fixed in
[vocabulary.md](vocabulary.md).

| | What it is | How it changes |
|---|---|---|
| **Preset** | A named filter pipeline with its parameters. Global and reusable. | Edited deliberately in the preset editor |
| **Mode** | A slot holding *output type* (mouse or tablet) + *a preset reference*. Lives inside a profile. | **Instantly, by hotkey — mid-task** |
| **Profile** | A named set of mode slots for one activity. | Deliberately: menu, tray, or opt-in per-app rule |

```
Profile "Line art"                 Profile "CS2"
├── Mode 1  Click  mouse  → raw    ├── Mode 1  Aim     mouse → raw-1600
├── Mode 2  Draw   tablet → inking └── Mode 2  Sniper  mouse → fine-accel
└── default: Mode 1

Profile "Shading"                  Preset library (global, reusable)
├── Mode 1  Click  mouse  → raw      raw · inking · loose-heavy
├── Mode 2  Blend  tablet → loose    fine-accel · raw-1600 · steady
└── Mode 3  Smudge tablet → smudge
```

**Modes are slots, not device types.** Two modes in one profile may both output a
mouse with different sensitivity settings — a fine curve for precision and a flat
one for tracking — exactly as easily as one mouse and one tablet.

**Presets are referenced, not embedded.** Editing `raw` improves every mode in
every profile that uses it. This is the same reasoning as D8: fix it once, fixed
everywhere.

### The mode switch is the headline interaction

It has to survive being used dozens of times in a session, mid-task, without
thought:

> Gartic Phone. Click buttons normally. One hotkey. Draw. Same hotkey. Back to
> clicking. No dialog, no lag, no app switch, no broken cursor.

- **Primary toggle** flips between the two most recently used modes in the current
  profile — alt-tab semantics. One key covers the draw/click loop above.
- **Direct select** binds individual slots, for mice with buttons to spare.
- **Cycle next/prev** for profiles with more slots than bindings.

**No cap on slots.** The UI is *designed* around two to four and direct-select
bindings cover the first four, but nothing is clamped — see the project's standing
preference against arbitrary limits.

### Tablet mode must work in apps that know nothing about tablets

This is core to the primary use case, not a fallback. A virtual tablet still
drives the pointer through the compositor, so clicking and drawing work in any
application; only *pressure* requires `tablet_v2` support.

Combined with smoothing, that means the Gartic Phone case works fully: better
lines everywhere, pressure where the app supports it.

**Where pressure is unavailable, say so** — the tray and OSD indicate degraded
output, so nobody is left wondering why pressure "stopped working."

> **Assumption to verify:** that KWin translates virtual-tablet input to ordinary
> pointer events for clients which never bind `tablet_v2`, as it does for physical
> tablets. Everything above depends on it. Test before building on it.

## 5. Navigation

```
StabMouse
├── Dashboard           what's happening right now
├── Devices             which exist, which are managed, which are remembered
├── Profiles            list; each holds mode slots
│   └── Profile editor  per slot: output type + preset, and the default slot
├── Presets             global library of filter pipelines
│   └── Preset editor   pipeline + params + live canvas
├── Bindings            mode hotkeys, buttons, opt-in auto-switch rules
├── Library             import/export, sharing, templates
└── Settings            startup, safety, tablet mapping, advanced
```

Profiles and presets are separate top-level areas because they are edited on
different rhythms: presets get tuned in long sessions with the scratch canvas,
profiles get assembled once and then switched between for years.

Mirrored in a CLI, because the GUI must never be load-bearing:

```
stabmouse status | profile <name> | mode <n|name> | devices | bench
stabmouse export > mine.toml
stabmouse panic
```

## 6. Dashboard

```
┌─ StabMouse ─────────────────────────────── [Art ▾] ──┐
│                                                       │
│  Mad Catz R.A.T. 8+ ADV            1000 Hz · 1600 dpi │
│                                                       │
│  raw ──▶ normalize ──▶ smooth ──▶ pressure ──▶ tablet │
│           1600dpi      pulled      envelope    virtual│
│                        r=24px      ×speed      pen    │
│                                                       │
│  ┌───────────────────────────────────────────────┐   │
│  │  ╱╲    live delta scope        latency 0.3 ms │   │
│  │ ╱  ╲__╱╲___                                   │   │
│  └───────────────────────────────────────────────┘   │
│                                                       │
│  ⚠ libinput acceleration is not flat — stacking       │
│    two curves. [Set flat]  [Ignore]  [Explain]        │
│                                                       │
│                              [ Disable StabMouse ]    │
└───────────────────────────────────────────────────────┘
```

The pipeline strip is the mental model, permanently visible. Conflict warnings
are inline.

## 7. Profile editor

Assembling slots. Deliberately thin — this screen gets used once per profile, then
never again.

```
┌─ Profile: Line art ──────────────────────────────────┐
│                                                       │
│  slot  name     output          preset                │
│  ┌──────────────────────────────────────────────┐    │
│  │ 1 ◉  Click    [mouse  ▾]   [raw         ▾] ✎ │    │
│  │ 2    Draw     [tablet ▾]   [inking      ▾] ✎ │    │
│  │ + add slot                                   │    │
│  └──────────────────────────────────────────────┘    │
│    ◉ = default on profile activation                  │
│                                                       │
│  Toggle key cycles: 1 ⇄ 2          [change binding]   │
│  Auto-activate for: (off)          [add app rule]     │
└───────────────────────────────────────────────────────┘
```

The `✎` opens that preset in the preset editor. Changing a preset here changes it
everywhere it is referenced — the UI says so at the point of edit, since shared
references surprising people is the main risk of the reuse model.

## 8. Preset editor — the core screen

```
┌─ Preset: inking ──────────────────────── [A]  B  ────┐
│ PIPELINE            │ STABILISER                      │
│ ┌─────────────────┐ │                                 │
│ │≡ Normalise    ●│ │  Radius      ▂▄▆████ 24 px      │
│ │≡ Sensitivity  ○│ │  Catch-up    ▂▄▆░░░░ 0.35       │
│ │≡ Stabiliser   ●│◀│  Corner snap ▂▆░░░░░ 0.12       │
│ │≡ Pressure     ●│ │                                 │
│ │≡ Snapping     ○│ │  ┌───────────────────────────┐  │
│ │+ add stage     │ │  │ ╱‾‾╲     response curve   │  │
│ └─────────────────┘ │  │╱    ╲___                  │  │
│                     │  └───────────────────────────┘  │
│ ┌─── scratch canvas ──────────────────────────────┐   │
│ │                                                  │   │
│ │        ╭──────╮                    pressure      │   │
│ │       ╱        ╲___                ▆▆▆▆▃▃▁      │   │
│ │      ╱                                           │   │
│ │  [clear]  [ghost: raw input]      [A/B: space]  │   │
│ └──────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────┘
```

Three things carry this screen:

- **The scratch canvas is always visible**, never a separate dialog. Tuning
  without immediate feel is guessing.
- **Ghost overlay** draws raw unfiltered input underneath, so the effect of the
  filter is visible rather than inferred.
- **A/B on the spacebar.** Two live slots, instant flip, switchable mid-stroke.
  Feel is unreliable from memory.

## 9. Tray

Deliberately minimal to start.

```
┌──────────────────────────────┐
│  ● Line art — Draw           │   ← profile — active mode
├──────────────────────────────┤
│  Enabled             [ ●— ]  │
├──────────────────────────────┤
│  Mode                     ▸  │     1  Click   mouse
│                              │   ✓ 2  Draw    tablet
├──────────────────────────────┤
│  Profile                  ▸  │   ✓ Line art
│                              │     Shading
│                              │     CS2
│                              │     ── new from template ──
├──────────────────────────────┤
│  ⏻  PANIC — STOP EVERYTHING  │
├──────────────────────────────┤
│  Open StabMouse…             │
└──────────────────────────────┘
```

The mode list shows the *current profile's* slots, with their output type — mode
numbering is positional and per-profile, so slot 2 in "Line art" is unrelated to
slot 2 in "CS2". Switching profile re-populates it.

The tray is a convenience, not the fast path: the hotkey is. Nobody opens a menu
mid-stroke.

**Toggle and panic are different things:**

- **Enabled toggle** — stops filtering, passes raw through. One click, reversible,
  state remembered. Virtual devices **stay alive** so applications do not see a
  device vanish mid-session. (To be validated against real games — see Deferred.)
- **PANIC — STOP EVERYTHING** — drops every grab, tears down virtual devices, inert
  until explicitly re-enabled. Visually distinct; never one misclick from the mode
  list. Labelled for the worst moment it will be read in, not the calmest.

## 10. Modes are never silently active

- Mode and profile always visible at a glance via the tray icon.
- Every switch — manual or automatic — fires an OSD.
- **Auto-switching is opt-in**, off by default, with the reason stated: it can
  misfire, and misfiring silently during a game is precisely the trust failure
  being designed against.

## 11. Tablet mode

### Hover-relative, stroke-absolute

While the pen is **up**, motion is **relative** — navigate anywhere, across any
monitor, no mode switch. When a stroke begins, mapping **locks absolute** to the
current monitor for the duration of that stroke.

This mirrors real pen hover-versus-contact, and solves the clutch problem in the
same stroke: running out of desk between strokes is free, just lift and
reposition.

The cost is losing "top-left of mousepad = top-left of canvas" muscle memory —
but that is a tablet user's expectation, and the target users are mouse users who
never had it. **This is the default.** Fixed-region mapping remains available in
Settings for anyone who wants classic tablet behaviour, with explicit
switch → cross → switch as the fallback.

### Other tablet requirements

- Works in Krita, Blender, GIMP, Inkscape, browsers — with **graceful
  degradation**: no `tablet_v2` support in the app → fall back to smoothed
  relative mouse rather than nothing.
- **Mode toggle reachable while drawing** — a mouse button, not alt-tabbing.
- **Visible pressure meter while tuning**, otherwise inferred pressure is guesswork.

## 12. The tinkerer contract

Breaking any of these is how tools lose their power users.

- **The config file is the source of truth**, not a GUI export. Hand-editable,
  hot-reloaded, bidirectional.
- **Comments and formatting survive GUI writes.**
- **Every GUI control shows its underlying key/value**, so GUI knowledge and file
  knowledge are the same knowledge.
- **Nothing is GUI-only.**
- **Versioned schema with automatic migration**, so tuning survives refactors.
- **Import/export as pasteable text.** These communities share configs in forum
  posts and chat — that is a first-class format, not a file dialog.

## 13. Performance and observability

- **`stabmouse bench`** reports added latency, distribution, and worst case. The
  gaming audience is rightly hostile to anything sitting between them and the
  cursor; give them a measurement to post, not a claim to trust.
- Near-zero idle CPU (the GUI is a separate process and usually not running).
- **`stabmouse status`** — active device, profile, mode, loaded preset, measured rate.
- **Live event inspector** — raw deltas versus processed, side by side.
- Survives sleep/resume, unplug/replug, compositor restart, different USB port.

## 14. Multiple devices

- Per-device configuration via the overlay model above.
- **Only touch devices explicitly opted in.** Never grab the trackpad by default.

## Deferred / to be settled by testing

- **Whether "Enabled: off" should keep virtual devices alive.** Current assumption
  is yes, to avoid mid-session device disappearance. Needs testing against real
  games.
- **Game device enumeration.** Titles that enumerate input at startup may see the
  real device disappear and a virtual one appear; ordering may matter, especially
  under Proton and with Steam Input. Do the sensible thing, then test.
- **Richer tray** (quick sliders, per-device switching) once the simple one is
  proven.

## Explicitly out of scope

- **Migration from yeetmouse.** Clean break — no curve import.
- **Device configuration** (DPI stages, RGB, onboard profiles). StabMouse never
  speaks hidraw; see decision D7.
