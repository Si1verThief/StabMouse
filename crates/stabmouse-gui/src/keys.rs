//! Turning the toolkit's idea of a key into the evdev name the config speaks.
//!
//! # Why the window captures keys and the daemon captures buttons
//!
//! The daemon holds an exclusive grab on the *mouse*, so its buttons reach nothing else and
//! only it can report them. It does not grab the keyboard — that is the project's first
//! working rule — so keys reach this window normally, and capturing them here needs no new
//! device to be opened and no keystroke to be read outside the moment the user asked for one.
//!
//! The split is therefore not an accident of implementation: each side captures the thing it
//! is the only one able to see, and neither gains a capability it did not already have.
//!
//! # Modifiers name the physical key
//!
//! `Ctrl+A` is two keys held together, and evdev has no "either control" code — it has
//! `KEY_LEFTCTRL` and `KEY_RIGHTCTRL`. Slint reports the modifier state as a flag rather than
//! as the key that produced it, so the left-hand code is used: it is what almost every hand
//! presses, and a user who wants the right-hand one can write it in the file. Guessing wrong
//! here costs a binding that does not fire, which is visible immediately.

/// The evdev name for a character Slint delivered, if there is one.
///
/// Handles the printable keys directly and the named ones by table. Anything unrecognised
/// returns `None`, and the caller says so rather than binding something arbitrary.
pub fn evdev_name(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        // Slint delivers named keys as multi-character strings from its own key constants,
        // which arrive here as the control characters below rather than as words.
        return named(text);
    }

    let name = match first {
        'a'..='z' => format!("KEY_{}", first.to_ascii_uppercase()),
        'A'..='Z' => format!("KEY_{first}"),
        '0'..='9' => format!("KEY_{first}"),
        ' ' => "KEY_SPACE".to_string(),
        '-' => "KEY_MINUS".to_string(),
        '=' => "KEY_EQUAL".to_string(),
        '[' => "KEY_LEFTBRACE".to_string(),
        ']' => "KEY_RIGHTBRACE".to_string(),
        ';' => "KEY_SEMICOLON".to_string(),
        '\'' => "KEY_APOSTROPHE".to_string(),
        '`' => "KEY_GRAVE".to_string(),
        '\\' => "KEY_BACKSLASH".to_string(),
        ',' => "KEY_COMMA".to_string(),
        '.' => "KEY_DOT".to_string(),
        '/' => "KEY_SLASH".to_string(),
        _ => return named(text),
    };
    Some(name)
}

/// Slint's named keys, which arrive as single control characters.
fn named(text: &str) -> Option<String> {
    let name = match text {
        "\u{8}" => "KEY_BACKSPACE",
        "\u{9}" => "KEY_TAB",
        "\u{a}" | "\u{d}" => "KEY_ENTER",
        "\u{1b}" => "KEY_ESC",
        "\u{7f}" => "KEY_DELETE",
        "\u{f700}" => "KEY_UP",
        "\u{f701}" => "KEY_DOWN",
        "\u{f702}" => "KEY_LEFT",
        "\u{f703}" => "KEY_RIGHT",
        "\u{f704}" => "KEY_F1",
        "\u{f705}" => "KEY_F2",
        "\u{f706}" => "KEY_F3",
        "\u{f707}" => "KEY_F4",
        "\u{f708}" => "KEY_F5",
        "\u{f709}" => "KEY_F6",
        "\u{f70a}" => "KEY_F7",
        "\u{f70b}" => "KEY_F8",
        "\u{f70c}" => "KEY_F9",
        "\u{f70d}" => "KEY_F10",
        "\u{f70e}" => "KEY_F11",
        "\u{f70f}" => "KEY_F12",
        "\u{f727}" => "KEY_INSERT",
        "\u{f72b}" => "KEY_END",
        "\u{f729}" => "KEY_HOME",
        "\u{f72c}" => "KEY_PAGEUP",
        "\u{f72d}" => "KEY_PAGEDOWN",
        _ => return None,
    };
    Some(name.to_string())
}

/// The modifier keys held, as evdev names, in a stable order.
///
/// Ordered so a chord reads the way a person writes one — `KEY_LEFTCTRL+KEY_LEFTSHIFT+KEY_A`
/// rather than whichever order the flags happened to be tested in.
pub fn modifier_names(control: bool, alt: bool, shift: bool, meta: bool) -> Vec<String> {
    let mut out = Vec::new();
    if control {
        out.push("KEY_LEFTCTRL".to_string());
    }
    if alt {
        out.push("KEY_LEFTALT".to_string());
    }
    if shift {
        out.push("KEY_LEFTSHIFT".to_string());
    }
    if meta {
        out.push("KEY_LEFTMETA".to_string());
    }
    out
}

/// Whether a captured name is itself a modifier, so a chord of only modifiers is still valid.
pub fn is_modifier(name: &str) -> bool {
    matches!(
        name,
        "KEY_LEFTCTRL"
            | "KEY_RIGHTCTRL"
            | "KEY_LEFTALT"
            | "KEY_RIGHTALT"
            | "KEY_LEFTSHIFT"
            | "KEY_RIGHTSHIFT"
            | "KEY_LEFTMETA"
            | "KEY_RIGHTMETA"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_digits_map_to_their_evdev_names() {
        assert_eq!(evdev_name("a").unwrap(), "KEY_A");
        assert_eq!(evdev_name("Z").unwrap(), "KEY_Z");
        assert_eq!(evdev_name("7").unwrap(), "KEY_7");
    }

    #[test]
    fn punctuation_uses_the_kernels_own_names_not_the_symbols() {
        // `KEY_DOT`, not `KEY_.` — the config is parsed by evdev's table, so the names must be
        // the ones that table holds.
        assert_eq!(evdev_name(".").unwrap(), "KEY_DOT");
        assert_eq!(evdev_name("/").unwrap(), "KEY_SLASH");
        assert_eq!(evdev_name(" ").unwrap(), "KEY_SPACE");
    }

    #[test]
    fn named_keys_arrive_as_control_characters() {
        assert_eq!(evdev_name("\u{1b}").unwrap(), "KEY_ESC");
        assert_eq!(evdev_name("\u{f708}").unwrap(), "KEY_F5");
    }

    #[test]
    fn an_unknown_key_binds_nothing_rather_than_something_arbitrary() {
        assert!(evdev_name("\u{f7ff}").is_none());
        assert!(evdev_name("").is_none());
    }

    #[test]
    fn modifiers_come_out_in_a_stable_written_order() {
        let m = modifier_names(true, false, true, false);
        assert_eq!(m, ["KEY_LEFTCTRL", "KEY_LEFTSHIFT"]);
    }

    #[test]
    fn every_name_this_produces_is_one_evdev_recognises() {
        // The whole point of the table: a name the daemon cannot resolve is a binding that
        // silently never fires.
        let mut names: Vec<String> = ["a", "Z", "7", ".", "/", " ", "\u{1b}", "\u{f708}", "\u{9}"]
            .iter()
            .filter_map(|t| evdev_name(t))
            .collect();
        names.extend(modifier_names(true, true, true, true));
        for name in names {
            assert!(
                stabmouse_input::code_for(&name).is_some(),
                "{name} is not a name evdev knows"
            );
        }
    }
}
