//! A wedged daemon must not keep the user's mouse.
//!
//! # What is already safe, and what is not
//!
//! The grab is held by a file descriptor and **the kernel releases it when that fd closes**,
//! which happens on process death however the process dies. A panic, a `SIGTERM`, a `kill -9`
//! and a segfault are therefore all survivable already — the cursor comes back on its own.
//!
//! The one dangerous state is *alive but wedged*: the process holds the grab, is not dead, and
//! is not emitting. The user has no cursor and nothing is going to give it back. So this
//! watchdog's whole job is to turn "wedged" into "dead", because dead is the state the kernel
//! already knows how to clean up.
//!
//! # Why in-work timing rather than a periodic tick
//!
//! The obvious watchdog — "the loop must check in every N milliseconds" — is wrong here,
//! because the loop **legitimately blocks forever** when nothing is happening. That is the
//! point of the watch-based config reload: an idle daemon costs zero wakeups. A tick-based
//! watchdog would either fire on a daemon that is merely idle, or force the loop awake to
//! prove it is alive, reintroducing exactly the cost the design removed.
//!
//! So the loop marks when it *enters* a unit of work and when it *leaves*, and the watchdog
//! trips only on work that never finished. Blocking in `poll` is not work. An idle daemon is
//! silent; a wedged one is caught within a bounded time no matter how long it has been idle.
//!
//! # Why `abort`, and not a polite ungrab
//!
//! Releasing the grab from this thread would mean sharing the capture behind a lock — and a
//! hot thread wedged *while holding that lock* would wedge the watchdog too, which is the
//! precise failure this exists to prevent. `abort()` needs nothing from the wedged thread: it
//! closes every fd on the way out, the kernel drops the grab, and `Restart=on-failure` brings
//! the daemon back.
//!
//! The residual risk is a whole-process freeze — `SIGSTOP`, severe OOM, a kernel-level stall —
//! where no thread of ours runs at all. Only an external supervisor covers that, and it stays
//! deferred until we see it happen.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a single unit of work may take before the loop is presumed wedged.
///
/// The loop's normal work is microseconds; its slowest legitimate work is a config reload,
/// which reads a handful of small files. Two seconds is far beyond anything real and far
/// below the patience of someone whose cursor has stopped.
///
/// Being wrong in each direction costs very different things, which is why the margin is this
/// wide: a false trip drops the user to a raw mouse and a restart, while a missed trip leaves
/// them with no cursor at all.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2000);

/// The hot loop's proof of life.
///
/// One word, written with relaxed stores: the hot path pays two atomic stores per iteration
/// and holds no lock, so there is nothing here for a wedged thread to be stuck inside and
/// nothing the watchdog can block on.
#[derive(Clone)]
pub struct Heartbeat {
    /// Milliseconds since `origin` at which the current unit of work began; 0 when idle.
    in_work_since: Arc<AtomicU64>,
    origin: Instant,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            in_work_since: Arc::new(AtomicU64::new(0)),
            origin: Instant::now(),
        }
    }

    /// Mark a unit of work, ending when the returned guard drops.
    ///
    /// A guard rather than paired `enter`/`leave` calls, because the pairing is a safety
    /// invariant and not a convention: any path out of the loop body that skipped `leave` —
    /// a `continue` added later, a `?` propagating, an early `return` — would leave the
    /// heartbeat marked busy while the loop went back to blocking in `poll`, and the watchdog
    /// would abort a perfectly healthy daemon two seconds later. `Drop` cannot be forgotten.
    ///
    /// Costs one atomic increment for the clone, once per wakeup rather than per event.
    #[inline]
    pub fn work(&self) -> Work {
        let ms = self.elapsed_ms();
        // Saturated to 1 so that work beginning in the daemon's first millisecond is not
        // indistinguishable from idle.
        self.in_work_since.store(ms.max(1), Ordering::Relaxed);
        Work(self.clone())
    }

    #[inline]
    fn leave(&self) {
        self.in_work_since.store(0, Ordering::Relaxed);
    }

    fn elapsed_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    /// Whether the loop is inside a unit of work right now.
    #[cfg(test)]
    fn busy(&self) -> bool {
        self.in_work_since.load(Ordering::Relaxed) != 0
    }

    /// How long the loop has been inside one unit of work, or `None` when idle.
    fn stuck_for(&self) -> Option<Duration> {
        let started = self.in_work_since.load(Ordering::Relaxed);
        if started == 0 {
            return None;
        }
        Some(Duration::from_millis(self.elapsed_ms().saturating_sub(started)))
    }
}

/// Marks the loop as working until dropped. See [`Heartbeat::work`].
pub struct Work(Heartbeat);

impl Drop for Work {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// Start watching. A zero timeout returns an unwatched heartbeat rather than a special case
/// the hot loop would have to branch on.
pub fn start(timeout: Duration) -> Heartbeat {
    let beat = Heartbeat::new();
    if timeout.is_zero() {
        eprintln!("  watchdog: off — a wedged daemon will keep the grab");
        return beat;
    }

    // Quarter of the timeout, so the worst-case overshoot is small, but never so fast that the
    // watchdog itself becomes a source of wakeups on an idle machine.
    let interval = (timeout / 4).clamp(Duration::from_millis(50), Duration::from_millis(500));
    let watched = beat.clone();

    let spawned = std::thread::Builder::new()
        .name("stabmouse-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(interval);
            if let Some(stuck) = watched.stuck_for() {
                if stuck >= timeout {
                    trip(stuck);
                }
            }
        });

    match spawned {
        Ok(_) => println!("  watchdog: on ({}ms)", timeout.as_millis()),
        // Not fatal. A daemon that filters input without a watchdog is strictly better than no
        // daemon, and the failure is stated rather than assumed away.
        Err(e) => eprintln!("  watchdog: could not start ({e}); a wedged daemon will keep the grab"),
    }
    beat
}

/// Say what happened, then die so the kernel releases the grab.
fn trip(stuck: Duration) -> ! {
    use std::io::Write;
    // Written directly and flushed: this is the last thing the process will ever say, and a
    // buffered explanation of a deliberate abort would be lost with the buffer.
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "\nWATCHDOG: the input loop has been inside one unit of work for {}ms.\n\
         Aborting on purpose — the kernel releases the grab as the process dies, so your\n\
         cursor comes back immediately, and systemd will restart the daemon if it runs one.\n\
         This is a bug. Whatever was on screen when the cursor stopped is the useful detail.",
        stuck.as_millis()
    );
    let _ = err.flush();
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_loop_is_never_stuck() {
        // The whole reason for in-work timing: a daemon blocked in `poll` with nothing to do
        // is the normal resting state and must never be mistaken for a wedge.
        let beat = Heartbeat::new();
        assert!(beat.stuck_for().is_none());
        drop(beat.work());
        assert!(beat.stuck_for().is_none());
    }

    #[test]
    fn work_in_progress_reports_its_duration() {
        let beat = Heartbeat::new();
        let _work = beat.work();
        assert!(beat.stuck_for().is_some(), "work under way must be visible to the watchdog");
    }

    #[test]
    fn an_early_exit_from_the_loop_body_still_ends_the_work() {
        // The reason this is a guard and not a pair of calls: a `continue` that skipped the
        // end mark would strand the heartbeat busy, and the watchdog would abort a healthy
        // daemon the moment it went back to waiting.
        let beat = Heartbeat::new();
        for i in 0..3 {
            let _work = beat.work();
            if i == 1 {
                continue;
            }
        }
        assert!(!beat.busy(), "leaving the body by any route must clear the mark");
    }

    #[test]
    fn a_zero_timeout_yields_a_heartbeat_that_still_works() {
        // `--watchdog-ms 0` must not make the hot loop's calls invalid, only unwatched.
        let beat = start(Duration::ZERO);
        drop(beat.work());
        assert!(beat.stuck_for().is_none());
    }

    #[test]
    fn the_check_interval_stays_within_its_bounds() {
        let of = |ms: u64| {
            (Duration::from_millis(ms) / 4)
                .clamp(Duration::from_millis(50), Duration::from_millis(500))
        };
        // A very short timeout must not turn the watchdog into a busy loop, and a very long
        // one must not let the check drift into minutes.
        assert_eq!(of(40), Duration::from_millis(50));
        assert_eq!(of(2000), Duration::from_millis(500));
        assert_eq!(of(600), Duration::from_millis(150));
    }
}
