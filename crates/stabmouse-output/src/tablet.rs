//! Virtual tablet sink.
//!
//! Verified 2026-07-30: a uinput device declaring `BTN_TOOL_PEN` + `BTN_TOUCH` alongside
//! `ABS_X`/`ABS_Y`/`ABS_PRESSURE` is tagged `ID_INPUT_TABLET=1` by udev, which is what
//! libinput reads to classify a tablet tool — so KWin exposes it via `tablet_v2` and
//! Krita and Blender receive real pressure.

use crate::{Error, Result};
use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent,
    KeyCode, RelativeAxisCode, UinputAbsSetup,
};
use std::path::PathBuf;

/// Logical extent of the tablet surface. The compositor maps this onto the screen, so the
/// magnitude only sets positional resolution.
pub const SURFACE_MAX: i32 = 32767;

/// Pressure range, matching docs/stages.md.
pub const PRESSURE_MAX: i32 = 4095;

/// Units per millimetre reported for the surface. Combined with `SURFACE_MAX` this gives
/// udev a physical size; 200 puts it at ~164mm, a plausible medium tablet.
const RESOLUTION: i32 = 200;

pub struct TabletSink {
    device: VirtualDevice,
    name: String,
    pending: Vec<InputEvent>,

    // Last emitted values. Restating an unchanged axis on every report is an event storm
    // that measurably breaks application UI — it caused doubled clicks in Krita's menus
    // until fixed. See docs/modules.md.
    last_x: i32,
    last_y: i32,
    last_pressure: i32,
    last_touch: bool,
    in_proximity: bool,

    // Per-axis, not one shared maximum: a surface is sized to its screen's aspect ratio so
    // millimetres stay isotropic, which makes the two axes different lengths.
    max_x: i32,
    max_y: i32,
}

impl TabletSink {
    /// A square surface. Correct when one tablet covers the whole desktop.
    pub fn new(name: &str) -> Result<Self> {
        Self::with_extent(name, SURFACE_MAX, SURFACE_MAX)
    }

    /// A surface with explicit extents, so it can match a screen's aspect ratio.
    ///
    /// The compositor stretches the whole surface onto the screen it is mapped to. Sizing the
    /// surface to the screen's proportions makes that stretch an identity; a square surface on
    /// a 16:9 screen makes vertical hand movement travel further than horizontal, and a
    /// hand-drawn circle comes out as an ellipse.
    pub fn with_extent(name: &str, max_x: i32, max_y: i32) -> Result<Self> {
        let max_x = max_x.max(1);
        let max_y = max_y.max(1);
        let mut keys = AttributeSet::<KeyCode>::new();
        // BTN_TOOL_PEN is the bit udev keys `ID_INPUT_TABLET` off; without it this is
        // just an absolute pointer and no application will offer pressure.
        keys.insert(KeyCode::BTN_TOOL_PEN);
        keys.insert(KeyCode::BTN_TOUCH);
        keys.insert(KeyCode::BTN_STYLUS);
        keys.insert(KeyCode::BTN_STYLUS2);

        // One resolution for both axes. It is what declares the surface's physical size to
        // udev, and an axis-dependent value would describe a non-square millimetre.
        let x_axis = AbsInfo::new(0, 0, max_x, 0, 0, RESOLUTION);
        let y_axis = AbsInfo::new(0, 0, max_y, 0, 0, RESOLUTION);
        let pressure = AbsInfo::new(0, 0, PRESSURE_MAX, 0, 0, 0);

        // **The tablet carries its own wheel**, the way a real one carries a ring or a dial.
        //
        // Krita discards wheel events that arrive while a pen is in proximity — the standard
        // defence against drivers that synthesise mouse input from tablet input, which would
        // otherwise double every action. Our wheel used to come from a separate virtual mouse,
        // which Krita cannot distinguish from exactly that, so scrolling worked only while the
        // pen held still and the suppression window had expired.
        //
        // Coming from the tablet itself, there is no second device to be suspicious of.
        // Verified before relying on it (probe P9): a tablet keeps `ID_INPUT_TABLET` and stays
        // listed by KWin as a tablet tool with these axes attached — the failure that would
        // have mattered, since losing the classification would stop drawing altogether.
        let mut wheels = AttributeSet::<RelativeAxisCode>::new();
        wheels.insert(RelativeAxisCode::REL_WHEEL);
        // Hi-res as well: whole-notch scrolling feels broken for anything continuous, and a
        // device cannot gain an axis later without being recreated under a new identity.
        wheels.insert(RelativeAxisCode::REL_WHEEL_HI_RES);
        wheels.insert(RelativeAxisCode::REL_HWHEEL);
        wheels.insert(RelativeAxisCode::REL_HWHEEL_HI_RES);

        let device = VirtualDevice::builder()
            .and_then(|b| {
                b.name(name)
                    .with_keys(&keys)
                    .and_then(|b| b.with_relative_axes(&wheels))
                    .and_then(|b| {
                        b.with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, x_axis))
                    })
                    .and_then(|b| {
                        b.with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, y_axis))
                    })
                    .and_then(|b| {
                        b.with_absolute_axis(&UinputAbsSetup::new(
                            AbsoluteAxisCode::ABS_PRESSURE,
                            pressure,
                        ))
                    })
            })
            .and_then(|b| b.build())
            .map_err(|source| Error::Create {
                name: name.to_string(),
                source,
            })?;

        Ok(Self {
            device,
            name: name.to_string(),
            pending: Vec::with_capacity(8),
            // Deliberately impossible starting values so the first real report is always
            // emitted rather than being mistaken for "unchanged".
            last_x: -1,
            last_y: -1,
            last_pressure: -1,
            last_touch: false,
            in_proximity: false,
            max_x,
            max_y,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the tool is currently in proximity.
    ///
    /// Exposed so a caller can enforce "only one pointing device is live at a time" by
    /// inspection rather than by remembering every path that might have left one behind.
    pub fn in_proximity(&self) -> bool {
        self.in_proximity
    }

    pub fn nodes(&mut self) -> Vec<PathBuf> {
        self.device
            .enumerate_dev_nodes_blocking()
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    }

    /// Queue an absolute pen position, pressure and contact state.
    ///
    /// `pressure` is `0.0..=1.0`; the tablet range is applied here so callers never deal
    /// in device units.
    pub fn pen(&mut self, x: i32, y: i32, pressure: f64, touching: bool) {
        if !self.in_proximity {
            self.pending.push(InputEvent::new(
                EventType::KEY.0,
                KeyCode::BTN_TOOL_PEN.code(),
                1,
            ));
            self.in_proximity = true;
        }

        let x = x.clamp(0, self.max_x);
        let y = y.clamp(0, self.max_y);
        let p = if pressure.is_finite() {
            (pressure.clamp(0.0, 1.0) * f64::from(PRESSURE_MAX)) as i32
        } else {
            0
        };

        if x != self.last_x {
            self.pending
                .push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x));
            self.last_x = x;
        }
        if y != self.last_y {
            self.pending
                .push(InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y));
            self.last_y = y;
        }
        if p != self.last_pressure {
            self.pending.push(InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_PRESSURE.0,
                p,
            ));
            self.last_pressure = p;
        }
        if touching != self.last_touch {
            self.pending.push(InputEvent::new(
                EventType::KEY.0,
                KeyCode::BTN_TOUCH.code(),
                i32::from(touching),
            ));
            self.last_touch = touching;
        }
    }

    /// Queue a stylus barrel button.
    pub fn stylus(&mut self, second: bool, pressed: bool) {
        let code = if second {
            KeyCode::BTN_STYLUS2.code()
        } else {
            KeyCode::BTN_STYLUS.code()
        };
        self.pending
            .push(InputEvent::new(EventType::KEY.0, code, i32::from(pressed)));
    }

    /// Queue a wheel event verbatim — wheel, hi-res wheel, horizontal pan.
    ///
    /// Emitted by the tablet rather than by a separate pointer so that an application
    /// filtering mouse input during pen proximity still receives it. See `with_extent`.
    pub fn wheel(&mut self, code: u16, value: i32) {
        if value != 0 {
            self.pending
                .push(InputEvent::new(EventType::RELATIVE.0, code, value));
        }
    }

    /// Lift the pen out of proximity, used when tablet output is left.
    ///
    /// The order matters and the sequence has to be complete. A tool that goes out of
    /// proximity while still reporting pressure is an inconsistent state, and a compositor
    /// tracking a tablet tool is entitled to keep owning the cursor until it sees a coherent
    /// proximity-out — which is what a handover back to relative motion is waiting on.
    ///
    /// So: pressure to zero, then contact released, then the tool itself leaves.
    pub fn leave_proximity(&mut self) {
        if !self.in_proximity {
            return;
        }

        if self.last_pressure != 0 {
            self.pending.push(InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_PRESSURE.0,
                0,
            ));
            self.last_pressure = 0;
        }
        if self.last_touch {
            self.pending.push(InputEvent::new(
                EventType::KEY.0,
                KeyCode::BTN_TOUCH.code(),
                0,
            ));
            self.last_touch = false;
        }
        self.pending.push(InputEvent::new(
            EventType::KEY.0,
            KeyCode::BTN_TOOL_PEN.code(),
            0,
        ));
        self.in_proximity = false;

        // Forget the last emitted position so re-entering proximity restates it.
        //
        // Without this the change-detection suppresses X and Y on re-entry whenever the pen
        // comes back at the same coordinates, and the tool would enter proximity with the
        // compositor holding no position for it at all.
        self.last_x = -1;
        self.last_y = -1;
        self.last_pressure = -1;
    }

    pub fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let result = self.device.emit(&self.pending).map_err(|source| Error::Emit {
            name: self.name.clone(),
            source,
        });
        self.pending.clear();
        result
    }
}

/// Maps millimetres of hand movement onto the tablet surface.
///
/// **Hover-relative, stroke-absolute** (see docs/ux-requirements.md). While the pen is up,
/// motion is relative so the user can cross monitors and reposition freely; when a stroke
/// begins the mapping locks so smoothing and pressure see a stable coordinate space. This
/// mirrors how a real pen behaves and solves the clutch problem in the same gesture.
pub struct SurfaceMapper {
    x: f64,
    y: f64,
    units_per_mm: f64,
}

impl SurfaceMapper {
    /// `span_mm` is how much hand movement should cross the whole surface.
    pub fn new(span_mm: f64) -> Self {
        let span = if span_mm.is_finite() && span_mm > 0.0 {
            span_mm
        } else {
            200.0
        };
        Self {
            x: f64::from(SURFACE_MAX) / 2.0,
            y: f64::from(SURFACE_MAX) / 2.0,
            units_per_mm: f64::from(SURFACE_MAX) / span,
        }
    }

    pub fn set_span_mm(&mut self, span_mm: f64) {
        if span_mm.is_finite() && span_mm > 0.0 {
            self.units_per_mm = f64::from(SURFACE_MAX) / span_mm;
        }
    }

    /// Advance by a millimetre delta and return the surface position.
    pub fn advance(&mut self, dx_mm: f64, dy_mm: f64) -> (i32, i32) {
        if dx_mm.is_finite() {
            self.x = (self.x + dx_mm * self.units_per_mm).clamp(0.0, f64::from(SURFACE_MAX));
        }
        if dy_mm.is_finite() {
            self.y = (self.y + dy_mm * self.units_per_mm).clamp(0.0, f64::from(SURFACE_MAX));
        }
        (self.x as i32, self.y as i32)
    }

    pub fn position(&self) -> (i32, i32) {
        (self.x as i32, self.y as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_proximity_emits_a_complete_and_ordered_sequence() {
        // A tool that leaves proximity while still reporting pressure is an inconsistent
        // state, and a compositor tracking it is entitled to keep owning the cursor.
        let mut t = match TabletSink::new("StabMouse test proximity") {
            Ok(t) => t,
            // uinput is unavailable in some environments; the logic is covered by the
            // ordering assertions below when it is.
            Err(_) => return,
        };

        t.pen(100, 200, 0.8, true);
        t.pending.clear();

        t.leave_proximity();

        let codes: Vec<(u16, u16, i32)> = t
            .pending
            .iter()
            .map(|e| (e.event_type().0, e.code(), e.value()))
            .collect();

        let pressure_at = codes
            .iter()
            .position(|c| c.0 == EventType::ABSOLUTE.0 && c.1 == AbsoluteAxisCode::ABS_PRESSURE.0);
        let touch_at = codes
            .iter()
            .position(|c| c.0 == EventType::KEY.0 && c.1 == KeyCode::BTN_TOUCH.code());
        let tool_at = codes
            .iter()
            .position(|c| c.0 == EventType::KEY.0 && c.1 == KeyCode::BTN_TOOL_PEN.code());

        assert!(pressure_at.is_some(), "pressure must be zeroed: {codes:?}");
        assert!(touch_at.is_some(), "contact must be released: {codes:?}");
        assert!(tool_at.is_some(), "the tool must leave: {codes:?}");
        assert!(
            pressure_at < touch_at && touch_at < tool_at,
            "order must be pressure, then touch, then tool: {codes:?}"
        );
        assert!(
            codes.iter().all(|c| c.2 == 0),
            "every value in a proximity-out is zero: {codes:?}"
        );
    }

    #[test]
    fn re_entering_proximity_restates_the_position() {
        let mut t = match TabletSink::new("StabMouse test re-entry") {
            Ok(t) => t,
            Err(_) => return,
        };

        t.pen(500, 600, 0.0, false);
        t.pending.clear();
        t.leave_proximity();
        t.pending.clear();

        // Same coordinates as before. Change-detection would suppress them, leaving the tool
        // in proximity with no position ever sent.
        t.pen(500, 600, 0.0, false);
        let has_x = t
            .pending
            .iter()
            .any(|e| e.event_type() == EventType::ABSOLUTE && e.code() == AbsoluteAxisCode::ABS_X.0);
        assert!(has_x, "position must be restated on re-entry: {:?}", t.pending.len());
    }

    #[test]
    fn the_mapper_starts_centred() {
        let m = SurfaceMapper::new(200.0);
        let (x, y) = m.position();
        assert_eq!(x, SURFACE_MAX / 2);
        assert_eq!(y, SURFACE_MAX / 2);
    }

    #[test]
    fn a_full_span_of_hand_movement_crosses_the_surface() {
        let mut m = SurfaceMapper::new(100.0);
        // Half a span right from centre reaches the edge.
        let (x, _) = m.advance(50.0, 0.0);
        assert_eq!(x, SURFACE_MAX, "50mm of a 100mm span should reach the edge");
    }

    #[test]
    fn the_surface_clamps_rather_than_wrapping() {
        let mut m = SurfaceMapper::new(100.0);
        let (x, y) = m.advance(1_000.0, -1_000.0);
        assert_eq!(x, SURFACE_MAX);
        assert_eq!(y, 0);
        // And it recovers when moved back.
        let (x, _) = m.advance(-50.0, 0.0);
        assert!(x < SURFACE_MAX);
    }

    #[test]
    fn non_finite_deltas_leave_the_position_intact() {
        let mut m = SurfaceMapper::new(100.0);
        let before = m.position();
        let after = m.advance(f64::NAN, f64::INFINITY);
        assert_eq!(before, after);
    }

    #[test]
    fn an_invalid_span_falls_back_instead_of_dividing_by_zero() {
        for bad in [0.0, -5.0, f64::NAN] {
            let mut m = SurfaceMapper::new(bad);
            let (x, _) = m.advance(1.0, 0.0);
            assert!(x > 0 && x <= SURFACE_MAX, "span {bad} produced {x}");
        }
    }
}
