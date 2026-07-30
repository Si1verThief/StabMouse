# Modules and acceptance criteria

What each piece is responsible for, and what "working as intended" means for it.
Written to be checkable — if a criterion here can't be tested, it's badly worded.

Read alongside [architecture.md](architecture.md) (shape and rationale) and
[ux-requirements.md](ux-requirements.md) (what the user experiences).

## Runtime shape

Three requirements together force the threading model: mode switching must be
instant and allocation-free, the control plane is async D-Bus, and config
hot-reloads.

```
┌─ hot thread ──────────────────────────────────────┐
│  evdev read ─▶ preset.process() ─▶ uinput write    │
│  blocking, no locks, no allocation                 │
│         ▲                                          │
│         │ atomic pointer swap                      │
└─────────┼──────────────────────────────────────────┘
          │
┌─ control thread (async) ──────────────────────────┐
│  D-Bus · config watch · hotplug · ratbagd ·       │
│  focus tracking · watchdog                         │
│  builds new state off-path, publishes by swap      │
└────────────────────────────────────────────────────┘
```

Two consequences that constrain everything downstream:

- **Mode switching is an atomic swap of a pre-built preset.** Every preset in the
  active profile is constructed and resident *before* the hotkey is pressed. It is
  not a config reload.
- **Core never reads the clock.** Time arrives on the event; evdev provides
  timestamps. Any `Instant::now()` inside a filter makes replay diverge from live
  and turns the research harness into a lie.

## Crates

| Crate | Role | Platform-bound |
|---|---|---|
| `stabmouse-core` | Filters, pipeline, math. Pure. | No — also WASM + PyO3 |
| `stabmouse-config` | Schema, cascade, migration, format-preserving IO | No |
| `stabmouse-ipc` | D-Bus interface and shared types | Linux |
| `stabmouse-input` | evdev capture, grab, identity, hotplug | Linux |
| `stabmouse-output` | uinput sinks: relative mouse + tablet | Linux |
| `stabmouse-daemon` | Orchestration, runtime state, watchdog | Linux |
| `stabmouse-cli` | Full-parity control surface | Linux |
| `stabmouse-gui` | Slint app, launched on demand | Linux |
| `stabmouse-tray` | Always-on, minimal, separate process | Linux |
| `stabmouse-replay` | Recording, synthetic device, regression harness | Linux |
| `stabmouse-py` | PyO3 bindings to core | No |

The tray is **a separate process from the GUI**, not a window of it. It must be
always-resident at near-zero cost, and emergency release has to work on a machine
where the main window has never been opened.

`stabmouse-ipc` is its own crate despite being thin, because the daemon, CLI, GUI
and tray must all agree on it, and because it is a documented public API that
third-party tools are expected to call.

---

## stabmouse-core

Transforms input events into output events. Pure.

- **Deterministic.** Identical input sequence and parameters produce bit-identical
  output. The replay harness and every regression test rest on this.
- **No clock, no I/O, no allocation** after construction.
- **Motion is conserved.** Σ output ≈ Σ input × gain with the subpixel remainder
  carried; no drift across millions of events. Property-testable.
- **Identity settings are pass-through** for every stage.
- Survives pathological input: zero deltas, saturated deltas, duplicate
  timestamps, **time moving backwards**, multi-hour gaps from suspend.
- **Cannot panic** on any combination of input and parameters.
- Builds for `wasm32` and through PyO3 without feature gymnastics.

## stabmouse-config

Schema, the device cascade, migration, and file IO that respects the user's file.

- **Byte-exact round-trip.** Parse → write → identical file, *including comments,
  ordering and whitespace*. This is the tinkerer contract; requires a
  format-preserving editor, not `serde` alone.
- **Cascade is explainable.** For any effective value, report which level supplied
  it (default / group / device). The GUI displays provenance.
- **Reference integrity.** Deleting or renaming a preset that profiles reference is
  refused or cascaded — never silently broken.
- **Every historical schema version loads and migrates**, version stamped in-file.
- **Unknown keys are preserved**, so an older build cannot eat a newer config's
  fields.
- **Invalid config never takes down the daemon** — fall back to last-known-good
  and surface the error.
- Hot reload is **atomic**: no half-applied state reaches the hot path.

### Layout: a directory, not one file

```
~/.config/stabmouse/
├── config.toml         defaults, devices, groups, bindings
├── presets/             one file per preset
│   ├── raw.toml
│   └── inking.toml
└── profiles/            one file per profile
    └── line-art.toml
```

One file per preset makes sharing a single preset trivial, which is what the Library
feature wants. Load cost is negligible at this scale — these are small files read
once at startup and on change.

**Export bundles the directory into one pasteable document** so sharing stays a
copy-paste operation rather than a zip file.

## stabmouse-ipc

The D-Bus interface, shared by daemon, CLI, GUI and tray.

- **Versioned, introspectable, documented as a public API.** Third-party tools are
  an intended consumer, not an accident.
- **Every CLI and GUI action is a method.** No side channels; poking the config
  file is not a control mechanism.
- Signals for: mode changed, profile changed, device added/removed, config
  reloaded, output degraded.
- The **daemon never depends on a client being present.**

## stabmouse-input

Device enumeration, identity, grabbing, reading, hotplug.

- **Only grabs devices explicitly opted in.** Never the keyboard. Never the
  trackpad by default.
- **Grab is released on every exit path**: clean shutdown, panic, SIGTERM/SIGINT,
  watchdog, D-Bus disable, parent death.
- Identity matching degrades serial → VID:PID → default, and device identities
  remain **stable across reconnect**.
- **Hotplug** re-establishes without user action. **Sleep/resume** recovers,
  including the timestamp discontinuity.
- Grab failure is **explained** — naming the permission cause — and leaves the
  system fail-open.
- Exposes read timing for `bench`.

## stabmouse-output

Virtual device creation and event emission.

- **Emits only axes whose value actually changed.** Restating every axis on every
  `SYN_REPORT` is an event storm that *measurably breaks application UI* — it
  caused doubled clicks in Krita's menus until fixed. This is a correctness
  requirement, not an optimisation. Verified 2026-07-30.
- **Both sinks are created at daemon startup and never torn down** — see D13.
- Relative sink reproduces **every axis and button the source had**, including
  hi-res wheel.
- Tablet sink declares evdev bits such that **libinput classifies it as a tablet
  tool** and KWin exposes it through `tablet_v2`.
- Virtual devices **persist across mode switches and across `Enabled: off`**, so
  applications never see a device vanish mid-session.
- **No orphan devices after a crash** — verified by `kill -9` followed by
  re-enumeration.
- Absolute coordinate space **survives monitor hotplug and geometry change**.
- Device names and IDs are stable across daemon restarts.

## stabmouse-daemon

Orchestration, runtime state, safety.

- **Mode switch takes effect on the next event**, allocation-free.
- **Timer-driven settle phase after stroke end.** When the button is released, filters
  still hold state that must be flushed — the stabiliser's accumulated lag and the
  pressure release envelope. The pipeline is event-driven and no further mouse events
  may arrive, so the daemon must keep generating zero-motion samples until filters
  converge or a timeout elapses. Without this, the stabiliser recovers its lag as a
  single straight-line jump (measured at 4000–9000 mm/s) and pressure never ramps
  down. Found by the replay bench, 2026-07-30; see stages.md.
- **Profile switch** rebuilds presets off-path and swaps; no input dropped.
- **Fail-open on panic**: grabs released, raw input flows.
- **Idle CPU ≈ 0** with no input.
- Detects and reports conflicts: yeetmouse/maccel loaded, non-flat libinput
  acceleration, Steam Input.
- Subscribes to `ratbagd`; exposes `SetDeviceResolution` for any other caller.
- **Auto-switch is opt-in and always announces itself.**

### Watchdog

The grab is held by a file descriptor, and **the kernel releases it when the fd
closes** — which happens on process death. So process death is already safe; the
only dangerous state is *alive but wedged*.

Design:

1. **A watchdog thread** with its own timer, independent of the hot thread. If the
   hot thread misses its heartbeat, the watchdog calls `abort()`.
2. `abort()` closes the fds → **the kernel releases the grab** → the cursor comes
   back.
3. `systemd Restart=on-failure` brings the daemon back.

This needs no second process to install or supervise. The residual risk is a
*whole-process* freeze — SIGSTOP, severe OOM, kernel-level stall — where no
in-process thread runs. An external releaser is the only cover for that, and is
deferred until we see it happen.

The stronger guarantee is not to wedge at all: no locks on the hot path, bounded
work per event, no unbounded loops in any filter, and non-blocking writes.

## stabmouse-cli

- **Full parity.** Any state reachable in the GUI is reachable here.
- Useful when the daemon is **absent or broken** — reports clearly, never hangs.
- `--json` everywhere.
- `bench` reports distribution and worst case, not a mean.
- Meaningful exit codes.

## stabmouse-gui

- **Zero cost when closed**; low and flat when open and idle.
- Scratch canvas with **ghost overlay** and **A/B on spacebar**, always visible in
  the preset editor.
- **Bidirectional sync**: external file edits appear live; GUI writes preserve
  comments.
- Every control **displays its config key**.
- Shows **cascade provenance**, and warns when editing a **shared preset**.
- **Never required** for any function.

## stabmouse-tray

- Always-on, minimal RSS, ~0 CPU.
- Profile and mode legible **from the icon alone**.
- Emergency release works with the main GUI never launched.
- **Reconnects automatically** when the daemon restarts.

## stabmouse-replay

- Records raw timestamped events in a **stable, documented format**.
- Creates a **synthetic source device** and replays with accurate inter-event
  timing.
- **Deterministic** — same recording, same output, every run, in CI.
- This is the safe development loop. **It must exist before anything grabs a real
  mouse.**

## stabmouse-py

- Exposes the core preset to numpy.
- **Bit-identical to the daemon** on the same input, enforced by golden vectors
  shared between the Rust and Python test suites. If these ever disagree, the
  research harness is worthless.

---

## Cross-cutting

### Latency budget

**Measured baseline** (2026-07-30, synthetic closed loop, release build): the full
evdev-read → forward → uinput-write → read-back round trip is **13µs median**, p99
0.215ms, p99.9 0.714ms, max 1.77ms. The kernel transits are far cheaper than the
50–200µs each originally assumed.

| Stage | Target |
|---|---|
| Full transport round trip | ≤50µs median |
| preset process | <10µs |
| daemon overhead | <20µs |
| **p99.9 total** | **<1ms** — one polling interval at 1000Hz |

The median is already comfortable. **The tail is the real target**, and it is
scheduler jitter rather than compute — so the hot thread wants elevated scheduling
priority, and `bench` must report distribution and worst case, never a mean.

### Hazard: never threshold a per-sample value when the quantity accumulates

This has now caused three separate bugs in three different places. It should be
checked for deliberately in review.

**The pattern:** a quantity accumulates across samples, but the code gates on the
*per-sample* value. At 1000 Hz each individual sample is tiny, so the gate rejects
every one of them while the total is large. Slow-but-continuous motion is silently
lost, and the failure is invisible in testing because fast motion works fine.

Occurrences:

| Where | Symptom |
|---|---|
| Quantizer, mm → counts | Any multiplier below 1.0 truncates every sub-count step to zero. A slowly-moved mouse does not move the cursor at all |
| `pressure` stall handling | Banking time for zero-distance samples inflates the divisor, so resumption reads as spuriously slow and produces the exact hotspot the feature prevents |
| Canvas ink stamping | With a low `catch_up` the anchor advances a fraction of a pixel per sample, so the output point travels visibly while drawing nothing |

**The fix is always the same shape:** accumulate, threshold the accumulated value,
and reset the accumulator only when it fires. Keep a "last committed" reference point
rather than comparing against the previous sample.

**Two corollaries that are easy to miss:**

- **Flush on completion.** Whatever is still banked when a stroke ends must be
  emitted, or the tail of every stroke is lost.
- **Zero and small are different.** A genuinely stationary point should hold; a
  slowly-moving one should bank. Conflating them is what caused the `pressure` bug.

### Error policy

The hot path never returns `Result` for expected conditions — anything that can
fail is resolved on the control thread before the swap. A panic anywhere means
release-and-passthrough. There is no failure mode that results in dead input.

### Focus tracking

Used only by opt-in auto-switch. The mechanism differs across compositors and
portability is unresearched.

**Everything must work fully without it.** Where it is unavailable, the software
says so plainly rather than silently not switching — a feature that quietly stops
working is worse than one that is honestly absent.

### Testing

- Property tests for core invariants (motion conservation, determinism, identity
  pass-through).
- Golden replay vectors shared between Rust and Python.
- `kill -9` recovery tests for orphan devices and grab release.
- Synthetic-device integration suite running in the container with no real
  hardware attached.
