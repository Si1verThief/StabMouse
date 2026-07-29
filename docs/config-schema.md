# Config schema

Settled in Batch 3. Implemented by `stabmouse-config`. Terms are fixed by
[vocabulary.md](vocabulary.md); stage parameters by [stages.md](stages.md).

## Layout

```
~/.config/stabmouse/            authored, shareable, safe to version-control
├── config.toml                 defaults, devices, groups, global bindings
├── presets/
│   ├── raw.toml
│   └── inking.toml
└── profiles/
    └── line-art.toml

~/.local/state/stabmouse/       runtime state, never shared
└── state.toml                  last active profile and mode
```

**Runtime state lives outside the config directory.** People will put
`~/.config/stabmouse/` in git, and if last-active-mode were stored there, every
mode switch would dirty their working tree. Config is authored and shareable;
state is neither.

**The filename is the identity.** `presets/inking.toml` *is* the preset `inking`.
Renaming the file renames the preset, sharing the file is unambiguous, and the
filename can never disagree with an internal name field. `display_name` inside is
optional and purely cosmetic. Slugs are `kebab-case` and must be filesystem-safe.

**Schema version is per-file**, so an individually shared preset can be migrated
without touching anything else.

## Conventions

| | |
|---|---|
| Keys | `snake_case` |
| Slugs and filenames | `kebab-case` |
| Enable flag | `enabled` |
| Units | suffixed in the key — `radius_px`, `attack_ms`, `v_max_mm_s` |

## Preset file

```toml
schema = 1
display_name = "Inking"

[[stage]]
type = "normalize"
dpi = 1600

[[stage]]
type = "stabilize"
radius_px = 24.0
catch_up = 0.35

[[stage]]
type = "smooth"
enabled = false
amount = 0.4
min_cutoff_hz = 1.0
beta = 0.007
d_cutoff_hz = 1.0
```

- **Pipeline order is file order.** No `order` field to drift out of sync.
- Disabled stages **stay in the file** with `enabled = false`, so toggling a stage
  never loses its tuning.
- A stage type may appear more than once; an optional `id` disambiguates it in the
  UI.

### Macro controls: raw values are authoritative

Where a stage exposes a macro control over several real parameters (per D11), the
file stores **both**, and the raw values win.

If the raw values do not match what the macro would currently produce, the UI shows
the macro as **"custom"** rather than snapping it to a nearby position.

Storing only the macro would discard hand-edits. Storing only raw values would lose
the macro position. Storing both with raw authoritative means a tinkerer's edit
always survives, and the interface reports honestly that the macro no longer
describes the state.

## Profile file

```toml
schema = 1
display_name = "Line art"
default_mode = 1

[[mode]]
name = "Click"
output = "mouse"
preset = "raw"

[[mode]]
name = "Draw"
output = "tablet"
preset = "inking"
```

- Slot numbers are implicit from file order.
- **Auto-activate rules live here**, since they are a property of this profile.
- The mode toggle binding has a global default in `config.toml`, overridable per
  profile.

## config.toml

```toml
schema = 1

[defaults]
profile = "line-art"

[[group]]
name = "CAD pucks"
devices = [{ serial = "A1" }, { serial = "B2" }]
overrides = { "inking.stabilize.radius_px" = 12.0 }

[[device]]
match = { vid = "0738", pid = "0c08" }
label = "R.A.T. 8+"
managed = true
overrides = { "raw.normalize.dpi" = 1600 }
```

**Devices are opt-in.** Absence, or `managed = false`, means the device is never
touched. This is what keeps trackpads and unrelated hardware safe by default.

**Match precedence: `serial` → `vid`+`pid` → global default.** Most specific wins.

**Overrides use dotted string keys** — `"<preset>.<stage>.<param>"`. Nested tables
would imply a whole section is present, when the entire point is that an override
is sparse.

## References and integrity

- Presets are referenced by slug.
- **A missing referenced preset is never silently substituted.** The mode refuses to
  activate, the error is surfaced loudly, and that mode falls back to raw
  passthrough. Quietly swapping in a different preset would leave someone drawing
  with settings they did not choose and cannot see.
- Unknown keys survive a round-trip, so an older build cannot eat a newer config's
  fields.
- **`raw` is built in and cannot be deleted.** It is the fallback, so it must always
  exist.

## Export and import

- Export produces a **single pasteable TOML document**. A profile bundles the
  presets it references; a bare preset exports alone.
- Import collisions rename with a suffix by default; the GUI offers overwrite.

Sharing happens in forum posts and chat, so the format is a document you can paste,
not an archive you have to attach.

## Migration

**A migrated file is never written back unprompted.** Migration happens in memory;
the new form is written only when the user next saves something.

Silently rewriting a hand-commented config on the first launch after an upgrade is
precisely the betrayal the tinkerer contract exists to prevent. Read the old form,
run the new one, touch nothing until asked.

## Round-trip guarantee

Parse → write must produce a byte-identical file, including comments, key order and
whitespace. This requires a format-preserving editor (`toml_edit`), not `serde`
alone.
