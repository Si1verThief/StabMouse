//! The D-Bus contract, shared by the daemon, the CLI and the GUI.
//!
//! Implements [docs/api.md](../../../docs/api.md). **This is a public API** — third-party tools
//! calling it are an intended use, not an accident, and the ratbagd seam in D7 depends on
//! exactly that. So the names and signatures here are not free to churn: additive changes only
//! within a major version, signalled by [`INTERFACE_VERSION`].
//!
//! One crate rather than definitions repeated on each side, because a wire contract written
//! twice is a wire contract that will eventually disagree with itself.

pub mod client;

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

/// Session bus, because the daemon is per-user. A system service would have to arbitrate
/// between logged-in users for a device only one of them is holding.
pub const BUS_NAME: &str = "io.github.si1verthief.StabMouse";
pub const OBJECT_PATH: &str = "/io/github/si1verthief/StabMouse";

pub const DAEMON_INTERFACE: &str = "io.github.si1verthief.StabMouse.Daemon";
pub const DEVICES_INTERFACE: &str = "io.github.si1verthief.StabMouse.Devices";
pub const CONFIG_INTERFACE: &str = "io.github.si1verthief.StabMouse.Config";

/// Bumped only for a breaking change. Additive changes leave it alone, which is what lets a
/// client written against version 1 keep working against a daemon that has grown methods.
pub const INTERFACE_VERSION: u32 = 1;

/// Process exit codes, fixed by docs/api.md.
///
/// [`ExitCode::Permission`] is separate from a generic error because a permission failure is
/// the single most likely first-run problem — not being in the `input` group — and deserves a
/// scriptable outcome rather than being folded in with everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    Error = 1,
    Usage = 2,
    Permission = 3,
    NoDaemon = 4,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// A device as the daemon sees it.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable identifier used by `SetManaged` and `SetResolution`.
    pub id: String,
    pub name: String,
    pub path: String,
    /// Whether StabMouse filters this device. Opt-in — see D8.
    pub managed: bool,
    /// Active DPI, as last learned. Zero when unknown, which is different from "1000 and we
    /// are sure": a caller deciding whether to prompt needs to tell those apart.
    pub dpi: u32,
}

/// One mode slot.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct ModeInfo {
    /// 1-based, matching what the user sees and what `SetMode` takes.
    pub slot: u32,
    pub name: String,
    /// `mouse` or `tablet`.
    pub output: String,
    pub preset: String,
}

/// Why output is degraded, when it is.
///
/// Carries the "Limited — no pressure" state from ux-requirements.md. A string rather than an
/// enum on the wire so a daemon can report a cause a client predates without the client
/// failing to parse it.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq, Default)]
pub struct Degraded {
    pub degraded: bool,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no StabMouse daemon is running")]
    NoDaemon,
    #[error("the daemon refused: {0}")]
    Refused(String),
    #[error("d-bus: {0}")]
    Bus(String),
}

impl Error {
    /// The exit code a CLI should use for this failure.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            // Distinguished so a script can retry or start the daemon rather than treating an
            // absent daemon as a hard failure.
            Error::NoDaemon => ExitCode::NoDaemon,
            _ => ExitCode::Error,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_name_and_path_agree() {
        // A path is the bus name with dots replaced by slashes; a mismatch is the kind of
        // thing that only shows up as a mysterious "no such object" at runtime.
        let derived = format!("/{}", BUS_NAME.replace('.', "/"));
        assert_eq!(derived, OBJECT_PATH);
    }

    #[test]
    fn interfaces_are_children_of_the_bus_name() {
        for iface in [DAEMON_INTERFACE, DEVICES_INTERFACE, CONFIG_INTERFACE] {
            assert!(iface.starts_with(BUS_NAME), "{iface} is not under {BUS_NAME}");
        }
    }

    #[test]
    fn exit_codes_match_the_documented_table() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::Error.code(), 1);
        assert_eq!(ExitCode::Usage.code(), 2);
        assert_eq!(ExitCode::Permission.code(), 3);
        assert_eq!(ExitCode::NoDaemon.code(), 4);
    }

    #[test]
    fn an_absent_daemon_is_its_own_exit_code() {
        assert_eq!(Error::NoDaemon.exit_code(), ExitCode::NoDaemon);
        assert_eq!(Error::Refused("no".into()).exit_code(), ExitCode::Error);
    }
}
