//! Toggle (F-K1, P2-kit 2.6): the dock's switch, pixel for pixel in its rest and on states (R1: popup.rs toggle_switch,
//! settings_view.rs's unavailable veil), plus the states the dock lacks (hover, pressed, focus, busy) from SPEC-platform 5.3.4.

use super::paint::lerp_ink;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::{RectP, SizeP};
use crate::model::Align;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, SizeTokens, TypeRole};

/// What a toggle tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleOut {
    Changed(bool),
}

/// Gap between the knob and the track edge, logical px.
const INSET: i32 = 3;
/// Gap between the label and the track, logical px.
const LABEL_GAP: i32 = 12;
/// The hover lift of the off track's alpha.
const OFF_TRACK_HOT: u32 = 20;
/// The lowest alpha of the busy pulse.
const BUSY_LOW: u32 = 120;

/// An on/off switch with its label.
pub struct Toggle {
    id: WidgetId,
    label: Text,
    on: bool,
    enabled: bool,
    busy: bool,
    rect: RectP,
}

impl Toggle {
    pub fn new(id: WidgetId, label: Text, on: bool) -> Toggle {
        Toggle { id, label, on, enabled: true, busy: false, rect: RectP::EMPTY }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn set_on(&mut self, v: bool) {
        self.on = v;
    }

    pub fn set_enabled(&mut self, v: bool) {
        self.enabled = v;
    }

    /// The knob pulses and the switch is inert.
    pub fn set_busy(&mut self, v: bool) {
        self.busy = v;
    }

    /// The toggle's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn live(&self) -> bool {
        self.enabled && !self.busy
    }

    /// The track, right-aligned in the rect and centred on its middle (the dock's row placement).
    fn track(rect: RectP, px: &dyn Fn(i32) -> i32, sz: &SizeTokens) -> RectP {
        let (w, h) = (px(sz.toggle_w), px(sz.toggle_h));
        RectP::wh(rect.r - w, (rect.t + rect.b) / 2 - h / 2, w, h)
    }

    fn flip(&mut self, cx: &mut EventCx) -> Option<ToggleOut> {
        self.on = !self.on;
        cx.damage(self.rect);
        Some(ToggleOut::Changed(self.on))
    }
}

impl Widget for Toggle {
    type Out = ToggleOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let mut flags = NodeFlags::HOVER_FADE;
        if self.on {
            flags = flags | NodeFlags::CHECKED;
        }
        if !self.enabled {
            flags = flags | NodeFlags::DISABLED;
        }
        if self.busy {
            flags = flags | NodeFlags::BUSY;
        }
        let focus = if self.enabled { Focusable::Tab } else { Focusable::No };
        let node = Node::new(self.node_id(), Role::Switch, rect)
            .flags(flags)
            .focus(focus)
            .label(self.label.clone())
            .value(NodeValue::Toggle(self.on))
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        let sz = cx.tokens().size.clone();
        let (w, h, knob) = (cx.px(sz.toggle_w), cx.px(sz.toggle_h), cx.px(sz.toggle_knob));
        let inset = cx.px(INSET);
        let track = Self::track(self.rect, &|v| cx.px(v), &sz);
        let c = cx.tokens().color.clone();
        let reduced = cx.tokens().motion.reduced;
        let period = u64::from(cx.tokens().motion.sweep_period.max(1));
        let t = if self.live() { cx.hover_t(id) } else { 0.0 };
        let pressed = self.live() && cx.pressed(id);

        // The knob keeps its centre when it grows under the pointer.
        let ky = track.t + (h - knob) / 2;
        let left = if self.on { track.l + w - inset - knob } else { track.l + inset };
        let k = if pressed { cx.px(sz.toggle_knob_pressed) } else { knob };
        let (kcx, kcy) = (left + knob / 2, ky + knob / 2);
        let knob_r = RectP::wh(kcx - k / 2, kcy - k / 2, k, k);

        let (track_ink, knob_ink) = if self.on {
            (lerp_ink(c.fill_selected, c.fill_selected_hot, t), c.fg_on_selected)
        } else {
            let hot = Ink { a: (c.toggle_off_track.a + OFF_TRACK_HOT).min(255), ..c.toggle_off_track };
            (lerp_ink(c.toggle_off_track, hot, t), lerp_ink(c.toggle_off_knob, c.fg_hot, t))
        };
        let knob_ink = if self.busy && !reduced {
            // A triangle wave over SWEEP_PERIOD between BUSY_LOW and the full alpha.
            let ph = (cx.now_ms() % period) as f32 / period as f32;
            let tri = 1.0 - (2.0 * ph - 1.0).abs();
            Ink { a: BUSY_LOW + ((knob_ink.a.max(BUSY_LOW) - BUSY_LOW) as f32 * tri).round() as u32, ..knob_ink }
        } else {
            knob_ink
        };

        cx.fill(track, h / 2, track_ink);
        if !self.on {
            cx.stroke(track, h / 2, cx.px(1).max(1), c.toggle_off_rim);
        }
        cx.fill(knob_r, k / 2, knob_ink);
        if !self.enabled {
            // Unavailable: the switch fades into the background.
            cx.fill(RectP { l: track.l - 1, t: track.t - 1, r: track.r + 1, b: track.b + 1 }, h / 2, c.disabled_veil);
        }

        if !self.label.is_empty() {
            let room = RectP { r: track.l - cx.px(LABEL_GAP), ..self.rect };
            let ink = if self.enabled { c.fg_primary } else { c.fg_disabled };
            cx.text_in(&self.label, TypeRole::Body, room, Align::Start, Trim::End, ink);
        }
        if cx.focused(id) {
            cx.focus_ring(track, h / 2, false);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ToggleOut> {
        if cx.target() != Some(self.node_id()) || !self.live() {
            return None;
        }
        match ev {
            Event::Click { button: MouseButton::Left, .. } => self.flip(cx),
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                self.flip(cx)
            }
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
        let sz = cx.tokens().size.clone();
        let font = cx.font(TypeRole::Body);
        let label = if self.label.is_empty() { 0 } else { cx.text().width(&self.label, font) + cx.px(LABEL_GAP) };
        SizeP { w: label + cx.px(sz.toggle_w), h: cx.px(sz.control_h) }
    }
}

/// The R1 helpers the controls' reference tests share: a black window buffer drawn with the dock's own calls.
#[cfg(test)]
pub(crate) mod r1 {
    use crate::gfx::scale::Scale;
    use crate::w3::Canvas;

    /// An opaque black pixel, premultiplied.
    pub(crate) const BLACK: u32 = 0xFF00_0000;

    /// A black buffer of w x h physical px, drawn by f with the scale of dpi.
    pub(crate) fn buffer(w: i32, h: i32, dpi: u32, f: impl FnOnce(&mut Canvas, Scale)) -> Vec<u32> {
        let mut buf = vec![BLACK; (w * h) as usize];
        let mut cv = Canvas { buf: &mut buf, w, h, clip: None };
        f(&mut cv, Scale { dpi });
        buf
    }

    /// The first differing pixel of two buffers, for the failure message.
    pub(crate) fn first_diff(a: &[u32], b: &[u32], w: i32) -> Option<(i32, i32, u32, u32)> {
        a.iter().zip(b).position(|(x, y)| x != y).map(|i| ((i as i32) % w, (i as i32) / w, a[i], b[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Input;
    use crate::event::Mods;
    use crate::geom::{PointL, PointP, SizeL};
    use crate::gfx::scale::Scale;
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::w3::{rect_wh, Canvas};
    use crate::widgets::paint::Host;
    use windows::Win32::Foundation::RECT;

    const SIZE: SizeL = SizeL { w: 120, h: 40 };
    /// The toggle's rect, logical px: the row of the dock's control column.
    const AT: (i32, i32, i32, i32) = (10, 8, 100, 24);

    fn harness(dpi: u32, f: impl FnOnce(&mut Toggle)) -> Harness<Host<Toggle>> {
        let mut t = Toggle::new(WidgetId(1), Text::from(""), false);
        f(&mut t);
        Harness::new(Host::new(t, AT), SIZE, dpi, Tokens::oled())
    }

    /// The dock's toggle_switch (popup.rs) with Canvas for Surface, same literals.
    fn dock_toggle(cv: &mut Canvas, m: crate::gfx::scale::Scale, x: i32, y: i32, on: bool) {
        let (w, h, knob, inset) = (m.px(32), m.px(18), m.px(12), m.px(3));
        let track = rect_wh(x, y, w, h);
        let ky = y + (h - knob) / 2;
        if on {
            cv.fill_rr(track, h / 2, (255, 255, 255), 235);
            cv.fill_rr(rect_wh(x + w - inset - knob, ky, knob, knob), knob / 2, (0, 0, 0), 255);
        } else {
            cv.fill_rr(track, h / 2, (255, 255, 255), 40);
            cv.stroke_rr(track, h / 2, m.px(1).max(1), 70);
            cv.fill_rr(rect_wh(x + inset, ky, knob, knob), knob / 2, (255, 255, 255), 160);
        }
    }

    /// The Settings row's placement (settings_view.rs paint_row) around the dock's switch, and its veil.
    fn dock_row(dpi: u32, on: bool, enabled: bool) -> Vec<u32> {
        let size = Scale { dpi }.size(SIZE);
        r1::buffer(size.w, size.h, dpi, |cv, m| {
            let (x0, y0, w, h) = AT;
            let ctl = RECT { left: m.px(x0), top: m.px(y0), right: m.px(x0 + w), bottom: m.px(y0 + h) };
            let (sw, sh) = (m.px(32), m.px(18));
            let (x, y) = (ctl.right - sw, (ctl.top + ctl.bottom) / 2 - sh / 2);
            dock_toggle(cv, m, x, y, on);
            if !enabled {
                cv.fill_rr(rect_wh(x - 1, y - 1, sw + 2, sh + 2), sh / 2, (0, 0, 0), 150);
            }
        })
    }

    use super::r1;

    fn same(dpi: u32, on: bool, enabled: bool) {
        let mut h = harness(dpi, |t| {
            t.set_on(on);
            t.set_enabled(enabled);
        });
        let img = h.render();
        let want = dock_row(dpi, on, enabled);
        assert_eq!(r1::first_diff(&img.px, &want, img.w as i32), None, "dpi {dpi} on {on} enabled {enabled}");
    }

    #[test]
    fn toggle_rest_matches_dock_reference() {
        for dpi in [96, 144] {
            same(dpi, false, true);
            same(dpi, false, false);
        }
    }

    #[test]
    fn toggle_on_matches_dock_reference() {
        for dpi in [96, 144] {
            same(dpi, true, true);
            same(dpi, true, false);
        }
    }

    fn centre() -> PointL {
        PointL { x: 94, y: 20 }
    }

    /// The track's middle pixel left of the knob (on = off track alpha), blue channel.
    fn probe(h: &mut Harness<Host<Toggle>>) -> u32 {
        let img = h.render();
        // The track spans x 78..110 and the knob 81..93: x 100 is track only.
        img.px[(20 * img.w + 100) as usize] & 0xFF
    }

    #[test]
    fn hover_lifts_the_off_track_and_the_knob() {
        let mut h = harness(96, |_| {});
        let rest = probe(&mut h);
        h.hover(centre());
        h.advance(200);
        assert_eq!(probe(&mut h), rest + 20, "40 -> 60 white over black");
        let img = h.render();
        // The knob at hot is FG_HOT 255 (rest 160): its centre pixel.
        let knob = img.px[(20 * img.w + 87) as usize] & 0xFF;
        assert_eq!(knob, 255);
    }

    #[test]
    fn pressed_knob_is_14_px() {
        let mut h = harness(96, |_| {});
        let count = |img: &crate::harness::Image| {
            (0..img.h).map(|y| img.px[(y * img.w + 87) as usize] & 0xFF).filter(|v| *v >= 100).count()
        };
        let rest = count(&h.render());
        h.input(Input::Down(PointP { x: 94, y: 20 }, MouseButton::Left, 1, Mods::NONE));
        let down = count(&h.render());
        assert!(down > rest, "the pressed knob is taller ({rest} -> {down})");
    }

    #[test]
    fn toggle_keys() {
        let mut h = harness(96, |_| {});
        h.key("Tab").key("Enter");
        assert_eq!(h.screen().outs, [ToggleOut::Changed(true)]);
        h.input(Input::Key(Key::SPACE, true, false, Mods::NONE));
        assert_eq!(h.screen().outs.len(), 1, "Space acts on release");
        h.input(Input::Key(Key::SPACE, false, false, Mods::NONE));
        assert_eq!(h.screen().outs, [ToggleOut::Changed(true), ToggleOut::Changed(false)]);
        h.click(centre());
        assert_eq!(h.screen().outs.last(), Some(&ToggleOut::Changed(true)));
        assert!(h.screen().w.on());
    }

    #[test]
    fn disabled_and_busy_are_inert_and_busy_pulses() {
        for f in [|t: &mut Toggle| t.set_enabled(false), |t: &mut Toggle| t.set_busy(true)] {
            let mut h = harness(96, f);
            h.key("Tab").key("Enter");
            h.click(centre());
            assert!(h.screen().outs.is_empty());
        }
        let mut h = harness(96, |t| t.set_busy(true));
        let at = |h: &mut Harness<Host<Toggle>>| {
            let img = h.render();
            img.px[(20 * img.w + 87) as usize] & 0xFF
        };
        let a = at(&mut h);
        h.advance(600);
        let b = at(&mut h);
        assert_ne!(a, b, "the knob pulses while busy");
        let mut t = Tokens::oled();
        t.motion.reduced = true;
        let mut h = harness(96, |t| t.set_busy(true));
        h.set_tokens(t);
        let a = at(&mut h);
        h.advance(600);
        assert_eq!(a, at(&mut h), "static under reduced motion");
    }

    #[test]
    fn the_node_is_a_switch() {
        let mut h = harness(96, |t| t.set_on(true));
        let dump = h.a11y_dump();
        assert!(dump.contains("Switch"), "{dump}");
        let _ = h.render();
    }
}
