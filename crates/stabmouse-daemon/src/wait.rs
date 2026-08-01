//! Waiting for input with a deadline.
//!
//! The hot loop needs to wake on either of two things: a device report arriving, or a tick
//! deadline expiring so filters can advance. `read()` alone gives the first; `std` offers
//! no timed wait on a file descriptor, so this is a thin wrapper over `poll(2)`.
//!
//! **When nothing is outstanding the wait is indefinite.** A fixed 2ms tick would mean 500
//! wakeups a second while the user is not touching the mouse, against a requirement of
//! near-zero idle CPU. Ticking only while a stroke is live or a filter is mid-convergence
//! costs nothing at rest.

use std::os::fd::RawFd;

#[derive(Debug, PartialEq, Eq)]
pub enum Wake {
    /// A bitmask over the `fds` slice: bit *n* set means `fds[n]` is readable.
    ///
    /// **Which** descriptor woke us has to be reported, not just *that* one did. Reading a
    /// descriptor that has nothing waiting blocks — so a loop that assumed "readable" meant
    /// "the device" would drain one control command and then block on the device until the
    /// user happened to move the mouse, leaving every later command queued behind it.
    Ready(u32),
    /// The deadline expired with no input.
    Timeout,
    /// Interrupted by a signal; the caller should loop and re-check its shutdown flag.
    Interrupted,
}

impl Wake {
    /// Whether `fds[index]` is readable.
    pub fn has(&self, index: usize) -> bool {
        matches!(self, Wake::Ready(mask) if mask & (1 << index) != 0)
    }
}

/// Block until any of `fds` is readable or `timeout` elapses. `None` waits indefinitely.
///
/// Several descriptors rather than one because the control socket has to wake the loop as
/// promptly as the device does. Signals cannot: glibc installs handlers with `SA_RESTART`, so
/// a signal restarts `poll` instead of interrupting it, and a command would sit unseen until
/// the timeout expired.
pub fn wait_any(fds: &[RawFd], timeout: Option<std::time::Duration>) -> Wake {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();

    // `poll` takes whole milliseconds. Round *up* so a sub-millisecond deadline never
    // becomes a zero timeout, which would spin.
    let timeout_ms = match timeout {
        None => -1,
        Some(d) => {
            let ms = (d.as_micros() as i64 + 999) / 1000;
            ms.clamp(1, i32::MAX as i64) as i32
        }
    };

    // SAFETY: `pfds` is a valid, initialised array for the lifetime of the call and the count
    // matches its length.
    let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };

    if n < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Wake::Interrupted;
        }
        // Any other poll failure on descriptors we already opened means something is gone.
        // Reporting everything as ready lets the caller's own read surface the real error
        // rather than this layer inventing one.
        return Wake::Ready(u32::MAX);
    }
    if n == 0 {
        return Wake::Timeout;
    }

    let mut mask = 0u32;
    for (i, pfd) in pfds.iter().enumerate().take(32) {
        // POLLERR/POLLHUP also mean "go read it and find out", so they count as ready.
        if pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            mask |= 1 << i;
        }
    }
    Wake::Ready(mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_timeout_actually_waits_and_reports_timeout() {
        // A pipe with nothing written to it never becomes readable.
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        let start = Instant::now();
        let wake = wait_any(&[fds[0]], Some(Duration::from_millis(20)));
        let elapsed = start.elapsed();

        assert_eq!(wake, Wake::Timeout);
        assert!(
            elapsed >= Duration::from_millis(15),
            "returned after only {elapsed:?}, so it did not really wait"
        );

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn available_data_wakes_immediately() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let byte = b"x";
        assert_eq!(unsafe { libc::write(fds[1], byte.as_ptr().cast(), 1) }, 1);

        let start = Instant::now();
        let wake = wait_any(&[fds[0]], Some(Duration::from_secs(5)));

        assert!(wake.has(0), "the written-to pipe should be reported ready: {wake:?}");
        assert!(start.elapsed() < Duration::from_millis(500));

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn only_the_ready_descriptor_is_reported() {
        // The bug this guards: a loop told merely "something is readable" will read the wrong
        // descriptor and block there.
        let mut a = [0i32; 2];
        let mut b = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(a.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(b.as_mut_ptr()) }, 0);

        let byte = b"x";
        assert_eq!(unsafe { libc::write(b[1], byte.as_ptr().cast(), 1) }, 1);

        let wake = wait_any(&[a[0], b[0]], Some(Duration::from_secs(5)));
        assert!(!wake.has(0), "the quiet pipe must not be reported ready");
        assert!(wake.has(1), "the written-to pipe must be reported ready");

        for fd in [a[0], a[1], b[0], b[1]] {
            unsafe { libc::close(fd) };
        }
    }

    #[test]
    fn a_sub_millisecond_deadline_does_not_become_a_spin() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        let start = Instant::now();
        assert_eq!(wait_any(&[fds[0]], Some(Duration::from_micros(1))), Wake::Timeout);
        // Rounded up to 1ms rather than truncated to 0, so it genuinely blocked.
        assert!(start.elapsed() >= Duration::from_micros(500));

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
