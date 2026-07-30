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

**Resolved by measurement, 2026-07-30.** The round trip was estimated at
0.2–0.5ms. Measured on a closed synthetic loop (release build, 5000 samples):

| | |
|---|---|
| median | **0.013 ms** |
| p95 | 0.036 ms |
| p99 | 0.215 ms |
| p99.9 | 0.714 ms |
| max | 1.77 ms |

**The estimate was wrong by more than an order of magnitude.** Userspace capture
costs ~13µs at the median — far below the 1 ms polling interval of a 1000 Hz mouse.
There is no latency argument for going in-kernel.

**What this changes:**

- The concern that motivated keeping HID-BPF as an escape hatch is largely gone.
  Keep sensitivity filters separable anyway — it is nearly free (see D12) — but
  stop treating in-kernel as a likely destination.
- **The tail is the thing to engineer, not the median.** p99.9 of 0.7ms and
  occasional ~1.8ms outliers are scheduler jitter, not compute. The lever is
  therefore **scheduling priority, not faster code** — the daemon's hot thread
  should request elevated priority, and that is where any remaining latency work
  belongs.

**Caveats on the number:** measured on an idle system with a busy-wait pacer and a
trivial passthrough forwarder. Under gaming load with CPU contention the tail will
be worse, which is exactly why priority matters. Still needs a subjective
back-to-back comparison against yeetmouse before that module is retired — 13µs
says the architecture is sound, not that the feel is identical.

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

---

## D10 — The internal unit is millimetres

**Decision:** `stabmouse-core` works in millimetres of physical movement as `f64`.
All distance and speed parameters are physical quantities. Parameter keys carry
explicit unit suffixes (`v_max_mm_s`, `attack_ms`).

**Why:** preset sharing is a first-class feature, and physical units are what make
a shared preset mean the same thing on someone else's hardware. `v_max_mm_s = 500`
is identical on a 400-DPI office mouse and a 20,000-DPI gaming sensor.

**Rejected:** normalized counts at a reference DPI, which is what libinput does.
Simpler internally, but every shared config would be subtly wrong for anyone with
a different sensor — a failure mode that is quiet and would erode the Library
feature rather than break it visibly.

**No exceptions.** An earlier draft carved out `stabilize.radius` in screen pixels,
on the grounds that the user perceives it as on-screen lag. That was wrong on two
counts, and it is worth recording why so it is not reintroduced:

1. **Pixels are not computable here.** The mm→pixel mapping depends on compositor
   pointer gain — outside this process, user-mutable, not reliably queryable — and on
   per-monitor scale, so "30 px" is a different physical size on each screen of a
   multi-monitor setup.
2. **Millimetres are what deliver the portability requirement.** Two people on
   different mice and screens who have each set DPI correctly must adjust the same
   number and get the same hand-movement-to-screen-response relationship.

The physics cooperates: hand tremor is physical, so a physical correction is right,
and both tremor and correction pass through the same pointer gain — preserving the
ratio for high- and low-sensitivity users alike. A pixel radius removes a fixed screen
distance regardless of the hand movement it represents, under-correcting one and
over-correcting the other.

**Consequence for stage order:** position-domain filters (`smooth`, `stabilize`) run
**before** `sensitivity`, so their parameters are in true hand millimetres. After it, a
`multiplier` of 0.5 would silently double the effective radius and sharing would break.

---

## D11 — Progressive disclosure is a project-wide pattern

**Decision:** any stage with more than two parameters exposes **one macro control**
that drives the rest, with the real parameters revealed under "advanced". The
config file always contains the real values.

Established by `sensitivity` (flat multiplier plain, acceleration curve nested) and
`smooth` (one `amount` knob over `min_cutoff_hz` / `beta` / `d_cutoff_hz`). Applies
to every stage meeting the bar.

**Why:** this project serves two populations with opposite needs from the same
screen. A person with tremor, or an artist who has never touched an acceleration
curve, must be able to get a good result without confronting parameters they
cannot evaluate — and will not touch a control that looks like it belongs to
something they do not understand. A tinkerer must have every value.

Progressive disclosure serves both without building two applications. The
alternative — a "simple mode" and an "advanced mode" — splits the product and
doubles the surface.

**Constraint:** the macro must be a real function of the underlying parameters, not
a separate code path. Moving the macro moves the real values, and they stay
visible.

---

## D12 — Extension points are designed in, not retrofitted

**Decision:** where a feature is already known to be an early extension, its
interface is pluggable from the first commit — but the extensions themselves are
not built.

Current instances:

- **`snap` constraint types.** Ellipse and perspective constraints are explicitly
  planned as among the first post-v1 additions. The constraint type is a pluggable
  interface; only `angle` and `line` ship initially.
- **Platform I/O behind traits** (per D6), so a Windows port stays possible at
  ~60–70% reuse without being built.
- **`sensitivity` curve types**, so `jump`/sigmoid can be added without touching
  the stage.
- **The `scroll` stage** — drag-to-scroll and middle-click joystick autoscroll.
  Specified in stages.md, built after the core is solid. In scope because the daemon
  already grabs a mouse and synthesises a virtual device, so it is one filter stage
  rather than new infrastructure, and it fills a real gap: middle-click autoscroll is
  standard on Windows and inconsistent on Linux desktops.

**Why:** the cost of leaving a seam is near zero at design time and high once a
concrete implementation has hardened around a single case. This is not
speculative generality — it applies only where a specific extension is already
named and expected.

---

## D13 — Virtual devices are created once at startup and never torn down

**Decision:** the daemon creates **both** sinks — relative mouse and virtual
tablet — at startup, unconditionally, and keeps them alive for its entire
lifetime. Mode switching routes events between them. It does not create or destroy
them.

**Why — measured, and the mechanism is narrower than it first appeared.**
Verified 2026-07-30; see `external-docs/research/probe-results.md`.

Krita (Qt) initialises its tablet subsystem **only if a tablet is present when the
application starts**. That initialisation never retries. But once it *has*
initialised, hotplug works fine — removing the virtual tablet and recreating it,
Krita re-hooks without complaint.

So the failure is specifically: *application started with no tablet on the system
at all* → no pressure, ever, for that process lifetime.

**Blender is unaffected.** It picks up the tablet even when started before the
device exists, and handles it more smoothly than Krita throughout. This is
therefore a **toolkit-level quirk, not a platform rule** — do not assume it
generalises. GTK is untested.

**Consequences:**

- Both sinks exist from daemon start, even when the active mode uses only one.
- **The daemon should start before the user's applications.** Its systemd user unit
  wants ordering early in session startup, and it belongs in the install docs.
  This is a real requirement for Qt applications and merely tidy for others.
- **"Panic — stop everything" still must not destroy the sinks**, but for a weaker
  reason than first recorded: since already-initialised applications re-hook on
  device return, tearing sinks down is *recoverable* rather than catastrophic. It
  is still pointless — panic's job is returning the cursor, which ungrabbing
  achieves — so panic *ungrabs and goes inert*, leaving the sinks in place.
- `Enabled: off` keeps devices alive. Cheap, and avoids churn.
- **The watchdog's `abort()` is cheaper than first assumed.** It kills the sinks,
  but applications that were started with a tablet present will re-hook when the
  daemon restarts. Only apps launched during the outage lose pressure.
- A user who starts the daemon *after* opening their art application gets no
  pressure and no error. **The UI must detect this and say so explicitly** — it is
  otherwise indistinguishable from the feature being broken, and the fix ("restart
  Krita") is not guessable.

---

## D14 — Config is a directory; filenames are identities; state lives elsewhere

**Decision:** see [config-schema.md](config-schema.md) for the full schema. Three
choices with consequences beyond the file format:

1. **A directory of small files**, not one monolith — one file per preset, one per
   profile.
2. **The filename is the entity's identity.** `presets/inking.toml` *is* the preset
   `inking`. No internal name field to disagree with it.
3. **Runtime state lives outside the config directory**, in
   `~/.local/state/stabmouse/`.

**Why:**

1. Sharing a single preset is then a single file, which is what the Library feature
   wants. Load cost is negligible at this scale.
2. Renaming a file renames the thing; there is no possible disagreement between
   filename and internal name; a shared file is unambiguous about what it is. Cost:
   slugs must be filesystem-safe.
3. **People will version-control `~/.config/stabmouse/`.** If last-active-mode lived
   there, every mode switch would dirty their working tree — and mode switching is
   the headline interaction, happening dozens of times a session. Config is authored
   and shareable; state is neither.

---

## D15 — The user's config file is never rewritten unprompted

**Decision:** migration happens in memory. A file on disk is only rewritten when
the user takes an action that saves. Round-trips are byte-identical including
comments, key order and whitespace.

**Why:** the tinkerer contract's whole premise is that the config file belongs to
the user. Silently rewriting a hand-commented config on first launch after an
upgrade is precisely the betrayal that contract exists to prevent — and it is
irreversible, because their comments are already gone by the time they notice.

Read the old form, run the new one, touch nothing until asked.

**Consequence:** `stabmouse-config` needs a format-preserving editor (`toml_edit`),
not `serde` alone, and every schema version must remain loadable indefinitely
rather than only until the next migration runs.

---

## D16 — Stages declare their interactions, and the UI surfaces them

**Decision:** where two stages interact in a way that is not evident from either
one alone, the relationship is declared in the stage definition and the UI presents
it at the point of use — as a recommendation to review a named group of related
settings, not as a warning or an error.

**First instance:** the **anti-stall group** inside `pressure.speed`
(`threshold_mm`, `behaviour`, `timeout_ms`). It exists solely because pulled-string
stabilisation holds its anchor still whenever the cursor moves within the radius,
which yields a zero-velocity sample that the speed term reads as maximum pressure.
So whenever `stabilize` is in the pipeline, the UI recommends reviewing anti-stall.

**Why:** this interaction took a live probe, a wrong first fix, and a geometric
argument about radial versus tangential motion to identify. A user hitting the same
blobs would have no route to the cause — the parameter that fixes it lives in a
*different stage* from the one causing it, and nothing in either stage's own
controls hints at the connection. Discoverability here is not a nicety; without it
the feature is effectively unreachable.

**Constraints:**

- It is a **recommendation, not a warning.** The combination is not wrong, and users
  who want the hotspots must not be nagged. This follows the same reasoning as
  permitting `min_pressure = 0` and `velocity_smoothing_ms = 0`.
- Related parameters are presented as a **named group**, so the relationship is
  legible in the config file as well as the UI.
- Generalises rather than being special-cased: `normalize.dpi` being wrong
  mis-scales everything downstream, and stacking `smooth` with `stabilize` has its
  own interactions. The mechanism should carry those too.

**Where this came from:** identified from use on 2026-07-30, after the probe made
the interaction visible. Recorded because the reasoning is not recoverable from the
parameter list.
