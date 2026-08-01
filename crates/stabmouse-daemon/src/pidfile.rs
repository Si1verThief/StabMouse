//! Where the running daemon can be found.
//!
//! Exists so control commands are `stabmoused switch` rather than
//! `kill -USR1 $(pgrep stabmoused)`. A hotkey the user has to assemble themselves is not a
//! hotkey, and name-matching with `pgrep` breaks the moment there are two builds around.

use anyhow::{bail, Context};
use std::path::PathBuf;

pub fn path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("stabmouse.pid")
}

/// Record this process as the running daemon.
pub struct Guard(PathBuf);

impl Guard {
    pub fn acquire() -> anyhow::Result<Self> {
        let p = path();

        // A stale file from a crashed run must not block a fresh start, so check whether the
        // recorded process is actually alive rather than trusting the file's existence.
        if let Some(existing) = read() {
            bail!(
                "another StabMouse daemon is already running (pid {existing}). Stop it first, \
                 or remove {} if you are sure it is gone",
                p.display()
            );
        }

        std::fs::write(&p, std::process::id().to_string())
            .with_context(|| format!("writing {}", p.display()))?;
        Ok(Self(p))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The pid of a *live* daemon, if there is one.
pub fn read() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(path()).ok()?.trim().parse().ok()?;
    if is_alive(pid) {
        Some(pid)
    } else {
        // Stale: tidy up so the next start is not blocked by a dead run's leftovers.
        let _ = std::fs::remove_file(path());
        None
    }
}

fn is_alive(pid: u32) -> bool {
    // Signal 0 checks for existence and permission without delivering anything.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
