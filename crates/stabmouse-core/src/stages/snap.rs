//! `snap` — constrain motion to allowed directions.

use crate::sample::Sample;
use crate::stage::Stage;

/// What the position is constrained *to*.
///
/// An enum rather than a `dyn` trait: the set is closed at any given build, the pipeline
/// forbids allocation in `process`, and adding a variant is a smaller change than adding an
/// implementor would be. What stages.md asks for is that adding ellipse or perspective
/// constraints later is cheap — and it is, because everything outside this enum and
/// [`Constraint::project`] is constraint-agnostic. The stage does not know what a line is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Constraint {
    /// Snap to the nearest of `divisions` evenly spaced directions from the anchor.
    ///
    /// `divisions = 4` is axis lock — the spec folds `axis_lock` into exactly this. 8 gives
    /// 45° steps, 12 gives 30°, and 2 gives a single unsigned axis.
    Angle { divisions: u32, tolerance_deg: f64 },
    /// Hold whichever direction the constrained segment set off in.
    ///
    /// Not a fixed compass direction: a freehand line at any angle, the way dragging with a
    /// straight-edge behaves. The direction is taken once, a short distance in, because the
    /// first sample of a stroke is mostly noise.
    Line,
}

/// Hand travel used to establish a `Line`'s direction before it locks.
///
/// Long enough that sensor noise does not decide the angle of the whole line, short enough
/// that the lock feels immediate. About a third of the distance the measured slowest careful
/// stroke covers in its first 20ms.
const LINE_COMMIT_MM: f64 = 1.5;

impl Constraint {
    /// The nearest allowed position to `p`, given the anchor and the committed direction.
    ///
    /// Returns `None` when the constraint declines to act — outside an angle tolerance, or a
    /// line that has not yet seen enough motion to know its own direction.
    fn project(
        &self,
        anchor: (f64, f64),
        p: (f64, f64),
        committed: Option<(f64, f64)>,
    ) -> Option<(f64, f64)> {
        let (dx, dy) = (p.0 - anchor.0, p.1 - anchor.1);
        // Nothing to constrain from a standing start: with no travel there is no direction,
        // and `atan2(0, 0)` would invent one.
        if dx.hypot(dy) <= 0.0 {
            return None;
        }

        let direction = match *self {
            Constraint::Angle {
                divisions,
                tolerance_deg,
            } => {
                let divisions = divisions.max(1) as f64;
                let step = std::f64::consts::TAU / divisions;
                let theta = dy.atan2(dx);
                let snapped = (theta / step).round() * step;
                // Beyond the tolerance the constraint keeps out of the way, which is what
                // makes a soft snap a magnet rather than a cage.
                let off_by = (theta - snapped).abs().to_degrees();
                if !(off_by <= tolerance_deg.max(0.0)) {
                    return None;
                }
                (snapped.cos(), snapped.sin())
            }
            Constraint::Line => committed?,
        };

        // **Perpendicular projection**, not distance preserved along the constrained
        // direction. Projection is what every drawing application means by constrain — the
        // cursor tracks the component of the hand's travel that lies along the line, so moving
        // diagonally under an axis lock advances by the axis component and the rest is simply
        // not there. Preserving distance instead would make a wobbling hand *overshoot* along
        // the axis, turning noise perpendicular to the line into length along it.
        //
        // Allowed to be negative, so drawing back along the line retraces it rather than
        // sticking at the anchor.
        let (ux, uy) = direction;
        let along = dx * ux + dy * uy;
        Some((anchor.0 + along * ux, anchor.1 + along * uy))
    }
}

/// Constrains the path while the modifier is held.
///
/// # Motion is deliberately not conserved here
///
/// Every other position stage conserves motion — the path may lag but it always arrives. A
/// constraint cannot: discarding the component perpendicular to the allowed direction is the
/// entire feature. `deadzone` has the same character, and the core's conservation invariant is
/// about the subpixel carry never silently dropping slow motion, which is a different claim.
///
/// What *is* guaranteed is that engaging and releasing never move the cursor. On release the
/// stage re-anchors to where the output actually is, so the discarded motion stays discarded
/// instead of springing back — which is what a user means when they let go of shift and the
/// line simply continues from where they can see it.
#[derive(Debug, Clone)]
pub struct Snap {
    enabled: bool,
    pub constraint: Constraint,
    /// 0 leaves the path alone; 1 pins it to the constraint. Between the two it is a lean.
    pub strength: f64,
    /// When false the constraint is always on and the modifier is irrelevant.
    pub needs_modifier: bool,

    /// True position, as it would be with no constraint.
    position_x: f64,
    position_y: f64,
    /// Position actually emitted so far.
    out_x: f64,
    out_y: f64,
    /// Where the current constrained segment began.
    anchor: Option<(f64, f64)>,
    /// Unit direction a `Line` has committed to.
    committed: Option<(f64, f64)>,
    engaged: bool,
}

impl Default for Snap {
    fn default() -> Self {
        Self::new(
            Constraint::Angle {
                divisions: 4,
                // 45° — half of a quarter turn, so every direction belongs to some axis and
                // axis lock behaves as a lock rather than as an occasional magnet.
                tolerance_deg: 45.0,
            },
            1.0,
            true,
        )
    }
}

impl Snap {
    pub fn new(constraint: Constraint, strength: f64, needs_modifier: bool) -> Self {
        Self {
            enabled: true,
            constraint,
            strength,
            needs_modifier,
            position_x: 0.0,
            position_y: 0.0,
            out_x: 0.0,
            out_y: 0.0,
            anchor: None,
            committed: None,
            engaged: false,
        }
    }

    fn disengage(&mut self) {
        self.engaged = false;
        self.anchor = None;
        self.committed = None;
        // The constrained position becomes the truth. Without this the perpendicular motion
        // discarded during the segment would be owed back, and releasing the modifier would
        // fling the cursor to where an unconstrained hand would have been.
        self.position_x = self.out_x;
        self.position_y = self.out_y;
    }

    fn clear(&mut self) {
        self.position_x = 0.0;
        self.position_y = 0.0;
        self.out_x = 0.0;
        self.out_y = 0.0;
        self.anchor = None;
        self.committed = None;
        self.engaged = false;
    }
}

impl Stage for Snap {
    fn name(&self) -> &'static str {
        "snap"
    }

    fn process(&mut self, s: &mut Sample) {
        if !s.dx.is_finite() || !s.dy.is_finite() {
            s.dx = 0.0;
            s.dy = 0.0;
            return;
        }
        if s.discontinuity {
            self.clear();
        }

        let want = !self.needs_modifier || s.constrain;
        // Identity: with the constraint inactive the sample passes through untouched, and no
        // position bookkeeping leaks into the next engagement.
        if !want {
            if self.engaged {
                self.disengage();
            }
            self.position_x += s.dx;
            self.position_y += s.dy;
            self.out_x = self.position_x;
            self.out_y = self.position_y;
            return;
        }

        if !self.engaged {
            self.engaged = true;
            self.anchor = Some((self.position_x, self.position_y));
            self.committed = None;
        }

        self.position_x += s.dx;
        self.position_y += s.dy;

        let anchor = self.anchor.unwrap_or((self.position_x, self.position_y));

        // A line takes its direction once, after enough travel to mean something.
        if matches!(self.constraint, Constraint::Line) && self.committed.is_none() {
            let (dx, dy) = (self.position_x - anchor.0, self.position_y - anchor.1);
            let travelled = dx.hypot(dy);
            if travelled >= LINE_COMMIT_MM {
                self.committed = Some((dx / travelled, dy / travelled));
            }
        }

        let target = self
            .constraint
            .project(anchor, (self.position_x, self.position_y), self.committed)
            .unwrap_or((self.position_x, self.position_y));

        // `strength` leans the output toward the constraint rather than pinning it, so a soft
        // snap guides without taking control away.
        let k = if self.strength.is_finite() {
            self.strength.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let next_x = self.position_x + (target.0 - self.position_x) * k;
        let next_y = self.position_y + (target.1 - self.position_y) * k;

        s.dx = next_x - self.out_x;
        s.dy = next_y - self.out_y;
        self.out_x = next_x;
        self.out_y = next_y;
    }

    fn reset(&mut self) {
        self.clear();
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(stage: &mut Snap, steps: &[(f64, f64, bool)]) -> (f64, f64) {
        let (mut x, mut y) = (0.0, 0.0);
        for (i, (dx, dy, constrain)) in steps.iter().enumerate() {
            let mut s = Sample::new(*dx, *dy, (i as u64 + 1) * 1000, true);
            s.dt = 0.001;
            s.constrain = *constrain;
            stage.process(&mut s);
            x += s.dx;
            y += s.dy;
        }
        (x, y)
    }

    #[test]
    fn without_the_modifier_it_is_pass_through() {
        let mut stage = Snap::default();
        let mut s = Sample::new(1.3, -0.7, 1000, true);
        let before = s;
        stage.process(&mut s);
        assert_eq!(s, before, "an unheld modifier must leave the sample untouched");
    }

    #[test]
    fn axis_lock_flattens_the_off_axis_component() {
        let mut stage = Snap::default();
        // Mostly rightward with a wobble: the wobble must not survive.
        let steps: Vec<(f64, f64, bool)> = (0..40).map(|i| (0.5, if i % 2 == 0 { 0.1 } else { -0.1 }, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!((x - 20.0).abs() < 0.01, "travel along the axis is kept: {x}");
        assert!(y.abs() < 0.001, "travel across it is discarded: {y}");
    }

    #[test]
    fn it_snaps_to_the_nearest_division_not_only_to_horizontal() {
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 4, tolerance_deg: 45.0 },
            1.0,
            true,
        );
        let steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.1, 0.5, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!(x.abs() < 0.001, "a mostly-vertical drag locks to vertical: x={x}");
        assert!((y - 10.0).abs() < 0.01, "and keeps its vertical travel: y={y}");
    }

    #[test]
    fn a_diagonal_survives_at_eight_divisions() {
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 8, tolerance_deg: 22.5 },
            1.0,
            true,
        );
        let steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.5, 0.5, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!((x - y).abs() < 0.01, "45 degrees is an allowed direction: ({x}, {y})");
        assert!(x > 6.0, "and it is not collapsed onto an axis: {x}");
    }

    #[test]
    fn outside_the_tolerance_the_constraint_keeps_out_of_the_way() {
        // A magnet, not a cage — the spec's soft snap.
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 4, tolerance_deg: 5.0 },
            1.0,
            true,
        );
        let steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.5, 0.5, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!((y - 10.0).abs() < 0.01, "45 degrees is 40 degrees off an axis, so it is left alone");
        assert!((x - 10.0).abs() < 0.01);
    }

    #[test]
    fn strength_between_zero_and_one_leans_rather_than_pins() {
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 4, tolerance_deg: 45.0 },
            0.5,
            true,
        );
        let steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.5, 0.1, true)).collect();
        let (_, y) = run(&mut stage, &steps);
        assert!(y > 0.5 && y < 1.5, "half strength keeps about half the deviation: {y}");
    }

    #[test]
    fn zero_strength_changes_nothing() {
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 4, tolerance_deg: 45.0 },
            0.0,
            true,
        );
        let steps: Vec<(f64, f64, bool)> = (0..10).map(|_| (0.5, 0.2, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!((x - 5.0).abs() < 1e-9 && (y - 2.0).abs() < 1e-9, "({x}, {y})");
    }

    #[test]
    fn releasing_the_modifier_does_not_move_the_cursor() {
        // The failure this prevents: perpendicular motion discarded during the segment being
        // owed back all at once, flinging the cursor the moment the user lets go.
        let mut stage = Snap::default();
        let mut steps: Vec<(f64, f64, bool)> = (0..30).map(|_| (0.5, 0.3, true)).collect();
        steps.push((0.0, 0.0, false));
        steps.push((0.0, 0.0, false));
        let (_, y) = run(&mut stage, &steps);
        assert!(y.abs() < 0.001, "no spring-back on release: {y}");
    }

    #[test]
    fn a_line_holds_the_direction_it_set_off_in() {
        let mut stage = Snap::new(Constraint::Line, 1.0, true);
        // Establish a 2:1 direction, then try to veer off it.
        let mut steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.2, 0.1, true)).collect();
        steps.extend((0..20).map(|_| (0.2, -0.3, true)));
        let (x, y) = run(&mut stage, &steps);
        // Everything emitted must lie on the committed direction, whatever the hand did.
        let ratio = y / x;
        assert!((ratio - 0.5).abs() < 0.05, "the line held its slope: {ratio}");
    }

    #[test]
    fn a_line_is_free_until_it_has_a_direction_to_hold() {
        let mut stage = Snap::new(Constraint::Line, 1.0, true);
        // Less than the commit distance: nothing is locked yet, so nothing is discarded.
        let steps: Vec<(f64, f64, bool)> = (0..4).map(|_| (0.1, 0.1, true)).collect();
        let (x, y) = run(&mut stage, &steps);
        assert!((x - 0.4).abs() < 1e-9 && (y - 0.4).abs() < 1e-9, "({x}, {y})");
    }

    #[test]
    fn re_engaging_starts_a_fresh_segment() {
        let mut stage = Snap::default();
        let mut steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.5, 0.0, true)).collect();
        steps.extend((0..10).map(|_| (0.0, 0.5, false)));
        // Second engagement, now moving vertically: it must lock to vertical, not resume the
        // first segment's horizontal.
        steps.extend((0..20).map(|_| (0.05, 0.5, true)));
        let (x, _) = run(&mut stage, &steps);
        assert!((x - 10.0).abs() < 0.05, "the second segment locked vertical: x={x}");
    }

    #[test]
    fn always_on_needs_no_modifier() {
        let mut stage = Snap::new(
            Constraint::Angle { divisions: 4, tolerance_deg: 45.0 },
            1.0,
            false,
        );
        let steps: Vec<(f64, f64, bool)> = (0..20).map(|_| (0.5, 0.1, false)).collect();
        let (_, y) = run(&mut stage, &steps);
        assert!(y.abs() < 0.001, "activation=always ignores the modifier: {y}");
    }

    #[test]
    fn nothing_panics_on_pathological_input() {
        for c in [
            Constraint::Angle { divisions: 0, tolerance_deg: -5.0 },
            Constraint::Angle { divisions: u32::MAX, tolerance_deg: f64::NAN },
            Constraint::Line,
        ] {
            for strength in [0.0, 1.0, f64::NAN, -3.0, 1e9] {
                let mut stage = Snap::new(c, strength, true);
                for (dx, dy) in [(0.0, 0.0), (f64::NAN, 1.0), (1e300, -1e300), (0.1, 0.1)] {
                    let mut s = Sample::new(dx, dy, 1000, true);
                    s.dt = 0.001;
                    s.constrain = true;
                    stage.process(&mut s);
                    assert!(s.dx.is_finite() && s.dy.is_finite());
                }
            }
        }
    }
}
