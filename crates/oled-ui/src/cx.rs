//! The contexts a screen lays out, paints and handles events with (F-K1, P2-kit 2.5). Each borrows the window's
//! Dispatcher (frame, interaction state, animation, menus, tips, toasts) and its drawing resources; the Harness and the
//! host make them through the pub(crate) constructors below. The bodies marked "filled by" are KA1's (K1a.4).

use crate::anim::HOVER_CHANNEL;
use crate::command::CommandId;
use crate::dispatch::{Dispatcher, Effect};
use crate::event::TimerId;
use crate::geom::{self, PointP, RectP, Scale};
use crate::glyph::{Glyph, GlyphCache};
use crate::host::Caption;
use crate::model::Align;
use crate::node::{Node, NodeId, Text};
use crate::strings::UiLang;
use crate::text::{FontId, LayoutOpts, TextEngine, TextLayout, Trim};
use crate::theme::{Ink, Tokens, TypeRole};
use crate::w3::{argb, Canvas, IconBitmap};
use windows::Win32::Foundation::RECT;

/// An animated value of a node; a node may animate several channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnimId {
    pub node: NodeId,
    pub channel: u8,
}

/// An easing curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    /// 1 - (1 - t)^3.
    OutCubic,
    /// t^2.
    In,
    Linear,
}

/// An open menu or list popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MenuId(pub u32);

/// Where a popup opens, in client coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Point(PointP),
    Below(RectP),
    Above(RectP),
    Right(RectP),
}

/// One menu entry.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MenuItem {
    /// Label, glyph, chord, enabled and checked from the screen's CommandTable.
    Command(CommandId),
    Custom {
        id: CommandId,
        label: Text,
        glyph: Option<Glyph>,
        chord: Option<Text>,
        enabled: bool,
        checked: Option<bool>,
        /// A CAPTION line under a disabled item: the reason.
        note: Option<Text>,
        danger: bool,
    },
    Submenu {
        label: Text,
        glyph: Option<Glyph>,
        items: Vec<MenuItem>,
    },
    Header(Text),
    Separator,
}

/// A toast: a short message over the window's bottom, with an optional action and progress.
#[derive(Clone, Debug, PartialEq)]
pub struct ToastSpec {
    pub text: Text,
    pub action: Option<(Text, CommandId)>,
    pub progress: Option<f32>,
    pub hold_ms: Option<u32>,
}

/// The clipboard as a screen sees it (the system one in a window, FakeClipboard in the Harness).
pub trait Clipboard {
    fn text(&mut self) -> Option<String>;
    fn set_text(&mut self, s: &str) -> bool;
    fn has_files(&mut self) -> bool;
}

/// What every context of one window reads: scale, tokens, language, the clock and the requested caption.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub(crate) scale: Scale,
    pub(crate) tokens: &'a Tokens,
    pub(crate) lang: UiLang,
    pub(crate) now_ms: u64,
    pub(crate) caption: Caption,
    /// S-CAPTION's rig (EventLoop::caption_spike): Custom takes effect while CUSTOM_CAPTION is false.
    pub(crate) caption_spike: bool,
}

/// A window's drawing resources kept between frames: the text engine and the glyph cache.
pub(crate) struct Res {
    pub(crate) text: Box<dyn TextEngine>,
    pub(crate) glyphs: GlyphCache,
    /// The digit atlases of the fonts in use (K2a.4), built at the first figure.
    pub(crate) digits: crate::text::digits::Atlases,
}

impl Res {
    pub(crate) fn new(text: Box<dyn TextEngine>) -> Res {
        Res { text, glyphs: GlyphCache::new(), digits: crate::text::digits::Atlases::default() }
    }
}

/// The layout context: the screen pushes its nodes through it.
pub struct LayoutCx<'a> {
    pub(crate) env: Env<'a>,
    pub(crate) disp: &'a mut Dispatcher,
    pub(crate) res: &'a mut Res,
}

impl<'a> LayoutCx<'a> {
    pub(crate) fn new(env: Env<'a>, disp: &'a mut Dispatcher, res: &'a mut Res) -> LayoutCx<'a> {
        LayoutCx { env, disp, res }
    }
}

impl LayoutCx<'_> {
    /// Adds a node to this layout's frame (after the ones pushed before it: later nodes are hit first).
    pub fn push(&mut self, node: Node) {
        self.disp.frame.push(node);
    }

    pub fn scale(&self) -> Scale {
        self.env.scale
    }

    pub fn px(&self, logical: i32) -> i32 {
        self.env.scale.px(logical)
    }

    pub fn tokens(&self) -> &Tokens {
        self.env.tokens
    }

    pub fn lang(&self) -> UiLang {
        self.env.lang
    }

    pub fn text(&mut self) -> &mut dyn TextEngine {
        &mut *self.res.text
    }

    /// The font of a role at this window's scale and text scale.
    pub fn font(&mut self, role: TypeRole) -> FontId {
        let (scale, ts) = (self.env.scale, self.env.tokens.text_scale);
        self.res.text.font(role, None, scale, ts)
    }

    /// Custom only when S-CAPTION passed (K3.5); else System.
    pub fn effective_caption(&self) -> Caption {
        let on = crate::host::caption::CUSTOM_CAPTION || self.env.caption_spike;
        crate::host::caption::effective(self.env.caption, on)
    }

    /// Toasts sit 12 px above this y (files: the status line top).
    pub fn set_toast_baseline(&mut self, y: i32) {
        self.disp.toasts.baseline = Some(y);
    }

    pub fn now_ms(&self) -> u64 {
        self.env.now_ms
    }
}

/// The paint context of one band: draws in window coordinates, clipped to band ∩ damage; the band origin is hidden.
pub struct PaintCx<'a> {
    pub(crate) env: Env<'a>,
    pub(crate) canvas: Canvas<'a>,
    /// The band's top-left in window coordinates.
    pub(crate) origin: PointP,
    /// Band ∩ damage, window coordinates.
    pub(crate) clip: RectP,
    pub(crate) disp: &'a Dispatcher,
    pub(crate) res: &'a mut Res,
    /// The text-rect log of the golden comparator (Harness only).
    pub(crate) text_log: Option<&'a mut Vec<RectP>>,
}

impl<'a> PaintCx<'a> {
    pub(crate) fn new(
        env: Env<'a>,
        canvas: Canvas<'a>,
        origin: PointP,
        clip: RectP,
        disp: &'a Dispatcher,
        res: &'a mut Res,
        text_log: Option<&'a mut Vec<RectP>>,
    ) -> PaintCx<'a> {
        PaintCx { env, canvas, origin, clip, disp, res, text_log }
    }
}

impl PaintCx<'_> {
    /// Points the canvas clip at band ∩ damage in canvas coordinates (every drawing call starts here).
    pub(crate) fn sync_clip(&mut self) {
        let c = self.clip;
        self.canvas.clip = Some(RECT {
            left: c.l - self.origin.x,
            top: c.t - self.origin.y,
            right: c.r - self.origin.x,
            bottom: c.b - self.origin.y,
        });
    }

    /// Band ∩ damage, window coordinates.
    pub fn clip(&self) -> RectP {
        self.clip
    }

    /// True when `r` meets the clip.
    pub fn visible(&self, r: RectP) -> bool {
        !self.clip.intersect(r).is_empty()
    }

    pub fn scale(&self) -> Scale {
        self.env.scale
    }

    pub fn px(&self, logical: i32) -> i32 {
        self.env.scale.px(logical)
    }

    pub fn tokens(&self) -> &Tokens {
        self.env.tokens
    }

    pub fn lang(&self) -> UiLang {
        self.env.lang
    }

    pub fn hot(&self, id: NodeId) -> bool {
        self.disp.state.hot == Some(id)
    }

    pub fn pressed(&self, id: NodeId) -> bool {
        self.disp.state.pressed == Some(id)
    }

    pub fn focused(&self, id: NodeId) -> bool {
        self.disp.state.focused == Some(id)
    }

    pub fn focus_visible(&self) -> bool {
        self.disp.state.focus_visible
    }

    pub fn window_active(&self) -> bool {
        self.disp.state.window_active
    }

    /// 0..=1, animated for HOVER_FADE nodes (DUR_FAST in, DUR_NORMAL out), else 0 or 1.
    pub fn hover_t(&self, id: NodeId) -> f32 {
        let fade = AnimId { node: id, channel: HOVER_CHANNEL };
        match self.disp.anim.value(fade, self.env.now_ms) {
            Some(v) => v.clamp(0.0, 1.0),
            None if self.hot(id) => 1.0,
            None => 0.0,
        }
    }

    /// The live value of an animation at this frame.
    pub fn anim(&self, id: AnimId) -> Option<f32> {
        self.disp.anim.value(id, self.env.now_ms)
    }

    pub fn now_ms(&self) -> u64 {
        self.env.now_ms
    }

    /// `r` in canvas coordinates.
    fn local(&self, r: RectP) -> RECT {
        RECT {
            left: r.l - self.origin.x,
            top: r.t - self.origin.y,
            right: r.r - self.origin.x,
            bottom: r.b - self.origin.y,
        }
    }

    /// Canvas::fill_rr.
    pub fn fill(&mut self, r: RectP, radius: i32, ink: Ink) {
        if r.is_empty() || ink.a == 0 || !self.visible(r) {
            return;
        }
        self.sync_clip();
        let rc = self.local(r);
        self.canvas.fill_rr(rc, radius, ink.rgb, ink.a);
    }

    /// An inner ring of `width` px, any colour (high contrast).
    pub fn stroke(&mut self, r: RectP, radius: i32, width: i32, ink: Ink) {
        if r.is_empty() || ink.a == 0 || !self.visible(r) {
            return;
        }
        self.sync_clip();
        let rc = self.local(r);
        self.canvas.stroke_rr_ink(rc, radius, width, ink);
    }

    /// A 1 physical px horizontal line from x0 to x1.
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, ink: Ink) {
        if ink.a == 0 {
            return;
        }
        self.sync_clip();
        let src = argb(ink.a, ink.rgb.0, ink.rgb.1, ink.rgb.2);
        self.canvas.blend_run(y - self.origin.y, x0 - self.origin.x, x1 - self.origin.x, src);
    }

    /// A 1 physical px vertical line from y0 to y1.
    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, ink: Ink) {
        if ink.a == 0 {
            return;
        }
        self.sync_clip();
        let src = argb(ink.a, ink.rgb.0, ink.rgb.1, ink.rgb.2);
        for y in y0..y1 {
            self.canvas.blend(x - self.origin.x, y - self.origin.y, src);
        }
    }

    /// A glyph centred in `r`, as large as the smaller side of `r`.
    pub fn glyph(&mut self, r: RectP, g: Glyph, ink: Ink) {
        let size = r.w().min(r.h());
        if size <= 0 || ink.a == 0 {
            return;
        }
        let hits = self.res.glyphs.hits(g, size);
        let x = r.l + (r.w() - size) / 2 - self.origin.x;
        let y = r.t + (r.h() - size) / 2 - self.origin.y;
        self.sync_clip();
        self.canvas.glyph_from_hits(x, y, size, ink.rgb, ink.a, &hits);
    }

    /// An image at `at`, `size` px square.
    pub fn image(&mut self, img: &IconBitmap, at: PointP, size: i32, opacity: u32) {
        if size <= 0 || !self.visible(RectP::wh(at.x, at.y, size, size)) {
            return;
        }
        self.sync_clip();
        self.canvas.blit(img, at.x - self.origin.x, at.y - self.origin.y, size, opacity);
    }

    /// A layout at `at`; records the rect for the golden comparator.
    pub fn text(&mut self, l: &TextLayout, at: PointP, ink: Ink) {
        let r = RectP::wh(at.x, at.y, l.width, l.height);
        if !self.visible(r) {
            return;
        }
        if let Some(log) = self.text_log.as_deref_mut() {
            log.push(r);
        }
        self.sync_clip();
        let local = PointP { x: at.x - self.origin.x, y: at.y - self.origin.y };
        self.res.text.draw(&mut self.canvas, l, local, ink);
    }

    /// One line of text in `r`, centred vertically; returns the width drawn. Start aligns left, End and Decimal right.
    pub fn text_in(&mut self, s: &str, role: TypeRole, r: RectP, align: Align, trim: Trim, ink: Ink) -> i32 {
        let (scale, ts) = (self.env.scale, self.env.tokens.text_scale);
        let font = self.res.text.font(role, None, scale, ts);
        let opts = LayoutOpts { max_width: Some(r.w().max(0)), trim, wrap: false };
        let l = self.res.text.layout(s, font, &opts);
        let x = match align {
            Align::Start => r.l,
            Align::End | Align::Decimal => r.r - l.width,
        };
        let y = r.t + (r.h() - l.height) / 2;
        self.text(&l, PointP { x, y }, ink);
        l.width
    }

    /// Figures from the digit atlas, right-aligned at `right_top` (K2a.4); returns the width drawn.
    pub fn digits(&mut self, s: &str, right_top: PointP, role: TypeRole, ink: Ink, unit: Ink, decimal: bool) -> i32 {
        crate::text::digits::draw(self, s, right_top, role, ink, unit, decimal)
    }

    /// A column-span graph from x0 (K4.1).
    pub fn spans(&mut self, x0: i32, spans: &[(i16, i16)], ink: Ink) {
        crate::gfx::spans::draw(self, x0, spans, ink);
    }

    /// The focus ring around a plate (inset: on a row plate), only if focus_visible() && window_active(). With
    /// rim_focus white (every theme but high contrast) the pixels are Canvas::stroke_rr's: the dock's ring.
    pub fn focus_ring(&mut self, around: RectP, radius: i32, inset: bool) {
        if !self.focus_visible() || !self.window_active() {
            return;
        }
        let s = self.env.scale;
        let ring = if inset { geom::focus_ring_inset(around, radius, s) } else { geom::focus_ring(around, radius, s) };
        let ink = self.env.tokens.color.rim_focus;
        self.stroke(ring.rect, ring.radius, ring.width, ink);
    }

    /// Runs `f` with the clip narrowed to `r`, then restores it.
    pub fn with_clip(&mut self, r: RectP, f: &mut dyn FnMut(&mut PaintCx<'_>)) {
        let saved = self.clip;
        self.clip = saved.intersect(r);
        f(self);
        self.clip = saved;
    }
}

/// What the last bring_to_front of a window achieved (TK1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontResult {
    /// The window became the foreground window.
    Foreground,
    /// Windows kept the foreground elsewhere: the window was raised above the other windows and its taskbar button
    /// flashed.
    Topmost,
    /// Neither took effect.
    None,
}

/// The event context: what a screen reads and asks for while it handles one event. Requests become effects that the
/// host (or the Harness) applies after the event returns.
pub struct EventCx<'a> {
    pub(crate) env: Env<'a>,
    pub(crate) disp: &'a mut Dispatcher,
    pub(crate) res: &'a mut Res,
    pub(crate) clipboard: &'a mut dyn Clipboard,
    pub(crate) target: Option<NodeId>,
    pub(crate) consumed: bool,
}

impl<'a> EventCx<'a> {
    pub(crate) fn new(
        env: Env<'a>,
        disp: &'a mut Dispatcher,
        res: &'a mut Res,
        clipboard: &'a mut dyn Clipboard,
        target: Option<NodeId>,
    ) -> EventCx<'a> {
        EventCx { env, disp, res, clipboard, target, consumed: false }
    }
}

impl EventCx<'_> {
    /// Pointer: the hit node or the captured one; keys: the focused node.
    pub fn target(&self) -> Option<NodeId> {
        self.target
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.disp.state.focused
    }

    pub fn hot(&self) -> Option<NodeId> {
        self.disp.state.hot
    }

    pub fn is_focus_visible(&self) -> bool {
        self.disp.state.focus_visible
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.disp.frame.get(id)
    }

    /// A widget that handled a key calls it: the key goes no further.
    pub fn consume(&mut self) {
        self.consumed = true;
    }

    pub fn consumed(&self) -> bool {
        self.consumed
    }

    pub fn damage(&mut self, r: RectP) {
        self.disp.effects.push(Effect::Damage(r));
    }

    pub fn redraw(&mut self) {
        self.disp.effects.push(Effect::Redraw);
    }

    pub fn relayout(&mut self) {
        self.disp.effects.push(Effect::Relayout);
    }

    pub fn set_focus(&mut self, id: NodeId) {
        self.disp.effects.push(Effect::SetFocus(id));
    }

    pub fn capture_pointer(&mut self) {
        self.disp.effects.push(Effect::Capture);
    }

    pub fn release_pointer(&mut self) {
        self.disp.effects.push(Effect::Release);
    }

    pub fn animate(&mut self, id: AnimId, to: f32, dur_ms: u32, ease: Ease) {
        self.disp.effects.push(Effect::Animate { id, to, dur_ms, ease });
    }

    /// Opens a menu at `at`; its picks come back as Event::Command with the opener as target.
    pub fn open_menu(&mut self, items: Vec<MenuItem>, at: Anchor) -> MenuId {
        let id = self.disp.next_menu_id();
        let opener = self.target;
        crate::widgets::menu::opened(&mut self.disp.menus, id, items.clone(), at, opener);
        self.disp.effects.push(Effect::OpenMenu { id, items, at, opener });
        id
    }

    /// Opens a list popup (keys stay with the caller); picks come back as Event::ListPicked.
    pub fn open_list(&mut self, items: Vec<Text>, at: Anchor, highlight: Option<usize>) -> MenuId {
        let id = self.disp.next_menu_id();
        crate::widgets::menu::list_opened(&mut self.disp.menus, id, &items, at, highlight, self.target);
        self.disp.effects.push(Effect::OpenList { id, items, at, highlight });
        id
    }

    pub fn update_list(&mut self, id: MenuId, items: Vec<Text>, highlight: Option<usize>) {
        crate::widgets::menu::list_updated(&mut self.disp.menus, id, &items, highlight);
        self.disp.effects.push(Effect::UpdateList { id, items, highlight });
    }

    pub fn close_menus(&mut self) {
        crate::widgets::menu::closed_by_screen(&mut self.disp.menus);
        self.disp.effects.push(Effect::CloseMenus);
    }

    /// Shows a toast (the host draws it above the screen and gives it hit priority).
    pub fn toast(&mut self, t: ToastSpec) {
        crate::host::toast::push(&mut self.disp.toasts, t, self.env.now_ms);
        self.disp.effects.push(Effect::Relayout);
    }

    /// Arms a one-shot timer: Event::Timer(id) after `after_ms`.
    pub fn set_timer(&mut self, id: TimerId, after_ms: u32) {
        self.disp.effects.push(Effect::SetTimer(id, after_ms));
    }

    pub fn cancel_timer(&mut self, id: TimerId) {
        self.disp.effects.push(Effect::CancelTimer(id));
    }

    /// Retained scroll: the host moves the viewport's pixels by dy and paints only the exposed strip.
    pub fn scroll(&mut self, viewport: RectP, dy: i32) {
        self.disp.effects.push(Effect::Scroll { viewport, dy });
    }

    /// The IME composition window and the accessibility caret.
    pub fn set_caret(&mut self, r: Option<RectP>) {
        self.disp.effects.push(Effect::SetCaret(r));
    }

    pub fn clipboard(&mut self) -> &mut dyn Clipboard {
        &mut *self.clipboard
    }

    pub fn close_window(&mut self) {
        self.disp.effects.push(Effect::CloseWindow);
    }

    /// Brings the window to the front once the event has returned (TK1): restored if minimised, then made the
    /// foreground window; when Windows refuses that, raised above the other windows without taking the activation and
    /// its taskbar button flashed. Nothing is logged (the app logs front_result).
    pub fn bring_to_front(&mut self) {
        self.disp.effects.push(Effect::BringToFront);
    }

    /// Keeps the window above every window that is not topmost (on) or lets it back among them (off), once the event
    /// has returned; the activation is not taken (TK1).
    pub fn set_topmost(&mut self, on: bool) {
        self.disp.effects.push(Effect::Topmost(on));
    }

    /// The outcome of the last bring_to_front of this window, once applied (None before the first one).
    pub fn front_result(&self) -> Option<FrontResult> {
        self.disp.front
    }

    pub fn now_ms(&self) -> u64 {
        self.env.now_ms
    }

    pub fn scale(&self) -> Scale {
        self.env.scale
    }

    pub fn px(&self, logical: i32) -> i32 {
        self.env.scale.px(logical)
    }

    pub fn tokens(&self) -> &Tokens {
        self.env.tokens
    }

    pub fn lang(&self) -> UiLang {
        self.env.lang
    }

    pub fn text(&mut self) -> &mut dyn TextEngine {
        &mut *self.res.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::GdiTextEngine;
    use windows::Win32::Foundation::RECT;

    const W: i32 = 100;
    const H: i32 = 80;

    /// Paints into a fresh buffer through a PaintCx with the given interaction state.
    fn paint(dpi: u32, visible: bool, active: bool, tokens: &Tokens, f: impl FnOnce(&mut PaintCx)) -> Vec<u32> {
        let mut disp = Dispatcher::new();
        disp.state.focus_visible = visible;
        disp.state.window_active = active;
        let mut res = Res::new(Box::new(GdiTextEngine::new().unwrap()));
        let env = Env {
            scale: Scale { dpi },
            tokens,
            lang: UiLang::EN,
            now_ms: 0,
            caption: Caption::System,
            caption_spike: false,
        };
        let mut buf = vec![0u32; (W * H) as usize];
        {
            let canvas = Canvas { buf: &mut buf, w: W, h: H, clip: None };
            let mut cx = PaintCx::new(env, canvas, PointP { x: 0, y: 0 }, RectP::wh(0, 0, W, H), &disp, &mut res, None);
            f(&mut cx);
        }
        buf
    }

    fn reference(rc: RECT, radius: i32, width: i32) -> Vec<u32> {
        let mut buf = vec![0u32; (W * H) as usize];
        Canvas { buf: &mut buf, w: W, h: H, clip: None }.stroke_rr(rc, radius, width, 160);
        buf
    }

    #[test]
    fn ring_widths() {
        for (dpi, w) in [(96, 2), (120, 2), (144, 2), (192, 3)] {
            assert_eq!(Scale { dpi }.ring(), crate::tokens::rim_focus_px(dpi));
            assert_eq!(Scale { dpi }.ring(), w);
        }
    }

    #[test]
    fn ring_pixels_match_dock_keys() {
        let t = Tokens::oled();
        // Z3's geometry: the plate inflated by FOCUS_GAP + width, the radius grown by the same.
        let got = paint(96, true, true, &t, |cx| cx.focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, false));
        let want = reference(RECT { left: 6, top: 1, right: 40, bottom: 35 }, 11, 2);
        assert!(got.iter().any(|p| *p != 0));
        assert_eq!(got, want, "96 dpi");
        let got = paint(192, true, true, &t, |cx| cx.focus_ring(RectP { l: 20, t: 10, r: 72, b: 62 }, 14, false));
        let want = reference(RECT { left: 13, top: 3, right: 79, bottom: 69 }, 21, 3);
        assert_eq!(got, want, "192 dpi");
    }

    #[test]
    fn ring_needs_keyboard_focus_and_an_active_window() {
        let t = Tokens::oled();
        let ring = |cx: &mut PaintCx| cx.focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, false);
        assert!(paint(96, false, true, &t, ring).iter().all(|p| *p == 0));
        assert!(paint(96, true, false, &t, ring).iter().all(|p| *p == 0));
    }

    #[test]
    fn a_coloured_ring_has_the_same_shape() {
        let mut t = Tokens::oled();
        let white = paint(96, true, true, &t, |cx| cx.focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, false));
        t.color.rim_focus = Ink { rgb: (0, 255, 255), a: 160 };
        let cyan = paint(96, true, true, &t, |cx| cx.focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, false));
        let alpha = |v: &[u32]| v.iter().map(|p| p >> 24).collect::<Vec<_>>();
        assert_eq!(alpha(&white), alpha(&cyan));
        assert!(cyan.iter().any(|p| *p != 0 && (p >> 16) & 0xFF == 0), "no red in a cyan ring");
    }

    #[test]
    fn drawing_is_translated_and_clipped_to_the_band() {
        let t = Tokens::oled();
        let mut disp = Dispatcher::new();
        disp.state.window_active = true;
        let mut res = Res::new(Box::new(GdiTextEngine::new().unwrap()));
        let env = Env {
            scale: Scale { dpi: 96 },
            tokens: &t,
            lang: UiLang::EN,
            now_ms: 0,
            caption: Caption::System,
            caption_spike: false,
        };
        // A 20-row band at window y = 30; the fill spans window rows 20..60 but only 30..50 is the band.
        let (w, h) = (40, 20);
        let mut buf = vec![0u32; (w * h) as usize];
        {
            let canvas = Canvas { buf: &mut buf, w, h, clip: None };
            let mut cx =
                PaintCx::new(env, canvas, PointP { x: 0, y: 30 }, RectP::wh(0, 30, w, h), &disp, &mut res, None);
            cx.fill(RectP::wh(5, 20, 20, 40), 0, Ink::white(255));
        }
        let row = |y: i32| (0..w).map(|x| buf[(y * w + x) as usize] != 0).collect::<Vec<_>>();
        assert!(row(0)[5] && row(19)[24] && !row(0)[4] && !row(0)[25], "the fill lands at band-local x 5..25");
    }
}
