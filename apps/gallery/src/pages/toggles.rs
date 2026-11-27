//! The toggles page (K1b.1): the states a still frame can show of Toggle, Checkbox, Slider and Segmented. Hover, press
//! and focus need the pointer and the keyboard: the kit's widget tests drive them through the Harness.

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
use oled_ui::widgets::{Checkbox, Segmented, Slider, TextLabel, Toggle};

const SIZE: SizeL = SizeL { w: 480, h: 300 };

/// A widget and where it goes, logical px.
struct Placed {
    w: Box<dyn Widget<Out = ()>>,
    rect: (i32, i32, i32, i32),
}

/// Adapts a widget with another output to Out = (): the page ignores what the widgets say.
struct Silent<W: Widget>(W);

impl<W: Widget> Widget for Silent<W> {
    type Out = ();

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.0.layout(cx, rect);
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.0.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        self.0.event(cx, ev).map(|_| ())
    }
}

struct Toggles {
    items: Vec<Placed>,
    commands: CommandTable,
}

impl Toggles {
    fn add<W: Widget + 'static>(&mut self, w: W, rect: (i32, i32, i32, i32)) {
        self.items.push(Placed { w: Box::new(Silent(w)), rect });
    }
}

impl Screen for Toggles {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for p in &mut self.items {
            let (x, y, w, h) = p.rect;
            p.w.layout(cx, RectP::wh(cx.px(x), cx.px(y), cx.px(w), cx.px(h)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for p in &self.items {
            p.w.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for p in &mut self.items {
            p.w.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

fn heading(id: u32, text: Text, y: i32) -> (TextLabel, (i32, i32, i32, i32)) {
    (TextLabel::new(WidgetId(id), text, TypeRole::Section, FgRole::Secondary), (16, y, 300, 18))
}

/// The page: switches, boxes, sliders and segmented controls in the states a still frame can show.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut t = Toggles { items: Vec::new(), commands: CommandTable::new() };
    let mut id = 0;
    let mut next = || {
        id += 1;
        WidgetId(id)
    };
    let (h, r) = heading(next().0, s(lang, "Toggles", "Interruttori"), 12);
    t.add(h, r);
    let toggles = [
        (s(lang, "Off", "Spento"), false, true, false, 16),
        (s(lang, "On", "Acceso"), true, true, false, 16 + 232),
        (s(lang, "Off, unavailable", "Spento, non disponibile"), false, false, false, 16),
        (s(lang, "On, unavailable", "Acceso, non disponibile"), true, false, false, 16 + 232),
        (s(lang, "Busy", "Occupato"), true, true, true, 16),
    ];
    for (i, (label, on, enabled, busy, x)) in toggles.into_iter().enumerate() {
        let mut tg = Toggle::new(next(), label, on);
        tg.set_enabled(enabled);
        tg.set_busy(busy);
        t.add(tg, (x, 36 + 28 * (i as i32 / 2), 216, 24));
    }
    let (h, r) = heading(next().0, s(lang, "Check boxes", "Caselle"), 128);
    t.add(h, r);
    let boxes = [
        (s(lang, "Unchecked", "Non selezionata"), false, false, true, 16, 152),
        (s(lang, "Checked", "Selezionata"), true, false, true, 16 + 232, 152),
        (s(lang, "Mixed", "Mista"), false, true, true, 16, 180),
        (s(lang, "Unavailable", "Non disponibile"), true, false, false, 16 + 232, 180),
    ];
    for (label, checked, mixed, enabled, x, y) in boxes {
        let mut c = Checkbox::new(next(), label, checked);
        c.set_mixed(mixed);
        c.set_enabled(enabled);
        t.add(c, (x, y, 216, 24));
    }
    let (h, r) = heading(next().0, s(lang, "Sliders", "Cursori"), 214);
    t.add(h, r);
    for (i, v) in [0, 40, 100].into_iter().enumerate() {
        let mut sl = Slider::new(next(), s(lang, "Level", "Livello"), 0, 100, 1, v);
        sl.set_value_text(Some(Text::from(format!("{v}%"))));
        t.add(sl, (16 + 152 * i as i32, 238, 144, 24));
    }
    let options = vec![s(lang, "Off", "No"), s(lang, "Auto", "Auto"), s(lang, "On", "Sì")];
    t.add(Segmented::new(next(), s(lang, "Mode", "Modo"), options.clone(), Some(1)), (248, 92, 216, 24));
    t.add(Segmented::new(next(), s(lang, "Mode, none", "Modo, nessuno"), options, None), (248, 266, 216, 24));
    Some(Page { name: "toggles", size: SIZE, states: 1, screen: Box::new(t) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_names_its_interactive_nodes_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            let dump = h.a11y_dump();
            assert!(dump.contains("Switch") && dump.contains("CheckBox") && dump.contains("Slider"), "{dump}");
            assert!(dump.contains("RadioGroup") && dump.contains("Radio"), "{dump}");
            assert!(dump.contains("mixed") && dump.contains("busy") && dump.contains("disabled"), "{dump}");
        }
    }

    #[test]
    fn the_page_has_ink_in_every_section() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        for (y0, y1) in [(12, 110), (128, 205), (214, 270)] {
            let band = img.crop(RectP::wh(0, y0, 480, y1 - y0));
            assert!(band.px.iter().any(|px| px & 0xFF != 0), "nothing drawn between {y0} and {y1}");
        }
    }
}
