//! Grouped mode: the Apps, Background and Part of Windows sections, groups and their members, hot rows that escape,
//! the header's view-mode menu and the view commands. Filled by TA-T3a (T3.1).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3a

use oled_ui::model::TableOut;
use oled_ui::{CommandId, EventCx, PointP};

use super::list;
use super::rows::RowEntry;
use super::screen::TasksScreen;
use super::view::View;
use crate::app::resolver::NameBook;
use crate::app::state::StateHandle;
use crate::model::view::ViewMode;

/// Expanded sections and groups, escaped rows.
#[derive(Default)]
pub struct GroupedState;

/// The mode the window opens in (the remembered one). T3.1. Stub: ViewMode::List.
pub fn initial_mode(state: &StateHandle) -> ViewMode {
    ViewMode::List
}

/// The rows of Grouped mode. T3.1. Stub: List mode's rows.
pub fn build(v: &View, names: &NameBook, st: &mut GroupedState, rows: &mut Vec<RowEntry>) {
    list::build(v, names, rows);
}

/// The header's right click: Grouped, Tree, List. T3.1. Stub: nothing.
pub fn header_menu(cx: &mut EventCx, at: PointP, mode: ViewMode) {}

/// ExpandRequested and Activated on a section or group row; true when the rows change (the pipeline runs). T3.1.
/// Stub: false.
pub fn on_table_out(cx: &mut EventCx, st: &mut GroupedState, out: &TableOut, state: &StateHandle) -> bool {
    false
}

/// Part of Windows expanded, or an escaped svchost row shown (open point 18). T3.1. Stub: false.
pub fn windows_rows_visible(st: &GroupedState, v: &View) -> bool {
    false
}

/// VIEW_GROUPED, VIEW_TREE, VIEW_LIST. T3.1. Stub: nothing.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {}
