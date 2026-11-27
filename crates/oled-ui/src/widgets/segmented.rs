//! Segmented (F-K1, P2-kit 2.6): the dock's choice control (R1: settings_view.rs Control::Choice and seg_widths), with
//! pressed and focus states and one Tab stop with roving arrows (SPEC-platform 5.3.5).

use super::paint::{lerp_ink, text_centered};
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::theme::{Ink, TypeRole};

/// What a segmented control tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegOut {
    Selected(usize),
}

/// The track height, the padding each side of a label and the narrowest segment, logical px (settings_view.rs).
const SEG_H: i32 = 24;
const SEG_PAD: i32 = 9;
const SEG_MIN_W: i32 = 30;

/// A row of exclusive options.
pub struct Segmented {
    id: WidgetId,
    label: Text,
    options: Vec<Text>,
    selected: Option<usize>,
    /// The segments' rects, set by layout.
    segs: Vec<RectP>,
}

impl Segmented {
    pub fn new(id: WidgetId, label: Text, options: Vec<Text>, selected: Option<usize>) -> Segmented {
        Segmented { id, label, options, selected, segs: Vec::new() }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_selected(&mut self, i: Option<usize>) {
        self.selected = i.filter(|i| *i < self.options.len());
    }

    /// The group's node (slot 0); the segments are slots 1 and up.
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn seg_node(&self, i: usize) -> NodeId {
        NodeId::new(self.id, i as u32 + 1)
    }

    /// The segment a node of this widget is.
    fn index_of(&self, n: NodeId) -> Option<usize> {
        (n.owner() == self.id && n.slot() >= 1).then(|| n.slot() as usize - 1).filter(|i| *i < self.options.len())
    }

    /// The segment that is the Tab stop: the selected one, else the first.
    fn stop(&self) -> usize {
        self.selected.unwrap_or(0)
    }

    fn widths(&self, cx: &mut LayoutCx) -> Vec<i32> {
        let font = cx.font(TypeRole::Small);
        let (pad, min) = (cx.px(SEG_PAD), cx.px(SEG_MIN_W));
        let mut out = Vec::with_capacity(self.options.len());
        for o in &self.options {
            out.push((cx.text().width(o, font) + 2 * pad).max(min));
        }
        out
    }

    fn select(&mut self, cx: &mut EventCx, i: usize) -> Option<SegOut> {
        cx.set_focus(self.seg_node(i));
        if self.selected == Some(i) {
            return None;
        }
        self.selected = Some(i);
        cx.relayout();
        Some(SegOut::Selected(i))
    }
}

impl Widget for Segmented {
    type Out = SegOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let widths = self.widths(cx);
        let total: i32 = widths.iter().sum();
        let h = cx.px(SEG_H);
        let top = (rect.t + rect.b) / 2 - h / 2;
        let mut x = rect.r - total;
        self.segs.clear();
        for w in widths {
            self.segs.push(RectP { l: x, t: top, r: x + w, b: top + h });
            x += w;
        }
        let track = RectP { l: rect.r - total, t: top, r: rect.r, b: top + h };
        cx.push(Node::new(self.node_id(), Role::RadioGroup, track).label(self.label.clone()));
        let stop = self.stop();
        for (i, (seg, text)) in self.segs.iter().zip(&self.options).enumerate() {
            let mut flags = NodeFlags::HOVER_FADE;
            if self.selected == Some(i) {
                flags = flags | NodeFlags::SELECTED;
            }
            let focus = if i == stop { Focusable::Tab } else { Focusable::Roving };
            let node = Node::new(self.seg_node(i), Role::Radio, *seg)
                .parent(self.node_id())
                .flags(flags)
                .focus(focus)
                .label(text.clone())
                .actions(Actions::CLICK | Actions::FOCUS);
            cx.push(node);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let (Some(first), Some(last)) = (self.segs.first(), self.segs.last()) else { return };
        let c = cx.tokens().color.clone();
        let track = RectP { l: first.l, r: last.r, ..*first };
        let radius = track.h() / 2;
        cx.fill(track, radius, c.seg_track);
        cx.stroke(track, radius, cx.px(1).max(1), c.seg_rim);
        for (i, (seg, text)) in self.segs.iter().zip(&self.options).enumerate() {
            let id = self.seg_node(i);
            if self.selected == Some(i) {
                cx.fill(*seg, radius, c.fill_selected);
                text_centered(cx, text, TypeRole::Small, *seg, c.fg_on_selected);
            } else {
                let t = cx.hover_t(id);
                let clear = Ink { a: 0, ..c.seg_hot };
                let fill = if cx.pressed(id) { c.fill_press } else { lerp_ink(clear, c.seg_hot, t) };
                cx.fill(*seg, radius, fill);
                let ink = Ink { a: (c.fg_secondary.a + (25.0 * t).round() as u32).min(255), ..c.fg_secondary };
                text_centered(cx, text, TypeRole::Small, *seg, ink);
            }
            if cx.focused(id) {
                cx.focus_ring(*seg, radius, false);
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SegOut> {
        let i = self.index_of(cx.target()?)?;
        match ev {
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click) => self.select(cx, i),
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                self.select(cx, i)
            }
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                if *down {
                    None
                } else {
                    self.select(cx, i)
                }
            }
            Event::Key { key, down: true, .. } => {
                let last = self.options.len().checked_sub(1)?;
                let to = match *key {
                    Key::LEFT => i.saturating_sub(1),
                    Key::RIGHT => (i + 1).min(last),
                    Key::HOME => 0,
                    Key::END => last,
                    _ => return None,
                };
                cx.consume();
                self.select(cx, to)
            }
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let widths = self.widths(cx);
        SizeP { w: widths.iter().sum(), h: cx.px(SEG_H) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, PointP, SizeL};
    use crate::gfx::scale::Scale;
    use crate::harness::Harness;
    use crate::text::{LayoutOpts, TextEngine};
    use crate::theme::Tokens;
    use crate::w3::Canvas;
    use crate::widgets::paint::Host;
    use crate::widgets::toggle::r1;
    use windows::Win32::Foundation::RECT;

    const SIZE: SizeL = SizeL { w: 200, h: 40 };
    const AT: (i32, i32, i32, i32) = (10, 8, 180, 24);
    const OPTS: [&str; 3] = ["Off", "Auto", "Always on"];

    fn harness(dpi: u32, sel: Option<usize>) -> Harness<Host<Segmented>> {
        let s = Segmented::new(WidgetId(1), Text::from("Mode"), OPTS.iter().map(|o| Text::from(*o)).collect(), sel);
        Harness::new(Host::new(s, AT), SIZE, dpi, Tokens::oled())
    }

    /// The dock's Choice drawing (settings_view.rs paint_row and seg_widths), Canvas and the text engine for Surface.
    fn dock_choice(dpi: u32, selected: Option<usize>, hot: Option<usize>) -> Vec<u32> {
        let size = Scale { dpi }.size(SIZE);
        let mut eng = crate::text::engine::GdiTextEngine::new().unwrap();
        r1::buffer(size.w, size.h, dpi, |cv: &mut Canvas, m| {
            let font = eng.font(TypeRole::Small, None, m, 1.0);
            let widths: Vec<i32> = OPTS.iter().map(|o| (eng.width(o, font) + 2 * m.px(9)).max(m.px(30))).collect();
            let (x0, y0, w, h) = AT;
            let right = m.px(x0 + w);
            let (top, sh) = ((m.px(y0) + m.px(y0 + h)) / 2 - m.px(24) / 2, m.px(24));
            let mut x = right - widths.iter().sum::<i32>();
            let parts: Vec<RECT> = widths
                .iter()
                .map(|w| {
                    let r = RECT { left: x, top, right: x + w, bottom: top + sh };
                    x += w;
                    r
                })
                .collect();
            let (first, last) = (parts.first().unwrap(), parts.last().unwrap());
            let track = RECT { left: first.left, right: last.right, ..*first };
            let r = (track.bottom - track.top) / 2;
            cv.fill_rr(track, r, (255, 255, 255), 14);
            cv.stroke_rr(track, r, m.px(1).max(1), 40);
            for (j, (opt, seg)) in OPTS.iter().zip(&parts).enumerate() {
                let seg = *seg;
                let l =
                    eng.layout(opt, font, &LayoutOpts { max_width: None, trim: crate::text::Trim::End, wrap: false });
                let tx = seg.left + (seg.right - seg.left - l.width) / 2;
                let ty = seg.top + ((seg.bottom - seg.top) - l.height) / 2;
                if selected == Some(j) {
                    cv.fill_rr(seg, r, (255, 255, 255), 235);
                    eng.draw(cv, &l, PointP { x: tx, y: ty }, Ink::solid(0, 0, 0));
                } else {
                    let is_hot = hot == Some(j);
                    if is_hot {
                        cv.fill_rr(seg, r, (255, 255, 255), 26);
                    }
                    let a = if is_hot { 195 } else { 170 };
                    eng.draw(cv, &l, PointP { x: tx, y: ty }, Ink::white(a));
                }
            }
        })
    }

    #[test]
    fn segmented_matches_dock_reference() {
        for dpi in [96, 144] {
            // Rest with one selected, and with the third segment hot.
            let mut h = harness(dpi, Some(1));
            let img = h.render();
            assert_eq!(r1::first_diff(&img.px, &dock_choice(dpi, Some(1), None), img.w as i32), None, "rest at {dpi}");
            let mut h = harness(dpi, Some(1));
            h.hover(PointL { x: 170, y: 20 });
            h.advance(300);
            let img = h.render();
            assert_eq!(
                r1::first_diff(&img.px, &dock_choice(dpi, Some(1), Some(2)), img.w as i32),
                None,
                "hot at {dpi}"
            );
        }
    }

    #[test]
    fn segmented_roving() {
        let mut h = harness(96, Some(1));
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 2)), "Tab lands on the selected segment");
        h.key("Right");
        assert_eq!(h.screen().outs, [SegOut::Selected(2)]);
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 3)));
        h.key("Right");
        assert_eq!(h.screen().outs.len(), 1, "the end stops");
        h.key("Home");
        assert_eq!(h.screen().outs.last(), Some(&SegOut::Selected(0)));
        h.key("Left");
        assert_eq!(h.screen().outs.len(), 2);
        // One Tab stop: Tab returns to the selected segment, never to the others.
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)));
        h.click(PointL { x: 100, y: 20 });
        assert!(h.screen().outs.len() >= 2);
        assert_eq!(h.a11y_dump().lines().next(), Some("RadioGroup \"Mode\""));
    }

    #[test]
    fn nothing_selected_tabs_to_the_first() {
        let mut h = harness(96, None);
        h.key("Tab");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 1)));
        h.key("Enter");
        assert_eq!(h.screen().outs, [SegOut::Selected(0)]);
    }
}
