//! Watching the config directory for edits.
//!
//! Replaces a 400ms mtime poll. The poll worked, but it broke a rule the project holds to:
//! *the user only ever waits for an actual load, never for a delay in case something is
//! loading.* Saving a preset and waiting up to 400ms to feel the change is exactly that delay,
//! and during tuning it is paid on every single edit.
//!
//! There is a second gain. The loop's idle wait was capped at the poll interval, so a daemon
//! with nobody touching the mouse still woke 2.5 times a second forever. With a watch
//! descriptor in the poll set the idle wait becomes indefinite, and idle cost becomes zero —
//! which is what `wait.rs` says it wants and could not previously have.
//!
//! # Which events, and why these
//!
//! - `CLOSE_WRITE` — an editor that writes in place is done when it closes the file. Watching
//!   `MODIFY` instead would fire mid-write and reload a half-written file.
//! - `MOVED_TO` — an editor that saves atomically writes a temporary file and renames it over
//!   the target. This is the *only* event such a save produces, and most serious editors work
//!   this way, so omitting it would make the watch appear broken for exactly those users.
//! - `CREATE` / `DELETE` — adding or removing a preset changes the config as much as editing
//!   one does.
//!
//! # Falling back
//!
//! Watches are a finite kernel resource and can be refused. When that happens the caller keeps
//! the mtime poll, which is slower but always works — a degraded reload is worth far more than
//! a daemon that will not start.

use inotify::{Inotify, WatchMask};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

pub struct Watcher {
    inotify: Inotify,
}

/// Directories to watch under the config root.
///
/// The root itself plus each subdirectory, because inotify is **not recursive** — a watch on
/// `config/` says nothing about `config/presets/`, which is where nearly every edit lands.
const SUBDIRECTORIES: [&str; 2] = ["presets", "profiles"];

impl Watcher {
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        let inotify = Inotify::init()?;
        let mask = WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_TO
            | WatchMask::CREATE
            | WatchMask::DELETE;

        // The root has to succeed; a missing subdirectory does not, since a config that has
        // never had a profile written is still a valid config.
        inotify.watches().add(dir, mask)?;
        for sub in SUBDIRECTORIES {
            let path = dir.join(sub);
            if path.is_dir() {
                let _ = inotify.watches().add(&path, mask);
            }
        }

        Ok(Self { inotify })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.inotify.as_raw_fd()
    }

    /// Consume every queued event, reporting whether any arrived.
    ///
    /// Draining matters as much as the answer: a readable descriptor that is never read stays
    /// readable, and `poll` would then return immediately forever — turning the idle-cost win
    /// into a busy loop.
    pub fn drain(&mut self) -> bool {
        let mut buffer = [0u8; 4096];
        let mut saw_any = false;
        loop {
            match self.inotify.read_events(&mut buffer) {
                Ok(events) => {
                    let mut empty = true;
                    for _ in events {
                        empty = false;
                        saw_any = true;
                    }
                    if empty {
                        return saw_any;
                    }
                }
                // Nothing left to read. The descriptor is non-blocking, so this is the normal
                // end of a drain rather than a failure.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return saw_any,
                Err(_) => return saw_any,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("stabmouse-watch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("presets")).unwrap();
        p
    }

    /// Wait briefly for the kernel to deliver, since inotify is asynchronous.
    fn saw_change(w: &mut Watcher) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if w.drain() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn editing_a_preset_in_place_is_noticed() {
        let dir = scratch("inplace");
        let mut w = Watcher::new(&dir).expect("watch");
        std::fs::write(dir.join("presets/inking.toml"), "schema = 1\n").unwrap();
        assert!(saw_change(&mut w), "a write into presets/ should be seen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_atomic_save_is_noticed() {
        // The case a naive watch misses entirely: the editor never touches the target file, it
        // renames another file over it. Most serious editors save this way.
        let dir = scratch("atomic");
        let mut w = Watcher::new(&dir).expect("watch");

        let tmp = dir.join("presets/.inking.toml.swp");
        std::fs::write(&tmp, "schema = 1\n").unwrap();
        std::fs::rename(&tmp, dir.join("presets/inking.toml")).unwrap();

        assert!(saw_change(&mut w), "a rename-over-target save should be seen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_preset_is_noticed() {
        let dir = scratch("delete");
        let path = dir.join("presets/gone.toml");
        std::fs::write(&path, "schema = 1\n").unwrap();
        let mut w = Watcher::new(&dir).expect("watch");
        std::fs::remove_file(&path).unwrap();
        assert!(saw_change(&mut w), "removing a preset changes the config too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untouched_directory_reports_nothing() {
        // The property the idle-cost win rests on: no events means the loop keeps sleeping.
        let dir = scratch("quiet");
        let mut w = Watcher::new(&dir).expect("watch");
        assert!(!w.drain(), "nothing happened, so nothing should be reported");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn draining_twice_does_not_repeat_an_event() {
        // A descriptor left readable would make poll return immediately forever.
        let dir = scratch("drain");
        let mut w = Watcher::new(&dir).expect("watch");
        std::fs::write(dir.join("presets/a.toml"), "schema = 1\n").unwrap();
        assert!(saw_change(&mut w));
        assert!(!w.drain(), "the queue should have been emptied");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
