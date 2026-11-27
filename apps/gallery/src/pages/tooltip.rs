//! The tooltip page (K1b.4): the chip as a tooltip and a name chip draw it, with and without a chord and trimmed when it is
//! too long, and a row of icon buttons whose tips the real window shows after half a second (the owner's live check).

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{PointP, RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{chip_size, paint_chip, IconButton, TextLabel};
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 480, h: 240 };

/// A chip's text and chord, and where it goes (logical px); the size is measured at layout.
struct Chip {
    text: Text,
    chord: Option<&'static str>,
    at: (i32, i32),
    size: SizeP,
}

struct Tooltips {
    heads: Vec<(TextLabel, (i32, i32, i32, i32))>,
    chips: Vec<Chip>,
    icons: Vec<(IconButton, (i32, i32, i32, i32))>,
    commands: CommandTable,
}

impl Screen for Tooltips {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (l, (x, y, w, h)) in &mut self.heads {
            l.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        for c in &mut self.chips {
            c.size = chip_size(cx, &c.text, c.chord);
        }
        for (i, (x, y, w, h)) in &mut self.icons {
            i.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (l, _) in &self.heads {
            l.paint(cx);
        }
        for c in &self.chips {
            let at = PointP { x: cx.px(c.at.0), y: cx.px(c.at.1) };
            let r = paint_chip(cx, at, &c.text, c.chord);
            debug_assert_eq!((r.w(), r.h()), (c.size.w, c.size.h), "the painted chip is the measured one");
        }
        for (i, _) in &self.icons {
            i.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (i, _) in &mut self.icons {
            i.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The page.
pub fn page(lang: UiLang) -> Option<Page> {
    let head =
        |id, text, y| (TextLabel::new(WidgetId(id), text, TypeRole::Section, FgRole::Secondary), (16, y, 440, 18));
    let chip = |text: Text, chord, y| Chip { text, chord, at: (16, y), size: SizeP::default() };
    let long = s(
        lang,
        "A tooltip is one line: when the text is longer than the chip may grow, the end of it is trimmed with an ellipsis",
        "Un suggerimento è una riga sola: se il testo è più lungo di quanto il chip possa crescere, la fine viene tagliata",
    );
    let icons = [
        (Glyph::ChevronLeft, "Back", "Indietro"),
        (Glyph::Refresh, "Refresh", "Aggiorna"),
        (Glyph::Search, "Search", "Cerca"),
        (Glyph::Pin, "Pin", "Fissa"),
        (Glyph::Gear, "Settings", "Impostazioni"),
    ];
    let screen = Tooltips {
        heads: vec![
            head(1, s(lang, "Chips", "Chip"), 10),
            head(
                2,
                s(
                    lang,
                    "Icon buttons (hold the pointer for half a second)",
                    "Pulsanti icona (fermo per mezzo secondo)",
                ),
                160,
            ),
        ],
        chips: vec![
            chip(s(lang, "Back", "Indietro"), None, 36),
            chip(s(lang, "Back", "Indietro"), Some("Alt+Left"), 68),
            chip(s(lang, "Search", "Cerca"), Some("Ctrl+F"), 100),
            chip(long, None, 132),
        ],
        icons: icons
            .into_iter()
            .enumerate()
            .map(|(n, (g, en, it))| {
                (IconButton::new(WidgetId(10 + n as u32), g, s(lang, en, it)), (16 + 40 * n as i32, 188, 28, 28))
            })
            .collect(),
        commands: CommandTable::new(),
    };
    Some(Page { name: "tooltip", size: SIZE, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_names_its_icon_buttons_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            assert_eq!(h.a11y_dump().lines().filter(|l| l.starts_with("Button")).count(), 5);
        }
    }

    #[test]
    fn the_chips_are_drawn_with_a_rim_and_the_long_one_is_capped() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        // The rim is RIM_CONTROL 34 on black at the chip's left edge, mid height.
        assert!(img.px[(48 * img.w + 16) as usize] & 0xFF >= 20, "the rim is there");
        // The long chip stops at 360 logical px: nothing beyond its right edge at 16 + 360.
        let beyond = img.crop(RectP::wh(380, 132, 100, 24));
        assert!(beyond.px.iter().all(|px| px & 0xFF == 0), "capped at 360 px");
        let inside = img.crop(RectP::wh(300, 132, 76, 24));
        assert!(inside.px.iter().any(|px| px & 0xFF != 0));
    }
}
