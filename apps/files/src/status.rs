//! The status line (P3-files-app 2.2; FA-WIN, F1a.9): counts, the selection's size, free space, and the notice every
//! group shows through notice().

use oled_trust::frame::files::{Location, OpId};
use oled_ui::model::{FgRole, ListModel};
use oled_ui::node::Text;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{Button, ButtonKind, ButtonOut, TextLabel};
use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply, UiLang, Widget, WidgetId};

use crate::keys::Cmd;
use crate::listing::{TabId, WidePath};
use crate::model::id_of;
use crate::names;
use crate::nav::Page;
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen};
use crate::{handoff, notices, open, ops_ui};

const ID_ITEMS: WidgetId = WidgetId(30);
const ID_SELECTED: WidgetId = WidgetId(31);
const ID_FREE: WidgetId = WidgetId(32);
const ID_NOTICE: WidgetId = WidgetId(33);
/// The notice's action button.
pub const ID_ACTION: WidgetId = WidgetId(34);

/// Logical px.
const INSET: i32 = 12;
const GAP: i32 = 8;
const ACTION_H: i32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeAct {
    Retry,
    OpenInExplorer,
    Reopen,
    Rest(OpId),
    Undo(OpId),
    OpenRecycleBin,
    Details(OpId),
    /// The loop guard's notice: the hand-off it held back (F4.13).
    HandOff,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: Text,
    pub action: Option<(Text, NoticeAct)>,
    /// 6,000 by default.
    pub hold_ms: u32,
}

/// How long a notice stays by default.
pub const NOTICE_HOLD_MS: u32 = 6_000;

impl Notice {
    /// A notice with no action, held NOTICE_HOLD_MS.
    pub fn new(text: Text) -> Notice {
        Notice { text, action: None, hold_ms: NOTICE_HOLD_MS }
    }
}

/// What the line counts: items of the shown listing, the selected rows and the sum of the selected files' sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub items: u32,
    pub selected: usize,
    pub bytes: u64,
    /// At least one selected row is not a folder: only then is a size shown.
    pub files: bool,
}

/// The counts, the free space, the notice slot and the labels the last layout built from them.
pub struct Status {
    stats: Stats,
    /// The free bytes of the volume of the folder the path names.
    free: Option<(WidePath, u64)>,
    notice: Option<Notice>,
    /// Counts every notice posted, and is kept after the notice goes: an action's owner compares it with the serial it
    /// noted when it posted its own notice.
    serial: u64,
    /// Whether the last layout drew the free space.
    laid_free: bool,
    items: TextLabel,
    selected: TextLabel,
    free_label: TextLabel,
    text: TextLabel,
    action: Option<(Button, NoticeAct)>,
}

impl std::fmt::Debug for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Status")
    }
}

impl Default for Status {
    fn default() -> Status {
        Status::new()
    }
}

impl Status {
    pub fn new() -> Status {
        let e = || Text::Borrowed("");
        Status {
            stats: Stats::default(),
            free: None,
            notice: None,
            serial: 0,
            laid_free: false,
            items: TextLabel::new(ID_ITEMS, e(), TypeRole::Caption, FgRole::Tertiary),
            selected: TextLabel::new(ID_SELECTED, e(), TypeRole::Caption, FgRole::Secondary),
            free_label: TextLabel::new(ID_FREE, e(), TypeRole::Caption, FgRole::Tertiary),
            text: TextLabel::new(ID_NOTICE, e(), TypeRole::Caption, FgRole::Primary),
            action: None,
        }
    }

    /// The counts as of the last refresh.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The serial of the latest notice posted (0 before the first).
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// The notice on show, if any.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }
}

/// The counts of the active tab; the selection's size only when asked (it walks the selected rows).
fn count(s: &FilesScreen, with_selection: bool, before: Stats) -> Stats {
    let t = s.tabs.active();
    let m = t.table.model();
    let rows = ListModel::len(m);
    let mut st = Stats { items: rows as u32, ..before };
    if with_selection {
        let sel = t.table.selection();
        st.selected = sel.count(rows);
        st.bytes = 0;
        st.files = false;
        if st.selected > 0 {
            sel.for_each(m, &mut |_, key| {
                let id = id_of(key);
                if !m.listing().is_dir(id) {
                    st.files = true;
                    st.bytes += m.listing().entry(id).map_or(0, |e| e.size);
                }
            });
        }
    }
    st
}

/// Recounts. True when something the line shows changed, so its labels must be laid out again.
pub fn refresh(s: &mut FilesScreen, with_selection: bool) -> bool {
    let st = count(s, with_selection, s.status.stats);
    let changed = st != s.status.stats;
    s.status.stats = st;
    changed
}

/// The free space of a folder, when the tab is still there.
pub fn on_free_space(s: &mut FilesScreen, cx: &mut EventCx, root: WidePath, free: Option<u64>) {
    let here = matches!(s.tabs.active().nav.location(), Location::Fs(p) if *p == root);
    if !here {
        return;
    }
    s.status.free = free.map(|f| (root, f));
    cx.relayout();
}

/// "1 item" / "1,204 items".
fn items_text(s: &FilesScreen, n: u32, lang: UiLang) -> Text {
    if n == 1 {
        return t(k::FILES_STATUS_ITEMS_ONE, lang);
    }
    let fmt = s.shared.fmt.borrow().clone();
    tf(k::FILES_STATUS_ITEMS_OTHER, lang, &[("n", &fmt.count(u64::from(n)))])
}

/// " . 3 selected . 4.2 MB", or nothing when nothing is selected.
fn selected_text(s: &FilesScreen, st: Stats, lang: UiLang) -> String {
    if st.selected == 0 {
        return String::new();
    }
    let fmt = s.shared.fmt.borrow().clone();
    let mut out = String::from(" \u{b7} ");
    if st.selected == 1 {
        out.push_str(&t(k::FILES_STATUS_SELECTED_ONE, lang));
    } else {
        out.push_str(&tf(k::FILES_STATUS_SELECTED_OTHER, lang, &[("n", &fmt.count(st.selected as u64))]));
    }
    if st.files {
        out.push_str(" \u{b7} ");
        out.push_str(&fmt.size(st.bytes, false));
    }
    out
}

/// The items and the selection on the left (or the notice in their place), the free space on the right.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    refresh(s, true);
    let lang = s.shared.lang.get();
    let st = s.status.stats;
    let home = matches!(s.tabs.active().nav.page(), Page::Home);
    let items = items_text(s, st.items, lang);
    let selected = selected_text(s, st, lang);
    // Only the free space of the folder the tab shows: the old volume's number goes at once on a navigation.
    let at = match s.tabs.active().nav.location() {
        Location::Fs(p) => Some(p),
        _ => None,
    };
    let free = match (&s.status.free, home) {
        (Some((root, f)), false) if at == Some(root) => {
            let fmt = s.shared.fmt.borrow().clone();
            Some(tf(k::FILES_STATUS_FREE, lang, &[("size", &fmt.size(*f, false))]))
        }
        _ => None,
    };
    let sc = cx.scale();
    let line = sc.px(TypeRole::Caption.spec().line);
    let y = r.t + (r.h() - line) / 2;
    let (x0, right) = (r.l + sc.px(INSET), r.r - sc.px(INSET));
    let me = &mut s.status;
    me.laid_free = free.is_some();
    let mut edge = right;
    if let Some(text) = free {
        me.free_label = TextLabel::new(ID_FREE, text, TypeRole::Caption, FgRole::Tertiary);
        let w = me.free_label.preferred(cx).w.min((right - x0).max(0));
        me.free_label.layout(cx, RectP { l: right - w, t: y, r: right, b: y + line });
        edge = right - w - sc.px(GAP);
    }
    let avail = (edge - x0).max(0);
    match me.notice.clone() {
        Some(n) => {
            me.text = TextLabel::new(ID_NOTICE, n.text.clone(), TypeRole::Caption, FgRole::Primary);
            let mut text_w = me.text.preferred(cx).w.min(avail);
            me.action = n.action.map(|(label, act)| (Button::new(ID_ACTION, label, ButtonKind::Secondary), act));
            let mut btn_w = 0;
            if let Some((b, _)) = me.action.as_mut() {
                btn_w = b.preferred(cx).w.min(avail / 2);
                text_w = text_w.min((avail - btn_w - sc.px(GAP)).max(0));
            }
            me.text.layout(cx, RectP { l: x0, t: y, r: x0 + text_w, b: y + line });
            if let Some((b, _)) = me.action.as_mut() {
                let (bh, bx) = (sc.px(ACTION_H), x0 + text_w + sc.px(GAP));
                let by = r.t + (r.h() - bh) / 2;
                b.layout(cx, RectP { l: bx, t: by, r: bx + btn_w, b: by + bh });
            }
        }
        None => {
            me.action = None;
            me.items = TextLabel::new(ID_ITEMS, items, TypeRole::Caption, FgRole::Tertiary);
            me.selected =
                TextLabel::new(ID_SELECTED, Text::Owned(selected.clone()), TypeRole::Caption, FgRole::Secondary);
            let iw = me.items.preferred(cx).w.min(avail);
            me.items.layout(cx, RectP { l: x0, t: y, r: x0 + iw, b: y + line });
            if !selected.is_empty() {
                let sw = me.selected.preferred(cx).w.min((avail - iw).max(0));
                me.selected.layout(cx, RectP { l: x0 + iw, t: y, r: x0 + iw + sw, b: y + line });
            }
        }
    }
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let me = &s.status;
    if me.laid_free {
        me.free_label.paint(cx);
    }
    if me.notice.is_some() {
        me.text.paint(cx);
        if let Some((b, _)) = &me.action {
            b.paint(cx);
        }
        return;
    }
    me.items.paint(cx);
    if me.stats.selected > 0 {
        me.selected.paint(cx);
    }
}

fn notice_timer() -> oled_ui::TimerId {
    window::timer(names::TIMER_NOTICE, TabId(0))
}

/// Shows the notice in place of the items for its hold or until Esc.
pub fn notice(s: &mut FilesScreen, cx: &mut EventCx, n: Notice) {
    oled_sys::log!("files: notice: {}", n.text);
    cx.set_timer(notice_timer(), n.hold_ms);
    s.status.notice = Some(n);
    s.status.serial += 1;
    cx.relayout();
}

/// The selection changed: the line counts it.
pub fn selection_changed(s: &mut FilesScreen, cx: &mut EventCx) {
    if refresh(s, true) {
        cx.relayout();
    }
}

fn clear_notice(s: &mut FilesScreen, cx: &mut EventCx) {
    cx.cancel_timer(notice_timer());
    s.status.notice = None;
    s.status.action = None;
    cx.relayout();
}

/// The notice's action button: a click on it runs on_notice_action with the notice's NoticeAct.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let (b, act) = s.status.action.as_mut()?;
    let act = *act;
    match b.event(cx, ev) {
        Some(ButtonOut::Clicked) => {
            clear_notice(s, cx);
            Some(on_notice_action(s, cx, act))
        }
        None => None,
    }
}

/// Esc dismisses a notice first; true when one was showing.
pub fn on_escape(s: &mut FilesScreen, cx: &mut EventCx) -> bool {
    if s.status.notice.is_none() {
        return false;
    }
    clear_notice(s, cx);
    true
}

/// Timer kind 9: the hold is over.
pub fn on_notice_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    if s.status.notice.is_some() {
        s.status.notice = None;
        s.status.action = None;
        cx.relayout();
    }
}

/// Retry -> window::cmd(Refresh); OpenInExplorer -> open::hand_off_current; Reopen -> notices::reopen; Rest ->
/// ops_ui::on_rest; Undo -> ops_ui::cmd(Undo); OpenRecycleBin -> handoff::hand_off; Details -> ops_ui::flyout_open;
/// HandOff -> handoff::open_held.
pub fn on_notice_action(s: &mut FilesScreen, cx: &mut EventCx, a: NoticeAct) -> Reply {
    match a {
        NoticeAct::Retry => match open::retry(s, cx) {
            Some(r) => r,
            None => window::cmd(s, cx, Cmd::Refresh),
        },
        NoticeAct::OpenInExplorer => open::hand_off_current(s, cx),
        NoticeAct::Reopen => notices::reopen(s, cx),
        NoticeAct::Rest(op) => ops_ui::on_rest(s, cx, op),
        NoticeAct::Undo(_) => ops_ui::cmd(s, cx, Cmd::Undo),
        NoticeAct::OpenRecycleBin => handoff::hand_off(s, cx, handoff::Target::RecycleBin, false),
        NoticeAct::Details(op) => ops_ui::flyout_open(s, cx, Some(op)),
        NoticeAct::HandOff => handoff::open_held(s, cx),
    }
}
