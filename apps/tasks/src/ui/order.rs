//! Rows that hold still (SPEC-tasks 5.2.7 through the model's order rules): per-scope order, the hold under the
//! pointer and after a list key, the click guard, ghosts, and the release motion. Filled by TA-T2 (T2.1-T2.3) and
//! TA-T2M (T2.4).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T2

use std::cmp::Ordering;

use oled_ui::model::TableOut;
use oled_ui::widgets::VirtualTable;
use oled_ui::{Event, EventCx, PaintCx, TimerId};

use super::rows::{RowEntry, TasksTable};
use super::view::{SortState, View};
use crate::model::view::ColumnId;

/// The order state between ticks.
#[derive(Default)]
pub struct OrderGlue;

/// Whether a pointer event reaches the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerGate {
    /// A click within the guard after a move under the pointer: consumed, never reaches the table.
    Swallow,
    Pass,
}

/// T2.1. Stub: a stable sort by v.sort() inside each run of consecutive rows with the same parent; Section and Hardware
/// rows never move; a row without the value sorts last.
#[allow(clippy::ptr_arg)] // the frozen signature: T2.1 keeps ghost rows in place
pub fn apply(v: &View, g: &mut OrderGlue, rows: &mut Vec<RowEntry>, now: u64) {
    let sort = v.sort();
    let mut start = 0;
    while let Some(first) = rows.get(start) {
        if first.is_anchor() {
            start += 1;
            continue;
        }
        let parent = first.parent;
        let mut end = start + 1;
        while rows.get(end).is_some_and(|r| !r.is_anchor() && r.parent == parent) {
            end += 1;
        }
        if let Some(run) = rows.get_mut(start..end) {
            run.sort_by(|a, b| by_sort(a, b, sort));
        }
        start = end;
    }
}

/// The same value list.rs sorts by (the idle process has none).
fn value(r: &RowEntry, c: ColumnId) -> Option<f64> {
    super::rows::sort_value(r, c)
}

fn by_sort(a: &RowEntry, b: &RowEntry, s: SortState) -> Ordering {
    match (value(a, s.column), value(b, s.column)) {
        (Some(x), Some(y)) => {
            let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
            if s.descending {
                o.reverse()
            } else {
                o
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Writes each visible RowEntry.motion while a release slides or fades. T2.4. Stub: nothing.
pub fn on_paint_anim(g: &OrderGlue, cx: &mut PaintCx, model: &TasksTable) {}

/// The short flash of a row that moved under a dropped click. T2.2. Stub: nothing.
pub fn paint_flash(g: &OrderGlue, cx: &mut PaintCx, table: &VirtualTable<TasksTable>) {}

/// TIMER_HOLD, TIMER_FRESH. T2.2, T2.3. Stub: nothing.
pub fn on_timer(cx: &mut EventCx, g: &mut OrderGlue, id: TimerId) {}

/// A navigation key went to the focused table: the order holds 3 s. T2.2. Stub: nothing.
pub fn note_list_key(g: &mut OrderGlue, now: u64) {}

/// A pointer move, down or up inside the table's viewport. T2.2. Stub: Pass.
pub fn on_pointer(
    cx: &mut EventCx,
    g: &mut OrderGlue,
    ev: &Event,
    table: &VirtualTable<TasksTable>,
    now: u64,
) -> PointerGate {
    PointerGate::Pass
}

/// T2.2. Stub: nothing.
pub fn on_table_out(g: &mut OrderGlue, out: &TableOut, now: u64) {}

/// A menu closed: the hold of an open menu ends. T2.2. Stub: nothing.
pub fn on_menu_closed(g: &mut OrderGlue) {}

/// T2.2. Stub: false.
pub fn is_held(g: &OrderGlue, now: u64) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use oled_ui::model::{RowKey, RowOut};

    use super::*;
    use crate::model::view::Section;
    use crate::ui::rows::{RowKind, RowVals};

    fn row(key: u64, kind: RowKind, parent: Option<u64>, cpu: Option<f32>) -> RowEntry {
        RowEntry {
            key: RowKey(key),
            kind,
            section: None,
            parent: parent.map(RowKey),
            depth: 0,
            vals: RowVals { cpu, ..RowVals::default() },
            out: RowOut::default(),
            motion: Cell::new((0, 255)),
            expandable: false,
            expanded: false,
        }
    }

    #[test]
    fn stub_sorts_runs_and_keeps_anchors() {
        let p = |n| RowKind::Proc(oled_sysmon::ProcKey { pid: n, created: 1 });
        let mut rows = vec![
            row(1, RowKind::Section(Section::Apps), None, None),
            row(2, p(2), Some(1), Some(1.0)),
            row(3, p(3), Some(1), None),
            row(4, p(4), Some(1), Some(5.0)),
            row(5, RowKind::Hardware, None, Some(9.0)),
            row(6, p(6), Some(5), Some(0.5)),
            row(7, p(7), Some(5), Some(2.0)),
        ];
        apply(&View::default(), &mut OrderGlue, &mut rows, 0);
        let keys: Vec<u64> = rows.iter().map(|r| r.key.0).collect();
        assert_eq!(keys, [1, 4, 2, 3, 5, 7, 6], "CPU descending inside each run, no value last, anchors in place");
    }
}
