//! The contexts a screen lays out, paints and handles events with (F-K1, P2-kit 2.5). Each borrows the window's
//! Dispatcher (frame, interaction state, animation, menus, tips, toasts) and its drawing resources; the Harness and the
//! host make them through the pub(crate) constructors below. The bodies marked "filled by" are KA1's (K1a.4).

use crate::command::CommandId;
use crate::dispatch::{Dispatcher, Effect};
use crate::event::TimerId;
use crate::geom::{PointP, RectP, Scale};
use crate::glyph::Glyph;
use crate::host::Caption;
use crate::model::Align;
use crate::node::{Node, NodeId, Text};
use crate::strings::UiLang;
use crate::text::{FontId, TextEngine, TextLayout, Trim};
use crate::theme::{Ink, Tokens, TypeRole};
use crate::w3::{Canvas, IconBitmap};

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

/// A window's drawing resources kept between frames: the text engine (K1a.3 adds the glyph cache here).
pub(crate) struct Res {
    pub(crate) text: Box<dyn TextEngine>,
}

impl Res {
    pub(crate) fn new(text: Box<dyn TextEngine>) -> Res {
        Res { text }
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

    /// 0..=1, animated for HOVER_FADE nodes (K1a.4); until then 0 or 1.
    pub fn hover_t(&self, id: NodeId) -> f32 {
        if self.hot(id) {
            1.0
        } else {
            0.0
        }
    }

    /// The live value of an animation at this frame.
    pub fn anim(&self, id: AnimId) -> Option<f32> {
        self.disp.anim.value(id, self.env.now_ms)
    }

    pub fn now_ms(&self) -> u64 {
        self.env.now_ms
    }

    /// Canvas::fill_rr. Filled by K1a.4.
    pub fn fill(&mut self, r: RectP, radius: i32, ink: Ink) {
        let _ = (r, radius, ink, &mut self.canvas, self.origin);
    }

    /// An inner ring of `width` px, any colour (high contrast). Filled by K1a.4.
    pub fn stroke(&mut self, r: RectP, radius: i32, width: i32, ink: Ink) {
        let _ = (r, radius, width, ink);
    }

    /// A 1 physical px horizontal line from x0 to x1. Filled by K1a.4.
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, ink: Ink) {
        let _ = (x0, x1, y, ink);
    }

    /// A 1 physical px vertical line from y0 to y1. Filled by K1a.4.
    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, ink: Ink) {
        let _ = (x, y0, y1, ink);
    }

    /// A glyph centred in `r`. Filled by K1a.3.
    pub fn glyph(&mut self, r: RectP, g: Glyph, ink: Ink) {
        let _ = (r, g, ink);
    }

    /// An image at `at`, `size` px square. Filled by K1a.4.
    pub fn image(&mut self, img: &IconBitmap, at: PointP, size: i32, opacity: u32) {
        let _ = (img, at, size, opacity);
    }

    /// A layout at `at`; records the rect for the golden comparator. Filled by K1a.4.
    pub fn text(&mut self, l: &TextLayout, at: PointP, ink: Ink) {
        let _ = (l, at, ink, &mut self.res, self.text_log.is_some());
    }

    /// One line of text in `r`, centred vertically; returns the width drawn. Filled by K1a.4.
    pub fn text_in(&mut self, s: &str, role: TypeRole, r: RectP, align: Align, trim: Trim, ink: Ink) -> i32 {
        let _ = (s, role, r, align, trim, ink);
        0
    }

    /// Figures from the digit atlas, right-aligned at `right_top` (K2a.4); returns the width drawn.
    pub fn digits(&mut self, s: &str, right_top: PointP, role: TypeRole, ink: Ink, unit: Ink, decimal: bool) -> i32 {
        crate::text::digits::draw(self, s, right_top, role, ink, unit, decimal)
    }

    /// A column-span graph from x0 (K4.1).
    pub fn spans(&mut self, x0: i32, spans: &[(i16, i16)], ink: Ink) {
        crate::gfx::spans::draw(self, x0, spans, ink);
    }

    /// The focus ring around a plate (inset: on a row plate), only if focus_visible() && window_active(). Filled by
    /// K1a.4.
    pub fn focus_ring(&mut self, around: RectP, radius: i32, inset: bool) {
        let _ = (around, radius, inset);
    }

    /// Runs `f` with the clip narrowed to `r`, then restores it.
    pub fn with_clip(&mut self, r: RectP, f: &mut dyn FnMut(&mut PaintCx<'_>)) {
        let saved = self.clip;
        self.clip = saved.intersect(r);
        f(self);
        self.clip = saved;
    }
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
        self.disp.effects.push(Effect::OpenMenu { id, items, at, opener });
        id
    }

    /// Opens a list popup (keys stay with the caller); picks come back as Event::ListPicked.
    pub fn open_list(&mut self, items: Vec<Text>, at: Anchor, highlight: Option<usize>) -> MenuId {
        let id = self.disp.next_menu_id();
        self.disp.effects.push(Effect::OpenList { id, items, at, highlight });
        id
    }

    pub fn update_list(&mut self, id: MenuId, items: Vec<Text>, highlight: Option<usize>) {
        self.disp.effects.push(Effect::UpdateList { id, items, highlight });
    }

    pub fn close_menus(&mut self) {
        self.disp.effects.push(Effect::CloseMenus);
    }

    /// Shows a toast (the host draws it above the screen and gives it hit priority).
    pub fn toast(&mut self, t: ToastSpec) {
        crate::host::toast::push(&mut self.disp.toasts, t, self.env.now_ms);
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
