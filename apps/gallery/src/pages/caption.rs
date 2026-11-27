//! The caption page (K3.5): the custom caption's buttons on TITLEBAR_H strips, in the states a still frame can show: at
//! rest, the maximise button hovered, the close button hovered, and a maximised window's Restore. The strips are laid out
//! whatever the effective caption is: the page shows what a window with the custom caption paints. Its top strip is the
//! caption (caption_hit) when S-CAPTION's rig opens the page with the custom caption on.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{PointP, RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{CaptionHit, Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{CaptionButtons, TextLabel};

const SIZE: SizeL = SizeL { w: 480, h: 184 };
/// A strip and the gap under it, logical px.
const ROW: i32 = 48;

/// One strip: what it shows, and its buttons.
struct Strip {
    label: TextLabel,
    buttons: CaptionButtons,
    rect: RectP,
}

/// The strips; the first is the caption.
pub struct CaptionPage {
    strips: Vec<Strip>,
    commands: CommandTable,
}

impl Screen for CaptionPage {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let bar = cx.px(cx.tokens().size.titlebar_h);
        let buttons = 3 * cx.px(cx.tokens().size.caption_btn_w);
        for (i, s) in self.strips.iter_mut().enumerate() {
            s.rect = RectP::wh(0, cx.px(ROW * i as i32), avail.w, bar);
            let text = RectP::wh(cx.px(12), s.rect.t, (avail.w - buttons - cx.px(20)).max(0), bar);
            s.label.layout(cx, text);
            s.buttons.layout(cx, s.rect);
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for s in &self.strips {
            s.label.paint(cx);
            s.buttons.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if matches!(ev, Event::CloseRequested) {
            return Reply::Close;
        }
        for s in &mut self.strips {
            let _ = s.buttons.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }

    /// The top strip: its buttons, else empty caption space; Client below it.
    fn caption_hit(&self, p: PointP) -> CaptionHit {
        match self.strips.first() {
            Some(s) if s.rect.contains(p) => match s.buttons.hit(p) {
                CaptionHit::Client => CaptionHit::Caption,
                b => b,
            },
            _ => CaptionHit::Client,
        }
    }
}

/// The page: four strips (rest, maximise hovered, close hovered, maximised).
pub fn page(lang: UiLang) -> Option<Page> {
    let rows: [(Text, Option<CaptionHit>, bool); 4] = [
        (s(lang, "At rest", "A riposo"), None, false),
        (s(lang, "Maximize, hover", "Ingrandisci, al passaggio"), Some(CaptionHit::MaxButton), false),
        (s(lang, "Close, hover", "Chiudi, al passaggio"), Some(CaptionHit::CloseButton), false),
        (s(lang, "Maximized window", "Finestra ingrandita"), None, true),
    ];
    let strips = rows
        .into_iter()
        .enumerate()
        .map(|(i, (text, hover, maximized))| {
            let id = 10 * i as u32;
            let mut buttons = CaptionButtons::new(WidgetId(id + 2));
            buttons.set_still_hover(hover);
            buttons.set_maximized(maximized);
            let label = TextLabel::new(WidgetId(id + 1), text, TypeRole::Body, FgRole::Secondary);
            Strip { label, buttons, rect: RectP::default() }
        })
        .collect();
    let screen = CaptionPage { strips, commands: CommandTable::new() };
    Some(Page { name: "caption", size: SIZE, states: 4, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_names_its_buttons_in_both_languages() {
        for (lang, l, max, restore) in
            [(UiLang::EN, Lang::En, "Maximize", "Restore"), (UiLang::IT, Lang::It, "Ingrandisci", "Ripristina")]
        {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            let dump = h.a11y_dump();
            assert_eq!(dump.matches(&format!("Button \"{max}\"")).count(), 3, "{dump}");
            assert_eq!(dump.matches(&format!("Button \"{restore}\"")).count(), 1, "{dump}");
        }
    }

    #[test]
    fn only_the_top_strip_is_the_caption() {
        let p = page(UiLang::EN).unwrap();
        let h = Harness::new(p.screen, p.size, 144, Tokens::oled());
        let at = |x: i32, y: i32| h.screen().caption_hit(PointP { x, y });
        let w = 480 * 3 / 2;
        assert_eq!(at(30, 10), CaptionHit::Caption);
        assert_eq!(at(w - 10, 10), CaptionHit::CloseButton);
        assert_eq!(at(w - 69 - 10, 10), CaptionHit::MaxButton);
        assert_eq!(at(w - 10, 72 + 10), CaptionHit::Client, "the sample strips are not the caption");
    }

    #[test]
    fn the_hover_samples_show_their_plates() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        let px = |x: i32, y: i32| img.px[(y as u32 * img.w + x as u32) as usize] & 0x00FF_FFFF;
        assert_eq!(px(480 - 92 + 3, 3), 0, "rest: no plate");
        assert_eq!(px(480 - 92 + 3, 48 + 3), 0x0014_1414, "maximise hovered: FILL_HOVER");
        assert_eq!(px(480 - 46 + 3, 96 + 3), 0x00FF_453A, "close hovered: ACCENT_DANGER");
        assert!(h.is_idle());
    }
}
