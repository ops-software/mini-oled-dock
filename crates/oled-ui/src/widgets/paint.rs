//! Painting helpers shared by the widgets: the coloured ring (K1a.4) and the plate and ring helpers the widgets build on.

use windows::Win32::Foundation::RECT;

use crate::cx::PaintCx;
use crate::geom::{PointP, RectP};
use crate::model::FgRole;
use crate::theme::{Ink, Tokens, TypeRole};
use crate::w3::{argb, rr_cov, Canvas};

/// The text colour of a role.
pub(crate) fn fg_ink(role: FgRole, t: &Tokens) -> Ink {
    let c = &t.color;
    match role {
        FgRole::Primary => c.fg_primary,
        // fg_hot is the text colour on a hot plate (highlight_text under high contrast): on the plain window it
        // would be black on black, so the plain-window text takes the primary there.
        FgRole::Hot if t.high_contrast => c.fg_primary,
        FgRole::Hot => c.fg_hot,
        FgRole::Secondary => c.fg_secondary,
        FgRole::Tertiary => c.fg_tertiary,
        FgRole::Disabled => c.fg_disabled,
        FgRole::Attention => c.accent_attention,
        FgRole::Danger => c.accent_danger,
    }
}

/// `a` towards `b` by `t` (0..=1): colour and alpha, rounded.
pub(crate) fn lerp_ink(a: Ink, b: Ink, t: f32) -> Ink {
    let l = |x: u32, y: u32| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u32;
    Ink { rgb: (l(a.rgb.0, b.rgb.0), l(a.rgb.1, b.rgb.1), l(a.rgb.2, b.rgb.2)), a: l(a.a, b.a) }
}

/// One line of text centred in `r` both ways, trimmed to its width; returns the width drawn.
pub(crate) fn text_centered(cx: &mut PaintCx, s: &str, role: TypeRole, r: RectP, ink: Ink) -> i32 {
    let (scale, ts) = (cx.env.scale, cx.env.tokens.text_scale);
    let font = cx.res.text.font(role, None, scale, ts);
    let opts = crate::text::LayoutOpts { max_width: Some(r.w().max(0)), trim: crate::text::Trim::End, wrap: false };
    let l = cx.res.text.layout(s, font, &opts);
    let at = PointP { x: r.l + (r.w() - l.width) / 2, y: r.t + (r.h() - l.height) / 2 };
    cx.text(&l, at, ink);
    l.width
}

/// A widget on its own in a Screen, for the widget tests: it fills `rect` (logical px) and collects the outputs.
#[cfg(test)]
pub(crate) struct Host<W: crate::screen::Widget> {
    pub(crate) w: W,
    pub(crate) rect: (i32, i32, i32, i32),
    pub(crate) outs: Vec<W::Out>,
    cmds: crate::command::CommandTable,
}

#[cfg(test)]
impl<W: crate::screen::Widget> Host<W> {
    pub(crate) fn new(w: W, rect: (i32, i32, i32, i32)) -> Host<W> {
        Host { w, rect, outs: Vec::new(), cmds: crate::command::CommandTable::new() }
    }
}

#[cfg(test)]
impl<W: crate::screen::Widget> crate::screen::Screen for Host<W> {
    fn layout(&mut self, cx: &mut crate::cx::LayoutCx, avail: crate::geom::SizeP) -> crate::geom::SizeP {
        let (x, y, w, h) = self.rect;
        self.w.layout(cx, RectP::wh(cx.px(x), cx.px(y), cx.px(w), cx.px(h)));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.w.paint(cx);
    }

    fn event(&mut self, cx: &mut crate::cx::EventCx, ev: &crate::event::Event) -> crate::event::Reply {
        if let Some(o) = self.w.event(cx, ev) {
            self.outs.push(o);
        }
        crate::event::Reply::None
    }

    fn commands(&self) -> &crate::command::CommandTable {
        &self.cmds
    }
}

impl Canvas<'_> {
    /// An inner ring of `bw` physical px of the rounded rectangle `rc` in any colour. With white ink it is
    /// Canvas::stroke_rr itself (the dock's ring, pixel for pixel); a coloured ring (high contrast) evaluates the same
    /// coverage per pixel.
    pub(crate) fn stroke_rr_ink(&mut self, rc: RECT, radius: i32, bw: i32, ink: Ink) {
        if ink.a == 0 {
            return;
        }
        if ink.rgb == (255, 255, 255) {
            self.stroke_rr(rc, radius, bw, ink.a);
            return;
        }
        for y in rc.top.max(0)..rc.bottom.min(self.h) {
            for x in rc.left.max(0)..rc.right.min(self.w) {
                let outer = rr_cov(x, y, rc.left, rc.top, rc.right, rc.bottom, radius);
                let inner = rr_cov(x, y, rc.left + bw, rc.top + bw, rc.right - bw, rc.bottom - bw, radius - bw);
                let a = (ink.a as f32 * (outer - inner).max(0.0) + 0.5) as u32;
                if a > 0 {
                    self.blend(x, y, argb(a, ink.rgb.0, ink.rgb.1, ink.rgb.2));
                }
            }
        }
    }
}
