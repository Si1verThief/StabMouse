//! Format-preserving reads and writes.
//!
//! D15: the config file belongs to the user. A round-trip returns it byte-identical —
//! comments, key order, blank lines, inline-versus-table style, all of it — and a
//! migrated file is never written back unprompted.
//!
//! This is why the crate carries `toml_edit` as well as `serde`. `serde` gives the typed
//! view the rest of the program wants; `toml_edit` keeps the document the user wrote.
//! Both are held, and writes go through the document.

use crate::error::{Error, Result};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

/// A config file as both typed data and the original document.
#[derive(Debug)]
pub struct Document<T> {
    parsed: T,
    doc: DocumentMut,
    path: PathBuf,
    dirty: bool,
}

impl<T: DeserializeOwned> Document<T> {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let text = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_str(path, &text)
    }

    pub fn from_str(path: impl Into<PathBuf>, text: &str) -> Result<Self> {
        let path = path.into();
        let doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| Error::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let parsed: T = toml::from_str(text).map_err(|e| Error::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        Ok(Self {
            parsed,
            doc,
            path,
            dirty: false,
        })
    }

    pub fn data(&self) -> &T {
        &self.parsed
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True once something has been changed in memory but not written.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The document as text. Byte-identical to the input while unmodified.
    pub fn to_text(&self) -> String {
        self.doc.to_string()
    }

    /// Write the file, but only if something actually changed.
    ///
    /// Refusing to write an unchanged file is not an optimisation: it is what keeps a
    /// mere *read* from touching mtime, and what stops a migration on load from
    /// rewriting a hand-commented file the user never asked us to edit (D15).
    pub fn save_if_dirty(&mut self) -> Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        std::fs::write(&self.path, self.doc.to_string()).map_err(|source| Error::Write {
            path: self.path.clone(),
            source,
        })?;
        self.dirty = false;
        Ok(true)
    }

    /// Re-derive the typed view after edits.
    fn reparse(&mut self) -> Result<()> {
        let text = self.doc.to_string();
        self.parsed = toml::from_str(&text).map_err(|e| Error::Parse {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        Ok(())
    }

    /// Set a top-level dotted key, e.g. `["defaults", "profile"]`.
    ///
    /// Intermediate tables are created as needed. Surrounding formatting and comments
    /// are untouched, including any comment attached to the key being changed.
    pub fn set(&mut self, path: &[&str], value: toml::Value) -> Result<()> {
        let Some((last, parents)) = path.split_last() else {
            return Err(Error::BadOverrideKey {
                key: String::new(),
            });
        };

        let mut item = self.doc.as_item_mut();
        for key in parents {
            // `or_insert` on a missing key needs an implicit table so the document does
            // not gain a spurious `[a.b]` header the user never wrote.
            let table = item.as_table_like_mut().ok_or_else(|| Error::Parse {
                path: self.path.clone(),
                message: format!("cannot descend into '{key}': not a table"),
            })?;
            if table.get(key).is_none() {
                let mut new = toml_edit::Table::new();
                new.set_implicit(true);
                table.insert(key, toml_edit::Item::Table(new));
            }
            item = table.get_mut(key).expect("just inserted");
        }

        let table = item.as_table_like_mut().ok_or_else(|| Error::Parse {
            path: self.path.clone(),
            message: format!("cannot set '{last}': parent is not a table"),
        })?;
        assign(table, last, to_edit_value(&value));

        self.dirty = true;
        self.reparse()
    }

    /// Set a parameter on the nth `[[stage]]` entry of a preset file.
    pub fn set_stage_param(&mut self, index: usize, param: &str, value: toml::Value) -> Result<()> {
        let stages = self
            .doc
            .get_mut("stage")
            .and_then(|i| i.as_array_of_tables_mut())
            .ok_or_else(|| Error::Parse {
                path: self.path.clone(),
                message: "no [[stage]] entries".to_string(),
            })?;
        let table = stages.get_mut(index).ok_or_else(|| Error::Parse {
            path: self.path.clone(),
            message: format!("stage index {index} out of range"),
        })?;
        assign(table, param, to_edit_value(&value));
        self.dirty = true;
        self.reparse()
    }
}

/// Set `key` to `value` without disturbing any formatting around it.
///
/// `Table::insert` replaces the whole item — key included — which discards the key's
/// decor, and a key's *leading comment lives in its decor*. So editing one value with
/// `insert` silently deletes the comment above it, which is precisely the betrayal D15
/// exists to prevent.
///
/// When the key already exists this therefore mutates the value in place and never
/// touches the key, restoring the value's own decor so the spacing after `=` and any
/// trailing comment survive too.
fn assign(table: &mut dyn toml_edit::TableLike, key: &str, value: toml_edit::Value) {
    if let Some(existing) = table.get_mut(key).and_then(|i| i.as_value_mut()) {
        let decor = existing.decor().clone();
        *existing = value;
        *existing.decor_mut() = decor;
        return;
    }
    table.insert(key, toml_edit::Item::Value(value));
}

/// `toml::Value` (what serde gives us) into `toml_edit::Value` (what the document holds).
///
/// The two crates have separate value types; there is no blanket conversion.
fn to_edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        toml::Value::Integer(i) => toml_edit::Value::from(*i),
        toml::Value::Float(f) => toml_edit::Value::from(*f),
        toml::Value::Boolean(b) => toml_edit::Value::from(*b),
        toml::Value::Datetime(d) => toml_edit::Value::from(d.to_string()),
        toml::Value::Array(items) => {
            let mut arr = toml_edit::Array::new();
            for i in items {
                arr.push(to_edit_value(i));
            }
            toml_edit::Value::Array(arr)
        }
        toml::Value::Table(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, val) in map {
                t.insert(k, to_edit_value(val));
            }
            toml_edit::Value::InlineTable(t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Preset, Root};

    /// Deliberately awkward: comments in three positions, inconsistent spacing, a blank
    /// line inside a table, an inline table, and a trailing comment with no newline
    /// conventions respected.
    const MESSY: &str = r#"# StabMouse config — hand written, do not eat my comments
schema = 1

[defaults]
# I like this profile
profile   =   "line-art"

# a deliberately odd blank line follows

overrides = { "inking.stabilize.radius_mm" = 0.5 }   # inline trailing comment

[[device]]
match = { vid = "0738", pid = "0c08" }   # the R.A.T.
label = "R.A.T. 8+"
managed = true
"#;

    #[test]
    fn an_untouched_round_trip_is_byte_identical() {
        let d: Document<Root> = Document::from_str("t.toml", MESSY).unwrap();
        assert_eq!(d.to_text(), MESSY, "round-trip altered the document");
        assert!(!d.is_dirty());
    }

    #[test]
    fn reading_never_marks_the_file_dirty() {
        let d: Document<Root> = Document::from_str("t.toml", MESSY).unwrap();
        assert!(!d.is_dirty(), "a read must not schedule a write");
    }

    #[test]
    fn saving_an_unchanged_document_writes_nothing() {
        let mut d: Document<Root> = Document::from_str("/nonexistent/t.toml", MESSY).unwrap();
        // Would fail with a write error if it actually attempted the write.
        assert_eq!(d.save_if_dirty().unwrap(), false);
    }

    #[test]
    fn editing_one_value_preserves_every_comment() {
        let mut d: Document<Root> = Document::from_str("t.toml", MESSY).unwrap();
        d.set(&["defaults", "profile"], toml::Value::String("shading".into()))
            .unwrap();

        let out = d.to_text();
        assert!(out.contains("do not eat my comments"));
        assert!(out.contains("# I like this profile"));
        assert!(out.contains("# inline trailing comment"));
        assert!(out.contains("# the R.A.T."));
        assert!(out.contains(r#"profile   =   "shading""#) || out.contains(r#"profile = "shading""#));
        assert!(d.is_dirty());
        assert_eq!(d.data().defaults.profile.as_deref(), Some("shading"));
    }

    #[test]
    fn editing_does_not_disturb_unrelated_whitespace() {
        let mut d: Document<Root> = Document::from_str("t.toml", MESSY).unwrap();
        let before = d.to_text();
        d.set(&["defaults", "profile"], toml::Value::String("shading".into()))
            .unwrap();
        let after = d.to_text();

        // Only the one line should differ.
        let differing: Vec<_> = before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            differing.len(),
            1,
            "expected exactly one changed line, got {differing:?}"
        );
    }

    #[test]
    fn a_new_nested_key_does_not_add_a_spurious_table_header() {
        let mut d: Document<Root> = Document::from_str("t.toml", "schema = 1\n").unwrap();
        d.set(&["defaults", "profile"], toml::Value::String("x".into()))
            .unwrap();
        let out = d.to_text();
        assert!(out.contains("profile"), "value should be written: {out}");
        assert_eq!(d.data().defaults.profile.as_deref(), Some("x"));
    }

    const PRESET: &str = r#"schema = 1
display_name = "Inking"

[[stage]]
type = "normalize"
dpi = 1600

[[stage]]
# the important one
type = "stabilize"
radius_mm = 0.5
catch_up = 0.35
"#;

    #[test]
    fn preset_round_trips_and_stage_edits_are_surgical() {
        let mut d: Document<Preset> = Document::from_str("p.toml", PRESET).unwrap();
        assert_eq!(d.to_text(), PRESET);

        d.set_stage_param(1, "radius_mm", toml::Value::Float(0.8))
            .unwrap();

        let out = d.to_text();
        assert!(out.contains("# the important one"));
        assert!(out.contains("radius_mm = 0.8"));
        assert!(out.contains("catch_up = 0.35"));
        assert!(out.contains("dpi = 1600"));
        assert_eq!(
            d.data().stages[1]
                .params
                .get("radius_mm")
                .and_then(|v| v.as_float()),
            Some(0.8)
        );
    }

    #[test]
    fn a_syntax_error_names_the_file() {
        let err = Document::<Root>::from_str("broken.toml", "this is not = = toml").unwrap_err();
        assert!(
            err.to_string().contains("broken.toml"),
            "error should name the file: {err}"
        );
    }
}
