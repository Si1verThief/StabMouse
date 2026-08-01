//! What each stage parameter is called, what it means, and where its slider should stop.
//!
//! # Why this lives in the frontend
//!
//! These are presentation facts, not schema facts. `stabmouse-config` decides what a preset
//! may *contain*; this decides how to *show* it — a label a person reads, a unit suffix, and a
//! range a slider spans. The daemon needs none of it, and the core must not depend on it.
//!
//! # Sliders suggest, fields decide
//!
//! `soft_min`/`soft_max` are the **recommended** range from docs/stages.md, not a clamp. The
//! numeric field accepts anything; the slider simply pins to its end and the field shows the
//! truth (gui.md). This project has produced two cases already where the value outside the
//! reasonable range was the one somebody wanted — `velocity_smoothing_ms = 0` has a
//! distinctive look, and `min_pressure = 0` is permitted with a warning rather than refused.
//!
//! # An unlisted parameter is still editable
//!
//! Anything absent from this table gets a generic entry rather than being hidden. A preset may
//! legitimately carry a key this build has never heard of — a newer schema, or a hand-written
//! experiment — and silently omitting it from the editor would be the one behaviour the
//! format-preserving config machinery exists to prevent.

/// How to present one parameter.
pub struct Meta {
    pub label: &'static str,
    pub unit: &'static str,
    pub soft_min: f64,
    pub soft_max: f64,
    /// Field step for the keyboard, and the rounding the field displays.
    pub decimals: i32,
    pub help: &'static str,
}

/// Presentation for `stage.param`, or a generic entry when unknown.
pub fn meta(stage: &str, key: &str) -> Meta {
    match (stage, key) {
        ("normalize", "dpi") => Meta {
            label: "Sensor resolution",
            unit: "dpi",
            soft_min: 400.0,
            soft_max: 8000.0,
            decimals: 0,
            help: "Your mouse's true DPI. Everything downstream is in millimetres, so a wrong \
                   value silently mis-scales every other setting.",
        },
        ("rotate", "angle_deg") => Meta {
            label: "Rotation",
            unit: "°",
            soft_min: -45.0,
            soft_max: 45.0,
            decimals: 1,
            help: "Turns the whole motion vector. For holding the mouse off-square, or working \
                   at an angle.",
        },
        ("deadzone", "threshold_mm") => Meta {
            label: "Threshold",
            unit: "mm",
            soft_min: 0.0,
            soft_max: 1.0,
            decimals: 3,
            help: "Movement below this is discarded. 0 by default — the sensor essentially \
                   never reverses spuriously at ordinary resolutions.",
        },
        ("sensitivity", "multiplier") => Meta {
            label: "Sensitivity",
            unit: "×",
            soft_min: 0.1,
            soft_max: 4.0,
            decimals: 2,
            help: "A flat multiplier on all movement.",
        },
        ("sensitivity", "y_ratio") => Meta {
            label: "Vertical ratio",
            unit: "×",
            soft_min: 0.5,
            soft_max: 2.0,
            decimals: 2,
            help: "Vertical sensitivity relative to horizontal. 1.0 keeps them equal.",
        },
        ("smooth", "min_cutoff_hz") => Meta {
            label: "Cutoff",
            unit: "Hz",
            soft_min: 1.0,
            soft_max: 20.0,
            decimals: 2,
            help: "Lower smooths more. Measured: 5 is the general default at 0.30mm of lag, \
                   2 is the tremor setting at 0.44mm.",
        },
        ("smooth", "beta") => Meta {
            label: "Speed response",
            unit: "",
            soft_min: 0.0,
            soft_max: 0.3,
            decimals: 3,
            help: "How much fast movement loosens the smoothing. Must be 0.05–0.2 in these \
                   units, not the ~0.007 seen in one-euro literature.",
        },
        ("smooth", "d_cutoff_hz") => Meta {
            label: "Speed smoothing",
            unit: "Hz",
            soft_min: 0.1,
            soft_max: 5.0,
            decimals: 2,
            help: "Smoothing applied to the speed estimate that drives the cutoff.",
        },
        ("stabilize", "radius_mm") => Meta {
            label: "Radius",
            unit: "mm",
            soft_min: 0.0,
            soft_max: 4.0,
            decimals: 2,
            help: "Hand movement the cursor lags behind by. Measured: 0.2 removes wobble with \
                   all detail intact, 1.0 begins rounding corners, 4.0 destroys a drawing.",
        },
        ("stabilize", "catch_up") => Meta {
            label: "Catch-up",
            unit: "",
            soft_min: 0.05,
            soft_max: 1.0,
            decimals: 2,
            help: "How quickly the anchor closes the gap once it starts moving.",
        },
        ("average", "window_ms") => Meta {
            label: "Window",
            unit: "ms",
            soft_min: 0.0,
            soft_max: 150.0,
            decimals: 0,
            help: "Measured: 50ms removes a fifth of path wobble for 0.057mm of lag. Past \
                   ~100ms each extra millisecond buys less and costs the same.",
        },
        ("snap", "divisions") => Meta {
            label: "Divisions",
            unit: "",
            soft_min: 2.0,
            soft_max: 16.0,
            decimals: 0,
            help: "4 locks to the axes, 8 adds the diagonals.",
        },
        ("snap", "tolerance_deg") => Meta {
            label: "Tolerance",
            unit: "°",
            soft_min: 1.0,
            soft_max: 90.0,
            decimals: 1,
            help: "How near an allowed direction the hand must be for the snap to act. Half a \
                   division makes it a lock; narrower makes it a magnet.",
        },
        ("snap", "strength") => Meta {
            label: "Strength",
            unit: "",
            soft_min: 0.0,
            soft_max: 1.0,
            decimals: 2,
            help: "1 pins to the constraint; below that it leans without taking over.",
        },
        ("scroll", "drag_mm_per_unit") => Meta {
            label: "Distance per notch",
            unit: "mm",
            soft_min: 1.0,
            soft_max: 20.0,
            decimals: 1,
            help: "Hand travel per scroll notch while dragging. Larger scrolls slower.",
        },
        ("scroll", "joystick_deadzone_mm") => Meta {
            label: "Deadzone",
            unit: "mm",
            soft_min: 0.0,
            soft_max: 10.0,
            decimals: 1,
            help: "Displacement before autoscroll starts, so a resting hand does not creep.",
        },
        ("scroll", "joystick_gain") => Meta {
            label: "Speed",
            unit: "",
            soft_min: 0.1,
            soft_max: 6.0,
            decimals: 2,
            help: "Notches per second per millimetre of displacement past the deadzone.",
        },
        ("pressure", "attack_ms") => Meta {
            label: "Attack",
            unit: "ms",
            soft_min: 0.0,
            soft_max: 300.0,
            decimals: 0,
            help: "How long pressure takes to ramp in. Every real stroke is 93ms or longer, so \
                   60 means they all reach full pressure.",
        },
        ("pressure", "release_ms") => Meta {
            label: "Release",
            unit: "ms",
            soft_min: 0.0,
            soft_max: 300.0,
            decimals: 0,
            help: "How long pressure takes to ramp out. This is most of what reads as a \
                   tapered stroke end.",
        },
        ("pressure", "v_max_mm_s") => Meta {
            label: "Speed for minimum pressure",
            unit: "mm/s",
            soft_min: 20.0,
            soft_max: 300.0,
            decimals: 0,
            help: "Measured: careful drawing peaks at 20–30mm/s, fast gesturing at 73–124. 50 \
                   suits careful work, 150 suits gesture.",
        },
        ("pressure", "gamma") => Meta {
            label: "Speed curve",
            unit: "",
            soft_min: 0.5,
            soft_max: 4.0,
            decimals: 2,
            help: "Shapes how sharply pressure falls off with speed.",
        },
        ("pressure", "velocity_smoothing_ms") => Meta {
            label: "Velocity smoothing",
            unit: "ms",
            soft_min: 0.0,
            soft_max: 200.0,
            decimals: 0,
            help: "Required: raw per-event velocity is quantisation noise and makes pressure \
                   gritty. 0 is allowed and has a distinctive look.",
        },
        ("pressure", "min_pressure") => Meta {
            label: "Minimum pressure",
            unit: "",
            soft_min: 0.0,
            soft_max: 0.5,
            decimals: 3,
            help: "Floor below which pressure never falls. 0 is permitted; it can make stroke \
                   ends vanish entirely.",
        },
        // Not a failure — a preset may carry a key this build predates, and hiding it would
        // lose exactly what the format-preserving config exists to protect.
        _ => Meta {
            label: "",
            unit: "",
            soft_min: 0.0,
            soft_max: 1.0,
            decimals: 3,
            help: "",
        },
    }
}

/// A readable label, falling back to the config key itself made presentable.
pub fn label_for(stage: &str, key: &str) -> String {
    let m = meta(stage, key);
    if !m.label.is_empty() {
        return m.label.to_string();
    }
    // `min_cutoff_hz` reads as "min cutoff hz" — plainer than the key, and never a lie about
    // what the parameter is.
    let mut out = key.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

/// A slider range wide enough to hold the value it has to display.
///
/// A preset carrying a value outside the recommended range would otherwise render with its
/// handle pinned and no way to drag it back — the range widens to include it instead, which
/// keeps "the field decides" true in both directions.
pub fn range_for(stage: &str, key: &str, value: f64) -> (f64, f64) {
    let m = meta(stage, key);
    let (mut lo, mut hi) = (m.soft_min, m.soft_max);
    if value.is_finite() {
        lo = lo.min(value);
        hi = hi.max(value);
    }
    if hi <= lo {
        hi = lo + 1.0;
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_parameter_carries_its_measured_guidance() {
        let m = meta("stabilize", "radius_mm");
        assert_eq!(m.unit, "mm");
        assert!(m.help.contains("0.2"), "the measured bracket should reach the user");
    }

    #[test]
    fn an_unknown_parameter_is_still_shown() {
        // A preset may carry a key this build predates. Hiding it would lose the very thing
        // the format-preserving config protects.
        assert_eq!(label_for("future", "warp_factor_mm"), "Warp factor mm");
    }

    #[test]
    fn the_slider_widens_to_hold_an_unreasonable_value() {
        // Sliders suggest, fields decide — but a handle pinned off the end with no way back
        // would make the suggestion a trap.
        let (lo, hi) = range_for("stabilize", "radius_mm", 12.0);
        assert!(hi >= 12.0, "the range must include the value it shows");
        assert!(lo <= 0.0);
    }

    #[test]
    fn a_range_is_never_empty() {
        let (lo, hi) = range_for("unknown", "thing", f64::NAN);
        assert!(hi > lo);
    }
}
