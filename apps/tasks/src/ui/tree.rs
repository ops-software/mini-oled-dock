//! Tree mode: processes under their parents. Filled by TA-T3a (T3.2).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3a

use super::list;
use super::rows::RowEntry;
use super::view::View;
use crate::app::resolver::NameBook;

/// Expanded processes.
#[derive(Default)]
pub struct TreeState;

/// The rows of Tree mode. T3.2. Stub: List mode's rows.
pub fn build(v: &View, names: &NameBook, st: &mut TreeState, rows: &mut Vec<RowEntry>) {
    list::build(v, names, rows);
}
