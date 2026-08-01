//! The unit of data flowing through the pipeline.

/// One motion sample.
///
/// Distances are **millimetres** and time is **microseconds taken from the source
/// event** — never read from a clock. See D10 and the determinism requirement in
/// docs/modules.md: any `Instant::now()` inside a filter would make replay diverge
/// from live and render the research harness meaningless.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Motion for this sample, in millimetres. Stages rewrite this in place.
    pub dx: f64,
    pub dy: f64,

    /// The pre-filter motion, preserved unmodified for the whole pipeline.
    ///
    /// Needed because the `pressure` stage can derive its speed term from hand
    /// *intent* rather than output motion (`speed.source = cursor`), and by the time
    /// it runs the output has been through several transforms. See stages.md.
    pub raw_dx: f64,
    pub raw_dy: f64,

    /// Source event timestamp, monotonic microseconds.
    pub t_us: u64,

    /// Seconds since the previous sample. Computed and sanitised by the pipeline,
    /// never by individual stages.
    pub dt: f64,

    /// True while the drawing button is held.
    pub down: bool,

    /// True on the first sample of a press.
    pub stroke_start: bool,

    /// True on the sample where the press ended.
    pub stroke_end: bool,

    /// Whether the user is holding the constrain modifier.
    ///
    /// Arrives on the sample rather than being read from a device, for the same reason time
    /// does: a stage that queried the world would make replay diverge from live. The daemon
    /// resolves the binding — a mouse button it can already see, or a keyboard key it watches
    /// without grabbing — and states the answer here.
    pub constrain: bool,

    /// Set when the gap since the previous sample was long enough that filter state
    /// should be treated as stale — suspend/resume, or a device reconnect. Stages
    /// must reset their internal state when they see it.
    pub discontinuity: bool,

    /// Synthesised pressure in `0.0..=1.0`. `None` until the `pressure` stage runs.
    pub pressure: Option<f64>,

    /// The speed estimate the `pressure` stage derived, in mm/s. Exposed because the
    /// dashboard's live scope needs it and because tuning is impossible without seeing
    /// the intermediate value that drives the speed term.
    pub speed_mm_s: Option<f64>,
}

impl Sample {
    /// A sample carrying raw device motion. `dt`, `stroke_*` and `discontinuity` are
    /// filled in by the pipeline.
    pub fn new(dx: f64, dy: f64, t_us: u64, down: bool) -> Self {
        Self {
            dx,
            dy,
            raw_dx: dx,
            raw_dy: dy,
            t_us,
            dt: 0.0,
            down,
            stroke_start: false,
            stroke_end: false,
            constrain: false,
            discontinuity: false,
            pressure: None,
            speed_mm_s: None,
        }
    }

    /// Magnitude of the current (post-filter-so-far) motion, in mm.
    pub fn magnitude(&self) -> f64 {
        self.dx.hypot(self.dy)
    }

    /// Magnitude of the pre-filter motion, in mm.
    pub fn raw_magnitude(&self) -> f64 {
        self.raw_dx.hypot(self.raw_dy)
    }
}
