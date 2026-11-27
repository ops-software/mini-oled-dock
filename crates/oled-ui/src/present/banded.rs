//! The banded presenter of opaque windows (K1a.7, K1a.9, KA2): the paint rect in bands of band_height rows, each
//! cleared to bg, painted by the screen, then hooks::after_paint_band, then copied to the window. One band DIB, made at
//! the first paint and again on a width change, so a window never holds a window-sized back buffer (SPEC-platform 5.3.1).

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateRectRgn, DeleteDC, DeleteObject, EndPaint,
    GdiFlush, GetRegionData, GetUpdateRgn, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    HDC, HGDIOBJ, HRGN, NULLREGION, PAINTSTRUCT, RGNDATA, RGN_ERROR, SRCCOPY,
};

use crate::geom::{PointP, RectP, SizeP};
use crate::host::window;
use crate::present::{band_height, Damage};
use crate::w3::Canvas;

/// The bands of `r`: from its top, band_h rows each (the last one shorter), disjoint, their union `r`.
pub(crate) fn bands(r: RectP, band_h: i32) -> impl Iterator<Item = RectP> {
    let step = band_h.max(1);
    let empty = r.is_empty();
    (0..).map(move |k: i32| r.t + k * step).take_while(move |t| !empty && *t < r.b).map(move |t| RectP {
        l: r.l,
        t,
        r: r.r,
        b: (t + step).min(r.b),
    })
}

/// One top-down 32 bpp DIB section selected in its own memory DC (the band of an opaque window, the whole of a layered
/// one).
pub(crate) struct BandDib {
    pub(crate) dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl BandDib {
    pub(crate) fn new(w: i32, h: i32) -> Option<BandDib> {
        if w <= 0 || h <= 0 {
            return None;
        }
        // SAFETY: a memory DC compatible with the screen; deleted in Drop (or below on failure).
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `bmi` is valid; `bits` receives the pointer to the pixels the bitmap owns.
        let bmp = match unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) } {
            Ok(b) if !bits.is_null() => b,
            other => {
                // SAFETY: the DC and any bitmap were just made here and are selected nowhere.
                unsafe {
                    if let Ok(b) = other {
                        let _ = DeleteObject(HGDIOBJ(b.0));
                    }
                    let _ = DeleteDC(dc);
                }
                return None;
            }
        };
        // SAFETY: `dc` and `bmp` are ours; the DC's original bitmap is kept to restore it before deleting ours.
        let old = unsafe { SelectObject(dc, HGDIOBJ(bmp.0)) };
        Some(BandDib { dc, bmp, old, bits: bits.cast::<u32>(), w, h })
    }

    /// The first `rows` rows of pixels (premultiplied 0xAARRGGBB).
    pub(crate) fn rows_mut(&mut self, rows: i32) -> &mut [u32] {
        let rows = rows.clamp(0, self.h);
        // SAFETY: `bits` points to w * h u32 owned by the DIB section, alive as long as `self`; only the CPU writes
        // them (GDI reads them in BitBlt, after this slice is gone and before the next one is made).
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w as usize) * (rows as usize)) }
    }
}

impl Drop for BandDib {
    fn drop(&mut self) {
        // SAFETY: the DC and bitmap were made by new(); the original bitmap is selected back before ours is deleted.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The presenter of an opaque window.
pub(crate) struct BandedPresenter {
    dib: Option<BandDib>,
}

impl BandedPresenter {
    /// Nothing is made before the first paint.
    pub(crate) fn new() -> BandedPresenter {
        BandedPresenter { dib: None }
    }

    /// Drops the band DIB (a DPI change): the next paint makes it again.
    pub(crate) fn reset(&mut self) {
        self.dib = None;
    }

    /// Paints `rects` (window coordinates) of a `client`-sized window onto `hdc`: each in bands cleared to `bg`
    /// (premultiplied), drawn by `band` (a canvas over the band, the band's origin and its clip), then copied.
    pub(crate) fn paint(
        &mut self,
        hdc: HDC,
        client: SizeP,
        rects: &[RectP],
        bg: u32,
        band: &mut dyn FnMut(Canvas<'_>, PointP, RectP),
    ) {
        if client.w <= 0 || client.h <= 0 {
            return;
        }
        let bh = band_height(client.w);
        if self.dib.as_ref().is_none_or(|d| d.w != client.w || d.h != bh) {
            self.dib = None;
            self.dib = BandDib::new(client.w, bh);
        }
        let Some(dib) = self.dib.as_mut() else { return };
        let full = RectP::wh(0, 0, client.w, client.h);
        for r in rects {
            for b in bands(r.intersect(full), bh) {
                let (w, rows) = (dib.w, b.h());
                {
                    let buf = dib.rows_mut(rows);
                    for y in 0..rows {
                        let row = (y * w) as usize;
                        buf[row + b.l as usize..row + b.r as usize].fill(bg);
                    }
                    band(Canvas { buf, w, h: rows, clip: None }, PointP { x: 0, y: b.t }, b);
                }
                // SAFETY: both DCs are valid; the DIB's rows [0, rows) hold the band. GdiFlush makes the copy happen
                // before the DIB is written again for the next band.
                unsafe {
                    let _ = BitBlt(hdc, b.l, b.t, b.w(), rows, Some(dib.dc), b.l, 0, SRCCOPY);
                    let _ = GdiFlush();
                }
            }
        }
    }
}

/// The rects of a GetRegionData buffer: its RGNDATAHEADER, then nCount RECTs. None for a malformed buffer, no rect, or
/// more than Damage::MAX of them (the caller paints the paint rect instead).
pub(crate) fn region_rects(data: &[u8]) -> Option<Vec<RectP>> {
    let word = |o: usize| data.get(o..o + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let header = usize::try_from(word(0)?).ok()?;
    let count = usize::try_from(word(8)?).ok()?;
    if count == 0 || count > Damage::MAX {
        return None;
    }
    (0..count)
        .map(|i| {
            let o = header + 16 * i;
            Some(RectP { l: word(o)?, t: word(o + 4)?, r: word(o + 8)?, b: word(o + 12)? })
        })
        .collect()
}

/// The update region's rects, read before BeginPaint validates it; None when there are more than Damage::MAX, none, or
/// the region cannot be read.
fn update_rects(hwnd: HWND) -> Option<Vec<RectP>> {
    // SAFETY: a region of our own, deleted below.
    let rgn = unsafe { CreateRectRgn(0, 0, 0, 0) };
    if rgn.is_invalid() {
        return None;
    }
    let rects = region_of(hwnd, rgn);
    // SAFETY: the region made above, selected nowhere.
    let _ = unsafe { DeleteObject(HGDIOBJ(rgn.0)) };
    rects
}

fn region_of(hwnd: HWND, rgn: HRGN) -> Option<Vec<RectP>> {
    // SAFETY: our window and our region: GetUpdateRgn copies the update region into it and sends no message.
    let kind = unsafe { GetUpdateRgn(hwnd, rgn, false) };
    if kind == RGN_ERROR || kind == NULLREGION {
        return None;
    }
    // SAFETY: a size query, no buffer.
    let size = unsafe { GetRegionData(rgn, 0, None) } as usize;
    if size < std::mem::size_of::<windows::Win32::Graphics::Gdi::RGNDATAHEADER>() {
        return None;
    }
    let mut buf = vec![0u64; size.div_ceil(8)];
    // SAFETY: `buf` holds at least `size` bytes, aligned for RGNDATA.
    let got = unsafe { GetRegionData(rgn, size as u32, Some(buf.as_mut_ptr().cast::<RGNDATA>())) } as usize;
    if got != size {
        return None;
    }
    // SAFETY: the `size` bytes GetRegionData wrote at the start of `buf`.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size) };
    region_rects(bytes)
}

/// WM_PAINT: the update region's rects (at most Damage::MAX, else the paint rect), each in bands.
pub(crate) fn on_paint(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (msg, wp, lp);
    let region = update_rects(hwnd);
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: WM_PAINT of our window, paired with EndPaint below.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let rect = RectP::from(ps.rcPaint);
    let rects: Vec<RectP> = match region {
        Some(v) => v.into_iter().map(|r| r.intersect(rect)).filter(|r| !r.is_empty()).collect(),
        None => vec![rect],
    };
    let done = window::with_host(hwnd, |h| h.paint(hdc, &rects));
    // SAFETY: the PAINTSTRUCT BeginPaint filled.
    let _ = unsafe { EndPaint(hwnd, &ps) };
    if done.is_none() && !rect.is_empty() {
        window::queue_repaint(hwnd, rect);
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_rects_cover_the_paint_rect() {
        for (r, bh) in [
            (RectP::wh(0, 0, 640, 560), 256),
            (RectP::wh(13, 7, 100, 1), 32),
            (RectP::wh(-5, 300, 40, 513), 68),
            (RectP::wh(0, 0, 3840, 2160), 68),
            (RectP::wh(10, 10, 50, 64), 32),
        ] {
            let b: Vec<RectP> = bands(r, bh).collect();
            assert!(b.iter().all(|x| x.l == r.l && x.r == r.r && x.h() > 0 && x.h() <= bh), "{r:?}");
            assert_eq!(b.first().map(|x| x.t), Some(r.t));
            assert_eq!(b.last().map(|x| x.b), Some(r.b));
            for w in b.windows(2) {
                assert_eq!(w[0].b, w[1].t, "disjoint and gapless");
            }
            assert_eq!(b.iter().map(RectP::area).sum::<i64>(), r.area());
        }
        assert_eq!(bands(RectP::EMPTY, 32).count(), 0);
        assert_eq!(bands(RectP::wh(0, 0, 10, 10), 0).count(), 10, "a zero band height still advances");
    }

    /// A GetRegionData buffer of `rects`: a 32-byte header, then the rects.
    fn region(rects: &[RectP]) -> Vec<u8> {
        let mut v = Vec::new();
        for w in [32i32, 1, rects.len() as i32, 16 * rects.len() as i32, 0, 0, 0, 0] {
            v.extend_from_slice(&w.to_le_bytes());
        }
        for r in rects {
            for w in [r.l, r.t, r.r, r.b] {
                v.extend_from_slice(&w.to_le_bytes());
            }
        }
        v
    }

    /// Fills and text across band boundaries.
    struct Stripes(crate::command::CommandTable);

    impl crate::screen::Screen for Stripes {
        fn layout(&mut self, cx: &mut crate::cx::LayoutCx, avail: SizeP) -> SizeP {
            let _ = cx;
            avail
        }

        fn paint(&self, cx: &mut crate::cx::PaintCx) {
            use crate::theme::{Ink, TypeRole};
            for i in 0..14 {
                let r = RectP::wh(cx.px(10 + i * 7), cx.px(10 + i * 48), cx.px(200), cx.px(40));
                if !cx.visible(r) {
                    continue;
                }
                cx.fill(r, cx.px(6), Ink::white(30 + (i as u32 % 4) * 40));
                let fg = cx.tokens().color.fg_primary;
                let at = r.inset(cx.px(8), 0);
                cx.text_in(
                    &format!("Band text {i}"),
                    TypeRole::Body,
                    at,
                    crate::model::Align::Start,
                    crate::text::Trim::End,
                    fg,
                );
            }
        }

        fn event(&mut self, cx: &mut crate::cx::EventCx, ev: &crate::event::Event) -> crate::event::Reply {
            let _ = (cx, ev);
            crate::event::Reply::None
        }

        fn commands(&self) -> &crate::command::CommandTable {
            &self.0
        }
    }

    /// The presenter's pixels of `rects` over a target prefilled with `sentinel` (a memory DC: no window).
    fn present(rects: &[RectP], size: SizeP, sentinel: u32) -> Vec<u32> {
        use crate::cx::{Env, PaintCx, Res};
        use crate::screen::Screen;
        let tokens = crate::theme::Tokens::oled();
        let env = Env {
            scale: crate::geom::Scale { dpi: 96 },
            tokens: &tokens,
            lang: crate::strings::UiLang::EN,
            now_ms: 0,
            caption: crate::host::Caption::System,
            caption_spike: false,
        };
        let mut screen = Stripes(crate::command::CommandTable::new());
        let mut disp = crate::dispatch::Dispatcher::new();
        let mut res = Res::new(Box::new(crate::text::GdiTextEngine::new().unwrap()));
        disp.layout(&mut screen, env, &mut res, size);
        let mut target = BandDib::new(size.w, size.h).unwrap();
        target.rows_mut(size.h).fill(sentinel);
        let bg = tokens.color.bg;
        let d = |c: u32| crate::w3::div255(c * bg.a);
        let bg_px = crate::w3::argb(bg.a, d(bg.rgb.0), d(bg.rgb.1), d(bg.rgb.2));
        let mut p = BandedPresenter::new();
        p.paint(target.dc, size, rects, bg_px, &mut |canvas, origin, clip| {
            let mut cx = PaintCx::new(env, canvas, origin, clip, &disp, &mut res, None);
            screen.paint(&mut cx);
            crate::host::hooks::after_paint_band(&mut cx);
        });
        // SAFETY: no precondition; the copies into the target are done before its pixels are read.
        let _ = unsafe { GdiFlush() };
        target.rows_mut(size.h).iter().map(|px| px & 0x00FF_FFFF).collect()
    }

    #[test]
    fn banded_presenter_paints_what_the_harness_paints() {
        let size = SizeP { w: 300, h: 700 };
        assert_eq!(band_height(size.w), 256, "three bands");
        let mut h = crate::harness::Harness::new(
            Stripes(crate::command::CommandTable::new()),
            crate::geom::SizeL { w: 300, h: 700 },
            96,
            crate::theme::Tokens::oled(),
        );
        let want: Vec<u32> = h.render().px.iter().map(|px| px & 0x00FF_FFFF).collect();
        assert!(want.iter().any(|px| *px != want[0]), "something is drawn");
        let full = present(&[RectP::wh(0, 0, size.w, size.h)], size, 0x00AB_CDEF);
        assert!(full == want, "a full paint equals the Harness's render");
        let rects = [RectP::wh(0, 250, 300, 20), RectP::wh(50, 600, 100, 50)];
        let part = present(&rects, size, 0x00AB_CDEF);
        for y in 0..size.h {
            for x in 0..size.w {
                let i = (y * size.w + x) as usize;
                let inside = rects.iter().any(|r| r.contains(crate::geom::PointP { x, y }));
                assert_eq!(part[i], if inside { want[i] } else { 0x00AB_CDEF }, "({x}, {y})");
            }
        }
    }

    #[test]
    fn region_data_gives_its_rects() {
        let two = [RectP::wh(0, 0, 10, 10), RectP::wh(-5, 20, 30, 4)];
        assert_eq!(region_rects(&region(&two)), Some(two.to_vec()));
        let nine: Vec<RectP> = (0..9).map(|i| RectP::wh(i * 20, 0, 10, 10)).collect();
        assert_eq!(region_rects(&region(&nine)), None, "more than 8: the paint rect instead");
        assert_eq!(region_rects(&region(&[])), None);
        let cut = region(&two);
        assert_eq!(region_rects(&cut[..cut.len() - 4]), None, "a truncated buffer");
        assert_eq!(region_rects(&[0; 7]), None);
    }
}
