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

### Related: leaving tablet mode appears to freeze the cursor — in Krita only

Observed 2026-07-31. Switching from a tablet mode to a mouse mode, the cursor
appears frozen indefinitely. It is **not** frozen. The pointer moves normally the
whole time; Krita is drawing a stale canvas cursor at the last tablet position and
suppressing the real one, because it still believes the tool is in proximity.

The diagnosis rests on which boundary clears it: **the edge of Krita's canvas, not
the edge of the screen.** For the pointer to reach that boundary it must have been
moving all along — and once it leaves the canvas widget, Krita stops painting its
own cursor and the real one reappears where it actually was. Clicking does not
clear it, because the click lands under the invisible real pointer and changes
nothing visible.

**Every other application switches practically instantly, Blender included.**

**Tested 2026-07-31 and this is worth knowing before attempting any fix: killing the
daemon outright — which destroys the virtual tablet completely — does not clear it
either.** Krita stays stuck on a device that no longer exists. The state is entirely
internal to Krita, so *no* device-side action can reach it: not a cleaner
proximity-out, not teardown, not recreation. Anything that works will have to either
move the pointer off the canvas or change Krita.

This is Krita's tablet state machine, not ours, and no daemon-side change is
warranted:

- A synthetic pointer nudge was written and then removed. The user is already
  generating pointer motion by moving the mouse; if that does not clear it, one
  more event will not either.
- Destroying the tablet device on leaving tablet mode was expected to force Krita to
  reset. **It does not** — see the test above. The `destroy_tablet_on_leave` option
  built for it is retained, off by default and honestly documented, but it does not
  solve this and should not be offered as though it does.

Two genuine defects in `leave_proximity` were found while chasing this and are
worth keeping on their own merits: pressure was left non-zero at proximity-out (an
inconsistent tool state), and the change-detection sentinels were not cleared, so
re-entering proximity at the same coordinates suppressed X and Y and the tool
entered with no position.

**This does not constrain the multi-monitor design.** Crossing between per-screen
tablets is a tablet→tablet handover that never leaves tablet input, so Krita's
proximity state stays coherent throughout.

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

---

## D17 — One virtual tablet per screen, placed automatically

**Decision:** in a multi-monitor session, create **one virtual tablet per output**
rather than one tablet spanning the desktop, and confine each to its screen by
asking the compositor directly. Crossing a screen edge is a handover from one
tablet to the next.

**Why not the alternatives.** A single tablet stretched across the whole desktop
divides a fixed surface among every monitor, so precision falls with each screen
added, and it cannot be isotropic on more than one aspect ratio at a time — on
this host, `1920x1080` beside `1280x1024`, either one screen is letterboxed or a
hand-drawn circle comes out elliptical on the other. An earlier idea of toggling
tablet mode on and off at edges was rejected by the user and is worse still: it
churns proximity state, which is the one thing applications handle badly.

**Both prerequisites were measured before committing, not assumed.**

*Screens can be enumerated portably.* `wl_output` version 4 reports the connector
name alongside logical position and size on any Wayland compositor. Verified
against this session:

```text
HDMI-A-1  1920x1080 at    0,0   aspect 1.78
DP-2      1280x1024 at 1920,0   aspect 1.25
```

*Placement can be automatic.* KWin exposes every input device at
`/org/kde/KWin/InputDevice/<sysname>`, and `outputName` there is **writable**.
Verified 2026-07-31 with `stabmouse-probe mapping` on a throwaway device: the
mapping takes effect immediately, with no restart and no visit to System Settings.
This was the condition the user attached to the design — *"fine as long as we can
handle everything automatically"* — so it was proved before the feature was built.

**The seam, and what is deliberately not built.** These two capabilities are split
because only one of them is compositor-specific:

| | Mechanism | Portability |
|---|---|---|
| Enumerating screens | `wl_output` | Any Wayland compositor |
| Confining a tablet to one | KWin D-Bus | KDE Plasma only |

There is no protocol for "this tablet covers that monitor" — it is a compositor
setting, and every desktop exposes it differently or not at all.

So `stabmouse-desktop` has **one backend and no abstraction over it**: no trait, no
registry, no plugin loading. What is preserved is the shape — `map_tablet` takes a
device name and an output name, and returns `Unsupported` when it cannot act.
Adding another desktop means adding a module and a branch, not restructuring.

**Failure is reported, never swallowed.** A tablet silently landing on the wrong
monitor is far harder to diagnose than one that says it could not be placed;
`stabmoused screens` prints what the compositor actually believes, which is the
thing that decides behaviour, rather than what we asked for.

**Placement waits for the compositor to adopt the device.** Found on the first live
run, not by reasoning: mapping a tablet immediately after creating it fails with
"no such device". The ~50ms adoption latency measured for D13's teardown option is
the *same* latency, applying here — creating a `uinput` device does not make it
addressable. Anything acting on a device it just created has to wait rather than
assume. The same run exposed a second defect: iterating with `all()` short-circuits,
so one racing device left every later screen unmapped for an unrelated reason.

**Consequences:**

- Device names are load-bearing. A compositor keys per-device settings off the
  name, so it must be stable across teardown and recreation and must never be
  derived from anything volatile such as the event node number. Confirmed
  accidentally: KWin restored mappings from an earlier run purely by name.
- Construction creates devices; **placement is a separate explicit call**. Putting
  it in the constructor meant `cargo test` reached out and mutated the running
  desktop's settings.
- Each surface is sized to its own output's aspect ratio, so millimetres stay
  isotropic per screen.
- **Edges are not crossed mid-stroke.** With the pen down, motion clamps at the
  boundary instead. A stroke cannot be lost to a twitch, and it matches how a
  physical tablet behaves.

---

## D18 — A pen tip is not a click, so tablet mode mirrors buttons onto the pointer

**The constraint, from KWin's source rather than inference.** KWin converts a
tablet tip into a pointer button **only** behind an environment variable, and that
path is marked for removal:

```cpp
// Tablet input emulation is deprecated. It will be removed in the near future.
static bool emulateInput = qEnvironmentVariableIntValue("KWIN_WAYLAND_EMULATE_TABLET") == 1;
if (!emulateInput) { return false; }
```

So a client that does not speak `tablet_v2` receives **no button at all** from the
pen. Cursor motion still works, because the compositor moves the global cursor for
tablet input regardless — which is why this presents as "the pointer is in the right
place and nothing happens" rather than as an obviously dead device.

**Measured, 2026-07-31, with `stabmouse-probe tap`:**

| Test | Result |
|---|---|
| Tablet tip over a Slint window | no click delivered |
| Tablet hover, then a *mouse* button press | click delivered, at the hovered point |

The second line is both the control and the fix. It proves the aim was right — so
the tip specifically is what fails — and it proves a relative button press lands
exactly where the pen is, because the pen has already moved the global cursor.

**Decision:** mirroring was implemented, tested in use, and **turned off by
default because it does not work.**

The control experiment was weaker than it was read as. Hovering with a tablet and
then pressing a mouse button did land on the hovered window — but that only showed
the pointer *happened* to be over the same large window, not that the two positions
agree. In use the mirrored click lands somewhere else entirely.

The reason: **the compositor tracks the tablet's position and the relative pointer's
position separately.** The visible cursor follows the tablet, so a mirrored press
goes wherever the pointer was last left, which is invisible and arbitrary. A click at
an unpredictable location is worse than no click.

`tablet_emits_mouse_clicks` remains, defaulting off. The mechanism is right and only
the position is wrong.

**What would actually fix it: hover on the pointer, draw on the tablet.** If the
relative mouse carries hover motion and the tablet only engages while the pen is
down — entering proximity at the position already being tracked — then the pointer
position is always the true one, clicks land correctly, and pressure still works for
strokes. The cost is a proximity transition per stroke, which is the operation Krita
handles worst (see D13), so it needs measuring before it is committed to.

**Reported behaviour is not uniform, and the default is a judgement call.**

| | |
|---|---|
| Browsers | **Work already** — Gartic Phone confirmed fine |
| Most other applications | Press appears to register visually but does nothing |
| Slint (this project's own GUI) | Nothing at all |

Default on because "cannot press anything" is a worse failure than a possible
double. But it is a real trade: an application that handles **both** tablet and
mouse input will see one press twice, and browsers — which already work — are the
most likely place for that. **If a browser starts double-acting, this is why**, and
one line per profile turns it off.

**Consequences beyond the option:**

- **Tablet mode is not a general-purpose pointer mode**, and should not be
  described as one. Mode switching is instant and is the intended way to go and
  interact with something else.
- **The preset editor's scratch canvas cannot be fed by toolkit pointer events.**
  Slint sits on winit, which does not implement `tablet_v2`, so the canvas would
  receive neither pressure nor clicks. It must be fed from the daemon instead —
  which is better anyway, since it then shows exactly what the pipeline computed
  rather than what the toolkit reconstructed.

---

## D19 — Focus tracking is a KWin script, because no protocol offers it

**Context.** D18 leaves tablet mode unable to click in applications that lack
`tablet_v2`. Choosing the output per application fixes that without the risky
hover/stroke split — but only if a focus change can be learned quickly and
reliably.

**There is no portable route.** Checked against this session: of **67** advertised
Wayland globals, none of these is offered to an ordinary client.

| Protocol | |
|---|---|
| `org_kde_plasma_window_management` | absent |
| `zwlr_foreign_toplevel_manager_v1` | absent |
| `ext_foreign_toplevel_list_v1` | absent |
| `zcosmic_toplevel_info_v1` | absent |

KWin reserves window management for privileged clients. So this is the first
genuinely KDE-shaped dependency in the project — the tablet mapping in D17 at
least had a portable half in `wl_output`.

**Measured, 2026-07-31, with `stabmouse-probe focus` on KWin 6.7.3.** A script
connecting `workspace.windowActivated` and reporting over `callDBus`:

```text
    5.169s  stabmouse-gui    StabMouse
    9.006s  codium           Mouse input interception… - VSCodium
   10.166s  stabmouse-gui    StabMouse
   14.007s  codium           Mouse input interception… - VSCodium
```

Four activations, four reports, no misses. Each window closing at a known instant
was reported within single-digit milliseconds of it — far inside anything a hand
would notice. `resourceClass` arrives directly, which is exactly the key an
application table needs.

**Decision:** use it, and keep it removable.

- `windowActivated`, not `clientActivated` — renamed in KWin 6.
- **The script is unloaded on start as well as on exit.** A daemon that dies leaves
  its script attached to the compositor, and the next run would then double-report.
  Unloading first makes a crash self-healing rather than something the user has to
  know about.
- Verified `isScriptLoaded` returns false afterwards, so a diagnostic leaves nothing
  behind in the user's session.

**On breadth.** The seam is the same shape as D17: one backend, no abstraction over
it, and the failure reported rather than swallowed. A compositor with no focus source
should lose *per-application output selection* and nothing else — tablet and mouse
modes must keep working, switched by hand.

---

## D20 — A mode is intent; tablet-versus-mouse is only a transport

**Decision:** when the focused application cannot receive tablet input, the daemon
**keeps the current mode and silently delivers through the mouse instead.** The mode
does not change, the preset does not change, the feel does not change. Only the
transport does.

**Why this and not automatic mode switching.** The obvious reading of D18's problem
was "switch to a mouse mode for applications that cannot do tablet". That is worse,
and the difference matters:

- A mode is what the *user* chose. Changing it on their behalf means the interface
  now disagrees with their intent, and the mode they return to is not the one they
  left.
- The filters are the point. Someone in an inking mode wants that smoothing whether
  the pixels arrive as pen pressure or as pointer motion; dropping to a "mouse mode"
  would drop the tuning with it.
- It reads as the program acting on its own. This project has been consistent that
  a surprise is worse than an inconvenience.

Keeping the mode and swapping only the transport means **one profile with a drawing
mode and a general mouse mode draws correctly in Krita and in Gartic Phone**,
without the user arranging anything.

**What is lost in the fallback:** pressure, and only pressure — because the
application could not have received it anyway. Everything upstream of the sink is
identical.

**Sensitivity had to be made identical too; it was not, and that broke the promise.**
Reported in use as roughly double when the fallback engaged. The two paths had no
relationship: tablet output maps millimetres across a screen, while pointer output
went through the quantizer at the source device's DPI and then through the
compositor's own acceleration curve. Nothing made those agree.

Two changes make them agree:

- The fallback quantizes at the **tablet's** pixels-per-millimetre, not the source
  device's counts-per-millimetre, sharing the same subpixel carry so no motion is
  dropped when the transport changes.
- StabMouse's **own** pointer is set to one pixel per count over KWin's D-Bus —
  flat profile, zero acceleration. The user's real mouse is left exactly as they
  configured it. Verified on this host: our device reads `accel=0 flat=true` while
  the real mouse stays at `accel=1`.

This also removes the acceleration prerequisite noted below: the daemon no longer
*requires* the user to flatten anything, because it flattens the one device whose
behaviour it needs to predict.

**Why only the GUI looked broken.** It was the only application tested that actually
falls back — everything else in the test set (Krita, Blender, Chromium) supports
tablet input, so the fallback path was never exercised there. A bug that appears in
exactly one application is not necessarily about that application.

**Consequences:**

- `Mode.output` becomes a *preferred* transport rather than an absolute one.
- The state has to stay **legible**: status and the GUI say "Draw — mouse fallback"
  rather than hiding it. Silent is not the same as invisible, and a user wondering
  why pressure stopped deserves the answer on screen.
- Support is a property of the **toolkit**, not the application (D18), so the table
  is keyed accordingly and a per-application entry is the exception.

**Settled: the cursor does jump.** D18's mechanism was right — the tablet position
and the relative pointer position genuinely diverge. The jump was tolerable when it
only happened on a deliberate hotkey press; **under D20 it happens on every focus
change**, which is far more often and unprompted. So the fallback is not yet the
seamless thing this decision describes.

The fix is a deterministic reposition: slam the relative pointer into a corner with
one large delta, then move it back by a known amount to the position the mapper is
already tracking. That requires the mapper to keep advancing during a fallback,
which it currently does not, and it requires pointer acceleration to be flat — an
accelerated pointer travels a different distance from the one asked for, so the two
positions would drift apart again. Both are prerequisites, not details.

**Two live cursors — fixed by invariant, not by transition handling.** Jumping
between modes, clicking across windows and crossing screens could leave a tool in
proximity on one screen while the pointer drove another: two cursors, both live,
observed drawing side by side in Blender.

The cause was handling this on *transitions*. Every path had to be right — mode
switch, focus change, screen crossing, panic, config reload — and missing one left a
pen down. It is now enforced as a **state check on every emit**: while output goes to
the mouse no tablet may be in proximity, and while it goes to a tablet only the active
one may be. The check is a bool per tablet, and lifting only emits when something is
actually down, so the hot path pays almost nothing for a guarantee that no longer
depends on enumerating every route.

---

## D21 — Pen capability is read from the toolkit the process loaded, not searched for as a string

**Context.** D20 needs to know, per window, whether the application can receive
tablet input. The first attempt searched the process's binaries for the
`zwp_tablet_manager_v2` interface string and was wrong in both directions, so the
decision collapsed to an allow-list of two (Krita, Blender) — meaning out of the
box, pressure worked in two applications and every other pen-capable one got the
pointer.

**Why the string search failed.** Presence proves nothing: every binary compiled
against `wayland-protocols` embeds the interface strings unused — this project's
own window carries the string three times and never binds it. And the one absence
it reported confidently was an artifact of scanning the wrong file: GTK3's Wayland
code lives in `libgdk-3`, not `libgtk-3`. Measured on this host, 2026-07-31:

```text
libgdk-3.so.0               1 match     ← the file the old scan never opened
libgtk-3.so.0               0 matches   ← the file it condemned GTK3 by
libgtk-4.so.1               1 match
libQt6WaylandClient.so.6   26 matches
libSDL3.so.0                1 match
firefox/libxul.so           0 matches
```

So GIMP, Inkscape and MyPaint — GTK3, real pressure support since 3.22 — were
being reported as *conclusively* unable to take a pen.

**Decision: ask which toolkit the process loaded, because the toolkit binds, not
the app.** Qt's and GDK's Wayland backends (and SDL3's) bind the tablet manager
whenever the compositor offers it, for every application. Presence of the loaded
library in `/proc/<pid>/maps` therefore answers for the application, by basename —
which also survives Flatpak, where the file itself is unreadable from outside but
its name is still visible in the map. The ladder, strongest first:

1. User override — always wins, corrects everything below.
2. Built-in drawing list (Krita, Blender) — apps carrying their own tablet code,
   kept named so a sandboxed build no inspection can see through keeps its pen.
3. No `libwayland-client` mapped → **X11 under XWayland**, which emulates a wacom
   stylus: pressure as XI2 valuators, clicks as core emulation. Always safe, and
   it is the route Wine and Proton drawing applications (CSP, SAI) take. The
   reverse is deliberately not symmetric — Mesa's EGL links `libwayland-client`,
   so plenty of X11 processes map it.
4. Chromium-family marker (`*.pak`, `v8_context_snapshot`, `icudtl.dat`) →
   unproven, pointer. **Checked before the toolkits because it defeats them**,
   found live: Vivaldi maps `libQt6WaylandClient` *and* `libgdk-3` purely for
   theming while its windows belong to its own Ozone code; Codium likewise maps
   gdk. Whether a given Chromium build binds the tablet protocol is
   version-dependent and unprovable from outside; one override line promotes a
   browser that is known to.
5. Toolkit tier: Qt Wayland / GDK (GTK3 via `libgdk-3`, GTK4 via `libgtk-4`) /
   SDL3 → pen.
6. String scan of the few files that carry their own Wayland code (`libxul`, the
   executable from `/proc/<pid>/exe`): absence everywhere readable is conclusive
   can't-bind; anything else — including an unreadable sandboxed file — is
   honestly unproven. No more scanning every non-`.so` mapping, which had been
   sweeping up locale archives and resource packs.

**Unknown still means the pointer**, for D18's asymmetry: a wrong pen loses
clicking entirely and silently; a wrong pointer loses pressure in one application,
visibly, with a one-line fix.

### Correction, 2026-08-01: the toolkit tier was wrong and has been demoted

**Capability is not use.** Granting the pen to every Qt and GTK application was a
mistake, and the reasoning above contains the error: it treats "the toolkit binds
`zwp_tablet_manager_v2`" as equivalent to "this application does something with a pen".

The two differ on Wayland in a way that matters. Under XWayland, a tablet arrives as an
XInput device and **the X server emulates core pointer events from it**, so every X11
client gets working motion, hover and clicks whether or not it knows what a tablet is.
Wayland has no equivalent — `wl_pointer` and `zwp_tablet_tool_v2` are separate protocols
with separate focus and nothing bridges them. A Wayland application without its own
tablet handling receives **nothing at all** from a pen while its cursor moves normally,
which reads as the application having frozen.

Reported in use: in a drawing mode, KDE's panel and every Qt window stopped highlighting
and stopped accepting clicks. plasmashell is Qt, so it was being handed a pen it has no
code to receive — and because the panel is also how you switch applications, the whole
desktop appeared to lock up.

So the ladder is now: user override → **a curated list of applications that actually
implement pen input** → X11 (where the platform guarantees it) → everything else takes
the pointer. The toolkit signal survives only as a *hint*, reported in the transport log
so the user can see that an application could take a pen if they add it to
`[tablet_support]`.

What survives from the original change is the part that was genuinely broken: GTK3 was
being condemned by a scan of `libgtk-3` when its Wayland code lives in `libgdk-3`, and
the X11 tier was being computed and then ignored. The allow-list this replaced was closer
to right than what replaced it, which is worth remembering — **the safe direction here is
narrow**, because being wrong toward the pen costs every click in that window.

**Known cost — since fixed.** Broadening the pen transport meant the wheel stopped
scrolling over pen-capable windows, because a pen carries no wheel and the relative
pointer's position was untrustworthy. The absolute pointer removed that obstacle: the
wheel now passes through it, positioned on the pen, whenever the pen is hovering
(D22's consequences). With the pen *down* the wheel remains reserved for
`pressure.manual`, which stages.md specifies as active only during a stroke.

---

## D22 — The fallback transport is an absolute pointer

**Decision:** when a tablet mode delivers through the pointer (D20), it emits through a
VMware-style **absolute pointer** — `ABS_X`/`ABS_Y` plus the source's buttons and wheel,
no pen or touch bits — fed with the mapper's tracked position. The relative mouse no
longer carries the fallback.

**Why.** D20 ended on "the cursor does jump": the compositor keeps the relative
pointer's position separately from the tablet's, so every transport change resumed from
a stale register. Every reposition scheme considered — the corner-slam, a `cursorPos`
query plus corrective delta — corrects the divergence after the fact, on every path,
forever. An absolute pointer **removes the register**: each emit states the position
outright, so fallback and tablet output cannot disagree, by construction. The same
lesson as D20's two-cursor fix — an invariant beats transition handling.

**Measured before building (P6, 2026-08-01, `stabmouse-probe abspointer`).** KWin
adopts the device within a second, drives the visible cursor from it, applies no
acceleration, and maps the absolute range **linearly onto the desktop's bounding box**:
emitted fractions landed at exactly `fraction × (3200, 1080)` on this host's
1920×1080 + 1280×1024 layout, origin (0, 0). The sink inverts that mapping. The same
probe measured `workspace.cursorPos` ignoring tablet tools entirely, which is the root
cause of hover focus tracking going blind in tablet transport (see the focus module).

**Consequences:**

- The transport-change teleport is gone, and D20's planned "deterministic reposition"
  is obsolete — there is nothing left to reposition.
- Fallback wheel and buttons travel through the same sink, so they land **under the
  visible cursor**. The relative path could guarantee neither.
- **The wheel works in tablet transport too**, by this route, but only because the pen
  stops while it turns — see D25. Still hover-only: with the pen down the wheel belongs
  to `pressure.manual`.
- D18's mirrored tablet clicks become correct at last: the press goes through the
  absolute pointer placed on the pen's position first — the same pixel the cursor
  already occupies, so nothing visibly moves.
- Flattening our own relative pointer's acceleration now matters only for mouse modes
  (where the daemon's curves must be the only acceleration) and the degraded path.
- The relative sink remains for mouse-intent modes, and as the **degraded fallback**
  when the compositor reports no screen layout — an absolute range with nothing to map
  onto. That path keeps the old teleport, and says so at startup.
- A *known* single screen now takes the per-screen tablet path rather than
  `Tablets::single`, so the mapper always has a layout when one exists. Previously the
  one-screen case left the mapper empty and fallback position tracking silently did
  nothing — found while wiring this, not by a report.
- `REL_X`/`REL_Y` are deliberately **not** declared on the absolute device, against
  this project's replicate-everything rule: a device carrying both relative and
  absolute motion axes invites libinput to classify it as something else.

**Deliberately not included: hover-on-pointer / pen-on-stroke.** With the absolute
pointer, D18's refinement becomes safe — proximity-in can happen at the exact position
the pointer already holds — and it is what would restore *hover* window detection in
tablet modes (P6: `cursorPos` never tracks a tablet tool, so only clicks switch the
transport today). It also restores hover wheel-scrolling (D21's known cost). But it
changes proximity behaviour per stroke, which applications must be measured against;
it is its own decision, taken separately.

---

## D23 — One position drives every transport

**Decision:** the desktop mapper's position is *the* position, for every mode. Mouse
modes advance it at the source device's counts-per-millimetre and deliver it through
the absolute pointer; tablet modes advance it at the span scale and deliver it through
the pen or the fallback (D22); each mode differs only in scale, filters, and which sink
speaks. Switching between any two modes therefore continues from where the cursor
actually is — the transport-change teleport D22 fixed for the fallback is now gone for
mode switches too, because there is no second position left to disagree.

**The hover question is inverted.** "What is under the cursor" cannot be asked for a
pen (P6), so the KWin script no longer answers it. It ships the **window layout** —
class, pid, geometry, stacking order, re-sent on change — and the daemon hit-tests its
own position locally. Hover transport switching in tablet modes works again, from the
mechanism rather than from a workaround, and works identically on every transport.
Inspection verdicts (D21) are computed per class when a layout arrives, on the D-Bus
thread, so the hot path's hit-test is rectangle arithmetic only.

**Devices we do not manage are reconciled, not fought.** A second mouse or a touchpad
moves the cursor without the daemon seeing it, and a stated absolute position would
snap it back — the unified design's one new hazard. So the script also reports the
pointer cursor's position (rate-limited to ~60Hz), and the daemon adopts a report that
disagrees by more than a tolerance — **only while the hand is still** (150ms since our
last emission), because our own emissions echo back through the same channel a few
pixels stale, and adopting an echo mid-motion would drag the position backwards. Never
while a pen is in proximity: the report describes the pointer's cursor, which the pen
does not move.

**`output = "relative"` is the escape hatch, and games are why.** Pointer-lock and
raw-input consumers want motion, not position. Whether they behave under absolute
motion is **unprobed** — P4's anti-cheat and passthrough results were measured with
relative output — so rather than gamble the gaming story on it, a mode can opt out of
the shared position entirely and emit raw deltas as before. Such a mode's cursor
drifts from the shared position by design; the reconciliation above re-syncs it the
moment the hand pauses, which is what makes *leaving* a relative mode jump-free too.
If a mouse mode ever misbehaves in a pointer-lock application, `output = "relative"`
on that mode is the immediate fix, and a probe of absolute motion under pointer-lock
is the follow-up that would retire the question.

**What this retires:** the mouse-versus-tablet position split, the last teleport, and
the wrong-window hazard of evaluating `windowAt` against a stale cursor. **What it
does not:** pressure over non-pen windows (physics, not policy), and the hover-wheel
cost over pen windows (D21) — still waiting on hover-on-pointer / pen-on-stroke,
which this design is one step closer to.

---

## D25 — Scrolling stops the pen, because the platform offers no other way

**Decision:** while the wheel is turning in tablet transport, the pen holds still. Hand
movement during that window is **discarded, not banked**. On by default, with
`freeze_position_while_scrolling = false` and `scroll_freeze_ms` per profile.

**The constraint is mutually exclusive, and both halves were measured.**

*Krita ignores mouse input while a pen is in proximity.* This is the standard defence
against tablet drivers that synthesise mouse events from pen events, which would
otherwise double every action, and the suppression is **time-based, resetting on each
tablet event**. A moving pen therefore keeps the wheel suppressed forever. Reported from
use as "scroll only works when not moving at all" — and the Krita-only nature of it is
what identified the mechanism, since Blender's GHOST does not filter this way. An earlier
compositor-side explanation had already been built and shipped and did nothing, which is
the cost of diagnosing by inference rather than by measurement.

*A tablet cannot carry the wheel itself.* The obvious escape — one device, no second
device to be suspicious of, exactly as a real tablet carries a ring — was probed (P9) and
fails at the libinput level. libinput classifies a device as a tablet tool **or** a
pointer, never both, and scroll is a pointer capability. With wheel axes attached, KWin
reports `pointer = false, tabletTool = true` on our tablet; the relative axes are routed
into the tablet-tool interface as an airbrush finger wheel, a channel no toolkit surfaces
to applications. The classification itself survives intact, which is worth keeping in the
record, but the wheel goes nowhere useful.

So a device that can scroll must not be a tablet, and a device that is a tablet cannot
scroll. Nothing in the arrangement of virtual devices resolves that.

**What is left is to make the tablet quiet.** Holding the pen still lets the proximity
filter lapse, and the wheel — back on the absolute pointer, positioned at the pen —
lands. The freeze outlasts the last notch by `scroll_freeze_ms` so a slow deliberate
scroll does not stutter between notches.

**Discarded, not banked**, deliberately. Banking the hand's movement would fling the
cursor the moment the freeze lifted, which is the transport-change teleport D22 exists to
prevent, arriving by another door. And moving the cursor was not what the hand was asking
for while it was scrolling — which is the same reasoning, reached from the user's side.

**Consequences:**

- The pen is **not taken out of proximity** to achieve this. Proximity churn is what
  applications handle worst (D13), and the freeze needs the tool present anyway.
- Off is a legitimate setting for someone who never scrolls in a tablet-aware application
  and would rather keep the pen live at all times.
- Blender never needed this, and is unharmed by it: a pen that holds still while the
  wheel turns is reasonable behaviour everywhere, which is why it is one global setting
  rather than a per-application table nobody could maintain.

---

## D24 — A modifier may be a mouse button or a keyboard key, and the keyboard is only ever *listened* to

**Decision:** `snap`'s constrain modifier is bound per mode to either a mouse button or
a keyboard key. A mouse button is read from the device already grabbed. A keyboard key
is read from an **ungrabbed, read-only** listener that opens a keyboard only when some
mode actually binds one.

**Why the question arose.** The stage spec asks for modifier-held activation — Photoshop's
shift-constrain — and nothing in the existing architecture could deliver it. Hotkeys are
KGlobalAccel global shortcuts (D19, interaction.md), which fire on press and cannot express
"while held"; and the project's first working rule is that **the keyboard is never grabbed**,
because a daemon that dies holding both the mouse and the keyboard leaves no way out.

**Reading is not grabbing, and the distinction is the whole decision.** An `EVIOCGRAB`
takes a device away from everyone else — that is what makes grabbing the keyboard
unrecoverable. An ungrabbed reader sees events that are *also* delivered normally, takes
nothing, and vanishes when its fd closes. The working rule is about grabbing, and it is
untouched here.

**But a process reading a keyboard is keylogger-shaped whatever its intent**, so the
capability is built to be unable to do more than it claims:

- **Opt-in and absent by default.** No binding, no keyboard opened. The strongest
  guarantee available is the file that was never opened.
- **Only the bound codes are recorded**, one pressed/not-pressed bit each. Every other
  event is discarded at the comparison and never reaches a variable, a counter or a log.
- **No history of any kind** — no buffer, no timestamps, no sequence. Only "is this one
  key down now", which is the entire question a modifier asks.
- **Opened read-only**, so the descriptor cannot write to the device or take it.
- **Announced at every startup**, naming how many keyboards are being watched. A daemon
  that has opened a keyboard should say so without being asked.

**The mouse-button path needs none of this**, which is why it is worth keeping as a
first-class option rather than a fallback: the grabbed device's buttons are already in
front of us, so a side-button binding adds no capability at all. Users with a spare side
button should prefer it, and the config's example uses one.

**Consequences:**

- The binding is resolved to an evdev code **once at mode build time**. A name-to-code
  lookup is a string comparison against the whole evdev table and has no business on the
  hot path.
- Every keyboard carrying the bound key is watched, not just one: laptops have a built-in
  keyboard and an external one, and a modifier must work from whichever was pressed.
- The modifier state reaches the filter **on the sample**, exactly as time does. A stage
  that queried the world would make replay diverge from live and turn the research harness
  into a lie (see the core's contract).
- A mode that binds nothing is simply never constrained, so `activation = "modifier"` with
  no binding is inert rather than stuck on.
