//! Checkbox (F-K1, P2-kit 2.6): a 16 x 16 box with a label; no dock twin, so the states come from SPEC-platform 5.3.4.

use super::paint::lerp_ink;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::glyph::Glyph;
use crate::model::Align;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::TypeRole;

/// What a checkbox tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOut {
    Changed(bool),
}

/// The box side, logical px.
const BOX: i32 = 16;
/// The gap between the box and the label.
const GAP: i32 = 8;
/// The mixed bar, logical px.
const BAR_W: i32 = 8;
const BAR_H: i32 = 2;

/// A checkbox with its label; mixed shows the third state.
pub struct Checkbox {
    id: WidgetId,
    label: Text,
    checked: bool,
    mixed: bool,
    enabled: bool,
    rect: RectP,
}

impl Checkbox {
    pub fn new(id: WidgetId, label: Text, checked: bool) -> Checkbox {
        Checkbox { id, label, checked, mixed: false, enabled: true, rect: RectP::EMPTY }
    }

    pub fn set_mixed(&mut self, v: bool) {
        self.mixed = v;
    }

    pub fn checked(&self) -> bool {
        self.checked
    }

    pub fn set_checked(&mut self, v: bool) {
        self.checked = v;
    }

    pub fn set_enabled(&mut self, v: bool) {
        self.enabled = v;
    }

    /// The checkbox's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// The box: at the left of the rect, centred on its middle.
    fn boxr(&self, side: i32) -> RectP {
        RectP::wh(self.rect.l, (self.rect.t + self.rect.b) / 2 - side / 2, side, side)
    }

    /// A click or a key: a mixed box becomes checked.
    fn flip(&mut self, cx: &mut EventCx) -> Option<CheckOut> {
        self.checked = if self.mixed { true } else { !self.checked };
        self.mixed = false;
        cx.damage(self.rect);
        Some(CheckOut::Changed(self.checked))
    }
}

impl Widget for Checkbox {
    type Out = CheckOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let mut flags = NodeFlags::HOVER_FADE;
        if self.checked {
            flags = flags | NodeFlags::CHECKED;
        }
        if self.mixed {
            flags = flags | NodeFlags::MIXED;
        }
        if !self.enabled {
            flags = flags | NodeFlags::DISABLED;
        }
        let focus = if self.enabled { Focusable::Tab } else { Focusable::No };
        let node = Node::new(self.node_id(), Role::CheckBox, rect)
            .flags(flags)
            .focus(focus)
            .label(self.label.clone())
            .value(NodeValue::Toggle(self.checked))
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        let side = cx.px(BOX);
        let r = self.boxr(side);
        let radius = cx.px(cx.tokens().radius.xs);
        let c = cx.tokens().color.clone();
        let t = if self.enabled { cx.hover_t(id) } else { 0.0 };
        let on = self.checked || self.mixed;
        if on {
            let fill = if self.enabled && cx.pressed(id) {
                c.fill_selected_press
            } else {
                lerp_ink(c.fill_selected, c.fill_selected_hot, t)
            };
            cx.fill(r, radius, fill);
            if self.mixed {
                let (bw, bh) = (cx.px(BAR_W), cx.px(BAR_H).max(1));
                let bar = RectP::wh(r.l + (side - bw) / 2, r.t + (side - bh) / 2, bw, bh);
                cx.fill(bar, 0, c.fg_on_selected);
            } else {
                cx.glyph(r, Glyph::Check, c.fg_on_selected);
            }
        } else {
            let fill = if self.enabled && cx.pressed(id) {
                c.fill_press
            } else {
                lerp_ink(c.fill_control, c.fill_control_hot, t)
            };
            cx.fill(r, radius, fill);
            cx.stroke(r, radius, 1, c.rim_control);
        }
        if !self.enabled {
            // Unavailable: the box fades into the background, as the Toggle does.
            cx.fill(RectP { l: r.l - 1, t: r.t - 1, r: r.r + 1, b: r.b + 1 }, radius, c.disabled_veil);
        }
        if !self.label.is_empty() {
            let room = RectP { l: r.r + cx.px(GAP), ..self.rect };
            let ink = if self.enabled { c.fg_primary } else { c.fg_disabled };
            cx.text_in(&self.label, TypeRole::Body, room, Align::Start, Trim::End, ink);
        }
        if cx.focused(id) {
            cx.focus_ring(r, radius, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<CheckOut> {
        if cx.target() != Some(self.node_id()) || !self.enabled {
            return None;
        }
        match ev {
            Event::Click { button: MouseButton::Left, .. } => self.flip(cx),
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                if *down {
                    None
                } else {
                    self.flip(cx)
                }
            }
            Event::A11y(_, A11yAction::Click) => self.flip(cx),
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let font = cx.font(TypeRole::Body);
        let label = if self.label.is_empty() { 0 } else { cx.text().width(&self.label, font) + cx.px(GAP) };
        SizeP { w: cx.px(BOX) + label, h: cx.px(cx.tokens().size.control_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Input;
    use crate::event::Mods;
    use crate::geom::{PointP, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    const SIZE: SizeL = SizeL { w: 120, h: 40 };

    fn harness(f: impl FnOnce(&mut Checkbox)) -> Harness<Host<Checkbox>> {
        let mut c = Checkbox::new(WidgetId(1), Text::from(""), false);
        f(&mut c);
        Harness::new(Host::new(c, (10, 8, 100, 24)), SIZE, 96, Tokens::oled())
    }

    /// The box's top-left pixel inside its corner arc (x 14, y 14: well inside), blue channel.
    fn px(h: &mut Harness<Host<Checkbox>>, x: u32, y: u32) -> u32 {
        let img = h.render();
        img.px[(y * img.w + x) as usize] & 0xFF
    }

    #[test]
    fn checkbox_states() {
        // The box spans x 10..26, y 12..28.
        let mut h = harness(|_| {});
        assert_eq!(px(&mut h, 14, 14), 16, "FILL_CONTROL");
        h.hover(crate::geom::PointL { x: 18, y: 20 });
        h.advance(200);
        assert_eq!(px(&mut h, 14, 14), 28, "FILL_CONTROL_HOT");
        let mut h = harness(|c| c.set_checked(true));
        assert_eq!(px(&mut h, 12, 20), 235, "FILL_SELECTED beside the tick");
        let mut h = harness(|c| c.set_mixed(true));
        assert_eq!(px(&mut h, 12, 20), 235);
        assert_eq!(px(&mut h, 18, 20), 0, "the bar is FG_ON_SELECTED black on the plate");
        let mut h = harness(|c| c.set_enabled(false));
        assert!(px(&mut h, 14, 14) < 16, "the veil dims the box");
    }

    #[test]
    fn checkbox_keys_and_mixed() {
        let mut h = harness(|_| {});
        h.key("Tab");
        h.input(Input::Key(Key::SPACE, true, false, Mods::NONE));
        assert!(h.screen().outs.is_empty(), "Space acts on release");
        h.input(Input::Key(Key::SPACE, false, false, Mods::NONE));
        assert_eq!(h.screen().outs, [CheckOut::Changed(true)]);
        h.click(crate::geom::PointL { x: 18, y: 20 });
        assert_eq!(h.screen().outs.last(), Some(&CheckOut::Changed(false)));
        let mut h = harness(|c| c.set_mixed(true));
        h.click(crate::geom::PointL { x: 18, y: 20 });
        assert_eq!(h.screen().outs, [CheckOut::Changed(true)], "mixed becomes checked");
        let _ = PointP { x: 0, y: 0 };
    }

    #[test]
    fn the_node_is_a_checkbox() {
        let h = harness(|c| c.set_mixed(true));
        assert!(h.a11y_dump().starts_with("CheckBox"), "{}", h.a11y_dump());
    }
}
