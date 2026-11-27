//! The key map of the beta (T5.4): every binding the kit does not own. Filled by TA-T5b.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T5b

use oled_ui::widgets::VirtualTable;
use oled_ui::{Event, EventCx};

use super::rows::TasksTable;

/// Type-ahead and the region cycle.
#[derive(Default)]
pub struct KeyState;

/// A key other than Esc; true when handled. T5.4. Stub: false.
pub fn on_key(cx: &mut EventCx, st: &mut KeyState, ev: &Event, table: &mut VirtualTable<TasksTable>) -> bool {
    false
}
