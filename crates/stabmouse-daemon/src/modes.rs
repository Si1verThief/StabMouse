//! Mode slots and the switching rules.
//!
//! Mode switching is the **headline interaction**: hotkey, mid-task, dozens of times a
//! session (click in Gartic Phone → draw → click). Everything here exists to make that
//! instant and predictable.

use stabmouse_config::Output;
use stabmouse_core::Pipeline;

pub struct Mode {
    pub name: String,
    pub output: Output,
    pub preset: String,
    /// Built up front. Switching is an index change, never a config load — the pipeline for
    /// every slot is resident before the hotkey is ever pressed.
    pub pipeline: Pipeline,
    /// Chords that engage this mode's scroll gesture. Any chord, all of its parts.
    pub scroll_button: Vec<Vec<u16>>,
    /// Chords that engage this mode's constrain modifier.
    ///
    /// Resolved once at build time rather than per sample: the name-to-code lookup is a string
    /// comparison against the whole evdev table, which has no business on the hot path.
    ///
    /// **A list of chords**, not a list of codes. The outer list is alternatives — which
    /// button is free depends on what the hand is doing — and each inner list must be held
    /// together, so `Ctrl+A+Middle` is one binding rather than three.
    pub modifier: Vec<Vec<u16>>,
    /// Chords that hand the physical wheel to the scroll stage while held.
    pub wheel_binding: Vec<Vec<u16>>,
    /// What a bound mouse button still does for the application.
    pub passthrough: stabmouse_config::Passthrough,
}

pub struct Modes {
    slots: Vec<Mode>,
    current: usize,
    last_used: Option<usize>,
    /// A switch asked for mid-stroke, held until the stroke ends.
    pending: Option<usize>,
}

impl Modes {
    pub fn new(slots: Vec<Mode>, default_index: usize) -> Self {
        let current = default_index.min(slots.len().saturating_sub(1));
        Self {
            slots,
            current,
            last_used: None,
            pending: None,
        }
    }

    /// The slots as the bus describes them.
    ///
    /// Structured rather than the formatted strings `names` produces: a client rendering its
    /// own UI needs the fields, not a line of text meant for a terminal.
    pub fn infos(&self) -> Vec<stabmouse_ipc::ModeInfo> {
        self.slots
            .iter()
            .enumerate()
            .map(|(i, m)| stabmouse_ipc::ModeInfo {
                // 1-based, matching what the user sees and what SetMode takes.
                slot: (i + 1) as u32,
                name: m.name.clone(),
                output: format!("{:?}", m.output).to_lowercase(),
                preset: m.preset.clone(),
            })
            .collect()
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> Option<&Mode> {
        self.slots.get(self.current)
    }

    pub fn current_mut(&mut self) -> Option<&mut Mode> {
        self.slots.get_mut(self.current)
    }

    /// Every slot, for callers that must consider all modes rather than the current one —
    /// which keyboards to watch is a union across the whole profile, not a per-mode question.
    pub fn slots(&self) -> &[Mode] {
        &self.slots
    }

    pub fn names(&self) -> Vec<String> {
        self.slots
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "{}: {} — {:?} via '{}'",
                    i + 1,
                    m.name,
                    m.output,
                    m.preset
                )
            })
            .collect()
    }

    /// Where an action would land, without applying it.
    ///
    /// **Cycling is the default and nothing here depends on timing.** A double-tap gesture was
    /// tried and rejected in use: you cannot tell a slow double-tap from two single taps, so
    /// you cannot tell whether the program understood you — it reads as unreliable even when
    /// it is working. Anyone who wants that gesture can bind their own detection to `Flip`.
    pub fn target_for(&self, action: Action) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let n = self.slots.len();
        match action {
            Action::Cycle => (n > 1).then(|| (self.current + 1) % n),
            Action::CyclePrev => (n > 1).then(|| (self.current + n - 1) % n),
            // Back-and-forth. Falls back to cycling the first time, when there is no history.
            Action::Flip => match self.last_used {
                Some(i) if i != self.current && i < n => Some(i),
                _ => (n > 1).then(|| (self.current + 1) % n),
            },
            Action::Select(one_based) => {
                let i = one_based.checked_sub(1)?;
                (i < n && i != self.current).then_some(i)
            }
        }
    }

    /// Perform an action, deferring it if a stroke is open.
    ///
    /// **A request during a stroke is held to the stroke's end.** Switching output type
    /// mid-stroke would leave a dangling `BTN_TOUCH` on a device that stops receiving events,
    /// which is how a pen gets stuck down in the target application.
    pub fn request(&mut self, action: Action, stroke_active: bool) -> Option<Switch> {
        let target = self.target_for(action)?;
        if stroke_active {
            self.pending = Some(target);
            return Some(Switch::Deferred(target));
        }
        Some(Switch::Applied(self.apply(target)))
    }

    /// Apply a deferred switch, if one is waiting. Called when a stroke ends.
    pub fn take_pending(&mut self) -> Option<Applied> {
        let target = self.pending.take()?;
        Some(self.apply(target))
    }

    fn apply(&mut self, target: usize) -> Applied {
        let from = self.current;
        self.current = target;

        self.last_used = Some(from);

        // Carry no per-stroke state across a switch: a half-formed stroke in the outgoing
        // pipeline has nothing to do with the incoming one.
        if let Some(mode) = self.slots.get_mut(target) {
            mode.pipeline.reset();
        }

        Applied {
            from,
            to: target,
            left_tablet: self.slots.get(from).map(|m| m.output) == Some(Output::Tablet)
                && self.slots.get(target).map(|m| m.output) != Some(Output::Tablet),
            name: self
                .slots
                .get(target)
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            output: self
                .slots
                .get(target)
                .map(|m| m.output)
                .unwrap_or(Output::Mouse),
        }
    }
}

/// What a binding can ask for. Each is separately bindable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Cycle,
    CyclePrev,
    Flip,
    /// 1-based slot number.
    Select(usize),
}

#[derive(Debug)]
pub enum Switch {
    Applied(Applied),
    /// Held until the current stroke ends.
    Deferred(usize),
}

#[derive(Debug)]
pub struct Applied {
    pub from: usize,
    pub to: usize,
    pub name: String,
    pub output: Output,
    /// True when leaving tablet output, so the pen can be taken out of proximity.
    pub left_tablet: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(n: usize) -> Modes {
        let slots = (0..n)
            .map(|i| Mode {
                name: format!("m{i}"),
                output: if i % 2 == 0 {
                    Output::Mouse
                } else {
                    Output::Tablet
                },
                preset: "raw".into(),
                pipeline: Pipeline::new(vec![]),
                modifier: Vec::new(),
                scroll_button: Vec::new(),
                wheel_binding: Vec::new(),
                passthrough: Default::default(),
            })
            .collect();
        Modes::new(slots, 0)
    }

    #[test]
    fn cycling_visits_every_slot_in_order_and_wraps() {
        let mut m = modes(3);
        let mut seen = vec![m.current_index()];
        for _ in 0..3 {
            let t = m.target_for(Action::Cycle).unwrap();
            m.apply(t);
            seen.push(t);
        }
        assert_eq!(seen, vec![0, 1, 2, 0], "cycling should wrap cleanly");
    }

    #[test]
    fn cycling_backwards_wraps_the_other_way() {
        let mut m = modes(3);
        let t = m.target_for(Action::CyclePrev).unwrap();
        assert_eq!(t, 2);
        m.apply(t);
        assert_eq!(m.target_for(Action::CyclePrev), Some(1));
    }

    #[test]
    fn cycling_is_purely_positional_and_never_depends_on_timing() {
        // The whole point of the change: the same state gives the same answer, always.
        let m = modes(4);
        for _ in 0..5 {
            assert_eq!(m.target_for(Action::Cycle), Some(1));
        }
    }

    #[test]
    fn flip_returns_to_where_you_came_from() {
        let mut m = modes(4);
        m.apply(2);
        // Came from 0, so a flip goes back to 0 rather than onward to 3.
        assert_eq!(m.target_for(Action::Flip), Some(0));
        m.apply(0);
        assert_eq!(m.target_for(Action::Flip), Some(2), "and back again");
    }

    #[test]
    fn flip_falls_back_to_cycling_before_any_history_exists() {
        let m = modes(3);
        assert_eq!(m.target_for(Action::Flip), Some(1));
    }

    #[test]
    fn direct_select_is_one_based_and_ignores_the_current_slot() {
        let m = modes(3);
        assert_eq!(m.target_for(Action::Select(3)), Some(2));
        assert_eq!(m.target_for(Action::Select(1)), None, "already there");
        assert_eq!(m.target_for(Action::Select(9)), None, "out of range");
        assert_eq!(m.target_for(Action::Select(0)), None, "not zero-based");
    }

    #[test]
    fn a_single_mode_profile_has_nothing_to_switch_to() {
        let m = modes(1);
        assert_eq!(m.target_for(Action::Cycle), None);
        assert_eq!(m.target_for(Action::Flip), None);
    }

    #[test]
    fn a_request_during_a_stroke_is_deferred_not_dropped() {
        let mut m = modes(2);
        let before = m.current_index();

        let switch = m.request(Action::Cycle, true).unwrap();
        assert!(matches!(switch, Switch::Deferred(1)));
        assert_eq!(
            m.current_index(),
            before,
            "the switch must not take effect mid-stroke"
        );

        let applied = m.take_pending().expect("the deferred switch should be waiting");
        assert_eq!(applied.to, 1);
        assert_eq!(m.current_index(), 1);
        assert!(m.take_pending().is_none(), "and it should only fire once");
    }

    #[test]
    fn leaving_tablet_output_is_reported_so_the_pen_can_be_lifted() {
        let mut m = modes(2);
        let to_tablet = m.apply(1);
        assert!(!to_tablet.left_tablet);

        let back_to_mouse = m.apply(0);
        assert!(
            back_to_mouse.left_tablet,
            "leaving tablet output must be flagged, or the target app keeps a hovering pen"
        );
    }
}
