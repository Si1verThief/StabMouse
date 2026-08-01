//! Undo, by remembering what a file said before it was changed.
//!
//! # Whole files, not diffs
//!
//! Config files here are small — a few hundred bytes — and the operations are coarse: a whole
//! stage removed, a profile deleted, a slot reordered. Keeping the previous *text* is exact,
//! survives every operation without one of them needing to describe its own inverse, and
//! restores comments and formatting along with the values. A diff or a command log would be
//! more clever and would have to be right about more things.
//!
//! # Absence is a state
//!
//! Creating a file records `None`, so undoing a create deletes it again, and deleting a file
//! records its contents, so undoing a delete brings it back whole. Without that, the two
//! operations most likely to be regretted — the ones that make something vanish — would be the
//! two undo could not reach.
//!
//! # In memory only, and deliberately
//!
//! The stack lives for the life of the window. Persisting it would mean an undo could reach
//! back past edits made in a text editor since, and restoring a file over changes the user
//! made elsewhere is a worse failure than not offering the undo.

use std::path::{Path, PathBuf};

/// What one file looked like before an edit.
struct Snapshot {
    path: PathBuf,
    /// `None` when the file did not exist — a create.
    before: Option<String>,
    /// What the user did, so the button can say what it will take back.
    label: String,
}

/// Bounded so a long session cannot grow without limit. Deep enough that the mistake is still
/// on the stack by the time it is noticed, which is the only number that matters.
const DEPTH: usize = 64;

#[derive(Default)]
pub struct History {
    stack: Vec<Snapshot>,
}

impl History {
    /// Record a file's current state before changing it.
    ///
    /// Call this *before* the write. A failed read is recorded as absence, which is correct
    /// for a create and harmless for anything else — the alternative, skipping the entry,
    /// would leave a gap in the stack that undo would silently step over.
    pub fn record(&mut self, path: &Path, label: impl Into<String>) {
        let before = std::fs::read_to_string(path).ok();
        self.stack.push(Snapshot {
            path: path.to_path_buf(),
            before,
            label: label.into(),
        });
        if self.stack.len() > DEPTH {
            self.stack.remove(0);
        }
    }

    /// What the next undo would take back, for the button's label.
    pub fn next_label(&self) -> Option<&str> {
        self.stack.last().map(|s| s.label.as_str())
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Put the most recent file back the way it was.
    ///
    /// Returns what was undone. The entry is consumed either way: an undo that failed and
    /// stayed on the stack would be pressed again to the same effect, which reads as the
    /// button being broken rather than the operation being impossible.
    pub fn undo(&mut self) -> anyhow::Result<String> {
        let Some(snapshot) = self.stack.pop() else {
            anyhow::bail!("nothing to undo");
        };
        match &snapshot.before {
            Some(text) => {
                if let Some(parent) = snapshot.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&snapshot.path, text)?;
            }
            // It did not exist before, so putting it back means removing it. A file already
            // gone is the state being asked for, not a failure.
            None => match std::fs::remove_file(&snapshot.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            },
        }
        Ok(snapshot.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stabmouse-history-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("f.toml")
    }

    #[test]
    fn an_edit_is_taken_back_exactly() {
        let p = temp("edit");
        std::fs::write(&p, "before # with a comment\n").unwrap();
        let mut h = History::default();
        h.record(&p, "change radius");
        std::fs::write(&p, "after\n").unwrap();

        assert_eq!(h.undo().unwrap(), "change radius");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "before # with a comment\n");
    }

    #[test]
    fn undoing_a_delete_brings_the_file_back_whole() {
        // The operation most worth undoing, and the one a value-based scheme would miss.
        let p = temp("delete");
        std::fs::write(&p, "schema = 1\n# a note\n").unwrap();
        let mut h = History::default();
        h.record(&p, "delete preset");
        std::fs::remove_file(&p).unwrap();

        h.undo().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "schema = 1\n# a note\n");
    }

    #[test]
    fn undoing_a_create_removes_it_again() {
        let p = temp("create");
        let _ = std::fs::remove_file(&p);
        let mut h = History::default();
        h.record(&p, "create preset");
        std::fs::write(&p, "schema = 1\n").unwrap();

        h.undo().unwrap();
        assert!(!p.exists(), "a file that did not exist before must not exist after");
    }

    #[test]
    fn undo_walks_back_one_step_at_a_time() {
        let p = temp("steps");
        std::fs::write(&p, "one\n").unwrap();
        let mut h = History::default();
        h.record(&p, "first");
        std::fs::write(&p, "two\n").unwrap();
        h.record(&p, "second");
        std::fs::write(&p, "three\n").unwrap();

        assert_eq!(h.undo().unwrap(), "second");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two\n");
        assert_eq!(h.undo().unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\n");
        assert!(h.is_empty());
    }

    #[test]
    fn an_empty_history_says_so_rather_than_panicking() {
        let mut h = History::default();
        assert!(h.is_empty());
        assert!(h.next_label().is_none());
        assert!(h.undo().is_err());
    }

    #[test]
    fn the_stack_is_bounded() {
        let p = temp("bound");
        std::fs::write(&p, "x\n").unwrap();
        let mut h = History::default();
        for i in 0..(DEPTH + 20) {
            h.record(&p, format!("edit {i}"));
        }
        assert_eq!(h.stack.len(), DEPTH);
        // The oldest fell off the bottom, not the newest off the top.
        assert_eq!(h.next_label().unwrap(), format!("edit {}", DEPTH + 19));
    }

    #[test]
    fn undoing_a_delete_that_was_already_undone_is_not_an_error() {
        let p = temp("gone");
        let _ = std::fs::remove_file(&p);
        let mut h = History::default();
        h.record(&p, "create");
        // The file was never created; undo should still succeed rather than reporting a
        // failure the user cannot act on.
        assert!(h.undo().is_ok());
    }
}
