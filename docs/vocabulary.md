# Vocabulary

Settled names. This file is authoritative — if code, config keys, UI labels or
other docs disagree with it, they are wrong.

Decided in Batch 1. Where a name was chosen over an obvious alternative, the
reason is recorded, because these get re-litigated otherwise.

## Core entities

```
Profile "Line art"                     ← the activity container
├── Mode 1  Click  mouse  → raw        ← hotkey slots
└── Mode 2  Draw   tablet → inking
                             ▲
                             └─ Preset: the filter tuning (global, reusable)
```

| Term | Is | Notes |
|---|---|---|
| **Profile** | The activity container — "Line art", "CS2" | Every gaming mouse already uses "profile" for per-game settings; the audience has this word loaded correctly |
| **Mode** | A hotkey slot inside a profile: output type + preset reference | Positional and per-profile. Mode 2 in one profile is unrelated to Mode 2 in another |
| **Preset** | A named filter tuning — "inking", "raw" | Global and reusable. Matches universal convention: Krita brush presets, audio plugin presets. Calling this anything else fights user expectation |
| **Stage** | One filter within a preset | |
| **Group** | A user-defined set of devices sharing overrides | Never an imposed taxonomy |
| **Override** | A per-device or per-group changed leaf value | Keys as `<preset>.<stage>.<param>` |

Reads naturally: *"CS2 profile, mode 2, using the fine-accel preset."*

## Stages

| Config key | UI label | Does |
|---|---|---|
| `normalize` | Normalise | Raw counts → physical units via known DPI |
| `sensitivity` | **Sensitivity** | Flat multiplier, **with acceleration curve as nested advanced disclosure** |
| `smooth` | Smoothing | One-euro adaptive low-pass |
| `stabilize` | Stabiliser | Pulled-string / lazy rope |
| `average` | Averaging | Weighted moving average |
| `snap` | Snapping | Angle / line / shape constraints |
| `pressure` | Pressure | Synthesised pressure |
| — | *(none)* | Subpixel remainder carry: always on, never exposed. Disabling it is never correct |

### Why `sensitivity` and not `accel`

The stage that scales motion is called **Sensitivity**, and the acceleration
curve lives *inside* it behind an expandable "Acceleration curve" section.

The users least served by this project — people with tremor, and artists who have
never touched an accel curve — need to change sensitivity on day one. If that
number sits under a heading called "Acceleration" next to a curve editor they
don't understand, they will not touch it. A flat multiplier is the common case and
gets the plain name; curves are the specialist case and get progressive
disclosure.

Config key is `sensitivity` to match the label, per the tinkerer contract that
every control shows its real key.

A curve with zero slope *is* flat sensitivity, so this is one stage internally —
the split is presentation, not structure.

## Output types

| Key | UI label |
|---|---|
| `mouse` | Mouse |
| `tablet` | Tablet |

## The do-nothing preset: `raw`

Not `flat`. "Flat" names a *kind of acceleration curve*, so a preset called flat
implies we are still shaping the signal. **`raw` states that we are not touching
it**, which is exactly the promise the gaming audience wants to verify.

## Interactions

| Thing | Label | Notes |
|---|---|---|
| Mode toggle | "Switch mode" | |
| **The emergency action** | **"Panic — stop everything"** | Not "Release all" — see below |
| Pause toggle | "Enabled" | |
| Tablet without pressure | "Limited — no pressure" | |
| Draw-to-test area | "Scratch canvas" | |
| Raw-input underlay | "Ghost" | |
| Comparison slots | "A/B" | |

### Why "Panic" and not "Release"

The emergency action is read by someone whose mouse is misbehaving and who does
not know why. "Release all" invites you to work out what is being released
first — a label you must understand before daring to press it. "Panic — stop
everything" is legible in exactly the state it exists for.

The label is written for the worst moment it will be read in, not the calmest.

## Navigation

Dashboard · Devices · Profiles · Presets · Bindings · Library · Settings

## CLI

Binary `stabmouse`, with `sm` installed as an alias.

| Command | Does |
|---|---|
| `stabmouse status` | Current state |
| `stabmouse mode 2` / `mode draw` | Switch mode |
| `stabmouse profile line-art` | Switch profile |
| `stabmouse panic` | Emergency stop (alias: `release`) |
| `stabmouse pause` / `resume` | Suspend / resume filtering |
| `stabmouse bench` | Measure added latency |
| `stabmouse devices` | List devices |
| `stabmouse export` / `import` | Config in/out |
| `stabmouse watch` | Live event view |

`panic` is the primary verb so the CLI matches the UI label; `release` stays as an
alias because it is the more descriptive word for anyone reading a script.

## Project-level

| Thing | Value |
|---|---|
| Display name | StabMouse |
| Daemon | `stabmoused` (unit: `stabmouse.service`) |
| Config directory | `~/.config/stabmouse/` |
| D-Bus name | `io.github.si1verthief.StabMouse` |
| Crate prefix | `stabmouse-*` |
