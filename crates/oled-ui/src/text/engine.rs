//! The text engine (F-K1, P2-kit 2.3): fonts by type role, cached layouts, drawing onto W3's Canvas, hit testing. GdiText
//! is the suite's text renderer (ROADMAP Q12); hit testing is refined by K2b.2.

use std::rc::Rc;

use windows::Win32::Graphics::Gdi::{GetTextMetricsW, SelectObject, HFONT, HGDIOBJ, TEXTMETRICW};

use super::cache::{fnv, Mask, RunCache};
use super::hit;
use super::trim::{trim_to, wrap_lines, Trim};
use crate::geom::{PointP, Scale};
use crate::host::UiError;
use crate::theme::{Ink, TypeRole};
use crate::tokens;
use crate::w3::{argb, create_font, delete_font, div255, line_height, over, Canvas, Gdi};

/// A font of the engine (a role, weight, DPI and text scale).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId(pub u16);

/// How a text is laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOpts {
    pub max_width: Option<i32>,
    pub trim: Trim,
    pub wrap: bool,
}

/// A laid-out text: the text after trimming, its size in physical px and its lines (byte range, y).
#[derive(Debug)]
pub struct TextLayout {
    pub text: String,
    pub font: FontId,
    pub width: i32,
    pub height: i32,
    pub lines: Vec<(std::ops::Range<usize>, i32)>,
    pub trimmed: bool,
}

/// A position in a text: the byte offset of a character and whether the hit is on its trailing half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextHit {
    pub index: usize,
    pub trailing: bool,
}

/// A font's vertical metrics in physical px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontMetrics {
    pub ascent: i32,
    pub descent: i32,
    pub line_height: i32,
}

/// What the kit measures and draws text with.
pub trait TextEngine {
    /// The font of a role (with an optional weight) at a scale and text scale; the same inputs give the same id.
    fn font(&mut self, role: TypeRole, weight: Option<u16>, scale: Scale, text_scale: f32) -> FontId;
    /// The layout of `text` (LRU-cached).
    fn layout(&mut self, text: &str, font: FontId, opts: &LayoutOpts) -> Rc<TextLayout>;
    /// The width of `text` on one line, physical px.
    fn width(&mut self, text: &str, font: FontId) -> i32;
    /// Draws a layout with its top-left corner at `at` (canvas-local).
    fn draw(&mut self, cv: &mut Canvas<'_>, l: &TextLayout, at: PointP, ink: Ink);
    /// The character boundary nearest to x (by cluster, K2b.2).
    fn hit_test(&self, l: &TextLayout, x: i32) -> TextHit;
    /// The x of the caret before the character at byte `index`.
    fn caret_x(&self, l: &TextLayout, index: usize) -> i32;
    /// A font's metrics.
    fn metrics(&self, font: FontId) -> FontMetrics;
}

/// One font of the engine.
struct FontEntry {
    hfont: HFONT,
    role: TypeRole,
    weight: u16,
    dpi: u32,
    px: i32,
    line_height: i32,
    ascent: i32,
    descent: i32,
}

/// The engine on W3's GdiText. Its Gdi (the scratch DIB) is created at the first font or layout call, so a window that
/// draws no text holds no text DIB. The digit atlases join it in K2a.4 and the cluster hit testing in K2b.2.
pub struct GdiTextEngine {
    gdi: Option<Gdi>,
    /// Indexed by FontId; an evicted font leaves a hole, ids are never reused.
    fonts: Vec<Option<FontEntry>>,
    cache: RunCache,
    #[cfg(test)]
    pub(crate) raster_calls: u32,
}

impl GdiTextEngine {
    /// An engine with nothing created yet.
    pub fn new() -> Result<GdiTextEngine, UiError> {
        Ok(GdiTextEngine {
            gdi: None,
            fonts: Vec::new(),
            cache: RunCache::new(),
            #[cfg(test)]
            raster_calls: 0,
        })
    }

    /// Drops the fonts and cached runs of one DPI (WM_DPICHANGED).
    pub fn evict_dpi(&mut self, dpi: u32) {
        let mut gone = Vec::new();
        for (i, slot) in self.fonts.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|f| f.dpi == dpi) {
                if let Some(f) = slot.take() {
                    delete_font(f.hfont);
                }
                gone.push(FontId(i as u16));
            }
        }
        self.cache.evict_fonts(&gone);
    }

    /// The GDI object, created at the first use; None when Windows refuses the DCs.
    fn gdi(&mut self) -> Option<&Gdi> {
        if self.gdi.is_none() {
            self.gdi = Gdi::new().ok();
        }
        self.gdi.as_ref()
    }

    fn entry(&self, id: FontId) -> Option<&FontEntry> {
        self.fonts.get(usize::from(id.0)).and_then(Option::as_ref)
    }

    /// The font's handle and line height.
    fn face(&self, id: FontId) -> Option<(HFONT, i32)> {
        self.entry(id).map(|f| (f.hfont, f.line_height))
    }

    /// The coverage mask of one line (cached): rasterised once through the scratch DIB.
    fn mask_for(&mut self, font: FontId, s: &str) -> Option<Rc<Mask>> {
        let key = fnv(&[s.as_bytes(), &font.0.to_le_bytes()]);
        if let Some(m) = self.cache.mask(key) {
            return Some(m);
        }
        let (hf, lh) = self.face(font)?;
        self.gdi()?;
        let gdi = self.gdi.as_mut()?;
        let w = gdi.text_width(s, hf, 0);
        if w <= 0 || gdi.ensure_txt(w, lh).is_err() {
            return None;
        }
        #[cfg(test)]
        {
            self.raster_calls += 1;
        }
        let gdi = self.gdi.as_ref()?;
        let (mw, mh, data) = gdi.text_mask(s, (hf, lh), w)?;
        let m = Rc::new(Mask { w: mw, h: mh, data });
        self.cache.put_mask(key, font, s.len(), Rc::clone(&m));
        Some(m)
    }

    fn build_layout(&mut self, text: &str, font: FontId, opts: &LayoutOpts) -> TextLayout {
        let (Some((hf, lh)), Some(gdi)) = (self.face(font), self.gdi.as_ref()) else {
            return TextLayout {
                text: text.to_string(),
                font,
                width: 0,
                height: 0,
                lines: vec![(0..text.len(), 0)],
                trimmed: false,
            };
        };
        let mut wf = |s: &str| gdi.text_width(s, hf, 0);
        if opts.wrap {
            let ranges = wrap_lines(text, opts.max_width.unwrap_or(i32::MAX), &mut wf);
            let width = ranges.iter().map(|r| wf(&text[r.clone()])).max().unwrap_or(0);
            let height = lh * ranges.len() as i32;
            let lines = ranges.into_iter().enumerate().map(|(i, r)| (r, lh * i as i32)).collect();
            return TextLayout { text: text.to_string(), font, width, height, lines, trimmed: false };
        }
        let cut = opts.max_width.and_then(|mw| trim_to(text, opts.trim, mw, &mut wf));
        let trimmed = cut.is_some();
        let text = cut.unwrap_or_else(|| text.to_string());
        let width = wf(&text);
        TextLayout { font, width, height: lh, lines: vec![(0..text.len(), 0)], trimmed, text }
    }
}

impl Drop for GdiTextEngine {
    fn drop(&mut self) {
        for f in self.fonts.drain(..).flatten() {
            delete_font(f.hfont);
        }
    }
}

fn font_metrics_of(dc: windows::Win32::Graphics::Gdi::HDC, f: HFONT) -> (i32, i32) {
    let mut tm = TEXTMETRICW::default();
    // SAFETY: `dc` is the engine's live text DC and `tm` is writable; the font is selected only for the call and the
    // previous object is restored right away.
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(f.0));
        let ok = GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        if ok.as_bool() {
            (tm.tmAscent, tm.tmDescent)
        } else {
            (0, 0)
        }
    }
}

impl TextEngine for GdiTextEngine {
    fn font(&mut self, role: TypeRole, weight: Option<u16>, scale: Scale, text_scale: f32) -> FontId {
        let spec = role.spec();
        let weight = weight.unwrap_or(spec.weight);
        let px = ((scale.px(spec.size) as f32) * text_scale).round() as i32;
        if let Some(i) = self.fonts.iter().position(|f| {
            f.as_ref().is_some_and(|f| f.role == role && f.weight == weight && f.dpi == scale.dpi && f.px == px)
        }) {
            return FontId(i as u16);
        }
        let Some(gdi) = self.gdi() else { return FontId(0) };
        let faces: &[&str] = match role {
            TypeRole::Title => &tokens::FACE_TITLE,
            TypeRole::Display => &tokens::FACE_DISPLAY,
            _ => &tokens::FACE_TEXT,
        };
        let dc = gdi.dc_txt();
        let Ok(hfont) = create_font(dc, faces, px, i32::from(weight)) else { return FontId(0) };
        let (ascent, descent) = font_metrics_of(dc, hfont);
        let entry =
            FontEntry { hfont, role, weight, dpi: scale.dpi, px, line_height: line_height(dc, hfont), ascent, descent };
        let id = self.fonts.len().min(usize::from(u16::MAX));
        if id == usize::from(u16::MAX) {
            delete_font(hfont);
            return FontId(0);
        }
        self.fonts.push(Some(entry));
        FontId(id as u16)
    }

    fn layout(&mut self, text: &str, font: FontId, opts: &LayoutOpts) -> Rc<TextLayout> {
        let (mw, trim) = (opts.max_width.unwrap_or(-1).to_le_bytes(), opts.trim);
        let trim_bytes = match trim {
            Trim::None => [0, 0],
            Trim::End => [1, 0],
            Trim::Middle { tail } => [2, tail],
            Trim::Start => [3, 0],
        };
        let key = fnv(&[text.as_bytes(), &font.0.to_le_bytes(), &mw, &trim_bytes, &[u8::from(opts.wrap)]]);
        if let Some(l) = self.cache.layout(key) {
            return l;
        }
        self.gdi();
        let l = Rc::new(self.build_layout(text, font, opts));
        // Rasterise now, once: the draw that follows is a cache hit.
        for (r, _) in &l.lines {
            if !l.text[r.clone()].is_empty() {
                self.mask_for(font, &l.text[r.clone()]);
            }
        }
        self.cache.put_layout(key, Rc::clone(&l));
        l
    }

    fn width(&mut self, text: &str, font: FontId) -> i32 {
        let Some((hf, _)) = self.face(font) else { return 0 };
        self.gdi().map_or(0, |g| g.text_width(text, hf, 0))
    }

    fn draw(&mut self, cv: &mut Canvas<'_>, l: &TextLayout, at: PointP, ink: Ink) {
        if ink.a == 0 {
            return;
        }
        for (range, y) in &l.lines {
            let s = &l.text[range.clone()];
            if s.is_empty() {
                continue;
            }
            let Some(m) = self.mask_for(l.font, s) else { continue };
            // The same blends as Canvas::blend, with the buffer and clip bounds worked out once per run and per row.
            let (x0, y0) = (at.x, at.y + y);
            let (mut cl, mut ct, mut cr, mut cb) = (0, 0, cv.w, cv.h);
            if let Some(c) = cv.clip {
                (cl, ct, cr, cb) = (cl.max(c.left), ct.max(c.top), cr.min(c.right), cb.min(c.bottom));
            }
            let (col_lo, col_hi) = ((cl - x0).max(0), (cr - x0).min(m.w));
            if col_lo >= col_hi {
                continue;
            }
            for row in 0..m.h {
                let py = y0 + row;
                if py < ct || py >= cb {
                    continue;
                }
                let line = &m.data[(row * m.w) as usize..((row + 1) * m.w) as usize];
                let dst = (py * cv.w + x0) as usize;
                for col in col_lo..col_hi {
                    let cov = u32::from(line[col as usize]);
                    if cov != 0 {
                        let i = dst.wrapping_add(col as usize);
                        cv.buf[i] = over(cv.buf[i], argb(div255(cov * ink.a), ink.rgb.0, ink.rgb.1, ink.rgb.2));
                    }
                }
            }
        }
    }

    fn hit_test(&self, l: &TextLayout, x: i32) -> TextHit {
        let mut ext = Vec::new();
        if let (Some(g), Some((hf, _))) = (self.gdi.as_ref(), self.face(l.font)) {
            g.char_extents(&l.text, hf, 0, &mut ext);
        }
        hit::hit_test(l, &ext, x)
    }

    fn caret_x(&self, l: &TextLayout, index: usize) -> i32 {
        let mut ext = Vec::new();
        if let (Some(g), Some((hf, _))) = (self.gdi.as_ref(), self.face(l.font)) {
            g.char_extents(&l.text, hf, 0, &mut ext);
        }
        hit::caret_x(l, &ext, index)
    }

    fn metrics(&self, font: FontId) -> FontMetrics {
        self.entry(font).map_or(FontMetrics { ascent: 0, descent: 0, line_height: 0 }, |f| FontMetrics {
            ascent: f.ascent,
            descent: f.descent,
            line_height: f.line_height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::w3::Gdi as RefGdi;

    const OPTS: LayoutOpts = LayoutOpts { max_width: None, trim: Trim::End, wrap: false };

    fn engine() -> GdiTextEngine {
        GdiTextEngine::new().unwrap()
    }

    #[test]
    fn draw_equals_gdi_text() {
        for dpi in [96u32, 144] {
            let scale = Scale { dpi };
            let text = "Settings 123 \u{e0}\u{e8}\u{ec}";
            let (w, h) = (260, 48);
            let mut e = engine();
            let f = e.font(TypeRole::Body, None, scale, 1.0);
            let l = e.layout(text, f, &OPTS);
            let mut buf = vec![0u32; (w * h) as usize];
            e.draw(
                &mut Canvas { buf: &mut buf, w, h, clip: None },
                &l,
                PointP { x: 5, y: 4 },
                Ink { rgb: (200, 220, 255), a: 230 },
            );

            let mut g = RefGdi::new().unwrap();
            g.resize(w, h, h).unwrap();
            let px = scale.px(13);
            let hf = create_font(g.dc_txt(), &tokens::FACE_TEXT, px, 400).unwrap();
            let lh = line_height(g.dc_txt(), hf);
            let tw = g.text_width(text, hf, 0);
            {
                let mut cv = g.canvas(None).unwrap();
                g.text(&mut cv, text, 5, 4, (hf, lh), tw, (200, 220, 255), 230);
            }
            let (_, _, want) = g.snapshot_pixels().unwrap();
            delete_font(hf);
            assert_eq!(l.width, tw);
            assert_eq!(l.height, lh);
            assert!(buf.iter().any(|p| *p != 0), "the text drew nothing");
            assert_eq!(buf, want, "dpi {dpi}");
        }
    }

    #[test]
    fn fonts_are_reused_and_scaled() {
        let mut e = engine();
        let a = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        assert_eq!(a, e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0));
        assert_eq!(a, e.font(TypeRole::Body, Some(400), Scale { dpi: 96 }, 1.0));
        assert_ne!(a, e.font(TypeRole::BodyStrong, None, Scale { dpi: 96 }, 1.0));
        assert_ne!(a, e.font(TypeRole::Body, None, Scale { dpi: 144 }, 1.0));
        let big = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.5);
        assert!(e.metrics(big).line_height > e.metrics(a).line_height);
        assert!(e.metrics(a).ascent > 0);
    }

    #[test]
    fn trim_end_matches_ellipsize() {
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let reference = |e: &mut GdiTextEngine, s: &str, max_w: i32| -> String {
            if e.width(s, f) <= max_w {
                return s.to_string();
            }
            let chars: Vec<char> = s.chars().collect();
            let (mut lo, mut hi) = (0usize, chars.len());
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let mut cand: String = chars[..mid].iter().collect();
                cand = cand.trim_end().to_string();
                cand.push('\u{2026}');
                if e.width(&cand, f) <= max_w {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            let mut out: String = chars[..lo].iter().collect();
            out = out.trim_end().to_string();
            out.push('\u{2026}');
            out
        };
        let cases = [
            ("Short", 200),
            ("A rather long label for a button", 120),
            ("Process Explorer helper service", 80),
            ("iiiiiiiiiiiiiiiiiiiiiiii", 50),
            ("WWWWWWWWWWWWWWWWWW", 90),
            ("tail   with spaces      here", 100),
        ];
        for (s, w) in cases {
            let want = reference(&mut e, s, w);
            let l = e.layout(s, f, &LayoutOpts { max_width: Some(w), ..OPTS });
            assert_eq!(l.text, want, "{s} at {w}");
            assert_eq!(l.trimmed, want != s);
            assert!(l.width <= w || want == "\u{2026}");
        }
    }

    #[test]
    fn trim_middle_keeps_tail_and_start_keeps_end() {
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let s = "very-long-name-of-the-report-draft-07.docx";
        let l = e.layout(s, f, &LayoutOpts { max_width: Some(200), trim: Trim::Middle { tail: 10 }, wrap: false });
        assert!(l.text.ends_with("ft-07.docx") && l.text.contains('\u{2026}') && l.width <= 200, "{}", l.text);
        let p = "C:\\Users\\someone\\Documents\\reports\\2026\\final.docx";
        let l = e.layout(p, f, &LayoutOpts { max_width: Some(180), trim: Trim::Start, wrap: false });
        assert!(l.text.starts_with('\u{2026}') && l.text.ends_with("final.docx") && l.width <= 180, "{}", l.text);
    }

    #[test]
    fn cache_hit_skips_gdi() {
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let mut buf = vec![0u32; 200 * 30];
        let mut cv = Canvas { buf: &mut buf, w: 200, h: 30, clip: None };
        let l = e.layout("Cached run", f, &OPTS);
        e.draw(&mut cv, &l, PointP { x: 2, y: 2 }, Ink::white(255));
        let calls = e.raster_calls;
        assert_eq!(calls, 1);
        let l2 = e.layout("Cached run", f, &OPTS);
        assert!(Rc::ptr_eq(&l, &l2));
        e.draw(&mut cv, &l2, PointP { x: 2, y: 2 }, Ink::white(255));
        assert_eq!(e.raster_calls, calls);
    }

    #[test]
    fn cache_evicts_past_512_kib() {
        let mut e = engine();
        let f = e.font(TypeRole::Display, None, Scale { dpi: 192 }, 1.0);
        for i in 0..400 {
            let s = format!("A long display run number {i} that costs real mask bytes {i}{i}{i}");
            e.layout(&s, f, &OPTS);
        }
        assert!(e.cache.bytes() <= 512 * 1024, "{}", e.cache.bytes());
        assert!(e.cache.len() < 800, "nothing was evicted: {}", e.cache.len());
    }

    #[test]
    fn evict_dpi_drops_fonts_and_runs() {
        let mut e = engine();
        let a = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let b = e.font(TypeRole::Body, None, Scale { dpi: 144 }, 1.0);
        e.layout("one", a, &OPTS);
        e.layout("two", b, &OPTS);
        e.evict_dpi(96);
        assert_eq!(e.metrics(a).line_height, 0);
        assert!(e.metrics(b).line_height > 0);
        assert_eq!(e.cache.len(), 2); // b's layout and mask
        let a2 = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        assert_ne!(a2, a);
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let s = "Close the window without saving the changes you made to this document";
        let l = e.layout(s, f, &LayoutOpts { max_width: Some(150), trim: Trim::None, wrap: true });
        assert!(l.lines.len() >= 3);
        let lh = e.metrics(f).line_height;
        for (i, (r, y)) in l.lines.iter().enumerate() {
            assert_eq!(*y, lh * i as i32);
            assert!(e.width(&l.text[r.clone()], f) <= 150, "{}", &l.text[r.clone()]);
        }
        assert_eq!(l.height, lh * l.lines.len() as i32);
    }

    #[test]
    fn a_longer_run_after_a_short_one_still_draws() {
        // The scratch DIB grows for the second run: the text must still land in the new one.
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let draw = |e: &mut GdiTextEngine, s: &str| {
            let l = e.layout(s, f, &OPTS);
            let mut buf = vec![0u32; 400 * 30];
            e.draw(
                &mut Canvas { buf: &mut buf, w: 400, h: 30, clip: None },
                &l,
                PointP { x: 2, y: 2 },
                Ink::white(255),
            );
            buf.iter().filter(|p| **p != 0).count()
        };
        assert!(draw(&mut e, "ab") > 0);
        assert!(draw(&mut e, "A much longer run of text than the first one") > 100);
        assert!(draw(&mut e, "ab") > 0);
    }

    #[test]
    fn draw_clipped_equals_blend_by_blend() {
        // The run-wise draw and a per-pixel Canvas::blend reference agree, whatever the clip and the position
        // (partly outside the buffer, left of it, above it).
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 144 }, 1.0);
        let l = e.layout("Clipped 0123 text", f, &OPTS);
        let mask = e.mask_for(f, &l.text).unwrap();
        let ink = Ink { rgb: (200, 120, 40), a: 180 };
        let rect = |l, t, r, b| Some(windows::Win32::Foundation::RECT { left: l, top: t, right: r, bottom: b });
        for (at, clip) in [
            (PointP { x: 5, y: 3 }, None),
            (PointP { x: -20, y: -6 }, None),
            (PointP { x: 40, y: 10 }, rect(50, 14, 90, 30)),
            (PointP { x: 90, y: 25 }, rect(0, 0, 100, 40)),
            (PointP { x: 5, y: 3 }, rect(60, 40, 10, 5)),
        ] {
            let (w, h) = (120, 40);
            let mut got = vec![0u32; (w * h) as usize];
            e.draw(&mut Canvas { buf: &mut got, w, h, clip }, &l, at, ink);
            let mut want = vec![0u32; (w * h) as usize];
            let mut cv = Canvas { buf: &mut want, w, h, clip };
            for row in 0..mask.h {
                for col in 0..mask.w {
                    let cov = u32::from(mask.data[(row * mask.w + col) as usize]);
                    if cov != 0 {
                        cv.blend(at.x + col, at.y + row, argb(div255(cov * ink.a), ink.rgb.0, ink.rgb.1, ink.rgb.2));
                    }
                }
            }
            assert_eq!(got, want, "at {at:?} clip {clip:?}");
        }
    }

    #[test]
    fn no_text_means_no_gdi() {
        let e = engine();
        assert!(e.gdi.is_none());
    }

    #[test]
    fn hit_test_uses_the_font() {
        let mut e = engine();
        let f = e.font(TypeRole::Body, None, Scale { dpi: 96 }, 1.0);
        let l = e.layout("abc", f, &OPTS);
        assert_eq!(e.caret_x(&l, 0), 0);
        assert_eq!(e.caret_x(&l, 3), l.width);
        assert_eq!(e.hit_test(&l, 0).index, 0);
        assert_eq!(e.hit_test(&l, l.width + 10).index, 3);
    }
}
