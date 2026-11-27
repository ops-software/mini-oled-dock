//! Multi-select (T5.5): Shift and Ctrl with the move keys, Ctrl+Space, Ctrl+A in a section; a selection never spans
//! two sections. Filled by TA-T5b.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T5b

use oled_ui::model::TableOut;
use oled_ui::widgets::VirtualTable;
use oled_ui::EventCx;

use super::rows::TasksTable;

/// The anchor and the section of the selection.
#[derive(Default)]
pub struct SelState;

/// Esc clears a multi-selection (after the search); true when it did. T5.5. Stub: false.
pub fn on_esc(cx: &mut EventCx, st: &mut SelState, table: &mut VirtualTable<TasksTable>) -> bool {
    false
}

/// SelectionChanged and FocusChanged keep the selection inside one section. T5.5. Stub: nothing.
pub fn on_table_out(cx: &mut EventCx, st: &mut SelState, out: &TableOut, table: &mut VirtualTable<TasksTable>) {}
