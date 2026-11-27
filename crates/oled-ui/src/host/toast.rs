//! Toasts (P2-kit 2.5, 2.13; K2b.6, KD2): in-window surfaces drawn above the screen, with hit priority and F6. K0.1b
//! wrote the state type and the entry points the dispatcher, the hooks and the Harness call; this fills them.
//!
//! Bottom centre, 12 px above the toast baseline (LayoutCx::set_toast_baseline, else the client bottom), TOAST_H 40,
//! up to TOAST_W_MAX 480, R_M 10, the window colour with RIM_SURFACE, Body text, an optional action as a secondary
//! button and an optional 2 px progress line. A toast stays TOAST_HOLD ms (paused while the pointer is over it or it
//! holds the toast focus), slides in over DUR_NORMAL by SLIDE_PX and fades out over DUR_EXIT; three at most, the oldest
//! goes. Nothing runs while none is shown: the next deadline is the hold's end, or the next frame of a slide or fade.
//!
//! Geometry is made at layout (Dispatcher::layout calls `layout` after the screen) so that the dispatcher and the
//! accessibility tree see the toasts as nodes; the slide and the fade are read from the clock at paint.

use crate::command::CommandId;
use crate::cx::{LayoutCx, PaintCx, ToastSpec};
use crate::dispatch::Effect;
use crate::event::Key;
use crate::geom::{PointP, RectP};
use crate::model::Align;
use crate::node::{Actions, Node, NodeId, Role, WidgetId};
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// The toast stack holds at most this many.
const MAX_TOASTS: usize = 3;
/// The space between two stacked toasts, and above the baseline (logical px).
const STACK_GAP: i32 = 8;
const BASELINE_GAP: i32 = 12;
/// Horizontal padding in a toast, the gap before its action, the action's own padding (logical px).
const PAD: i32 = 12;
const ACTION_GAP: i32 = 12;
const ACTION_PAD: i32 = 12;
/// The progress line's height (logical px).
const PROGRESS_H: i32 = 2;
/// The frame step of a slide or a fade, ms.
const FRAME_MS: u64 = 16;
/// The widget id of the toast layer (a kit owner: toasts are nodes of `TOAST_OWNER`).
const TOAST_OWNER: WidgetId = WidgetId(0x7F_FF00);

/// The times of one toast.
#[derive(Clone, Copy, Debug)]
struct Life {
    born: u64,
    /// When it starts to leave; None while it is paused.
    due: Option<u64>,
    /// When it started to leave.
    leaving: Option<u64>,
}

/// Where a toast stands after the last layout (physical px).
#[derive(Clone, Copy, Debug)]
struct Placed {
    rect: RectP,
    action: Option<RectP>,
}

/// The durations the clock reads, taken from the tokens at each layout.
#[derive(Clone, Copy, Debug)]
struct Timing {
    dur_in: u64,
    dur_out: u64,
    hold: u64,
    slide: i32,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            dur_in: u64::from(crate::tokens::DUR_NORMAL),
            dur_out: u64::from(crate::tokens::DUR_EXIT),
            hold: u64::from(crate::tokens::TOAST_HOLD),
            slide: crate::tokens::SLIDE_PX_POPUP,
        }
    }
}

/// The window's toasts, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct ToastStack {
    /// The toasts shown, oldest first (Harness::toasts lists them).
    pub(crate) specs: Vec<ToastSpec>,
    /// LayoutCx::set_toast_baseline: toasts sit 12 px above it.
    pub(crate) baseline: Option<i32>,
    life: Vec<Life>,
    placed: Vec<Placed>,
    timing: Timing,
    /// The toast under the pointer and the one that holds the toast focus (F6).
    hot: Option<usize>,
    focus: Option<usize>,
    /// The action of the toast the last press or Enter ran, for the dispatcher to send.
    action: Option<CommandId>,
    /// The last time the stack saw (push, due, hover).
    now: u64,
}

impl ToastStack {
    fn paused(&self, i: usize) -> bool {
        self.hot == Some(i) || self.focus == Some(i)
    }

    /// Re-arms or suspends each toast's hold after the pause state changed.
    fn refresh_holds(&mut self, now: u64) {
        for i in 0..self.life.len() {
            let paused = self.paused(i);
            let l = &mut self.life[i];
            if l.leaving.is_some() {
                continue;
            }
            match (paused, l.due) {
                (true, Some(_)) => l.due = None,
                (false, None) => {
                    let hold = self.specs[i].hold_ms.map_or(self.timing.hold, u64::from);
                    l.due = Some(now + hold);
                }
                _ => {}
            }
        }
    }

    fn remove(&mut self, i: usize) {
        self.specs.remove(i);
        self.life.remove(i);
        if i < self.placed.len() {
            self.placed.remove(i);
        }
        let fix = |v: &mut Option<usize>| {
            *v = match *v {
                Some(j) if j == i => None,
                Some(j) if j > i => Some(j - 1),
                other => other,
            }
        };
        fix(&mut self.hot);
        fix(&mut self.focus);
    }

    fn start_leaving(&mut self, i: usize, now: u64) {
        if let Some(l) = self.life.get_mut(i) {
            if l.leaving.is_none() {
                l.leaving = Some(now);
                l.due = None;
            }
        }
    }
}

/// EventCx::toast: the spec is kept (Harness::toasts lists it) and its hold starts.
pub(crate) fn push(stack: &mut ToastStack, t: ToastSpec, now_ms: u64) {
    stack.now = now_ms;
    if stack.specs.len() >= MAX_TOASTS {
        stack.remove(0);
    }
    let hold = t.hold_ms.map_or(stack.timing.hold, u64::from);
    stack.life.push(Life { born: now_ms, due: Some(now_ms + hold), leaving: None });
    stack.specs.push(t);
}

/// A press: Some(index) when a toast takes it. A press on the action arms its command (take_action); a press on the
/// body dismisses the toast.
pub(crate) fn hit(stack: &mut ToastStack, p: PointP) -> Option<usize> {
    let i = (0..stack.placed.len())
        .rev()
        .find(|i| stack.life[*i].leaving.is_none() && stack.placed[*i].rect.contains(p))?;
    let on_action = stack.placed[i].action.is_some_and(|a| a.contains(p));
    if on_action {
        stack.action = stack.specs[i].action.as_ref().map(|a| a.1);
    }
    let now = stack.now;
    stack.start_leaving(i, now);
    Some(i)
}

/// The command a press or key armed, once.
pub(crate) fn take_action(stack: &mut ToastStack) -> Option<CommandId> {
    stack.action.take()
}

/// The pointer moved (None: it left the window): a toast under it keeps its hold paused.
pub(crate) fn on_hover(stack: &mut ToastStack, p: Option<PointP>, now_ms: u64) {
    stack.now = now_ms;
    let hot = p.and_then(|p| {
        (0..stack.placed.len()).rev().find(|i| stack.life[*i].leaving.is_none() && stack.placed[*i].rect.contains(p))
    });
    if hot != stack.hot {
        stack.hot = hot;
        stack.refresh_holds(now_ms);
    }
}

/// F6 to the toasts first; true when one took the focus. A second F6 gives it back (false: the pane cycle goes on).
pub(crate) fn on_f6(stack: &mut ToastStack) -> bool {
    let now = stack.now;
    if stack.focus.is_some() {
        stack.focus = None;
        stack.refresh_holds(now);
        return false;
    }
    match (0..stack.specs.len()).rev().find(|i| stack.life[*i].leaving.is_none()) {
        Some(i) => {
            stack.focus = Some(i);
            stack.refresh_holds(now);
            true
        }
        None => false,
    }
}

/// A key while a toast holds the toast focus: Enter runs its action (or dismisses it), Esc dismisses it. The outer
/// option is None when the key is not the toast's; the inner one is the command to send.
pub(crate) fn on_key(stack: &mut ToastStack, key: Key) -> Option<Option<CommandId>> {
    let i = stack.focus?;
    let now = stack.now;
    let act = match key {
        Key::ENTER => stack.specs.get(i).and_then(|s| s.action.as_ref().map(|a| a.1)),
        Key::ESC => None,
        _ => return None,
    };
    stack.start_leaving(i, now);
    stack.focus = None;
    stack.refresh_holds(now);
    Some(act)
}

/// The hold deadline: toasts whose time has come start to leave, and the ones that have left are removed.
pub(crate) fn on_due(stack: &mut ToastStack, now_ms: u64, fx: &mut Vec<Effect>) {
    stack.now = now_ms;
    let mut changed = false;
    for i in 0..stack.life.len() {
        if stack.life[i].leaving.is_none() && stack.life[i].due.is_some_and(|d| d <= now_ms) {
            stack.start_leaving(i, now_ms);
            changed = true;
        }
    }
    let out = stack.timing.dur_out;
    while let Some(i) = stack.life.iter().position(|l| l.leaving.is_some_and(|s| s + out <= now_ms)) {
        stack.remove(i);
        changed = true;
    }
    if changed || stack.life.iter().any(|l| animating(l, stack.timing, now_ms)) {
        fx.push(Effect::Relayout);
    }
}

/// True while a toast slides in or fades out at `now`.
fn animating(l: &Life, t: Timing, now: u64) -> bool {
    l.leaving.is_some() || now < l.born + t.dur_in
}

/// The next hold deadline, or the next frame while one slides or fades. None when no toast is shown.
pub(crate) fn next_due(stack: &ToastStack) -> Option<u64> {
    let mut next: Option<u64> = None;
    let mut min = |t: u64| next = Some(next.map_or(t, |n| n.min(t)));
    for l in &stack.life {
        if let Some(s) = l.leaving {
            min((stack.now + FRAME_MS).min(s + stack.timing.dur_out));
        } else {
            if let Some(d) = l.due {
                min(d);
            }
            if stack.now < l.born + stack.timing.dur_in {
                min(stack.now + FRAME_MS);
            }
        }
    }
    next
}

/// Lays the toasts out for a client of `client` (called after the screen's layout): their rectangles, and a node each.
pub(crate) fn layout(cx: &mut LayoutCx<'_>, client: crate::geom::SizeP) {
    let t = cx.tokens().motion;
    let size = cx.tokens().size.clone();
    let timing = Timing {
        dur_in: u64::from(t.dur_normal),
        dur_out: u64::from(t.dur_exit),
        hold: u64::from(t.toast_hold),
        slide: t.slide_px,
    };
    cx.disp.toasts.timing = timing;
    let n = cx.disp.toasts.specs.len();
    cx.disp.toasts.placed.clear();
    if n == 0 {
        return;
    }
    let font = cx.font(TypeRole::Body);
    let (pad, gap, apad) = (cx.px(PAD), cx.px(ACTION_GAP), cx.px(ACTION_PAD));
    let (h, wmax) = (cx.px(size.toast_h), cx.px(size.toast_w_max).min(client.w - 2 * cx.px(STACK_GAP)).max(0));
    let base = cx.disp.toasts.baseline.unwrap_or(client.h) - cx.px(BASELINE_GAP);
    let specs = cx.disp.toasts.specs.clone();
    let mut placed = Vec::with_capacity(n);
    let mut bottom = base;
    for spec in specs.iter().rev() {
        let text_w = cx.text().width(&spec.text, font);
        let action_w = spec.action.as_ref().map(|a| cx.text().width(&a.0, font) + 2 * apad);
        let w = (pad + text_w + action_w.map_or(0, |a| gap + a) + pad).min(wmax);
        let (l, t) = ((client.w - w) / 2, bottom - h);
        let rect = RectP::wh(l, t, w, h);
        let action = action_w.map(|aw| {
            let bh = cx.px(size.control_h).min(h - cx.px(8));
            RectP::wh(rect.r - pad / 2 - aw, rect.t + (h - bh) / 2, aw, bh)
        });
        placed.push(Placed { rect, action });
        bottom = t - cx.px(STACK_GAP);
    }
    placed.reverse();
    for (i, (p, spec)) in placed.iter().zip(&specs).enumerate() {
        let id = NodeId::new(TOAST_OWNER, 2 * i as u32);
        cx.push(Node::new(id, Role::Alert, p.rect).label(spec.text.clone()));
        if let (Some(a), Some((label, _))) = (p.action, spec.action.as_ref()) {
            let bid = NodeId::new(TOAST_OWNER, 2 * i as u32 + 1);
            cx.push(Node::new(bid, Role::Button, a).parent(id).label(label.clone()).actions(Actions::CLICK));
        }
    }
    cx.disp.toasts.placed = placed;
}

/// `i` scaled by `f` (0..=1) in alpha.
fn faded(ink: Ink, f: f32) -> Ink {
    Ink { a: (ink.a as f32 * f.clamp(0.0, 1.0)).round() as u32, ..ink }
}

/// Draws the toasts in a band (hooks::after_paint_band).
pub(crate) fn paint(cx: &mut PaintCx<'_>) {
    let disp = cx.disp;
    let st = &disp.toasts;
    if st.placed.is_empty() {
        return;
    }
    let now = cx.now_ms();
    let c = cx.tokens().color.clone();
    let radius = cx.px(cx.tokens().radius.m);
    let (pad, px1) = (cx.px(PAD), cx.px(1).max(1));
    for (i, (p, spec)) in st.placed.iter().zip(&st.specs).enumerate() {
        let life = st.life[i];
        // In: slides up by SLIDE_PX and fades in over DUR_NORMAL; out: fades over DUR_EXIT.
        let (alpha, dy) = match life.leaving {
            Some(s) => (1.0 - (now.saturating_sub(s)) as f32 / st.timing.dur_out.max(1) as f32, 0),
            None => {
                let t = ((now.saturating_sub(life.born)) as f32 / st.timing.dur_in.max(1) as f32).clamp(0.0, 1.0);
                let e = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
                (e, (cx.px(st.timing.slide) as f32 * (1.0 - e)).round() as i32)
            }
        };
        if alpha <= 0.0 {
            continue;
        }
        let at = |r: RectP| RectP { l: r.l, t: r.t + dy, r: r.r, b: r.b + dy };
        let r = at(p.rect);
        cx.fill(r, radius, faded(c.bg, alpha));
        cx.stroke(r, radius, px1, faded(c.rim_surface, alpha));
        let text_r = RectP { l: r.l + pad, r: p.action.map_or(r.r - pad, |a| a.l - cx.px(ACTION_GAP)), ..r };
        cx.text_in(&spec.text, TypeRole::Body, text_r, Align::Start, Trim::End, faded(c.fg_primary, alpha));
        if let (Some(a), Some((label, _))) = (p.action, spec.action.as_ref()) {
            let a = at(a);
            let rs = cx.px(cx.tokens().radius.s);
            cx.fill(a, rs, faded(c.fill_control, alpha));
            cx.stroke(a, rs, px1, faded(c.rim_control, alpha));
            crate::widgets::paint::text_centered(cx, label, TypeRole::Body, a, faded(c.fg_primary, alpha));
        }
        if let Some(v) = spec.progress {
            let h = cx.px(PROGRESS_H);
            let inner = RectP { l: r.l + radius, r: r.r - radius, ..r };
            let w = (inner.w() as f32 * v.clamp(0.0, 1.0)).round() as i32;
            cx.fill(RectP { l: inner.l, t: r.b - h, r: inner.l + w, b: r.b }, 0, faded(c.fg_secondary, alpha));
        }
        if st.focus == Some(i) {
            cx.focus_ring(r, radius, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx};
    use crate::event::{Event, Reply};
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;
    use std::borrow::Cow;

    /// A window that shows toasts when told to and counts the commands it receives.
    struct Win {
        cmds: CommandTable,
        got: Vec<CommandId>,
    }

    impl Screen for Win {
        fn layout(&mut self, _: &mut LayoutCx, avail: SizeP) -> SizeP {
            avail
        }

        fn paint(&self, _: &mut PaintCx) {}

        fn event(&mut self, _: &mut EventCx, ev: &Event) -> Reply {
            if let Event::Command(id) = ev {
                self.got.push(*id);
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(t) = msg.downcast::<ToastSpec>() {
                cx.toast(*t);
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn window() -> Harness<Win> {
        Harness::new(Win { cmds: CommandTable::new(), got: Vec::new() }, SizeL { w: 400, h: 200 }, 96, Tokens::oled())
    }

    fn show(h: &mut Harness<Win>, t: ToastSpec) {
        h.send(Box::new(t));
    }

    fn alerts(h: &mut Harness<Win>) -> Vec<RectP> {
        h.dispatcher().frame.iter().filter(|n| n.role == Role::Alert).map(|n| n.rect).collect()
    }

    #[test]
    fn toast_hold_pauses_on_hover() {
        let mut h = window();
        show(&mut h, spec("Copied"));
        let r = alerts(&mut h)[0];
        assert_eq!(r.h(), 40);
        assert_eq!(r.b, 200 - 12, "12 px above the client bottom");
        assert_eq!((r.l + r.r) / 2, 200, "centred");
        h.hover(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
        h.advance(9000);
        assert_eq!(h.toasts().len(), 1, "a hovered toast stays");
        h.hover(PointL { x: 2, y: 2 });
        h.advance(3900);
        assert_eq!(h.toasts().len(), 1, "a full hold after the pointer left");
        h.advance(300);
        assert!(h.toasts().is_empty());
        assert!(alerts(&mut h).is_empty());
    }

    #[test]
    fn toast_stack_of_three() {
        let mut h = window();
        for t in ["one", "two", "three", "four"] {
            show(&mut h, spec(t));
            h.advance(100);
        }
        let texts: Vec<_> = h.toasts().iter().map(|t| t.text.to_string()).collect();
        assert_eq!(texts, ["two", "three", "four"]);
        let a = alerts(&mut h);
        assert_eq!(a.len(), 3);
        assert!(a[0].b < a[1].t && a[1].b < a[2].t, "the newest is the lowest, 8 px apart: {a:?}");
        assert_eq!(a[2].t - a[1].b, 8);
    }

    #[test]
    fn baseline_and_width() {
        let mut h = window();
        let mut t = spec("A very long message that goes on and on and on and on and on and on and on and on and on");
        t.progress = Some(0.5);
        show(&mut h, t);
        h.advance(200);
        let r = alerts(&mut h)[0];
        assert!(r.w() <= 480 && r.w() >= 400 - 16 - 1, "capped by the window: {}", r.w());
        let img = h.render();
        assert!(!img.text_rects.is_empty());
        let prog = img.px[((r.b - 1) as u32 * img.w + (r.l + 40) as u32) as usize] & 0xFF;
        assert!(prog > 0, "the progress line along the bottom");
    }

    #[test]
    fn f6_reaches_the_toast_and_enter_runs_its_action() {
        let mut h = window();
        let mut t = spec("Moved 3 items");
        t.action = Some((Cow::Borrowed("Undo"), CommandId(42)));
        show(&mut h, t);
        h.key("F6");
        h.advance(9000);
        assert_eq!(h.toasts().len(), 1, "a toast that holds the focus stays");
        h.key("Enter");
        assert_eq!(h.screen().got, [CommandId(42)]);
        h.advance(200);
        assert!(h.toasts().is_empty());
    }

    #[test]
    fn a_press_on_the_action_runs_it() {
        let mut h = window();
        let mut t = spec("Moved");
        t.action = Some((Cow::Borrowed("Undo"), CommandId(43)));
        show(&mut h, t);
        let b = h.dispatcher().frame.iter().find(|n| n.role == Role::Button).map(|n| n.rect).unwrap();
        h.click(PointL { x: (b.l + b.r) / 2, y: (b.t + b.b) / 2 });
        assert_eq!(h.screen().got, [CommandId(43)]);
        h.advance(200);
        assert!(h.toasts().is_empty());
    }

    #[test]
    fn all_idle_afterwards() {
        let mut h = window();
        show(&mut h, spec("Saved"));
        assert!(!h.is_idle());
        h.advance(4000 + 83 + 32);
        assert!(h.toasts().is_empty());
        assert!(h.is_idle(), "no timer, tween or deadline is left");
        let _ = h.render();
        assert!(h.is_idle());
    }

    fn spec(text: &'static str) -> ToastSpec {
        ToastSpec { text: Cow::Borrowed(text), action: None, progress: None, hold_ms: None }
    }

    #[test]
    fn holds_end_and_exits_finish() {
        let mut s = ToastStack::default();
        push(&mut s, spec("a"), 0);
        let mut fx = Vec::new();
        on_due(&mut s, 3999, &mut fx);
        assert_eq!(s.specs.len(), 1);
        on_due(&mut s, 4000, &mut fx);
        assert!(s.life[0].leaving.is_some(), "the hold is over");
        assert_eq!(s.specs.len(), 1, "it fades first");
        on_due(&mut s, 4000 + 83, &mut fx);
        assert!(s.specs.is_empty());
        assert_eq!(next_due(&s), None);
    }

    #[test]
    fn oldest_goes_past_three() {
        let mut s = ToastStack::default();
        for t in ["a", "b", "c", "d"] {
            push(&mut s, spec(t), 0);
        }
        assert_eq!(s.specs.iter().map(|t| t.text.as_ref()).collect::<Vec<_>>(), ["b", "c", "d"]);
    }

    #[test]
    fn pause_suspends_and_resume_restarts_the_hold() {
        let mut s = ToastStack::default();
        push(&mut s, spec("a"), 0);
        s.placed.push(Placed { rect: RectP::wh(0, 0, 100, 40), action: None });
        on_hover(&mut s, Some(PointP { x: 5, y: 5 }), 1000);
        assert_eq!(s.life[0].due, None);
        on_hover(&mut s, None, 9000);
        assert_eq!(s.life[0].due, Some(13000), "a full hold after the pointer leaves");
        assert!(on_f6(&mut s) && s.focus == Some(0));
        assert_eq!(s.life[0].due, None);
        assert!(!on_f6(&mut s), "a second F6 gives the focus back");
        assert!(s.life[0].due.is_some());
    }

    #[test]
    fn enter_runs_the_action_of_the_focused_toast() {
        let mut s = ToastStack::default();
        let mut t = spec("Moved");
        t.action = Some((Cow::Borrowed("Undo"), CommandId(7)));
        push(&mut s, t, 0);
        assert_eq!(on_key(&mut s, Key::ENTER), None, "not the toast's key without the focus");
        assert!(on_f6(&mut s));
        assert_eq!(on_key(&mut s, Key::ENTER), Some(Some(CommandId(7))));
        assert!(s.life[0].leaving.is_some());
    }
}
