//! The charts page (K4.1): the strips and the area chart of the tasks app's headers, drawn from fixed data so the
//! pictures never change: a quiet trace, a lone spike, a noisy load past its threshold, two series and the narrow
//! widths the peak-keeping resampling is for. No hue but the attention colour past a threshold.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::theme::{Tokens, TypeRole};
use oled_ui::widgets::chart::{AreaChart, Series, Sparkline};
use oled_ui::widgets::TextLabel;

const SIZE: SizeL = SizeL { w: 480, h: 330 };

/// A widget and where it goes, logical px.
struct Placed {
    w: Box<dyn Widget<Out = ()>>,
    rect: (i32, i32, i32, i32),
}

struct Charts {
    items: Vec<Placed>,
    commands: CommandTable,
}

impl Charts {
    fn add<W: Widget<Out = ()> + 'static>(&mut self, w: W, rect: (i32, i32, i32, i32)) {
        self.items.push(Placed { w: Box::new(w), rect });
    }
}

impl Screen for Charts {
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

/// A repeatable pseudo-random value in 0..100 (no clock, no entropy: the page must render the same every time).
fn noise(i: u32) -> f32 {
    let x = i.wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(40_503);
    (x >> 8) as f32 % 100.0
}

/// A smooth load that wanders between about 10 and 70, with a little noise.
fn load(n: u32) -> Vec<f32> {
    (0..n).map(|i| 40.0 + 25.0 * (i as f32 / 7.0).sin() + 8.0 * (i as f32 / 2.3).cos() + noise(i) / 25.0).collect()
}

fn heading(id: u32, text: Text, y: i32) -> (TextLabel, (i32, i32, i32, i32)) {
    (TextLabel::new(WidgetId(id), text, TypeRole::Section, FgRole::Secondary), (16, y, 440, 18))
}

/// The page: strips, an area chart and the narrow widths.
pub fn page(lang: UiLang) -> Option<Page> {
    let ink = Tokens::oled().color;
    let mut t = Charts { items: Vec::new(), commands: CommandTable::new() };
    let mut id = 0;
    let mut next = || {
        id += 1;
        WidgetId(id)
    };

    let (h, r) = heading(next().0, s(lang, "Strips", "Strisce"), 10);
    t.add(h, r);
    let quiet = vec![2.0f32; 60];
    let mut spike = vec![3.0f32; 60];
    spike[31] = 100.0;
    let busy: Vec<f32> = (0..60).map(noise).collect();
    let strips: [(&Vec<f32>, Option<f32>, Text); 3] = [
        (&quiet, None, s(lang, "Idle, 2 % for 60 s", "Inattivo, 2 % per 60 s")),
        (&spike, None, s(lang, "One spike of 100 % in the last 60 s", "Un picco al 100 % negli ultimi 60 s")),
        (&busy, Some(80.0), s(lang, "Busy, past 80 % now and then", "Occupato, oltre l'80 % a tratti")),
    ];
    for (i, (values, threshold, summary)) in strips.into_iter().enumerate() {
        let mut sp = Sparkline::new(next(), summary);
        sp.set(&[Series { values, ink: ink.fg_secondary }], 100.0, threshold);
        t.add(sp, (16 + 152 * i as i32, 34, 144, 24));
    }

    let (h, r) = heading(next().0, s(lang, "Area chart", "Grafico ad area"), 72);
    t.add(h, r);
    let cpu = load(120);
    let mem: Vec<f32> = (0..120).map(|i| 30.0 + i as f32 / 6.0).collect();
    let mut ch = AreaChart::new(
        next(),
        s(lang, "CPU 23 %, memory 31 % in the last 2 minutes", "CPU 23 %, memoria 31 % negli ultimi 2 minuti"),
    );
    ch.set(
        &[Series { values: &cpu, ink: ink.fg_secondary }, Series { values: &mem, ink: ink.fg_tertiary }],
        100.0,
        Some(60.0),
    );
    ch.set_labels(s(lang, "100 %", "100 %"), s(lang, "0", "0"));
    t.add(ch, (16, 96, 448, 112));

    let (h, r) =
        heading(next().0, s(lang, "Narrow, one spike in 10,000 samples", "Stretto, un picco in 10.000 campioni"), 220);
    t.add(h, r);
    let mut long = vec![0.0f32; 10_000];
    long[4_321] = 100.0;
    for (i, w) in [37, 120, 400].into_iter().enumerate() {
        let x = 16 + [0, 52, 184][i];
        let mut sp = Sparkline::new(next(), s(lang, "One spike among 10,000 samples", "Un picco fra 10.000 campioni"));
        sp.set(&[Series { values: &long, ink: ink.fg_secondary }], 100.0, None);
        t.add(sp, (x, 248, w.min(264), 24));
    }
    let (h, r) = heading(next().0, s(lang, "Trace (12 px)", "Traccia (12 px)"), 284);
    t.add(h, r);
    let trace = load(300);
    let mut sp = Sparkline::new(next(), s(lang, "CPU trace, 12 px", "Traccia CPU, 12 px")).trace(true);
    sp.set(&[Series { values: &trace, ink: ink.fg_primary }], 100.0, None);
    t.add(sp, (16, 308, 200, 12));
    Some(Page { name: "charts", size: SIZE, states: 1, screen: Box::new(t) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;

    #[test]
    fn the_page_names_every_chart_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            let dump = h.a11y_dump();
            assert!(dump.matches("Image").count() >= 7, "{dump}");
        }
    }

    #[test]
    fn the_spike_shows_at_every_width() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        // The three narrow strips sit at y 248..272; each has a stroke reaching the top rows of its strip.
        for (x, w) in [(16, 37), (68, 120), (200, 264)] {
            let top = RectP::wh(x, 248, w, 6);
            let lit = img.crop(top).px.iter().any(|px| px & 0xFF > 100);
            assert!(lit, "no spike in the strip at x {x}");
        }
    }
}
