//! The S-RASTER bench (K2a.3): the Settings-like page as a 4K full frame through the Harness.
//!
//! Harness::render allocates the 3840 x 2160 image and keeps a copy of it for look-backs, which costs the same for any
//! page (about 11 ms on a busy development PC, measured with an empty screen of the same size). That is the harness's
//! own work, not the rasteriser's, so the bench reports the page's median minus the empty screen's median: the
//! question S-RASTER asks is what painting the page costs.

use super::run;
use crate::pages;
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{SizeL, SizeP};
use oled_ui::harness::Harness;
use oled_ui::screen::Screen;
use oled_ui::strings::UiLang;
use oled_ui::theme::Tokens;

/// The window of the bench in logical px: 3840 x 2160 physical at 192 DPI.
const WINDOW: SizeL = SizeL { w: 1920, h: 1080 };
pub(crate) const WARMUP: u32 = 20;
pub(crate) const RUNS: u32 = 200;

/// A screen that paints nothing: the harness's own cost of a frame.
pub(crate) struct Empty(CommandTable);

impl Empty {
    pub(crate) fn new() -> Empty {
        Empty(CommandTable::new())
    }
}

/// The median ms of rendering an empty screen of `size` at `dpi`: what a frame costs the Harness before any widget
/// paints (the image is allocated and a copy of it is kept). The table benches report their frame less this.
pub(crate) fn empty_frame_ms(size: SizeL, dpi: u32) -> f64 {
    let mut empty = Harness::new(Empty::new(), size, dpi, Tokens::oled());
    run("empty_frame", WARMUP, RUNS, || {
        std::hint::black_box(empty.render());
    })
}

impl Screen for Empty {
    fn layout(&mut self, _: &mut LayoutCx, avail: SizeP) -> SizeP {
        avail
    }

    fn paint(&self, _: &mut PaintCx) {}

    fn event(&mut self, _: &mut EventCx, _: &Event) -> Reply {
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.0
    }
}

/// The median ms of one 3840 x 2160 settings_like frame at 192 DPI, full frame, caches warm (20 warm-up renders and
/// 200 measured), less the same frame of an empty screen.
pub fn raster_4k_settings_like() -> Option<f64> {
    let page = pages::settings_like::page(UiLang::EN)?;
    let base = empty_frame_ms(WINDOW, 192);
    let mut h = Harness::new(page.screen, WINDOW, 192, Tokens::oled());
    let full = run("raster_4k_settings_like", WARMUP, RUNS, || {
        std::hint::black_box(h.render());
    });
    println!("raster_4k_settings_like: frame {full:.3} ms, empty frame {base:.3} ms");
    Some((full - base).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_is_4k() {
        let page = pages::settings_like::page(UiLang::EN).unwrap();
        let mut h = Harness::new(page.screen, WINDOW, 192, Tokens::oled());
        let img = h.render();
        assert_eq!((img.w, img.h), (3840, 2160));
        assert!(img.px.iter().any(|p| p & 0xFF != 0), "the page paints something");
        let mut e = Harness::new(Empty::new(), WINDOW, 192, Tokens::oled());
        assert!(e.render().px.iter().all(|p| p & 0xFF == 0), "the empty screen paints nothing");
    }
}
