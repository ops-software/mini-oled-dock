//! ScrollBar (F-K1, P2-kit 2.6): a thin overlay bar on the moved ScrollModel (the dock popups' numbers, not copied), with its own
//! one-shot hide timer: ScrollModel::by moves without asking for a tick, so every move here arms TimerId::kit(id, 0) for
//! SCROLLBAR_HIDE_MS + 16 ms and repaints the bar's zone when it fires.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{Event, MouseButton, TimerId, WheelDelta};
use crate::geom::{RectP, Scale};
use crate::node::{Node, NodeId, NodeValue, Role, WidgetId};
use crate::screen::Widget;
use crate::scroll::{ScrollModel, SCROLLBAR_ALPHA_HOT, SCROLLBAR_HIDE_MS};
use windows::Win32::Foundation::RECT;

/// What a scrollbar tells its screen: the new offset, px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollOut {
    Scrolled(i64),
}

/// The largest content height the model's i32 numbers carry; a taller content is scaled into it for the thumb.
const MODEL_CAP: i64 = (i32::MAX / 2) as i64;

/// A thin overlay scrollbar.
pub struct ScrollBar {
    id: WidgetId,
    viewport: RectP,
    content_h: i64,
    offset: i64,
    model: ScrollModel,
    scale: Scale,
    /// The last time a layout or an event told.
    now: u64,
    /// No interaction since the bar became scrollable: nothing is drawn (no timer could hide it again).
    quiet: bool,
    /// The thumb is held.
    dragging: bool,
}

impl ScrollBar {
    pub fn new(id: WidgetId) -> ScrollBar {
        ScrollBar {
            id,
            viewport: RectP::EMPTY,
            content_h: 0,
            offset: 0,
            model: ScrollModel::new(),
            scale: Scale { dpi: 96 },
            now: 0,
            quiet: true,
            dragging: false,
        }
    }

    /// The viewport, the content height and the offset, physical px.
    pub fn set(&mut self, viewport: RectP, content_h: i64, offset: i64) {
        self.viewport = viewport;
        self.content_h = content_h.max(0);
        self.offset = offset.clamp(0, self.max());
        self.apply();
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// The bar's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// The offset moves by delta px (positive = down) as the wheel or a key of the screen asks: the bar shows, the hide
    /// timer is armed, and the new offset comes back if it moved.
    pub fn scroll_by(&mut self, cx: &mut EventCx, delta: i64) -> Option<ScrollOut> {
        self.now = cx.now_ms();
        let was = self.offset;
        self.offset = (self.offset + delta).clamp(0, self.max());
        self.apply();
        self.show(cx);
        (self.offset != was).then_some(ScrollOut::Scrolled(self.offset))
    }

    /// The screen moved the offset by itself (a key, a drag of the content): shows the bar and arms its hide timer.
    pub fn show(&mut self, cx: &mut EventCx) {
        self.now = cx.now_ms();
        self.quiet = false;
        self.model.by(0, self.now);
        cx.set_timer(TimerId::kit(self.id, 0), (SCROLLBAR_HIDE_MS + 16) as u32);
        cx.damage(self.zone());
    }

    fn max(&self) -> i64 {
        (self.content_h - i64::from(self.viewport.h())).max(0)
    }

    fn view(&self) -> RECT {
        self.viewport.into()
    }

    /// The model's content height: the content, scaled into i32 when it is taller than MODEL_CAP.
    fn model_content(&self) -> i32 {
        self.content_h.min(MODEL_CAP) as i32
    }

    /// An i64 offset in the model's i32 range, and back.
    fn to_model(&self, offset: i64) -> i32 {
        if self.content_h <= MODEL_CAP {
            offset as i32
        } else {
            (i128::from(offset) * i128::from(MODEL_CAP) / i128::from(self.content_h)) as i32
        }
    }

    fn offset_of_model(&self, off: i32) -> i64 {
        if self.content_h <= MODEL_CAP {
            i64::from(off)
        } else {
            (i128::from(off) * i128::from(self.content_h) / i128::from(MODEL_CAP)) as i64
        }
    }

    /// Hands the geometry and the offset to the model.
    fn apply(&mut self) {
        let was = self.model.scrollable();
        self.model.set(self.scale, self.view(), self.model_content(), self.now);
        let want = self.to_model(self.offset);
        let have = self.model.offset();
        if want != have {
            self.model.by(want - have, self.now);
        }
        if !self.model.scrollable() || !was {
            self.quiet = true;
        }
    }

    /// The whole zone that grabs the bar, as a rect.
    fn zone(&self) -> RectP {
        self.model.hit_zone().into()
    }

    /// The model moved: the offset follows.
    fn moved(&mut self, cx: &mut EventCx) -> Option<ScrollOut> {
        let off = self.offset_of_model(self.model.offset());
        self.offset = off.clamp(0, self.max());
        self.show(cx);
        Some(ScrollOut::Scrolled(self.offset))
    }
}

impl Widget for ScrollBar {
    type Out = ScrollOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = rect;
        self.scale = cx.scale();
        self.now = cx.now_ms();
        self.apply();
        if !self.model.scrollable() {
            return;
        }
        let percent = if self.max() > 0 { self.offset as f32 * 100.0 / self.max() as f32 } else { 0.0 };
        let node = Node::new(self.node_id(), Role::ScrollBar, self.zone()).value(NodeValue::Percent(percent));
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        if !self.model.scrollable() {
            return;
        }
        let hot = cx.hot(id) || self.dragging;
        let thumb = if hot {
            // The model's thumb at its resting width, widened to the hot one on the same right edge.
            let t: RectP = self.model.thumb().into();
            Some((RectP { l: t.r - cx.px(cx.tokens().size.scrollbar_hot_w), ..t }, SCROLLBAR_ALPHA_HOT))
        } else if self.quiet {
            None
        } else {
            self.model.paint_rect(cx.now_ms()).map(|(r, a)| (r.into(), a))
        };
        if let Some((r, a)) = thumb {
            let ink = cx.tokens().color.scrollbar;
            let ink = if a == SCROLLBAR_ALPHA_HOT { cx.tokens().color.scrollbar_hot } else { ink };
            cx.fill(r, r.w() / 2, ink);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ScrollOut> {
        self.now = cx.now_ms();
        if let Event::Timer(t) = ev {
            if *t == TimerId::kit(self.id, 0) {
                // A drag in progress outlives the timer: only PointerUp or DragEnd ends it.
                cx.damage(self.zone());
            }
            return None;
        }
        if cx.target() != Some(self.node_id()) {
            return None;
        }
        match ev {
            Event::PointerDown { pos, button: MouseButton::Left, .. } => {
                let thumb = self.model.thumb();
                if pos.y >= thumb.top && pos.y < thumb.bottom {
                    self.dragging = true;
                    return None;
                }
                // A click on the track pages by the viewport height towards it.
                let page = i64::from(self.viewport.h());
                let delta = if pos.y < thumb.top { -page } else { page };
                self.scroll_by(cx, delta)
            }
            Event::PointerMove { pos, .. } | Event::DragStart { pos, .. } | Event::DragMove { pos, .. }
                if self.dragging =>
            {
                let before = self.model.offset();
                self.model.drag_to(pos.y, self.now);
                (self.model.offset() != before).then(|| self.moved(cx)).flatten()
            }
            Event::PointerUp { .. } | Event::DragEnd { .. } => {
                self.dragging = false;
                self.show(cx);
                None
            }
            Event::Wheel { delta, .. } => {
                let step = i64::from(self.model.step());
                let px = match delta {
                    WheelDelta::Lines(n) => -(f64::from(*n) * step as f64),
                    WheelDelta::Pixels(p) => -f64::from(*p),
                };
                self.scroll_by(cx, px.round() as i64)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, PointP, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    fn harness(h_px: i32, content: i64, offset: i64) -> Harness<Host<ScrollBar>> {
        let mut bar = ScrollBar::new(WidgetId(1));
        bar.set(RectP::wh(0, 0, 200, h_px), content, offset);
        Harness::new(Host::new(bar, (0, 0, 200, h_px)), SizeL { w: 200, h: h_px }, 96, Tokens::oled())
    }

    /// The blue channel of a pixel of a render.
    fn px(h: &mut Harness<Host<ScrollBar>>, x: u32, y: u32) -> u32 {
        let img = h.render();
        img.px[(y * img.w + x) as usize] & 0xFF
    }

    #[test]
    fn hides_one_second_after_the_last_scroll() {
        let mut h = harness(300, 3000, 0);
        assert_eq!(px(&mut h, 195, 20), 0, "nothing is drawn before the first scroll");
        h.wheel(PointL { x: 195, y: 100 }, -1.0);
        assert_eq!(h.screen().outs, [ScrollOut::Scrolled(48)]);
        h.hover(PointL { x: 50, y: 100 });
        assert_eq!(px(&mut h, 195, 20), 50, "the thumb at SCROLLBAR_REST, 3 px wide, moved by the step");
        h.advance(990);
        assert_eq!(px(&mut h, 195, 20), 50, "still there before the second is out");
        h.advance(100);
        assert_eq!(px(&mut h, 195, 20), 0, "hidden after SCROLLBAR_HIDE_MS");
        assert!(h.is_idle(), "the one-shot timer fired and nothing else is armed");
    }

    #[test]
    fn a_later_scroll_moves_the_hide_back() {
        let mut h = harness(300, 3000, 0);
        h.wheel(PointL { x: 195, y: 100 }, -1.0);
        h.hover(PointL { x: 50, y: 100 });
        h.advance(900);
        h.wheel(PointL { x: 195, y: 100 }, -1.0);
        h.hover(PointL { x: 50, y: 100 });
        h.advance(900);
        assert_eq!(px(&mut h, 195, 30), 50, "1.8 s after the first scroll, 0.9 s after the last");
        h.advance(200);
        assert_eq!(px(&mut h, 195, 30), 0);
        assert!(h.is_idle());
    }

    #[test]
    fn hover_widens_to_6_px_at_110() {
        let mut h = harness(300, 3000, 0);
        h.hover(PointL { x: 195, y: 100 });
        assert_eq!(px(&mut h, 195, 10), 110);
        assert_eq!(px(&mut h, 192, 10), 110, "6 px wide: x 191 to 197");
        assert_eq!(px(&mut h, 188, 10), 0);
        h.hover(PointL { x: 50, y: 100 });
        assert_eq!(px(&mut h, 195, 10), 0, "quiet again off the zone");
    }

    #[test]
    fn no_timer_when_static() {
        // Content that fits: no node, no ink, no timer.
        let mut h = harness(300, 200, 0);
        assert_eq!(h.a11y_dump(), "");
        h.hover(PointL { x: 195, y: 100 });
        assert_eq!(px(&mut h, 195, 10), 0);
        assert!(h.is_idle());
        // Scrollable but untouched: the bar is there for the pointer, and arms nothing.
        let mut h = harness(300, 3000, 0);
        h.advance(5000);
        assert!(h.is_idle());
        assert_eq!(h.a11y_dump(), "ScrollBar 0%\n");
    }

    #[test]
    fn thumb_drag_maps_to_offset_at_1_000_000_rows() {
        let content = 1_000_000_i64 * 28;
        let mut h = harness(600, content, 0);
        // The thumb is SCROLL_THUMB_MIN (24) tall at the top; its centre is y 12.
        h.drag(PointL { x: 195, y: 12 }, PointL { x: 195, y: 300 });
        let max = content - 600;
        assert_eq!(h.screen().outs.last(), Some(&ScrollOut::Scrolled(max / 2)), "{:?}", h.screen().outs.last());
        assert_eq!(h.screen().w.offset(), max / 2);
        h.drag(PointL { x: 195, y: 300 }, PointL { x: 195, y: 900 });
        assert_eq!(h.screen().w.offset(), max, "clamped at the end");
    }

    #[test]
    fn a_taller_content_is_scaled_for_the_thumb_only() {
        let content = 4_000_000_000_000_i64;
        let mut h = harness(600, content, content / 2);
        assert_eq!(h.screen().w.offset(), content / 2, "the i64 offset is kept as given");
        h.drag(PointL { x: 195, y: 300 }, PointL { x: 195, y: 150 });
        let off = h.screen().w.offset();
        // The thumb centre at y 150 puts its top at 138 of the 576 px of travel.
        let want = (content - 600) / 576 * 138;
        assert!((off - want).abs() < want / 1000, "{off} against {want}");
    }

    #[test]
    fn a_held_drag_survives_the_hide_timer() {
        use crate::dispatch::Input;
        use crate::event::{Mods, MouseButton};
        let mut h = harness(600, 28_000, 0);
        let a = PointP { x: 195, y: 12 };
        h.input(Input::Move(a, Mods::NONE));
        h.input(Input::Down(a, MouseButton::Left, 1, Mods::NONE));
        h.advance(1100);
        h.input(Input::Move(PointP { x: 195, y: 200 }, Mods::NONE));
        assert!(h.screen().w.offset() > 0, "the thumb still follows the pointer after the hide timer fired");
        h.input(Input::Up(PointP { x: 195, y: 200 }, MouseButton::Left, Mods::NONE));
    }

    #[test]
    fn a_click_on_the_track_pages_by_the_viewport() {
        let mut h = harness(300, 3000, 0);
        h.click(PointL { x: 195, y: 250 });
        assert_eq!(h.screen().outs, [ScrollOut::Scrolled(300)]);
        h.click(PointL { x: 195, y: 290 });
        assert_eq!(h.screen().outs.last(), Some(&ScrollOut::Scrolled(600)));
        h.click(PointL { x: 195, y: 2 });
        assert_eq!(h.screen().outs.last(), Some(&ScrollOut::Scrolled(300)), "above the thumb pages back");
    }
}
