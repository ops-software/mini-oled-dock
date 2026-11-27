//! Button (F-K1, P2-kit 2.6): a secondary (FILL_CONTROL plate) or primary (FILL_SELECTED plate) text button, the states of
//! SPEC-platform 5.3.4 and the two mockups.

use super::paint::{lerp_ink, text_centered};
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::theme::{Ink, TypeRole};

/// A secondary (FILL_CONTROL plate) or primary (FILL_SELECTED plate) button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Secondary,
    Primary,
}

/// What a button (or an IconButton) tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonOut {
    Clicked,
}

/// Horizontal padding, logical px.
const PAD: i32 = 12;

/// A text button.
pub struct Button {
    id: WidgetId,
    label: Text,
    kind: ButtonKind,
    enabled: bool,
    busy: bool,
    danger: bool,
    rect: RectP,
}

impl Button {
    pub fn new(id: WidgetId, label: Text, kind: ButtonKind) -> Button {
        Button { id, label, kind, enabled: true, busy: false, danger: false, rect: RectP::EMPTY }
    }

    pub fn set_label(&mut self, t: Text) {
        self.label = t;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// The label in ACCENT_DANGER while the button is live; plate and rims are unchanged.
    pub fn set_danger(&mut self, on: bool) {
        self.danger = on;
    }

    /// The label in FG_SECONDARY with a progress line, inert.
    pub fn set_busy(&mut self, on: bool) {
        self.busy = on;
    }

    /// The button's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// True while it answers the pointer and the keys.
    fn live(&self) -> bool {
        self.enabled && !self.busy
    }
}

impl Widget for Button {
    type Out = ButtonOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let mut flags = NodeFlags::HOVER_FADE;
        if !self.enabled {
            flags = flags | NodeFlags::DISABLED;
        }
        if self.busy {
            flags = flags | NodeFlags::BUSY;
        }
        let focus = if self.enabled { Focusable::Tab } else { Focusable::No };
        let node = Node::new(self.node_id(), Role::Button, rect)
            .flags(flags)
            .focus(focus)
            .label(self.label.clone())
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let r = self.rect;
        let id = self.node_id();
        let radius = cx.px(cx.tokens().radius.s);
        let c = &cx.tokens().color;
        // (rest fill, hot fill, pressed fill, rest text, hot text, rim)
        let (rest, hot, press, fg, fg_hot, rim) = match self.kind {
            ButtonKind::Secondary => {
                (c.fill_control, c.fill_control_hot, c.fill_press, c.fg_primary, c.fg_hot, Some(c.rim_control))
            }
            ButtonKind::Primary => {
                (c.fill_selected, c.fill_selected_hot, c.fill_selected_press, c.fg_on_selected, c.fg_on_selected, None)
            }
        };
        // Busy: the label dims and a line runs along the bottom, in the colours that read on the plate.
        let (off, disabled_fg) = (c.fill_control_off, c.fg_disabled);
        let (busy_fg, busy_line) = match self.kind {
            ButtonKind::Secondary => (c.fg_secondary, c.fg_primary),
            ButtonKind::Primary => (Ink { a: 170, ..c.fg_on_selected }, c.fg_on_selected),
        };
        let (fill, ink, rim) = if !self.enabled {
            (off, disabled_fg, None)
        } else if self.busy {
            (rest, busy_fg, rim)
        } else if cx.pressed(id) {
            (press, fg_hot, rim)
        } else {
            let t = cx.hover_t(id);
            (lerp_ink(rest, hot, t), lerp_ink(fg, fg_hot, t), rim)
        };
        let ink = if self.danger && self.live() { c.accent_danger } else { ink };
        cx.fill(r, radius, fill);
        if let Some(rim) = rim {
            cx.stroke(r, radius, 1, rim);
        }
        if self.busy {
            let line = cx.px(2);
            cx.fill(RectP { l: r.l + radius, t: r.b - line, r: r.r - radius, b: r.b }, 0, busy_line);
        }
        text_centered(cx, &self.label, TypeRole::Body, r.inset(cx.px(PAD), 0), ink);
        if cx.focused(id) {
            cx.focus_ring(r, radius, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ButtonOut> {
        if cx.target() != Some(self.node_id()) || !self.live() {
            return None;
        }
        match ev {
            Event::Click { button: MouseButton::Left, .. } => Some(ButtonOut::Clicked),
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                Some(ButtonOut::Clicked)
            }
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                (!*down).then_some(ButtonOut::Clicked)
            }
            Event::A11y(_, A11yAction::Click) => Some(ButtonOut::Clicked),
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let font = cx.font(TypeRole::Body);
        let w = cx.text().width(&self.label, font);
        SizeP { w: w + 2 * cx.px(PAD), h: cx.px(cx.tokens().size.control_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Input;
    use crate::event::Mods;
    use crate::geom::{PointL, PointP, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    const SIZE: SizeL = SizeL { w: 120, h: 44 };

    fn harness(kind: ButtonKind, f: impl FnOnce(&mut Button)) -> Harness<Host<Button>> {
        let mut b = Button::new(WidgetId(1), Text::from("OK"), kind);
        f(&mut b);
        Harness::new(Host::new(b, (10, 8, 100, 28)), SIZE, 96, Tokens::oled())
    }

    /// A pixel of the plate left of the label (no text there), as its blue channel over the black window.
    fn plate(h: &mut Harness<Host<Button>>) -> u32 {
        let img = h.render();
        img.px[(22 * img.w + 18) as usize] & 0xFF
    }

    fn centre() -> PointL {
        PointL { x: 60, y: 22 }
    }

    #[test]
    fn button_states() {
        // Secondary: 16 at rest, 28 hot, 36 pressed, 10 disabled; the label is FG_HOT when hot.
        let mut h = harness(ButtonKind::Secondary, |_| {});
        assert_eq!(plate(&mut h), 16);
        h.hover(centre());
        h.advance(200);
        assert!(h.is_idle(), "the fade ended");
        assert_eq!(plate(&mut h), 28);
        h.input(Input::Down(PointP { x: 60, y: 22 }, MouseButton::Left, 1, Mods::NONE));
        assert_eq!(plate(&mut h), 36, "pressed");
        let mut h = harness(ButtonKind::Secondary, |b| b.set_enabled(false));
        assert_eq!(plate(&mut h), 10);
        // Primary: 235 at rest, 255 hot, 200 pressed.
        let mut h = harness(ButtonKind::Primary, |_| {});
        assert_eq!(plate(&mut h), 235);
        h.hover(centre());
        h.advance(200);
        assert_eq!(plate(&mut h), 255);
        h.input(Input::Down(PointP { x: 60, y: 22 }, MouseButton::Left, 1, Mods::NONE));
        assert_eq!(plate(&mut h), 200);
    }

    #[test]
    fn hover_fades_in_between() {
        let mut h = harness(ButtonKind::Secondary, |_| {});
        h.hover(centre());
        h.advance(45);
        let mid = plate(&mut h);
        assert!(mid > 16 && mid < 28, "{mid}");
    }

    #[test]
    fn focus_ring_by_tab_and_busy_line() {
        let mut h = harness(ButtonKind::Secondary, |_| {});
        let before = h.render().px.iter().filter(|p| **p & 0xFF != 0).count();
        h.key("Tab");
        let after = h.render().px.iter().filter(|p| **p & 0xFF != 0).count();
        assert!(after > before + 100, "the ring was drawn ({before} -> {after})");
        let mut h = harness(ButtonKind::Secondary, |b| b.set_busy(true));
        let img = h.render();
        // The bottom line inside the plate: FG_PRIMARY (230) over the fill, 2 px tall.
        assert!(img.px[(8 + 28 - 1) * 120 + 60] & 0xFF >= 230);
        assert!(img.px[(8 + 28 - 2) * 120 + 60] & 0xFF >= 230);
    }

    #[test]
    fn button_activates_by_keys() {
        let mut h = harness(ButtonKind::Secondary, |_| {});
        h.key("Tab");
        h.key("Enter");
        assert_eq!(h.screen().outs.len(), 1);
        h.input(Input::Key(Key::SPACE, true, false, Mods::NONE));
        assert_eq!(h.screen().outs.len(), 1, "Space acts on release");
        h.input(Input::Key(Key::SPACE, false, false, Mods::NONE));
        assert_eq!(h.screen().outs.len(), 2);
        h.click(centre());
        assert_eq!(h.screen().outs.len(), 3);
        assert!(h.screen().outs.iter().all(|o| *o == ButtonOut::Clicked));
    }

    #[test]
    fn disabled_and_busy_buttons_are_inert() {
        for f in [|b: &mut Button| b.set_enabled(false), |b: &mut Button| b.set_busy(true)] {
            let mut h = harness(ButtonKind::Primary, f);
            h.key("Tab").key("Enter");
            h.click(centre());
            assert!(h.screen().outs.is_empty());
        }
        let h = harness(ButtonKind::Primary, |b| b.set_enabled(false));
        assert!(h.a11y_dump().contains("[disabled]"));
    }

    #[test]
    fn the_node_names_the_button() {
        let h = harness(ButtonKind::Secondary, |_| {});
        assert_eq!(h.a11y_dump(), "Button \"OK\" [focusable]\n");
        h.assert_named();
    }
}
