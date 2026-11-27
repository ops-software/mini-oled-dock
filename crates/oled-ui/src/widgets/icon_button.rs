//! IconButton (F-K1, P2-kit 2.6): a 28 x 28 glyph button; its tip is the chip text and the accessible name.

use super::button::ButtonOut;
use super::paint::lerp_ink;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::glyph::Glyph;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::theme::Ink;

/// A 28 x 28 glyph button; its tip is the chip text and the accessible name.
pub struct IconButton {
    id: WidgetId,
    glyph: Glyph,
    tip: Text,
    toggled: Option<bool>,
    enabled: bool,
    /// The plate: control_h square, centred in the rect the screen gave.
    plate: RectP,
}

impl IconButton {
    pub fn new(id: WidgetId, glyph: Glyph, tip: Text) -> IconButton {
        IconButton { id, glyph, tip, toggled: None, enabled: true, plate: RectP::EMPTY }
    }

    /// Some(on) makes it a toggle button.
    pub fn set_toggled(&mut self, v: Option<bool>) {
        self.toggled = v;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }
}

impl Widget for IconButton {
    type Out = ButtonOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let side = cx.px(cx.tokens().size.control_h);
        let (cx0, cy0) = ((rect.l + rect.r) / 2, (rect.t + rect.b) / 2);
        self.plate = RectP::wh(cx0 - side / 2, cy0 - side / 2, side, side);
        let mut flags = NodeFlags::HOVER_FADE;
        if !self.enabled {
            flags = flags | NodeFlags::DISABLED;
        }
        if self.toggled == Some(true) {
            flags = flags | NodeFlags::CHECKED;
        }
        let focus = if self.enabled { Focusable::Tab } else { Focusable::No };
        let node = Node::new(self.node_id(), Role::Button, self.plate)
            .flags(flags)
            .focus(focus)
            .label(self.tip.clone())
            .description(self.tip.clone())
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let r = self.plate;
        let id = self.node_id();
        let radius = cx.px(cx.tokens().radius.s);
        let icon = cx.px(cx.tokens().size.icon_s);
        let c = &cx.tokens().color;
        let (hover, press, control, rim, rest_fg, hot_fg, off_fg) =
            (c.fill_hover, c.fill_press, c.fill_control, c.rim_control, c.fg_secondary, c.fg_hot, c.fg_disabled);
        let clear = Ink { a: 0, ..hover };
        let (plate, glyph) = if !self.enabled {
            (None, off_fg)
        } else if cx.pressed(id) {
            (Some(press), hot_fg)
        } else {
            let t = cx.hover_t(id);
            let fill = if self.toggled == Some(true) { lerp_ink(control, hover, t) } else { lerp_ink(clear, hover, t) };
            (Some(fill), lerp_ink(rest_fg, hot_fg, t))
        };
        if let Some(p) = plate {
            cx.fill(r, radius, p);
        }
        if self.toggled == Some(true) && self.enabled {
            cx.stroke(r, radius, 1, rim);
        }
        let g = RectP::wh(r.l + (r.w() - icon) / 2, r.t + (r.h() - icon) / 2, icon, icon);
        cx.glyph(g, self.glyph, glyph);
        if cx.focused(id) {
            cx.focus_ring(r, radius, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ButtonOut> {
        if cx.target() != Some(self.node_id()) || !self.enabled {
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
        let s = cx.px(cx.tokens().size.control_h);
        SizeP { w: s, h: s }
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

    fn harness(f: impl FnOnce(&mut IconButton)) -> Harness<Host<IconButton>> {
        let mut b = IconButton::new(WidgetId(1), Glyph::ChevronLeft, Text::from("Back \u{b7} Alt+\u{2190}"));
        f(&mut b);
        Harness::new(Host::new(b, (10, 8, 28, 28)), SizeL { w: 48, h: 44 }, 96, Tokens::oled())
    }

    /// A plate pixel away from the glyph (the top-left corner area), as its blue channel.
    fn plate(h: &mut Harness<Host<IconButton>>) -> u32 {
        let img = h.render();
        img.px[(11 * img.w + 13) as usize] & 0xFF
    }

    #[test]
    fn icon_button_has_tip_as_name() {
        let mut h = harness(|_| {});
        assert_eq!(h.a11y_dump(), "Button \"Back \u{b7} Alt+\u{2190}\" [focusable]\n");
        h.assert_named();
        let n = h.dispatcher().frame.iter().next().cloned().unwrap();
        assert_eq!(n.description.as_deref(), Some("Back \u{b7} Alt+\u{2190}"));
    }

    #[test]
    fn icon_button_states() {
        let mut h = harness(|_| {});
        assert_eq!(plate(&mut h), 0, "no plate at rest");
        h.hover(PointL { x: 24, y: 22 });
        h.advance(200);
        assert_eq!(plate(&mut h), 20, "FILL_HOVER");
        h.input(Input::Down(PointP { x: 24, y: 22 }, MouseButton::Left, 1, Mods::NONE));
        assert_eq!(plate(&mut h), 36, "FILL_PRESS");
        let mut h = harness(|b| b.set_toggled(Some(true)));
        assert_eq!(plate(&mut h), 16, "a toggle that is on: FILL_CONTROL");
        assert!(h.a11y_dump().contains("checked"));
        let mut h = harness(|b| b.set_enabled(false));
        let lit = h.render().px.iter().filter(|p| **p & 0xFF != 0).count();
        assert!(lit > 0 && lit < 60, "the glyph at FG_DISABLED only: {lit}");
        h.key("Tab").key("Enter");
        assert!(h.screen().outs.is_empty());
    }

    #[test]
    fn icon_button_clicks() {
        let mut h = harness(|_| {});
        h.click(PointL { x: 24, y: 22 });
        h.key("Tab").key("Enter");
        assert_eq!(h.screen().outs, [ButtonOut::Clicked, ButtonOut::Clicked]);
    }
}
