# Interaction specification

Hotkeys, mode switching, tablet behaviour, safety flows. Settled in Batch 6.

## Hotkeys

| | |
|---|---|
| Registration | KDE global shortcuts (KGlobalAccel), **not** a keyboard grab |
| Mode toggle default | **None — first run asks the user to press one** |
| Direct mode-select defaults | None; bound on demand |
| Panic default | `Meta+Shift+Esc` — ships bound |
| Binding UI | Press-to-bind capture |
| Mouse buttons | Bindable; the grabbed device's events are already visible |

No default mode-toggle key: any global default collides with something for someone,
and this is the headline interaction used dozens of times a session. It deserves a
deliberate choice at the moment the user is thinking about it.

Panic is the deliberate exception — an unbound panic key is useless precisely when
it is needed.

### Conflict detection: warn, never block

Every binding — default or user-chosen — is checked against three classes of
conflict, and the user is told and then allowed to proceed anyway.

| Source | How | Accuracy |
|---|---|---|
| **System shortcuts** | Query KGlobalAccel over D-Bus for registered shortcuts | Exact |
| **Commonly-bound keys** | Small curated list: `Tab`, `Space`, `Esc`, function keys, `Ctrl+C/V/X/Z/S` | Heuristic |
| **Application defaults** | Small curated per-app table for the apps this project targets — Krita, Blender, GIMP, browsers | Best-effort |

Phrasing is a question, not a refusal:

> `Meta+Shift+L` is already registered as a system shortcut. Bind anyway?

> Krita uses `Shift+Backspace` by default; this may conflict. Bind anyway?

> `Tab` is commonly bound by applications. Bind anyway?

**Best-effort is the honest standard here.** There is no way to enumerate every
application's bindings, so the curated tables cover the apps this project's users
actually run and make no claim beyond that. A missed conflict must degrade to
"the user finds out" rather than to a false assurance that the key was free.

This is the D16 pattern again — surface a non-obvious interaction, recommend, do not
obstruct. Consistent with permitting `min_pressure = 0` and unclamped fields.

## Mode switching

### Tap to flip, repeat to cycle

One key must serve both quick back-and-forth *and* walking a list of three or more
modes. Resolved with a timing window rather than a second binding:

| Input | Result |
|---|---|
| Press, window expired | Switch to the **most recently used other mode** |
| Press again **within 600 ms** | Advance to the next mode in MRU order |
| Window expires | The next press flips to MRU again |

So a lone tap is a flip, and rapid taps walk the list — the same feel as alt-tab,
without needing a modifier held. Two modes behave as a plain toggle; five modes are
reachable from the same key.

`cycle-next` / `cycle-prev` and direct slot select remain separately bindable for
anyone who prefers explicit control.

### Other switching behaviour

| | |
|---|---|
| **Requested mid-stroke** | **Deferred until the stroke ends**; OSD shows it pending |
| Filter state across a switch | Per-stroke state reset (anchor, envelope, velocity). Nothing carried |
| OSD | Every switch, manual or automatic |
| OSD duration | 1.2 s |
| Debounce | None — deferral already prevents the harmful case |

Switching output type mid-stroke would leave a dangling `BTN_TOUCH` on a device that
stops receiving events, which is how a pen gets stuck down in the target
application. Deferring to stroke end costs nothing real; nobody deliberately
switches mid-line.

## Profile switching

| | |
|---|---|
| On switch | Activate that profile's default mode |
| Auto-switch trigger | Focus change, 150 ms debounce |
| **Manual override** | **Sticky** until the profile changes or auto is re-armed |
| Auto target missing | Stay put, log, surface once |

Auto-switch reclaiming control seconds after a deliberate manual choice is the
failure mode that makes people disable the feature permanently.

## Tablet mode

| | |
|---|---|
| Pen-down button | Left, configurable |
| Right button | `BTN_STYLUS` |
| Middle button | `BTN_STYLUS2` |
| Release outside the window | Treat as pen-up, end the stroke cleanly |
| Absolute-mode edges | Clamp, never wrap |

### The wheel is a poor third axis

Wheel-as-pressure is **opt-in and off by default** — see
[stages.md](stages.md#pressure--pinned-last).

Most mice have **detented** wheels: discrete clicks, not a continuous axis. Treating
one as a pressure dial gives coarse, steppy control that feels bad and cannot be
smoothed into feeling good. Free-spinning wheels exist and suit it well, which is why
the option exists at all — but it must not be the default, and nothing should be
designed on the assumption that a usable analogue input is available.

When it *is* enabled, the wheel acts as pressure **only while a stroke is active**.
Otherwise it scrolls normally. Without that gate you could not scroll or zoom a
canvas in tablet mode.

## Safety

| | |
|---|---|
| Panic | Release grabs, cease emission, **keep sinks**, notify, require explicit re-enable |
| Revert-on-timeout triggers | Any change to the *active* preset while it is active |
| Timeout | 15 s |
| **If the user cannot move the mouse to confirm** | **Revert is automatic; the dialog is keyboard-dismissible** |

Confirmation must never be required to recover. The changes worth protecting are
precisely the ones that might stop you clicking "keep".

## First run

- The chosen card creates a profile with two modes (mouse plus the card's output) and
  the presets they reference.
- **A mode-toggle hotkey is bound during first run**, since none ships by default and
  this is the moment the user is thinking about it.
