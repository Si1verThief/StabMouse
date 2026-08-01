//! Device discovery and exclusive capture.
//!
//! # Grab safety
//!
//! An exclusive grab on the user's only pointing device is the most dangerous thing this
//! program does: if it is held while nothing is emitted, the mouse is dead system-wide.
//! Three properties keep that recoverable, in order of importance:
//!
//! 1. **The kernel releases the grab when the file descriptor closes**, and fds close when
//!    the process dies — by signal, by `abort()`, by OOM kill, by anything. So process
//!    death is always safe, whether or not `Drop` runs.
//! 2. `Drop` releases explicitly, covering an unwinding panic while the process survives.
//! 3. **The keyboard is never grabbed** — enforced here in code, not by convention — so
//!    the user always retains a way to reach a terminal or the panic hotkey.
//!
//! The residual risk is a daemon that is alive but wedged while holding a grab. That is
//! the watchdog's job (see docs/modules.md), not this crate's.

mod listen;

pub use listen::{code_for, is_mouse_button, Listener};

use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use stabmouse_config::Identity;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("opening {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "cannot read {path}: permission denied. Add yourself to the 'input' group \
         (`sudo usermod -aG input $USER`) and log out and back in"
    )]
    Permission { path: PathBuf },

    #[error("grabbing {path}: {source}")]
    Grab {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("refusing to grab {path} ({name}): it has keyboard keys, and grabbing the keyboard would remove the user's only escape route")]
    WouldGrabKeyboard { path: PathBuf, name: String },

    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "no readable keyboard carries that key, so a keyboard modifier cannot work. \
         Bind a mouse button instead, or check 'input' group membership"
    )]
    NoKeyboard,
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a device appears to be, for listing and for safety checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Keyboard,
    TabletPen,
    AbsolutePointer,
    Other,
}

#[derive(Debug, Clone)]
pub struct Discovered {
    pub path: PathBuf,
    pub name: String,
    pub identity: Identity,
    pub kind: Kind,
    pub keys: AttributeSet<KeyCode>,
    pub relative_axes: AttributeSet<RelativeAxisCode>,
}

impl Discovered {
    /// Whether grabbing this device would take the keyboard with it.
    ///
    /// Checked against letter keys rather than a device-name heuristic: many mice present
    /// a keyboard collection for their macro keys, and a name match would miss those while
    /// a bit check catches them.
    pub fn is_keyboard_like(&self) -> bool {
        [KeyCode::KEY_A, KeyCode::KEY_Q, KeyCode::KEY_SPACE, KeyCode::KEY_ENTER]
            .iter()
            .any(|k| self.keys.contains(*k))
    }
}

/// Everything under `/dev/input` that can be read, with identities extracted.
///
/// Unreadable devices are skipped rather than reported: on a normal desktop several nodes
/// belong to other users or to hardware this program has no business touching.
pub fn enumerate() -> Vec<Discovered> {
    let mut out: Vec<Discovered> = evdev::enumerate()
        .map(|(path, device)| describe(path, &device))
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// `supported_*` hands back a borrowed `AttributeSetRef`, which is not `Clone`. The sinks
/// need owned sets that outlive the source device, so copy the bits across.
fn own_keys(set: Option<&evdev::AttributeSetRef<KeyCode>>) -> AttributeSet<KeyCode> {
    let mut out = AttributeSet::new();
    if let Some(s) = set {
        for k in s.iter() {
            out.insert(k);
        }
    }
    out
}

fn own_rel(set: Option<&evdev::AttributeSetRef<RelativeAxisCode>>) -> AttributeSet<RelativeAxisCode> {
    let mut out = AttributeSet::new();
    if let Some(s) = set {
        for a in s.iter() {
            out.insert(a);
        }
    }
    out
}

fn describe(path: PathBuf, device: &Device) -> Discovered {
    let id = device.input_id();
    let keys = own_keys(device.supported_keys());
    let relative_axes = own_rel(device.supported_relative_axes());

    let has_rel = relative_axes.contains(RelativeAxisCode::REL_X);
    let has_abs = device
        .supported_absolute_axes()
        .is_some_and(|a| a.contains(evdev::AbsoluteAxisCode::ABS_X));
    let has_pen = keys.contains(KeyCode::BTN_TOOL_PEN);
    let has_click = keys.contains(KeyCode::BTN_LEFT);
    let has_letters = keys.contains(KeyCode::KEY_A);

    let kind = if has_pen {
        Kind::TabletPen
    } else if has_rel && has_click {
        Kind::Mouse
    } else if has_abs && has_click {
        Kind::AbsolutePointer
    } else if has_letters {
        Kind::Keyboard
    } else {
        Kind::Other
    };

    Discovered {
        path,
        name: device.name().unwrap_or("<unnamed>").to_string(),
        identity: Identity {
            // Lower-case 4-digit hex, matching what a user writes in config.
            vid: Some(format!("{:04x}", id.vendor())),
            pid: Some(format!("{:04x}", id.product())),
            serial: device
                .unique_name()
                .map(str::to_string)
                .filter(|s| !s.is_empty()),
        },
        kind,
        keys,
        relative_axes,
    }
}

/// An open device, optionally grabbed exclusively.
pub struct Capture {
    device: Device,
    path: PathBuf,
    name: String,
    grabbed: bool,
}

impl Capture {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let device = Device::open(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::PermissionDenied {
                Error::Permission { path: path.clone() }
            } else {
                Error::Open {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        let name = device.name().unwrap_or("<unnamed>").to_string();
        Ok(Self {
            device,
            path,
            name,
            grabbed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_grabbed(&self) -> bool {
        self.grabbed
    }

    /// The underlying descriptor, so a caller can wait on it with a deadline.
    ///
    /// Exposed because the hot loop must wake on either input or a tick deadline, and
    /// `read()` alone can only do the first.
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.device.as_raw_fd()
    }

    pub fn keys(&self) -> AttributeSet<KeyCode> {
        own_keys(self.device.supported_keys())
    }

    pub fn relative_axes(&self) -> AttributeSet<RelativeAxisCode> {
        own_rel(self.device.supported_relative_axes())
    }

    /// Take exclusive control, so the compositor stops seeing the physical device.
    ///
    /// **Refuses on anything with letter keys.** This is the code-level form of "never
    /// grab the keyboard": without it, one config mistake naming the wrong event node
    /// would take away the user's keyboard *and* their mouse simultaneously, leaving no
    /// way to recover short of a hard reboot.
    pub fn grab(&mut self) -> Result<()> {
        if self.grabbed {
            return Ok(());
        }
        let keys = self.keys();
        if [KeyCode::KEY_A, KeyCode::KEY_Q, KeyCode::KEY_SPACE, KeyCode::KEY_ENTER]
            .iter()
            .any(|k| keys.contains(*k))
        {
            return Err(Error::WouldGrabKeyboard {
                path: self.path.clone(),
                name: self.name.clone(),
            });
        }

        self.device.grab().map_err(|source| Error::Grab {
            path: self.path.clone(),
            source,
        })?;
        self.grabbed = true;
        Ok(())
    }

    pub fn ungrab(&mut self) {
        if self.grabbed {
            // Nothing useful to do on failure: the fd close in `Drop` is the backstop, and
            // process death is the backstop to that.
            let _ = self.device.ungrab();
            self.grabbed = false;
        }
    }

    /// Block until events are available, then yield them.
    pub fn read(&mut self) -> Result<impl Iterator<Item = InputEvent> + '_> {
        self.device.fetch_events().map_err(|source| Error::Read {
            path: self.path.clone(),
            source,
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.ungrab();
    }
}

/// The interesting parts of one report, accumulated across an evdev sync group.
///
/// evdev delivers a report as several events terminated by `SYN_REPORT`; treating them
/// individually would process an X move and its matching Y move as two separate samples.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub dx: i32,
    pub dy: i32,
    /// Relative axes other than X/Y — wheel, hi-res wheel, pan — forwarded verbatim.
    pub other_relative: Vec<(u16, i32)>,
    pub keys: Vec<(u16, bool)>,
    /// Source timestamp in microseconds. Never a clock read at processing time, so replay
    /// reproduces live behaviour exactly.
    pub t_us: u64,
    pub complete: bool,
}

impl Report {
    /// Fold one event in. Returns true when `SYN_REPORT` completes the report.
    pub fn accumulate(&mut self, event: &InputEvent) -> bool {
        match event.event_type() {
            EventType::RELATIVE => {
                let code = RelativeAxisCode(event.code());
                if code == RelativeAxisCode::REL_X {
                    self.dx += event.value();
                } else if code == RelativeAxisCode::REL_Y {
                    self.dy += event.value();
                } else {
                    self.other_relative.push((event.code(), event.value()));
                }
                false
            }
            EventType::KEY => {
                self.keys.push((event.code(), event.value() != 0));
                false
            }
            EventType::SYNCHRONIZATION => {
                self.t_us = timestamp_us(event);
                self.complete = true;
                true
            }
            _ => false,
        }
    }

    pub fn clear(&mut self) {
        self.dx = 0;
        self.dy = 0;
        self.other_relative.clear();
        self.keys.clear();
        self.complete = false;
    }
}

fn timestamp_us(event: &InputEvent) -> u64 {
    event
        .timestamp()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventType, code: u16, value: i32) -> InputEvent {
        InputEvent::new(kind.0, code, value)
    }

    #[test]
    fn a_report_accumulates_until_syn() {
        let mut r = Report::default();
        assert!(!r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 3)));
        assert!(!r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_Y.0, -2)));
        assert!(!r.accumulate(&ev(EventType::KEY, KeyCode::BTN_LEFT.code(), 1)));
        assert!(r.accumulate(&ev(EventType::SYNCHRONIZATION, 0, 0)));

        assert_eq!((r.dx, r.dy), (3, -2));
        assert_eq!(r.keys, vec![(KeyCode::BTN_LEFT.code(), true)]);
        assert!(r.complete);
    }

    #[test]
    fn repeated_axis_events_in_one_report_are_summed() {
        let mut r = Report::default();
        r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 2));
        r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 3));
        r.accumulate(&ev(EventType::SYNCHRONIZATION, 0, 0));
        assert_eq!(r.dx, 5);
    }

    #[test]
    fn wheel_axes_are_kept_separate_from_pointer_motion() {
        let mut r = Report::default();
        r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, 1));
        r.accumulate(&ev(
            EventType::RELATIVE,
            RelativeAxisCode::REL_WHEEL_HI_RES.0,
            120,
        ));
        r.accumulate(&ev(EventType::SYNCHRONIZATION, 0, 0));

        assert_eq!((r.dx, r.dy), (0, 0), "wheel must not become pointer motion");
        assert_eq!(r.other_relative.len(), 2, "including hi-res wheel");
    }

    #[test]
    fn clear_resets_everything_except_the_timestamp() {
        let mut r = Report::default();
        r.accumulate(&ev(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 9));
        r.accumulate(&ev(EventType::SYNCHRONIZATION, 0, 0));
        r.clear();
        assert_eq!((r.dx, r.dy), (0, 0));
        assert!(r.other_relative.is_empty());
        assert!(r.keys.is_empty());
        assert!(!r.complete);
    }

    #[test]
    fn enumerate_does_not_panic_and_classifies_something() {
        // Runs against whatever the machine has; asserts only that discovery is safe and
        // that identities are well formed.
        for d in enumerate() {
            assert!(!d.name.is_empty());
            if let Some(vid) = &d.identity.vid {
                assert_eq!(vid.len(), 4, "vid should be 4 hex digits, got {vid}");
            }
        }
    }

    #[test]
    fn keyboards_are_recognised_as_such_wherever_they_appear() {
        // Not asserting the machine has one, only that anything with letter keys is
        // classified so the grab refusal will trigger.
        for d in enumerate() {
            if d.is_keyboard_like() {
                assert!(
                    d.kind == Kind::Keyboard || d.keys.contains(KeyCode::BTN_LEFT),
                    "{} has letter keys but was classified {:?}",
                    d.name,
                    d.kind
                );
            }
        }
    }
}
