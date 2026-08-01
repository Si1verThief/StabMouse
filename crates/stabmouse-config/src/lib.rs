//! Config schema, device cascade, and format-preserving IO.
//!
//! Implements docs/config-schema.md. Two requirements shape the whole crate:
//!
//! - **The config file is the user's.** A migrated file is never written back
//!   unprompted, and a round-trip preserves comments, key order and whitespace
//!   exactly. See D15.
//! - **The filename is the identity.** `presets/inking.toml` *is* the preset
//!   `inking`; there is no internal name field to disagree with it. See D14.

mod assemble;
pub mod catalog;
mod cascade;
mod edit;
mod error;
mod schema;
mod store;

pub use assemble::{assemble, Assembly};
pub use catalog::{ParamKind, ParamSpec, StageSpec, STAGES};
pub use cascade::{DeviceView, Origin, OverrideKey, Resolved};
pub use edit::Document;
pub use store::{LoadReport, Store, RAW};
pub use error::{Error, Result};
pub use schema::{
    is_valid_slug, AppRule, Defaults, Device, Group, Identity, Match, Mode, Output, Params,
    Passthrough,
    Preset, Profile, Root, StageEntry, CURRENT_SCHEMA,
};
