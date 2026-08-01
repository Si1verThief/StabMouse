//! The set of virtual tablets, one per screen, and the handover between them.
//!
//! See D17. A single tablet spanning the desktop divides one fixed surface among every monitor,
//! so precision falls with each screen added, and it cannot be isotropic on more than one
//! aspect ratio at a time. One tablet per screen avoids both.
//!
//! # Placement is attempted, never assumed
//!
//! Creating the devices always works; confining each to its screen is a compositor setting that
//! only KDE currently exposes. When placement fails the tablets still exist and still work —
//! they simply cover the whole desktop until the user maps them, which is the same position
//! every other tablet on Linux starts from. So a failure here **degrades and reports**, it does
//! not abort startup.
//!
//! # One screen, or none
//!
//! With a single screen this is one tablet and no crossings, which is the same behaviour as
//! before and costs nothing extra. With no screens reported — X11, or a compositor that told us
//! nothing — [`Tablets::single`] falls back to one desktop-wide tablet.

use crate::tablet::Tablet;
use stabmouse_output::{DesktopMapper, Screen, TabletSink, SURFACE_MAX};
use std::time::{Duration, Instant};

/// How long to wait for the compositor to adopt a newly created tablet before mapping it.
///
/// Creating a `uinput` device does not make it addressable: udev has to process it and the
/// compositor has to adopt it, measured at a median of 49ms and a maximum of 60ms on this host
/// (`stabmouse-probe recreate`). Mapping immediately after creation therefore fails with "no
/// such device" — which is exactly what happened on the first live run. Generous relative to
/// the measurement because the cost of waiting is paid once at startup, while the cost of
/// giving up too early is a tablet silently covering the whole desktop.
const ADOPTION_TIMEOUT: Duration = Duration::from_secs(2);

/// Prefix for every tablet device name.
///
/// The name is the identity a compositor keys per-device settings off, so it must be stable
/// across teardown and recreation and must never contain anything volatile like an event node
/// number. Appending the connector name makes each one both stable and self-describing in
/// system settings.
const PREFIX: &str = "StabMouse tablet";

pub struct Tablets {
    tablets: Vec<Tablet>,
    mapper: DesktopMapper,
    /// Whether every tablet was successfully confined to its screen.
    placed: bool,
}

fn device_name(screen: Option<&Screen>) -> String {
    match screen {
        Some(s) => format!("{PREFIX} {}", s.name),
        None => PREFIX.to_string(),
    }
}

impl Tablets {
    /// One tablet per screen, each sized to its screen's aspect.
    ///
    /// Does **not** place them. Construction creates local devices and nothing else; asking the
    /// compositor to confine them is a separate, explicit [`Tablets::place`]. Doing it here
    /// would mean every construction — including in tests — reaches out and mutates the running
    /// desktop's settings, which is not something a constructor should do.
    pub fn per_screen(
        screens: Vec<Screen>,
        span_mm: f64,
        destroy_on_leave: bool,
    ) -> stabmouse_output::Result<Self> {
        let mut tablets = Vec::with_capacity(screens.len());
        for s in &screens {
            let (max_x, max_y) = s.surface_extent(SURFACE_MAX);
            let sink = TabletSink::with_extent(&device_name(Some(s)), max_x, max_y)?;
            tablets.push(Tablet::new(sink, destroy_on_leave));
        }

        Ok(Self {
            tablets,
            mapper: DesktopMapper::new(screens, span_mm, SURFACE_MAX),
            placed: false,
        })
    }

    /// One desktop-wide tablet, for when the screen layout is unknown.
    pub fn single(span_mm: f64, destroy_on_leave: bool) -> stabmouse_output::Result<Self> {
        let sink = TabletSink::new(&device_name(None))?;
        Ok(Self {
            tablets: vec![Tablet::new(sink, destroy_on_leave)],
            // An empty screen list makes the mapper decline to place, which is what routes the
            // caller to the single-surface path rather than inventing a screen rectangle.
            mapper: DesktopMapper::new(Vec::new(), span_mm, SURFACE_MAX),
            placed: true,
        })
    }

    /// Ask the compositor to confine each tablet to its screen.
    ///
    /// Best-effort by design: a desktop with no such setting is not an error, it is a desktop
    /// where the user maps tablets themselves.
    pub fn place(&mut self) {
        if self.tablets.len() < 2 {
            // One tablet on one screen: covering the desktop *is* the correct placement, so
            // reporting it unplaced would send the user to display settings for nothing.
            self.placed = true;
            return;
        }
        if !stabmouse_desktop::can_map_tablets() {
            return;
        }
        let names: Vec<(String, String)> = self
            .mapper
            .screens()
            .iter()
            .map(|s| (device_name(Some(s)), s.name.clone()))
            .collect();

        // Every tablet is attempted, and the results combined afterwards. Short-circuiting on
        // the first failure would leave the remaining screens unmapped because of an unrelated
        // one, which is how a single racing device left the whole desktop unplaced.
        let mut all_placed = true;
        for (device, output) in &names {
            if !wait_for_adoption(device) {
                eprintln!("the compositor did not pick up {device:?} in time to place it");
                all_placed = false;
                continue;
            }
            if let Err(e) = stabmouse_desktop::map_tablet(device, output) {
                eprintln!("could not place {device:?} on {output}: {e}");
                all_placed = false;
            }
        }
        self.placed = all_placed;
    }

    pub fn placed(&self) -> bool {
        self.placed
    }

    pub fn len(&self) -> usize {
        self.tablets.len()
    }

    pub fn per_screen_active(&self) -> bool {
        self.tablets.len() > 1
    }

    pub fn set_destroy_on_leave(&mut self, destroy: bool) {
        for t in &mut self.tablets {
            t.destroy_on_leave = destroy;
        }
    }

    /// The tablet currently receiving the pen.
    pub fn active(&mut self) -> &mut Tablet {
        let i = self.mapper.active().min(self.tablets.len().saturating_sub(1));
        &mut self.tablets[i]
    }

    /// The active tablet's name, and whether its device currently exists.
    ///
    /// Presence is worth stating because with teardown enabled a tablet is legitimately absent
    /// between strokes, and "no such device" otherwise reads as a fault.
    pub fn describe_active(&self) -> String {
        let i = self.mapper.active().min(self.tablets.len().saturating_sub(1));
        match self.tablets.get(i) {
            Some(t) if t.present() => t.name().to_string(),
            Some(t) => format!("{} (absent until next used)", t.name()),
            None => "none".into(),
        }
    }

    /// Advance by a millimetre delta, handing over between screens as needed.
    ///
    /// Returns the surface position for whichever tablet is now active. The handover — a
    /// proximity-out on the screen being left — is performed here rather than left to the
    /// caller, because a tablet that keeps its tool in proximity while another takes over leaves
    /// two tools claiming the pointer.
    pub fn advance(&mut self, dx_mm: f64, dy_mm: f64, pen_down: bool) -> Option<(i32, i32)> {
        let placement = self.mapper.advance(dx_mm, dy_mm, pen_down)?;

        if let Some(from) = placement.crossed_from {
            if let Some(previous) = self.tablets.get_mut(from) {
                previous.leave();
            }
        }
        Some((placement.x, placement.y))
    }

    /// The tablet path's scale, so a fallback can match it.
    pub fn px_per_mm(&self) -> f64 {
        self.mapper.px_per_mm()
    }

    /// The tracked position in desktop pixels, `None` when the layout is unknown.
    pub fn position_px(&self) -> Option<(f64, f64)> {
        self.mapper.position_px()
    }

    /// Advance the tracked position without emitting anything.
    ///
    /// Called while output is going to the pointer, so the position the tablet would resume at
    /// stays current. Without it the mapper freezes during a fallback and the pen reappears
    /// wherever it was minutes ago.
    pub fn track(&mut self, dx_mm: f64, dy_mm: f64, pen_down: bool) {
        let _ = self.mapper.advance(dx_mm, dy_mm, pen_down);
    }

    /// Advance the tracked position by a pixel delta, for modes with their own scale (D23).
    pub fn track_px(&mut self, dx_px: f64, dy_px: f64) {
        let _ = self.mapper.advance_px(dx_px, dy_px, false);
    }

    /// How far the compositor's reported cursor may sit from the tracked position before it
    /// counts as motion we did not produce. Our own emissions echo back within a pixel;
    /// an unmanaged device lands far outside this.
    const ADOPT_TOLERANCE_PX: f64 = 2.0;

    /// Adopt the compositor's reported cursor position, if it meaningfully disagrees.
    ///
    /// This is what keeps the shared position truthful when an unmanaged pointer — a second
    /// mouse, a touchpad — moves the cursor, and what re-syncs it after a `relative` mode
    /// (which deliberately bypasses the shared position). The tolerance keeps our own
    /// reports, quantised to whole pixels by the compositor, from nudging the position.
    pub fn adopt_position_px(&mut self, x: f64, y: f64) {
        if let Some((cx, cy)) = self.mapper.position_px() {
            if (x - cx).abs() <= Self::ADOPT_TOLERANCE_PX
                && (y - cy).abs() <= Self::ADOPT_TOLERANCE_PX
            {
                return;
            }
        }
        self.mapper.set_position_px(x, y);
    }

    /// Whether any tablet still has its tool in proximity.
    pub fn any_in_proximity(&self) -> bool {
        self.tablets.iter().any(|t| t.in_proximity())
    }

    /// Lift every pen that is still down, without destroying anything.
    ///
    /// **The invariant this enforces: while output is going to the mouse, no tablet may be in
    /// proximity.** Enforcing it by checking state on every emit, rather than by acting on
    /// transitions, is deliberate. Transition handling has to be right on every path — mode
    /// switch, focus change, screen crossing, panic, config reload — and missing one leaves a
    /// tool in proximity on one screen while the pointer moves on another. Two live cursors
    /// then draw in two places at once, which is what happened.
    pub fn lift_all(&mut self) {
        for t in &mut self.tablets {
            t.lift();
        }
    }

    /// Lift every pen except the one currently receiving input.
    ///
    /// The mirror of [`Tablets::lift_all`] for the tablet path. A screen crossing hands over
    /// from one tablet to the next, and any path that misses the handover leaves two tools in
    /// proximity on two screens — again giving two live cursors. Checking the state costs a
    /// bool per tablet; relying on every transition being correct costs a bug per missed one.
    pub fn lift_inactive(&mut self) {
        let active = self.mapper.active().min(self.tablets.len().saturating_sub(1));
        for (i, t) in self.tablets.iter_mut().enumerate() {
            if i != active {
                t.lift();
            }
        }
    }

    /// Lift the pen on every tablet.
    ///
    /// All of them, not just the active one: a crossing that failed partway, or a config reload
    /// that rebuilt the set, can leave an inactive tablet still in proximity, and one of those
    /// is enough to keep a compositor thinking a tool is present.
    pub fn leave_all(&mut self) {
        for t in &mut self.tablets {
            t.leave();
        }
    }
}

/// Block until the compositor knows about `device`, or the timeout expires.
fn wait_for_adoption(device: &str) -> bool {
    let deadline = Instant::now() + ADOPTION_TIMEOUT;
    loop {
        if stabmouse_desktop::device_known(device) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        // Each check is a D-Bus round trip, so polling much faster measures the bus rather than
        // the device.
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screens() -> Vec<Screen> {
        vec![
            Screen { name: "HDMI-A-1".into(), x: 0, y: 0, width: 1920, height: 1080 },
            Screen { name: "DP-2".into(), x: 1920, y: 0, width: 1280, height: 1024 },
        ]
    }

    #[test]
    fn names_are_stable_and_identify_their_screen() {
        // A compositor keys per-device settings off the name, so a device that came back under a
        // different name would silently lose its output mapping.
        let s = screens();
        assert_eq!(device_name(Some(&s[0])), "StabMouse tablet HDMI-A-1");
        assert_eq!(device_name(Some(&s[1])), "StabMouse tablet DP-2");
        assert_eq!(device_name(None), "StabMouse tablet");
    }

    #[test]
    fn a_single_tablet_reports_no_per_screen_behaviour() {
        let Ok(t) = Tablets::single(200.0, false) else { return };
        assert_eq!(t.len(), 1);
        assert!(!t.per_screen_active());
        // No screen layout, so the caller uses its own single-surface mapper instead.
        assert!(t.mapper.placement_now().is_none());
    }

    #[test]
    fn a_tablet_is_created_for_every_screen() {
        let Ok(t) = Tablets::per_screen(screens(), 200.0, false) else { return };
        assert_eq!(t.len(), 2);
        assert!(t.per_screen_active());
    }

    #[test]
    fn crossing_a_screen_boundary_switches_the_active_tablet() {
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, false) else { return };
        assert_eq!(t.describe_active(), "StabMouse tablet HDMI-A-1");
        // Far enough right to land on the second screen.
        t.advance(150.0, 0.0, false).unwrap();
        assert_eq!(t.describe_active(), "StabMouse tablet DP-2");
    }

    #[test]
    fn only_one_tablet_is_ever_live_at_a_time() {
        // The two-cursor bug: a tool left in proximity on one screen while another drives the
        // cursor elsewhere means both draw at once.
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, false) else { return };

        // Put the first screen's tool in proximity, then cross to the second.
        t.advance(0.0, 0.0, false);
        let _ = t.active().ensure().map(|s| s.pen(100, 100, 0.0, false));
        t.advance(150.0, 0.0, false).unwrap();
        let _ = t.active().ensure().map(|s| s.pen(100, 100, 0.0, false));

        t.lift_inactive();
        let live = t.tablets.iter().filter(|x| x.in_proximity()).count();
        assert!(live <= 1, "{live} tablets in proximity at once");
    }

    #[test]
    fn lifting_all_leaves_nothing_in_proximity() {
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, false) else { return };
        t.advance(0.0, 0.0, false);
        let _ = t.active().ensure().map(|s| s.pen(100, 100, 0.0, false));
        assert!(t.any_in_proximity());

        t.lift_all();
        assert!(!t.any_in_proximity(), "mouse transport requires no live pen");
    }

    #[test]
    fn lifting_does_not_destroy_the_device() {
        // `lift` runs on the hot path; if it honoured destroy_on_leave it would tear the device
        // down and rebuild it on every sample.
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, true) else { return };
        t.advance(0.0, 0.0, false);
        let _ = t.active().ensure().map(|s| s.pen(100, 100, 0.0, false));
        t.lift_all();
        assert!(t.tablets.iter().all(|x| x.present()), "lift must not tear devices down");
    }

    #[test]
    fn adoption_ignores_our_own_echo_but_takes_real_motion() {
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, false) else { return };
        let (x0, y0) = t.position_px().unwrap();
        // A compositor report one pixel off is our own emission, quantised. Adopting it
        // would nudge the position on every idle report.
        t.adopt_position_px(x0 + 1.0, y0 + 1.0);
        assert_eq!(t.position_px().unwrap(), (x0, y0));
        // A report far away is a device we do not manage; the position must follow it.
        t.adopt_position_px(x0 + 300.0, y0);
        assert_eq!(t.position_px().unwrap(), (x0 + 300.0, y0));
    }

    #[test]
    fn a_stroke_is_never_split_across_two_tablets() {
        let Ok(mut t) = Tablets::per_screen(screens(), 200.0, false) else { return };
        t.advance(500.0, 0.0, true).unwrap();
        assert_eq!(
            t.describe_active(),
            "StabMouse tablet HDMI-A-1",
            "with the pen down the position must clamp rather than hand over"
        );
    }
}
