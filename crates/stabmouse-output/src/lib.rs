//! Virtual devices the filtered stream is written to.
//!
//! Three sinks — a relative mouse, the tablets, and an absolute pointer carrying the tablet
//! modes' fallback — **all created at daemon startup and never torn down** (D13). Qt only
//! initialises its tablet subsystem if a tablet exists when the application starts, and
//! that initialisation never retries — so a tablet created later gives Krita no pressure
//! at all, for the life of that process. Creating sinks lazily on mode switch would make
//! the feature silently unavailable to anything already running.
//!
//! Two hard-won rules are encoded here:
//!
//! - **Emit only what changed.** Restating every axis on every report is an event storm
//!   that measurably breaks application UI — it caused doubled clicks in Krita's menus
//!   until fixed. This is a correctness requirement, not an optimisation.
//! - **Replicate every capability the source had**, hi-res wheel included. Dropping one
//!   silently degrades the device in a way that is hard to attribute later.

mod desktop;
mod mouse;
mod pointer;
mod tablet;

pub use desktop::{DesktopMapper, Placement, Screen};
pub use mouse::MouseSink;
pub use pointer::{PointerSink, POINTER_ABS_MAX};
pub use tablet::{SurfaceMapper, TabletSink, PRESSURE_MAX, SURFACE_MAX};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("creating virtual device '{name}': {source}")]
    Create {
        name: String,
        #[source]
        source: std::io::Error,
    },

    #[error("emitting to '{name}': {source}")]
    Emit {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
