//! SidebarList (F-K1, P2-kit 2.6): the Settings nav's rows pixel for pixel (R1: settings_view.rs paint_nav_entry and
//! Surface::plate), plus the tree, sections, trailing text and drop target the files app needs (SPEC-files 5.2.4).

use super::paint::{fg_ink, lerp_ink};
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{PointP, RectP, SizeP};
use crate::glyph::Glyph;
use crate::model::{Align, FgRole};
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};
use crate::tokens::TYPEAHEAD_RESET;
use crate::w3::IconBitmap;
use std::rc::Rc;

/// One entry of a sidebar.
pub struct SidebarItem {
    pub key: u64,
    pub label: Text,
    pub glyph: Option<Glyph>,
    pub icon: Option<Rc<IconBitmap>>,
    pub depth: u16,
    pub expandable: bool,
    pub expanded: bool,
    pub section: bool,
    pub trailing: Option<(Text, FgRole)>,
    pub disabled: bool,
    pub busy: bool,
}

/// A sidebar's row height and inset, logical px.
pub struct SidebarStyle {
    /// 36 = today's Settings nav, 28 = files.
    pub row_h: i32,
    /// 8.
    pub inset: i32,
}

/// What a sidebar tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarOut {
    Activated(u64),
    Expand(u64, bool),
    ContextMenu(u64, PointP),
}

/// The glyph's size, its left offset and the label's offset after a glyph, logical px (paint_nav_entry).
const ICON: i32 = 16;
const GLYPH_X: i32 = 12;
const LABEL_X: i32 = 40;
/// The right margin of the label and of the trailing text.
const RIGHT: i32 = 8;
const TRAIL_RIGHT: i32 = 12;
/// One depth level's indent and the expander's box.
const INDENT: i32 = 16;
const EXPANDER: i32 = 12;
/// The nav bar's width and, off the 36 px rows, its height.
const BAR_W: i32 = 3;
const BAR_H: i32 = 16;
/// The drop target's rim, in logical px times two.
const DROP_RIM_X2: i32 = 3;

/// A navigation list with sections, a tree and a current entry.
pub struct SidebarList {
    id: WidgetId,
    style: SidebarStyle,
    items: Vec<SidebarItem>,
    current: Option<u64>,
    drop_target: Option<u64>,
    rect: RectP,
    /// The rows' plates (inset in the rect), set by layout.
    rows: Vec<RectP>,
    /// The type-ahead text and when its last character came.
    typeahead: (String, u64),
}

impl SidebarList {
    pub fn new(id: WidgetId, style: SidebarStyle) -> SidebarList {
        SidebarList {
            id,
            style,
            items: Vec::new(),
            current: None,
            drop_target: None,
            rect: RectP::EMPTY,
            rows: Vec::new(),
            typeahead: (String::new(), 0),
        }
    }

    /// The entries; the focused one stays focused by its key, and the list is laid out again.
    pub fn set_items(&mut self, cx: &mut EventCx, items: Vec<SidebarItem>) {
        let kept = cx.focused().and_then(|n| self.index_of(n)).map(|i| self.items[i].key);
        self.items = items;
        if let Some(i) = kept.and_then(|k| self.items.iter().position(|it| it.key == k)) {
            cx.set_focus(self.row_node(i));
        }
        cx.relayout();
    }

    /// The entries before any window exists (a screen's constructor has no EventCx to give set_items).
    pub fn with_items(mut self, items: Vec<SidebarItem>) -> SidebarList {
        self.items = items;
        self
    }

    pub fn set_current(&mut self, key: Option<u64>) {
        self.current = key;
    }

    /// The entry a drag would drop on.
    pub fn set_drop_target(&mut self, key: Option<u64>) {
        self.drop_target = key;
    }

    /// The list's node (slot 0); the rows are slots 1 and up.
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn row_node(&self, i: usize) -> NodeId {
        NodeId::new(self.id, i as u32 + 1)
    }

    fn index_of(&self, n: NodeId) -> Option<usize> {
        (n.owner() == self.id && n.slot() >= 1).then(|| n.slot() as usize - 1).filter(|i| *i < self.items.len())
    }

    /// True for the rows that take the focus and the pointer.
    fn live(it: &SidebarItem) -> bool {
        !it.section && !it.disabled
    }

    /// The Tab stop: the current entry, else the first live one.
    fn stop(&self) -> Option<usize> {
        let cur = self.current.and_then(|k| self.items.iter().position(|it| it.key == k && Self::live(it)));
        cur.or_else(|| self.items.iter().position(Self::live))
    }

    /// The nearest live row from i in direction d (not i itself).
    fn step(&self, i: usize, d: isize) -> Option<usize> {
        let mut j = i as isize + d;
        while j >= 0 && (j as usize) < self.items.len() {
            if Self::live(&self.items[j as usize]) {
                return Some(j as usize);
            }
            j += d;
        }
        None
    }

    fn go(&self, cx: &mut EventCx, to: Option<usize>) {
        if let Some(j) = to {
            cx.set_focus(self.row_node(j));
            cx.consume();
        }
    }

    /// The expander's box of a row, physical px.
    fn expander(&self, row: RectP, depth: u16, px: &dyn Fn(i32) -> i32) -> RectP {
        let side = px(EXPANDER);
        RectP::wh(row.l + px(INDENT) * i32::from(depth), (row.t + row.b) / 2 - side / 2, side, side)
    }

    fn type_ahead(&mut self, cx: &mut EventCx, i: usize, c: char) {
        if c.is_control() || self.items.is_empty() {
            return;
        }
        let now = cx.now_ms();
        if now.saturating_sub(self.typeahead.1) > u64::from(TYPEAHEAD_RESET) {
            self.typeahead.0.clear();
        }
        if c.is_whitespace() && self.typeahead.0.is_empty() {
            return;
        }
        self.typeahead.0.extend(c.to_lowercase());
        self.typeahead.1 = now;
        let len = self.items.len();
        let start = if self.typeahead.0.chars().count() > 1 { i } else { (i + 1) % len };
        let found = (0..len)
            .map(|k| (start + k) % len)
            .find(|&r| Self::live(&self.items[r]) && self.items[r].label.to_lowercase().starts_with(&self.typeahead.0));
        self.go(cx, found);
    }
}

impl Widget for SidebarList {
    type Out = SidebarOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let (rh, inset) = (cx.px(self.style.row_h), cx.px(self.style.inset));
        let total = rh * self.items.len() as i32;
        cx.push(Node::new(self.node_id(), Role::Tree, RectP { b: rect.t + total, ..rect }));
        self.rows.clear();
        let stop = self.stop();
        for (i, it) in self.items.iter().enumerate() {
            let row = RectP {
                l: rect.l + inset,
                t: rect.t + rh * i as i32,
                r: rect.r - inset,
                b: rect.t + rh * (i as i32 + 1),
            };
            self.rows.push(row);
            if it.section {
                let node = Node::new(self.row_node(i), Role::Label, row).parent(self.node_id()).label(it.label.clone());
                cx.push(node);
                continue;
            }
            let mut flags = NodeFlags::HOVER_FADE;
            if self.current == Some(it.key) {
                flags = flags | NodeFlags::SELECTED;
            }
            if it.expandable {
                flags = flags | NodeFlags::EXPANDABLE;
            }
            if it.expanded {
                flags = flags | NodeFlags::EXPANDED;
            }
            if it.busy {
                flags = flags | NodeFlags::BUSY;
            }
            if it.disabled {
                flags = flags | NodeFlags::DISABLED;
            }
            let focus = if it.disabled {
                Focusable::No
            } else if Some(i) == stop {
                Focusable::Tab
            } else {
                Focusable::Roving
            };
            let mut actions = Actions::CLICK | Actions::FOCUS | Actions::CONTEXT_MENU;
            if it.expandable {
                actions = actions | if it.expanded { Actions::COLLAPSE } else { Actions::EXPAND };
            }
            let node = Node::new(self.row_node(i), Role::TreeItem, row)
                .parent(self.node_id())
                .flags(flags)
                .focus(focus)
                .label(it.label.clone())
                .value(NodeValue::Level(it.depth + 1))
                .actions(actions);
            cx.push(node);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        let icon = cx.px(ICON);
        for (i, (it, row)) in self.items.iter().zip(&self.rows).enumerate() {
            if !cx.visible(*row) {
                continue;
            }
            let (id, row) = (self.row_node(i), *row);
            let indent = cx.px(INDENT) * i32::from(it.depth);
            if it.section {
                let room = RectP { l: row.l + cx.px(GLYPH_X), r: row.r - cx.px(RIGHT), ..row };
                cx.text_in(&it.label, TypeRole::Section, room, Align::Start, Trim::End, c.fg_secondary);
                continue;
            }
            let current = self.current == Some(it.key);
            let live = !it.disabled;
            let t = if live { cx.hover_t(id) } else { 0.0 };
            let hot = t > 0.0;
            if live && (current || hot) {
                // Surface::plate: the fill, and a 1 px rim at 18 x min(a, 20) / 20.
                let top = if cx.pressed(id) { c.fill_press } else { c.fill_hover };
                let fill = if current { top } else { lerp_ink(Ink { a: 0, ..top }, top, t) };
                let rim_a = c.rim_hover.a * fill.a.min(c.fill_hover.a) / c.fill_hover.a.max(1);
                cx.fill(row, radius, fill);
                cx.stroke(row, radius, cx.px(1).max(1), Ink { a: rim_a, ..c.rim_hover });
            }
            if live && self.drop_target == Some(it.key) {
                cx.fill(row, radius, c.fill_hover);
                cx.stroke(row, radius, (cx.px(DROP_RIM_X2) + 1) / 2, c.rim_focus);
            }
            if current && live {
                // The 3 px bar on the left edge: 10 px in on the 36 px rows, else 16 px tall and centred.
                let (top, bottom) = if self.style.row_h == 36 {
                    (row.t + cx.px(10), row.b - cx.px(10))
                } else {
                    let h = cx.px(BAR_H);
                    (row.t + (row.h() - h) / 2, row.t + (row.h() - h) / 2 + h)
                };
                cx.fill(RectP { l: row.l, t: top, r: row.l + cx.px(BAR_W), b: bottom }, cx.px(1), c.nav_bar);
            }
            // Text and glyph: the current entry at full alpha, the others at FG_SECONDARY, lifted 25 on hover.
            let ink = if !live {
                c.fg_disabled
            } else if current {
                c.fg_hot
            } else {
                Ink { a: (c.fg_secondary.a + (25.0 * t).round() as u32).min(255), ..c.fg_secondary }
            };
            if it.expandable && (hot || it.expanded) && live {
                let g = if it.expanded { Glyph::ChevronDown } else { Glyph::ChevronRight };
                cx.glyph(self.expander(row, it.depth, &|v| cx.px(v)), g, c.fg_secondary);
            }
            let has_icon = it.glyph.is_some() || it.icon.is_some();
            let gx = row.l + cx.px(GLYPH_X) + indent;
            let gy = row.t + (row.h() - icon) / 2;
            if let Some(g) = it.glyph {
                cx.glyph(RectP::wh(gx, gy, icon, icon), g, ink);
            } else if let Some(bmp) = &it.icon {
                cx.image(bmp, PointP { x: gx, y: gy }, icon, if live { ink.a } else { 90 });
            }
            let lx = if has_icon { row.l + cx.px(LABEL_X) + indent } else { gx };
            let mut right = row.r - cx.px(RIGHT);
            let trailing = it.trailing.as_ref().map(|(t, role)| (t, fg_ink(*role, cx.tokens())));
            if let Some((text, tink)) = trailing {
                let room = RectP { l: lx, r: row.r - cx.px(TRAIL_RIGHT), ..row };
                let w = cx.text_in(
                    text,
                    TypeRole::Caption,
                    room,
                    Align::End,
                    Trim::End,
                    if live { tink } else { c.fg_disabled },
                );
                right = right.min(row.r - cx.px(TRAIL_RIGHT) - w - cx.px(RIGHT));
            } else if it.busy {
                let room = RectP { l: lx, r: row.r - cx.px(TRAIL_RIGHT), ..row };
                let w = cx.text_in("\u{2026}", TypeRole::Caption, room, Align::End, Trim::End, c.fg_tertiary); // lint: not UI
                right = right.min(row.r - cx.px(TRAIL_RIGHT) - w - cx.px(RIGHT));
            }
            let role = if current { TypeRole::BodyStrong } else { TypeRole::Body };
            cx.text_in(&it.label, role, RectP { l: lx, r: right, ..row }, Align::Start, Trim::End, ink);
            if cx.focused(id) {
                cx.focus_ring(row, radius, true);
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SidebarOut> {
        let i = self.index_of(cx.target()?)?;
        let key = self.items[i].key;
        if !Self::live(&self.items[i]) {
            return None;
        }
        match ev {
            Event::Click { button: MouseButton::Left, pos, .. } => {
                let it = &self.items[i];
                let exp = self.expander(self.rows[i], it.depth, &|v| cx.px(v));
                if it.expandable && exp.inset(-cx.px(2), 0).contains(*pos) {
                    Some(SidebarOut::Expand(key, !it.expanded))
                } else {
                    Some(SidebarOut::Activated(key))
                }
            }
            Event::Click { button: MouseButton::Right, pos, .. } => Some(SidebarOut::ContextMenu(key, *pos)),
            Event::A11y(_, A11yAction::Click) => Some(SidebarOut::Activated(key)),
            Event::A11y(_, A11yAction::ShowContextMenu) => {
                let r = self.rows[i];
                Some(SidebarOut::ContextMenu(key, PointP { x: r.l, y: r.b }))
            }
            Event::A11y(_, A11yAction::Expand) if self.items[i].expandable => Some(SidebarOut::Expand(key, true)),
            Event::A11y(_, A11yAction::Collapse) if self.items[i].expandable => Some(SidebarOut::Expand(key, false)),
            Event::Text(c) => {
                self.type_ahead(cx, i, *c);
                None
            }
            Event::Key { key: k, down: true, .. } => {
                let it = &self.items[i];
                match *k {
                    Key::UP => self.go(cx, self.step(i, -1)),
                    Key::DOWN => self.go(cx, self.step(i, 1)),
                    Key::HOME => self.go(cx, self.items.iter().position(Self::live)),
                    Key::END => self.go(cx, self.items.iter().rposition(Self::live)),
                    Key::ENTER => {
                        cx.consume();
                        return Some(SidebarOut::Activated(key));
                    }
                    Key::LEFT => {
                        if it.expandable && it.expanded {
                            cx.consume();
                            return Some(SidebarOut::Expand(key, false));
                        }
                        let parent =
                            (0..i).rev().find(|&j| self.items[j].depth < it.depth && Self::live(&self.items[j]));
                        self.go(cx, parent);
                    }
                    Key::RIGHT if it.expandable => {
                        if !it.expanded {
                            cx.consume();
                            return Some(SidebarOut::Expand(key, true));
                        }
                        let child =
                            self.items.get(i + 1).filter(|n| n.depth > it.depth && Self::live(n)).map(|_| i + 1);
                        self.go(cx, child);
                    }
                    Key::APPS => {
                        cx.consume();
                        let r = self.rows[i];
                        return Some(SidebarOut::ContextMenu(key, PointP { x: r.l, y: r.b }));
                    }
                    _ => {}
                }
                None
            }
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: cx.px(cx.tokens().size.nav_w), h: cx.px(self.style.row_h) * self.items.len() as i32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, SizeL};
    use crate::gfx::scale::Scale;
    use crate::harness::Harness;
    use crate::text::{LayoutOpts, TextEngine};
    use crate::theme::Tokens;
    use crate::w3::Canvas;
    use crate::widgets::paint::Host;
    use crate::widgets::toggle::r1;
    use windows::Win32::Foundation::RECT;

    const SIZE: SizeL = SizeL { w: 200, h: 120 };

    fn item(key: u64, label: &'static str, depth: u16) -> SidebarItem {
        SidebarItem {
            key,
            label: Text::from(label),
            glyph: None,
            icon: None,
            depth,
            expandable: false,
            expanded: false,
            section: false,
            trailing: None,
            disabled: false,
            busy: false,
        }
    }

    fn list(row_h: i32, items: Vec<SidebarItem>) -> SidebarList {
        let mut l = SidebarList::new(WidgetId(1), SidebarStyle { row_h, inset: 8 });
        l.items = items;
        l
    }

    fn harness(dpi: u32, l: SidebarList) -> Harness<Host<SidebarList>> {
        Harness::new(Host::new(l, (0, 0, 200, 120)), SIZE, dpi, Tokens::oled())
    }

    fn nav_items() -> Vec<SidebarItem> {
        let mut v = vec![item(1, "Display", 0), item(2, "Sound", 0), item(3, "Power", 0)];
        for (it, g) in v.iter_mut().zip([Glyph::Monitor, Glyph::Gear, Glyph::Power]) {
            it.glyph = Some(g);
        }
        v
    }

    /// The Settings nav's drawing (settings_view.rs paint_nav_entry and Surface::plate for one entry, no badge), Canvas
    /// and the text engine for Surface; same literals.
    fn dock_nav(dpi: u32, current: usize, hot: Option<usize>) -> Vec<u32> {
        let size = Scale { dpi }.size(SIZE);
        let mut eng = crate::text::engine::GdiTextEngine::new().unwrap();
        r1::buffer(size.w, size.h, dpi, |cv: &mut Canvas, m| {
            let p = |v: i32| m.px(v);
            let titles = ["Display", "Sound", "Power"];
            let glyphs = [Glyph::Monitor, Glyph::Gear, Glyph::Power];
            for (i, (title, glyph)) in titles.iter().zip(glyphs).enumerate() {
                let rc =
                    RECT { left: p(8), top: p(36) * i as i32, right: p(200) - p(8), bottom: p(36) * (i as i32 + 1) };
                let is_cur = i == current;
                let is_hot = hot == Some(i);
                if is_cur || is_hot {
                    cv.fill_rr(rc, p(7), (255, 255, 255), 20);
                    cv.stroke_rr(rc, p(7), p(1).max(1), 18 * 20 / 20);
                }
                if is_cur {
                    cv.fill_rr(
                        RECT { left: rc.left, top: rc.top + p(10), right: rc.left + p(3), bottom: rc.bottom - p(10) },
                        p(1),
                        (255, 255, 255),
                        255,
                    );
                }
                let a = if is_cur {
                    255
                } else if is_hot {
                    195
                } else {
                    170
                };
                let icon = p(16);
                let shape = glyph.shape();
                cv.glyph(rc.left + p(12), rc.top + (rc.bottom - rc.top - icon) / 2, icon, (255, 255, 255), a, &shape);
                let role = if is_cur { TypeRole::BodyStrong } else { TypeRole::Body };
                let font = eng.font(role, None, m, 1.0);
                let lx = rc.left + p(40);
                let room = rc.right - p(8) - lx;
                let l = eng.layout(title, font, &LayoutOpts { max_width: Some(room), trim: Trim::End, wrap: false });
                let at = PointP { x: lx, y: rc.top + ((rc.bottom - rc.top) - l.height) / 2 };
                eng.draw(cv, &l, at, Ink::white(a));
            }
        })
    }

    #[test]
    fn current_entry_matches_settings_nav() {
        for dpi in [96, 144] {
            let mut l = list(36, nav_items());
            l.set_current(Some(1));
            let mut h = harness(dpi, l);
            h.hover(PointL { x: 100, y: 36 + 18 });
            h.advance(300);
            let img = h.render();
            let want = dock_nav(dpi, 0, Some(1));
            assert_eq!(r1::first_diff(&img.px, &want, img.w as i32), None, "current and hot at {dpi}");
        }
    }

    fn tree_items() -> Vec<SidebarItem> {
        let mut places = item(10, "Places", 0);
        places.section = true;
        let mut a = item(1, "Alpha", 0);
        a.expandable = true;
        a.expanded = true;
        let mut b = item(4, "Beta", 0);
        b.expandable = true;
        vec![places, a, item(2, "Alpha one", 1), item(3, "Alpha two", 1), b]
    }

    fn focused_index(h: &Harness<Host<SidebarList>>) -> Option<usize> {
        h.focused().map(|n| n.slot() as usize - 1)
    }

    #[test]
    fn tree_keys() {
        let mut h = harness(96, list(28, tree_items()));
        h.key("Tab");
        assert_eq!(focused_index(&h), Some(1), "the section is skipped");
        h.key("Down");
        assert_eq!(focused_index(&h), Some(2));
        h.key("Left");
        assert_eq!(focused_index(&h), Some(1), "Left on a leaf goes to the parent");
        h.key("Left");
        assert_eq!(h.screen().outs, [SidebarOut::Expand(1, false)], "Left on an expanded node collapses");
        h.key("Right");
        assert_eq!(focused_index(&h), Some(2), "Right on an expanded node enters it");
        h.key("End");
        assert_eq!(focused_index(&h), Some(4));
        h.key("Right");
        assert_eq!(h.screen().outs.last(), Some(&SidebarOut::Expand(4, true)));
        h.key("Up").key("Up").key("Up").key("Up");
        assert_eq!(focused_index(&h), Some(1), "Up stops at the first live row");
        h.key("Home");
        assert_eq!(focused_index(&h), Some(1));
        h.key("Enter");
        assert_eq!(h.screen().outs.last(), Some(&SidebarOut::Activated(1)));
        let dump = h.a11y_dump();
        assert!(dump.starts_with("Tree\n"), "{dump}");
        assert!(dump.contains("TreeItem \"Alpha one\"") && dump.contains("level 2"), "{dump}");
    }

    #[test]
    fn clicks_and_context() {
        let mut h = harness(96, list(28, tree_items()));
        // Row 2 (Alpha one) spans y 56..84; the expander of row 1 is at x 8..20, y 28..56.
        h.click(PointL { x: 100, y: 70 });
        assert_eq!(h.screen().outs, [SidebarOut::Activated(2)]);
        h.click(PointL { x: 14, y: 42 });
        assert_eq!(h.screen().outs.last(), Some(&SidebarOut::Expand(1, false)));
        h.right_click(PointL { x: 100, y: 70 });
        assert!(matches!(h.screen().outs.last(), Some(SidebarOut::ContextMenu(2, _))));
        h.click(PointL { x: 100, y: 14 });
        assert_eq!(h.screen().outs.len(), 3, "a section header does nothing");
    }

    fn typeahead_items() -> Vec<SidebarItem> {
        vec![item(1, "Alpha", 0), item(2, "Beta", 0), item(3, "Bravo", 0), item(4, "Charlie", 0)]
    }

    #[test]
    fn typeahead_resets_after_1000_ms() {
        let mut h = harness(96, list(28, typeahead_items()));
        h.key("Tab");
        h.type_text("b");
        assert_eq!(focused_index(&h), Some(1));
        h.type_text("r");
        assert_eq!(focused_index(&h), Some(2), "the text grows: br");
        h.advance(1000);
        h.type_text("a");
        assert_eq!(focused_index(&h), Some(2), "1000 ms is not yet a reset: bra");
        h.advance(1001);
        h.type_text("c");
        assert_eq!(focused_index(&h), Some(3), "a reset: c");
        h.advance(1001);
        h.type_text("b");
        assert_eq!(focused_index(&h), Some(1), "b again from after Charlie wraps to Beta");
    }

    #[test]
    fn disabled_rows_are_skipped_and_inert() {
        let mut items = typeahead_items();
        items[1].disabled = true;
        let mut h = harness(96, list(28, items));
        h.key("Tab").key("Down");
        assert_eq!(focused_index(&h), Some(2));
        h.click(PointL { x: 100, y: 42 });
        assert!(h.screen().outs.is_empty());
    }

    #[test]
    fn trailing_busy_and_drop_target_draw() {
        let mut items = typeahead_items();
        items[0].trailing = Some((Text::from("12"), FgRole::Attention));
        items[1].busy = true;
        let mut l = list(28, items);
        l.set_drop_target(Some(3));
        let mut h = harness(96, l);
        let img = h.render();
        // The trailing text sits at the right of its row, the busy chip too; the drop target is a plate with a rim.
        let ink = |y0: i32, y1: i32, x0: i32, x1: i32| {
            img.crop(RectP::wh(x0, y0, x1 - x0, y1 - y0)).px.iter().any(|p| p & 0xFFFFFF != 0)
        };
        assert!(ink(0, 28, 150, 192), "trailing text");
        assert!(ink(28, 56, 150, 192), "busy chip");
        assert!(ink(56, 84, 8, 12), "drop target plate");
    }
}
