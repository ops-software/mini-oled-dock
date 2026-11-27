//! Dialog (F-K2b, P2-kit 2.9): an in-window sheet over SCRIM on the whole client. DIALOG_W 400 (wide 480), R_L 14, a 1 px
//! RIM_SURFACE rim, padding S_6; the title in Title type, the body in Body FG_SECONDARY wrapped, the buttons at the bottom
//! right with the last one primary. It traps focus (Tab cycles inside), Enter takes the primary button, Esc cancels. The
//! screen lays it out and paints it last and routes the events to it first.

use super::button::{Button, ButtonKind, ButtonOut};
use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::{Event, Key};
use crate::geom::{PointP, RectP};
use crate::node::{Node, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::{LayoutOpts, Trim};
use crate::theme::TypeRole;

/// A dialog's content; the last button is the primary one.
pub struct DialogSpec {
    pub title: Text,
    pub body: Text,
    pub buttons: Vec<(Text, CommandId)>,
    pub wide: bool,
}

/// What a dialog tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogOut {
    Chose(CommandId),
    Cancelled,
}

/// The gap between the title and the body, between the body and the buttons, and between two buttons; a button's
/// padding and narrowest width (logical px).
const TITLE_GAP: i32 = 8;
const BUTTON_ROW_GAP: i32 = 20;
const BUTTON_GAP: i32 = 8;
const BUTTON_PAD: i32 = 16;
const BUTTON_MIN_W: i32 = 72;

/// A modal surface inside the window.
pub struct Dialog {
    id: WidgetId,
    spec: DialogSpec,
    /// The buttons, widget ids `id` + 1 and up.
    buttons: Vec<Button>,
    client: RectP,
    panel: RectP,
    title_at: PointP,
    body_rect: RectP,
}

impl Dialog {
    /// A dialog takes the widget ids `id` + 1 .. `id` + buttons for its buttons.
    pub fn new(id: WidgetId, spec: DialogSpec) -> Dialog {
        let n = spec.buttons.len();
        let buttons = spec
            .buttons
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let kind = if i + 1 == n { ButtonKind::Primary } else { ButtonKind::Secondary };
                Button::new(WidgetId(id.0.wrapping_add(1 + i as u32)), label.clone(), kind)
            })
            .collect();
        Dialog {
            id,
            spec,
            buttons,
            client: RectP::EMPTY,
            panel: RectP::EMPTY,
            title_at: PointP { x: 0, y: 0 },
            body_rect: RectP::EMPTY,
        }
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// Puts the keyboard focus on the primary button (the screen calls it when it shows the dialog).
    pub fn focus(&self, cx: &mut EventCx) {
        if let Some(b) = self.buttons.last() {
            cx.set_focus(b.node_id());
        }
    }

    /// The index of the dialog button that holds the focus.
    fn focused_button(&self, cx: &EventCx) -> Option<usize> {
        let f = cx.focused()?;
        self.buttons.iter().position(|b| b.node_id() == f)
    }

    fn cycle(&self, cx: &mut EventCx, back: bool) {
        let n = self.buttons.len();
        if n == 0 {
            return;
        }
        let to = match (self.focused_button(cx), back) {
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
            (None, false) => 0,
            (None, true) => n - 1,
        };
        cx.set_focus(self.buttons[to].node_id());
    }
}

impl Widget for Dialog {
    type Out = DialogOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.client = rect;
        let size = cx.tokens().size.clone();
        let pad = cx.px(cx.tokens().space.s6);
        let want = cx.px(if self.spec.wide { size.dialog_w_wide } else { size.dialog_w });
        let w = want.min((rect.w() - 2 * cx.px(cx.tokens().space.s4)).max(0));
        let inner = (w - 2 * pad).max(1);
        let (title_f, body_f) = (cx.font(TypeRole::Title), cx.font(TypeRole::Body));
        let title = cx.text().layout(
            &self.spec.title,
            title_f,
            &LayoutOpts { max_width: Some(inner), trim: Trim::End, wrap: false },
        );
        let body = cx.text().layout(
            &self.spec.body,
            body_f,
            &LayoutOpts { max_width: Some(inner), trim: Trim::None, wrap: true },
        );
        let bh = cx.px(size.control_h);
        let h = pad + title.height + cx.px(TITLE_GAP) + body.height + cx.px(BUTTON_ROW_GAP) + bh + pad;
        let (l, t) = (rect.l + (rect.w() - w) / 2, rect.t + (rect.h() - h) / 2);
        self.panel = RectP::wh(l, t, w, h);
        self.title_at = PointP { x: l + pad, y: t + pad };
        self.body_rect = RectP::wh(l + pad, t + pad + title.height + cx.px(TITLE_GAP), inner, body.height);
        // The scrim takes every pointer press outside the panel.
        cx.push(Node::new(NodeId::new(self.id, u32::MAX), Role::Group, rect));
        cx.push(
            Node::new(self.node_id(), Role::Dialog, self.panel)
                .label(self.spec.title.clone())
                .description(self.spec.body.clone()),
        );
        // Buttons from the right edge, the last one nearest to it.
        let mut x = self.panel.r - pad;
        let y = self.panel.b - pad - bh;
        let font = cx.font(TypeRole::Body);
        let mut rects = Vec::with_capacity(self.buttons.len());
        for (label, _) in self.spec.buttons.iter().rev() {
            let bw = (cx.text().width(label, font) + 2 * cx.px(BUTTON_PAD)).max(cx.px(BUTTON_MIN_W));
            rects.push(RectP::wh(x - bw, y, bw, bh));
            x -= bw + cx.px(BUTTON_GAP);
        }
        rects.reverse();
        for (b, r) in self.buttons.iter_mut().zip(rects) {
            b.layout(cx, r);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        cx.fill(self.client, 0, c.scrim);
        let radius = cx.px(cx.tokens().radius.l);
        cx.fill(self.panel, radius, c.bg);
        cx.stroke(self.panel, radius, cx.px(1).max(1), c.rim_surface);
        let (scale, ts) = (cx.env.scale, cx.env.tokens.text_scale);
        let title_f = cx.res.text.font(TypeRole::Title, None, scale, ts);
        let inner = self.body_rect.w();
        let title = cx.res.text.layout(
            &self.spec.title,
            title_f,
            &LayoutOpts { max_width: Some(inner), trim: Trim::End, wrap: false },
        );
        cx.text(&title, self.title_at, c.fg_primary);
        let body_f = cx.res.text.font(TypeRole::Body, None, scale, ts);
        let body = cx.res.text.layout(
            &self.spec.body,
            body_f,
            &LayoutOpts { max_width: Some(inner), trim: Trim::None, wrap: true },
        );
        cx.text(&body, PointP { x: self.body_rect.l, y: self.body_rect.t }, c.fg_secondary);
        for b in &self.buttons {
            b.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<DialogOut> {
        for (i, b) in self.buttons.iter_mut().enumerate() {
            if let Some(ButtonOut::Clicked) = b.event(cx, ev) {
                return self.spec.buttons.get(i).map(|(_, id)| DialogOut::Chose(*id));
            }
        }
        let Event::Key { key, down: true, mods, .. } = ev else { return None };
        match *key {
            Key::ESC => {
                cx.consume();
                Some(DialogOut::Cancelled)
            }
            Key::TAB if !mods.ctrl() && !mods.alt() => {
                cx.consume();
                self.cycle(cx, mods.shift());
                None
            }
            // Enter on a button was the button's; anywhere else it takes the primary one.
            Key::ENTER if !cx.consumed() && self.focused_button(cx).is_none() => {
                cx.consume();
                self.spec.buttons.last().map(|(_, id)| DialogOut::Chose(*id))
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
    use crate::screen::Screen;
    use crate::theme::Tokens;

    const KEEP: CommandId = CommandId(11);
    const DROP: CommandId = CommandId(12);

    /// A window with a button of its own and a dialog over it (laid out last, events to it first).
    struct Modal {
        base: Button,
        dialog: Dialog,
        outs: Vec<DialogOut>,
        base_clicks: u32,
        cmds: CommandTable,
    }

    impl Screen for Modal {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.base.layout(cx, RectP::wh(cx.px(10), cx.px(10), cx.px(80), cx.px(28)));
            self.dialog.layout(cx, RectP::wh(0, 0, avail.w, avail.h));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.base.paint(cx);
            self.dialog.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if let Some(o) = self.dialog.event(cx, ev) {
                self.outs.push(o);
            } else if self.base.event(cx, ev).is_some() {
                self.base_clicks += 1;
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn harness(wide: bool) -> Harness<Modal> {
        let spec = DialogSpec {
            title: Text::from("Delete 3 items?"),
            body: Text::from(
                "They will be moved to the Recycle Bin. You can restore them from there at any time you like.",
            ),
            buttons: vec![(Text::from("Cancel"), DROP), (Text::from("Delete"), KEEP)],
            wide,
        };
        let m = Modal {
            base: Button::new(WidgetId(50), Text::from("Behind"), ButtonKind::Secondary),
            dialog: Dialog::new(WidgetId(100), spec),
            outs: Vec::new(),
            base_clicks: 0,
            cmds: CommandTable::new(),
        };
        Harness::new(m, SizeL { w: 600, h: 320 }, 96, Tokens::oled())
    }

    fn node(w: u32) -> NodeId {
        NodeId::new(WidgetId(w), 0)
    }

    #[test]
    fn dialog_traps_focus() {
        let mut h = harness(false);
        h.key("Tab");
        assert_eq!(h.focused(), Some(node(101)));
        h.key("Tab");
        assert_eq!(h.focused(), Some(node(102)));
        h.key("Tab");
        assert_eq!(h.focused(), Some(node(101)), "Tab cycles inside the dialog");
        h.key("Shift+Tab");
        assert_eq!(h.focused(), Some(node(102)));
        for _ in 0..5 {
            h.key("Tab");
            assert_ne!(h.focused(), Some(node(50)), "the button behind is out of reach");
        }
        // A press outside the panel goes to the scrim, not to the button behind it.
        h.click(PointL { x: 20, y: 20 });
        assert_eq!(h.screen().base_clicks, 0);
    }

    #[test]
    fn dialog_enter_esc() {
        let mut h = harness(false);
        h.key("Enter");
        assert_eq!(h.screen().outs, [DialogOut::Chose(KEEP)], "Enter anywhere takes the primary button");
        h.key("Esc");
        assert_eq!(h.screen().outs.last(), Some(&DialogOut::Cancelled));
        h.key("Tab").key("Enter");
        assert_eq!(h.screen().outs.last(), Some(&DialogOut::Chose(DROP)), "Enter on a button is that button's");
        let b = h.dispatcher().frame.get(node(102)).map(|n| n.rect).unwrap();
        h.click(PointL { x: (b.l + b.r) / 2, y: (b.t + b.b) / 2 });
        assert_eq!(h.screen().outs.last(), Some(&DialogOut::Chose(KEEP)));
    }

    #[test]
    fn dialog_geometry_and_names() {
        for (wide, w) in [(false, 400), (true, 480)] {
            let mut h = harness(wide);
            let p = h.dispatcher().frame.iter().find(|n| n.role == Role::Dialog).map(|n| n.rect).unwrap();
            assert_eq!(p.w(), w);
            assert_eq!(p.l, (600 - w) / 2, "centred");
            assert!(((p.t + p.b) / 2 - 160).abs() <= 1, "centred vertically");
            let primary = h.dispatcher().frame.get(node(102)).map(|n| n.rect).unwrap();
            assert_eq!(p.r - primary.r, 20, "the buttons end at the padding S_6");
            assert_eq!(p.b - primary.b, 20);
            h.assert_named();
        }
        let h = harness(false);
        assert!(h.a11y_dump().contains("Dialog \"Delete 3 items?\""), "{}", h.a11y_dump());
    }

    #[test]
    fn scrim_and_rim_are_drawn() {
        let mut h = harness(false);
        let img = h.render();
        // The window colour is black, so the scrim leaves the corner black; the panel's rim is lit.
        assert_eq!(img.px[0] & 0xFFFFFF, 0);
        let p = h.dispatcher().frame.iter().find(|n| n.role == Role::Dialog).map(|n| n.rect).unwrap();
        let rim = img.px[((p.t + p.h() / 2) as u32 * img.w + p.l as u32) as usize] & 0xFF;
        assert!(rim > 0, "RIM_SURFACE on the left edge");
        assert!(!img.text_rects.is_empty());
    }
}
