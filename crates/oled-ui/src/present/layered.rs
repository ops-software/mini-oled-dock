//! The layered presenter of popups (K1a.9, KA2): menus, tooltips and, later, the dock's popups. One premultiplied DIB
//! of the window's size; a paint draws the damage's bounding box, cleared to transparent, and UpdateLayeredWindowIndirect
//! shows it with ULW_ALPHA, the source DC and that dirty rect. A move or an alpha change alone passes no source DC
//! (A03 section 1). The call is made after the window's borrow has ended: it sends messages.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HDC};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO};

use super::banded::BandDib;
use super::Damage;
use crate::geom::{PointP, RectP, SizeP};
use crate::w3::Canvas;

/// What UpdateLayeredWindowIndirect needs, taken out of the window's borrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Present {
    /// The DC holding the DIB; None for a move or an alpha change alone.
    pub(crate) src: Option<HDC>,
    pub(crate) size: SizeP,
    pub(crate) dirty: RectP,
    /// The window's new top-left corner (screen coordinates); None keeps it.
    pub(crate) pos: Option<PointP>,
    pub(crate) alpha: u8,
}

/// The rect a present updates: the damage's bounding box, inside the window.
pub(crate) fn dirty_rect(d: &Damage, size: SizeP) -> RectP {
    d.bbox().intersect(RectP::wh(0, 0, size.w, size.h))
}

/// The presenter of a layered window.
pub(crate) struct LayeredPresenter {
    dib: Option<BandDib>,
}

impl LayeredPresenter {
    /// Nothing is made before the first paint.
    pub(crate) fn new() -> LayeredPresenter {
        LayeredPresenter { dib: None }
    }

    /// Drops the DIB (a DPI change): the next paint makes it again.
    pub(crate) fn reset(&mut self) {
        self.dib = None;
    }

    /// The whole DIB as a canvas (K2a.10's retained scroll); an empty canvas before the first paint.
    #[allow(dead_code)] // K2a.10 (KC2) scrolls a layered window's pixels through it.
    pub(crate) fn canvas(&mut self) -> Canvas<'_> {
        match self.dib.as_mut() {
            Some(d) => {
                let (w, h) = (d.w, d.h);
                Canvas { buf: d.rows_mut(h), w, h, clip: None }
            }
            None => Canvas { buf: &mut [], w: 0, h: 0, clip: None },
        }
    }

    /// Paints the damage of a `size` window: its bounding box cleared to transparent, then drawn by `paint` (a canvas over
    /// the whole DIB, the origin and the clip). Some(the present) when there is something to show.
    pub(crate) fn render(
        &mut self,
        size: SizeP,
        damage: &Damage,
        paint: &mut dyn FnMut(Canvas<'_>, PointP, RectP),
    ) -> Option<Present> {
        let dirty = dirty_rect(damage, size);
        if dirty.is_empty() {
            return None;
        }
        if self.dib.as_ref().is_none_or(|d| d.w != size.w || d.h != size.h) {
            self.dib = None;
            self.dib = BandDib::new(size.w, size.h);
        }
        let dib = self.dib.as_mut()?;
        let (w, h) = (dib.w, dib.h);
        let buf = dib.rows_mut(h);
        for y in dirty.t..dirty.b {
            let row = (y * w) as usize;
            buf[row + dirty.l as usize..row + dirty.r as usize].fill(0);
        }
        paint(Canvas { buf, w, h, clip: None }, PointP { x: 0, y: 0 }, dirty);
        Some(Present { src: Some(dib.dc), size, dirty, pos: None, alpha: 255 })
    }
}

/// UpdateLayeredWindowIndirect with ULW_ALPHA; false when Windows refuses it.
pub(crate) fn present(hwnd: HWND, p: &Present) -> bool {
    let size = SIZE { cx: p.size.w, cy: p.size.h };
    let origin = POINT { x: 0, y: 0 };
    let pos = p.pos.map(|q| POINT { x: q.x, y: q.y });
    let dirty = RECT::from(p.dirty);
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: p.alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let with_src = p.src.is_some();
    let info = UPDATELAYEREDWINDOWINFO {
        cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
        hdcDst: HDC::default(),
        pptDst: pos.as_ref().map_or(std::ptr::null(), |q| q as *const POINT),
        psize: &size,
        hdcSrc: p.src.unwrap_or_default(),
        pptSrc: if with_src { &origin } else { std::ptr::null() },
        crKey: COLORREF(0),
        pblend: &blend,
        dwFlags: ULW_ALPHA,
        prcDirty: if with_src { &dirty } else { std::ptr::null() },
    };
    // SAFETY: every pointer in `info` points to a local that outlives the call; the source DC holds the window-sized
    // DIB (it lives in the window's presenter, which nothing frees while this runs on the window's thread).
    unsafe { UpdateLayeredWindowIndirect(hwnd, &info) }.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_dirty_rect_is_damage_bbox() {
        let size = SizeP { w: 300, h: 200 };
        let all = RectP::wh(0, 0, 300, 200);
        let mut d = Damage::default();
        assert_eq!(dirty_rect(&d, size), RectP::EMPTY);
        d.add(RectP::wh(10, 10, 20, 20), all);
        d.add(RectP::wh(200, 150, 30, 30), all);
        assert_eq!(d.rects().len(), 2);
        assert_eq!(dirty_rect(&d, size), RectP { l: 10, t: 10, r: 230, b: 180 }, "one update for both rects");
        assert_eq!(
            dirty_rect(&d, SizeP { w: 100, h: 100 }),
            RectP { l: 10, t: 10, r: 100, b: 100 },
            "inside the window"
        );
    }
}
