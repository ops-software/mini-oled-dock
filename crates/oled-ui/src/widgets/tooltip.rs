//! Tooltip and NameChip (K1b.4, KB): the dispatcher's tip state and the stubs it calls (P2-kit 2.13, K0.1b), and the
//! chip painter (2.6, K0.1c), also D4a's later.

use crate::cx::{LayoutCx, PaintCx};
use crate::dispatch::Effect;
use crate::geom::{PointP, RectP, SizeP};
use crate::node::Node;

/// The tooltip's delay and warm state, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct TipState {
    /// When the shown tip (or the pending one) is due, if any.
    pub(crate) due: Option<u64>,
}

/// Inputs that hide or cancel a tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TipInput {
    Press,
    Key,
    Wheel,
    Deactivate,
}

/// A press, a key, the wheel or deactivation. Until KB: nothing.
pub(crate) fn on_input(tip: &mut TipState, what: TipInput, fx: &mut Vec<Effect>) {
    let _ = (tip, what, fx);
}

/// The pointer is over `node` (None: over nothing). Until KB: nothing.
pub(crate) fn on_hover(tip: &mut TipState, node: Option<&Node>, now_ms: u64, fx: &mut Vec<Effect>) {
    let _ = (tip, node, now_ms, fx);
}

/// The tooltip delay. Until KB: nothing.
pub(crate) fn on_due(tip: &mut TipState, now_ms: u64, fx: &mut Vec<Effect>) {
    let _ = (tip, now_ms, fx);
}

/// The next tooltip deadline.
pub(crate) fn next_due(tip: &TipState) -> Option<u64> {
    tip.due
}

/// Paints a chip (tooltip, NameChip) with its top-left at `at` and an optional chord; returns its rect. Filled by K1b.4;
/// until then it draws nothing and returns the empty rect at `at`.
pub fn paint_chip(cx: &mut PaintCx, at: PointP, text: &str, chord: Option<&str>) -> RectP {
    let _ = (cx, text, chord);
    RectP { l: at.x, t: at.y, r: at.x, b: at.y }
}

/// The size of that chip. Filled by K1b.4; until then 0 x 0.
pub fn chip_size(cx: &mut LayoutCx, text: &str, chord: Option<&str>) -> SizeP {
    let _ = (cx, text, chord);
    SizeP::default()
}
