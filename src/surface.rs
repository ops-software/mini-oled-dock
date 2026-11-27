//! Drawing layer shared by the bar (`render.rs`) and the POPUPs, plus the popups' own `Surface`.
//!
//! Shared with `render.rs` (`pub(crate)`): the GDI backing `Gdi` (two memory DCs, the premultiplied
//! 32-bit main DIB and the scratch DIB for text), font creation with face fallback, hand-composited
//! grayscale GDI text (WHITE `DrawTextW` onto the black scratch DIB, coverage read from the G
//! channel), `UpdateLayeredWindow` presentation, and the pixel primitives of `Canvas` (AA rounded
//! rect, `IconBitmap` blit, 16x16 glyphs with supersampling; `div255`, `argb`, `over`, `rr_cov`).
//! There is ONE implementation of each: the bar and the popups draw identical pixels.
//!
//! The popups use them WITHOUT global state: each open popup has its own `Surface` (only one at a
//! time), presented with `UpdateLayeredWindow` (or `BitBlt` for the non-layered window picker).
//!
//! THREADING: UI thread only (GDI objects of the thread).

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{BitBlt, HDC, HFONT, SRCCOPY};

use crate::icons::IconBitmap;
use crate::log;
use crate::render::Metrics;
pub(crate) use oled_ui::gfx::canvas::{glyph_hit_counts, Canvas};
pub(crate) use oled_ui::gfx::pixel::{argb, div255, over, rr_cov, white};
pub use oled_ui::gfx::pixel::{in_rect, rect_wh};
#[cfg(test)]
pub(crate) use oled_ui::text::gdi::EXTENT_CALLS;
pub(crate) use oled_ui::text::gdi::{create_font, delete_font, line_height, present_layered, Gdi, NO_TEXT};

// --------------------------------------------------------------- style

/// Corner radius of the panel (logical px).
pub const PANEL_RADIUS: i32 = oled_ui::tokens::R_L;
/// Alpha of the 1 px white border.
pub const PANEL_BORDER_ALPHA: u32 = oled_ui::tokens::RIM_SURFACE;
/// Inner padding of the panel (logical px).
pub const PANEL_PAD: i32 = oled_ui::tokens::S_4;
/// Radius of the hover/press plates inside the popups (logical px).
pub const ROW_RADIUS: i32 = oled_ui::tokens::R_S;
/// Alpha of the hover / press plate (same as the bar).
pub const HOVER_ALPHA: u32 = oled_ui::tokens::FILL_HOVER;
pub const PRESS_ALPHA: u32 = oled_ui::tokens::FILL_PRESS;
/// Inner 1 px border of the hover plates (same as the bar).
pub const HOVER_RIM_ALPHA: u32 = oled_ui::tokens::RIM_HOVER;
/// Text column of all popups (logical px from the panel edge): row inset 8 + 12.
pub const TEXT_COL: i32 = oled_ui::tokens::TEXT_COL;
/// Alpha of primary / secondary / tertiary text (white).
pub const TEXT_ALPHA: u32 = oled_ui::tokens::FG_PRIMARY;
pub const TEXT2_ALPHA: u32 = oled_ui::tokens::FG_SECONDARY;
/// 128: 5.3:1 on black (4.5:1 floor for text that carries information).
pub const TEXT3_ALPHA: u32 = oled_ui::tokens::FG_TERTIARY;
/// Disabled labels and controls only, never information (3.0:1 on black).
pub const DISABLED_ALPHA: u32 = oled_ui::tokens::FG_DISABLED;
/// Alpha of the 1 px horizontal separators.
pub const SEP_ALPHA: u32 = oled_ui::tokens::SEPARATOR;

/// White color, for the callers' convenience.
pub const WHITE: (u32, u32, u32) = (255, 255, 255);

/// Faces of the text fonts, in order of preference (bar and popups).
pub(crate) const TEXT_FACES: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];

/// Fonts available on the surface ("Segoe UI Variable Text", fallback "Segoe UI";
/// "Segoe UI Variable Display" for `Big`, fallback "Segoe UI Light").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    /// 11 px, weight 400: times, subtitles, day headers.
    Small,
    /// 13 px, weight 400: normal text of the rows.
    Normal,
    /// 13 px, weight 600: titles (app name of a notification, month title).
    Bold,
    /// 32 px, weight 300: big time of the calendar popup.
    Big,
    /// 20 px, weight 600: page title of the Settings window.
    Title,
    /// 12 px, weight 400: descriptions under the options of the Settings window.
    Caption,
    /// 18 px, weight 400: the launcher's query (Q7). Made on first use, so surfaces that never draw it do not pay for it.
    Query,
}

impl Font {
    fn idx(self) -> usize {
        match self {
            Font::Small => 0,
            Font::Normal => 1,
            Font::Bold => 2,
            Font::Big => 3,
            Font::Title => 4,
            Font::Caption => 5,
            // Not in the table: `Surface::face` makes it.
            Font::Query => 1,
        }
    }
}

// ------------------------------------------------------------- popup surface

/// DIB + DC + fonts of a popup.
pub struct Surface {
    metrics: Metrics,
    gdi: Gdi,
    fonts: [HFONT; 6],
    lhs: [i32; 6],
    /// The `Font::Query` font and its line height, made by the first `face` call that needs it.
    query: Cell<Option<(HFONT, i32)>>,
    /// Clipping rectangle (physical px) applied to every write; `None` = the whole DIB.
    clip: Option<RECT>,
    /// 4 x 4 hit counts of the glyphs painted with `glyph_cached`: (key, size, counts); freed with the surface.
    glyph_hits: Vec<(u32, i32, Box<[u8]>)>,
}

/// Largest glyph (side in physical px) and number of entries `glyph_cached` keeps.
const GLYPH_CACHE_MAX_SIZE: i32 = 64;
const GLYPH_CACHE_MAX_ENTRIES: usize = 32;

fn delete_fonts(fonts: &[HFONT; 6]) {
    for f in fonts {
        delete_font(*f);
    }
}

fn build_fonts(dc: HDC, m: Metrics) -> Result<([HFONT; 6], [i32; 6]), String> {
    let display = ["Segoe UI Variable Display", "Segoe UI Light"];
    let title = ["Segoe UI Variable Display", "Segoe UI"];
    let mut fonts = [HFONT::default(); 6];
    let specs: [(&[&str], i32, i32); 6] = [
        (&TEXT_FACES, m.px(11), 400),
        (&TEXT_FACES, m.px(13), 400),
        (&TEXT_FACES, m.px(13), 600),
        (&display, m.px(32), 300),
        (&title, m.px(20), 600),
        (&TEXT_FACES, m.px(12), 400),
    ];
    for (i, (faces, px, weight)) in specs.iter().enumerate() {
        match create_font(dc, faces, *px, *weight) {
            Ok(f) => fonts[i] = f,
            Err(e) => {
                delete_fonts(&fonts);
                return Err(e);
            }
        }
    }
    let lhs = fonts.map(|f| line_height(dc, f));
    Ok((fonts, lhs))
}

impl Surface {
    /// Creates the memory DC and the six fonts for `m`. The DIB is created by `resize`.
    pub fn new(m: Metrics) -> Result<Surface, String> {
        let gdi = Gdi::new()?;
        // On error `gdi` is dropped, which deletes the DCs.
        let (fonts, lhs) = build_fonts(gdi.dc_txt(), m)?;
        Ok(Surface { metrics: m, gdi, fonts, lhs, query: Cell::new(None), clip: None, glyph_hits: Vec::new() })
    }

    /// Current metrics.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// (Re)creates the main `w` x `h` physical-px DIB and the scratch one for text. No-op if
    /// the dimensions do not change.
    pub fn resize(&mut self, w: i32, h: i32) -> Result<(), String> {
        let txt_h = self.lhs.iter().copied().max().unwrap_or(17) + 4;
        self.gdi.resize(w, h, txt_h)
    }

    /// Sets (or clears) the clipping rectangle of the drawing primitives.
    pub fn set_clip(&mut self, rc: Option<RECT>) {
        self.clip = rc;
    }

    /// Current DIB dimensions (physical px).
    pub fn size(&self) -> (i32, i32) {
        self.gdi.size().unwrap_or((0, 0))
    }

    /// Background panel over the whole DIB: config `background` tone (black by default) at the bar's opacity
    /// (`Config::bar_alpha`, at least 1 so the panel stays clickable at 0 %), 1 px border `PANEL_BORDER_ALPHA`,
    /// antialiased `PANEL_RADIUS` corners, transparent outside the corners. Replaces all pixels.
    pub fn panel(&mut self) {
        let Some(cv) = self.gdi.canvas(None) else { return };
        let (w, h) = (cv.w, cv.h);
        let buf = cv.buf;
        let radius = self.metrics.px(PANEL_RADIUS);
        let bw = 1.max(self.metrics.px(1));
        let cfg = crate::config::get();
        let bg_alpha = cfg.bar_alpha().max(1);
        oled_ui::gfx::panel::panel_pixels(buf, w, h, radius, bw, cfg.background.rgb(), bg_alpha);
    }

    /// Filled AA rounded rectangle (client coordinates, physical px; `radius` in physical px).
    pub fn fill_rr(&mut self, rc: RECT, radius: i32, rgb: (u32, u32, u32), alpha: u32) {
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.fill_rr(rc, radius, rgb, alpha);
        }
    }

    /// Inner ring of `bw` physical px of a rounded rectangle, white with alpha `alpha`.
    pub fn stroke_rr(&mut self, rc: RECT, radius: i32, bw: i32, alpha: u32) {
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.stroke_rr(rc, radius, bw, alpha);
        }
    }

    /// Hover/press plate like the bar one: white fill `alpha` + inner border
    /// 1 px `HOVER_RIM_ALPHA` (scaled if `alpha` is below the hover value). `radius` in physical px.
    pub fn plate(&mut self, rc: RECT, radius: i32, alpha: u32) {
        self.fill_rr(rc, radius, WHITE, alpha);
        let rim = HOVER_RIM_ALPHA * alpha.min(HOVER_ALPHA) / HOVER_ALPHA;
        let bw = self.metrics.px(1).max(1);
        self.stroke_rr(rc, radius, bw, rim);
    }

    /// Opaque fill of the rounded rectangle `rc` (AA corners, `radius` physical px) with a colour
    /// computed per pixel by `color(x, y)` (client coordinates): colour picker surfaces and swatches.
    pub fn fill_fn(&mut self, rc: RECT, radius: i32, color: &dyn Fn(i32, i32) -> (u32, u32, u32)) {
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.fill_fn(rc, radius, color);
        }
    }

    /// Blend of a premultiplied bitmap into the `size` square (see `Canvas::blit`).
    pub fn blit(&mut self, bmp: &IconBitmap, x: i32, y: i32, size: i32, opacity: u32) {
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.blit(bmp, x, y, size, opacity);
        }
    }

    /// Glyph from `crate::glyphs` (16x16 space) in white, in a `size` px square with corner
    /// `(x, y)`, 4x4 supersampling.
    pub fn glyph(&mut self, x: i32, y: i32, size: i32, alpha: u32, shape: &dyn Fn(f32, f32) -> bool) {
        self.glyph_rgb(x, y, size, WHITE, alpha, shape);
    }

    /// Like `glyph`, but the 4 x 4 hit counts of the shape are kept per (`key`, `size`) in the surface, so a frame
    /// after the first only blends. `key` names the shape (never a pointer: capture-less closures can share an
    /// address). A size over 64 px, or a full cache, paints like `glyph`.
    pub fn glyph_cached(&mut self, key: u32, x: i32, y: i32, size: i32, alpha: u32, shape: &dyn Fn(f32, f32) -> bool) {
        self.glyph_cached_rgb(key, x, y, size, WHITE, alpha, shape);
    }

    #[cfg(test)]
    pub(crate) fn glyph_hits_clear(&mut self) {
        self.glyph_hits.clear();
    }

    /// Like `glyph_cached` with color `rgb`.
    #[allow(clippy::too_many_arguments)]
    pub fn glyph_cached_rgb(
        &mut self,
        key: u32,
        x: i32,
        y: i32,
        size: i32,
        rgb: (u32, u32, u32),
        alpha: u32,
        shape: &dyn Fn(f32, f32) -> bool,
    ) {
        if size <= 0 || size > GLYPH_CACHE_MAX_SIZE {
            return self.glyph_rgb(x, y, size, rgb, alpha, shape);
        }
        let at = match self.glyph_hits.iter().position(|(k, sz, _)| *k == key && *sz == size) {
            Some(i) => i,
            None if self.glyph_hits.len() >= GLYPH_CACHE_MAX_ENTRIES => {
                return self.glyph_rgb(x, y, size, rgb, alpha, shape);
            }
            None => {
                self.glyph_hits.push((key, size, glyph_hit_counts(size, shape)));
                self.glyph_hits.len() - 1
            }
        };
        let Some((_, _, hits)) = self.glyph_hits.get(at) else { return };
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.glyph_from_hits(x, y, size, rgb, alpha, hits);
        }
    }

    /// Like `glyph` with color `rgb` (e.g. black on the white plate).
    pub fn glyph_rgb(
        &mut self,
        x: i32,
        y: i32,
        size: i32,
        rgb: (u32, u32, u32),
        alpha: u32,
        shape: &dyn Fn(f32, f32) -> bool,
    ) {
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            cv.glyph(x, y, size, rgb, alpha, shape);
        }
    }

    /// Single-line text with the top-left corner at `(x, y)`; `rgb` non-premultiplied
    /// (white normally, black for today's day on the white plate).
    pub fn text(&mut self, s: &str, x: i32, y: i32, font: Font, rgb: (u32, u32, u32), alpha: u32) {
        if self.gdi.size().is_none() || s.is_empty() || alpha == 0 {
            return;
        }
        let w = self.text_width(s, font);
        self.text_w(s, w, x, y, font, rgb, alpha);
    }

    /// `text` with the width of `s` in `font` already measured by `text_width`: drawn without measuring it again.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_w(&mut self, s: &str, w: i32, x: i32, y: i32, font: Font, rgb: (u32, u32, u32), alpha: u32) {
        let Some(mut cv) = self.gdi.canvas(self.clip) else { return };
        if s.is_empty() || alpha == 0 {
            return;
        }
        let f = self.face(font);
        self.gdi.text(&mut cv, s, x, y, f, w, rgb, alpha);
    }

    /// Like `text` with a font made for this one call, `px` physical px tall and of `weight`, in the dock's text face. Returns
    /// the width. Only the Q7 field-size candidate of the preview needs a size the six fonts do not have.
    pub(crate) fn text_sized(
        &mut self,
        s: &str,
        x: i32,
        y: i32,
        size: (i32, i32),
        rgb: (u32, u32, u32),
        alpha: u32,
    ) -> i32 {
        let (px, weight) = size;
        let Ok(font) = create_font(self.gdi.dc_txt(), &TEXT_FACES, px, weight) else { return 0 };
        let w = self.gdi.text_width(s, font, self.metrics.px(7));
        let lh = line_height(self.gdi.dc_txt(), font);
        if let Some(mut cv) = self.gdi.canvas(self.clip) {
            self.gdi.text(&mut cv, s, x, y, (font, lh), w, rgb, alpha);
        }
        delete_font(font);
        w
    }

    /// The width of each prefix of `s` in `font`: `out[i]` is the width up to and including the character `i`, from one
    /// GDI call (a character's own width is the difference of two neighbours). `out` keeps its capacity.
    pub(crate) fn char_extents(&self, s: &str, font: Font, out: &mut Vec<i32>) {
        self.gdi.char_extents(s, self.face(font).0, self.metrics.px(7), out);
    }

    /// Width in physical px of `s` with `font`.
    pub fn text_width(&self, s: &str, font: Font) -> i32 {
        self.gdi.text_width(s, self.face(font).0, self.metrics.px(7))
    }

    /// Line height (physical px) of `font`.
    pub fn line_height(&self, font: Font) -> i32 {
        self.face(font).1
    }

    /// The GDI font and line height of `font`. `Font::Query` is made on the first call (a failure falls back to
    /// `Normal`, and is tried again next time).
    fn face(&self, font: Font) -> (HFONT, i32) {
        if font == Font::Query {
            if let Some(q) = self.query.get() {
                return q;
            }
            let dc = self.gdi.dc_txt();
            if let Ok(f) = create_font(dc, &TEXT_FACES, self.metrics.px(18), 400) {
                let q = (f, line_height(dc, f));
                self.query.set(Some(q));
                return q;
            }
        }
        let i = font.idx();
        (self.fonts[i], self.lhs[i])
    }

    /// `s` truncated with "…" so that `text_width <= max_w`.
    pub fn ellipsize(&self, s: &str, font: Font, max_w: i32) -> String {
        if self.text_width(s, font) <= max_w {
            return s.to_string();
        }
        let chars: Vec<char> = s.chars().collect();
        // Binary search for the longest prefix that, with "…", fits in `max_w`.
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let mut cand: String = chars[..mid].iter().collect();
            cand = cand.trim_end().to_string();
            cand.push('\u{2026}');
            if self.text_width(&cand, font) <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let mut out: String = chars[..lo].iter().collect();
        out = out.trim_end().to_string();
        out.push('\u{2026}');
        out
    }

    /// Copies the DIB as is (no alpha) onto `hdc` at `(x, y)`: for a NON-layered window drawn in
    /// `WM_PAINT` (the window picker), where every pixel has been painted opaque.
    pub fn copy_to(&self, hdc: HDC, x: i32, y: i32) {
        let Some((w, h)) = self.gdi.size() else { return };
        // SAFETY: `hdc` comes from BeginPaint of the caller; `dc_main` holds our DIB of `w` x `h`.
        if let Err(e) = unsafe { BitBlt(hdc, x, y, w, h, Some(self.gdi.dc_main()), 0, 0, SRCCOPY) } {
            log!("surface: BitBlt failed: {e}");
        }
    }

    /// Composites the premultiplied 0xAARRGGBB pixels `px` (`w` x `h`, row-major) source-over at
    /// `(x, y)`; honours the clip and the DIB bounds. Does nothing if `px` is shorter than `w * h`.
    pub fn blend_pixels(&mut self, x: i32, y: i32, w: i32, h: i32, px: &[u32]) {
        if w <= 0 || h <= 0 || px.len() < (w as usize) * (h as usize) {
            return;
        }
        let Some(mut cv) = self.gdi.canvas(self.clip) else { return };
        for row in 0..h {
            for col in 0..w {
                cv.blend(x + col, y + row, px[(row * w + col) as usize]);
            }
        }
    }

    /// `UpdateLayeredWindow(ULW_ALPHA)` with corner at screen `(x, y)` and constant alpha `alpha`
    /// (opening fade). false on failure (logged). The caller must not hold a borrow
    /// of its own state during the call (it may re-enter with messages to the same window).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32, alpha: u8) -> bool {
        let Some((w, h)) = self.gdi.size() else { return false };
        match present_layered(hwnd, self.gdi.dc_main(), w, h, x, y, alpha) {
            Ok(()) => true,
            Err(e) => {
                log!("popup: UpdateLayeredWindow failed: {e}");
                false
            }
        }
    }
}

impl Drop for Surface {
    /// Releases the fonts; the DIBs and the DCs are released by the `Gdi` field.
    fn drop(&mut self) {
        delete_fonts(&self.fonts);
        if let Some((f, _)) = self.query.take() {
            delete_font(f);
        }
    }
}

// PREVIEW-HOOK (used by `preview.rs`): copy of the main DIB pixels (premultiplied 0xAARRGGBB).
impl Surface {
    pub(crate) fn snapshot_pixels(&self) -> Option<(i32, i32, Vec<u32>)> {
        self.gdi.snapshot_pixels()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const M: Metrics = Metrics { dpi: 96 };

    /// WCAG contrast of white at alpha `a` over black.
    fn contrast_on_black(a: u32) -> f64 {
        let c = a as f64 / 255.0;
        let lin = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        (lin + 0.05) / 0.05
    }

    #[test]
    fn surface_aliases_equal_their_old_literals() {
        assert_eq!(PANEL_RADIUS, 14);
        assert_eq!(PANEL_BORDER_ALPHA, 22);
        assert_eq!(PANEL_PAD, 12);
        assert_eq!(ROW_RADIUS, 7);
        assert_eq!(HOVER_ALPHA, 20);
        assert_eq!(PRESS_ALPHA, 36);
        assert_eq!(HOVER_RIM_ALPHA, 18);
        assert_eq!(TEXT_COL, 20);
        assert_eq!(TEXT_ALPHA, 230);
        assert_eq!(TEXT2_ALPHA, 170);
        assert_eq!(TEXT3_ALPHA, 128);
        assert_eq!(DISABLED_ALPHA, 90);
        assert_eq!(SEP_ALPHA, 30);
    }

    #[test]
    fn tertiary_and_disabled_meet_the_contrast_floor() {
        assert_eq!(TEXT3_ALPHA, 128);
        assert_eq!(DISABLED_ALPHA, 90);
        assert!((contrast_on_black(110) - 4.12).abs() < 0.01);
        assert!((contrast_on_black(128) - 5.32).abs() < 0.01);
        assert!((contrast_on_black(90) - 3.04).abs() < 0.01);
        assert!(contrast_on_black(TEXT3_ALPHA) >= 4.5);
        assert!(contrast_on_black(DISABLED_ALPHA) >= 3.0);
        assert!(contrast_on_black(DISABLED_ALPHA) < contrast_on_black(TEXT3_ALPHA));
    }

    #[test]
    fn glyph_shape_called_once_per_surface() {
        let mut s = Surface::new(M).expect("surface");
        s.resize(64, 64).unwrap();
        let calls = Cell::new(0u32);
        let shape = |u: f32, v: f32| {
            calls.set(calls.get() + 1);
            u + v < 16.0
        };
        s.glyph_cached(7, 4, 4, 20, 255, &shape);
        assert_eq!(calls.get(), 16 * 20 * 20);
        s.glyph_cached(7, 30, 30, 20, 90, &shape);
        assert_eq!(calls.get(), 16 * 20 * 20, "the second paint reads the cache");
    }

    #[test]
    fn glyph_cache_is_bounded() {
        let mut s = Surface::new(M).expect("surface");
        s.resize(200, 200).unwrap();
        let shape = |u: f32, _v: f32| u < 8.0;
        for key in 0..(GLYPH_CACHE_MAX_ENTRIES as u32 + 10) {
            s.glyph_cached(key, 0, 0, 12, 255, &shape);
        }
        assert_eq!(s.glyph_hits.len(), GLYPH_CACHE_MAX_ENTRIES);
        s.glyph_cached(1, 0, 0, GLYPH_CACHE_MAX_SIZE + 1, 255, &shape);
        assert_eq!(s.glyph_hits.len(), GLYPH_CACHE_MAX_ENTRIES, "a big glyph is not kept");
        assert!(s.glyph_hits.iter().all(|(_, size, h)| h.len() == (size * size) as usize));
    }

    #[test]
    fn title_and_caption_fonts_exist() {
        let s = Surface::new(M).expect("surface");
        assert!(s.line_height(Font::Title) > s.line_height(Font::Normal));
        let c = s.line_height(Font::Caption);
        assert!(c >= s.line_height(Font::Small) && c <= s.line_height(Font::Normal));
        assert!(s.text_width("Appearance", Font::Title) > s.text_width("Appearance", Font::Bold));
    }

    #[test]
    fn query_font_is_made_on_first_use_and_is_larger_than_normal() {
        let s = Surface::new(M).expect("surface");
        assert!(s.query.get().is_none(), "not made with the surface");
        assert!(s.line_height(Font::Query) > s.line_height(Font::Normal));
        assert!(s.query.get().is_some());
        assert!(s.text_width("calc", Font::Query) > s.text_width("calc", Font::Normal));
    }

    #[test]
    fn blend_pixels_composites_and_clips() {
        let mut s = Surface::new(M).expect("surface");
        s.resize(4, 4).unwrap();
        s.fill_rr(rect_wh(0, 0, 4, 4), 0, (0, 0, 0), 255);
        s.blend_pixels(2, 2, 3, 3, &[0xFFFF_FFFF; 9]); // partly outside the DIB
        s.blend_pixels(0, 0, 2, 2, &[0x8000_0000; 4]); // half-transparent black over black
        let (_, _, px) = s.snapshot_pixels().unwrap();
        assert_eq!(px[0], 0xFF00_0000);
        assert_eq!(px[2 * 4 + 2], 0xFFFF_FFFF);
        assert_eq!(px[3 * 4 + 3], 0xFFFF_FFFF);
        s.blend_pixels(0, 0, 4, 4, &[0xFFFF_FFFF; 3]); // short buffer: ignored
        assert_eq!(s.snapshot_pixels().unwrap().2[0], 0xFF00_0000);
    }
}
