//! The glyphs page (K1a.3): every glyph at 16, 24 and 32 logical px.

use super::Page;
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::screen::Screen;
use oled_ui::strings::UiLang;
use oled_ui::Glyph;

const SIZES: [i32; 3] = [16, 24, 32];
const COLUMNS: usize = 13;
const MARGIN: i32 = 12;
const COL_W: i32 = 40;
const BLOCK_GAP: i32 = 12;

/// The row pitch of a block of glyphs of `size`.
fn pitch(size: i32) -> i32 {
    size + 12
}

/// The page's logical size.
fn page_size() -> SizeL {
    let rows = Glyph::ALL.len().div_ceil(COLUMNS) as i32;
    let h: i32 =
        SIZES.iter().map(|s| rows * pitch(*s)).sum::<i32>() + BLOCK_GAP * (SIZES.len() as i32 - 1) + MARGIN * 2;
    SizeL { w: MARGIN * 2 + COLUMNS as i32 * COL_W, h }
}

struct Glyphs {
    commands: CommandTable,
}

impl Screen for Glyphs {
    fn layout(&mut self, _cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let ink = cx.tokens().color.fg_primary;
        let mut y = MARGIN;
        for size in SIZES {
            for (i, g) in Glyph::ALL.iter().enumerate() {
                let (col, row) = ((i % COLUMNS) as i32, (i / COLUMNS) as i32);
                let x = MARGIN + col * COL_W + (COL_W - size) / 2;
                let top = y + row * pitch(size) + (pitch(size) - size) / 2;
                let r = RectP::wh(cx.px(x), cx.px(top), cx.px(size), cx.px(size));
                if cx.visible(r) {
                    cx.glyph(r, *g, ink);
                }
            }
            y += Glyph::ALL.len().div_ceil(COLUMNS) as i32 * pitch(size) + BLOCK_GAP;
        }
    }

    fn event(&mut self, _cx: &mut EventCx, _ev: &Event) -> Reply {
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// Every glyph at three sizes; glyphs carry no text, so the page is the same in every language.
pub fn page(lang: UiLang) -> Option<Page> {
    let _ = lang;
    Some(Page {
        name: "glyphs",
        size: page_size(),
        states: 1,
        screen: Box::new(Glyphs { commands: CommandTable::default() }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    fn render(dpi: u32) -> oled_ui::harness::Image {
        let p = page(UiLang::EN).unwrap();
        Harness::new(p.screen, p.size, dpi, Tokens::oled()).render()
    }

    #[test]
    fn every_glyph_leaves_ink_in_its_cell() {
        for dpi in [96u32, 144] {
            let img = render(dpi);
            let s = oled_ui::geom::Scale { dpi };
            let mut y = MARGIN;
            for size in SIZES {
                for (i, g) in Glyph::ALL.iter().enumerate() {
                    let (col, row) = ((i % COLUMNS) as i32, (i / COLUMNS) as i32);
                    let x = MARGIN + col * COL_W + (COL_W - size) / 2;
                    let top = y + row * pitch(size) + (pitch(size) - size) / 2;
                    let cell = RectP::wh(s.px(x), s.px(top), s.px(size), s.px(size));
                    let c = img.crop(cell);
                    assert!(c.px.iter().any(|p| *p & 0x00FF_FFFF != 0), "{g:?} at {size} px, {dpi} dpi is empty");
                }
                y += Glyph::ALL.len().div_ceil(COLUMNS) as i32 * pitch(size) + BLOCK_GAP;
            }
        }
    }

    #[test]
    fn the_page_is_the_same_in_every_language() {
        let a = Harness::new(page(UiLang::EN).unwrap().screen, page_size(), 96, Tokens::oled()).render();
        let b = Harness::new(page(UiLang::IT).unwrap().screen, page_size(), 96, Tokens::oled()).render();
        assert_eq!(a.px, b.px);
    }
}
