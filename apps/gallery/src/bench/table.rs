//! The table benches (K2a.7): the hover frame, 40 rows of text, the scroll strip and the 4K table, at 150 % (144 DPI)
//! except the 4K one (192 DPI), on a table of a hundred thousand rows with the caches warm.
//!
//! The Harness always renders whole frames and keeps a copy of each, which costs the same for any screen (see
//! raster.rs), so every bench reports its frame less the same frame of an empty screen. The windows are sized so that
//! the frame is the work the budget names: two rows for the hover (the two damaged row rects), one row for the strip
//! the retained scroll exposes, 41 rows for the text bench.

use std::borrow::Cow;

use super::raster::{empty_frame_ms, RUNS, WARMUP};
use super::run;
use crate::pages::table::{columns, Demo};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{PointL, RectP, SizeL, SizeP};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::{
    Align, CellKind, CellOut, ColId, Column, ListModel, RowKey, ScrollTo, SelectionMode, SortDir, TableModel,
    TableStyle,
};
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::theme::Tokens;
use oled_ui::widgets::VirtualTable;

/// A closure the bench runs inside the table's EventCx (Harness::send).
type Op<M> = Box<dyn FnOnce(&mut VirtualTable<M>, &mut EventCx) + Send>;

/// One table filling the window.
struct One<M: TableModel> {
    t: VirtualTable<M>,
    size: SizeL,
    cmds: CommandTable,
}

impl<M: TableModel + 'static> Screen for One<M> {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.t.layout(cx, RectP::wh(0, 0, cx.px(self.size.w), cx.px(self.size.h)));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.t.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.t.event(cx, ev);
        let _ = self.t.take_extra();
        Reply::None
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if let Ok(op) = msg.downcast::<Op<M>>() {
            (*op)(&mut self.t, cx);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// Rows of four text cells.
struct Text4(usize);

impl ListModel for Text4 {
    fn len(&self) -> usize {
        self.0
    }
    fn key(&self, row: usize) -> RowKey {
        RowKey(row as u64)
    }
    fn row_of(&self, key: RowKey) -> Option<usize> {
        ((key.0 as usize) < self.0).then_some(key.0 as usize)
    }
    fn label(&self, row: usize) -> Cow<'_, str> {
        Cow::Owned(format!("row {row}"))
    }
    fn generation(&self) -> u64 {
        0
    }
}

impl TableModel for Text4 {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.set_text(CellKind::Text, &format!("cell {row}.{}", col.0));
    }
}

fn text_columns() -> Vec<Column> {
    let col = |id: u16, flex: bool| Column {
        id: ColId(id),
        title: Cow::Owned(format!("Column {id}")),
        width: 200,
        min: 48,
        flex,
        align: Align::Start,
        sort: None,
        first_sort: SortDir::Asc,
        visible: true,
        hideable: false,
        tip: None,
    };
    vec![col(0, false), col(1, false), col(2, false), col(3, true)]
}

/// The bench frame: `step` once per run on a table of `size` logical px at `dpi`, less the empty frame.
fn measure<M: TableModel + 'static>(
    name: &str,
    size: SizeL,
    dpi: u32,
    table: VirtualTable<M>,
    mut step: impl FnMut(&mut Harness<One<M>>, u32),
) -> f64 {
    let base = empty_frame_ms(size, dpi);
    let screen = One { t: table, size, cmds: CommandTable::new() };
    let mut h = Harness::new(screen, size, dpi, Tokens::oled());
    let mut i = 0u32;
    let full = run(name, WARMUP, RUNS, || {
        step(&mut h, i);
        i = i.wrapping_add(1);
    });
    println!("{name}: frame {full:.3} ms, empty frame {base:.3} ms");
    (full - base).max(0.0)
}

fn demo_table(rows: usize, style: TableStyle) -> VirtualTable<Demo> {
    let mut demo = Demo::synthetic(rows);
    demo.ext_spans = true;
    VirtualTable::new(WidgetId(1), demo, columns(UiLang::EN), SelectionMode::Multi, style)
}

/// The pointer moves from one row to the next and the two damaged row rects are repainted: a window of two rows.
pub fn table_hover() -> Option<f64> {
    let size = SizeL { w: 1100, h: 56 };
    Some(measure("table_hover", size, 144, demo_table(100_000, TableStyle::TASKS), |h, i| {
        h.hover(PointL { x: 400, y: if i % 2 == 0 { 14 } else { 42 } });
        std::hint::black_box(h.render());
    }))
}

/// 41 rows of 4 text cells painted.
pub fn table_text_40x4() -> Option<f64> {
    let size = SizeL { w: 1100, h: 28 + 41 * 28 };
    let t = VirtualTable::new(WidgetId(1), Text4(100_000), text_columns(), SelectionMode::Multi, TableStyle::FILES);
    Some(measure("table_text_40x4", size, 144, t, |h, _| {
        std::hint::black_box(h.render());
    }))
}

/// The strip a one-row scroll exposes: a window one row high, scrolled one row per run.
pub fn scroll_strip() -> Option<f64> {
    let size = SizeL { w: 1100, h: 28 };
    Some(measure("scroll_strip", size, 144, demo_table(100_000, TableStyle::TASKS), |h, i| {
        let row = (i % 90_000) as u64;
        h.send(Box::new(Box::new(move |t: &mut VirtualTable<Demo>, cx: &mut EventCx| {
            t.set_focused(cx, RowKey(row), ScrollTo::Top);
        }) as Op<Demo>));
        std::hint::black_box(h.render());
    }))
}

/// A full 4K table frame: 3840 x 2160 at 192 DPI.
pub fn table_4k_full() -> Option<f64> {
    let size = SizeL { w: 1920, h: 1080 };
    Some(measure("table_4k_full", size, 192, demo_table(100_000, TableStyle::FILES), |h, _| {
        std::hint::black_box(h.render());
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_hold_the_rows_the_benches_name() {
        // 41 rows of 28 px under a 28 px header: 40 full rows are painted.
        let size = SizeL { w: 1100, h: 28 + 41 * 28 };
        let t = VirtualTable::new(WidgetId(1), Text4(1000), text_columns(), SelectionMode::Multi, TableStyle::FILES);
        let screen = One { t, size, cmds: CommandTable::new() };
        let mut h = Harness::new(screen, size, 144, Tokens::oled());
        let img = h.render();
        assert_eq!((img.w, img.h), (1650, 1764));
        assert!(h.screen().t.visible_rows().len() >= 40);
        // The strip window shows exactly one row and the hover window two.
        let strip =
            One { t: demo_table(1000, TableStyle::TASKS), size: SizeL { w: 1100, h: 28 }, cmds: CommandTable::new() };
        let h = Harness::new(strip, SizeL { w: 1100, h: 28 }, 144, Tokens::oled());
        assert_eq!(h.screen().t.visible_rows().len(), 1);
        let hover =
            One { t: demo_table(1000, TableStyle::TASKS), size: SizeL { w: 1100, h: 56 }, cmds: CommandTable::new() };
        let h = Harness::new(hover, SizeL { w: 1100, h: 56 }, 144, Tokens::oled());
        assert_eq!(h.screen().t.visible_rows().len(), 2);
    }
}
