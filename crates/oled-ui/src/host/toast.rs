//! Toasts (P2-kit 2.5, 2.13; K2b.6, KD2): in-window surfaces drawn above the screen, with hit priority and F6. K0.1b
//! writes the state type and the stubs the dispatcher and the hooks call.

use crate::cx::{PaintCx, ToastSpec};
use crate::dispatch::Effect;
use crate::geom::PointP;

/// The window's toasts, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct ToastStack {
    /// The toasts shown, oldest first (Harness::toasts lists them).
    pub(crate) specs: Vec<ToastSpec>,
    /// LayoutCx::set_toast_baseline: toasts sit 12 px above it.
    pub(crate) baseline: Option<i32>,
}

/// EventCx::toast. Until KD2: the spec is kept, so Harness::toasts lists it.
pub(crate) fn push(stack: &mut ToastStack, t: ToastSpec, now_ms: u64) {
    let _ = now_ms;
    stack.specs.push(t);
}

/// A press: Some(index) when a toast takes it. Until KD2: None.
pub(crate) fn hit(stack: &mut ToastStack, p: PointP) -> Option<usize> {
    let _ = (stack, p);
    None
}

/// F6 to the toasts first; true when one took the focus. Until KD2: false.
pub(crate) fn on_f6(stack: &mut ToastStack) -> bool {
    let _ = stack;
    false
}

/// The hold deadline. Until KD2: nothing.
pub(crate) fn on_due(stack: &mut ToastStack, now_ms: u64, fx: &mut Vec<Effect>) {
    let _ = (stack, now_ms, fx);
}

/// The next hold deadline. Until KD2: none.
pub(crate) fn next_due(stack: &ToastStack) -> Option<u64> {
    let _ = stack;
    None
}

/// Draws the toasts in a band (hooks::after_paint_band). Until KD2: nothing.
pub(crate) fn paint(cx: &mut PaintCx<'_>) {
    let _ = (cx.disp.toasts.baseline, cx.disp.toasts.specs.len());
}
