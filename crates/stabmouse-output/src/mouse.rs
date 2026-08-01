//! Relative-pointer sink.

use crate::{Error, Result};
use evdev::{
    uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
};
use std::path::PathBuf;

/// A virtual mouse carrying whatever the source device carried.
///
/// Buttons and relative axes are copied wholesale from the source rather than assumed.
/// A hand-written list would work for the common case and quietly lose hi-res scroll,
/// horizontal wheel, or extra buttons on anything unusual — and the user would experience
/// that as "StabMouse broke my mouse" with no obvious cause.
pub struct MouseSink {
    device: VirtualDevice,
    name: String,
    pending: Vec<InputEvent>,
}

impl MouseSink {
    pub fn new(
        name: &str,
        keys: &AttributeSet<KeyCode>,
        axes: &AttributeSet<RelativeAxisCode>,
    ) -> Result<Self> {
        let device = VirtualDevice::builder()
            .and_then(|b| {
                b.name(name)
                    .with_keys(keys)
                    .and_then(|b| b.with_relative_axes(axes))
            })
            .and_then(|b| b.build())
            .map_err(|source| Error::Create {
                name: name.to_string(),
                source,
            })?;

        Ok(Self {
            device,
            name: name.to_string(),
            // Sized for the realistic worst case of one report: two axes, a wheel pair
            // and a button. Never grows during operation, so `emit` does not allocate.
            pending: Vec::with_capacity(8),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `/dev/input/eventN` nodes this device appears as.
    pub fn nodes(&mut self) -> Vec<PathBuf> {
        self.device
            .enumerate_dev_nodes_blocking()
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    }

    /// Queue relative motion. Zero deltas are dropped rather than emitted.
    pub fn motion(&mut self, dx: i32, dy: i32) {
        if dx != 0 {
            self.pending.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_X.0,
                dx,
            ));
        }
        if dy != 0 {
            self.pending.push(InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_Y.0,
                dy,
            ));
        }
    }

    /// Queue any other relative axis verbatim — wheel, hi-res wheel, horizontal pan.
    ///
    /// These are forwarded untouched: the filters operate on pointer motion, and
    /// reinterpreting a scroll event as motion would be wrong.
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
