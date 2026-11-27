//! The inline bar that asks before a permanent delete (P3-files-app 2.2, F3.2, U3). It replaces the status items when
//! a Del meets an item that cannot go to the Recycle Bin, takes the keyboard while open, and only a fresh Enter (never
//! a repeat) or its Delete button sends DeletePermanent. A Del on the open bar, pressed again or held, is consumed and
//! sends nothing: the key that opened the bar never confirms it. Esc, Cancel, a navigation, a selection change or a
//! tab switch close it and nothing is sent.

use oled_trust::frame::files::Location;
use oled_ui::model::{FgRole, RowKey};
use oled_ui::node::WidgetId;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{Button, ButtonKind, ButtonOut, TextLabel};
use oled_ui::{Event, EventCx, Key, LayoutCx, PaintCx, RectP, Reply, Widget};

use crate::listing::{TabId, WidePath};
use crate::ops_ui::{self, MutReq};
use crate::tab::Tab;
use crate::tr::{k, t, tf};
use crate::window::FilesScreen;

const ID_TEXT: WidgetId = WidgetId(350);
const ID_DELETE: WidgetId = WidgetId(351);
const ID_CANCEL: WidgetId = WidgetId(352);

/// Logical px.
const INSET: i32 = 12;
const GAP: i32 = 12;
const BUTTON_H: i32 = 24;

/// A Del whose classification is on its way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub req: u64,
    pub tab: TabId,
    pub at: Location,
    /// The Del of a redo (its requests keep the redo stack).
    pub redo: bool,
}

/// The open bar: what Enter sends, and what it opened over.
struct Open {
    yes: Vec<WidePath>,
    no: Vec<WidePath>,
    tab: TabId,
    at: Location,
    sel: (usize, Option<RowKey>),
    redo: bool,
}

pub struct RecycleBar {
    pub(crate) pending: Option<Pending>,
    open: Option<Open>,
    text: TextLabel,
    delete: Button,
    cancel: Button,
    rect: RectP,
    delete_r: RectP,
    cancel_r: RectP,
}

impl std::fmt::Debug for RecycleBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecycleBar").field("open", &self.open.is_some()).finish()
    }
}

impl Default for RecycleBar {
    fn default() -> RecycleBar {
        RecycleBar::new()
    }
}

impl RecycleBar {
    pub fn new() -> RecycleBar {
        let e = oled_ui::node::Text::Borrowed("");
        RecycleBar {
            pending: None,
            open: None,
            text: TextLabel::new(ID_TEXT, e.clone(), TypeRole::Caption, FgRole::Primary),
            delete: Button::new(ID_DELETE, e.clone(), ButtonKind::Secondary),
            cancel: Button::new(ID_CANCEL, e, ButtonKind::Secondary),
            rect: RectP::EMPTY,
            delete_r: RectP::EMPTY,
            cancel_r: RectP::EMPTY,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The items the bar would delete permanently, while open.
    pub fn permanent(&self) -> &[WidePath] {
        self.open.as_ref().map_or(&[], |o| o.no.as_slice())
    }

    /// The items the bar would recycle, while open.
    pub fn recyclable(&self) -> &[WidePath] {
        self.open.as_ref().map_or(&[], |o| o.yes.as_slice())
    }

    /// A Del waits for its classification.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The bar's rect and its two buttons' as laid out (Delete, Cancel).
    pub fn rects(&self) -> Option<(RectP, RectP, RectP)> {
        self.open.as_ref()?;
        Some((self.rect, self.delete_r, self.cancel_r))
    }
}

/// The window's bar (what tests read).
pub fn bar_of(s: &FilesScreen) -> &RecycleBar {
    &s.bar
}

/// What the bar compares to see that the selection changed.
fn snapshot(t: &Tab) -> (usize, Option<RowKey>) {
    let len = oled_ui::model::ListModel::len(t.table.model());
    (t.table.selection().count(len), t.table.focused())
}

/// Opens the bar over the active tab: yes may go to the bin, no cannot.
pub(crate) fn open(s: &mut FilesScreen, cx: &mut EventCx, yes: Vec<WidePath>, no: Vec<WidePath>, redo: bool) {
    let tab = s.tabs.active();
    let o = Open { yes, no, tab: tab.id, at: tab.nav.location().clone(), sel: snapshot(tab), redo };
    s.bar.open = Some(o);
    cx.relayout();
}

/// Closes the bar; nothing is sent.
fn close(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    s.bar.open = None;
    cx.relayout();
    Reply::Relayout
}

/// Enter or the Delete button: the recyclable items to the bin (if any), the others deleted permanently.
fn confirm(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let Some(o) = s.bar.open.take() else { return Reply::None };
    if !o.yes.is_empty() {
        if o.redo {
            s.shared.ops.borrow_mut().mark_redo_next();
        }
        ops_ui::request(s, cx, MutReq::Recycle(o.yes));
    }
    if o.redo {
        s.shared.ops.borrow_mut().mark_redo_next();
    }
    ops_ui::request(s, cx, MutReq::DeletePermanent(o.no));
    // A request the gate refused leaves the mark: it must not reach a later, unrelated request.
    s.shared.ops.borrow_mut().take_redo_next();
    cx.relayout();
    Reply::Relayout
}

/// In the status rect: the question, then Delete (Enter) in ACCENT_DANGER and Cancel (Esc). A tab, folder or
/// selection that changed since the bar opened closes it here.
pub fn bar_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let Some(o) = &s.bar.open else { return };
    let tab = s.tabs.active();
    if tab.id != o.tab || *tab.nav.location() != o.at || snapshot(tab) != o.sel {
        s.bar.open = None;
        return;
    }
    let lang = cx.lang();
    let n = o.no.len() as u64;
    let text = if n == 1 {
        t(k::FILES_OPS_NONRECYCLABLE_ONE, lang)
    } else {
        let fmt = s.shared.fmt.borrow().clone();
        tf(k::FILES_OPS_NONRECYCLABLE_OTHER, lang, &[("n", &fmt.count(n))])
    };
    let bar = &mut s.bar;
    bar.rect = r;
    bar.text = TextLabel::new(ID_TEXT, text, TypeRole::Caption, FgRole::Primary);
    bar.delete = Button::new(ID_DELETE, t(k::FILES_OPS_BAR_DELETE, lang), ButtonKind::Secondary);
    bar.delete.set_danger(true);
    bar.cancel = Button::new(ID_CANCEL, t(k::FILES_OPS_BAR_CANCEL, lang), ButtonKind::Secondary);
    let (x0, right) = (r.l + cx.px(INSET), r.r - cx.px(INSET));
    let gap = cx.px(GAP);
    let dw = bar.delete.preferred(cx).w;
    let cw = bar.cancel.preferred(cx).w;
    let line = cx.px(TypeRole::Caption.spec().line);
    let tw = bar.text.preferred(cx).w.min((right - x0 - dw - cw - gap * 2).max(0));
    let y = r.t + (r.h() - line) / 2;
    bar.text.layout(cx, RectP { l: x0, t: y, r: x0 + tw, b: y + line });
    let bh = cx.px(BUTTON_H).min(r.h());
    let by = r.t + (r.h() - bh) / 2;
    let dl = x0 + tw + gap;
    bar.delete_r = RectP::wh(dl, by, dw, bh);
    bar.cancel_r = RectP::wh(dl + dw + gap, by, cw, bh);
    let (dr, cr) = (bar.delete_r, bar.cancel_r);
    bar.delete.layout(cx, dr);
    bar.cancel.layout(cx, cr);
}

/// Over the status items: the window's black, then the question and the buttons.
pub fn bar_paint(s: &FilesScreen, cx: &mut PaintCx) {
    if s.bar.open.is_none() {
        return;
    }
    let bg = cx.tokens().color.bg;
    let line = cx.px(cx.tokens().size.progress_h);
    cx.fill(RectP { t: s.bar.rect.t + line, ..s.bar.rect }, 0, bg);
    s.bar.text.paint(cx);
    s.bar.delete.paint(cx);
    s.bar.cancel.paint(cx);
}

/// While open the bar takes the keyboard: a fresh Enter confirms, Esc cancels, other keys stop here, Del among them
/// (pressed again or held past the repeat delay, it sends nothing and the bar stays). Its buttons act; a press
/// anywhere else closes it and goes on.
pub fn bar_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    s.bar.open.as_ref()?;
    if let Some(ButtonOut::Clicked) = s.bar.delete.event(cx, ev) {
        return Some(confirm(s, cx));
    }
    if let Some(ButtonOut::Clicked) = s.bar.cancel.event(cx, ev) {
        return Some(close(s, cx));
    }
    match ev {
        Event::Key { key: Key::ENTER, down: true, repeat: false, .. } => {
            cx.consume();
            Some(confirm(s, cx))
        }
        Event::Key { key: Key::ESC, down: true, .. } => {
            cx.consume();
            Some(close(s, cx))
        }
        Event::Key { down, .. } => {
            if *down {
                cx.consume();
            }
            Some(Reply::None)
        }
        Event::Text(_) => Some(Reply::None),
        Event::PointerDown { pos, .. } if !s.bar.rect.contains(*pos) => {
            close(s, cx);
            None
        }
        _ => None,
    }
}
