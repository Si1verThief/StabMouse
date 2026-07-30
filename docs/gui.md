# GUI specification

Screens and controls. Settled in Batch 4. Implemented by `stabmouse-gui` (Slint,
separate process — see D3, D4).

Read alongside [ux-requirements.md](ux-requirements.md) for the screen sketches and
walkthroughs this fills in.

Slint's viability was verified before any of this was decided: buffer-in-Rust →
`SharedPixelBuffer` → `Image` on a timer renders a responsive drawing canvas, and
the toolkit observes ~450–900 motion events/sec rather than being refresh-limited,
so the scratch canvas can own its own pointer events.

## Window shell

| | |
|---|---|
| Navigation | Left sidebar, 200px fixed, single window |
| Size | 1280×860 default, resizable, minimum 1000×700 |
| Size/position memory | Yes — `state.toml`, never config (D14) |
| Theme | **Follow system, with manual override** |
| Decorations | Native |

Dark-only was tempting and tested well, but it looks broken beside a light desktop
and excludes users who need light. Following the system costs one extra colour set.

## Visual design

Settled in Batch 5, building on the canvas probe's theme.

### Colour

| | |
|---|---|
| **Accent** | **Read KDE's accent colour from D-Bus** (`org.kde.plasmashell.accentColor`), falling back to `#6ea8fe` |
| Dark | ground `#1b1b1f` · surface `#25252b` · text `#d8d8de` · muted `#8a8a93` |
| Light | ground `#fafafb` · surface `#ffffff` · text `#1e1e24` · muted `#6b6b74` |
| Semantic | warning amber · error red · success green · info = accent |
| Accent scope | Interactive state and selection only — never decoration |

Reading the system accent costs one D-Bus call and makes the app feel native rather
than like a foreign application that happens to be dark.

### Typography

| | |
|---|---|
| UI | System default |
| **Numeric fields and config keys** | **Monospace** — digits align, keys stay legible |
| Scale | 13px base · 11px muted · 15px headings |
| Weight | Regular and medium only — no bold-as-emphasis in dense parameter lists |

### Density and shape

| | |
|---|---|
| Density | Comfortable-leaning-compact — long sessions, many parameters |
| Grid | 4px |
| Radius | 4px controls · 6px cards |
| Separation | Spacing over dividers; dividers only between major regions |

### Iconography

| | |
|---|---|
| Source | Small bundled SVG set — system icon themes vary too much across distros |
| Style | Outline, 1.5px stroke |
| Stage icons | **None.** Text labels; nine near-identical abstract glyphs help nobody |
| Tray | Monochrome silhouette plus a small state dot; must read at 16px |

### Motion

| | |
|---|---|
| Scope | Expander open/close and value transitions only. No decorative motion |
| Duration | 120ms, ease-out |
| **Reduced-motion preference** | **Respected** — the accessibility audience is first-class, not an afterthought |

### Canvas

| | |
|---|---|
| Background | **White always by default**, theme-independent, with an option. It is a drawing surface; artists expect paper |
| Stroke / ghost | `#18181f` / `#c8c8d2` |
| Grid or guides | None in v1 |

## Controls

This section governs every parameter in the application, so it matters more than any
individual screen.

| | |
|---|---|
| Default control | **Slider and numeric field, side by side, always both** |
| **Range** | **Slider spans the *recommended* range; the field accepts anything beyond it** |
| Numeric entry | Plain field, no steppers |
| Reset | Small revert affordance per parameter, visible only when changed |
| Config key | Shown on hover; a "show keys" mode pins them all |
| Curve editing | Draggable points **and** a numeric table |
| Enums | Dropdown; segmented buttons for two or three options |
| Booleans | Switch |
| Units | In the field suffix, not the label |

### Sliders suggest, fields decide

Typing `500` into a field whose slider stops at 150 works. The value is accepted, the
slider pins to its end, and the field shows the truth.

This is the concrete form of the project's standing preference against arbitrary
clamps. A slider communicates a sensible working range without imprisoning anyone in
it — and the probe work produced two cases where the "unreasonable" value was the
right one: `velocity_smoothing_ms = 0` has a distinctive look someone may want, and
`min_pressure = 0` is permitted with a warning rather than refused.

No stepper buttons: they imply a meaningful granularity that continuous parameters do
not have.

### Every control admits what it writes

Config keys appear on hover, with a mode that pins them all. This is the tinkerer
contract made visible — GUI knowledge and file knowledge become the same knowledge,
and someone learning the file format can read it off the interface.

## Dashboard

Pipeline strip · active device · conflict warnings · mode and profile switcher ·
Disable button.

- **Live event scope: present, but off by default.** It is the single best
  trust-builder in the application and the worst idle-CPU offender. Opt in.
- Conflict warnings are **inline cards in the flow**, not a dismissible banner.
- The quick switcher duplicates the tray deliberately — they serve different moments.

## Profile editor

Table-based: dense, orderable, and reads like the config file it represents.

- `+ add slot` row at the bottom; `×` per row.
- Default slot marked by a radio column.
- Toggle binding in the footer, showing the inherited global with an override option.

## Preset editor

Pipeline list left, parameters right, scratch canvas bottom.

| | |
|---|---|
| Reorder | **Drag, plus keyboard up/down** — drag alone is inaccessible |
| Stage enable | Switch per row |
| Add stage | Button opening a categorised menu |
| **Scratch canvas** | **Always visible, resizable split. Never a dialog** |
| A/B | One toggle plus a visible `A|B` indicator; spacebar bound |
| Ghost overlay | Checkbox on the canvas itself |
| Response curve | In the parameters pane, under the stage that owns it |

The canvas is not a preview you open — it is where the work happens. Tuning without
immediate feel is guessing, and every filter finding in this project so far came from
drawing rather than from reasoning.

## Progressive disclosure

**Per-group expanders, not a global "advanced" switch.**

- Macro control at the top of its group; the expander beneath reads "Acceleration
  curve", "Advanced", or similar.
- Expanded state persists per group, in `state.toml`.
- The macro reads **"custom"** when raw values diverge from what it would produce,
  rather than snapping to a nearby position.

A global toggle makes people hunt for what appeared or vanished. Per-group keeps the
disclosure adjacent to the thing being disclosed.

## Devices

Two sections: **Connected** and **Remembered** (greyed).

Per device: `managed` switch, label, DPI, overrides.

- **Only overridden parameters are listed**, with an "add override" picker. Showing
  every parameter with inherited values greyed out would be hundreds of rows per
  device; overrides are sparse by design and the interface should be too.
- Each value carries an inline origin tag — `default`, `group: CAD pucks`, `device` —
  satisfying the cascade-provenance requirement from D8.

## Feedback and status

| | |
|---|---|
| Errors and warnings | Inline where relevant, plus a persistent status area in the sidebar footer |
| Mode-switch OSD | KDE-native notification style, no custom overlay |
| **"Started after Krita"** (D13) | Dashboard card, dismissible, naming the application and the fix |
| **Anti-stall recommendation** (D16) | Inline hint in the `pressure` group when `stabilize` is present — a suggestion, never a warning |
| "Limited — no pressure" | Tray tooltip and the dashboard pipeline strip |

The D13 card matters disproportionately: an application launched before the daemon
gets no pressure and no error, which is indistinguishable from the feature being
broken, and the fix — restart the application — is not guessable.
