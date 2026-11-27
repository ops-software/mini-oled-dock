//! Row verbs and their states (T3.8): Close or End by row, the inline action, busy rows, ended rows with Reopen, the
//! outcome notes, and the commands that act. Every act goes through deps.exec (the Executor). Filled by TA-T3c.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3c

use oled_ui::model::TableOut;
use oled_ui::widgets::VirtualTable;
use oled_ui::{CommandId, EventCx, TimerId};

use super::rows::{RowEntry, TasksTable};
use super::screen::TasksScreen;
use super::view::View;
use crate::app::exec::ActionDone;
use crate::app::Deps;

/// Rows in flight and their outcomes, the Reopen window, the note expiries.
#[derive(Default)]
pub struct ActionState;

/// The pipeline step: busy, ended and noted rows. T3.8. Stub: nothing.
#[allow(clippy::ptr_arg)] // the frozen signature: T3.8 changes rows in place
pub fn decorate(v: &View, st: &mut ActionState, rows: &mut Vec<RowEntry>, now: u64) {}

/// TIMER_CLOSE_CHECK, TIMER_NOTES. T3.7, T3.8. Stub: nothing.
pub fn on_timer(cx: &mut EventCx, st: &mut ActionState, id: TimerId) {}

/// RowAction and ContextMenu (context_menu::open). T3.8, T3.9. Stub: nothing.
pub fn on_table_out(
    cx: &mut EventCx,
    st: &mut ActionState,
    out: &TableOut,
    table: &VirtualTable<TasksTable>,
    v: &View,
    deps: &Deps,
) {
}

/// An outcome from the executor. T3.8. Stub: nothing.
pub fn on_done(cx: &mut EventCx, st: &mut ActionState, m: ActionDone) {}

/// CLOSE_OR_END, END_NOW, END_TREE, REOPEN. T3.8. Stub: nothing.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {}
