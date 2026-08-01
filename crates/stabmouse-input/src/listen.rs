//! Watching a few named keys, without grabbing anything.
//!
//! # The rule this is careful about
//!
//! StabMouse **never grabs the keyboard** — that is the project's first working rule, because
//! a daemon that dies holding both the mouse and the keyboard leaves no way out. Reading is a
//! different operation from grabbing: an ungrabbed reader sees events that are also delivered
//! to everyone else, takes nothing away, and disappears without trace when the fd closes. No
//! `EVIOCGRAB` is issued here, and there is no code path that could.
//!
//! # But reading a keyboard is still a serious capability
//!
//! A process reading `/dev/input/event*` for a keyboard can see every keystroke, which is the
//! shape of a keylogger whatever the intent. So this is deliberately built to be unable to do
//! that even by accident:
//!
//! - It is **opt-in**, existing only when a mode binds a keyboard key.
//! - It records **only the codes it was asked to watch**, as a pressed/not-pressed bit each.
//!   Every other event is discarded at the point of comparison, never stored, never counted,
//!   never logged.
//! - It keeps **no history**: not a buffer, not a timestamp, not a sequence. Only "is this one
//!   key down right now", which is the entire question a modifier asks.
//! - It opens keyboards **read-only** (`O_RDONLY`), so the file descriptor itself cannot write
//!   to the device or take it from anyone.
//!
//! A mouse-button binding needs none of this — the grabbed device's buttons are already in
//! front of us — so the listener is simply never constructed for one.

use crate::{Error, Result};
use evdev::{Device, EventType, KeyCode};
use std::os::fd::RawFd;
use std::path::PathBuf;

/// Keyboards being watched for a small set of keys.
pub struct Listener {
    devices: Vec<Watched>,
    /// The only codes this listener will ever record. Anything else is discarded on sight.
    watching: Vec<u16>,
    /// Parallel to `watching`: whether each is currently held.
    held: Vec<bool>,
}

struct Watched {
    device: Device,
    path: PathBuf,
}

impl Listener {
    /// Open every keyboard carrying one of `codes`, read-only, ungrabbed.
    ///
    /// Every keyboard rather than one: laptops have a built-in keyboard and an external one,
    /// and a modifier must work from whichever the user actually pressed. A device that cannot
    /// be opened is skipped rather than fatal — a modifier that works on one keyboard and not
    /// another is a far better outcome than a daemon that refuses to start.
    pub fn watching(codes: &[u16]) -> Result<Self> {
        if codes.is_empty() {
            return Ok(Self {
                devices: Vec::new(),
                watching: Vec::new(),
                held: Vec::new(),
            });
        }

        let mut devices = Vec::new();
        for (path, device) in evdev::enumerate() {
            let carries = device
                .supported_keys()
                .is_some_and(|keys| codes.iter().any(|c| keys.contains(KeyCode::new(*c))));
            if !carries {
                continue;
            }
            // Non-blocking so draining a descriptor that has gone quiet cannot stall the hot
            // loop. The loop only reads what `poll` reported readable, but a device can be
            // drained by something else between the two.
            match Device::open(&path).and_then(|d| {
                d.set_nonblocking(true)?;
                Ok(d)
            }) {
                Ok(device) => devices.push(Watched { device, path }),
                Err(e) => eprintln!("cannot watch {} for a modifier: {e}", path.display()),
            }
        }

        if devices.is_empty() {
            return Err(Error::NoKeyboard);
        }

        Ok(Self {
            devices,
            watching: codes.to_vec(),
            held: vec![false; codes.len()],
        })
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Descriptors for the caller's `poll` set, in the order [`Listener::drain`] indexes them.
    pub fn raw_fds(&self) -> Vec<RawFd> {
        use std::os::fd::AsRawFd;
        self.devices.iter().map(|w| w.device.as_raw_fd()).collect()
    }

    pub fn paths(&self) -> Vec<&std::path::Path> {
        self.devices.iter().map(|w| w.path.as_path()).collect()
    }

    /// Read pending events from one watched keyboard and update the held bits.
    ///
    /// This is the only place keyboard events are ever looked at, and it looks at exactly one
    /// property of them: whether the code is one of the few being watched. Everything else
    /// falls off the end of the loop.
    pub fn drain(&mut self, index: usize) {
        let Some(watched) = self.devices.get_mut(index) else {
            return;
        };
        let Ok(events) = watched.device.fetch_events() else {
            return;
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            // The comparison is the filter. A code that is not watched never reaches a
            // variable, a counter or a log line.
            if let Some(slot) = self.watching.iter().position(|c| *c == event.code()) {
                // 0 release, 1 press, 2 autorepeat — autorepeat means still held.
                self.held[slot] = event.value() != 0;
            }
        }
    }

    /// Whether a watched code is held right now.
    pub fn is_held(&self, code: u16) -> bool {
        self.watching
            .iter()
            .position(|c| *c == code)
            .is_some_and(|slot| self.held[slot])
    }

    /// Whether any watched code is held.
    pub fn any_held(&self) -> bool {
        self.held.iter().any(|h| *h)
    }
}

/// Resolve a binding name to an evdev code — `"KEY_LEFTSHIFT"`, `"BTN_SIDE"`.
///
/// Case-insensitive, and the `KEY_`/`BTN_` prefix may be omitted, because a config file is
/// written by a person and `side` is what a person types.
pub fn code_for(name: &str) -> Option<u16> {
    let cleaned = name.trim().to_ascii_uppercase().replace(['-', ' '], "_");
    let candidates = [
        cleaned.clone(),
        format!("KEY_{cleaned}"),
        format!("BTN_{cleaned}"),
    ];
    // evdev's `from_str` covers the whole table, so no hand-written list can drift from it.
    candidates
        .iter()
        .find_map(|c| c.parse::<KeyCode>().ok())
        .map(|k| k.code())
}

/// Whether a code is a mouse button, and therefore already visible on the grabbed device.
///
/// `BTN_LEFT..=BTN_TASK` is the mouse button block — `BTN_LEFT` is what the kernel also calls
/// `BTN_MOUSE`, the start of the range. Anything else that a mouse happens to carry — a
/// keyboard key on a macro pad, say — is treated as a keyboard binding and watched, which is
/// the safe direction: watching a key the source device also reports costs nothing, while
/// *not* watching one that only a keyboard has would silently never fire.
pub fn is_mouse_button(code: u16) -> bool {
    (KeyCode::BTN_LEFT.code()..=KeyCode::BTN_TASK.code()).contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_names_resolve_with_or_without_a_prefix() {
        assert_eq!(code_for("KEY_LEFTSHIFT"), Some(KeyCode::KEY_LEFTSHIFT.code()));
        assert_eq!(code_for("leftshift"), Some(KeyCode::KEY_LEFTSHIFT.code()));
        assert_eq!(code_for("BTN_SIDE"), Some(KeyCode::BTN_SIDE.code()));
        assert_eq!(code_for("side"), Some(KeyCode::BTN_SIDE.code()));
        assert_eq!(code_for("  Btn_Extra "), Some(KeyCode::BTN_EXTRA.code()));
    }

    #[test]
    fn nonsense_resolves_to_nothing_rather_than_a_wrong_key() {
        assert_eq!(code_for("not_a_key"), None);
        assert_eq!(code_for(""), None);
    }

    #[test]
    fn mouse_buttons_are_told_apart_from_keyboard_keys() {
        // The distinction decides whether a keyboard is opened at all, so it must not be
        // approximate.
        for name in ["BTN_LEFT", "BTN_RIGHT", "BTN_MIDDLE", "BTN_SIDE", "BTN_EXTRA"] {
            assert!(is_mouse_button(code_for(name).unwrap()), "{name} is a mouse button");
        }
        for name in ["KEY_LEFTSHIFT", "KEY_LEFTCTRL", "KEY_SPACE", "KEY_A"] {
            assert!(!is_mouse_button(code_for(name).unwrap()), "{name} is not a mouse button");
        }
    }

    #[test]
    fn watching_nothing_opens_nothing() {
        // The privacy guarantee that matters most: no binding, no keyboard opened at all.
        let l = Listener::watching(&[]).unwrap();
        assert!(l.is_empty());
        assert!(l.raw_fds().is_empty());
        assert!(!l.any_held());
    }

    #[test]
    fn an_unwatched_code_is_never_reported_as_held() {
        let l = Listener::watching(&[]).unwrap();
        assert!(!l.is_held(KeyCode::KEY_A.code()));
    }
}
