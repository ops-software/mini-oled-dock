//! TabStrip (F-K3, P2-kit 2.10; K3.1): the window's tabs. Plates TAB_H high, centred in the rect the screen gives (y 4 of a
//! TITLEBAR_H strip, or of a TAB_ROW_H row under a system caption), widths shrinking evenly between TAB_W_MIN and
//! TAB_W_MAX, a "+" button after the last tab. One Tab stop (the selected tab) with roving Left/Right/Home/End; the
//! app's chords (Ctrl+Tab, Ctrl+PgDn, Ctrl+1..9) call select_next and select_index.

use std::rc::Rc;

use super::paint::lerp_ink;
use crate::cx::{AnimId, Ease, EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton, TimerId, WheelDelta};
use crate::geom::{PointP, RectP};
use crate::glyph::Glyph;
use crate::model::Align;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::{CaptionHit, Widget};
use crate::strings::k;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};
use crate::w3::IconBitmap;

/// One tab.
pub struct TabSpec {
    pub key: u64,
    pub label: Text,
    pub tip: Option<Text>,
    pub icon: Option<Rc<IconBitmap>>,
    pub busy: bool,
    pub error: bool,
}

/// What a tab strip tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TabOut {
    Selected(u64),
    CloseRequested(u64),
    NewRequested,
    Moved { key: u64, to: usize },
    ContextMenu(u64, PointP),
}

/// Space at both ends of the strip, between two plates, the plate's padding, the icon-to-label gap, the close plate and
/// its distance from the plate's end, the "+" button, the wheel step (logical px).
const MARGIN: i32 = 4;
const GAP: i32 = 2;
const PAD: i32 = 10;
const ICON_GAP: i32 = 8;
const CLOSE_PLATE: i32 = 24;
const CLOSE_GLYPH: i32 = 12;
const CLOSE_INSET: i32 = 4;
const PLUS: i32 = 32;
const WHEEL_STEP: i32 = 60;
const SWEEP_H: i32 = 2;

/// Node slots: the group, the tabs from 1, the close plates from CLOSE_BASE, the "+" button.
const CLOSE_BASE: u32 = 0x1_0000;
const PLUS_SLOT: u32 = 0x2_0000;
/// Animation channels of the group node and its timer slot.
const CH_SWEEP: u8 = 0;
const CH_WIDTH: u8 = 1;
const SLOT_RENEW: u8 = 0;
/// A busy sweep is one long linear tween of this many periods, renewed by a timer when it ends.
const SWEEP_PERIODS: u32 = 1500;

/// The widths of n tabs sharing `avail` (the gaps included): equal to the pixel, within min..=max.
pub(crate) fn tab_widths(n: usize, avail: i32, gap: i32, min: i32, max: i32) -> Vec<i32> {
    if n == 0 {
        return Vec::new();
    }
    let count = n as i32;
    let room = (avail - gap * (count - 1)).max(0);
    let each = room / count;
    let mut v = vec![each.clamp(min, max); n];
    if each >= min && each < max {
        // The remainder goes one pixel at a time to the first tabs: the plates differ by at most 1.
        for w in v.iter_mut().take((room - each * count).max(0) as usize) {
            *w += 1;
        }
    }
    v
}

/// What a node of the strip is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Group,
    Tab(usize),
    Close(usize),
    Plus,
}

/// A tab being dragged: where it is, the pointer's grip on it and where it would land.
#[derive(Clone, Copy, Debug)]
struct Drag {
    from: usize,
    grip: i32,
    x: i32,
    to: usize,
}

/// The window's tab strip (widths shrink evenly within TAB_W_MIN..TAB_W_MAX).
pub struct TabStrip {
    id: WidgetId,
    tabs: Vec<TabSpec>,
    selected: u64,
    /// The rect of the last layout, the part of it that shows tabs, the plates (scroll applied, unclipped), their
    /// target widths, the "+" button and where the first plate starts (physical px).
    strip: RectP,
    view: RectP,
    rects: Vec<RectP>,
    widths: Vec<i32>,
    plus: RectP,
    x0: i32,
    gap: i32,
    /// How far the plates are scrolled to the left, and whether the next layout brings the selected one into view.
    scroll: i32,
    max_scroll: i32,
    reveal: bool,
    /// The widths the plates had before the last change of the tab set, by key (they animate to the new ones).
    prev: Vec<(u64, i32)>,
    drag: Option<Drag>,
    sweeping: bool,
}

impl TabStrip {
    pub fn new(id: WidgetId) -> TabStrip {
        TabStrip {
            id,
            tabs: Vec::new(),
            selected: 0,
            strip: RectP::EMPTY,
            view: RectP::EMPTY,
            rects: Vec::new(),
            widths: Vec::new(),
            plus: RectP::EMPTY,
            x0: 0,
            gap: 0,
            scroll: 0,
            max_scroll: 0,
            reveal: true,
            prev: Vec::new(),
            drag: None,
            sweeping: false,
        }
    }

    /// The strip with its tabs from the start (a screen builds its widgets before it has an EventCx).
    pub fn with_tabs(mut self, tabs: Vec<TabSpec>, selected: u64) -> TabStrip {
        self.tabs = tabs;
        self.selected = selected;
        self
    }

    pub fn set_tabs(&mut self, cx: &mut EventCx, tabs: Vec<TabSpec>, selected: u64) {
        let same = self.tabs.len() == tabs.len() && self.tabs.iter().zip(&tabs).all(|(a, b)| a.key == b.key);
        if !same {
            self.prev = self.tabs.iter().zip(&self.widths).map(|(t, w)| (t.key, *w)).collect();
            let node = self.group();
            let (to, dur) = (AnimId { node, channel: CH_WIDTH }, cx.tokens().motion.dur_slow);
            cx.animate(to, 0.0, 0, Ease::Linear);
            cx.animate(to, 1.0, dur, Ease::OutCubic);
        }
        if selected != self.selected {
            self.reveal = true;
        }
        self.tabs = tabs;
        self.selected = selected;
        self.drag = None;
        self.sync_sweep(cx);
        cx.relayout();
    }

    /// The selected tab's key.
    pub fn selected(&self) -> u64 {
        self.selected
    }

    /// Empty strip space is Caption (WM_NCHITTEST); a tab or the "+" button is Client.
    pub fn caption_hit(&self, p: PointP) -> CaptionHit {
        let on_tab = self.view.contains(p) && self.rects.iter().any(|r| r.contains(p));
        if !self.strip.contains(p) || on_tab || self.plus.contains(p) {
            CaptionHit::Client
        } else {
            CaptionHit::Caption
        }
    }

    /// The next tab (back: the previous one), wrapping; the key now selected. The strip repaints; no TabOut is emitted
    /// for a change the app made itself.
    pub fn select_next(&mut self, cx: &mut EventCx, back: bool) -> Option<u64> {
        let n = self.tabs.len();
        if n == 0 {
            return None;
        }
        let at = self.index_of(self.selected);
        let to = match (at, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        Some(self.pick(cx, to))
    }

    /// Tab i (Ctrl+1..8 = 0..7, None past the count; usize::MAX = the last tab, Ctrl+9).
    pub fn select_index(&mut self, cx: &mut EventCx, i: usize) -> Option<u64> {
        let n = self.tabs.len();
        let to = if i == usize::MAX { n.checked_sub(1)? } else { i };
        if to >= n {
            return None;
        }
        Some(self.pick(cx, to))
    }

    // ---- state

    fn group(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn tab_node(&self, i: usize) -> NodeId {
        NodeId::new(self.id, 1 + i as u32)
    }

    fn close_node(&self, i: usize) -> NodeId {
        NodeId::new(self.id, CLOSE_BASE + i as u32)
    }

    fn plus_node(&self) -> NodeId {
        NodeId::new(self.id, PLUS_SLOT)
    }

    fn part(&self, n: NodeId) -> Option<Part> {
        if n.owner() != self.id {
            return None;
        }
        let count = self.tabs.len() as u32;
        match n.slot() {
            0 => Some(Part::Group),
            PLUS_SLOT => Some(Part::Plus),
            s if s >= CLOSE_BASE && s - CLOSE_BASE < count => Some(Part::Close((s - CLOSE_BASE) as usize)),
            s if s >= 1 && s - 1 < count => Some(Part::Tab((s - 1) as usize)),
            _ => None,
        }
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.key == key)
    }

    /// Makes tab i the selected one; the key.
    fn pick(&mut self, cx: &mut EventCx, i: usize) -> u64 {
        let key = self.tabs[i].key;
        if key != self.selected {
            self.selected = key;
            self.reveal = true;
            cx.relayout();
        }
        key
    }

    /// Selection by the user: Some(Selected) when it changed.
    fn choose(&mut self, cx: &mut EventCx, i: usize) -> Option<TabOut> {
        let changed = self.tabs[i].key != self.selected;
        let key = self.pick(cx, i);
        changed.then_some(TabOut::Selected(key))
    }

    /// The sweeps run while a tab is busy: one long linear tween on the group node keeps the frame clock armed.
    fn sync_sweep(&mut self, cx: &mut EventCx) {
        let want = self.tabs.iter().any(|t| t.busy) && !cx.tokens().motion.reduced;
        let to = AnimId { node: self.group(), channel: CH_SWEEP };
        let timer = TimerId::kit(self.id, SLOT_RENEW);
        if want {
            let period = cx.tokens().motion.sweep_period.max(1);
            if !self.sweeping {
                self.sweeping = true;
                cx.animate(to, 0.0, 0, Ease::Linear);
                cx.animate(to, SWEEP_PERIODS as f32, SWEEP_PERIODS * period, Ease::Linear);
                cx.set_timer(timer, SWEEP_PERIODS * period);
            }
        } else if self.sweeping {
            self.sweeping = false;
            cx.animate(to, 0.0, 0, Ease::Linear);
            cx.cancel_timer(timer);
        }
    }

    fn on_timer(&mut self, cx: &mut EventCx, t: TimerId) {
        if t == TimerId::kit(self.id, SLOT_RENEW) && self.sweeping {
            self.sweeping = false;
            self.sync_sweep(cx);
        }
    }

    /// Where the dragged tab would land for the pointer at x.
    fn drop_index(&self, d: &Drag) -> usize {
        let w = self.rects.get(d.from).map_or(0, |r| r.w());
        let centre = d.x - d.grip + w / 2;
        let before = self.rects.iter().enumerate().filter(|(i, r)| *i != d.from && (r.l + r.r) / 2 <= centre).count();
        before.min(self.tabs.len().saturating_sub(1))
    }

    /// The plates as painted: the final ones, or their widths part-way to them while a change of the tab set animates,
    /// in the order a drag would leave them.
    fn display_rects(&self, t: f32) -> Vec<RectP> {
        let n = self.tabs.len();
        let mut order: Vec<usize> = (0..n).collect();
        if let Some(d) = self.drag.filter(|d| d.from < n) {
            order.remove(d.from);
            order.insert(d.to.min(order.len()), d.from);
        }
        let mut out = self.rects.clone();
        let mut x = self.x0;
        for i in order {
            let target = self.widths.get(i).copied().unwrap_or(0);
            let w = match self.prev.iter().find(|p| p.0 == self.tabs[i].key) {
                Some(&(_, from)) if t < 1.0 => from + ((target - from) as f32 * t).round() as i32,
                _ => target,
            };
            out[i] = RectP { l: x, r: x + w, ..self.rects[i] };
            x += w + self.gap;
        }
        out
    }

    /// The close plate of a plate.
    fn close_rect(plate: RectP, px: &dyn Fn(i32) -> i32) -> RectP {
        let s = px(CLOSE_PLATE);
        RectP::wh(plate.r - px(CLOSE_INSET) - s, plate.t + (plate.h() - s) / 2, s, s)
    }

    /// Roving focus: the tabs, then the "+" button (index = tab count).
    fn rove(&mut self, cx: &mut EventCx, from: usize, key: Key) -> bool {
        let n = self.tabs.len();
        let to = match key {
            Key::LEFT => from.saturating_sub(1),
            Key::RIGHT => (from + 1).min(n),
            Key::HOME => 0,
            Key::END => n.saturating_sub(1),
            _ => return false,
        };
        let node = if to >= n { self.plus_node() } else { self.tab_node(to) };
        cx.consume();
        cx.set_focus(node);
        true
    }
}

impl Widget for TabStrip {
    type Out = TabOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let sz = cx.tokens().size.clone();
        let (margin, gap, plus_w) = (cx.px(MARGIN), cx.px(GAP), cx.px(PLUS));
        let th = cx.px(sz.tab_h);
        let y = rect.t + (rect.h() - th) / 2;
        let left = rect.l + margin;
        let n = self.tabs.len();
        let room = (rect.w() - 2 * margin - plus_w - gap).max(0);
        self.widths = tab_widths(n, room, gap, cx.px(sz.tab_w_min), cx.px(sz.tab_w_max));
        let content = self.widths.iter().sum::<i32>() + gap * (n as i32 - 1).max(0);
        let overflow = content > room;
        let view_w = if overflow { room } else { content };
        self.strip = rect;
        self.gap = gap;
        self.view = RectP::wh(left, rect.t, view_w, rect.h());
        let max_scroll = (content - view_w).max(0);
        self.max_scroll = max_scroll;
        self.scroll = self.scroll.clamp(0, max_scroll);
        self.x0 = left - self.scroll;
        self.rects.clear();
        let mut x = left;
        for w in &self.widths {
            self.rects.push(RectP::wh(x, y, *w, th));
            x += w + gap;
        }
        if self.reveal {
            self.reveal = false;
            if let Some(r) = self.index_of(self.selected).map(|i| self.rects[i]) {
                if r.r - self.scroll > self.view.r {
                    self.scroll = r.r - self.view.r;
                }
                if r.l - self.scroll < self.view.l {
                    self.scroll = r.l - self.view.l;
                }
                self.scroll = self.scroll.clamp(0, max_scroll);
                self.x0 = left - self.scroll;
            }
        }
        for r in &mut self.rects {
            *r = r.offset(-self.scroll, 0);
        }
        let plus_x = if overflow { rect.r - margin - plus_w } else { left + content + gap };
        self.plus = RectP::wh(plus_x, y + (th - plus_w) / 2, plus_w, plus_w);

        let lang = cx.lang();
        let group = self.group();
        cx.push(Node::new(group, Role::TabList, rect));
        for (i, (t, r)) in self.tabs.iter().zip(&self.rects).enumerate() {
            let hit = r.intersect(self.view);
            if hit.is_empty() {
                continue;
            }
            let selected = t.key == self.selected;
            let mut flags = NodeFlags::HOVER_FADE;
            if selected {
                flags = flags | NodeFlags::SELECTED;
            }
            if t.busy {
                flags = flags | NodeFlags::BUSY;
            }
            if t.error {
                flags = flags | NodeFlags::INVALID;
            }
            let mut node = Node::new(self.tab_node(i), Role::Tab, hit)
                .parent(group)
                .flags(flags)
                .focus(if selected { Focusable::Tab } else { Focusable::Roving })
                .label(t.label.clone())
                .actions(Actions::CLICK | Actions::FOCUS | Actions::CLOSE | Actions::CONTEXT_MENU);
            if let Some(tip) = &t.tip {
                node = node.description(tip.clone());
            }
            cx.push(node);
            let close = Self::close_rect(*r, &|v| cx.px(v)).intersect(self.view);
            if !close.is_empty() {
                let name = lang.kit(k::KIT_CLOSE_TAB);
                cx.push(
                    Node::new(self.close_node(i), Role::Button, close)
                        .parent(self.tab_node(i))
                        .flags(NodeFlags::HOVER_FADE)
                        .label(name.clone())
                        .description(name)
                        .actions(Actions::CLICK),
                );
            }
        }
        let name = lang.kit(k::KIT_NEW_TAB);
        cx.push(
            Node::new(self.plus_node(), Role::Button, self.plus)
                .parent(group)
                .flags(NodeFlags::HOVER_FADE)
                .focus(Focusable::Roving)
                .label(name.clone())
                .description(name)
                .actions(Actions::CLICK | Actions::FOCUS),
        );
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        let reduced = cx.tokens().motion.reduced;
        let group = self.group();
        let grown = cx.anim(AnimId { node: group, channel: CH_WIDTH }).unwrap_or(1.0);
        let rects = self.display_rects(grown);
        // A still frame (no tween ever ran) shows the segment part-way.
        let sweep = cx.anim(AnimId { node: group, channel: CH_SWEEP }).unwrap_or(0.4);
        let phase = sweep - sweep.floor();
        let dragged = self.drag.map(|d| d.from);
        let one = cx.px(1).max(1);

        let plate = |cx: &mut PaintCx, i: usize, r: RectP| {
            let t = &self.tabs[i];
            let (id, close) = (self.tab_node(i), self.close_node(i));
            let hover = cx.hover_t(id).max(cx.hover_t(close));
            let active = t.key == self.selected;
            if cx.pressed(id) || dragged == Some(i) {
                cx.fill(r, radius, c.fill_press);
            } else if active {
                cx.fill(r, radius, lerp_ink(c.fill_control, c.fill_control_hot, hover));
            } else {
                cx.fill(r, radius, lerp_ink(Ink { a: 0, ..c.fill_hover }, c.fill_hover, hover));
            }
            if active {
                cx.stroke(r, radius, one, c.rim_control);
            }
            // The icon (or the Error glyph), then the label up to the close plate.
            let (pad, icon) = (cx.px(PAD), cx.px(16));
            let mut x = r.l + pad;
            let slot = RectP::wh(x, r.t + (r.h() - icon) / 2, icon, icon);
            if t.error {
                cx.glyph(slot, Glyph::Error, c.accent_danger);
                x += icon + cx.px(ICON_GAP);
            } else if let Some(img) = &t.icon {
                cx.image(img, PointP { x: slot.l, y: slot.t }, icon, 255);
                x += icon + cx.px(ICON_GAP);
            }
            let close_r = Self::close_rect(r, &|v| cx.px(v));
            let label = RectP { l: x, r: close_r.l - cx.px(2), ..r };
            let ink = if active { c.fg_primary } else { lerp_ink(c.fg_secondary, c.fg_secondary_hot, hover) };
            if label.w() > 0 {
                cx.text_in(&t.label, TypeRole::Body, label, Align::Start, Trim::End, ink);
            }
            // The close plate: on the active tab and on hover.
            let show = if active { 1.0 } else { hover };
            if show > 0.0 {
                let ch = cx.hover_t(close);
                if cx.pressed(close) {
                    cx.fill(close_r, cx.px(cx.tokens().radius.xs), c.fill_press);
                } else if ch > 0.0 {
                    cx.fill(
                        close_r,
                        cx.px(cx.tokens().radius.xs),
                        lerp_ink(Ink { a: 0, ..c.fill_hover }, c.fill_hover, ch),
                    );
                }
                let g = cx.px(CLOSE_GLYPH);
                let gr = RectP::wh(close_r.l + (close_r.w() - g) / 2, close_r.t + (close_r.h() - g) / 2, g, g);
                let ink = lerp_ink(c.fg_secondary, c.fg_secondary_hot, ch);
                cx.glyph(gr, Glyph::Close, Ink { a: (ink.a as f32 * show).round() as u32, ..ink });
            }
            // Busy: a 2 px line under the plate, a quarter of its width sweeping over SWEEP_PERIOD, or the whole line
            // when motion is reduced.
            if t.busy {
                let h = cx.px(SWEEP_H);
                let y = r.b + one;
                let (l, rr) = if reduced {
                    (r.l, r.r)
                } else {
                    let seg = r.w() / 4;
                    let at = r.l - seg + (phase * (r.w() + seg) as f32).round() as i32;
                    (at.max(r.l), (at + seg).min(r.r))
                };
                if rr > l {
                    let ink = if reduced { c.fg_tertiary } else { c.fg_primary };
                    cx.fill(RectP { l, t: y, r: rr, b: y + h }, one, ink);
                }
            }
            if cx.focused(id) {
                cx.focus_ring(r, radius, false);
            }
        };

        let view = self.view;
        cx.with_clip(view, &mut |cx| {
            for (i, r) in rects.iter().enumerate() {
                if dragged != Some(i) && cx.visible(*r) {
                    plate(cx, i, *r);
                }
            }
        });
        if let Some(d) = self.drag.filter(|d| d.from < rects.len()) {
            let r = rects[d.from];
            let l = (d.x - d.grip).clamp(view.l, (view.r - r.w()).max(view.l));
            plate(cx, d.from, RectP { l, r: l + r.w(), ..r });
        }

        // The "+" button.
        let id = self.plus_node();
        let hover = cx.hover_t(id);
        let pr = self.plus;
        if cx.pressed(id) {
            cx.fill(pr, radius, c.fill_press);
        } else {
            cx.fill(pr, radius, lerp_ink(Ink { a: 0, ..c.fill_hover }, c.fill_hover, hover));
        }
        let g = cx.px(16);
        let gr = RectP::wh(pr.l + (pr.w() - g) / 2, pr.t + (pr.h() - g) / 2, g, g);
        cx.glyph(gr, Glyph::Plus, lerp_ink(c.fg_secondary, c.fg_secondary_hot, hover));
        if cx.focused(id) {
            cx.focus_ring(pr, radius, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<TabOut> {
        if let Event::Timer(t) = ev {
            self.on_timer(cx, *t);
            return None;
        }
        let part = self.part(cx.target()?)?;
        match (ev, part) {
            (Event::Wheel { delta, .. }, _) => {
                let step = match *delta {
                    WheelDelta::Lines(l) => (l * cx.px(WHEEL_STEP) as f32).round() as i32,
                    WheelDelta::Pixels(p) => p.round() as i32,
                };
                let to = (self.scroll - step).clamp(0, self.max_scroll);
                if to != self.scroll {
                    self.scroll = to;
                    cx.relayout();
                }
                None
            }
            (Event::PointerDown { button: MouseButton::Left, .. }, Part::Tab(i)) => self.choose(cx, i),
            (Event::Click { button: MouseButton::Left, .. }, Part::Tab(i)) => self.choose(cx, i),
            (Event::Click { button: MouseButton::Middle, .. }, Part::Tab(i) | Part::Close(i)) => {
                Some(TabOut::CloseRequested(self.tabs[i].key))
            }
            (Event::Click { button: MouseButton::Left, .. }, Part::Close(i)) => {
                Some(TabOut::CloseRequested(self.tabs[i].key))
            }
            (Event::Click { button: MouseButton::Right, pos, .. }, Part::Tab(i)) => {
                Some(TabOut::ContextMenu(self.tabs[i].key, *pos))
            }
            (Event::Click { button: MouseButton::Left, .. }, Part::Plus) => Some(TabOut::NewRequested),
            (Event::A11y(_, A11yAction::Click), Part::Tab(i)) => self.choose(cx, i),
            (Event::A11y(_, A11yAction::Click), Part::Close(i)) => Some(TabOut::CloseRequested(self.tabs[i].key)),
            (Event::A11y(_, A11yAction::Click), Part::Plus) => Some(TabOut::NewRequested),
            (Event::DragStart { origin, button: MouseButton::Left, .. }, Part::Tab(i)) => {
                let grip = origin.x - self.rects[i].l;
                self.drag = Some(Drag { from: i, grip, x: origin.x, to: i });
                cx.redraw();
                None
            }
            (Event::DragMove { pos, .. }, Part::Tab(_)) => {
                if let Some(mut d) = self.drag {
                    d.x = pos.x;
                    d.to = self.drop_index(&d);
                    self.drag = Some(d);
                    cx.damage(self.strip);
                }
                None
            }
            (Event::DragEnd { cancelled, .. }, Part::Tab(_)) => {
                let d = self.drag.take()?;
                cx.damage(self.strip);
                if *cancelled || d.to == d.from {
                    return None;
                }
                let tab = self.tabs.remove(d.from);
                let key = tab.key;
                self.tabs.insert(d.to, tab);
                cx.relayout();
                Some(TabOut::Moved { key, to: d.to })
            }
            (Event::Key { key, down: true, mods, .. }, Part::Tab(_) | Part::Plus) if !mods.ctrl() && !mods.alt() => {
                let at = match part {
                    Part::Tab(i) => i,
                    _ => self.tabs.len(),
                };
                if self.rove(cx, at, *key) {
                    return None;
                }
                match (*key, part) {
                    (Key::ENTER, Part::Tab(i)) => {
                        cx.consume();
                        self.choose(cx, i)
                    }
                    (Key::ENTER, _) => {
                        cx.consume();
                        Some(TabOut::NewRequested)
                    }
                    (Key::SPACE, _) => {
                        cx.consume();
                        None
                    }
                    (Key::DELETE, Part::Tab(i)) => {
                        cx.consume();
                        Some(TabOut::CloseRequested(self.tabs[i].key))
                    }
                    _ => None,
                }
            }
            (Event::Key { key: Key::SPACE, down: false, mods, .. }, Part::Tab(i)) if !mods.ctrl() && !mods.alt() => {
                cx.consume();
                self.choose(cx, i)
            }
            (Event::Key { key: Key::SPACE, down: false, mods, .. }, Part::Plus) if !mods.ctrl() && !mods.alt() => {
                cx.consume();
                Some(TabOut::NewRequested)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Chord, Command as Cmd, CommandId, CommandTable};
    use crate::cx::EventCx;
    use crate::event::Reply;
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::{Harness, Image};
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    const NEXT: u32 = 1;
    const PREV: u32 = 2;
    const PGDN: u32 = 3;
    const PGUP: u32 = 4;
    const DIGIT: u32 = 10;

    /// A window as an app builds it: the strip, and the Tabs row's chords bound to the two selection methods.
    struct App {
        strip: TabStrip,
        outs: Vec<TabOut>,
        picked: Vec<Option<u64>>,
        cmds: CommandTable,
    }

    fn spec(key: u64, busy: bool) -> TabSpec {
        TabSpec { key, label: Text::from(format!("Folder {key}")), tip: None, icon: None, busy, error: false }
    }

    fn app(n: u64, selected: u64) -> App {
        let mut cmds = CommandTable::new();
        let mut bind = |id: u32, chord: &str| {
            let c = Cmd::new(CommandId(id), Text::from(format!("cmd {id}"))).chord(Chord::parse(chord).unwrap());
            cmds.add(c).unwrap();
        };
        bind(NEXT, "Ctrl+Tab");
        bind(PREV, "Ctrl+Shift+Tab");
        bind(PGDN, "Ctrl+PgDn");
        bind(PGUP, "Ctrl+PgUp");
        for d in 1..=9u32 {
            bind(DIGIT + d, &format!("Ctrl+{d}"));
        }
        let tabs = (1..=n).map(|k| spec(k, false)).collect();
        App { strip: TabStrip::new(WidgetId(1)).with_tabs(tabs, selected), outs: Vec::new(), picked: Vec::new(), cmds }
    }

    impl Screen for App {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.strip.layout(cx, RectP::wh(0, 0, avail.w, cx.px(40)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.strip.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if let Event::Command(id) = ev {
                let r = match id.0 {
                    NEXT | PGDN => self.strip.select_next(cx, false),
                    PREV | PGUP => self.strip.select_next(cx, true),
                    n if (DIGIT + 1..=DIGIT + 8).contains(&n) => self.strip.select_index(cx, (n - DIGIT - 1) as usize),
                    n if n == DIGIT + 9 => self.strip.select_index(cx, usize::MAX),
                    _ => return Reply::None,
                };
                self.picked.push(r);
            } else if let Some(o) = self.strip.event(cx, ev) {
                self.outs.push(o);
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(v) = msg.downcast::<Vec<(u64, bool)>>() {
                let tabs = v.iter().map(|(k, busy)| spec(*k, *busy)).collect();
                let sel = self.strip.selected();
                self.strip.set_tabs(cx, tabs, sel);
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }

        fn caption_hit(&self, p: PointP) -> CaptionHit {
            self.strip.caption_hit(p)
        }
    }

    fn harness(n: u64, selected: u64) -> Harness<App> {
        Harness::new(app(n, selected), SizeL { w: 1100, h: 40 }, 96, Tokens::oled())
    }

    fn node(h: &mut Harness<App>, slot: u32) -> RectP {
        h.dispatcher().frame.get(NodeId::new(WidgetId(1), slot)).map(|n| n.rect).unwrap()
    }

    fn mid(r: RectP) -> PointL {
        PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
    }

    /// The brightest blue value in the 7 x 7 pixels around the centre of r.
    fn lit(img: &Image, r: RectP) -> u32 {
        let (cx, cy) = ((r.l + r.r) / 2, (r.t + r.b) / 2);
        let mut best = 0;
        for y in cy - 3..=cy + 3 {
            for x in cx - 3..=cx + 3 {
                best = best.max(img.px[(y as u32 * img.w + x as u32) as usize] & 0xFF);
            }
        }
        best
    }

    fn focus(h: &Harness<App>) -> u32 {
        h.focused().map_or(u32::MAX, |n| n.slot())
    }

    #[test]
    fn widths_shrink_evenly_within_bounds() {
        for n in 1..=30usize {
            let w = tab_widths(n, 1100, 2, 96, 220);
            assert_eq!(w.len(), n);
            assert!(w.iter().all(|x| (96..=220).contains(x)), "{n}: {w:?}");
            let (lo, hi) = (w.iter().min().unwrap(), w.iter().max().unwrap());
            assert!(hi - lo <= 1, "{n}: even to the pixel: {w:?}");
            let used = w.iter().sum::<i32>() + 2 * (n as i32 - 1);
            if n * 96 + 2 * (n - 1) <= 1100 {
                assert!(used <= 1100, "{n}: fits: {used}");
                if n >= 6 {
                    assert!(used >= 1100 - 1, "{n}: fills the room: {used}");
                }
            } else {
                assert!(w.iter().all(|x| *x == 96), "{n}: past the room every plate is TAB_W_MIN");
            }
        }
        assert_eq!(tab_widths(1, 1100, 2, 96, 220), [220]);
        assert!(tab_widths(0, 1100, 2, 96, 220).is_empty());
        // Through the widget: 3 tabs at 1100 px are TAB_W_MAX; 12 tabs share the room.
        let mut h = harness(3, 1);
        assert_eq!(node(&mut h, 1).w(), 220);
        assert_eq!((node(&mut h, 1).t, node(&mut h, 1).h()), (4, 32), "TAB_H 32 at y 4 of the 40 px strip");
        let mut h = harness(12, 1);
        let w = node(&mut h, 1).w();
        assert!((90..=96 + 40).contains(&w) && w < 220, "{w}");
    }

    #[test]
    fn close_shows_on_hover_and_active() {
        let mut h = harness(3, 2);
        let img = h.render();
        let close = |h: &mut Harness<App>, i: u32| {
            let r = node(h, 1 + i);
            RectP::wh(r.r - 4 - 24, r.t + 4, 24, 24)
        };
        let (c0, c1) = (close(&mut h, 0), close(&mut h, 1));
        assert!(lit(&img, c1) > 100, "the active tab shows its close glyph");
        assert!(lit(&img, c0) < 30, "an idle tab does not");
        let r0 = node(&mut h, 1);
        h.hover(mid(r0));
        h.advance(200);
        let img = h.render();
        assert!(lit(&img, c0) > 100, "the hovered tab does");
        h.hover(PointL { x: 1090, y: 20 });
        h.advance(300);
        let img = h.render();
        assert!(lit(&img, c0) < 30, "and not once the pointer left");
    }

    #[test]
    fn drag_reorders_after_threshold() {
        let mut h = harness(3, 1);
        let (a, c) = (node(&mut h, 1), node(&mut h, 3));
        // Under DRAG_THRESHOLD nothing moves.
        h.drag(mid(a), PointL { x: mid(a).x + 3, y: mid(a).y });
        assert_eq!(h.screen().outs, []);
        // Past it: the first tab dropped over the third lands at index 2.
        h.drag(mid(a), mid(c));
        assert_eq!(h.screen().outs, [TabOut::Moved { key: 1, to: 2 }]);
        // The strip keeps the order it was dropped in.
        let k0 = h.screen().strip.tabs.iter().map(|t| t.key).collect::<Vec<_>>();
        assert_eq!(k0, [2, 3, 1]);
    }

    #[test]
    fn middle_click_closes() {
        let mut h = harness(3, 1);
        let b = node(&mut h, 2);
        let p = PointP { x: (b.l + b.r) / 2, y: (b.t + b.b) / 2 };
        h.input(crate::dispatch::Input::Move(p, crate::event::Mods::NONE));
        h.input(crate::dispatch::Input::Down(p, MouseButton::Middle, 1, crate::event::Mods::NONE));
        h.input(crate::dispatch::Input::Up(p, MouseButton::Middle, crate::event::Mods::NONE));
        assert_eq!(h.screen().outs, [TabOut::CloseRequested(2)]);
        // The close plate and a right click.
        let c = node(&mut h, CLOSE_BASE + 2);
        h.click(mid(c));
        assert_eq!(h.screen().outs[1], TabOut::CloseRequested(3));
        h.right_click(mid(b));
        assert!(matches!(h.screen().outs[2], TabOut::ContextMenu(2, _)));
    }

    #[test]
    fn empty_space_is_caption() {
        let mut h = harness(3, 1);
        let s = h.screen();
        let p = |x, y| PointP { x, y };
        assert_eq!(s.caption_hit(p(900, 20)), CaptionHit::Caption, "empty strip space");
        assert_eq!(s.caption_hit(p(100, 20)), CaptionHit::Client, "a tab");
        let plus = node(&mut h, PLUS_SLOT);
        assert_eq!(h.screen().caption_hit(PointP { x: plus.l + 5, y: plus.t + 5 }), CaptionHit::Client, "the plus");
        assert_eq!(h.screen().caption_hit(p(900, 60)), CaptionHit::Client, "outside the strip");
        assert_eq!(h.screen().caption_hit(p(2, 2)), CaptionHit::Caption, "the margin above a tab is caption");
    }

    #[test]
    fn sweep_only_while_busy() {
        let mut h = harness(3, 1);
        h.advance(50);
        assert!(h.is_idle(), "a strip with nothing busy keeps no clock");
        h.send(Box::new(vec![(1u64, false), (2, true), (3, false)]));
        assert!(!h.is_idle(), "the sweep keeps the frame clock");
        let line = |h: &mut Harness<App>| {
            let r = node(h, 2);
            let img = h.render();
            (r.l..r.r).filter(|x| img.px[((r.b + 1) as u32 * img.w + *x as u32) as usize] & 0xFF > 100).count()
        };
        h.advance(300);
        let n = line(&mut h);
        assert!(n > 0 && n <= 60, "a quarter of the plate is lit, not all: {n}");
        h.advance(300);
        h.send(Box::new(vec![(1u64, false), (2, false), (3, false)]));
        assert!(h.is_idle(), "idle again when nothing is busy");
        assert_eq!(line(&mut h), 0);
    }

    #[test]
    fn strip_is_one_tab_stop() {
        let mut h = harness(3, 2);
        h.key("Tab");
        assert_eq!(focus(&h), 2, "the selected tab takes the focus on entry");
        h.key("Right");
        assert_eq!(focus(&h), 3);
        h.key("Tab");
        assert_ne!(focus(&h), 3, "Tab leaves a roving stop");
        h.key("Tab");
        assert_eq!(focus(&h), 2, "and comes back to the selected tab, not to the others");
    }

    #[test]
    fn left_right_home_end_rove() {
        let mut h = harness(3, 2);
        h.key("Tab");
        h.key("Right");
        assert_eq!(focus(&h), 3);
        h.key("Right");
        assert_eq!(focus(&h), PLUS_SLOT);
        h.key("Right");
        assert_eq!(focus(&h), PLUS_SLOT, "no wrap");
        h.key("Left");
        assert_eq!(focus(&h), 3);
        h.key("Home");
        assert_eq!(focus(&h), 1);
        h.key("Left");
        assert_eq!(focus(&h), 1, "no wrap at the start");
        h.key("End");
        assert_eq!(focus(&h), 3);
        assert_eq!(h.screen().strip.selected(), 2, "roving moves the focus, not the selection");
        assert_eq!(h.screen().outs, []);
    }

    #[test]
    fn enter_and_space_select_the_focused_tab() {
        let mut h = harness(3, 2);
        h.key("Tab");
        h.key("Left");
        h.key("Enter");
        assert_eq!(h.screen().outs, [TabOut::Selected(1)]);
        h.key("Right");
        h.key("Right");
        h.key("Space");
        assert_eq!(h.screen().outs, [TabOut::Selected(1), TabOut::Selected(3)]);
        assert_eq!(h.screen().strip.selected(), 3);
    }

    #[test]
    fn delete_closes_the_focused_tab() {
        let mut h = harness(3, 2);
        h.key("Tab");
        h.key("Delete");
        assert_eq!(h.screen().outs, [TabOut::CloseRequested(2)]);
        h.key("Left");
        h.key("Delete");
        assert_eq!(h.screen().outs[1], TabOut::CloseRequested(1));
    }

    #[test]
    fn plus_is_the_last_stop() {
        let mut h = harness(3, 2);
        h.key("Tab");
        h.key("Right");
        h.key("Right");
        assert_eq!(focus(&h), PLUS_SLOT);
        h.key("Enter");
        assert_eq!(h.screen().outs, [TabOut::NewRequested]);
        h.key("Space");
        assert_eq!(h.screen().outs.len(), 2);
        h.key("Right");
        assert_eq!(focus(&h), PLUS_SLOT, "it is the last");
        assert!(h.a11y_dump().contains("New tab"), "{}", h.a11y_dump());
        h.assert_named();
    }

    fn after(chords: &[&str], n: u64, selected: u64) -> (Vec<Option<u64>>, Vec<TabOut>, u64) {
        let mut h = harness(n, selected);
        for c in chords {
            h.key(c);
        }
        let s = h.screen();
        (s.picked.clone(), s.outs.clone(), s.strip.selected())
    }

    #[test]
    fn ctrl_tab_selects_next_and_wraps() {
        let (picked, outs, sel) = after(&["Ctrl+Tab", "Ctrl+Tab"], 3, 2);
        assert_eq!(picked, [Some(3), Some(1)], "the second one wraps");
        assert_eq!(outs, [], "an app-driven change emits no TabOut");
        assert_eq!(sel, 1);
    }

    #[test]
    fn ctrl_shift_tab_selects_previous_and_wraps() {
        let (picked, _, sel) = after(&["Ctrl+Shift+Tab", "Ctrl+Shift+Tab"], 3, 2);
        assert_eq!(picked, [Some(1), Some(3)]);
        assert_eq!(sel, 3);
    }

    #[test]
    fn ctrl_pgdn_pgup_select_next_previous() {
        let (picked, _, sel) = after(&["Ctrl+PgDn", "Ctrl+PgDn", "Ctrl+PgUp"], 3, 1);
        assert_eq!(picked, [Some(2), Some(3), Some(2)]);
        assert_eq!(sel, 2);
    }

    #[test]
    fn ctrl_digit_selects_index_and_ignores_past_count() {
        let (picked, _, sel) = after(&["Ctrl+3", "Ctrl+5", "Ctrl+1"], 3, 2);
        assert_eq!(picked, [Some(3), None, Some(1)], "Ctrl+5 has no fifth tab");
        assert_eq!(sel, 1);
        let (picked, _, sel) = after(&["Ctrl+8"], 3, 2);
        assert_eq!((picked, sel), (vec![None], 2), "no effect past the count");
    }

    #[test]
    fn ctrl_9_selects_last() {
        let (picked, _, sel) = after(&["Ctrl+9"], 5, 1);
        assert_eq!(picked, [Some(5)]);
        assert_eq!(sel, 5);
    }

    #[test]
    fn overflow_scrolls_and_keeps_the_selected_in_view() {
        let mut h = harness(30, 30);
        let last = node(&mut h, 30);
        assert!(last.r <= 1100, "the last tab was revealed: {last:?}");
        h.wheel(PointL { x: 300, y: 20 }, 80.0);
        let first = node(&mut h, 1);
        assert!(first.l >= 0 && first.l < 100, "the wheel scrolled back: {first:?}");
        assert!(node(&mut h, PLUS_SLOT).r <= 1100, "the plus stays at the right end");
    }
}
