//! Named filter configurations to compare against each other.
//!
//! A stand-in for `stabmouse-config`, which does not exist yet. Deliberately flat and
//! small: the point is comparing a handful of candidates on identical input, not
//! expressing every possible pipeline.

use anyhow::{Context, Result};
use serde::Deserialize;
use stabmouse_core::stages::{
    Average, Normalize, Pressure, Sensitivity, SpeedSource, StallBehaviour, Smooth, Stabilize,
    Weighting,
};
use stabmouse_core::Pipeline;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct VariantFile {
    #[serde(default, rename = "variant")]
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Variant {
    pub name: String,

    #[serde(default = "one")]
    pub sens: f64,

    #[serde(default)]
    pub stab_radius_mm: f64,
    #[serde(default = "one")]
    pub stab_catch_up: f64,

    /// Zero is the identity, so an unset average changes nothing.
    #[serde(default)]
    pub average_window_ms: f64,
    /// `"linear"`, `"exponential"` or `"gaussian"`.
    #[serde(default = "linear")]
    pub average_weighting: String,

    /// Effectively transparent by default.
    #[serde(default = "thousand")]
    pub smooth_min_cutoff_hz: f64,
    #[serde(default)]
    pub smooth_beta: f64,
    #[serde(default = "one")]
    pub smooth_d_cutoff_hz: f64,

    #[serde(default = "sixty")]
    pub pressure_attack_ms: f64,
    #[serde(default = "four_hundred")]
    pub pressure_v_max_mm_s: f64,
    #[serde(default = "one")]
    pub pressure_gamma: f64,
    /// `"cursor"` or `"output"`.
    #[serde(default = "cursor")]
    pub pressure_source: String,
    #[serde(default = "forty")]
    pub pressure_velocity_smoothing_ms: f64,
    /// `"hold"` or `"decay"`.
    #[serde(default = "hold")]
    pub pressure_stall: String,
    #[serde(default = "min_pressure_default")]
    pub pressure_min: f64,
}

fn one() -> f64 {
    1.0
}
fn thousand() -> f64 {
    1000.0
}
fn sixty() -> f64 {
    60.0
}
fn forty() -> f64 {
    40.0
}
fn four_hundred() -> f64 {
    400.0
}
fn min_pressure_default() -> f64 {
    0.05
}
fn cursor() -> String {
    "cursor".into()
}
fn linear() -> String {
    "linear".into()
}
fn hold() -> String {
    "hold".into()
}

impl Variant {
    /// Build the pipeline this variant describes.
    ///
    /// Stage order follows the recommended default in stages.md, with `normalize`
    /// pinned first and `pressure` pinned last.
    pub fn build(&self, dpi: f64) -> Pipeline {
        let sens = Sensitivity::flat(self.sens);

        // Assigned rather than built with struct-update syntax: `Pressure` keeps its
        // accumulator state private, so `..Default::default()` cannot be used from
        // another crate. Only the tunables are public, which is the right split.
        let mut pressure = Pressure::default();
        pressure.attack_s = self.pressure_attack_ms / 1000.0;
        pressure.release_s = self.pressure_attack_ms / 1000.0;
        pressure.v_max_mm_s = self.pressure_v_max_mm_s;
        pressure.gamma = self.pressure_gamma;
        pressure.source = match self.pressure_source.as_str() {
            "output" => SpeedSource::Output,
            _ => SpeedSource::Cursor,
        };
        pressure.velocity_smoothing_s = self.pressure_velocity_smoothing_ms / 1000.0;
        pressure.stall_behaviour = match self.pressure_stall.as_str() {
            "decay" => StallBehaviour::Decay,
            _ => StallBehaviour::Hold,
        };
        pressure.min_pressure = self.pressure_min;

        // Position filters before `sensitivity`, per the default order in stages.md:
        // their parameters are then in true hand millimetres, which is what makes a
        // shared preset transfer between setups.
        let weighting = match self.average_weighting.as_str() {
            "exponential" => Weighting::Exponential,
            "gaussian" => Weighting::Gaussian,
            _ => Weighting::Linear,
        };

        Pipeline::new(vec![
            Box::new(Normalize::new(dpi)),
            Box::new(Smooth::new(
                self.smooth_min_cutoff_hz,
                self.smooth_beta,
                self.smooth_d_cutoff_hz,
            )),
            Box::new(Average::new(self.average_window_ms, weighting)),
            Box::new(Stabilize::new(self.stab_radius_mm, self.stab_catch_up)),
            Box::new(sens),
            Box::new(pressure),
        ])
    }
}

pub fn load(path: &Path) -> Result<Vec<Variant>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: VariantFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    anyhow::ensure!(
        !parsed.variants.is_empty(),
        "{} defines no [[variant]] entries",
        path.display()
    );
    Ok(parsed.variants)
}

/// Written on demand so there is always something to start from.
pub const EXAMPLE: &str = r#"# Candidate configurations, compared on identical recorded input.
# Every unset field falls back to a transparent or neutral value.

[[variant]]
name = "raw"

[[variant]]
name = "stabilised"
stab_radius_mm = 4.0
stab_catch_up = 0.35

[[variant]]
name = "heavy"
stab_radius_mm = 9.0
stab_catch_up = 0.15

# The open question from stages.md: which velocity source feels right.
[[variant]]
name = "output-velocity"
stab_radius_mm = 4.0
stab_catch_up = 0.35
pressure_source = "output"

[[variant]]
name = "long-smoothing"
stab_radius_mm = 4.0
stab_catch_up = 0.35
pressure_velocity_smoothing_ms = 150.0

[[variant]]
name = "decay-stall"
stab_radius_mm = 4.0
stab_catch_up = 0.35
pressure_stall = "decay"
"#;
