//! The toolbar page (K2b.5): the tasks-like toolbar (page tabs, buttons, status text, search field) with each tab
//! selected, and the files-like one (buttons and a field with a clear button).

use super::{s, Page};
use oled_ui::command::{CommandId, CommandTable};
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{FieldKind, PageTabs, TextField, Toolbar, ToolbarButton};
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 640, h: 200 };
const TOOLBAR_H: i32 = 44;

struct Toolbars {
    bars: Vec<Toolbar>,
    commands: CommandTable,
}

impl Screen for Toolbars {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, b) in self.bars.iter_mut().enumerate() {
            let y = 8 + i as i32 * (TOOLBAR_H + 8);
            b.layout(cx, RectP::wh(0, cx.px(y), avail.w, cx.px(TOOLBAR_H)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for b in &self.bars {
            b.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for b in &mut self.bars {
            b.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

fn button(cmd: u32, glyph: Glyph, tip: oled_ui::node::Text, toggled: Option<bool>) -> ToolbarButton {
    ToolbarButton { command: CommandId(cmd), glyph, tip, toggled }
}

/// The page: three toolbars, one under each state a still frame can show.
pub fn page(lang: UiLang) -> Option<Page> {
    let tabs = |id| {
        PageTabs::new(
            WidgetId(id),
            vec![
                s(lang, "Processes", "Processi"),
                s(lang, "Performance", "Prestazioni"),
                s(lang, "Services", "Servizi"),
            ],
        )
    };
    let mut bars = Vec::new();
    for (n, second) in [false, true].into_iter().enumerate() {
        let base = 10 * (n as u32 + 1);
        let t = tabs(base + 1).with_selected(usize::from(second));
        let mut field = TextField::new(WidgetId(base + 2), FieldKind::Search);
        field.set_placeholder(s(lang, "Search", "Cerca"));
        field.set_label(s(lang, "Search", "Cerca"));
        field.set_width(200);
        let buttons = vec![
            button(900 + base, Glyph::Search, s(lang, "Refresh", "Aggiorna"), None),
            button(901 + base, Glyph::Pin, s(lang, "Pause", "Pausa"), Some(second)),
        ];
        let mut bar = Toolbar::new(WidgetId(base), Some(t), buttons, Some(field));
        bar.set_status(Some(if second {
            (s(lang, "Paused", "In pausa"), FgRole::Attention)
        } else {
            (s(lang, "Every 2 s", "Ogni 2 s"), FgRole::Secondary)
        }));
        bars.push(bar);
    }
    let mut field = TextField::new(WidgetId(32), FieldKind::Path);
    field.set_label(s(lang, "Path", "Percorso"));
    field.set_clear_button(true);
    field.set_width(320);
    field.preview("C:\\Data\\Reports", false, None, None, None);
    let buttons = vec![
        button(930, Glyph::ChevronLeft, s(lang, "Back", "Indietro"), None),
        button(931, Glyph::More, s(lang, "More", "Altro"), None),
    ];
    bars.push(Toolbar::new(WidgetId(30), None, buttons, Some(field)));
    Some(Page {
        name: "toolbar",
        size: SIZE,
        states: 1,
        screen: Box::new(Toolbars { bars, commands: CommandTable::new() }),
    })
}
