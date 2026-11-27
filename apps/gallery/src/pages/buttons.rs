//! The buttons page (K1a.6): the states a static page can show of Button, IconButton and TextLabel. Hover, press and focus
//! need the pointer and the keyboard: the kit's widget tests drive them through the Harness and sample their pixels.

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
use oled_ui::widgets::{Button, ButtonKind, IconButton, TextLabel};
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 480, h: 360 };

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

struct Buttons {
    items: Vec<Placed>,
    commands: CommandTable,
}

impl Buttons {
    fn add<W: Widget + 'static>(&mut self, w: W, rect: (i32, i32, i32, i32)) {
        self.items.push(Placed { w: Box::new(Silent(w)), rect });
    }
}

impl Screen for Buttons {
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

/// The page: every state of the three widgets that a still frame can show.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut b = Buttons { items: Vec::new(), commands: CommandTable::new() };
    let mut id = 0;
    let mut next = || {
        id += 1;
        WidgetId(id)
    };
    for (kind, title, y) in [
        (ButtonKind::Secondary, s(lang, "Secondary", "Secondario"), 12),
        (ButtonKind::Primary, s(lang, "Primary", "Primario"), 84),
    ] {
        let (h, r) = heading(next().0, title, y);
        b.add(h, r);
        let states = [
            (s(lang, "Rest", "Normale"), true, false),
            (s(lang, "Disabled", "Disattivato"), false, false),
            (s(lang, "Busy", "Occupato"), true, true),
        ];
        for (i, (label, enabled, busy)) in states.into_iter().enumerate() {
            let mut btn = Button::new(next(), label, kind);
            btn.set_enabled(enabled);
            btn.set_busy(busy);
            b.add(btn, (16 + 112 * i as i32, y + 24, 104, 28));
        }
    }
    let (h, r) = heading(next().0, s(lang, "Icon buttons", "Pulsanti icona"), 156);
    b.add(h, r);
    let icons = [
        (Glyph::ChevronLeft, None, true, "Back", "Indietro"),
        (Glyph::Pin, Some(true), true, "Pinned", "Fissato"),
        (Glyph::Pin, Some(false), true, "Pin", "Fissa"),
        (Glyph::Search, None, true, "Search", "Cerca"),
        (Glyph::More, None, true, "More", "Altro"),
        (Glyph::Gear, None, false, "Settings", "Impostazioni"),
    ];
    for (i, (glyph, toggled, enabled, en, it)) in icons.into_iter().enumerate() {
        let mut ib = IconButton::new(next(), glyph, s(lang, en, it));
        ib.set_toggled(toggled);
        ib.set_enabled(enabled);
        b.add(ib, (16 + 40 * i as i32, 180, 28, 28));
    }
    let (h, r) = heading(next().0, s(lang, "Text", "Testo"), 228);
    b.add(h, r);
    let roles = [
        (FgRole::Primary, "Primary text", "Testo primario"),
        (FgRole::Hot, "Hot text", "Testo evidenziato"),
        (FgRole::Secondary, "Secondary text", "Testo secondario"),
        (FgRole::Tertiary, "Tertiary text", "Testo terziario"),
        (FgRole::Disabled, "Disabled text", "Testo disattivato"),
        (FgRole::Attention, "Attention", "Attenzione"),
        (FgRole::Danger, "Danger", "Pericolo"),
    ];
    for (i, (role, en, it)) in roles.into_iter().enumerate() {
        let (col, row) = (i as i32 / 4, i as i32 % 4);
        b.add(TextLabel::new(next(), s(lang, en, it), TypeRole::Body, role), (16 + 232 * col, 252 + 22 * row, 216, 18));
    }
    Some(Page { name: "buttons", size: SIZE, states: 1, screen: Box::new(b) })
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
            assert!(
                dump.contains(if lang.italian {
                    "\"Occupato\" [focusable, busy]"
                } else {
                    "\"Busy\" [focusable, busy]"
                }),
                "{dump}"
            );
            assert!(dump.contains("[focusable, disabled]") || dump.contains("[disabled]"), "{dump}");
        }
    }

    #[test]
    fn the_page_has_ink_in_every_section() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        for (y0, y1) in [(12, 60), (84, 132), (156, 210), (228, 340)] {
            let band = img.crop(RectP::wh(0, y0, 480, y1 - y0));
            assert!(band.px.iter().any(|px| px & 0xFF != 0), "nothing drawn between {y0} and {y1}");
        }
    }
}
