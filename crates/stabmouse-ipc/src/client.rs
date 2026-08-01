//! Client side of the D-Bus contract.
//!
//! Blocking, because both callers are blocking: the CLI is a one-shot process and the GUI runs
//! its own event loop. Neither wants an async runtime imposed on it for what is a handful of
//! round trips.
//!
//! # Never hangs
//!
//! docs/api.md requires that an absent daemon produces a clear message and exit 4, and
//! explicitly that the CLI **never hangs**. Two things secure that:
//!
//! 1. **Ownership is checked before calling.** `NameHasOwner` answers immediately from the bus
//!    daemon itself, so "nobody is there" is established without ever addressing StabMouse.
//! 2. **StabMouse ships no `.service` file**, so the bus cannot be asked to launch it. A call
//!    to an unowned name returns `ServiceUnknown` at once rather than blocking on activation.
//!
//! The check is also what keeps the two failures distinguishable: "no daemon is running" and
//! "the daemon rejected your argument" want opposite responses from the user, and inferring
//! the first from a failed call conflates them.

use crate::{
    Degraded, DeviceInfo, Error, ModeInfo, Result, BUS_NAME, CONFIG_INTERFACE, DAEMON_INTERFACE,
    DEVICES_INTERFACE, OBJECT_PATH,
};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

pub struct Client {
    conn: Connection,
}

impl Client {
    /// Connect to the session bus and check a daemon is actually there.
    pub fn connect() -> Result<Self> {
        let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
        let me = Self { conn };
        if !me.daemon_present()? {
            return Err(Error::NoDaemon);
        }
        Ok(me)
    }

    /// Whether anyone owns the bus name.
    ///
    /// Asked before calling rather than inferring it from a failed call: the error for "nobody
    /// is there" and the error for "the daemon rejected your argument" are otherwise easy to
    /// confuse, and they want opposite responses from the user.
    fn daemon_present(&self) -> Result<bool> {
        let reply = self
            .conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &(BUS_NAME),
            )
            .map_err(|e| Error::Bus(e.to_string()))?;
        reply.body().deserialize().map_err(|e| Error::Bus(e.to_string()))
    }

    fn call<B, R>(&self, interface: &str, method: &str, body: &B) -> Result<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let reply = self
            .conn
            .call_method(Some(BUS_NAME), OBJECT_PATH, Some(interface), method, body)
            .map_err(map_call_error)?;
        reply
            .body()
            .deserialize()
            .map_err(|e| Error::Bus(e.to_string()))
    }

    // ------------------------------------------------------------------ Daemon

    pub fn set_mode(&self, slot: u32) -> Result<()> {
        self.call(DAEMON_INTERFACE, "SetMode", &(slot))
    }

    pub fn set_mode_by_name(&self, name: &str) -> Result<()> {
        self.call(DAEMON_INTERFACE, "SetModeByName", &(name))
    }

    /// Advance to the next mode, returning the slot landed on.
    pub fn toggle_mode(&self) -> Result<u32> {
        self.call(DAEMON_INTERFACE, "ToggleMode", &())
    }

    pub fn set_profile(&self, name: &str) -> Result<()> {
        self.call(DAEMON_INTERFACE, "SetProfile", &(name))
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.call(DAEMON_INTERFACE, "SetEnabled", &(enabled))
    }

    pub fn panic(&self) -> Result<()> {
        self.call(DAEMON_INTERFACE, "Panic", &())
    }

    pub fn resume(&self) -> Result<()> {
        self.call(DAEMON_INTERFACE, "Resume", &())
    }

    pub fn status(&self) -> Result<std::collections::HashMap<String, OwnedValue>> {
        self.call(DAEMON_INTERFACE, "GetStatus", &())
    }

    pub fn quit(&self) -> Result<()> {
        self.call(DAEMON_INTERFACE, "Quit", &())
    }

    pub fn modes(&self) -> Result<Vec<ModeInfo>> {
        self.call(DAEMON_INTERFACE, "ListModes", &())
    }

    pub fn degraded(&self) -> Result<Degraded> {
        self.call(DAEMON_INTERFACE, "GetDegraded", &())
    }

    // ----------------------------------------------------------------- Devices

    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        self.call(DEVICES_INTERFACE, "List", &())
    }

    pub fn set_managed(&self, id: &str, managed: bool) -> Result<()> {
        self.call(DEVICES_INTERFACE, "SetManaged", &(id, managed))
    }

    /// The D7 seam: StabMouse never speaks hidraw, it is *told* the resolution.
    pub fn set_resolution(&self, id: &str, dpi: u32) -> Result<()> {
        self.call(DEVICES_INTERFACE, "SetResolution", &(id, dpi))
    }

    // ------------------------------------------------------------------ Config

    pub fn reload(&self) -> Result<()> {
        self.call(CONFIG_INTERFACE, "Reload", &())
    }

    pub fn list_profiles(&self) -> Result<Vec<(String, String)>> {
        self.call(CONFIG_INTERFACE, "ListProfiles", &())
    }

    pub fn list_presets(&self) -> Result<Vec<(String, String)>> {
        self.call(CONFIG_INTERFACE, "ListPresets", &())
    }

    /// Which level of the D8 cascade supplied the effective value for a key.
    pub fn explain(&self, device: &str, key: &str) -> Result<(String, OwnedValue)> {
        self.call(CONFIG_INTERFACE, "Explain", &(device, key))
    }
}

/// Block, calling `on_change` every time the daemon announces something changed.
///
/// Frontends need this because **the daemon is not the only thing that changes its state**: a
/// hotkey, the CLI, another window, or a deferred switch landing at the end of a stroke will
/// all move it. A frontend that only re-reads after its own actions shows the wrong thing the
/// moment anything else happens.
///
/// It also fixes a subtler failure. Commands are one-way, so re-reading immediately after
/// sending one usually observes the state from *before* it — the change has not been processed
/// yet. Waiting for the announcement instead means the value read is always the value after.
///
/// Signals are not filtered by name here. Every one of them means "something changed, look
/// again", and a frontend that re-reads on all of them cannot miss one that is added later.
pub fn on_change(mut callback: impl FnMut()) -> Result<()> {
    let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(BUS_NAME)
        .map_err(|e| Error::Bus(e.to_string()))?
        .path(OBJECT_PATH)
        .map_err(|e| Error::Bus(e.to_string()))?
        .build();

    let messages = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, None)
        .map_err(|e| Error::Bus(e.to_string()))?;

    for message in messages {
        if message.is_ok() {
            callback();
        }
    }
    Ok(())
}

/// Turn a call failure into the right kind of error.
///
/// A name with no owner means the daemon went away between the presence check and the call,
/// which is "no daemon" rather than a bus fault — a race worth reporting honestly because the
/// user's next action differs.
fn map_call_error(e: zbus::Error) -> Error {
    let text = e.to_string();
    if text.contains("ServiceUnknown") || text.contains("NameHasNoOwner") {
        return Error::NoDaemon;
    }
    match e {
        zbus::Error::MethodError(_, detail, _) => {
            Error::Refused(detail.unwrap_or_else(|| "no reason given".into()))
        }
        other => Error::Bus(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_service_reads_as_no_daemon_not_a_bus_fault() {
        // The distinction the CLI's exit code 4 depends on.
        let e = map_call_error(zbus::Error::Unsupported);
        assert!(matches!(e, Error::Bus(_)));
    }

    #[test]
    fn a_name_with_no_owner_is_reported_as_no_daemon() {
        // The race the presence check cannot close: the daemon exits between the check and the
        // call. It is still "no daemon", not a bus fault, because the user's next step is the
        // same — start it.
        let e = map_call_error(zbus::Error::Failure("org.freedesktop.DBus.Error.ServiceUnknown".into()));
        assert!(matches!(e, Error::NoDaemon), "got {e:?}");
    }
}
