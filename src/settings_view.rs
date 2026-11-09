//! View of the Settings window: layout, hit ids, painting, scrolling, the mini live bar and the
//! `--preview` pixels. No window and no input here: `settings_window` owns the HWND, feeds a `Frame`
//! (page, rows, hover, focus) to `layout` and `paint`, and maps the hit ids back with `decode`.
//!
//! Layout (logical px): a 200 wide navigation column (entries 36 high, About at the bottom with
//! Updates above it) and the page on its right: title and subtitle, then the rows of
//! `settings_pages::rows`, at most 640 wide, scrolled as one block. A row shows an icon in a 20 px
//! column, the label with an optional description below it, and its control at the right. When the
//! control does not fit beside the label (a long choice, Italian labels at the minimum width) it
//! moves to a second line inside the same row, so nothing ever draws over a label. Descriptions wrap
//! instead of being cut.
//!
//! Every pixel of the client is written (opaque config background), as `Surface::copy_to` needs.

// The consumer (`settings_window`) lands in the next task.
#![allow(dead_code)]

use windows::Win32::Foundation::RECT;

use crate::audio::VolumeState;
use crate::config::{Background, Config, Position, BACKGROUND_PRESETS};
use crate::glyphs;
use crate::i18n::tr;
use crate::popup::{center_y, lift, paint_slider, toggle_switch, HitMap};
use crate::popup_tray::TrayRow;
use crate::render::{self, AppView, BarSizes, BatteryView, Indicator, ItemId, Metrics, Scene, SystemView};
use crate::settings_pages::{self, hsv_to_rgb, Control, Ctx, Page, Row};
use crate::surface::{
    over, rect_wh, white, Font, Surface, HOVER_ALPHA, PANEL_BORDER_ALPHA, PRESS_ALPHA, ROW_RADIUS, SEP_ALPHA,
    TEXT2_ALPHA, TEXT3_ALPHA, TEXT_ALPHA, WHITE,
};

/// Width of the navigation column (logical px; physical = `Metrics::px`).
pub(crate) const NAV_W: i32 = 200;
/// Height of a normal row.
pub(crate) const ROW_H: i32 = 44;
/// Height of a group header.
pub(crate) const HEADER_H: i32 = 32;
/// Minimum height of a note without icon.
pub(crate) const NOTE_H: i32 = 32;
/// Height of a tray app row of the inline chooser.
pub(crate) const TRAY_ROW_H: i32 = 36;
/// Height of the preview card (Appearance).
pub(crate) const PREVIEW_H: i32 = 120;
/// Height of the inline colour picker.
pub(crate) const PICKER_H: i32 = 170;
/// A row whose control goes on a second line (no description).
pub(crate) const WRAPPED_ROW_H: i32 = 72;
/// Maximum width of the page content.
pub(crate) const PAGE_MAX_W: i32 = 640;
/// Left and right padding of the page.
pub(crate) const PAGE_PAD_X: i32 = 32;
/// Top of the page title.
pub(crate) const PAGE_TOP: i32 = 24;
/// Minimum height of the title block (title + subtitle + gap).
pub(crate) const TITLE_BLOCK: i32 = 64;
/// Scroll per wheel notch.
pub(crate) const SCROLL_STEP: i32 = 48;
/// Fake caption of the preview PNG.
pub(crate) const CAPTION_H: i32 = 32;

/// Free space under the last row.
const PAGE_BOTTOM: i32 = 24;
/// Navigation entries.
const NAV_ENTRY_H: i32 = 36;
const NAV_INSET: i32 = 8;
const NAV_TOP: i32 = 12;
const NAV_GAP: i32 = 2;
/// Icon column and text column of a row, from its left edge; inset of the control from its right.
const ICON_X: i32 = 12;
const TEXT_X: i32 = 44;
const CTL_INSET: i32 = 12;
/// Smallest gap between a label (or description) and the control at its right.
const LABEL_GAP: i32 = 12;
/// Narrowest description column beside a control; below it the control goes on a second line.
const MIN_DESC_W: i32 = 150;
/// Vertical padding of a row around its text block.
const ROW_PAD_Y: i32 = 10;
/// Segmented choice.
const SEG_H: i32 = 24;
const SEG_PAD: i32 = 9;
const SEG_MIN_W: i32 = 30;
const SEG_TRACK_ALPHA: u32 = 14;
const SEG_RIM_ALPHA: u32 = 40;
const SEG_HOT_ALPHA: u32 = 26;
/// Slider: track width (it may shrink down to `SLIDER_MIN_W`), value column, knob.
const SLIDER_W: i32 = 180;
const SLIDER_MIN_W: i32 = 96;
const VALUE_W: i32 = 52;
const SLIDER_BAND_H: i32 = 20;
const KNOB_R: i32 = 7;
/// Swatches: diameter, ring room around each one (its hit square), rim of the swatch.
const SWATCH_D: i32 = 20;
const SWATCH_RING: i32 = 3;
const SWATCH_RIM_ALPHA: u32 = 70;
/// Picker parts.
const SV_H: i32 = 112;
const HUE_H: i32 = 12;
const HEX_ROW_H: i32 = 28;
const PART_SV: usize = 0;
const PART_HUE: usize = 1;
/// Buttons.
const BTN_H: i32 = 28;
const BTN_PAD: i32 = 14;
const BTN_GAP: i32 = 8;
const BTN_ALPHA: u32 = 30;
const BTN_HOT_ALPHA: u32 = 46;
const BTN_PRESS_ALPHA: u32 = 62;
const BTN_OFF_ALPHA: u32 = 12;
const BTN_RIM_ALPHA: u32 = 24;
/// Glyph size of icons and chevrons.
const ICON: i32 = 16;
/// Alpha of the navigation column fill and of the focus ring.
const NAV_FILL_ALPHA: u32 = 8;
const FOCUS_ALPHA: u32 = 110;
/// Scroll thumb.
const THUMB_W: i32 = 3;
const THUMB_ALPHA: u32 = 50;

/// Where the keyboard focus is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Nav,
    Page,
}

/// A clickable part: a navigation entry, or a row (`None` = the whole row, `Some(j)` = segment,
/// swatch, button or picker part `j`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    Nav(Page),
    Row(usize, Option<usize>),
}

/// Nav `k` -> `k + 1`; `Row(i, part)` -> `1000 + i * 16 + part.map_or(0, |j| j + 1)` (parts 0..=14).
pub(crate) fn hit_id(h: Hit) -> u32 {
    match h {
        Hit::Nav(p) => Page::ALL.iter().position(|q| *q == p).unwrap_or(0) as u32 + 1,
        Hit::Row(i, part) => 1000 + i as u32 * 16 + part.map_or(0, |j| j as u32 + 1),
    }
}

pub(crate) fn decode(id: u32) -> Option<Hit> {
    match id {
        1..=7 => Some(Hit::Nav(Page::ALL[(id - 1) as usize])),
        1000.. => {
            let k = id - 1000;
            let part = k % 16;
            Some(Hit::Row((k / 16) as usize, (part > 0).then(|| part as usize - 1)))
        }
        _ => None,
    }
}

/// A painted bar: premultiplied 0xAARRGGBB, row-major.
pub(crate) struct BarPixels {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

/// Everything a frame shows.
pub(crate) struct Frame<'a> {
    pub page: Page,
    pub rows: &'a [Row],
    /// The live tray list: the bitmaps of the chooser rows, found by key.
    pub tray: &'a [TrayRow],
    pub hot: Option<u32>,
    pub pressed: Option<u32>,
    pub focus: Focus,
    /// Colour picker state (hue 0..360, saturation, value).
    pub hsv: (f32, f32, f32),
    /// The mini bar of the Appearance preview card.
    pub bar: Option<&'a BarPixels>,
    /// Count on the Updates entry (`UpdateStatus::badge`); 0 = none.
    pub badge: u32,
    /// Window background (config `background`).
    pub background: Background,
}

/// Where the parts of a row are (physical px, scrolled).
#[derive(Clone, Default)]
struct Cell {
    /// Left of the label and description.
    text_x: i32,
    /// Top of the label line.
    text_y: i32,
    /// Description lines (wrapped to the room the control leaves).
    desc: Vec<String>,
    /// Vertical centre of the icon.
    icon_cy: i32,
    /// The control's box; for a wrapped row it is on the second line.
    ctl: RECT,
    /// Segments, swatches or buttons, in order (unclipped; a disabled button has no hit).
    parts: Vec<RECT>,
    /// The status text as drawn (ellipsized only when even a line of its own is too narrow).
    status: String,
    /// Lines of a note without icon.
    note: Vec<String>,
}

/// Result of `layout` (physical px, client coordinates).
#[derive(Default)]
pub(crate) struct Geometry {
    pub hits: HitMap,
    pub nav: Vec<(Page, RECT)>,
    pub rows: Vec<RECT>,
    pub viewport: RECT,
    /// Height of the whole page (title to last row plus the bottom gap), unscrolled.
    pub content_h: i32,
    /// The scroll actually applied (clamped).
    pub scroll: i32,
    pub sv: RECT,
    pub hue: RECT,
    pub knob_r: i32,
    /// The preview card (Appearance).
    pub preview: RECT,
    title_y: i32,
    subtitle: Vec<String>,
    cells: Vec<Cell>,
}

/// `scroll` limited to `0..=content_h - viewport_h`.
pub(crate) fn clamp_scroll(scroll: i32, content_h: i32, viewport_h: i32) -> i32 {
    scroll.clamp(0, (content_h - viewport_h).max(0))
}

/// Right end of `label` in `row`.
pub(crate) fn label_right(s: &Surface, row: &RECT, label: &str) -> i32 {
    row.left + s.metrics().px(TEXT_X) + s.text_width(label, Font::Normal)
}

fn seg_widths(s: &Surface, options: &[&str]) -> Vec<i32> {
    let m = s.metrics();
    options.iter().map(|o| (s.text_width(o, Font::Small) + 2 * m.px(SEG_PAD)).max(m.px(SEG_MIN_W))).collect()
}

/// Left end of the segments of a choice right-aligned in `row`.
pub(crate) fn segments_left(s: &Surface, row: &RECT, options: &[&str]) -> i32 {
    row.right - s.metrics().px(CTL_INSET) - seg_widths(s, options).iter().sum::<i32>()
}

/// (saturation, value) at `(x, y)` of the SV area, clamped to 0..=1.
pub(crate) fn sv_at(sv: RECT, x: i32, y: i32) -> (f32, f32) {
    let w = (sv.right - sv.left - 1).max(1) as f32;
    let h = (sv.bottom - sv.top - 1).max(1) as f32;
    (((x - sv.left) as f32 / w).clamp(0.0, 1.0), (1.0 - (y - sv.top) as f32 / h).clamp(0.0, 1.0))
}

/// Hue at abscissa `x` of the hue strip, 0.0..=359.9.
pub(crate) fn hue_at(hue: RECT, x: i32) -> f32 {
    let w = (hue.right - hue.left - 1).max(1) as f32;
    ((x - hue.left) as f32 / w * 360.0).clamp(0.0, 359.9)
}

/// `text` broken at spaces into lines no wider than `max_w`; a word wider than that is split.
fn wrap(s: &Surface, text: &str, font: Font, max_w: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if s.text_width(&cand, font) <= max_w {
            cur = cand;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        let mut rest: Vec<char> = word.chars().collect();
        while rest.len() > 1 && s.text_width(&rest.iter().collect::<String>(), font) > max_w {
            let mut k = rest.len() - 1;
            while k > 1 && s.text_width(&rest[..k].iter().collect::<String>(), font) > max_w {
                k -= 1;
            }
            lines.push(rest[..k].iter().collect());
            rest.drain(..k);
        }
        cur = rest.into_iter().collect();
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn shift(r: RECT, dy: i32) -> RECT {
    RECT { top: r.top - dy, bottom: r.bottom - dy, ..r }
}

fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

/// Widths of the buttons of a row (physical px).
fn button_widths(s: &Surface, buttons: &[settings_pages::Button]) -> Vec<i32> {
    let pad = s.metrics().px(BTN_PAD);
    buttons.iter().map(|b| s.text_width(b.text, Font::Normal) + 2 * pad).collect()
}

/// Buttons flowed into lines of at most `max_w`: `(x offset, line)` per button, and the line count.
fn flow(widths: &[i32], gap: i32, max_w: i32) -> (Vec<(i32, i32)>, i32) {
    let mut out = Vec::with_capacity(widths.len());
    let (mut x, mut line) = (0, 0);
    for &w in widths {
        if x > 0 && x + w > max_w {
            x = 0;
            line += 1;
        }
        out.push((x, line));
        x += w + gap;
    }
    (out, line + 1)
}

/// Lays out the navigation and the page for a `client` of physical px, scrolled by `scroll`
/// (clamped here; the value used is `Geometry::scroll`).
pub(crate) fn layout(s: &Surface, client: (i32, i32), f: &Frame<'_>, scroll: i32) -> Geometry {
    let m = s.metrics();
    let p = |v: i32| m.px(v);
    let (cw, ch) = client;
    let mut g = Geometry { knob_r: p(KNOB_R), ..Geometry::default() };

    // Navigation: the pages from the top, Updates and About from the bottom.
    let nav_w = p(NAV_W);
    let (eh, inset, gap) = (p(NAV_ENTRY_H), p(NAV_INSET), p(NAV_GAP));
    let mut y = p(NAV_TOP);
    for page in Page::ALL.into_iter().filter(|q| !q.at_bottom()) {
        g.nav.push((page, RECT { left: inset, top: y, right: nav_w - inset, bottom: y + eh }));
        y += eh + gap;
    }
    let bottom: Vec<Page> = Page::ALL.into_iter().filter(|q| q.at_bottom()).collect();
    let mut y = ch - inset - (eh + gap) * bottom.len() as i32 + gap;
    for page in bottom {
        g.nav.push((page, RECT { left: inset, top: y, right: nav_w - inset, bottom: y + eh }));
        y += eh + gap;
    }

    // Page: everything is placed unscrolled, then moved up by the clamped scroll.
    let x0 = nav_w + p(PAGE_PAD_X);
    let w = (cw - nav_w - 2 * p(PAGE_PAD_X)).min(p(PAGE_MAX_W)).max(p(120));
    let (ln, lc) = (s.line_height(Font::Normal), s.line_height(Font::Caption));
    g.viewport = RECT { left: nav_w, top: 0, right: cw.max(nav_w), bottom: ch };
    g.title_y = p(PAGE_TOP);
    g.subtitle = wrap(s, f.page.subtitle(), Font::Caption, w);
    let title_h = s.line_height(Font::Title) + p(2) + lc * g.subtitle.len() as i32 + p(18);
    let mut y = g.title_y + title_h.max(p(TITLE_BLOCK));
    let mut hits: Vec<(u32, RECT)> = Vec::new();
    for (i, row) in f.rows.iter().enumerate() {
        let (rc, cell) = layout_row(s, i, row, x0, w, y, &mut g, &mut hits, (ln, lc));
        y = rc.bottom;
        g.rows.push(rc);
        g.cells.push(cell);
    }
    g.content_h = y + p(PAGE_BOTTOM);
    g.scroll = clamp_scroll(scroll, g.content_h, ch);

    let dy = g.scroll;
    g.title_y -= dy;
    for rc in &mut g.rows {
        *rc = shift(*rc, dy);
    }
    for c in &mut g.cells {
        c.text_y -= dy;
        c.icon_cy -= dy;
        c.ctl = shift(c.ctl, dy);
        for r in &mut c.parts {
            *r = shift(*r, dy);
        }
    }
    g.sv = shift(g.sv, dy);
    g.hue = shift(g.hue, dy);
    g.preview = shift(g.preview, dy);
    for (id, r) in hits {
        // Parts scrolled out of the page are not clickable.
        if let Some(r) = intersect(shift(r, dy), g.viewport) {
            g.hits.add(id, r);
        }
    }
    for (page, rc) in &g.nav {
        g.hits.add(hit_id(Hit::Nav(*page)), *rc);
    }
    g
}

/// Lays out row `i` at the top `y` (unscrolled): its rect, the cell, and its clickable parts.
#[allow(clippy::too_many_arguments)] // layout helper: one call site
fn layout_row(
    s: &Surface,
    i: usize,
    row: &Row,
    x0: i32,
    w: i32,
    y: i32,
    g: &mut Geometry,
    hits: &mut Vec<(u32, RECT)>,
    (ln, lc): (i32, i32),
) -> (RECT, Cell) {
    let m = s.metrics();
    let p = |v: i32| m.px(v);
    let right = x0 + w - p(CTL_INSET);
    let id = |part: Option<usize>| hit_id(Hit::Row(i, part));
    let mut cell = Cell::default();
    let full = |h: i32| RECT { left: x0, top: y, right: x0 + w, bottom: y + h };

    match &row.control {
        Control::Header { .. } => {
            let rc = full(p(HEADER_H));
            cell.text_x = x0 + p(ICON_X);
            cell.text_y = rc.bottom - s.line_height(Font::Small) - p(6);
            return (rc, cell);
        }
        Control::Preview => {
            let rc = full(p(PREVIEW_H) + p(8));
            g.preview = RECT { bottom: rc.bottom - p(8), ..rc };
            return (rc, cell);
        }
        Control::Picker => {
            let rc = full(p(PICKER_H));
            let (l, r) = (x0 + p(TEXT_X), right);
            g.sv = RECT { left: l, top: y + p(2), right: r, bottom: y + p(2) + p(SV_H) };
            g.hue = RECT { left: l, top: g.sv.bottom + p(10), right: r, bottom: g.sv.bottom + p(10) + p(HUE_H) };
            cell.ctl = RECT { left: l, top: g.hue.bottom + p(6), right: r, bottom: g.hue.bottom + p(6) + p(HEX_ROW_H) };
            // Grabbable a little beyond the edges (the value is clamped).
            let gr = p(4);
            let grow =
                |r: RECT| RECT { left: r.left - gr, top: r.top - gr, right: r.right + gr, bottom: r.bottom + gr };
            hits.push((id(Some(PART_SV)), grow(g.sv)));
            hits.push((id(Some(PART_HUE)), grow(g.hue)));
            return (rc, cell);
        }
        Control::Note if row.icon.is_none() => {
            cell.text_x = x0 + p(ICON_X);
            cell.note = wrap(s, &row.label, Font::Caption, right - cell.text_x);
            let text_h = lc * cell.note.len() as i32;
            let rc = full((text_h + 2 * p(8)).max(p(NOTE_H)));
            cell.text_y = rc.top + (rc.bottom - rc.top - text_h) / 2;
            return (rc, cell);
        }
        _ => {}
    }

    // A row with a label: icon column, text column, control at the right.
    let tray_row = matches!(row.control, Control::TrayApp { .. });
    let no_label = row.label.is_empty();
    cell.text_x = x0 + p(TEXT_X);
    let label_w = if no_label { 0 } else { s.text_width(&row.label, Font::Normal) };
    let base_h = p(if tray_row { TRAY_ROW_H } else { ROW_H });

    // Control size (width, height) and whether it can stand beside the label.
    let slider_value_w = p(VALUE_W);
    let (ctl_w, ctl_h) = match &row.control {
        Control::Toggle { .. } | Control::TrayApp { .. } => (p(32), p(18)),
        Control::Choice { options, .. } => (seg_widths(s, options).iter().sum(), p(SEG_H)),
        Control::Slider { .. } => (p(SLIDER_MIN_W) + p(8) + slider_value_w, p(SLIDER_BAND_H)),
        Control::Swatches { .. } => (
            (BACKGROUND_PRESETS.len() as i32 + 1) * (p(SWATCH_D) + 2 * p(SWATCH_RING)),
            p(SWATCH_D) + 2 * p(SWATCH_RING),
        ),
        Control::Buttons(b) => {
            let ws = button_widths(s, b);
            (ws.iter().sum::<i32>() + p(BTN_GAP) * (ws.len() as i32 - 1).max(0), p(BTN_H))
        }
        Control::Chooser { .. } => (p(ICON), p(ICON)),
        Control::Status(t) => (s.text_width(t, Font::Normal), ln),
        _ => (0, 0),
    };
    let label_end = cell.text_x + label_w + if no_label { 0 } else { p(LABEL_GAP) };
    let ctl_left = right - ctl_w;
    let desc_inline_w = ctl_left - p(LABEL_GAP) - cell.text_x;
    let fits = if no_label {
        // A row of buttons only: they start at the text column, flowing to more lines if needed.
        false
    } else {
        label_end <= ctl_left && (row.desc.is_none() || desc_inline_w >= p(MIN_DESC_W))
    };
    let desc_w = if fits { desc_inline_w } else { right - cell.text_x };
    cell.desc = row.desc.as_deref().map(|d| wrap(s, d, Font::Caption, desc_w)).unwrap_or_default();
    let text_h =
        if no_label { 0 } else { ln + if cell.desc.is_empty() { 0 } else { p(2) + lc * cell.desc.len() as i32 } };

    let rc;
    if fits {
        rc = full(base_h.max(text_h + 2 * p(ROW_PAD_Y)));
        let cy = (rc.top + rc.bottom) / 2;
        cell.text_y = cy - text_h / 2;
        cell.icon_cy = cy;
        let mut cw = ctl_w;
        if let Control::Slider { .. } = row.control {
            // The track takes what the label leaves, up to its full width.
            cw = (right - label_end - slider_value_w - p(8)).min(p(SLIDER_W)) + p(8) + slider_value_w;
        }
        cell.ctl = RECT { left: right - cw, top: cy - ctl_h / 2, right, bottom: cy - ctl_h / 2 + ctl_h };
    } else if no_label {
        // Buttons only, left-aligned on the text column, as many lines as they need.
        let Control::Buttons(b) = &row.control else { unreachable!("only button rows have no label") };
        let ws = button_widths(s, b);
        let (pos, lines) = flow(&ws, p(BTN_GAP), right - cell.text_x);
        let block = lines * p(BTN_H) + (lines - 1) * p(BTN_GAP);
        rc = full(base_h.max(block + 2 * p(8)));
        let top = rc.top + (rc.bottom - rc.top - block) / 2;
        cell.ctl = RECT { left: cell.text_x, top, right, bottom: top + block };
        for (j, ((x, line), bw)) in pos.iter().zip(&ws).enumerate() {
            let r = rect_wh(cell.text_x + x, top + line * (p(BTN_H) + p(BTN_GAP)), *bw, p(BTN_H));
            cell.parts.push(r);
            if b[j].enabled {
                hits.push((id(Some(j)), r));
            }
        }
        return (rc, cell);
    } else {
        // The control on a second line, right-aligned under the label and the description.
        let band = text_h + 2 * p(ROW_PAD_Y) - p(4);
        rc = full(p(WRAPPED_ROW_H).max(band + ctl_h + p(ROW_PAD_Y) + p(4)));
        cell.text_y = rc.top + p(ROW_PAD_Y);
        cell.icon_cy = cell.text_y + ln / 2;
        let top = rc.bottom - p(ROW_PAD_Y) - ctl_h;
        let left = match row.control {
            // A slider gets the whole text column.
            Control::Slider { .. } => cell.text_x,
            _ => (right - ctl_w).max(cell.text_x),
        };
        cell.ctl = RECT { left, top, right, bottom: top + ctl_h };
    }

    // Clickable parts.
    let ctl = cell.ctl;
    match &row.control {
        Control::Toggle { enabled, .. } => {
            if *enabled {
                hits.push((id(None), rc));
            }
        }
        Control::Chooser { .. } | Control::TrayApp { .. } => hits.push((id(None), rc)),
        Control::Choice { options, .. } => {
            let mut x = ctl.left;
            for (j, sw) in seg_widths(s, options).iter().enumerate() {
                let r = rect_wh(x, ctl.top, *sw, ctl.bottom - ctl.top);
                cell.parts.push(r);
                hits.push((id(Some(j)), r));
                x += sw;
            }
        }
        Control::Slider { .. } => {
            let track = RECT { right: ctl.right - slider_value_w - p(8), ..ctl };
            hits.push((id(Some(0)), RECT { top: track.top - p(6), bottom: track.bottom + p(6), ..track }));
        }
        Control::Swatches { .. } => {
            let d = p(SWATCH_D) + 2 * p(SWATCH_RING);
            let n = BACKGROUND_PRESETS.len() + 1;
            for k in 0..n {
                let r = rect_wh(ctl.right - (n - k) as i32 * d, ctl.top, d, d);
                cell.parts.push(r);
                hits.push((id(Some(k)), r));
            }
        }
        Control::Buttons(b) => {
            let mut x = ctl.right - ctl_w;
            for (j, bw) in button_widths(s, b).iter().enumerate() {
                let r = rect_wh(x, ctl.top, *bw, p(BTN_H));
                cell.parts.push(r);
                if b[j].enabled {
                    hits.push((id(Some(j)), r));
                }
                x += bw + p(BTN_GAP);
            }
        }
        Control::Status(t) => {
            cell.status = if s.text_width(t, Font::Normal) <= right - cell.text_x {
                t.clone()
            } else {
                s.ellipsize(t, Font::Normal, right - cell.text_x)
            };
        }
        _ => {}
    }
    (rc, cell)
}

/// The row of hit id `hot`, if any.
fn hot_row(hot: Option<u32>) -> Option<usize> {
    match hot.and_then(decode) {
        Some(Hit::Row(i, _)) => Some(i),
        _ => None,
    }
}

/// Paints the frame laid out by `layout`. Every pixel of the surface is written.
pub(crate) fn paint(s: &mut Surface, g: &Geometry, f: &Frame<'_>) {
    let m = s.metrics();
    let p = |v: i32| m.px(v);
    let (w, h) = s.size();
    let bg = f.background.rgb();
    s.set_clip(None);
    s.fill_rr(RECT { left: 0, top: 0, right: w, bottom: h }, 0, bg, 255);

    // Navigation column: a faint fill and a hairline at its right edge.
    let nav_w = p(NAV_W);
    s.fill_rr(RECT { left: 0, top: 0, right: nav_w, bottom: h }, 0, WHITE, NAV_FILL_ALPHA);
    let t = p(1).max(1);
    s.fill_rr(RECT { left: nav_w - t, top: 0, right: nav_w, bottom: h }, 0, WHITE, SEP_ALPHA);
    for (page, rc) in &g.nav {
        paint_nav_entry(s, *page, *rc, f);
    }

    // Page.
    s.set_clip(Some(g.viewport));
    let x0 = g.rows.first().map_or(nav_w + p(PAGE_PAD_X), |r| r.left);
    s.text(f.page.title(), x0, g.title_y, Font::Title, WHITE, TEXT_ALPHA);
    let mut sy = g.title_y + s.line_height(Font::Title) + p(2);
    for line in &g.subtitle {
        s.text(line, x0, sy, Font::Caption, WHITE, TEXT2_ALPHA);
        sy += s.line_height(Font::Caption);
    }
    let hot_i = hot_row(f.hot);
    for (i, row) in f.rows.iter().enumerate() {
        let (Some(rc), Some(cell)) = (g.rows.get(i), g.cells.get(i)) else { continue };
        if rc.bottom <= g.viewport.top || rc.top >= g.viewport.bottom {
            continue;
        }
        paint_row(s, g, f, i, row, *rc, cell, hot_i == Some(i));
    }

    // Scroll thumb at the right edge when the page is taller than the window.
    let vh = g.viewport.bottom - g.viewport.top;
    if g.content_h > vh && vh > 0 {
        let th = (vh * vh / g.content_h).max(p(24));
        let ty = g.scroll * (vh - th) / (g.content_h - vh).max(1);
        let tw = p(THUMB_W);
        let x = g.viewport.right - tw - p(3);
        s.fill_rr(rect_wh(x, g.viewport.top + ty, tw, th), tw / 2, WHITE, THUMB_ALPHA);
    }
    s.set_clip(None);
}

fn paint_nav_entry(s: &mut Surface, page: Page, rc: RECT, f: &Frame<'_>) {
    let m = s.metrics();
    let p = |v: i32| m.px(v);
    let id = hit_id(Hit::Nav(page));
    let current = page == f.page;
    let hot = f.hot == Some(id);
    if current || hot {
        let a = if f.pressed == Some(id) { PRESS_ALPHA } else { HOVER_ALPHA };
        s.plate(rc, p(ROW_RADIUS), a);
    }
    if current {
        // The 3 px white bar on the left edge of the current entry.
        s.fill_rr(
            RECT { left: rc.left, top: rc.top + p(10), right: rc.left + p(3), bottom: rc.bottom - p(10) },
            p(1),
            WHITE,
            255,
        );
    }
    let a = if current { lift(TEXT_ALPHA, true) } else { lift(TEXT2_ALPHA, hot) };
    let icon = p(ICON);
    let shape = page.icon();
    s.glyph(rc.left + p(12), rc.top + (rc.bottom - rc.top - icon) / 2, icon, a, &shape);
    let font = if current { Font::Bold } else { Font::Normal };
    let lx = rc.left + p(40);
    let mut room = rc.right - p(8) - lx;
    let badge = if page == Page::Updates { crate::popup_power::settings_badge(f.badge) } else { None };
    if let Some(text) = &badge {
        let right = p(NAV_W) - p(16);
        crate::popup_power::paint_badge(s, right, &rc, text);
        room = right - s.text_width(text, Font::Small).max(p(8)) - p(16) - lx;
    }
    let label = s.ellipsize(page.title(), font, room);
    s.text(&label, lx, center_y(&rc, s.line_height(font)), font, WHITE, a);
    if current && f.focus == Focus::Nav {
        s.stroke_rr(rc, p(ROW_RADIUS), p(1).max(1), FOCUS_ALPHA);
    }
}

/// True for the rows that react to the pointer as a whole or through their parts.
fn interactive(c: &Control) -> bool {
    match c {
        Control::Toggle { enabled, .. } => *enabled,
        Control::Choice { .. }
        | Control::Slider { .. }
        | Control::Swatches { .. }
        | Control::Buttons(_)
        | Control::Chooser { .. }
        | Control::TrayApp { .. } => true,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)] // paint helper: one call site
fn paint_row(s: &mut Surface, g: &Geometry, f: &Frame<'_>, i: usize, row: &Row, rc: RECT, cell: &Cell, row_hot: bool) {
    let m = s.metrics();
    let p = |v: i32| m.px(v);
    let id = |part: Option<usize>| hit_id(Hit::Row(i, part));
    let lsm = s.line_height(Font::Small);
    let ln = s.line_height(Font::Normal);
    let right = rc.right - p(CTL_INSET);

    match &row.control {
        Control::Header { value } => {
            // Uppercase, letter-spaced, dim; the value (current colour name) at the right.
            let mut x = cell.text_x;
            let extra = p(1);
            for ch in row.label.to_uppercase().chars() {
                let c = ch.to_string();
                s.text(&c, x, cell.text_y, Font::Small, WHITE, TEXT3_ALPHA);
                x += s.text_width(&c, Font::Small) + extra;
            }
            if let Some(v) = value {
                let vw = s.text_width(v, Font::Small);
                s.text(v, right - vw, cell.text_y, Font::Small, WHITE, TEXT2_ALPHA);
            }
            return;
        }
        Control::Preview => {
            paint_preview(s, g.preview, f.bar);
            return;
        }
        Control::Picker => {
            paint_picker(s, g, f);
            let _ = cell;
            return;
        }
        Control::Note if row.icon.is_none() => {
            let mut y = cell.text_y;
            for line in &cell.note {
                s.text(line, cell.text_x, y, Font::Caption, WHITE, TEXT2_ALPHA);
                y += s.line_height(Font::Caption);
            }
            return;
        }
        _ => {}
    }

    // Hover plate on the whole row of an interactive one (pressed: only for a whole-row click).
    let enabled = !matches!(row.control, Control::Toggle { enabled: false, .. });
    let lit = row_hot && interactive(&row.control);
    if lit {
        let a = if f.pressed.is_some() && f.pressed == f.hot && f.hot == Some(id(None)) {
            PRESS_ALPHA
        } else {
            HOVER_ALPHA
        };
        s.plate(rc, p(ROW_RADIUS), a);
    }

    // Icon (or the tray app's own bitmap), label, description.
    let icon = p(ICON);
    let ix = rc.left + p(ICON_X) + (p(20) - icon) / 2;
    let iy = cell.icon_cy - icon / 2;
    if let Control::TrayApp { key, .. } = &row.control {
        if let Some(bmp) = f.tray.iter().find(|t| t.key == *key).and_then(|t| t.bitmap.as_ref()) {
            s.blit(bmp, ix, iy, icon, 255);
        }
    } else if let Some(shape) = row.icon.filter(|_| !row.label.is_empty()) {
        let a = if enabled { lift(TEXT2_ALPHA, lit) } else { TEXT3_ALPHA };
        s.glyph(ix, iy, icon, a, &shape);
    }
    if !row.label.is_empty() {
        let a = if enabled { lift(TEXT_ALPHA, lit) } else { TEXT3_ALPHA };
        let label = s.ellipsize(&row.label, Font::Normal, right - cell.text_x);
        s.text(&label, cell.text_x, cell.text_y, Font::Normal, WHITE, a);
        let mut y = cell.text_y + ln + p(2);
        for line in &cell.desc {
            s.text(line, cell.text_x, y, Font::Caption, WHITE, TEXT3_ALPHA);
            y += s.line_height(Font::Caption);
        }
    }

    let ctl = cell.ctl;
    match &row.control {
        Control::Toggle { on, .. } | Control::TrayApp { visible: on, .. } => {
            let (sw, sh) = (p(32), p(18));
            let (x, y) = (ctl.right - sw, (ctl.top + ctl.bottom) / 2 - sh / 2);
            toggle_switch(s, x, y, *on);
            if !enabled {
                // Unavailable: the switch fades into the background.
                s.fill_rr(rect_wh(x - 1, y - 1, sw + 2, sh + 2), sh / 2, f.background.rgb(), 150);
            }
        }
        Control::Choice { options, selected, .. } => {
            let (Some(first), Some(last)) = (cell.parts.first(), cell.parts.last()) else { return };
            let track = RECT { left: first.left, right: last.right, ..*first };
            let r = (track.bottom - track.top) / 2;
            s.fill_rr(track, r, WHITE, SEG_TRACK_ALPHA);
            s.stroke_rr(track, r, p(1).max(1), SEG_RIM_ALPHA);
            for (j, (opt, seg)) in options.iter().zip(&cell.parts).enumerate() {
                let seg = *seg;
                let ty = center_y(&seg, lsm);
                let tx = seg.left + (seg.right - seg.left - s.text_width(opt, Font::Small)) / 2;
                if *selected == Some(j) {
                    s.fill_rr(seg, r, WHITE, 235);
                    s.text(opt, tx, ty, Font::Small, (0, 0, 0), 255);
                } else {
                    let hot = f.hot == Some(id(Some(j)));
                    if hot {
                        let a = if f.pressed == f.hot { PRESS_ALPHA } else { SEG_HOT_ALPHA };
                        s.fill_rr(seg, r, WHITE, a);
                    }
                    s.text(opt, tx, ty, Font::Small, WHITE, lift(TEXT2_ALPHA, hot));
                }
            }
        }
        Control::Slider { value, min, max, unit, .. } => {
            let hot = f.hot == Some(id(Some(0)));
            let track = RECT { right: ctl.right - p(VALUE_W) - p(8), ..ctl };
            paint_slider(s, &track, g.knob_r, *min, *max, *value);
            let val = format!("{value}{unit}");
            let vw = s.text_width(&val, Font::Normal);
            s.text(&val, ctl.right - vw, center_y(&ctl, ln), Font::Normal, WHITE, lift(TEXT2_ALPHA, hot));
        }
        Control::Swatches { selected, .. } => paint_swatches(s, f, i, *selected, &cell.parts),
        Control::Buttons(buttons) => {
            for (j, (b, r)) in buttons.iter().zip(&cell.parts).enumerate() {
                let r = *r;
                let hid = id(Some(j));
                let (fill, ta) = if !b.enabled {
                    (BTN_OFF_ALPHA, TEXT3_ALPHA)
                } else if f.hot == Some(hid) {
                    (if f.pressed == Some(hid) { BTN_PRESS_ALPHA } else { BTN_HOT_ALPHA }, lift(TEXT_ALPHA, true))
                } else {
                    (BTN_ALPHA, TEXT_ALPHA)
                };
                let rad = (r.bottom - r.top) / 2;
                s.fill_rr(r, rad, WHITE, fill);
                s.stroke_rr(r, rad, p(1).max(1), BTN_RIM_ALPHA);
                let tw = s.text_width(b.text, Font::Normal);
                s.text(b.text, r.left + (r.right - r.left - tw) / 2, center_y(&r, ln), Font::Normal, WHITE, ta);
            }
        }
        Control::Chooser { open } => {
            let shape: fn(f32, f32) -> bool =
                if *open { glyphs::chevron_down_shape } else { glyphs::chevron_right_shape };
            let gy = (ctl.top + ctl.bottom) / 2 - icon / 2;
            s.glyph(ctl.right - icon, gy, icon, lift(TEXT2_ALPHA, lit), &shape);
        }
        Control::Status(_) => {
            let tw = s.text_width(&cell.status, Font::Normal);
            s.text(&cell.status, ctl.right - tw, center_y(&ctl, ln), Font::Normal, WHITE, TEXT2_ALPHA);
        }
        _ => {}
    }
}

fn paint_preview(s: &mut Surface, card: RECT, bar: Option<&BarPixels>) {
    let m = s.metrics();
    let (c0, c1) = ((0x2a, 0x2f, 0x3a), (0x10, 0x13, 0x1a));
    let h = (card.bottom - card.top - 1).max(1) as f32;
    let top = card.top;
    s.fill_fn(card, m.px(10), &|_, y| {
        let t = ((y - top) as f32 / h).clamp(0.0, 1.0);
        let mix = |a: u32, b: u32| (a as f32 + (b as f32 - a as f32) * t + 0.5) as u32;
        (mix(c0.0, c1.0), mix(c0.1, c1.1), mix(c0.2, c1.2))
    });
    if let Some(b) = bar {
        let x = card.left + (card.right - card.left - b.w) / 2;
        let y = card.top + (card.bottom - card.top - b.h) / 2;
        s.blend_pixels(x, y, b.w, b.h, &b.px);
    }
}

fn paint_swatches(s: &mut Surface, f: &Frame<'_>, i: usize, selected: usize, parts: &[RECT]) {
    let m = s.metrics();
    let bw = m.px(1).max(1);
    for (k, hit) in parts.iter().copied().enumerate() {
        let id = hit_id(Hit::Row(i, Some(k)));
        let gr = m.px(SWATCH_RING);
        let rc = RECT { left: hit.left + gr, top: hit.top + gr, right: hit.right - gr, bottom: hit.bottom - gr };
        let d = rc.right - rc.left;
        if let Some((_, p)) = BACKGROUND_PRESETS.get(k) {
            s.fill_rr(rc, d / 2, p.rgb(), 255);
        } else {
            // Custom: hue wheel around the centre, white towards the middle.
            let (cx, cy) = (rc.left as f32 + d as f32 / 2.0, rc.top as f32 + d as f32 / 2.0);
            let rad = d as f32 / 2.0;
            s.fill_fn(rc, d / 2, &|x, y| {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let h = dy.atan2(dx).to_degrees() + 90.0;
                let sat = ((dx * dx + dy * dy).sqrt() / rad).clamp(0.0, 1.0).powf(0.6);
                hsv_to_rgb(h, sat, 1.0)
            });
        }
        s.stroke_rr(rc, d / 2, bw, SWATCH_RIM_ALPHA);
        // Selection ring outside the swatch; a dimmer one on hover.
        let ring = if selected == k {
            235
        } else if f.hot == Some(id) {
            90
        } else {
            0
        };
        s.stroke_rr(hit, (hit.right - hit.left) / 2, m.px(2).max(1), ring);
    }
}

fn paint_picker(s: &mut Surface, g: &Geometry, f: &Frame<'_>) {
    let m = s.metrics();
    let (h, sat, val) = f.hsv;
    let (sv, hue) = (g.sv, g.hue);
    let (sw, sh) = ((sv.right - sv.left - 1).max(1) as f32, (sv.bottom - sv.top - 1).max(1) as f32);
    s.fill_fn(sv, m.px(6), &|x, y| hsv_to_rgb(h, (x - sv.left) as f32 / sw, 1.0 - (y - sv.top) as f32 / sh));
    let hw = (hue.right - hue.left - 1).max(1) as f32;
    s.fill_fn(hue, (hue.bottom - hue.top) / 2, &|x, _| hsv_to_rgb((x - hue.left) as f32 / hw * 360.0, 1.0, 1.0));
    // Knobs: dark halo, white ring, the colour inside (readable on light and dark areas).
    let knob = |s: &mut Surface, x: i32, y: i32, r: i32, rgb: (u32, u32, u32)| {
        let c = |r: i32| RECT { left: x - r, top: y - r, right: x + r, bottom: y + r };
        s.fill_rr(c(r + m.px(1).max(1)), r + 1, (0, 0, 0), 110);
        s.fill_rr(c(r), r, WHITE, 255);
        let ri = r - m.px(2).max(1);
        s.fill_rr(c(ri), ri, rgb, 255);
    };
    let kx = sv.left + (sat * sw + 0.5) as i32;
    let ky = sv.top + ((1.0 - val) * sh + 0.5) as i32;
    knob(s, kx, ky, m.px(7), hsv_to_rgb(h, sat, val));
    let hx = hue.left + (h.rem_euclid(360.0) / 360.0 * hw + 0.5) as i32;
    knob(s, hx, (hue.top + hue.bottom) / 2, m.px(8), hsv_to_rgb(h, 1.0, 1.0));
    // The current colour and its hex code under the strip.
    let hex_row =
        RECT { left: sv.left, top: hue.bottom + m.px(6), right: sv.right, bottom: hue.bottom + m.px(6 + HEX_ROW_H) };
    let pv = rect_wh(hex_row.left, center_y(&hex_row, m.px(20)), m.px(36), m.px(20));
    s.fill_rr(pv, m.px(5), f.background.rgb(), 255);
    s.stroke_rr(pv, m.px(5), m.px(1).max(1), SWATCH_RIM_ALPHA);
    let lh = s.line_height(Font::Normal);
    s.text(&f.background.hex(), pv.right + m.px(10), center_y(&pv, lh), Font::Normal, WHITE, TEXT_ALPHA);
}

// ------------------------------------------------------------------ mini bar and preview

/// The mini live bar of the Appearance page: three sample apps, volume and clock, drawn by the bar's
/// own renderer at the config's sizes. The renderer reads the display mode, background and opacity
/// from the global config (`config::get`), which is the edited one while the window is open.
pub(crate) fn mini_bar(m: Metrics, c: &Config, max_w: i32) -> Option<BarPixels> {
    let sizes = BarSizes::from_config(c);
    let ip = sizes.icon_px(m);
    let icons = [
        crate::preview::mock_icon('F', 0xE8A317, ip),
        crate::preview::mock_icon('B', 0x0F6CBD, ip),
        crate::preview::mock_icon('M', 0xD44638, ip),
    ];
    let app = |name, icon, indicator, dimmed| AppView {
        name,
        icon: Some(icon),
        indicator,
        dimmed,
        active_t: if indicator == Indicator::Active { 1.0 } else { 0.0 },
    };
    let apps = [
        app(tr("Files", "File"), &icons[0], Indicator::Active, false),
        app("Browser", &icons[1], Indicator::Open, false),
        app("Mail", &icons[2], Indicator::None, true),
    ];
    let modules = [ItemId::Volume, ItemId::Clock];
    let time = if c.show_seconds { "14:32:05" } else { "14:32" };
    let scene = Scene {
        metrics: m,
        sizes,
        apps: &apps,
        tray: &[],
        overflow_enabled: false,
        force_chevron: false,
        bar_top: c.position == Position::Top,
        system: SystemView {
            volume: VolumeState { level: 60, muted: false, available: true },
            show_volume_percent: c.show_volume_percent,
            time,
            date: c.show_date.then(|| tr("Fri 23 Oct", "ven 23 ott")),
            bell_badge: false,
            bell_dnd: false,
            net: Default::default(),
            usage: Default::default(),
            battery: BatteryView::default(),
        },
        plate: None,
        modules: &modules,
        start_button: false,
        lift: None,
        hover: None,
        max_width: max_w,
    };
    let (w, h, px) = render::paint_offscreen(&scene)?;
    Some(BarPixels { w, h, px })
}

/// The window as `--preview` shows it: a client of 860 x 600 logical px under a fake caption, with a
/// 1 px border; navigation focus, nothing hovered.
pub(crate) fn preview_pixels(m: Metrics, page: Page, c: &Config, ctx: &Ctx<'_>) -> Option<(i32, i32, Vec<u32>)> {
    window_pixels(m, page, c, ctx, None, Focus::Nav, (860, 600))
}

/// `preview_pixels` with hit id `hot` under the pointer, the focus on `focus` and a client of
/// `size` logical px.
pub(crate) fn window_pixels(
    m: Metrics,
    page: Page,
    c: &Config,
    ctx: &Ctx<'_>,
    hot: Option<u32>,
    focus: Focus,
    size: (i32, i32),
) -> Option<(i32, i32, Vec<u32>)> {
    let (w, h, cap) = (m.px(size.0), m.px(size.1), m.px(CAPTION_H));
    let rows = settings_pages::rows(page, c, ctx);
    let mut s = Surface::new(m).ok()?;
    s.resize(w, h).ok()?;
    let mut f = Frame {
        page,
        rows: &rows,
        tray: ctx.tray,
        hot,
        pressed: None,
        focus,
        hsv: settings_pages::rgb_to_hsv(c.background.rgb()),
        bar: None,
        badge: ctx.update.badge(),
        background: c.background,
    };
    let g = layout(&s, (w, h), &f, 0);
    let bar = if g.preview.right > g.preview.left {
        mini_bar(m, c, g.preview.right - g.preview.left - 2 * m.px(16))
    } else {
        None
    };
    f.bar = bar.as_ref();
    paint(&mut s, &g, &f);
    let (_, _, client) = s.snapshot_pixels()?;

    // Caption: the background colour, the title at the left and the close glyph at the right.
    let mut cs = Surface::new(m).ok()?;
    cs.resize(w, cap).ok()?;
    cs.fill_rr(RECT { left: 0, top: 0, right: w, bottom: cap }, 0, c.background.rgb(), 255);
    let title = tr("Dock settings", "Impostazioni dock");
    cs.text(title, m.px(12), (cap - cs.line_height(Font::Normal)) / 2, Font::Normal, WHITE, TEXT_ALPHA);
    let gs = m.px(ICON);
    cs.glyph(w - m.px(23) - gs / 2, (cap - gs) / 2, gs, TEXT2_ALPHA, &glyphs::close_shape);
    let (_, _, caption) = cs.snapshot_pixels()?;

    let mut out = caption;
    out.extend_from_slice(&client);
    let th = h + cap;
    let border = white(PANEL_BORDER_ALPHA);
    for y in 0..th {
        for x in 0..w {
            if x == 0 || y == 0 || x == w - 1 || y == th - 1 {
                let i = (y * w + x) as usize;
                out[i] = over(out[i], border);
            }
        }
    }
    Some((w, th, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_pages::{Memory, Setting};
    use crate::start_import;
    use crate::update::{Outcome, Phase, UpdateStatus};
    use crate::update_release::Version;

    const M: Metrics = Metrics { dpi: 96 };

    fn frame<'a>(page: Page, rows: &'a [Row]) -> Frame<'a> {
        Frame {
            page,
            rows,
            tray: &[],
            hot: None,
            pressed: None,
            focus: Focus::Nav,
            hsv: (0.0, 0.0, 0.0),
            bar: None,
            badge: 0,
            background: Config::default().background,
        }
    }

    fn idle_status() -> UpdateStatus {
        UpdateStatus {
            installed: Version { major: 1, minor: 4, patch: 0 },
            phase: Phase::Idle,
            available: None,
            last_check: String::new(),
            outcome: Outcome::None,
            staged: false,
        }
    }

    fn test_ctx(st: &UpdateStatus) -> Ctx<'_> {
        Ctx {
            screens: &[],
            picker_open: false,
            chooser_open: false,
            tray: &[],
            import: start_import::Status::Idle,
            update: st,
            memory: Memory::default(),
            data_dir: None,
            log_path: None,
        }
    }

    #[test]
    fn hit_ids_roundtrip() {
        for p in Page::ALL {
            assert_eq!(decode(hit_id(Hit::Nav(p))), Some(Hit::Nav(p)));
        }
        for h in [Hit::Row(0, None), Hit::Row(12, Some(7)), Hit::Row(3, Some(14))] {
            assert_eq!(decode(hit_id(h)), Some(h));
        }
        assert_eq!(decode(0), None);
    }

    #[test]
    fn layout_places_nav_and_rows() {
        let s = Surface::new(M).expect("surface");
        let st = idle_status();
        let rows = settings_pages::rows(Page::Appearance, &Config::default(), &test_ctx(&st));
        let g = layout(&s, (860, 600), &frame(Page::Appearance, &rows), 0);
        assert_eq!(g.nav.len(), 7);
        let about = g.nav.iter().find(|(p, _)| *p == Page::About).unwrap().1;
        assert_eq!(about.bottom, 600 - 8);
        assert!(g.nav.iter().all(|(_, r)| r.right <= NAV_W));
        assert_eq!(g.rows.len(), rows.len());
        for w in g.rows.windows(2) {
            assert!(w[0].bottom <= w[1].top);
        }
        for (r, rc) in rows.iter().zip(&g.rows) {
            assert!(rc.left >= NAV_W && rc.right - rc.left <= PAGE_MAX_W);
            if matches!(r.control, Control::Toggle { .. } | Control::Slider { .. } | Control::Buttons(_)) {
                assert_eq!(rc.bottom - rc.top, ROW_H);
            }
        }
        assert_eq!(g.hits.at(about.left + 2, about.top + 2), Some(hit_id(Hit::Nav(Page::About))));
    }

    #[test]
    fn choices_never_cover_their_label() {
        let s = Surface::new(M).expect("surface");
        let row = RECT { left: NAV_W + 32, top: 0, right: 640 - 32, bottom: ROW_H }; // minimum window width
                                                                                     // The widest labels in both languages, with their options as they are drawn.
        for (label, opts) in [
            ("Display mode", vec!["Icon + name", "Icon", "Name"]),
            ("Mostra", vec!["Icona + nome", "Icona", "Nome"]),
            ("Start layout", vec!["Vertical", "Horizontal"]),
            ("Layout Start", vec!["Verticale", "Orizzontale"]),
            ("Mostra batteria", vec!["Automatica", "S\u{ec}", "No"]),
            ("Lingua", vec!["Sistema", "English", "Italiano"]),
            ("Posizione", vec!["In alto", "In basso"]),
        ] {
            assert!(label_right(&s, &row, label) + 6 <= segments_left(&s, &row, &opts), "{label} does not fit inline");
        }
        // Six displays + Pointer do not fit inline: the row wraps, segments stay inside it, below the label.
        let six: Vec<&'static str> = vec!["Principale", "2", "3", "4", "5", "6", "Puntatore"];
        assert!(label_right(&s, &row, "Schermo") + 6 > segments_left(&s, &row, &six));
        let ctl = Control::Choice { setting: Setting::Monitor, options: six.clone(), selected: Some(0) };
        let rows =
            [Row { icon: Some(crate::glyphs::monitor_shape), label: "Schermo".into(), desc: None, control: ctl }];
        let g = layout(&s, (640, 460), &frame(Page::Screen, &rows), 0);
        let rc = g.rows[0];
        assert_eq!(rc.bottom - rc.top, WRAPPED_ROW_H);
        for j in 0..six.len() {
            let seg = g.hits.rect(hit_id(Hit::Row(0, Some(j)))).unwrap();
            assert!(seg.left >= rc.left && seg.right <= rc.right && seg.top >= rc.top + ROW_H / 2);
        }
    }

    #[test]
    fn current_nav_entry_is_marked() {
        let mut s = Surface::new(M).expect("surface");
        s.resize(860, 600).unwrap();
        let st = idle_status();
        let rows = settings_pages::rows(Page::Bar, &Config::default(), &test_ctx(&st));
        let f = Frame { focus: Focus::Page, ..frame(Page::Bar, &rows) }; // no focus ring in the way
        let g = layout(&s, (860, 600), &f, 0);
        paint(&mut s, &g, &f);
        let (w, _, px) = s.snapshot_pixels().unwrap();
        let at = |x: i32, y: i32| px[(y * w + x) as usize];
        let nav = |p: Page| g.nav.iter().find(|(q, _)| *q == p).unwrap().1;
        let (cur, other) = (nav(Page::Bar), nav(Page::Appearance));
        let cy = |r: RECT| (r.top + r.bottom) / 2;
        assert_eq!(at(cur.left + 1, cy(cur)), 0xFFFF_FFFF); // the 3 px white bar
        assert_ne!(at(other.left + 1, cy(other)), 0xFFFF_FFFF);
        assert!(at(cur.right - 6, cy(cur)) & 0xFF > at(other.right - 6, cy(other)) & 0xFF);
        // the hover fill, not hot
    }

    #[test]
    fn scroll_and_picker_mapping() {
        assert_eq!(clamp_scroll(-5, 1000, 400), 0);
        assert_eq!(clamp_scroll(900, 1000, 400), 600);
        assert_eq!(clamp_scroll(50, 300, 400), 0);
        let sv = rect_wh(10, 10, 101, 101);
        assert_eq!(sv_at(sv, 10, 10), (0.0, 1.0));
        assert_eq!(sv_at(sv, 110, 110), (1.0, 0.0));
        assert_eq!(sv_at(sv, 500, -50), (1.0, 1.0));
        let hue = rect_wh(0, 0, 361, 12);
        assert_eq!(hue_at(hue, -5), 0.0);
        assert!((hue_at(hue, 360) - 359.9).abs() < 0.01);
    }

    #[test]
    fn preview_pixels_have_the_window_size() {
        let st = idle_status();
        let c = Config::default();
        for (dpi, w, h) in [(96, 860, 632), (144, 1290, 948)] {
            let (pw, ph, px) = preview_pixels(Metrics { dpi }, Page::Appearance, &c, &test_ctx(&st)).expect("pixels");
            assert_eq!((pw, ph), (w, h));
            assert_eq!(px.len(), (w * h) as usize);
        }
        let bar = mini_bar(M, &c, 600).expect("mini bar");
        assert_eq!(bar.h, crate::render::bar_height(M, crate::render::BarSizes::from_config(&c)));
    }
}
