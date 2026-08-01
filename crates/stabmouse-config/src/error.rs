//! Errors.
//!
//! Typed rather than `anyhow`, because callers act differently on them: a missing
//! preset reference falls back to raw passthrough and warns, an unreadable file is
//! fatal at startup but recoverable on reload, and a permission problem gets its own
//! CLI exit code (3).

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("{path}: schema version {found} is newer than this build understands ({max})")]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        max: u32,
    },

    #[error("{path}: no migration path from schema version {from}")]
    NoMigration { path: PathBuf, from: u32 },

    /// Deliberately not fatal at the config layer: the caller decides. Per the schema
    /// spec a mode with a missing preset refuses to activate and falls back to raw
    /// passthrough rather than silently substituting something else.
    #[error("profile '{profile}' mode {mode} references preset '{preset}', which does not exist")]
    DanglingPreset {
        profile: String,
        mode: usize,
        preset: String,
    },

    #[error("{what} '{name}' is not a valid slug (expect kebab-case, filesystem-safe)")]
    BadSlug { what: &'static str, name: String },

    #[error("override key '{key}' is malformed; expected <preset>.<stage>.<param>")]
    BadOverrideKey { key: String },
}

pub type Result<T> = std::result::Result<T, Error>;
