//! The field bench (K2b.3): a keystroke's edit, layout and repaint of a field, at 150 % (144 DPI).
//!
//! Like the table benches it reports the frame less the same frame of an empty screen (the Harness's own copy of the
//! image), so the figure is the field's work.

use super::raster::{empty_frame_ms, RUNS, WARMUP};
use super::run;
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::harness::Harness;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::theme::Tokens;
use oled_ui::widgets::{FieldKind, TextField};

/// One search field filling a short window.
struct One {
    f: TextField,
    size: SizeL,
    cmds: CommandTable,
}

impl Screen for One {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.f.layout(cx, RectP::wh(cx.px(8), cx.px(8), cx.px(self.size.w - 16), cx.px(28)));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.f.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.f.event(cx, ev);
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// A TextField keystroke to the repainted field: the character typed, the field laid out again and the window painted.
/// The text is cleared every 40 keys so that the field stays a typical one (a search term, a name).
pub fn field_key() -> Option<f64> {
    let size = SizeL { w: 320, h: 44 };
    let dpi = 144;
    let base = empty_frame_ms(size, dpi);
    let mut f = TextField::new(WidgetId(1), FieldKind::Search);
    f.set_placeholder(Text::from("Search"));
    let mut h = Harness::new(One { f, size, cmds: CommandTable::new() }, size, dpi, Tokens::oled());
    h.key("Tab");
    let mut i = 0u32;
    let full = run("field_key", WARMUP, RUNS, || {
        if i % 40 == 39 {
            h.key("Ctrl+A").key("Delete");
        }
        h.type_text(if i.is_multiple_of(7) { " " } else { "e" });
        std::hint::black_box(h.render());
        i = i.wrapping_add(1);
    });
    println!("field_key: frame {full:.3} ms, empty frame {base:.3} ms");
    Some((full - base).max(0.0))
}
