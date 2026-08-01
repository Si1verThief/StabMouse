# D-Bus and CLI surface

Settled in Batch 7. Implemented by `stabmouse-ipc` (shared definitions),
`stabmouse-daemon` (server) and `stabmouse-cli`.

## Implementation status

Verified end-to-end against a live session 2026-07-31.

| Working | `SetMode` · `SetModeByName` · `ToggleMode` · `SetEnabled` · `Panic` · `Resume` · `Quit` · `GetStatus` · `ListModes` · `GetDegraded` · `Config.Reload` · `ListProfiles` · `ListPresets` · all properties · `ModeChanged` / `EnabledChanged` / `ConfigReloaded` / `OutputDegraded` |
|---|---|
| **Reports `NotSupported`** | `SetProfile` · `Devices.List` (empty) · `SetManaged` · `SetResolution` · `Config.Explain` · `Bench` |

Unimplemented methods **return an error naming what to do instead**, rather than
being absent or silently succeeding. A method that is missing from introspection
looks like a version mismatch; one that returns success without acting is worse.

### How the server avoids touching the hot loop

The input loop is single-threaded and blocks in `poll(2)`; zbus brings its own
executor. Merging them would put a D-Bus queue on the one path where jitter is the
whole problem (D5). Instead they meet at two points:

- **Commands out** become the same `control::Command` values the socket already
  carries. The loop needs no new wakeup source and there is one code path for both
  transports.
- **State in** is a snapshot the loop publishes on change and the service reads.

`GetStatus` therefore never touches the loop. Asking it and waiting would couple a
client's latency to whether the user happens to be moving their mouse, and would let
a wedged loop hang every caller — the failure this document forbids. The cost is a
snapshot up to one state change stale, which for values that only change on a mode
switch is not a cost.

**A bus failure is not fatal.** The daemon filters input with no bus at all; a
session without D-Bus loses the CLI and GUI, not the mouse.

**This is a public API.** Third-party tools calling it are an intended use, not an
accident — the ratbagd seam in D7 depends on exactly that.

## Bus

| | |
|---|---|
| Bus | Session (the daemon is per-user) |
| Name | `io.github.si1verthief.StabMouse` |
| Object path | `/io/github/si1verthief/StabMouse` |
| Interfaces | `.Daemon`, `.Devices`, `.Config` |
| Versioning | `InterfaceVersion` property; additive changes only within a major |

## Methods

| Interface | Method | Signature |
|---|---|---|
| `Daemon` | `SetMode` | `(u slot) → ()` |
| `Daemon` | `SetModeByName` | `(s name) → ()` |
| `Daemon` | `ToggleMode` | `() → (u slot)` |
| `Daemon` | `SetProfile` | `(s name) → ()` |
| `Daemon` | `SetEnabled` | `(b) → ()` |
| `Daemon` | `Panic` | `() → ()` |
| `Daemon` | `Resume` | `() → ()` |
| `Daemon` | `GetStatus` | `() → (a{sv})` |
| `Daemon` | `Bench` | `(u samples) → (a{sv})` |
| `Devices` | `List` | `() → (aa{sv})` |
| `Devices` | `SetManaged` | `(s id, b managed) → ()` |
| `Devices` | **`SetResolution`** | `(s id, u dpi) → ()` |
| `Config` | `Reload` | `() → ()` |
| `Config` | `ListProfiles` | `() → (a(ss))` |
| `Config` | `ListPresets` | `() → (a(ss))` |
| `Config` | **`Explain`** | `(s device, s key) → (s origin, v value)` |
| `Daemon` | `PointerOverWindow` | `(s class, s pid) → ()` — compositor-script feed |
| `Daemon` | `WindowLayout` | `(s rows) → ()` — compositor-script feed, see below |
| `Daemon` | `CursorMoved` | `(s x, s y) → ()` — compositor-script feed |

The three compositor-script methods are called by the KWin script the daemon installs,
not by clients — documented because nothing on this bus is secret, but their formats
serve the script and may change with it. `WindowLayout` carries the window stacking as
tab-separated `class pid x y width height` lines, bottom to top; the daemon hit-tests
its own position against it (D23). `CursorMoved` reports the pointer cursor so motion
from unmanaged devices re-syncs the shared position. Values are strings because a KWin
script's numbers marshal as `int32` and silently fail to match any other signature.

Two of these carry design weight:

**`Devices.SetResolution`** is the D7 seam. StabMouse never speaks hidraw; it learns
the active DPI either by subscribing to `ratbagd` or by any tool calling this. A
future Mad Catz driver is then just another caller and nothing device-specific enters
this codebase.

**`Config.Explain`** makes D8's cascade provenance queryable rather than GUI-only.
Given a device and a key it reports which level supplied the effective value —
`default`, `group: CAD pucks`, or `device`.

## Signals

| Signal | Signature |
|---|---|
| `ModeChanged` | `(u slot, s name)` |
| `ProfileChanged` | `(s name)` |
| `EnabledChanged` | `(b)` |
| `DeviceAdded` / `DeviceRemoved` | `(s id)` |
| `ConfigReloaded` | `()` |
| `OutputDegraded` | `(s reason)` |
| `ConflictDetected` | `(s description)` |
| `AppStartedBeforeDaemon` | `(s app)` |

`OutputDegraded` carries the "Limited — no pressure" state; `ConflictDetected` covers
yeetmouse being loaded or libinput acceleration not being flat;
`AppStartedBeforeDaemon` is the D13 case, which is otherwise silent and
indistinguishable from the feature being broken.

## Properties

`InterfaceVersion` · `Version` · `ActiveProfile` · `ActiveMode` · `Enabled` ·
`ManagedDevices` · `Degraded`

## CLI

Binary `stabmouse`, alias `sm`. Full parity with the GUI — see
[vocabulary.md](vocabulary.md) for the command list.

| | |
|---|---|
| Global flags | `--json`, `--quiet`, `--socket` |
| Daemon absent | Clear message, exit 4, **never hangs** |
| `status` | Human-readable table; `--json` for the full object |
| `bench` | Distribution and worst case, **never a bare mean** |
| `watch` | Live raw-versus-processed event view, Ctrl-C to exit |
| `export` / `import` | stdout / stdin by default, so piping works |
| `panic` | Alias `release` |
| Completions | Generated for bash, zsh, fish |

### Exit codes

| | |
|---|---|
| 0 | Success |
| 1 | Error |
| 2 | Usage error |
| 3 | Permission problem — e.g. not in the `input` group |
| 4 | No daemon reachable |

Exit 3 exists separately because permission failures are the single most likely
first-run problem and deserve a distinguishable, scriptable outcome rather than being
folded into a generic error.
