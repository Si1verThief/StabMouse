//! Turning a preset on disk into a runnable pipeline.
//!
//! The seam between config and core. Config carries stage parameters as opaque TOML so
//! it never has to know every stage's fields; this module is the one place that does, and
//! therefore the one place that can report an unknown stage or a misspelled parameter.
//!
//! **Nothing here is fatal.** A typo in one parameter must not cost the user their mouse,
//! so unknown stages and unknown keys become warnings and the rest of the pipeline is
//! built. The daemon surfaces the warnings; it does not refuse to run.

use crate::cascade::DeviceView;
use crate::schema::{Preset, StageEntry};
use stabmouse_core::stages::{
    Average, Curve, Deadzone, Normalize, Pressure, Rotate, Sensitivity, Smooth, SpeedSource,
    Weighting,
    StallBehaviour, Stabilize,
};
use stabmouse_core::{Pipeline, Stage};
use std::collections::BTreeSet;

/// Stages that are specified but not yet implemented in core.
///
/// Listed explicitly so a preset referencing one gets an honest "not implemented yet"
/// rather than a silent omission that leaves the user wondering why nothing changed.
const PLANNED: &[&str] = &["snap", "scroll"];

pub struct Assembly {
    pub pipeline: Pipeline,
    pub warnings: Vec<String>,
}

/// Build a pipeline from a preset, applying any device overrides.
///
/// `slug` is the preset's own slug, needed because override keys are
/// `<preset>.<stage>.<param>`.
pub fn assemble(preset: &Preset, slug: &str, view: Option<&DeviceView<'_>>) -> Assembly {
    let mut warnings = Vec::new();

    // Enforce the two pins from docs/stages.md rather than trusting file order: a
    // `normalize` that is not first leaves every downstream unit wrong, and a `pressure`
    // that is not last reads unsettled velocity. Reordering with a warning beats both
    // refusing to load and silently producing nonsense.
    let mut ordered: Vec<&StageEntry> = preset.stages.iter().collect();
    let was = ordered.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>();
    ordered.sort_by_key(|s| match s.kind.as_str() {
        "normalize" => 0,
        "pressure" => 2,
        _ => 1,
    });
    let now = ordered.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>();
    if was != now {
        warnings.push(format!(
            "preset '{slug}': reordered stages to honour the pinned positions \
             (normalize first, pressure last)"
        ));
    }

    let mut stages: Vec<Box<dyn Stage>> = Vec::new();

    for entry in ordered {
        // The override key uses the instance id when present, so two instances of one
        // stage kind can be targeted separately.
        let stage_key = entry.id.as_deref().unwrap_or(&entry.kind);
        let mut p = Params::new(preset_params(entry, slug, stage_key, view), slug, stage_key);

        let built: Option<Box<dyn Stage>> = match entry.kind.as_str() {
            "normalize" => Some(Box::new(Normalize::new(p.f64("dpi", 1000.0)))),
            "rotate" => Some(Box::new(Rotate::new(p.f64("angle_deg", 0.0)))),
            "deadzone" => Some(Box::new(Deadzone::new(p.f64("threshold_mm", 0.0)))),
            "sensitivity" => Some(Box::new(build_sensitivity(&mut p))),
            "smooth" => {
                let d = Smooth::default();
                Some(Box::new(Smooth::new(
                    p.f64("min_cutoff_hz", d.min_cutoff_hz),
                    p.f64("beta", d.beta),
                    p.f64("d_cutoff_hz", d.d_cutoff_hz),
                )))
            }
            "stabilize" => Some(Box::new(Stabilize::new(
                p.f64("radius_mm", 0.4),
                p.f64("catch_up", 0.35),
            ))),
            "average" => Some(Box::new(build_average(&mut p))),
            "pressure" => Some(Box::new(build_pressure(&mut p))),
            other => {
                if PLANNED.contains(&other) {
                    warnings.push(format!(
                        "preset '{slug}': stage '{other}' is specified but not implemented yet; skipped"
                    ));
                } else {
                    warnings.push(format!("preset '{slug}': unknown stage '{other}'; skipped"));
                }
                None
            }
        };

        warnings.extend(p.finish());

        if let Some(mut stage) = built {
            stage.set_enabled(entry.enabled);
            stages.push(stage);
        }
    }

    Assembly {
        pipeline: Pipeline::new(stages),
        warnings,
    }
}

/// A stage's parameters with device overrides applied on top.
fn preset_params(
    entry: &StageEntry,
    slug: &str,
    stage_key: &str,
    view: Option<&DeviceView<'_>>,
) -> crate::schema::Params {
    let mut params = entry.params.clone();
    if let Some(view) = view {
        for name in params.keys().cloned().collect::<Vec<_>>() {
            if let Some(r) = view.resolve(&format!("{slug}.{stage_key}.{name}")) {
                params.insert(name, r.value);
            }
        }
        // An override may also introduce a parameter the preset never mentions.
        for (key, r) in view.effective_overrides() {
            let want = format!("{slug}.{stage_key}.");
            if let Some(name) = key.strip_prefix(&want) {
                params.insert(name.to_string(), r.value);
            }
        }
    }
    params
}

fn build_average(p: &mut Params) -> Average {
    // Zero is the identity and therefore the safe default: a stage named in a preset without
    // parameters should do nothing visible rather than silently add lag.
    let window_ms = p.f64("window_ms", 0.0);
    // Exponential by measurement, not convention: it removed the most wobble per millimetre of
    // lag at every window tested. See the table in stages.md.
    let weighting = match p.str("weighting", "exponential").as_str() {
        "exponential" => Weighting::Exponential,
        "linear" => Weighting::Linear,
        "gaussian" => Weighting::Gaussian,
        other => {
            p.warn(format!("unknown weighting '{other}'; using exponential"));
            Weighting::Exponential
        }
    };
    Average::new(window_ms, weighting)
}

fn build_sensitivity(p: &mut Params) -> Sensitivity {
    let mut s = Sensitivity::flat(p.f64("multiplier", 1.0));
    s.y_ratio = p.f64("y_ratio", 1.0);
    s.max_multiplier = p.opt_f64("max_multiplier");

    // The curve is nested under this stage rather than being its own: a flat multiplier
    // is the common case and gets the plain name, so that the users who most need it are
    // not deterred by a heading they do not understand. See vocabulary.md.
    match p.str("curve_type", "flat").as_str() {
        "flat" => {}
        "power" => {
            s.curve = Curve::Power {
                reference_mm_s: p.f64("curve_reference_mm_s", 50.0),
                exponent: p.f64("curve_exponent", 1.0),
                min: p.f64("curve_min", 0.5),
                max: p.f64("curve_max", 3.0),
            };
        }
        other => p.warn(format!("unknown curve type '{other}'; using flat")),
    }
    s
}

fn build_pressure(p: &mut Params) -> Pressure {
    let d = Pressure::default();
    let mut pr = Pressure::default();

    pr.attack_s = p.f64("attack_ms", d.attack_s * 1000.0) / 1000.0;
    pr.release_s = p.f64("release_ms", d.release_s * 1000.0) / 1000.0;
    pr.envelope_enabled = p.bool("envelope_enabled", d.envelope_enabled);

    pr.speed_enabled = p.bool("speed_enabled", d.speed_enabled);
    pr.v_max_mm_s = p.f64("v_max_mm_s", d.v_max_mm_s);
    pr.gamma = p.f64("gamma", d.gamma);
    pr.velocity_smoothing_s =
        p.f64("velocity_smoothing_ms", d.velocity_smoothing_s * 1000.0) / 1000.0;

    pr.source = match p.str("source", "cursor").as_str() {
        "output" => SpeedSource::Output,
        "cursor" => SpeedSource::Cursor,
        other => {
            p.warn(format!("unknown pressure source '{other}'; using cursor"));
            SpeedSource::Cursor
        }
    };

    pr.stall_threshold_mm = p.f64("stall_threshold_mm", d.stall_threshold_mm);
    pr.stall_timeout_s = p.f64("stall_timeout_ms", d.stall_timeout_s * 1000.0) / 1000.0;
    pr.stall_behaviour = match p.str("stall_behaviour", "hold").as_str() {
        "decay" => StallBehaviour::Decay,
        "hold" => StallBehaviour::Hold,
        other => {
            p.warn(format!("unknown stall behaviour '{other}'; using hold"));
            StallBehaviour::Hold
        }
    };

    pr.min_pressure = p.f64("min_pressure", d.min_pressure);
    if pr.min_pressure == 0.0 {
        // Permitted but warned, never clamped: many applications read zero pressure as
        // pen-up and split one stroke into two. Someone may want that.
        p.warn(
            "min_pressure is 0: many applications treat zero pressure as pen-up, which \
             will break a stroke in two"
                .to_string(),
        );
    }
    pr
}

/// Reads parameters, tracking which were consumed so the leftovers can be reported.
struct Params {
    values: crate::schema::Params,
    used: BTreeSet<String>,
    warnings: Vec<String>,
    context: String,
}

impl Params {
    fn new(values: crate::schema::Params, slug: &str, stage: &str) -> Self {
        Self {
            values,
            used: BTreeSet::new(),
            warnings: Vec::new(),
            context: format!("preset '{slug}' stage '{stage}'"),
        }
    }

    fn warn(&mut self, message: String) {
        self.warnings.push(format!("{}: {message}", self.context));
    }

    fn f64(&mut self, key: &str, default: f64) -> f64 {
        self.used.insert(key.to_string());
        match self.values.get(key) {
            None => default,
            // Integers are accepted where a float is wanted: `dpi = 1600` is what a
            // person writes, and rejecting it would be pedantry.
            Some(toml::Value::Integer(i)) => *i as f64,
            Some(toml::Value::Float(f)) => *f,
            Some(other) => {
                let kind = other.type_str();
                self.warn(format!("'{key}' should be a number, found {kind}; using default"));
                default
            }
        }
    }

    fn opt_f64(&mut self, key: &str) -> Option<f64> {
        self.used.insert(key.to_string());
        match self.values.get(key) {
            Some(toml::Value::Integer(i)) => Some(*i as f64),
            Some(toml::Value::Float(f)) => Some(*f),
            _ => None,
        }
    }

    fn bool(&mut self, key: &str, default: bool) -> bool {
        self.used.insert(key.to_string());
        match self.values.get(key) {
            Some(toml::Value::Boolean(b)) => *b,
            None => default,
            Some(other) => {
                let kind = other.type_str();
                self.warn(format!("'{key}' should be a boolean, found {kind}; using default"));
                default
            }
        }
    }

    fn str(&mut self, key: &str, default: &str) -> String {
        self.used.insert(key.to_string());
        match self.values.get(key) {
            Some(toml::Value::String(s)) => s.clone(),
            None => default.to_string(),
            Some(other) => {
                let kind = other.type_str();
                self.warn(format!("'{key}' should be a string, found {kind}; using default"));
                default.to_string()
            }
        }
    }

    /// Report any parameter the stage never asked for — almost always a typo.
    fn finish(mut self) -> Vec<String> {
        let unused: Vec<String> = self
            .values
            .keys()
            .filter(|k| !self.used.contains(*k))
            .cloned()
            .collect();
        for key in unused {
            self.warnings
                .push(format!("{}: unknown parameter '{key}'", self.context));
        }
        self.warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Root;
    use stabmouse_core::Sample;

    fn preset_from(text: &str) -> Preset {
        toml::from_str(text).unwrap()
    }

    fn run(a: &mut Assembly, n: usize, dx: f64) -> (f64, Option<f64>) {
        let mut total = 0.0;
        let mut pressure = None;
        for i in 0..n {
            let mut s = Sample::new(dx, 0.0, (i as u64 + 1) * 1_000, true);
            a.pipeline.process(&mut s);
            total += s.dx;
            pressure = s.pressure;
        }
        (total, pressure)
    }

    #[test]
    fn a_minimal_preset_assembles_and_runs() {
        let p = preset_from(
            r#"
            [[stage]]
            type = "normalize"
            dpi = 1600
            [[stage]]
            type = "stabilize"
            radius_mm = 0.0
            catch_up = 1.0
            "#,
        );
        let mut a = assemble(&p, "test", None);
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        assert_eq!(a.pipeline.len(), 2);

        // 1600 counts at 1600 dpi is one inch.
        let (total, _) = run(&mut a, 1600, 1.0);
        assert!((total - 25.4).abs() < 1e-6, "got {total}mm");
    }

    #[test]
    fn stage_pins_are_enforced_with_a_warning_not_a_refusal() {
        let p = preset_from(
            r#"
            [[stage]]
            type = "pressure"
            [[stage]]
            type = "stabilize"
            [[stage]]
            type = "normalize"
            "#,
        );
        let a = assemble(&p, "wonky", None);
        let order: Vec<&str> = a.pipeline.stage_names().collect();
        assert_eq!(order, vec!["normalize", "stabilize", "pressure"]);
        assert!(a.warnings.iter().any(|w| w.contains("reordered")));
    }

    #[test]
    fn a_misspelled_parameter_is_reported_rather_than_ignored() {
        let p = preset_from(
            r#"
            [[stage]]
            type = "stabilize"
            radius_mm = 0.5
            catchup = 0.3
            "#,
        );
        let a = assemble(&p, "typo", None);
        assert!(
            a.warnings.iter().any(|w| w.contains("catchup")),
            "{:?}",
            a.warnings
        );
        // ...and the rest of the stage still works.
        assert_eq!(a.pipeline.len(), 1);
    }

    #[test]
    fn an_unknown_stage_is_skipped_with_a_warning() {
        let p = preset_from(
            r#"
            [[stage]]
            type = "normalize"
            [[stage]]
            type = "teleport"
            "#,
        );
        let a = assemble(&p, "odd", None);
        assert_eq!(a.pipeline.len(), 1);
        assert!(a.warnings.iter().any(|w| w.contains("unknown stage 'teleport'")));
    }

    #[test]
    fn a_planned_but_unimplemented_stage_says_so_specifically() {
        let p = preset_from("[[stage]]\ntype = \"snap\"\n");
        let a = assemble(&p, "future", None);
        assert!(
            a.warnings
                .iter()
                .any(|w| w.contains("not implemented yet")),
            "{:?}",
            a.warnings
        );
    }

    #[test]
    fn integers_are_accepted_where_floats_are_expected() {
        // `dpi = 1600` is what a person writes.
        let p = preset_from("[[stage]]\ntype = \"normalize\"\ndpi = 1600\n");
        let a = assemble(&p, "ints", None);
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    }

    #[test]
    fn a_wrongly_typed_parameter_warns_and_falls_back() {
        let p = preset_from("[[stage]]\ntype = \"normalize\"\ndpi = \"lots\"\n");
        let a = assemble(&p, "badtype", None);
        assert!(
            a.warnings.iter().any(|w| w.contains("should be a number")),
            "{:?}",
            a.warnings
        );
    }

    #[test]
    fn device_overrides_reach_the_built_stage() {
        let root: Root = toml::from_str(
            r#"
            [[device]]
            match = { serial = "A1" }
            managed = true
            overrides = { "inking.stabilize.radius_mm" = 0.0, "inking.stabilize.catch_up" = 1.0 }
            "#,
        )
        .unwrap();
        let id = crate::schema::Identity {
            serial: Some("A1".into()),
            ..Default::default()
        };
        let view = root.view_for(&id);

        let p = preset_from(
            r#"
            [[stage]]
            type = "stabilize"
            radius_mm = 9.0
            catch_up = 0.1
            "#,
        );

        // Overridden to a zero-radius leash, so the stage becomes pass-through.
        let mut a = assemble(&p, "inking", Some(&view));
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        let (total, _) = run(&mut a, 100, 1.0);
        assert!(
            (total - 100.0).abs() < 1e-9,
            "override should have zeroed the radius; got {total}"
        );
    }

    #[test]
    fn an_override_can_introduce_a_parameter_the_preset_omits() {
        let root: Root = toml::from_str(
            r#"
            [defaults]
            overrides = { "inking.normalize.dpi" = 800 }
            "#,
        )
        .unwrap();
        let view = root.view_for(&crate::schema::Identity::default());
        let p = preset_from("[[stage]]\ntype = \"normalize\"\n");

        let mut a = assemble(&p, "inking", Some(&view));
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        // 800 counts at 800 dpi is one inch.
        let (total, _) = run(&mut a, 800, 1.0);
        assert!((total - 25.4).abs() < 1e-6, "got {total}mm");
    }

    #[test]
    fn min_pressure_of_zero_is_allowed_but_warned() {
        let p = preset_from("[[stage]]\ntype = \"pressure\"\nmin_pressure = 0.0\n");
        let a = assemble(&p, "risky", None);
        assert!(
            a.warnings.iter().any(|w| w.contains("pen-up")),
            "{:?}",
            a.warnings
        );
    }

    #[test]
    fn a_disabled_stage_is_present_but_inert() {
        let p = preset_from(
            r#"
            [[stage]]
            type = "stabilize"
            enabled = false
            radius_mm = 9.0
            catch_up = 0.1
            "#,
        );
        let mut a = assemble(&p, "off", None);
        assert_eq!(a.pipeline.len(), 1, "disabled stages stay in the pipeline");
        let (total, _) = run(&mut a, 100, 1.0);
        assert!(
            (total - 100.0).abs() < 1e-9,
            "a disabled stage must not transform anything; got {total}"
        );
    }

    #[test]
    fn pressure_defaults_come_from_core_not_from_here() {
        let p = preset_from("[[stage]]\ntype = \"pressure\"\n");
        let mut a = assemble(&p, "d", None);
        // Slow motion, so the measured v_max/gamma leave pressure high.
        let (_, pressure) = run(&mut a, 300, 0.0);
        let pressure = pressure.expect("pressure stage should set it");
        assert!(pressure > 0.9, "got {pressure}");
    }
}
