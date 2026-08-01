//! Placing a tablet on a screen, on KDE Plasma.
//!
//! KWin exposes every input device it knows at `/org/kde/KWin/InputDevice/<sysname>` on the
//! session bus, and `outputName` there is **writable**. Setting it does what choosing a screen
//! in System Settings does, immediately and without a restart — which is what makes automatic
//! placement possible rather than a documented manual step.
//!
//! Devices are looked up by their `name` property rather than by event node. The node number is
//! whatever the kernel handed out and changes every time a device is recreated; the name is
//! chosen by us and is stable, which matters because tablet teardown (see the daemon's
//! `tablet` module) recreates the device routinely.

use crate::{Error, Result};
use zbus::blocking::Connection;

const SERVICE: &str = "org.kde.KWin";
const ROOT: &str = "/org/kde/KWin/InputDevice";
const IFACE: &str = "org.kde.KWin.InputDevice";

/// Whether KWin is on the session bus and exposing input devices.
pub fn available() -> bool {
    let Ok(conn) = Connection::session() else {
        return false;
    };
    children(&conn).is_ok_and(|c| !c.is_empty())
}

/// The `eventN` names below the input device root.
fn children(conn: &Connection) -> Result<Vec<String>> {
    let xml: String = conn
        .call_method(
            Some(SERVICE),
            ROOT,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .map_err(|e| Error::Bus(e.to_string()))?
        .body()
        .deserialize()
        .map_err(|e| Error::Bus(e.to_string()))?;

    // Introspection XML rather than a listing method, because D-Bus offers no other way to
    // enumerate child objects. Parsed with a string scan to avoid pulling in an XML crate for
    // one attribute; the shape is fixed and produced by D-Bus itself, not by a user.
    let mut names = Vec::new();
    for chunk in xml.split("<node name=\"").skip(1) {
        if let Some(end) = chunk.find('"') {
            names.push(chunk[..end].to_string());
        }
    }
    Ok(names)
}

fn get_property<T>(conn: &Connection, sys_name: &str, property: &str) -> Result<T>
where
    T: TryFrom<zbus::zvariant::OwnedValue>,
{
    let path = format!("{ROOT}/{sys_name}");
    let value: zbus::zvariant::OwnedValue = conn
        .call_method(
            Some(SERVICE),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(IFACE, property),
        )
        .map_err(|e| Error::Bus(e.to_string()))?
        .body()
        .deserialize()
        .map_err(|e| Error::Bus(e.to_string()))?;

    T::try_from(value).map_err(|_| Error::Bus(format!("{property} was not of the expected type")))
}

/// Find the compositor's object for a device, by the name we gave it.
fn find_device(conn: &Connection, device: &str) -> Result<String> {
    for sys_name in children(conn)? {
        // A device can disappear between listing and querying — a recreated tablet, a mouse
        // unplugged — so a failure on one is skipped rather than failing the whole search.
        if let Ok(name) = get_property::<String>(conn, &sys_name, "name") {
            if name == device {
                return Ok(sys_name);
            }
        }
    }
    Err(Error::NoSuchDevice(device.to_string()))
}

/// Confine `device` to the screen named `output`.
pub fn map_tablet(device: &str, output: &str) -> Result<()> {
    let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
    let sys_name = find_device(&conn, device)?;
    let path = format!("{ROOT}/{sys_name}");

    // A plain string, not a variant of a variant: `Set` takes `ssv`, so the value is wrapped
    // once here and zbus supplies the signature.
    let value = zbus::zvariant::Value::from(output);
    conn.call_method(
        Some(SERVICE),
        path.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Set",
        &(IFACE, "outputName", &value),
    )
    .map_err(|e| Error::Bus(e.to_string()))?;

    Ok(())
}

/// Make a pointer device move one pixel per count.
///
/// Without this the compositor's own acceleration sits between what the daemon emits and where
/// the cursor lands, so a millimetre of hand movement covers an unpredictable distance. Two
/// things need it: matching the feel of tablet output when falling back to the pointer, and
/// placing the pointer at a known position — neither of which survives a curve in between.
///
/// Applied to StabMouse's own virtual device only. The user's real mouse is left alone.
pub fn set_pointer_unaccelerated(device: &str) -> Result<()> {
    let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
    let sys_name = find_device(&conn, device)?;
    let path = format!("{ROOT}/{sys_name}");

    // Flat profile first, then zero on the flat scale: setting the amount while an adaptive
    // curve is still selected sets it on the wrong scale.
    for (property, value) in [
        ("pointerAccelerationProfileFlat", zbus::zvariant::Value::from(true)),
        ("pointerAcceleration", zbus::zvariant::Value::from(0.0f64)),
    ] {
        conn.call_method(
            Some(SERVICE),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(IFACE, property, &value),
        )
        .map_err(|e| Error::Bus(e.to_string()))?;
    }
    Ok(())
}

/// Which screen a device is currently confined to, if any.
///
/// An empty string means the whole desktop, which KWin reports rather than omitting.
pub fn mapped_output(device: &str) -> Result<Option<String>> {
    let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
    let sys_name = find_device(&conn, device)?;
    let name: String = get_property(&conn, &sys_name, "outputName")?;
    Ok(if name.is_empty() { None } else { Some(name) })
}

/// Every device KWin classifies as a tablet tool, by name.
///
/// Useful for telling the user what the compositor actually sees, which is the thing that
/// decides whether pressure works — not what we believe we created.
pub fn tablet_tools() -> Result<Vec<String>> {
    let conn = Connection::session().map_err(|e| Error::Bus(e.to_string()))?;
    let mut found = Vec::new();
    for sys_name in children(&conn)? {
        if get_property::<bool>(&conn, &sys_name, "tabletTool").unwrap_or(false) {
            if let Ok(name) = get_property::<String>(&conn, &sys_name, "name") {
                found.push(name);
            }
        }
    }
    Ok(found)
}
