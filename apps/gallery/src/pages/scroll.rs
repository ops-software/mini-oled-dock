//! The scroll page (K1b.3): a list of rows under an overlay ScrollBar. A still frame shows the list at the top with the bar
//! hidden (it shows on the first wheel notch and hides a second after the last); the kit's tests drive the bar through the
//! Harness.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply, WheelDelta};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::Align;
use oled_ui::node::{Node, NodeId, Role, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{ScrollBar, ScrollOut};

const SIZE: SizeL = SizeL { w: 320, h: 240 };
/// The list's room and its rows, logical px.
const VIEW: (i32, i32, i32, i32) = (10, 10, 300, 220);
const ROW_H: i32 = 28;
const ROWS: i32 = 40;

struct ScrollPage {
    bar: ScrollBar,
    offset: i64,
    view: RectP,
    row_h: i32,
    label: String,
    commands: CommandTable,
}

impl Screen for ScrollPage {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let (x, y, w, h) = VIEW;
        self.view = RectP::wh(cx.px(x), cx.px(y), cx.px(w), cx.px(h));
        self.row_h = cx.px(ROW_H);
        cx.push(Node::new(NodeId::new(WidgetId(2), 0), Role::List, self.view));
        self.bar.set(self.view, i64::from(self.row_h) * i64::from(ROWS), self.offset);
        self.bar.layout(cx, self.view);
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let ink = cx.tokens().color.fg_primary;
        let view = self.view;
        let (row_h, offset) = (self.row_h, self.offset);
        let label = &self.label;
        cx.with_clip(view, &mut |cx| {
            for i in 0..ROWS {
                let top = view.t + row_h * i - offset as i32;
                let r = RectP { l: view.l + 12, t: top, r: view.r - 24, b: top + row_h };
                if cx.visible(r) {
                    cx.text_in(&format!("{label} {}", i + 1), TypeRole::Body, r, Align::Start, Trim::End, ink);
                }
            }
        });
        self.bar.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Wheel { delta, .. } = ev {
            let step = f64::from(oled_ui::scroll::SCROLL_STEP) * f64::from(cx.scale().dpi) / 96.0;
            let px = match delta {
                WheelDelta::Lines(n) => -(f64::from(*n) * step),
                WheelDelta::Pixels(p) => -f64::from(*p),
            };
            if let Some(ScrollOut::Scrolled(o)) = self.bar.scroll_by(cx, px.round() as i64) {
                self.offset = o;
                cx.damage(self.view);
            }
            return Reply::None;
        }
        if let Some(ScrollOut::Scrolled(o)) = self.bar.event(cx, ev) {
            self.offset = o;
            cx.damage(self.view);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: forty rows and the bar.
pub fn page(lang: UiLang) -> Option<Page> {
    let screen = ScrollPage {
        bar: ScrollBar::new(WidgetId(1)),
        offset: 0,
        view: RectP::EMPTY,
        row_h: 0,
        label: s(lang, "Row", "Riga").into_owned(),
        commands: CommandTable::new(),
    };
    Some(Page { name: "scroll", size: SIZE, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::geom::PointL;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    fn harness(lang: UiLang, l: Lang) -> Harness<Box<dyn Screen>> {
        let p = page(lang).unwrap();
        Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l)
    }

    #[test]
    fn the_page_lists_rows_and_hides_the_bar_at_rest() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let mut h = harness(lang, l);
            h.assert_named();
            let img = h.render();
            assert!(img.px.iter().any(|p| p & 0xFF != 0), "rows are drawn");
            let bar = img.crop(RectP::wh(300, 10, 8, 220));
            assert!(bar.px.iter().all(|p| p & 0xFF == 0), "no bar before the first notch");
        }
    }

    #[test]
    fn the_wheel_scrolls_the_rows_and_the_bar_hides_again() {
        let mut h = harness(UiLang::EN, Lang::En);
        let before = h.render();
        h.wheel(PointL { x: 100, y: 100 }, -2.0);
        h.hover(PointL { x: 100, y: 100 });
        let during = h.render();
        assert_ne!(before.px, during.px, "the rows moved");
        let bar = during.crop(RectP::wh(300, 10, 8, 220));
        assert!(bar.px.iter().any(|p| p & 0xFF != 0), "the bar shows after the wheel");
        h.advance(1100);
        let after = h.render();
        let bar = after.crop(RectP::wh(300, 10, 8, 220));
        assert!(bar.px.iter().all(|p| p & 0xFF == 0), "and hides a second later");
        assert!(h.is_idle());
    }
}
