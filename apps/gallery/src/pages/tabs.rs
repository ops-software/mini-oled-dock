//! The tabs page (K3.1): the tab strip at rest (one selected, one long label trimmed), with a busy and an error tab, and
//! overflowing into a scroll with the "+" button pinned at the end.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{TabSpec, TabStrip};

const SIZE: SizeL = SizeL { w: 960, h: 176 };
const STRIP_H: i32 = 40;

struct Strips {
    strips: Vec<TabStrip>,
    commands: CommandTable,
}

impl Screen for Strips {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, t) in self.strips.iter_mut().enumerate() {
            t.layout(cx, RectP::wh(0, cx.px(8 + (STRIP_H + 8) * i as i32), avail.w, cx.px(STRIP_H)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for t in &self.strips {
            t.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for t in &mut self.strips {
            t.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

fn tab(key: u64, label: Text, busy: bool, error: bool) -> TabSpec {
    TabSpec { key, label, tip: None, icon: None, busy, error }
}

/// The page: three strips.
pub fn page(lang: UiLang) -> Option<Page> {
    let rest = vec![
        tab(1, s(lang, "Documents", "Documenti"), false, false),
        tab(2, s(lang, "Pictures", "Immagini"), false, false),
        tab(
            3,
            s(lang, "A folder with a name too long to fit its tab", "Una cartella con un nome troppo lungo"),
            false,
            false,
        ),
        tab(4, s(lang, "Downloads", "Scaricati"), false, false),
    ];
    let states = vec![
        tab(1, s(lang, "Archive", "Archivio"), false, false),
        tab(2, s(lang, "Loading", "In caricamento"), true, false),
        tab(3, s(lang, "Unavailable", "Non disponibile"), false, true),
    ];
    let many: Vec<TabSpec> =
        (1..=14u64).map(|k| tab(k, s(lang, "Project folder", "Cartella progetto"), false, false)).collect();
    let strips = vec![
        TabStrip::new(WidgetId(1)).with_tabs(rest, 1),
        TabStrip::new(WidgetId(2)).with_tabs(states, 2),
        TabStrip::new(WidgetId(3)).with_tabs(many, 14),
    ];
    Some(Page {
        name: "tabs",
        size: SIZE,
        states: 1,
        screen: Box::new(Strips { strips, commands: CommandTable::new() }),
    })
}
