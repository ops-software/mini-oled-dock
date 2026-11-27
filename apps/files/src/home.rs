//! Home (P3-files-app 2.2; FD-SIDE, F4.10): the pinned folders as tiles, the drives with their capacity, the network
//! places and the two hand-offs, in one scrollable page. One Tab stop per section, the arrows rove inside it.

use oled_trust::frame::files::Location;
use oled_ui::model::Align;
use oled_ui::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text};
use oled_ui::text::{LayoutOpts, Trim};
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{ScrollBar, ScrollOut};
use oled_ui::{Event, EventCx, Glyph, Key, LayoutCx, MouseButton, PaintCx, PointP, RectP, Reply, Widget, WidgetId};

use crate::drives::{DriveInfo, DriveKind};
use crate::handoff::{self, Target};
use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::pins;
use crate::sidebar::{self, drive_root, drive_title, low_space, place_name};
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen};

const ID_ITEMS: WidgetId = WidgetId(70);
const ID_SCROLL: WidgetId = WidgetId(71);

/// Logical px.
const TILE_W: i32 = 112;
const TILE_H: i32 = 88;
const GAP: i32 = 8;
const PAD: i32 = 16;
const HEADER_H: i32 = 32;
const ROW_H: i32 = 34;
const BAR_H: i32 = 4;
const SECTION_GAP: i32 = 12;
const ICON: i32 = 32;
const ICON_ROW: i32 = 16;
const ICON_LABEL: i32 = 36;
/// Fraction used above which the capacity bar turns to the attention colour, percent.
const BAR_ATTENTION: u64 = 90;

/// What an item opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Go {
    Folder(WidePath),
    Handoff(Target),
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Tile,
    Row,
}

/// One thing on the page.
#[derive(Clone, Debug)]
pub struct HItem {
    pub section: u8,
    pub shape: Shape,
    pub glyph: Glyph,
    pub label: String,
    /// The label's lines of a tile, with their widths (physical px).
    lines: Vec<(String, i32)>,
    pub detail: Option<String>,
    /// Used fraction in percent, for a drive.
    pub used_pct: Option<u64>,
    pub disabled: bool,
    pub go: Go,
    /// Physical px, relative to the page's origin at offset 0.
    rect: RectP,
}

/// The page state of a window (tests read it).
pub fn of(s: &FilesScreen) -> &Home {
    &s.home
}

pub struct Home {
    scroll: ScrollBar,
    items: Vec<HItem>,
    /// (section id, title, header rect) of the sections shown.
    headers: Vec<(u8, String, RectP)>,
    viewport: RectP,
    content_h: i64,
    offset: i64,
    /// Tiles per row of the last layout (the arrows step by it).
    cols: usize,
}

impl std::fmt::Debug for Home {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Home")
    }
}

impl Default for Home {
    fn default() -> Home {
        Home::new()
    }
}

impl Home {
    pub fn new() -> Home {
        Home {
            scroll: ScrollBar::new(ID_SCROLL),
            items: Vec::new(),
            headers: Vec::new(),
            viewport: RectP::EMPTY,
            content_h: 0,
            offset: 0,
            cols: 1,
        }
    }

    /// The items of the last layout, in order.
    pub fn items(&self) -> &[HItem] {
        &self.items
    }

    /// The section ids shown, in order (0 pinned, 1 drives, 2 network, 3 hand-offs).
    pub fn sections(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.items.iter().map(|i| i.section).collect();
        v.dedup();
        v
    }

    fn node(&self, i: usize) -> NodeId {
        NodeId::new(ID_ITEMS, i as u32 + 1)
    }

    fn index_of(&self, n: NodeId) -> Option<usize> {
        (n.owner() == ID_ITEMS && n.slot() >= 1).then(|| n.slot() as usize - 1).filter(|i| *i < self.items.len())
    }
}

/// The page's content, without positions. Pure.
pub fn content(s: &FilesScreen) -> Vec<HItem> {
    let shared = &s.shared;
    let lang = shared.lang.get();
    let fmt = shared.fmt.borrow().clone();
    let like = shared.opts.borrow().sizes_like_explorer;
    let mut out = Vec::new();
    let blank = |section: u8, shape: Shape, glyph: Glyph, label: String, go: Go| HItem {
        section,
        shape,
        glyph,
        label,
        lines: Vec::new(),
        detail: None,
        used_pct: None,
        disabled: false,
        go,
        rect: RectP::EMPTY,
    };
    for p in pins::paths(shared) {
        let mut it = blank(0, Shape::Tile, Glyph::Folder, place_name(&p), Go::Folder(p.clone()));
        it.disabled = s.side.is_missing(&p);
        out.push(it);
    }
    let drives = shared.drives.borrow();
    for d in drives.all().iter().filter(|d| d.kind != DriveKind::Network) {
        out.push(drive_item(d, 1, lang, &fmt, like));
    }
    for d in drives.all().iter().filter(|d| d.kind == DriveKind::Network) {
        out.push(drive_item(d, 2, lang, &fmt, like));
    }
    for p in sidebar::network_places(shared) {
        out.push(blank(2, Shape::Row, Glyph::Network, p.to_string_lossy(), Go::Folder(p)));
    }
    out.push(blank(
        3,
        Shape::Row,
        Glyph::Delete,
        t(k::FILES_SIDE_RECYCLE_BIN, lang).into_owned(),
        Go::Handoff(Target::RecycleBin),
    ));
    out.push(blank(
        3,
        Shape::Row,
        Glyph::Monitor,
        t(k::FILES_HOME_PHONES, lang).into_owned(),
        Go::Handoff(Target::ThisPc),
    ));
    out.sort_by_key(|i| i.section);
    out
}

fn drive_item(d: &DriveInfo, section: u8, lang: oled_ui::UiLang, fmt: &crate::format::Fmt, like: bool) -> HItem {
    let net = d.kind == DriveKind::Network;
    let detail = match (d.free, d.total, net || !d.ready) {
        (Some(f), Some(t), false) => Some(
            tf(k::FILES_SIDE_FREE_OF, lang, &[("free", &fmt.size(f, like)), ("total", &fmt.size(t, like))])
                .into_owned(),
        ),
        _ => None,
    };
    let used_pct = match (d.free, d.total) {
        (Some(f), Some(t)) if t > 0 && !net && d.ready => {
            Some((u128::from(t.saturating_sub(f)) * 100 / u128::from(t)) as u64)
        }
        _ => None,
    };
    HItem {
        section,
        shape: Shape::Row,
        glyph: if net { Glyph::Network } else { Glyph::Disk },
        label: drive_title(d, lang),
        lines: Vec::new(),
        detail,
        used_pct,
        disabled: !d.ready,
        go: drive_root(d.letter).map_or(Go::Nothing, Go::Folder),
        rect: RectP::EMPTY,
    }
}

fn section_title(id: u8, lang: oled_ui::UiLang) -> Text {
    match id {
        0 => t(k::FILES_HOME_PINNED, lang),
        1 => t(k::FILES_HOME_DRIVES, lang),
        2 => t(k::FILES_HOME_NETWORK, lang),
        _ => Text::Borrowed(""),
    }
}

/// Lays the page out: tiles in rows that wrap, rows full width, from the viewport's top at the scroll offset.
pub fn home_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let lang = s.shared.lang.get();
    let mut items = content(s);
    let sc = cx.scale();
    let px = move |v: i32| sc.px(v);
    let (pad, gap, tile_w, tile_h, row_h) = (px(PAD), px(GAP), px(TILE_W), px(TILE_H), px(ROW_H));
    let width = (r.w() - 2 * pad).max(tile_w);
    let cols = ((width + gap) / (tile_w + gap)).max(1);
    let mut headers = Vec::new();
    let mut y = pad;
    let mut col = 0;
    let mut last_section = u8::MAX;
    let font = cx.font(TypeRole::Caption);
    for it in items.iter_mut() {
        if it.section != last_section {
            if last_section != u8::MAX {
                y += if col > 0 { tile_h + gap } else { 0 } + px(SECTION_GAP);
            }
            col = 0;
            last_section = it.section;
            if it.section < 3 {
                headers.push((
                    it.section,
                    section_title(it.section, lang).into_owned(),
                    RectP::wh(r.l + pad, r.t + y, width, px(HEADER_H)),
                ));
                y += px(HEADER_H);
            }
        }
        match it.shape {
            Shape::Tile => {
                let x = r.l + pad + col * (tile_w + gap);
                it.rect = RectP::wh(x, r.t + y, tile_w, tile_h);
                let room = tile_w - 2 * px(6);
                let opts = LayoutOpts { max_width: Some(room), trim: Trim::End, wrap: true };
                let l = cx.text().layout(&it.label, font, &opts);
                let mut lines = Vec::new();
                for (n, (range, _)) in l.lines.iter().take(2).enumerate() {
                    let text =
                        if n == 1 { l.text.get(range.start..) } else { l.text.get(range.clone()) }.unwrap_or_default();
                    let one = LayoutOpts { max_width: Some(room), trim: Trim::End, wrap: false };
                    let w = cx.text().layout(text.trim_end(), font, &one).width;
                    lines.push((text.trim_end().to_string(), w));
                }
                if lines.is_empty() {
                    lines.push((it.label.clone(), 0));
                }
                it.lines = lines;
                col += 1;
                if col >= cols {
                    col = 0;
                    y += tile_h + gap;
                }
            }
            Shape::Row => {
                if col > 0 {
                    col = 0;
                    y += tile_h + gap;
                }
                it.rect = RectP::wh(r.l + pad, r.t + y, width, row_h);
                y += row_h + px(2);
            }
        }
    }
    if col > 0 {
        y += tile_h;
    }
    let content_h = i64::from(y + pad);
    let h = &mut s.home;
    h.content_h = content_h;
    h.viewport = r;
    h.cols = cols as usize;
    h.offset = h.offset.clamp(0, (content_h - i64::from(r.h())).max(0));
    h.scroll.set(r, content_h, h.offset);
    let off = h.offset as i32;
    for it in items.iter_mut() {
        it.rect = RectP { t: it.rect.t - off, b: it.rect.b - off, ..it.rect };
    }
    for hd in headers.iter_mut() {
        hd.2 = RectP { t: hd.2.t - off, b: hd.2.b - off, ..hd.2 };
    }
    // One Tab stop per section: its first enabled item; the rest are reached with the arrows.
    for (i, it) in items.iter().enumerate() {
        let vis = RectP { l: it.rect.l.max(r.l), t: it.rect.t.max(r.t), r: it.rect.r.min(r.r), b: it.rect.b.min(r.b) };
        if vis.w() <= 0 || vis.h() <= 0 {
            continue;
        }
        let first = items.iter().position(|o| o.section == it.section && !o.disabled) == Some(i);
        let mut flags = NodeFlags::HOVER_FADE;
        if it.disabled {
            flags = flags | NodeFlags::DISABLED;
        }
        let node = Node::new(NodeId::new(ID_ITEMS, i as u32 + 1), Role::ListItem, vis)
            .flags(flags)
            .focus(if it.disabled {
                Focusable::No
            } else if first {
                Focusable::Tab
            } else {
                Focusable::Roving
            })
            .label(Text::Owned(it.label.clone()))
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }
    h.items = items;
    h.headers = headers;
    h.scroll.layout(cx, r);
}

pub fn home_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let h = &s.home;
    let c = cx.tokens().color.clone();
    let radius = cx.px(cx.tokens().radius.s);
    let view = h.viewport;
    cx.with_clip(view, &mut |cx| {
        for (_, title, rect) in &h.headers {
            if cx.visible(*rect) {
                cx.text_in(title, TypeRole::Section, *rect, Align::Start, Trim::End, c.fg_secondary);
            }
        }
        for (i, it) in h.items.iter().enumerate() {
            if !cx.visible(it.rect) {
                continue;
            }
            let id = h.node(i);
            let ink = if it.disabled { c.fg_disabled } else { c.fg_secondary };
            if !it.disabled {
                let t = cx.hover_t(id);
                if cx.pressed(id) {
                    cx.fill(it.rect, radius, c.fill_press);
                } else if t > 0.0 {
                    let a = (f32::from(c.fill_hover.a as u16) * t) as u32;
                    cx.fill(it.rect, radius, oled_ui::Ink { a, ..c.fill_hover });
                }
            }
            match it.shape {
                Shape::Tile => {
                    let icon = cx.px(ICON);
                    let gx = it.rect.l + (it.rect.w() - icon) / 2;
                    cx.glyph(RectP::wh(gx, it.rect.t + cx.px(8), icon, icon), it.glyph, ink);
                    let line_h = cx.px(16);
                    for (n, (text, w)) in it.lines.iter().enumerate() {
                        let room = RectP {
                            l: it.rect.l + cx.px(6),
                            r: it.rect.r - cx.px(6),
                            t: it.rect.t + cx.px(46) + n as i32 * line_h,
                            b: it.rect.t + cx.px(46) + (n as i32 + 1) * line_h,
                        };
                        let x = it.rect.l + (it.rect.w() - (*w).min(room.w())) / 2;
                        cx.text_in(
                            text,
                            TypeRole::Caption,
                            RectP { l: x, ..room },
                            Align::Start,
                            Trim::End,
                            if it.disabled { c.fg_disabled } else { c.fg_primary },
                        );
                    }
                }
                Shape::Row => {
                    let icon = cx.px(ICON_ROW);
                    cx.glyph(
                        RectP::wh(
                            it.rect.l + cx.px(10),
                            it.rect.t + (it.rect.h() - icon) / 2 - cx.px(if it.used_pct.is_some() { 2 } else { 0 }),
                            icon,
                            icon,
                        ),
                        it.glyph,
                        ink,
                    );
                    let text_r = RectP {
                        l: it.rect.l + cx.px(ICON_LABEL),
                        r: it.rect.r - cx.px(10),
                        b: it.rect.t + cx.px(if it.used_pct.is_some() { 26 } else { 34 }),
                        ..it.rect
                    };
                    let w = it
                        .detail
                        .as_ref()
                        .map_or(0, |d| cx.text_in(d, TypeRole::Caption, text_r, Align::End, Trim::End, c.fg_secondary));
                    let label_r = RectP { r: text_r.r - w - if w > 0 { cx.px(12) } else { 0 }, ..text_r };
                    cx.text_in(
                        &it.label,
                        TypeRole::Body,
                        label_r,
                        Align::Start,
                        Trim::End,
                        if it.disabled { c.fg_disabled } else { c.fg_primary },
                    );
                    if let Some(pct) = it.used_pct {
                        let bar = RectP {
                            l: text_r.l,
                            r: text_r.r,
                            t: it.rect.b - cx.px(BAR_H) - cx.px(4),
                            b: it.rect.b - cx.px(4),
                        };
                        let hc = cx.tokens().high_contrast;
                        if hc {
                            // Rail and fill are the same white in high contrast: the rail is an outline instead.
                            cx.stroke(bar, cx.px(2), cx.px(1).max(1), c.fg_secondary);
                        } else {
                            cx.fill(bar, cx.px(2), c.slider_rail);
                        }
                        let fill = RectP { r: bar.l + bar.w() * pct.min(100) as i32 / 100, ..bar };
                        let ink = if pct > BAR_ATTENTION {
                            c.accent_attention
                        } else {
                            oled_ui::Ink { a: 230, ..c.fg_primary }
                        };
                        cx.fill(fill, cx.px(2), ink);
                    }
                }
            }
            if cx.focused(id) {
                cx.focus_ring(it.rect, radius, false);
            }
        }
    });
    h.scroll.paint(cx);
}

fn activate(s: &mut FilesScreen, cx: &mut EventCx, i: usize) -> Reply {
    let Some(it) = s.home.items.get(i).cloned() else { return Reply::None };
    if it.disabled {
        return Reply::None;
    }
    match it.go {
        Go::Folder(p) => {
            window::navigate(s, cx, Location::Fs(p), None);
            Reply::Redraw
        }
        Go::Handoff(t) => handoff::hand_off(s, cx, t, false),
        Go::Nothing => Reply::None,
    }
}

/// The item the arrow lands on, inside the section of 'i'.
pub fn step(items: &[HItem], i: usize, key: Key, cols: usize) -> Option<usize> {
    let it = items.get(i)?;
    let same = |j: usize| items.get(j).filter(|o| o.section == it.section && !o.disabled).map(|_| j);
    let nearest = |from: usize, d: isize| {
        let mut j = from as isize + d;
        while j >= 0 && (j as usize) < items.len() && items[j as usize].section == it.section {
            if !items[j as usize].disabled {
                return Some(j as usize);
            }
            j += d;
        }
        None
    };
    let in_section = |f: &dyn Fn(&HItem) -> bool| items.iter().position(|o| o.section == it.section && f(o));
    match key {
        Key::LEFT if it.shape == Shape::Tile => nearest(i, -1),
        Key::RIGHT if it.shape == Shape::Tile => nearest(i, 1),
        Key::UP if it.shape == Shape::Row => nearest(i, -1),
        Key::DOWN if it.shape == Shape::Row => nearest(i, 1),
        Key::UP => i.checked_sub(cols.max(1)).and_then(same),
        Key::DOWN => same(i + cols.max(1)),
        Key::HOME => in_section(&|o| !o.disabled),
        Key::END => items.iter().rposition(|o| o.section == it.section && !o.disabled),
        _ => None,
    }
}

pub fn home_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if let Some(ScrollOut::Scrolled(off)) = s.home.scroll.event(cx, ev) {
        s.home.offset = off;
        return Some(Reply::Relayout);
    }
    match ev {
        Event::Wheel { pos, delta, horizontal: false, .. } if s.home.viewport.contains(*pos) => {
            let px = match delta {
                oled_ui::WheelDelta::Lines(l) => (-l * cx.px(ROW_H) as f32).round() as i64,
                oled_ui::WheelDelta::Pixels(p) => -p.round() as i64,
            };
            if let Some(ScrollOut::Scrolled(off)) = s.home.scroll.scroll_by(cx, px) {
                s.home.offset = off;
                return Some(Reply::Relayout);
            }
            None
        }
        Event::Click { button: MouseButton::Left, .. } => {
            let i = cx.target().and_then(|n| s.home.index_of(n))?;
            Some(activate(s, cx, i))
        }
        Event::A11y(n, oled_ui::A11yAction::Click) => {
            let i = s.home.index_of(*n)?;
            Some(activate(s, cx, i))
        }
        Event::Key { key, down: true, .. } => {
            let i = cx.target().and_then(|n| s.home.index_of(n))?;
            if *key == Key::ENTER {
                cx.consume();
                return Some(activate(s, cx, i));
            }
            let to = step(&s.home.items, i, *key, s.home.cols)?;
            cx.consume();
            cx.set_focus(s.home.node(to));
            // An item outside the viewport has no node yet: scroll it into view first.
            let (rect, view) = (s.home.items[to].rect, s.home.viewport);
            let shift = if rect.b > view.b {
                i64::from(rect.b - view.b)
            } else if rect.t < view.t {
                -i64::from(view.t - rect.t)
            } else {
                0
            };
            if shift != 0 {
                s.home.offset += shift;
                return Some(Reply::Relayout);
            }
            Some(Reply::Redraw)
        }
        _ => None,
    }
}

/// Alt+Home: the Home page.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    if c == Cmd::Home {
        window::navigate(s, cx, Location::Home, None);
        return Reply::Redraw;
    }
    Reply::None
}

/// Whether a drive's bar is in the attention colour.
pub fn bar_attention(pct: u64) -> bool {
    pct > BAR_ATTENTION
}

/// Whether the drive row of the sidebar and the bar agree on low space (kept together for the tests).
pub fn drive_is_low(d: &DriveInfo) -> bool {
    low_space(d)
}

/// The point of a mouse position inside the page (tests).
pub fn contains(s: &FilesScreen, p: PointP) -> bool {
    s.home.viewport.contains(p)
}
