//! The fields page (K2b.3): the looks of TextField a still frame can show. Focus, selection and composition are painted
//! through TextField::preview, since a page has no pointer or keyboard.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{FieldKind, TextField, TextLabel};

const SIZE: SizeL = SizeL { w: 500, h: 380 };

struct Fields {
    labels: Vec<(TextLabel, (i32, i32, i32, i32))>,
    fields: Vec<(TextField, (i32, i32, i32, i32))>,
    commands: CommandTable,
}

impl Screen for Fields {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (l, (x, y, w, h)) in &mut self.labels {
            l.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        for (f, (x, y, w, h)) in &mut self.fields {
            f.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (l, _) in &self.labels {
            l.paint(cx);
        }
        for (f, _) in &self.fields {
            f.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (f, _) in &mut self.fields {
            f.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: every look of the field.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut p = Fields { labels: Vec::new(), fields: Vec::new(), commands: CommandTable::new() };
    let mut next_id = 0;
    let mut add = |p: &mut Fields, title: Text, col: i32, row: i32, kind: FieldKind, f: &dyn Fn(&mut TextField)| {
        next_id += 1;
        let (x, y) = (16 + 240 * col, 12 + 70 * row);
        p.labels.push((
            TextLabel::new(WidgetId(100 + next_id), title, TypeRole::Caption, FgRole::Secondary),
            (x, y, 224, 16),
        ));
        let mut field = TextField::new(WidgetId(next_id), kind);
        f(&mut field);
        p.fields.push((field, (x, y + 20, 224, 28)));
    };
    let name = s(lang, "Quarterly report", "Rapporto trimestrale");
    let n = name.clone();
    add(&mut p, s(lang, "Rest", "Normale"), 0, 0, FieldKind::Plain, &move |f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview(&n, false, None, None, None);
    });
    add(&mut p, s(lang, "Placeholder", "Segnaposto"), 1, 0, FieldKind::Plain, &|f| {
        f.set_placeholder(s(lang, "Name", "Nome"));
        f.set_label(s(lang, "Name", "Nome"));
    });
    let n = name.clone();
    add(&mut p, s(lang, "Focused, selection", "Con focus, selezione"), 0, 1, FieldKind::Plain, &move |f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview(&n, true, Some(0..9), None, None);
    });
    let n = name.clone();
    add(&mut p, s(lang, "Error", "Errore"), 1, 1, FieldKind::Plain, &move |f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview(
            &n,
            false,
            None,
            None,
            Some(s(lang, "A name can't contain \\ / : * ?", "Un nome non può contenere \\ / : * ?")),
        );
    });
    add(&mut p, s(lang, "Busy", "Occupato"), 0, 2, FieldKind::Plain, &|f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview("C:\\Data", false, None, None, None);
        f.set_busy(true);
    });
    add(&mut p, s(lang, "Disabled", "Disattivato"), 1, 2, FieldKind::Plain, &|f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview("C:\\Data", false, None, None, None);
        f.set_enabled(false);
    });
    add(&mut p, s(lang, "Search with clear", "Cerca con cancella"), 0, 3, FieldKind::Search, &|f| {
        f.set_label(s(lang, "Search", "Cerca"));
        f.preview("invoice", false, None, None, None);
        f.set_clear_button(true);
    });
    add(&mut p, s(lang, "Search, empty", "Cerca, vuoto"), 1, 3, FieldKind::Search, &|f| {
        f.set_label(s(lang, "Search", "Cerca"));
        f.set_placeholder(s(lang, "Search", "Cerca"));
    });
    add(&mut p, s(lang, "Composition", "Composizione"), 0, 4, FieldKind::Plain, &|f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview("ab", true, None, Some(("nihon", 5)), None);
    });
    add(&mut p, s(lang, "Rename", "Rinomina"), 1, 4, FieldKind::Rename { stem_end: 6 }, &|f| {
        f.set_label(s(lang, "Name", "Nome"));
        f.preview("report.docx", true, Some(0..6), None, None);
    });
    Some(Page { name: "fields", size: SIZE, states: 1, screen: Box::new(p) })
}
