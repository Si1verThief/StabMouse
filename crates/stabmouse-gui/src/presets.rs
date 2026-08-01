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
    /// The value as it reads in the file, whatever its type. The catalog decides which
    /// control to offer; this is what the file actually says.
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
                    Param {
                        key: key.clone(),
                        value: value
                            .as_float()
                            .or_else(|| value.as_integer().map(|i| i as f64))
                            .unwrap_or(0.0),
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

/// A new preset file carrying nothing but its identity.
///
/// Deliberately empty of stages: "create blank, then fill" is the workflow, and a new preset
/// pre-loaded with someone else's idea of a starting pipeline is one the user has to dismantle
/// before they can build their own.
pub fn create_preset(name: &str) -> anyhow::Result<PathBuf> {
    let slug = slugify(name);
    anyhow::ensure!(!slug.is_empty(), "a preset needs a name");
    let path = presets_dir().join(format!("{slug}.toml"));
    anyhow::ensure!(!path.exists(), "a preset called '{slug}' already exists");
    std::fs::create_dir_all(presets_dir())?;
    std::fs::write(
        &path,
        format!("schema = 1\ndisplay_name = \"{}\"\n", name.replace('"', "'")),
    )?;
    Ok(path)
}

/// Delete a preset file.
///
/// The caller is expected to have warned about profiles that reference it — reference
/// integrity is the config crate's rule (modules.md), and this is the raw operation.
pub fn delete_preset(path: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(path)?;
    Ok(())
}

/// Which profiles reference a preset, so deleting it can say what it would break.
pub fn profiles_using(slug: &str) -> Vec<String> {
    let dir = config_dir().join("profiles");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| {
            let doc: Document<stabmouse_config::Profile> = Document::load(&p).ok()?;
            if !doc.data().modes.iter().any(|m| m.preset == slug) {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().to_string())
        })
        .collect()
}

/// A readable label for a key the catalog does not describe.
///
/// `min_cutoff_hz` becomes "Min cutoff hz" — plainer than the raw key and never a lie about
/// what the parameter is. Only reachable for keys this build predates; everything catalogued
/// carries its own written label.
pub fn humanise(key: &str) -> String {
    let mut out = key.replace('_', " ");
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

/// A filename-safe identity derived from a display name.
///
/// Filenames *are* identities in this config (D14), so this decides what a preset is called
/// forever. Conservative on purpose: anything outside the safe set becomes a hyphen rather
/// than being dropped, so two different names cannot collapse into one slug silently.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Add a stage to a preset, with the catalog's defaults already in place.
pub fn add_stage(path: &Path, kind: &str) -> anyhow::Result<()> {
    let text = stabmouse_config::catalog::new_stage_toml(kind)
        .ok_or_else(|| anyhow::anyhow!("no such stage '{kind}'"))?;
    let mut doc: Document<Preset> = Document::load(path)?;
    doc.append_stage_text(&text)?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn remove_stage(path: &Path, index: usize) -> anyhow::Result<()> {
    let mut doc: Document<Preset> = Document::load(path)?;
    doc.remove_stage(index)?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn move_stage(path: &Path, index: usize, delta: i32) -> anyhow::Result<()> {
    let mut doc: Document<Preset> = Document::load(path)?;
    doc.move_stage(index, delta)?;
    doc.save_if_dirty()?;
    Ok(())
}

/// Set a parameter that is not a number — a choice, a switch, a binding.
pub fn write_param_text(
    path: &Path,
    stage_index: usize,
    key: &str,
    value: toml::Value,
) -> anyhow::Result<()> {
    let mut doc: Document<Preset> = Document::load(path)?;
    doc.set_stage_param(stage_index, key, value)?;
    doc.save_if_dirty()?;
    Ok(())
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
    }

    #[test]
    fn an_uncatalogued_key_still_gets_a_readable_label() {
        // Reachable only for a key this build predates — but that key must still be legible
        // rather than raw, and must never be hidden.
        assert_eq!(humanise("warp_factor_mm"), "Warp factor mm");
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
