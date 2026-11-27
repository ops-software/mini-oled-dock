//! Vertical scrolling of a popup list inside the room the work area leaves (wsD D8).
//!
//! A content that cannot fit lays its rows out shifted up by `offset` and paints them under a clip; this
//! type only keeps the numbers (view, content height, offset), the thin bar and the moment it fades. It adds no
//! timer: the dock's wrapper asks the popup to fire its own 1 s tick early after `by`, so the bar disappears on
//! time.

use std::cell::Cell;

use windows::Win32::Foundation::RECT;

use crate::gfx::scale::Scale;

/// Bar width (logical px), and when the pointer is over its zone.
pub const SCROLLBAR_W: i32 = crate::tokens::SCROLLBAR_W;
pub const SCROLLBAR_HOT_W: i32 = crate::tokens::SCROLLBAR_HOT_W;
/// Width (logical px) of the zone that grabs the bar, at the right edge of the view.
pub const SCROLLBAR_HIT: i32 = crate::tokens::SCROLLBAR_HIT;
/// Shortest thumb (logical px).
pub const SCROLL_THUMB_MIN: i32 = crate::tokens::SCROLL_THUMB_MIN;
pub const SCROLLBAR_ALPHA: u32 = crate::tokens::SCROLLBAR_REST;
pub const SCROLLBAR_ALPHA_HOT: u32 = crate::tokens::SCROLLBAR_HOT;
/// The bar disappears this long after the last scroll.
pub const SCROLLBAR_HIDE_MS: u64 = 1000;
/// One wheel notch (logical px).
pub const SCROLL_STEP: i32 = 48;
/// Gap (logical px) between the bar and the right edge of the view.
const BAR_INSET: i32 = 3;

pub struct ScrollModel {
    /// The part of the popup where the rows show (physical px).
    pub view: RECT,
    content_h: i32,
    offset: i32,
    shown_at: u64,
    /// The pointer is over the bar zone.
    pub hot: bool,
    /// The last paint drew the bar.
    painted_visible: Cell<bool>,
    m: Scale,
}

impl Default for ScrollModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollModel {
    pub fn new() -> ScrollModel {
        ScrollModel {
            view: RECT::default(),
            content_h: 0,
            offset: 0,
            shown_at: 0,
            hot: false,
            painted_visible: Cell::new(false),
            m: Scale { dpi: 96 },
        }
    }

    /// New geometry (physical px): the offset is clamped, and the first call that makes the area scrollable
    /// shows the bar.
    pub fn set(&mut self, m: Scale, view: RECT, content_h: i32, now: u64) {
        let was = self.scrollable();
        self.m = m;
        self.view = view;
        self.content_h = content_h.max(0);
        self.offset = self.offset.clamp(0, self.max());
        if self.scrollable() && !was {
            self.shown_at = now;
        }
    }

    /// Everything fits: no bar, no offset.
    pub fn clear(&mut self) {
        self.content_h = 0;
        self.offset = 0;
        self.hot = false;
    }

    /// One wheel notch (physical px).
    pub fn step(&self) -> i32 {
        self.m.px(SCROLL_STEP)
    }

    fn view_h(&self) -> i32 {
        (self.view.bottom - self.view.top).max(0)
    }

    /// Largest offset.
    pub fn max(&self) -> i32 {
        (self.content_h - self.view_h()).max(0)
    }

    pub fn scrollable(&self) -> bool {
        self.max() > 0
    }

    pub fn offset(&self) -> i32 {
        self.offset
    }

    /// Moves by `delta` physical px (positive = down). true if the offset changed. Shows the bar; the
    /// caller asks for the tick that will hide it.
    pub fn by(&mut self, delta: i32, now: u64) -> bool {
        let to = (self.offset + delta).clamp(0, self.max());
        let moved = to != self.offset;
        self.offset = to;
        self.shown_at = now;
        moved
    }

    fn bar_w(&self) -> i32 {
        self.m.px(if self.hot { SCROLLBAR_HOT_W } else { SCROLLBAR_W })
    }

    /// The thumb (physical px).
    pub fn thumb(&self) -> RECT {
        let vh = self.view_h();
        let h = if self.content_h > 0 { (vh as i64 * vh as i64 / self.content_h as i64) as i32 } else { vh };
        let h = h.max(self.m.px(SCROLL_THUMB_MIN)).min(vh);
        let max = self.max();
        let top = if max > 0 {
            self.view.top + ((vh - h) as i64 * self.offset as i64 / max as i64) as i32
        } else {
            self.view.top
        };
        let right = self.view.right - self.m.px(BAR_INSET);
        RECT { left: right - self.bar_w(), top, right, bottom: top + h }
    }

    /// The zone that grabs the thumb: the whole height of the view, at its right edge.
    pub fn hit_zone(&self) -> RECT {
        RECT {
            left: self.view.right - self.m.px(SCROLLBAR_HIT),
            top: self.view.top,
            right: self.view.right,
            bottom: self.view.bottom,
        }
    }

    pub fn in_zone(&self, x: i32, y: i32) -> bool {
        let z = self.hit_zone();
        self.scrollable() && x >= z.left && x < z.right && y >= z.top && y < z.bottom
    }

    /// Opacity of the bar at `now` (0 = not drawn).
    pub fn alpha(&self, now: u64) -> u32 {
        if !self.scrollable() {
            0
        } else if self.hot {
            SCROLLBAR_ALPHA_HOT
        } else if now.saturating_sub(self.shown_at) < SCROLLBAR_HIDE_MS {
            SCROLLBAR_ALPHA
        } else {
            0
        }
    }

    /// What to draw at `now`: the thumb and its opacity when the bar shows; remembers that it was drawn.
    pub fn paint_rect(&self, now: u64) -> Option<(RECT, u32)> {
        let a = self.alpha(now);
        self.painted_visible.set(a > 0);
        if a > 0 {
            Some((self.thumb(), a))
        } else {
            None
        }
    }

    /// true once, when the bar has to go from drawn to hidden.
    pub fn hide_due(&self, now: u64) -> bool {
        let due = self.painted_visible.get() && self.alpha(now) == 0;
        if due {
            self.painted_visible.set(false);
        }
        due
    }

    /// The thumb follows the pointer at `y` (its centre under the pointer). true if the offset changed.
    pub fn drag_to(&mut self, y: i32, now: u64) -> bool {
        let vh = self.view_h();
        let th = {
            let t = self.thumb();
            t.bottom - t.top
        };
        let room = (vh - th).max(1) as i64;
        let pos = (y - self.view.top - th / 2).clamp(0, room as i32) as i64;
        let to = (pos * self.max() as i64 / room) as i32;
        let moved = to != self.offset;
        self.offset = to.clamp(0, self.max());
        self.shown_at = now;
        moved
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scroll_aliases_equal_their_old_literals() {
        assert_eq!(SCROLLBAR_W, 3);
        assert_eq!(SCROLLBAR_HOT_W, 6);
        assert_eq!(SCROLLBAR_HIT, 12);
        assert_eq!(SCROLL_THUMB_MIN, 24);
        assert_eq!(SCROLLBAR_ALPHA, 50);
        assert_eq!(SCROLLBAR_ALPHA_HOT, 110);
    }

    use super::*;

    fn view(h: i32) -> RECT {
        RECT { left: 0, top: 0, right: 300, bottom: h }
    }

    fn area(dpi: u32, vh: i32, ch: i32) -> ScrollModel {
        let mut a = ScrollModel::new();
        a.set(Scale { dpi }, view(vh), ch, 0);
        a
    }

    #[test]
    fn clamp_and_max() {
        let mut a = area(96, 200, 500);
        assert_eq!(a.max(), 300);
        assert!(a.by(1000, 1));
        assert_eq!(a.offset(), 300);
        assert!(!a.by(10, 2));
        assert!(a.by(-1000, 3));
        assert_eq!(a.offset(), 0);
        // The content shrinks: the offset follows.
        a.by(250, 4);
        a.set(Scale { dpi: 96 }, view(200), 300, 5);
        assert_eq!(a.offset(), 100);
    }

    #[test]
    fn thumb_geometry() {
        let a = area(144, 400, 4000);
        let t = a.thumb();
        // 400 * 400 / 4000 = 40 px, over the minimum of 36 at 144 dpi.
        assert_eq!(t.bottom - t.top, 40);
        let small = area(144, 400, 400_000);
        let t = small.thumb();
        assert_eq!(t.bottom - t.top, 36);
        let mut a = area(96, 200, 500);
        a.by(300, 1);
        let t = a.thumb();
        assert_eq!(t.bottom, 200);
        assert_eq!(t.right - t.left, SCROLLBAR_W);
        a.hot = true;
        assert_eq!(a.thumb().right - a.thumb().left, SCROLLBAR_HOT_W);
    }

    #[test]
    fn fits_means_no_bar() {
        let mut a = area(96, 200, 200);
        assert!(!a.scrollable());
        assert_eq!(a.alpha(0), 0);
        assert!(!a.in_zone(299, 10));
        a.hot = true;
        assert_eq!(a.alpha(0), 0);
        let mut b = area(96, 200, 500);
        b.clear();
        assert!(!b.scrollable() && b.offset() == 0);
    }

    #[test]
    fn bar_fades_after_a_second() {
        let mut a = area(96, 200, 500);
        assert_eq!(a.alpha(10), SCROLLBAR_ALPHA);
        assert_eq!(a.alpha(SCROLLBAR_HIDE_MS - 1), SCROLLBAR_ALPHA);
        assert_eq!(a.alpha(SCROLLBAR_HIDE_MS), 0);
        a.by(10, 5_000);
        assert_eq!(a.alpha(5_999), SCROLLBAR_ALPHA);
        assert_eq!(a.alpha(6_000), 0);
        a.hot = true;
        assert_eq!(a.alpha(60_000), SCROLLBAR_ALPHA_HOT);
        // The hide edge fires once, after a paint that drew the bar.
        a.hot = false;
        assert!(!a.hide_due(7_000));
        a.painted_visible.set(true);
        assert!(a.hide_due(7_000));
        assert!(!a.hide_due(7_000));
    }

    #[test]
    fn hit_zone_is_12px_wide() {
        let a = area(96, 200, 500);
        let z = a.hit_zone();
        assert_eq!((z.left, z.right), (288, 300));
        assert!(a.in_zone(288, 0) && a.in_zone(299, 199));
        assert!(!a.in_zone(287, 0) && !a.in_zone(299, 200));
        let b = area(192, 200, 500);
        assert_eq!(b.hit_zone().right - b.hit_zone().left, 24);
    }

    #[test]
    fn dragging_the_thumb_maps_to_the_offset() {
        let mut a = area(96, 200, 500);
        let th = a.thumb().bottom - a.thumb().top;
        assert!(!a.drag_to(0, 1));
        assert_eq!(a.offset(), 0);
        assert!(a.drag_to(1000, 2));
        assert_eq!(a.offset(), a.max());
        a.drag_to(th / 2 + (200 - th) / 2, 3);
        assert!((a.offset() - a.max() / 2).abs() <= 2);
    }
}
