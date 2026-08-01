//! Reading and editing preset files from the frontend.
//!
//! # Why the GUI edits files rather than asking the daemon
//!
//! The daemon already watches the config directory and reloads within about 0.4s, so a write
//! here reaches the running pipeline with no new protocol, no round trip, and no second
//! implementation of what a preset means. It also means the GUI works with **no daemon
//! running** — editing presets is exactly the thing someone might do before starting one.
//!
//! The tinkerer contract holds in both directions: writes go through the format-preserving
//! document, so comments, ordering and whitespace survive being edited by the interface
//! (modules.md). Someone can hand-edit a preset, tune it here, and open it again to find their
//! own comments intact.

use stabmouse_config::{Document, Preset};
use std::path::{Path, PathBuf};

/// One editable parameter of one stage.
pub struct Param {
    pub key: String,
    pub value: f64,
    /// False for anything not expressible as a slider — strings, booleans, tables. Shown
    /// read-only rather than hidden, so the editor never implies a preset is simpler than it is.
    pub numeric: bool,
    pub text: String,
}

/// One stage of a preset, as the editor shows it.
pub struct StageView {
    pub kind: String,
    pub enabled: bool,
    pub params: Vec<Param>,
}

pub struct PresetFile {
    pub slug: String,
    pub display_name: String,
    pub path: PathBuf,
    pub stages: Vec<StageView>,
}

/// Where presets live, matching the daemon's own default.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("stabmouse")
}

fn presets_dir() -> PathBuf {
    config_dir().join("presets")
}

/// Every preset on disk, in a stable order.
///
/// Read straight from the directory rather than from the daemon's view: a preset that no
/// profile references is still editable, and one that fails to assemble is exactly the one
/// somebody needs to open.
pub fn load_all() -> Vec<PresetFile> {
    let dir = presets_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();

    paths.iter().filter_map(|p| load_one(p)).collect()
}

fn load_one(path: &Path) -> Option<PresetFile> {
    let doc: Document<Preset> = Document::load(path).ok()?;
    let preset = doc.data();
    let slug = path.file_stem()?.to_string_lossy().to_string();

    let stages = preset
        .stages
        .iter()
        .map(|entry| StageView {
            kind: entry.kind.clone(),
            enabled: entry.enabled,
            params: entry
                .params
                .iter()
                .map(|(key, value)| {
                    let numeric = value.as_float().is_some() || value.as_integer().is_some();
                    Param {
                        key: key.clone(),
                        value: value
                            .as_float()
                            .or_else(|| value.as_integer().map(|i| i as f64))
                            .unwrap_or(0.0),
                        numeric,
                        text: match value {
                            toml::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        },
                    }
                })
                .collect(),
        })
        .collect();

    Some(PresetFile {
        // The slug is the identity; a display name is decoration a preset need not carry.
        display_name: preset.display_name.clone().unwrap_or_else(|| slug.clone()),
        slug,
        path: path.to_path_buf(),
        stages,
    })
}

/// Write one parameter back, preserving everything around it.
///
/// Reloading the document per edit rather than holding one open: the file is small, and it
/// means an edit made in a text editor between two edits made here is not silently discarded.
/// The daemon's own reload has the same character — last writer wins, and neither side keeps a
/// stale copy to overwrite the other with.
pub fn write_param(path: &Path, stage_index: usize, key: &str, value: f64) -> anyhow::Result<()> {
    let mut doc: Document<Preset> = Document::load(path)?;

    // Integers stay integers. Writing `dpi = 1600.0` into a file that said `1600` is a
    // gratuitous diff, and the point of the format-preserving editor is that the file still
    // looks like the one the user wrote.
    let was_integer = doc
        .data()
        .stages
        .get(stage_index)
        .and_then(|s| s.params.get(key))
        .is_some_and(|v| v.is_integer());

    let value = if was_integer && value.is_finite() && value.abs() < 1e15 {
        toml::Value::Integer(value.round() as i64)
    } else {
        toml::Value::Float(value)
    };

    doc.set_stage_param(stage_index, key, value)?;
    doc.save_if_dirty()?;
    Ok(())
}

/// Enable or disable a stage without losing its tuning.
pub fn write_stage_enabled(path: &Path, stage_index: usize, enabled: bool) -> anyhow::Result<()> {
    let mut doc: Document<Preset> = Document::load(path)?;
    doc.set_stage_param(stage_index, "enabled", toml::Value::Boolean(enabled))?;
    doc.save_if_dirty()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stabmouse-gui-test-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preset.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    const SAMPLE: &str = r#"schema = 1
display_name = "Test"

# A comment the user wrote, which must survive being edited by the interface.
[[stage]]
type = "stabilize"
radius_mm = 0.4   # trailing note
catch_up = 0.35
"#;

    #[test]
    fn editing_a_parameter_keeps_the_users_comments() {
        // The tinkerer contract from modules.md, exercised through the path the GUI uses.
        let path = write_temp("comments", SAMPLE);
        write_param(&path, 0, "radius_mm", 1.25).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# A comment the user wrote"), "{after}");
        assert!(after.contains("# trailing note"), "{after}");
        assert!(after.contains("1.25"), "{after}");
    }

    #[test]
    fn loading_reports_the_stages_and_their_values() {
        let path = write_temp("load", SAMPLE);
        let loaded = load_one(&path).unwrap();
        assert_eq!(loaded.display_name, "Test");
        assert_eq!(loaded.stages.len(), 1);
        assert_eq!(loaded.stages[0].kind, "stabilize");
        let radius = loaded.stages[0]
            .params
            .iter()
            .find(|p| p.key == "radius_mm")
            .unwrap();
        assert!((radius.value - 0.4).abs() < 1e-9);
        assert!(radius.numeric);
    }

    #[test]
    fn a_non_numeric_parameter_is_reported_rather_than_dropped() {
        // Hiding what it cannot render would make the editor lie about the preset's contents.
        let path = write_temp("strings", "schema = 1\n[[stage]]\ntype = \"pressure\"\nsource = \"cursor\"\n");
        let loaded = load_one(&path).unwrap();
        let source = loaded.stages[0]
            .params
            .iter()
            .find(|p| p.key == "source")
            .unwrap();
        assert!(!source.numeric);
        assert_eq!(source.text, "cursor");
    }

    #[test]
    fn disabling_a_stage_leaves_its_tuning_in_the_file() {
        let path = write_temp("disable", SAMPLE);
        write_stage_enabled(&path, 0, false).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("radius_mm"), "tuning must survive a toggle: {after}");
        assert!(after.contains("false"), "{after}");
    }
}
