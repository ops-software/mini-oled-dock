//! The properties page (K4.2): a PropertySheet built from descriptors written here (the sheet takes nothing else): a
//! toggle with help, a choice, a slider with a unit, a select, a text field, a dock-only control shown disabled, and a
//! row that appears only while the toggle is on. The descriptors' texts are plain functions of the language, so the page
//! keeps one table and picks a column with a const parameter.

use super::Page;
use oled_config::desc::{Cond, Control, Descriptor, PageId, Range, Scope, SecId, TextFn, Unit, ValueType};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::property_sheet::PropertySheet;

const SIZE: SizeL = SizeL { w: 520, h: 470 };

/// (English, Italian): a row's label or help, an option's label, a section's title.
const TEXTS: [(&str, &str); 19] = [
    ("Show names", "Mostra i nomi"),
    ("Draws the name under each icon when there is room for it.", "Scrive il nome sotto ogni icona quando c'è spazio."),
    ("Size", "Dimensione"),
    ("Small", "Piccola"),
    ("Medium", "Media"),
    ("Large", "Grande"),
    ("Opacity", "Opacità"),
    ("How see-through the bar is, from clear to solid.", "Quanto è trasparente la barra, da chiara a piena."),
    ("Order", "Ordine"),
    ("Newest first", "Prima i più recenti"),
    ("Oldest first", "Prima i più vecchi"),
    ("By name", "Per nome"),
    ("Profile name", "Nome del profilo"),
    ("Accent colours", "Colori di risalto"),
    ("Picked in the dock itself, not here.", "Si scelgono nel dock, non qui."),
    ("Show badges", "Mostra i badge"),
    ("Appearance", "Aspetto"),
    ("Behaviour", "Comportamento"),
    ("", ""),
];

/// Text N of the table in the language IT (a distinct function per pair of parameters, so it is a TextFn).
fn t<const IT: bool, const N: usize>() -> &'static str {
    if IT {
        TEXTS[N].1
    } else {
        TEXTS[N].0
    }
}

fn desc(
    json: &'static str,
    order: u16,
    section: &'static str,
    control: Control,
    label: TextFn,
    help: TextFn,
) -> Descriptor {
    Descriptor {
        path: json,
        json,
        kind: ValueType::Bool,
        default: || serde_json::Value::Null,
        range: None,
        label,
        help,
        glyph: "",
        page: PageId("gallery"),
        section: SecId(section),
        order,
        keywords: &[],
        scope: Scope::Global,
        profileable: false,
        visible_when: None,
        cost: None,
        control,
        since: 0,
        takeover: None,
        reason: "A sample row of the gallery.",
    }
}

/// The page's descriptors in one language.
fn descriptors<const IT: bool>() -> Vec<Descriptor> {
    let sizes: &'static [(TextFn, &'static str)] = &[(t::<IT, 3>, "s"), (t::<IT, 4>, "m"), (t::<IT, 5>, "l")];
    let orders: &'static [(TextFn, &'static str)] = &[(t::<IT, 9>, "new"), (t::<IT, 10>, "old"), (t::<IT, 11>, "name")];
    let mut names = desc("gallery.names", 1, "look", Control::Toggle, t::<IT, 0>, t::<IT, 1>);
    names.glyph = "eye";
    names.default = || serde_json::Value::Bool(true);
    let mut size = desc("gallery.size", 2, "look", Control::Segmented(sizes), t::<IT, 2>, t::<IT, 18>);
    size.default = || serde_json::Value::String("m".to_string());
    let mut opacity = desc("gallery.opacity", 3, "look", Control::Slider, t::<IT, 6>, t::<IT, 7>);
    opacity.range = Some(Range { min: 20, max: 100, step: 5, unit: Unit::Percent });
    opacity.default = || serde_json::Value::from(65);
    let mut badges = desc("gallery.badges", 4, "look", Control::Toggle, t::<IT, 15>, t::<IT, 18>);
    badges.visible_when = Some(Cond { json: "gallery.names", equals: "true" });
    let mut order = desc("gallery.order", 5, "behaviour", Control::Select(orders), t::<IT, 8>, t::<IT, 18>);
    order.default = || serde_json::Value::String("old".to_string());
    let name = desc("gallery.profile", 6, "behaviour", Control::Text { max: 24 }, t::<IT, 12>, t::<IT, 18>);
    let accents = desc("gallery.accents", 7, "behaviour", Control::Swatches, t::<IT, 13>, t::<IT, 14>);
    vec![accents, name, order, badges, opacity, size, names]
}

struct Properties {
    sheet: PropertySheet,
    commands: CommandTable,
}

impl Screen for Properties {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.sheet.layout(cx, RectP::wh(0, cx.px(8), avail.w, avail.h));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.sheet.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.sheet.event(cx, ev);
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page: a sheet of two sections.
pub fn page(lang: UiLang) -> Option<Page> {
    let descs = if lang.italian { descriptors::<true>() } else { descriptors::<false>() };
    let mut sheet = PropertySheet::new(WidgetId(1), &descs);
    let title = |n: usize| Text::Borrowed(if lang.italian { TEXTS[n].1 } else { TEXTS[n].0 });
    sheet.set_section_titles(vec![(SecId("look"), title(16)), (SecId("behaviour"), title(17))]);
    Some(Page {
        name: "properties",
        size: SIZE,
        states: 1,
        screen: Box::new(Properties { sheet, commands: CommandTable::new() }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_is_its_descriptors_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            let dump = h.a11y_dump();
            let want = if lang.italian {
                ["Mostra i nomi", "Dimensione", "Opacità", "Ordine"]
            } else {
                ["Show names", "Size", "Opacity", "Order"]
            };
            for w in want {
                assert!(dump.contains(w), "{w}: {dump}");
            }
        }
    }

    #[test]
    fn the_sample_values_are_the_descriptors_defaults() {
        let p = page(UiLang::EN).unwrap();
        let h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let dump = h.a11y_dump();
        assert!(dump.contains("Slider \"Opacity\" [focusable] 65"), "{dump}");
        assert!(dump.contains("Radio \"Medium\" [focusable, selected]"), "{dump}");
        assert!(dump.contains("Show badges"), "the badges row shows while names are on: {dump}");
    }
}
