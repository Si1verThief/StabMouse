//! The typed representation of what is on disk.
//!
//! Mirrors docs/config-schema.md. Terms are fixed by docs/vocabulary.md: a **preset**
//! is a filter pipeline, a **mode** is a slot holding an output type plus a preset
//! reference, and a **profile** is a set of mode slots for one activity.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Highest schema version this build writes and fully understands.
pub const CURRENT_SCHEMA: u32 = 1;

/// Stage parameters are carried as opaque TOML values rather than typed per stage.
///
/// Deliberate: it keeps this crate independent of `stabmouse-core`'s stage structs, so
/// adding a stage is a one-place change rather than two, and it satisfies the
/// requirement that unknown keys survive a round-trip — an older build cannot eat a
/// newer config's fields if it never had to name them. Type checking happens where the
/// values are turned into stages, which is the only place that knows what they mean.
pub type Params = BTreeMap<String, toml::Value>;

// ---------------------------------------------------------------------------- preset

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, rename = "stage")]
    pub stages: Vec<StageEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageEntry {
    /// Stage kind, e.g. `stabilize`. Names fixed in docs/vocabulary.md.
    #[serde(rename = "type")]
    pub kind: String,
    /// Disambiguates repeated instances of the same kind in one preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Disabled stages stay in the file so toggling never loses their tuning.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Everything else. Pipeline order is file order; there is no `order` field to
    /// drift out of sync.
    #[serde(flatten)]
    pub params: Params,
}

// --------------------------------------------------------------------------- profile

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 1-based slot number activated when this profile is selected.
    #[serde(default = "one")]
    pub default_mode: usize,
    #[serde(default, rename = "mode")]
    pub modes: Vec<Mode>,
    /// Opt-in focus rules. Absent means no auto-switching for this profile.
    #[serde(default, rename = "auto_activate", skip_serializing_if = "Vec::is_empty")]
    pub auto_activate: Vec<AppRule>,
    /// Overrides the global toggle binding for this profile only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle_binding: Option<String>,
    /// Destroy the virtual tablet on leaving tablet output, rather than keeping it alive.
    ///
    /// Off by default, and deliberately per-profile rather than global.
    ///
    /// Written for Krita's stale-canvas-cursor defect and **does not fix it** — Krita stays
    /// stuck even when the device is destroyed outright (see D13). Retained as a tested
    /// mechanism with no known beneficiary rather than advertised as a remedy. Costs ~50ms to
    /// bring the device back, and forfeits D13 for anything launched while it is absent.
    #[serde(default)]
    pub destroy_tablet_on_leave: bool,
    /// In tablet mode, also send ordinary mouse buttons alongside the pen.
    ///
    /// **Off by default, and it does not currently work.** The idea was that a pen tip reaches
    /// no application that lacks `tablet_v2` support, so mirroring the button onto the relative
    /// pointer would restore clicking. In use the click lands *somewhere else*: the compositor
    /// tracks the tablet's position and the relative pointer's position separately, and the
    /// visible cursor follows the tablet, so the mirrored press goes wherever the pointer was
    /// last left.
    ///
    /// A click at an unpredictable location is worse than no click, which is why this defaults
    /// off. Kept rather than deleted because the mechanism is right and only the position is
    /// wrong — see D18 for what would fix it.
    #[serde(default)]
    pub tablet_emits_mouse_clicks: bool,

    /// Whether to hold the pen still while scrolling in an application not named in
    /// `[scroll_freeze]` and not on the built-in list.
    ///
    /// **Off**, following the same asymmetry as the pen tier itself: doing nothing to an
    /// application that turned out not to need it is invisible, while doing something to one
    /// that did not want it removes a capability the user had. The applications known to need
    /// the freeze are named, and adding one is a line.
    #[serde(default)]
    pub freeze_position_while_scrolling: bool,

    /// How long the pen stays frozen after the last wheel event, in milliseconds.
    ///
    /// Long enough to bridge the gap between notches of a slow deliberate scroll, so the pen
    /// does not stutter back to life between them, and long enough for the filter that made
    /// this necessary to lapse. Short enough that the pen feels attached to the hand again as
    /// soon as scrolling stops.
    #[serde(default = "scroll_freeze_default")]
    pub scroll_freeze_ms: u64,
}

fn scroll_freeze_default() -> u64 {
    250
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mode {
    pub name: String,
    pub output: Output,
    /// Preset slug. Resolved against the preset library, never embedded.
    pub preset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    /// The ordinary pointer. Delivered absolutely from the shared position when the screen
    /// layout is known (D22/D23), so switching modes never teleports the cursor.
    Mouse,
    /// A pen on the virtual tablet, falling back to `Mouse` per window (D20).
    Tablet,
    /// Raw relative deltas, bypassing the shared position. For pointer-lock and raw-input
    /// consumers — games, primarily — which want motion, not position. The one transport
    /// where the cursor can drift from the shared position; the compositor's own reports
    /// re-sync it whenever the hand pauses (D23).
    Relative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRule {
    /// Window class or app id to match.
    pub app: String,
    /// Slot to activate, 1-based.
    #[serde(default = "one")]
    pub mode: usize,
}

// ----------------------------------------------------------------------- config.toml

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Root {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "group", skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
    #[serde(default, rename = "device", skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<Device>,
    /// Whether an application can receive tablet input, keyed by window class.
    ///
    /// Overrides the built-in table. It exists because the built-in table cannot be right:
    /// support depends on the toolkit *and* on whether the application runs natively or under
    /// XWayland, and the window class a compositor reports is not always the name anyone would
    /// guess. Without this, a wrong entry could only be fixed by rebuilding.
    ///
    /// `stabmouse-probe focus` prints the classes as they are actually reported.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tablet_support: BTreeMap<String, bool>,

    /// Whether an application needs the pen held still to receive a scroll, by window class.
    ///
    /// **A property of the application, not of the mode.** Krita ignores mouse input while a
    /// pen is in proximity, so it needs the pen to stop before the wheel reaches it; Blender
    /// does not filter that way and scrolls perfectly well mid-movement, so freezing there
    /// removes the ability to move while scrolling and gains nothing. One setting for both
    /// would have to be wrong for one of them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scroll_freeze: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle_binding: Option<String>,
    /// Global overrides, applying to every device that does not override them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: Params,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub devices: Vec<Match>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: Params,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    #[serde(rename = "match")]
    pub matcher: Match,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// **Opt-in.** Absent or false means this device is never touched, which is what
    /// keeps trackpads and unrelated hardware safe by default.
    #[serde(default)]
    pub managed: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: Params,
}

/// How a config entry identifies hardware.
///
/// Precedence when several entries match: `serial` beats `vid`+`pid` beats the global
/// default. Most specific wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
}

/// A device as actually discovered, to be matched against `Match` entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub serial: Option<String>,
    pub vid: Option<String>,
    pub pid: Option<String>,
}

impl Match {
    /// How specifically this matcher identifies `id`, or `None` if it does not.
    ///
    /// Higher is more specific. Used to pick a winner when several entries apply.
    pub fn specificity(&self, id: &Identity) -> Option<u8> {
        // An empty matcher would otherwise match everything, which is never intended.
        if self.serial.is_none() && self.vid.is_none() && self.pid.is_none() {
            return None;
        }

        if let Some(want) = &self.serial {
            match &id.serial {
                Some(have) if eq_ci(want, have) => return Some(3),
                _ => return None,
            }
        }

        let vid_ok = match (&self.vid, &id.vid) {
            (Some(w), Some(h)) => eq_ci(w, h),
            (Some(_), None) => false,
            (None, _) => true,
        };
        let pid_ok = match (&self.pid, &id.pid) {
            (Some(w), Some(h)) => eq_ci(w, h),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !(vid_ok && pid_ok) {
            return None;
        }

        match (self.vid.is_some(), self.pid.is_some()) {
            (true, true) => Some(2),
            (true, false) | (false, true) => Some(1),
            (false, false) => None,
        }
    }
}

/// Hex device IDs appear in both cases in the wild (`0C08` vs `0c08`).
fn eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.chars().zip(b.chars()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

fn default_schema() -> u32 {
    CURRENT_SCHEMA
}
fn yes() -> bool {
    true
}
fn one() -> usize {
    1
}

/// Filenames are identities (D14), so a slug has to be safe as a filename and stable
/// as a reference.
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(serial: Option<&str>, vid: Option<&str>, pid: Option<&str>) -> Identity {
        Identity {
            serial: serial.map(Into::into),
            vid: vid.map(Into::into),
            pid: pid.map(Into::into),
        }
    }

    #[test]
    fn serial_beats_vid_pid_beats_partial() {
        let dev = id(Some("A1"), Some("0738"), Some("0c08"));

        let by_serial = Match {
            serial: Some("A1".into()),
            ..Default::default()
        };
        let by_vidpid = Match {
            vid: Some("0738".into()),
            pid: Some("0c08".into()),
            ..Default::default()
        };
        let by_vid = Match {
            vid: Some("0738".into()),
            ..Default::default()
        };

        assert_eq!(by_serial.specificity(&dev), Some(3));
        assert_eq!(by_vidpid.specificity(&dev), Some(2));
        assert_eq!(by_vid.specificity(&dev), Some(1));
    }

    #[test]
    fn an_empty_matcher_matches_nothing() {
        let dev = id(Some("A1"), Some("0738"), Some("0c08"));
        assert_eq!(Match::default().specificity(&dev), None);
    }

    #[test]
    fn hex_ids_compare_case_insensitively() {
        let dev = id(None, Some("0C08"), Some("ABCD"));
        let m = Match {
            vid: Some("0c08".into()),
            pid: Some("abcd".into()),
            ..Default::default()
        };
        assert_eq!(m.specificity(&dev), Some(2));
    }

    #[test]
    fn a_wanted_serial_that_the_device_lacks_does_not_match() {
        let dev = id(None, Some("0738"), Some("0c08"));
        let m = Match {
            serial: Some("A1".into()),
            ..Default::default()
        };
        assert_eq!(m.specificity(&dev), None);
    }

    #[test]
    fn slug_rules() {
        for good in ["raw", "line-art", "inking", "my_preset", "r2"] {
            assert!(is_valid_slug(good), "{good} should be valid");
        }
        for bad in ["", "Raw", "with space", "-leading", "trailing-", "with/slash", "../escape"] {
            assert!(!is_valid_slug(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn a_minimal_preset_parses() {
        let p: Preset = toml::from_str(
            r#"
            schema = 1
            [[stage]]
            type = "normalize"
            dpi = 1600
            [[stage]]
            type = "stabilize"
            radius_mm = 0.5
            catch_up = 0.35
            "#,
        )
        .unwrap();
        assert_eq!(p.stages.len(), 2);
        assert_eq!(p.stages[0].kind, "normalize");
        assert!(p.stages[0].enabled, "stages default to enabled");
        assert_eq!(
            p.stages[1].params.get("radius_mm").and_then(|v| v.as_float()),
            Some(0.5)
        );
    }

    #[test]
    fn unknown_stage_params_are_preserved_not_rejected() {
        // An older build must not choke on, or silently drop, a newer config's fields.
        let p: Preset = toml::from_str(
            r#"
            [[stage]]
            type = "stabilize"
            radius_mm = 0.5
            some_future_param = "hello"
            "#,
        )
        .unwrap();
        assert!(p.stages[0].params.contains_key("some_future_param"));
    }

    #[test]
    fn a_profile_parses_with_defaults_filled_in() {
        let p: Profile = toml::from_str(
            r#"
            display_name = "Line art"
            [[mode]]
            name = "Click"
            output = "mouse"
            preset = "raw"
            [[mode]]
            name = "Draw"
            output = "tablet"
            preset = "inking"
            "#,
        )
        .unwrap();
        assert_eq!(p.schema, CURRENT_SCHEMA);
        assert_eq!(p.default_mode, 1);
        assert_eq!(p.modes[1].output, Output::Tablet);
    }

    #[test]
    fn devices_are_not_managed_unless_they_say_so() {
        let r: Root = toml::from_str(
            r#"
            [[device]]
            match = { vid = "0738", pid = "0c08" }
            "#,
        )
        .unwrap();
        assert!(!r.devices[0].managed, "managed must default to false");
    }
}
