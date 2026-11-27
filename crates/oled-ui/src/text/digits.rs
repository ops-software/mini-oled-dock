//! The digit atlas and decimal-aligned figures (PaintCx::digits, P2-kit 2.13): filled by K2a.4 (KC1). Until then the
//! figures are drawn through the text engine.

use crate::cx::PaintCx;
use crate::geom::{PointP, RectP};
use crate::model::Align;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// Draws `s` right-aligned at `right_top`; returns the width drawn.
pub(crate) fn draw(
    cx: &mut PaintCx<'_>,
    s: &str,
    right_top: PointP,
    role: TypeRole,
    ink: Ink,
    unit: Ink,
    decimal: bool,
) -> i32 {
    let _ = (unit, decimal);
    let (scale, ts) = (cx.env.scale, cx.env.tokens.text_scale);
    let font = cx.res.text.font(role, None, scale, ts);
    let w = cx.res.text.width(s, font);
    let h = scale.px(role.spec().line);
    let r = RectP { l: right_top.x - w, t: right_top.y, r: right_top.x, b: right_top.y + h };
    cx.text_in(s, role, r, Align::End, Trim::None, ink);
    w
}
