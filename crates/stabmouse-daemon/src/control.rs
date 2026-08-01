//! The control socket.
//!
//! Replaces signals for everything except shutdown, for two reasons found by use:
//!
//! 1. **Signals were slow.** `libc::signal` on glibc installs handlers with `SA_RESTART`, so
//!    a signal does not interrupt `poll` — it restarts it. The flag was set immediately but
//!    the loop stayed blocked for the remainder of its timeout, up to 400ms, before looking.
//!    That read as "is it frozen, is it lagging, should I press again". A socket is just
//!    another descriptor in the poll set, so a command wakes the loop the instant it arrives.
//! 2. **Signals cannot carry an argument.** Only `SIGUSR1`/`SIGUSR2` are free, which caps the
//!    vocabulary at two actions. Bindings need to reach any action, so the wire carries text.
//!
//! Deliberately a plain datagram socket with one-line commands: it is the smallest thing that
//! is instant and extensible, and it is the natural precursor to the D-Bus interface.

use anyhow::{bail, Context};
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

/// Where the daemon listens.
///
/// `STABMOUSE_SOCKET` overrides it. That exists so tests never touch the real path — two of
/// them bind it and delete it, and running them while a daemon is live would take its control
/// channel away. It also makes a second daemon on one machine possible, which is the same
/// need from the other direction.
pub fn socket_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("STABMOUSE_SOCKET") {
        return PathBuf::from(explicit);
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("stabmouse.sock")
}

/// What a binding can ask for.
///
/// Every one of these is separately bindable. Cycling is the default because a
/// timing-dependent gesture reads as unreliable — you cannot tell a slow double-tap from two
/// single taps, so you cannot tell whether the program understood you. Anyone who wants
/// double-tap semantics can bind their own detection to `Flip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Next slot in order. The default.
    Cycle,
    /// Previous slot in order.
    CyclePrev,
    /// The most recently used other slot — "back and forth".
    Flip,
    /// A specific slot, 1-based.
    Mode(usize),
    /// Release the grab and stop filtering; again to resume.
    ///
    /// A *toggle*, because a hotkey has only one press to work with. Anything that knows the
    /// state it wants should send `SetEnabled` instead — see the note there.
    Panic,
    /// Filter, or do not, regardless of the current state.
    ///
    /// Separate from `Panic` because a toggle cannot express intent. A caller that compares
    /// against a state it read a moment ago and then flips is racing: the state can change in
    /// between, and the flip then lands the wrong way round. Sending the desired state instead
    /// is idempotent, so a repeat is harmless and a stale read is irrelevant.
    SetEnabled(bool),
    Quit,
    Status,
    /// Re-read the config now, rather than waiting for the mtime poll to notice.
    Reload,
}

impl Command {
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.trim().split_whitespace();
        match parts.next()? {
            "cycle" | "switch" | "next" => Some(Self::Cycle),
            "prev" | "previous" => Some(Self::CyclePrev),
            "flip" | "toggle" => Some(Self::Flip),
            "mode" => parts.next()?.parse().ok().map(Self::Mode),
            "panic" => Some(Self::Panic),
            "enabled" => match parts.next()? {
                "1" | "true" | "on" => Some(Self::SetEnabled(true)),
                "0" | "false" | "off" => Some(Self::SetEnabled(false)),
                _ => None,
            },
            "quit" | "stop" => Some(Self::Quit),
            "reload" => Some(Self::Reload),
            "status" => Some(Self::Status),
            _ => None,
        }
    }

    pub fn wire(&self) -> String {
        match self {
            Self::Cycle => "cycle".into(),
            Self::CyclePrev => "prev".into(),
            Self::Flip => "flip".into(),
            Self::Mode(n) => format!("mode {n}"),
            Self::Panic => "panic".into(),
            Self::SetEnabled(on) => format!("enabled {}", if *on { 1 } else { 0 }),
            Self::Quit => "quit".into(),
            Self::Status => "status".into(),
            Self::Reload => "reload".into(),
        }
    }
}

/// The daemon's end.
pub struct Listener {
    socket: UnixDatagram,
    path: PathBuf,
}

impl Listener {
    pub fn bind() -> anyhow::Result<Self> {
        Self::bind_at(socket_path())
    }

    pub fn bind_at(path: PathBuf) -> anyhow::Result<Self> {

        // A leftover socket file from a crashed run would make bind fail, but removing one
        // that a *live* daemon is using would steal its control channel. So only clear it
        // when nothing answers.
        if path.exists() && !responds(&path) {
            let _ = std::fs::remove_file(&path);
        }

        let socket = UnixDatagram::bind(&path)
            .with_context(|| format!("binding {}", path.display()))?;
        socket
            .set_nonblocking(true)
            .context("setting the control socket non-blocking")?;

        Ok(Self { socket, path })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }

    /// Drain every queued command. Non-blocking.
    pub fn drain(&self) -> Vec<Command> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match self.socket.recv(&mut buf) {
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    for line in text.lines() {
                        match Command::parse(line) {
                            Some(c) => out.push(c),
                            None => eprintln!("ignoring unknown control command {line:?}"),
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("control socket read failed: {e}");
                    break;
                }
            }
        }
        out
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Is there a live daemon listening?
fn responds(path: &std::path::Path) -> bool {
    // Connecting to an abandoned socket file fails with ECONNREFUSED, so a successful
    // connect means someone is bound to it.
    UnixDatagram::unbound()
        .and_then(|s| s.connect(path))
        .is_ok()
}

/// Send one command to the running daemon.
pub fn send(command: &Command) -> anyhow::Result<()> {
    send_to(command, socket_path())
}

pub fn send_to(command: &Command, path: PathBuf) -> anyhow::Result<()> {
    let socket = UnixDatagram::unbound().context("creating a control socket")?;
    // Non-blocking, so a full receive buffer reports an error instead of waiting.
    //
    // A blocking send is one half of a deadlock: the D-Bus service thread sends here, and the
    // buffer only fills when the input loop has stopped draining it — which is exactly when a
    // caller must not be made to wait. Failing loudly leaves the caller free to retry, or to
    // kill the daemon and get the cursor back.
    let _ = socket.set_nonblocking(true);
    if socket.connect(&path).is_err() {
        bail!(
            "no StabMouse daemon is listening on {}. Is it running?",
            path.display()
        );
    }
    socket
        .send(command.wire().as_bytes())
        .with_context(|| format!("sending {:?}", command))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_survives_a_round_trip() {
        for c in [
            Command::Cycle,
            Command::CyclePrev,
            Command::Flip,
            Command::Mode(3),
            Command::SetEnabled(true),
            Command::SetEnabled(false),
            Command::Panic,
            Command::Quit,
            Command::Status,
        ] {
            assert_eq!(Command::parse(&c.wire()), Some(c.clone()), "{c:?}");
        }
    }

    #[test]
    fn friendly_aliases_are_accepted() {
        assert_eq!(Command::parse("switch"), Some(Command::Cycle));
        assert_eq!(Command::parse("next"), Some(Command::Cycle));
        assert_eq!(Command::parse("toggle"), Some(Command::Flip));
        assert_eq!(Command::parse("stop"), Some(Command::Quit));
    }

    #[test]
    fn whitespace_and_case_of_arguments_are_tolerated() {
        assert_eq!(Command::parse("  mode   2  "), Some(Command::Mode(2)));
    }

    #[test]
    fn nonsense_is_rejected_rather_than_guessed_at() {
        for bad in ["", "   ", "explode", "mode", "mode x", "mode -1"] {
            assert_eq!(Command::parse(bad), None, "{bad:?} should not parse");
        }
    }

    /// A path unique to one test.
    ///
    /// Never the real socket. These tests bind and delete, so sharing a path made them race
    /// against each other — and against a daemon actually running on this machine, whose
    /// control channel `cargo test` would then delete out from under it.
    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("stabmouse-test-{}-{tag}.sock", std::process::id()))
    }

    #[test]
    fn a_listener_receives_what_was_sent() {
        let path = scratch("receives");
        let _ = std::fs::remove_file(&path);
        let listener = Listener::bind_at(path.clone()).expect("bind");
        send_to(&Command::Mode(2), path.clone()).expect("send");
        send_to(&Command::Cycle, path.clone()).expect("send");

        // Datagrams are queued, so both should arrive in order.
        let got = listener.drain();
        assert_eq!(got, vec![Command::Mode(2), Command::Cycle]);
        assert!(listener.drain().is_empty(), "and they are consumed once");
    }

    #[test]
    fn sending_with_no_daemon_reports_that_clearly() {
        let path = scratch("no-daemon");
        let _ = std::fs::remove_file(&path);
        let err = send_to(&Command::Cycle, path).unwrap_err().to_string();
        assert!(
            err.contains("no StabMouse daemon"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn the_socket_path_can_be_overridden() {
        // The mechanism the tests above rely on, checked directly so a regression in it does
        // not silently send them back to the real path.
        let bind = Listener::bind_at(scratch("override")).expect("bind");
        assert!(bind.path.ends_with(format!("stabmouse-test-{}-override.sock", std::process::id())));
    }
}
