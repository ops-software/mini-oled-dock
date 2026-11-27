//! GdiText: the white-on-black coverage text path (moved from surface.rs by W3).

#[cfg(any(test, feature = "test-counters"))]
use std::cell::Cell;
use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, GetDC,
    GetTextExtentExPointW, GetTextExtentPoint32W, GetTextFaceW, GetTextMetricsW, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_LEFT, DT_NOCLIP, DT_NOPREFIX, DT_SINGLELINE, DT_TOP, HBITMAP, HDC, HFONT,
    HGDIOBJ, LOGFONTW, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use crate::gfx::canvas::Canvas;
use crate::gfx::pixel::{argb, div255};

/// PREVIEW-HOOK (golden tests, set by preview.rs): when set, `Gdi::text` and `Gdi::text_mask` draw nothing.
/// Widths are still measured, so every layout is identical; the difference between a normal render and a
/// no-text render is exactly the set of pixels that text touches.
pub static NO_TEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 32-bit top-down DIB selected into a DC.
struct Dib {
    bmp: HBITMAP,
    /// Bitmap that was selected in the DC before ours (to restore before deleting the DIB).
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

/// Pixels of a DIB as a slice.
/// Invariant: the bitmap (w*h*4 bytes) is alive as long as the `Dib` exists; the slice must be used in short
/// scopes and must not coexist with GDI calls that write into the same DIB (GDI only writes into the
/// scratch DIB for text, and never while a slice of the latter is being read).
fn dib_pixels<'a>(d: &Dib) -> &'a mut [u32] {
    // SAFETY: see the invariant above; `bits` points to w*h u32 allocated by CreateDIBSection.
    unsafe { std::slice::from_raw_parts_mut(d.bits, (d.w as usize) * (d.h as usize)) }
}

fn make_dib(dc: HDC, w: i32, h: i32) -> Result<Dib, String> {
    if w <= 0 || h <= 0 {
        return Err(format!("invalid DIB dimensions {w}x{h}"));
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
    // SAFETY: `bmi` is valid; `bits` receives the pointer to the buffer owned by the bitmap.
    let bmp = unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }
        .map_err(|e| format!("CreateDIBSection {w}x{h}: {e}"))?;
    if bits.is_null() {
        // SAFETY: `bmp` was just created and is not selected anywhere.
        let _ = unsafe { DeleteObject(HGDIOBJ(bmp.0)) };
        return Err("CreateDIBSection: null bits".into());
    }
    // SAFETY: `dc` and `bmp` are valid; the previous bitmap is saved for restoring.
    let old = unsafe { SelectObject(dc, HGDIOBJ(bmp.0)) };
    Ok(Dib { bmp, old, bits: bits.cast::<u32>(), w, h })
}

fn free_dib(dc: HDC, d: Dib) {
    // SAFETY: `d` was created by `make_dib` with this same `dc`; the original bitmap is restored
    // before deleting ours (a selected bitmap cannot be deleted).
    unsafe {
        SelectObject(dc, d.old);
        let _ = DeleteObject(HGDIOBJ(d.bmp.0));
    }
}

fn make_font(face: &str, height_px: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -height_px,
        lfWeight: weight,
        lfCharSet: DEFAULT_CHARSET,
        lfQuality: ANTIALIASED_QUALITY, // grayscale, no ClearType
        ..Default::default()
    };
    for (d, s) in lf.lfFaceName.iter_mut().zip(face.encode_utf16().take(31)) {
        *d = s;
    }
    // SAFETY: `lf` is fully initialized.
    unsafe { CreateFontIndirectW(&lf) }
}

/// Creates the font trying the `faces` in order. GDI silently substitutes a missing font, so
/// the actual name is verified with `GetTextFaceW`; "Segoe UI" goes last without verification.
pub fn create_font(dc: HDC, faces: &[&str], height_px: i32, weight: i32) -> Result<HFONT, String> {
    for face in faces {
        let f = make_font(face, height_px, weight);
        if f.is_invalid() {
            continue;
        }
        let mut buf = [0u16; 64];
        // SAFETY: `dc` is valid; the font is selected only for the check and then restored.
        let got = unsafe {
            let old = SelectObject(dc, HGDIOBJ(f.0));
            let n = GetTextFaceW(dc, Some(&mut buf));
            SelectObject(dc, old);
            n
        };
        let name = String::from_utf16_lossy(&buf[..(got.max(0) as usize).min(buf.len())]);
        if name.trim_end_matches('\0').eq_ignore_ascii_case(face) {
            return Ok(f);
        }
        // SAFETY: `f` was created above and is not selected.
        let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
    }
    let f = make_font("Segoe UI", height_px, weight);
    if f.is_invalid() {
        Err("CreateFontIndirectW failed".into())
    } else {
        Ok(f)
    }
}

/// Line height (physical px) of `font`: `tmHeight`, 17 if unavailable.
pub fn line_height(dc: HDC, font: HFONT) -> i32 {
    let mut tm = TEXTMETRICW::default();
    // SAFETY: `dc` is valid, `tm` is writable; the font is restored right away.
    let ok = unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let ok = GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        ok
    };
    if ok.as_bool() && tm.tmHeight > 0 {
        tm.tmHeight
    } else {
        17
    }
}

/// Deletes a font made by `create_font` (no-op if invalid).
pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        // SAFETY: font created by `create_font`, never left selected in a DC.
        let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
    }
}

/// GDI backing of a layered drawing: memory DC with the main DIB (source of
/// `UpdateLayeredWindow`) and memory DC with the scratch DIB for text (white text, transparent
/// background). The fonts belong to the owner (bar or popup). Dropping it frees DIBs and DCs.
pub struct Gdi {
    /// DC holding the main DIB.
    dc_main: HDC,
    /// DC holding the scratch DIB for text.
    dc_txt: HDC,
    main: Option<Dib>,
    txt: Option<Dib>,
    /// The UTF-16 of the text being measured or drawn: one buffer, its capacity kept, freed with the Gdi.
    wide: RefCell<Vec<u16>>,
}

impl Gdi {
    /// Creates the two memory DCs (no DIB yet: see `resize`).
    pub fn new() -> Result<Gdi, String> {
        // SAFETY: the screen DC is taken and released here; the memory DCs are ours.
        let (dc_main, dc_txt) = unsafe {
            let screen = GetDC(None);
            let a = CreateCompatibleDC(Some(screen));
            let b = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            (a, b)
        };
        if dc_main.is_invalid() || dc_txt.is_invalid() {
            // SAFETY: any valid DCs created above, not yet used.
            unsafe {
                if !dc_main.is_invalid() {
                    let _ = DeleteDC(dc_main);
                }
                if !dc_txt.is_invalid() {
                    let _ = DeleteDC(dc_txt);
                }
            }
            return Err("CreateCompatibleDC failed".into());
        }
        // SAFETY: `dc_txt` is valid. White text, transparent background: the coverage ends up in the G channel.
        unsafe {
            SetBkMode(dc_txt, TRANSPARENT);
            SetTextColor(dc_txt, COLORREF(0x00FF_FFFF));
        }
        Ok(Gdi { dc_main, dc_txt, main: None, txt: None, wide: RefCell::new(Vec::new()) })
    }

    /// DC holding the main DIB (source for `present_layered` / `BitBlt`).
    pub fn dc_main(&self) -> HDC {
        self.dc_main
    }

    /// DC used to create and measure fonts (the text one).
    pub fn dc_txt(&self) -> HDC {
        self.dc_txt
    }

    /// Size (physical px) of the main DIB; `None` before `resize`.
    pub fn size(&self) -> Option<(i32, i32)> {
        self.main.as_ref().map(|d| (d.w, d.h))
    }

    /// Frees both DIBs (the next `resize` recreates them).
    pub fn free_dibs(&mut self) {
        if let Some(d) = self.main.take() {
            free_dib(self.dc_main, d);
        }
        if let Some(d) = self.txt.take() {
            free_dib(self.dc_txt, d);
        }
    }

    /// (Re)creates the main `w` x `h` physical-px DIB and the `w` x `txt_h` scratch one for text.
    /// No-op if the main DIB already has these dimensions.
    pub fn resize(&mut self, w: i32, h: i32, txt_h: i32) -> Result<(), String> {
        if self.size() == Some((w, h)) {
            return Ok(());
        }
        self.free_dibs();
        let main = make_dib(self.dc_main, w, h)?;
        let txt = match make_dib(self.dc_txt, w, txt_h) {
            Ok(t) => t,
            Err(e) => {
                free_dib(self.dc_main, main);
                return Err(e);
            }
        };
        self.main = Some(main);
        self.txt = Some(txt);
        Ok(())
    }

    /// Canvas over the main DIB, writes limited to `clip` (`None` = the whole DIB). Short-lived:
    /// see the invariant of `dib_pixels`.
    pub fn canvas(&self, clip: Option<RECT>) -> Option<Canvas<'_>> {
        let main = self.main.as_ref()?;
        Some(Canvas { buf: dib_pixels(main), w: main.w, h: main.h, clip })
    }

    /// Width in physical px of `s` with `font`; if GDI fails, `per_char` px per UTF-16 unit.
    pub fn text_width(&self, s: &str, font: HFONT, per_char: i32) -> i32 {
        if s.is_empty() {
            return 0;
        }
        #[cfg(any(test, feature = "test-counters"))]
        EXTENT_CALLS.with(|c| c.set(c.get() + 1));
        let mut wide = self.wide.borrow_mut();
        wide.clear();
        wide.extend(s.encode_utf16());
        let mut sz = SIZE::default();
        // SAFETY: `dc_txt` is valid; `wide` and `sz` outlive the call; the font is restored.
        let ok = unsafe {
            let old = SelectObject(self.dc_txt, HGDIOBJ(font.0));
            let ok = GetTextExtentPoint32W(self.dc_txt, &wide, &mut sz);
            SelectObject(self.dc_txt, old);
            ok
        };
        if ok.as_bool() {
            sz.cx
        } else {
            wide.len() as i32 * per_char
        }
    }

    /// The width of the text up to and including each character of `s` (`out[i]` for the character `i`, so the last
    /// is the whole width), from one `GetTextExtentExPointW` call. If GDI fails, `per_char` px per UTF-16 unit.
    pub fn char_extents(&self, s: &str, font: HFONT, per_char: i32, out: &mut Vec<i32>) {
        out.clear();
        if s.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "test-counters"))]
        EXTENT_CALLS.with(|c| c.set(c.get() + 1));
        let mut wide = self.wide.borrow_mut();
        wide.clear();
        wide.extend(s.encode_utf16());
        let units = wide.len();
        out.resize(units, 0);
        let mut sz = SIZE::default();
        // SAFETY: `dc_txt` is valid; `wide` holds `units` units and `out` has `units` writable entries, both alive for
        // the call; `lpnFit` is null, so `nMaxExtent` is ignored and every partial extent is written; the font is restored.
        let ok = unsafe {
            let old = SelectObject(self.dc_txt, HGDIOBJ(font.0));
            let ok = GetTextExtentExPointW(
                self.dc_txt,
                PCWSTR(wide.as_ptr()),
                units as i32,
                0,
                None,
                Some(out.as_mut_ptr()),
                &mut sz,
            );
            SelectObject(self.dc_txt, old);
            ok
        };
        if !ok.as_bool() {
            for (i, e) in out.iter_mut().enumerate() {
                *e = (i as i32 + 1) * per_char;
            }
        }
        // One entry per UTF-16 unit became one per character: the extent after its last unit (forward, in place).
        let mut end = 0usize;
        for (j, ch) in s.chars().enumerate() {
            end += ch.len_utf16();
            let v = out.get(end.saturating_sub(1)).copied().unwrap_or(0);
            if let Some(slot) = out.get_mut(j) {
                *slot = v;
            }
        }
        out.truncate(s.chars().count());
    }

    /// Single-line text `s` with the top-left corner at `(x, y)` onto `cv` (a canvas of this
    /// `Gdi`): white grayscale GDI onto the black scratch DIB, then manual compositing with colour
    /// `rgb` (non-premultiplied) and alpha `alpha`. `font` = (font, line height); `w` = width of
    /// `s` as measured by `text_width`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &self,
        cv: &mut Canvas<'_>,
        s: &str,
        x: i32,
        y: i32,
        font: (HFONT, i32),
        w: i32,
        rgb: (u32, u32, u32),
        alpha: u32,
    ) {
        let Some(txt) = self.txt.as_ref() else { return };
        if NO_TEXT.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if s.is_empty() || alpha == 0 {
            return;
        }
        let (fnt, lh) = font;
        let w = w.min(txt.w);
        let h = lh.min(txt.h);
        if w <= 0 || h <= 0 {
            return;
        }
        let tbuf = dib_pixels(txt);
        for row in 0..h as usize {
            let start = row * txt.w as usize;
            tbuf[start..start + w as usize].fill(0);
        }
        let mut wide = self.wide.borrow_mut();
        wide.clear();
        wide.extend(s.encode_utf16());
        let mut rc = RECT { left: 0, top: 0, right: w, bottom: h };
        // SAFETY: `dc_txt` has the scratch DIB selected; `wide` and `rc` outlive the call;
        // the font is restored right away. GdiFlush guarantees GDI has written before the read.
        unsafe {
            let old = SelectObject(self.dc_txt, HGDIOBJ(fnt.0));
            DrawTextW(self.dc_txt, &mut wide, &mut rc, DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
            SelectObject(self.dc_txt, old);
            let _ = GdiFlush();
        }
        drop(wide);
        for row in 0..h {
            for col in 0..w {
                let cov = (tbuf[(row * txt.w + col) as usize] >> 8) & 0xFF; // coverage = G channel
                if cov == 0 {
                    continue;
                }
                cv.blend(x + col, y + row, argb(div255(cov * alpha), rgb.0, rgb.1, rgb.2));
            }
        }
    }

    /// Coverage mask (0..255, row-major, `w` x line height) of `s` drawn with `font` = (font, line
    /// height), top-left at (0, 0); `w` = width of `s` as measured by `text_width`. Lets the caller
    /// composite the text per pixel (e.g. knocked out of a fill). `None` without the scratch DIB.
    pub fn text_mask(&self, s: &str, font: (HFONT, i32), w: i32) -> Option<(i32, i32, Vec<u8>)> {
        let txt = self.txt.as_ref()?;
        if NO_TEXT.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let (fnt, lh) = font;
        let (w, h) = (w.min(txt.w), lh.min(txt.h));
        if s.is_empty() || w <= 0 || h <= 0 {
            return None;
        }
        let tbuf = dib_pixels(txt);
        for row in 0..h as usize {
            let start = row * txt.w as usize;
            tbuf[start..start + w as usize].fill(0);
        }
        let mut wide = self.wide.borrow_mut();
        wide.clear();
        wide.extend(s.encode_utf16());
        let mut rc = RECT { left: 0, top: 0, right: w, bottom: h };
        // SAFETY: as in `text`: `dc_txt` has the scratch DIB selected; `wide` and `rc` outlive the
        // call; the font is restored right away; GdiFlush makes GDI write before the read.
        unsafe {
            let old = SelectObject(self.dc_txt, HGDIOBJ(fnt.0));
            DrawTextW(self.dc_txt, &mut wide, &mut rc, DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP);
            SelectObject(self.dc_txt, old);
            let _ = GdiFlush();
        }
        let mut mask = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                mask.push(((tbuf[(row * txt.w + col) as usize] >> 8) & 0xFF) as u8); // coverage = G channel
            }
        }
        Some((w, h, mask))
    }

    /// Copy of the main DIB pixels (premultiplied 0xAARRGGBB), for the preview.
    pub fn snapshot_pixels(&self) -> Option<(i32, i32, Vec<u32>)> {
        let d = self.main.as_ref()?;
        Some((d.w, d.h, dib_pixels(d).to_vec()))
    }
}

impl Drop for Gdi {
    /// Releases the DIBs and the DCs (first restoring the selected bitmaps).
    fn drop(&mut self) {
        self.free_dibs();
        // SAFETY: the DCs are ours and no longer have any of our objects selected.
        unsafe {
            let _ = DeleteDC(self.dc_main);
            let _ = DeleteDC(self.dc_txt);
        }
    }
}

/// `UpdateLayeredWindow(ULW_ALPHA)` of `hwnd` from the `w` x `h` DIB selected in `dc` (a
/// `Gdi::dc_main`), corner at screen `(x, y)`, constant alpha `alpha` on top of the per-pixel
/// alpha. Takes the raw DC so that the caller can release any borrow of its own state first
/// (the call may re-enter with messages to the same window).
pub fn present_layered(hwnd: HWND, dc: HDC, w: i32, h: i32, x: i32, y: i32, alpha: u8) -> windows::core::Result<()> {
    let pos = POINT { x, y };
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: all pointers point to local variables alive for the call; `dc` has the DIB
    // selected (a re-entry that recreates it still leaves a valid DC, which lives until its `Gdi`
    // is dropped); the screen DC is taken and released here.
    unsafe {
        let screen = GetDC(None);
        let r = UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&pos),
            Some(&size),
            Some(dc),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen);
        r
    }
}

// Number of GDI text-extent calls on this thread, `GetTextExtentExPointW` (`char_extents`) and `GetTextExtentPoint32W`
// (`text_width`, which `text` calls): the measurement gate of the launcher's field.
#[cfg(any(test, feature = "test-counters"))]
thread_local! {
    pub static EXTENT_CALLS: Cell<u64> = const { Cell::new(0) };
}
