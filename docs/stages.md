# Filter stage specification

What `stabmouse-core` implements. Settled in Batch 2. Names are fixed by
[vocabulary.md](vocabulary.md).

## Units

**The internal unit is millimetres of physical movement, as `f64`.**

Every distance and speed parameter below is physical, not device-relative. This is
what makes a shared preset mean the same thing on a 400-DPI office mouse and a
20,000-DPI gaming sensor — and preset sharing is a first-class feature, so configs
that are subtly wrong on someone else's hardware would undermine it.

One deliberate exception: `stabilize.radius_px`. See that stage.

**Parameter keys carry explicit unit suffixes** — `radius_px`, `attack_ms`,
`v_max_mm_s`. Self-documenting, and it makes unit mistakes visible in review
rather than at runtime.

## Stage model

- Stages are **separate and stackable**. One-euro to kill sensor jitter *then*
  pulled-string for the confident arc is a real combination a dropdown could not
  express.
- **A stage may appear more than once** in a preset. Config therefore needs
  instance identity, not just stage names.
- **Order is user-controlled, with two pins**: `normalize` is always first (or
  every downstream unit is wrong) and `pressure` is always last (it needs settled
  velocity).
- Every stage has `enabled: bool`.

### Recommended default order

```
normalize → rotate → deadzone → sensitivity → smooth → stabilize → average → snap → pressure
```

Noise gating goes before smoothing; coordinate transforms go before anything that
reasons about direction.

---

## `normalize` — pinned first

| Param | Type | Notes |
|---|---|---|
| `dpi` | int | Device resolution |

Converts raw counts to millimetres. If DPI is unknown, assume 1000 and **surface
that assumption visibly in the UI** — a wrong DPI silently mis-scales everything.

## `rotate`

| Param | Type | Notes |
|---|---|---|
| `angle_deg` | f64 | Rotates the motion vector |

Included primarily as an **accessibility feature** — for users who cannot hold the
mouse square to the desk — and secondarily for artists working at an angle.

## `deadzone`

| Param | Type | Notes |
|---|---|---|
| `threshold_mm` | f64 | Motion below this is suppressed |

Gates sensor noise at high DPI. Default 0 (off).

## `sensitivity`

| Param | Type | Notes |
|---|---|---|
| `multiplier` | f64 | Free float, no clamp |
| `y_ratio` | f64 | Optional, default 1.0 |
| `max_multiplier` | f64? | Optional output clamp, off by default |
| `curve.type` | enum | `flat` · `power` · `natural` · `custom` |
| `curve.*` | — | Per-type parameters |

The curve maps **speed in mm/s → multiplier**.

**Named `sensitivity`, not `accel`, and the curve is nested inside it.** The users
least served by this project — people with tremor, and artists who have never
touched an acceleration curve — must be able to change sensitivity on day one
without feeling they are editing something they do not understand. A flat
multiplier is the common case and gets the plain name.

`custom` is a spline. `jump`/sigmoid is deferred until someone asks.

## `smooth` — one-euro

| Param | Type | Notes |
|---|---|---|
| `amount` | f64 | Macro knob driving the three below |
| `min_cutoff_hz` | f64 | Advanced |
| `beta` | f64 | Advanced |
| `d_cutoff_hz` | f64 | Advanced |

Adaptive low-pass: heavy smoothing when slow, low lag when fast.

## `stabilize` — pulled string

| Param | Type | Notes |
|---|---|---|
| `radius_px` | f64 | **Screen pixels, not mm** |
| `catch_up` | f64 | 0–1 |
| `corner_boost` | f64 | |

The cursor drags an anchor on a leash. This is what produces the characteristic
confident sweeping arc, and it is the single most important parameter for drawing
feel.

**`radius_px` is the one screen-space parameter in the project.** The user
perceives this quantity directly as "how far the cursor lags behind my hand on
screen", which is a screen distance — not a physical one. Forcing it into
millimetres would make the number meaningless to the person adjusting it.

The anchor **snaps to the cursor on stroke end**, otherwise every stroke stops
short of where the hand actually stopped.

## `average` — weighted moving average

| Param | Type | Notes |
|---|---|---|
| `window_ms` | f64 | **Milliseconds, not samples** |
| `weighting` | enum | `linear` · `exponential` · `gaussian` |

A window in samples means different things at 125 Hz and 8000 Hz. Time is the
portable unit.

## `snap`

| Param | Type | Notes |
|---|---|---|
| `constraint` | enum | `angle` · `line` |
| `divisions` | int | For `angle`; 8 = 45° |
| `tolerance_deg` | f64 | |
| `strength` | f64 | 0–1; soft snap rather than hard lock |
| `activation` | enum | `modifier` (default) · `always` |

Modifier-held by default, the way Photoshop's shift-constrain works.

`axis_lock` is absorbed here — it is `angle` with `divisions = 4`.

> **Build for extension.** Ellipse and perspective constraints are explicitly
> planned as one of the first post-v1 additions. The constraint type must be a
> pluggable interface from the start — do not hard-code an assumption that only
> `angle` and `line` exist. Don't build them out now; don't make them expensive
> to add.

## `pressure` — pinned last

| Param | Type | Notes |
|---|---|---|
| `envelope.attack_ms` | f64 | |
| `envelope.release_ms` | f64 | |
| `envelope.shape` | enum | |
| `speed.v_max_mm_s` | f64 | |
| `speed.gamma` | f64 | |
| `speed.velocity_smoothing_ms` | f64 | **Required — see below** |
| `manual.source` | binding | Scroll wheel default; any axis or button |
| `<term>.enabled` | bool | Per term |
| `<term>.weight` | f64 | Per term |
| `min_pressure` | f64 | Floor. **May be 0, but warn** |
| — | — | Output range 0–4095 on the tablet axis |

Terms combine by **multiplication**, each individually enabled and weighted:

```
p = envelope(t) × speed(v) × manual(input)
```

Only the manual term carries actual *intent* rather than inference, which is why
it exists at all — it is the closest a mouse gets to a real pen.

### Velocity needs its own smoothing, independent of position smoothing

**Per-event velocity is unusable.** At 1000 Hz each report carries deltas of 0–2
counts over roughly 1 ms of wall time with scheduling jitter on top, so an
instantaneous `distance / dt` estimate is dominated by quantisation noise. Feeding
that into the speed term produces visibly gritty pressure — measured in the probe
on 2026-07-30.

The pressure stage therefore maintains its **own** low-passed velocity estimate
over `velocity_smoothing_ms`. This is separate from the position-smoothing stages
and cannot be replaced by them: `pressure` is pinned last, so it sees post-smoothing
*position*, but it still needs a deliberately-smoothed *velocity*, and a stroke can
legitimately want sharp positional response with gentle pressure response.

Consequence for the pipeline: the pressure stage is not stateless with respect to
motion history, even though it runs last.

**`velocity_smoothing_ms = 0` stays available and is not clamped.** Unsmoothed
velocity produces a distinctive gritty texture which is not *correct* but is
aesthetically interesting — a legitimate creative choice rather than a debug
setting. A good illustration of why tunables do not get arbitrary floors.

### `min_pressure = 0` is permitted but warned

Speed-inverse pressure legitimately reaches zero on a fast flick, and **many
applications treat zero pressure as pen-up — so a single stroke breaks into two.**

The value is not clamped, because clamping tunables is against project preference
and someone will have a reason. But the UI warns when it is set to 0, explaining
the consequence.

## Always on, never exposed

- **Subpixel accumulator** — carries the fractional remainder between events.
  Omitting it silently loses slow motion. There is no correct reason to disable it,
  so it is not a stage and has no toggle.
