//! Reading and editing profile files.
//!
//! A profile is the list of mode slots the toggle cycles through, so this is where a preset
//! becomes something reachable. Same approach as presets: edit the file, let the daemon's own
//! watch notice, keep the user's comments (see [`crate::presets`]).

use crate::presets::{config_dir, slugify};
use stabmouse_config::{Document, Profile};
use std::path::{Path, PathBuf};

pub struct ModeSlot {
    pub name: String,
    pub output: String,
    pub preset: String,
}

pub struct ProfileFile {
    pub slug: String,
    pub display_name: String,
    pub path: PathBuf,
    pub default_mode: usize,
    pub modes: Vec<ModeSlot>,
}

/// The three transports a mode may ask for, in the order the editor should offer them.
pub const OUTPUTS: &[&str] = &["mouse", "tablet", "relative"];

fn profiles_dir() -> PathBuf {
    config_dir().join("profiles")
}

pub fn load_all() -> Vec<ProfileFile> {
    let Ok(entries) = std::fs::read_dir(profiles_dir()) else {
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

fn load_one(path: &Path) -> Option<ProfileFile> {
    let doc: Document<Profile> = Document::load(path).ok()?;
    let data = doc.data();
    let slug = path.file_stem()?.to_string_lossy().to_string();
    Some(ProfileFile {
        display_name: data.display_name.clone().unwrap_or_else(|| slug.clone()),
        slug,
        path: path.to_path_buf(),
        default_mode: data.default_mode,
        modes: data
            .modes
            .iter()
            .map(|m| ModeSlot {
                name: m.name.clone(),
                output: format!("{:?}", m.output).to_lowercase(),
                preset: m.preset.clone(),
            })
            .collect(),
    })
}

/// A new profile with no slots.
///
/// Empty rather than pre-populated, for the same reason a new preset is: the point of "create
/// blank" is that the user builds it, not that they edit someone else's starting point.
pub fn create(name: &str) -> anyhow::Result<PathBuf> {
    let slug = slugify(name);
    anyhow::ensure!(!slug.is_empty(), "a profile needs a name");
    let path = profiles_dir().join(format!("{slug}.toml"));
    anyhow::ensure!(!path.exists(), "a profile called '{slug}' already exists");
    std::fs::create_dir_all(profiles_dir())?;
    std::fs::write(
        &path,
        format!(
            "schema = 1\ndisplay_name = \"{}\"\ndefault_mode = 1\n",
            name.replace('"', "'")
        ),
    )?;
    Ok(path)
}

pub fn delete(path: &Path) -> anyhow::Result<()> {
    std::fs::remove_file(path)?;
    Ok(())
}

/// Append a mode slot pointing at a preset.
pub fn add_mode(path: &Path, name: &str, output: &str, preset: &str) -> anyhow::Result<()> {
    let mut doc: Document<Profile> = Document::load(path)?;
    let text = format!(
        "\n[[mode]]\nname = \"{}\"\noutput = \"{output}\"\npreset = \"{preset}\"\n",
        name.replace('"', "'")
    );
    doc.append_table_text("mode", &text)?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn remove_mode(path: &Path, index: usize) -> anyhow::Result<()> {
    let mut doc: Document<Profile> = Document::load(path)?;
    doc.remove_table("mode", index)?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn move_mode(path: &Path, index: usize, delta: i32) -> anyhow::Result<()> {
    let mut doc: Document<Profile> = Document::load(path)?;
    doc.move_table("mode", index, delta)?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn set_mode_field(
    path: &Path,
    index: usize,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    let mut doc: Document<Profile> = Document::load(path)?;
    doc.set_table_param("mode", index, field, toml::Value::String(value.to_string()))?;
    doc.save_if_dirty()?;
    Ok(())
}

pub fn set_default_mode(path: &Path, slot: usize) -> anyhow::Result<()> {
    let mut doc: Document<Profile> = Document::load(path)?;
    doc.set(&["default_mode"], toml::Value::Integer(slot.max(1) as i64))?;
    doc.save_if_dirty()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stabmouse-profile-test-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    const SAMPLE: &str = r#"schema = 1
display_name = "Test"
default_mode = 1

# The user's own note.
[[mode]]
name = "Mouse"
output = "mouse"
preset = "raw"

[[mode]]
name = "Draw"
output = "tablet"
preset = "inking"
"#;

    #[test]
    fn slots_load_with_their_output_and_preset() {
        let p = temp("load", SAMPLE);
        let f = load_one(&p).unwrap();
        assert_eq!(f.modes.len(), 2);
        assert_eq!(f.modes[1].output, "tablet");
        assert_eq!(f.modes[1].preset, "inking");
    }

    #[test]
    fn a_slot_can_be_added_and_removed() {
        let p = temp("add", SAMPLE);
        add_mode(&p, "Game", "relative", "raw").unwrap();
        let f = load_one(&p).unwrap();
        assert_eq!(f.modes.len(), 3);
        assert_eq!(f.modes[2].output, "relative");

        // Removing a *later* slot leaves the first one's comment where it was.
        remove_mode(&p, 2).unwrap();
        let f = load_one(&p).unwrap();
        assert_eq!(f.modes.len(), 2);
        assert!(std::fs::read_to_string(&p).unwrap().contains("# The user's own note."));
    }

    #[test]
    fn deleting_a_slot_takes_its_own_comment_with_it() {
        // A comment above a block is that block's, so it belongs to what is being removed.
        // Leaving it behind would orphan it above an unrelated slot, which is worse than
        // losing it — the user would read it as describing something it does not.
        let p = temp("comment", SAMPLE);
        remove_mode(&p, 0).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("# The user's own note."), "{text}");
        assert!(text.contains("display_name"), "the rest of the file survives: {text}");
    }

    #[test]
    fn slots_reorder() {
        let p = temp("move", SAMPLE);
        move_mode(&p, 1, -1).unwrap();
        let f = load_one(&p).unwrap();
        assert_eq!(f.modes[0].name, "Draw");
        assert_eq!(f.modes[0].preset, "inking", "the whole slot moved, not just its name");
    }

    #[test]
    fn a_slots_transport_can_be_changed() {
        let p = temp("field", SAMPLE);
        set_mode_field(&p, 0, "output", "relative").unwrap();
        assert_eq!(load_one(&p).unwrap().modes[0].output, "relative");
    }
}
