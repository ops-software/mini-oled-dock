//! The text engine (F-K1, P2-kit 2.3): fonts by type role, cached layouts, drawing onto W3's Canvas, hit testing. GdiText
//! is the suite's text renderer (ROADMAP Q12); the engine behind the trait is filled by K1a.2, hit testing by K2b.2.

use std::rc::Rc;

use super::hit;
use super::trim::Trim;
use crate::geom::{PointP, Scale};
use crate::host::UiError;
use crate::theme::{Ink, TypeRole};
use crate::w3::{Canvas, Gdi};

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

/// The engine on W3's GdiText. Its Gdi (the scratch DIB) is created at the first font or layout call, so a window that
/// draws no text holds no text DIB. Filled by K1a.2 (fonts, run cache), K2a.4 (digit atlases) and K2b.2 (hit testing).
pub struct GdiTextEngine {
    gdi: Option<Gdi>,
}

impl GdiTextEngine {
    /// An engine with nothing created yet.
    pub fn new() -> Result<GdiTextEngine, UiError> {
        Ok(GdiTextEngine { gdi: None })
    }

    /// Drops the fonts and cached runs of one DPI (WM_DPICHANGED). Filled by K1a.2.
    pub fn evict_dpi(&mut self, dpi: u32) {
        let _ = dpi;
    }
}

impl TextEngine for GdiTextEngine {
    /// Filled by K1a.2; until then every role is FontId(0).
    fn font(&mut self, role: TypeRole, weight: Option<u16>, scale: Scale, text_scale: f32) -> FontId {
        let _ = (role, weight, scale, text_scale);
        FontId(0)
    }

    /// Filled by K1a.2; until then the untrimmed text on one line of size 0.
    fn layout(&mut self, text: &str, font: FontId, opts: &LayoutOpts) -> Rc<TextLayout> {
        let _ = opts;
        Rc::new(TextLayout {
            text: text.to_string(),
            font,
            width: 0,
            height: 0,
            lines: vec![(0..text.len(), 0)],
            trimmed: false,
        })
    }

    /// Filled by K1a.2; until then 0.
    fn width(&mut self, text: &str, font: FontId) -> i32 {
        let _ = (text, font);
        0
    }

    /// Filled by K1a.2; until then it draws nothing.
    fn draw(&mut self, cv: &mut Canvas<'_>, l: &TextLayout, at: PointP, ink: Ink) {
        let _ = (cv, l, at, ink, self.gdi.is_some());
    }

    fn hit_test(&self, l: &TextLayout, x: i32) -> TextHit {
        // K1a.2 passes the layout font's char extents (Gdi::char_extents); no font exists before it.
        hit::hit_test(l, &[], x)
    }

    fn caret_x(&self, l: &TextLayout, index: usize) -> i32 {
        hit::caret_x(l, &[], index)
    }

    /// Filled by K1a.2; until then all 0.
    fn metrics(&self, font: FontId) -> FontMetrics {
        let _ = font;
        FontMetrics { ascent: 0, descent: 0, line_height: 0 }
    }
}
