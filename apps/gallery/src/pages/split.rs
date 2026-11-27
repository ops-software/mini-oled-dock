//! The split page (K3.2): the split view at its default width and dragged wider, with a pane title in each pane.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::Align;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::SplitView;

const SIZE: SizeL = SizeL { w: 640, h: 200 };
const ROW_H: i32 = 84;

struct Splits {
    rows: Vec<(SplitView, Text, Text)>,
    rects: Vec<RectP>,
    commands: CommandTable,
}

impl Screen for Splits {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.rects.clear();
        for (i, (split, _, _)) in self.rows.iter_mut().enumerate() {
            let r = RectP::wh(cx.px(8), cx.px(8 + (ROW_H + 8) * i as i32), avail.w - 2 * cx.px(8), cx.px(ROW_H));
            split.layout(cx, r);
            self.rects.push(r);
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        for ((split, left, right), r) in self.rows.iter().zip(&self.rects) {
            let (a, b) = split.panes(*r);
            cx.text_in(left, TypeRole::Body, a.inset(cx.px(12), 0), Align::Start, Trim::End, c.fg_secondary);
            cx.text_in(right, TypeRole::Body, b.inset(cx.px(12), 0), Align::Start, Trim::End, c.fg_secondary);
            split.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (split, _, _) in &mut self.rows {
            split.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: a split at the files' default (NAV_W 200) and one set wider.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut wide = SplitView::new(WidgetId(2), 160, 400, 200);
    wide.set_first(320);
    let rows = vec![
        (
            SplitView::new(WidgetId(1), 160, 400, 200),
            s(lang, "Sidebar", "Barra laterale"),
            s(lang, "Content", "Contenuto"),
        ),
        (wide, s(lang, "Sidebar, wider", "Barra laterale, più larga"), s(lang, "Content", "Contenuto")),
    ];
    Some(Page {
        name: "split",
        size: SIZE,
        states: 1,
        screen: Box::new(Splits { rows, rects: Vec::new(), commands: CommandTable::new() }),
    })
}
