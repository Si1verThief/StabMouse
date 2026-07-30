//! The interface every filter implements.

use crate::sample::Sample;

/// One filter in a preset's pipeline.
///
/// Contract, from docs/modules.md:
///
/// - **Deterministic.** Identical input and parameters must produce identical output.
/// - **No clock.** Time arrives on the sample. Reading a clock here would make
///   replay diverge from live.
/// - **No allocation** in `process`.
/// - **Cannot panic** on any combination of input and parameters.
/// - **Identity settings are pass-through** — a stage configured to do nothing must
///   leave the sample byte-identical.
pub trait Stage {
    fn name(&self) -> &'static str;

    /// Transform the sample in place.
    fn process(&mut self, sample: &mut Sample);

    /// Discard accumulated state. Called on mode switch and whenever the pipeline
    /// reports a discontinuity.
    fn reset(&mut self);

    /// Disabled stages remain in the pipeline so config keeps their tuning, but are
    /// skipped during processing.
    fn enabled(&self) -> bool {
        true
    }

    fn set_enabled(&mut self, _enabled: bool) {}

    /// False while the stage still holds state that has not reached the output.
    ///
    /// The pipeline is event driven, so a filter mid-convergence has no way to finish
    /// if no further input arrives. Consumers use this to decide whether to keep
    /// feeding zero-motion ticks. A stage with nothing in flight is always settled.
    fn settled(&self) -> bool {
        true
    }
}
