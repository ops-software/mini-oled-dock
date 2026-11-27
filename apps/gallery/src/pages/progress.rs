//! The progress page (K3.2): a value at 0, 40 and 100 %, an indeterminate line while running (a still frame shows the
//! segment part-way) and one that is not running.

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
use oled_ui::widgets::Progress;

const SIZE: SizeL = SizeL { w: 480, h: 208 };
const ROW_H: i32 = 32;
const LABEL_W: i32 = 200;

struct Lines {
    rows: Vec<(Progress, Text)>,
    commands: CommandTable,
}

impl Screen for Lines {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, (p, _)) in self.rows.iter_mut().enumerate() {
            let y = cx.px(12 + ROW_H * i as i32 + 15);
            p.layout(cx, RectP::wh(cx.px(16 + LABEL_W), y, avail.w - cx.px(32 + LABEL_W), cx.px(2)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        for (i, (p, label)) in self.rows.iter().enumerate() {
            let r = RectP::wh(cx.px(16), cx.px(12 + ROW_H * i as i32), cx.px(LABEL_W - 8), cx.px(ROW_H));
            cx.text_in(label, TypeRole::Body, r, Align::Start, Trim::End, c.fg_secondary);
            p.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (p, _) in &mut self.rows {
            p.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: six lines.
pub fn page(lang: UiLang) -> Option<Page> {
    let row = |id, v: Option<f32>, running, en: &'static str, it: &'static str| {
        (Progress::new(WidgetId(id)).with_value(v, running), s(lang, en, it))
    };
    let rows = vec![
        row(1, Some(0.0), true, "Empty", "Vuoto"),
        row(2, Some(0.4), true, "40 %", "40 %"),
        row(3, Some(1.0), false, "Done", "Completato"),
        row(4, None, true, "Indeterminate, running", "Indeterminato, in corso"),
        row(5, None, false, "Indeterminate, stopped", "Indeterminato, fermo"),
    ];
    Some(Page {
        name: "progress",
        size: SIZE,
        states: 1,
        screen: Box::new(Lines { rows, commands: CommandTable::new() }),
    })
}
