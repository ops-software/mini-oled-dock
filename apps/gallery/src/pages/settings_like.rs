//! The settings-like page (K1a.6): the S-RASTER workload, fixed here and not changed later: a nav column of 10 entries and
//! 30 rows of a TextLabel plus a control. The control is a Button, or, for the toggle and slider rows, plain fills with the
//! dock's toggle and slider geometry drawn through PaintCx::fill, so the pixel work equals a Settings page without waiting
//! for the kit's own toggle and slider.

use std::borrow::Cow;

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::{pseudo, UiLang};
use oled_ui::theme::{Ink, TypeRole};
use oled_ui::widgets::{Button, ButtonKind, TextLabel};

const NAV: usize = 10;
const ROWS: usize = 30;
/// Logical px.
const PITCH: i32 = 36;
const NAV_W: i32 = 200;
const MARGIN: i32 = 16;
const SIZE: SizeL = SizeL { w: 960, h: 2 * MARGIN + ROWS as i32 * PITCH };

const NAV_NAMES: [(&str, &str); NAV] = [
    ("General", "Generale"),
    ("Appearance", "Aspetto"),
    ("Display", "Schermo"),
    ("Sound", "Audio"),
    ("Network", "Rete"),
    ("Power", "Alimentazione"),
    ("Storage", "Archiviazione"),
    ("Privacy", "Privacy"),
    ("Updates", "Aggiornamenti"),
    ("About", "Informazioni"),
];

const ROW_NAMES: [(&str, &str); 10] = [
    ("Show the clock", "Mostra l'orologio"),
    ("Brightness", "Luminosita"),
    ("Open the folder", "Apri la cartella"),
    ("Start with Windows", "Avvia con Windows"),
    ("Volume", "Volume"),
    ("Reset the layout", "Reimposta il layout"),
    ("Notifications", "Notifiche"),
    ("Opacity", "Opacita"),
    ("Export the settings", "Esporta le impostazioni"),
    ("Keep on top", "Tieni in primo piano"),
];

/// A row's text, numbered so that no two rows read the same.
fn row_text(lang: UiLang, i: usize) -> Text {
    let (en, it) = ROW_NAMES[i % ROW_NAMES.len()];
    let t = format!("{} {}", if lang.italian { it } else { en }, i / ROW_NAMES.len() + 1);
    if lang.pseudo {
        Cow::Owned(pseudo(&t))
    } else {
        Cow::Owned(t)
    }
}

/// What a row's control is.
#[derive(Clone, Copy)]
enum Control {
    Button,
    Toggle(bool),
    Slider(i32),
}

fn control(i: usize) -> Control {
    match i % 3 {
        0 => Control::Button,
        1 => Control::Toggle(i.is_multiple_of(2)),
        _ => Control::Slider(10 + (i as i32 * 13) % 80),
    }
}

struct SettingsLike {
    nav: Vec<TextLabel>,
    labels: Vec<TextLabel>,
    buttons: Vec<Button>,
    /// Where each row's control sits, physical px, set at layout.
    slots: Vec<RectP>,
    commands: CommandTable,
}

fn row_y(i: usize) -> i32 {
    MARGIN + i as i32 * PITCH
}

impl Screen for SettingsLike {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, l) in self.nav.iter_mut().enumerate() {
            l.layout(cx, RectP::wh(cx.px(MARGIN + 12), cx.px(row_y(i) + 8), cx.px(NAV_W - 2 * MARGIN), cx.px(20)));
        }
        self.slots.clear();
        let right = SIZE.w - MARGIN;
        let mut b = 0;
        for (i, l) in self.labels.iter_mut().enumerate() {
            let y = row_y(i);
            l.layout(cx, RectP::wh(cx.px(NAV_W + MARGIN + 12), cx.px(y + 8), cx.px(380), cx.px(20)));
            let slot = match control(i) {
                Control::Button => RectP::wh(cx.px(right - 96), cx.px(y + 4), cx.px(96), cx.px(28)),
                Control::Toggle(_) => RectP::wh(cx.px(right - 32), cx.px(y + 9), cx.px(32), cx.px(18)),
                Control::Slider(_) => RectP::wh(cx.px(right - 200), cx.px(y + 4), cx.px(200), cx.px(28)),
            };
            if matches!(control(i), Control::Button) {
                self.buttons[b].layout(cx, slot);
                b += 1;
            }
            self.slots.push(slot);
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        // The selected nav entry's plate, then the nav labels.
        let sel = RectP::wh(cx.px(MARGIN), cx.px(row_y(0)), cx.px(NAV_W - 2 * MARGIN), cx.px(32));
        let (hover, rim, sep) =
            (cx.tokens().color.fill_hover, cx.tokens().color.rim_hover, cx.tokens().color.separator);
        cx.fill(sel, cx.px(7), hover);
        cx.stroke(sel, cx.px(7), 1, rim);
        for l in &self.nav {
            l.paint(cx);
        }
        for (i, l) in self.labels.iter().enumerate() {
            l.paint(cx);
            let (x0, x1, y) = (cx.px(NAV_W + MARGIN), cx.px(SIZE.w - MARGIN), cx.px(row_y(i) + PITCH - 1));
            cx.hline(x0, x1, y, sep);
        }
        let mut b = 0;
        for (i, slot) in self.slots.iter().enumerate() {
            match control(i) {
                Control::Button => {
                    self.buttons[b].paint(cx);
                    b += 1;
                }
                Control::Toggle(on) => paint_toggle(cx, *slot, on),
                Control::Slider(v) => paint_slider(cx, *slot, v),
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for b in &mut self.buttons {
            b.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The dock's toggle: a 32 x 18 track, a 12 px knob 3 px in.
fn paint_toggle(cx: &mut PaintCx, r: RectP, on: bool) {
    let c = cx.tokens().color.clone();
    let (knob, inset) = (cx.px(12), cx.px(3));
    let ky = r.t + (r.h() - knob) / 2;
    if on {
        cx.fill(r, r.h() / 2, c.fill_selected);
        cx.fill(RectP::wh(r.r - inset - knob, ky, knob, knob), knob / 2, c.fg_on_selected);
    } else {
        cx.fill(r, r.h() / 2, c.toggle_off_track);
        cx.stroke(r, r.h() / 2, cx.px(1).max(1), c.toggle_off_rim);
        cx.fill(RectP::wh(r.l + inset, ky, knob, knob), knob / 2, c.toggle_off_knob);
    }
}

/// The dock's slider: a 4 px rail, the filled part to the knob, a 7 px knob.
fn paint_slider(cx: &mut PaintCx, r: RectP, value: i32) {
    let c = cx.tokens().color.clone();
    let (knob_r, th) = (cx.px(7), cx.px(4));
    let cy = (r.t + r.b) / 2;
    let rail = RectP::wh(r.l, cy - th / 2, r.w(), th);
    cx.fill(rail, th / 2, c.slider_rail);
    let (tl, tr) = (r.l + knob_r, r.r - knob_r);
    let kx = tl + (tr - tl) * value / 100;
    cx.fill(RectP { r: kx, ..rail }, th / 2, c.fg_primary);
    cx.fill(
        RectP { l: kx - knob_r, t: cy - knob_r, r: kx + knob_r, b: cy + knob_r },
        knob_r,
        Ink { a: 255, ..c.fg_primary },
    );
}

/// The workload page.
pub fn page(lang: UiLang) -> Option<Page> {
    let nav = NAV_NAMES
        .iter()
        .enumerate()
        .map(|(i, (en, it))| TextLabel::new(WidgetId(1 + i as u32), s(lang, en, it), TypeRole::Body, FgRole::Primary))
        .collect();
    let labels = (0..ROWS)
        .map(|i| TextLabel::new(WidgetId(100 + i as u32), row_text(lang, i), TypeRole::Body, FgRole::Primary))
        .collect();
    let buttons = (0..ROWS)
        .filter(|i| matches!(control(*i), Control::Button))
        .map(|i| Button::new(WidgetId(200 + i as u32), s(lang, "Open", "Apri"), ButtonKind::Secondary))
        .collect();
    let screen = SettingsLike { nav, labels, buttons, slots: Vec::new(), commands: CommandTable::new() };
    Some(Page { name: "settings_like", size: SIZE, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_workload_is_fixed() {
        // S-RASTER's page: do not change its shape (the bench numbers are compared across groups).
        let p = page(UiLang::EN).unwrap();
        assert_eq!((p.size.w, p.size.h), (960, 1112));
        let h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let dump = h.a11y_dump();
        assert_eq!(dump.matches("Button \"Open\"").count(), 10);
        assert_eq!(dump.lines().count(), NAV + ROWS + 10);
        h.assert_named();
    }

    #[test]
    fn the_page_paints_text_toggles_and_sliders() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        assert_eq!(img.text_rects.len(), NAV + ROWS + 10, "one text run per label and button");
        // The first toggle row (i = 1): an off toggle's track, 40 over black, at its centre-left.
        let y = (row_y(1) + 9 + 9) as u32;
        let x = (SIZE.w - MARGIN - 32 + 6) as u32;
        assert!(img.px[(y * img.w + x) as usize] & 0xFF >= 30, "the toggle track is drawn");
    }
}
