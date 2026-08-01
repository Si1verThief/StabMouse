//! Resolving a parameter for a specific device, and saying where the value came from.
//!
//! Three levels, most specific winning: **device → group → defaults** (D8). Overrides
//! are sparse by design — a device names only what it changes — so this is a lookup
//! chain rather than a merge of whole sections.
//!
//! Reporting the *origin* is not a nicety: D8 requires the GUI to show provenance, and
//! the D-Bus API exposes it as `Config.Explain`. Without it a user staring at a value
//! has no way to discover which of three places set it.

use crate::error::{Error, Result};
use crate::schema::{Device, Group, Identity, Params, Root};
use std::collections::BTreeMap;

/// Which level supplied a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The preset's own value; no override applies.
    Preset,
    Defaults,
    Group(String),
    Device(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Preset => write!(f, "preset"),
            Origin::Defaults => write!(f, "default"),
            Origin::Group(name) => write!(f, "group: {name}"),
            Origin::Device(name) => write!(f, "device: {name}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub value: toml::Value,
    pub origin: Origin,
}

/// An override key: `<preset>.<stage>.<param>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideKey {
    pub preset: String,
    pub stage: String,
    pub param: String,
}

impl OverrideKey {
    pub fn parse(key: &str) -> Result<Self> {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(Error::BadOverrideKey {
                key: key.to_string(),
            });
        }
        Ok(Self {
            preset: parts[0].to_string(),
            stage: parts[1].to_string(),
            param: parts[2].to_string(),
        })
    }
}

/// The override chain that applies to one device.
///
/// Borrowed from `Root` rather than copied: resolution happens on the control thread
/// during config load and mode switching, not on the hot path, and copying every
/// override table per device would be waste.
pub struct DeviceView<'a> {
    device: Option<&'a Device>,
    group: Option<&'a Group>,
    defaults: &'a Params,
}

impl<'a> DeviceView<'a> {
    /// Whether this device is opted in. Absent from config means no.
    pub fn is_managed(&self) -> bool {
        self.device.map(|d| d.managed).unwrap_or(false)
    }

    pub fn label(&self) -> Option<&str> {
        self.device.and_then(|d| d.label.as_deref())
    }

    pub fn group_name(&self) -> Option<&str> {
        self.group.map(|g| g.name.as_str())
    }

    /// The effective override for `key`, if any level sets one.
    ///
    /// `None` means no override applies and the preset's own value stands.
    pub fn resolve(&self, key: &str) -> Option<Resolved> {
        if let Some(device) = self.device {
            if let Some(v) = device.overrides.get(key) {
                return Some(Resolved {
                    value: v.clone(),
                    origin: Origin::Device(
                        device.label.clone().unwrap_or_else(|| describe(device)),
                    ),
                });
            }
        }
        if let Some(group) = self.group {
            if let Some(v) = group.overrides.get(key) {
                return Some(Resolved {
                    value: v.clone(),
                    origin: Origin::Group(group.name.clone()),
                });
            }
        }
        if let Some(v) = self.defaults.get(key) {
            return Some(Resolved {
                value: v.clone(),
                origin: Origin::Defaults,
            });
        }
        None
    }

    /// Every override in effect, keyed as `<preset>.<stage>.<param>`.
    ///
    /// Only overridden parameters appear — the GUI lists these and offers an "add
    /// override" picker, because showing every parameter with inherited values greyed
    /// out would be hundreds of rows per device (see gui.md).
    pub fn effective_overrides(&self) -> BTreeMap<String, Resolved> {
        let mut out = BTreeMap::new();
        // Least specific first, so more specific levels overwrite.
        for (k, v) in self.defaults {
            out.insert(
                k.clone(),
                Resolved {
                    value: v.clone(),
                    origin: Origin::Defaults,
                },
            );
        }
        if let Some(group) = self.group {
            for (k, v) in &group.overrides {
                out.insert(
                    k.clone(),
                    Resolved {
                        value: v.clone(),
                        origin: Origin::Group(group.name.clone()),
                    },
                );
            }
        }
        if let Some(device) = self.device {
            let who = device.label.clone().unwrap_or_else(|| describe(device));
            for (k, v) in &device.overrides {
                out.insert(
                    k.clone(),
                    Resolved {
                        value: v.clone(),
                        origin: Origin::Device(who.clone()),
                    },
                );
            }
        }
        out
    }
}

impl Root {
    /// Build the override chain for a discovered device.
    ///
    /// Picks the **most specific** matching `[[device]]` entry (serial beats vid+pid
    /// beats a partial match) and the **first** matching `[[group]]` in file order.
    /// Groups are user-authored lists rather than a computed hierarchy, so there is no
    /// meaningful specificity between two of them; first-wins is predictable and
    /// visible in the file.
    pub fn view_for(&self, id: &Identity) -> DeviceView<'_> {
        let device = self
            .devices
            .iter()
            .filter_map(|d| d.matcher.specificity(id).map(|s| (s, d)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, d)| d);

        let group = self
            .groups
            .iter()
            .find(|g| g.devices.iter().any(|m| m.specificity(id).is_some()));

        DeviceView {
            device,
            group,
            defaults: &self.defaults.overrides,
        }
    }
}

fn describe(d: &Device) -> String {
    match (&d.matcher.serial, &d.matcher.vid, &d.matcher.pid) {
        (Some(s), _, _) => format!("serial {s}"),
        (None, Some(v), Some(p)) => format!("{v}:{p}"),
        (None, Some(v), None) => format!("vid {v}"),
        (None, None, Some(p)) => format!("pid {p}"),
        _ => "unmatched".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Root {
        toml::from_str(
            r#"
            schema = 1

            [defaults]
            profile = "line-art"
            overrides = { "inking.stabilize.radius_mm" = 0.5, "raw.normalize.dpi" = 1000 }

            [[group]]
            name = "CAD pucks"
            devices = [{ serial = "A1" }, { serial = "B2" }]
            overrides = { "inking.stabilize.radius_mm" = 0.3 }

            [[device]]
            match = { serial = "A1" }
            label = "left puck"
            managed = true
            overrides = { "inking.stabilize.catch_up" = 0.8 }

            [[device]]
            match = { vid = "0738", pid = "0c08" }
            label = "R.A.T. 8+"
            managed = true
            overrides = { "raw.normalize.dpi" = 1600 }
            "#,
        )
        .unwrap()
    }

    fn id(serial: Option<&str>, vid: Option<&str>, pid: Option<&str>) -> Identity {
        Identity {
            serial: serial.map(Into::into),
            vid: vid.map(Into::into),
            pid: pid.map(Into::into),
        }
    }

    #[test]
    fn device_overrides_beat_group_which_beats_defaults() {
        let r = root();
        let v = r.view_for(&id(Some("A1"), None, None));

        // Only the device sets catch_up.
        let c = v.resolve("inking.stabilize.catch_up").unwrap();
        assert_eq!(c.origin, Origin::Device("left puck".into()));

        // Group and defaults both set radius; the group wins.
        let radius = v.resolve("inking.stabilize.radius_mm").unwrap();
        assert_eq!(radius.origin, Origin::Group("CAD pucks".into()));
        assert_eq!(radius.value.as_float(), Some(0.3));

        // Only defaults set dpi.
        let dpi = v.resolve("raw.normalize.dpi").unwrap();
        assert_eq!(dpi.origin, Origin::Defaults);
    }

    #[test]
    fn the_most_specific_device_entry_wins() {
        let r = root();
        // Matches the serial entry AND the vid/pid entry; serial is more specific.
        let v = r.view_for(&id(Some("A1"), Some("0738"), Some("0c08")));
        assert_eq!(v.label(), Some("left puck"));
    }

    #[test]
    fn an_unknown_device_inherits_defaults_and_is_not_managed() {
        let r = root();
        let v = r.view_for(&id(Some("ZZ"), Some("1234"), Some("5678")));

        assert!(
            !v.is_managed(),
            "a device absent from config must never be touched"
        );
        assert_eq!(v.label(), None);
        assert_eq!(v.group_name(), None);

        // But it still sees global defaults, so "swap in a new mouse and everything
        // stays the same" holds (D8).
        let radius = v.resolve("inking.stabilize.radius_mm").unwrap();
        assert_eq!(radius.origin, Origin::Defaults);
        assert_eq!(radius.value.as_float(), Some(0.5));
    }

    #[test]
    fn unset_keys_resolve_to_nothing_so_the_preset_value_stands() {
        let r = root();
        let v = r.view_for(&id(Some("A1"), None, None));
        assert!(v.resolve("inking.smooth.beta").is_none());
    }

    #[test]
    fn effective_overrides_merges_all_levels_with_origins() {
        let r = root();
        let v = r.view_for(&id(Some("A1"), None, None));
        let all = v.effective_overrides();

        assert_eq!(
            all["inking.stabilize.radius_mm"].origin,
            Origin::Group("CAD pucks".into())
        );
        assert_eq!(
            all["inking.stabilize.catch_up"].origin,
            Origin::Device("left puck".into())
        );
        assert_eq!(all["raw.normalize.dpi"].origin, Origin::Defaults);
    }

    #[test]
    fn override_keys_must_have_three_segments() {
        assert_eq!(
            OverrideKey::parse("inking.stabilize.radius_mm").unwrap(),
            OverrideKey {
                preset: "inking".into(),
                stage: "stabilize".into(),
                param: "radius_mm".into(),
            }
        );
        for bad in ["", "inking", "inking.stabilize", "a.b.c.d", "a..c", ".b.c"] {
            assert!(
                OverrideKey::parse(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn a_device_with_no_config_entry_still_gets_a_usable_view() {
        let empty = Root::default();
        let v = empty.view_for(&id(None, Some("1"), Some("2")));
        assert!(!v.is_managed());
        assert!(v.effective_overrides().is_empty());
        assert!(v.resolve("anything.at.all").is_none());
    }
}
