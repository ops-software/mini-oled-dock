//! CaptionButtons (F-K3, P2-kit 2.10; K3.5): the custom caption's minimise, maximise/restore and close buttons, 46 x 40
//! each at the right end of the rect the screen gives (SPEC-platform 5.3.4). Glyphs at 10 px in FG_SECONDARY; hover
//! FILL_HOVER and pressed FILL_PRESS with the hot text colour (as IconButton, so high contrast reads); Close hover
//! ACCENT_DANGER with a black glyph. The buttons are nodes (Role Button,
//! named Minimize, Maximize or Restore, Close) that never take the focus, as the system's do.
//!
//! In a window with the custom caption the host owns them: WM_NCHITTEST maps them to HTMINBUTTON, HTMAXBUTTON (the Snap
//! Layouts flyout) and HTCLOSE through Screen::caption_hit, which the screen answers with `hit`; their hover and press
//! follow the non-client mouse messages, the host runs the action as WM_SYSCOMMAND, and it tells the widget when the
//! window is maximised or restored (a kit command to its owner, delivered by the screen like any kit command). A click
//! that reaches the widget through the dispatcher (the Harness, assistive technology) gives a CaptionOut.

use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, MouseButton};
use crate::geom::{PointP, RectP, SizeP};
use crate::glyph::Glyph;
use crate::node::{Actions, Node, NodeFlags, NodeId, Role, WidgetId};
use crate::screen::{CaptionHit, Widget};
use crate::strings::k;
use crate::theme::Ink;
use crate::widgets::paint::lerp_ink;

/// What the caption buttons tell their screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionOut {
    Minimize,
    MaximizeRestore,
    Close,
}

/// The glyphs' size, logical px.
const GLYPH: i32 = 10;
/// The kit command items the host sends when the window is maximised or restored (CommandId::kit(owner, item)).
pub(crate) const ITEM_MAXIMIZED: u16 = 1;
pub(crate) const ITEM_RESTORED: u16 = 2;
/// The buttons in order, left to right.
const ORDER: [CaptionHit; 3] = [CaptionHit::MinButton, CaptionHit::MaxButton, CaptionHit::CloseButton];

/// The custom caption's minimise, maximise/restore and close buttons.
pub struct CaptionButtons {
    id: WidgetId,
    /// Minimise, maximise, close (physical px), from the last layout.
    rects: [RectP; 3],
    maximized: bool,
    /// The gallery's still frames: a button drawn as hovered.
    still: Option<CaptionHit>,
}

impl CaptionButtons {
    pub fn new(id: WidgetId) -> CaptionButtons {
        CaptionButtons { id, rects: [RectP::EMPTY; 3], maximized: false, still: None }
    }

    /// Which button a point is over (client px): MinButton, MaxButton or CloseButton; Client elsewhere.
    pub fn hit(&self, p: PointP) -> CaptionHit {
        self.rects.iter().zip(ORDER).find(|(r, _)| r.contains(p)).map_or(CaptionHit::Client, |(_, b)| b)
    }

    /// Whether the window is maximised: the middle button shows Restore. The host keeps it current in a window.
    pub fn set_maximized(&mut self, on: bool) {
        self.maximized = on;
    }

    pub fn maximized(&self) -> bool {
        self.maximized
    }

    /// The gallery's still frames only (the caption page's golden): draws one button as hovered. A window's hover
    /// comes from the pointer.
    #[doc(hidden)]
    pub fn set_still_hover(&mut self, b: Option<CaptionHit>) {
        self.still = b;
    }

    fn node(&self, i: usize) -> NodeId {
        NodeId::new(self.id, i as u32 + 1)
    }

    /// The button a node of this widget is.
    fn button_of(&self, id: NodeId) -> Option<CaptionHit> {
        (0..3).find(|i| self.node(*i) == id).map(|i| ORDER[i])
    }
}

impl Widget for CaptionButtons {
    type Out = CaptionOut;

    /// The three buttons at the right end of `rect`, top-aligned, as high as the strip (at most TITLEBAR_H).
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let w = cx.px(cx.tokens().size.caption_btn_w);
        let h = cx.px(cx.tokens().size.titlebar_h).min(rect.h()).max(0);
        let x0 = rect.r - 3 * w;
        let lang = cx.lang();
        for (i, b) in ORDER.into_iter().enumerate() {
            self.rects[i] = RectP::wh(x0 + w * i as i32, rect.t, w, h);
            let name = match b {
                CaptionHit::MinButton => k::KIT_MINIMIZE,
                CaptionHit::MaxButton if self.maximized => k::KIT_RESTORE,
                CaptionHit::MaxButton => k::KIT_MAXIMIZE,
                _ => k::KIT_CLOSE,
            };
            let node = Node::new(self.node(i), Role::Button, self.rects[i])
                .flags(NodeFlags::HOVER_FADE)
                .label(lang.kit(name))
                .actions(Actions::CLICK);
            cx.push(node);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = &cx.tokens().color;
        let (hover, press, danger, rest, hot) = (c.fill_hover, c.fill_press, c.accent_danger, c.fg_secondary, c.fg_hot);
        let black = Ink::solid(0, 0, 0);
        let g = cx.px(GLYPH);
        for (i, r) in self.rects.iter().enumerate() {
            if r.is_empty() || !cx.visible(*r) {
                continue;
            }
            let id = self.node(i);
            let b = ORDER[i];
            let t = if self.still == Some(b) { 1.0 } else { cx.hover_t(id) };
            let (fill, ink) = match (b, cx.pressed(id)) {
                (CaptionHit::CloseButton, true) => (Some(danger), black),
                (CaptionHit::CloseButton, false) => (Some(danger.faded(t)), lerp_ink(rest, black, t)),
                (_, true) => (Some(press), hot),
                (_, false) => (Some(hover.faded(t)), lerp_ink(rest, hot, t)),
            };
            if let Some(f) = fill.filter(|f| f.a > 0) {
                cx.fill(*r, 0, f);
            }
            let glyph = match b {
                CaptionHit::MinButton => Glyph::Minimize,
                CaptionHit::MaxButton if self.maximized => Glyph::Restore,
                CaptionHit::MaxButton => Glyph::Maximize,
                _ => Glyph::Close,
            };
            let gr = RectP::wh(r.l + (r.w() - g) / 2, r.t + (r.h() - g) / 2, g, g);
            cx.glyph(gr, glyph, ink);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<CaptionOut> {
        if let Event::Command(id) = ev {
            let on = if *id == CommandId::kit(self.id, ITEM_MAXIMIZED) {
                true
            } else if *id == CommandId::kit(self.id, ITEM_RESTORED) {
                false
            } else {
                return None;
            };
            if on != self.maximized {
                self.maximized = on;
                cx.damage(self.rects[1]);
                cx.relayout();
            }
            return None;
        }
        let b = cx.target().and_then(|t| self.button_of(t))?;
        let clicked = matches!(ev, Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click));
        if !clicked {
            return None;
        }
        Some(match b {
            CaptionHit::MinButton => CaptionOut::Minimize,
            CaptionHit::MaxButton => CaptionOut::MaximizeRestore,
            _ => CaptionOut::Close,
        })
    }

    /// Three buttons wide, TITLEBAR_H high.
    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: 3 * cx.px(cx.tokens().size.caption_btn_w), h: cx.px(cx.tokens().size.titlebar_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Input;
    use crate::event::Mods;
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::strings::Lang;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    const W: i32 = 200;

    fn harness(dpi: u32) -> Harness<Host<CaptionButtons>> {
        Harness::new(
            Host::new(CaptionButtons::new(WidgetId(9)), (0, 0, W, 40)),
            SizeL { w: W, h: 48 },
            dpi,
            Tokens::oled(),
        )
    }

    /// A pixel of the image, as (r, g, b).
    fn rgb(img: &crate::harness::Image, x: i32, y: i32) -> (u32, u32, u32) {
        let p = img.px[(y as u32 * img.w + x as u32) as usize];
        ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF)
    }

    #[test]
    fn three_buttons_46_by_40_at_the_right_end() {
        for (dpi, w, h) in [(96, 46, 40), (144, 69, 60)] {
            let h = {
                let mut hs = harness(dpi);
                let _ = hs.render();
                let b = &hs.screen().w;
                assert_eq!(b.rects.map(|r| (r.w(), r.h())), [(w, h); 3], "{dpi} dpi");
                let right = W * dpi as i32 / 96;
                assert_eq!(b.rects[2].r, right, "close reaches the right edge");
                assert_eq!((b.rects[0].r, b.rects[1].r), (b.rects[1].l, b.rects[2].l), "adjacent");
                let mid = |r: RectP| PointP { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 };
                assert_eq!(b.rects.map(|r| b.hit(mid(r))), ORDER);
                assert_eq!(b.hit(PointP { x: b.rects[0].l - 1, y: 5 }), CaptionHit::Client);
                assert_eq!(b.hit(PointP { x: right - 1, y: h }), CaptionHit::Client, "below the strip");
                h
            };
            let _ = h;
        }
    }

    #[test]
    fn the_buttons_are_named_and_never_focused() {
        let mut h = harness(96).lang(Lang::En);
        h.assert_named();
        assert_eq!(h.a11y_dump(), "Button \"Minimize\"\nButton \"Maximize\"\nButton \"Close\"\n");
        h.key("Tab");
        assert_eq!(h.focused(), None, "caption buttons are not Tab stops");
        let it = harness(96).lang(Lang::It);
        assert!(it.a11y_dump().contains("\"Riduci a icona\""), "{}", it.a11y_dump());
    }

    #[test]
    fn maximized_shows_restore() {
        let mut h = harness(96);
        h.command(CommandId::kit(WidgetId(9), ITEM_MAXIMIZED));
        assert!(h.screen().w.maximized());
        assert!(h.a11y_dump().contains("Button \"Restore\""), "{}", h.a11y_dump());
        h.command(CommandId::kit(WidgetId(9), ITEM_RESTORED));
        assert!(h.a11y_dump().contains("Button \"Maximize\""));
        h.command(CommandId::kit(WidgetId(8), ITEM_MAXIMIZED));
        assert!(!h.screen().w.maximized(), "another widget's command");
    }

    #[test]
    fn hover_press_and_close_hover() {
        let mut h = harness(96);
        let img = h.render();
        let (max_x, close_x) = (W - 46 - 23, W - 23);
        assert_eq!(rgb(&img, max_x - 15, 4), (0, 0, 0), "no plate at rest");
        h.hover(PointL { x: max_x, y: 20 });
        h.advance(300);
        let img = h.render();
        assert_eq!(rgb(&img, max_x - 15, 4), (20, 20, 20), "FILL_HOVER");
        h.input(Input::Down(PointP { x: max_x, y: 20 }, MouseButton::Left, 1, Mods::NONE));
        assert_eq!(rgb(&h.render(), max_x - 15, 4), (36, 36, 36), "FILL_PRESS");
        h.input(Input::Up(PointP { x: max_x, y: 20 }, MouseButton::Left, Mods::NONE));
        h.hover(PointL { x: close_x, y: 20 });
        h.advance(300);
        let img = h.render();
        assert_eq!(rgb(&img, close_x - 15, 4), (0xFF, 0x45, 0x3A), "ACCENT_DANGER");
        let glyph: Vec<(u32, u32, u32)> = (15..25)
            .flat_map(|y| (close_x - 5..close_x + 5).map(move |x| (x, y)))
            .map(|(x, y)| rgb(&img, x, y))
            .collect();
        let darkest = glyph.iter().map(|p| p.0).min().unwrap_or(255);
        assert!(darkest < 0x60, "the close glyph is black on the red plate: {glyph:?}");
        assert!(h.is_idle(), "the fades end");
    }

    #[test]
    fn clicks_give_caption_out() {
        let mut h = harness(96);
        for (x, out) in
            [(W - 115, CaptionOut::Minimize), (W - 69, CaptionOut::MaximizeRestore), (W - 23, CaptionOut::Close)]
        {
            h.click(PointL { x, y: 20 });
            assert_eq!(h.screen().outs.last(), Some(&out));
        }
        h.click(PointL { x: 10, y: 20 });
        assert_eq!(h.screen().outs.len(), 3, "nothing outside the buttons");
    }

    #[test]
    fn still_hover_draws_without_a_pointer() {
        let mut h = harness(96);
        h.screen_mut().w.set_still_hover(Some(CaptionHit::CloseButton));
        assert_eq!(rgb(&h.render(), W - 38, 4), (0xFF, 0x45, 0x3A));
        assert!(h.is_idle());
    }
}
