//! Filter stages.
//!
//! Roster and parameter definitions live in docs/stages.md. Order is
//! config-controlled with two pins: `normalize` first, `pressure` last.

mod normalize;
mod pressure;
mod sensitivity;
mod smooth;
mod stabilize;

pub use normalize::Normalize;
pub use pressure::{Pressure, SpeedSource, StallBehaviour};
pub use sensitivity::{Curve, Sensitivity};
pub use smooth::Smooth;
pub use stabilize::Stabilize;
