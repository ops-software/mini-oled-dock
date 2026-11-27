//! The status page (K2b.6): the status line (files' item count in FG_TERTIARY, tasks' items) and the notice with and
//! without an action. Toasts are host surfaces: their pixels are probed in the kit's own tests.

use super::{s, Page};
use oled_ui::command::{CommandId, CommandTable};
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::StatusLine;

const SIZE: SizeL = SizeL { w: 560, h: 160 };

struct Lines {
    lines: Vec<StatusLine>,
    commands: CommandTable,
}

impl Screen for Lines {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, l) in self.lines.iter_mut().enumerate() {
            l.layout(cx, RectP::wh(0, cx.px(12 + 36 * i as i32), avail.w, cx.px(28)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for l in &self.lines {
            l.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for l in &mut self.lines {
            l.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: two status lines and two notices.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut files = StatusLine::new(WidgetId(1));
    files.set_items(vec![
        (s(lang, "1,204 items", "1.204 elementi"), FgRole::Tertiary),
        (s(lang, "3 selected", "3 selezionati"), FgRole::Secondary),
    ]);
    let mut tasks = StatusLine::new(WidgetId(2));
    tasks.set_items(vec![
        (s(lang, "142 processes", "142 processi"), FgRole::Secondary),
        (s(lang, "Paused", "In pausa"), FgRole::Attention),
    ]);
    let notice = |id, with_action: bool| {
        let mut l = StatusLine::new(WidgetId(id));
        l.preview_notice(
            s(lang, "Some processes could not be read.", "Alcuni processi non si sono potuti leggere."),
            with_action.then(|| (s(lang, "Details", "Dettagli"), CommandId(7))),
        );
        l
    };
    let lines = vec![files, tasks, notice(3, true), notice(4, false)];
    Some(Page {
        name: "status",
        size: SIZE,
        states: 1,
        screen: Box::new(Lines { lines, commands: CommandTable::new() }),
    })
}
