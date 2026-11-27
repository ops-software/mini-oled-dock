//! The window's chrome (P3-files-app 2.2; FB-CHROME, F1b.1): the tab strip, the Beta label and, with the custom
//! caption, the caption buttons. With the system caption the strip sits in its own TAB_ROW_H row under Windows'
//! caption; with the custom one it fills a TITLEBAR_H strip at the top of the client area.

use oled_ui::host::Caption;
use oled_ui::model::FgRole;
use oled_ui::node::Text;
use oled_ui::theme::TypeRole;
use oled_ui::tokens::{CAPTION_BTN_W, TAB_ROW_H, TITLEBAR_H};
use oled_ui::widgets::{CaptionButtons, CaptionOut, TabStrip, TextLabel};
use oled_ui::{
    CaptionHit, Event, EventCx, Key, LayoutCx, PaintCx, PointP, RectP, Reply, Scale, Screen, Widget, WidgetId,
};

use crate::keys::Cmd;
use crate::pathline::PathLine;
use crate::tr::{k, t};
use crate::window::FilesScreen;
use crate::{pathline, tab};

pub(crate) const ID_STRIP: WidgetId = WidgetId(20);
const ID_BUTTONS: WidgetId = WidgetId(21);
const ID_BETA: WidgetId = WidgetId(22);

/// The space between the Beta label and the caption buttons, the label's room, and the space between the tabs and
/// the label (logical px).
const BETA_GAP: i32 = 12;
const BETA_W: i32 = 48;
const TABS_GAP: i32 = 8;

/// What the strip shows, to tell a change from a repaint: key, label, tip, busy, error.
type Sig = (u64, String, String, bool, bool);

pub struct Chrome {
    strip: TabStrip,
    buttons: CaptionButtons,
    beta: TextLabel,
    rects: ChromeRects,
    caption: Caption,
    /// What the strip holds now (set by layout or by an event) and the selected tab's key.
    sig: (Vec<Sig>, u64),
    /// Layout changed the strip's tabs without an EventCx: the next event restarts the busy sweep and the widths.
    stale: bool,
    pub(crate) path: PathLine,
}

impl std::fmt::Debug for Chrome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Chrome")
    }
}

impl Default for Chrome {
    fn default() -> Chrome {
        Chrome::new()
    }
}

impl Chrome {
    pub fn new() -> Chrome {
        Chrome {
            strip: TabStrip::new(ID_STRIP),
            buttons: CaptionButtons::new(ID_BUTTONS),
            beta: TextLabel::new(ID_BETA, Text::Borrowed(""), TypeRole::Caption, FgRole::Tertiary),
            rects: ChromeRects::default(),
            caption: Caption::System,
            sig: (Vec::new(), 0),
            stale: false,
            path: PathLine::new(),
        }
    }

    /// The rects of the last layout.
    pub fn last_rects(&self) -> ChromeRects {
        self.rects
    }

    /// The caption the last layout used.
    pub fn caption(&self) -> Caption {
        self.caption
    }

    /// The tab strip, for the chords (select_next) and the tests.
    pub(crate) fn strip_mut(&mut self) -> &mut TabStrip {
        &mut self.strip
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeRects {
    pub strip: RectP,
    pub tabs: RectP,
    pub beta: Option<RectP>,
    pub buttons: Option<RectP>,
    pub below: i32,
}

/// Pure. Custom: a TITLEBAR_H strip holding the tabs, the Beta label (when STAGE_BETA) 12 px left of the three
/// caption buttons, which fill the right end. System: Windows' caption stays and the tabs get a TAB_ROW_H row
/// at the top of the client area, with no label (the title says Beta) and no buttons. `below` is the first y under it.
pub fn rects(caption: Caption, width: i32, scale: Scale) -> ChromeRects {
    let px = |v: i32| scale.px(v);
    let width = width.max(0);
    match caption {
        Caption::Custom => {
            let h = px(TITLEBAR_H);
            let strip = RectP::wh(0, 0, width, h);
            let btn_w = (3 * px(CAPTION_BTN_W)).min(width);
            let buttons = RectP::wh(width - btn_w, 0, btn_w, h);
            let beta = crate::names::STAGE_BETA.then(|| {
                let r = buttons.l - px(BETA_GAP);
                RectP::wh((r - px(BETA_W)).max(0), 0, px(BETA_W).min(r.max(0)), h)
            });
            let tabs_r = beta.map_or(buttons.l, |b| b.l - px(TABS_GAP)).max(0);
            ChromeRects { strip, tabs: RectP::wh(0, 0, tabs_r, h), beta, buttons: Some(buttons), below: h }
        }
        _ => {
            let row = RectP::wh(0, 0, width, px(TAB_ROW_H));
            ChromeRects { strip: row, tabs: row, beta: None, buttons: None, below: row.b }
        }
    }
}

/// Pure. What a point of the custom strip is: a caption button, a tab (or the "+" button: `on_tab`) is the client,
/// anything else of the strip is the caption. Client below the strip and everywhere when there is no strip.
pub fn hit_of(r: &ChromeRects, p: PointP, on_tab: bool) -> CaptionHit {
    let Some(b) = r.buttons else { return CaptionHit::Client };
    if !r.strip.contains(p) {
        return CaptionHit::Client;
    }
    if b.contains(p) {
        let w = (b.w() / 3).max(1);
        return match ((p.x - b.l) / w).min(2) {
            0 => CaptionHit::MinButton,
            1 => CaptionHit::MaxButton,
            _ => CaptionHit::CloseButton,
        };
    }
    if on_tab && r.tabs.contains(p) {
        CaptionHit::Client
    } else {
        CaptionHit::Caption
    }
}

/// The rect below the chrome.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    let caption = cx.effective_caption();
    let rc = rects(caption, r.w(), cx.scale());
    let lang = cx.lang();
    let (specs, selected) = (s.tabs.specs(lang), s.tabs.active().id.0 as u64);
    let sig = sig_of(&specs, selected);
    let me = &mut s.chrome;
    if sig != me.sig {
        let old = std::mem::replace(&mut me.strip, TabStrip::new(ID_STRIP));
        me.strip = old.with_tabs(specs, selected);
        me.sig = sig;
        me.stale = true;
    }
    me.caption = caption;
    me.rects = rc;
    let tabs = RectP { l: r.l + rc.tabs.l, t: r.t + rc.tabs.t, r: r.l + rc.tabs.r, b: r.t + rc.tabs.b };
    me.strip.layout(cx, tabs);
    if let Some(b) = rc.buttons {
        me.buttons.layout(cx, b.offset(r.l, r.t));
    }
    if let Some(b) = rc.beta {
        me.beta = TextLabel::new(ID_BETA, t(k::FILES_APP_BETA, lang), TypeRole::Caption, FgRole::Tertiary);
        let line = cx.scale().px(TypeRole::Caption.spec().line);
        let y = b.t + (b.h() - line) / 2;
        let w = me.beta.preferred(cx).w.min(b.w());
        me.beta.layout(cx, RectP { l: r.l + b.r - w, t: r.t + y, r: r.l + b.r, b: r.t + y + line });
    }
    RectP { t: r.t + rc.below, ..r }
}

fn sig_of(specs: &[oled_ui::widgets::TabSpec], selected: u64) -> (Vec<Sig>, u64) {
    let v = specs
        .iter()
        .map(|t| {
            (t.key, t.label.to_string(), t.tip.as_ref().map(|x| x.to_string()).unwrap_or_default(), t.busy, t.error)
        })
        .collect();
    (v, selected)
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let me = &s.chrome;
    me.strip.paint(cx);
    if me.rects.buttons.is_some() {
        me.buttons.paint(cx);
    }
    if me.rects.beta.is_some() {
        me.beta.paint(cx);
    }
}

/// Brings the strip in line with the tabs with an EventCx: its widths and its busy sweep (layout alone cannot start
/// them).
pub(crate) fn sync(s: &mut FilesScreen, cx: &mut EventCx) {
    let lang = s.shared.lang.get();
    let (specs, selected) = (s.tabs.specs(lang), s.tabs.active().id.0 as u64);
    let sig = sig_of(&specs, selected);
    let me = &mut s.chrome;
    if sig != me.sig || me.stale {
        me.strip.set_tabs(cx, specs, selected);
        me.sig = sig;
        me.stale = false;
    }
}

/// TabOut -> tab::on_out; PathOut -> pathline::on_out; the caption buttons' clicks (the host handles the real ones
/// through caption_hit; these arrive through assistive technology).
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    sync(s, cx);
    // Ctrl+PageUp and Ctrl+PageDown are the tab chords, but the list also reads them (move the focus without
    // selecting) and a key it takes never reaches the command table: the tabs take them first.
    if let Event::Key { key, down: true, mods, .. } = ev {
        let plain_ctrl = mods.ctrl() && !mods.shift() && !mods.alt() && !mods.win();
        if plain_ctrl && (*key == Key::PAGE_UP || *key == Key::PAGE_DOWN) {
            cx.consume();
            return Some(tab::cmd(s, cx, if *key == Key::PAGE_UP { Cmd::PrevTab } else { Cmd::NextTab }));
        }
    }
    if s.chrome.rects.buttons.is_some() {
        if let Some(CaptionOut::Close) = s.chrome.buttons.event(cx, ev) {
            return Some(s.event(cx, &Event::CloseRequested));
        }
    }
    if let Some(out) = s.chrome.strip.event(cx, ev) {
        let r = tab::on_out(s, cx, out);
        sync(s, cx);
        return Some(r);
    }
    pathline::event(s, cx, ev)
}

/// Custom caption only: the buttons, a tab or the "+" button is client, the rest of the strip is the caption.
pub fn caption_hit(s: &FilesScreen, p: PointP) -> CaptionHit {
    let me = &s.chrome;
    if me.caption != Caption::Custom {
        return CaptionHit::Client;
    }
    let on_tab = me.strip.caption_hit(p) == CaptionHit::Client;
    hit_of(&me.rects, p, on_tab)
}

/// A tab strip exists: always, since F1b (the location area is the breadcrumb, not the F1a label).
pub fn has_strip(s: &FilesScreen) -> bool {
    let _ = s;
    true
}
