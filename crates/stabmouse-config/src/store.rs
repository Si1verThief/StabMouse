//! Loading the config directory, and keeping it honest.
//!
//! Layout per docs/config-schema.md:
//!
//! ```text
//! ~/.config/stabmouse/
//! ├── config.toml
//! ├── presets/<slug>.toml
//! └── profiles/<slug>.toml
//! ```
//!
//! **The filename is the identity** (D14) — `presets/inking.toml` *is* the preset
//! `inking`, with no internal name field that could disagree.
//!
//! **A bad file never takes the daemon down.** Problems are collected and returned
//! alongside a store that is still usable, because the alternative is a user with a
//! typo in one preset losing their mouse entirely.

use crate::edit::Document;
use crate::error::{Error, Result};
use crate::schema::{is_valid_slug, Preset, Profile, Root, StageEntry, CURRENT_SCHEMA};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The slug of the built-in fallback preset, which always exists.
pub const RAW: &str = "raw";

/// Non-fatal problems found while loading.
#[derive(Debug, Default)]
pub struct LoadReport {
    pub problems: Vec<Error>,
}

impl LoadReport {
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

pub struct Store {
    dir: PathBuf,
    pub root: Document<Root>,
    pub presets: BTreeMap<String, Document<Preset>>,
    pub profiles: BTreeMap<String, Document<Profile>>,
    /// Synthesised `raw`, used when the user has no `presets/raw.toml`.
    builtin_raw: Preset,
}

impl Store {
    /// Load a config directory. A missing directory is a first run, not an error.
    pub fn load(dir: impl Into<PathBuf>) -> Result<(Self, LoadReport)> {
        let dir = dir.into();
        let mut report = LoadReport::default();

        let root_path = dir.join("config.toml");
        let root = if root_path.exists() {
            match Document::<Root>::load(&root_path) {
                Ok(d) => d,
                Err(e) => {
                    // Fall back to defaults rather than refusing to start.
                    report.problems.push(e);
                    Document::from_str(&root_path, "schema = 1\n")?
                }
            }
        } else {
            Document::from_str(&root_path, "schema = 1\n")?
        };
        check_schema(&root_path, root.data().schema, &mut report);

        let presets = load_dir::<Preset>(&dir.join("presets"), &mut report, |p| p.schema);
        let profiles = load_dir::<Profile>(&dir.join("profiles"), &mut report, |p| p.schema);

        Ok((
            Self {
                dir,
                root,
                presets,
                profiles,
                builtin_raw: builtin_raw(),
            },
            report,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A preset by slug, falling back to the built-in `raw`.
    ///
    /// `raw` must always resolve: it is the fallback a mode drops to when its own preset
    /// is missing, so it cannot itself be missing.
    pub fn preset(&self, slug: &str) -> Option<&Preset> {
        match self.presets.get(slug) {
            Some(d) => Some(d.data()),
            None if slug == RAW => Some(&self.builtin_raw),
            None => None,
        }
    }

    pub fn profile(&self, slug: &str) -> Option<&Profile> {
        self.profiles.get(slug).map(|d| d.data())
    }

    pub fn preset_slugs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.presets.keys().cloned().collect();
        if !self.presets.contains_key(RAW) {
            v.push(RAW.to_string());
            v.sort();
        }
        v
    }

    pub fn profile_slugs(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Every mode whose preset does not exist.
    ///
    /// Deliberately returns problems rather than refusing to load. Per the schema spec a
    /// dangling reference makes that mode refuse to activate and fall back to raw
    /// passthrough — never a silent substitution, which would leave someone drawing with
    /// settings they did not choose and cannot see.
    pub fn dangling_references(&self) -> Vec<Error> {
        let mut out = Vec::new();
        for (slug, doc) in &self.profiles {
            for (i, mode) in doc.data().modes.iter().enumerate() {
                if self.preset(&mode.preset).is_none() {
                    out.push(Error::DanglingPreset {
                        profile: slug.clone(),
                        mode: i + 1,
                        preset: mode.preset.clone(),
                    });
                }
            }
        }
        out
    }

    /// Refuse to delete a preset that profiles still reference.
    pub fn referrers_of(&self, preset: &str) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|(_, d)| d.data().modes.iter().any(|m| m.preset == preset))
            .map(|(slug, _)| slug.clone())
            .collect()
    }

    /// Which profile to activate at startup.
    fn default_profile_slug(&self) -> Option<String> {
        self.root
            .data()
            .defaults
            .profile
            .clone()
            .filter(|p| self.profiles.contains_key(p))
            .or_else(|| self.profiles.keys().next().cloned())
    }

    pub fn startup_profile(&self) -> Option<&Profile> {
        self.default_profile_slug()
            .and_then(|s| self.profile(&s))
    }
}

fn load_dir<T>(
    dir: &Path,
    report: &mut LoadReport,
    schema_of: impl Fn(&T) -> u32,
) -> BTreeMap<String, Document<T>>
where
    T: serde::de::DeserializeOwned,
{
    let mut out = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // A missing subdirectory is a first run, not a fault.
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_valid_slug(stem) {
            report.problems.push(Error::BadSlug {
                what: "file",
                name: stem.to_string(),
            });
            continue;
        }
        match Document::<T>::load(&path) {
            Ok(doc) => {
                check_schema(&path, schema_of(doc.data()), report);
                out.insert(stem.to_string(), doc);
            }
            Err(e) => report.problems.push(e),
        }
    }
    out
}

/// Version gate.
///
/// A *newer* schema is refused rather than guessed at — silently ignoring fields we do
/// not understand would apply a config the user did not write. An *older* one is
/// migrated in memory; nothing is written back unless the user saves (D15).
fn check_schema(path: &Path, found: u32, report: &mut LoadReport) {
    if found > CURRENT_SCHEMA {
        report.problems.push(Error::SchemaTooNew {
            path: path.to_path_buf(),
            found,
            max: CURRENT_SCHEMA,
        });
    } else if found < 1 {
        report.problems.push(Error::NoMigration {
            path: path.to_path_buf(),
            from: found,
        });
    }
    // 1..=CURRENT needs no migration yet. When it does, the step chain goes here and
    // stays in memory.
}

/// The do-nothing preset.
///
/// Named `raw` rather than `flat`, because "flat" names a *kind of acceleration curve*
/// and would imply the signal is still being shaped. `raw` states that it is not — which
/// is the promise the gaming audience wants to verify.
fn builtin_raw() -> Preset {
    Preset {
        schema: CURRENT_SCHEMA,
        display_name: Some("Raw".to_string()),
        stages: vec![StageEntry {
            kind: "normalize".to_string(),
            id: None,
            enabled: true,
            params: Default::default(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!("stabmouse-store-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("presets")).unwrap();
            std::fs::create_dir_all(p.join("profiles")).unwrap();
            Self(p)
        }
        fn write(&self, rel: &str, body: &str) {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_directory_is_a_first_run_not_an_error() {
        let (store, report) = Store::load("/nonexistent/stabmouse-config-xyz").unwrap();
        assert!(report.is_clean());
        assert!(store.profiles.is_empty());
        // raw must still resolve, since it is the fallback.
        assert!(store.preset(RAW).is_some());
        assert_eq!(store.preset_slugs(), vec![RAW.to_string()]);
    }

    #[test]
    fn filenames_become_slugs() {
        let t = Tmp::new("slugs");
        t.write("presets/inking.toml", "schema = 1\n");
        t.write("presets/line-art.toml", "schema = 1\n");
        let (store, report) = Store::load(&t.0).unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);
        assert!(store.preset("inking").is_some());
        assert!(store.preset("line-art").is_some());
    }

    #[test]
    fn one_broken_preset_does_not_prevent_loading_the_rest() {
        let t = Tmp::new("broken");
        t.write("presets/good.toml", "schema = 1\n");
        t.write("presets/bad.toml", "this is = = not toml\n");
        let (store, report) = Store::load(&t.0).unwrap();

        assert!(store.preset("good").is_some());
        assert!(store.preset("bad").is_none());
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].to_string().contains("bad.toml"));
    }

    #[test]
    fn a_corrupt_config_toml_falls_back_to_defaults_rather_than_failing() {
        let t = Tmp::new("badroot");
        t.write("config.toml", "= = =\n");
        let (store, report) = Store::load(&t.0).unwrap();
        assert_eq!(report.problems.len(), 1);
        assert_eq!(store.root.data().schema, CURRENT_SCHEMA);
    }

    #[test]
    fn a_newer_schema_is_refused_not_guessed_at() {
        let t = Tmp::new("newer");
        t.write("presets/future.toml", "schema = 99\n");
        let (_store, report) = Store::load(&t.0).unwrap();
        assert!(
            report
                .problems
                .iter()
                .any(|p| matches!(p, Error::SchemaTooNew { found: 99, .. })),
            "{:?}",
            report.problems
        );
    }

    #[test]
    fn non_toml_files_and_bad_slugs_are_handled_distinctly() {
        let t = Tmp::new("junk");
        t.write("presets/notes.txt", "ignore me");
        t.write("presets/Bad Name.toml", "schema = 1\n");
        let (store, report) = Store::load(&t.0).unwrap();

        assert!(store.presets.is_empty());
        // The .txt is silently skipped; the bad slug is reported.
        assert_eq!(report.problems.len(), 1);
        assert!(matches!(report.problems[0], Error::BadSlug { .. }));
    }

    #[test]
    fn dangling_preset_references_are_reported_with_enough_detail_to_act_on() {
        let t = Tmp::new("dangling");
        t.write("presets/inking.toml", "schema = 1\n");
        t.write(
            "profiles/line-art.toml",
            r#"
            [[mode]]
            name = "Click"
            output = "mouse"
            preset = "raw"
            [[mode]]
            name = "Draw"
            output = "tablet"
            preset = "inking"
            [[mode]]
            name = "Broken"
            output = "tablet"
            preset = "does-not-exist"
            "#,
        );
        let (store, report) = Store::load(&t.0).unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);

        let dangling = store.dangling_references();
        assert_eq!(dangling.len(), 1);
        let msg = dangling[0].to_string();
        assert!(msg.contains("line-art") && msg.contains("does-not-exist") && msg.contains('3'));
    }

    #[test]
    fn raw_resolves_even_with_no_file_and_is_listed() {
        let t = Tmp::new("rawfallback");
        t.write("presets/inking.toml", "schema = 1\n");
        let (store, _) = Store::load(&t.0).unwrap();
        assert!(store.preset(RAW).is_some());
        assert!(store.preset_slugs().contains(&RAW.to_string()));
    }

    #[test]
    fn a_user_supplied_raw_shadows_the_builtin() {
        let t = Tmp::new("rawshadow");
        t.write(
            "presets/raw.toml",
            "schema = 1\ndisplay_name = \"My Raw\"\n",
        );
        let (store, _) = Store::load(&t.0).unwrap();
        assert_eq!(
            store.preset(RAW).unwrap().display_name.as_deref(),
            Some("My Raw")
        );
    }

    #[test]
    fn deleting_a_referenced_preset_can_be_refused() {
        let t = Tmp::new("referrers");
        t.write("presets/inking.toml", "schema = 1\n");
        t.write(
            "profiles/art.toml",
            "[[mode]]\nname = \"Draw\"\noutput = \"tablet\"\npreset = \"inking\"\n",
        );
        let (store, _) = Store::load(&t.0).unwrap();
        assert_eq!(store.referrers_of("inking"), vec!["art".to_string()]);
        assert!(store.referrers_of("unused").is_empty());
    }

    #[test]
    fn startup_profile_falls_back_when_the_named_default_is_missing() {
        let t = Tmp::new("startup");
        t.write("config.toml", "schema = 1\n[defaults]\nprofile = \"gone\"\n");
        t.write(
            "profiles/only.toml",
            "display_name = \"Only\"\n[[mode]]\nname = \"m\"\noutput = \"mouse\"\npreset = \"raw\"\n",
        );
        let (store, _) = Store::load(&t.0).unwrap();
        assert_eq!(
            store.startup_profile().unwrap().display_name.as_deref(),
            Some("Only"),
            "a missing default profile should fall back, not leave nothing active"
        );
    }
}
