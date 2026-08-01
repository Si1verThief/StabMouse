//! Mapping hand movement onto a multi-screen desktop.
//!
//! [`SurfaceMapper`](crate::SurfaceMapper) maps millimetres onto one tablet surface. This maps
//! them onto a *layout* of screens, each of which has its own tablet, and says when the pen has
//! crossed from one to another.
//!
//! # Why position is tracked in desktop pixels
//!
//! The obvious alternative — a per-screen surface coordinate, reset on each crossing — cannot
//! answer "where am I" during the crossing itself. Tracking one position across the whole
//! layout means an edge crossing is a lookup rather than a state transition, and the arithmetic
//! that decides *which* screen never has to agree with a separate copy of it.
//!
//! Pixels rather than millimetres because that is the space the layout is expressed in.
//! Millimetres enter once, as a scale.
//!
//! # Screens are not a partition
//!
//! Monitor layouts have gaps. Two screens of different heights side by side leave an L-shaped
//! void that is inside the bounding box but on no screen at all. A position there belongs
//! nowhere, so the rule is to **stay on the screen already occupied** rather than pick a
//! nearest one — the cursor slides along the edge, which is what every desktop does.

/// One screen in the layout, in logical desktop pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// Connector name, used to address the screen's tablet — `DP-2`.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Screen {
    fn right(&self) -> f64 {
        f64::from(self.x) + f64::from(self.width)
    }

    fn bottom(&self) -> f64 {
        f64::from(self.y) + f64::from(self.height)
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x) && x < self.right() && y >= f64::from(self.y) && y < self.bottom()
    }

    /// Surface extent for this screen's tablet, scaled so the longer axis is `full`.
    ///
    /// This is what keeps millimetres isotropic. A square surface stretched onto a non-square
    /// screen makes vertical movement travel further than horizontal for the same hand motion,
    /// so a hand-drawn circle comes out as an ellipse.
    pub fn surface_extent(&self, full: i32) -> (i32, i32) {
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        if w <= 0.0 || h <= 0.0 {
            return (full, full);
        }
        let full_f = f64::from(full);
        if w >= h {
            (full, (full_f * h / w).round().max(1.0) as i32)
        } else {
            ((full_f * w / h).round().max(1.0) as i32, full)
        }
    }
}

/// Where the pen is, expressed for the screen it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Index into the screen list.
    pub screen: usize,
    /// Position in that screen's tablet surface units.
    pub x: i32,
    pub y: i32,
    /// The screen just left, when this advance crossed a boundary.
    pub crossed_from: Option<usize>,
}

pub struct DesktopMapper {
    screens: Vec<Screen>,
    /// Position in logical desktop pixels.
    x: f64,
    y: f64,
    px_per_mm: f64,
    /// Full surface extent along a screen's longer axis.
    surface_max: i32,
    active: usize,
}

impl DesktopMapper {
    /// `span_mm` is how much hand movement crosses the widest screen horizontally.
    ///
    /// One scale for the whole desktop, not one per screen: a millimetre of hand movement has
    /// to mean the same distance everywhere, or the pointer changes speed as it crosses a
    /// boundary.
    pub fn new(screens: Vec<Screen>, span_mm: f64, surface_max: i32) -> Self {
        let widest = screens.iter().map(|s| s.width).max().unwrap_or(1920).max(1);
        let span = if span_mm.is_finite() && span_mm > 0.0 {
            span_mm
        } else {
            200.0
        };

        // Start centred on the first screen rather than at the layout origin, which may be a
        // gap or a corner.
        let (x, y) = screens
            .first()
            .map(|s| {
                (
                    f64::from(s.x) + f64::from(s.width) / 2.0,
                    f64::from(s.y) + f64::from(s.height) / 2.0,
                )
            })
            .unwrap_or((0.0, 0.0));

        Self {
            screens,
            x,
            y,
            px_per_mm: f64::from(widest) / span,
            surface_max: surface_max.max(1),
            active: 0,
        }
    }

    pub fn screens(&self) -> &[Screen] {
        &self.screens
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// Desktop pixels per millimetre of hand movement.
    ///
    /// The scale a tablet mode is tuned against. A fallback to the pointer has to reuse it or
    /// the sensitivity changes as the transport does, which is the one thing the fallback is
    /// supposed to hide.
    pub fn px_per_mm(&self) -> f64 {
        self.px_per_mm
    }

    pub fn set_span_mm(&mut self, span_mm: f64) {
        if span_mm.is_finite() && span_mm > 0.0 {
            let widest = self.screens.iter().map(|s| s.width).max().unwrap_or(1920).max(1);
            self.px_per_mm = f64::from(widest) / span_mm;
        }
    }

    /// Advance by a millimetre delta.
    ///
    /// `pen_down` confines movement to the current screen. Crossing mid-stroke would have to
    /// end the stroke on one tablet and start a new one on the next, which loses the stroke —
    /// so the pen slides along the boundary instead, as it would on a physical tablet's edge.
    pub fn advance(&mut self, dx_mm: f64, dy_mm: f64, pen_down: bool) -> Option<Placement> {
        let (dx_px, dy_px) = (dx_mm * self.px_per_mm, dy_mm * self.px_per_mm);
        self.advance_px(dx_px, dy_px, pen_down)
    }

    /// Advance by a pixel delta, for callers with their own scale.
    ///
    /// Mouse modes share this position but not the tablet's millimetre span — their scale is
    /// the source device's counts-per-millimetre (D23). One position, per-mode scales.
    pub fn advance_px(&mut self, dx_px: f64, dy_px: f64, pen_down: bool) -> Option<Placement> {
        if self.screens.is_empty() {
            return None;
        }

        let mut x = self.x;
        let mut y = self.y;
        if dx_px.is_finite() {
            x += dx_px;
        }
        if dy_px.is_finite() {
            y += dy_px;
        }

        let from = self.active;
        if pen_down {
            let s = &self.screens[self.active];
            // `next_below` rather than the edge itself: the right and bottom edges belong to the
            // neighbour, so clamping to them would place the pen on a screen it is not on.
            self.x = x.clamp(f64::from(s.x), next_below(s.right()));
            self.y = y.clamp(f64::from(s.y), next_below(s.bottom()));
        } else {
            match self.screens.iter().position(|s| s.contains(x, y)) {
                Some(found) => {
                    self.x = x;
                    self.y = y;
                    self.active = found;
                }
                None => {
                    // Off every screen: either outside the layout or in a gap between monitors.
                    // Slide along the current screen's edge rather than jumping somewhere.
                    let s = &self.screens[self.active];
                    self.x = x.clamp(f64::from(s.x), next_below(s.right()));
                    self.y = y.clamp(f64::from(s.y), next_below(s.bottom()));
                }
            }
        }

        Some(self.placement(if self.active == from { None } else { Some(from) }))
    }

    /// The tracked position in logical desktop pixels, `None` when the layout is unknown.
    ///
    /// This is what the absolute pointer emits during a fallback: the same position the
    /// tablet would have used, so the cursor cannot diverge between the two transports.
    pub fn position_px(&self) -> Option<(f64, f64)> {
        if self.screens.is_empty() {
            None
        } else {
            Some((self.x, self.y))
        }
    }

    /// Adopt a position reported from outside — the compositor's own cursor.
    ///
    /// An unmanaged device (a second mouse, a touchpad) moves the cursor without this mapper
    /// seeing it; adopting the report keeps the shared position truthful so the next managed
    /// motion continues from where the cursor actually is instead of teleporting it back.
    ///
    /// A position on no screen — reported mid-gap, or from a stale layout — clamps into the
    /// current screen, matching how `advance` treats the void between monitors.
    pub fn set_position_px(&mut self, x: f64, y: f64) {
        if self.screens.is_empty() || !x.is_finite() || !y.is_finite() {
            return;
        }
        match self.screens.iter().position(|s| s.contains(x, y)) {
            Some(found) => {
                self.x = x;
                self.y = y;
                self.active = found;
            }
            None => {
                let s = &self.screens[self.active];
                self.x = x.clamp(f64::from(s.x), next_below(s.right()));
                self.y = y.clamp(f64::from(s.y), next_below(s.bottom()));
            }
        }
    }

    /// Where the pen currently is, without moving it.
    pub fn placement_now(&self) -> Option<Placement> {
        if self.screens.is_empty() {
            return None;
        }
        Some(self.placement(None))
    }

    fn placement(&self, crossed_from: Option<usize>) -> Placement {
        let s = &self.screens[self.active];
        let (max_x, max_y) = s.surface_extent(self.surface_max);

        let fx = if s.width > 0 {
            (self.x - f64::from(s.x)) / f64::from(s.width)
        } else {
            0.0
        };
        let fy = if s.height > 0 {
            (self.y - f64::from(s.y)) / f64::from(s.height)
        } else {
            0.0
        };

        Placement {
            screen: self.active,
            x: (fx.clamp(0.0, 1.0) * f64::from(max_x)) as i32,
            y: (fy.clamp(0.0, 1.0) * f64::from(max_y)) as i32,
            crossed_from,
        }
    }
}

/// The largest value strictly below `v`, so a clamp lands inside a half-open range.
fn next_below(v: f64) -> f64 {
    let below = v - 1.0;
    if below < 0.0 {
        0.0
    } else {
        below
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, x: i32, y: i32, w: i32, h: i32) -> Screen {
        Screen {
            name: name.into(),
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// The host layout this was designed against: 16:9 beside 5:4.
    fn two() -> Vec<Screen> {
        vec![
            screen("HDMI-A-1", 0, 0, 1920, 1080),
            screen("DP-2", 1920, 0, 1280, 1024),
        ]
    }

    fn mapper() -> DesktopMapper {
        // 200mm of hand movement crosses the widest screen, so 9.6 px/mm.
        DesktopMapper::new(two(), 200.0, 32767)
    }

    #[test]
    fn it_starts_on_the_first_screen() {
        let m = mapper();
        let p = m.placement_now().unwrap();
        assert_eq!(p.screen, 0);
        assert_eq!(p.crossed_from, None);
    }

    #[test]
    fn moving_right_far_enough_crosses_to_the_next_screen() {
        let mut m = mapper();
        // Centre is 960px in; 150mm at 9.6 px/mm is 1440px, landing at 2400 — on DP-2.
        let p = m.advance(150.0, 0.0, false).unwrap();
        assert_eq!(p.screen, 1);
        assert_eq!(p.crossed_from, Some(0), "a crossing must be reported so the pen can hand over");
    }

    #[test]
    fn a_crossing_is_reported_once_not_every_sample() {
        let mut m = mapper();
        m.advance(150.0, 0.0, false).unwrap();
        let again = m.advance(1.0, 0.0, false).unwrap();
        assert_eq!(again.screen, 1);
        assert_eq!(
            again.crossed_from, None,
            "reporting a crossing repeatedly would send a proximity-out on every sample"
        );
    }

    #[test]
    fn the_pen_does_not_cross_screens_mid_stroke() {
        let mut m = mapper();
        // Far enough to reach the next screen twice over, but with the pen down.
        let p = m.advance(500.0, 0.0, true).unwrap();
        assert_eq!(p.screen, 0, "a stroke must not be split across two tablets");
        assert_eq!(p.crossed_from, None);
        // Against the edge, not stopped short of it. Not an exact value: the clamp lands one
        // pixel inside the half-open range, so the surface figure is just under maximum.
        assert!(p.x > 32700, "it should slide along the boundary, got {}", p.x);
    }

    #[test]
    fn releasing_after_a_clamped_stroke_allows_the_crossing() {
        let mut m = mapper();
        m.advance(500.0, 0.0, true).unwrap();
        // The position was held at screen 0's edge, so a small further move crosses.
        let p = m.advance(1.0, 0.0, false).unwrap();
        assert_eq!(p.screen, 1);
        assert_eq!(p.crossed_from, Some(0));
    }

    #[test]
    fn surface_extents_match_each_screens_aspect() {
        let s = two();
        // 16:9 -> the short axis is 9/16 of full.
        assert_eq!(s[0].surface_extent(32767), (32767, 18431));
        // 5:4 -> 4/5 of full.
        assert_eq!(s[1].surface_extent(32767), (32767, 26214));
    }

    #[test]
    fn equal_hand_movement_covers_equal_pixels_on_both_screens() {
        // The point of a single desktop-wide scale. 10mm of movement must be the same distance
        // on either screen, or the pointer changes speed as it crosses.
        let mut m = mapper();
        let before = m.placement_now().unwrap();
        m.advance(10.0, 0.0, false).unwrap();
        let after = m.advance(0.0, 0.0, false).unwrap();
        let on_first = f64::from(after.x - before.x) / 32767.0 * 1920.0;

        let mut m2 = mapper();
        m2.advance(150.0, 0.0, false).unwrap();
        let b2 = m2.placement_now().unwrap();
        m2.advance(10.0, 0.0, false).unwrap();
        let a2 = m2.placement_now().unwrap();
        let on_second = f64::from(a2.x - b2.x) / 32767.0 * 1280.0;

        assert!(
            (on_first - on_second).abs() < 2.0,
            "same movement gave {on_first:.1}px on one screen and {on_second:.1}px on the other"
        );
    }

    #[test]
    fn a_gap_between_screens_keeps_the_pen_where_it_was() {
        // Stacked with a horizontal offset, leaving a region inside the bounding box that is on
        // no screen at all.
        let screens = vec![screen("A", 0, 0, 1000, 1000), screen("B", 2000, 0, 1000, 1000)];
        let mut m = DesktopMapper::new(screens, 100.0, 32767);
        let p = m.advance(60.0, 0.0, false).unwrap();
        assert_eq!(p.screen, 0, "a position in the void must not teleport to another screen");
        assert!(p.x > 32700, "it should rest against the edge it reached, got {}", p.x);
    }

    #[test]
    fn vertical_layouts_cross_too() {
        let screens = vec![screen("top", 0, 0, 1920, 1080), screen("bottom", 0, 1080, 1920, 1080)];
        let mut m = DesktopMapper::new(screens, 200.0, 32767);
        let p = m.advance(0.0, 100.0, false).unwrap();
        assert_eq!(p.screen, 1);
        assert_eq!(p.crossed_from, Some(0));
    }

    #[test]
    fn no_screens_means_no_placement() {
        // Not a panic and not a fabricated screen: the caller falls back to the single-tablet
        // path when the compositor tells us nothing.
        let mut m = DesktopMapper::new(Vec::new(), 200.0, 32767);
        assert!(m.advance(10.0, 10.0, false).is_none());
        assert!(m.placement_now().is_none());
    }

    #[test]
    fn pixel_advance_moves_the_same_shared_position() {
        let mut m = mapper();
        let (x0, y0) = m.position_px().unwrap();
        m.advance_px(100.0, 0.0, false);
        assert_eq!(m.position_px().unwrap(), (x0 + 100.0, y0));
    }

    #[test]
    fn an_adopted_position_lands_on_the_screen_that_contains_it() {
        let mut m = mapper();
        m.set_position_px(2500.0, 500.0);
        assert_eq!(m.position_px().unwrap(), (2500.0, 500.0));
        assert_eq!(m.active(), 1, "adoption must also move the active screen");
    }

    #[test]
    fn an_adopted_position_in_the_void_clamps_into_the_current_screen() {
        // (2500, 1060) is inside the bounding box but below DP-2 — the dead strip. A stale
        // layout or a mid-gap report must not teleport the position off every screen.
        let mut m = mapper();
        m.set_position_px(2500.0, 1060.0);
        let (x, y) = m.position_px().unwrap();
        assert!(x < 1920.0, "clamped into the active screen, got ({x}, {y})");
        assert_eq!(m.active(), 0);
    }

    #[test]
    fn non_finite_movement_is_ignored_rather_than_poisoning_the_position() {
        let mut m = mapper();
        let before = m.placement_now().unwrap();
        let after = m.advance(f64::NAN, f64::INFINITY, false).unwrap();
        assert_eq!(before.x, after.x);
        assert_eq!(before.y, after.y);
    }

    #[test]
    fn a_portrait_screen_scales_its_long_axis_to_full() {
        let s = screen("rotated", 0, 0, 1080, 1920);
        assert_eq!(s.surface_extent(32767), (18431, 32767));
    }
}
