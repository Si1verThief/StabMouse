//! Reading the desktop's own colours, so the window looks like it belongs.
//!
//! # The accent colour is not where the spec said it would be
//!
//! docs/gui.md specifies `org.kde.plasmashell.accentColor` on D-Bus. **That property does not
//! exist on Plasma 6** — checked against a live session on 2026-07-31; `/PlasmaShell` exposes a
//! `color` method, but it is the desktop containment's colour, not the accent.
//!
//! What does exist, and is what the user actually sees when they select something, is
//! `[Colors:Selection] BackgroundNormal` in `kdeglobals`. It is written by the colour-scheme
//! machinery whether the accent came from a scheme or from the accent picker, so it stays
//! correct either way.
//!
//! Reading a config file is less elegant than a property, but the elegant option is not there,
//! and the alternative is hard-coding a blue that will clash with everyone who chose otherwise.

use std::path::PathBuf;

/// Fallback from docs/gui.md, used when nothing better can be read.
pub const DEFAULT_ACCENT: (u8, u8, u8) = (0x6e, 0xa8, 0xfe);

fn kdeglobals() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("kdeglobals")
}

/// Read `key` from `section` of an INI-style file.
///
/// Hand-parsed rather than pulling in an INI crate: the format is two lines of interest, and
/// the file belongs to another application so the parser should be forgiving rather than
/// strict — an unfamiliar line must be skipped, never treated as an error.
fn ini_value(body: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == section;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn parse_rgb(value: &str) -> Option<(u8, u8, u8)> {
    let parts: Vec<&str> = value.split(',').map(|p| p.trim()).collect();
    if parts.len() < 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// The desktop's accent colour, or the documented fallback.
pub fn accent() -> (u8, u8, u8) {
    std::fs::read_to_string(kdeglobals())
        .ok()
        .and_then(|body| ini_value(&body, "[Colors:Selection]", "BackgroundNormal"))
        .and_then(|v| parse_rgb(&v))
        .unwrap_or(DEFAULT_ACCENT)
}

/// Whether the desktop is using a dark colour scheme.
///
/// Inferred from the window background's luminance rather than a scheme *name*: names are
/// arbitrary, and a user on a third-party dark scheme not called "Dark" should still get the
/// dark theme.
pub fn prefers_dark() -> bool {
    let Ok(body) = std::fs::read_to_string(kdeglobals()) else {
        return true;
    };
    let Some((r, g, b)) = ini_value(&body, "[Colors:Window]", "BackgroundNormal")
        .and_then(|v| parse_rgb(&v))
    else {
        return true;
    };
    // Rec. 601 luma, which tracks perceived brightness closely enough to classify a background.
    let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    luma < 128.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[General]
BrowserApplication=vivaldi-stable.desktop

[Colors:Selection]
BackgroundAlternate=255,25,25
BackgroundNormal=235,49,49
DecorationFocus=39,39,42

[Colors:Window]
BackgroundNormal=27,27,31
";

    #[test]
    fn a_value_is_read_from_the_right_section() {
        // Both sections define BackgroundNormal, so picking the wrong one is silent and wrong
        // rather than an error — exactly the bug worth a test.
        assert_eq!(
            ini_value(SAMPLE, "[Colors:Selection]", "BackgroundNormal").as_deref(),
            Some("235,49,49")
        );
        assert_eq!(
            ini_value(SAMPLE, "[Colors:Window]", "BackgroundNormal").as_deref(),
            Some("27,27,31")
        );
    }

    #[test]
    fn a_missing_key_is_none_rather_than_a_guess() {
        assert_eq!(ini_value(SAMPLE, "[Colors:Selection]", "Nope"), None);
        assert_eq!(ini_value(SAMPLE, "[Nothing]", "BackgroundNormal"), None);
    }

    #[test]
    fn rgb_triples_parse_and_rubbish_does_not() {
        assert_eq!(parse_rgb("235,49,49"), Some((235, 49, 49)));
        assert_eq!(parse_rgb(" 1 , 2 , 3 "), Some((1, 2, 3)));
        assert_eq!(parse_rgb("235,49"), None);
        assert_eq!(parse_rgb("nope"), None);
        // Out of range for u8, so it must be refused rather than wrapped.
        assert_eq!(parse_rgb("300,0,0"), None);
    }

    #[test]
    fn a_dark_window_background_is_classified_dark() {
        let dark = parse_rgb("27,27,31").unwrap();
        let luma = 0.299 * f64::from(dark.0) + 0.587 * f64::from(dark.1) + 0.114 * f64::from(dark.2);
        assert!(luma < 128.0);

        let light = parse_rgb("252,252,252").unwrap();
        let luma = 0.299 * f64::from(light.0) + 0.587 * f64::from(light.1) + 0.114 * f64::from(light.2);
        assert!(luma >= 128.0);
    }
}
