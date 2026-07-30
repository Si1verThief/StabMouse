# D-Bus and CLI surface

Settled in Batch 7. Implemented by `stabmouse-ipc` (shared definitions),
`stabmouse-daemon` (server) and `stabmouse-cli`.

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
