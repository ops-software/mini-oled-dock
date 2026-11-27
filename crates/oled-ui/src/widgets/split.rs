//! SplitView (F-K3, P2-kit 2.10; K3.2): two panes side by side with a 1 px SEPARATOR line between them and a SPLITTER_HIT
//! wide hit zone over it. The first pane is kept between min and max (logical px); dragging moves it, a double click resets
//! it to the default, and with the focus on the splitter Alt+Shift+Left/Right move it 16 px. The screen lays the
//! split out first, then asks `panes` for the two rects it fills.

use super::paint::lerp_ink;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::RectP;
use crate::gfx::scale::Scale;
use crate::node::{Actions, Cursor, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, WidgetId};
use crate::screen::Widget;
use crate::strings::k;

/// What a split view tells its screen: the first pane's new width (logical px), or a reset to the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOut {
    Moved(i32),
    Reset,
}

/// What the keys and the accessibility actions move the splitter by (logical px).
const STEP: i32 = 16;

/// Two panes side by side with a splitter.
pub struct SplitView {
    id: WidgetId,
    min: i32,
    max: i32,
    default: i32,
    first: i32,
    /// The rect and the scale of the last layout, and where the pointer took hold of the line (physical px).
    rect: RectP,
    scale: Scale,
    grip: i32,
}

impl SplitView {
    /// min, max and default: the first pane's width, logical px.
    pub fn new(id: WidgetId, min: i32, max: i32, default: i32) -> SplitView {
        SplitView {
            id,
            min,
            max,
            default,
            first: default.clamp(min, max.max(min)),
            rect: RectP::EMPTY,
            scale: Scale::default(),
            grip: 0,
        }
    }

    /// The first pane's width, logical px.
    pub fn first(&self) -> i32 {
        self.first
    }

    /// Sets it, kept within min..=max.
    pub fn set_first(&mut self, w: i32) {
        self.first = self.clamp(w);
    }

    /// The two panes inside `rect` (physical px), at the scale of the last layout: the first pane, then the second after
    /// the 1 px line.
    pub fn panes(&self, rect: RectP) -> (RectP, RectP) {
        let w = self.scale.px(self.first).clamp(0, rect.w().max(0));
        (RectP { r: rect.l + w, ..rect }, RectP { l: (rect.l + w + 1).min(rect.r), ..rect })
    }

    fn clamp(&self, w: i32) -> i32 {
        w.clamp(self.min, self.max.max(self.min))
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// The first pane's width now `w`: Some(Moved) when it changed.
    fn move_to(&mut self, cx: &mut EventCx, w: i32) -> Option<SplitOut> {
        let w = self.clamp(w);
        if w == self.first {
            return None;
        }
        self.first = w;
        cx.relayout();
        Some(SplitOut::Moved(w))
    }
}

impl Widget for SplitView {
    type Out = SplitOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        self.scale = cx.scale();
        let x = rect.l + self.scale.px(self.first);
        let hit = cx.px(cx.tokens().size.splitter_hit);
        let zone = RectP { l: x + 1 - hit / 2 - hit % 2, r: x + 1 + hit / 2, ..rect };
        let total = self.scale.logical(rect.w()).max(1);
        let percent = self.first as f32 * 100.0 / total as f32;
        cx.push(
            Node::new(self.node_id(), Role::Splitter, zone)
                .flags(NodeFlags::HOVER_FADE)
                .focus(Focusable::Tab)
                .cursor(Cursor::SizeWE)
                .label(cx.lang().kit(k::KIT_SPLITTER))
                .value(NodeValue::Percent(percent))
                .actions(Actions::FOCUS | Actions::INCREMENT | Actions::DECREMENT),
        );
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        let c = cx.tokens().color.clone();
        let x = self.rect.l + cx.px(self.first);
        let ink = if cx.pressed(id) { c.fg_secondary } else { lerp_ink(c.separator, c.fg_tertiary, cx.hover_t(id)) };
        cx.vline(x, self.rect.t, self.rect.b, ink);
        if cx.focused(id) {
            let ring = RectP { l: x - cx.px(2), r: x + 1 + cx.px(2), ..self.rect };
            cx.focus_ring(ring, cx.px(2), false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SplitOut> {
        if let Event::A11y(n, act) = ev {
            if *n != self.node_id() {
                return None;
            }
            return match act {
                A11yAction::Increment => self.move_to(cx, self.first + STEP),
                A11yAction::Decrement => self.move_to(cx, self.first - STEP),
                _ => None,
            };
        }
        if cx.target() != Some(self.node_id()) {
            return None;
        }
        match ev {
            Event::PointerDown { pos, button: MouseButton::Left, .. } => {
                self.grip = pos.x - (self.rect.l + self.scale.px(self.first));
                None
            }
            Event::DragStart { pos, button: MouseButton::Left, .. } | Event::DragMove { pos, .. } => {
                let w = self.scale.logical(pos.x - self.grip - self.rect.l);
                self.move_to(cx, w)
            }
            Event::Click { button: MouseButton::Left, clicks, .. } if *clicks >= 2 => {
                self.first = self.clamp(self.default);
                cx.relayout();
                Some(SplitOut::Reset)
            }
            Event::Key { key, down: true, mods, .. } if mods.alt() && mods.shift() && !mods.ctrl() => {
                let to = match *key {
                    Key::LEFT => self.first - STEP,
                    Key::RIGHT => self.first + STEP,
                    _ => return None,
                };
                cx.consume();
                self.move_to(cx, to)
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

    fn harness(dpi: u32) -> Harness<Host<SplitView>> {
        let w = SplitView::new(WidgetId(1), 160, 400, 200);
        Harness::new(Host::new(w, (0, 0, 600, 100)), SizeL { w: 600, h: 100 }, dpi, Tokens::oled())
    }

    fn line_x(h: &mut Harness<Host<SplitView>>) -> i32 {
        h.dispatcher().frame.iter().find(|n| n.role == Role::Splitter).map(|n| n.rect).map(|r| (r.l + r.r) / 2).unwrap()
    }

    #[test]
    fn split_clamps_and_resets() {
        let mut w = SplitView::new(WidgetId(1), 160, 400, 200);
        assert_eq!(w.first(), 200);
        w.set_first(100);
        assert_eq!(w.first(), 160);
        w.set_first(900);
        assert_eq!(w.first(), 400);
        assert_eq!(SplitView::new(WidgetId(1), 160, 400, 900).first(), 400, "the default is clamped too");
        // The panes at 100 % and the line between them.
        let mut h = harness(96);
        let (a, b) = h.screen().w.panes(RectP::wh(0, 0, 600, 100));
        assert_eq!((a.r, b.l, b.r), (200, 201, 600), "the line is the pixel between the panes");
        // A drag moves the first pane and stops at the bounds; every move is reported.
        h.drag(PointL { x: 200, y: 50 }, PointL { x: 300, y: 50 });
        assert_eq!(h.screen().w.first(), 300);
        assert_eq!(h.screen().outs.last(), Some(&SplitOut::Moved(300)));
        let x = line_x(&mut h);
        h.drag(PointL { x, y: 50 }, PointL { x: 590, y: 50 });
        assert_eq!(h.screen().w.first(), 400, "max");
        let x = line_x(&mut h);
        h.drag(PointL { x, y: 50 }, PointL { x: 20, y: 50 });
        assert_eq!(h.screen().w.first(), 160, "min");
        // A double click on the line resets it.
        let x = line_x(&mut h);
        h.double_click(PointL { x, y: 50 });
        assert_eq!(h.screen().outs.last(), Some(&SplitOut::Reset));
        assert_eq!(h.screen().w.first(), 200);
    }

    #[test]
    fn split_keys_move_16() {
        let mut h = harness(96);
        h.key("Tab");
        assert!(h.focused().is_some(), "the splitter is a Tab stop");
        h.key("Alt+Shift+Right");
        assert_eq!(h.screen().w.first(), 216);
        assert_eq!(h.screen().outs, [SplitOut::Moved(216)]);
        h.key("Alt+Shift+Left");
        h.key("Alt+Shift+Left");
        assert_eq!(h.screen().w.first(), 184);
        for _ in 0..4 {
            h.key("Alt+Shift+Left");
        }
        assert_eq!(h.screen().w.first(), 160, "stops at min");
        let n = h.screen().outs.len();
        h.key("Alt+Shift+Left");
        assert_eq!(h.screen().outs.len(), n, "no output when nothing moved");
        h.key("Left");
        assert_eq!(h.screen().w.first(), 160, "a plain arrow does nothing");
        assert!(h.a11y_dump().contains("Splitter"), "{}", h.a11y_dump());
        h.assert_named();
    }

    #[test]
    fn split_line_is_one_pixel_and_scales() {
        let mut h = harness(144);
        let img = h.render();
        let x = 300; // 200 logical px at 150 %: 1.5 -> 144 DPI is 1.5x
        let blue = |x: i32| img.px[(50 * img.w + x as u32) as usize] & 0xFF;
        assert_eq!(blue(x), 30, "SEPARATOR");
        assert_eq!(blue(x - 1), 0);
        assert_eq!(blue(x + 1), 0);
    }
}
