//! Tooltip and NameChip (K1b.4, KB): the dispatcher's tip state (P2-kit 2.13, K0.1b) and the chip painter (2.6, K0.1c),
//! also D4a's later.
//!
//! The timing is a pure state machine over the hovered node: the pointer must rest TOOLTIP_DELAY on a node that has a
//! description; another tip that hid less than TOOLTIP_WARM ago makes the next one come at once; a press, a key, the wheel
//! or the window's deactivation dismisses the tip and keeps it away until the pointer is on another node. The host
//! (host/tooltip.rs) shows and hides the chip window on the effects.

use crate::cx::{LayoutCx, PaintCx};
use crate::dispatch::Effect;
use crate::geom::{PointP, RectP, SizeP};
use crate::model::Align;
use crate::node::{Node, NodeId, Text};
use crate::text::Trim;
use crate::theme::TypeRole;
use crate::tokens::{TOOLTIP_DELAY, TOOLTIP_WARM};

/// A tip waiting for its delay.
struct Pending {
    node: NodeId,
    rect: RectP,
    text: Text,
    chord: Option<Text>,
}

/// The tooltip's delay and warm state, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct TipState {
    /// When the pending tip is due, if any.
    pub(crate) due: Option<u64>,
    pending: Option<Pending>,
    /// The node whose tip is on screen.
    shown: Option<NodeId>,
    /// The node a press, key, wheel or deactivation dismissed the tip of: no tip for it until the pointer is elsewhere.
    dismissed: Option<NodeId>,
    /// When a tip last went away because the pointer left or moved to another node.
    last_hidden: Option<u64>,
}

impl TipState {
    /// The pointer left the node (or moved to one without a tip): nothing pends, a shown tip hides, and the time is what
    /// the warm rule counts from.
    fn leave(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        self.pending = None;
        self.due = None;
        self.dismissed = None;
        if self.shown.take().is_some() {
            fx.push(Effect::HideTip);
            self.last_hidden = Some(now_ms);
        }
    }

    fn show(&mut self, p: Pending, fx: &mut Vec<Effect>) {
        self.shown = Some(p.node);
        self.due = None;
        fx.push(Effect::ShowTip { node: p.node, rect: p.rect, text: p.text, chord: p.chord });
    }
}

/// Inputs that hide or cancel a tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TipInput {
    Press,
    Key,
    Wheel,
    Deactivate,
}

/// The separator between a tip's text and its chord in a description: "Back · Alt+Left".
const CHORD_SEP: &str = " \u{b7} "; // lint: not UI

/// A press, a key, the wheel or deactivation: the tip goes and stays away for the node it was for. A dismissal is not
/// "a tip hid a moment ago": the next node waits the full delay.
pub(crate) fn on_input(tip: &mut TipState, what: TipInput, fx: &mut Vec<Effect>) {
    let _ = what;
    let was_shown = tip.shown.take();
    if was_shown.is_some() {
        fx.push(Effect::HideTip);
    }
    let node = was_shown.or_else(|| tip.pending.take().map(|p| p.node));
    tip.pending = None;
    tip.due = None;
    tip.last_hidden = None;
    if node.is_some() {
        tip.dismissed = node;
    }
}

/// The pointer is over node (None: over nothing).
pub(crate) fn on_hover(tip: &mut TipState, node: Option<&Node>, now_ms: u64, fx: &mut Vec<Effect>) {
    let Some(n) = node.filter(|n| n.description.as_ref().is_some_and(|d| !d.is_empty())) else {
        tip.leave(now_ms, fx);
        return;
    };
    if tip.shown == Some(n.id) || tip.pending.as_ref().is_some_and(|p| p.node == n.id) || tip.dismissed == Some(n.id) {
        return;
    }
    // Another node than the one the tip is for.
    tip.leave(now_ms, fx);
    let desc = n.description.clone().unwrap_or_default();
    let (text, chord): (Text, Option<Text>) = match desc.split_once(CHORD_SEP) {
        Some((t, c)) => (Text::from(t.to_string()), Some(Text::from(c.to_string()))),
        None => (desc, None),
    };
    let warm = tip.last_hidden.is_some_and(|t| now_ms.saturating_sub(t) < u64::from(TOOLTIP_WARM));
    let pending = Pending { node: n.id, rect: n.rect, text, chord };
    if warm {
        tip.show(pending, fx);
    } else {
        tip.due = Some(now_ms + u64::from(TOOLTIP_DELAY));
        tip.pending = Some(pending);
    }
}

/// The tooltip delay: the pending tip shows once its deadline is reached.
pub(crate) fn on_due(tip: &mut TipState, now_ms: u64, fx: &mut Vec<Effect>) {
    if tip.due.is_some_and(|d| d <= now_ms) {
        if let Some(p) = tip.pending.take() {
            tip.show(p, fx);
        }
        tip.due = None;
    }
}

/// The next tooltip deadline.
pub(crate) fn next_due(tip: &TipState) -> Option<u64> {
    tip.due
}

/// The chip's text padding at the sides, logical px.
const PAD_X: i32 = 8;
/// The widest a chip grows before its text is trimmed, logical px.
const MAX_W: i32 = 360;

/// The chord part as it is drawn: the separator and the chord.
fn chord_run(chord: &str) -> String {
    format!("{CHORD_SEP}{chord}") // lint: not UI
}

/// The chip's width in physical px from the widths of its runs.
fn width_of(text_w: i32, chord_w: i32, pad: i32, max: i32) -> i32 {
    (text_w + chord_w + 2 * pad).min(max)
}

/// Paints a chip (tooltip, NameChip) with its top-left at at and an optional chord; returns its rect. Caption text in
/// FG_PRIMARY on the window colour with a 1 px RIM_CONTROL rim at R_S; the chord follows after " · " in FG_TERTIARY.
pub fn paint_chip(cx: &mut PaintCx, at: PointP, text: &str, chord: Option<&str>) -> RectP {
    let (scale, ts) = (cx.env.scale, cx.env.tokens.text_scale);
    let font = cx.res.text.font(TypeRole::Caption, None, scale, ts);
    let run = chord.map(chord_run);
    let text_w = cx.res.text.width(text, font);
    let chord_w = run.as_deref().map_or(0, |r| cx.res.text.width(r, font));
    let (pad, h) = (cx.px(PAD_X), cx.px(cx.tokens().size.chip_h));
    let r = RectP::wh(at.x, at.y, width_of(text_w, chord_w, pad, cx.px(MAX_W)), h);
    let radius = cx.px(cx.tokens().radius.s);
    let c = cx.tokens().color.clone();
    cx.fill(r, radius, c.bg);
    cx.stroke(r, radius, cx.px(1).max(1), c.rim_control);
    let room = RectP { l: r.l + pad, r: r.r - pad, ..r };
    let w = cx.text_in(text, TypeRole::Caption, room, Align::Start, Trim::End, c.fg_primary);
    if let Some(run) = run {
        cx.text_in(&run, TypeRole::Caption, RectP { l: room.l + w, ..room }, Align::Start, Trim::End, c.fg_tertiary);
    }
    r
}

/// The size of that chip.
pub fn chip_size(cx: &mut LayoutCx, text: &str, chord: Option<&str>) -> SizeP {
    let font = cx.font(TypeRole::Caption);
    let text_w = cx.text().width(text, font);
    let chord_w = chord.map_or(0, |c| cx.text().width(&chord_run(c), font));
    SizeP { w: width_of(text_w, chord_w, cx.px(PAD_X), cx.px(MAX_W)), h: cx.px(cx.tokens().size.chip_h) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Role, WidgetId};

    fn node(slot: u32, desc: &'static str) -> Node {
        Node::new(NodeId::new(WidgetId(1), slot), Role::Button, RectP::wh(10, 10, 28, 28)).description(Text::from(desc))
    }

    fn shown(fx: &[Effect]) -> Vec<NodeId> {
        fx.iter().filter_map(|e| if let Effect::ShowTip { node, .. } = e { Some(*node) } else { None }).collect()
    }

    fn hides(fx: &[Effect]) -> usize {
        fx.iter().filter(|e| matches!(e, Effect::HideTip)).count()
    }

    #[test]
    fn tip_waits_500_then_warm_1000() {
        let (a, b) = (node(1, "Back"), node(2, "Forward"));
        let (mut tip, mut fx) = (TipState::default(), Vec::new());
        on_hover(&mut tip, Some(&a), 100, &mut fx);
        assert!(fx.is_empty());
        assert_eq!(next_due(&tip), Some(600), "TOOLTIP_DELAY after the pointer came");
        on_hover(&mut tip, Some(&a), 400, &mut fx);
        assert_eq!(next_due(&tip), Some(600), "moving inside the node does not restart the wait");
        on_due(&mut tip, 599, &mut fx);
        assert!(fx.is_empty());
        on_due(&mut tip, 600, &mut fx);
        assert_eq!(shown(&fx), [a.id]);
        assert_eq!(next_due(&tip), None);
        // Over the next node 300 ms after leaving the first: at once (warm).
        fx.clear();
        on_hover(&mut tip, None, 1000, &mut fx);
        assert_eq!(hides(&fx), 1);
        fx.clear();
        on_hover(&mut tip, Some(&b), 1300, &mut fx);
        assert_eq!(shown(&fx), [b.id], "a tip hid 300 ms ago: no wait");
        // Straight from one node to the other: hides the first, shows the second at once.
        fx.clear();
        on_hover(&mut tip, Some(&a), 1500, &mut fx);
        assert_eq!((hides(&fx), shown(&fx)), (1, vec![a.id]));
        // Cold again after TOOLTIP_WARM without a tip.
        fx.clear();
        on_hover(&mut tip, None, 1600, &mut fx);
        fx.clear();
        on_hover(&mut tip, Some(&b), 2700, &mut fx);
        assert!(fx.is_empty());
        assert_eq!(next_due(&tip), Some(3200));
    }

    #[test]
    fn a_node_without_a_description_has_no_tip_and_ends_the_one_before() {
        let (a, plain) = (node(1, "Back"), Node::new(NodeId::new(WidgetId(1), 9), Role::Button, RectP::wh(0, 0, 5, 5)));
        let (mut tip, mut fx) = (TipState::default(), Vec::new());
        on_hover(&mut tip, Some(&a), 0, &mut fx);
        on_hover(&mut tip, Some(&plain), 100, &mut fx);
        assert_eq!(next_due(&tip), None, "the pending tip is cancelled");
        on_due(&mut tip, 1000, &mut fx);
        assert!(fx.is_empty());
        let empty = node(3, "");
        on_hover(&mut tip, Some(&empty), 200, &mut fx);
        assert_eq!(next_due(&tip), None);
    }

    #[test]
    fn tip_hides_on_press_and_key() {
        for what in [TipInput::Press, TipInput::Key, TipInput::Wheel, TipInput::Deactivate] {
            let a = node(1, "Back");
            let (mut tip, mut fx) = (TipState::default(), Vec::new());
            on_hover(&mut tip, Some(&a), 0, &mut fx);
            on_due(&mut tip, 500, &mut fx);
            fx.clear();
            on_input(&mut tip, what, &mut fx);
            assert_eq!(hides(&fx), 1, "{what:?} hides the shown tip");
            // The pointer is still on the node: it does not come back.
            fx.clear();
            on_hover(&mut tip, Some(&a), 600, &mut fx);
            on_due(&mut tip, 5000, &mut fx);
            assert!(fx.is_empty() && next_due(&tip).is_none(), "{what:?}: dismissed until the pointer leaves");
            // Leaving and coming back starts again, cold.
            on_hover(&mut tip, None, 700, &mut fx);
            on_hover(&mut tip, Some(&a), 800, &mut fx);
            assert_eq!(next_due(&tip), Some(1300));
        }
        // A press while only pending cancels it and closes no window.
        let a = node(1, "Back");
        let (mut tip, mut fx) = (TipState::default(), Vec::new());
        on_hover(&mut tip, Some(&a), 0, &mut fx);
        on_input(&mut tip, TipInput::Press, &mut fx);
        assert!(fx.is_empty());
        assert_eq!(next_due(&tip), None);
        // A key with nothing to dismiss does nothing, and does not make the next node wait less or more.
        let mut tip = TipState::default();
        on_input(&mut tip, TipInput::Key, &mut fx);
        on_hover(&mut tip, Some(&a), 10, &mut fx);
        assert_eq!(next_due(&tip), Some(510));
    }

    #[test]
    fn a_chord_after_the_separator_is_split_off() {
        let a = node(1, "Back \u{b7} Alt+Left");
        let (mut tip, mut fx) = (TipState::default(), Vec::new());
        on_hover(&mut tip, Some(&a), 0, &mut fx);
        on_due(&mut tip, 500, &mut fx);
        match &fx[..] {
            [Effect::ShowTip { text, chord, rect, .. }] => {
                assert_eq!(&**text, "Back");
                assert_eq!(chord.as_deref(), Some("Alt+Left"));
                assert_eq!(*rect, a.rect);
            }
            other => panic!("{other:?}"),
        }
    }
}
