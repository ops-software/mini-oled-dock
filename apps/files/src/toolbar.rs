//! The toolbar (P3-files-app 2.2; FA-WIN, F1a.8): Back, Forward, Up, the location area and the right cluster (New, Sort,
//! View, More).

use oled_trust::frame::files::Location;
use oled_ui::model::FgRole;
use oled_ui::node::Text;
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{ButtonOut, IconButton, TextLabel};
use oled_ui::{
    Anchor, Event, EventCx, Glyph, Key, LayoutCx, MouseButton, NodeId, PaintCx, RectP, Reply, UiLang, Widget, WidgetId,
};

use oled_sys::i18n::S;

use crate::keys::{self, Cmd};
use crate::tr::{k, t, tf};
use crate::window::FilesScreen;
use crate::{chrome, nav, pathline, tab, toolmenus};

/// Logical px.
const BTN: i32 = 28;
const LEFT_X: [i32; 3] = [8, 40, 72];
const LOC_X: i32 = 112;
const GAP: i32 = 4;
const INSET: i32 = 8;
const TOP: i32 = 8;

const ID_BACK: WidgetId = WidgetId(10);
const ID_FORWARD: WidgetId = WidgetId(11);
const ID_UP: WidgetId = WidgetId(12);
const ID_LOC: WidgetId = WidgetId(13);
const ID_NEW: WidgetId = WidgetId(14);
const ID_SORT: WidgetId = WidgetId(15);
const ID_VIEW: WidgetId = WidgetId(16);
const ID_MORE: WidgetId = WidgetId(17);

/// Back, Forward, Up IconButtons, the location area (F1a: a TextLabel; F1b: pathline), the right cluster.
pub struct Toolbar {
    back: IconButton,
    forward: IconButton,
    up: IconButton,
    loc: TextLabel,
    new: IconButton,
    sort: IconButton,
    view: IconButton,
    more: IconButton,
    /// The cells of Back and Forward and of the right cluster (New, Sort, View, More) from the last layout: a menu
    /// opens under its own button.
    back_r: RectP,
    forward_r: RectP,
    right_r: [RectP; 4],
}

impl std::fmt::Debug for Toolbar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Toolbar")
    }
}

impl Default for Toolbar {
    fn default() -> Toolbar {
        Toolbar::new()
    }
}

impl Toolbar {
    pub fn new() -> Toolbar {
        let e = || Text::Borrowed("");
        Toolbar {
            back: IconButton::new(ID_BACK, Glyph::ChevronLeft, e()),
            forward: IconButton::new(ID_FORWARD, Glyph::ChevronRight, e()),
            up: IconButton::new(ID_UP, Glyph::ChevronUp, e()),
            loc: TextLabel::new(ID_LOC, e(), TypeRole::Body, FgRole::Secondary),
            new: IconButton::new(ID_NEW, Glyph::Plus, e()),
            sort: IconButton::new(ID_SORT, Glyph::SortUp, e()),
            view: IconButton::new(ID_VIEW, Glyph::List, e()),
            more: IconButton::new(ID_MORE, Glyph::More, e()),
            back_r: RectP::EMPTY,
            forward_r: RectP::EMPTY,
            right_r: [RectP::EMPTY; 4],
        }
    }
}

/// The last path component, the drive for a root, the app's Home word for Home.
pub fn place_name(l: &Location, lang: UiLang) -> String {
    match l {
        Location::Fs(p) => {
            let s = p.to_string_lossy();
            let trimmed = s.trim_end_matches('\\');
            match trimmed.rsplit('\\').next() {
                Some(last) if !last.is_empty() && trimmed.contains('\\') && !trimmed.ends_with(':') => last.to_string(),
                _ => trimmed.to_string(),
            }
        }
        Location::Home => t(k::FILES_APP_HOME, lang).to_string(),
        _ => String::new(),
    }
}

/// The full path as the location area shows it.
fn location_text(l: &Location, lang: UiLang) -> String {
    match l {
        Location::Fs(p) => p.to_string_lossy(),
        other => place_name(other, lang),
    }
}

fn with_chord(base: Text, cmd: Cmd, lang: UiLang) -> Text {
    let label = keys::TABLE
        .iter()
        .find(|r| r.cmd == cmd)
        .and_then(|r| r.chords.first())
        .and_then(|c| c.chord())
        .map(|c| c.label(lang));
    match label {
        Some(l) => Text::Owned(format!("{base} \u{b7} {l}")),
        None => base,
    }
}

/// The location area goes to pathline::layout; the plain label draws only while no tab strip exists.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let lang = s.shared.lang.get();
    let (back_to, fwd_to, up_to, loc, can) = {
        let nav = &s.tabs.active().nav;
        let back = nav.history(true, 1).into_iter().next().map(|(_, l)| place_name(&l, lang));
        let fwd = nav.history(false, 1).into_iter().next().map(|(_, l)| place_name(&l, lang));
        let up = nav::parent_of(nav.location()).map(|l| place_name(&l, lang));
        (back, fwd, up, location_text(nav.location(), lang), (nav.can_back(), nav.can_forward(), nav.can_up()))
    };
    let tb = &mut s.toolbar;
    let tip = |plain: S, named: S, who: Option<String>, c: Cmd| {
        let base = match who {
            Some(n) => tf(named, lang, &[("name", &n)]),
            None => t(plain, lang),
        };
        with_chord(base, c, lang)
    };
    *tb = Toolbar {
        back: rebuild(
            &tb.back,
            ID_BACK,
            Glyph::ChevronLeft,
            tip(k::FILES_NAV_BACK, k::FILES_NAV_BACK_TO, back_to, Cmd::Back),
            can.0,
        ),
        forward: rebuild(
            &tb.forward,
            ID_FORWARD,
            Glyph::ChevronRight,
            tip(k::FILES_NAV_FORWARD, k::FILES_NAV_FORWARD_TO, fwd_to, Cmd::Forward),
            can.1,
        ),
        up: rebuild(&tb.up, ID_UP, Glyph::ChevronUp, tip(k::FILES_NAV_UP, k::FILES_NAV_UP_TO, up_to, Cmd::Up), can.2),
        loc: {
            let mut l = TextLabel::new(ID_LOC, Text::Owned(loc), TypeRole::Body, FgRole::Secondary);
            l.set_trim(Trim::Start);
            l
        },
        new: rebuild(&tb.new, ID_NEW, Glyph::Plus, t(k::FILES_MENU_NEW, lang), true),
        sort: rebuild(&tb.sort, ID_SORT, Glyph::SortUp, t(k::FILES_MENU_SORT, lang), true),
        view: rebuild(&tb.view, ID_VIEW, Glyph::List, t(k::FILES_MENU_VIEW, lang), true),
        more: rebuild(&tb.more, ID_MORE, Glyph::More, t(k::FILES_MENU_MORE_ACTIONS, lang), true),
        back_r: RectP::EMPTY,
        forward_r: RectP::EMPTY,
        right_r: [RectP::EMPTY; 4],
    };
    let sc = cx.scale();
    let px = move |v: i32| sc.px(v);
    let (side, y) = (px(BTN), r.t + px(TOP));
    let left = LEFT_X.map(|x| RectP::wh(r.l + px(x), y, side, side));
    tb.back.layout(cx, left[0]);
    tb.forward.layout(cx, left[1]);
    tb.up.layout(cx, left[2]);
    (tb.back_r, tb.forward_r) = (left[0], left[1]);
    let mut right = r.r - px(INSET);
    let mut rects = [RectP::EMPTY; 4];
    for slot in rects.iter_mut().rev() {
        *slot = RectP::wh(right - side, y, side, side);
        right -= side + px(GAP);
    }
    tb.new.layout(cx, rects[0]);
    tb.sort.layout(cx, rects[1]);
    tb.view.layout(cx, rects[2]);
    tb.more.layout(cx, rects[3]);
    tb.right_r = rects;
    let loc_l = r.l + px(LOC_X);
    let text_h = px(20);
    let loc_r =
        RectP { l: loc_l, t: y + (side - text_h) / 2, r: (right - px(GAP)).max(loc_l), b: y + (side + text_h) / 2 };
    if !chrome::has_strip(s) {
        s.toolbar.loc.layout(cx, loc_r);
    }
    pathline::layout(s, cx, loc_r);
}

fn rebuild(old: &IconButton, id: WidgetId, g: Glyph, tip: Text, enabled: bool) -> IconButton {
    let _ = old;
    let mut b = IconButton::new(id, g, tip);
    b.set_enabled(enabled);
    b
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let tb = &s.toolbar;
    for w in [&tb.back, &tb.forward, &tb.up, &tb.new, &tb.sort, &tb.view, &tb.more] {
        w.paint(cx);
    }
    if !chrome::has_strip(s) {
        tb.loc.paint(cx);
    }
    pathline::paint(s, cx);
}

/// The rects of New, Sort, View and More (physical pixels, as of the last layout).
pub fn right_rects(s: &FilesScreen) -> [RectP; 4] {
    s.toolbar.right_r
}

/// Back, Forward, Up -> keys::run; the right cluster -> toolmenus::open_*; a right click or the Menu key over Back or
/// Forward -> tab::history_menu (F4.3).
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if let Some(r) = history_menu_event(s, cx, ev) {
        return Some(r);
    }
    let tb = &mut s.toolbar;
    let hit = [
        (tb.back.event(cx, ev), 0u8),
        (tb.forward.event(cx, ev), 1),
        (tb.up.event(cx, ev), 2),
        (tb.new.event(cx, ev), 3),
        (tb.sort.event(cx, ev), 4),
        (tb.view.event(cx, ev), 5),
        (tb.more.event(cx, ev), 6),
    ]
    .into_iter()
    .find_map(|(o, i)| matches!(o, Some(ButtonOut::Clicked)).then_some(i))?;
    let at = |i: usize| Anchor::Below(s.toolbar.right_r[i]);
    Some(match hit {
        0 => keys::run(s, cx, Cmd::Back),
        1 => keys::run(s, cx, Cmd::Forward),
        2 => keys::run(s, cx, Cmd::Up),
        3 => toolmenus::open_new(s, cx, at(0)),
        4 => toolmenus::open_sort(s, cx, at(1)),
        5 => toolmenus::open_view(s, cx, at(2)),
        _ => toolmenus::open_more(s, cx, at(3)),
    })
}

/// A right click, or the Menu key, on Back or Forward opens that button's history (F4.3).
fn history_menu_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let asked =
        matches!(ev, Event::Click { button: MouseButton::Right, .. } | Event::Key { key: Key::APPS, down: true, .. });
    if !asked {
        return None;
    }
    let (back, below) = match cx.target() {
        Some(n) if n == NodeId::new(ID_BACK, 0) => (true, s.toolbar.back_r),
        Some(n) if n == NodeId::new(ID_FORWARD, 0) => (false, s.toolbar.forward_r),
        _ => return None,
    };
    tab::history_menu(s, cx, back, Anchor::Below(below)).then_some(Reply::Redraw)
}
