//! Shutdown signals.
//!
//! Control actions used to arrive here too, and that was a mistake worth recording: glibc's
//! `signal()` installs handlers with `SA_RESTART`, so a signal *restarts* `poll` rather than
//! interrupting it. The flag was set immediately but the loop stayed blocked for the rest of
//! its timeout — up to 400ms — before looking at it. A mode switch that lands somewhere in the
//! next 400ms reads as "is it frozen, should I press again". Control moved to a socket, which
//! `poll` sees the instant it arrives.
//!
//! Signals cannot carry an argument either, and only `SIGUSR1`/`SIGUSR2` are free, so the
//! vocabulary was capped at two actions when bindings need to reach all of them.
//!
//! What remains is termination, which signals are the right mechanism for.

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub struct Signals;

impl Signals {
    pub fn install() -> Self {
        // SAFETY: the handler performs one relaxed atomic store and nothing else, which is
        // async-signal-safe. No allocation, no locks, no reentrancy concerns.
        unsafe {
            libc::signal(libc::SIGINT, on_shutdown as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_shutdown as *const () as libc::sighandler_t);
        }
        Self
    }

    pub fn should_shutdown(&self) -> bool {
        SHUTDOWN.load(Ordering::Relaxed)
    }
}

extern "C" fn on_shutdown(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}
