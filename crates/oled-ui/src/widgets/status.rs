//! StatusLine and its notice (F-K2b, P2-kit 2.9): STATUS_H 28 with Caption items in their roles separated by " · "; the
//! notice variant is a 28 px band with a 2 px ACCENT_ATTENTION bar at its left, one optional action, hidden after 6 s or
//! on Esc (one one-shot timer).

use super::paint::{fg_ink, lerp_ink};
use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton, TimerId};
use crate::geom::RectP;
use crate::model::{Align, FgRole};
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// The notice line: a 2 px ACCENT_ATTENTION bar, hidden after 6 s or Esc.
pub struct Notice {
    pub text: Text,
    pub action: Option<(Text, CommandId)>,
}

/// What a status line tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusOut {
    Action(CommandId),
}

/// How long a notice stays, ms.
const NOTICE_MS: u32 = 6000;
/// The accent bar's width, the side padding, the gap before the action and the action's padding (logical px).
const BAR_W: i32 = 2;
const PAD: i32 = 12;
const ACTION_GAP: i32 = 12;
const ACTION_PAD: i32 = 8;
const SEPARATOR: &str = " \u{b7} "; // lint: not UI

/// The status line at the bottom of a window.
pub struct StatusLine {
    id: WidgetId,
    items: Vec<(Text, FgRole)>,
    notice: Option<Notice>,
    rect: RectP,
    action_rect: RectP,
}

impl StatusLine {
    pub fn new(id: WidgetId) -> StatusLine {
        StatusLine { id, items: Vec::new(), notice: None, rect: RectP::EMPTY, action_rect: RectP::EMPTY }
    }

    pub fn set_items(&mut self, items: Vec<(Text, FgRole)>) {
        self.items = items;
    }

    /// Shows a notice (it hides after 6 s) or, with None, takes it away.
    pub fn set_notice(&mut self, cx: &mut EventCx, n: Option<Notice>) {
        let timer = TimerId::kit(self.id, 0);
        match &n {
            Some(_) => cx.set_timer(timer, NOTICE_MS),
            None => cx.cancel_timer(timer),
        }
        self.notice = n;
        cx.relayout();
    }

    /// For galleries and goldens: a notice that stays (no timer), without a dispatcher around it.
    pub fn preview_notice(&mut self, text: Text, action: Option<(Text, CommandId)>) {
        self.notice = Some(Notice { text, action });
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn action_node(&self) -> NodeId {
        NodeId::new(self.id, 1)
    }
}

impl Widget for StatusLine {
    type Out = StatusOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let h = cx.px(cx.tokens().size.status_h);
        self.rect = RectP { b: rect.t + h, ..rect };
        self.action_rect = RectP::EMPTY;
        if let Some(n) = &self.notice {
            cx.push(Node::new(self.node_id(), Role::Alert, self.rect).label(n.text.clone()));
            if let Some((label, _)) = &n.action {
                let font = cx.font(TypeRole::Caption);
                let w = cx.text().width(label, font) + 2 * cx.px(ACTION_PAD);
                let bh = h - cx.px(6);
                self.action_rect = RectP::wh(self.rect.r - cx.px(PAD) - w, self.rect.t + (h - bh) / 2, w, bh);
                cx.push(
                    Node::new(self.action_node(), Role::Button, self.action_rect)
                        .parent(self.node_id())
                        .flags(NodeFlags::HOVER_FADE)
                        .focus(Focusable::Tab)
                        .label(label.clone())
                        .actions(Actions::CLICK | Actions::FOCUS),
                );
            }
        } else {
            let all: Vec<&str> = self.items.iter().map(|i| i.0.as_ref()).collect();
            cx.push(Node::new(self.node_id(), Role::Status, self.rect).label(Text::Owned(all.join(SEPARATOR))));
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let r = self.rect;
        if r.is_empty() {
            return;
        }
        let c = cx.tokens().color.clone();
        let pad = cx.px(PAD);
        if let Some(n) = &self.notice {
            let bar = RectP { r: r.l + cx.px(BAR_W), ..r };
            cx.fill(bar, 0, c.accent_attention);
            let right = if self.action_rect.is_empty() { r.r - pad } else { self.action_rect.l - cx.px(ACTION_GAP) };
            cx.text_in(
                &n.text,
                TypeRole::Caption,
                RectP { l: r.l + pad, r: right, ..r },
                Align::Start,
                Trim::End,
                c.fg_primary,
            );
            if let Some((label, _)) = &n.action {
                let id = self.action_node();
                let radius = cx.px(cx.tokens().radius.xs);
                let t = cx.hover_t(id);
                let clear = Ink { a: 0, ..c.fill_hover };
                cx.fill(
                    self.action_rect,
                    radius,
                    if cx.pressed(id) { c.fill_press } else { lerp_ink(clear, c.fill_hover, t) },
                );
                let ink = lerp_ink(c.fg_secondary, c.fg_hot, t);
                cx.text_in(label, TypeRole::Caption, self.action_rect, Align::Start, Trim::End, ink);
                if cx.focused(id) {
                    cx.focus_ring(self.action_rect, radius, false);
                }
            }
            return;
        }
        let mut x = r.l + pad;
        let font_role = TypeRole::Caption;
        for (i, (text, role)) in self.items.iter().enumerate() {
            if i > 0 {
                x += cx.text_in(SEPARATOR, font_role, RectP { l: x, ..r }, Align::Start, Trim::None, c.fg_tertiary);
            }
            let ink = fg_ink(*role, cx.tokens());
            x += cx.text_in(text, font_role, RectP { l: x, r: r.r - pad, ..r }, Align::Start, Trim::End, ink);
            if x >= r.r - pad {
                break;
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<StatusOut> {
        let notice = self.notice.as_ref()?;
        let action = notice.action.as_ref().map(|a| a.1);
        match ev {
            Event::Timer(id) if *id == TimerId::kit(self.id, 0) => {
                self.notice = None;
                cx.relayout();
                None
            }
            // Esc hides the notice wherever the focus is.
            Event::Key { key: Key::ESC, down: true, .. } => {
                cx.cancel_timer(TimerId::kit(self.id, 0));
                self.notice = None;
                cx.consume();
                cx.relayout();
                None
            }
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click)
                if cx.target() == Some(self.action_node()) =>
            {
                action.map(StatusOut::Action)
            }
            Event::Key { key: Key::ENTER, down: true, .. } if cx.target() == Some(self.action_node()) => {
                cx.consume();
                action.map(StatusOut::Action)
            }
            Event::Key { key: Key::SPACE, down, .. } if cx.target() == Some(self.action_node()) => {
                cx.consume();
                if *down {
                    None
                } else {
                    action.map(StatusOut::Action)
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx, PaintCx};
    use crate::event::Reply;
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    type Op = Box<dyn FnOnce(&mut StatusLine, &mut EventCx) + Send>;

    struct Bar {
        w: StatusLine,
        outs: Vec<StatusOut>,
        cmds: CommandTable,
    }

    impl Screen for Bar {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.w.layout(cx, RectP::wh(0, cx.px(12), avail.w, cx.px(28)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.w.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if let Some(o) = self.w.event(cx, ev) {
                self.outs.push(o);
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(op) = msg.downcast::<Op>() {
                (*op)(&mut self.w, cx);
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn harness() -> Harness<Bar> {
        let mut w = StatusLine::new(WidgetId(1));
        w.set_items(vec![(Text::from("1,204 items"), FgRole::Tertiary), (Text::from("3 selected"), FgRole::Secondary)]);
        let bar = Bar { w, outs: Vec::new(), cmds: CommandTable::new() };
        Harness::new(bar, SizeL { w: 360, h: 52 }, 96, Tokens::oled())
    }

    fn op(h: &mut Harness<Bar>, f: impl FnOnce(&mut StatusLine, &mut EventCx) + Send + 'static) {
        let o: Op = Box::new(f);
        h.send(Box::new(o));
    }

    fn notice(action: bool) -> Notice {
        Notice {
            text: Text::from("Some items could not be read."),
            action: action.then(|| (Text::from("Details"), CommandId(5))),
        }
    }

    #[test]
    fn items_are_separated_and_named() {
        let mut h = harness();
        assert_eq!(h.a11y_dump(), "Status \"1,204 items \u{b7} 3 selected\"\n");
        let img = h.render();
        assert!(img.text_rects.len() >= 3, "two items and the separator: {:?}", img.text_rects);
        let node = h.dispatcher().frame.iter().next().map(|n| n.rect).unwrap();
        assert_eq!(node.h(), 28);
        assert!(h.is_idle());
    }

    #[test]
    fn notice_hides_after_6_s() {
        let mut h = harness();
        op(&mut h, |w, cx| w.set_notice(cx, Some(notice(false))));
        assert!(h.a11y_dump().contains("Alert \"Some items could not be read.\""), "{}", h.a11y_dump());
        let img = h.render();
        // The 2 px attention bar at the band's left edge, ACCENT_ATTENTION (not grey).
        let px = img.px[(20 * img.w) as usize];
        assert!(px & 0xFF0000 != px & 0xFF, "a coloured bar: {px:#x}");
        let px1 = img.px[(20 * img.w + 1) as usize];
        assert_eq!(px, px1, "two px wide");
        assert_eq!(img.px[(20 * img.w + 2) as usize] & 0xFFFFFF, 0);
        h.advance(5900);
        assert!(h.a11y_dump().contains("Alert"));
        h.advance(200);
        assert!(h.a11y_dump().contains("Status"), "back to the items after 6 s");
        assert!(h.is_idle());
    }

    #[test]
    fn esc_hides_the_notice_and_the_action_answers() {
        let mut h = harness();
        op(&mut h, |w, cx| w.set_notice(cx, Some(notice(true))));
        let b = h.dispatcher().frame.iter().find(|n| n.role == Role::Button).map(|n| n.rect).unwrap();
        h.click(PointL { x: (b.l + b.r) / 2, y: (b.t + b.b) / 2 });
        assert_eq!(h.screen().outs, [StatusOut::Action(CommandId(5))]);
        h.key("Esc");
        assert!(h.a11y_dump().contains("Status"));
        h.advance(300);
        assert!(h.is_idle(), "the timer went with the notice");
        op(&mut h, |w, cx| w.set_notice(cx, None));
        assert!(h.is_idle());
    }
}
