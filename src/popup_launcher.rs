//! The launcher popup (SPEC-launcher 5.2, 5.3 and 5.5): the content the popup host shows for `PopupKind::Launcher`.
//! L1a-13 added the stub; L1a-14 (this file) is the view: layout and painting of the field, the mode chip, the result rows,
//! the detail and review lines, the hint and the scrollbar, all from the `Session` the content holds. L1a-15 adds the
//! controller: the chord flow from the hook's post to an open launcher (or to nothing, with the type-ahead ended), which
//! reaches the hook and Windows through two small ports, `HookPort` and `SysPort` (real in launcher_sys.rs, recorders
//! in the tests).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::KillTimer;

use super::{center_y, Content, HitMap, PopupKind, Reply};
use crate::glyphs;
use crate::hook::{self, KeyRec, PassFlags, PostAction, RING_CAP};
use crate::icons::IconBitmap;
use crate::launcher::catalog::CatApp;
use crate::launcher::field::KeyEvent;
use crate::launcher::gesture;
use crate::launcher::model::{
    GlyphId, IconRef, Item, Kind, Mods, NumberStyle, Payload, ProviderId, SettingRow, Verb, WindowRow,
};
use crate::launcher::providers::{Context, LauncherOpts};
use crate::launcher::runline;
use crate::launcher::session::{Effect, ReplayKey, Session};
use crate::launcher::strings;
use crate::launcher::style;
use crate::launcher::verbs;
use crate::launcher_sys::{RealHook, RealSys};
use crate::log;
use crate::render::Metrics;
use crate::surface::{rect_wh, Font, Surface, WHITE};

/// Height of the empty panel, the field alone (logical px, SPEC 5.2.3).
const EMPTY_H: i32 = 62;
/// Height of the mode chip (logical px).
const CHIP_H: i32 = 24;
/// Horizontal padding of the mode chip.
const CHIP_PAD: i32 = style::S_3;
/// Gap between the chip and the query.
const CHIP_GAP: i32 = 8;
/// Height of the caret (logical px, SPEC 5.3).
const CARET_H: i32 = 18;
/// Distance of the name from the top of a row that has a detail line, and of the detail from the name.
const NAME_TOP: i32 = 4;
const DETAIL_GAP: i32 = 2;
/// The running indicator: 8 x 2, 3 px under the icon.
const PILL_W: i32 = 8;
const PILL_H: i32 = 2;
const PILL_GAP: i32 = 3;
/// Space between a long name and the hint (logical px).
const HINT_GAP: i32 = 12;
/// A hidden-scrollbar countdown in ticks of the popup's 1 s timer: the bar shows for 1 to 2 s after a scroll.
const SCROLLBAR_TICKS: u8 = 2;
/// Wheel notches move this many rows.
const WHEEL_ROWS: usize = 1;
/// The review and a notice show at most this many lines.
const MAX_LINES: usize = style::MAX_REVIEW_LINES;

const ID_FIELD: u32 = 1;
const ID_ROW: u32 = 100;

// ---------------------------------------------------------------- text runs and markers

/// A piece of text drawn in one style: plain, or a marker (SPEC 5.3: a control or bidi character shown visibly).
#[derive(Clone, PartialEq, Eq, Debug)]
struct Run {
    text: String,
    marker: bool,
}

/// The width of each character of `text` as it is drawn (a marker by its display text).
fn char_widths(s: &Surface, text: &str) -> Vec<i32> {
    text.chars().map(|c| s.text_width(&unit(c).text, Font::Normal)).collect()
}

/// The width of the characters of `text` before the byte offset `at`.
fn prefix_width(widths: &[i32], text: &str, at: usize) -> i32 {
    let n = text.get(..at).map_or(0, |t| t.chars().count());
    widths.iter().take(n).sum()
}

/// How a marker character is drawn: control pictures for C0 and DEL, "<U+XXXX>" for the rest.
fn marker_text(c: char) -> Option<String> {
    if !gesture::is_marker(c) {
        return None;
    }
    let u = u32::from(c);
    Some(match u {
        0..=0x1F => char::from_u32(0x2400 + u).map_or_else(|| format!("<U+{u:04X}>"), String::from),
        0x7F => String::from('\u{2421}'),
        _ => format!("<U+{u:04X}>"),
    })
}

/// One character as a run of its own.
fn unit(c: char) -> Run {
    match marker_text(c) {
        Some(text) => Run { text, marker: true },
        None => Run { text: c.to_string(), marker: false },
    }
}

/// Appends a run, merging it into the last one of the same kind.
fn push_run(runs: &mut Vec<Run>, r: Run) {
    match runs.last_mut() {
        Some(last) if last.marker == r.marker => last.text.push_str(&r.text),
        _ => runs.push(r),
    }
}

/// The text as runs: markers apart from the rest.
fn runs_of(text: &str) -> Vec<Run> {
    let mut out = Vec::new();
    for c in text.chars() {
        push_run(&mut out, unit(c));
    }
    out
}

/// Draws the runs from `x` and returns the x after the last. Markers are FG_SECONDARY with a 2 px ACCENT_ATTENTION
/// underline.
fn draw_runs(s: &mut Surface, runs: &[Run], mut x: i32, y: i32, font: Font, alpha: u32) -> i32 {
    let m = s.metrics();
    let lh = s.line_height(font);
    for r in runs {
        let w = s.text_width(&r.text, font);
        if r.marker {
            s.text(&r.text, x, y, font, WHITE, style::FG_SECONDARY);
            let bar = m.px(2).max(1);
            s.fill_rr(rect_wh(x, y + lh - bar, w, bar), 0, style::ACCENT_ATTENTION, 255);
        } else {
            s.text(&r.text, x, y, font, WHITE, alpha);
        }
        x += w;
    }
    x
}

/// Draws `text` on one line in `max_w` px: ellipsized at the end, or clipped when it holds markers.
fn draw_line(s: &mut Surface, text: &str, x: i32, y: i32, max_w: i32, font: Font, alpha: u32) {
    if text.chars().any(gesture::is_marker) {
        let lh = s.line_height(font);
        s.set_clip(Some(rect_wh(x, y, max_w.max(0), lh)));
        draw_runs(s, &runs_of(text), x, y, font, alpha);
        s.set_clip(None);
    } else {
        let shown = s.ellipsize(text, font, max_w);
        s.text(&shown, x, y, font, WHITE, alpha);
    }
}

/// `text` cut in the middle with "..." so that it fits `max_w` (a path keeps its end).
fn ellipsize_middle(s: &Surface, text: &str, font: Font, max_w: i32) -> String {
    if s.text_width(text, font) <= max_w {
        return text.to_string();
    }
    let n = text.chars().count();
    let build = |keep: usize| -> String {
        let (head, tail) = (keep.div_ceil(2), keep / 2);
        let mut out: String = text.chars().take(head).collect();
        out.push('\u{2026}');
        out.extend(text.chars().skip(n.saturating_sub(tail)));
        out
    };
    let (mut lo, mut hi) = (0usize, n.saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if s.text_width(&build(mid), font) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

/// `text` wrapped over lines of `max_w` px: at the last space that fits, else inside a word. Stops one line past
/// `max_lines`, so the caller can tell a text that does not fit from one that does.
fn wrap(s: &Surface, text: &str, font: Font, max_w: i32, max_lines: usize) -> Vec<Vec<Run>> {
    let mut lines: Vec<Vec<Run>> = Vec::new();
    let mut cur: Vec<Run> = Vec::new();
    let mut cur_w = 0;
    let mut last_break: Option<usize> = None;
    let flush = |units: Vec<Run>| -> Vec<Run> {
        let mut merged = Vec::new();
        for u in units {
            push_run(&mut merged, u);
        }
        merged
    };
    for c in text.chars() {
        let u = unit(c);
        let w = s.text_width(&u.text, font);
        if cur_w + w > max_w && !cur.is_empty() {
            let split = last_break.unwrap_or(cur.len());
            let rest = cur.split_off(split);
            lines.push(flush(cur));
            if lines.len() > max_lines {
                return lines;
            }
            cur = rest;
            cur_w = cur.iter().map(|r| s.text_width(&r.text, font)).sum();
            last_break = None;
        }
        let is_space = !u.marker && u.text == " ";
        cur.push(u);
        cur_w += w;
        if is_space {
            last_break = Some(cur.len());
        }
    }
    if !cur.is_empty() {
        lines.push(flush(cur));
    }
    lines
}

// ---------------------------------------------------------------- the view

/// What the last layout decided: rectangles in client px, for painting and for the hit test.
#[derive(Clone, Default)]
struct Geo {
    w: i32,
    h: i32,
    field: RECT,
    /// The visible rows, top to bottom.
    rows: Vec<RowGeo>,
    /// The rows that do not fit are below (or above): the scrollbar's track, and the visible window.
    first: usize,
    total: usize,
    track: RECT,
}

#[derive(Clone)]
struct RowGeo {
    idx: usize,
    rect: RECT,
    /// The detail shown wrapped over several lines (a review, a notice); empty for the one-line detail.
    lines: Vec<Vec<Run>>,
}

/// The launcher's popup content.
pub(crate) struct Launcher {
    session: Session,
    /// Panel width (logical px, 560-880).
    width: i32,
    /// Rows shown at most (4-12, the setting).
    max_rows: usize,
    /// Largest height the popup may take (physical px).
    max_h: i32,
    /// The launcher is the foreground window: caret, selected plate and hint show only then.
    foreground: bool,
    /// App icons by lower-case id, the bar's by "bar:" and the lower-case group key.
    icons: HashMap<String, IconBitmap>,
    /// First visible row, and the selection the last layout saw (the view follows a new selection, not a wheel).
    first: Cell<usize>,
    last_sel: Cell<Option<usize>>,
    /// Horizontal scroll of the query (physical px) that keeps the caret in the field.
    scroll: Cell<i32>,
    /// Visible rows of the last layout, for the wheel.
    count: Cell<usize>,
    geo: RefCell<Geo>,
    /// The height last given to the host, and whether a shrink waits (120 ms without a keystroke).
    shown_h: i32,
    hold_shrink: bool,
    /// Ticks the scrollbar still shows after a scroll.
    bar_ticks: Cell<u8>,
    /// The sequence of the chord post that opened it (`hook::LAUNCHER_SEQ`): its type-ahead window is ended at the close.
    open_seq: u32,
    /// The first activation took the type-ahead ring (once per open).
    taken: bool,
    /// The session's effects that run after the close, in order.
    queued: Vec<Effect>,
}

impl Launcher {
    fn with_session(session: Session) -> Launcher {
        Launcher {
            session,
            width: style::LAUNCHER_W,
            max_rows: usize::from(style::ROWS_DEFAULT),
            max_h: i32::MAX,
            foreground: false,
            icons: HashMap::new(),
            first: Cell::new(0),
            last_sel: Cell::new(None),
            scroll: Cell::new(0),
            count: Cell::new(0),
            geo: RefCell::new(Geo::default()),
            shown_h: 0,
            hold_shrink: false,
            bar_ticks: Cell::new(0),
            open_seq: 0,
            taken: false,
            queued: Vec::new(),
        }
    }

    /// The launcher became the foreground window (or lost it): caret, selected plate and hint come with it.
    fn set_foreground(&mut self, active: bool) -> Reply {
        if self.foreground == active {
            return Reply::None;
        }
        self.foreground = active;
        // The review and the hint come with the focus, and the review changes the height.
        Reply::Relayout
    }

    /// `focus` on the ports. At the first activation the keys the hook kept for this open's post are taken and replayed
    /// into the session in order; losing the foreground ends the window (a no-op after the take).
    fn on_focus(&mut self, active: bool, cx: &Context, hook: &mut dyn HookPort, sys: &mut dyn SysPort) -> Reply {
        let view = self.set_foreground(active);
        if !active {
            hook.end(self.open_seq);
            return view;
        }
        if self.taken {
            return view;
        }
        self.taken = true;
        let mut buf = [KeyRec::default(); RING_CAP];
        let n = hook.take(self.open_seq, &mut buf);
        let keys = replay_keys(buf.get(..n).unwrap_or(&[]), sys);
        if keys.is_empty() {
            return view;
        }
        let fx = self.session.replay(&keys, cx);
        view.merge(self.run(fx, cx, sys))
    }

    /// Runs the session's effects in order: the view's at once, the others after the close.
    fn run(&mut self, fx: Vec<Effect>, _cx: &Context, _sys: &mut dyn SysPort) -> Reply {
        let mut reply = Reply::None;
        for e in fx {
            match e {
                Effect::Relayout => reply = reply.merge(Reply::Relayout),
                Effect::Redraw => reply = reply.merge(Reply::Redraw),
                Effect::Hide { .. } => reply = Reply::Close,
                other => self.queued.push(other),
            }
        }
        reply
    }

    /// The chord again while open (the controller's `OpenPlan::Close`): the final Esc.
    pub(crate) fn chord_again(&mut self) -> Reply {
        Reply::Close
    }

    /// The close, with the popup already hidden: the hook stops treating the launcher as shown and the type-ahead window
    /// of the open's post ends (a no-op when the first activation took it).
    fn finish(&mut self, hook: &mut dyn HookPort) {
        hook.shown(false);
        hook.end(self.open_seq);
        // The effects of the close run on the system port (L1a-15c).
        self.queued.clear();
    }

    /// The session the keys and the late batches go to (L1a-15).
    // The chord flow of L1a-15 reaches the session through it; until then only the tests do.
    #[allow(dead_code)]
    pub(crate) fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Panel width in logical px and the visible rows (the settings of L1a-11).
    // L1a-15 hands the config's values over at open.
    #[allow(dead_code)]
    pub(crate) fn set_shape(&mut self, width: i32, rows: u8) {
        self.width = width.clamp(*style::WIDTH_RANGE.start(), *style::WIDTH_RANGE.end());
        self.max_rows =
            usize::from(rows).clamp(usize::from(*style::ROWS_RANGE.start()), usize::from(*style::ROWS_RANGE.end()));
    }

    /// An icon arrived: an app id, or "bar:" + group key, lower-case (L1a-16).
    // The icon cache of L1a-16 fills it.
    #[allow(dead_code)]
    pub(crate) fn set_icon(&mut self, key: &str, bmp: IconBitmap) {
        self.icons.insert(key.to_lowercase(), bmp);
    }

    /// A shrink waits (while the user types) or is let go (120 ms without a keystroke): growing is never held.
    // The TIMER_LAUNCHER_SHRINK of L1a-15 toggles it.
    #[allow(dead_code)]
    pub(crate) fn hold_shrink(&mut self, on: bool) {
        self.hold_shrink = on;
    }

    // ------------------------------------------------------------ layout

    /// The lines under the name of a row: a notice's text or the selected row's review, wrapped; empty for the others.
    fn detail_lines(&self, s: &Surface, item: &Item, selected: bool) -> Vec<Vec<Run>> {
        let m = s.metrics();
        let max_w = hint_right(m, self.width) - m.px(style::NAME_X);
        if item.kind == Kind::Notice {
            let Some(text) = item.detail.as_deref() else { return Vec::new() };
            let mut lines = wrap(s, text, Font::Caption, max_w, MAX_LINES);
            if lines.len() > MAX_LINES {
                lines.truncate(MAX_LINES);
                if let Some(last) = lines.last_mut() {
                    push_run(last, Run { text: "\u{2026}".to_string(), marker: false });
                }
            }
            return lines;
        }
        if !selected || !self.foreground {
            return Vec::new();
        }
        match review_text(item) {
            Some(text) => {
                let lines = wrap(s, text, Font::Caption, max_w, MAX_LINES);
                if lines.len() > MAX_LINES {
                    Vec::new()
                } else {
                    lines
                }
            }
            None => Vec::new(),
        }
    }

    /// How many CAPTION lines the review of the selected row needs, capped one past the limit (the session's measure).
    fn review_lines(&self, s: &Surface) -> Option<u8> {
        let item = self.session.selected()?;
        let text = review_text(item)?;
        let m = s.metrics();
        let max_w = hint_right(m, self.width) - m.px(style::NAME_X);
        let n = wrap(s, text, Font::Caption, max_w, MAX_LINES).len().min(MAX_LINES + 1);
        u8::try_from(n.max(1)).ok()
    }

    /// Places everything for the surface's metrics and the session as it is now.
    fn layout(&self, s: &Surface) -> Geo {
        let m = s.metrics();
        let w = m.px(self.width);
        let inset = m.px(style::S_4);
        let field =
            rect_wh(m.px(style::FIELD_X), m.px(style::FIELD_Y), w - 2 * m.px(style::FIELD_X), m.px(style::SEARCH_H));
        self.follow_caret(s, &field);
        let rows = self.session.rows();
        let total = rows.len();
        if total == 0 {
            self.count.set(0);
            self.first.set(0);
            self.last_sel.set(self.session.selected_index());
            return Geo { w, h: m.px(EMPTY_H), field, ..Geo::default() };
        }
        let top = m.px(style::LIST_Y);
        let avail = (self.max_h.saturating_sub(top + inset)).max(0);
        let sel = self.session.selected_index();
        let base = m.px(style::ROW_COMFORT);
        let step = m.px(style::REVIEW_LINE);
        // The rows from `first` that fit: their count and rectangles.
        let fit = |first: usize| -> Vec<RowGeo> {
            let mut out: Vec<RowGeo> = Vec::new();
            let mut y = top;
            for (idx, item) in rows.iter().enumerate().skip(first).take(self.max_rows) {
                let lines = self.detail_lines(s, item, sel == Some(idx));
                let extra = i32::try_from(lines.len().saturating_sub(1)).unwrap_or(0);
                let h = base + extra * step;
                if !out.is_empty() && y + h - top > avail {
                    break;
                }
                out.push(RowGeo {
                    idx,
                    rect: rect_wh(m.px(style::FIELD_X), y, w - 2 * m.px(style::FIELD_X), h),
                    lines,
                });
                y += h;
            }
            out
        };
        let mut first = self.first.get().min(total.saturating_sub(1));
        let mut shown = fit(first);
        if let (Some(i), true) = (sel, self.last_sel.get() != sel) {
            if i < first {
                first = i;
                shown = fit(first);
            }
            while i >= first + shown.len() && first < i {
                first += 1;
                shown = fit(first);
            }
        }
        // A wheel past the end, or fewer rows than before: never an empty window.
        while first > 0 && first + shown.len() > total {
            first -= 1;
            shown = fit(first);
        }
        if first != self.first.get() {
            self.bar_ticks.set(SCROLLBAR_TICKS);
        }
        self.first.set(first);
        self.last_sel.set(sel);
        self.count.set(shown.len());
        let list_bottom = shown.last().map_or(top, |r| r.rect.bottom);
        let h = list_bottom + inset;
        let track = RECT {
            left: w - m.px(style::SCROLLBAR_X_INSET) - m.px(style::SCROLLBAR_W),
            top,
            right: w - m.px(style::SCROLLBAR_X_INSET),
            bottom: list_bottom,
        };
        Geo { w, h, field, rows: shown, first, total, track }
    }

    /// Keeps the caret inside the query's room (the field's width less the chip and the margins).
    fn follow_caret(&self, s: &Surface, field: &RECT) {
        let m = s.metrics();
        let (_, room) = self.query_room(s, field);
        let text = self.session.query_text();
        let widths = char_widths(s, text);
        let caret_x = prefix_width(&widths, text, self.session.field().caret());
        let whole: i32 = widths.iter().sum();
        let mut scroll = self.scroll.get();
        let slack = m.px(2);
        if caret_x - scroll > room - slack {
            scroll = caret_x - room + slack;
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        if whole + slack <= room {
            scroll = 0;
        }
        self.scroll.set(scroll.max(0));
    }

    /// Where the query starts (physical px) and how wide its room is.
    fn query_room(&self, s: &Surface, field: &RECT) -> (i32, i32) {
        let m = s.metrics();
        let mut tx = m.px(style::NAME_X);
        if let Some(p) = self.session.chip() {
            tx += chip_width(s, p) + m.px(CHIP_GAP);
        }
        (tx, field.right - m.px(style::S_4) - tx)
    }

    // ------------------------------------------------------------ painting

    fn paint_field(&self, s: &mut Surface, g: &Geo, hot: bool) {
        let m = s.metrics();
        let f = g.field;
        let r = m.px(style::R_S);
        s.fill_rr(f, r, WHITE, if hot { style::FILL_CONTROL_HOT } else { style::FILL_CONTROL });
        s.stroke_rr(f, r, m.px(1).max(1), style::RIM_CONTROL);
        let gs = m.px(style::ICON_S);
        let gx = f.left + m.px(16);
        s.glyph(gx, f.top + (f.bottom - f.top - gs) / 2, gs, style::FG_SECONDARY, &super::popup_startmenu::magnifier);
        let lit = self.foreground;
        let text_alpha = if lit { style::FG_PRIMARY } else { style::FG_TERTIARY };
        if let Some(p) = self.session.chip() {
            let cw = chip_width(s, p);
            let ch = m.px(CHIP_H);
            let rc = rect_wh(m.px(style::NAME_X), f.top + (f.bottom - f.top - ch) / 2, cw, ch);
            s.fill_rr(rc, m.px(style::R_XS), WHITE, style::FILL_CONTROL_HOT);
            s.stroke_rr(rc, m.px(style::R_XS), m.px(1).max(1), style::RIM_CONTROL);
            if let Some(label) = strings::chip(p) {
                let alpha = if lit { style::FG_SECONDARY } else { style::FG_TERTIARY };
                s.text(
                    label,
                    rc.left + m.px(CHIP_PAD),
                    center_y(&rc, s.line_height(Font::Caption)),
                    Font::Caption,
                    WHITE,
                    alpha,
                );
            }
        }
        let (tx, room) = self.query_room(s, &f);
        let lh = s.line_height(Font::Normal);
        let ty = center_y(&f, lh);
        let ch = m.px(CARET_H);
        let cy = f.top + (f.bottom - f.top - ch) / 2;
        let scroll = self.scroll.get();
        let text = self.session.query_text();
        s.set_clip(Some(rect_wh(tx, f.top, room.max(0), f.bottom - f.top)));
        if text.is_empty() {
            if self.session.chip().is_none() {
                s.text(strings::placeholder(), tx, ty, Font::Normal, WHITE, style::FG_TERTIARY);
            }
        } else {
            let field = self.session.field();
            let widths = char_widths(s, text);
            if let Some(sel) = field.selection() {
                let before = prefix_width(&widths, text, sel.start);
                let upto = prefix_width(&widths, text, sel.end);
                let rc = rect_wh(tx + before - scroll, cy, upto - before, ch);
                s.fill_rr(rc, 0, WHITE, style::FILL_TEXT_SELECTION);
            }
            // Only the characters in the room are drawn: a long query is wider than the text scratch.
            let mut runs: Vec<Run> = Vec::new();
            let (mut at, mut start_x) = (0, None);
            for (ch_, w) in text.chars().zip(&widths) {
                if at + w > scroll && at < scroll + room {
                    start_x.get_or_insert(at);
                    push_run(&mut runs, unit(ch_));
                }
                at += w;
            }
            draw_runs(s, &runs, tx + start_x.unwrap_or(0) - scroll, ty, Font::Normal, text_alpha);
        }
        if lit {
            let widths = char_widths(s, text);
            let x = tx + prefix_width(&widths, text, self.session.field().caret()) - scroll + m.px(1);
            s.fill_rr(rect_wh(x, cy, m.px(1).max(1), ch), 0, WHITE, style::FG_HOT);
        }
        s.set_clip(None);
    }

    /// The icon column of a row: the icon, a placeholder, or a glyph; `box_y` is the top of the 24 px box.
    fn paint_icon(&self, s: &mut Surface, item: &Item, x: i32, box_y: i32, alpha: u32) {
        let m = s.metrics();
        let size = m.px(style::ICON_M);
        let bitmap = match &item.icon {
            IconRef::App(id) => self.icons.get(&id.to_lowercase()),
            IconRef::Bar(key) => self.icons.get(&format!("bar:{}", key.to_lowercase())),
            _ => None,
        };
        match (&item.icon, bitmap) {
            (_, Some(b)) => s.blit(b, x, box_y, size, 255),
            (IconRef::Bar(key), None) if crate::ui::paint_bar_icon(s, key, x, box_y, size) => {}
            (IconRef::Glyph(g), _) => {
                let gs = m.px(style::ICON_S);
                let off = (size - gs) / 2;
                s.glyph(x + off, box_y + off, gs, alpha, glyph_shape(*g));
            }
            _ => s.fill_rr(rect_wh(x, box_y, size, size), m.px(style::R_XS), WHITE, style::FILL_CONTROL),
        }
    }

    fn paint_row(&self, s: &mut Surface, g: &RowGeo, item: &Item, selected: bool, pressed: bool) {
        let m = s.metrics();
        let rc = g.rect;
        let radius = m.px(style::R_S);
        let base = m.px(style::ROW_COMFORT);
        let first_line = RECT { bottom: rc.top + base, ..rc };
        if pressed {
            s.plate(rc, radius, style::FILL_PRESS);
        } else if selected {
            s.plate(rc, radius, style::FILL_HOVER);
        }
        let name_alpha = if selected { style::FG_HOT } else { style::FG_PRIMARY };
        // Icon column.
        let ix = m.px(style::ICON_X);
        let iy = rc.top + (base - m.px(style::ICON_M)) / 2;
        self.paint_icon(s, item, ix, iy, style::FG_SECONDARY);
        if matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_)) && matches!(item.kind, Kind::App | Kind::Window)
        {
            let px = ix + (m.px(style::ICON_M) - m.px(PILL_W)) / 2;
            let py = iy + m.px(style::ICON_M) + m.px(PILL_GAP);
            s.fill_rr(rect_wh(px, py, m.px(PILL_W), m.px(PILL_H)), m.px(1), WHITE, style::OPEN_INDICATOR);
        }
        // Hint, selected row only.
        let right = hint_right(m, self.width);
        let mut name_right = right;
        if selected && self.foreground {
            if let Some(hint) = hint_for(item) {
                let hw = s.text_width(hint, Font::Caption);
                let y = center_y(&first_line, s.line_height(Font::Caption));
                s.text(hint, right - hw, y, Font::Caption, WHITE, style::FG_SECONDARY);
                name_right = right - hw - m.px(HINT_GAP);
            }
        }
        // Name and detail.
        let nx = m.px(style::NAME_X);
        let font = if item.kind == Kind::Answer { Font::Bold } else { Font::Normal };
        let name_alpha = if item.kind == Kind::Answer { style::FG_HOT } else { name_alpha };
        let nlh = s.line_height(font);
        let has_detail =
            item.detail.is_some() && !g.lines.is_empty() || item.detail.is_some() && item.kind != Kind::App;
        let has_review = !g.lines.is_empty();
        let ny = if has_detail || has_review { rc.top + m.px(NAME_TOP) } else { center_y(&first_line, nlh) };
        draw_line(s, &item.title, nx, ny, name_right - nx, font, name_alpha);
        let dy = ny + nlh + m.px(DETAIL_GAP);
        if has_review {
            let step = m.px(style::REVIEW_LINE);
            for (i, line) in g.lines.iter().enumerate() {
                let y = dy + i32::try_from(i).unwrap_or(0) * step;
                draw_runs(s, line, nx, y, Font::Caption, style::FG_SECONDARY);
            }
        } else if let Some(detail) = item.detail.as_deref() {
            let alpha = if detail.contains(strings::thousands()) { style::FG_SECONDARY } else { style::FG_TERTIARY };
            let max_w = hint_right(m, self.width) - nx;
            if detail.chars().any(gesture::is_marker) {
                draw_line(s, detail, nx, dy, max_w, Font::Caption, alpha);
            } else {
                let shown = ellipsize_middle(s, detail, Font::Caption, max_w);
                s.text(&shown, nx, dy, Font::Caption, WHITE, alpha);
            }
        }
    }

    fn paint_scrollbar(&self, s: &mut Surface, g: &Geo, hot: bool) {
        if g.total <= g.rows.len() || g.rows.is_empty() {
            return;
        }
        if self.bar_ticks.get() == 0 && !hot {
            return;
        }
        let m = s.metrics();
        let t = g.track;
        let track_h = (t.bottom - t.top).max(1);
        let total = i32::try_from(g.total).unwrap_or(i32::MAX).max(1);
        let thumb = (track_h * i32::try_from(g.rows.len()).unwrap_or(0) / total).max(m.px(16)).min(track_h);
        let travel = track_h - thumb;
        let denom = (total - i32::try_from(g.rows.len()).unwrap_or(0)).max(1);
        let y = t.top + travel * i32::try_from(g.first).unwrap_or(0) / denom;
        let rc = RECT { top: y, bottom: y + thumb, ..t };
        s.fill_rr(rc, (t.right - t.left) / 2, WHITE, if hot { style::SCROLLBAR_HOT } else { style::SCROLLBAR_REST });
    }
}

/// The right edge of the hint and of the names (physical px): the panel's width less HINT_RIGHT_INSET.
fn hint_right(m: Metrics, width: i32) -> i32 {
    m.px(width) - m.px(style::HINT_RIGHT_INSET)
}

/// The mode chip's width: its label with the padding either side.
fn chip_width(s: &Surface, p: ProviderId) -> i32 {
    let m = s.metrics();
    let label = strings::chip(p).unwrap_or("");
    s.text_width(label, Font::Caption) + 2 * m.px(CHIP_PAD)
}

/// The text the review shows whole (SPEC 5.3): the run line, or the path of a file that offers Run as administrator.
fn review_text(item: &Item) -> Option<&str> {
    if !item.verbs.has(Verb::RunAsAdmin) {
        return None;
    }
    match (&item.kind, &item.payload) {
        (Kind::RunLine, Payload::RunLine(r)) => Some(&r.line),
        (Kind::File, Payload::Path { path, dir: false, .. }) => Some(path),
        _ => None,
    }
}

/// The hint of the selected row: what Enter does (5.3).
fn hint_for(item: &Item) -> Option<&'static str> {
    let running = matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_));
    let verb = verbs::primary(item.kind, running);
    if !item.verbs.has(verb) {
        return None;
    }
    Some(match (item.kind, verb) {
        (Kind::RunLine | Kind::RunSuggestion, _) => strings::hint_run(),
        (_, Verb::Switch) => strings::hint_switch(),
        (_, Verb::Copy) => strings::hint_copy(),
        _ => strings::hint_open(),
    })
}

/// The shape of a glyph id.
fn glyph_shape(g: GlyphId) -> &'static dyn Fn(f32, f32) -> bool {
    match g {
        GlyphId::Magnifier => &super::popup_startmenu::magnifier,
        GlyphId::Equals => &glyphs::equals_shape,
        GlyphId::Prompt => &glyphs::prompt_shape,
        GlyphId::Window => &glyphs::window_shape,
        GlyphId::Document => &glyphs::document_shape,
        GlyphId::Image => &glyphs::image_shape,
        GlyphId::Archive => &glyphs::archive_shape,
        GlyphId::Executable => &glyphs::executable_shape,
        GlyphId::Copy => &glyphs::copy_shape,
        GlyphId::Globe => &glyphs::globe_shape,
        GlyphId::Clock => &glyphs::clock_shape,
        GlyphId::Folder => &glyphs::folder_shape,
        GlyphId::Info => &glyphs::info_shape,
        GlyphId::Gear => &glyphs::gear_shape,
        GlyphId::Lock => &glyphs::lock_shape,
        GlyphId::Moon => &glyphs::moon_shape,
        GlyphId::Restart => &glyphs::restart_shape,
        GlyphId::Power => &glyphs::power_shape,
        GlyphId::Exit => &glyphs::exit_shape,
    }
}

impl Content for Launcher {
    fn kind(&self) -> PopupKind {
        PopupKind::Launcher
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let g = self.layout(s);
        // The review of the selected row is measured with the real font, for the session's conditions.
        if let (Some(lines), Some(key)) = (self.review_lines(s), self.session.selected().map(|i| i.key)) {
            self.session.set_review_lines(key, lines);
        }
        let mut h = g.h;
        if self.hold_shrink && h < self.shown_h {
            h = self.shown_h;
        }
        self.shown_h = h;
        let w = g.w;
        *self.geo.borrow_mut() = g;
        (w, h)
    }

    fn limit(&mut self, max: (i32, i32)) {
        self.max_h = max.1;
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let g = self.layout(s);
        self.paint_field(s, &g, hot == Some(ID_FIELD));
        let sel = self.session.selected_index();
        for rg in &g.rows {
            let Some(item) = self.session.rows().get(rg.idx) else { continue };
            let id = ID_ROW + u32::try_from(rg.idx).unwrap_or(0);
            self.paint_row(s, rg, item, self.foreground && sel == Some(rg.idx), pressed == Some(id));
        }
        self.paint_scrollbar(s, &g, false);
        if let Ok(mut slot) = self.geo.try_borrow_mut() {
            *slot = g;
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        let g = self.geo.try_borrow().ok()?;
        let mut map = HitMap::default();
        map.add(ID_FIELD, g.field);
        for r in &g.rows {
            map.add(ID_ROW + u32::try_from(r.idx).unwrap_or(0), r.rect);
        }
        map.at(x, y)
    }

    fn clickable(&self, id: u32) -> bool {
        id >= ID_ROW
    }

    fn click(&mut self, _id: u32) -> Reply {
        // L1a-15 maps a row click to `Session::click` with the Context.
        Reply::None
    }

    fn wheel(&mut self, notches: i32, _x: i32, _y: i32) -> Reply {
        let total = self.session.rows().len();
        let count = self.count.get();
        if total <= count || notches == 0 {
            return Reply::None;
        }
        let step = WHEEL_ROWS * usize::try_from(notches.unsigned_abs()).unwrap_or(1);
        let first = self.first.get();
        let next = if notches > 0 { first.saturating_sub(step) } else { (first + step).min(total - count) };
        if next == first {
            return Reply::None;
        }
        self.first.set(next);
        self.bar_ticks.set(SCROLLBAR_TICKS);
        Reply::Relayout
    }

    fn focus(&mut self, active: bool) -> Reply {
        match ctx(|cx| self.on_focus(active, cx, &mut RealHook, &mut RealSys)) {
            Some(r) => r,
            None => self.set_foreground(active),
        }
    }

    fn tick(&mut self) -> Reply {
        match self.bar_ticks.get() {
            0 => Reply::None,
            n => {
                self.bar_ticks.set(n - 1);
                if n == 1 {
                    Reply::Redraw
                } else {
                    Reply::None
                }
            }
        }
    }

    fn wants_sys_keys(&self) -> bool {
        true
    }

    fn focus_lost_closes(&self) -> bool {
        true
    }

    fn after_close(&mut self) {
        self.finish(&mut RealHook);
    }

    fn launcher(&mut self) -> Option<&mut Launcher> {
        Some(self)
    }
}

/// The content of a new launcher popup (`popup::make_content`), for the post the controller is opening.
pub(crate) fn new() -> Box<dyn Content> {
    let mut l = Launcher::with_session(Session::open(&LauncherOpts::default(), None));
    l.open_seq = with_ctrl(|c| c.open_seq).unwrap_or(0);
    Box::new(l)
}

/// Once, at the end of `popup::init` (wired by L1a-15): nothing to set up yet.
// The chord flow of L1a-15 calls it; until then nothing does.
#[allow(dead_code)]
pub(crate) fn init() {}

/// One of the launcher's one-shot timers (`TIMER_LAUNCHER_*`) fired on the popup window: killed first.
pub(crate) fn on_timer(hwnd: HWND, id: usize) {
    // SAFETY: a timer of our popup window; killing an id that is not armed fails harmlessly.
    let _ = unsafe { KillTimer(Some(hwnd), id) };
}

// ---------------------------------------------------------------- the controller (L1a-15)

/// What the controller asks of the keyboard hook: hook.rs's statics in the dock (`launcher_sys::RealHook`), a recorder in
/// the tests.
pub(crate) trait HookPort {
    /// `hook::ack_launcher`: the post of `seq` arrived.
    fn ack(&mut self, seq: u32);
    /// `hook::end_typeahead`: the keys typed after the chord of `seq` are discarded (a no-op for another sequence).
    fn end(&mut self, seq: u32);
    /// `hook::set_launcher_shown`.
    fn shown(&mut self, on: bool);
    /// `hook::set_pass_flags`.
    fn pass(&mut self, flags: PassFlags);
    /// The newest sequence the hook issued and the newest it failed open on (`LAUNCHER_SEQ`, `LAST_FAILOPEN_SEQ`).
    fn seqs(&self) -> (u32, u32);
    /// `hook::alt_tracked_down`.
    fn alt_down(&self) -> bool;
    /// `hook::take_typeahead`: the keys typed after the chord of `seq`, once (0 for another sequence or an ended one).
    fn take(&mut self, seq: u32, out: &mut [KeyRec; RING_CAP]) -> usize;
}

/// What the controller asks of Windows: Win32 and the popup host in the dock (`launcher_sys::RealSys`), a recorder in the
/// tests.
pub(crate) trait SysPort {
    /// `GetTickCount`: the clock of the hook's `kb.time`.
    fn now(&self) -> u32;
    /// Exclusive Direct3D fullscreen (`SHQueryUserNotificationState` is `QUNS_RUNNING_D3D_FULL_SCREEN`).
    fn d3d_fullscreen(&self) -> bool;
    /// The foreground window covers its whole monitor (`ui::foreground_is_fullscreen`).
    fn fullscreen_foreground(&self) -> bool;
    /// The launcher is the open popup.
    fn launcher_open(&self) -> bool;
    /// Opens the launcher centred on the monitor `pick` names; false when nothing is shown (no popups, no surface).
    fn open(&mut self, pick: MonitorPick, top_percent: u8) -> bool;
    /// The chord while the launcher is open: its close request (the final Esc).
    fn close_launcher(&mut self);
    /// `SendInput` of these (vk, up) events, tagged `hook::INJECT_TAG`.
    fn inject(&mut self, keys: &[(u16, bool)]);
    /// What a kept key types with the UI thread's keyboard layout and its recorded modifiers (`ToUnicodeEx`, keyboard
    /// state left unchanged).
    fn translate(&mut self, rec: &KeyRec) -> Typed;
}

/// What one kept key types.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Typed {
    /// These characters, none of them a control character.
    Text(String),
    /// A dead key: dropped (the character it would start never comes).
    Dead,
    /// No character, or a control one (Enter, Esc, Tab, Backspace, Ctrl+letter): the key itself.
    Key,
}

/// `ToUnicodeEx`'s answer (`n` units written to `buf`, negative for a dead key) as what the key types.
pub(crate) fn typed_of(n: i32, buf: &[u16]) -> Typed {
    if n < 0 {
        return Typed::Dead;
    }
    let units = buf.get(..usize::try_from(n).unwrap_or(0)).unwrap_or(&[]);
    let text = String::from_utf16_lossy(units);
    if text.is_empty() || text.chars().any(char::is_control) {
        Typed::Key
    } else {
        Typed::Text(text)
    }
}

/// The kept keys as the session replays them, in order: characters as text, the other keys as key events.
fn replay_keys(recs: &[KeyRec], sys: &mut dyn SysPort) -> Vec<ReplayKey> {
    let mut out = Vec::with_capacity(recs.len());
    for r in recs {
        match sys.translate(r) {
            Typed::Text(s) => out.extend(s.chars().map(ReplayKey::Text)),
            Typed::Dead => {}
            Typed::Key => {
                out.push(ReplayKey::Key(KeyEvent { vk: r.vk, mods: r.mods, repeat: false, sys: r.mods.has(Mods::ALT) }))
            }
        }
    }
    out
}

/// Which monitor the launcher opens on (`launcher.monitor`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // on_config reads launcher.monitor (L1a-15d)
pub(crate) enum MonitorPick {
    /// The one under the pointer.
    Pointer,
    /// The one of the foreground window.
    Active,
    Primary,
}

/// What a chord post leads to (spec rule 8 and 5.2.12).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OpenPlan {
    /// Nothing opens: a dropped post, or a fullscreen window in front.
    Nothing,
    /// Open the launcher.
    TryOpen,
    /// The launcher is open: close it like the final Esc.
    Close,
}

/// The plan of a post: `action` is `hook::post_verdict`'s; an exclusive D3D fullscreen (`quns_d3d`) always passes, a
/// fullscreen foreground window (`fs_window`) passes while `launcher.over_fullscreen` is "pass".
fn open_plan(action: PostAction, quns_d3d: bool, fs_window: bool, over_fullscreen_pass: bool) -> OpenPlan {
    match action {
        PostAction::Drop => OpenPlan::Nothing,
        PostAction::Close => OpenPlan::Close,
        PostAction::Open if quns_d3d || (fs_window && over_fullscreen_pass) => OpenPlan::Nothing,
        PostAction::Open => OpenPlan::TryOpen,
    }
}

/// The controller's state between opens (an open's own state is in its content, `Launcher`).
struct Ctrl {
    /// The sequence of the post being opened (read by `new`).
    open_seq: u32,
    /// `launcher.monitor`, `launcher.top_percent` and `launcher.over_fullscreen == "pass"`.
    pick: MonitorPick,
    top_percent: u8,
    over_fullscreen_pass: bool,
    /// The pass flags the hook was last given.
    flags: PassFlags,
    /// The session's view of the config and the locale's number marks.
    opts: LauncherOpts,
    num: NumberStyle,
    /// What the providers' context borrows (the catalogue, the settings, the windows and the run history).
    apps: Vec<CatApp>,
    settings: Vec<SettingRow>,
    windows: Vec<WindowRow>,
    history: Vec<Box<str>>,
}

impl Ctrl {
    fn new() -> Ctrl {
        Ctrl {
            open_seq: 0,
            pick: MonitorPick::Pointer,
            top_percent: crate::config::LAUNCHER_TOP_PERCENT.2,
            over_fullscreen_pass: true,
            flags: PassFlags::default(),
            opts: LauncherOpts::default(),
            num: NumberStyle { decimal: '.', group: ',' },
            apps: Vec::new(),
            settings: Vec::new(),
            windows: Vec::new(),
            history: Vec::new(),
        }
    }

    /// The providers' context of one keystroke: borrowed, nothing copied.
    fn context(&self) -> Context<'_> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        Context {
            apps: &self.apps,
            settings: &self.settings,
            windows: &self.windows,
            history: &self.history,
            known: &runline::KNOWN,
            opts: &self.opts,
            num: self.num,
            now_unix: now.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
        }
    }
}

thread_local! {
    /// Touched only while a chord is set: the paths that exist without the launcher never reach it.
    static CTRL: RefCell<Ctrl> = RefCell::new(Ctrl::new());
}

/// Runs `f` on the controller's state; None (logged) on a re-entry, never a panic.
fn with_ctrl<R>(f: impl FnOnce(&mut Ctrl) -> R) -> Option<R> {
    CTRL.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => Some(f(&mut g)),
        Err(_) => {
            log!("launcher: controller state already in use (re-entry), operation skipped");
            None
        }
    })
}

/// Runs `f` with the providers' context; None (logged) on a re-entry.
fn ctx<R>(f: impl FnOnce(&Context) -> R) -> Option<R> {
    CTRL.with(|c| match c.try_borrow() {
        Ok(g) => Some(f(&g.context())),
        Err(_) => {
            log!("launcher: controller state already in use (re-entry), key skipped");
            None
        }
    })
}

/// `WM_DOCK_LAUNCHER`: the hook swallowed the chord and posted `seq` with the key's `kb.time`.
pub(crate) fn on_chord(seq: u32, sent_time: u32) {
    chord_flow(seq, sent_time, &mut RealHook, &mut RealSys);
}

/// The chord flow of one post. Every plan that does not end in an open launcher ends the type-ahead window of this post
/// first, so the next key reaches the window behind and the keys already kept are never replayed by a later chord.
fn chord_flow(seq: u32, sent_time: u32, hook: &mut dyn HookPort, sys: &mut dyn SysPort) -> OpenPlan {
    hook.ack(seq);
    let (newest, failopen) = hook.seqs();
    let action = hook::post_verdict(seq, sent_time, sys.now(), newest, failopen, sys.launcher_open());
    let defaults = (MonitorPick::Pointer, crate::config::LAUNCHER_TOP_PERCENT.2, true);
    let (pick, top_percent, over_pass) =
        with_ctrl(|c| (c.pick, c.top_percent, c.over_fullscreen_pass)).unwrap_or(defaults);
    let (d3d, fs) = if action == PostAction::Open {
        let d3d = sys.d3d_fullscreen();
        (d3d, !d3d && over_pass && sys.fullscreen_foreground())
    } else {
        (false, false)
    };
    let plan = open_plan(action, d3d, fs, over_pass);
    match plan {
        OpenPlan::Nothing => {
            hook.end(seq);
            if action == PostAction::Open {
                // The state changed without a foreground change: the hook passes the next chords too.
                if let Some(flags) = with_ctrl(|c| {
                    c.flags = c.flags.with(PassFlags::FULLSCREEN);
                    c.flags
                }) {
                    hook.pass(flags);
                }
                log!("launcher: chord over a fullscreen window, nothing opens");
            } else {
                log!("launcher: chord post {seq} dropped (stale)");
            }
        }
        OpenPlan::Close => {
            hook.end(seq);
            sys.close_launcher();
        }
        OpenPlan::TryOpen => {
            with_ctrl(|c| c.open_seq = seq);
            hook.shown(true);
            if !sys.open(pick, top_percent) {
                hook.shown(false);
                hook.end(seq);
                // The window menu opens as Windows would have opened it.
                let keys = hook::reinject_plan(hook.alt_down());
                sys.inject(keys.keys());
                log!("launcher: the launcher could not open, the chord goes back to Windows");
            }
        }
    }
    plan
}

// ---------------------------------------------------------------- preview scenes (mock data, nothing runs)

/// The launcher scenes of `--preview` (L1a-18 lists them; the field pair is listed since L1a-14).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PreviewScene {
    Empty,
    Inactive,
    Apps,
    Running,
    Calc,
    Run,
    RunReview,
    Windows,
    Notice,
    FieldPair,
}

impl PreviewScene {
    /// `launcher_empty`, `launcher_field_pair` and so on.
    pub(crate) fn from_name(name: &str) -> Option<PreviewScene> {
        Some(match name.strip_prefix("launcher_")? {
            "empty" => PreviewScene::Empty,
            "inactive" => PreviewScene::Inactive,
            "apps" => PreviewScene::Apps,
            "running" => PreviewScene::Running,
            "calc" => PreviewScene::Calc,
            "run" => PreviewScene::Run,
            "run_review" => PreviewScene::RunReview,
            "windows" => PreviewScene::Windows,
            "notice" => PreviewScene::Notice,
            "field_pair" => PreviewScene::FieldPair,
            _ => return None,
        })
    }
}

/// A launcher built from fixture data: the catalogue, two running windows and the icons of `preview::mock_icon`.
pub(crate) fn preview_new(scene: PreviewScene) -> Box<dyn Content> {
    if scene == PreviewScene::FieldPair {
        return Box::new(FieldPair);
    }
    Box::new(scene_launcher(scene))
}

/// The launcher of a scene other than the field pair.
fn scene_launcher(scene: PreviewScene) -> Launcher {
    use crate::i18n::tr;
    use crate::launcher::catalog::CatApp;
    use crate::launcher::model::{ItemKey, NumberStyle, Score, SettingRow, VerbSet, WindowRow};
    use crate::launcher::providers::Context;
    use crate::launcher::rank;
    use crate::launcher::runline;
    use crate::start_search::Keys;
    use std::cmp::Reverse;

    let notepad = tr("Notepad", "Blocco note");
    let calc = tr("Calculator", "Calcolatrice");
    let apps_def: [(&str, &str, char, u32); 9] = [
        ("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", calc, 'C', 0x3A3A3A),
        ("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", notepad, 'N', 0x2B78B8),
        ("{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Notepad++\\notepad++.exe", "Notepad++", 'N', 0x5C9E31),
        ("Microsoft.Office.OneNote_8wekyb3d8bbwe!microsoft.onenoteim", "OneNote", 'N', 0x7719AA),
        ("Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe!App", tr("Sticky Notes", "Note adesive"), 'S', 0xB8860B),
        ("Mozilla.Firefox", "Firefox", 'F', 0xE66000),
        ("Microsoft.VisualStudioCode", "Visual Studio Code", 'V', 0x0078D4),
        ("Microsoft.Windows.Explorer", tr("File Explorer", "Esplora file"), 'F', 0xE8A317),
        ("Valve.Steam", "Steam", 'S', 0x1B2838),
    ];
    let mut apps = Vec::new();
    let mut icons: Vec<(String, IconBitmap)> = Vec::new();
    for (i, (id, name, letter, rgb)) in apps_def.iter().enumerate() {
        apps.push(CatApp::new(id, name, Keys::default(), 10 + u32::try_from(i).unwrap_or(0)));
        icons.push((id.to_lowercase(), crate::preview::mock_icon(*letter, *rgb, 64)));
    }
    let settings = vec![SettingRow::new(0, tr("Display", "Schermo"), Keys::default(), 1)];
    let window = |hwnd: isize, key: &str, name: &str, title: &str| {
        let mut folded = String::new();
        rank::fold(name, &mut folded);
        WindowRow {
            hwnd,
            group_key: key.into(),
            aumid: None,
            name: name.into(),
            name_folded: folded.into(),
            title: Some(title.into()),
            hung: false,
        }
    };
    let notepad_key = "c:\\windows\\system32\\notepad.exe";
    let windows = vec![
        window(0x1001, notepad_key, notepad, tr("Untitled - Notepad", "Senza titolo - Blocco note")),
        window(0x2002, "c:\\tools\\footool.exe", "FooTool", "FooTool - main"),
        window(0x3003, "c:\\program files\\firefox\\firefox.exe", "Firefox", "Dock releases - Mozilla Firefox"),
    ];
    icons.push((format!("bar:{notepad_key}"), crate::preview::mock_icon('N', 0x2B78B8, 64)));
    icons.push((
        format!("bar:{}", windows.get(1).map_or("", |w| &w.group_key)),
        crate::preview::mock_icon('F', 0x6B4FBB, 64),
    ));
    icons.push((
        format!("bar:{}", windows.get(2).map_or("", |w| &w.group_key)),
        crate::preview::mock_icon('F', 0xE66000, 64),
    ));
    let history: Vec<Box<str>> = vec!["ping 1.1.1.1".into()];
    let opts = LauncherOpts::default();
    let cx = Context {
        apps: &apps,
        settings: &settings,
        windows: &windows,
        history: &history,
        known: &runline::KNOWN,
        opts: &opts,
        num: NumberStyle {
            decimal: tr(".", ",").chars().next().unwrap_or('.'),
            group: tr(",", ".").chars().next().unwrap_or(','),
        },
        now_unix: 1_790_000_000,
    };
    // The notepad window belongs to the Notepad app: its row shows the running indicator.
    let mut session = Session::open(&opts, None);
    let long_line = "> powershell -NoProfile -ExecutionPolicy Bypass -File \"C:\\Users\\Public\\Scripts\\cleanup-temp-files.ps1\" \
        -Days 30 -Recurse -Verbose -LogFile \"C:\\Users\\Public\\Logs\\cleanup-temp-files-2026-11.log\" -Confirm:$false";
    let query: &str = match scene {
        PreviewScene::Empty => "",
        PreviewScene::Inactive | PreviewScene::Running => "note",
        PreviewScene::Apps => "calc",
        PreviewScene::Calc => tr("=2.5*4", "=2,5*4"),
        PreviewScene::Run => "> ping 1.1.1.1",
        PreviewScene::RunReview => long_line,
        PreviewScene::Windows => "<",
        PreviewScene::Notice => "zz",
        PreviewScene::FieldPair => "",
    };
    if !query.is_empty() {
        session.chars(query, &cx);
    }
    if scene == PreviewScene::Notice {
        let notice = Item {
            key: ItemKey::of(ProviderId::Apps, "notice"),
            from: ProviderId::Apps,
            kind: Kind::Notice,
            title: tr("PowerToys Command Palette also answers Alt+Space", "PowerToys Command Palette risponde anche ad Alt+Spazio").into(),
            detail: Some(
                tr(
                    "Both launchers would open at once. Turn its Alt+Space shortcut off in PowerToys, or keep this launcher on Alt+Shift+Space.",
                    "I due launcher si aprirebbero insieme. Disattiva la sua scorciatoia Alt+Spazio in PowerToys, oppure lascia questo launcher su Alt+Maiusc+Spazio.",
                )
                .into(),
            ),
            icon: IconRef::Glyph(GlyphId::Info),
            score: Score { tier: 0, learned: Reverse(0), idle: true, kind: 0, rank: 0, len: 0 },
            verbs: VerbSet::empty(),
            payload: Payload::None,
            destructive: false,
        };
        session.set_notices(vec![notice]);
        session.chars("z", &cx);
    }
    let mut l = Launcher::with_session(session);
    for (k, b) in icons {
        l.icons.insert(k, b);
    }
    l.foreground = scene != PreviewScene::Inactive;
    l
}

/// The Q7 pair (SPEC 5.3, "Field size"): the same query in Start's search control, in the launcher's (the same control
/// at the launcher's width) and in the larger candidate, SEARCH_H 48 with a QUERY style of 18/24 in the same face.
struct FieldPair;

/// The query every field of the pair shows.
const PAIR_QUERY: &str = "calc";
/// The candidate's field height and query size (logical px).
const CANDIDATE_H: i32 = 48;
const CANDIDATE_QUERY_PX: i32 = 18;
/// Start's search control width in its 420 px popup (inset 14 each side).
const START_FIELD_W: i32 = 392;
/// A caption above each field (logical px).
const PAIR_CAPTION_H: i32 = 16;
const PAIR_CAPTION_GAP: i32 = 4;
const PAIR_GAP: i32 = 12;

impl FieldPair {
    fn heights() -> [i32; 3] {
        [style::SEARCH_H, style::SEARCH_H, CANDIDATE_H]
    }
}

impl Content for FieldPair {
    fn kind(&self) -> PopupKind {
        PopupKind::Launcher
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let blocks: i32 = Self::heights().iter().map(|h| PAIR_CAPTION_H + PAIR_CAPTION_GAP + h + PAIR_GAP).sum();
        (m.px(style::LAUNCHER_W), m.px(PAIR_GAP + blocks))
    }

    fn paint(&self, s: &mut Surface, _hot: Option<u32>, _pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let mut y = m.px(PAIR_GAP);
        let cases = [
            ("Start 38 / 13", START_FIELD_W, style::SEARCH_H),
            ("Launcher 38 / 13", style::LAUNCHER_W - 24, style::SEARCH_H),
            ("Candidate 48 / 18", style::LAUNCHER_W - 24, CANDIDATE_H),
        ];
        for (i, (caption, fw, fh)) in cases.into_iter().enumerate() {
            s.text(caption, m.px(style::FIELD_X), y, Font::Caption, WHITE, style::FG_TERTIARY);
            y += m.px(PAIR_CAPTION_H + PAIR_CAPTION_GAP);
            let rc = rect_wh(m.px(style::FIELD_X), y, m.px(fw), m.px(fh));
            let radius = m.px(style::R_S);
            s.fill_rr(rc, radius, WHITE, style::FILL_CONTROL);
            s.stroke_rr(rc, radius, m.px(1).max(1), style::RIM_CONTROL);
            let gs = m.px(style::ICON_S);
            let gx = rc.left + if i == 0 { m.px(12) } else { m.px(16) };
            s.glyph(
                gx,
                rc.top + (rc.bottom - rc.top - gs) / 2,
                gs,
                style::FG_SECONDARY,
                &super::popup_startmenu::magnifier,
            );
            if i == 0 {
                // Start's own control (popup_startmenu.rs paint_search): the text 10 px after the glyph, a 200 caret.
                let tx = gx + gs + m.px(10);
                let ty = center_y(&rc, s.line_height(Font::Normal));
                s.text(PAIR_QUERY, tx, ty, Font::Normal, WHITE, style::FG_PRIMARY);
                let cx = tx + s.text_width(PAIR_QUERY, Font::Normal) + m.px(1);
                s.fill_rr(rect_wh(cx, ty, m.px(1).max(1), s.line_height(Font::Normal)), 0, WHITE, 200);
            } else if i == 1 {
                let tx = m.px(style::NAME_X);
                let ty = center_y(&rc, s.line_height(Font::Normal));
                s.text(PAIR_QUERY, tx, ty, Font::Normal, WHITE, style::FG_PRIMARY);
                let cx = tx + s.text_width(PAIR_QUERY, Font::Normal) + m.px(1);
                let ch = m.px(CARET_H);
                s.fill_rr(
                    rect_wh(cx, rc.top + (rc.bottom - rc.top - ch) / 2, m.px(1).max(1), ch),
                    0,
                    WHITE,
                    style::FG_HOT,
                );
            } else {
                let tx = m.px(style::NAME_X);
                let (px, lh) = (m.px(CANDIDATE_QUERY_PX), m.px(24));
                let ty = rc.top + (rc.bottom - rc.top - lh) / 2;
                let tw =
                    s.text_sized(PAIR_QUERY, tx, ty + (lh - px) / 2 - m.px(1), (px, 400), WHITE, style::FG_PRIMARY);
                s.fill_rr(rect_wh(tx + tw, ty, m.px(1).max(1), lh), 0, WHITE, style::FG_HOT);
            }
            y += m.px(fh + PAIR_GAP);
        }
    }

    fn hit(&self, _x: i32, _y: i32) -> Option<u32> {
        None
    }

    fn click(&mut self, _id: u32) -> Reply {
        Reply::None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    // ---- the controller's ports, recorded

    #[derive(Default)]
    struct FakeHook {
        acks: Vec<u32>,
        ends: Vec<u32>,
        shown: Vec<bool>,
        passes: Vec<PassFlags>,
        newest: u32,
        failopen: u32,
        alt: bool,
        /// The type-ahead ring: its post's sequence and its keys.
        ring: (u32, Vec<KeyRec>),
        takes: Vec<u32>,
    }

    impl HookPort for FakeHook {
        fn ack(&mut self, seq: u32) {
            self.acks.push(seq);
        }
        fn end(&mut self, seq: u32) {
            self.ends.push(seq);
        }
        fn shown(&mut self, on: bool) {
            self.shown.push(on);
        }
        fn pass(&mut self, flags: PassFlags) {
            self.passes.push(flags);
        }
        fn seqs(&self) -> (u32, u32) {
            (self.newest, self.failopen)
        }
        fn alt_down(&self) -> bool {
            self.alt
        }
        fn take(&mut self, seq: u32, out: &mut [KeyRec; RING_CAP]) -> usize {
            self.takes.push(seq);
            if seq != self.ring.0 {
                return 0;
            }
            let keys = std::mem::take(&mut self.ring.1);
            for (slot, k) in out.iter_mut().zip(&keys) {
                *slot = *k;
            }
            keys.len().min(RING_CAP)
        }
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Op {
        Open,
        CloseLauncher,
        Inject(Vec<(u16, bool)>),
    }

    #[derive(Default)]
    struct FakeSys {
        now: u32,
        d3d: bool,
        fs: bool,
        open_ok: bool,
        is_open: bool,
        ops: Vec<Op>,
    }

    impl SysPort for FakeSys {
        fn now(&self) -> u32 {
            self.now
        }
        fn d3d_fullscreen(&self) -> bool {
            self.d3d
        }
        fn fullscreen_foreground(&self) -> bool {
            self.fs
        }
        fn launcher_open(&self) -> bool {
            self.is_open
        }
        fn open(&mut self, _pick: MonitorPick, _top_percent: u8) -> bool {
            self.ops.push(Op::Open);
            self.open_ok
        }
        fn close_launcher(&mut self) {
            self.ops.push(Op::CloseLauncher);
        }
        fn inject(&mut self, keys: &[(u16, bool)]) {
            self.ops.push(Op::Inject(keys.to_vec()));
        }
        /// The fake layout: `scan` holds the character a key types (0: none, 0xFFFF: a dead key).
        fn translate(&mut self, rec: &KeyRec) -> Typed {
            match rec.scan {
                0 => Typed::Key,
                0xFFFF => Typed::Dead,
                c => Typed::Text(char::from_u32(u32::from(c)).unwrap().to_string()),
            }
        }
    }

    const VK_RETURN: u16 = 0x0D;

    /// A kept key that types `c`.
    fn ch(c: char) -> KeyRec {
        KeyRec { vk: c.to_ascii_uppercase() as u16, scan: c as u16, ..KeyRec::default() }
    }

    fn enter(mods: Mods) -> KeyRec {
        KeyRec { vk: VK_RETURN, mods, ..KeyRec::default() }
    }

    /// A launcher content opened for the post `seq`.
    fn opened(seq: u32) -> Launcher {
        let mut l = Launcher::with_session(Session::open(&LauncherOpts::default(), None));
        l.open_seq = seq;
        l
    }

    #[test]
    fn replay_builds_events_in_order() {
        use crate::launcher::providers::tests::Fx;
        use crate::launcher::session::{Effect, LaunchReq, ReplayKey};
        let fx = Fx::new();
        let dead = KeyRec { vk: 0xDE, scan: 0xFFFF, ..KeyRec::default() };
        let ring = vec![ch('c'), dead, ch('a'), ch('l'), ch('c'), enter(Mods::NONE)];
        let keys = replay_keys(&ring, &mut FakeSys::default());
        let want = [
            ReplayKey::Text('c'),
            ReplayKey::Text('a'),
            ReplayKey::Text('l'),
            ReplayKey::Text('c'),
            ReplayKey::Key(KeyEvent { vk: VK_RETURN, mods: Mods::NONE, repeat: false, sys: false }),
        ];
        assert_eq!(keys, want, "four characters then Enter; the dead key is dropped");
        // What ToUnicodeEx's answer types.
        assert_eq!(typed_of(1, &[u16::from(b'c'), 0]), Typed::Text("c".into()));
        assert_eq!(typed_of(-1, &[0x5E]), Typed::Dead);
        assert_eq!(typed_of(0, &[0]), Typed::Key, "an arrow");
        assert_eq!(typed_of(1, &[0x0D]), Typed::Key, "Enter");
        assert_eq!(typed_of(1, &[0x01]), Typed::Key, "Ctrl+A");
        assert_eq!(typed_of(2, &[0xD83D, 0xDE00]), Typed::Text("\u{1F600}".into()), "a pair of surrogates");
        // At the first activation: the ring of the open's sequence is taken and acted on.
        let mut l = opened(7);
        let mut hook = FakeHook { ring: (7, ring), ..FakeHook::default() };
        let reply = l.on_focus(true, &fx.cx(), &mut hook, &mut FakeSys::default());
        assert_eq!(hook.takes, [7]);
        assert_eq!(l.session.query_text(), "calc");
        assert_eq!(reply, Reply::Close, "Enter on the rows of \"calc\" launches");
        let calc = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(l.queued, [Effect::Launch(LaunchReq::App(calc.into()))], "the Launch of Calcolatrice");
        // Once per open: a second activation takes nothing.
        l.on_focus(true, &fx.cx(), &mut hook, &mut FakeSys::default());
        assert_eq!(hook.takes, [7]);
        // Losing the foreground ends the window (a no-op after the take).
        l.on_focus(false, &fx.cx(), &mut hook, &mut FakeSys::default());
        assert_eq!(hook.ends, [7]);
    }

    #[test]
    fn replay_takes_only_its_own_sequence() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = opened(7);
        let mut hook = FakeHook { ring: (6, vec![ch('s'), ch('h'), enter(Mods::NONE)]), ..FakeHook::default() };
        let reply = l.on_focus(true, &fx.cx(), &mut hook, &mut FakeSys::default());
        assert_eq!(hook.takes, [7]);
        assert_eq!(l.session.query_text(), "", "the keys of post 6 are never replayed into the open of post 7");
        assert_eq!(reply, Reply::Relayout, "only the focus change");
        assert!(l.queued.is_empty());
    }

    #[test]
    fn replayed_ctrl_shift_enter_never_gesture() {
        use crate::launcher::gesture::Gesture;
        use crate::launcher::providers::tests::Fx;
        use crate::launcher::session::Effect;
        let fx = Fx::new();
        let ctrl_shift = Mods(Mods::CTRL.0 | Mods::SHIFT.0);
        let typed = |l: &mut Launcher| {
            l.session.chars("> cmd /c dir", &fx.cx());
            let key = l.session.selected().unwrap().key;
            l.session.set_review_lines(key, 1);
        };
        // A person's Ctrl+Shift+Enter with the hook's proof runs the line as administrator.
        let mut live = opened(7);
        typed(&mut live);
        let ev = KeyEvent { vk: VK_RETURN, mods: ctrl_shift, repeat: false, sys: false };
        let fx_live = live.session.key(ev, Some(Gesture::for_test(9)), &fx.cx());
        assert!(fx_live.iter().any(|e| matches!(e, Effect::Launch(_))), "{fx_live:?}");
        // The same keys kept by the hook and replayed never carry a gesture: nothing runs.
        let mut l = opened(7);
        typed(&mut l);
        let mut hook = FakeHook { ring: (7, vec![enter(ctrl_shift)]), ..FakeHook::default() };
        let reply = l.on_focus(true, &fx.cx(), &mut hook, &mut FakeSys::default());
        assert_ne!(reply, Reply::Close);
        assert!(!l.queued.iter().any(|e| matches!(e, Effect::Launch(_))), "{:?}", l.queued);
    }

    /// A post of `seq` sent at 1000 and handled at `now`, the hook's newest sequence `newest`.
    fn post(seq: u32, now: u32, newest: u32, sys: FakeSys) -> (OpenPlan, FakeHook, FakeSys) {
        let mut h = FakeHook { newest, ..FakeHook::default() };
        let mut s = FakeSys { now, ..sys };
        let plan = super::chord_flow(seq, 1_000, &mut h, &mut s);
        (plan, h, s)
    }

    fn fresh() -> FakeSys {
        FakeSys { open_ok: true, ..FakeSys::default() }
    }

    #[test]
    fn chord_flow() {
        // 5.2.12 and rule 8, every combination.
        for d3d in [false, true] {
            for fs in [false, true] {
                for pass in [false, true] {
                    assert_eq!(open_plan(PostAction::Drop, d3d, fs, pass), OpenPlan::Nothing);
                    assert_eq!(open_plan(PostAction::Close, d3d, fs, pass), OpenPlan::Close, "closing is always safe");
                    let want = if d3d || (fs && pass) { OpenPlan::Nothing } else { OpenPlan::TryOpen };
                    assert_eq!(open_plan(PostAction::Open, d3d, fs, pass), want, "d3d {d3d} fs {fs} pass {pass}");
                }
            }
        }
        // Through the flow: acknowledged first, whatever follows.
        let (plan, h, s) = post(5, 1_010, 5, fresh());
        assert_eq!((plan, h.acks.as_slice(), s.ops.as_slice()), (OpenPlan::TryOpen, &[5][..], &[Op::Open][..]));
        assert_eq!(post(5, 1_301, 5, fresh()).0, OpenPlan::Nothing, "stale");
        assert_eq!(post(4, 1_010, 5, fresh()).0, OpenPlan::Nothing, "superseded");
        let mut h = FakeHook { newest: 5, failopen: 5, ..FakeHook::default() };
        let failed_open = super::chord_flow(5, 1_000, &mut h, &mut FakeSys { now: 1_010, ..fresh() });
        assert_eq!(failed_open, OpenPlan::Nothing, "failed open");
        let (plan, _, s) = post(5, 1_301, 5, FakeSys { is_open: true, ..fresh() });
        assert_eq!((plan, s.ops.as_slice()), (OpenPlan::Close, &[Op::CloseLauncher][..]), "a stale post still closes");
        // Exclusive D3D always passes; a borderless fullscreen window passes with over_fullscreen "pass" (the default).
        assert_eq!(post(5, 1_010, 5, FakeSys { d3d: true, ..fresh() }).0, OpenPlan::Nothing);
        let (plan, h, s) = post(5, 1_010, 5, FakeSys { fs: true, ..fresh() });
        assert_eq!(plan, OpenPlan::Nothing);
        assert!(s.ops.is_empty(), "nothing opens and nothing is re-injected into a game");
        assert!(h.passes.last().is_some_and(|f| f.has(PassFlags::FULLSCREEN)), "the next chords pass too");
        with_ctrl(|c| c.over_fullscreen_pass = false);
        assert_eq!(post(5, 1_010, 5, FakeSys { fs: true, ..fresh() }).0, OpenPlan::TryOpen, "over_fullscreen \"open\"");
        assert_eq!(post(5, 1_010, 5, FakeSys { d3d: true, ..fresh() }).0, OpenPlan::Nothing, "D3D passes anyway");
        with_ctrl(|c| c.over_fullscreen_pass = true);
    }

    #[test]
    fn every_outcome_but_open_ends_the_typeahead() {
        // Dropped, fullscreen, closing and failed open: the post's window ends at once.
        for (what, seq, now, sys) in [
            ("stale", 7, 1_400, fresh()),
            ("superseded", 6, 1_010, fresh()),
            ("fullscreen", 7, 1_010, FakeSys { fs: true, ..fresh() }),
            ("closing", 7, 1_010, FakeSys { is_open: true, ..fresh() }),
            ("failed open", 7, 1_010, FakeSys { open_ok: false, ..fresh() }),
        ] {
            let (plan, h, _) = post(seq, now, 7, sys);
            assert_eq!(plan == OpenPlan::TryOpen, what == "failed open", "{what}: only the failed open tried");
            assert_eq!(h.ends, [seq], "{what}");
        }
        // An open keeps its window until the activation takes it or the close ends it.
        let (plan, mut h, _) = post(7, 1_010, 7, fresh());
        assert_eq!((plan, h.ends.len(), h.shown.as_slice()), (OpenPlan::TryOpen, 0, &[true][..]));
        let mut l = Launcher::with_session(Session::open(&LauncherOpts::default(), None));
        l.open_seq = with_ctrl(|c| c.open_seq).unwrap();
        assert_eq!(l.open_seq, 7, "the content opens for the post being opened");
        l.finish(&mut h);
        assert_eq!((h.ends.as_slice(), h.shown.as_slice()), (&[7][..], &[true, false][..]));
    }

    #[test]
    fn failed_open_resets_shown_and_reinjects() {
        let mut h = FakeHook { newest: 3, alt: true, ..FakeHook::default() };
        let mut s = FakeSys { now: 1_010, open_ok: false, ..FakeSys::default() };
        super::chord_flow(3, 1_000, &mut h, &mut s);
        assert_eq!(h.shown, [true, false]);
        assert_eq!(h.ends, [3]);
        assert_eq!(s.ops, [Op::Open, Op::Inject(vec![(0x20, false), (0x20, true)])], "Alt still down: Space only");
        let mut h = FakeHook { newest: 3, alt: false, ..FakeHook::default() };
        let mut s = FakeSys { now: 1_010, open_ok: false, ..FakeSys::default() };
        super::chord_flow(3, 1_000, &mut h, &mut s);
        assert_eq!(
            s.ops.last(),
            Some(&Op::Inject(vec![(0x12, false), (0x20, false), (0x20, true), (0x12, true)])),
            "Alt already up: Alt down, Space, Alt up"
        );
        // A successful open injects nothing.
        let (_, _, s) = post(3, 1_010, 3, fresh());
        assert_eq!(s.ops, [Op::Open]);
    }

    fn surface(dpi: u32) -> Surface {
        let mut s = Surface::new(Metrics { dpi }).unwrap();
        s.resize(1000, 800).unwrap();
        s
    }

    /// The "note" scene with eight 44 px rows: a query that matches many (the catalogue and the web row).
    fn many_rows() -> Launcher {
        let mut l = scene_launcher(PreviewScene::Running);
        l.foreground = true;
        l
    }

    #[test]
    fn layout_numbers() {
        let s = surface(96);
        let mut empty = scene_launcher(PreviewScene::Empty);
        assert_eq!(empty.size(&s), (640, 62), "empty: the field alone, 12 + 38 + 12");
        let mut l = many_rows();
        let (w, h) = l.size(&s);
        let n = l.session.rows().len();
        assert!(n >= 5, "the fixture gives five rows or more, got {n}");
        assert_eq!((w, h), (640, 58 + 44 * i32::try_from(n).unwrap() + 12));
        let g = l.geo.borrow().clone();
        assert_eq!((g.field.left, g.field.top, g.field.right, g.field.bottom), (12, 12, 628, 50));
        let first = g.rows.first().unwrap();
        assert_eq!((first.rect.left, first.rect.top, first.rect.right), (12, 58, 628));
        assert_eq!(first.rect.bottom - first.rect.top, 44);
        let m = s.metrics();
        assert_eq!(m.px(style::NAME_X), 56, "the query and every name share the column at 56");
        assert_eq!(hint_right(m, 640), 616);
        assert_eq!(hint_right(m, 880), 856);
        let mut wide = scene_launcher(PreviewScene::Empty);
        wide.set_shape(880, 8);
        assert_eq!(wide.size(&s).0, 880);
    }

    #[test]
    fn eight_rows_are_422_high() {
        // Eight rows of 44 and nothing else: 58 + 8 * 44 + 12.
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::Apps);
        l.max_rows = 8;
        let (_, h) = l.size(&s);
        let n = i32::try_from(l.session.rows().len()).unwrap().min(8);
        assert_eq!(h, 58 + n * 44 + 12);
        assert_eq!(58 + 8 * 44 + 12, 422);
    }

    #[test]
    fn dib_at_144_dpi_within_2_6_mib() {
        // Eight rows with a three-line notice (+32): the tallest panel the launcher makes.
        let s = surface(144);
        let mut l = scene_launcher(PreviewScene::Notice);
        l.max_rows = 8;
        let (w, h) = l.size(&s);
        let rows = l.geo.borrow().rows.len();
        assert!(rows >= 2, "the notice and the web row at least");
        assert!(i64::from(w) * i64::from(h) * 4 <= 2_726_297, "{w} x {h} is more than 2.6 MiB");
        // The worst case by arithmetic: 8 rows, 3 review lines.
        let worst_h = (58 + 7 * 44 + 44 + 2 * 16 + 12) * 3 / 2;
        assert!(i64::from(w) * i64::from(worst_h) * 4 <= 2_726_297, "{w} x {worst_h}");
    }

    #[test]
    fn rows_shrink_to_fit_the_work_area() {
        let s = surface(96);
        let mut l = many_rows();
        l.size(&s);
        let all = l.geo.borrow().rows.len();
        assert!(all >= 4);
        // Room for three rows only.
        l.limit((i32::MAX, 58 + 3 * 44 + 12));
        let (_, h) = l.size(&s);
        assert_eq!(l.geo.borrow().rows.len(), 3);
        assert!(h <= 58 + 3 * 44 + 12, "the panel stays inside the limit: {h}");
        // The selection stays visible when it moves past them.
        assert!(l.geo.borrow().first == 0);
    }

    #[test]
    fn shrinking_waits_but_growing_does_not() {
        let s = surface(96);
        let mut l = many_rows();
        let (_, tall) = l.size(&s);
        l.hold_shrink(true);
        l.session_mut().chars("z", &scene_context_free());
        let (_, held) = l.size(&s);
        assert_eq!(held, tall, "the panel keeps its height while the user types");
        l.hold_shrink(false);
        let (_, free) = l.size(&s);
        assert!(free < tall, "and shrinks once let go");
        // Growing is never held.
        let mut g = scene_launcher(PreviewScene::Empty);
        g.hold_shrink(true);
        assert_eq!(g.size(&s).1, 62);
    }

    /// A context with nothing in it: a keystroke finds no row.
    fn scene_context_free() -> crate::launcher::providers::Context<'static> {
        use crate::launcher::model::NumberStyle;
        static OPTS: std::sync::OnceLock<LauncherOpts> = std::sync::OnceLock::new();
        let opts = OPTS.get_or_init(LauncherOpts::default);
        crate::launcher::providers::Context {
            apps: &[],
            settings: &[],
            windows: &[],
            history: &[],
            known: &crate::launcher::runline::KNOWN,
            opts,
            num: NumberStyle { decimal: '.', group: ',' },
            now_unix: 0,
        }
    }

    #[test]
    fn hit_finds_the_field_and_the_rows() {
        let s = surface(96);
        let mut l = many_rows();
        l.size(&s);
        assert_eq!(l.hit(100, 30), Some(ID_FIELD));
        assert_eq!(l.hit(100, 70), Some(ID_ROW));
        assert_eq!(l.hit(100, 70 + 44), Some(ID_ROW + 1));
        assert_eq!(l.hit(2, 2), None, "the panel's own margin");
        assert!(!l.clickable(ID_FIELD) && l.clickable(ID_ROW));
    }

    #[test]
    fn review_of_a_long_line_takes_extra_lines_and_is_measured() {
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::RunReview);
        let (_, h) = l.size(&s);
        let rows = l.geo.borrow().rows.clone();
        let sel = rows.iter().find(|r| r.lines.len() > 1).expect("the selected run line shows its review");
        assert!(sel.lines.len() <= 3);
        assert_eq!(sel.rect.bottom - sel.rect.top, 44 + 16 * (i32::try_from(sel.lines.len()).unwrap() - 1));
        assert!(h >= 58 + 44 + 32);
        // The session knows the measure: no "too long" refusal for a line that fits.
        assert_eq!(l.session.admin_refusal(), None);
        // The same line while the launcher is not foreground shows no review.
        l.foreground = false;
        l.size(&s);
        assert!(l.geo.borrow().rows.iter().all(|r| r.lines.len() <= 1));
    }

    #[test]
    fn a_review_over_three_lines_is_refused() {
        use crate::launcher::gesture::Refusal;
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::Empty);
        let long = format!("> {}", "WWWWWWW ".repeat(40));
        l.session_mut().chars(&long, &scene_context_free());
        l.foreground = true;
        l.size(&s);
        assert_eq!(l.session.admin_refusal(), Some(Refusal::TooLong));
        assert!(l.geo.borrow().rows.iter().all(|r| r.lines.len() <= 1), "no review: the refusal is the detail");
    }

    #[test]
    fn caret_stays_in_view() {
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::Empty);
        l.foreground = true;
        l.session_mut().chars(&"w".repeat(120), &scene_context_free());
        l.size(&s);
        assert!(l.scroll.get() > 0, "a query wider than the field scrolls");
        let g = l.geo.borrow().clone();
        let (_, room) = l.query_room(&s, &g.field);
        let widths = char_widths(&s, l.session.query_text());
        let caret = prefix_width(&widths, l.session.query_text(), l.session.field().caret());
        assert!(caret - l.scroll.get() <= room, "the caret is inside the room");
        // A short text does not scroll.
        let mut short = scene_launcher(PreviewScene::Apps);
        short.size(&s);
        assert_eq!(short.scroll.get(), 0);
    }

    #[test]
    fn markers_are_visible_and_split_from_plain_text() {
        assert_eq!(marker_text('\u{7}').as_deref(), Some("\u{2407}"));
        assert_eq!(marker_text('\u{0}').as_deref(), Some("\u{2400}"));
        assert_eq!(marker_text('\u{7f}').as_deref(), Some("\u{2421}"));
        assert_eq!(marker_text('\u{202E}').as_deref(), Some("<U+202E>"));
        assert_eq!(marker_text('\u{85}').as_deref(), Some("<U+0085>"));
        assert_eq!(marker_text('a'), None);
        let runs = runs_of("ab\u{202E}cd");
        assert_eq!(runs.len(), 3);
        assert!(!runs[0].marker && runs[1].marker && !runs[2].marker);
        assert_eq!(runs[0].text, "ab");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_stops_past_the_limit() {
        let s = surface(96);
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
        let w = s.text_width("alpha beta gamma delta epsilon", Font::Caption);
        let lines = wrap(&s, text, Font::Caption, w, 10);
        assert!(lines.len() >= 3);
        for l in &lines {
            let line_w: i32 = l.iter().map(|r| s.text_width(&r.text, Font::Caption)).sum();
            assert!(line_w <= w + 4, "a line is within the width: {line_w} > {w}");
        }
        let joined: String = lines.iter().flatten().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, text, "nothing is lost or ellipsized");
        let cut = wrap(&s, text, Font::Caption, w, 2);
        assert_eq!(cut.len(), 3, "one line past the limit tells the caller it does not fit");
        // A word longer than the line breaks inside it.
        let long = wrap(&s, &"x".repeat(200), Font::Caption, w, 10);
        assert!(long.len() >= 2);
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends() {
        let s = surface(96);
        let path = "C:\\Users\\someone\\Documents\\Projects\\dock\\target\\release\\dock.exe";
        let w = s.text_width("C:\\Users\\some…dock.exe", Font::Caption);
        let got = ellipsize_middle(&s, path, Font::Caption, w);
        assert!(got.starts_with("C:\\") && got.ends_with("dock.exe") && got.contains('\u{2026}'), "{got}");
        assert!(s.text_width(&got, Font::Caption) <= w);
        assert_eq!(ellipsize_middle(&s, "short", Font::Caption, w), "short");
    }

    #[test]
    fn the_hint_names_what_enter_does() {
        let l = scene_launcher(PreviewScene::Running);
        let sel = l.session.selected().unwrap();
        assert_eq!(hint_for(sel), Some("Switch · Enter"), "a running app switches");
        let calc = scene_launcher(PreviewScene::Calc);
        assert_eq!(hint_for(calc.session.selected().unwrap()), Some("Copy · Enter"));
        let run = scene_launcher(PreviewScene::Run);
        assert_eq!(hint_for(run.session.selected().unwrap()), Some("Run · Enter"));
        let apps = scene_launcher(PreviewScene::Apps);
        assert_eq!(hint_for(apps.session.selected().unwrap()), Some("Open · Enter"));
    }

    #[test]
    fn wheel_scrolls_without_moving_the_selection_back() {
        let s = surface(96);
        let mut l = many_rows();
        l.limit((i32::MAX, 58 + 3 * 44 + 12));
        l.size(&s);
        let total = l.session.rows().len();
        assert!(total > 3);
        assert_eq!(l.wheel(-1, 0, 0), Reply::Relayout);
        l.size(&s);
        assert_eq!(l.geo.borrow().first, 1, "one notch down is one row");
        assert_eq!(l.wheel(1, 0, 0), Reply::Relayout);
        l.size(&s);
        assert_eq!(l.geo.borrow().first, 0);
        assert_eq!(l.wheel(1, 0, 0), Reply::None, "at the top already");
    }

    #[test]
    fn scrollbar_hides_after_its_ticks() {
        let mut l = many_rows();
        l.bar_ticks.set(SCROLLBAR_TICKS);
        assert_eq!(l.tick(), Reply::None);
        assert_eq!(l.tick(), Reply::Redraw, "hidden: one redraw");
        assert_eq!(l.tick(), Reply::None);
    }

    #[test]
    fn focus_shows_the_caret_and_the_hint_through_a_relayout() {
        let mut l = scene_launcher(PreviewScene::Inactive);
        assert!(!l.foreground);
        assert_eq!(l.focus(true), Reply::Relayout);
        assert_eq!(l.focus(true), Reply::None);
        assert_eq!(l.focus(false), Reply::Relayout);
    }

    #[test]
    fn scenes_are_named_and_all_render() {
        let names = [
            "launcher_empty",
            "launcher_inactive",
            "launcher_apps",
            "launcher_running",
            "launcher_calc",
            "launcher_run",
            "launcher_run_review",
            "launcher_windows",
            "launcher_notice",
            "launcher_field_pair",
        ];
        let s = surface(96);
        for n in names {
            let scene = PreviewScene::from_name(n).unwrap_or_else(|| panic!("{n}"));
            let mut c = preview_new(scene);
            let (w, h) = c.size(&s);
            assert!(w > 0 && h >= 62, "{n}: {w} x {h}");
            let mut s2 = Surface::new(Metrics { dpi: 96 }).unwrap();
            s2.resize(w, h).unwrap();
            c.paint(&mut s2, None, None);
        }
        assert!(PreviewScene::from_name("launcher_nope").is_none());
        assert!(PreviewScene::from_name("popup_tray").is_none());
    }

    #[test]
    fn glyph_shapes_are_inside_the_box() {
        use crate::glyphs::*;
        type Shape = fn(f32, f32) -> bool;
        let shapes: [(&str, Shape); 8] = [
            ("equals", equals_shape),
            ("prompt", prompt_shape),
            ("window", window_shape),
            ("document", document_shape),
            ("image", image_shape),
            ("archive", archive_shape),
            ("executable", executable_shape),
            ("copy", copy_shape),
        ];
        for size in [16, 24, 32] {
            crate::glyphs::set_scale(size);
            for (name, f) in shapes {
                let mut any = false;
                for iu in -16..=48 {
                    for iv in -16..=48 {
                        let (u, v) = (iu as f32 * 0.5, iv as f32 * 0.5);
                        let inside = (0.0..=16.0).contains(&u) && (0.0..=16.0).contains(&v);
                        let hit = f(u, v);
                        assert!(inside || !hit, "{name} at {size}: ({u}, {v}) is outside the 16 grid");
                        any |= hit;
                    }
                }
                assert!(any, "{name} draws something at {size}");
            }
        }
        crate::glyphs::set_scale(16);
    }
}
