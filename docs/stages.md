# Filter stage specification

What `stabmouse-core` implements. Settled in Batch 2. Names are fixed by
[vocabulary.md](vocabulary.md).

## Units

**The internal unit is millimetres of physical movement, as `f64`.**

Every distance and speed parameter below is physical, not device-relative. This is
what makes a shared preset mean the same thing on a 400-DPI office mouse and a
20,000-DPI gaming sensor — and preset sharing is a first-class feature, so configs
that are subtly wrong on someone else's hardware would undermine it.

**Parameter keys carry explicit unit suffixes** — `radius_mm`, `attack_ms`,
`v_max_mm_s`. Self-documenting, and it makes unit mistakes visible in review
rather than at runtime.

### There are no screen-space parameters

An earlier draft specified `stabilize.radius` in *pixels*, on the grounds that the
user perceives it as on-screen lag. That was wrong, and the reason is worth keeping.

**Pixels are not computable here.** The millimetre → pixel mapping depends on the
compositor's pointer gain, which lives outside this process, is user-mutable at any
moment and is not reliably queryable; and on per-monitor scale, which means "30 px"
is a *different physical size on each screen* of a multi-monitor setup. A value that
cannot be computed cannot be promised.

**Millimetres also give the portability guarantee we actually want:** two people on
different mice and different screens who have each set their DPI correctly adjust the
same number and get the same relationship between hand movement and on-screen
response.

The physics cooperates. The artefact being corrected — hand tremor — is a physical
property of the hand, so the correction should be physical too. Tremor and correction
then pass through the *same* pointer gain: a high-sensitivity user sees magnified
tremor and receives a proportionally larger correction, a low-sensitivity user sees
less of both. The ratio is preserved for both. A pixel radius would break exactly
this, removing a fixed screen distance regardless of how much hand movement it
represents — under-correcting one user and over-correcting the other.

The UI may *display* an approximate pixel equivalent as a hint, clearly marked
approximate. It is never what gets stored.

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
normalize → rotate → deadzone → smooth → stabilize → average → sensitivity → snap → scroll → pressure
```

- Coordinate transforms and noise gating go first, before anything reasons about
  direction or magnitude.
- **Position-domain filters run before `sensitivity`, not after.** Their parameters
  are then in *true hand millimetres*, which is what makes the portability guarantee
  above hold — after `sensitivity`, a user with `multiplier = 0.5` would silently get
  double the radius in hand terms and a shared preset would not transfer. It is also
  better signal practice: applying an acceleration curve to already-smoothed motion
  beats amplifying jitter through the curve.
- The accepted cost is that visible on-screen lag becomes `radius × multiplier`, so
  changing sensitivity changes visible smoothing. That is the better side of the
  trade — sensitivity is an explicit setup choice, whereas broken sharing is a silent
  failure.
- `pressure` stays pinned last and `normalize` pinned first; everything between is
  user-reorderable, with these defaults chosen deliberately.

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
| `radius_mm` | f64 | Millimetres of hand movement |
| `catch_up` | f64 | 0–1 |
| `corner_boost` | f64 | |

The cursor drags an anchor on a leash. This is what produces the characteristic
confident sweeping arc, and it is the single most important parameter for drawing
feel.

**The radius is millimetres of hand movement**, not screen pixels. Pixels are not
computable from here and would break preset portability — see the units section above
for the full argument.

The UI may show an approximate pixel equivalent as a hint. It is never what is stored.

Measured range from real recordings (2026-07-30, tremor sample): **0.2mm removes
visible wobble with all shape detail intact, 0.5mm is smooth with shape preserved,
1.0mm begins rounding corners, 2.0mm distorts, 4.0mm destroys the drawing.** For
scale, a whole small drawing spans roughly 8mm of hand movement.

### Button state must not touch the leash

**Settled by use, 2026-07-30, after getting it wrong twice in opposite directions.**

Two earlier versions moved the anchor at a stroke boundary, on the reasoning that ink
should begin and end at the hand's *true* position:

1. **Snapping to the cursor on press.** Teleported the cursor forward by a full radius on
   every click — measured at exactly 2.02mm for a 2mm radius, the largest single-sample
   delta anywhere in a recording.
2. **Converging to the cursor on release.** Pushed the stroke past its intended end, *and*
   emptied the leash so the next movement had a dead zone one radius wide before the cursor
   moved at all. Reported from use as having to "feed a bunch of inputs to be allowed to
   continue moving".

Both were backwards for the same reason: **the anchor is what the user sees.** They aim
with it, press when it is on target, and release when it is where the stroke should end.
Moving it to catch up with the hand moves it away from what they were aiming at.

So the rule is simply: **a stabiliser is a motion filter, and clicking is orthogonal to
motion.** Button state changes nothing.

Consequences, all of them wanted:

- The stroke ends a leash-length short of the hand — which is exactly where the user was
  looking when they released.
- Output totals trail input by up to the radius, permanently. That is the filter working,
  not motion lost; the replay bench checks the residual against the radius as a budget
  rather than expecting zero.
- `settled()` is always true for this stage. Nothing is ever held back for a later flush,
  so no tick loop can hang waiting on it.
- `snap_on_stroke_end` survives as an opt-in for a consumer drawing with a very large
  radius, defaulting off.

`pressure` still needs ticks for its release envelope, so the daemon's tick loop remains.

## `average` — weighted moving average

| Param | Type | Notes |
|---|---|---|
| `window_ms` | f64 | **Milliseconds, not samples.** 0 is the identity |
| `weighting` | enum | `exponential` (default) · `linear` · `gaussian` |

A window in samples means different things at 125 Hz and 8000 Hz. Time is the
portable unit.

Averages *position* and emits the change in that average, so motion is conserved:
the path lags but always arrives. It reports itself unsettled while it still owes
lag, which is what keeps the daemon's settle phase feeding it zero-motion ticks
until the stroke ends where the hand did.

**Measured on `supershaky.tsv`, 2026-08-01**, replaying one recording through every
candidate. Wobble is the reduction in total path length for the same net
displacement; lag is mean distance behind the unfiltered path.

| window | `exponential` | `linear` | `gaussian` |
|---|---|---|---|
| 20ms | 10.7% / 0.026mm | 11.1% / 0.032mm | 10.6% / 0.026mm |
| 50ms | **20.9% / 0.057mm** | 21.5% / 0.069mm | 17.9% / 0.058mm |
| 80ms | 28.7% / 0.081mm | 31.0% / 0.095mm | 26.6% / 0.081mm |
| 120ms | 34.0% / 0.109mm | 36.2% / 0.128mm | 33.2% / 0.108mm |

**`exponential` is the default because it is the most efficient at every window** —
it removes the most wobble per millimetre of lag. `linear` removes more wobble at a
given window but pays proportionally more lag for it, and it is the most predictable
to reason about: halving the window halves the lag. `gaussian` was consistently the
weakest here and is kept for the case it is meant for, a single jittery sample rather
than continuous tremor.

**50ms is the useful starting point** — a fifth of the wobble gone for lag that is
under a tenth of a millimetre, well below the 0.4mm a `stabilize` radius costs by
design. Note the shape of the curve: wobble reduction is sublinear in window size
while lag is very nearly linear, so past ~100ms each extra millisecond buys less and
costs the same.

Not a replacement for `stabilize`, which removed 95% of the same wobble at 2.7mm of
lag. These are different tools: `average` takes the tremor off an otherwise fine
line, `stabilize` reshapes the line entirely.

## `snap`

| Param | Type | Notes |
|---|---|---|
| `constraint` | enum | `angle` (default) · `line` |
| `divisions` | int | For `angle`; 4 = axis lock, 8 = 45° |
| `tolerance_deg` | f64 | Defaults to half a division, so a lock locks |
| `strength` | f64 | 0–1; soft snap rather than hard lock |
| `activation` | enum | `modifier` (default) · `always` |
| `modifier` | binding | Key or button that engages it, or a list of them — `BTN_SIDE`, `["BTN_SIDE", "KEY_LEFTSHIFT"]` |

Modifier-held by default, the way Photoshop's shift-constrain works. **The binding may be
a mouse button or a keyboard key**, chosen per mode — see D24 for what each costs.

`axis_lock` is absorbed here — it is `angle` with `divisions = 4`.

**Projection, not distance.** The constrained position is the perpendicular projection of
the hand's travel onto the allowed direction, which is what every drawing application
means by constrain. Preserving distance along the direction instead would make a wobbling
hand *overshoot*, converting noise across the line into length along it.

**`tolerance_deg` is what makes a snap soft.** Beyond it the constraint declines to act, so
a narrow tolerance is a magnet that bites near an axis and a wide one is a cage. The
default is half a division — every direction then belongs to some allowed one, and axis
lock behaves as a lock rather than as an occasional nudge.

**Releasing never moves the cursor.** Motion discarded perpendicular to the constraint stays
discarded: on release the stage re-anchors to where the output actually is, rather than
owing back the difference and springing to where an unconstrained hand would have been.

**Motion is deliberately not conserved here**, unlike every other position stage. Discarding
the perpendicular component is the entire feature — `deadzone` has the same character. The
core's conservation invariant is about the subpixel carry never silently losing slow motion,
which is a different claim.

`line` takes its direction from the first 1.5mm of the constrained segment rather than from
the first sample, which is mostly sensor noise.

> **Build for extension.** Ellipse and perspective constraints are explicitly
> planned as one of the first post-v1 additions. The constraint type must be a
> pluggable interface from the start — do not hard-code an assumption that only
> `angle` and `line` exist. Don't build them out now; don't make them expensive
> to add.

## `scroll`

Diverts pointer motion into scroll events while a bound button is active. Opt-in,
`off` by default.

| Param | Type | Notes |
|---|---|---|
| `mode` | enum | `drag` (default) · `grab` · `joystick` |
| `button` | binding | What activates it |
| `latch` | bool | `joystick`: click-to-latch versus hold |
| `drag.mm_per_unit` | f64 | Hand travel per scroll unit |
| `drag.invert` | bool | Natural versus traditional |
| `joystick.deadzone_mm` | f64 | Displacement before scrolling starts |
| `joystick.gain` | f64 | Scroll rate per mm of displacement |
| `hi_res` | bool | Emit `REL_WHEEL_HI_RES` — always on, see below |

**There is no `off` mode.** Every stage already has `enabled`, which turns it off without
losing its tuning; a second way to say the same thing reads as a setting that does
something the user has not been told about. Removed after exactly that confusion.

**A button bound to a gesture is consumed by it** and does not also do its ordinary job —
otherwise binding the middle button to autoscroll also pastes, which reads as the gesture
firing when it should not. The pen button is never consumed, since a preset that bound it
would silently lose the ability to draw.

- **`drag`** — touchscreen-style swipe. While held, hand motion scrolls directly and
  the cursor is frozen, as a finger on glass has no cursor to move. **This is the only
  mode that freezes the cursor**, and it is the whole difference between it and `grab`.
- **`grab`** — a hand tool. The cursor keeps moving and the page moves with it, so the
  point under the cursor stays under it. `drag_mm_per_unit` is what decides whether the
  page keeps pace, and it is a tuned figure rather than an exact one: the millimetre-to-
  pixel mapping lives in the compositor and is not queryable from here (see the units
  section).
- **`joystick`** — middle-click autoscroll. Displacement from the press origin sets a
  continuous scroll *velocity*.

**Correction: `joystick` does not freeze the cursor**, though this document said it did.
Freezing it made the gesture unusable in practice — displacement is the control, so with
no cursor to see there is no way to judge the speed or to find the way back to a stop, and
it reads as the scroll having locked up. Every autoscroll worth copying leaves the cursor
free for exactly this reason. Reported from use, 2026-08-01.

**Momentum** (`momentum`, `momentum_decay_ms`) carries a released flick onward, decaying
exponentially, so a long page feels like a surface with weight rather than a crank. Off by
default. Any new press cancels a glide, because a hand back on the mouse wants control
rather than to fight the page's leftover speed. Joystick has no release to carry from and
ignores it.

**Why this is in scope.** Middle-click autoscroll is standard on Windows and in
browsers, and inconsistent or missing across Linux desktops — a common complaint from
people switching over. Drag-to-scroll is familiar from touch devices. Both are
genuinely cheap here: the daemon already grabs a mouse and synthesises a virtual
device, so this is one filter stage rather than new infrastructure. It also applies
equally in tablet mode, where scrolling otherwise competes with drawing.

**Must emit hi-res wheel.** Whole-notch scrolling feels broken for gestures that are
continuous by nature.

While active it **consumes motion** — downstream position stages see nothing and
drawing is suspended for the duration.

**Built.** Parameter names are flattened to match the rest of the schema —
`drag_mm_per_unit`, `drag_invert`, `joystick_deadzone_mm`, `joystick_gain` — and `button`
takes a key or button name exactly as `snap.modifier` does (D24), so a spare side button
or the middle button both work.

**Both resolutions are always emitted**, so `hi_res` needs no switch: an application that
understands `REL_WHEEL_HI_RES` gets the smooth version and one that does not still
scrolls. Emitting only the fine axis loses scrolling entirely in anything not updated for
it, which is not a trade worth offering.

**The fractional remainder is carried per axis and per resolution.** A notch is 120 hi-res
units, and at 1000Hz a single sample of a slow drag is a tiny fraction of either — so
truncating each sample independently would scroll nothing at all. This is the accumulation
hazard from modules.md, and the two resolutions need separate accumulators because a
notch's worth of hi-res motion is reported long before a whole notch is owed.

**`joystick` reports itself unsettled while coasting**, which is what keeps the daemon's
zero-motion ticks coming — a velocity that persists while the hand is still has no input
of its own to be driven by. `drag` is settled between samples, since it produces nothing
without movement, and claiming otherwise would hold the tick loop open for a gesture with
nothing to add.

## `pressure` — pinned last

| Param | Type | Notes |
|---|---|---|
| `envelope.attack_ms` | f64 | |
| `envelope.release_ms` | f64 | |
| `envelope.shape` | enum | |
| `speed.v_max_mm_s` | f64 | |
| `speed.gamma` | f64 | |
| `speed.velocity_smoothing_ms` | f64 | **Required — see below** |
| `speed.source` | enum | `output` · `cursor` — **unresolved, see below** |

**Anti-stall group** — presented as a named, collapsible group in the UI, not as
loose parameters:

| Param | Type | Notes |
|---|---|---|
| `speed.stall.threshold_mm` | f64 | Movement below this counts as stalled |
| `speed.stall.behaviour` | enum | `hold` (default) · `decay` |
| `speed.stall.timeout_ms` | f64 | Stall longer than this discards banked time |

All three are user-exposed. They exist because of a specific interaction with
`stabilize`, so the UI **recommends reviewing this group whenever `stabilize` is in
the pipeline** — see D16.
| `manual.source` | enum | **`none` by default** · `wheel` · `button_ramp` |
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

### The manual term is opt-in, because mice have no good analogue axis

**`manual.source` defaults to `none`.**

Most mice have **detented** wheels — discrete clicks, not a continuous axis. Used as
a pressure dial, a detented wheel gives coarse steppy control that feels bad and
cannot be smoothed into feeling good. Free-spinning wheels suit it well, which is why
the option exists, but it must not be assumed.

Nothing in the design may depend on a usable analogue input being available.

Two sources:

- **`wheel`** — best on a free-spinning wheel. Active **only while a stroke is in
  progress**, so scrolling and zooming still work between strokes. The between-strokes
  half is built: a hovering pen's wheel is emitted through the absolute pointer at the
  pen's position, so it scrolls whatever the pen is over (D22). The wheel is therefore
  already reserved during strokes, and this term can be added without contending for it.
- **`button_ramp`** — hold a button and pressure ramps toward a target; release and it
  ramps back. Coarser than a real analogue axis but *continuous*, and it works on any
  mouse rather than only the ones with the right wheel.

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

### The velocity must be that of the output point, not the input

**Measure velocity on the post-filter position — the point actually being drawn —
never on the raw cursor.**

Taking it from the raw cursor produces heavy blobs on every direction change: the
cursor decelerates as it reverses, so the speed term spikes pressure to maximum,
while the stabiliser's anchor is still travelling at speed. The result is a wide
line laid down along a fast-moving path, exactly where a stroke should be at its
thinnest.

Observed in the canvas probe on 2026-07-30. This is the concrete reason `pressure`
is pinned last, and the pin is load-bearing rather than tidiness.

### A stalled output point must not produce a zero-velocity sample

Measuring velocity at the output is necessary but **not sufficient**. The stabiliser
legitimately holds its anchor still whenever the cursor is moving *within* the
radius — reversing along a zig-zag, for instance. The output point genuinely is not
moving. That is correct filter behaviour, but it yields a velocity of zero, which
the speed term reads as "maximum pressure", producing a hotspot exactly where the
hand was moving fastest.

So the stall must be excluded rather than smoothed:

- **`hold` (default)** — while output movement stays under `stall_threshold_mm`, the
  velocity estimate is *frozen* rather than pulled toward zero. A
  fast → stalled → fast sequence then collapses into a single slightly-slower sample
  that ordinary pressure smoothing absorbs, instead of a deep excursion.
- **`decay`** — the naive behaviour: let velocity fall toward zero during the stall.
  Produces the hotspots, which someone may want.

Implementation note: the estimator should accumulate distance and time and update
velocity only once the accumulated distance crosses the threshold, rather than
gating per event. That handles slow-but-continuous motion correctly — it reports a
genuinely low speed — while still holding through a true stall. A long stall must
discard its accumulated time rather than eventually dividing by it, or the hotspot
reappears on resumption.

Identified from use on 2026-07-30: the filter was right, the derived signal was not.

### Open: which velocity source, and how much smoothing

Stall handling is necessary but still not sufficient, for a geometric reason.

The stabiliser anchor only advances along the **radial** direction — the component
of cursor motion pointing away from the anchor. Move the cursor tangentially, in an
arc at roughly the radius distance, and the anchor barely moves while the hand is
travelling at full speed. Reversals are exactly this case. So output speed
genuinely dips at every direction change, and not to zero, so the stall gate does
not catch it.

Two candidate answers, both exposed as `speed.source`:

- **`output`** — velocity of the drawn point. Physically consistent with where ink
  lands, but carries the geometric artefact above.
- **`cursor`** — velocity of the raw input. Represents hand *intent*, which is
  arguably what pressure should track, and has no radial artefact. Its objection
  was the original blob, but that was caused by *unsmoothed* cursor velocity; the
  smoothing and stall fixes address it independently. The residual concern is that
  the output point lags the cursor, so pressure would reflect hand speed slightly
  ahead of where the ink is.

`velocity_smoothing_ms` interacts strongly with this. A window long enough to span
a reversal averages the dip away, which is why the useful range is much wider than
first assumed — likely well past 100 ms, where the initial guess was 40 ms.

**Not resolved by reasoning.** Both are implemented in the canvas probe on live
controls; the default follows from feel testing, and both stay available regardless.

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

## Defaults

Batch 8. Every value is tagged with what actually backs it, because "measured" and
"guessed" should not look alike in a spec.

- **measured** — derived from real recordings, numbers below
- **proposed** — reasoned, awaiting a feel check
- **unvalidated** — works, never tuned

### Measured from recordings (2026-07-30)

Corpus: 11 recordings, ~60k samples, one R.A.T. 8+ ADV at 1600 dpi.

| Quantity | Value |
|---|---|
| Sample interval | p50 1ms · p90 3ms · p99 22ms · p99.9 130ms |
| Drawing speed, all strokes | p10 9 · p50 18 · p90 65 · p99 148 mm/s |
| Peak speed *within* a careful stroke | median 20 · p90 30 mm/s |
| Peak speed *within* a fast gesture | median 73 · p90 124 mm/s |
| Stroke duration | shortest ~93ms · taps p50 112ms · lines p50 153ms |
| Sensor direction-reversal rate | 0.2–0.4% while drawing |
| Whole small drawing | ~8mm of hand movement |

**There are two speed regimes, not one.** Careful drawing and fast gesturing differ by
about 4×, which is why `v_max_mm_s` cannot have a single global value — it belongs to
the preset, not the program.

| Param | Default | Evidence |
|---|---|---|
| `deadzone.threshold_mm` | `0` | **measured** — the sensor essentially never reverses spuriously, and a still mouse emits no reports at all. Keep the stage: it will matter at 20,000 dpi |
| `pressure.speed.v_max_mm_s` | `50` (careful) / `150` (fast) | **measured** — 50 gives a 0.45 pressure arch with *zero* clipping on careful strokes. The earlier 400 only swung pressure 0.98→0.63, which is why every trace looked flat |
| `pressure.speed.gamma` | `2.0` | **measured** — with `v_max = 50` gives ends 0.84, mid-stroke 0.37 |
| Daemon tick interval | `2ms` | **measured** — 0.033 envelope steps against a 60ms attack; 4ms gave the visible 0.067 steps. Most gaps are already under 2ms so it rarely fires |
| `smooth.min_cutoff_hz` | `5.0` general · `2.0` tremor | **measured** — see sweep below |
| `smooth.beta` | `0.05` general · `0.2` tremor | **measured** — see sweep below |
| `average.window_ms` | `0` (off) · `50` when used | **measured** (2026-08-01) — 21% of path wobble for 0.057mm of lag; the curve flattens past ~100ms while lag keeps growing. Off by default because `smooth` already occupies this role in the shipped presets |
| `average.weighting` | `exponential` | **measured** (2026-08-01) — most wobble removed per millimetre of lag at every window tested |

#### `smooth` sweep

Stabiliser disabled so one-euro is isolated. *wobble* = output path length / input path
length, so lower means more tremor removed. *lag* = mean distance between the true hand
position and the output.

| | wobble (tremor) | lag (tremor) | wobble (drawing) | lag (drawing) |
|---|---|---|---|---|
| `mc2, b0.2` | **0.56** | 0.18mm | 0.86 | 0.44mm |
| `mc3, b0.05` | 0.59 | 0.15mm | 0.85 | 0.45mm |
| **`mc5, b0.05`** | 0.65 | 0.11mm | 0.88 | 0.30mm |
| `mc10, b0.05` | 0.75 | 0.07mm | 0.91 | 0.16mm |

`mc5/b0.05` is the general default: gentle, 0.30mm lag on deliberate strokes. `mc2/b0.2`
for tremor, where removing 44% of path wobble is worth 0.44mm of lag.

**`beta` must be 0.05–0.2 in these units, not the ~0.007 that appears in one-euro
literature.** Beta multiplies velocity, and velocity here is mm/s — so a published beta
is meaningless without knowing the units of the signal it was tuned against. At 0.01 it
did nothing measurable across the whole corpus.

The same caution applies to borrowing from Lazy Nezumi or Krita: their stabiliser values
are in **pixels**, which is not this project's unit and cannot be converted without
knowing their assumed DPI and pointer gain.

An earlier attempt to derive a cutoff from the tremor *spectrum* was inconclusive —
power came out flat across 1–25Hz and the apparent 12Hz peak sat inside the noise. The
sweep above measures the thing we care about directly instead of via a proxy.

### Proposed, pending a feel check

| Param | Default | Note |
|---|---|---|
| `stabilize.radius_mm` | `0.4` inking · `0.8` sketch · `1.5` steady | Bracketed by measurement: 0.2 keeps all detail, 0.5 is smooth with shape intact, 1.0 rounds corners |
| `pressure.envelope.attack_ms` | `60` | **Currently invisible** — every real stroke is ≥93ms, so all of them reach full pressure. Whether a 112ms tap should be a solid dot or a light one is style, not correctness |
| `pressure.min_pressure` | `0.05` | Higher never gets truly thin; lower risks apps reading pen-up |
| `pressure.speed.source` | `cursor` | Both implemented; unresolved by reasoning |

### Unvalidated

| Param | Default | Note |
|---|---|---|
| `smooth.d_cutoff_hz` | `1.0` | Left at the conventional value; not swept |
| `stabilize.catch_up` | `0.35` | Never tuned independently of radius |
| `pressure.speed.velocity_smoothing_ms` | `40` | Removed the visible grit in the probe; not swept |
| `pressure.speed.stall.*` | 0.04mm / 120ms / `hold` | Chosen to make the mechanism work, not optimised |

## Always on, never exposed

- **Subpixel accumulator** — carries the fractional remainder between events.
  Omitting it silently loses slow motion. There is no correct reason to disable it,
  so it is not a stage and has no toggle.
