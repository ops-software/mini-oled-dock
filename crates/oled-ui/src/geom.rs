//! Geometry (F-K1, P2-kit 2.2): points, sizes and rectangles in physical and logical px, the kit's methods on W3's
//! Scale and the focus ring's geometry (P1-dock-keys Z3).

use windows::Win32::Foundation::RECT;

use crate::tokens;
pub use crate::w3::Scale;

/// A point in physical px, client coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PointP {
    pub x: i32,
    pub y: i32,
}

/// A size in physical px.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SizeP {
    pub w: i32,
    pub h: i32,
}

/// A rectangle in physical px: [l, r) x [t, b).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RectP {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

/// A point in logical px (96 DPI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PointL {
    pub x: i32,
    pub y: i32,
}

/// A size in logical px (96 DPI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SizeL {
    pub w: i32,
    pub h: i32,
}

impl RectP {
    /// The empty rectangle at the origin.
    pub const EMPTY: RectP = RectP { l: 0, t: 0, r: 0, b: 0 };

    /// The rectangle at (x, y) of size w x h.
    pub const fn wh(x: i32, y: i32, w: i32, h: i32) -> RectP {
        RectP { l: x, t: y, r: x + w, b: y + h }
    }

    /// Width.
    pub const fn w(&self) -> i32 {
        self.r - self.l
    }

    /// Height.
    pub const fn h(&self) -> i32 {
        self.b - self.t
    }

    /// True when it covers no pixel (w <= 0 or h <= 0).
    pub fn is_empty(&self) -> bool {
        self.w() <= 0 || self.h() <= 0
    }

    /// True when `p` is inside: [l, r) x [t, b).
    pub fn contains(&self, p: PointP) -> bool {
        p.x >= self.l && p.x < self.r && p.y >= self.t && p.y < self.b
    }

    /// The common part; EMPTY when the two are disjoint.
    pub fn intersect(&self, o: RectP) -> RectP {
        let r = RectP { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) };
        if r.is_empty() {
            RectP::EMPTY
        } else {
            r
        }
    }

    /// The smallest rectangle holding both; an empty side is ignored (EMPTY when both are).
    pub fn union(&self, o: RectP) -> RectP {
        match (self.is_empty(), o.is_empty()) {
            (true, true) => RectP::EMPTY,
            (true, false) => o,
            (false, true) => *self,
            (false, false) => RectP { l: self.l.min(o.l), t: self.t.min(o.t), r: self.r.max(o.r), b: self.b.max(o.b) },
        }
    }

    /// Shrunk by dx on the left and right and dy on the top and bottom (negative values grow it).
    pub fn inset(&self, dx: i32, dy: i32) -> RectP {
        RectP { l: self.l + dx, t: self.t + dy, r: self.r - dx, b: self.b - dy }
    }

    /// Moved by (dx, dy).
    pub fn offset(&self, dx: i32, dy: i32) -> RectP {
        RectP { l: self.l + dx, t: self.t + dy, r: self.r + dx, b: self.b + dy }
    }

    /// Pixel count; 0 when empty.
    pub fn area(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            i64::from(self.w()) * i64::from(self.h())
        }
    }
}

impl From<RectP> for RECT {
    fn from(r: RectP) -> RECT {
        RECT { left: r.l, top: r.t, right: r.r, bottom: r.b }
    }
}

impl From<RECT> for RectP {
    fn from(r: RECT) -> RectP {
        RectP { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }
}

impl Scale {
    /// A logical size in physical px.
    pub fn size(&self, s: SizeL) -> SizeP {
        SizeP { w: self.px(s.w), h: self.px(s.h) }
    }

    /// A logical point in physical px.
    pub fn point(&self, p: PointL) -> PointP {
        PointP { x: self.px(p.x), y: self.px(p.y) }
    }

    /// Physical px to logical px, rounded: the inverse of px().
    pub fn logical(&self, physical: i32) -> i32 {
        ((physical as f32) / self.scale()).round() as i32
    }

    /// Focus ring width in physical px: tokens::rim_focus_px, 1.5 logical px rounded half up (2, 2, 2, 3 px at 96, 120,
    /// 144, 192 DPI).
    pub fn ring(&self) -> i32 {
        tokens::rim_focus_px(self.dpi)
    }
}

/// Where a focus ring is drawn: an inner ring of `width` px inside `rect`, with corner `radius`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusRing {
    pub rect: RectP,
    pub radius: i32,
    pub width: i32,
}

/// The ring outside a control (P1-dock-keys Z3): the plate inflated by FOCUS_GAP plus the ring width, the radius grown by
/// the same amount.
pub fn focus_ring(plate: RectP, plate_radius: i32, s: Scale) -> FocusRing {
    let width = s.ring();
    let grow = s.px(tokens::FOCUS_GAP) + width;
    FocusRing { rect: plate.inset(-grow, -grow), radius: plate_radius + grow, width }
}

/// The ring on a row plate: the plate inset by one logical px, the radius shrunk by the same amount (never below 0).
pub fn focus_ring_inset(plate: RectP, plate_radius: i32, s: Scale) -> FocusRing {
    let d = s.px(1);
    FocusRing { rect: plate.inset(d, d), radius: (plate_radius - d).max(0), width: s.ring() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_set_operations() {
        let a = RectP::wh(0, 0, 10, 10);
        let b = RectP::wh(5, 5, 10, 10);
        assert_eq!(a.intersect(b), RectP { l: 5, t: 5, r: 10, b: 10 });
        assert_eq!(a.intersect(RectP::wh(20, 20, 1, 1)), RectP::EMPTY);
        assert_eq!(a.union(b), RectP { l: 0, t: 0, r: 15, b: 15 });
        assert_eq!(a.union(RectP::EMPTY), a);
        assert_eq!(RectP::EMPTY.union(b), b);
        assert!(a.contains(PointP { x: 0, y: 9 }) && !a.contains(PointP { x: 10, y: 0 }));
        assert_eq!(a.inset(2, 3), RectP { l: 2, t: 3, r: 8, b: 7 });
        assert_eq!(a.offset(1, -1), RectP { l: 1, t: -1, r: 11, b: 9 });
        assert_eq!(a.area(), 100);
        assert_eq!(RectP { l: 5, t: 0, r: 2, b: 9 }.area(), 0);
        let r: RECT = b.into();
        assert_eq!(RectP::from(r), b);
    }

    #[test]
    fn scale_round_trips() {
        let s = Scale { dpi: 144 };
        assert_eq!(s.size(SizeL { w: 400, h: 300 }), SizeP { w: 600, h: 450 });
        assert_eq!(s.point(PointL { x: 7, y: 2 }), PointP { x: 11, y: 3 });
        assert_eq!(s.logical(600), 400);
        assert_eq!(Scale { dpi: 120 }.logical(9), 7);
    }

    #[test]
    fn ring_geometry_matches_dock_keys() {
        let r = focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, Scale { dpi: 96 });
        assert_eq!(r, FocusRing { rect: RectP { l: 6, t: 1, r: 40, b: 35 }, radius: 11, width: 2 });
        let r = focus_ring(RectP { l: 20, t: 10, r: 72, b: 62 }, 14, Scale { dpi: 192 });
        assert_eq!(r, FocusRing { rect: RectP { l: 13, t: 3, r: 79, b: 69 }, radius: 21, width: 3 });
        let r = focus_ring_inset(RectP::wh(0, 0, 100, 28), 4, Scale { dpi: 144 });
        assert_eq!(r, FocusRing { rect: RectP { l: 2, t: 2, r: 98, b: 26 }, radius: 2, width: 2 });
    }
}
