//! Progress (F-K3, P2-kit 2.10; K3.2): a PROGRESS_H line, SEPARATOR as the track and FG_PRIMARY as the fill. A value
//! animates linearly over DUR_PROGRESS; None (indeterminate) while running is a quarter-width segment sweeping over
//! SWEEP_PERIOD, and only then does the widget keep the frame clock. After 10 s of indeterminate running
//! `long_running` is true: the caller shows its "Working..." text (k::KIT_WORKING).

use crate::cx::{AnimId, Ease, EventCx, LayoutCx, PaintCx};
use crate::event::{Event, TimerId};
use crate::geom::RectP;
use crate::node::{Node, NodeId, NodeValue, Role, WidgetId};
use crate::screen::Widget;

/// How long an indeterminate run lasts before it is a long one (ms).
const LONG_AFTER_MS: u32 = 10_000;
/// The animation channels of the node and the timer slots.
const CH_VALUE: u8 = 0;
const CH_SWEEP: u8 = 1;
const SLOT_RENEW: u8 = 0;
const SLOT_LONG: u8 = 1;
/// A sweep is one long linear tween of this many periods, renewed by a timer when it ends.
const SWEEP_PERIODS: u32 = 1500;

/// A progress line: a value, or indeterminate while running.
pub struct Progress {
    id: WidgetId,
    value: Option<f32>,
    running: bool,
    sweeping: bool,
    long: bool,
    /// The line (physical px), from the last layout.
    line: RectP,
}

impl Progress {
    pub fn new(id: WidgetId) -> Progress {
        Progress { id, value: None, running: false, sweeping: false, long: false, line: RectP::EMPTY }
    }

    /// The line showing `v` (0.0 to 1.0) from the start (a screen builds its widgets before it has an EventCx).
    pub fn with_value(mut self, v: Option<f32>, running: bool) -> Progress {
        self.value = v.map(|x| x.clamp(0.0, 1.0));
        self.running = running;
        self
    }

    /// v None = indeterminate (it sweeps only while running).
    pub fn set(&mut self, cx: &mut EventCx, v: Option<f32>, running: bool) {
        let v = v.map(|x| x.clamp(0.0, 1.0));
        if let Some(x) = v {
            let dur = cx.tokens().motion.dur_progress;
            cx.animate(AnimId { node: self.node_id(), channel: CH_VALUE }, x, dur, Ease::Linear);
        }
        self.value = v;
        self.running = running;
        self.sync_sweep(cx);
        cx.damage(self.line);
    }

    /// True once an indeterminate run has lasted 10 s: the caller shows its "Working..." text.
    pub fn long_running(&self) -> bool {
        self.long
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn indeterminate_run(&self) -> bool {
        self.value.is_none() && self.running
    }

    /// The sweep and the 10 s timer run only while indeterminate and running.
    fn sync_sweep(&mut self, cx: &mut EventCx) {
        let want = self.indeterminate_run();
        let anim = AnimId { node: self.node_id(), channel: CH_SWEEP };
        let (renew, long) = (TimerId::kit(self.id, SLOT_RENEW), TimerId::kit(self.id, SLOT_LONG));
        if want && !self.sweeping {
            self.sweeping = true;
            if !cx.tokens().motion.reduced {
                let period = cx.tokens().motion.sweep_period.max(1);
                cx.animate(anim, 0.0, 0, Ease::Linear);
                cx.animate(anim, SWEEP_PERIODS as f32, SWEEP_PERIODS * period, Ease::Linear);
                cx.set_timer(renew, SWEEP_PERIODS * period);
            }
            cx.set_timer(long, LONG_AFTER_MS);
        } else if !want && self.sweeping {
            self.sweeping = false;
            self.long = false;
            cx.animate(anim, 0.0, 0, Ease::Linear);
            cx.cancel_timer(renew);
            cx.cancel_timer(long);
        }
    }
}

impl Widget for Progress {
    type Out = ();

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.line = RectP { b: rect.t + cx.px(cx.tokens().size.progress_h), ..rect };
        let mut node = Node::new(self.node_id(), Role::ProgressIndicator, self.line);
        if let Some(v) = self.value {
            node = node.value(NodeValue::Percent(v * 100.0));
        }
        cx.push(node);
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        let r = self.line;
        let radius = (r.h() / 2).max(0);
        cx.fill(r, radius, c.separator);
        match self.value {
            Some(v) => {
                let shown = cx.anim(AnimId { node: self.node_id(), channel: CH_VALUE }).unwrap_or(v).clamp(0.0, 1.0);
                let w = (r.w() as f32 * shown).round() as i32;
                if w > 0 {
                    cx.fill(RectP { r: r.l + w, ..r }, radius, c.fg_primary);
                }
            }
            None if self.running => {
                if cx.tokens().motion.reduced {
                    cx.fill(r, radius, c.fg_tertiary);
                    return;
                }
                // A still frame (no tween ever ran) shows the segment part-way.
                let v = cx.anim(AnimId { node: self.node_id(), channel: CH_SWEEP }).unwrap_or(0.4);
                let phase = v - v.floor();
                let seg = r.w() / 4;
                let at = r.l - seg + (phase * (r.w() + seg) as f32).round() as i32;
                let (l, rr) = (at.max(r.l), (at + seg).min(r.r));
                if rr > l {
                    cx.fill(RectP { l, r: rr, ..r }, radius, c.fg_primary);
                }
            }
            None => {}
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        match ev {
            Event::Timer(t) if *t == TimerId::kit(self.id, SLOT_RENEW) => {
                if self.sweeping {
                    self.sweeping = false;
                    let long = self.long;
                    self.sync_sweep(cx);
                    self.long = long;
                }
                None
            }
            Event::Timer(t) if *t == TimerId::kit(self.id, SLOT_LONG) => {
                if self.indeterminate_run() && !self.long {
                    self.long = true;
                    return Some(());
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Reply;
    use crate::geom::{SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    /// A screen with one progress line, driven through Screen::data like an app would.
    struct Page {
        p: Progress,
        cmds: crate::command::CommandTable,
        outs: usize,
    }

    impl Screen for Page {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.p.layout(cx, RectP::wh(0, cx.px(4), cx.px(200), cx.px(2)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.p.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if self.p.event(cx, ev).is_some() {
                self.outs += 1;
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(m) = msg.downcast::<(Option<f32>, bool)>() {
                self.p.set(cx, m.0, m.1);
            }
            Reply::None
        }

        fn commands(&self) -> &crate::command::CommandTable {
            &self.cmds
        }
    }

    fn harness() -> Harness<Page> {
        let page = Page { p: Progress::new(WidgetId(1)), cmds: crate::command::CommandTable::new(), outs: 0 };
        Harness::new(page, SizeL { w: 200, h: 10 }, 96, Tokens::oled())
    }

    /// The columns of the line (row 4) lit above the track.
    fn fill(h: &mut Harness<Page>) -> usize {
        let img = h.render();
        (0..200u32).filter(|x| img.px[(4 * img.w + x) as usize] & 0xFF > 100).count()
    }

    fn set(h: &mut Harness<Page>, v: Option<f32>, running: bool) {
        h.send(Box::new((v, running)));
    }

    #[test]
    fn progress_animates_and_stops() {
        let mut h = harness();
        set(&mut h, Some(0.0), true);
        h.advance(300);
        assert!(h.is_idle());
        set(&mut h, Some(1.0), true);
        assert!(!h.is_idle(), "the value tween keeps the clock");
        h.advance(125);
        let half = fill(&mut h);
        assert!((90..=110).contains(&half), "half way over half of DUR_PROGRESS: {half}");
        h.advance(200);
        assert!(h.is_idle(), "idle after done");
        assert_eq!(fill(&mut h), 200);
        let img = h.render();
        assert_eq!(img.px[(3 * img.w + 10) as usize] & 0xFF, 0, "the line is 2 px: nothing above");
        assert_eq!(img.px[(6 * img.w + 10) as usize] & 0xFF, 0, "and nothing below");
        assert!(h.a11y_dump().contains("ProgressIndicator"), "{}", h.a11y_dump());
    }

    #[test]
    fn track_is_separator() {
        let mut h = harness();
        set(&mut h, Some(0.25), false);
        h.advance(300);
        let img = h.render();
        assert_eq!(img.px[(4 * img.w + 150) as usize] & 0xFF, 30, "SEPARATOR track");
        let v = img.px[(4 * img.w + 20) as usize] & 0xFF;
        assert!((225..=240).contains(&v), "FG_PRIMARY fill: {v}");
    }

    #[test]
    fn indeterminate_only_while_running() {
        let mut h = harness();
        set(&mut h, None, false);
        assert!(h.is_idle(), "indeterminate but not running: no clock");
        assert_eq!(fill(&mut h), 0);
        set(&mut h, None, true);
        assert!(!h.is_idle());
        h.advance(500);
        let seg = fill(&mut h);
        assert!(seg > 0 && seg <= 50, "a quarter of the line: {seg}");
        h.advance(100);
        assert!(!h.is_idle(), "it keeps sweeping");
        set(&mut h, None, false);
        assert!(h.is_idle(), "stopped: no clock, no timer");
        assert_eq!(fill(&mut h), 0);
    }

    #[test]
    fn long_running_after_ten_seconds() {
        let mut h = harness();
        set(&mut h, None, true);
        h.advance(9_000);
        assert!(!h.screen().p.long_running());
        h.advance(1_100);
        assert!(h.screen().p.long_running(), "10 s of indeterminate running");
        assert_eq!(h.screen().outs, 1, "the screen is told once");
        set(&mut h, Some(0.5), true);
        assert!(!h.screen().p.long_running(), "a value ends it");
        set(&mut h, None, true);
        h.advance(1_000);
        assert!(!h.screen().p.long_running(), "a new run starts counting again");
    }
}
