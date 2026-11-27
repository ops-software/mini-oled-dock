//! The column-span graph (PaintCx::spans, P2-kit 2.13): filled by K4.1 (KF).

use crate::cx::PaintCx;
use crate::theme::Ink;

/// Draws one vertical run (y0, y1) per column from x0. Until K4.1: nothing.
pub(crate) fn draw(cx: &mut PaintCx<'_>, x0: i32, spans: &[(i16, i16)], ink: Ink) {
    let _ = (cx, x0, spans, ink);
}
