//! PageTabs (F-K2b, P2-kit 2.9): the page tabs of a toolbar (X16, SPEC-tasks 5.3.2). Body text in FG_SECONDARY, 28 px hit
//! boxes at y 8 of the toolbar, a 2 px underline at y 36-38 under the selected one. One Tab stop with roving Left/Right
//! (the selection follows the focus); Ctrl+1..n go through the screen's CommandTable, which calls set_selected.

use super::paint::{lerp_ink, text_centered};
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::RectP;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::theme::{Ink, TypeRole};

/// What page tabs tell their screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTabsOut {
    Selected(usize),
}

/// A hit box's height and its distance from the toolbar's top, the padding each side of a label, the gap between two
/// boxes (labels end up 20 px apart) and the underline's place (logical px).
const BOX_H: i32 = 28;
const BOX_Y: i32 = 8;
const PAD: i32 = 8;
const GAP: i32 = 4;
const UNDERLINE_Y: i32 = 36;
const UNDERLINE_H: i32 = 2;

/// The page tabs of a toolbar.
pub struct PageTabs {
    id: WidgetId,
    tabs: Vec<Text>,
    selected: usize,
    /// The hit boxes, from the last layout.
    rects: Vec<RectP>,
    /// Whose nodes these are and from which slot: the tabs' own id and slot 1, or a Toolbar's.
    owner: WidgetId,
    slot0: u32,
    /// The toolbar's top, from the last layout.
    top: i32,
}

impl PageTabs {
    pub fn new(id: WidgetId, tabs: Vec<Text>) -> PageTabs {
        PageTabs { id, tabs, selected: 0, rects: Vec::new(), owner: id, slot0: 1, top: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selects tab `i` (kept within the tabs).
    pub fn set_selected(&mut self, cx: &mut EventCx, i: usize) {
        let i = i.min(self.tabs.len().saturating_sub(1));
        if i != self.selected {
            self.selected = i;
            cx.relayout();
        }
    }

    /// The tabs with tab `i` selected from the start (kept within the tabs).
    pub fn with_selected(mut self, i: usize) -> PageTabs {
        self.selected = i.min(self.tabs.len().saturating_sub(1));
        self
    }

    // ---- what a Toolbar shares

    /// A Toolbar lays the tabs out and owns their nodes: slot `slot0` and up under its own id.
    pub(crate) fn adopt(&mut self, owner: WidgetId, slot0: u32) {
        self.owner = owner;
        self.slot0 = slot0;
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn tab_node(&self, i: usize) -> NodeId {
        NodeId::new(self.owner, self.slot0 + i as u32)
    }

    /// The tab a node of the owner is.
    pub(crate) fn index_of(&self, n: NodeId) -> Option<usize> {
        (n.owner() == self.owner && n.slot() >= self.slot0)
            .then(|| (n.slot() - self.slot0) as usize)
            .filter(|i| *i < self.tabs.len())
    }

    /// Sets the hit boxes from x0 on, the toolbar's top at `top`; returns the width taken.
    pub(crate) fn measure(&mut self, cx: &mut LayoutCx, x0: i32, top: i32) -> i32 {
        let font = cx.font(TypeRole::Body);
        let (pad, gap) = (cx.px(PAD), cx.px(GAP));
        let mut x = x0;
        self.rects.clear();
        for t in &self.tabs {
            let w = cx.text().width(t, font) + 2 * pad;
            self.rects.push(RectP::wh(x, top + cx.px(BOX_Y), w, cx.px(BOX_H)));
            x += w + gap;
        }
        (x - gap - x0).max(0)
    }

    pub(crate) fn paint_tabs(&self, cx: &mut PaintCx, top: i32) {
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        for (i, (r, text)) in self.rects.iter().zip(&self.tabs).enumerate() {
            let id = self.tab_node(i);
            let selected = i == self.selected;
            if cx.pressed(id) {
                cx.fill(*r, radius, c.fill_press);
            } else {
                let t = cx.hover_t(id);
                let clear = Ink { a: 0, ..c.fill_hover };
                cx.fill(*r, radius, lerp_ink(clear, c.fill_hover, t));
            }
            let t = cx.hover_t(id);
            let ink = if selected { c.fg_primary } else { lerp_ink(c.fg_secondary, c.fg_secondary_hot, t) };
            text_centered(cx, text, TypeRole::Body, *r, ink);
            if selected {
                let (y0, h) = (top + cx.px(UNDERLINE_Y), cx.px(UNDERLINE_H));
                let bar = RectP { l: r.l + cx.px(PAD), t: y0, r: r.r - cx.px(PAD), b: y0 + h };
                cx.fill(bar, cx.px(1), c.fg_primary);
            }
            if cx.focused(id) {
                cx.focus_ring(*r, radius, false);
            }
        }
    }

    /// Pushes the tab nodes: the selected one is the Tab stop unless `stop` says otherwise.
    pub(crate) fn push_nodes(&self, cx: &mut LayoutCx, parent: NodeId, tab_stop: Option<usize>) {
        for (i, (r, text)) in self.rects.iter().zip(&self.tabs).enumerate() {
            let mut flags = NodeFlags::HOVER_FADE;
            if i == self.selected {
                flags = flags | NodeFlags::SELECTED;
            }
            let stop = tab_stop.unwrap_or(self.selected);
            let focus = if i == stop { Focusable::Tab } else { Focusable::Roving };
            cx.push(
                Node::new(self.tab_node(i), Role::Tab, *r)
                    .parent(parent)
                    .flags(flags)
                    .focus(focus)
                    .label(text.clone())
                    .actions(Actions::CLICK | Actions::FOCUS),
            );
        }
    }

    /// Selects a tab by the user: focus follows, the output tells the screen.
    pub(crate) fn pick(&mut self, cx: &mut EventCx, i: usize) -> Option<PageTabsOut> {
        cx.set_focus(self.tab_node(i));
        if self.selected == i {
            return None;
        }
        self.selected = i;
        cx.relayout();
        Some(PageTabsOut::Selected(i))
    }
}

impl Widget for PageTabs {
    type Out = PageTabsOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let w = self.measure(cx, rect.l, rect.t);
        let group = NodeId::new(self.id, 0);
        let span = RectP { l: rect.l, t: rect.t + cx.px(BOX_Y), r: rect.l + w, b: rect.t + cx.px(BOX_Y + BOX_H) };
        cx.push(Node::new(group, Role::TabList, span));
        self.push_nodes(cx, group, None);
        self.top = rect.t;
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.paint_tabs(cx, self.top);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PageTabsOut> {
        let i = self.index_of(cx.target()?)?;
        match ev {
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click) => self.pick(cx, i),
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                self.pick(cx, i)
            }
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                if *down {
                    None
                } else {
                    self.pick(cx, i)
                }
            }
            Event::Key { key, down: true, .. } => {
                let last = self.tabs.len().checked_sub(1)?;
                let to = match *key {
                    Key::LEFT => i.saturating_sub(1),
                    Key::RIGHT => (i + 1).min(last),
                    Key::HOME => 0,
                    Key::END => last,
                    _ => return None,
                };
                cx.consume();
                self.pick(cx, to)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    fn harness(dpi: u32) -> Harness<Host<PageTabs>> {
        let t = PageTabs::new(
            WidgetId(1),
            vec![Text::from("Processes"), Text::from("Performance"), Text::from("Services")],
        );
        Harness::new(Host::new(t, (16, 0, 360, 44)), SizeL { w: 400, h: 44 }, dpi, Tokens::oled())
    }

    fn blue(img: &crate::harness::Image, x: i32, y: i32) -> u32 {
        img.px[(y as u32 * img.w + x as u32) as usize] & 0xFF
    }

    #[test]
    fn page_tabs_underline_matches_mockup() {
        let mut h = harness(96);
        let img = h.render();
        let tab = h.dispatcher().frame.iter().find(|n| n.role == Role::Tab).map(|n| n.rect).unwrap();
        let mid = (tab.l + tab.r) / 2;
        assert_eq!(tab.t, 8, "the hit box starts at y 8");
        assert_eq!(tab.h(), 28);
        assert_eq!(blue(&img, mid, 35), 0, "nothing above the underline");
        assert_eq!(blue(&img, mid, 36), 230, "FG_PRIMARY at y 36");
        assert_eq!(blue(&img, mid, 37), 230, "and y 37: two px");
        assert_eq!(blue(&img, mid, 38), 0, "nothing below");
        assert_eq!(blue(&img, tab.l + 4, 37), 0, "it starts 8 px inside the box");
        assert_eq!(blue(&img, tab.r - 4, 37), 0, "and ends 8 px inside it");
        // The tab after it has no underline.
        let next = h.dispatcher().frame.iter().filter(|n| n.role == Role::Tab).nth(1).map(|n| n.rect).unwrap();
        assert_eq!(blue(&img, (next.l + next.r) / 2, 37), 0);
        assert_eq!(next.l - tab.r, 4, "the labels end up 20 px apart");
    }

    #[test]
    fn page_tabs_roving() {
        let mut h = harness(96);
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)), "the selected tab is the Tab stop");
        h.key("Right");
        assert_eq!(h.screen().outs, [PageTabsOut::Selected(1)]);
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 2)));
        h.key("End");
        assert_eq!(h.screen().w.selected(), 2);
        h.key("Right");
        assert_eq!(h.screen().w.selected(), 2, "the end stays put");
        h.key("Home");
        assert_eq!(h.screen().w.selected(), 0);
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)), "one Tab stop: Tab comes back to it");
    }

    #[test]
    fn page_tabs_click_and_a11y() {
        let mut h = harness(96);
        let tab = h.dispatcher().frame.iter().filter(|n| n.role == Role::Tab).nth(2).map(|n| n.rect).unwrap();
        h.click(PointL { x: (tab.l + tab.r) / 2, y: 20 });
        assert_eq!(h.screen().outs, [PageTabsOut::Selected(2)]);
        assert!(h.a11y_dump().contains("Tab \"Services\" [focusable, selected]"), "{}", h.a11y_dump());
        h.assert_named();
    }

    #[test]
    fn hover_and_pressed_plates() {
        let mut h = harness(96);
        let probe = |h: &mut Harness<Host<PageTabs>>| {
            let img = h.render();
            blue(&img, 24, 12)
        };
        assert_eq!(probe(&mut h), 0);
        h.hover(PointL { x: 30, y: 20 });
        h.advance(200);
        assert_eq!(probe(&mut h), 20, "FILL_HOVER");
    }
}
