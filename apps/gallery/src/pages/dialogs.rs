//! The dialogs page (K2b.6): a dialog at both widths, each over its own half of the window (the scrim covers the half).

use super::{s, Page};
use oled_ui::command::{CommandId, CommandTable};
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{Dialog, DialogSpec};

const SIZE: SizeL = SizeL { w: 1000, h: 280 };

struct Dialogs {
    dialogs: Vec<Dialog>,
    commands: CommandTable,
}

impl Screen for Dialogs {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let half = avail.w / 2;
        for (i, d) in self.dialogs.iter_mut().enumerate() {
            d.layout(cx, RectP::wh(half * i as i32, 0, half, avail.h));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for d in &self.dialogs {
            d.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for d in &mut self.dialogs {
            d.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: the 400 px dialog and the 480 px one.
pub fn page(lang: UiLang) -> Option<Page> {
    let narrow = DialogSpec {
        title: s(lang, "Delete 3 items?", "Eliminare 3 elementi?"),
        body: s(
            lang,
            "They will be moved to the Recycle Bin. You can restore them from there.",
            "Verranno spostati nel Cestino. Potrai ripristinarli da li.",
        ),
        buttons: vec![(s(lang, "Cancel", "Annulla"), CommandId(1)), (s(lang, "Delete", "Elimina"), CommandId(2))],
        wide: false,
    };
    let wide = DialogSpec {
        title: s(lang, "End this process?", "Terminare questo processo?"),
        body: s(
            lang,
            "Anything it has not saved is lost. If it has windows open, ask it to close first.",
            "Tutto cio che non ha salvato va perso. Se ha finestre aperte, chiedigli prima di chiudersi.",
        ),
        buttons: vec![
            (s(lang, "Cancel", "Annulla"), CommandId(3)),
            (s(lang, "Close it", "Chiudi"), CommandId(4)),
            (s(lang, "End now", "Termina ora"), CommandId(5)),
        ],
        wide: true,
    };
    let dialogs = vec![Dialog::new(WidgetId(10), narrow), Dialog::new(WidgetId(20), wide)];
    Some(Page {
        name: "dialogs",
        size: SIZE,
        states: 1,
        screen: Box::new(Dialogs { dialogs, commands: CommandTable::new() }),
    })
}
