//! The unit of data flowing through the pipeline.

/// How many separately-bound gesture stages one preset may carry.
///
/// A fixed cap because [`Sample`] is `Copy` and on the hot path. Eight is far past what any
/// hand can hold bindings for; the assembler warns and shares the last slot rather than
/// dropping a stage, so exceeding it degrades rather than vanishing.
pub const MAX_GESTURES: usize = 8;

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

    /// Whether each bound gesture is fully held, indexed by the stage instance's slot.
    ///
    /// **One flag per instance, not one for the pipeline.** A preset may carry several
    /// `scroll` stages — a wheel gesture on one binding and a drag on another is the obvious
    /// pairing — and a single shared flag meant whichever binding the daemon happened to read
    /// engaged all of them at once. Holding the wheel's modifier started the drag too.
    ///
    /// Arrives on the sample for the same reason `constrain` does: a stage that asked a device
    /// would make replay diverge from live.
    pub gestures: [bool; MAX_GESTURES],

    /// Whether *any* part of each gesture's binding is still held.
    ///
    /// Only differs from `gestures` for a chord: all parts down engages it, some parts down is
    /// a hand halfway off. That middle state is what lets a chord say "let go of one and it
    /// coasts, let go of both and it stops".
    pub gestures_partial: [bool; MAX_GESTURES],

    /// Physical wheel movement arriving with this sample, in notches — vertical then
    /// horizontal, positive up and right.
    ///
    /// **A stage that does nothing with these must leave them alone**, and the daemon emits
    /// whatever is left verbatim. That is what keeps an untouched wheel exactly as good as it
    /// was before any of this existed: routing it through the pipeline must never be able to
    /// swallow it by accident.
    pub wheel_v: f64,
    pub wheel_h: f64,

    /// Scroll the pipeline decided to produce, in wheel notches, positive up and right.
    ///
    /// An **output** of the `scroll` stage rather than an input. The core cannot emit events
    /// — it is pure — so a gesture that means "scroll" says so here and the daemon turns it
    /// into wheel events on whichever sink is carrying the mode.
    ///
    /// Fractional on purpose: a hi-res wheel resolves 1/120th of a notch, and rounding to
    /// whole notches is what makes drag-scrolling feel like a ratchet instead of a surface.
    pub scroll_x: f64,
    pub scroll_y: f64,

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
            gestures: [false; MAX_GESTURES],
            gestures_partial: [false; MAX_GESTURES],
            wheel_v: 0.0,
            wheel_h: 0.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            constrain: false,
            discontinuity: false,
            pressure: None,
            speed_mm_s: None,
        }
    }

    /// Whether the gesture in `slot` is fully held. Out-of-range slots are never held, which
    /// is what makes a stage past the cap inert rather than stuck on.
    pub fn gesture(&self, slot: usize) -> bool {
        self.gestures.get(slot).copied().unwrap_or(false)
    }

    /// Whether any part of `slot`'s binding is held.
    pub fn gesture_partial(&self, slot: usize) -> bool {
        self.gestures_partial.get(slot).copied().unwrap_or(false)
    }

    /// State one gesture's binding. Slots past the cap are dropped rather than panicking.
    pub fn set_gesture(&mut self, slot: usize, held: bool, partial: bool) {
        if let Some(g) = self.gestures.get_mut(slot) {
            *g = held;
        }
        if let Some(g) = self.gestures_partial.get_mut(slot) {
            *g = partial;
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
