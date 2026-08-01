//! Desktop integration: what screens exist, and putting a tablet on one of them.
//!
//! # The seam
//!
//! These are two different problems and they have deliberately been given two different
//! answers, because only one of them is compositor-specific.
//!
//! **Enumerating screens is a Wayland protocol.** `wl_output` reports a connector name and a
//! logical position and size on any Wayland compositor, so [`outputs`] is not KDE code and
//! will not need replacing to support another desktop.
//!
//! **Mapping a tablet onto a screen is not standardised.** There is no protocol for "this
//! tablet covers that monitor" — it is a compositor setting, and every compositor exposes it
//! differently or not at all. KWin makes it a writable D-Bus property, which is why KDE is the
//! one implemented ([`kde`]).
//!
//! # Adding a compositor later
//!
//! Nothing here is built out in advance: there is no trait, no registry, no plugin loading, and
//! only one backend. What is deliberately preserved is the *shape* — [`map_tablet`] takes a
//! device name and an output name and nothing else, and reports [`Error::Unsupported`] when it
//! cannot act. Adding Hyprland or GNOME means adding a module beside `kde` and a branch in
//! [`map_tablet`], not restructuring anything.
//!
//! The failure is reported rather than swallowed on purpose. A tablet silently landing on the
//! wrong monitor is far harder to diagnose than one that says it could not be placed.

pub mod kde;
mod wayland;

pub use wayland::outputs;

/// A screen, as the compositor lays it out.
///
/// Logical rather than physical pixels, because that is the coordinate space monitors are
/// arranged in — a scaled display is a different size in the layout than its panel resolution
/// suggests, and adjacency has to be computed in the space the cursor actually moves through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Connector name, as the compositor uses it — `DP-2`, `HDMI-A-1`.
    pub name: String,
    /// Human-readable, when the compositor offers one.
    pub description: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Output {
    pub fn right_edge(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom_edge(&self) -> i32 {
        self.y + self.height
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right_edge() && y >= self.y && y < self.bottom_edge()
    }

    /// Aspect ratio, used to size a tablet surface so millimetres stay isotropic.
    ///
    /// A single surface stretched across differently-shaped monitors cannot do this: it is
    /// either letterboxed on one or non-square on another, and non-square means a circle drawn
    /// by hand does not come out as a circle.
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        f64::from(self.width) / f64::from(self.height)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not running under Wayland (WAYLAND_DISPLAY is unset)")]
    NoWayland,
    #[error("could not talk to the Wayland compositor: {0}")]
    Wayland(String),
    #[error("could not reach the session bus: {0}")]
    Bus(String),
    #[error("no input device named {0:?} is known to the compositor")]
    NoSuchDevice(String),
    #[error("this desktop does not support mapping a tablet to a screen from outside its settings")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Ask the compositor to confine `device` to the screen named `output`.
///
/// `device` is the uinput device name, which is the identity compositors key their per-device
/// settings off — so it has to be stable across a teardown and recreation, not derived from
/// anything volatile like the event node number.
pub fn map_tablet(device: &str, output: &str) -> Result<()> {
    // One backend, dispatched directly. A second one belongs here as another branch.
    kde::map_tablet(device, output)
}

/// Make a pointer device move one pixel per count, so emitted motion is predictable.
pub fn set_pointer_unaccelerated(device: &str) -> Result<()> {
    kde::set_pointer_unaccelerated(device)
}

/// Whether the compositor has adopted a device yet, by name.
///
/// A freshly created `uinput` device is not immediately addressable: udev has to process it and
/// the compositor has to adopt it, measured at ~50ms on a KDE session. Anything that acts on a
/// device it just created has to wait for this rather than assume.
pub fn device_known(device: &str) -> bool {
    kde::mapped_output(device).is_ok()
}

/// Whether tablet mapping can be performed on this desktop at all.
///
/// Worth asking before promising the user automatic placement, so the fallback ("map it once in
/// your display settings") can be offered up front rather than after a failure.
pub fn can_map_tablets() -> bool {
    kde::available()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, x: i32, y: i32, w: i32, h: i32) -> Output {
        Output {
            name: name.to_string(),
            description: None,
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn containment_excludes_the_far_edge() {
        // The far edge belongs to the neighbouring screen. Treating it as inclusive would put
        // one column of pixels on both, and an edge crossing would oscillate there.
        let o = out("DP-2", 1920, 0, 1280, 1024);
        assert!(o.contains(1920, 0));
        assert!(o.contains(3199, 1023));
        assert!(!o.contains(3200, 0));
        assert!(!o.contains(1919, 0));
        assert!(!o.contains(1920, 1024));
    }

    #[test]
    fn adjacent_screens_tile_without_gap_or_overlap() {
        let left = out("HDMI-A-1", 0, 0, 1920, 1080);
        let right = out("DP-2", 1920, 0, 1280, 1024);
        assert_eq!(left.right_edge(), right.x);
        assert!(!left.contains(1920, 500));
        assert!(right.contains(1920, 500));
    }

    #[test]
    fn aspect_is_per_output() {
        assert!((out("a", 0, 0, 1920, 1080).aspect() - 16.0 / 9.0).abs() < 1e-9);
        assert!((out("b", 0, 0, 1280, 1024).aspect() - 1.25).abs() < 1e-9);
    }

    #[test]
    fn a_zero_height_output_does_not_divide_by_zero() {
        assert_eq!(out("bad", 0, 0, 100, 0).aspect(), 1.0);
    }
}
