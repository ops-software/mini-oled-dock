//! The launcher popup (SPEC-launcher 5.2, 5.3 and 5.5): the content the popup host shows for `PopupKind::Launcher`.
//! L1a-13 added the stub; L1a-14 (this file) is the view: layout and painting of the field, the mode chip, the result rows,
//! the detail and review lines, the hint and the scrollbar, all from the `Session` the content holds. L1a-15 adds the
//! controller: the chord flow from the hook's post to an open launcher (or to nothing, with the type-ahead ended), which
//! reaches the hook and Windows through two small ports, `HookPort` and `SysPort` (real in launcher_sys.rs, recorders
//! in the tests).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use super::{center_y, Button, Content, HitMap, PopupKind, Reply};
use crate::config::LauncherConfig;
use crate::glyphs;
use crate::hook::{self, KeyRec, PassFlags, PostAction, RING_CAP};
use crate::icons::IconBitmap;
use crate::launcher::aliases::AliasTable;
use crate::launcher::arena::{self, Arena, Caps, CatalogView, Kind as ArenaKind, SettingIn};
use crate::launcher::catalog::{self, IconCache};
use crate::launcher::datetime::TimeStyle;
use crate::launcher::field::KeyEvent;
use crate::launcher::frecency::Frecency;
use crate::launcher::gesture::{self, Gesture};
use crate::launcher::history;
use crate::launcher::model::{
    Batch, Choice, CommandId, GlyphId, IconRef, Item, ItemKey, KeyChord, Kind, Mode, Mods, NumberStyle, Payload,
    ProviderId, ProviderMask, Verb, WindowRow,
};
use crate::launcher::powertoys::{self, Pt, PtEvent, PtNotice, PtState};
use crate::launcher::providers::{self, Budget, Context, LauncherOpts};
use crate::launcher::query::{self, PrefixTable};
use crate::launcher::runline;
use crate::launcher::session::{Act, ActEntry, Effect, ReplayKey, Session};
use crate::launcher::strings;
use crate::launcher::style;
use crate::launcher::verbs;
use crate::launcher::web::{self, Engine, Template};
use crate::launcher_sys::{RealHook, RealSys, SettingsArena, SettingsMiss};
use crate::log;
use crate::render::Metrics;
use crate::shell_thread::{IconPlan, LaunchKind, ShellRequest};
use crate::start_search;
use crate::surface::{rect_wh, Font, Surface, WHITE};

/// Height of the empty panel, the field alone (logical px, SPEC 5.2.3).
const EMPTY_H: i32 = style::FIELD_Y + style::FIELD_H + style::FIELD_Y;
/// Height of the mode chip (logical px).
const CHIP_H: i32 = 28;
/// Horizontal padding of the mode chip.
const CHIP_PAD: i32 = style::S_3;
/// Gap between the chip and the query.
const CHIP_GAP: i32 = 8;
/// Height of the caret (logical px, SPEC 5.3).
const CARET_H: i32 = style::FIELD_CARET_H;
/// The caret of the 38 / 13 field of the Q7 record scene.
const PAIR_CARET_H: i32 = 18;
/// Room between the query and the field's right edge.
const FIELD_PAD_R: i32 = 16;
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
/// Covers the panel while the action list is open: a click on it closes only the list.
const ID_BACKDROP: u32 = 2;
const ID_ROW: u32 = 100;
/// The entries of the action list: this plus their index.
const ID_ACTION: u32 = 10_000;

/// The action list (SPEC 5.3): 280 wide, its entries 6 px inside, its right edge 24 px from the panel's.
const ACTION_W: i32 = 280;
const ACTION_PAD: i32 = 6;
/// Between the selected row and the list.
const ACTION_GAP: i32 = 4;
/// Entry-relative: the glyph, and the label.
const ACTION_GLYPH_X: i32 = 6;
const ACTION_LABEL_X: i32 = 34;
/// Between the label and the key name.
const ACTION_KEY_GAP: i32 = 8;
/// The keyboard focus ring: 1.5 px wide, 2 px off the entry, radius 9.
const FOCUS_GAP: i32 = 2;
const FOCUS_RADIUS: i32 = 9;

/// The panel keeps its height while keys come faster than this (`TIMER_LAUNCHER_SHRINK`, SPEC 5.3).
const SHRINK_MS: u32 = 120;
const VK_RETURN: u16 = 0x0D;
/// Win+V opens Windows' clipboard history.
const VK_V: u16 = 0x56;
const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

// ---------------------------------------------------------------- text runs and markers

/// A piece of text drawn in one style: plain, or a marker (SPEC 5.3: a control or bidi character shown visibly).
#[derive(Clone, PartialEq, Eq, Debug)]
struct Run {
    /// A marker's text is static; plain text is owned (a wrapped detail line).
    text: Cow<'static, str>,
    marker: bool,
}

/// The width up to and including each character of `text` as it is drawn, into `out` (a marker counts by its display
/// text): one measurement of the whole text, or, when it holds a marker, of the text as drawn (`shown`, a scratch whose
/// capacity is kept).
fn query_extents(s: &Surface, text: &str, shown: &mut String, out: &mut Vec<i32>) {
    if !text.chars().any(gesture::is_marker) {
        s.char_extents(text, Font::Query, out);
        return;
    }
    shown.clear();
    for c in text.chars() {
        match marker_text(c) {
            Some(m) => shown.push_str(m),
            None => shown.push(c),
        }
    }
    s.char_extents(shown, Font::Query, out);
    // One entry per drawn character became one per character of `text`: the extent after its drawn text (forward, in
    // place: a character's drawn text ends at or after its own position).
    let mut end = 0usize;
    for (j, c) in text.chars().enumerate() {
        end += marker_text(c).map_or(1, |m| m.chars().count());
        let v = out.get(end.saturating_sub(1)).copied().unwrap_or(0);
        if let Some(slot) = out.get_mut(j) {
            *slot = v;
        }
    }
    out.truncate(text.chars().count());
}

/// The width of the characters of `text` before the byte offset `at`, from the cumulative widths of `query_extents`.
fn prefix_width(extents: &[i32], text: &str, at: usize) -> i32 {
    let n = text.get(..at).map_or(0, |t| t.chars().count());
    n.checked_sub(1).map_or(0, |i| extents.get(i).or(extents.last()).copied().unwrap_or(0))
}

/// How a marker character is drawn: control pictures for C0 and DEL, "<U+XXXX>" for the rest.
fn marker_text(c: char) -> Option<&'static str> {
    if !gesture::is_marker(c) {
        return None;
    }
    // The marked set is closed (Cc and the bidi marks), so every text is a literal and nothing is formatted per key.
    Some(match u32::from(c) {
        0x00 => "\u{2400}",
        0x01 => "\u{2401}",
        0x02 => "\u{2402}",
        0x03 => "\u{2403}",
        0x04 => "\u{2404}",
        0x05 => "\u{2405}",
        0x06 => "\u{2406}",
        0x07 => "\u{2407}",
        0x08 => "\u{2408}",
        0x09 => "\u{2409}",
        0x0A => "\u{240A}",
        0x0B => "\u{240B}",
        0x0C => "\u{240C}",
        0x0D => "\u{240D}",
        0x0E => "\u{240E}",
        0x0F => "\u{240F}",
        0x10 => "\u{2410}",
        0x11 => "\u{2411}",
        0x12 => "\u{2412}",
        0x13 => "\u{2413}",
        0x14 => "\u{2414}",
        0x15 => "\u{2415}",
        0x16 => "\u{2416}",
        0x17 => "\u{2417}",
        0x18 => "\u{2418}",
        0x19 => "\u{2419}",
        0x1A => "\u{241A}",
        0x1B => "\u{241B}",
        0x1C => "\u{241C}",
        0x1D => "\u{241D}",
        0x1E => "\u{241E}",
        0x1F => "\u{241F}",
        0x7F => "\u{2421}",
        0x0080 => "<U+0080>",
        0x0081 => "<U+0081>",
        0x0082 => "<U+0082>",
        0x0083 => "<U+0083>",
        0x0084 => "<U+0084>",
        0x0085 => "<U+0085>",
        0x0086 => "<U+0086>",
        0x0087 => "<U+0087>",
        0x0088 => "<U+0088>",
        0x0089 => "<U+0089>",
        0x008A => "<U+008A>",
        0x008B => "<U+008B>",
        0x008C => "<U+008C>",
        0x008D => "<U+008D>",
        0x008E => "<U+008E>",
        0x008F => "<U+008F>",
        0x0090 => "<U+0090>",
        0x0091 => "<U+0091>",
        0x0092 => "<U+0092>",
        0x0093 => "<U+0093>",
        0x0094 => "<U+0094>",
        0x0095 => "<U+0095>",
        0x0096 => "<U+0096>",
        0x0097 => "<U+0097>",
        0x0098 => "<U+0098>",
        0x0099 => "<U+0099>",
        0x009A => "<U+009A>",
        0x009B => "<U+009B>",
        0x009C => "<U+009C>",
        0x009D => "<U+009D>",
        0x009E => "<U+009E>",
        0x009F => "<U+009F>",
        0x061C => "<U+061C>",
        0x200E => "<U+200E>",
        0x200F => "<U+200F>",
        0x202A => "<U+202A>",
        0x202B => "<U+202B>",
        0x202C => "<U+202C>",
        0x202D => "<U+202D>",
        0x202E => "<U+202E>",
        0x2066 => "<U+2066>",
        0x2067 => "<U+2067>",
        0x2068 => "<U+2068>",
        0x2069 => "<U+2069>",
        _ => "\u{FFFD}",
    })
}

/// One character as a run of its own.
fn unit(c: char) -> Run {
    match marker_text(c) {
        Some(text) => Run { text: Cow::Borrowed(text), marker: true },
        None => Run { text: Cow::Owned(c.to_string()), marker: false },
    }
}

/// Appends a run, merging it into the last one of the same kind.
fn push_run(runs: &mut Vec<Run>, r: Run) {
    match runs.last_mut() {
        Some(last) if last.marker == r.marker => last.text.to_mut().push_str(&r.text),
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
    for r in runs {
        x = draw_run(s, &r.text, r.marker, x, y, font, alpha);
    }
    x
}

/// Draws one run at `x` and returns the x after it.
fn draw_run(s: &mut Surface, text: &str, marker: bool, x: i32, y: i32, font: Font, alpha: u32) -> i32 {
    let m = s.metrics();
    let lh = s.line_height(font);
    let w = s.text_width(text, font);
    if marker {
        s.text_w(text, w, x, y, font, WHITE, style::FG_SECONDARY);
        let bar = m.px(2).max(1);
        s.fill_rr(rect_wh(x, y + lh - bar, w, bar), 0, style::ACCENT_ATTENTION, 255);
    } else {
        s.text_w(text, w, x, y, font, WHITE, alpha);
    }
    x + w
}

/// Draws `text` as runs (markers apart from the rest) from `x` and returns the x after it: the runs are slices of
/// `text` and the markers' own static texts, so nothing is allocated.
fn draw_text_runs(s: &mut Surface, text: &str, mut x: i32, y: i32, font: Font, alpha: u32) -> i32 {
    let mut plain = 0;
    for (i, c) in text.char_indices() {
        let Some(shown) = marker_text(c) else { continue };
        if let Some(run) = text.get(plain..i).filter(|r| !r.is_empty()) {
            x = draw_run(s, run, false, x, y, font, alpha);
        }
        x = draw_run(s, shown, true, x, y, font, alpha);
        plain = i + c.len_utf8();
    }
    match text.get(plain..).filter(|r| !r.is_empty()) {
        Some(run) => draw_run(s, run, false, x, y, font, alpha),
        None => x,
    }
}

/// Draws `text` on one line in `max_w` px: ellipsized at the end, or clipped when it holds markers.
fn draw_line(s: &mut Surface, text: &str, x: i32, y: i32, max_w: i32, font: Font, alpha: u32) {
    if text.chars().any(gesture::is_marker) {
        let lh = s.line_height(font);
        s.set_clip(Some(rect_wh(x, y, max_w.max(0), lh)));
        draw_text_runs(s, text, x, y, font, alpha);
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
/// `max_lines`, so the caller can tell a text that does not fit from one that does. The first line is `first_w` wide
/// (narrower than the rest when the row's hint sits beside it), the others `max_w`.
fn wrap(s: &Surface, text: &str, font: Font, first_w: i32, max_w: i32, max_lines: usize) -> Vec<Vec<Run>> {
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
        let limit = if lines.is_empty() { first_w } else { max_w };
        if cur_w + w > limit && !cur.is_empty() {
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

// Number of `Launcher::layout` calls on this thread (the one-layout-per-key gate), and of the GDI text-extent calls the
// field made in the layout and the paint (the field's measurement gate).
#[cfg(test)]
thread_local! {
    static LAYOUTS: Cell<u32> = const { Cell::new(0) };
    static FIELD_EXTENTS: Cell<u64> = const { Cell::new(0) };
}

/// Adds the GDI text-extent calls made since `before` (`surface::EXTENT_CALLS`) to `FIELD_EXTENTS`.
#[cfg(test)]
fn count_field_extents(before: u64) {
    let n = crate::surface::EXTENT_CALLS.with(Cell::get).saturating_sub(before);
    FIELD_EXTENTS.with(|c| c.set(c.get() + n));
}

/// What the last layout decided: rectangles in client px, for painting and for the hit test.
#[derive(Clone, Default)]
struct Geo {
    w: i32,
    h: i32,
    field: RECT,
    /// Where the query starts and how wide its room is, and the mode chip's width (0 without a chip), physical px:
    /// measured by the layout, read by the paint.
    query_x: i32,
    room: i32,
    chip_w: i32,
    /// The visible rows, top to bottom.
    rows: Vec<RowGeo>,
    /// The rows that do not fit are below (or above): the scrollbar's track, and the visible window.
    first: usize,
    total: usize,
    track: RECT,
    /// The action list over the selected row, when it is open.
    actions: Option<ActionGeo>,
}

/// The action list's rectangle and its entries' (client px).
#[derive(Clone)]
struct ActionGeo {
    rect: RECT,
    items: Vec<RECT>,
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
    /// App icons by lower-case id, the bar's by "bar:" and the lower-case group key: the cache's own bitmaps, shared.
    icons: HashMap<String, Rc<IconBitmap>>,
    /// The lower-case app ids whose icons this open asked the shell thread for (`ask_icons`), each once.
    asked: RefCell<HashSet<String>>,
    /// First visible row, and the selection the last layout saw (the view follows a new selection, not a wheel).
    first: Cell<usize>,
    last_sel: Cell<Option<usize>>,
    /// Horizontal scroll of the query (physical px) that keeps the caret in the field.
    scroll: Cell<i32>,
    /// Visible rows of the last layout, for the wheel.
    count: Cell<usize>,
    geo: RefCell<Geo>,
    /// `geo` was laid out by `size` and not yet painted: the paint that follows reads it instead of laying out again.
    fresh: Cell<bool>,
    /// The cumulative widths of the query's characters (`query_extents`), measured by the layout and read by the paint,
    /// and the scratch of the query as drawn when it holds a marker.
    extents: RefCell<Vec<i32>>,
    shown: RefCell<String>,
    /// The mode chip's width for its provider at a DPI, measured once in an open.
    chip_w: Cell<Option<(ProviderId, u32, i32)>>,
    /// The height last given to the host, and whether a shrink waits (120 ms without a keystroke).
    shown_h: i32,
    hold_shrink: bool,
    /// Ticks the scrollbar still shows after a scroll.
    bar_ticks: Cell<u8>,
    /// The sequence of the chord post that opened it (`hook::LAUNCHER_SEQ`): its type-ahead window is ended at the close.
    open_seq: u32,
    /// This open's number (`Ctrl::opens`; a reopen and the alias prompt keep the last `open_seq`, this never repeats):
    /// its files requests carry it and its pump tags every post with it, so a late post of an earlier open is dropped.
    open_no: u32,
    /// The first activation took the type-ahead ring (once per open).
    taken: bool,
    /// The session's effects that run after the close, in order.
    queued: Vec<Effect>,
    /// The window in front when the chord came: the restore and the window menu act on it.
    fg_at_open: isize,
    /// The close gives the foreground back to `fg_at_open` (a `Hide` with restore; never a close without one).
    restore: bool,
    /// The window a Switch activates, in `closing`: before the hide, while the dock still holds the foreground.
    switch: Option<isize>,
    /// Run-history lines forgotten by Shift+Delete, taken out of the controller's history after the key.
    forgotten: Vec<Box<str>>,
    /// The row this open ran (frecency learns it at the close) and the rows Shift+Delete made it forget.
    learn: Option<ItemKey>,
    unlearn: Vec<ItemKey>,
    /// The icon size of the rows at the surface's DPI (`StartIconsFor`).
    icon_px: u32,
    /// The titles of the windows were read for this open (the "<" mode, once).
    titles_read: bool,
    /// The close sent a launch to the shell thread (a refused one may reopen the launcher, `after_failure`).
    launched: bool,
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
            asked: RefCell::new(HashSet::new()),
            first: Cell::new(0),
            last_sel: Cell::new(None),
            scroll: Cell::new(0),
            count: Cell::new(0),
            geo: RefCell::new(Geo::default()),
            fresh: Cell::new(false),
            extents: RefCell::new(Vec::new()),
            shown: RefCell::new(String::new()),
            chip_w: Cell::new(None),
            shown_h: 0,
            hold_shrink: false,
            bar_ticks: Cell::new(0),
            open_seq: 0,
            open_no: 0,
            taken: false,
            queued: Vec::new(),
            fg_at_open: 0,
            restore: false,
            switch: None,
            forgotten: Vec::new(),
            learn: None,
            unlearn: Vec::new(),
            icon_px: style::ICON_M as u32,
            titles_read: false,
            launched: false,
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
    /// into the session in order, and a close they ask for is posted; losing the foreground ends the window (a no-op
    /// after the take).
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
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::focused();
        let mut buf = [KeyRec::default(); RING_CAP];
        let n = hook.take(self.open_seq, &mut buf);
        let keys = replay_keys(buf.get(..n).unwrap_or(&[]), sys);
        if keys.is_empty() {
            return view;
        }
        let fx = self.session.replay(&keys, cx);
        let reply = self.run(fx, cx, sys);
        // Never a close inside the first activation: it comes from WM_ACTIVATE inside the host's SetForegroundWindow
        // (`ui::force_foreground`), whose later variants would take the foreground back from the window the close
        // restores, switches to or launches. The queued effects, the restore and the switch stay as they are and the
        // close is posted: posted messages come before input, so the keys typed after it still follow it.
        if reply == Reply::Close && sys.defer_close(self.open_seq) {
            return view;
        }
        view.merge(reply)
    }

    /// Runs the session's effects in order. The view's, the field's copy, Ctrl+W and the paste act at once; a Switch
    /// waits for `closing` (before the hide); a `Hide` closes, with or without the restore; launches, power commands,
    /// the window menu and the row's copy run after the close, in `finish`.
    fn run(&mut self, fx: Vec<Effect>, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        let mut reply = Reply::None;
        if let Some(key) = self.session.take_learn() {
            self.learn = Some(key);
        }
        for e in fx {
            match e {
                Effect::Relayout => reply = reply.merge(Reply::Relayout),
                Effect::Redraw => reply = reply.merge(Reply::Redraw),
                Effect::Hide { restore } => {
                    self.restore = restore;
                    reply = Reply::Close;
                }
                Effect::Switch(h) => self.switch = Some(h),
                Effect::CloseWindow(h) => sys.close_window(h),
                Effect::CopyNow(text) => sys.copy(&text),
                Effect::Paste => match sys.paste() {
                    Pasted::Text(text) => {
                        let more = self.session.paste(&text, cx);
                        reply = reply.merge(self.run(more, cx, sys));
                    }
                    // Marked private: nothing was read, the field stays as it is and a notice says why.
                    Pasted::Private => reply = reply.merge(self.show_notices(vec![private_paste_row()], cx, sys)),
                    Pasted::Empty => {}
                },
                Effect::ForgetRun(line) => self.forgotten.push(line),
                // Nothing matched: Windows' typo rewrite is read on the shell thread (a 1.4 MB file) and comes back as a
                // late batch of this generation (`converted`).
                Effect::RequestRewrite { gen } => {
                    sys.shell(ShellRequest::LauncherConvert { gen, query: self.session.last_query() });
                }
                // Shift+Delete on an app, a setting or a command: its learned rank goes (`forget_runs`).
                Effect::Forget(key) => self.unlearn.push(key),
                // The PowerToys notice's answer is applied on the dock window, outside this content (`on_pt_message`):
                // the config is written there and the controller follows it.
                Effect::Choose(c) => sys.pt_answer(answer_code(Some(c))),
                Effect::DismissNotice(key) if key == pt_notice_key() => sys.pt_answer(answer_code(None)),
                // An alias saved or removed: config.json is written on the dock window, outside this content and the key
                // handler (`on_alias_message`), which then runs the query again.
                Effect::SaveAlias { word, target } => sys.alias(AliasOp::Save { word, target }),
                Effect::RemoveAlias(word) => sys.alias(AliasOp::Remove(word)),
                // Files (plan L2-F3): one channel send to the pump, never a wait.
                Effect::RequestFiles(r) => sys.files(self.open_no, r),
                Effect::DismissNotice(_) => {}
                e @ (Effect::Launch(_)
                | Effect::LaunchAlias(_)
                | Effect::Copy(_)
                | Effect::Power(_)
                | Effect::WindowMenu
                | Effect::ClipboardHistory
                | Effect::OpenSettings) => {
                    self.queued.push(e);
                }
            }
        }
        // A switch without a hide (none today) is not left waiting for a close.
        if reply != Reply::Close {
            if let Some(h) = self.switch.take() {
                sys.activate(h);
            }
        }
        reply
    }

    /// A key down for the session; `gesture` is `gesture_for`'s.
    fn on_key(&mut self, ev: KeyEvent, gesture: Option<Gesture>, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        let fx = self.session.key(ev, gesture, cx);
        let reply = self.run(fx, cx, sys);
        self.typed(reply, sys)
    }

    /// After a keystroke: a new layout waits to shrink until no key came for `SHRINK_MS`.
    /// Notice rows learned while this launcher is open: they show after the others, in order, over the same query.
    fn show_notices(&mut self, rows: Vec<Item>, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        for row in rows {
            self.session.push_notice(row);
        }
        let fx = self.session.refresh(cx);
        self.run(fx, cx, sys)
    }

    fn typed(&mut self, reply: Reply, sys: &mut dyn SysPort) -> Reply {
        if reply == Reply::Relayout {
            self.hold_shrink = true;
            sys.arm(crate::msg::TIMER_LAUNCHER_SHRINK, SHRINK_MS);
        }
        reply
    }

    /// Alt+F4, SC_CLOSE and WM_CLOSE: the final Esc (the restore included).
    fn on_close_request(&mut self, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        let fx = self.session.close_request();
        self.run(fx, cx, sys)
    }

    /// The chord while open: the final Esc.
    fn chord_again_on(&mut self, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        let fx = self.session.chord_again();
        self.run(fx, cx, sys)
    }

    /// The close `on_focus` posted for the open of `seq` (`WM_DOCK_LAUNCHER_CLOSE`): Close for that open, nothing for a
    /// launcher a later post opened.
    fn close_posted(&self, seq: u32) -> Reply {
        if seq == self.open_seq {
            Reply::Close
        } else {
            Reply::None
        }
    }

    /// The host is closing the popup, still shown: a Switch activates its window now.
    fn closing_on(&mut self, sys: &mut dyn SysPort) {
        if let Some(h) = self.switch.take() {
            sys.activate(h);
        }
    }

    /// `ShellEvent::LauncherConverted`: Windows read the query of `gen` as `meant`. Taken only while `gen` is the
    /// current generation and it still has no rows, so nothing can move under the selection. `meant` comes from a file
    /// in the user's profile (untrusted, start_search.rs), so it is searched as a name and nothing else (its rows carry `rank::T_REWRITE`): no
    /// prefix, and only the catalogue's app and setting rows come in, as one late batch of that generation. A run line,
    /// a path, a URL, an answer or a command the user never typed never does (C4); the web row stays the typed query's.
    pub(crate) fn converted(&mut self, gen: u64, meant: &str, cx: &Context) -> Reply {
        if gen != self.session.gen() || self.session.has_matches() {
            return Reply::None;
        }
        let no_prefixes = PrefixTable::from_pairs(&std::collections::BTreeMap::new());
        let q = query::parse_query(meant, &no_prefixes, cx.num, gen);
        let mut items = providers::query_all(&q, cx, &mut Budget::default()).into_sorted();
        items.retain(|i| {
            matches!((i.from, i.kind), (ProviderId::Apps, Kind::App) | (ProviderId::Settings, Kind::Setting))
        });
        if items.is_empty() {
            return Reply::None;
        }
        for i in &mut items {
            i.score.tier = crate::launcher::rank::T_REWRITE;
        }
        let batch = Batch { gen, from: ProviderId::Apps, items, done: true };
        if self.session.late(batch, cx) {
            Reply::Relayout
        } else {
            Reply::None
        }
    }

    /// `ShellEvent::LauncherFiles`: the pump's post, for the session (a batch of the current generation is merged by the
    /// stability rule; anything of an older one is dropped). A post of another open's pump (`open` is not this open's
    /// number: it came after its close) is dropped whole: generations restart at every open, and Off carries none.
    pub(crate) fn files_event(&mut self, open: u32, ev: crate::launcher::files::FilesEvent, cx: &Context) -> Reply {
        if open != self.open_no {
            return Reply::None;
        }
        #[cfg(feature = "owner-probe")]
        let probe = match &ev {
            crate::launcher::files::FilesEvent::Batch(b) if b.gen == self.session.gen() => {
                Some((b.gen, b.items.len(), self.session.selected().map(|r| r.key)))
            }
            _ => None,
        };
        let fx = self.session.files_event(ev, cx);
        #[cfg(feature = "owner-probe")]
        if let Some((gen, n, before)) = probe {
            let moved = before != self.session.selected().map(|r| r.key);
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::late_batch(gen, n, moved);
        }
        fx.iter().fold(Reply::None, |r, e| match e {
            Effect::Relayout => r.merge(Reply::Relayout),
            Effect::Redraw => r.merge(Reply::Redraw),
            _ => r,
        })
    }

    /// The forgotten run lines leave the controller's history (outside the context's borrow).
    fn forget_runs(&mut self) {
        if self.forgotten.is_empty() && self.unlearn.is_empty() {
            return;
        }
        let lines = std::mem::take(&mut self.forgotten);
        let keys = std::mem::take(&mut self.unlearn);
        with_ctrl(|c| {
            let before = c.history.len();
            c.history.retain(|h| !lines.contains(h));
            c.dirty |= c.history.len() != before;
            for key in keys {
                c.frec_dirty |= c.frecency.forget(key);
            }
        });
    }

    /// The chord again while open (the controller's `OpenPlan::Close`): the final Esc.
    pub(crate) fn chord_again(&mut self) -> Reply {
        ctx(|cx| self.chord_again_on(cx, &mut RealSys)).unwrap_or(Reply::Close)
    }

    /// The close, with the popup already hidden, in this order: the hook stops treating the launcher as shown and the
    /// type-ahead window of the open's post ends (a no-op when the first activation took it); with the restore, the window
    /// the user came from gets the foreground back; then the queued effects. So a copy lands where the user came from,
    /// and launches leave the foreground to Explorer's grant.
    fn finish(&mut self, hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
        // The files pump ends with the open (D2): its helper is retired on the pump thread.
        sys.files_end();
        hook.shown(false);
        hook.end(self.open_seq);
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::closed();
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::icon_cache(icon_cache_bytes());
        if self.restore && self.fg_at_open != 0 {
            sys.restore(self.fg_at_open);
        }
        let mut elevated = false;
        // A setting's target and arguments come from the settings arena this open searched (its rows carry the entry
        // index): it is still held here, and never replaced while the launcher is open.
        let setting = |i: u32| with_ctrl(|c| setting_target(c.settings.as_ref()?, i)).flatten();
        if let Some(key) = self.learn.take() {
            with_ctrl(|c| c.learn_pick(key));
        }
        for e in std::mem::take(&mut self.queued) {
            // An alias's launch is the same request as a row's, never recorded in the run history (plan L2-A2).
            let (e, alias) = match e {
                Effect::LaunchAlias(r) => (Effect::Launch(r), true),
                e => (e, false),
            };
            match e {
                Effect::Launch(r) => {
                    let elevates = r.elevates();
                    elevated |= elevates;
                    if let Some(line) = run_line_of(&r).filter(|_| !alias) {
                        with_ctrl(|c| c.dirty |= c.learn && history::push_front(&mut c.history, line));
                    }
                    if let Some(req) = crate::launcher_sys::to_request(r, &setting) {
                        self.launched = true;
                        sys.shell(req);
                    }
                    if elevates {
                        // The secure desktop of the UAC prompt can lose the releases the hook would see.
                        hook.reset_mods();
                    }
                }
                Effect::Power(id) => sys.power(id),
                Effect::WindowMenu => sys.window_menu(self.fg_at_open),
                // Windows' own history: the dock sends the keys and reads nothing (plan L2 D8).
                Effect::ClipboardHistory => sys.win_key(VK_V),
                Effect::Copy(text) => sys.copy(&text),
                Effect::OpenSettings => sys.open_settings(),
                _ => {}
            }
        }
        // keep_query: the query as typed, in memory only, for the next open; never after an admin launch.
        let session = &self.session;
        with_ctrl(|c| {
            c.last_query = (c.opts.keep_query && !elevated).then(|| session.last_query()).filter(|q| !q.is_empty());
        });
    }

    /// After a close, with the popup hidden (the host's `after_close`, after `finish` took the launches' targets): the icons
    /// the rows used stay in the cache; the settings mapping and the window rows go (L3 T19: mapped and listed again at
    /// every open, nothing of the open kept), and the next hidden trim is re-armed for the small blocks; a changed run
    /// history or frecency is written 2 s later, the query of a close that launched something is forgotten then (its
    /// reopen window is over), and a catalogue older than 10 minutes is listed again.
    fn housekeeping(&mut self, sys: &mut dyn SysPort) {
        // The icons child of this open's rows ends with it (L3 T20): what it has not made yet is asked for again by the
        // next open that shows those rows.
        if !self.asked.get_mut().is_empty() {
            sys.shell(ShellRequest::LauncherClosed);
        }
        let used: Vec<String> = self
            .session
            .rows()
            .iter()
            .take(self.max_rows)
            .filter_map(|i| match &i.icon {
                IconRef::App(id) => Some(id.to_lowercase()),
                _ => None,
            })
            .collect();
        let now = sys.now_ms();
        let launched = std::mem::take(&mut self.launched);
        let failed_query = launched.then(|| self.session.last_query());
        let plan = with_ctrl(|c| {
            for id in &used {
                c.icons.touch(id);
            }
            c.hidden_at_ms = now;
            c.failed_query = failed_query;
            let gone = (c.settings.take(), std::mem::take(&mut c.windows));
            (c.dirty || c.frec_dirty, c.catalogue_stale(now), gone)
        });
        let (save, stale, gone) = plan.unwrap_or_default();
        drop(gone);
        crate::mem::rearm();
        // The save timer also ends the reopen window of a launch (`save_state` forgets the query as typed).
        if save || launched {
            sys.arm(crate::msg::TIMER_LAUNCHER_SAVE, SAVE_MS);
        }
        if stale {
            with_ctrl(|c| c.listing_since = Some(now));
            sys.shell(list_request());
        }
    }

    /// In the "<" mode the titles of the windows are read, once per open: the rows are searched again with them. In every
    /// other mode no title is ever read.
    fn read_titles(&mut self, reply: Reply, sys: &mut dyn SysPort) -> Reply {
        if self.titles_read || self.session.mode() != Mode::Only(ProviderId::Windows) {
            return reply;
        }
        if with_ctrl(|c| sys.fill_titles(&mut c.windows)).is_none() {
            return reply;
        }
        self.titles_read = true;
        let again = ctx(|cx| {
            let fx = self.session.refresh(cx);
            self.run(fx, cx, sys)
        });
        reply.merge(again.unwrap_or(Reply::None))
    }

    /// The session the keys and the late batches go to (L1a-15).
    // The chord flow of L1a-15 reaches the session through it; until then only the tests do.
    #[allow(dead_code)]
    pub(crate) fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Panel width in logical px and the visible rows (the settings of L1a-11).
    pub(crate) fn set_shape(&mut self, width: i32, rows: u8) {
        self.width = width.clamp(*style::WIDTH_RANGE.start(), *style::WIDTH_RANGE.end());
        self.max_rows =
            usize::from(rows).clamp(usize::from(*style::ROWS_RANGE.start()), usize::from(*style::ROWS_RANGE.end()));
    }

    /// An icon arrived: an app id, or "bar:" + group key, lower-case (L1a-16).
    pub(crate) fn set_icon(&mut self, key: &str, bmp: impl Into<Rc<IconBitmap>>) {
        self.icons.insert(key.to_lowercase(), bmp.into());
    }

    /// A shrink waits (while the user types) or is let go (120 ms without a keystroke): growing is never held.
    pub(crate) fn hold_shrink(&mut self, on: bool) {
        self.hold_shrink = on;
    }

    // ------------------------------------------------------------ layout

    /// The lines under the name of a row: a notice's text or the selected row's review, wrapped; empty for the others.
    fn detail_lines(&self, s: &Surface, item: &Item, selected: bool) -> Vec<Vec<Run>> {
        let (first_w, max_w) = self.detail_widths(s, item, selected);
        if item.kind == Kind::Notice {
            let Some(text) = item.detail.as_deref() else { return Vec::new() };
            let mut lines = wrap(s, text, Font::Caption, first_w, max_w, MAX_LINES);
            if lines.len() > MAX_LINES {
                lines.truncate(MAX_LINES);
                if let Some(last) = lines.last_mut() {
                    push_run(last, Run { text: Cow::Borrowed("\u{2026}"), marker: false });
                }
            }
            return lines;
        }
        if !selected || !self.foreground {
            return Vec::new();
        }
        match review_text(item) {
            Some(text) => {
                let lines = wrap(s, text, Font::Caption, first_w, max_w, MAX_LINES);
                if lines.len() > MAX_LINES {
                    // Too long to show whole: Run as administrator is refused, and the detail says why (5.3).
                    let why = self.session.admin_refusal().and_then(gesture::Refusal::detail);
                    why.map(|d| vec![runs_of(d)]).unwrap_or_default()
                } else {
                    lines
                }
            }
            None => Vec::new(),
        }
    }

    /// The widths of the detail lines of a row (physical px): the first, and the others. The hint of the selected row
    /// sits at the right of the name's line and reaches into the first detail line, so that line stops before the
    /// hint's column (the name's own limit); the lines below it have the whole width.
    fn detail_widths(&self, s: &Surface, item: &Item, selected: bool) -> (i32, i32) {
        let m = s.metrics();
        let rest = hint_right(m, self.width) - m.px(style::NAME_X);
        match hint_for(item).filter(|_| selected && self.foreground) {
            Some(hint) => (rest - s.text_width(hint, Font::Caption) - m.px(HINT_GAP), rest),
            None => (rest, rest),
        }
    }

    /// How many CAPTION lines the review of the selected row needs, capped one past the limit (the session's measure).
    fn review_lines(&self, s: &Surface) -> Option<u8> {
        let item = self.session.selected()?;
        let text = review_text(item)?;
        let (first_w, max_w) = self.detail_widths(s, item, true);
        let n = wrap(s, text, Font::Caption, first_w, max_w, MAX_LINES).len().min(MAX_LINES + 1);
        u8::try_from(n.max(1)).ok()
    }

    /// Places everything for the surface's metrics and the session as it is now.
    fn layout(&self, s: &Surface) -> Geo {
        #[cfg(test)]
        LAYOUTS.with(|c| c.set(c.get() + 1));
        let m = s.metrics();
        let w = m.px(self.width);
        let inset = m.px(style::S_4);
        let field =
            rect_wh(m.px(style::FIELD_X), m.px(style::FIELD_Y), w - 2 * m.px(style::FIELD_X), m.px(style::FIELD_H));
        #[cfg(test)]
        let before = crate::surface::EXTENT_CALLS.with(Cell::get);
        let (query_x, room, chip_w) = self.query_room(s, &field);
        self.follow_caret(s, room);
        #[cfg(test)]
        count_field_extents(before);
        let rows = self.session.rows();
        let total = rows.len();
        if total == 0 {
            self.count.set(0);
            self.first.set(0);
            self.last_sel.set(self.session.selected_index());
            return Geo { w, h: m.px(EMPTY_H), field, query_x, room, chip_w, ..Geo::default() };
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
        let mut h = list_bottom + inset;
        // The action list hangs under the selected row, or over it when it would not fit; the panel grows to hold it.
        let actions = self.session.action_list().map(|list| {
            let pad = m.px(ACTION_PAD);
            let item_h = m.px(style::ROW_COMPACT);
            let n = i32::try_from(list.items().len()).unwrap_or(0);
            // A notice's choices are long sentences: the list widens to show them whole, within the panel.
            let longest = list
                .items()
                .iter()
                .filter(|e| matches!(e.act, Act::Choice(_)))
                .map(|e| s.text_width(act_face(e).0, Font::Normal) + m.px(ACTION_LABEL_X + ACTION_GLYPH_X) + 2 * pad)
                .max()
                .unwrap_or(0);
            let room = w - m.px(style::HINT_RIGHT_INSET) - m.px(style::FIELD_X);
            let ow = m.px(ACTION_W).max(longest.min(room));
            let oh = 2 * pad + n * item_h;
            let (row_top, row_bottom) =
                shown.iter().find(|r| Some(r.idx) == sel).map_or((top, top), |r| (r.rect.top, r.rect.bottom));
            let at =
                overlay_top(row_top, row_bottom, oh, self.max_h, field.bottom + m.px(ACTION_GAP), m.px(ACTION_GAP));
            let rect = rect_wh(w - m.px(style::HINT_RIGHT_INSET) - ow, at, ow, oh);
            h = h.max(rect.bottom + inset);
            let items =
                (0..n).map(|k| rect_wh(rect.left + pad, rect.top + pad + k * item_h, ow - 2 * pad, item_h)).collect();
            ActionGeo { rect, items }
        });
        let track = RECT {
            left: w - m.px(style::SCROLLBAR_X_INSET) - m.px(style::SCROLLBAR_W),
            top,
            right: w - m.px(style::SCROLLBAR_X_INSET),
            bottom: list_bottom,
        };
        Geo { w, h, field, query_x, room, chip_w, rows: shown, first, total, track, actions }
    }

    /// Keeps the caret inside the query's room (the field's width less the chip and the margins, `query_room`).
    fn follow_caret(&self, s: &Surface, room: i32) {
        let m = s.metrics();
        let text = self.session.query_text();
        let mut extents = self.extents.borrow_mut();
        query_extents(s, text, &mut self.shown.borrow_mut(), &mut extents);
        let caret_x = prefix_width(&extents, text, self.session.field().caret());
        let whole = extents.last().copied().unwrap_or(0);
        drop(extents);
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

    /// Where the query starts (physical px), how wide its room is, and the mode chip's width (0 without a chip).
    fn query_room(&self, s: &Surface, field: &RECT) -> (i32, i32, i32) {
        let m = s.metrics();
        let mut tx = m.px(style::NAME_X);
        let chip = self.session.chip().map(|p| self.chip_width(s, p));
        if let Some(cw) = chip {
            tx += cw + m.px(CHIP_GAP);
        }
        (tx, field.right - m.px(FIELD_PAD_R) - tx, chip.unwrap_or(0))
    }

    /// `chip_width` of `p`, measured once per provider and DPI in an open (the label does not change while it is open).
    fn chip_width(&self, s: &Surface, p: ProviderId) -> i32 {
        let dpi = s.metrics().dpi;
        if let Some((kept, at, w)) = self.chip_w.get() {
            if kept == p && at == dpi {
                return w;
            }
        }
        let w = chip_width(s, p);
        self.chip_w.set(Some((p, dpi, w)));
        w
    }

    // ------------------------------------------------------------ painting

    fn paint_field(&self, s: &mut Surface, g: &Geo, hot: bool) {
        let m = s.metrics();
        let f = g.field;
        let r = m.px(style::R_S);
        s.fill_rr(f, r, WHITE, if hot { style::FILL_CONTROL_HOT } else { style::FILL_CONTROL });
        s.stroke_rr(f, r, m.px(1).max(1), style::RIM_CONTROL);
        // The magnifier is centred in the icon column the rows use.
        let gs = m.px(style::FIELD_ICON);
        let gx = m.px(style::ICON_X) + (m.px(style::ICON_M) - gs) / 2;
        let gy = f.top + (f.bottom - f.top - gs) / 2;
        s.glyph_cached(GlyphId::Magnifier as u32, gx, gy, gs, style::FG_SECONDARY, glyph_shape(GlyphId::Magnifier));
        let lit = self.foreground;
        let text_alpha = if lit { style::FG_PRIMARY } else { style::FG_TERTIARY };
        if let Some(p) = self.session.chip() {
            let cw = g.chip_w;
            let ch = m.px(CHIP_H);
            let rc = rect_wh(m.px(style::NAME_X), f.top + (f.bottom - f.top - ch) / 2, cw, ch);
            s.fill_rr(rc, m.px(style::R_XS), WHITE, style::FILL_CONTROL_HOT);
            s.stroke_rr(rc, m.px(style::R_XS), m.px(1).max(1), style::RIM_CONTROL);
            if let Some(label) = strings::chip(p) {
                let alpha = if lit { style::FG_SECONDARY } else { style::FG_TERTIARY };
                // The layout measured the label: the chip is its width with the padding either side.
                s.text_w(
                    label,
                    cw - 2 * m.px(CHIP_PAD),
                    rc.left + m.px(CHIP_PAD),
                    center_y(&rc, s.line_height(Font::Caption)),
                    Font::Caption,
                    WHITE,
                    alpha,
                );
            }
        }
        let (tx, room) = (g.query_x, g.room);
        let lh = s.line_height(Font::Query);
        let ty = center_y(&f, lh);
        let ch = m.px(CARET_H);
        let cy = f.top + (f.bottom - f.top - ch) / 2;
        let scroll = self.scroll.get();
        let text = self.session.query_text();
        let mut extents = self.extents.borrow_mut();
        s.set_clip(Some(rect_wh(tx, f.top, room.max(0), f.bottom - f.top)));
        if text.is_empty() {
            match self.session.chip() {
                None => s.text(strings::placeholder(), tx, ty, Font::Query, WHITE, style::FG_TERTIARY),
                Some(ProviderId::Files) => {
                    s.text(strings::files_placeholder(), tx, ty, Font::Query, WHITE, style::FG_TERTIARY)
                }
                Some(_) => {}
            }
        } else {
            let field = self.session.field();
            // The layout of this paint measured the text; a text of another length is measured again.
            if extents.len() != text.chars().count() {
                query_extents(s, text, &mut self.shown.borrow_mut(), &mut extents);
            }
            if let Some(sel) = field.selection() {
                let before = prefix_width(&extents, text, sel.start);
                let upto = prefix_width(&extents, text, sel.end);
                let rc = rect_wh(tx + before - scroll, cy, upto - before, ch);
                s.fill_rr(rc, 0, WHITE, style::FILL_TEXT_SELECTION);
            }
            // Only the characters in the room are drawn: a long query is wider than the text scratch.
            let (mut at, mut seen) = (0, None::<(usize, usize, i32)>);
            for ((b, ch_), edge) in text.char_indices().zip(extents.iter()) {
                let w = edge - at;
                if at + w > scroll && at < scroll + room {
                    let first = seen.map_or((b, at), |(from, _, x0)| (from, x0));
                    seen = Some((first.0, b + ch_.len_utf8(), first.1));
                }
                at = *edge;
            }
            if let Some((from, to, start_x)) = seen {
                if let Some(visible) = text.get(from..to) {
                    draw_text_runs(s, visible, tx + start_x - scroll, ty, Font::Query, text_alpha);
                }
            }
        }
        let caret = self.session.field().caret();
        if lit && caret > 0 {
            let x = tx + prefix_width(&extents, text, caret) - scroll + m.px(1);
            s.fill_rr(rect_wh(x, cy, m.px(1).max(1), ch), 0, WHITE, style::FG_HOT);
        }
        s.set_clip(None);
        if self.session.files_busy() {
            // The files request is taking long (FilesEvent::Slow): three dots in the right padding of the field.
            let (d, gap) = (m.px(2).max(1), m.px(2).max(1));
            let x0 = f.right - m.px(FIELD_PAD_R) - m.px(2);
            let y = f.top + (f.bottom - f.top - d) / 2;
            for i in 0..3 {
                s.fill_rr(rect_wh(x0 + i * (d + gap), y, d, d), d / 2, WHITE, style::FG_TERTIARY);
            }
        }
        if lit && caret == 0 {
            // Before the first character (or the placeholder): the clip starts at `tx`, so this one is drawn outside it.
            let x = tx - m.px(1).max(1);
            s.fill_rr(rect_wh(x, cy, m.px(1).max(1), ch), 0, WHITE, style::FG_HOT);
        }
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
                s.glyph_cached(*g as u32, x + off, box_y + off, gs, alpha, glyph_shape(*g));
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
        // An app row shows no detail, except an alias's (the app it opens, or "App not found").
        let has_detail = item.detail.is_some() && !g.lines.is_empty()
            || item.detail.is_some() && (item.kind != Kind::App || item.from == ProviderId::Alias);
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
            let (max_w, _) = self.detail_widths(s, item, selected);
            if detail.chars().any(gesture::is_marker) {
                draw_line(s, detail, nx, dy, max_w, Font::Caption, alpha);
            } else {
                let shown = ellipsize_middle(s, detail, Font::Caption, max_w);
                s.text(&shown, nx, dy, Font::Caption, WHITE, alpha);
            }
        }
    }

    /// The app rows of `g` whose icon is neither in hand nor asked for in this open: one `StartIconsFor` for them all
    /// (its reply, `LauncherIcons`, fills the cache and the rows, `on_icons`). Painting is where the rows on screen are
    /// final: a selection moved by an arrow scrolls them with a redraw, without a new size.
    fn ask_icons(&self, g: &Geo, sys: &mut dyn SysPort) {
        let Ok(mut asked) = self.asked.try_borrow_mut() else { return };
        let mut ids = Vec::new();
        for rg in &g.rows {
            let Some(IconRef::App(id)) = self.session.rows().get(rg.idx).map(|i| &i.icon) else { continue };
            let key = id.to_lowercase();
            if !self.icons.contains_key(&key) && asked.insert(key) {
                ids.push(id.to_string());
            }
        }
        if !ids.is_empty() {
            sys.shell(ShellRequest::StartIconsFor { icon_px: self.icon_px, ids });
        }
    }

    /// `Content::paint` on a port: the rows on screen ask for the icons they lack.
    fn paint_on(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>, sys: &mut dyn SysPort) {
        s.panel();
        // After `size` (the render of a key) the layout is the one it stored; any other render lays out again.
        let stored = if self.fresh.replace(false) {
            self.geo.try_borrow_mut().ok().map(|mut slot| std::mem::take(&mut *slot))
        } else {
            None
        };
        let g = stored.unwrap_or_else(|| self.layout(s));
        #[cfg(test)]
        let before = crate::surface::EXTENT_CALLS.with(Cell::get);
        self.paint_field(s, &g, hot == Some(ID_FIELD));
        #[cfg(test)]
        count_field_extents(before);
        let sel = self.session.selected_index();
        for rg in &g.rows {
            let Some(item) = self.session.rows().get(rg.idx) else { continue };
            let id = ID_ROW + u32::try_from(rg.idx).unwrap_or(0);
            self.paint_row(s, rg, item, self.foreground && sel == Some(rg.idx), pressed == Some(id));
        }
        self.paint_scrollbar(s, &g, false);
        self.paint_actions(s, &g, pressed);
        self.ask_icons(&g, sys);
        if let Ok(mut slot) = self.geo.try_borrow_mut() {
            *slot = g;
        }
    }

    /// The action list over the panel (SPEC 5.3): opaque black, the control rim, its entries with their glyph, label and
    /// key name; the highlighted one on a plate, with the focus ring after keyboard input.
    fn paint_actions(&self, s: &mut Surface, g: &Geo, pressed: Option<u32>) {
        let (Some(list), Some(ag)) = (self.session.action_list(), g.actions.as_ref()) else { return };
        let m = s.metrics();
        let bw = m.px(1).max(1);
        s.fill_rr(ag.rect, m.px(style::R_M), (0, 0, 0), 255);
        s.stroke_rr(ag.rect, m.px(style::R_M), bw, style::RIM_CONTROL);
        let (lh, kh) = (s.line_height(Font::Normal), s.line_height(Font::Caption));
        let gs = m.px(style::ICON_S);
        for (k, (rc, e)) in ag.items.iter().zip(list.items()).enumerate() {
            let id = ID_ACTION + u32::try_from(k).unwrap_or(0);
            let lit = k == list.sel();
            if pressed == Some(id) {
                s.plate(*rc, m.px(style::R_S), style::FILL_PRESS);
            } else if lit {
                s.plate(*rc, m.px(style::R_S), style::FILL_HOVER);
            }
            let (label, glyph, chord) = act_face(e);
            let name_alpha = if !e.enabled {
                style::FG_DISABLED
            } else if lit {
                style::FG_HOT
            } else {
                style::FG_PRIMARY
            };
            let gx = rc.left + m.px(ACTION_GLYPH_X);
            let gy = rc.top + (rc.bottom - rc.top - gs) / 2;
            if !e.enabled {
                s.glyph_cached(glyph as u32, gx, gy, gs, style::FG_DISABLED, glyph_shape(glyph));
            } else if e.danger {
                s.glyph_cached_rgb(glyph as u32, gx, gy, gs, style::ACCENT_DANGER, style::FG_HOT, glyph_shape(glyph));
            } else {
                s.glyph_cached(glyph as u32, gx, gy, gs, style::FG_SECONDARY, glyph_shape(glyph));
            }
            let mut label_right = rc.right - m.px(ACTION_GLYPH_X);
            let lx = rc.left + m.px(ACTION_LABEL_X);
            // The key name shows when the whole label still fits beside it: a label cut short says less than a
            // missing hint (Italian "Esegui come amministratore" beside "Ctrl+Maiusc+Invio").
            if let Some(c) = chord {
                let key = strings::key_name(c);
                let kw = s.text_width(&key, Font::Caption);
                if s.text_width(label, Font::Normal) + kw + m.px(ACTION_KEY_GAP) <= label_right - lx {
                    let alpha = if e.enabled { style::FG_TERTIARY } else { style::FG_DISABLED };
                    s.text(&key, label_right - kw, center_y(rc, kh), Font::Caption, WHITE, alpha);
                    label_right -= kw + m.px(ACTION_KEY_GAP);
                }
            }
            let shown = s.ellipsize(label, Font::Normal, label_right - lx);
            s.text(&shown, lx, center_y(rc, lh), Font::Normal, WHITE, name_alpha);
            if lit && list.ring() {
                let g2 = m.px(FOCUS_GAP);
                let ring = RECT { left: rc.left - g2, top: rc.top - g2, right: rc.right + g2, bottom: rc.bottom + g2 };
                s.stroke_rr(ring, m.px(FOCUS_RADIUS), ((m.px(3) + 1) / 2).max(1), style::RIM_FOCUS);
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

/// The top of the action list (physical px): `gap` under the row, or over it when it would not fit below within `room`
/// and still clears `min_top` (the field). When neither fits it sits as low as `room` allows, over the rows.
fn overlay_top(row_top: i32, row_bottom: i32, h: i32, room: i32, min_top: i32, gap: i32) -> i32 {
    let below = row_bottom + gap;
    if below + h <= room {
        return below;
    }
    let above = row_top - gap - h;
    if above >= min_top {
        return above;
    }
    (room - h).max(min_top)
}

/// The label, glyph and key name of an entry of the action list.
fn act_face(e: &ActEntry) -> (&'static str, GlyphId, Option<KeyChord>) {
    match e.act {
        Act::Verb(v) => {
            let spec = verbs::spec(v);
            let label = if e.new_window { strings::verb_new_window() } else { (spec.label)() };
            let chord = if e.new_window { Some(KeyChord { vk: VK_RETURN, mods: Mods::SHIFT }) } else { spec.chord };
            (label, spec.glyph, chord)
        }
        // Enter runs a notice's first choice, Take Alt+Space (5.7).
        Act::Choice(c @ Choice::TakeChord) => {
            (strings::choice_label(c), GlyphId::Magnifier, Some(KeyChord { vk: VK_RETURN, mods: Mods::NONE }))
        }
        Act::Choice(c @ Choice::LeaveChord) => (strings::choice_label(c), GlyphId::Exit, None),
        Act::Choice(c @ Choice::UseAltShiftSpace) => (strings::choice_label(c), GlyphId::Gear, None),
        Act::DontShowAgain => (strings::verb_dont_show_again(), GlyphId::Info, None),
        Act::AssignAlias => (strings::verb_assign_alias(), GlyphId::Prompt, None),
        Act::RemoveAlias => (strings::verb_remove_alias(), GlyphId::Info, None),
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
    if let (Kind::Notice, Payload::Choices(choices)) = (item.kind, &item.payload) {
        return match choices.first() {
            Some(Choice::TakeChord) => Some(strings::hint_pt_take()),
            Some(_) => Some(strings::hint_open()),
            None => None,
        };
    }
    let running = matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_));
    let verb = verbs::primary(item.kind, running);
    if !item.verbs.has(verb) {
        // An executable file row has no Open (file_verbs): Ctrl+Enter shows it in its folder.
        let is_path = matches!(item.kind, Kind::File | Kind::Folder);
        return (is_path && item.verbs.has(Verb::Reveal)).then(strings::hint_reveal);
    }
    if item.from == ProviderId::Alias && item.kind == Kind::Command {
        // The alias prompt's row (alias rows themselves are apps, addresses, paths and lines).
        return Some(strings::hint_save());
    }
    Some(match (item.kind, verb) {
        // A known console program runs in a console kept open (5.4.9), and says so.
        (Kind::RunLine | Kind::RunSuggestion, _) => match &item.payload {
            Payload::RunLine(r) if runline::runs_in_console(r) => strings::hint_run_in_console(),
            _ => strings::hint_run(),
        },
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
        let px = u32::try_from(s.metrics().px(style::ICON_M)).unwrap_or(24);
        if px != self.icon_px {
            self.icon_px = px;
            // The size the cache keeps new icons at (a silent try: the controller may be borrowed by the key being handled).
            CTRL.with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    c.icon_px = px;
                }
            });
        }
        // The review of the selected row is measured with the real font, for the session's conditions, before the one
        // layout of this key: the layout painted is the layout whose height is returned.
        if let (Some(lines), Some(key)) = (self.review_lines(s), self.session.selected().map(|i| i.key)) {
            self.session.set_review_lines(key, lines);
        }
        let g = self.layout(s);
        let mut h = g.h;
        if self.hold_shrink && h < self.shown_h {
            h = self.shown_h;
        }
        self.shown_h = h;
        let w = g.w;
        *self.geo.borrow_mut() = g;
        self.fresh.set(true);
        (w, h)
    }

    fn limit(&mut self, max: (i32, i32)) {
        self.max_h = max.1;
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        self.paint_on(s, hot, pressed, &mut RealSys);
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        let g = self.geo.try_borrow().ok()?;
        let mut map = HitMap::default();
        map.add(ID_FIELD, g.field);
        for r in &g.rows {
            map.add(ID_ROW + u32::try_from(r.idx).unwrap_or(0), r.rect);
        }
        // Over the open action list: the backdrop (a click there closes only the list), then its entries.
        if let Some(a) = &g.actions {
            map.add(ID_BACKDROP, rect_wh(0, 0, g.w, g.h));
            for (k, rc) in a.items.iter().enumerate() {
                map.add(ID_ACTION + u32::try_from(k).unwrap_or(0), *rc);
            }
        }
        map.at(x, y)
    }

    fn clickable(&self, id: u32) -> bool {
        id >= ID_ROW
    }

    fn click(&mut self, id: u32) -> Reply {
        let mut sys = RealSys;
        let now = sys.now_ms();
        if id == ID_BACKDROP || id >= ID_ACTION {
            let entry = id.checked_sub(ID_ACTION).and_then(|k| usize::try_from(k).ok());
            return ctx(|cx| {
                let fx = match entry {
                    Some(k) => self.session.action_click(k, cx),
                    None => self.session.close_actions(),
                };
                let reply = self.run(fx, cx, &mut sys);
                self.typed(reply, &mut sys)
            })
            .unwrap_or(Reply::None);
        }
        let Some(row) = id.checked_sub(ID_ROW).and_then(|r| usize::try_from(r).ok()) else { return Reply::None };
        ctx(|cx| {
            let fx = self.session.click(row, now, cx);
            self.run(fx, cx, &mut sys)
        })
        .unwrap_or(Reply::None)
    }

    /// A right click on a row opens its action list; with the list open it only closes it.
    fn button(&mut self, id: u32, which: Button) -> Reply {
        if which != Button::Right {
            return Reply::None;
        }
        let row = id.checked_sub(ID_ROW).filter(|_| id < ID_ACTION).and_then(|r| usize::try_from(r).ok());
        let mut sys = RealSys;
        ctx(|cx| {
            let fx = match (self.session.action_list().is_some(), row) {
                (true, _) => self.session.close_actions(),
                (false, Some(r)) => self.session.context_menu(r),
                (false, None) => Vec::new(),
            };
            let reply = self.run(fx, cx, &mut sys);
            self.typed(reply, &mut sys)
        })
        .unwrap_or(Reply::None)
    }

    fn pointer(&mut self, x: i32, y: i32) -> Reply {
        if self.session.action_list().is_some() {
            // Over the list the pointer lights an entry; the rows behind it are not hot.
            let k = self.hit(x, y).and_then(|id| id.checked_sub(ID_ACTION)).and_then(|k| usize::try_from(k).ok());
            return match k.map(|k| self.session.action_hover(k)) {
                Some(fx) if !fx.is_empty() => Reply::Redraw,
                _ => Reply::None,
            };
        }
        let row = self.hit(x, y).and_then(|id| id.checked_sub(ID_ROW)).and_then(|r| usize::try_from(r).ok());
        let mut sys = RealSys;
        let now = sys.now_ms();
        ctx(|cx| {
            let fx = self.session.pointer(x, y, row, now);
            self.run(fx, cx, &mut sys)
        })
        .unwrap_or(Reply::None)
    }

    fn key_ex(&mut self, ev: KeyEvent) -> Option<Reply> {
        let mut sys = RealSys;
        let gesture = gesture_for(&ev, &mut RealHook, &sys);
        let reply = ctx(|cx| self.on_key(ev, gesture, cx, &mut sys));
        self.forget_runs();
        reply.map(|r| self.read_titles(r, &mut sys))
    }

    fn char(&mut self, c: char) -> Reply {
        let mut sys = RealSys;
        ctx(|cx| {
            let mut buf = [0u8; 4];
            let fx = self.session.chars(c.encode_utf8(&mut buf), cx);
            let reply = self.run(fx, cx, &mut sys);
            self.typed(reply, &mut sys)
        })
        .map_or(Reply::None, |r| self.read_titles(r, &mut sys))
    }

    fn close_request(&mut self) -> Reply {
        ctx(|cx| self.on_close_request(cx, &mut RealSys)).unwrap_or(Reply::Close)
    }

    fn wheel(&mut self, notches: i32, _x: i32, _y: i32) -> Reply {
        if self.session.action_list().is_some() {
            return Reply::None;
        }
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
            Some(r) => self.read_titles(r, &mut RealSys),
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

    fn closing(&mut self) {
        self.closing_on(&mut RealSys);
    }

    fn after_close(&mut self) {
        self.finish(&mut RealHook, &mut RealSys);
        self.housekeeping(&mut RealSys);
        pt_closed(&mut RealHook, &mut RealSys);
    }

    fn launcher(&mut self) -> Option<&mut Launcher> {
        Some(self)
    }
}

/// The content of a new launcher popup (`popup::make_content`), for the post the controller is opening.
pub(crate) fn new() -> Box<dyn Content> {
    Box::new(new_launcher())
}

/// A launcher for the post being opened, with the config's options, shape and the kept query (fully selected, so
/// typing replaces it).
fn new_launcher() -> Launcher {
    let made = with_ctrl(|c| {
        let mut l = Launcher::with_session(Session::open(&c.opts, c.last_query.as_deref()));
        l.open_seq = c.open_seq;
        c.opens = c.opens.wrapping_add(1);
        l.open_no = c.opens;
        l.fg_at_open = c.fg_at_open;
        l.set_shape(c.width, c.opts.rows);
        // The icons kept between opens, shared with the cache (no pixel is copied): rows that have one never ask again.
        l.icons.reserve(c.icons.len());
        for (k, b) in c.icons.iter() {
            l.icons.insert(k.to_string(), Rc::clone(b));
        }
        l
    });
    let mut l = made.unwrap_or_else(|| Launcher::with_session(Session::open(&LauncherOpts::default(), None)));
    // The error of a launch refused while the launcher was away, a held Alt+Shift+Space and the PowerToys notice show
    // once, at this open.
    let (failed, held, pt) =
        with_ctrl(|c| (c.notice.take(), std::mem::take(&mut c.chord_held), c.pt.take_notice())).unwrap_or_default();
    let notices: Vec<Item> = failed
        .map(|_| failure_row())
        .into_iter()
        .chain(held.then(chord_held_row))
        .chain(pt.map(pt_notice_row))
        .collect();
    let noticed = !notices.is_empty();
    if noticed {
        l.session.set_notices(notices);
    }
    if noticed || !l.session.query_text().is_empty() {
        // The kept query's rows, and the notices (on an empty field too).
        let _ = ctx(|cx| l.session.refresh(cx));
    }
    l
}

/// The key of the PowerToys notice (one row, whichever text it shows).
fn pt_notice_key() -> ItemKey {
    ItemKey::of(ProviderId::Apps, "powertoys")
}

/// The PowerToys notice rows (5.7): Command Palette also uses Alt+Space (Enter takes it, the action list holds the other
/// choices), or it did not answer and the launcher took the chord (information only).
fn pt_notice_row(n: PtNotice) -> Item {
    let mut row = match n {
        PtNotice::Shared => notice_row("powertoys", strings::pt_shared(), Some(strings::pt_turn_off())),
        PtNotice::TookOver => notice_row("powertoys", strings::pt_took_over(), Some(strings::pt_took_over_detail())),
    };
    if n == PtNotice::Shared {
        row.verbs = crate::launcher::model::VerbSet::empty().with(Verb::Open);
        row.payload = Payload::Choices(&[Choice::TakeChord, Choice::LeaveChord, Choice::UseAltShiftSpace]);
    }
    row
}

/// The wParam of `WM_DOCK_LAUNCHER_PT` for the notice's answer (None: Don't show again); 0 is a chord owner's exit.
fn answer_code(a: Option<Choice>) -> usize {
    match a {
        Some(Choice::TakeChord) => 1,
        Some(Choice::LeaveChord) => 2,
        Some(Choice::UseAltShiftSpace) => 3,
        None => 4,
    }
}

/// The answer of a `WM_DOCK_LAUNCHER_PT` wParam; None for an exit (0) or an unknown value.
fn answer_of(code: usize) -> Option<Option<Choice>> {
    Some(match code {
        1 => Some(Choice::TakeChord),
        2 => Some(Choice::LeaveChord),
        3 => Some(Choice::UseAltShiftSpace),
        4 => None,
        _ => return None,
    })
}

/// Once, at the end of `popup::init`: the config's chord and options.
pub(crate) fn init() {
    on_config();
}

/// At the end of `popup::shutdown`, while the dock window still exists.
pub(crate) fn shutdown() {
    if REGISTERED.with(Cell::get) {
        crate::launcher_sys::unwatch_sessions();
    }
    crate::launcher_sys::release_owner_waits();
}

/// One of the launcher's one-shot timers (`TIMER_LAUNCHER_*`) fired on the dock window (`launcher_sys::timer_window`):
/// killed first.
pub(crate) fn on_timer(hwnd: HWND, id: usize) {
    // SAFETY: a timer of one of our windows (same thread); killing an id that is not armed fails harmlessly.
    let _ = unsafe { KillTimer(Some(hwnd), id) };
    match id {
        crate::msg::TIMER_LAUNCHER_SHRINK => {
            super::with_launcher(|l| {
                l.hold_shrink(false);
                Reply::Relayout
            });
        }
        crate::msg::TIMER_LAUNCHER_WARM => {
            if is_on() {
                // B7: an open or Start may have listed the catalogue already; the warm timer lists nothing twice.
                if with_ctrl(|c| c.warm_due()) == Some(true) {
                    ask_catalogue();
                }
                // The settings cache made sure of (built by a child when missing or stale), so the first open finds the
                // rows at its first keystroke; only with the Settings provider on.
                if with_ctrl(|c| warm_settings_due(&c.opts)) == Some(true) {
                    start_search::ensure_settings_cache(false);
                }
                warm_window();
            }
        }
        crate::msg::TIMER_LAUNCHER_SAVE => save_state(),
        crate::msg::TIMER_LAUNCHER_PT => {
            if is_on() {
                pt_timer(&mut RealHook, &mut RealSys);
            }
        }
        _ => {}
    }
}

/// A launch is refused (`ShellEvent::LaunchFailed`) up to this long after the hide: the launcher reopens with the error.
const REOPEN_WITHIN_MS: u64 = 1_000;

/// What a refused launch does (5.5.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FailurePlan {
    /// The launcher reopens with the same query and the error row.
    Reopen,
    /// A notice row shows at the next open; the launcher never reopens by itself.
    NoticeLater,
}

/// The plan for a refusal that came `ms_since_hide` after the close: within 1 s, with the foreground still the window of
/// the open (or none), the user is still waiting for the launch; later, or somewhere else, a reopen would steal their typing.
pub(crate) fn after_failure(ms_since_hide: u64, fg_is_ours_or_null: bool) -> FailurePlan {
    if ms_since_hide <= REOPEN_WITHIN_MS && fg_is_ours_or_null {
        FailurePlan::Reopen
    } else {
        FailurePlan::NoticeLater
    }
}

/// The error row of a refused launch (no action: Enter on it only closes).
fn failure_row() -> Item {
    notice_row("launch-failed", strings::launch_failed(), None)
}

/// The row of a refused "Use Alt+Shift+Space instead": another app holds that chord (no action).
fn chord_held_row() -> Item {
    notice_row("chord-held", strings::pt_alt_shift_space_held(), None)
}

/// The row of a paste refused because the app that copied the text marked it private (no action).
fn private_paste_row() -> Item {
    notice_row("private-paste", strings::paste_private(), None)
}

/// A notice row (5.5.2): an info mark, the text, no verb.
fn notice_row(id: &str, title: &str, detail: Option<&str>) -> Item {
    use crate::launcher::model::{ItemKey, Score, VerbSet};
    Item {
        key: ItemKey::of(ProviderId::Apps, id),
        from: ProviderId::Apps,
        kind: Kind::Notice,
        title: title.into(),
        detail: detail.map(Into::into),
        icon: IconRef::Glyph(GlyphId::Info),
        score: Score { tier: 0, learned: std::cmp::Reverse(0), idle: true, kind: 0, rank: 0, len: 0 },
        verbs: VerbSet::empty(),
        payload: Payload::None,
        destructive: false,
    }
}

/// `ShellEvent::LaunchFailed` (only the launcher's own requests push it): within a second of the hide, with the foreground
/// still where the open came from, the launcher reopens with the same query and the error; otherwise the error waits as a
/// notice for the next open.
pub(crate) fn on_launch_failed(kind: LaunchKind) {
    if !is_on() {
        return;
    }
    log!("launcher: a launch was refused ({kind:?})");
    let now = RealSys.now_ms();
    let fg = RealSys.foreground();
    let held = with_ctrl(|c| (now.saturating_sub(c.hidden_at_ms), c.fg_at_open, c.failed_query.take()));
    let Some((since, fg_open, query)) = held else { return };
    let ours_or_null = fg == 0 || fg == fg_open;
    match (after_failure(since, ours_or_null), query) {
        (FailurePlan::Reopen, Some(q)) => reopen_with_error(&q),
        _ => {
            with_ctrl(|c| c.notice = Some(kind));
        }
    }
}

/// Opens the launcher again as the chord would (no type-ahead: no key of the user is kept for it) and shows the error over
/// the same query.
fn reopen_with_error(query: &str) {
    let mut hook = RealHook;
    let mut sys = RealSys;
    let Some((pick, top_percent)) = with_ctrl(|c| (c.pick, c.top_percent)) else { return };
    sys.prepare_open();
    hook.shown(true);
    if !sys.open(pick, top_percent) {
        hook.shown(false);
        return;
    }
    with_ctrl(|c| c.notice = None);
    super::with_launcher(|l| {
        ctx(|cx| {
            l.session.set_notices(vec![failure_row()]);
            let fx = if query.is_empty() { l.session.refresh(cx) } else { l.session.chars(query, cx) };
            l.run(fx, cx, &mut RealSys)
        })
        .unwrap_or(Reply::None)
    });
}

/// Opens the launcher on the alias prompt in its "word target" form (Settings > Launcher's "Add…", L2-A3): a
/// programmatic open like `reopen_with_error`'s, with what a chord's open reads of the window in front (the window the
/// close gives the foreground back to: Settings), the window check and the locale styles. Only while a chord is set.
pub(crate) fn open_alias_prompt() {
    if alias_prompt_flow(&mut RealHook, &mut RealSys) {
        super::with_launcher(|l| {
            ctx(|cx| {
                let fx = l.session.alias_prompt(cx);
                l.run(fx, cx, &mut RealSys)
            })
            .unwrap_or(Reply::None)
        });
    }
}

/// `open_alias_prompt`'s open on the ports: true when a launcher is open for the prompt.
fn alias_prompt_flow(hook: &mut dyn HookPort, sys: &mut dyn SysPort) -> bool {
    if !is_on() {
        return false;
    }
    if sys.launcher_open() {
        return true;
    }
    let Some((pick, top_percent)) = with_ctrl(|c| (c.pick, c.top_percent)) else { return false };
    let fg = sys.foreground();
    let (num, time_style, utc_offset_min) = (sys.number_style(), sys.time_style(), sys.utc_offset_min());
    let window_ok = sys.window_target(fg);
    with_ctrl(|c| {
        c.fg_at_open = fg;
        c.window_ok = window_ok;
        c.num = num;
        c.time_style = time_style;
        c.utc_offset_min = utc_offset_min;
    });
    sys.prepare_open();
    hook.shown(true);
    if !sys.open(pick, top_percent) {
        hook.shown(false);
        return false;
    }
    true
}

/// A chord is set: the launcher's state exists.
pub(crate) fn is_on() -> bool {
    ON.with(Cell::get)
}

/// The pixel bytes of the icons kept between opens (the probe's line at each close).
#[cfg(feature = "owner-probe")]
fn icon_cache_bytes() -> usize {
    with_ctrl(|c| c.icons.bytes()).unwrap_or(0)
}

/// The launcher was opened in this process: its window, surface and paint code are paged in (`warm_window`).
fn note_open() {
    OPENED.with(|o| o.set(true));
}

fn opened_in_process() -> bool {
    OPENED.with(Cell::get)
}

/// The warm timer warms the window only while no popup is open (the shared window is in use, and a hidden paint would
/// cost its user) and only when no open has paged the code in yet.
fn warm_window_due(popup_open: bool, opened: bool) -> bool {
    !popup_open && !opened
}

/// The warm timer makes sure of the settings cache only with the Settings provider on: otherwise the settings child would
/// run after every start for nothing.
fn warm_settings_due(opts: &LauncherOpts) -> bool {
    opts.mask.has(ProviderId::Settings)
}

/// `TIMER_LAUNCHER_WARM`, after the listing (L3 T22, B6): the first open after a start would page in the window, the
/// surface and the paint code on the chord's path. Here instead, off screen and only until the first open of the process:
/// the launcher's window is created hidden, an empty launcher is laid out and painted on a surface at the target monitor's
/// DPI, and the surface is dropped at once. Nothing stays but the window (the one every open uses); the transient pages go back at the next hidden trim.
/// Not warmed: the settings rows, the keyboard focus (TSF) and the icons, which would stay resident in every session.
fn warm_window() {
    if !warm_window_due(super::current().is_some(), opened_in_process()) {
        return;
    }
    let Some(pick) = with_ctrl(|c| c.pick) else { return };
    // The probe's A/B of live check L-3 (c) skips the warm itself, nothing else.
    #[cfg(feature = "owner-probe")]
    let skip = crate::launcher_probe::no_warm();
    #[cfg(not(feature = "owner-probe"))]
    let skip = false;
    if !skip {
        let dpi = crate::launcher_sys::monitor_dpi(crate::launcher_sys::target_monitor(pick));
        #[cfg(feature = "owner-probe")]
        let t0 = std::time::Instant::now();
        super::warm_launcher(dpi);
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::warmed(t0);
    }
    crate::mem::rearm();
}

/// `popup::warm_launcher`'s layout and paint on `s`: see `warm_on`.
pub(crate) fn warm_content(s: &mut Surface) -> bool {
    warm_on(s, &mut RealSys)
}

/// A throwaway launcher with the config's options and shape and an empty field, laid out and painted once on `s`. It
/// takes nothing an open shows (no pending notice, no kept query, no icon) and asks for nothing: an empty field has no
/// rows. False when the controller is busy or the surface cannot be sized.
fn warm_on(s: &mut Surface, sys: &mut dyn SysPort) -> bool {
    let Some(mut l) = with_ctrl(|c| {
        let mut l = Launcher::with_session(Session::open(&c.opts, None));
        l.set_shape(c.width, c.opts.rows);
        l
    }) else {
        return false;
    };
    // The rows' icon size at this DPI already: `size` then leaves the cache's icon size to the opens.
    l.icon_px = u32::try_from(s.metrics().px(style::ICON_M)).unwrap_or(l.icon_px);
    let (w, h) = l.size(s);
    if let Err(e) = s.resize(w.max(1), h.max(1)) {
        log!("launcher: the warm surface could not be sized: {e}");
        return false;
    }
    l.paint_on(s, None, None, sys);
    true
}

/// Asks the shell thread for the catalogue, unless an answer is on its way.
fn ask_catalogue() {
    let now = RealSys.now_ms();
    let ask = with_ctrl(|c| {
        let waiting = c.listing_since.is_some_and(|t| now.saturating_sub(t) < LISTING_RETRY_MS);
        if !waiting {
            c.listing_since = Some(now);
        }
        !waiting
    });
    if ask == Some(true) {
        crate::shell_thread::request(list_request());
    }
}

/// `ShellEvent::StartApps` (`popup::on_start_apps`): the launcher keeps the arena the shell thread listed and checked
/// (folded and indexed in the `--start-apps` child, nothing built here on the UI thread, L3 KEY-11), shared with
/// nobody once Start has made its rows; swapped in at the next keystroke. Nothing happens without a chord.
pub(crate) fn on_catalog(apps: Arc<Arena>) {
    if !is_on() {
        return;
    }
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::Catalog);
    let (n, bytes) = (apps.len(), apps.bytes());
    let now = RealSys.now_ms();
    if with_ctrl(|c| {
        c.apps = apps;
        c.listed_at_ms = now.max(1);
        c.listing_since = None;
    })
    .is_some()
    {
        log!("launcher: catalogue of {n} apps, {bytes} B");
    }
}

/// `ShellEvent::StartSettings`: the settings cache file is good again (or Start's list came). An open launcher that holds
/// no settings takes them now (new rows appear at the next keystroke, under the stability rule); one that holds them
/// keeps what it has until it closes, so an Enter never meets an entry index of another file.
pub(crate) fn on_settings_ready() {
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::SettingsReady);
    if is_on() {
        settings_ready_on(super::current() == Some(PopupKind::Launcher), &mut RealSys);
    }
}

/// `on_settings_ready` with the launcher open (`open`) or not: only an open launcher that holds no settings maps.
fn settings_ready_on(open: bool, sys: &mut dyn SysPort) {
    if open && with_ctrl(|c| c.opts.mask.has(ProviderId::Settings) && c.settings.is_none()) == Some(true) {
        take_settings(sys);
    }
}

/// The settings of an open (L3 T19), before its window shows: with the Settings provider on, the cache file mapped and
/// checked; nothing with it off. Whatever an earlier open left (none: the close drops it) goes first.
fn settings_at_open(sys: &mut dyn SysPort) {
    let Some((on, stale)) = with_ctrl(|c| {
        c.rebuild_asked = false;
        (c.opts.mask.has(ProviderId::Settings), c.settings.take())
    }) else {
        return;
    };
    drop(stale);
    if on {
        take_settings(sys);
    }
}

/// The launcher's settings (L3 T18, T19): the version 2 cache file mapped read-only and checked whole against the key the
/// settings thread computed (the names, keywords and dictionaries were folded and indexed in the `--start-settings` child:
/// nothing is built here on the UI thread, KEY-11; 0 private bytes while held). A missing file, or one that fails, is not
/// used: the settings thread is asked to make it (rebuilt when there was one) at most once per open, so a file that keeps
/// failing cannot loop children, and the rows join when `StartSettings` comes (5.4.7).
fn take_settings(sys: &mut dyn SysPort) {
    match sys.settings_bytes() {
        Ok(a) => {
            with_ctrl(|c| c.settings = Some(a));
        }
        Err(miss) => {
            let first = with_ctrl(|c| !std::mem::replace(&mut c.rebuild_asked, true)) == Some(true);
            if let (true, Some(rebuild)) = (first, miss.rebuild()) {
                sys.ensure_settings(rebuild);
            }
        }
    }
}

/// The launch target and arguments of entry `i` of the held settings.
fn setting_target(settings: &SettingsArena, i: u32) -> Option<(String, Option<String>)> {
    let i = usize::try_from(i).ok().filter(|&i| i < settings.len())?;
    let (target, args) = settings.view().target(i);
    (!target.is_empty()).then(|| (target.to_string(), args.map(str::to_string)))
}

/// `ShellEvent::LauncherIcons`: kept in the cache (at the size of the rows) and shown at once when the launcher is open.
pub(crate) fn on_icons(icon_px: u32, icons: Vec<(String, Option<IconBitmap>)>) {
    if !is_on() {
        return;
    }
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::Icons);
    let mut got = Vec::new();
    let px = with_ctrl(|c| c.icon_px).unwrap_or(icon_px);
    for (id, bmp) in icons {
        let Some(b) = bmp else { continue };
        let b = if icon_px != px && px != 0 { crate::icons::resize_area(&b, px) } else { b };
        if !b.is_empty() {
            // One bitmap, shared by the cache and the open rows.
            let b = Rc::new(b);
            with_ctrl(|c| c.icons.insert(&id, Rc::clone(&b)));
            got.push((id, b));
        }
    }
    if got.is_empty() {
        return;
    }
    super::with_launcher(|l| {
        for (id, b) in got {
            l.set_icon(&id, b);
        }
        Reply::Redraw
    });
}

/// `ShellEvent::LauncherState`: the run history of the shell user (the learned ranks come in L1b). Lines added since the
/// open stay first.
pub(crate) fn on_state(frecency: Option<Vec<u8>>, run: Option<Vec<u8>>) {
    if !is_on() {
        return;
    }
    with_ctrl(|c| {
        if !c.learn {
            return;
        }
        let mut loaded = run.map_or_else(Vec::new, |b| history::parse_run(&b));
        for line in c.history.iter().rev() {
            history::push_front(&mut loaded, line);
        }
        c.history = loaded;
        c.state_arrived = true;
        // Picks made before the file arrived are added to what it holds.
        let mut learned = frecency.map_or_else(Frecency::new, |b| Frecency::parse(&b));
        learned.absorb(&c.frecency);
        c.frec_dirty |= !c.frecency.is_empty();
        c.frecency = learned;
    });
}

/// A PowerToys chord owner is running (Settings > Launcher shows its row only then). With the chord off the launcher's
/// state is not looked at (nor built): Settings refreshes call this.
pub(crate) fn pt_detected() -> bool {
    ON.with(Cell::get) && with_ctrl(|c| c.pt.state() != PtState::Absent).unwrap_or(false)
}

/// "Forget history" (Settings > Launcher, L1b-7): the run history and every learned rank leave the memory and the files.
/// With the chord off nothing is in memory (and the state is not built for this): only the files are deleted.
pub(crate) fn forget_history() {
    if ON.with(Cell::get) {
        let cleared = with_ctrl(|c| {
            c.history.clear();
            c.frecency.clear();
            c.dirty = false;
            c.frec_dirty = false;
        });
        if cleared.is_none() {
            return;
        }
    }
    crate::shell_thread::request(ShellRequest::LauncherStateSave { frecency: None, run: None, delete: true });
}

/// `TIMER_LAUNCHER_SAVE`: the query of the last launching close is forgotten (its reopen window is over), and a changed
/// history is written (the shell thread writes it for the shell user).
fn save_state() {
    let now = RealSys.now_ms();
    let waiting = with_ctrl(|c| {
        c.reopen_over(now);
        // The files were not read yet: a write now would replace them with the picks alone. The picks stay in memory
        // and are absorbed on load.
        c.learn && (c.dirty || c.frec_dirty) && !c.state_arrived
    })
    .unwrap_or(false);
    if waiting {
        crate::launcher_sys::arm_once(crate::msg::TIMER_LAUNCHER_SAVE, SAVE_MS);
        return;
    }
    let req = with_ctrl(|c| {
        let (run, frec) = (std::mem::take(&mut c.dirty), std::mem::take(&mut c.frec_dirty));
        if !(run || frec) || !c.learn {
            return None;
        }
        Some(ShellRequest::LauncherStateSave {
            frecency: frec.then(|| c.frecency.render()),
            run: run.then(|| history::render_run(&c.history)),
            delete: false,
        })
    })
    .flatten();
    if let Some(r) = req {
        crate::shell_thread::request(r);
    }
}

/// What an open needs before its window shows (`SysPort::prepare_open`): the settings mapped from the cache file (with
/// the Settings provider on), the windows listed, the history asked for the first time, the catalogue listed when it has
/// not been.
pub(crate) fn prepare_open() {
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::PrepareOpen);
    note_open();
    settings_at_open(&mut RealSys);
    let windows = crate::launcher_sys::window_rows();
    let (state, listed) = with_ctrl(|c| {
        c.windows = windows;
        c.failed_query = None;
        let state = (!std::mem::replace(&mut c.state_loaded, true)).then_some(c.learn);
        (state, c.apps.is_empty() && c.listing_since.is_none())
    })
    .unwrap_or((None, false));
    match state {
        Some(true) => crate::shell_thread::request(ShellRequest::LauncherStateLoad),
        Some(false) => {
            crate::shell_thread::request(ShellRequest::LauncherStateSave { frecency: None, run: None, delete: true })
        }
        None => {}
    }
    if listed {
        ask_catalogue();
    }
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
    /// `hook::reset_modifiers` (after an admin launch, a lock or a suspend).
    fn reset_mods(&mut self);
    /// `hook::take_gesture_time`: the kb.time of the last physical Ctrl+Shift+Enter, consumed.
    fn gesture_time(&mut self) -> Option<u32>;
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
    /// The foreground window belongs to a remote-desktop or VM client (`powertoys::is_remote_client` of its process's
    /// image, 5.2.13).
    fn remote_foreground(&self) -> bool;
    /// The launcher is the open popup.
    fn launcher_open(&self) -> bool;
    /// Opens the launcher centred on the monitor `pick` names; false when nothing is shown (no popups, no surface).
    fn open(&mut self, pick: MonitorPick, top_percent: u8) -> bool;
    /// The chord while the launcher is open: its close request (the final Esc).
    fn close_launcher(&mut self);
    /// `SendInput` of these (vk, up) events, tagged `hook::INJECT_TAG`.
    fn inject(&mut self, keys: &[(u16, bool)]);
    /// Win + `vk` pressed and released (`ui::send_win_key`: Win extended, every event tagged `hook::INJECT_TAG`).
    fn win_key(&mut self, vk: u16);
    /// What a kept key types with the UI thread's keyboard layout and its recorded modifiers (`ToUnicodeEx`, keyboard
    /// state left unchanged).
    fn translate(&mut self, rec: &KeyRec) -> Typed;
    /// `GetForegroundWindow`.
    fn foreground(&self) -> isize;
    /// `h` can take the window menu: there, not the desktop (Progman, WorkerW), not one of the dock's own windows.
    fn window_target(&self, h: isize) -> bool;
    /// A Switch: the bar's activation of one window (`apps::activate_window`).
    fn activate(&mut self, h: isize);
    /// The foreground back to `h` (`ui::force_foreground`), when it is still visible and not cloaked.
    fn restore(&mut self, h: isize);
    /// The text to the clipboard (`CF_UNICODETEXT`).
    fn copy(&mut self, text: &str);
    /// The clipboard's text, at most 64 KiB of it, unless the app that copied it marked it private (`paste_refused`):
    /// then its text is never read.
    fn paste(&mut self) -> Pasted;
    /// Ctrl+W: `WM_CLOSE` posted to `h`.
    fn close_window(&mut self, h: isize);
    /// The window menu of `h` (`WM_SYSCOMMAND` `SC_KEYMENU` posted), when `h` is a window to act on.
    fn window_menu(&mut self, h: isize);
    /// One request to the shell thread.
    fn shell(&mut self, r: ShellRequest);
    /// Lock, sleep, restart, shut down, sign out (`power::run`).
    fn power(&mut self, id: CommandId);
    /// Settings > Launcher.
    fn open_settings(&mut self);
    /// What an open needs before the window shows: the settings mapped, the open windows listed, the history asked for.
    fn prepare_open(&mut self) {}
    /// The settings cache file mapped and checked against the key the settings thread computed (L3 T19).
    fn settings_bytes(&mut self) -> Result<SettingsArena, SettingsMiss> {
        Err(SettingsMiss::NoKey)
    }
    /// Asks the settings thread to make sure of the cache file (`start_search::ensure_settings_cache`).
    fn ensure_settings(&mut self, _rebuild: bool) {}
    /// Reads the titles of the windows (`InternalGetWindowText`), once per open and only in the "<" mode.
    fn fill_titles(&mut self, _rows: &mut [WindowRow]) {}
    /// One of the launcher's one-shot timers on the popup window.
    fn arm(&mut self, id: usize, ms: u32);
    /// `GetMessageTime` and `GetMessageExtraInfo` of the key message being handled.
    fn msg_time(&self) -> u32;
    fn msg_extra(&self) -> usize;
    /// `GetTickCount64` (the pointer's and the click's clock).
    fn now_ms(&self) -> u64;
    /// The user locale's decimal and thousands marks (`GetLocaleInfoEx`), read at each open.
    fn number_style(&self) -> NumberStyle;
    /// The locale's 12/24-hour choice and the order of its short date (`GetLocaleInfoEx`), read at each open.
    fn time_style(&self) -> TimeStyle {
        TimeStyle::default()
    }
    /// The local time's offset from UTC in minutes (`GetTimeZoneInformation`), read at each open.
    fn utc_offset_min(&self) -> i32 {
        0
    }
    /// Closes the launcher without the session's Hide (no restore).
    fn close_quiet(&mut self);
    /// Posts the close of the open of `seq` (`WM_DOCK_LAUNCHER_CLOSE`), to run outside the message being handled; false
    /// when the post failed.
    fn defer_close(&mut self, seq: u32) -> bool;
    /// PowerToys (L1b-5): whether a chord owner runs now; the real port also waits on each one's exit.
    fn chord_owners(&mut self) -> bool {
        false
    }
    /// The window `h` belongs to a chord owner (the answer check).
    fn owner_window(&self, _h: isize) -> bool {
        false
    }
    /// Posts the PowerToys notice's answer to the dock window (`WM_DOCK_LAUNCHER_PT`, `answer_code`).
    fn pt_answer(&mut self, _code: usize) {}
    /// Holds an alias change for the dock window (one slot, `hold_alias_op`) and posts `WM_DOCK_LAUNCHER_ALIAS` to it:
    /// config.json is written there, never in the key handler.
    fn alias(&mut self, _op: AliasOp) {}
    /// A requery's files request of the open numbered `open` to its pump (`launcher_files::request`, plan L2-F3): a
    /// channel send, never a wait.
    fn files(&mut self, _open: u32, _r: Option<crate::launcher::files::Req>) {}
    /// Every close: the files pump ends (`launcher_files::end`).
    fn files_end(&mut self) {}
}

/// The proof of a person's Ctrl+Shift+Enter (SPEC 5.4.9), built only for that key: the hook's evidence is consumed by
/// it and by nothing else.
fn gesture_for(ev: &KeyEvent, hook: &mut dyn HookPort, sys: &dyn SysPort) -> Option<Gesture> {
    if ev.vk != VK_RETURN || ev.mods != CTRL_SHIFT {
        return None;
    }
    let hook_time = hook.gesture_time();
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::gesture(hook_time, sys.msg_time());
    Gesture::from_key(ev, hook_time, sys.msg_time(), sys.msg_extra(), hook::INJECT_TAG, sys.now())
}

/// What a paste found on the clipboard (`SysPort::paste`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Pasted {
    Text(String),
    /// The app that copied it marked it private (`launcher::field::paste_refused`): its text was never read.
    Private,
    /// No text on the clipboard, or the clipboard could not be opened.
    Empty,
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
pub(crate) enum MonitorPick {
    /// The one under the pointer.
    Pointer,
    /// The one of the foreground window.
    Active,
    Primary,
}

impl MonitorPick {
    /// "pointer", "active", "primary" (the config sanitizes the value; anything else is the pointer's).
    fn from_name(name: &str) -> MonitorPick {
        match name {
            "active" => MonitorPick::Active,
            "primary" => MonitorPick::Primary,
            _ => MonitorPick::Pointer,
        }
    }
}

/// `opts_from_config`, and how many entries of `launcher.aliases` the alias table ignores.
pub(crate) fn opts_with_ignored(c: &LauncherConfig) -> (LauncherOpts, usize) {
    let mut o = opts_from_config_without_aliases(c);
    let (aliases, ignored) = AliasTable::build(&c.aliases, &o.prefixes);
    o.aliases = aliases;
    (o, ignored)
}

/// The session's options from `config.launcher` (pure). A provider the map does not name is on, and apps are always
/// on; the web template is the engine's, or the custom one when it passes the validator (Google otherwise).
#[cfg(test)]
pub(crate) fn opts_from_config(c: &LauncherConfig) -> LauncherOpts {
    opts_with_ignored(c).0
}

fn opts_from_config_without_aliases(c: &LauncherConfig) -> LauncherOpts {
    let mut mask = ProviderMask::all();
    for (name, on) in &c.providers {
        if let Some(id) = ProviderId::from_name(name) {
            mask = mask.with(id, *on);
        }
    }
    let web = match Engine::from_name(&c.web_engine) {
        Some(Engine::Custom) => web::validate_template(&c.web_url).ok(),
        Some(e) => e.template().and_then(|t| web::validate_template(t).ok()),
        None => None,
    };
    LauncherOpts {
        prefixes: PrefixTable::from_pairs(&c.prefixes),
        mask,
        web: web.unwrap_or_else(Template::default),
        keep_query: c.keep_query,
        rows: c.rows.clamp(*style::ROWS_RANGE.start(), *style::ROWS_RANGE.end()),
        aliases: Default::default(),
    }
}

/// Why the chord passes (the hook's pass flags), pure: FULLSCREEN over exclusive D3D always and over a fullscreen window
/// with `over_fullscreen` "pass"; REMOTE for a remote or VM client in front (L1b-6); YIELD while another launcher answers
/// the chord (L1b-5).
fn pass_flags(quns_d3d: bool, fs: bool, over_fullscreen_pass: bool, remote: bool, yield_: bool) -> PassFlags {
    let mut f = PassFlags::default();
    if quns_d3d || (fs && over_fullscreen_pass) {
        f = f.with(PassFlags::FULLSCREEN);
    }
    if remote {
        f = f.with(PassFlags::REMOTE);
    }
    if yield_ {
        f = f.with(PassFlags::YIELD);
    }
    f
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
    /// The sequence of the post being opened and the window in front when its chord came (read by `new`).
    open_seq: u32,
    fg_at_open: isize,
    /// The opens so far: the number of the newest (`Launcher::open_no`).
    opens: u32,
    /// That window can take the window menu (`SysPort::window_target`).
    window_ok: bool,
    /// `launcher.monitor`, `launcher.top_percent` and `launcher.over_fullscreen == "pass"`.
    pick: MonitorPick,
    top_percent: u8,
    over_fullscreen_pass: bool,
    /// The pass flags the hook was last given.
    flags: PassFlags,
    /// keep_query: the last query, in memory only (never written anywhere).
    last_query: Option<String>,
    /// `launcher.width` (logical px).
    width: i32,
    /// The session's view of the config and the locale's number marks.
    opts: LauncherOpts,
    num: NumberStyle,
    /// The locale's 12/24-hour choice and date order, and the UTC offset in minutes, read at each open like `num`.
    time_style: TimeStyle,
    utc_offset_min: i32,
    /// What the providers' context borrows (the catalogue, the settings, the windows and the run history).
    apps: Arc<Arena>,
    /// The settings cache file mapped at the open (`take_settings`), while the Settings provider is on; dropped at the close.
    settings: Option<SettingsArena>,
    windows: Vec<WindowRow>,
    history: Vec<Box<str>>,
    /// What was picked (L1b-3): read by every keystroke, written 2 s after a close that changed it.
    frecency: Frecency,
    /// The icons kept between opens.
    icons: IconCache,
    /// The pixel size of the icons in `icons`: the size of the last open's rows.
    icon_px: u32,
    /// The tick when the catalogue was last stored (0: never) and when a listing was asked and not answered yet.
    listed_at_ms: u64,
    listing_since: Option<u64>,
    /// `launcher.learn`; the history files were asked for (or deleted) in this session; the history changed since the
    /// last write.
    learn: bool,
    state_loaded: bool,
    /// The load reply arrived (`on_state`): only then is a write safe.
    state_arrived: bool,
    dirty: bool,
    frec_dirty: bool,
    /// The last close that launched something: when it hid, and the query as typed (kept for the reopen of a refused launch
    /// only: `on_launch_failed` takes it, the save timer 2 s after that close forgets it, the next open clears it).
    hidden_at_ms: u64,
    failed_query: Option<String>,
    /// This open asked the settings thread for the cache file already (at most once per open).
    rebuild_asked: bool,
    /// A refused launch that could not reopen the launcher: its notice row shows at the next open.
    notice: Option<LaunchKind>,
    /// The PowerToys notice chose Alt+Shift+Space, another app holds it, and no launcher was open to say so: its notice
    /// row shows at the next open.
    chord_held: bool,
    /// PowerToys (L1b-5, 5.7): the chord is Alt+Space, so Command Palette and PowerToys Run are looked for; the policy;
    /// the one-shot 60 s check after the start has run.
    pt_on: bool,
    pt: Pt,
    pt_late: bool,
    /// The tick of the last look at the process list (0: none yet).
    pt_scanned_ms: u64,
}

/// A catalogue older than this is listed again after a close.
const CATALOGUE_STALE_MS: u64 = 600_000;
/// A listing nobody answered is asked again after this.
const LISTING_RETRY_MS: u64 = 60_000;
/// A changed history is written this long after the close (`TIMER_LAUNCHER_SAVE`); the reopen window of a launch is over
/// by then.
const SAVE_MS: u32 = 2_000;
const _: () = assert!(SAVE_MS as u64 > REOPEN_WITHIN_MS);
/// The first listing, this long after the dock starts or the chord is first set (`TIMER_LAUNCHER_WARM`).
const WARM_MS: u32 = 10_000;
/// PowerToys is checked again this long after the chord becomes Alt+Space (`TIMER_LAUNCHER_PT`, 5.7).
const PT_LATE_MS: u32 = 60_000;
/// While no chord owner runs, a close looks at the process list again at most this often (a look costs about 3 ms on
/// the UI thread: one image query per process).
const PT_RESCAN_MS: u64 = 30_000;

/// The catalogue's listing: from an `--start-apps` child, no icons (the rows ask for theirs), and what Start was sent is
/// left alone.
fn list_request() -> ShellRequest {
    ShellRequest::ListStartApps { icon_px: style::ICON_M as u32, icons: IconPlan::None, child: true }
}

/// The typed line of a launch that goes to the run history (a line run as administrator is not recorded).
fn run_line_of(r: &crate::launcher::session::LaunchReq) -> Option<&str> {
    use crate::launcher::session::LaunchReq;
    match r {
        LaunchReq::Command(l) | LaunchReq::Console(l) => Some(&l.line),
        _ => None,
    }
}

impl Ctrl {
    /// The warm timer still has a listing to ask for: nothing was ever stored (`on_catalog` sets `listed_at_ms` to at
    /// least 1, and `chord_off` resets it).
    fn warm_due(&self) -> bool {
        self.listed_at_ms == 0
    }

    /// A refused launch can no longer reopen the launcher (`after_failure`): the query as typed is not kept past that.
    fn reopen_over(&mut self, now: u64) {
        if now.saturating_sub(self.hidden_at_ms) > REOPEN_WITHIN_MS {
            self.failed_query = None;
        }
    }

    /// The catalogue was listed more than 10 minutes ago and no listing is waiting for its answer.
    fn catalogue_stale(&self, now: u64) -> bool {
        let waiting = self.listing_since.is_some_and(|t| now.saturating_sub(t) < LISTING_RETRY_MS);
        self.listed_at_ms != 0 && now.saturating_sub(self.listed_at_ms) > CATALOGUE_STALE_MS && !waiting
    }

    fn new() -> Ctrl {
        Ctrl {
            open_seq: 0,
            fg_at_open: 0,
            opens: 0,
            window_ok: false,
            pick: MonitorPick::Pointer,
            top_percent: crate::config::LAUNCHER_TOP_PERCENT.2,
            over_fullscreen_pass: true,
            flags: PassFlags::default(),
            last_query: None,
            width: style::LAUNCHER_W,
            opts: LauncherOpts::default(),
            num: NumberStyle { decimal: '.', group: ',' },
            time_style: TimeStyle::default(),
            utc_offset_min: 0,
            apps: Arc::default(),
            settings: None,
            windows: Vec::new(),
            history: Vec::new(),
            frecency: Frecency::new(),
            icons: IconCache::default(),
            icon_px: style::ICON_M as u32,
            listed_at_ms: 0,
            listing_since: None,
            learn: true,
            state_loaded: false,
            state_arrived: false,
            dirty: false,
            frec_dirty: false,
            hidden_at_ms: 0,
            failed_query: None,
            rebuild_asked: false,
            notice: None,
            chord_held: false,
            pt_on: false,
            pt: Pt::new(),
            pt_late: false,
            pt_scanned_ms: 0,
        }
    }

    /// `config.launcher` taken over (the chord is set): the next open uses it. The alias table is built here, once per
    /// config change, and the entries it ignores are counted in the log (never their words or targets).
    fn configure(&mut self, c: &LauncherConfig) {
        let (opts, ignored) = opts_with_ignored(c);
        self.opts = opts;
        if ignored > 0 {
            log!("launcher.aliases: {n} entries ignored", n = ignored);
        }
        if !self.opts.keep_query {
            self.last_query = None;
        }
        self.pick = MonitorPick::from_name(&c.monitor);
        self.top_percent = c.top_percent;
        self.over_fullscreen_pass = c.over_fullscreen != "open";
        self.width = i32::from(c.width);
        // Command Palette and PowerToys Run hold Alt+Space only: with another chord nothing of PowerToys is looked for.
        self.pt_on = c.chord == "alt+space";
        if !self.pt_on {
            self.pt = Pt::new();
            self.pt_late = false;
        }
        self.pt.configure(c.with_powertoys == "take", c.powertoys_done);
        if self.learn != c.learn {
            // Off: what was learned is forgotten and the files are deleted at the next open; on: they are read again.
            self.learn = c.learn;
            self.history.clear();
            self.frecency.clear();
            self.dirty = false;
            self.frec_dirty = false;
            self.state_loaded = false;
            self.state_arrived = false;
        }
    }

    /// The chord was switched off: a changed history is written one last time (the request returned), then nothing the
    /// launcher kept between opens stays (SPEC 5.5.6: chord Off holds no resident data).
    fn chord_off(&mut self) -> Option<ShellRequest> {
        let save = ((self.dirty || self.frec_dirty) && self.learn).then(|| ShellRequest::LauncherStateSave {
            frecency: self.frec_dirty.then(|| self.frecency.render()),
            run: self.dirty.then(|| history::render_run(&self.history)),
            delete: false,
        });
        *self = Ctrl::new();
        save
    }

    /// The close ran this row: +1 in frecency (not while learning is off).
    fn learn_pick(&mut self, key: ItemKey) {
        if !self.learn {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        let now = now.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        self.frec_dirty |= self.frecency.pick(key, now, false);
    }

    /// The providers' context of one keystroke: borrowed, nothing copied.
    fn context(&self) -> Context<'_> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        Context {
            apps: self.apps.view(),
            settings: match &self.settings {
                Some(s) if self.opts.mask.has(ProviderId::Settings) => s.view(),
                _ => CatalogView::default(),
            },
            windows: &self.windows,
            history: &self.history,
            opts: &self.opts,
            num: self.num,
            learned: &self.frecency,
            now_unix: now.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
            utc_offset_min: self.utc_offset_min,
            time_style: self.time_style,
            window_target: self.window_ok,
        }
    }
}

thread_local! {
    /// Touched only while a chord is set: the paths that exist without the launcher never reach it.
    static CTRL: RefCell<Ctrl> = RefCell::new(Ctrl::new());
    /// A chord is set: the foreground, session and power events concern the launcher.
    static ON: Cell<bool> = const { Cell::new(false) };
    /// The dock window receives the session notifications (registered once, at the first chord).
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    /// The launcher opened at least once in this process (`note_open`): the warm timer has nothing left to warm.
    static OPENED: Cell<bool> = const { Cell::new(false) };
}

/// The config changed (and once at start): the hook's chord, and with a chord the options the next open uses. With no
/// chord (every config that predates the launcher) nothing else happens: no state, no timer, no log line.
pub(crate) fn on_config() {
    let cfg = crate::config::get();
    let chord = cfg.launcher.chord_spec();
    hook::set_launcher_chord(chord);
    let was = ON.with(|on| on.replace(chord.is_some()));
    match (was, chord.is_some()) {
        (false, false) => {}
        (true, false) => {
            // An open launcher closes first, so what its close records lands in the state dropped below.
            if super::current() == Some(PopupKind::Launcher) {
                super::close("launcher chord off");
            }
            let save = with_ctrl(Ctrl::chord_off).flatten();
            crate::start_search::chord_changed(false);
            crate::launcher_sys::release_owner_waits();
            if let Some(h) = crate::launcher_sys::timer_window() {
                for id in crate::msg::TIMER_LAUNCHER_SHRINK..=crate::msg::TIMER_LAUNCHER_PT {
                    // SAFETY: timers of the dock window (same thread); killing an id that is not armed fails harmlessly.
                    let _ = unsafe { KillTimer(Some(h), id) };
                }
            }
            if let Some(r) = save {
                crate::shell_thread::request(r);
            }
            crate::mem::released_data();
            log!("launcher: chord off");
        }
        (_, true) => {
            crate::start_search::chord_changed(true);
            let pt = with_ctrl(|c| {
                let was = c.pt_on;
                c.configure(&cfg.launcher);
                (was, c.pt_on)
            });
            match pt {
                // Alt+Space: PowerToys is looked for now (the dock's start), and again 60 s later (it may start later
                // at logon).
                Some((false, true)) => {
                    pt_check(&mut RealHook, &mut RealSys);
                    crate::launcher_sys::arm_once(crate::msg::TIMER_LAUNCHER_PT, PT_LATE_MS);
                }
                Some((true, false)) => {
                    crate::launcher_sys::release_owner_waits();
                    if let Some(h) = crate::launcher_sys::timer_window() {
                        // SAFETY: a timer of the dock window (same thread); killing one that is not armed fails harmlessly.
                        let _ = unsafe { KillTimer(Some(h), crate::msg::TIMER_LAUNCHER_PT) };
                    }
                }
                _ => {}
            }
            // The flags of the window in front now, not at its next change.
            foreground_flow(&mut RealHook, &RealSys);
            if !was {
                log!("launcher: chord on");
                // The catalogue is listed 10 s from now, off the first open's path.
                crate::launcher_sys::arm_once(crate::msg::TIMER_LAUNCHER_WARM, WARM_MS);
                if !REGISTERED.with(|r| r.replace(true)) {
                    crate::launcher_sys::watch_sessions();
                }
            }
        }
    }
}

/// `EVENT_SYSTEM_FOREGROUND` (`ui::on_foreground_changed`, first thing): the hook's pass flags for the new foreground.
#[cfg_attr(not(feature = "owner-probe"), allow(unused_variables))]
pub(crate) fn on_foreground(fg: HWND) {
    if !ON.with(Cell::get) {
        return;
    }
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::foreground(fg);
    pt_foreground(fg.0 as isize, &RealSys);
    foreground_flow(&mut RealHook, &RealSys);
}

/// `WM_DOCK_LAUNCHER_YIELDED`: the chord pressed at `kb_time` passed under YIELD (Command Palette answers it): PowerToys
/// is checked again and the chord's answer window opens (5.7).
#[cfg_attr(not(feature = "owner-probe"), allow(unused_variables))]
pub(crate) fn on_yielded(kb_time: u32) {
    let on = ON.with(Cell::get);
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::yielded(on.then_some(kb_time));
    if on {
        pt_passed(&mut RealHook, &mut RealSys);
    }
}

/// `WM_DOCK_LAUNCHER_PT`: a chord owner the launcher waits on exited (0), or the PowerToys notice was answered (1-4).
pub(crate) fn on_pt_message(code: usize) {
    if !ON.with(Cell::get) {
        return;
    }
    match answer_of(code) {
        Some(answer) => apply_pt_answer(answer),
        None => pt_check(&mut RealHook, &mut RealSys),
    }
}

/// The PowerToys notice's answer, outside the content that showed it: the config keeps it (`apply_answer`), and the
/// controller follows the config at once.
fn apply_pt_answer(answer: Option<Choice>) {
    // Alt+Shift+Space only when no other app registered it (5.7, "Other holders"), as the Start offer: the probe runs
    // here, on the dock window's message, outside the launcher's content.
    if !answer_allowed(answer, &|| !crate::launcher_sys::chord_is_free(true)) {
        log!("launcher: another app holds Alt+Shift+Space, the PowerToys answer is not saved");
        show_chord_held();
        return;
    }
    let saved = crate::config::modify(|cfg| apply_answer(&mut cfg.launcher, answer));
    match saved {
        Ok(()) => {
            log!("launcher: the PowerToys notice was answered ({kind:?})", kind = answer);
            on_config();
        }
        Err(e) => log!("launcher: cannot save the PowerToys answer: {e}"),
    }
}

// ---------------------------------------------------------------- aliases (plan L2-A2)

/// A change of `launcher.aliases` asked by the launcher, applied on the dock window (`on_alias_message`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum AliasOp {
    Save {
        word: Box<str>,
        target: Box<str>,
    },
    /// The word as configured.
    Remove(Box<str>),
    /// The entry whose word has this `aliases::word_hash` (Settings > Launcher's Remove button).
    RemoveHash(u64),
}

thread_local! {
    /// The alias change `WM_DOCK_LAUNCHER_ALIAS` applies: one slot, a newer change replaces an older one. Outside
    /// `CTRL`, which the session's effects run under (borrowed).
    static ALIAS_OP: RefCell<Option<AliasOp>> = const { RefCell::new(None) };
}

/// `SysPort::alias` in the dock: the change waits here for its message.
pub(crate) fn hold_alias_op(op: AliasOp) {
    ALIAS_OP.with(|s| *s.borrow_mut() = Some(op));
}

/// `WM_DOCK_LAUNCHER_ALIAS`: the alias change the launcher posted is written to config.json (only while a chord is set).
pub(crate) fn on_alias_message() {
    let op = ALIAS_OP.with(|s| s.borrow_mut().take());
    if let (Some(op), true) = (op, is_on()) {
        apply_alias_op(&op, &mut RealAliasCfg);
    }
}

/// Settings > Launcher's Remove: the entry with this word hash goes through the same handler as the launcher's own
/// alias changes (a pending hand edit is loaded first and kept), whether or not a chord is set.
pub(crate) fn remove_alias_by_hash(hash: u64) {
    apply_alias_op(&AliasOp::RemoveHash(hash), &mut RealAliasCfg);
}

/// What `apply_alias_op` does with the config, in its order (a recorder in the tests).
trait AliasCfg {
    /// `config::external_edit_pending`: a hand edit not loaded yet.
    fn pending(&self) -> bool;
    /// `config::poll_changed`: true when it reloaded the file.
    fn poll(&mut self) -> bool;
    /// What the 2 s housekeeping runs after a reload (`ui::on_config_changed` and the rest, `ui::config_reloaded`).
    fn follow_up(&mut self);
    /// `config::modify` with the change applied to `launcher.aliases` (`edit_aliases`).
    fn modify(&mut self, op: &AliasOp) -> Result<(), String>;
    /// The launcher and the Settings window take the new config (`on_config`, `settings_window::on_config_changed`).
    fn applied(&mut self);
    /// The open launcher runs its query again (the new table's rows), or shows that the change was not saved.
    fn requery(&mut self, failed: bool);
}

/// Writes an alias change: a pending hand edit is loaded first (and followed up as the housekeeping would), so the
/// write keeps it; then the change is written, the launcher and Settings follow it and the open launcher's query runs
/// again. A failure shows a notice row and logs the reason only.
fn apply_alias_op(op: &AliasOp, cfg: &mut dyn AliasCfg) {
    if cfg.pending() && cfg.poll() {
        cfg.follow_up();
    }
    match cfg.modify(op) {
        Ok(()) => {
            cfg.applied();
            cfg.requery(false);
        }
        Err(e) => {
            log!("launcher: the alias change was not saved: {e}");
            cfg.requery(true);
        }
    }
}

/// The change on `launcher.aliases`. A saved word replaces an entry the table serves whose word is equal to it once
/// folded (the table would keep only the first of the two); the other entries, those the table ignores included, stay
/// as written (D9).
fn edit_aliases(l: &mut LauncherConfig, op: &AliasOp) {
    let folded = |w: &str| {
        let mut f = String::with_capacity(w.len());
        crate::launcher::rank::fold(w, &mut f);
        f
    };
    match op {
        AliasOp::Save { word, target } => {
            use crate::launcher::aliases::{check_word, classify};
            let f = folded(word);
            let p = PrefixTable::from_pairs(&l.prefixes);
            l.aliases.retain(|w, t| folded(w) != f || check_word(w, &p).is_err() || classify(t).is_err());
            l.aliases.insert(word.to_string(), target.to_string());
        }
        AliasOp::Remove(word) => {
            l.aliases.remove(&**word);
        }
        AliasOp::RemoveHash(h) => {
            crate::launcher::aliases::remove_by_hash(&mut l.aliases, *h);
        }
    }
}

/// The dock's `AliasCfg`.
struct RealAliasCfg;

impl AliasCfg for RealAliasCfg {
    fn pending(&self) -> bool {
        crate::config::external_edit_pending()
    }
    fn poll(&mut self) -> bool {
        crate::config::poll_changed()
    }
    fn follow_up(&mut self) {
        crate::ui::config_reloaded();
    }
    fn modify(&mut self, op: &AliasOp) -> Result<(), String> {
        crate::config::modify(|c| edit_aliases(&mut c.launcher, op))
    }
    fn applied(&mut self) {
        on_config();
        crate::settings_window::on_config_changed();
    }
    fn requery(&mut self, failed: bool) {
        super::with_launcher(|l| {
            ctx(|cx| {
                if failed {
                    l.show_notices(vec![alias_failed_row()], cx, &mut RealSys)
                } else {
                    let fx = l.session.refresh(cx);
                    l.run(fx, cx, &mut RealSys)
                }
            })
            .unwrap_or(Reply::None)
        });
    }
}

/// The row of an alias change config.json did not take (no action).
fn alias_failed_row() -> Item {
    notice_row("alias-not-saved", strings::alias_not_saved(), None)
}

/// The notice's answer may be written: Use Alt+Shift+Space only while no other app holds that chord (`held` runs the
/// RegisterHotKey probe, for that answer only). A held chord leaves the chord and `powertoys_done` as they were.
fn answer_allowed(answer: Option<Choice>, held: &dyn Fn() -> bool) -> bool {
    answer != Some(Choice::UseAltShiftSpace) || !held()
}

/// Another app holds Alt+Shift+Space: an open launcher says so at once, otherwise its next open does (once).
fn show_chord_held() {
    held_flow(&mut |rows| {
        super::with_launcher(|l| ctx(|cx| l.show_notices(rows, cx, &mut RealSys)).unwrap_or(Reply::None))
    });
}

/// `show_chord_held` with the open launcher as `show` (false: none is open). Nothing was written, so the PowerToys
/// notice's question is still open: its row (which the choice took out of the list) shows again under the held chord's,
/// unless the owner exited or the launcher took the chord meanwhile.
fn held_flow(show: &mut dyn FnMut(Vec<Item>) -> bool) {
    let again = with_ctrl(|c| c.pt.renotice()).unwrap_or(false);
    let rows = std::iter::once(chord_held_row()).chain(again.then(|| pt_notice_row(PtNotice::Shared))).collect();
    let shown = show(rows);
    with_ctrl(|c| {
        if !shown {
            c.chord_held = true;
        } else if again {
            // Shown now, not once more at the next open.
            let _ = c.pt.take_notice();
        }
    });
}

/// What the PowerToys notice's answer writes: Take and Leave set `with_powertoys`, Use Alt+Shift+Space sets the chord,
/// and every answer (Don't show again included) sets `powertoys_done`, so the notice is not asked again.
fn apply_answer(l: &mut LauncherConfig, answer: Option<Choice>) {
    l.powertoys_done = true;
    match answer {
        Some(Choice::TakeChord) => l.with_powertoys = "take".into(),
        Some(Choice::LeaveChord) => l.with_powertoys = "yield".into(),
        Some(Choice::UseAltShiftSpace) => l.chord = "alt+shift+space".into(),
        None => {}
    }
}

/// A PowerToys check (5.7): at the start, 60 s later, after each close, at unlock and resume, at each passed chord and
/// when a chord owner exits; only with the chord Alt+Space. The hook's flags follow the state (YIELD while it yields).
fn pt_check(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    if with_ctrl(|c| c.pt_on) != Some(true) {
        return;
    }
    let running = sys.chord_owners();
    let now = sys.now_ms();
    let Some((before, after)) = with_ctrl(|c| {
        c.pt_scanned_ms = now.max(1);
        let before = c.pt.state();
        c.pt.on(if running { PtEvent::Found } else { PtEvent::Gone });
        (before, c.pt.state())
    }) else {
        return;
    };
    if before == PtState::Absent && after != PtState::Absent {
        log!("launcher: PowerToys Command Palette runs ({kind:?})", kind = after);
    } else if before != PtState::Absent && after == PtState::Absent {
        log!("launcher: PowerToys Command Palette is gone, the chord is the launcher's");
    }
    if before != after || after != PtState::Absent {
        foreground_flow(hook, sys);
    }
}

/// After a close (the launcher hidden, off the open's path): with a chord owner running the hook may pass the chord
/// again; with none, PowerToys may have started since the last look (at most every `PT_RESCAN_MS`).
fn pt_closed(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    let now = sys.now_ms();
    let plan = with_ctrl(|c| {
        let absent = c.pt.state() == PtState::Absent;
        (c.pt_on, absent, now.saturating_sub(c.pt_scanned_ms) >= PT_RESCAN_MS)
    });
    match plan {
        Some((true, true, true)) => pt_check(hook, sys),
        Some((true, false, _)) => foreground_flow(hook, sys),
        _ => {}
    }
}

/// Settings > Launcher is shown (5.7: a check "when its Settings page opens"): PowerToys is looked for before the page's
/// rows are built, so the PowerToys row is there when Command Palette started after the 60 s check (its hook sees the
/// chord first, so no open or close of the launcher ever looks). With the chord off nothing is touched.
pub(crate) fn pt_refresh() {
    if ON.with(Cell::get) {
        pt_page_shown(&mut RealHook, &mut RealSys);
    }
}

/// The look of `pt_refresh`: only with the chord Alt+Space and no chord owner found (a running one is waited on), at
/// most every `PT_RESCAN_MS`, so going back and forth between the pages costs no scan each.
fn pt_page_shown(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    let now = sys.now_ms();
    let due = with_ctrl(|c| {
        c.pt_on && c.pt.state() == PtState::Absent && now.saturating_sub(c.pt_scanned_ms) >= PT_RESCAN_MS
    });
    if due == Some(true) {
        pt_check(hook, sys);
    }
}

/// A chord passed under YIELD: its answer window opens for `ANSWER_MS` (`TIMER_LAUNCHER_PT`). Command Palette already
/// in front counts as the answer (the chord closes it). The process list is looked at again first, unless it was looked
/// at less than `PT_RESCAN_MS` ago (the owner's exit is waited on anyway): presses in a row cost no scan each.
fn pt_passed(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    let now = sys.now_ms();
    if with_ctrl(|c| now.saturating_sub(c.pt_scanned_ms) >= PT_RESCAN_MS) == Some(true) {
        pt_check(hook, sys);
    }
    let in_front = sys.owner_window(sys.foreground());
    let watching = with_ctrl(|c| {
        if !c.pt.yields() {
            return false;
        }
        c.pt.on(PtEvent::PassedChord);
        if in_front {
            c.pt.on(PtEvent::OwnerAnswered);
        }
        c.pt.watching()
    });
    if watching == Some(true) {
        sys.arm(crate::msg::TIMER_LAUNCHER_PT, powertoys::ANSWER_MS);
    }
}

/// A foreground change while a passed chord waits for its answer: a chord owner's window is the answer.
fn pt_foreground(fg: isize, sys: &dyn SysPort) {
    if with_ctrl(|c| c.pt.watching()) == Some(true) && sys.owner_window(fg) {
        with_ctrl(|c| c.pt.on(PtEvent::OwnerAnswered));
    }
}

/// `TIMER_LAUNCHER_PT`: the end of an answer window (no answer came), and the one-shot check 60 s after the start (or at
/// the end of the first answer window, which takes its timer).
fn pt_timer(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    let Some((unanswered, took, late_done)) = with_ctrl(|c| {
        let watching = c.pt.watching();
        if watching {
            c.pt.on(PtEvent::NoAnswerIn400ms);
        }
        let n = match c.pt.state() {
            PtState::Yield { unanswered } => u32::from(unanswered),
            _ => u32::from(powertoys::MAX_UNANSWERED),
        };
        (watching.then_some(n), watching && c.pt.state() == PtState::Take, std::mem::replace(&mut c.pt_late, true))
    }) else {
        return;
    };
    if let Some(n) = unanswered {
        let ms = powertoys::ANSWER_MS;
        log!("launcher: Command Palette did not answer a passed chord in {ms} ms ({n} in a row)");
    }
    if took {
        log!("launcher: Command Palette did not answer twice, the launcher takes the chord");
        foreground_flow(hook, sys);
    }
    if !late_done {
        pt_check(hook, sys);
    }
}

/// `WM_DOCK_LAUNCHER_PASSED` (wParam the newest sequence the hook issued): the hook passed a chord under FULLSCREEN or
/// failed open.
pub(crate) fn on_passed(seq: u32) {
    // First: every post before this note was handled, so a lost one no longer fails the next chords open.
    hook::resync_launcher(seq);
    if ON.with(Cell::get) {
        pass_note(&mut RealHook, &RealSys);
    }
}

/// A window can leave fullscreen without a foreground change (F11 in a browser, Esc out of a video, a borderless
/// player): each chord passed under FULLSCREEN computes the flags again, so a stale flag loses one press (5.2.12).
fn pass_note(hook: &mut dyn HookPort, sys: &dyn SysPort) {
    // A fail-open's note comes without FULLSCREEN: only a flag that was set and cleared is logged.
    let was = with_ctrl(|c| c.flags.has(PassFlags::FULLSCREEN)) == Some(true);
    foreground_flow(hook, sys);
    if was && with_ctrl(|c| c.flags.has(PassFlags::FULLSCREEN)) == Some(false) {
        log!("launcher: no fullscreen window in front any more, the next chord opens");
    }
}

/// The pass flags of the window now in front, to the hook (a chord's FULLSCREEN is recomputed here).
fn foreground_flow(hook: &mut dyn HookPort, sys: &dyn SysPort) {
    let over_pass = with_ctrl(|c| c.over_fullscreen_pass).unwrap_or(true);
    let d3d = sys.d3d_fullscreen();
    let fs = !d3d && over_pass && sys.fullscreen_foreground();
    // A remote-desktop or VM client in front: the chord goes to its session (5.2.13). The launcher in front is the
    // dock's own process, never a remote client, so its chord still closes it.
    let remote = sys.remote_foreground();
    // YIELD while Command Palette answers the chord (L1b-5), never while the launcher is open: its chord closes it.
    let pt_yield = with_ctrl(|c| c.pt_on && c.pt.yields()).unwrap_or(false) && !sys.launcher_open();
    // The owner probe can force YIELD (S-PT).
    #[cfg(feature = "owner-probe")]
    let yield_ = pt_yield || crate::launcher_probe::force_yield();
    #[cfg(not(feature = "owner-probe"))]
    let yield_ = pt_yield;
    let flags = pass_flags(d3d, fs, over_pass, remote, yield_);
    let was_remote = with_ctrl(|c| std::mem::replace(&mut c.flags, flags).has(PassFlags::REMOTE)) == Some(true);
    if remote && !was_remote {
        log!("launcher: a remote or VM client is in front, the chord goes to its session");
    }
    hook.pass(flags);
}

/// `WM_WTSSESSION_CHANGE`: a lock, a console or a remote disconnect ends the session (`session_end`).
pub(crate) fn on_session_change(code: u32) {
    if ON.with(Cell::get) && matches!(code, WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) {
        session_end(&mut RealHook, &mut RealSys);
    }
    if ON.with(Cell::get) && code == WTS_SESSION_UNLOCK {
        pt_check(&mut RealHook, &mut RealSys);
    }
}

/// `PBT_APMRESUMEAUTOMATIC`: PowerToys is checked again (it may have started or ended meanwhile).
pub(crate) fn on_resume() {
    if ON.with(Cell::get) {
        pt_check(&mut RealHook, &mut RealSys);
    }
}

/// `PBT_APMSUSPEND` (`WM_POWERBROADCAST`).
pub(crate) fn on_suspend() {
    if ON.with(Cell::get) {
        session_end(&mut RealHook, &mut RealSys);
    }
}

/// Lock, disconnect, suspend: the launcher closes without restoring, the kept query goes and the hook forgets its
/// tracked modifiers (the secure desktop can swallow their releases).
fn session_end(hook: &mut dyn HookPort, sys: &mut dyn SysPort) {
    if sys.launcher_open() {
        sys.close_quiet();
    }
    with_ctrl(|c| c.last_query = None);
    hook.reset_mods();
}

/// `WM_DOCK_LAUNCHER_CLOSE` (wParam the open's sequence): the close a replay asked for at the first activation
/// (`Launcher::on_focus`), now outside it; a launcher a later post opened stays.
pub(crate) fn on_deferred_close(seq: u32) {
    super::with_launcher(|l| l.close_posted(seq));
}

/// `ShellEvent::LauncherConverted` (the reply to `LauncherConvert`).
pub(crate) fn on_converted(gen: u64, meant: Option<String>) {
    let Some(meant) = meant else { return };
    if ON.with(Cell::get) {
        super::with_launcher(|l| ctx(|cx| l.converted(gen, &meant, cx)).unwrap_or(Reply::None));
    }
}

/// `ShellEvent::LauncherFiles` (the files pump's posts, plan L2-F3), timed as KEY-11 bounds it (`UiWork::Files`).
pub(crate) fn on_files(open: u32, ev: crate::launcher::files::FilesEvent) {
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::Files);
    if ON.with(Cell::get) {
        super::with_launcher(|l| ctx(|cx| l.files_event(open, ev, cx)).unwrap_or(Reply::None));
    }
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
    #[cfg(feature = "owner-probe")]
    if crate::launcher_probe::drop_post() {
        hook.end(seq);
        return OpenPlan::Nothing;
    }
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
            let fg = sys.foreground();
            let num = sys.number_style();
            let time_style = sys.time_style();
            let utc_offset_min = sys.utc_offset_min();
            let window_ok = sys.window_target(fg);
            with_ctrl(|c| {
                c.open_seq = seq;
                c.fg_at_open = fg;
                c.window_ok = window_ok;
                c.num = num;
                c.time_style = time_style;
                c.utc_offset_min = utc_offset_min;
                // The first chord the dock sees while Command Palette runs opens with its notice (5.7).
                if c.pt_on {
                    c.pt.on(PtEvent::FirstChord);
                }
            });
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::opened(sent_time);
            sys.prepare_open();
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::prepared();
            hook.shown(true);
            if !sys.open(pick, top_percent) {
                #[cfg(feature = "owner-probe")]
                crate::launcher_probe::open_failed();
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
    /// The date and time rows (")").
    Time,
    /// A unit conversion answer ("10 km in mi").
    Units,
    Run,
    RunReview,
    Windows,
    Notice,
    Error,
    FieldPair,
    /// The action list open over a running app's row (opened by Ctrl+K, so the focus ring shows).
    Actions,
    /// The PowerToys notice's action list: its three choices and Don't show again (5.7).
    NoticeActions,
    /// The notice after two passed chords Command Palette did not answer (5.7), at the narrowest launcher width.
    NoticeTookOver,
    /// An alias typed whole: its row first and selected, the app it opens as the detail (plan L2-A2).
    Alias,
    /// The alias prompt opened from Settings, "word target" typed: the "Alias" chip and the row Enter saves.
    AliasPrompt,
    /// Mixed mode with the files' rows arrived: two apps, three file rows, the web row last (plan L2-F4).
    Files,
    /// The files request is taking long: the busy mark in the field, no file row yet.
    FilesBusy,
    /// "file " mode: eight of twenty rows shown, a glyph by class.
    FilesMode,
    /// "file " mode while Windows Search is off: the honest row.
    SearchOff,
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
            "time" => PreviewScene::Time,
            "units" => PreviewScene::Units,
            "run" => PreviewScene::Run,
            "run_review" => PreviewScene::RunReview,
            "windows" => PreviewScene::Windows,
            "notice" => PreviewScene::Notice,
            "error" => PreviewScene::Error,
            "field_pair" => PreviewScene::FieldPair,
            "actions" => PreviewScene::Actions,
            "notice_actions" => PreviewScene::NoticeActions,
            "notice_took_over" => PreviewScene::NoticeTookOver,
            "alias" => PreviewScene::Alias,
            "alias_prompt" => PreviewScene::AliasPrompt,
            "files" => PreviewScene::Files,
            "files_busy" => PreviewScene::FilesBusy,
            "files_mode" => PreviewScene::FilesMode,
            "search_off" => PreviewScene::SearchOff,
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
    use crate::launcher::model::{NumberStyle, SettingRow, WindowRow};
    use crate::launcher::providers::Context;
    use crate::launcher::rank;
    use crate::start_search::Keys;

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
    let settings = vec![SettingIn {
        row: SettingRow::new(0, tr("Display", "Schermo"), Keys::default(), 1),
        target: "ms-settings:display".into(),
        args: None,
        start_keys: String::new(),
    }];
    let apps_arena = arena::encode_apps(&apps, Caps::APPS)
        .and_then(|b| Arena::new(b, ArenaKind::Apps, None, Caps::APPS))
        .unwrap_or_default();
    let settings_arena = arena::encode_settings(catalog::IN_PROCESS_KEY, &settings, Caps::SETTINGS)
        .and_then(|b| Arena::new(b, ArenaKind::Settings, Some(catalog::IN_PROCESS_KEY), Caps::SETTINGS))
        .unwrap_or_default();
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
    let mut opts = LauncherOpts::default();
    // Invented aliases (plan L2-A2): an app and a web address.
    let aliases: std::collections::BTreeMap<String, String> =
        [("np", "app:Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"), ("docs", "https://example.com/docs")]
            .iter()
            .map(|(w, t)| (w.to_string(), t.to_string()))
            .collect();
    opts.aliases = AliasTable::build(&aliases, &opts.prefixes).0;
    let cx = Context {
        apps: apps_arena.view(),
        settings: settings_arena.view(),
        windows: &windows,
        history: &history,
        opts: &opts,
        num: NumberStyle {
            decimal: tr(".", ",").chars().next().unwrap_or('.'),
            group: tr(",", ".").chars().next().unwrap_or(','),
        },
        learned: &Frecency::new(),
        now_unix: 1_790_000_000,
        utc_offset_min: 60,
        time_style: Default::default(),
        window_target: true,
    };
    // The notepad window belongs to the Notepad app: its row shows the running indicator.
    let mut session = Session::open(&opts, None);
    let long_line = "> powershell -NoProfile -ExecutionPolicy Bypass -File \"C:\\Users\\Public\\Scripts\\cleanup-temp-files.ps1\" \
        -Days 30 -Recurse -Verbose -LogFile \"C:\\Users\\Public\\Logs\\cleanup-temp-files-2026-11.log\" -Confirm:$false";
    let query: &str = match scene {
        PreviewScene::Empty => "",
        PreviewScene::Inactive | PreviewScene::Running | PreviewScene::Actions => "note",
        PreviewScene::Apps => "calc",
        PreviewScene::Calc => tr("=2.5*4", "=2,5*4"),
        PreviewScene::Time => ")",
        PreviewScene::Units => "10 km in mi",
        PreviewScene::Run => "> ping 1.1.1.1",
        PreviewScene::RunReview => long_line,
        PreviewScene::Windows => "<",
        // The PowerToys notice comes with the first chord: the field is still empty.
        PreviewScene::Notice | PreviewScene::NoticeActions | PreviewScene::NoticeTookOver => "",
        PreviewScene::Error => "calc",
        PreviewScene::FieldPair => "",
        PreviewScene::Alias => "np",
        PreviewScene::AliasPrompt => "",
        PreviewScene::Files | PreviewScene::FilesBusy => "notep",
        PreviewScene::FilesMode | PreviewScene::SearchOff => "file report",
    };
    if !query.is_empty() {
        session.chars(query, &cx);
    }
    if matches!(scene, PreviewScene::Files | PreviewScene::FilesMode) {
        use crate::launcher::files::{items, FileHit};
        let hit = |name: &str, folder: &str, dir: bool| FileHit {
            path: format!("C:\\Users\\Public\\{folder}\\{name}").into(),
            name: name.into(),
            folder: folder.into(),
            dir,
        };
        let (words, cap, hits) = if scene == PreviewScene::Files {
            let hits = vec![
                hit("notepad-tips.txt", "Documents", false),
                hit("notepad-shortcuts.pdf", "Documents", false),
                hit("notepad-screenshot.png", "Pictures", false),
            ];
            (vec!["notep"], 3, hits)
        } else {
            let names: [(&str, &str, bool); 12] = [
                ("report-2026.docx", "Documents", false),
                ("report-summary.pdf", "Documents", false),
                ("report-chart.png", "Pictures", false),
                ("report-archive.zip", "Downloads", false),
                ("report-tool.exe", "Downloads", false),
                ("reports", "Documents", true),
                ("report-draft.txt", "Desktop", false),
                ("report-notes.md", "Desktop", false),
                ("report-old.docx", "Archive", false),
                ("report-photo.jpg", "Pictures", false),
                ("report-data.csv", "Documents", false),
                ("report-backup.7z", "Downloads", false),
            ];
            (vec!["report"], 20, names.iter().map(|(n, f, d)| hit(n, f, *d)).collect())
        };
        let batch = Batch { gen: session.gen(), from: ProviderId::Files, items: items(hits, &words, cap), done: true };
        let _ = session.late(batch, &cx);
    }
    if scene == PreviewScene::FilesBusy {
        let _ = session.files_event(crate::launcher::files::FilesEvent::Slow(session.gen()), &cx);
    }
    if scene == PreviewScene::SearchOff {
        use crate::launcher::files::{FilesEvent, Why};
        let _ = session.files_event(FilesEvent::Off(Why::SearchOff), &cx);
    }
    if scene == PreviewScene::Actions {
        let ctrl_k = KeyEvent { vk: 0x4B, mods: Mods::CTRL, repeat: false, sys: false };
        let _ = session.key(ctrl_k, None, &cx);
    }
    if matches!(scene, PreviewScene::Notice | PreviewScene::NoticeActions) {
        // The first chord the dock sees while Command Palette runs (5.7).
        session.set_notices(vec![pt_notice_row(PtNotice::Shared)]);
        let _ = session.refresh(&cx);
    }
    if scene == PreviewScene::NoticeTookOver {
        // The launcher took the chord: the next open says so.
        session.set_notices(vec![pt_notice_row(PtNotice::TookOver)]);
        let _ = session.refresh(&cx);
    }
    if scene == PreviewScene::NoticeActions {
        let ctrl_k = KeyEvent { vk: 0x4B, mods: Mods::CTRL, repeat: false, sys: false };
        let _ = session.key(ctrl_k, None, &cx);
    }
    if scene == PreviewScene::AliasPrompt {
        // Settings > Launcher's "Add…" (`open_alias_prompt`), then a word and its target typed.
        let _ = session.alias_prompt(&cx);
        let _ = session.chars("docs https://example.com/docs", &cx);
    }
    if scene == PreviewScene::Error {
        // A refused launch that reopened the launcher over the same query (`reopen_with_error`).
        session.set_notices(vec![failure_row()]);
        let _ = session.refresh(&cx);
    }
    let mut l = Launcher::with_session(session);
    for (k, b) in icons {
        l.icons.insert(k, Rc::new(b));
    }
    l.foreground = scene != PreviewScene::Inactive;
    if scene == PreviewScene::NoticeTookOver {
        l.set_shape(*style::WIDTH_RANGE.start(), style::ROWS_DEFAULT);
    }
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
            s.glyph_cached(
                GlyphId::Magnifier as u32,
                gx,
                rc.top + (rc.bottom - rc.top - gs) / 2,
                gs,
                style::FG_SECONDARY,
                glyph_shape(GlyphId::Magnifier),
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
                let ch = m.px(PAIR_CARET_H);
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
        resets: u32,
        gesture: Option<u32>,
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
        fn reset_mods(&mut self) {
            self.resets += 1;
        }
        fn gesture_time(&mut self) -> Option<u32> {
            self.gesture.take()
        }
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Op {
        Open,
        CloseLauncher,
        CloseQuiet,
        Inject(Vec<(u16, bool)>),
        WinKey(u16),
        DeferClose(u32),
        Arm(usize, u32),
        Alias(AliasOp),
    }

    #[derive(Default)]
    struct FakeSys {
        now: u32,
        d3d: bool,
        fs: bool,
        open_ok: bool,
        is_open: bool,
        /// The window in front can take the window menu.
        menu_ok: bool,
        /// The window in front belongs to a remote-desktop or VM client.
        remote: bool,
        /// A chord owner runs (the PowerToys check), and the window asked about is one of its; the looks so far.
        owners: bool,
        owner_fg: bool,
        scans: u32,
        ops: Vec<Op>,
        /// Every request to the shell thread (the icons' included).
        shells: Vec<String>,
        /// The settings the port hands out (checked again, as the real port checks the file), or why not; the mappings
        /// asked for and the settings thread's requests (`rebuild` each).
        settings: Option<Arena>,
        miss: Option<SettingsMiss>,
        /// A real cache file and its key: mapped through the dock's own code (`launcher_sys::map_settings`).
        file: Option<(std::path::PathBuf, String)>,
        maps: u32,
        ensures: Vec<bool>,
        /// The files requests' generations and the pump's closes.
        files: Vec<Option<u64>>,
        files_ends: u32,
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
        fn remote_foreground(&self) -> bool {
            self.remote
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
        fn win_key(&mut self, vk: u16) {
            self.ops.push(Op::WinKey(vk));
        }
        /// The fake layout: `scan` holds the character a key types (0: none, 0xFFFF: a dead key).
        fn translate(&mut self, rec: &KeyRec) -> Typed {
            match rec.scan {
                0 => Typed::Key,
                0xFFFF => Typed::Dead,
                c => Typed::Text(char::from_u32(u32::from(c)).unwrap().to_string()),
            }
        }
        fn foreground(&self) -> isize {
            0x5005
        }
        fn window_target(&self, h: isize) -> bool {
            h != 0 && self.menu_ok
        }
        fn activate(&mut self, _h: isize) {}
        fn restore(&mut self, _h: isize) {}
        fn copy(&mut self, _text: &str) {}
        fn paste(&mut self) -> Pasted {
            Pasted::Empty
        }
        fn close_window(&mut self, _h: isize) {}
        fn window_menu(&mut self, _h: isize) {}
        fn shell(&mut self, r: ShellRequest) {
            self.shells.push(format!("{r:?}"));
        }
        fn power(&mut self, _id: CommandId) {}
        fn open_settings(&mut self) {}
        fn arm(&mut self, id: usize, ms: u32) {
            self.ops.push(Op::Arm(id, ms));
        }
        fn msg_time(&self) -> u32 {
            0
        }
        fn msg_extra(&self) -> usize {
            0
        }
        fn now_ms(&self) -> u64 {
            u64::from(self.now)
        }
        fn number_style(&self) -> NumberStyle {
            NumberStyle { decimal: '.', group: ',' }
        }
        fn close_quiet(&mut self) {
            self.ops.push(Op::CloseQuiet);
        }
        fn defer_close(&mut self, seq: u32) -> bool {
            self.ops.push(Op::DeferClose(seq));
            true
        }
        fn chord_owners(&mut self) -> bool {
            self.scans += 1;
            self.owners
        }
        fn owner_window(&self, _h: isize) -> bool {
            self.owner_fg
        }
        fn settings_bytes(&mut self) -> Result<SettingsArena, SettingsMiss> {
            self.maps += 1;
            if let Some(m) = self.miss {
                return Err(m);
            }
            if let Some((path, key)) = &self.file {
                return crate::launcher_sys::map_settings(path, key);
            }
            self.settings.as_ref().and_then(SettingsArena::of).ok_or(SettingsMiss::NoFile)
        }
        fn ensure_settings(&mut self, rebuild: bool) {
            self.ensures.push(rebuild);
        }
        fn alias(&mut self, op: AliasOp) {
            self.ops.push(Op::Alias(op));
        }
        fn files(&mut self, _open: u32, r: Option<crate::launcher::files::Req>) {
            self.files.push(r.map(|r| r.gen));
        }
        fn files_end(&mut self) {
            self.files_ends += 1;
        }
    }

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
        let mut sys = FakeSys::default();
        let reply = l.on_focus(true, &fx.cx(), &mut hook, &mut sys);
        assert_eq!(hook.takes, [7]);
        assert_eq!(l.session.query_text(), "calc");
        // Enter on the rows of "calc" launches, but never from inside the activation (the host's SetForegroundWindow):
        // only the focus change now, and the close posted for this open.
        assert_eq!(reply, Reply::Relayout, "no close from the first activation");
        assert_eq!(sys.ops, [Op::DeferClose(7)], "one deferred close, for the open's sequence");
        let calc = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(l.queued, [Effect::Launch(LaunchReq::App(calc.into()))], "the Launch of Calcolatrice, still queued");
        // The posted close closes this open only, not one a later post made.
        assert_eq!(l.close_posted(7), Reply::Close);
        assert_eq!(l.close_posted(8), Reply::None);
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

    // ---- 15c: effects and the system port

    #[derive(Default)]
    struct EffectSys {
        /// The generations of the files requests (None: a withdrawal), the open numbers they carry and the closes of
        /// the pump.
        files: Vec<Option<u64>>,
        files_opens: Vec<u32>,
        files_ends: u32,
        clip: Option<String>,
        /// The app that copied `clip` marked it private: the port never reads it.
        private: bool,
        ops: Vec<String>,
        msg_time: u32,
        now: u32,
    }

    impl SysPort for EffectSys {
        fn now(&self) -> u32 {
            self.now
        }
        fn d3d_fullscreen(&self) -> bool {
            false
        }
        fn fullscreen_foreground(&self) -> bool {
            false
        }
        fn remote_foreground(&self) -> bool {
            false
        }
        fn launcher_open(&self) -> bool {
            true
        }
        fn open(&mut self, _pick: MonitorPick, _top_percent: u8) -> bool {
            true
        }
        fn close_launcher(&mut self) {}
        fn inject(&mut self, _keys: &[(u16, bool)]) {}
        fn win_key(&mut self, vk: u16) {
            self.ops.push(format!("WinKey({vk:#x})"));
        }
        fn translate(&mut self, _rec: &KeyRec) -> Typed {
            Typed::Key
        }
        fn foreground(&self) -> isize {
            0x7777
        }
        fn window_target(&self, h: isize) -> bool {
            h != 0
        }
        fn activate(&mut self, h: isize) {
            self.ops.push(format!("Switch({h:#x})"));
        }
        fn restore(&mut self, h: isize) {
            self.ops.push(format!("Restore({h:#x})"));
        }
        fn copy(&mut self, text: &str) {
            self.ops.push(format!("Copy({text})"));
        }
        fn paste(&mut self) -> Pasted {
            self.ops.push("Paste".into());
            match &self.clip {
                _ if self.private => Pasted::Private,
                Some(text) => Pasted::Text(text.clone()),
                None => Pasted::Empty,
            }
        }
        fn close_window(&mut self, h: isize) {
            self.ops.push(format!("CloseWindow({h:#x})"));
        }
        fn window_menu(&mut self, h: isize) {
            self.ops.push(format!("WindowMenu({h:#x})"));
        }
        fn shell(&mut self, r: ShellRequest) {
            self.ops.push(format!("Shell({r:?})"));
        }
        fn power(&mut self, id: CommandId) {
            self.ops.push(format!("Power({id:?})"));
        }
        fn open_settings(&mut self) {
            self.ops.push("OpenSettings".into());
        }
        fn arm(&mut self, id: usize, ms: u32) {
            self.ops.push(format!("Arm({id}, {ms})"));
        }
        fn fill_titles(&mut self, rows: &mut [WindowRow]) {
            self.ops.push(format!("FillTitles({})", rows.len()));
        }
        fn msg_time(&self) -> u32 {
            self.msg_time
        }
        fn msg_extra(&self) -> usize {
            0
        }
        fn now_ms(&self) -> u64 {
            u64::from(self.now)
        }
        fn number_style(&self) -> NumberStyle {
            NumberStyle { decimal: ',', group: '.' }
        }
        fn close_quiet(&mut self) {
            self.ops.push("CloseQuiet".into());
        }
        fn defer_close(&mut self, seq: u32) -> bool {
            self.ops.push(format!("DeferClose({seq})"));
            true
        }
        fn pt_answer(&mut self, code: usize) {
            self.ops.push(format!("PtAnswer({code})"));
        }
        fn alias(&mut self, op: AliasOp) {
            self.ops.push(format!("Alias({op:?})"));
        }
        fn files(&mut self, open: u32, r: Option<crate::launcher::files::Req>) {
            self.files.push(r.map(|r| r.gen));
            self.files_opens.push(open);
        }
        fn files_end(&mut self) {
            self.files_ends += 1;
        }
    }

    const FG: isize = 0x7777;

    /// A launcher opened over the window FG, `text` typed.
    fn typed(fx: &crate::launcher::providers::tests::Fx, text: &str) -> Launcher {
        let mut l = opened(9);
        l.fg_at_open = FG;
        l.session.chars(text, &fx.cx());
        l
    }

    fn key(
        l: &mut Launcher,
        fx: &crate::launcher::providers::tests::Fx,
        sys: &mut EffectSys,
        vk: u16,
        mods: Mods,
    ) -> Reply {
        let ev = KeyEvent { vk, mods, repeat: false, sys: mods.has(Mods::ALT) };
        l.on_key(ev, None, &fx.cx(), sys)
    }

    /// What the host does on a close: `closing`, the hide, then `after_close`; the sys records "Hide" for the hide.
    fn host_close(l: &mut Launcher, hook: &mut FakeHook, sys: &mut EffectSys) {
        l.closing_on(sys);
        sys.ops.push("Hide".into());
        l.finish(hook, sys);
    }

    /// The ops without the shrink timer the keystrokes arm.
    fn ops(sys: &EffectSys) -> Vec<String> {
        sys.ops.iter().filter(|o| !o.starts_with("Arm(")).cloned().collect()
    }

    #[test]
    fn visible_rows_ask_once_for_their_missing_icons() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let show = |l: &mut Launcher, sys: &mut EffectSys| {
            let mut s = surface(96);
            l.size(&s);
            l.paint_on(&mut s, None, None, sys);
        };
        let asks = |sys: &EffectSys| -> Vec<String> {
            sys.ops.iter().filter(|o| o.starts_with("Shell(StartIconsFor")).cloned().collect()
        };
        let calc = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        let want = format!("Shell({:?})", ShellRequest::StartIconsFor { icon_px: 24, ids: vec![calc.to_string()] });
        // "calc": one request, for the app row on screen (the other rows draw glyphs).
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "calc");
        show(&mut l, &mut sys);
        assert_eq!(asks(&sys), [want], "{:?}", sys.ops);
        // The next keystroke and a repaint show the same app: nothing is asked again in this open.
        l.session.chars("o", &fx.cx());
        show(&mut l, &mut sys);
        show(&mut l, &mut sys);
        assert_eq!(asks(&sys).len(), 1, "{:?}", sys.ops);
        // The close ends the icons child of an open that asked for icons (L3 T20).
        l.housekeeping(&mut sys);
        assert!(sys.ops.iter().any(|o| o == "Shell(LauncherClosed)"), "{:?}", sys.ops);
        // A row whose icon is in hand never asks, and its close has no child to end.
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "calc");
        l.set_icon(calc, IconBitmap::new(24, 24));
        show(&mut l, &mut sys);
        assert!(asks(&sys).is_empty(), "{:?}", sys.ops);
        l.housekeeping(&mut sys);
        assert!(!sys.ops.iter().any(|o| o.contains("LauncherClosed")), "{:?}", sys.ops);
    }

    /// L3 T20: an open takes the cached icons as shared bitmaps: with 40 icons in the cache (30 px, 141 KiB),
    /// `new_launcher` makes no block as large as one bitmap, and its rows point at the cache's own pixels.
    #[test]
    fn open_shares_icons_without_copy() {
        let px = 30u32;
        let bitmap_bytes = (px * px * 4) as usize;
        with_ctrl(|c| {
            *c = Ctrl::new();
            for i in 0..40 {
                c.icons.insert(&format!("vendor.app{i:02}"), Rc::new(IconBitmap::new(px, px)));
            }
        });
        let (l, largest) = crate::launcher::testkit::largest_block_during(new_launcher);
        assert_eq!(l.icons.len(), 40);
        assert!(largest < bitmap_bytes, "a block of {largest} B, a bitmap is {bitmap_bytes} B");
        let shared = with_ctrl(|c| c.icons.iter().all(|(k, b)| l.icons.get(k).is_some_and(|o| Rc::ptr_eq(o, b))));
        assert_eq!(shared, Some(true));
        drop(l);
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn overlay_fits_above_when_needed() {
        // Room below: under the row, 4 px away.
        assert_eq!(overlay_top(100, 144, 250, 600, 64, 4), 148);
        // Not enough room below, enough above: over the row, 4 px away.
        assert_eq!(overlay_top(400, 444, 250, 600, 64, 4), 146);
        // The first row that would clear the field above: the edge case on both sides.
        assert_eq!(overlay_top(318, 362, 250, 600, 64, 4), 64);
        assert_eq!(overlay_top(317, 361, 250, 600, 64, 4), 350);
        // Neither fits: as low as the room allows, never into the field.
        assert_eq!(overlay_top(100, 144, 250, 300, 64, 4), 64);
        assert_eq!(overlay_top(100, 144, 250, 397, 64, 4), 147);
    }

    #[test]
    fn the_action_list_is_laid_out_inside_the_panel_and_hit_by_entry() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "blocco");
        let s = surface(96);
        let (w0, h0) = l.size(&s);
        assert!(l.geo.borrow().actions.is_none());
        key(&mut l, &fx, &mut sys, 0x4B, Mods::CTRL);
        let (w, h) = l.size(&s);
        let g = l.geo.borrow().clone();
        let a = g.actions.as_ref().unwrap();
        assert_eq!(w, w0);
        assert!(h >= h0, "the panel grows to hold the list: {h0} -> {h}");
        // 8 entries (the app's 7 verbs and "Assign alias…") of 34 px and 6 px of inset on each side; the right edge is
        // 24 px from the panel's.
        assert_eq!(a.items.len(), 8);
        assert_eq!((a.rect.right - a.rect.left, a.rect.bottom - a.rect.top), (280, 2 * 6 + 8 * 34));
        assert_eq!(a.rect.right, w - 24);
        assert!(a.rect.top >= g.rows[0].rect.bottom - 1 && a.rect.bottom + 12 <= h);
        // Entries are hit by index, the rest of the panel (rows included) by the backdrop.
        for (k, rc) in a.items.iter().enumerate() {
            assert_eq!(l.hit((rc.left + rc.right) / 2, (rc.top + rc.bottom) / 2), Some(ID_ACTION + k as u32));
        }
        let row = &g.rows[0].rect;
        assert_eq!(l.hit(row.left + 4, row.top + 4), Some(ID_BACKDROP));
        // Closing gives the room back.
        key(&mut l, &fx, &mut sys, 0x1B, Mods::NONE);
        assert!(l.session.action_list().is_none());
        l.hold_shrink = false;
        assert_eq!(l.size(&s), (w0, h0));
        assert!(l.geo.borrow().actions.is_none());
    }

    #[test]
    fn the_action_list_paints_and_closes_with_the_host_keys() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "blocco");
        key(&mut l, &fx, &mut sys, 0x4B, Mods::CTRL);
        for dpi in [96, 144] {
            let mut s = surface(dpi);
            l.size(&s);
            l.paint_on(&mut s, None, None, &mut sys);
        }
        // Switch to: the list's Enter hides after the activation, as the row's Enter does.
        assert_eq!(key(&mut l, &fx, &mut sys, 0x0D, Mods::NONE), Reply::Close);
        assert_eq!(l.switch, Some(0x1001));
        assert!(l.session.action_list().is_none());
    }

    #[test]
    fn titles_only_in_windows_mode() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let fills = |sys: &EffectSys| sys.ops.iter().filter(|o| o.starts_with("FillTitles")).count();
        // Mixed mode: never.
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "note");
        assert_eq!(l.session.mode(), Mode::Mixed);
        l.read_titles(Reply::None, &mut sys);
        l.read_titles(Reply::None, &mut sys);
        assert_eq!(fills(&sys), 0, "{:?}", sys.ops);
        // The "<" mode: once, however many keys follow.
        let mut l = typed(&fx, "<");
        assert_eq!(l.session.mode(), Mode::Only(ProviderId::Windows));
        l.read_titles(Reply::None, &mut sys);
        assert_eq!(fills(&sys), 1, "{:?}", sys.ops);
        l.session.chars("no", &fx.cx());
        l.read_titles(Reply::None, &mut sys);
        assert_eq!(fills(&sys), 1, "once per open: {:?}", sys.ops);
        // A new open reads them again.
        let mut again = typed(&fx, "<");
        again.read_titles(Reply::None, &mut sys);
        assert_eq!(fills(&sys), 2);
    }

    #[test]
    fn launch_failed_reopens_only_within_1_s() {
        assert_eq!(after_failure(0, true), FailurePlan::Reopen);
        assert_eq!(after_failure(1_000, true), FailurePlan::Reopen);
        assert_eq!(after_failure(1_001, true), FailurePlan::NoticeLater);
        assert_eq!(after_failure(200, false), FailurePlan::NoticeLater, "the user moved on to another window");
        assert_eq!(after_failure(u64::MAX, true), FailurePlan::NoticeLater);
    }

    #[test]
    fn a_refusal_that_cannot_reopen_waits_for_the_next_open() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        with_ctrl(|c| c.notice = Some(LaunchKind::App));
        let mut l = new_launcher();
        assert_eq!(with_ctrl(|c| c.notice), Some(None), "shown once");
        l.session.chars("zz", &fx.cx());
        assert_eq!(l.session.rows().first().map(|r| r.kind), Some(Kind::Notice));
        assert_eq!(l.session.rows().first().map(|r| &*r.title), Some(strings::launch_failed()));
        let again = new_launcher();
        assert!(again.session.rows().iter().all(|r| r.kind != Kind::Notice));
    }

    #[test]
    fn save_armed_only_when_changed() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let arms = |sys: &EffectSys, id: usize| sys.ops.iter().filter(|o| o.starts_with(&format!("Arm({id},"))).count();
        let mut l = typed(&fx, "x");
        with_ctrl(|c| c.dirty = false);
        let mut sys = EffectSys::default();
        l.housekeeping(&mut sys);
        assert_eq!(arms(&sys, crate::msg::TIMER_LAUNCHER_SAVE), 0, "{:?}", sys.ops);
        assert!(!sys.ops.iter().any(|o| o.starts_with("Arm(37,")), "no release timer (L3 T19): {:?}", sys.ops);
        // A launched run line changes the history, and only a learning launcher records it.
        for (learn, want) in [(false, false), (true, true)] {
            with_ctrl(|c| {
                c.learn = learn;
                c.dirty = false;
                c.history.clear();
            });
            let mut l = typed(&fx, "x");
            l.queued.push(Effect::Launch(crate::launcher::session::LaunchReq::Command(
                runline::classify("ping 1.1.1.1").unwrap(),
            )));
            let mut hook = FakeHook::default();
            let mut sys = EffectSys::default();
            l.finish(&mut hook, &mut sys);
            assert_eq!(with_ctrl(|c| c.dirty).unwrap(), want, "learn {learn}");
            l.housekeeping(&mut sys);
            // A launching close arms it in both cases: the timer also ends the reopen window of the launch.
            assert_eq!(arms(&sys, crate::msg::TIMER_LAUNCHER_SAVE), 1, "learn {learn}: {:?}", sys.ops);
        }
        // A stale catalogue is listed again after the close, a fresh one is not.
        with_ctrl(|c| c.listed_at_ms = 1);
        let mut l = typed(&fx, "x");
        let mut sys = EffectSys { now: 0, ..EffectSys::default() };
        l.housekeeping(&mut sys);
        assert!(!sys.ops.iter().any(|o| o.contains("ListStartApps")), "{:?}", sys.ops);
        with_ctrl(|c| c.listing_since = None);
        sys.now = 700_000;
        l.housekeeping(&mut sys);
        let child = |o: &String| o.contains("ListStartApps") && o.contains("None") && o.contains("child: true");
        assert!(sys.ops.iter().any(child), "{:?}", sys.ops);
    }

    #[test]
    fn settings_with_the_chord_off_never_build_the_launcher_state() {
        // A thread of its own: its controller state was never touched, and no chord is set on it.
        std::thread::spawn(|| {
            assert!(!ON.with(Cell::get));
            assert!(!pt_detected());
            forget_history();
            // The first look at the state builds it (the prefix table, the web template): so nothing above did.
            let (_, built) = crate::launcher::testkit::allocations_during(|| with_ctrl(|_| ()));
            assert!(built > 0, "the Settings page's calls built the launcher state with the chord off");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_learned_pick_or_forget_arms_the_save() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let saves = |sys: &EffectSys| {
            let id = format!("Arm({},", crate::msg::TIMER_LAUNCHER_SAVE);
            sys.ops.iter().filter(|o| o.starts_with(&id)).count()
        };
        // A new instance of an app is a learned pick: only the frecency changed, and the close arms the save once.
        with_ctrl(|c| *c = Ctrl::new());
        let mut l = typed(&fx, "blocco");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::SHIFT), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(with_ctrl(|c| (c.dirty, c.frec_dirty)), Some((false, true)), "{:?}", sys.ops);
        let mut sys = EffectSys::default();
        l.housekeeping(&mut sys);
        assert_eq!(saves(&sys), 1, "{:?}", sys.ops);
        // Shift+Delete on a learned row: the forget alone arms it too.
        let calc = ItemKey::of(ProviderId::Apps, "calc");
        with_ctrl(|c| {
            *c = Ctrl::new();
            c.learn_pick(calc);
            c.frec_dirty = false;
        });
        let mut l = typed(&fx, "x");
        l.unlearn.push(calc);
        l.forget_runs();
        assert_eq!(with_ctrl(|c| (c.dirty, c.frec_dirty)), Some((false, true)));
        let mut sys = EffectSys::default();
        l.housekeeping(&mut sys);
        assert_eq!(saves(&sys), 1, "{:?}", sys.ops);
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn switch_runs_before_hide_launch_after() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // A running app: its window is activated before the hide, while the dock still holds the foreground.
        let mut l = typed(&fx, "blocco");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE), Reply::Close);
        assert!(ops(&sys).is_empty(), "nothing runs inside the key handler: {:?}", sys.ops);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Switch(0x1001)", "Hide"]);
        // A new instance: hidden first, launched after (Explorer gets the foreground grant).
        let mut l = typed(&fx, "blocco");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::SHIFT), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        let got = ops(&sys);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], "Hide");
        assert!(got[1].starts_with("Shell(LauncherLaunch") && got[1].contains("Microsoft.WindowsNotepad"), "{got:?}");
    }

    #[test]
    fn copy_restores_then_copies() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "=3,5*2");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)", "Copy(7)"]);
        // The window menu acts on the window the user came from, after the restore.
        let mut l = typed(&fx, "window menu");
        let mut sys = EffectSys::default();
        key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)", "WindowMenu(0x7777)"]);
        // Ctrl+C on a selection in the field copies now and the launcher stays.
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        key(&mut l, &fx, &mut sys, 0x41, Mods::CTRL);
        assert_ne!(key(&mut l, &fx, &mut sys, 0x43, Mods::CTRL), Reply::Close);
        assert_eq!(ops(&sys), ["Copy(calc)"]);
    }

    #[test]
    fn clipboard_history_injects_after_restore() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // Win+V goes after the restore, so Windows' panel opens over (and pastes into) the window the user came from.
        let mut l = typed(&fx, "clipboard");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE), Reply::Close);
        assert!(ops(&sys).is_empty(), "nothing before the close: {:?}", sys.ops);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)", "WinKey(0x56)"]);
        // A chord over no window: nothing to restore, the panel still opens.
        let mut l = typed(&fx, "cronologia appunti");
        l.fg_at_open = 0;
        let mut sys = EffectSys::default();
        key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "WinKey(0x56)"]);
    }

    #[test]
    fn window_menu_needs_a_window() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // The chord came over no window, the desktop or one of the dock's: the row says so and Enter does nothing.
        let cx = Context { window_target: false, ..fx.cx() };
        let mut l = typed(&fx, "");
        l.fg_at_open = 0;
        l.session.chars("window menu", &cx);
        let row = l.session.selected().unwrap();
        assert_eq!(row.payload, Payload::Command(CommandId::WindowMenu));
        assert_eq!(row.detail.as_deref(), Some(strings::no_window_to_act_on()), "disabled, with the reason");
        assert_eq!(hint_for(row), None, "no Enter hint on a disabled row");
        let mut sys = EffectSys::default();
        let ev = KeyEvent { vk: VK_RETURN, mods: Mods::NONE, repeat: false, sys: false };
        assert_eq!(l.on_key(ev, None, &cx, &mut sys), Reply::None, "the launcher stays");
        assert!(ops(&sys).is_empty() && l.queued.is_empty(), "{:?} {:?}", sys.ops, l.queued);
        // With a window in front the row is the usual one.
        let l = typed(&fx, "window menu");
        let row = l.session.selected().unwrap();
        assert_eq!((row.detail.as_deref(), hint_for(row).is_some()), (None, true));
        // The chord flow asks at each open whether the window in front can take it.
        for ok in [false, true] {
            let (plan, ..) = post(5, 1_010, 5, FakeSys { menu_ok: ok, ..fresh() });
            assert_eq!(plan, OpenPlan::TryOpen);
            assert_eq!(with_ctrl(|c| c.context().window_target), Some(ok));
        }
    }

    #[test]
    fn ctrl_comma_hides_then_opens_the_settings_page() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, 0xBC, Mods::CTRL), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "OpenSettings"], "no restore: the Settings window takes the foreground");
    }

    #[test]
    fn alt_f4_restores_like_esc() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        assert_eq!(l.on_close_request(&fx.cx(), &mut sys), Reply::Close, "WM_CLOSE and SC_CLOSE");
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)"]);
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, 0x73, Mods::ALT), Reply::Close, "Alt+F4 as a key");
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)"]);
        // The final Esc: the first clears the query, the second closes and restores.
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        assert_ne!(key(&mut l, &fx, &mut sys, 0x1B, Mods::NONE), Reply::Close);
        assert_eq!(key(&mut l, &fx, &mut sys, 0x1B, Mods::NONE), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)"]);
        // The chord again while open: the same.
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        assert_eq!(l.chord_again_on(&fx.cx(), &mut sys), Reply::Close);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(ops(&sys), ["Hide", "Restore(0x7777)"]);
    }

    #[test]
    fn launches_do_not_restore() {
        use crate::launcher::gesture::Gesture;
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        for (q, mods) in [("calcolatrice", Mods::NONE), ("> ping 1.1.1.1", Mods::NONE), ("lock", Mods::NONE)] {
            let mut l = typed(&fx, q);
            let mut sys = EffectSys::default();
            assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, mods), Reply::Close, "{q}");
            let mut hook = FakeHook::default();
            host_close(&mut l, &mut hook, &mut sys);
            let got = ops(&sys);
            assert_eq!(got.first().map(String::as_str), Some("Hide"), "{q}: {got:?}");
            assert!(!got.iter().any(|o| o.starts_with("Restore")), "{q}: {got:?}");
            assert_eq!(got.len(), 2, "{q}: one launch: {got:?}");
            assert_eq!(hook.resets, 0, "{q}");
        }
        // Run as administrator: no restore either, and the hook forgets its tracked modifiers.
        let ctrl_shift = Mods(Mods::CTRL.0 | Mods::SHIFT.0);
        let mut l = typed(&fx, "calcolatrice");
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, ctrl_shift), Reply::Close);
        let mut hook = FakeHook::default();
        host_close(&mut l, &mut hook, &mut sys);
        assert!(ops(&sys)[1].starts_with("Shell(LauncherAppAdmin"), "{:?}", sys.ops);
        assert_eq!(hook.resets, 1);
        // A typed line with the hook's proof of a person's Ctrl+Shift+Enter.
        let mut l = typed(&fx, "> cmd /c dir");
        let key_ = l.session.selected().unwrap().key;
        l.session.set_review_lines(key_, 1);
        let mut sys = EffectSys { msg_time: 5_000, now: 5_010, ..EffectSys::default() };
        let ev = KeyEvent { vk: VK_RETURN, mods: ctrl_shift, repeat: false, sys: false };
        let mut hook = FakeHook { gesture: Some(5_000), ..FakeHook::default() };
        let g = gesture_for(&ev, &mut hook, &sys);
        assert_eq!(g, Some(Gesture::for_test(5_000)));
        assert_eq!(l.on_key(ev, g, &fx.cx(), &mut sys), Reply::Close);
        host_close(&mut l, &mut hook, &mut sys);
        assert!(!ops(&sys).iter().any(|o| o.starts_with("Restore")));
        assert_eq!(hook.resets, 1, "after RunAsTyped too");
        // Only Ctrl+Shift+Enter consumes the hook's evidence; a time that is not the message's is no proof.
        let mut hook = FakeHook { gesture: Some(5_000), ..FakeHook::default() };
        let plain = KeyEvent { vk: VK_RETURN, mods: Mods::NONE, repeat: false, sys: false };
        assert_eq!(gesture_for(&plain, &mut hook, &sys), None);
        assert_eq!(hook.gesture, Some(5_000), "left for the real press");
        let mut hook = FakeHook { gesture: Some(4_999), ..FakeHook::default() };
        assert_eq!(gesture_for(&ev, &mut hook, &sys), None);
    }

    #[test]
    fn inactive_close_never_restores() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // WA_INACTIVE, the retries ended, a lock or a suspend: the host closes without a Hide from the session.
        let mut l = typed(&fx, "calc");
        let mut sys = EffectSys::default();
        let mut hook = FakeHook::default();
        l.on_focus(false, &fx.cx(), &mut hook, &mut sys);
        host_close(&mut l, &mut hook, &mut sys);
        assert_eq!(ops(&sys), ["Hide"]);
        assert_eq!(hook.ends, [9, 9], "the focus loss and the close both end the window");
    }

    #[test]
    fn app_switch_with_no_window_does_nothing() {
        use crate::launcher::providers::tests::Fx;
        let mut fx = Fx::new();
        let mut l = typed(&fx, "blocco");
        fx.windows.clear();
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE), Reply::None, "the launcher stays open");
        assert!(ops(&sys).is_empty());
        assert_eq!(l.switch, None, "never the bar's cycle or minimise");
    }

    #[test]
    fn paste_reads_the_clipboard_into_the_field() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "");
        let mut sys = EffectSys { clip: Some("calc\r\n".into()), ..EffectSys::default() };
        assert_eq!(key(&mut l, &fx, &mut sys, 0x56, Mods::CTRL), Reply::Relayout);
        assert_eq!(l.session.query_text().trim_end(), "calc");
        assert_eq!(sys.ops.first().map(String::as_str), Some("Paste"));
        assert!(sys.ops.contains(&format!("Arm({}, {SHRINK_MS})", crate::msg::TIMER_LAUNCHER_SHRINK)), "{:?}", sys.ops);
        // Nothing on the clipboard: nothing changes.
        let mut sys = EffectSys::default();
        assert_eq!(key(&mut l, &fx, &mut sys, 0x2D, Mods::SHIFT), Reply::None);
    }

    #[test]
    fn private_paste_shows_a_row_and_keeps_the_field() {
        use crate::launcher::model::VerbSet;
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "calc");
        let sel = l.session.selected().map(|r| r.key);
        // The app that copied it marked it private: the port hands nothing over, a notice says why.
        let mut sys = EffectSys { clip: Some("hunter2".into()), private: true, ..EffectSys::default() };
        assert_eq!(key(&mut l, &fx, &mut sys, 0x56, Mods::CTRL), Reply::Relayout);
        assert_eq!(l.session.query_text(), "calc", "the field is unchanged");
        assert_eq!(sys.ops.first().map(String::as_str), Some("Paste"));
        let notices: Vec<&Item> = l.session.rows().iter().filter(|r| r.kind == Kind::Notice).collect();
        assert_eq!(notices.len(), 1);
        assert_eq!(&*notices[0].title, strings::paste_private());
        assert_eq!(notices[0].verbs, VerbSet::empty(), "no action: Enter on it does nothing");
        assert_eq!(l.session.selected().map(|r| r.key), sel, "the typed query's row stays the default");
        // Shift+Insert, marked again: still one notice.
        assert_eq!(key(&mut l, &fx, &mut sys, 0x2D, Mods::SHIFT), Reply::Relayout);
        assert_eq!(l.session.rows().iter().filter(|r| r.kind == Kind::Notice).count(), 1);
        assert_eq!(l.session.query_text(), "calc");
        // A text without a mark pastes as before.
        let mut sys = EffectSys { clip: Some(" x".into()), ..EffectSys::default() };
        key(&mut l, &fx, &mut sys, 0x56, Mods::CTRL);
        assert_eq!(l.session.query_text(), "calc x");
        assert_eq!(strings::paste_private(), "Not pasted: the app that copied it marked it private");
        crate::i18n::with_italian(true, || {
            assert_eq!(strings::paste_private(), "Non incollato: l'app che l'ha copiato lo ha segnato come privato");
        });
    }

    // ---- 15d: config, pass flags, session events, the typo rewrite

    #[test]
    fn opts_from_config() {
        use crate::config::LauncherConfig;
        let mut c = LauncherConfig::default();
        c.prefixes.insert("#".into(), "windows".into());
        c.providers.insert("settings".into(), false);
        c.providers.insert("apps".into(), false);
        c.keep_query = true;
        c.rows = 6;
        let o = super::opts_from_config(&c);
        assert_eq!(o.prefixes.find("#note").map(|(n, id, _)| (n, id)), Some((1, ProviderId::Windows)));
        assert_eq!(o.prefixes.find("=1").map(|(_, id, _)| id), Some(ProviderId::Calc), "the defaults stay");
        assert!(!o.mask.has(ProviderId::Settings));
        assert!(o.mask.has(ProviderId::Apps), "apps are always on");
        assert!(o.mask.has(ProviderId::Calc) && o.mask.has(ProviderId::Web), "a provider not switched off is on");
        assert_eq!((o.keep_query, o.rows), (true, 6));
        assert_eq!(o.web.host(), "www.google.com");
        for (engine, host) in [("bing", "www.bing.com"), ("duckduckgo", "duckduckgo.com"), ("nope", "www.google.com")] {
            c.web_engine = engine.into();
            assert_eq!(super::opts_from_config(&c).web.host(), host, "{engine}");
        }
        c.web_engine = "custom".into();
        c.web_url = "https://search.example.org/?q={q}".into();
        assert_eq!(super::opts_from_config(&c).web.host(), "search.example.org");
        c.web_url = "https://user@search.example.org/?q={q}".into();
        assert_eq!(super::opts_from_config(&c).web.host(), "www.google.com", "userinfo: Google");
        // The defaults of the config are the session's defaults (the prefix table in another order).
        let (d, s) = (super::opts_from_config(&LauncherConfig::default()), LauncherOpts::default());
        assert_eq!((d.mask, &d.web, d.keep_query, d.rows), (s.mask, &s.web, s.keep_query, s.rows));
        for raw in ["=1", ">ping", "<note", "??x", "$disp", ")now", ":reg", "file a", "FILE a", "plain"] {
            assert_eq!(d.prefixes.find(raw), s.prefixes.find(raw), "{raw}");
        }
        // The aliases of live check V-12: four good, two the table ignores (a word with a space, a javascript: target).
        let mut c = LauncherConfig::default();
        for (w, t) in [
            ("l2t", "https://example.com/"),
            ("l2n", "notepad.exe"),
            ("l2d", "C:\\Windows"),
            ("l2c", "app:Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
            ("bad word", "x"),
            ("l2j", "javascript:x"),
        ] {
            c.aliases.insert(w.into(), t.into());
        }
        let (o, ignored) = super::opts_with_ignored(&c);
        assert_eq!((o.aliases.len(), ignored), (4, 2));
        assert!(o.aliases.find("l2d").is_some() && o.aliases.find("l2j").is_none());
        assert_eq!(super::opts_from_config(&c), o);
        assert_eq!(MonitorPick::from_name("active"), MonitorPick::Active);
        assert_eq!(MonitorPick::from_name("primary"), MonitorPick::Primary);
        assert_eq!(MonitorPick::from_name("pointer"), MonitorPick::Pointer);
    }

    #[test]
    fn keep_query_kept_only_when_set_and_cleared_on_lock() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let close = |l: &mut Launcher| host_close(l, &mut FakeHook::default(), &mut EffectSys::default());
        let kept = || with_ctrl(|c| c.last_query.clone()).flatten();
        with_ctrl(|c| {
            c.opts.keep_query = true;
            c.last_query = None;
        });
        let mut l = typed(&fx, "calc");
        close(&mut l);
        assert_eq!(kept().as_deref(), Some("calc"), "in memory only");
        let next = new_launcher();
        assert_eq!((next.session.query_text(), next.session.field().selection()), ("calc", Some(0..4)), "selected");
        // A lock, a disconnect or a suspend: the launcher closes without restoring and the query goes.
        let mut hook = FakeHook::default();
        let mut sys = FakeSys { is_open: true, ..FakeSys::default() };
        session_end(&mut hook, &mut sys);
        assert_eq!(sys.ops, [Op::CloseQuiet], "no restore");
        assert_eq!(hook.resets, 1, "the hook forgets its modifiers");
        assert_eq!(kept(), None);
        assert_eq!(new_launcher().session.query_text(), "");
        // An admin launch clears it too.
        let mut l = typed(&fx, "calcolatrice");
        key(&mut l, &fx, &mut EffectSys::default(), VK_RETURN, CTRL_SHIFT);
        close(&mut l);
        assert_eq!(kept(), None);
        // keep_query off: nothing is kept.
        with_ctrl(|c| c.opts.keep_query = false);
        let mut l = typed(&fx, "calc");
        close(&mut l);
        assert_eq!(kept(), None);
        with_ctrl(|c| c.opts = LauncherOpts::default());
    }

    #[test]
    fn rewrite_applies_only_to_its_generation() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "zzq");
        let mut sys = EffectSys::default();
        let effects = l.session.chars("x", &fx.cx());
        l.run(effects, &fx.cx(), &mut sys);
        assert!(!l.session.has_matches(), "the fixture has nothing for zzqx");
        let gen = l.session.gen();
        assert_eq!(ops(&sys), [format!("Shell(LauncherConvert {{ gen: {gen}, query: \"zzqx\" }})")]);
        // Another generation is dropped.
        assert_eq!(l.converted(gen - 1, "calc", &fx.cx()), Reply::None);
        assert!(!l.session.has_matches());
        // Its own generation: the rows of what Windows read the query as, as one late batch.
        assert_eq!(l.converted(gen, "calc", &fx.cx()), Reply::Relayout);
        assert!(l.session.rows().iter().any(|r| &*r.title == "Calcolatrice"), "{:?}", l.session.rows());
        let own = |r: &&crate::launcher::model::Item| {
            matches!((r.from, r.kind), (ProviderId::Apps, Kind::App) | (ProviderId::Settings, Kind::Setting))
        };
        assert!(l.session.rows().iter().filter(own).all(|r| r.score.tier == crate::launcher::rank::T_REWRITE));
        let web = l.session.rows().iter().filter(|r| r.from == ProviderId::Web).count();
        assert_eq!(web, 1, "one web row, the typed query's");
        // Rows now: a second answer moves nothing.
        assert_eq!(l.converted(gen, "blocco", &fx.cx()), Reply::None);
        // A newer keystroke: the answer for the old one is dropped.
        let mut l = typed(&fx, "zzqx");
        let old = l.session.gen();
        l.session.chars("w", &fx.cx());
        assert_eq!(l.converted(old, "calc", &fx.cx()), Reply::None);
    }

    #[test]
    fn close_ends_the_files_pump() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // The request goes to the pump at the key; every close (Esc, a launch, a focus loss: each ends in the host's
        // close) ends the pump once, at `finish`.
        const ESC: u16 = 0x1B;
        for (how, typed_first, last) in [("esc", "zzq", "x"), ("launch", "blocc", "o"), ("focus", "zzq", "x")] {
            let mut l = typed(&fx, typed_first);
            let mut sys = EffectSys::default();
            let effects = l.session.chars(last, &fx.cx());
            l.run(effects, &fx.cx(), &mut sys);
            assert_eq!(sys.files, [Some(l.session.gen())], "{how}");
            let mut hook = FakeHook::default();
            match how {
                "esc" => {
                    key(&mut l, &fx, &mut sys, ESC, Mods::NONE);
                    assert_eq!(sys.files.last(), Some(&None), "clearing the query withdrew the request");
                    assert_eq!(key(&mut l, &fx, &mut sys, ESC, Mods::NONE), Reply::Close);
                }
                "launch" => assert_eq!(key(&mut l, &fx, &mut sys, VK_RETURN, Mods::SHIFT), Reply::Close),
                _ => {
                    l.on_focus(false, &fx.cx(), &mut hook, &mut sys);
                }
            }
            assert_eq!(sys.files_ends, 0, "{how}: nothing ends inside the key handler");
            host_close(&mut l, &mut hook, &mut sys);
            assert_eq!(sys.files_ends, 1, "{how}");
        }
        // The controller's flow records them too.
        let mut sys = fresh();
        sys.files(1, None);
        sys.files_end();
        assert_eq!((sys.files.len(), sys.files_ends), (1, 1));
    }

    #[test]
    fn files_event_of_an_old_generation_is_dropped() {
        use crate::launcher::files::{self, FileHit, FilesEvent};
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut l = typed(&fx, "zzq");
        let hit = FileHit { path: "C:\\u\\zzq.txt".into(), name: "zzq.txt".into(), folder: "u".into(), dir: false };
        let batch = |gen| {
            let items = files::items(vec![hit.clone()], &["zzq"], 3);
            FilesEvent::Batch(crate::launcher::model::Batch { gen, from: ProviderId::Files, items, done: true })
        };
        let gen = l.session.gen();
        let open = l.open_no;
        assert_eq!(l.files_event(open, batch(gen - 1), &fx.cx()), Reply::None);
        assert!(l.session.rows().iter().all(|r| r.from != ProviderId::Files));
        assert_eq!(l.files_event(open, batch(gen), &fx.cx()), Reply::Relayout);
        let row = l.session.rows().iter().find(|r| r.from == ProviderId::Files).unwrap();
        assert_eq!((&*row.title, &row.icon), ("zzq.txt", &crate::launcher::model::IconRef::Glyph(GlyphId::Document)));
        assert_eq!(l.files_event(open, FilesEvent::Slow(gen - 1), &fx.cx()), Reply::None);
        assert_eq!(l.files_event(open, FilesEvent::Slow(gen), &fx.cx()), Reply::Redraw);
    }

    #[test]
    fn files_event_of_an_earlier_open_is_dropped() {
        use crate::launcher::files::{self, FileHit, FilesEvent, Why};
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // Open 1's pump posts after its close (its kill's reap took long); open 2 is at the same generation by now.
        let mut l = typed(&fx, "file zzq");
        l.open_no = 2;
        let gen = l.session.gen();
        let hit = FileHit { path: "C:\\u\\zzq.txt".into(), name: "zzq.txt".into(), folder: "u".into(), dir: false };
        let items = files::items(vec![hit], &["zzq"], 20);
        let batch = FilesEvent::Batch(Batch { gen, from: ProviderId::Files, items, done: true });
        for ev in [FilesEvent::Off(Why::Stalled), FilesEvent::Stalled(gen), FilesEvent::Slow(gen), batch] {
            assert_eq!(l.files_event(1, ev, &fx.cx()), Reply::None);
        }
        assert!(l.session.rows().iter().all(|r| r.from != ProviderId::Files), "no row of open 1");
        assert!(!l.session.files_busy());
        // Files stay on in open 2: the next key's request goes out, with open 2's number.
        let mut sys = EffectSys::default();
        let effects = l.session.chars("x", &fx.cx());
        l.run(effects, &fx.cx(), &mut sys);
        assert_eq!((sys.files, sys.files_opens), (vec![Some(l.session.gen())], vec![2]));
        // Its own pump's Off is taken.
        assert_eq!(l.files_event(2, FilesEvent::Off(Why::Stalled), &fx.cx()), Reply::Relayout);
    }

    #[test]
    fn rewrite_never_adds_run_open_or_url_rows() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // The rewrite comes from a file in the user's profile: it is searched as a name only, so a line, a prefix, a
        // path, a URL, an expression or a command never comes in as a row the user did not type.
        let meant = ["cmd /c whoami", "> x", "C:\\x.exe", "\\\\h\\s\\x.exe", "https://x.org", "=1+1", "shut down"];
        for m in meant {
            let mut l = typed(&fx, "zzqx");
            assert!(!l.session.has_matches(), "the fixture has nothing for zzqx");
            let gen = l.session.gen();
            assert_eq!(l.converted(gen, m, &fx.cx()), Reply::None, "{m}");
            assert!(!l.session.has_matches(), "{m}: {:?}", l.session.rows());
            assert!(l.session.rows().iter().all(|r| r.kind == Kind::Web), "{m}: {:?}", l.session.rows());
        }
    }

    #[test]
    fn pass_flags_table() {
        for bits in 0u8..32 {
            let [d3d, fs, pass, remote, yld] = [0, 1, 2, 3, 4].map(|i| bits & (1 << i) != 0);
            let f = pass_flags(d3d, fs, pass, remote, yld);
            assert_eq!(f.has(PassFlags::FULLSCREEN), d3d || (fs && pass), "{bits:05b}");
            assert_eq!(f.has(PassFlags::REMOTE), remote, "{bits:05b}");
            assert_eq!(f.has(PassFlags::YIELD), yld, "{bits:05b}");
        }
        // On a foreground change the hook gets the new flags, and a chord's FULLSCREEN is recomputed.
        with_ctrl(|c| {
            c.over_fullscreen_pass = true;
            c.flags = PassFlags::FULLSCREEN;
        });
        let mut hook = FakeHook::default();
        foreground_flow(&mut hook, &FakeSys::default());
        assert_eq!(hook.passes, [PassFlags::default()], "the game is gone: the chord opens again");
        foreground_flow(&mut hook, &FakeSys { fs: true, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::FULLSCREEN));
        with_ctrl(|c| c.over_fullscreen_pass = false);
        foreground_flow(&mut hook, &FakeSys { fs: true, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::default()), "over_fullscreen \"open\"");
        foreground_flow(&mut hook, &FakeSys { d3d: true, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::FULLSCREEN), "exclusive D3D passes anyway");
        with_ctrl(|c| c.over_fullscreen_pass = true);
    }

    #[test]
    fn chord_off_keeps_no_resident_data() {
        let filled = |dirty: bool, learn: bool| {
            let mut c = Ctrl::new();
            c.apps = Arc::new(crate::launcher::arena::test_apps(&[(
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
                "Calcolatrice",
                &[],
                1,
            )]));
            c.settings = SettingsArena::of(&crate::launcher::arena::test_settings(&[("Schermo", &[], 1)]));
            c.windows = vec![window_row(0x1001, "Blocco note")];
            c.icons.insert("microsoft.windowscalculator_8wekyb3d8bbwe!app", Rc::new(IconBitmap::new(24, 24)));
            c.history = vec!["ping 1.1.1.1".into()];
            c.last_query = Some("calc".into());
            c.listed_at_ms = 5;
            c.dirty = dirty;
            c.learn = learn;
            c
        };
        let empty = |c: &Ctrl| {
            c.apps.is_empty()
                && c.settings.is_none()
                && c.windows.is_empty()
                && c.icons.len() == 0
                && c.history.is_empty()
                && c.last_query.is_none()
                && c.listed_at_ms == 0
                && !c.dirty
        };
        // A changed history is written one last time, then nothing is kept.
        let mut c = filled(true, true);
        let save = c.chord_off();
        assert!(
            matches!(&save, Some(ShellRequest::LauncherStateSave { run: Some(r), delete: false, .. }) if !r.is_empty()),
            "{save:?}"
        );
        assert!(empty(&c));
        // Nothing changed, or learn is off: nothing is written.
        let mut c = filled(false, true);
        assert!(c.chord_off().is_none());
        assert!(empty(&c));
        let mut c = filled(true, false);
        assert!(c.chord_off().is_none());
        assert!(empty(&c));
        // A close with the chord still set: the settings mapping and the window rows go, the catalogue and the icons stay.
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        with_ctrl(|c| *c = filled(false, true));
        typed(&fx, "x").housekeeping(&mut FakeSys::default());
        let kept = with_ctrl(|c| (c.settings.is_none(), c.windows.is_empty(), c.apps.len(), c.icons.len())).unwrap();
        assert_eq!(kept, (true, true, 1, 1));
    }

    fn window_row(hwnd: isize, name: &str) -> WindowRow {
        WindowRow {
            hwnd,
            group_key: format!("c:\\apps\\{hwnd:x}.exe").into(),
            aumid: None,
            name: name.into(),
            name_folded: name.to_lowercase().into(),
            title: None,
            hung: false,
        }
    }

    fn held_names() -> Vec<String> {
        with_ctrl(|c| {
            let v = c.context().settings;
            (0..v.len()).map(|i| v.name(i).to_string()).collect()
        })
        .unwrap()
    }

    /// L3 T19: the settings are mapped at the open (with the Settings provider on), searched by every keystroke of it,
    /// and dropped at its close: nothing of them stays between opens and no timer is armed for them.
    #[test]
    fn settings_mapped_at_open_and_dropped_at_close() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut sys = FakeSys {
            settings: Some(crate::launcher::arena::test_settings(&[("Schermo", &[("display", 0)], 1)])),
            ..FakeSys::default()
        };
        settings_at_open(&mut sys);
        assert_eq!((sys.maps, sys.ensures.len()), (1, 0));
        assert_eq!(held_names(), ["Schermo"]);
        with_ctrl(|c| c.windows = vec![window_row(0x1001, "Blocco note")]);
        let mut l = typed(&fx, "schermo");
        l.housekeeping(&mut sys);
        assert_eq!(with_ctrl(|c| (c.settings.is_none(), c.windows.is_empty())), Some((true, true)));
        assert!(held_names().is_empty());
        assert!(!sys.ops.iter().any(|o| matches!(o, Op::Arm(37, _))), "{:?}", sys.ops);
        // The next open maps again.
        settings_at_open(&mut sys);
        assert_eq!((sys.maps, held_names()), (2, vec!["Schermo".to_string()]));
    }

    /// L3 T19: with the Settings provider off nothing is mapped or asked, and a held arena is not searched.
    #[test]
    fn no_settings_when_the_provider_is_off() {
        with_ctrl(|c| c.opts.mask = c.opts.mask.with(ProviderId::Settings, false));
        let mut sys = FakeSys {
            settings: Some(crate::launcher::arena::test_settings(&[("Schermo", &[], 1)])),
            ..FakeSys::default()
        };
        settings_at_open(&mut sys);
        settings_ready_on(true, &mut sys);
        assert_eq!((sys.maps, sys.ensures.len()), (0, 0));
        assert_eq!(with_ctrl(|c| c.settings.is_none()), Some(true));
        with_ctrl(|c| c.settings = SettingsArena::of(&crate::launcher::arena::test_settings(&[("Schermo", &[], 1)])));
        assert!(held_names().is_empty(), "the provider is off: the context has no settings");
        sys.miss = Some(SettingsMiss::NoFile);
        settings_at_open(&mut sys);
        assert_eq!((sys.maps, sys.ensures.len(), with_ctrl(|c| c.settings.is_none())), (0, 0, Some(true)));
    }

    /// L3 T19: a mapping is never replaced while the launcher is open (an Enter never meets an entry index of another
    /// file); an open that holds none maps when the settings are ready; a closed launcher maps nothing; a missing or
    /// failing file asks the settings thread once per open, and nothing without a key.
    #[test]
    fn a_mapping_is_never_replaced_while_open() {
        let mut sys = FakeSys {
            settings: Some(crate::launcher::arena::test_settings(&[("Schermo", &[], 1)])),
            ..FakeSys::default()
        };
        settings_at_open(&mut sys);
        sys.settings = Some(crate::launcher::arena::test_settings(&[("Bluetooth", &[], 1), ("Schermo", &[], 2)]));
        settings_ready_on(true, &mut sys);
        assert_eq!((sys.maps, held_names()), (1, vec!["Schermo".to_string()]), "held: kept as it is");
        // No file at the open: the thread is asked to make it; the rows come with the event, only while open.
        sys.miss = Some(SettingsMiss::NoFile);
        settings_at_open(&mut sys);
        assert_eq!((sys.maps, sys.ensures.clone()), (2, vec![false]));
        assert!(held_names().is_empty());
        sys.miss = None;
        settings_ready_on(false, &mut sys);
        assert_eq!(sys.maps, 2, "closed: nothing mapped");
        settings_ready_on(true, &mut sys);
        assert_eq!((sys.maps, held_names()), (3, vec!["Bluetooth".to_string(), "Schermo".to_string()]));
        // A file that keeps failing: one rebuild per open, however often the event comes.
        sys.miss = Some(SettingsMiss::Refused);
        settings_at_open(&mut sys);
        settings_ready_on(true, &mut sys);
        settings_ready_on(true, &mut sys);
        assert_eq!((sys.maps, sys.ensures.clone()), (6, vec![false, true]));
        settings_at_open(&mut sys);
        assert_eq!(sys.ensures, [false, true, true], "the next open asks again");
        // No key: no file is ever written, so nothing is asked.
        sys.miss = Some(SettingsMiss::NoKey);
        settings_at_open(&mut sys);
        assert_eq!(sys.ensures.len(), 3);
    }

    /// L3 T19: a settings row's launch takes its target and arguments from the arena this open searched, at the close,
    /// before the close drops it; an index the arena does not have launches nothing.
    #[test]
    fn setting_launch_reads_the_held_arena() {
        use crate::launcher::model::SettingRow;
        use crate::launcher::providers::tests::Fx;
        use crate::launcher::session::LaunchReq;
        use crate::start_search::Keys;
        let fx = Fx::new();
        let entry = |i: u32, name: &str, target: &str, args: Option<&str>| SettingIn {
            row: SettingRow::new(i, name, Keys::default(), i),
            target: target.into(),
            args: args.map(str::to_string),
            start_keys: String::new(),
        };
        let ins = [
            entry(0, "Schermo", "ms-settings:display", None),
            entry(1, "Mouse", r"C:\Windows\System32\control.exe", Some("/name Microsoft.Mouse")),
        ];
        let bytes = arena::encode_settings("test|fmt 1", &ins, Caps::SETTINGS).unwrap();
        let held = Arena::new(bytes, ArenaKind::Settings, None, Caps::SETTINGS).unwrap();
        with_ctrl(|c| c.settings = SettingsArena::of(&held));
        let mut l = typed(&fx, "mouse");
        l.queued.push(Effect::Launch(LaunchReq::Setting { index: 1 }));
        l.queued.push(Effect::Launch(LaunchReq::Setting { index: 7 }));
        let mut hook = FakeHook::default();
        let mut sys = EffectSys::default();
        l.finish(&mut hook, &mut sys);
        let launches: Vec<&String> = sys.ops.iter().filter(|o| o.contains("LauncherLaunch")).collect();
        assert_eq!(launches.len(), 1, "{:?}", sys.ops);
        assert!(launches[0]
            .contains(r#"target: "C:\\Windows\\System32\\control.exe", args: Some("/name Microsoft.Mouse")"#));
        l.housekeeping(&mut sys);
        assert_eq!(with_ctrl(|c| c.settings.is_none()), Some(true), "dropped after the launch took its target");
    }

    /// L3 T19, MEM-3: the open maps a real cache file through the dock's own code and allocates almost nothing for it on
    /// the heap (the bytes are the file's pages); the close unmaps it (the file can be deleted again: no handle is left).
    #[test]
    fn prepare_open_allocates_no_settings_data() {
        use crate::start_search::{Keys, Setting};
        let dir = std::env::temp_dir().join(format!("dock_map_open_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings-catalog.bin");
        let list: Vec<Setting> = (0..1_500)
            .map(|i| {
                let name = format!("Impostazione numero {i}");
                let mut keys = Keys::default();
                keys.push(&format!("parola{i}"), 3);
                keys.push(&format!("chiave {i}"), crate::start_search::XML_KEYWORD_PENALTY);
                Setting {
                    lower: name.to_lowercase(),
                    name,
                    keys: keys.tidy(),
                    rank: i,
                    target: std::borrow::Cow::Borrowed("ms-settings:"),
                    args: Some(format!("page=p{i}")),
                }
            })
            .collect();
        let key = "test|fmt 1";
        let bytes = catalog::encode_setting_list(key, &list).unwrap();
        assert!(bytes.len() > 100_000, "{}", bytes.len());
        std::fs::write(&path, &bytes).unwrap();
        let mut sys = FakeSys { file: Some((path.clone(), key.into())), ..FakeSys::default() };
        let ((), peak) = crate::launcher::testkit::peak_bytes_during(|| settings_at_open(&mut sys));
        assert!(peak <= 8 * 1024, "{peak} B on the heap for the settings");
        let held = with_ctrl(|c| c.settings.as_ref().map(|s| (s.is_mapped(), s.len()))).flatten();
        assert_eq!(held, Some((true, 1_500)));
        assert!(std::fs::remove_file(&path).is_err(), "mapped: nobody can delete or replace the file");
        use crate::launcher::providers::tests::Fx;
        typed(&Fx::new(), "x").housekeeping(&mut sys);
        std::fs::remove_file(&path).expect("unmapped at the close");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pick_is_not_written_before_the_files_were_read() {
        let key = ItemKey::of(ProviderId::Apps, "calc");
        with_ctrl(|c| {
            *c = Ctrl::new();
            c.learn_pick(key);
        });
        save_state();
        assert_eq!(with_ctrl(|c| c.frec_dirty), Some(true), "kept for the load");
        with_ctrl(|c| c.state_arrived = true);
        save_state();
        assert_eq!(with_ctrl(|c| c.frec_dirty), Some(false), "written once the files were read");
    }

    #[test]
    fn a_launching_close_forgets_its_query_at_the_save_timer() {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        // Not learning: nothing to write, the close still arms the timer for the query.
        with_ctrl(|c| {
            *c = Ctrl::new();
            c.learn = false;
        });
        let mut l = typed(&fx, "ping");
        l.queued.push(Effect::Launch(crate::launcher::session::LaunchReq::Command(
            runline::classify("ping 1.1.1.1").unwrap(),
        )));
        let mut sys = EffectSys { now: 5_000, ..EffectSys::default() };
        l.finish(&mut FakeHook::default(), &mut sys);
        l.housekeeping(&mut sys);
        let id = format!("Arm({},", crate::msg::TIMER_LAUNCHER_SAVE);
        assert_eq!(sys.ops.iter().filter(|o| o.starts_with(&id)).count(), 1, "{:?}", sys.ops);
        assert_eq!(with_ctrl(|c| c.failed_query.clone()), Some(Some("ping".into())), "kept for the reopen");
        // Up to the end of the reopen window the query stays; past it, it goes.
        with_ctrl(|c| c.reopen_over(5_000 + REOPEN_WITHIN_MS));
        assert!(with_ctrl(|c| c.failed_query.is_some()).unwrap());
        with_ctrl(|c| c.reopen_over(5_001 + REOPEN_WITHIN_MS));
        assert_eq!(with_ctrl(|c| c.failed_query.clone()), Some(None));
        // The timer itself, on the real clock (the hide at 0 is long past).
        with_ctrl(|c| {
            c.hidden_at_ms = 0;
            c.failed_query = Some("ping".into());
        });
        save_state();
        assert_eq!(with_ctrl(|c| c.failed_query.clone()), Some(None));
        // A close that launched nothing keeps no query and arms nothing.
        let mut l = typed(&fx, "ping");
        let mut sys = EffectSys::default();
        l.housekeeping(&mut sys);
        assert!(!sys.ops.iter().any(|o| o.starts_with(&id)), "{:?}", sys.ops);
        assert_eq!(with_ctrl(|c| c.failed_query.clone()), Some(None));
    }

    #[test]
    fn frecency_is_kept_saved_forgotten_and_dropped() {
        let key = ItemKey::of(ProviderId::Apps, "calc");
        // A pick marks the state changed; only the changed file is written.
        let mut c = Ctrl::new();
        c.learn_pick(key);
        assert!(c.frec_dirty && !c.dirty);
        assert!(c.context().learned.boost(key, i64::MAX / 2) > 0);
        let mut again = Ctrl::new();
        again.frecency = Frecency::parse(&c.frecency.render());
        assert_eq!(again.frecency, c.frecency);
        let save = c.chord_off();
        assert!(
            matches!(&save, Some(ShellRequest::LauncherStateSave { frecency: Some(f), run: None, delete: false }) if f.starts_with(b"mini-oled-dock launcher-frecency 1\n")),
            "{save:?}"
        );
        assert!(c.frecency.is_empty() && !c.frec_dirty, "chord Off keeps nothing");
        // Learning off: nothing is counted, and switching it off drops what there was.
        let mut c = Ctrl::new();
        c.learn = false;
        c.learn_pick(key);
        assert!(c.frecency.is_empty() && !c.frec_dirty);
        let mut c = Ctrl::new();
        c.learn_pick(key);
        c.configure(&LauncherConfig { learn: false, ..LauncherConfig::default() });
        assert!(c.frecency.is_empty() && !c.frec_dirty);
        // Shift+Delete on a row, and a pick that arrived before the file did.
        let mut l = Launcher::with_session(Session::open(&LauncherOpts::default(), None));
        l.unlearn.push(key);
        with_ctrl(|c| {
            *c = Ctrl::new();
            c.learn_pick(key);
            c.frec_dirty = false;
        });
        l.forget_runs();
        assert_eq!(with_ctrl(|c| (c.frecency.is_empty(), c.frec_dirty)), Some((true, true)));
        let mut loaded = Frecency::new();
        loaded.pick(key, 1_700_000_000, false);
        let mut mine = Frecency::new();
        mine.pick(key, 1_800_000_000, false);
        loaded.absorb(&mine);
        assert_eq!(loaded.boost(key, 1_800_000_000), 2 * 16, "the counts add, the later time wins");
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn remote_client_in_front_passes_the_chord() {
        // A remote-desktop or VM client in front: REMOTE, so the hook passes the chord to its session (no post); a
        // fullscreen session keeps it with over_fullscreen "open" and through a FULLSCREEN pass note; a local window in
        // front clears it.
        with_ctrl(|c| {
            c.over_fullscreen_pass = true;
            c.flags = PassFlags::default();
        });
        let mut hook = FakeHook::default();
        foreground_flow(&mut hook, &FakeSys { remote: true, ..FakeSys::default() });
        assert_eq!(hook.passes, [PassFlags::REMOTE]);
        assert_eq!(with_ctrl(|c| c.flags), Some(PassFlags::REMOTE));
        with_ctrl(|c| c.over_fullscreen_pass = false);
        foreground_flow(&mut hook, &FakeSys { remote: true, fs: true, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::REMOTE), "over_fullscreen \"open\": still the session's");
        with_ctrl(|c| c.over_fullscreen_pass = true);
        pass_note(&mut hook, &FakeSys { remote: true, fs: true, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::FULLSCREEN.with(PassFlags::REMOTE)));
        foreground_flow(&mut hook, &FakeSys::default());
        assert_eq!(hook.passes.last(), Some(&PassFlags::default()), "a local window: the chord opens again");
        assert_eq!(with_ctrl(|c| c.flags), Some(PassFlags::default()));
    }

    #[test]
    fn pass_note_rechecks_fullscreen() {
        // The note of a chord passed under FULLSCREEN computes the flags again: still fullscreen, the next chord passes
        // too; the window left fullscreen without a foreground change, the flag clears and the next chord opens.
        with_ctrl(|c| {
            c.over_fullscreen_pass = true;
            c.flags = PassFlags::FULLSCREEN;
        });
        let mut hook = FakeHook::default();
        pass_note(&mut hook, &FakeSys { fs: true, ..FakeSys::default() });
        assert_eq!(hook.passes, [PassFlags::FULLSCREEN]);
        pass_note(&mut hook, &FakeSys { fs: false, ..FakeSys::default() });
        assert_eq!(hook.passes.last(), Some(&PassFlags::default()), "the stale flag clears");
        assert_eq!(with_ctrl(|c| c.flags), Some(PassFlags::default()));
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
        l.finish(&mut h, &mut FakeSys::default());
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
        assert_eq!(empty.size(&s), (640, 72), "empty: the field alone, 12 + 48 + 12");
        let mut l = many_rows();
        let (w, h) = l.size(&s);
        let n = l.session.rows().len();
        assert!(n >= 5, "the fixture gives five rows or more, got {n}");
        assert_eq!((w, h), (640, 68 + 44 * i32::try_from(n).unwrap() + 12));
        let g = l.geo.borrow().clone();
        assert_eq!((g.field.left, g.field.top, g.field.right, g.field.bottom), (12, 12, 628, 60));
        let first = g.rows.first().unwrap();
        assert_eq!((first.rect.left, first.rect.top, first.rect.right), (12, 68, 628));
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
    fn eight_rows_are_432_high() {
        // Eight rows of 44 and nothing else: 68 + 8 * 44 + 12.
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::Apps);
        l.max_rows = 8;
        let (_, h) = l.size(&s);
        let n = i32::try_from(l.session.rows().len()).unwrap().min(8);
        assert_eq!(h, 68 + n * 44 + 12);
        assert_eq!(68 + 8 * 44 + 12, 432);
    }

    /// The DIB of the real run-review scene at 144 DPI, with the rows below the review filled to `rows` at the measured
    /// row height: (width, height, bytes).
    fn review_dib(width: i32, rows: u8) -> (i32, i32, i64, usize) {
        let s = surface(144);
        let mut l = scene_launcher(PreviewScene::RunReview);
        l.set_shape(width, rows);
        let (w, h) = l.size(&s);
        let g = l.geo.borrow().clone();
        let lines = g.rows.iter().map(|r| r.lines.len()).max().unwrap_or(0);
        let have = i32::try_from(g.rows.len()).unwrap();
        let row_h = s.metrics().px(44);
        let full = h + (i32::from(rows) - have).max(0) * row_h;
        (w, full, i64::from(w) * i64::from(full) * 4, lines)
    }

    #[test]
    fn dib_at_144_dpi_within_2_6_mib() {
        // The gate covers the default shape: 640 wide, 8 rows, the selected run line reviewed on 3 lines.
        let (w, h, bytes, lines) = review_dib(640, 8);
        assert_eq!(lines, 3, "the scene's review wraps to three lines");
        assert_eq!(w, 960);
        assert!(bytes <= 2_726_297, "{w} x {h} is more than 2.6 MiB: {bytes}");
        // Recorded, not gated at 2.6: wider or taller shapes are bigger (SPEC 5.5.6 allows about +4.5 MiB open at 150 %).
        let (w, h, bytes, _) = review_dib(880, 8);
        assert!(bytes <= 3_800_000, "880 wide, 8 rows: {w} x {h} = {bytes}");
        let (w, h, bytes, _) = review_dib(640, 12);
        assert!(bytes <= 3_700_000, "640 wide, 12 rows: {w} x {h} = {bytes}");
    }

    #[test]
    fn rows_shrink_to_fit_the_work_area() {
        let s = surface(96);
        let mut l = many_rows();
        l.size(&s);
        let all = l.geo.borrow().rows.len();
        assert!(all >= 4);
        // Room for three rows only.
        l.limit((i32::MAX, 68 + 3 * 44 + 12));
        let (_, h) = l.size(&s);
        assert_eq!(l.geo.borrow().rows.len(), 3);
        assert!(h <= 68 + 3 * 44 + 12, "the panel stays inside the limit: {h}");
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
        assert_eq!(g.size(&s).1, 72);
    }

    /// A context with nothing in it: a keystroke finds no row.
    fn scene_context_free() -> crate::launcher::providers::Context<'static> {
        use crate::launcher::model::NumberStyle;
        static OPTS: std::sync::OnceLock<LauncherOpts> = std::sync::OnceLock::new();
        static LEARNED: std::sync::OnceLock<Frecency> = std::sync::OnceLock::new();
        let opts = OPTS.get_or_init(LauncherOpts::default);
        crate::launcher::providers::Context {
            apps: Default::default(),
            settings: Default::default(),
            windows: &[],
            history: &[],
            opts,
            num: NumberStyle { decimal: '.', group: ',' },
            learned: LEARNED.get_or_init(Frecency::new),
            now_unix: 0,
            utc_offset_min: 0,
            time_style: Default::default(),
            window_target: true,
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
        let sel = l.session.selected_index();
        let g = l.geo.borrow();
        let row = g.rows.iter().find(|r| Some(r.idx) == sel).unwrap();
        assert_eq!(row.lines, [runs_of("Line too long to review")], "the detail says why");
    }

    /// A launcher with the query typed one character at a time, painted after each key the way `popup::render(true)` does.
    fn type_and_paint(text: &str, mut each: impl FnMut(&mut Launcher, &mut Surface)) {
        use crate::launcher::providers::tests::Fx;
        let fx = Fx::new();
        let mut s = surface(96);
        let mut l = typed(&fx, "");
        l.foreground = true;
        for ch in text.chars() {
            l.session.chars(&ch.to_string(), &fx.cx());
            each(&mut l, &mut s);
        }
    }

    #[test]
    fn one_layout_per_key() {
        let mut keys = 0;
        type_and_paint("calcolatrice", |l, s| {
            LAYOUTS.with(|c| c.set(0));
            l.size(s);
            l.paint_on(s, None, None, &mut EffectSys::default());
            assert_eq!(LAYOUTS.with(|c| c.get()), 1, "one layout for the key");
            keys += 1;
        });
        assert_eq!(keys, 12);
        // A render that is not a key (hover, a scrollbar tick) lays out once, as before.
        let l = scene_launcher(PreviewScene::Apps);
        let mut s = surface(96);
        LAYOUTS.with(|c| c.set(0));
        l.paint_on(&mut s, None, None, &mut EffectSys::default());
        assert_eq!(LAYOUTS.with(|c| c.get()), 1);
    }

    #[test]
    fn field_measured_once_per_key() {
        // Every GDI text-extent call the field makes counts: the query's extents, the runs drawn, the chip and its label.
        for text in ["calcolatrice", "$ blue"] {
            let mut chip = false;
            type_and_paint(text, |l, s| {
                FIELD_EXTENTS.with(|c| c.set(0));
                l.size(s);
                l.paint_on(s, None, None, &mut EffectSys::default());
                let calls = FIELD_EXTENTS.with(|c| c.get());
                assert!(calls <= 2, "{calls} measurements of the field for one key of {text:?}");
                assert!(calls >= 1 || l.session.query_text().is_empty(), "the field of {text:?} was measured");
                chip |= l.session.chip().is_some();
            });
            assert_eq!(chip, text.starts_with('$'), "the mode chip of {text:?}");
        }
    }

    #[test]
    fn marker_extents_in_one_call() {
        let s = surface(96);
        let (mut shown, mut out) = (String::new(), Vec::new());
        for text in ["ab\u{202E}cd", "\u{7}x\u{0}", "C:\\a\u{1b}b\u{202E}", "\u{202E}"] {
            let before = crate::surface::EXTENT_CALLS.with(Cell::get);
            query_extents(&s, text, &mut shown, &mut out);
            assert_eq!(crate::surface::EXTENT_CALLS.with(Cell::get) - before, 1, "one measurement of {text:?}");
            // The widths character by character, each by its drawn text, as the field measured them before.
            let mut total = 0;
            let each: Vec<i32> = text
                .chars()
                .map(|c| {
                    let mut buf = [0u8; 4];
                    total += s.text_width(marker_text(c).unwrap_or_else(|| c.encode_utf8(&mut buf)), Font::Query);
                    total
                })
                .collect();
            assert_eq!(out, each, "the extents of {text:?}");
        }
    }

    #[test]
    fn text_paths_allocate_nothing() {
        let mut s = surface(96);
        let mut l = scene_launcher(PreviewScene::Empty);
        l.foreground = true;
        l.session_mut().chars("calcolatrice 12345678", &scene_context_free());
        l.size(&s);
        let g = l.geo.borrow().clone();
        l.paint_field(&mut s, &g, false);
        let ((), n) = crate::launcher::testkit::allocations_during(|| l.paint_field(&mut s, &g, false));
        assert_eq!(n, 0, "painting the field allocates nothing after the first paint");
        // With a marker in the text too.
        l.session_mut().chars("\u{202E}x", &scene_context_free());
        l.size(&s);
        let g = l.geo.borrow().clone();
        l.paint_field(&mut s, &g, false);
        let ((), n) = crate::launcher::testkit::allocations_during(|| l.paint_field(&mut s, &g, false));
        assert_eq!(n, 0, "nor with a marker in it");
    }

    /// What `size` and `paint_on` did before: lay out, set the review lines, store that layout, paint with a second one.
    fn size_and_paint_two_layouts(l: &mut Launcher, s: &mut Surface) {
        let g = l.layout(s);
        if let (Some(lines), Some(key)) = (l.review_lines(s), l.session.selected().map(|i| i.key)) {
            l.session.set_review_lines(key, lines);
        }
        *l.geo.borrow_mut() = g;
        l.fresh.set(false);
        l.paint_on(s, None, None, &mut EffectSys::default());
    }

    #[test]
    fn review_change_paints_like_two_layouts() {
        for line in
            [format!("> {}", "WWWWWWW ".repeat(40)), format!("> {}", "alpha beta ".repeat(14)), "> calc".to_string()]
        {
            let make = || {
                let mut l = scene_launcher(PreviewScene::Empty);
                l.foreground = true;
                l.session_mut().chars(&line, &scene_context_free());
                l
            };
            let (mut a, mut b) = (make(), make());
            let (mut sa, mut sb) = (surface(96), surface(96));
            a.size(&sa);
            a.paint_on(&mut sa, None, None, &mut EffectSys::default());
            size_and_paint_two_layouts(&mut b, &mut sb);
            assert!(sa.snapshot_pixels() == sb.snapshot_pixels(), "pixels differ for {line:.20}");
        }
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
        let (_, room, _) = l.query_room(&s, &g.field);
        let mut extents = Vec::new();
        query_extents(&s, l.session.query_text(), &mut String::new(), &mut extents);
        let caret = prefix_width(&extents, l.session.query_text(), l.session.field().caret());
        assert!(caret - l.scroll.get() <= room, "the caret is inside the room");
        // A short text does not scroll.
        let mut short = scene_launcher(PreviewScene::Apps);
        short.size(&s);
        assert_eq!(short.scroll.get(), 0);
    }

    #[test]
    fn markers_are_visible_and_split_from_plain_text() {
        assert_eq!(marker_text('\u{7}'), Some("\u{2407}"));
        assert_eq!(marker_text('\u{0}'), Some("\u{2400}"));
        assert_eq!(marker_text('\u{7f}'), Some("\u{2421}"));
        assert_eq!(marker_text('\u{202E}'), Some("<U+202E>"));
        assert_eq!(marker_text('\u{85}'), Some("<U+0085>"));
        // Every marked character has a text of its own (the table is the closed set of `gesture::is_marker`).
        for u in 0..=0x2FFFu32 {
            if let Some(c) = char::from_u32(u).filter(|c| gesture::is_marker(*c)) {
                assert_ne!(marker_text(c), Some("\u{FFFD}"), "U+{u:04X}");
            }
        }
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
        let lines = wrap(&s, text, Font::Caption, w, w, 10);
        assert!(lines.len() >= 3);
        for l in &lines {
            let line_w: i32 = l.iter().map(|r| s.text_width(&r.text, Font::Caption)).sum();
            assert!(line_w <= w + 4, "a line is within the width: {line_w} > {w}");
        }
        let joined: String = lines.iter().flatten().map(|r| &*r.text).collect();
        assert_eq!(joined, text, "nothing is lost or ellipsized");
        let cut = wrap(&s, text, Font::Caption, w, w, 2);
        assert_eq!(cut.len(), 3, "one line past the limit tells the caller it does not fit");
        // A word longer than the line breaks inside it.
        let long = wrap(&s, &"x".repeat(200), Font::Caption, w, w, 10);
        assert!(long.len() >= 2);
    }

    #[test]
    fn the_first_line_may_be_narrower_than_the_rest() {
        let s = surface(96);
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
        let rest = s.text_width("alpha beta gamma delta epsilon", Font::Caption);
        let first = s.text_width("alpha beta gamma", Font::Caption);
        let lines = wrap(&s, text, Font::Caption, first, rest, 10);
        let width = |l: &[Run]| l.iter().map(|r| s.text_width(&r.text, Font::Caption)).sum::<i32>();
        assert!(width(&lines[0]) <= first, "the first line stops at its own width");
        assert!(lines.iter().skip(1).any(|l| width(l) > first), "the others use the whole width");
        let joined: String = lines.iter().flatten().map(|r| &*r.text).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn the_detail_stops_before_the_hint_column() {
        for dpi in [96, 120, 144, 192] {
            let s = surface(dpi);
            let mut l = scene_launcher(PreviewScene::RunReview);
            l.size(&s);
            let m = s.metrics();
            let item = l.session.selected().unwrap().clone();
            let hint = hint_for(&item).unwrap();
            let hint_left = hint_right(m, l.width) - s.text_width(hint, Font::Caption);
            let (first, rest) = l.detail_widths(&s, &item, true);
            assert!(
                m.px(style::NAME_X) + first <= hint_left - m.px(HINT_GAP),
                "{dpi}: the column ends before the hint"
            );
            assert!(first < rest, "{dpi}: only the first line gives way");
            assert_eq!(l.detail_widths(&s, &item, false), (rest, rest), "{dpi}: no hint, no give");
            let g = l.geo.borrow().clone();
            let row = g.rows.iter().find(|r| Some(r.idx) == l.session.selected_index()).unwrap();
            let width = |line: &[Run]| line.iter().map(|r| s.text_width(&r.text, Font::Caption)).sum::<i32>();
            assert!(width(&row.lines[0]) <= first, "{dpi}: the review's first line is within it");
            assert!(
                row.lines.iter().skip(1).all(|line| width(line) <= rest),
                "{dpi}: the others are within the names' width"
            );
        }
        // An unselected row, or a launcher without the focus, shows no hint: the detail has the whole width.
        let s = surface(96);
        let mut l = scene_launcher(PreviewScene::Inactive);
        l.size(&s);
        let item = l.session.selected().unwrap().clone();
        let (first, rest) = l.detail_widths(&s, &item, true);
        assert_eq!(first, rest);
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
        assert_eq!(
            hint_for(run.session.selected().unwrap()),
            Some("Run in console · Enter"),
            "ping: a console program"
        );
        let mut line = scene_launcher(PreviewScene::Empty);
        line.session_mut().chars("> notepad", &scene_context_free());
        assert_eq!(hint_for(line.session.selected().unwrap()), Some("Run · Enter"));
        let apps = scene_launcher(PreviewScene::Apps);
        assert_eq!(hint_for(apps.session.selected().unwrap()), Some("Open · Enter"));
    }

    #[test]
    fn wheel_scrolls_without_moving_the_selection_back() {
        let s = surface(96);
        let mut l = many_rows();
        l.limit((i32::MAX, 68 + 3 * 44 + 12));
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
            "launcher_time",
            "launcher_units",
            "launcher_run",
            "launcher_run_review",
            "launcher_windows",
            "launcher_notice",
            "launcher_notice_actions",
            "launcher_notice_took_over",
            "launcher_error",
            "launcher_field_pair",
            "launcher_alias",
            "launcher_alias_prompt",
            "launcher_files",
            "launcher_files_busy",
            "launcher_files_mode",
            "launcher_search_off",
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
    fn cached_glyph_equals_uncached() {
        use GlyphId::*;
        let all = [
            Magnifier, Equals, Prompt, Window, Document, Image, Archive, Executable, Copy, Globe, Clock, Folder, Info,
            Gear, Lock, Moon, Restart, Power, Exit,
        ];
        let mut s = Surface::new(Metrics { dpi: 96 }).expect("surface");
        s.resize(64, 64).unwrap();
        let snap = |s: &Surface| s.snapshot_pixels().expect("pixels").2;
        for g in all {
            for size in 12..=48 {
                for alpha in [255, 90] {
                    s.fill_rr(rect_wh(0, 0, 64, 64), 0, (30, 40, 50), 255);
                    s.glyph(5, 5, size, alpha, glyph_shape(g));
                    let plain = snap(&s);
                    s.fill_rr(rect_wh(0, 0, 64, 64), 0, (30, 40, 50), 255);
                    s.glyph_cached(g as u32, 5, 5, size, alpha, glyph_shape(g));
                    assert!(plain == snap(&s), "{g:?} at {size}, alpha {alpha}, first paint");
                    s.fill_rr(rect_wh(0, 0, 64, 64), 0, (30, 40, 50), 255);
                    s.glyph_cached(g as u32, 5, 5, size, alpha, glyph_shape(g));
                    assert!(plain == snap(&s), "{g:?} at {size}, alpha {alpha}, from the cache");
                }
            }
            // the cache is bounded: start afresh for the next glyph
            s.glyph_hits_clear();
        }
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

    // ---- L1b-5: PowerToys

    /// The controller with the chord on Alt+Space and a chord owner in `state` (the notice not answered for Detected).
    fn pt_ctrl(state: PtState) {
        with_ctrl(|c| {
            c.pt_on = true;
            c.pt = Pt::new();
            match state {
                PtState::Absent | PtState::Detected => {}
                PtState::Yield { .. } => c.pt.configure(false, true),
                PtState::Take => c.pt.configure(true, true),
            }
            if state != PtState::Absent {
                c.pt.on(PtEvent::Found);
            }
        });
    }

    fn pt_state() -> PtState {
        with_ctrl(|c| c.pt.state()).unwrap()
    }

    fn yields(hook: &FakeHook) -> bool {
        hook.passes.last().is_some_and(|f| f.has(PassFlags::YIELD))
    }

    #[test]
    fn the_first_chord_over_command_palette_opens_the_notice_once() {
        // Another chord: Command Palette is never looked for, and nothing changes.
        with_ctrl(|c| c.pt.on(PtEvent::Found));
        let (plan, _, _) = post(1, 1_000, 1, fresh());
        assert_eq!(plan, OpenPlan::TryOpen);
        assert_eq!(with_ctrl(|c| c.pt.take_notice()).unwrap(), None);
        pt_ctrl(PtState::Detected);
        // Detected: the hook still swallows the chord, so the dock sees the first one.
        let mut hook = FakeHook::default();
        foreground_flow(&mut hook, &fresh());
        assert!(!yields(&hook));
        let (plan, _, _) = post(2, 1_000, 2, fresh());
        assert_eq!(plan, OpenPlan::TryOpen);
        assert_eq!(pt_state(), PtState::Yield { unanswered: 0 });
        // That open shows the notice first, selected on the empty field, with what Enter does.
        let l = new_launcher();
        let first = l.session.rows().first().unwrap();
        assert_eq!(&*first.title, strings::pt_shared());
        assert_eq!(first.detail.as_deref(), Some(strings::pt_turn_off()));
        assert_eq!(l.session.selected().map(|i| i.key), Some(first.key));
        assert_eq!(hint_for(first), Some(strings::hint_pt_take()));
        // Once.
        assert!(new_launcher().session.rows().is_empty());
        // From now on the hook passes the chord, but never while the launcher is open (its chord closes it).
        foreground_flow(&mut hook, &FakeSys { is_open: true, ..fresh() });
        assert!(!yields(&hook));
        foreground_flow(&mut hook, &fresh());
        assert!(yields(&hook));
    }

    #[test]
    fn passed_chords_are_watched_and_two_unanswered_take_the_chord() {
        use crate::msg::TIMER_LAUNCHER_PT;
        pt_ctrl(PtState::Yield { unanswered: 0 });
        let mut hook = FakeHook::default();
        let mut sys = FakeSys { owners: true, ..fresh() };
        // A pass that Command Palette answers by coming to the front; the process list was looked at just now.
        with_ctrl(|c| c.pt_scanned_ms = 1);
        pt_passed(&mut hook, &mut sys);
        assert_eq!(sys.ops, [Op::Arm(TIMER_LAUNCHER_PT, powertoys::ANSWER_MS)]);
        assert_eq!(sys.scans, 0, "a press soon after a look costs no scan");
        assert!(with_ctrl(|c| c.pt.watching()).unwrap());
        pt_foreground(0x5005, &FakeSys { owner_fg: true, ..fresh() });
        assert_eq!(pt_state(), PtState::Yield { unanswered: 0 });
        pt_timer(&mut hook, &mut sys);
        assert_eq!(pt_state(), PtState::Yield { unanswered: 0 }, "the window ends after the answer: nothing changes");
        // Another program's window is no answer.
        pt_passed(&mut hook, &mut sys);
        pt_foreground(0x5005, &sys);
        pt_timer(&mut hook, &mut sys);
        assert_eq!(pt_state(), PtState::Yield { unanswered: 1 });
        // Command Palette already in front when the chord passed (the chord closes it): answered.
        pt_passed(&mut hook, &mut FakeSys { owners: true, owner_fg: true, ..fresh() });
        assert_eq!(pt_state(), PtState::Yield { unanswered: 0 });
        assert!(!with_ctrl(|c| c.pt.watching()).unwrap());
        // Two unanswered in a row: the launcher takes the chord and says so at its next open.
        for _ in 0..powertoys::MAX_UNANSWERED {
            pt_passed(&mut hook, &mut sys);
            pt_timer(&mut hook, &mut sys);
        }
        assert_eq!(pt_state(), PtState::Take);
        assert!(!yields(&hook));
        let l = new_launcher();
        assert_eq!(l.session.rows().first().map(|r| &*r.title), Some(strings::pt_took_over()));
        assert_eq!(l.session.rows().first().and_then(|r| r.detail.as_deref()), Some(strings::pt_took_over_detail()));
        assert_eq!(l.session.selected(), None, "a notice without a choice is never the default");
        // It exits: the chord is the launcher's.
        pt_check(&mut hook, &mut FakeSys { owners: false, ..fresh() });
        assert_eq!(pt_state(), PtState::Absent);
        assert!(!yields(&hook));
        // After a close: with nothing found, a look at most every PT_RESCAN_MS; with Command Palette running, only the
        // hook's flags (its exit is waited on).
        let mut sys = FakeSys { now: 20_000, ..fresh() };
        pt_check(&mut hook, &mut sys);
        assert_eq!(sys.scans, 1);
        sys.now += 1_000;
        pt_closed(&mut hook, &mut sys);
        assert_eq!(sys.scans, 1, "too soon");
        sys.now += PT_RESCAN_MS as u32;
        sys.owners = true;
        pt_closed(&mut hook, &mut sys);
        assert_eq!((sys.scans, pt_state()), (2, PtState::Yield { unanswered: 0 }), "answered before: no notice");
        let passes = hook.passes.len();
        pt_closed(&mut hook, &mut sys);
        assert_eq!(sys.scans, 2);
        assert!(hook.passes.len() > passes && yields(&hook), "the hook passes the chord again after the close");
        // Off the Alt+Space chord no check runs.
        with_ctrl(|c| c.pt_on = false);
        pt_check(&mut hook, &mut sys);
        pt_closed(&mut hook, &mut sys);
        assert_eq!(sys.scans, 2);
    }

    #[test]
    fn the_settings_page_looks_for_command_palette() {
        // Command Palette started after the 60 s check: its hook sees Alt+Space first, so no open or close looks again.
        with_ctrl(|c| *c = Ctrl::new());
        pt_ctrl(PtState::Absent);
        with_ctrl(|c| c.pt_scanned_ms = 60_000);
        let mut hook = FakeHook::default();
        let mut sys = FakeSys { now: 100_000, owners: true, ..fresh() };
        pt_page_shown(&mut hook, &mut sys);
        assert_eq!((sys.scans, pt_state()), (1, PtState::Detected), "Settings > Launcher shows its row");
        // A running one is waited on: showing the page again costs no scan.
        pt_page_shown(&mut hook, &mut sys);
        assert_eq!(sys.scans, 1);
        // None running: a look at most every PT_RESCAN_MS.
        sys.owners = false;
        pt_check(&mut hook, &mut sys);
        assert_eq!((sys.scans, pt_state()), (2, PtState::Absent));
        sys.now += 1_000;
        pt_page_shown(&mut hook, &mut sys);
        assert_eq!(sys.scans, 2, "too soon");
        sys.now += PT_RESCAN_MS as u32;
        pt_page_shown(&mut hook, &mut sys);
        assert_eq!(sys.scans, 3);
        // Another chord: nothing of PowerToys is looked for.
        with_ctrl(|c| c.pt_on = false);
        sys.now += PT_RESCAN_MS as u32;
        pt_page_shown(&mut hook, &mut sys);
        assert_eq!(sys.scans, 3);
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn the_notice_answers_are_posted_to_the_dock_window() {
        let fx = crate::launcher::providers::tests::Fx::new();
        let mut l = typed(&fx, "");
        let mut sys = EffectSys::default();
        l.run(vec![Effect::Choose(Choice::TakeChord)], &fx.cx(), &mut sys);
        l.run(vec![Effect::DismissNotice(pt_notice_key())], &fx.cx(), &mut sys);
        l.run(vec![Effect::DismissNotice(ItemKey::of(ProviderId::Apps, "another"))], &fx.cx(), &mut sys);
        assert_eq!(ops(&sys), ["PtAnswer(1)", "PtAnswer(4)"]);
        for a in [Some(Choice::TakeChord), Some(Choice::LeaveChord), Some(Choice::UseAltShiftSpace), None] {
            assert_eq!(answer_of(answer_code(a)), Some(a));
        }
        assert_eq!(answer_of(0), None, "0 is an exit");
        assert_eq!(answer_of(5), None);
    }

    #[test]
    fn alt_shift_space_from_the_notice_waits_for_the_probe() {
        let no_probe = || -> bool { panic!("the probe runs for Alt+Shift+Space only") };
        for a in [Some(Choice::TakeChord), Some(Choice::LeaveChord), None] {
            assert!(answer_allowed(a, &no_probe), "{a:?}");
        }
        assert!(answer_allowed(Some(Choice::UseAltShiftSpace), &|| false));
        assert!(!answer_allowed(Some(Choice::UseAltShiftSpace), &|| true), "held: nothing is written");
        // An open launcher says so at once, under the notices it already shows, over the same query.
        let fx = crate::launcher::providers::tests::Fx::new();
        let mut l = typed(&fx, "note");
        l.session.set_notices(vec![failure_row()]);
        let _ = l.session.refresh(&fx.cx());
        assert_eq!(l.show_notices(vec![chord_held_row()], &fx.cx(), &mut EffectSys::default()), Reply::Relayout);
        let titles: Vec<&str> = l.session.rows().iter().take(2).map(|r| &*r.title).collect();
        assert_eq!(titles, [strings::launch_failed(), strings::pt_alt_shift_space_held()]);
        assert!(l.session.rows().iter().any(|r| r.kind != Kind::Notice), "the query's rows stay");
        assert_eq!(
            l.session.selected().map(|r| r.kind),
            Some(Kind::App),
            "a notice without an action is not the default"
        );
        // No launcher open: the next open shows it, once.
        with_ctrl(|c| c.chord_held = true);
        let l = new_launcher();
        assert_eq!(l.session.rows().first().map(|r| &*r.title), Some(strings::pt_alt_shift_space_held()));
        assert!(new_launcher().session.rows().iter().all(|r| r.kind != Kind::Notice), "once");
        // The question is still open: the notice (which the choice took out of the list) shows again under that row,
        // with its choices, in the open launcher...
        let notice_titles = |l: &Launcher| -> Vec<String> {
            l.session.rows().iter().filter(|r| r.kind == Kind::Notice).map(|r| r.title.to_string()).collect()
        };
        let both = [strings::pt_alt_shift_space_held(), strings::pt_shared()];
        pt_ctrl(PtState::Detected);
        with_ctrl(|c| c.pt.on(PtEvent::FirstChord));
        let mut l = new_launcher();
        l.session.set_notices(Vec::new());
        let mut sys = EffectSys::default();
        held_flow(&mut |rows| {
            let _ = l.show_notices(rows, &fx.cx(), &mut sys);
            true
        });
        assert_eq!(notice_titles(&l), both);
        let shared = l.session.rows().iter().find(|r| &*r.title == strings::pt_shared()).unwrap();
        assert!(matches!(shared.payload, Payload::Choices(c) if c.len() == 3), "Take and Leave are there again");
        assert!(new_launcher().session.rows().iter().all(|r| r.kind != Kind::Notice), "shown, not once more");
        // ... or at the next open, once.
        held_flow(&mut |_| false);
        assert_eq!(notice_titles(&new_launcher()), both);
        assert!(new_launcher().session.rows().iter().all(|r| r.kind != Kind::Notice), "once");
        // With no question left (the owner exited) only the held chord's row shows.
        pt_check(&mut FakeHook::default(), &mut FakeSys { owners: false, ..fresh() });
        held_flow(&mut |_| false);
        assert_eq!(notice_titles(&new_launcher()), [strings::pt_alt_shift_space_held()]);
        with_ctrl(|c| *c = Ctrl::new());
    }

    // ---- L3 T2: the warm timer lists once

    #[test]
    fn warm_timer_never_lists_a_listed_catalogue() {
        let mut c = Ctrl::new();
        assert!(c.warm_due(), "nothing listed yet");
        c.listed_at_ms = 8_046; // the first open's listing came back (on_catalog)
        assert!(!c.warm_due(), "B7: the warm timer at 10 s lists nothing");
    }

    // ---- L3 T22: the warm timer

    #[test]
    fn warm_content_takes_no_notice_and_no_query() {
        with_ctrl(|c| {
            c.opts.keep_query = true;
            c.last_query = Some("calc".into());
            c.notice = Some(LaunchKind::App);
            c.chord_held = true;
        });
        let mut s = Surface::new(Metrics { dpi: 120 }).unwrap();
        assert!(warm_on(&mut s, &mut FakeSys::default()), "laid out and painted");
        assert_ne!(s.size(), (0, 0));
        with_ctrl(|c| {
            assert_eq!(c.last_query.as_deref(), Some("calc"), "the kept query waits for the next open");
            assert_eq!(c.notice, Some(LaunchKind::App), "the refused launch's notice waits for the next open");
            assert!(c.chord_held);
            assert_eq!(c.icon_px, style::ICON_M as u32, "the cache's icon size is left to the opens");
        })
        .unwrap();
        // The next open shows them.
        let l = new_launcher();
        assert_eq!(l.session.query_text(), "calc");
        assert!(l.session.rows().iter().any(|r| &*r.title == strings::launch_failed()));
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn warm_asks_nothing() {
        // A catalogue is there: an empty field still shows no row, so no icon is asked for.
        with_ctrl(|c| {
            c.apps = Arc::new(crate::launcher::arena::test_apps(&[(
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
                "Calcolatrice",
                &[],
                1,
            )]));
        });
        let mut s = Surface::new(Metrics { dpi: 96 }).unwrap();
        let mut sys = FakeSys::default();
        assert!(warm_on(&mut s, &mut sys));
        assert!(sys.shells.is_empty(), "no shell request, no icon request: {:?}", sys.shells);
        assert!(sys.ops.is_empty(), "no timer, no open: {:?}", sys.ops);
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn warm_skipped_when_a_popup_is_open_or_after_an_open() {
        assert!(warm_window_due(false, false), "after a start: no popup open, never opened");
        assert!(!warm_window_due(true, false), "a popup is open");
        assert!(!warm_window_due(false, true), "an open already paged the code in");
        assert!(!warm_window_due(true, true));
        // An open marks the process for good: a chord switched off and on again does not warm again.
        assert!(!opened_in_process());
        note_open();
        assert!(opened_in_process());
        with_ctrl(|c| *c = Ctrl::new());
        assert!(opened_in_process(), "the mark is the process's, not the controller's");
    }

    #[test]
    fn warm_ensures_settings_only_with_the_provider_on() {
        let on = LauncherOpts::default();
        assert!(warm_settings_due(&on));
        let off = LauncherOpts { mask: on.mask.with(ProviderId::Settings, false), ..LauncherOpts::default() };
        assert!(!warm_settings_due(&off), "no settings child after every start for nothing");
    }

    #[test]
    fn the_notice_answers_write_values_the_config_keeps() {
        let answers = [Some(Choice::TakeChord), Some(Choice::LeaveChord), Some(Choice::UseAltShiftSpace), None];
        for start in ["yield", "take"] {
            for a in answers {
                let mut l =
                    LauncherConfig { chord: "alt+space".into(), with_powertoys: start.into(), ..Default::default() };
                apply_answer(&mut l, a);
                let want = match a {
                    Some(Choice::TakeChord) => ("alt+space", "take"),
                    Some(Choice::LeaveChord) => ("alt+space", "yield"),
                    Some(Choice::UseAltShiftSpace) => ("alt+shift+space", start),
                    None => ("alt+space", start),
                };
                assert!(l.sanitize().is_empty(), "{a:?} from {start}: a value the config would fix");
                assert_eq!((l.chord.as_str(), l.with_powertoys.as_str()), want, "{a:?} from {start}");
                assert!(l.powertoys_done, "{a:?} from {start}: answered, so never asked again");
            }
        }
    }
    // ---- aliases (plan L2-A2)

    const NOTEPAD_ID: &str = "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App";

    /// The fixture with invented aliases; "np" spells the catalogue's id in lower case.
    fn alias_fx() -> crate::launcher::providers::tests::Fx {
        let mut fx = crate::launcher::providers::tests::Fx::new();
        let map = [
            ("np", "app:microsoft.windowsnotepad_8wekyb3d8bbwe!app"),
            ("gone", "app:Example.Missing_0000000000000!App"),
            ("gh", "https://example.com/gh"),
            ("win", "C:\\Windows"),
            ("disp", "ms-settings:display"),
            ("pad", "notepad.exe"),
            ("ip", "ipconfig /all"),
        ]
        .iter()
        .map(|(w, t)| (w.to_string(), t.to_string()))
        .collect();
        fx.opts.aliases = AliasTable::build(&map, &fx.opts.prefixes).0;
        fx
    }

    #[test]
    fn missing_app_alias_asks_no_icon() {
        let fx = alias_fx();
        let show = |l: &mut Launcher, sys: &mut EffectSys| {
            let mut s = surface(96);
            l.size(&s);
            l.paint_on(&mut s, None, None, sys);
        };
        let asks = |sys: &EffectSys| -> Vec<String> {
            sys.ops.iter().filter(|o| o.starts_with("Shell(StartIconsFor")).cloned().collect()
        };
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "gone");
        show(&mut l, &mut sys);
        assert!(asks(&sys).is_empty(), "{:?}", sys.ops);
        // A found app's request carries the catalogue's own id string, never the config's spelling.
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "np");
        show(&mut l, &mut sys);
        let want = format!("Shell({:?})", ShellRequest::StartIconsFor { icon_px: 24, ids: vec![NOTEPAD_ID.into()] });
        assert_eq!(asks(&sys), [want], "{:?}", sys.ops);
    }

    // ---- file rows (plan L2-F4)

    fn file_batch(l: &mut Launcher, fx: &crate::launcher::providers::tests::Fx) {
        use crate::launcher::files::{items, FileHit};
        let hit = |name: &str, dir: bool| FileHit {
            path: format!("C:\\x\\{name}").into(),
            name: name.into(),
            folder: "x".into(),
            dir,
        };
        let hits =
            vec![hit("note-a.txt", false), hit("note-b.png", false), hit("note-c.exe", false), hit("notes", true)];
        let rows = items(hits, &["note"], 20);
        let batch = Batch { gen: l.session.gen(), from: ProviderId::Files, items: rows, done: true };
        assert!(l.session.late(batch, &fx.cx()));
    }

    #[test]
    fn view_file_rows_ask_no_icons() {
        let fx = crate::launcher::providers::tests::Fx::new();
        let mut l = typed(&fx, "file note");
        file_batch(&mut l, &fx);
        let files = l.session.rows().iter().filter(|r| matches!(r.kind, Kind::File | Kind::Folder)).count();
        assert_eq!(files, 4);
        let mut sys = EffectSys::default();
        let mut s = surface(96);
        l.size(&s);
        l.paint_on(&mut s, None, None, &mut sys);
        let asks: Vec<&String> = sys.ops.iter().filter(|o| o.starts_with("Shell(StartIconsFor")).collect();
        assert!(asks.is_empty(), "{:?}", sys.ops);
        // The same rows in Mixed mode, next to the apps: the apps' ids go, no file path does.
        let mut l = typed(&fx, "note");
        file_batch(&mut l, &fx);
        let mut sys = EffectSys::default();
        l.size(&s);
        l.paint_on(&mut s, None, None, &mut sys);
        assert!(sys.ops.iter().all(|o| !o.contains("C:\\\\x")), "{:?}", sys.ops);
    }

    #[test]
    fn busy_mark_only_after_slow() {
        use crate::launcher::files::FilesEvent;
        let fx = crate::launcher::providers::tests::Fx::new();
        let mut l = typed(&fx, "note");
        assert!(!l.session.files_busy());
        let gen = l.session.gen();
        // Another generation's Slow is not the open query's.
        let _ = l.session.files_event(FilesEvent::Slow(gen.wrapping_sub(1)), &fx.cx());
        assert!(!l.session.files_busy());
        let _ = l.session.files_event(FilesEvent::Slow(gen), &fx.cx());
        assert!(l.session.files_busy());
        // The mark is drawn: the field differs from the same field without it.
        let render = |l: &mut Launcher| {
            let mut s = surface(96);
            l.size(&s);
            l.paint_on(&mut s, None, None, &mut EffectSys::default());
            s.snapshot_pixels().expect("pixels").2
        };
        let with = render(&mut l);
        let mut calm = typed(&fx, "note");
        assert_ne!(with, render(&mut calm));
        // The next keystroke ends it.
        calm.session.chars("x", &fx.cx());
        assert!(!calm.session.files_busy());
    }

    #[test]
    fn executable_row_hints_show_in_folder() {
        let fx = crate::launcher::providers::tests::Fx::new();
        let mut l = typed(&fx, "file note");
        file_batch(&mut l, &fx);
        let hint = |name: &str| {
            let item = l.session.rows().iter().find(|r| r.title.as_ref() == name).expect(name);
            hint_for(item)
        };
        assert_eq!(hint("note-a.txt"), Some(strings::hint_open()));
        assert_eq!(hint("notes"), Some(strings::hint_open()));
        assert_eq!(hint("note-c.exe"), Some(strings::hint_reveal()));
    }

    #[test]
    fn alias_launches_never_elevate() {
        use crate::launcher::session::Effect;
        let fx = alias_fx();
        let no_setting = |_: u32| None;
        for (word, want) in [
            ("np", "LauncherLaunch"),
            ("gh", "OpenUrl"),
            ("win", "OpenPath"),
            ("disp", "OpenPath"),
            ("pad", "LaunchCommand"),
            ("ip", "LaunchCommand"),
        ] {
            let mut l = typed(&fx, word);
            assert_eq!(key(&mut l, &fx, &mut EffectSys::default(), VK_RETURN, Mods::NONE), Reply::Close, "{word}");
            let [Effect::LaunchAlias(r)] = l.queued.as_slice() else { panic!("{word}: {:?}", l.queued) };
            assert!(!r.elevates(), "{word}");
            let req = crate::launcher_sys::to_request(r.clone(), &no_setting).unwrap();
            assert!(format!("{req:?}").starts_with(want), "{word}: {req:?}");
        }
        let mut l = typed(&fx, "ip");
        key(&mut l, &fx, &mut EffectSys::default(), VK_RETURN, Mods::NONE);
        let [Effect::LaunchAlias(r)] = l.queued.as_slice() else { panic!() };
        let req = crate::launcher_sys::to_request(r.clone(), &no_setting).unwrap();
        assert!(matches!(req, ShellRequest::LaunchCommand { console: true, .. }), "{req:?}");
    }

    #[test]
    fn alias_launches_skip_the_run_history() {
        let fx = alias_fx();
        with_ctrl(|c| {
            c.history.clear();
            c.dirty = false;
        });
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "pad");
        key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert!(sys.ops.iter().any(|o| o.starts_with("Shell(LaunchCommand")), "{:?}", sys.ops);
        assert_eq!(with_ctrl(|c| (c.history.len(), c.dirty, c.frecency.is_empty())), Some((0, false, true)));
        // The same line typed in the run mode is recorded.
        let mut l = typed(&fx, "> notepad.exe");
        key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(with_ctrl(|c| c.history.len()), Some(1));
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn save_alias_writes_nothing_in_the_key_handler() {
        let fx = alias_fx();
        let mut l = opened(9);
        let mut sys = EffectSys::default();
        let out = l.session.alias_prompt(&fx.cx());
        l.run(out, &fx.cx(), &mut sys);
        l.session.chars("docs https://example.com/docs", &fx.cx());
        let reply = key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        assert_ne!(reply, Reply::Close, "the launcher stays");
        let save = AliasOp::Save { word: "docs".into(), target: "https://example.com/docs".into() };
        assert_eq!(ops(&sys), [format!("Alias({save:?})")], "only the post to the dock window");
        assert!(l.queued.is_empty());
        assert_eq!(l.session.query_text(), "docs");
        // Remove alias, the same way.
        let mut l = typed(&fx, "gh");
        let mut sys = EffectSys::default();
        key(&mut l, &fx, &mut sys, 0x4B, Mods::CTRL);
        key(&mut l, &fx, &mut sys, 0x23, Mods::NONE);
        key(&mut l, &fx, &mut sys, VK_RETURN, Mods::NONE);
        assert_eq!(ops(&sys), [format!("Alias({:?})", AliasOp::Remove("gh".into()))]);
    }

    #[derive(Default)]
    struct FakeCfg {
        pending: bool,
        reloads: bool,
        fails: bool,
        calls: Vec<String>,
    }

    impl AliasCfg for FakeCfg {
        fn pending(&self) -> bool {
            self.pending
        }
        fn poll(&mut self) -> bool {
            self.calls.push("poll".into());
            self.reloads
        }
        fn follow_up(&mut self) {
            self.calls.push("follow_up".into());
        }
        fn modify(&mut self, op: &AliasOp) -> Result<(), String> {
            self.calls.push(format!("modify {op:?}"));
            if self.fails {
                Err("no writable data folder".into())
            } else {
                Ok(())
            }
        }
        fn applied(&mut self) {
            self.calls.push("applied".into());
        }
        fn requery(&mut self, failed: bool) {
            self.calls.push(format!("requery {failed}"));
        }
    }

    #[test]
    fn alias_save_loads_a_pending_hand_edit_first() {
        let op = AliasOp::Save { word: "gh".into(), target: "https://example.com/".into() };
        let m = format!("modify {op:?}");
        let run = |cfg: FakeCfg| {
            let mut cfg = cfg;
            apply_alias_op(&op, &mut cfg);
            cfg.calls
        };
        let edit = run(FakeCfg { pending: true, reloads: true, ..FakeCfg::default() });
        assert_eq!(edit, ["poll", "follow_up", m.as_str(), "applied", "requery false"]);
        assert_eq!(run(FakeCfg::default()), [m.as_str(), "applied", "requery false"], "nothing pending");
        // An edit that does not parse is not loaded: no follow-up, and the write saves it aside first (config::modify).
        let broken = run(FakeCfg { pending: true, ..FakeCfg::default() });
        assert_eq!(broken, ["poll", m.as_str(), "applied", "requery false"]);
        assert_eq!(run(FakeCfg { fails: true, ..FakeCfg::default() }), [m.as_str(), "requery true"]);
    }

    #[test]
    fn saving_replaces_a_word_equal_once_folded() {
        let mut l = LauncherConfig::default();
        l.aliases.insert("GH".into(), "https://example.com/old".into());
        // Equal once folded too, but the table refuses its target: it shadows nothing and stays as written (D9).
        l.aliases.insert("Gh".into(), "steam://run/1".into());
        l.aliases.insert("game".into(), "steam://run/1".into());
        edit_aliases(&mut l, &AliasOp::Save { word: "gh".into(), target: "https://example.com/new".into() });
        let all: Vec<(&str, &str)> = l.aliases.iter().map(|(w, t)| (w.as_str(), t.as_str())).collect();
        let kept = [("Gh", "steam://run/1"), ("game", "steam://run/1"), ("gh", "https://example.com/new")];
        assert_eq!(all, kept, "the ignored entries stay");
        edit_aliases(&mut l, &AliasOp::Remove("nope".into()));
        assert_eq!(l.aliases.len(), 3);
        edit_aliases(&mut l, &AliasOp::Remove("gh".into()));
        assert_eq!(l.aliases.keys().map(String::as_str).collect::<Vec<_>>(), ["Gh", "game"]);
    }

    #[test]
    fn remove_alias_goes_through_the_alias_handler() {
        // The hash removal is an alias change like the launcher's own: a pending hand edit is loaded first.
        let hash = crate::launcher::aliases::word_hash("gh");
        let op = AliasOp::RemoveHash(hash);
        let m = format!("modify {op:?}");
        let mut cfg = FakeCfg { pending: true, reloads: true, ..FakeCfg::default() };
        apply_alias_op(&op, &mut cfg);
        assert_eq!(cfg.calls, ["poll", "follow_up", m.as_str(), "applied", "requery false"]);
        // On the config: only the entry with that hash goes, the ones the table ignores stay.
        let mut l = LauncherConfig::default();
        l.aliases.insert("gh".into(), "https://example.com/".into());
        l.aliases.insert("game".into(), "steam://run/1".into());
        edit_aliases(&mut l, &AliasOp::RemoveHash(crate::launcher::aliases::word_hash("nope")));
        assert_eq!(l.aliases.len(), 2);
        edit_aliases(&mut l, &op);
        assert_eq!(l.aliases.keys().map(String::as_str).collect::<Vec<_>>(), ["game"]);
    }

    #[test]
    fn prompt_from_settings_restores_the_settings_window() {
        let fx = alias_fx();
        let mut hook = FakeHook::default();
        let mut sys = fresh();
        assert!(!alias_prompt_flow(&mut hook, &mut sys), "never with the chord off");
        assert!(sys.ops.is_empty());
        ON.with(|o| o.set(true));
        let opened = alias_prompt_flow(&mut hook, &mut sys);
        ON.with(|o| o.set(false));
        assert!(opened);
        assert_eq!((sys.ops.as_slice(), hook.shown.as_slice()), ([Op::Open].as_slice(), [true].as_slice()));
        // The content the host makes for it: the window in front (Settings, 0x5005) is the one the close restores.
        let mut l = new_launcher();
        assert_eq!(l.fg_at_open, 0x5005);
        let mut esys = EffectSys::default();
        let out = l.session.alias_prompt(&fx.cx());
        l.run(out, &fx.cx(), &mut esys);
        assert_eq!(l.session.chip(), Some(ProviderId::Alias));
        assert_eq!(key(&mut l, &fx, &mut esys, 0x1B, Mods::NONE), Reply::Relayout);
        assert_eq!(key(&mut l, &fx, &mut esys, 0x1B, Mods::NONE), Reply::Close);
        host_close(&mut l, &mut hook, &mut esys);
        assert!(esys.ops.iter().any(|o| o == "Restore(0x5005)"), "{:?}", esys.ops);
        with_ctrl(|c| *c = Ctrl::new());
    }

    #[test]
    fn prompt_text_is_never_kept() {
        let fx = alias_fx();
        with_ctrl(|c| c.opts.keep_query = true);
        let mut sys = EffectSys::default();
        let mut l = typed(&fx, "calc");
        let out = l.session.alias_prompt(&fx.cx());
        l.run(out, &fx.cx(), &mut sys);
        l.session.chars("secret https://example.com/", &fx.cx());
        // A refused launch's reopen and keep_query both take the query from before the prompt.
        l.launched = true;
        l.housekeeping(&mut sys);
        assert_eq!(with_ctrl(|c| c.failed_query.clone()).flatten().as_deref(), Some("calc"));
        key(&mut l, &fx, &mut sys, 0xBC, Mods::CTRL);
        host_close(&mut l, &mut FakeHook::default(), &mut sys);
        assert_eq!(with_ctrl(|c| c.last_query.clone()).flatten().as_deref(), Some("calc"));
        with_ctrl(|c| *c = Ctrl::new());
    }
}
