//! Slider (F-K1, P2-kit 2.6): the dock's slider (R1: popup.rs paint_slider and slider_value_at, the value text of
//! settings_view.rs) with hover, drag, focus and the keys of SPEC-platform 5.3.5.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::model::Align;
use crate::node::{Actions, Focusable, Node, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// What a slider tells its screen: Changed while it moves, Committed when it is let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderOut {
    Changed(i32),
    Committed(i32),
}

/// The room of the value text at the right, and its gap to the rail, logical px (settings_view.rs VALUE_W).
const VALUE_W: i32 = 52;
const VALUE_GAP: i32 = 8;

/// A slider from min to max by step, with an optional value text.
pub struct Slider {
    id: WidgetId,
    label: Text,
    min: i32,
    max: i32,
    step: i32,
    value: i32,
    value_text: Option<Text>,
    rect: RectP,
    /// The value when the drag began (a cancelled drag puts it back).
    start: i32,
}

impl Slider {
    pub fn new(id: WidgetId, label: Text, min: i32, max: i32, step: i32, value: i32) -> Slider {
        Slider {
            id,
            label,
            min,
            max,
            step,
            value: value.clamp(min, max.max(min)),
            value_text: None,
            rect: RectP::EMPTY,
            start: value,
        }
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    /// The value, kept within min..=max.
    pub fn set_value(&mut self, v: i32) {
        self.value = v.clamp(self.min, self.max.max(self.min));
    }

    pub fn set_value_text(&mut self, t: Option<Text>) {
        self.value_text = t;
    }

    /// The slider's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn hi(&self) -> i32 {
        self.max.max(self.min)
    }

    fn step(&self) -> i32 {
        self.step.max(1)
    }

    /// The rail's rect: the whole rect, less the value text when there is one.
    fn rail(&self, value_room: i32) -> RectP {
        if self.value_text.is_some() {
            RectP { r: self.rect.r - value_room, ..self.rect }
        } else {
            self.rect
        }
    }

    /// slider_value_at: the value for pointer x on the rail; the knob's centre travels between rail.l + knob_r and
    /// rail.r - knob_r. Rounded to the nearest step.
    fn value_at(&self, rail: RectP, knob_r: i32, x: i32) -> i32 {
        let (l, r) = (rail.l + knob_r, rail.r - knob_r);
        let span = (r - l).max(1);
        let (lo, hi) = (i64::from(self.min), i64::from(self.hi()));
        let raw =
            lo + ((i64::from(x - l).clamp(0, i64::from(span))) * (hi - lo) + i64::from(span) / 2) / i64::from(span);
        let step = i64::from(self.step());
        let snapped = lo + ((raw - lo) + step / 2) / step * step;
        snapped.clamp(lo, hi) as i32
    }

    /// Sets the value; true if it changed.
    fn put(&mut self, cx: &mut EventCx, v: i32) -> bool {
        let v = v.clamp(self.min, self.hi());
        let moved = v != self.value;
        self.value = v;
        if moved {
            cx.damage(self.rect);
        }
        moved
    }

    fn rail_px(&self, cx: &EventCx) -> (RectP, i32) {
        (self.rail(cx.px(VALUE_W + VALUE_GAP)), cx.px(cx.tokens().size.slider_knob_r))
    }
}

impl Widget for Slider {
    type Out = SliderOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let node = Node::new(self.node_id(), Role::Slider, rect)
            .focus(Focusable::Tab)
            .label(self.label.clone())
            .value(NodeValue::Range {
                value: f64::from(self.value),
                min: f64::from(self.min),
                max: f64::from(self.hi()),
                step: f64::from(self.step()),
            })
            .actions(Actions::INCREMENT | Actions::DECREMENT | Actions::SET_VALUE | Actions::FOCUS);
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        let rail_room = cx.px(VALUE_W + VALUE_GAP);
        let sz = cx.tokens().size.clone();
        let c = cx.tokens().color.clone();
        let rc = self.rail(rail_room);
        let rest_r = cx.px(sz.slider_knob_r);
        let hot = cx.hot(id);
        let drag = cx.pressed(id);
        let knob_r = if hot || drag { cx.px(sz.slider_knob_r_hot) } else { rest_r };
        let (tl, tr) = (rc.l + rest_r, rc.r - rest_r);
        let cy = (rc.t + rc.b) / 2;
        let th = cx.px(sz.slider_track);
        let rail = RectP { l: rc.l, t: cy - th / 2, r: rc.r, b: cy - th / 2 + th };
        cx.fill(rail, th / 2, c.slider_rail);
        let span = i64::from((self.hi() - self.min).max(1));
        let kx = tl + (i64::from(tr - tl) * i64::from(self.value - self.min) / span) as i32;
        cx.fill(RectP { r: kx, ..rail }, th / 2, c.fg_primary);
        let knob = RectP { l: kx - knob_r, t: cy - knob_r, r: kx + knob_r, b: cy + knob_r };
        cx.fill(knob, knob_r, c.fg_hot);
        if let Some(text) = &self.value_text {
            let a = if hot { (c.fg_secondary.a + 25).min(255) } else { c.fg_secondary.a };
            let room = RectP { l: rc.r + cx.px(VALUE_GAP), ..self.rect };
            cx.text_in(text, TypeRole::Body, room, Align::End, Trim::End, Ink { a, ..c.fg_secondary });
        }
        if cx.focused(id) {
            cx.focus_ring(knob, knob_r, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SliderOut> {
        if cx.target() != Some(self.node_id()) {
            return None;
        }
        let (rail, kr) = self.rail_px(cx);
        match ev {
            Event::PointerDown { pos, button: MouseButton::Left, .. } => {
                self.start = self.value;
                let v = self.value_at(rail, kr, pos.x);
                self.put(cx, v).then_some(SliderOut::Changed(self.value))
            }
            Event::DragStart { pos, button: MouseButton::Left, .. } | Event::DragMove { pos, .. } => {
                let v = self.value_at(rail, kr, pos.x);
                self.put(cx, v).then_some(SliderOut::Changed(self.value))
            }
            Event::DragEnd { cancelled, .. } => {
                if *cancelled {
                    let v = self.start;
                    self.put(cx, v);
                }
                Some(SliderOut::Committed(self.value))
            }
            Event::Click { button: MouseButton::Left, .. } => Some(SliderOut::Committed(self.value)),
            Event::Key { key, down: true, .. } => {
                let page = ((self.hi() - self.min) / 10).max(self.step());
                let to = match *key {
                    Key::LEFT => self.value - self.step(),
                    Key::RIGHT => self.value + self.step(),
                    Key::PAGE_UP => self.value + page,
                    Key::PAGE_DOWN => self.value - page,
                    Key::HOME => self.min,
                    Key::END => self.hi(),
                    _ => return None,
                };
                cx.consume();
                self.put(cx, to);
                Some(SliderOut::Committed(self.value))
            }
            Event::A11y(_, action) => {
                let to = match action {
                    A11yAction::Increment => self.value + self.step(),
                    A11yAction::Decrement => self.value - self.step(),
                    A11yAction::SetValue(s) => s.trim().parse::<i32>().ok()?,
                    _ => return None,
                };
                self.put(cx, to);
                Some(SliderOut::Committed(self.value))
            }
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let room = if self.value_text.is_some() { cx.px(VALUE_W + VALUE_GAP) } else { 0 };
        SizeP { w: cx.px(160) + room, h: cx.px(cx.tokens().size.control_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, SizeL};
    use crate::gfx::scale::Scale;
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::w3::Canvas;
    use crate::widgets::paint::Host;
    use crate::widgets::toggle::r1;
    use windows::Win32::Foundation::RECT;

    const SIZE: SizeL = SizeL { w: 120, h: 40 };
    const AT: (i32, i32, i32, i32) = (10, 8, 100, 24);

    fn harness(dpi: u32, v: i32) -> Harness<Host<Slider>> {
        let s = Slider::new(WidgetId(1), Text::from("Volume"), 0, 100, 1, v);
        Harness::new(Host::new(s, AT), SIZE, dpi, Tokens::oled())
    }

    /// The dock's paint_slider_dim at 100 % (popup.rs), with Canvas for Surface, same literals.
    fn dock_slider(cv: &mut Canvas, m: Scale, rc: &RECT, knob_r: i32, lo: i32, hi: i32, v: i32) {
        let (rail_a, fill_a) = ((60 * 100 + 50) / 100, (230 * 100 + 50) / 100);
        let (tl, tr) = (rc.left + knob_r, rc.right - knob_r);
        let cy = (rc.top + rc.bottom) / 2;
        let th = m.px(4);
        let rail = RECT { left: rc.left, top: cy - th / 2, right: rc.right, bottom: cy - th / 2 + th };
        cv.fill_rr(rail, th / 2, (255, 255, 255), rail_a);
        let kx = tl + (tr - tl) * (v - lo) / (hi - lo).max(1);
        cv.fill_rr(RECT { right: kx, ..rail }, th / 2, (255, 255, 255), fill_a);
        cv.fill_rr(
            RECT { left: kx - knob_r, top: cy - knob_r, right: kx + knob_r, bottom: cy + knob_r },
            knob_r,
            (255, 255, 255),
            255,
        );
    }

    #[test]
    fn slider_matches_dock_reference() {
        for dpi in [96, 144] {
            for v in [0, 37, 100] {
                let mut h = harness(dpi, v);
                let img = h.render();
                let want = {
                    let size = Scale { dpi }.size(SIZE);
                    r1::buffer(size.w, size.h, dpi, |cv, m| {
                        let (x, y, w, hh) = AT;
                        let rc = RECT { left: m.px(x), top: m.px(y), right: m.px(x + w), bottom: m.px(y + hh) };
                        dock_slider(cv, m, &rc, m.px(7), 0, 100, v);
                    })
                };
                assert_eq!(r1::first_diff(&img.px, &want, img.w as i32), None, "dpi {dpi} value {v}");
            }
        }
    }

    #[test]
    fn slider_keys_and_drag() {
        let mut h = harness(96, 50);
        h.key("Tab").key("Right");
        assert_eq!(h.screen().outs, [SliderOut::Committed(51)]);
        h.key("PageUp");
        assert_eq!(h.screen().outs.last(), Some(&SliderOut::Committed(61)), "10 % of the range");
        h.key("Home");
        assert_eq!(h.screen().outs.last(), Some(&SliderOut::Committed(0)));
        h.key("End");
        assert_eq!(h.screen().outs.last(), Some(&SliderOut::Committed(100)));
        h.key("PageDown").key("Left");
        assert_eq!(h.screen().outs.last(), Some(&SliderOut::Committed(89)));
        // The rail runs x 10..110 with the knob centre between 17 and 103: x 60 is 50.
        let mut h = harness(96, 0);
        h.drag(PointL { x: 17, y: 20 }, PointL { x: 60, y: 20 });
        let outs = &h.screen().outs;
        assert_eq!(outs.last(), Some(&SliderOut::Committed(50)), "{outs:?}");
        assert!(outs.iter().any(|o| matches!(o, SliderOut::Changed(_))));
        assert_eq!(h.screen().w.value(), 50);
        // Past the ends it clamps.
        h.drag(PointL { x: 60, y: 20 }, PointL { x: 300, y: 20 });
        assert_eq!(h.screen().w.value(), 100);
    }

    #[test]
    fn a_click_on_the_rail_sets_the_value() {
        let mut h = harness(96, 0);
        h.click(PointL { x: 103, y: 20 });
        assert_eq!(h.screen().outs, [SliderOut::Changed(100), SliderOut::Committed(100)]);
    }

    #[test]
    fn the_node_is_a_slider() {
        let h = harness(96, 25);
        assert_eq!(h.a11y_dump(), "Slider \"Volume\" [focusable] 25 (0..100)\n");
    }
}
