//! The digit atlas and decimal-aligned figures (PaintCx::digits, P2-kit 2.13, K2a.4). Per font an atlas of the cells a
//! number is made of ("0123456789.,KMGTBbps/%·—<" and the space) is rasterised once through the text engine; the ten
//! digits share the widest digit's advance (tabular figures in any font), so a column of figures lines up whatever the
//! face. A character outside the atlas goes through the text engine and is counted.

use std::collections::HashMap;

use crate::cx::PaintCx;
use crate::geom::{PointP, RectP};
use crate::model::Align;
use crate::text::{FontId, LayoutOpts, TextEngine, Trim};
use crate::theme::{Ink, TypeRole};
use crate::w3::{argb, div255, Canvas};

/// The characters of the atlas (the locale's two separators are among them).
const CHARS: &str = "0123456789.,KMGTBbps/%\u{b7}\u{2014}< ";

/// Fractional digits a decimal-aligned figure leaves room for after its decimal sign: a figure with fewer ends sooner
/// (an integer is placed as if its ".0" were there), one with more than this is not aligned.
pub(crate) const FRAC_SLOTS: i32 = 2;

/// One cell of the atlas: the coverage of a character and where it sits in its advance.
struct Cell {
    adv: i32,
    /// The coverage is drawn at this x inside the advance.
    dx: i32,
    w: i32,
    h: i32,
    cov: Vec<u8>,
}

/// The cells of one font.
struct Atlas {
    cells: HashMap<char, Cell>,
    line: i32,
    /// What the atlas was built for: a window has one DPI and one text scale at a time, so an atlas of another pair
    /// is dead weight (the text engine never reuses a font id after a DPI change).
    dpi: u32,
    text_scale: f32,
}

impl Atlas {
    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.cells.values().map(|c| c.cov.len() + std::mem::size_of::<Cell>()).sum()
    }

    /// The advance of `c`: the atlas's, or None for a character the atlas lacks.
    fn adv(&self, c: char) -> Option<i32> {
        self.cells.get(&c).map(|cell| cell.adv)
    }
}

/// Every window's digit atlases, by font (a font id is a role at a DPI and text scale, so an atlas is one role in use at
/// one DPI), and the count of characters that fell back to the text engine.
#[derive(Default)]
pub(crate) struct Atlases {
    fonts: HashMap<FontId, Atlas>,
    pub(crate) fallbacks: u32,
}

impl Atlases {
    /// The bytes of every atlas (the coverage and the cell records).
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.fonts.values().map(Atlas::bytes).sum()
    }

    /// How many atlases are kept.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.fonts.len()
    }
}

/// Rasterises the atlas of `font` through the engine: each character drawn white on transparent, its alpha read back.
fn build(engine: &mut dyn TextEngine, font: FontId, dpi: u32, text_scale: f32) -> Atlas {
    let opts = LayoutOpts { max_width: None, trim: Trim::None, wrap: false };
    let mut cells = HashMap::new();
    let mut line = 0;
    for ch in CHARS.chars() {
        let l = engine.layout(&ch.to_string(), font, &opts);
        line = line.max(l.height);
        let (w, h) = (l.width.max(0), l.height.max(0));
        let mut cov = Vec::new();
        if w > 0 && h > 0 {
            let mut buf = vec![0u32; (w * h) as usize];
            let mut cv = Canvas { buf: &mut buf, w, h, clip: None };
            engine.draw(&mut cv, &l, PointP { x: 0, y: 0 }, Ink::white(255));
            cov = buf.iter().map(|p| (p >> 24) as u8).collect();
        }
        if cov.iter().all(|c| *c == 0) {
            cov.clear();
        }
        cells.insert(ch, Cell { adv: w, dx: 0, w, h, cov });
    }
    // Tabular figures: every digit takes the widest digit's advance and sits centred in it.
    let digit_adv = ('0'..='9').filter_map(|d| cells.get(&d).map(|c| c.adv)).max().unwrap_or(0);
    for d in '0'..='9' {
        if let Some(c) = cells.get_mut(&d) {
            c.dx = (digit_adv - c.w) / 2;
            c.adv = digit_adv;
        }
    }
    Atlas { cells, line, dpi, text_scale }
}

/// True for a character of a figure's head (its number part): digits, the separators, a sign, "<" and the dash.
fn is_head(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '<' | '\u{2014}')
}

/// The room a decimal-aligned figure gives its tail: for a byte unit (" MB", " KB/s") the widest of the units of its kind,
/// else (percent, nothing) the tail's own width.
fn tail_reserve(tail: &[char], adv: &dyn Fn(char) -> i32) -> i32 {
    let own: i32 = tail.iter().map(|c| adv(*c)).sum();
    if !tail.iter().any(|c| c.is_alphabetic()) {
        return own;
    }
    let per_s = tail.ends_with(&['/', 's']);
    ["B", "KB", "MB", "GB", "TB"]
        .iter()
        .map(|u| {
            let rate = if per_s { "/s" } else { "" };
            format!(" {u}{rate}").chars().map(adv).sum::<i32>()
        })
        .fold(own, i32::max)
}

/// Where each character of `s` starts, and the x of the decimal point (for a figure without one: where its integer part
/// ends). `adv` is a character's advance. Right-aligned at `right`; with `decimal`, the tail (the unit) starts at a fixed x
/// (the widest unit's room from `right`) and the point sits FRAC_SLOTS digits before it, so figures with 0, 1 or 2
/// fractional digits and any unit line up on both.
pub(crate) fn place(
    s: &str,
    adv: &dyn Fn(char) -> i32,
    point: char,
    right: i32,
    decimal: bool,
) -> (Vec<(char, i32)>, i32) {
    let chars: Vec<char> = s.chars().collect();
    let head_len = chars.iter().take_while(|c| is_head(**c)).count();
    let width = |from: usize, to: usize| -> i32 { chars[from..to].iter().map(|c| adv(*c)).sum() };
    let (total, tail_w, head_w) = (width(0, chars.len()), width(head_len, chars.len()), width(0, head_len));
    let digit_adv = adv('0');
    let point_at = chars[..head_len].iter().position(|c| *c == point);
    // The tail starts at a fixed x: a unit takes the room of the widest unit of its family, so "MB", "GB" and "B" start
    // together and the reserved fraction slots sit between the number and the unit.
    let tail_x = right - tail_reserve(&chars[head_len..], adv).max(tail_w);
    let (x0, point_x) = if !decimal {
        let x0 = right - total;
        (x0, x0 + point_at.map_or(head_w, |p| width(0, p)))
    } else {
        let frac = point_at.map_or(0, |p| chars[p + 1..head_len].iter().filter(|c| c.is_ascii_digit()).count() as i32);
        let end_head = match point_at {
            Some(_) => tail_x - (FRAC_SLOTS - frac).max(0) * digit_adv,
            None => tail_x - adv(point) - FRAC_SLOTS * digit_adv,
        };
        let x0 = end_head - head_w;
        (x0, x0 + point_at.map_or(head_w, |p| width(0, p)))
    };
    let mut x = x0;
    let mut out = Vec::with_capacity(chars.len());
    for (i, c) in chars.into_iter().enumerate() {
        if decimal && i == head_len {
            x = tail_x;
        }
        out.push((c, x));
        x += adv(c);
    }
    (out, point_x)
}

/// Draws `s` right-aligned at `right_top`; returns the width drawn. Digits, separators and the dash take `ink`, letters,
/// "/s" and "%" take `unit`. With `decimal` the figure is aligned on its decimal sign (see `place`).
pub(crate) fn draw(
    cx: &mut PaintCx<'_>,
    s: &str,
    right_top: PointP,
    role: TypeRole,
    ink: Ink,
    unit: Ink,
    decimal: bool,
) -> i32 {
    if s.is_empty() {
        return 0;
    }
    let (scale, ts) = (cx.env.scale, cx.env.tokens.text_scale);
    let font = cx.res.text.font(role, None, scale, ts);
    if !cx.res.digits.fonts.contains_key(&font) {
        let atlas = build(&mut *cx.res.text, font, scale.dpi, ts);
        // The atlases of another DPI or text scale are dropped: memory stays at the cap over a long session.
        cx.res.digits.fonts.retain(|_, a| a.dpi == scale.dpi && a.text_scale == ts);
        cx.res.digits.fonts.insert(font, atlas);
    }
    let point = if cx.env.lang.italian { ',' } else { '.' };
    // The advance of a character outside the atlas comes from the engine, once per character of this call.
    let mut outside: HashMap<char, i32> = HashMap::new();
    for c in s.chars() {
        if cx.res.digits.fonts[&font].adv(c).is_none() && !outside.contains_key(&c) {
            let w = cx.res.text.width(&c.to_string(), font);
            outside.insert(c, w);
        }
    }
    let (items, x0, line, right) = {
        let atlas = &cx.res.digits.fonts[&font];
        let adv = |c: char| atlas.adv(c).or_else(|| outside.get(&c).copied()).unwrap_or(0);
        let (items, _) = place(s, &adv, point, right_top.x, decimal);
        let x0 = items.first().map_or(right_top.x, |i| i.1);
        (items, x0, atlas.line, right_top.x)
    };
    let width = right - x0;
    let area = RectP::wh(x0, right_top.y, width.max(0), line);
    if !cx.visible(area) {
        return width;
    }
    if let Some(log) = cx.text_log.as_deref_mut() {
        log.push(area);
    }
    cx.sync_clip();
    let origin = cx.origin;
    for (c, x) in items {
        let paint = if is_head(c) { ink } else { unit };
        if paint.a == 0 {
            continue;
        }
        let atlas = &cx.res.digits.fonts[&font];
        let Some(cell) = atlas.cells.get(&c) else {
            cx.res.digits.fallbacks += 1;
            let r = RectP::wh(x, right_top.y, outside.get(&c).copied().unwrap_or(0), line);
            cx.text_in(&c.to_string(), role, r, Align::Start, Trim::None, paint);
            cx.sync_clip();
            continue;
        };
        if cell.cov.is_empty() {
            continue;
        }
        let (ox, oy) = (x + cell.dx - origin.x, right_top.y - origin.y);
        for row in 0..cell.h {
            for col in 0..cell.w {
                let cov = u32::from(cell.cov[(row * cell.w + col) as usize]);
                if cov != 0 {
                    cx.canvas.blend(
                        ox + col,
                        oy + row,
                        argb(div255(cov * paint.a), paint.rgb.0, paint.rgb.1, paint.rgb.2),
                    );
                }
            }
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx};
    use crate::event::{Event, Reply};
    use crate::geom::{SizeL, SizeP};
    use crate::harness::Harness;
    use crate::screen::Screen;
    use crate::theme::Tokens;
    use std::cell::{Cell as StdCell, RefCell};

    /// One figure to draw: its text, right-top corner (px) and whether it aligns on the decimal sign; or the same text
    /// through the text engine.
    struct Fig {
        s: &'static str,
        right_top: (i32, i32),
        decimal: bool,
        engine: bool,
    }

    struct Figures {
        figs: Vec<Fig>,
        widths: RefCell<Vec<i32>>,
        fallbacks: StdCell<u32>,
        bytes: StdCell<usize>,
        count: StdCell<usize>,
        /// The point and the unit's start (x of the first space) of every figure, from the atlas's real advances.
        marks: RefCell<Vec<(i32, i32)>>,
        cmds: CommandTable,
    }

    impl Figures {
        fn new(figs: Vec<Fig>) -> Figures {
            Figures {
                figs,
                widths: RefCell::new(Vec::new()),
                fallbacks: StdCell::new(0),
                bytes: StdCell::new(0),
                count: StdCell::new(0),
                marks: RefCell::new(Vec::new()),
                cmds: CommandTable::new(),
            }
        }
    }

    impl Screen for Figures {
        fn layout(&mut self, _: &mut LayoutCx, avail: SizeP) -> SizeP {
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            let ink = Ink::white(255);
            self.widths.borrow_mut().clear();
            for f in &self.figs {
                let at = PointP { x: f.right_top.0, y: f.right_top.1 };
                let w = if f.engine {
                    let r = RectP::wh(at.x - 200, at.y, 200, 18);
                    cx.text_in(f.s, TypeRole::Body, r, Align::End, Trim::None, ink)
                } else {
                    cx.digits(f.s, at, TypeRole::Body, ink, ink, f.decimal)
                };
                self.widths.borrow_mut().push(w);
            }
            self.fallbacks.set(cx.res.digits.fallbacks);
            self.bytes.set(cx.res.digits.bytes());
            self.count.set(cx.res.digits.count());
            let font = cx.res.text.font(TypeRole::Body, None, cx.env.scale, cx.env.tokens.text_scale);
            let atlas = &cx.res.digits.fonts[&font];
            let adv = |c: char| atlas.adv(c).unwrap_or(0);
            let mut marks = self.marks.borrow_mut();
            marks.clear();
            for f in self.figs.iter().filter(|f| f.decimal) {
                let (items, point) = place(f.s, &adv, '.', f.right_top.0, true);
                marks.push((point, items.iter().find(|(c, _)| *c == ' ').map_or(0, |i| i.1)));
            }
        }

        fn event(&mut self, _: &mut EventCx, _: &Event) -> Reply {
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn fig(s: &'static str, y: i32, decimal: bool, engine: bool) -> Fig {
        Fig { s, right_top: (230, y), decimal, engine }
    }

    fn harness(figs: Vec<Fig>, dpi: u32) -> Harness<Figures> {
        Harness::new(Figures::new(figs), SizeL { w: 240, h: 120 }, dpi, Tokens::oled())
    }

    #[test]
    fn digits_are_tabular() {
        let mut h = harness(vec![fig("111.1", 8, false, false), fig("888.8", 30, false, false)], 96);
        h.render();
        let w = h.screen().widths.borrow().clone();
        assert!(w[0] > 0 && w[0] == w[1], "{w:?}");
        // Every digit has one advance, in any font and at any DPI.
        for dpi in [96, 192] {
            let mut h = harness(
                vec![fig("1111", 8, false, false), fig("0000", 30, false, false), fig("8888", 50, false, false)],
                dpi,
            );
            h.render();
            let w = h.screen().widths.borrow().clone();
            assert!(w[0] == w[1] && w[1] == w[2], "{dpi}: {w:?}");
        }
    }

    #[test]
    fn decimal_points_align() {
        // With fake advances: digits 8, point 4, letters and the space 9.
        let adv = |c: char| match c {
            '0'..='9' => 8,
            '.' | ',' => 4,
            _ => 9,
        };
        let points: Vec<i32> =
            ["3.10", "12.0", "100", "7.5"].iter().map(|s| place(s, &adv, '.', 200, true).1).collect();
        assert!(points.iter().all(|p| *p == points[0]), "{points:?}");
        // The figures with a unit: the point and the unit start are the same for every unit, " B" included.
        let placed: Vec<(i32, i32)> = ["612 MB", "1.25 GB", "80.4 MB", "3.10 KB", "12 B"]
            .iter()
            .map(|s| {
                let (items, point) = place(s, &adv, '.', 200, true);
                let unit = items.iter().find(|(c, _)| *c == ' ').unwrap().1;
                (point, unit)
            })
            .collect();
        assert!(placed.iter().all(|p| *p == placed[0]), "{placed:?}");
        assert_eq!(placed[0].1, 200 - 27, "the unit takes the room of the widest unit");
        // The Italian comma is the point there.
        assert_eq!(place("3,10", &adv, ',', 200, true).1, points[0]);
        // Through the real atlas: an integer's right edge falls where the point of "12.0" would be.
        let mut h =
            harness(vec![fig("12.0", 8, true, false), fig("100", 30, true, false), fig("3.10", 52, true, false)], 96);
        let img = h.render();
        let ink_right = |y0: u32| {
            (0..img.w).filter(|x| (y0..y0 + 18).any(|y| img.px[(y * img.w + x) as usize] & 0xFF > 100)).max().unwrap()
        };
        let (r_int, r_dec) = (ink_right(30), ink_right(8));
        assert!(r_int < r_dec, "the integer ends at the point, before the fraction: {r_int} {r_dec}");
        let w = h.screen().widths.borrow().clone();
        assert!(w[0] < w[1], "an integer reserves the point and the fraction: {w:?}");
    }

    #[test]
    fn real_atlas_aligns_points_and_units() {
        let mut h = harness(
            vec![
                fig("3.10 GB", 8, true, false),
                fig("289 MB", 30, true, false),
                fig("12.0 KB", 52, true, false),
                fig("100 MB", 74, true, false),
            ],
            96,
        );
        h.render();
        let m = h.screen().marks.borrow().clone();
        assert_eq!(m.len(), 4);
        assert!(m.iter().all(|x| *x == m[0]), "{m:?}");
    }

    #[test]
    fn atlas_close_to_gdi_text() {
        // "602 MB", not "612 MB": the face's "1" is narrower than the other digits, and the atlas widens it on purpose
        // (tabular figures), so a run with a 1 differs from the engine's by the width of the difference.
        let mut h = harness(vec![fig("602 MB", 10, false, false), fig("602 MB", 50, false, true)], 96);
        let img = h.render();
        let rows = |y0: u32| -> Vec<u32> {
            (y0..y0 + 18)
                .flat_map(|y| (30..230).map(move |x| (x, y)))
                .map(|(x, y)| img.px[(y * img.w + x) as usize])
                .collect()
        };
        let (a, b) = (rows(10), rows(50));
        let (mut sum, mut outliers, mut masked) = (0u64, 0usize, 0usize);
        for (pa, pb) in a.iter().zip(&b) {
            let ch = |p: u32, s: u32| ((p >> s) & 0xFF) as i32;
            let d = (ch(*pa, 0) - ch(*pb, 0)).abs();
            sum += d as u64;
            if ch(*pa, 0) > 0 || ch(*pb, 0) > 0 {
                masked += 1;
                if d > 64 {
                    outliers += 1;
                }
            }
        }
        assert!(masked > 40, "something was drawn");
        let mean_x100 = sum * 100 / a.len() as u64;
        assert!(mean_x100 <= 300, "mean delta {mean_x100}/100");
        assert!(outliers * 100 <= masked, "{outliers} of {masked} pixels differ by more than 64");
    }

    #[test]
    fn no_fallback_for_numbers() {
        let mut h = harness(
            vec![
                fig("1,234.5 MB/s", 8, true, false),
                fig("99%", 30, false, false),
                fig("<1 KB", 50, false, false),
                fig("\u{2014}", 70, false, false),
                fig("12.5 Tbps \u{b7} 3 GB", 90, false, false),
            ],
            144,
        );
        h.render();
        assert_eq!(h.screen().fallbacks.get(), 0);
        // A letter outside the atlas goes through the engine, and is counted.
        let mut h = harness(vec![fig("7 \u{e9}", 8, false, false)], 96);
        h.render();
        assert_eq!(h.screen().fallbacks.get(), 1);
    }

    #[test]
    fn atlases_stay_within_the_cap() {
        let mut h = harness(vec![fig("612 MB", 8, true, false)], 192);
        h.render();
        let bytes = h.screen().bytes.get();
        assert!(bytes > 0 && bytes <= 100 * 1024, "{bytes} bytes (cap 0.1 MiB)");
    }

    #[test]
    fn atlases_of_another_text_scale_are_dropped() {
        let mut h = harness(vec![fig("612 MB", 8, true, false)], 96);
        h.render();
        let one = h.screen().bytes.get();
        for ts in [1.25, 1.5, 1.0, 1.25] {
            let mut t = Tokens::oled();
            t.text_scale = ts;
            h.set_tokens(t);
            h.render();
            assert_eq!(h.screen().count.get(), 1, "text scale {ts}");
            assert!(h.screen().bytes.get() <= 100 * 1024);
        }
        assert!(one > 0);
    }
}
