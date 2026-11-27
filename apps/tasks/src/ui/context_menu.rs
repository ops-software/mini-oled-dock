//! The context menu of a row or a selection (T3.9): the verb with its description and chord, End now, End process
//! tree, Open Windows Task Manager; refused items disabled with their reason. Filled by TA-T3c.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3c

use oled_ui::model::RowKey;
use oled_ui::widgets::VirtualTable;
use oled_ui::{EventCx, PointP};

use super::rows::TasksTable;
use super::view::View;
use crate::app::Deps;

/// TableOut::ContextMenu (right click, Shift+F10, the Menu key). T3.9. Stub: nothing.
pub fn open(
    cx: &mut EventCx,
    key: Option<RowKey>,
    at: PointP,
    table: &VirtualTable<TasksTable>,
    v: &View,
    deps: &Deps,
) {
}
