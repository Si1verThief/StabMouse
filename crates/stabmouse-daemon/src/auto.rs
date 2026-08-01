//! The `auto_activate` rules: which mode a window asks for.
//!
//! Opt-in per profile, and absent by default — software that changes its own behaviour is
//! only welcome when it was asked to.
//!
//! # Entering, not occupying
//!
//! A rule fires when the position *enters* a window, not for every sample spent inside it.
//! Continuously re-asserting the rule would make manual switching impossible: the user would
//! press their hotkey, get the mode for an instant, and watch the rule take it back.
//!
//! # The user outranks the rules, until they leave
//!
//! Switching by hand while over a ruled window overrules that window's rule for as long as the
//! position stays there. Leaving and returning restores the rule's authority. So the rule
//! governs arriving somewhere and the user governs staying — which is the reading that makes
//! both a config file and a hotkey worth having.

/// The state machine, deliberately separate from the runtime so its corner cases can be
/// tested without a daemon, a device, or a compositor.
#[derive(Debug, Default)]
pub struct AutoSwitch {
    /// Window class to 1-based mode slot.
    rules: Vec<(String, usize)>,
    last_class: Option<String>,
    overruled: Option<String>,
}

impl AutoSwitch {
    pub fn new(rules: Vec<(String, usize)>) -> Self {
        Self {
            rules,
            last_class: None,
            overruled: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Record that the user switched by hand while over `class`.
    pub fn overrule(&mut self, class: &str) {
        if !class.is_empty() {
            self.overruled = Some(class.to_string());
        }
    }

    /// The slot `class` asks for, or `None` if nothing should change.
    ///
    /// Returns `Some` at most once per arrival: the caller acts on it, and staying in the same
    /// window produces nothing further.
    pub fn entering(&mut self, class: &str) -> Option<usize> {
        if self.rules.is_empty() || class.is_empty() {
            return None;
        }
        if self.last_class.as_deref() == Some(class) {
            return None;
        }
        self.last_class = Some(class.to_string());

        // Arriving anywhere other than the overruled window ends the overrule; arriving back
        // *at* it means we never left, so the user's choice still stands.
        if self.overruled.as_deref() == Some(class) {
            return None;
        }
        self.overruled = None;

        self.rules
            .iter()
            .find(|(app, _)| app.eq_ignore_ascii_case(class))
            .map(|(_, slot)| *slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> AutoSwitch {
        AutoSwitch::new(vec![("krita".into(), 2), ("blender".into(), 3)])
    }

    #[test]
    fn no_rules_means_nothing_ever_happens() {
        let mut a = AutoSwitch::default();
        assert!(a.is_empty());
        assert_eq!(a.entering("krita"), None);
    }

    #[test]
    fn entering_a_ruled_window_asks_for_its_mode() {
        let mut a = rules();
        assert_eq!(a.entering("krita"), Some(2));
        assert_eq!(a.entering("blender"), Some(3));
    }

    #[test]
    fn staying_in_a_window_asks_only_once() {
        // Otherwise the rule would re-assert itself every sample and a hotkey could never win.
        let mut a = rules();
        assert_eq!(a.entering("krita"), Some(2));
        assert_eq!(a.entering("krita"), None);
        assert_eq!(a.entering("krita"), None);
    }

    #[test]
    fn an_unruled_window_changes_nothing() {
        let mut a = rules();
        assert_eq!(a.entering("firefox"), None);
    }

    #[test]
    fn a_manual_switch_outranks_the_rule_while_you_stay() {
        let mut a = rules();
        assert_eq!(a.entering("krita"), Some(2));
        a.overrule("krita");
        // Still in the same window, so the rule must stay quiet — otherwise the hotkey press
        // the user just made would be undone by the next sample.
        assert_eq!(a.entering("krita"), None);
    }

    #[test]
    fn leaving_and_returning_restores_the_rule() {
        let mut a = rules();
        a.entering("krita");
        a.overrule("krita");
        assert_eq!(a.entering("firefox"), None, "firefox has no rule");
        assert_eq!(
            a.entering("krita"),
            Some(2),
            "arriving afresh is a new decision, so the rule speaks again"
        );
    }

    #[test]
    fn an_overrule_only_covers_the_window_it_was_made_in() {
        let mut a = rules();
        a.entering("krita");
        a.overrule("krita");
        assert_eq!(a.entering("blender"), Some(3), "another window's rule is unaffected");
    }

    #[test]
    fn matching_ignores_case() {
        // Compositors report classes with whatever capitalisation the application chose.
        let mut a = AutoSwitch::new(vec![("Krita".into(), 2)]);
        assert_eq!(a.entering("krita"), Some(2));
    }

    #[test]
    fn nothing_under_the_position_is_not_a_window_change() {
        // A gap between windows must not count as leaving, or crossing one would silently
        // clear an overrule the user is still standing in.
        let mut a = rules();
        a.entering("krita");
        a.overrule("krita");
        assert_eq!(a.entering(""), None);
        assert_eq!(a.entering("krita"), None, "the empty class did not move us anywhere");
    }
}
