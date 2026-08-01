//! Absolute-pointer sink — the tablet modes' fallback transport.
//!
//! # Why absolute
//!
//! The compositor tracks the relative pointer's position separately from a tablet tool's, so
//! a fallback through the relative mouse resumes from wherever the pointer was last left — a
//! teleport on every transport change, and the reason D20's fallback never felt seamless. An
//! absolute pointer states its position outright on every emit, so there is nothing to
//! diverge: the mapper's position *is* the cursor's position, on either transport.
//!
//! # The device shape
//!
//! The VMware/QEMU "absolute mouse": `ABS_X`/`ABS_Y` plus the source's buttons and wheel, no
//! pen or touch bits. udev classifies it `ID_INPUT_MOUSE`, libinput drives the ordinary
//! pointer with it, and no pointer acceleration applies — absolute motion has nothing to
//! accelerate.
//!
//! Measured (P6, 2026-08-01): KWin maps the absolute range **linearly onto the desktop's
//! bounding box** — emitted fractions of the range landed at exactly `fraction × (3200, 1080)`
//! on this host's 1920×1080 + 1280×1024 layout, origin (0, 0), adopted within a second of
//! creation. The conversion below is that measurement inverted.
//!
//! The source's `REL_X`/`REL_Y` are deliberately **not** replicated here, against this crate's
//! usual rule: a device carrying both relative and absolute motion axes invites libinput to
//! classify it as something other than an absolute mouse. The relative `MouseSink` still
//! exists for everything relative.

use crate::{Error, Result, Screen};
use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent,
    KeyCode, RelativeAxisCode, UinputAbsSetup,
};

/// Full scale of the absolute range on both axes.
pub const POINTER_ABS_MAX: i32 = 65535;

pub struct PointerSink {
    device: VirtualDevice,
    name: String,
    pending: Vec<InputEvent>,
    /// Desktop bounding box the compositor maps the absolute range onto, in logical pixels.
    origin: (f64, f64),
    extent: (f64, f64),
    /// Last emitted absolute values, so an unchanged axis is never restated (P1c).
    last: (i32, i32),
}

/// The bounding box of a layout: origin and extent in logical desktop pixels.
///
/// This is the rectangle the compositor stretches the absolute range over — measured, not
/// assumed, in P6. Gaps inside it (screens of unequal height) belong to no screen; callers
/// never target them because the mapper clamps to real screens.
fn bounding_box(screens: &[Screen]) -> Option<((f64, f64), (f64, f64))> {
    let first = screens.first()?;
    let (mut x0, mut y0) = (f64::from(first.x), f64::from(first.y));
    let (mut x1, mut y1) = (x0, y0);
    for s in screens {
        x0 = x0.min(f64::from(s.x));
        y0 = y0.min(f64::from(s.y));
        x1 = x1.max(f64::from(s.x) + f64::from(s.width));
        y1 = y1.max(f64::from(s.y) + f64::from(s.height));
    }
    let extent = (x1 - x0, y1 - y0);
    if extent.0 <= 0.0 || extent.1 <= 0.0 {
        return None;
    }
    Some(((x0, y0), extent))
}

impl PointerSink {
    /// `keys` and `wheel` come from the source device, minus its motion axes — see the module
    /// doc for why `REL_X`/`REL_Y` must not be declared here.
    pub fn new(
        name: &str,
        screens: &[Screen],
        keys: &AttributeSet<KeyCode>,
        wheel: &AttributeSet<RelativeAxisCode>,
    ) -> Result<Self> {
        let (origin, extent) = bounding_box(screens).ok_or_else(|| Error::Create {
            name: name.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no screen layout to map the absolute range onto",
            ),
        })?;

        let x = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_X,
            // Resolution 0: this is not a surface with a physical size, and giving it one
            // invites classification as a tablet. VMware's virtual mouse ships the same way.
            AbsInfo::new(0, 0, POINTER_ABS_MAX, 0, 0, 0),
        );
        let y = UinputAbsSetup::new(
            AbsoluteAxisCode::ABS_Y,
            AbsInfo::new(0, 0, POINTER_ABS_MAX, 0, 0, 0),
        );

        let device = VirtualDevice::builder()
            .and_then(|b| {
                b.name(name)
                    .with_keys(keys)
                    .and_then(|b| b.with_relative_axes(wheel))
                    .and_then(|b| b.with_absolute_axis(&x))
                    .and_then(|b| b.with_absolute_axis(&y))
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
            origin,
            extent,
            // Off-scale, so the first position always emits both axes.
            last: (-1, -1),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Queue a move to a desktop-pixel position. Returns whether anything actually changed.
    ///
    /// Sub-pixel positions are welcome: the absolute range is ~20 units per pixel on this
    /// host, so the fractional part is not discarded, and there is no accumulator to carry —
    /// stating a position cannot lose motion the way truncating a delta can.
    pub fn position(&mut self, x_px: f64, y_px: f64) -> bool {
        if !x_px.is_finite() || !y_px.is_finite() {
            return false;
        }
        let ax = abs_of(x_px, self.origin.0, self.extent.0);
        let ay = abs_of(y_px, self.origin.1, self.extent.1);

        let mut moved = false;
        if ax != self.last.0 {
            self.last.0 = ax;
            self.pending.push(InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_X.0,
                ax,
            ));
            moved = true;
        }
        if ay != self.last.1 {
            self.last.1 = ay;
            self.pending.push(InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_Y.0,
                ay,
            ));
            moved = true;
        }
        moved
    }

    /// Queue a non-motion relative axis verbatim — wheel, hi-res wheel, horizontal pan.
    pub fn relative(&mut self, code: u16, value: i32) {
        if value != 0 {
            self.pending
                .push(InputEvent::new(EventType::RELATIVE.0, code, value));
        }
    }

    /// Queue a button transition. Buttons pass through unfiltered.
    pub fn key(&mut self, code: u16, pressed: bool) {
        self.pending.push(InputEvent::new(
            EventType::KEY.0,
            code,
            i32::from(pressed),
        ));
    }

    /// Flush the queued report. A no-op when nothing is queued.
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

/// A pixel position as an absolute-range value, clamped to the range.
fn abs_of(px: f64, origin: f64, extent: f64) -> i32 {
    let fraction = ((px - origin) / extent).clamp(0.0, 1.0);
    (fraction * f64::from(POINTER_ABS_MAX)).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_screens() -> Vec<Screen> {
        vec![
            Screen { name: "HDMI-A-1".into(), x: 0, y: 0, width: 1920, height: 1080 },
            Screen { name: "DP-2".into(), x: 1920, y: 0, width: 1280, height: 1024 },
        ]
    }

    #[test]
    fn the_bounding_box_spans_the_whole_layout() {
        let (origin, extent) = bounding_box(&host_screens()).unwrap();
        assert_eq!(origin, (0.0, 0.0));
        // 1920 + 1280 wide; the taller screen decides the height. This is the exact rectangle
        // P6 measured KWin stretching the absolute range over.
        assert_eq!(extent, (3200.0, 1080.0));
    }

    #[test]
    fn a_layout_left_of_the_origin_keeps_its_offset() {
        let screens = vec![Screen { name: "L".into(), x: -1920, y: 0, width: 1920, height: 1080 }];
        let (origin, extent) = bounding_box(&screens).unwrap();
        assert_eq!(origin, (-1920.0, 0.0));
        assert_eq!(extent, (1920.0, 1080.0));
    }

    #[test]
    fn no_screens_is_refused_not_invented() {
        assert!(bounding_box(&[]).is_none());
    }

    #[test]
    fn pixel_positions_invert_the_measured_mapping() {
        // P6: emitting fraction f landed the cursor at f × extent. So a pixel position must
        // convert back to fraction × POINTER_ABS_MAX.
        assert_eq!(abs_of(800.0, 0.0, 3200.0), POINTER_ABS_MAX / 4 + 1); // 0.25 rounds up
        assert_eq!(abs_of(0.0, 0.0, 3200.0), 0);
        assert_eq!(abs_of(3200.0, 0.0, 3200.0), POINTER_ABS_MAX);
    }

    #[test]
    fn positions_off_the_layout_clamp_to_the_range() {
        assert_eq!(abs_of(-50.0, 0.0, 3200.0), 0);
        assert_eq!(abs_of(9999.0, 0.0, 3200.0), POINTER_ABS_MAX);
    }

    #[test]
    fn an_unchanged_axis_is_not_restated() {
        // P1c: restating unchanged axes at 1000Hz is the event storm that doubled clicks in
        // Krita. Exercised only where a uinput device can actually be created.
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_LEFT);
        let wheel = AttributeSet::<RelativeAxisCode>::new();
        let Ok(mut sink) = PointerSink::new("StabMouse test pointer", &host_screens(), &keys, &wheel)
        else {
            return;
        };
        assert!(sink.position(800.0, 270.0), "the first position must emit");
        sink.pending.clear();
        assert!(!sink.position(800.0, 270.0), "an identical position must emit nothing");
        assert!(sink.pending.is_empty());
        // Sub-pixel motion beyond the range's resolution still counts as a change.
        assert!(sink.position(800.5, 270.0));
    }
}
