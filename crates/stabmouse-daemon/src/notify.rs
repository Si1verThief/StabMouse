//! Desktop notifications.
//!
//! A mode switch the user cannot see is indistinguishable from one that did not happen — which
//! is what turns an instant action into "is it frozen, should I press again, should I panic".
//! The daemon usually runs in the background, so stdout is not visible; this is.
//!
//! Shelling out to `notify-send` rather than speaking D-Bus directly: it keeps the dependency
//! at zero for something that will be replaced by the real IPC layer, and switches are rare
//! enough (dozens a session, on the control path) that a process spawn is irrelevant.

use std::process::{Command, Stdio};

/// Announce a mode change, replacing any previous StabMouse notification rather than stacking.
pub fn mode(summary: &str, body: &str) {
    // The synchronous hint makes each notification supersede the last, so rapid cycling shows
    // one updating popup instead of a tower of them.
    let _ = Command::new("notify-send")
        .arg("--app-name=StabMouse")
        .arg("--expire-time=900")
        .arg("-h")
        .arg("string:x-canonical-private-synchronous:stabmouse-mode")
        .arg(summary)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        // Deliberately ignored: no notification daemon is a perfectly normal state, and it
        // must never affect input handling.
        .map(|mut c| {
            // Reap it so a long session does not accumulate zombies.
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        });
}
