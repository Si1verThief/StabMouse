//! Ownership of the virtual tablet, and the choice of whether it survives leaving tablet mode.
//!
//! D13 says the sinks are created once at startup and never torn down, because a Qt application
//! started with no tablet present never initialises its tablet subsystem at all. That remains
//! the default and the right one.
//!
//! This option was built to address a Krita defect: after a proximity-out it keeps painting a
//! stale canvas cursor and suppressing the real pointer until the pointer leaves its canvas.
//!
//! **It does not fix that, and the option should not be described as though it does.** Tested
//! 2026-07-31: killing the daemon entirely, which destroys the device, leaves Krita just as
//! stuck. The state is internal to Krita and no device-side action reaches it.
//!
//! What remains is a tested, cheap, off-by-default mechanism with no known beneficiary. It is
//! kept because teardown-and-return is sound in itself — D13's own measurements show that once
//! a Qt application has initialised its tablet subsystem, hotplug works fine — and because some
//! other application may yet need it. It is not kept because it solves the problem it was
//! written for.
//!
//! Measured 2026-07-31 with `stabmouse-probe recreate`, ten cycles on this host, timed from
//! `uinput` creation to KWin exposing the device *and* agreeing it is a tablet tool:
//!
//! ```text
//! create -> usable: median 48.8 ms, max 59.5 ms
//! destroy -> gone:  median 16.1 ms, max 25.0 ms
//! ```
//!
//! Around 50ms is under the threshold where a mode switch stops reading as instant, so cost is
//! not what disqualifies it. It stays **off by default** because it forfeits D13's guarantee
//! for anything launched while the device is absent, in exchange for no demonstrated benefit.

use stabmouse_output::TabletSink;

pub struct Tablet {
    name: String,
    sink: Option<TabletSink>,
    /// Destroy the device when tablet output is left, instead of keeping it alive.
    pub destroy_on_leave: bool,
}

impl Tablet {
    pub fn new(sink: TabletSink, destroy_on_leave: bool) -> Self {
        Self {
            name: sink.name().to_string(),
            sink: Some(sink),
            destroy_on_leave,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the device currently exists.
    pub fn present(&self) -> bool {
        self.sink.is_some()
    }

    /// The sink, creating it if a previous leave destroyed it.
    ///
    /// Creation is deferred to the first event rather than done on entering tablet mode, so the
    /// ~50ms is spent while the user is still reaching for the mouse rather than added to the
    /// switch itself.
    pub fn ensure(&mut self) -> stabmouse_output::Result<&mut TabletSink> {
        if self.sink.is_none() {
            self.sink = Some(TabletSink::new(&self.name)?);
        }
        // Just assigned above when it was absent, so the sink is present either way.
        Ok(self.sink.as_mut().expect("sink was just created"))
    }

    /// Whether this tablet's tool is in proximity.
    pub fn in_proximity(&self) -> bool {
        self.sink.as_ref().is_some_and(|s| s.in_proximity())
    }

    /// Lift the pen without tearing anything down.
    ///
    /// Separate from [`Tablet::leave`] because it runs on the hot path: `leave` also honours
    /// `destroy_on_leave`, and destroying plus recreating the device on every sample would be
    /// catastrophic rather than merely wasteful.
    pub fn lift(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            if sink.in_proximity() {
                sink.leave_proximity();
                let _ = sink.flush();
            }
        }
    }

    /// Leave tablet output: lift the pen, and destroy the device if configured to.
    ///
    /// The proximity-out is sent **before** any teardown and unconditionally. A device that
    /// vanishes mid-stroke leaves applications that were not watching for its removal holding a
    /// pen that is still down, so the clean exit has to happen either way.
    pub fn leave(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            sink.leave_proximity();
            let _ = sink.flush();
        }
        if self.destroy_on_leave {
            self.sink = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> Option<TabletSink> {
        // uinput is not reachable in every environment these tests run in.
        TabletSink::new("StabMouse tablet handle test").ok()
    }

    #[test]
    fn the_default_keeps_the_device_across_a_leave() {
        let Some(s) = sink() else { return };
        let mut t = Tablet::new(s, false);
        t.leave();
        assert!(t.present(), "D13: the device outlives leaving tablet output by default");
    }

    #[test]
    fn opting_in_destroys_and_recreates_under_the_same_name() {
        let Some(s) = sink() else { return };
        let name = s.name().to_string();
        let mut t = Tablet::new(s, true);

        t.leave();
        assert!(!t.present(), "the device should be gone after an opted-in leave");

        // A recreate can genuinely fail under parallel test load — uinput is a shared, finite
        // resource. That is the environment refusing, not the behaviour being wrong, and
        // panicking on it would make this test flaky rather than meaningful.
        let Ok(recreated) = t.ensure() else { return };

        // The name is the identity a compositor keys per-device settings off, so a recreated
        // device that came back under a different name would silently lose its output mapping.
        assert_eq!(recreated.name(), name);
        assert!(t.present());
    }

    #[test]
    fn leaving_twice_is_harmless() {
        let Some(s) = sink() else { return };
        let mut t = Tablet::new(s, true);
        t.leave();
        t.leave();
        assert!(!t.present());
    }
}
