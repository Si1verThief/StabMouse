//! Filter stages.
//!
//! Roster and parameter definitions live in docs/stages.md. Order is
//! config-controlled with two pins: `normalize` first, `pressure` last.

mod average;
mod deadzone;
mod normalize;
mod pressure;
mod rotate;
mod scroll;
mod sensitivity;
mod snap;
mod smooth;
mod stabilize;

pub use average::{Average, Weighting};
pub use deadzone::Deadzone;
pub use normalize::Normalize;
pub use pressure::{Pressure, SpeedSource, StallBehaviour};
pub use rotate::Rotate;
pub use scroll::{Mode as ScrollMode, Scroll};
pub use sensitivity::{Curve, Sensitivity};
pub use snap::{Constraint, Snap};
pub use smooth::Smooth;
pub use stabilize::Stabilize;
