//! What a preset may contain: every stage, every parameter, and what each one accepts.
//!
//! # Why this exists, and why it lives here
//!
//! [`assemble`](crate::assemble) reads parameters out of a preset; this describes what is
//! there to read. Without it an editor can only offer what a file already contains, so a stage
//! added blank would have no knobs at all and a new preset could never be filled in — which is
//! exactly the wall the first preset editor hit.
//!
//! It sits in `stabmouse-config` rather than in the frontend because it answers a schema
//! question, not a presentation one: *what is a preset allowed to say*. The GUI, the CLI and a
//! future settings importer all need the same answer, and a second copy would drift.
//!
//! # It is checked against the assembler
//!
//! A catalog that lists a parameter the assembler ignores, or omits one it reads, is worse
//! than no catalog — the editor would write keys nothing consumes, or hide keys that matter.
//! The tests at the bottom build every stage here through the real assembler and require it to
//! accept every key without a single "unknown parameter" warning.
//!
//! # Ranges are recommendations
//!
//! `soft_min`/`soft_max` bound a slider, never a value. Files may hold anything, and this
//! project has two cases already where the value outside the reasonable range was the wanted
//! one — see gui.md.

/// What a parameter accepts, and how an editor should offer it.
#[derive(Debug, Clone, Copy)]
pub enum ParamKind {
    Float {
        default: f64,
        soft_min: f64,
        soft_max: f64,
        /// Digits to display. Resolutions are whole numbers; radii are not.
        decimals: u8,
    },
    Bool {
        default: bool,
    },
    /// A fixed set of names. The first is not assumed to be the default.
    Choice {
        default: &'static str,
        options: &'static [&'static str],
    },
    /// A key or button name — `BTN_SIDE`, `KEY_LEFTSHIFT` — or a list of them, any of which
    /// engages. Resolved by the daemon (D24), which is also the only thing that can *capture*
    /// one: it holds an exclusive grab, so the source device's buttons reach nothing else.
    Binding,
}

#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub help: &'static str,
    pub kind: ParamKind,
    /// Parameters only meaningful when another is set a particular way — the joystick
    /// settings under `mode`, the curve settings under `curve_type`. An editor may fold these
    /// away; nothing depends on it doing so.
    pub depends_on: Option<(&'static str, &'static str)>,
    /// The inverse of `depends_on`: hidden when the named parameter *does* hold this value.
    ///
    /// Some settings are relevant to most modes and irrelevant to one. Listing the modes that
    /// do want them means editing every entry each time a mode is added; naming the one that
    /// does not is both shorter and less likely to go stale.
    pub hidden_when: Option<(&'static str, &'static str)>,
    /// Parameters that only mean anything when the named binding holds a *chord*.
    ///
    /// A halfway state needs more than one part to be halfway through; with a single button
    /// there is nothing between held and released, so offering the setting would be offering
    /// one that cannot do what it says.
    pub needs_chord: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct StageSpec {
    pub kind: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub params: &'static [ParamSpec],
    /// `normalize` must be first and `pressure` last; the assembler enforces it, and an editor
    /// should not offer a reorder it knows will be undone.
    pub pinned_first: bool,
    pub pinned_last: bool,
}

const fn f(
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    default: f64,
    soft_min: f64,
    soft_max: f64,
    decimals: u8,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        unit,
        help,
        kind: ParamKind::Float { default, soft_min, soft_max, decimals },
        depends_on: None,
        hidden_when: None,
        needs_chord: None,
    }
}

const fn when(spec: ParamSpec, param: &'static str, value: &'static str) -> ParamSpec {
    ParamSpec { depends_on: Some((param, value)), ..spec }
}

/// Shown everywhere except when `param` holds `value`.
const fn not_when(spec: ParamSpec, param: &'static str, value: &'static str) -> ParamSpec {
    ParamSpec { hidden_when: Some((param, value)), ..spec }
}

/// Only meaningful when `binding` holds a chord rather than a single button.
const fn when_chord(spec: ParamSpec, binding: &'static str) -> ParamSpec {
    ParamSpec { needs_chord: Some(binding), ..spec }
}

const fn b(key: &'static str, label: &'static str, default: bool, help: &'static str) -> ParamSpec {
    ParamSpec {
        key,
        label,
        unit: "",
        help,
        kind: ParamKind::Bool { default },
        depends_on: None,
        hidden_when: None,
        needs_chord: None,
    }
}

const fn c(
    key: &'static str,
    label: &'static str,
    default: &'static str,
    options: &'static [&'static str],
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        key,
        label,
        unit: "",
        help,
        kind: ParamKind::Choice { default, options },
        depends_on: None,
        hidden_when: None,
        needs_chord: None,
    }
}

const fn bind(key: &'static str, label: &'static str, help: &'static str) -> ParamSpec {
    ParamSpec {
        key,
        label,
        unit: "",
        help,
        kind: ParamKind::Binding,
        depends_on: None,
        hidden_when: None,
        needs_chord: None,
    }
}

pub const STAGES: &[StageSpec] = &[
    StageSpec {
        kind: "normalize",
        label: "Normalize",
        help: "Turns raw sensor counts into millimetres. Everything downstream is physical, \
               which is what lets a preset mean the same thing on someone else's mouse.",
        pinned_first: true,
        pinned_last: false,
        params: &[f(
            "dpi",
            "Sensor resolution",
            "dpi",
            1600.0,
            400.0,
            8000.0,
            0,
            "Your mouse's true DPI. A wrong value silently mis-scales every other setting.",
        )],
    },
    StageSpec {
        kind: "rotate",
        label: "Rotate",
        help: "Turns the whole motion vector. An accessibility feature first — for holding \
               the mouse off-square — and a convenience for working at an angle second.",
        pinned_first: false,
        pinned_last: false,
        params: &[f("angle_deg", "Rotation", "°", 0.0, -45.0, 45.0, 1, "")],
    },
    StageSpec {
        kind: "deadzone",
        label: "Deadzone",
        help: "Discards movement below a threshold. 0 by default: the sensor essentially \
               never reverses spuriously at ordinary resolutions.",
        pinned_first: false,
        pinned_last: false,
        params: &[f(
            "threshold_mm",
            "Threshold",
            "mm",
            0.0,
            0.0,
            1.0,
            3,
            "Movement smaller than this never reaches the cursor.",
        )],
    },
    StageSpec {
        kind: "smooth",
        label: "Smooth (one-euro)",
        help: "Adaptive low pass: heavy smoothing when slow, low lag when fast.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            f(
                "min_cutoff_hz",
                "Cutoff",
                "Hz",
                5.0,
                1.0,
                20.0,
                2,
                "Lower smooths more. Measured: 5 is the general default at 0.30mm of lag, 2 is \
                 the tremor setting at 0.44mm.",
            ),
            f(
                "beta",
                "Speed response",
                "",
                0.05,
                0.0,
                0.3,
                3,
                "How much fast movement loosens the smoothing. Must be 0.05–0.2 in these \
                 units, not the ~0.007 seen in one-euro literature.",
            ),
            f("d_cutoff_hz", "Speed smoothing", "Hz", 1.0, 0.1, 5.0, 2, ""),
        ],
    },
    StageSpec {
        kind: "stabilize",
        label: "Stabilize (pulled string)",
        help: "The cursor drags an anchor on a leash. This is what produces the confident \
               sweeping arc, and the single most important parameter for drawing feel.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            f(
                "radius_mm",
                "Radius",
                "mm",
                0.4,
                0.0,
                4.0,
                2,
                "Hand movement the cursor rests behind by. Measured: 0.2 removes wobble with \
                 all detail intact, 1.0 begins rounding corners, 4.0 destroys a drawing.",
            ),
            f(
                "catch_up",
                "Catch-up",
                "",
                0.35,
                0.05,
                1.0,
                2,
                "How quickly the anchor closes the gap once it starts moving.",
            ),
        ],
    },
    StageSpec {
        kind: "average",
        label: "Average (moving window)",
        help: "Averages position over a time window. Takes tremor off an otherwise fine line, \
               where stabilize reshapes the line entirely.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            f(
                "window_ms",
                "Window",
                "ms",
                50.0,
                0.0,
                150.0,
                0,
                "Measured: 50ms removes a fifth of path wobble for 0.057mm of lag. Past ~100ms \
                 each extra millisecond buys less and costs the same. 0 is off.",
            ),
            c(
                "weighting",
                "Weighting",
                "exponential",
                &["exponential", "linear", "gaussian"],
                "Measured: exponential removes the most wobble per millimetre of lag at every \
                 window tested. Linear is the most predictable to reason about.",
            ),
        ],
    },
    StageSpec {
        kind: "sensitivity",
        label: "Sensitivity",
        help: "A flat multiplier, with acceleration curves behind it for those who want them.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            f("multiplier", "Sensitivity", "×", 1.0, 0.1, 4.0, 2, ""),
            f(
                "y_ratio",
                "Vertical ratio",
                "×",
                1.0,
                0.5,
                2.0,
                2,
                "Vertical sensitivity relative to horizontal.",
            ),
            c(
                "curve_type",
                "Curve",
                "flat",
                &["flat", "power"],
                "Flat is a constant multiplier. Power accelerates with speed.",
            ),
            when(
                f("curve_reference_mm_s", "Reference speed", "mm/s", 50.0, 10.0, 300.0, 0, ""),
                "curve_type",
                "power",
            ),
            when(
                f("curve_exponent", "Exponent", "", 1.0, 0.2, 3.0, 2, ""),
                "curve_type",
                "power",
            ),
            when(f("curve_min", "Minimum", "×", 0.5, 0.1, 2.0, 2, ""), "curve_type", "power"),
            when(f("curve_max", "Maximum", "×", 3.0, 0.5, 8.0, 2, ""), "curve_type", "power"),
        ],
    },
    StageSpec {
        kind: "snap",
        label: "Snap (constrain)",
        help: "Constrains strokes to allowed directions while a button or key is held — \
               shift-constrain, with the modifier of your choosing.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            c(
                "constraint",
                "Constraint",
                "angle",
                &["angle", "line"],
                "Angle snaps to evenly spaced directions. Line holds whichever direction the \
                 stroke set off in.",
            ),
            when(
                f(
                    "divisions",
                    "Divisions",
                    "",
                    4.0,
                    2.0,
                    16.0,
                    0,
                    "4 locks to the axes, 8 adds the diagonals.",
                ),
                "constraint",
                "angle",
            ),
            when(
                f(
                    "tolerance_deg",
                    "Tolerance",
                    "°",
                    45.0,
                    1.0,
                    90.0,
                    1,
                    "How near an allowed direction the hand must be. Half a division makes it \
                     a lock; narrower makes it a magnet that only bites near an axis.",
                ),
                "constraint",
                "angle",
            ),
            f(
                "strength",
                "Strength",
                "",
                1.0,
                0.0,
                1.0,
                2,
                "1 pins to the constraint; below that it leans without taking over.",
            ),
            c(
                "activation",
                "Activation",
                "modifier",
                &["modifier", "always"],
                "Held, the way shift-constrain works everywhere else — or permanently on.",
            ),
            when(
                bind(
                    "modifier",
                    "Modifier",
                    "A mouse button costs nothing extra. A keyboard key makes the daemon watch \
                     that one key, read-only and never grabbed (D24).",
                ),
                "activation",
                "modifier",
            ),
        ],
    },
    StageSpec {
        kind: "scroll",
        label: "Scroll gesture",
        help: "Turns held movement into scrolling: a touchscreen-style swipe, or the \
               middle-click autoscroll Windows and browsers have.",
        pinned_first: false,
        pinned_last: false,
        params: &[
            c(
                "mode",
                "Mode",
                "drag",
                &["drag", "wheel", "joystick"],
                "Drag scrolls by how far you move. Wheel routes your own wheel through this \
                 stage so it picks up the speed and momentum set here. Joystick sets a speed \
                 from how far you hold away from where you pressed, and keeps scrolling while \
                 you hold still.",
            ),
            bind(
                "button",
                "Button",
                "What engages the gesture. BTN_MIDDLE is the familiar one — and in wheel \
                 mode, leaving it unbound means the wheel is always routed through the stage \
                 rather than only while something is held.",
            ),
            c(
                "mouse_passthrough",
                "Bound mouse button still",
                "unless_active",
                &["always", "unless_active", "reserved"],
                "What a bound *mouse* button still does for the application — keyboard keys \
                 are never taken, since this daemon does not emit them. `unless_active` keeps \
                 the button working except in the exact combination you bound, so with \
                 alt+middle a plain middle click still pastes. `always` never takes it; \
                 `reserved` always does.",
            ),
            when(
                f(
                    "wheel_gain",
                    "Wheel multiplier",
                    "×",
                    1.0,
                    0.1,
                    5.0,
                    2,
                    "Output notches per notch of your wheel.",
                ),
                "mode",
                "wheel",
            ),
            when(
                f(
                    "speed",
                    "Scroll speed",
                    "",
                    0.25,
                    0.02,
                    4.0,
                    3,
                    "How far the page moves for a given hand movement. Higher is faster — and \
                     with the cursor unfrozen this is what decides whether the page keeps pace \
                     with the pointer, which is somewhere near 0.1 on a typical screen.",
                ),
                "mode",
                "drag",
            ),
            b("drag_invert", "Invert", false, ""),
            when(
                f(
                    "joystick_deadzone_mm",
                    "Deadzone",
                    "mm",
                    2.0,
                    0.0,
                    10.0,
                    1,
                    "Displacement before scrolling starts, so a resting hand does not creep.",
                ),
                "mode",
                "joystick",
            ),
            when(
                f(
                    "joystick_gain",
                    "Speed",
                    "",
                    1.5,
                    0.1,
                    6.0,
                    2,
                    "Notches per second per millimetre past the deadzone.",
                ),
                "mode",
                "joystick",
            ),
            b(
                "latch",
                "Click to latch",
                false,
                "Click to start and click to stop, rather than holding throughout.",
            ),
            not_when(
                b(
                "freeze_cursor",
                "Hold the cursor still",
                true,
                "On, a swipe: the cursor stays where it is, as a finger on glass has none to \
                 move. Off, a hand tool: the page follows the pointer so the point under it \
                 stays under it. Joystick ignores this — it needs the cursor to steer by.",
                ),
                "mode",
                "wheel",
            ),
            not_when(
                b(
                    "momentum",
                    "Momentum",
                    false,
                    "Keep scrolling after a flick, decaying — a long page then feels like a \
                     surface with weight rather than a crank.",
                ),
                "mode",
                "joystick",
            ),
            not_when(
                f(
                    "momentum_decay_ms",
                    "Momentum decay",
                    "ms",
                    350.0,
                    100.0,
                    1500.0,
                    0,
                    "How long a flick takes to fade to about a third of its release speed.",
                ),
                "mode",
                "joystick",
            ),
            when_chord(
                b(
                    "full_release_stops_momentum",
                    "Full release stops momentum",
                    false,
                    "With a chord, letting go of one part leaves the page coasting and letting \
                     go of the rest stops it dead — a brake on the same binding, with no \
                     second one to find.",
                ),
                "button",
            ),
        ],
    },
    StageSpec {
        kind: "pressure",
        label: "Pressure",
        help: "Synthesises pen pressure. Terms multiply, so disabling one leaves the others \
               intact: p = envelope(time) × speed(velocity).",
        pinned_first: false,
        pinned_last: true,
        params: &[
            b(
                "envelope_enabled",
                "Time envelope",
                true,
                "Ramps pressure in and out over a stroke. This is most of what reads as a \
                 hand-drawn taper.",
            ),
            when(
                f(
                    "attack_ms",
                    "Attack",
                    "ms",
                    60.0,
                    0.0,
                    300.0,
                    0,
                    "Every real stroke is 93ms or longer, so 60 means they all reach full \
                     pressure.",
                ),
                "envelope_enabled",
                "true",
            ),
            when(
                f("release_ms", "Release", "ms", 60.0, 0.0, 300.0, 0, ""),
                "envelope_enabled",
                "true",
            ),
            b(
                "speed_enabled",
                "Speed term",
                true,
                "Fast strokes thin, slow deliberate ones stay heavy.",
            ),
            when(
                f(
                    "v_max_mm_s",
                    "Speed for minimum pressure",
                    "mm/s",
                    50.0,
                    20.0,
                    300.0,
                    0,
                    "Measured: careful drawing peaks at 20–30mm/s, fast gesturing at 73–124. \
                     50 suits careful work, 150 suits gesture.",
                ),
                "speed_enabled",
                "true",
            ),
            when(
                f("gamma", "Speed curve", "", 2.0, 0.5, 4.0, 2, ""),
                "speed_enabled",
                "true",
            ),
            when(
                c(
                    "source",
                    "Velocity from",
                    "cursor",
                    &["cursor", "output"],
                    "Cursor is the hand's own speed; output is the filtered path's. Measuring \
                     the cursor avoids a blob at every direction change.",
                ),
                "speed_enabled",
                "true",
            ),
            when(
                f(
                    "velocity_smoothing_ms",
                    "Velocity smoothing",
                    "ms",
                    40.0,
                    0.0,
                    200.0,
                    0,
                    "Required: raw per-event velocity is quantisation noise and makes pressure \
                     gritty. 0 is allowed and has a distinctive look.",
                ),
                "speed_enabled",
                "true",
            ),
            c(
                "stall_behaviour",
                "When the hand stops",
                "hold",
                &["hold", "decay"],
                "Hold keeps the last pressure; decay lets it fall away.",
            ),
            f(
                "stall_threshold_mm",
                "Stall threshold",
                "mm",
                0.05,
                0.0,
                1.0,
                3,
                "Movement below this counts as stopped.",
            ),
            f("stall_timeout_ms", "Stall timeout", "ms", 250.0, 0.0, 2000.0, 0, ""),
            f(
                "min_pressure",
                "Minimum pressure",
                "",
                0.05,
                0.0,
                0.5,
                3,
                "Floor below which pressure never falls. 0 is permitted; it can make stroke \
                 ends vanish entirely.",
            ),
        ],
    },
];

pub fn stage(kind: &str) -> Option<&'static StageSpec> {
    STAGES.iter().find(|s| s.kind == kind)
}

pub fn param(kind: &str, key: &str) -> Option<&'static ParamSpec> {
    stage(kind)?.params.iter().find(|p| p.key == key)
}

/// A new stage block carrying its defaults, ready to append to a preset file.
///
/// Written with every parameter present rather than relying on the assembler's defaults, so
/// the file the user ends up with says what it does — which is the whole tinkerer contract,
/// and the thing that makes a preset shareable.
pub fn new_stage_toml(kind: &str) -> Option<String> {
    let spec = stage(kind)?;
    let mut out = format!("\n[[stage]]\ntype = \"{}\"\n", spec.kind);
    for p in spec.params {
        let line = match p.kind {
            ParamKind::Float { default, decimals, .. } => {
                if decimals == 0 {
                    format!("{} = {}\n", p.key, default.round() as i64)
                } else {
                    format!("{} = {}\n", p.key, default)
                }
            }
            ParamKind::Bool { default } => format!("{} = {default}\n", p.key),
            ParamKind::Choice { default, .. } => format!("{} = \"{default}\"\n", p.key),
            // No sensible default: a binding nobody chose would engage something at random.
            ParamKind::Binding => format!("# {} = \"BTN_SIDE\"\n", p.key),
        };
        out.push_str(&line);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Preset;

    fn assemble_stage(kind: &str) -> crate::assemble::Assembly {
        let body = format!("schema = 1\n{}", new_stage_toml(kind).unwrap());
        let preset: Preset = toml::from_str(&body).expect("catalog produced invalid TOML");
        crate::assemble::assemble(&preset, "catalog-test", None)
    }

    #[test]
    fn every_catalogued_stage_assembles_without_a_single_complaint() {
        // The check that keeps this file honest. A parameter listed here but not read by the
        // assembler shows up as "unknown parameter", which is the editor writing keys nothing
        // consumes — worse than not offering them.
        for spec in STAGES {
            let a = assemble_stage(spec.kind);
            assert!(a.warnings.is_empty(), "{}: {:?}", spec.kind, a.warnings);
            assert!(
                a.pipeline.stage_names().any(|n| n == spec.kind),
                "{} did not build",
                spec.kind
            );
        }
    }

    #[test]
    fn the_catalog_covers_every_stage_the_assembler_knows() {
        // The other direction: a stage the assembler builds but the catalog omits would be
        // invisible in the editor and unaddable.
        for kind in [
            "normalize", "rotate", "deadzone", "sensitivity", "smooth", "stabilize", "average",
            "snap", "scroll", "pressure",
        ] {
            assert!(stage(kind).is_some(), "{kind} is missing from the catalog");
        }
    }

    #[test]
    fn a_choice_defaults_to_one_of_its_own_options() {
        for spec in STAGES {
            for p in spec.params {
                if let ParamKind::Choice { default, options } = p.kind {
                    assert!(
                        options.contains(&default),
                        "{}.{} defaults to {default}, which is not an option",
                        spec.kind,
                        p.key
                    );
                }
            }
        }
    }

    #[test]
    fn a_chord_dependency_names_a_binding_on_the_same_stage() {
        // Pointing at a parameter that is not a binding would hide a control on a condition
        // that can never be true.
        for spec in STAGES {
            for p in spec.params {
                let Some(on) = p.needs_chord else { continue };
                let target = spec.params.iter().find(|q| q.key == on);
                assert!(
                    target.is_some_and(|q| matches!(q.kind, ParamKind::Binding)),
                    "{}.{} depends on '{on}' being a chord, which is not a binding",
                    spec.kind,
                    p.key
                );
            }
        }
    }

    #[test]
    fn a_dependency_names_a_parameter_that_exists_on_the_same_stage() {
        for spec in STAGES {
            for p in spec.params {
                if let Some((on, _)) = p.depends_on {
                    assert!(
                        spec.params.iter().any(|q| q.key == on),
                        "{}.{} depends on {on}, which the stage does not have",
                        spec.kind,
                        p.key
                    );
                }
            }
        }
    }

    #[test]
    fn a_binding_is_commented_out_rather_than_guessed() {
        // A binding written with a made-up default would engage the gesture on a button the
        // user never chose.
        let toml = new_stage_toml("scroll").unwrap();
        assert!(toml.contains("# button ="), "{toml}");
    }

    #[test]
    fn only_the_two_documented_stages_are_pinned() {
        let first: Vec<&str> = STAGES.iter().filter(|s| s.pinned_first).map(|s| s.kind).collect();
        let last: Vec<&str> = STAGES.iter().filter(|s| s.pinned_last).map(|s| s.kind).collect();
        assert_eq!(first, ["normalize"]);
        assert_eq!(last, ["pressure"]);
    }
}
