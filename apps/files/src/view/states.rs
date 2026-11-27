//! The page states over the list (P3-files-app 2.2; FA-WIN, F1a.9): empty, denied, offline, connecting and the rest,
//! with their banner buttons.

use oled_trust::frame::files::Location;
use oled_ui::model::FgRole;
use oled_ui::node::Text;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{Button, ButtonKind, ButtonOut, TextLabel};
use oled_ui::{Event, EventCx, Glyph, LayoutCx, PaintCx, RectP, Reply, Widget, WidgetId};

use crate::keys::Cmd;
use crate::nav::Page;
use crate::open;
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen};

const ID_TEXT: WidgetId = WidgetId(40);
const ID_HINT: WidgetId = WidgetId(41);
const ID_BANNER_TEXT: WidgetId = WidgetId(42);
/// The banner's two buttons, left to right (tests walk the focus to them).
pub const ID_FIRST: WidgetId = WidgetId(43);
pub const ID_SECOND: WidgetId = WidgetId(44);

/// Logical px.
const BANNER_H: i32 = 36;
const BANNER_MARGIN: i32 = 12;
const BTN_H: i32 = 28;
const GAP: i32 = 8;
const LOCK: i32 = 16;
/// Where the message sits, as a share of the list's height (percent).
const TEXT_AT: i32 = 40;

/// What a banner button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    OpenInExplorer,
    Back,
    Retry,
}

/// Which state the last layout built: a change of it needs a new layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    Nothing,
    Message,
    Denied,
    Offline,
}

fn shown_of(p: &Page) -> Shown {
    match p {
        Page::Empty | Page::StillLoading | Page::TooLong | Page::Unsupported | Page::Connecting => Shown::Message,
        Page::Denied => Shown::Denied,
        Page::Offline => Shown::Offline,
        _ => Shown::Nothing,
    }
}

/// The widgets of the page state the last layout built.
pub struct States {
    shown: Shown,
    page: Page,
    text: TextLabel,
    hint: Option<TextLabel>,
    banner: RectP,
    lock: RectP,
    buttons: Vec<(Button, Act)>,
}

impl std::fmt::Debug for States {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("States")
    }
}

impl Default for States {
    fn default() -> States {
        States::new()
    }
}

impl States {
    pub fn new() -> States {
        States {
            shown: Shown::Nothing,
            page: Page::Home,
            text: TextLabel::new(ID_TEXT, Text::Borrowed(""), TypeRole::Body, FgRole::Tertiary),
            hint: None,
            banner: RectP::EMPTY,
            lock: RectP::EMPTY,
            buttons: Vec::new(),
        }
    }
}

/// True when the page changed since the last layout, so the state must be laid out again.
pub fn refresh(s: &FilesScreen) -> bool {
    s.states.page != *s.tabs.active().nav.page()
}

/// The Home page shows no page state: what a folder left behind goes, and the page it records is Home.
pub fn on_home(s: &mut FilesScreen) {
    let me = &mut s.states;
    me.page = Page::Home;
    me.shown = Shown::Nothing;
    me.hint = None;
    me.buttons.clear();
}

fn path_of(l: &Location) -> String {
    match l {
        Location::Fs(p) => p.to_string_lossy(),
        _ => String::new(),
    }
}

/// The state's widgets in the list's rect.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, body: RectP) {
    let lang = s.shared.lang.get();
    let page = s.tabs.active().nav.page().clone();
    let path = path_of(s.tabs.active().nav.location());
    let top = s.tabs.active().table.viewport().t.max(body.t);
    let sc = cx.scale();
    let me = &mut s.states;
    me.page = page.clone();
    me.shown = shown_of(&page);
    me.hint = None;
    me.buttons.clear();
    let shown = me.shown;
    match shown {
        Shown::Nothing => {}
        Shown::Message => {
            let text = match page {
                Page::Empty => t(k::FILES_STATE_EMPTY, lang),
                Page::StillLoading => t(k::FILES_STATE_STILL_LOADING, lang),
                Page::TooLong => t(k::FILES_STATE_TOO_LONG, lang),
                Page::Unsupported => t(k::FILES_STATE_UNSUPPORTED, lang),
                _ => tf(k::FILES_STATE_CONNECTING, lang, &[("path", &path)]),
            };
            let y = top + (body.b - top) * TEXT_AT / 100;
            me.text = TextLabel::new(ID_TEXT, text, TypeRole::Body, FgRole::Tertiary);
            let line = sc.px(TypeRole::Body.spec().line);
            let w = me.text.preferred(cx).w.min(body.w());
            let x = body.l + (body.w() - w) / 2;
            me.text.layout(cx, RectP { l: x, t: y, r: x + w, b: y + line });
            if page == Page::Empty {
                let mut hint =
                    TextLabel::new(ID_HINT, t(k::FILES_STATE_EMPTY_HINT, lang), TypeRole::Caption, FgRole::Tertiary);
                let hw = hint.preferred(cx).w.min(body.w());
                let hx = body.l + (body.w() - hw) / 2;
                let hy = y + line + sc.px(4);
                hint.layout(cx, RectP { l: hx, t: hy, r: hx + hw, b: hy + sc.px(TypeRole::Caption.spec().line) });
                me.hint = Some(hint);
            }
        }
        Shown::Denied | Shown::Offline => {
            let (text, acts): (Text, [(Text, Act); 2]) = if shown == Shown::Denied {
                (
                    t(k::FILES_STATE_DENIED, lang),
                    [
                        (t(k::FILES_ACTION_OPEN_EXPLORER, lang), Act::OpenInExplorer),
                        (t(k::FILES_ACTION_BACK, lang), Act::Back),
                    ],
                )
            } else {
                (
                    tf(k::FILES_STATE_OFFLINE, lang, &[("path", &path)]),
                    [
                        (t(k::FILES_ACTION_RETRY, lang), Act::Retry),
                        (t(k::FILES_ACTION_OPEN_EXPLORER, lang), Act::OpenInExplorer),
                    ],
                )
            };
            let m = sc.px(BANNER_MARGIN);
            let h = sc.px(BANNER_H);
            let banner =
                RectP { l: body.l + m, t: top + sc.px(8), r: (body.r - m).max(body.l + m), b: top + sc.px(8) + h };
            me.banner = banner;
            let pad = sc.px(8);
            let mut right = banner.r - pad;
            let bh = sc.px(BTN_H).min(h);
            let by = banner.t + (h - bh) / 2;
            let mut buttons = Vec::new();
            for (i, (label, act)) in acts.into_iter().enumerate().rev() {
                let id = if i == 0 { ID_FIRST } else { ID_SECOND };
                let mut b = Button::new(id, label, ButtonKind::Secondary);
                let w = b.preferred(cx).w;
                b.layout(cx, RectP { l: right - w, t: by, r: right, b: by + bh });
                right -= w + sc.px(GAP);
                buttons.push((b, act));
            }
            buttons.reverse();
            me.buttons = buttons;
            let mut left = banner.l + pad;
            if shown == Shown::Denied {
                let g = sc.px(LOCK);
                me.lock = RectP { l: left, t: banner.t + (h - g) / 2, r: left + g, b: banner.t + (h - g) / 2 + g };
                left += g + sc.px(GAP);
            } else {
                me.lock = RectP::EMPTY;
            }
            let line = sc.px(TypeRole::Body.spec().line);
            me.text = TextLabel::new(ID_BANNER_TEXT, text, TypeRole::Body, FgRole::Primary);
            let w = me.text.preferred(cx).w.min((right - left).max(0));
            me.text.layout(
                cx,
                RectP { l: left, t: banner.t + (h - line) / 2, r: left + w, b: banner.t + (h - line) / 2 + line },
            );
        }
    }
}

/// The state over the (empty) list.
pub fn paint_state(s: &FilesScreen, page: &Page, r: RectP, cx: &mut PaintCx) {
    let _ = (page, r);
    let me = &s.states;
    match me.shown {
        Shown::Nothing => {}
        Shown::Message => {
            me.text.paint(cx);
            if let Some(h) = &me.hint {
                h.paint(cx);
            }
        }
        Shown::Denied | Shown::Offline => {
            let (radius, fill) = (cx.px(cx.tokens().radius.s), cx.tokens().color.fill_control);
            cx.fill(me.banner, radius, fill);
            if me.shown == Shown::Denied {
                let ink = cx.tokens().color.fg_secondary;
                cx.glyph(me.lock, Glyph::Lock, ink);
            }
            me.text.paint(cx);
            for (b, _) in &me.buttons {
                b.paint(cx);
            }
        }
    }
}

/// The banners' buttons: Retry navigates again, Back goes back, Open in File Explorer hands the folder off.
pub fn state_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let mut hit = None;
    for (b, act) in s.states.buttons.iter_mut() {
        if let Some(ButtonOut::Clicked) = b.event(cx, ev) {
            hit = Some(*act);
            break;
        }
    }
    Some(match hit? {
        Act::Retry => window::cmd(s, cx, Cmd::Refresh),
        Act::Back => window::cmd(s, cx, Cmd::Back),
        Act::OpenInExplorer => open::hand_off_current(s, cx),
    })
}
