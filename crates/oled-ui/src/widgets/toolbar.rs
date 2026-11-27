//! Toolbar (F-K2b, P2-kit 2.9): TOOLBAR_H 44. Page tabs and 28 x 28 icon buttons form one Tab stop with roving
//! Left/Right (the item that holds the focus is the stop); the optional filter field is its own stop, because its arrow
//! keys move its caret. Status text sits left of the field.

use super::page_tabs::{PageTabs, PageTabsOut};
use super::paint::lerp_ink;
use super::text_field::{FieldOut, TextField};
use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::glyph::Glyph;
use crate::model::{Align, FgRole};
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// A toolbar button: a command with its glyph, tip and toggle state.
pub struct ToolbarButton {
    pub command: CommandId,
    pub glyph: Glyph,
    pub tip: Text,
    pub toggled: Option<bool>,
}

/// What a toolbar tells its screen.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolbarOut {
    Command(CommandId),
    Tabs(PageTabsOut),
    Field(FieldOut),
}

/// The slot of the first button; the tabs take slots 1 and up, the group node is slot 0.
const BUTTON_SLOT: u32 = 100;
/// Left margin, gap between the groups, gap between buttons, gap before the field (logical px).
const MARGIN: i32 = 16;
const GROUP_GAP: i32 = 12;
const BUTTON_GAP: i32 = 4;
const FIELD_GAP: i32 = 8;
const BUTTON_SIDE: i32 = 28;

/// The toolbar: page tabs, buttons, a field and a status text.
pub struct Toolbar {
    id: WidgetId,
    tabs: Option<PageTabs>,
    buttons: Vec<ToolbarButton>,
    field: Option<TextField>,
    status: Option<(Text, FgRole)>,
    /// The button plates and the status text's room, from the last layout.
    plates: Vec<RectP>,
    status_rect: RectP,
    /// The item (tabs first, then buttons) that is the Tab stop: the last one that held the focus.
    cur: usize,
    rect: RectP,
}

impl Toolbar {
    /// A toolbar takes the widget ids `id` + 1 and up for nothing: its tabs and buttons are nodes of `id` itself.
    pub fn new(id: WidgetId, tabs: Option<PageTabs>, buttons: Vec<ToolbarButton>, field: Option<TextField>) -> Toolbar {
        let mut tabs = tabs;
        if let Some(t) = tabs.as_mut() {
            t.adopt(id, 1);
        }
        let cur = tabs.as_ref().map_or(0, PageTabs::selected);
        Toolbar {
            id,
            tabs,
            buttons,
            field,
            status: None,
            plates: Vec::new(),
            status_rect: RectP::EMPTY,
            cur,
            rect: RectP::EMPTY,
        }
    }

    pub fn tabs_mut(&mut self) -> Option<&mut PageTabs> {
        self.tabs.as_mut()
    }

    pub fn field_mut(&mut self) -> Option<&mut TextField> {
        self.field.as_mut()
    }

    /// Tasks' "Every 2 s".
    pub fn set_status(&mut self, s: Option<(Text, FgRole)>) {
        self.status = s;
    }

    fn group_node(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn tab_count(&self) -> usize {
        self.tabs.as_ref().map_or(0, PageTabs::len)
    }

    /// The node of item i: tabs first, then the buttons.
    fn item_node(&self, i: usize) -> NodeId {
        let t = self.tab_count();
        if i < t {
            NodeId::new(self.id, 1 + i as u32)
        } else {
            NodeId::new(self.id, BUTTON_SLOT + (i - t) as u32)
        }
    }

    fn item_of(&self, n: NodeId) -> Option<usize> {
        if n.owner() != self.id {
            return None;
        }
        let t = self.tab_count();
        let s = n.slot();
        if s >= 1 && ((s - 1) as usize) < t {
            Some((s - 1) as usize)
        } else if s >= BUTTON_SLOT && ((s - BUTTON_SLOT) as usize) < self.buttons.len() {
            Some(t + (s - BUTTON_SLOT) as usize)
        } else {
            None
        }
    }

    fn items(&self) -> usize {
        self.tab_count() + self.buttons.len()
    }

    /// Activates item i: a tab selects, a button runs its command.
    fn activate(&mut self, cx: &mut EventCx, i: usize) -> Option<ToolbarOut> {
        let t = self.tab_count();
        if i < t {
            let tabs = self.tabs.as_mut()?;
            tabs.pick(cx, i).map(ToolbarOut::Tabs)
        } else {
            let b = self.buttons.get(i - t)?;
            Some(ToolbarOut::Command(b.command))
        }
    }

    fn item_event(&mut self, cx: &mut EventCx, i: usize, ev: &Event) -> Option<ToolbarOut> {
        match ev {
            Event::FocusIn(_) => {
                if self.cur != i {
                    self.cur = i;
                    cx.relayout();
                }
                None
            }
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click) => self.activate(cx, i),
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                self.activate(cx, i)
            }
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                if *down {
                    None
                } else {
                    self.activate(cx, i)
                }
            }
            Event::Key { key, down: true, .. } => {
                let last = self.items().checked_sub(1)?;
                let to = match *key {
                    Key::LEFT => i.saturating_sub(1),
                    Key::RIGHT => (i + 1).min(last),
                    Key::HOME => 0,
                    Key::END => last,
                    _ => return None,
                };
                cx.consume();
                if to < self.tab_count() {
                    // Arriving on a tab selects it, as in PageTabs.
                    self.activate(cx, to)
                } else {
                    cx.set_focus(self.item_node(to));
                    None
                }
            }
            _ => None,
        }
    }
}

impl Widget for Toolbar {
    type Out = ToolbarOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let side = cx.px(BUTTON_SIDE);
        let y = rect.t + (rect.h() - side) / 2;
        let mut x = rect.l + cx.px(MARGIN);
        cx.push(Node::new(self.group_node(), Role::Toolbar, rect));
        if let Some(t) = self.tabs.as_mut() {
            x += t.measure(cx, x, rect.t);
            x += cx.px(GROUP_GAP);
        }
        let stop = self.cur.min(self.items().saturating_sub(1));
        if let Some(t) = self.tabs.as_ref() {
            let tab_stop = if stop < t.len() { stop } else { usize::MAX };
            t.push_nodes(cx, self.group_node(), Some(tab_stop));
        }
        self.plates.clear();
        let t = self.tab_count();
        for (i, b) in self.buttons.iter().enumerate() {
            let plate = RectP::wh(x, y, side, side);
            self.plates.push(plate);
            x += side + cx.px(BUTTON_GAP);
            let mut flags = NodeFlags::HOVER_FADE;
            if b.toggled == Some(true) {
                flags = flags | NodeFlags::CHECKED;
            }
            let focus = if t + i == stop { Focusable::Tab } else { Focusable::Roving };
            cx.push(
                Node::new(NodeId::new(self.id, BUTTON_SLOT + i as u32), Role::Button, plate)
                    .parent(self.group_node())
                    .flags(flags)
                    .focus(focus)
                    .label(b.tip.clone())
                    .description(b.tip.clone())
                    .actions(Actions::CLICK | Actions::FOCUS),
            );
        }
        let mut right = rect.r - cx.px(MARGIN);
        if let Some(f) = self.field.as_mut() {
            let w = f.preferred(cx).w.min((right - x).max(0));
            f.layout(cx, RectP { l: right - w, t: rect.t, r: right, b: rect.b });
            right -= w + cx.px(FIELD_GAP);
        }
        self.status_rect = RectP { l: x + cx.px(GROUP_GAP), t: rect.t, r: right, b: rect.b };
    }

    fn paint(&self, cx: &mut PaintCx) {
        if let Some(t) = self.tabs.as_ref() {
            t.paint_tabs(cx, self.rect.t);
        }
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        let icon = cx.px(cx.tokens().size.icon_s);
        for (i, (b, plate)) in self.buttons.iter().zip(&self.plates).enumerate() {
            let id = NodeId::new(self.id, BUTTON_SLOT + i as u32);
            let (fill, glyph) = if cx.pressed(id) {
                (c.fill_press, c.fg_hot)
            } else {
                let t = cx.hover_t(id);
                let clear = Ink { a: 0, ..c.fill_hover };
                let base = if b.toggled == Some(true) {
                    lerp_ink(c.fill_control, c.fill_hover, t)
                } else {
                    lerp_ink(clear, c.fill_hover, t)
                };
                (base, lerp_ink(c.fg_secondary, c.fg_hot, t))
            };
            cx.fill(*plate, radius, fill);
            if b.toggled == Some(true) {
                cx.stroke(*plate, radius, 1, c.rim_control);
            }
            let g = RectP::wh(plate.l + (plate.w() - icon) / 2, plate.t + (plate.h() - icon) / 2, icon, icon);
            cx.glyph(g, b.glyph, glyph);
            if cx.focused(id) {
                cx.focus_ring(*plate, radius, false);
            }
        }
        if let Some((text, role)) = &self.status {
            let ink = super::paint::fg_ink(*role, cx.tokens());
            cx.text_in(text, TypeRole::Caption, self.status_rect, Align::End, Trim::End, ink);
        }
        if let Some(f) = self.field.as_ref() {
            f.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ToolbarOut> {
        if let Some(i) = cx.target().and_then(|t| self.item_of(t)) {
            return self.item_event(cx, i, ev);
        }
        if cx.target().is_some_and(|t| t.owner() == self.id) {
            return None;
        }
        self.field.as_mut().and_then(|f| f.event(cx, ev)).map(ToolbarOut::Field)
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: 0, h: cx.px(cx.tokens().size.toolbar_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;
    use crate::widgets::text_field::FieldKind;

    const REFRESH: CommandId = CommandId(901);
    const PAUSE: CommandId = CommandId(902);

    fn toolbar(with_field: bool) -> Toolbar {
        let tabs = PageTabs::new(WidgetId(2), vec![Text::from("Processes"), Text::from("Performance")]);
        let buttons = vec![
            ToolbarButton { command: REFRESH, glyph: Glyph::Search, tip: Text::from("Refresh"), toggled: None },
            ToolbarButton { command: PAUSE, glyph: Glyph::Pin, tip: Text::from("Pause"), toggled: Some(true) },
        ];
        let field = with_field.then(|| {
            let mut f = TextField::new(WidgetId(3), FieldKind::Search);
            f.set_placeholder(Text::from("Search"));
            f
        });
        Toolbar::new(WidgetId(1), Some(tabs), buttons, field)
    }

    fn harness(with_field: bool) -> Harness<Host<Toolbar>> {
        Harness::new(Host::new(toolbar(with_field), (0, 0, 640, 44)), SizeL { w: 640, h: 44 }, 96, Tokens::oled())
    }

    #[test]
    fn toolbar_is_one_tab_stop() {
        let mut h = harness(true);
        let stops = h.dispatcher().frame.iter().filter(|n| n.focus == Focusable::Tab).count();
        assert_eq!(stops, 2, "the tabs and buttons are one stop, the field another");
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)));
        h.key("Right").key("Right");
        assert_eq!(
            h.focused(),
            Some(NodeId::new(WidgetId(1), BUTTON_SLOT)),
            "Right goes on across the tabs to the buttons"
        );
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(3), 0)), "Tab leaves the group for the field");
        h.key("Shift+Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), BUTTON_SLOT)), "and Shift+Tab returns to the item left");
        let stops = h.dispatcher().frame.iter().filter(|n| n.focus == Focusable::Tab).count();
        assert_eq!(stops, 2);
    }

    #[test]
    fn roving_runs_tabs_then_buttons() {
        let mut h = harness(false);
        h.key("Tab").key("End");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), BUTTON_SLOT + 1)));
        h.key("Enter");
        assert_eq!(h.screen().outs.last(), Some(&ToolbarOut::Command(PAUSE)));
        h.key("Left").key("Space");
        assert_eq!(h.screen().outs.last(), Some(&ToolbarOut::Command(REFRESH)));
        h.key("Home");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)), "Home goes back to the first tab, already selected");
        h.key("Right");
        assert_eq!(h.screen().outs.last(), Some(&ToolbarOut::Tabs(PageTabsOut::Selected(1))));
        assert_eq!(h.screen().w.tabs.as_ref().map(PageTabs::selected), Some(1));
    }

    #[test]
    fn buttons_have_tips_and_the_field_works() {
        let mut h = harness(true);
        let dump = h.a11y_dump();
        assert!(
            dump.contains("Toolbar") && dump.contains("Button \"Refresh\"") && dump.contains("SearchInput"),
            "{dump}"
        );
        h.assert_named();
        let tip = h
            .dispatcher()
            .frame
            .iter()
            .find(|n| n.label == crate::node::Label::Text(Text::from("Pause")))
            .cloned()
            .unwrap();
        assert_eq!(tip.description.as_deref(), Some("Pause"));
        assert!(tip.flags.contains(NodeFlags::CHECKED));
        let field = h.dispatcher().frame.iter().find(|n| n.role == Role::SearchInput).map(|n| n.rect).unwrap();
        h.click(PointL { x: field.l + 40, y: 22 });
        h.type_text("abc").key("Enter");
        assert_eq!(h.screen().outs.last(), Some(&ToolbarOut::Field(FieldOut::Committed("abc".into()))));
        assert_eq!(field.h(), 28);
        assert_eq!(field.r, 640 - 16, "the field ends at the margin");
        assert_eq!(field.w(), 240);
    }

    #[test]
    fn click_runs_a_command_and_status_is_drawn() {
        let mut h = harness(true);
        h.screen_mut().w.set_status(Some((Text::from("Paused"), FgRole::Attention)));
        let img = h.render();
        assert!(!img.text_rects.is_empty());
        let b = h.dispatcher().frame.iter().find(|n| n.role == Role::Button).map(|n| n.rect).unwrap();
        h.click(PointL { x: (b.l + b.r) / 2, y: 22 });
        assert_eq!(h.screen().outs, [ToolbarOut::Command(REFRESH)]);
        assert_eq!(b.h(), 28);
        assert_eq!(b.w(), 28);
    }
}
