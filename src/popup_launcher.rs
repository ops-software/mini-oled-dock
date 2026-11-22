//! The launcher popup (SPEC-launcher 5.2, 5.3 and 5.5): the content the popup host shows for `PopupKind::Launcher`.
//! L1a-13 added the stub; L1a-14 (this file) is the view: layout and painting of the field, the mode chip, the result rows,
//! the detail and review lines, the hint and the scrollbar, all from the `Session` the content holds. L1a-15 adds the
//! controller: the chord flow from the hook's post to an open launcher (or to nothing, with the type-ahead ended), which
//! reaches the hook and Windows through two small ports, `HookPort` and `SysPort` (real in launcher_sys.rs, recorders
//! in the tests).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
};

use super::{center_y, Content, HitMap, PopupKind, Reply};
use crate::config::LauncherConfig;
use crate::glyphs;
use crate::hook::{self, KeyRec, PassFlags, PostAction, RING_CAP};
use crate::icons::IconBitmap;
use crate::launcher::catalog::{self, CatApp, IconCache};
use crate::launcher::field::KeyEvent;
use crate::launcher::gesture::{self, Gesture};
use crate::launcher::history;
use crate::launcher::model::{
    Batch, CommandId, GlyphId, IconRef, Item, Kind, Mode, Mods, NumberStyle, Payload, ProviderId, ProviderMask,
    SettingRow, Verb, WindowRow,
};
use crate::launcher::providers::{self, Budget, Context, LauncherOpts};
use crate::launcher::query::{self, PrefixTable};
use crate::launcher::runline;
use crate::launcher::session::{Effect, ReplayKey, Session};
use crate::launcher::strings;
use crate::launcher::style;
use crate::launcher::verbs;
use crate::launcher::web::{self, Engine, Template};
use crate::launcher_sys::{RealHook, RealSys};
use crate::log;
use crate::render::Metrics;
use crate::shell_thread::{IconPlan, LaunchKind, ShellRequest, StartApp};
use crate::start_search::{self, AppIndex, Holder, Setting};
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

/// The panel keeps its height while keys come faster than this (`TIMER_LAUNCHER_SHRINK`, SPEC 5.3).
const SHRINK_MS: u32 = 120;
const VK_RETURN: u16 = 0x0D;
const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

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
    /// The window in front when the chord came: the restore and the window menu act on it.
    fg_at_open: isize,
    /// The close gives the foreground back to `fg_at_open` (a `Hide` with restore; never a close without one).
    restore: bool,
    /// The window a Switch activates, in `closing`: before the hide, while the dock still holds the foreground.
    switch: Option<isize>,
    /// Run-history lines forgotten by Shift+Delete, taken out of the controller's history after the key.
    forgotten: Vec<Box<str>>,
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
            fg_at_open: 0,
            restore: false,
            switch: None,
            forgotten: Vec::new(),
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
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::focused();
        let mut buf = [KeyRec::default(); RING_CAP];
        let n = hook.take(self.open_seq, &mut buf);
        let keys = replay_keys(buf.get(..n).unwrap_or(&[]), sys);
        if keys.is_empty() {
            return view;
        }
        let fx = self.session.replay(&keys, cx);
        view.merge(self.run(fx, cx, sys))
    }

    /// Runs the session's effects in order. The view's, the field's copy, Ctrl+W and the paste act at once; a Switch
    /// waits for `closing` (before the hide); a `Hide` closes, with or without the restore; launches, power commands,
    /// the window menu and the row's copy run after the close, in `finish`.
    fn run(&mut self, fx: Vec<Effect>, cx: &Context, sys: &mut dyn SysPort) -> Reply {
        let mut reply = Reply::None;
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
                Effect::Paste => {
                    if let Some(text) = sys.paste() {
                        let more = self.session.paste(&text, cx);
                        reply = reply.merge(self.run(more, cx, sys));
                    }
                }
                Effect::RequestIcons(ids) => {
                    let ids = ids.into_iter().map(String::from).collect();
                    sys.shell(ShellRequest::StartIconsFor { icon_px: self.icon_px, ids });
                }
                Effect::ForgetRun(line) => self.forgotten.push(line),
                // Nothing matched: Windows' typo rewrite is read on the shell thread (a 1.4 MB file) and comes back as a
                // late batch of this generation (`converted`).
                Effect::RequestRewrite { gen } => {
                    sys.shell(ShellRequest::LauncherConvert { gen, query: self.session.last_query() });
                }
                // Learned ranks (L1b) and files (L2).
                Effect::Forget(_) | Effect::RequestFiles(_) => {}
                e @ (Effect::Launch(_)
                | Effect::Copy(_)
                | Effect::Power(_)
                | Effect::WindowMenu
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

    /// The host is closing the popup, still shown: a Switch activates its window now.
    fn closing_on(&mut self, sys: &mut dyn SysPort) {
        if let Some(h) = self.switch.take() {
            sys.activate(h);
        }
    }

    /// `ShellEvent::LauncherConverted`: Windows read the query of `gen` as `meant`. Taken only while `gen` is the current
    /// generation and it still has no rows, so nothing can move under the selection: the rows of `meant` come in as one
    /// late batch of that generation (the web row stays the typed query's).
    pub(crate) fn converted(&mut self, gen: u64, meant: &str, cx: &Context) -> Reply {
        if gen != self.session.gen() || self.session.has_matches() {
            return Reply::None;
        }
        let q = query::parse_query(meant, &cx.opts.prefixes, cx.num, gen);
        let mut items = providers::query_all(&q, cx, &mut Budget::default()).into_sorted();
        items.retain(|i| i.from != ProviderId::Web);
        if items.is_empty() {
            return Reply::None;
        }
        let batch = Batch { gen, from: ProviderId::Apps, items, done: true };
        if self.session.late(batch, cx) {
            Reply::Relayout
        } else {
            Reply::None
        }
    }

    /// The forgotten run lines leave the controller's history (outside the context's borrow).
    fn forget_runs(&mut self) {
        if self.forgotten.is_empty() {
            return;
        }
        let lines = std::mem::take(&mut self.forgotten);
        with_ctrl(|c| {
            let before = c.history.len();
            c.history.retain(|h| !lines.contains(h));
            c.dirty |= c.history.len() != before;
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
        hook.shown(false);
        hook.end(self.open_seq);
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::closed();
        if self.restore && self.fg_at_open != 0 {
            sys.restore(self.fg_at_open);
        }
        let mut elevated = false;
        // A setting's target comes from the list the controller holds next to the rows.
        let held = with_ctrl(|c| c.setting_list.clone()).unwrap_or_default();
        let setting = |i: u32| {
            let s = held.get(usize::try_from(i).ok()?)?;
            Some((s.target.to_string(), s.args.clone()))
        };
        for e in std::mem::take(&mut self.queued) {
            match e {
                Effect::Launch(r) => {
                    let elevates = r.elevates();
                    elevated |= elevates;
                    if let Some(line) = run_line_of(&r) {
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

    /// After a close, with the popup hidden (the host's `after_close`): the icons the rows used stay in the cache, the
    /// 10-minute release of the settings is armed, a changed history is written 2 s later, and a catalogue older than
    /// 10 minutes is listed again.
    fn housekeeping(&mut self, sys: &mut dyn SysPort) {
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
        let failed_query = std::mem::take(&mut self.launched).then(|| self.session.last_query());
        let plan = with_ctrl(|c| {
            for id in &used {
                c.icons.touch(id);
            }
            c.hidden_at_ms = now;
            c.failed_query = failed_query;
            (c.dirty, c.catalogue_stale(now))
        });
        let (save, stale) = plan.unwrap_or_default();
        sys.arm(crate::msg::TIMER_LAUNCHER_RELEASE, RELEASE_MS);
        if save {
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
    pub(crate) fn set_icon(&mut self, key: &str, bmp: IconBitmap) {
        self.icons.insert(key.to_lowercase(), bmp);
    }

    /// A shrink waits (while the user types) or is let go (120 ms without a keystroke): growing is never held.
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

    fn click(&mut self, id: u32) -> Reply {
        let Some(row) = id.checked_sub(ID_ROW).and_then(|r| usize::try_from(r).ok()) else { return Reply::None };
        let mut sys = RealSys;
        let now = sys.now_ms();
        ctx(|cx| {
            let fx = self.session.click(row, now, cx);
            self.run(fx, cx, &mut sys)
        })
        .unwrap_or(Reply::None)
    }

    fn pointer(&mut self, x: i32, y: i32) -> Reply {
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
        l.fg_at_open = c.fg_at_open;
        l.set_shape(c.width, c.opts.rows);
        // The icons kept between opens: rows that have one never ask again.
        for (k, b) in c.icons.iter() {
            l.icons.insert(k.to_string(), b.clone());
        }
        l
    });
    let mut l = made.unwrap_or_else(|| Launcher::with_session(Session::open(&LauncherOpts::default(), None)));
    // The error of a launch refused while the launcher was away shows once, at this open.
    if with_ctrl(|c| c.notice.take()).flatten().is_some() {
        l.session.set_notices(vec![failure_row()]);
    }
    if !l.session.query_text().is_empty() {
        // The kept query's rows.
        let _ = ctx(|cx| l.session.refresh(cx));
    }
    l
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
}

/// One of the launcher's one-shot timers (`TIMER_LAUNCHER_*`) fired on the popup window: killed first.
pub(crate) fn on_timer(hwnd: HWND, id: usize) {
    // SAFETY: a timer of our popup window; killing an id that is not armed fails harmlessly.
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
                ask_catalogue();
            }
        }
        crate::msg::TIMER_LAUNCHER_SAVE => save_state(),
        crate::msg::TIMER_LAUNCHER_RELEASE => release_resident(),
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

/// A chord is set: the launcher's state exists.
pub(crate) fn is_on() -> bool {
    ON.with(Cell::get)
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

/// `ShellEvent::StartApps` (`popup::on_start_apps`, which took Windows' app index for this catalogue): the launcher keeps its
/// own folded copy, swapped in at the next keystroke. Nothing happens without a chord.
pub(crate) fn on_catalog(list: &[StartApp], index: Option<&AppIndex>) {
    if !is_on() {
        return;
    }
    let apps = catalog::ingest(list, index);
    let n = apps.len();
    let now = RealSys.now_ms();
    if with_ctrl(|c| {
        c.apps = apps;
        c.listed_at_ms = now.max(1);
        c.listing_since = None;
    })
    .is_some()
    {
        log!("launcher: catalogue of {n} apps");
    }
}

/// `ShellEvent::StartSettings`: the settings list is built; when the launcher holds it, its rows are folded now.
pub(crate) fn on_settings_ready() {
    if is_on() && start_search::holds_settings(Holder::Launcher) {
        fold_held_settings();
    }
}

/// Folds the held settings list into rows (once per load: names and keywords are folded here, not per keystroke).
fn fold_held_settings() {
    let list = start_search::settings();
    if list.is_empty() {
        return;
    }
    let rows = catalog::fold_settings(&list);
    with_ctrl(|c| {
        c.settings = rows;
        c.setting_list = list;
    });
}

/// `ShellEvent::LauncherIcons`: kept in the cache (at the size of the rows) and shown at once when the launcher is open.
pub(crate) fn on_icons(icon_px: u32, icons: Vec<(String, Option<IconBitmap>)>) {
    if !is_on() {
        return;
    }
    let mut ids = Vec::new();
    let px = with_ctrl(|c| c.icon_px).unwrap_or(icon_px);
    for (id, bmp) in icons {
        let Some(b) = bmp else { continue };
        let b = if icon_px != px && px != 0 { crate::icons::resize_area(&b, px) } else { b };
        if !b.is_empty() {
            with_ctrl(|c| c.icons.insert(&id, b));
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return;
    }
    super::with_launcher(|l| {
        with_ctrl(|c| {
            for id in &ids {
                if let Some(b) = c.icons.get(&id.to_lowercase()) {
                    l.set_icon(id, b.clone());
                }
            }
        });
        Reply::Redraw
    });
}

/// `ShellEvent::LauncherState`: the run history of the shell user (the learned ranks come in L1b). Lines added since the
/// open stay first.
pub(crate) fn on_state(run: Option<Vec<u8>>) {
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
    });
}

/// `TIMER_LAUNCHER_SAVE`: a changed history is written (the shell thread writes it for the shell user).
fn save_state() {
    let req = with_ctrl(|c| {
        if !std::mem::take(&mut c.dirty) || !c.learn {
            return None;
        }
        Some(ShellRequest::LauncherStateSave {
            frecency: None,
            run: Some(history::render_run(&c.history)),
            delete: false,
        })
    })
    .flatten();
    if let Some(r) = req {
        crate::shell_thread::request(r);
    }
}

/// `TIMER_LAUNCHER_RELEASE`: 10 minutes after the last close the settings rows, their list and the window rows are freed
/// (the catalogue and its icons stay: they are what the 0.42 MiB resident set is).
fn release_resident() {
    if super::current() == Some(PopupKind::Launcher) {
        return;
    }
    let _ = with_ctrl(|c| {
        c.settings = Vec::new();
        c.setting_list = Arc::default();
        c.windows = Vec::new();
        c.failed_query = None;
    });
    start_search::release_settings_for(Holder::Launcher);
    crate::mem::released_data();
    log!("launcher: settings released");
}

/// What an open needs before its window shows (`SysPort::prepare_open`): the settings held (and folded when they are
/// already loaded), the windows listed, the history asked for the first time, the catalogue listed when it has not been.
pub(crate) fn prepare_open() {
    if let Some(h) = super::last_hwnd() {
        // SAFETY: a timer of our popup window; killing an id that is not armed fails harmlessly.
        let _ = unsafe { KillTimer(Some(h), crate::msg::TIMER_LAUNCHER_RELEASE) };
    }
    start_search::hold_settings(Holder::Launcher);
    if with_ctrl(|c| c.settings.is_empty()) == Some(true) {
        fold_held_settings();
    }
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
    /// `GetForegroundWindow`.
    fn foreground(&self) -> isize;
    /// A Switch: the bar's activation of one window (`apps::activate_window`).
    fn activate(&mut self, h: isize);
    /// The foreground back to `h` (`ui::force_foreground`), when it is still visible and not cloaked.
    fn restore(&mut self, h: isize);
    /// The text to the clipboard (`CF_UNICODETEXT`).
    fn copy(&mut self, text: &str);
    /// The clipboard's text, at most 64 KiB of it.
    fn paste(&mut self) -> Option<String>;
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
    /// What an open needs before the window shows: the settings held, the open windows listed, the history asked for.
    fn prepare_open(&mut self) {}
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
    /// Closes the launcher without the session's Hide (no restore).
    fn close_quiet(&mut self);
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

/// The session's options from `config.launcher` (pure). A provider the map does not name is on, and apps are always
/// on; the web template is the engine's, or the custom one when it passes the validator (Google otherwise).
pub(crate) fn opts_from_config(c: &LauncherConfig) -> LauncherOpts {
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
    /// What the providers' context borrows (the catalogue, the settings, the windows and the run history).
    apps: Vec<CatApp>,
    settings: Vec<SettingRow>,
    windows: Vec<WindowRow>,
    history: Vec<Box<str>>,
    /// The settings list the rows were folded from (the launch targets), and the icons kept between opens.
    setting_list: Arc<Vec<Setting>>,
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
    dirty: bool,
    /// The last close that launched something: when it hid, and the query as typed (kept for the reopen of a refused launch
    /// only, at most until the next open or the release).
    hidden_at_ms: u64,
    failed_query: Option<String>,
    /// A refused launch that could not reopen the launcher: its notice row shows at the next open.
    notice: Option<LaunchKind>,
}

/// A catalogue older than this is listed again after a close.
const CATALOGUE_STALE_MS: u64 = 600_000;
/// A listing nobody answered is asked again after this.
const LISTING_RETRY_MS: u64 = 60_000;
/// The settings rows are freed this long after a close (`TIMER_LAUNCHER_RELEASE`).
const RELEASE_MS: u32 = 600_000;
/// A changed history is written this long after the close (`TIMER_LAUNCHER_SAVE`).
const SAVE_MS: u32 = 2_000;
/// The first listing, this long after the dock starts or the chord is first set (`TIMER_LAUNCHER_WARM`).
const WARM_MS: u32 = 10_000;

/// The catalogue's listing: no icons (the rows ask for theirs), and what Start was sent is left alone.
fn list_request() -> ShellRequest {
    ShellRequest::ListStartApps { icon_px: style::ICON_M as u32, icons: IconPlan::None }
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
    /// The catalogue was listed more than 10 minutes ago and no listing is waiting for its answer.
    fn catalogue_stale(&self, now: u64) -> bool {
        let waiting = self.listing_since.is_some_and(|t| now.saturating_sub(t) < LISTING_RETRY_MS);
        self.listed_at_ms != 0 && now.saturating_sub(self.listed_at_ms) > CATALOGUE_STALE_MS && !waiting
    }

    fn new() -> Ctrl {
        Ctrl {
            open_seq: 0,
            fg_at_open: 0,
            pick: MonitorPick::Pointer,
            top_percent: crate::config::LAUNCHER_TOP_PERCENT.2,
            over_fullscreen_pass: true,
            flags: PassFlags::default(),
            last_query: None,
            width: style::LAUNCHER_W,
            opts: LauncherOpts::default(),
            num: NumberStyle { decimal: '.', group: ',' },
            apps: Vec::new(),
            settings: Vec::new(),
            windows: Vec::new(),
            history: Vec::new(),
            setting_list: Arc::default(),
            icons: IconCache::default(),
            icon_px: style::ICON_M as u32,
            listed_at_ms: 0,
            listing_since: None,
            learn: true,
            state_loaded: false,
            dirty: false,
            hidden_at_ms: 0,
            failed_query: None,
            notice: None,
        }
    }

    /// `config.launcher` taken over (the chord is set): the next open uses it.
    fn configure(&mut self, c: &LauncherConfig) {
        self.opts = opts_from_config(c);
        if !self.opts.keep_query {
            self.last_query = None;
        }
        self.pick = MonitorPick::from_name(&c.monitor);
        self.top_percent = c.top_percent;
        self.over_fullscreen_pass = c.over_fullscreen != "open";
        self.width = i32::from(c.width);
        if self.learn != c.learn {
            // Off: what was learned is forgotten and the files are deleted at the next open; on: they are read again.
            self.learn = c.learn;
            self.history.clear();
            self.dirty = false;
            self.state_loaded = false;
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
    /// A chord is set: the foreground, session and power events concern the launcher.
    static ON: Cell<bool> = const { Cell::new(false) };
    /// The dock window receives the session notifications (registered once, at the first chord).
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
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
            with_ctrl(|c| c.last_query = None);
            log!("launcher: chord off");
        }
        (_, true) => {
            with_ctrl(|c| c.configure(&cfg.launcher));
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
    foreground_flow(&mut RealHook, &RealSys);
}

/// `WM_DOCK_LAUNCHER_YIELDED`: the chord pressed at `kb_time` passed under YIELD (another launcher answers it). In
/// L1a only the probe watches it; the answer check comes with PowerToys detection (L1b-5).
#[cfg_attr(not(feature = "owner-probe"), allow(unused_variables))]
pub(crate) fn on_yielded(kb_time: u32) {
    let on = ON.with(Cell::get);
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::yielded(on.then_some(kb_time));
}

/// The pass flags of the window now in front, to the hook (a chord's FULLSCREEN is recomputed here).
fn foreground_flow(hook: &mut dyn HookPort, sys: &dyn SysPort) {
    let over_pass = with_ctrl(|c| c.over_fullscreen_pass).unwrap_or(true);
    let d3d = sys.d3d_fullscreen();
    let fs = !d3d && over_pass && sys.fullscreen_foreground();
    // REMOTE and YIELD come with their detection (L1b-6, L1b-5); the owner probe can force YIELD (S-PT).
    #[cfg(feature = "owner-probe")]
    let yield_ = crate::launcher_probe::force_yield();
    #[cfg(not(feature = "owner-probe"))]
    let yield_ = false;
    let flags = pass_flags(d3d, fs, over_pass, false, yield_);
    with_ctrl(|c| c.flags = flags);
    hook.pass(flags);
}

/// `WM_WTSSESSION_CHANGE`: a lock, a console or a remote disconnect ends the session (`session_end`).
pub(crate) fn on_session_change(code: u32) {
    if ON.with(Cell::get) && matches!(code, WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) {
        session_end(&mut RealHook, &mut RealSys);
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

/// `ShellEvent::LauncherConverted` (the reply to `LauncherConvert`).
pub(crate) fn on_converted(gen: u64, meant: Option<String>) {
    let Some(meant) = meant else { return };
    if ON.with(Cell::get) {
        super::with_launcher(|l| ctx(|cx| l.converted(gen, &meant, cx)).unwrap_or(Reply::None));
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
            with_ctrl(|c| {
                c.open_seq = seq;
                c.fg_at_open = fg;
                c.num = num;
            });
            sys.prepare_open();
            hook.shown(true);
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::opened(sent_time);
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
        fn foreground(&self) -> isize {
            0x5005
        }
        fn activate(&mut self, _h: isize) {}
        fn restore(&mut self, _h: isize) {}
        fn copy(&mut self, _text: &str) {}
        fn paste(&mut self) -> Option<String> {
            None
        }
        fn close_window(&mut self, _h: isize) {}
        fn window_menu(&mut self, _h: isize) {}
        fn shell(&mut self, _r: ShellRequest) {}
        fn power(&mut self, _id: CommandId) {}
        fn open_settings(&mut self) {}
        fn arm(&mut self, _id: usize, _ms: u32) {}
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

    // ---- 15c: effects and the system port

    #[derive(Default)]
    struct EffectSys {
        clip: Option<String>,
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
        fn launcher_open(&self) -> bool {
            true
        }
        fn open(&mut self, _pick: MonitorPick, _top_percent: u8) -> bool {
            true
        }
        fn close_launcher(&mut self) {}
        fn inject(&mut self, _keys: &[(u16, bool)]) {}
        fn translate(&mut self, _rec: &KeyRec) -> Typed {
            Typed::Key
        }
        fn foreground(&self) -> isize {
            0x7777
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
        fn paste(&mut self) -> Option<String> {
            self.ops.push("Paste".into());
            self.clip.clone()
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
        assert_eq!(arms(&sys, crate::msg::TIMER_LAUNCHER_RELEASE), 1, "the release is armed at every close");
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
            assert_eq!(arms(&sys, crate::msg::TIMER_LAUNCHER_SAVE), usize::from(want), "learn {learn}: {:?}", sys.ops);
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
        assert!(sys.ops.iter().any(|o| o.contains("ListStartApps") && o.contains("None")), "{:?}", sys.ops);
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
