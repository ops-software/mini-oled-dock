//! TextLabel (F-K1, P2-kit 2.6): one run of text in a type role and a text colour role.

use super::paint::fg_ink;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::{RectP, SizeP};
use crate::model::{Align, FgRole};
use crate::node::{Node, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::TypeRole;

/// One run of text in a type role and a text colour role.
pub struct TextLabel {
    id: WidgetId,
    text: Text,
    role: TypeRole,
    fg: FgRole,
    trim: Trim,
    rect: RectP,
}

impl TextLabel {
    pub fn new(id: WidgetId, text: Text, role: TypeRole, fg: FgRole) -> TextLabel {
        TextLabel { id, text, role, fg, trim: Trim::End, rect: RectP::EMPTY }
    }

    pub fn set_text(&mut self, t: Text) {
        self.text = t;
    }

    pub fn set_trim(&mut self, t: Trim) {
        self.trim = t;
    }
}

impl Widget for TextLabel {
    type Out = ();

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        cx.push(Node::new(NodeId::new(self.id, 0), Role::Label, rect).label(self.text.clone()));
    }

    fn paint(&self, cx: &mut PaintCx) {
        let ink = fg_ink(self.fg, cx.tokens());
        cx.text_in(&self.text, self.role, self.rect, Align::Start, self.trim, ink);
    }

    fn event(&mut self, _cx: &mut EventCx, _ev: &Event) -> Option<()> {
        None
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let font = cx.font(self.role);
        let w = cx.text().width(&self.text, font);
        SizeP { w, h: cx.px(self.role.spec().line) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::SizeL;
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;

    fn render(fg: FgRole, text: &'static str, w: i32) -> (Vec<u32>, usize) {
        let l = TextLabel::new(WidgetId(1), Text::from(text), TypeRole::Body, fg);
        let mut h = Harness::new(Host::new(l, (4, 4, w, 18)), SizeL { w: 200, h: 28 }, 96, Tokens::oled());
        let img = h.render();
        (img.px, img.text_rects.len())
    }

    #[test]
    fn a_label_draws_its_text_in_its_ink() {
        let (primary, rects) = render(FgRole::Primary, "Hello", 120);
        let (tertiary, _) = render(FgRole::Tertiary, "Hello", 120);
        assert_eq!(rects, 1);
        let peak = |v: &[u32]| v.iter().map(|p| p & 0xFF).max().unwrap();
        assert!(peak(&primary) > peak(&tertiary), "FG_PRIMARY is brighter than FG_TERTIARY");
        assert!(peak(&tertiary) <= 128);
    }

    #[test]
    fn hot_text_stays_visible_under_high_contrast() {
        use crate::theme::{HcColors, SysOverlay, ThemeSpec};
        let hc = HcColors {
            window: (0, 0, 0),
            window_text: (255, 255, 255),
            highlight: (26, 235, 255),
            highlight_text: (0, 0, 0),
            gray_text: (63, 242, 63),
        };
        let sys = SysOverlay { high_contrast: Some(hc), reduced_motion: false, text_scale: 1.0 };
        let tokens = Tokens::resolve(&ThemeSpec::oled(), &sys);
        let l = TextLabel::new(WidgetId(1), Text::from("Hot text"), TypeRole::Body, FgRole::Hot);
        let mut h = Harness::new(Host::new(l, (4, 4, 120, 18)), SizeL { w: 200, h: 28 }, 96, tokens);
        let img = h.render();
        assert!(img.px.iter().any(|p| p & 0x00FF_FFFF != 0), "the label leaves ink on the black window");
    }

    #[test]
    fn a_long_label_is_trimmed_to_its_rect() {
        let (px, _) =
            render(FgRole::Primary, "A very long label that cannot possibly fit in this narrow rectangle", 60);
        let right_of = |x0: usize| (0..28usize).any(|y| (x0..200).any(|x| px[y * 200 + x] & 0xFF != 0));
        assert!(!right_of(66), "nothing is drawn past the rect");
        assert!(right_of(20));
    }

    #[test]
    fn the_label_is_a_node_with_its_text_as_name() {
        let l = TextLabel::new(WidgetId(1), Text::from("Name"), TypeRole::Section, FgRole::Secondary);
        let h = Harness::new(Host::new(l, (4, 4, 80, 18)), SizeL { w: 100, h: 28 }, 96, Tokens::oled());
        assert_eq!(h.a11y_dump(), "Label \"Name\"\n");
    }
}
