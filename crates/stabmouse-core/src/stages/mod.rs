//! Filter stages.
//!
//! Roster and parameter definitions live in docs/stages.md. Order is
//! config-controlled with two pins: `normalize` first, `pressure` last.

mod deadzone;
mod normalize;
mod pressure;
mod rotate;
mod sensitivity;
mod smooth;
mod stabilize;

pub use deadzone::Deadzone;
pub use normalize::Normalize;
pub use pressure::{Pressure, SpeedSource, StallBehaviour};
pub use rotate::Rotate;
pub use sensitivity::{Curve, Sensitivity};
pub use smooth::Smooth;
pub use stabilize::Stabilize;
