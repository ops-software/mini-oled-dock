//! Multi-select (T5.5): Shift and Ctrl with the move keys, Ctrl+Space and Ctrl+A come from the kit table; this file keeps
//! a selection inside one section (Grouped; Tree and List are one section) and lets Esc clear it.

use oled_ui::model::{KeySet, ListModel, RowKey, Selection, TableOut};
use oled_ui::widgets::VirtualTable;
use oled_ui::EventCx;

use super::rows::TasksTable;
use crate::model::view::Section;

/// The section of a row; None for a row of Tree or List (and for a key no longer in the table).
fn section_of(table: &VirtualTable<TasksTable>, key: RowKey) -> Option<Section> {
    let row = table.model().row_of(key)?;
    table.model().rows.get(row)?.section
}

/// Esc clears a multi-selection down to the focused row (after the search); true when it did. The range anchor moves
/// to that row, so the next Shift+arrow starts from it and the cleared rows do not come back.
pub fn on_esc(cx: &mut EventCx, table: &mut VirtualTable<TasksTable>) -> bool {
    let mut selected = 0;
    table.selection().for_each(table.model(), &mut |_, _| selected += 1);
    if selected < 2 {
        return false;
    }
    let keep = table.focused().filter(|k| table.model().row_of(*k).is_some());
    table.set_selection(cx, keep.map_or(Selection::None, Selection::Single));
    table.set_anchor(keep);
    true
}

/// SelectionChanged and FocusChanged keep the selection inside one section.
pub fn on_table_out(cx: &mut EventCx, out: &TableOut, table: &mut VirtualTable<TasksTable>) {
    if matches!(out, TableOut::SelectionChanged | TableOut::FocusChanged(_)) {
        keep_one_section(cx, table);
    }
}

/// A selection of several rows lives in the section of the focused row (Ctrl+A takes that whole section): rows of
/// another section are dropped. Called after the table's events and after every publish, which can change the rows
/// under a selection without a SelectionChanged (a view switch).
pub fn keep_one_section(cx: &mut EventCx, table: &mut VirtualTable<TasksTable>) {
    if matches!(table.selection(), Selection::None | Selection::Single(_)) {
        return;
    }
    let focus = table.focused().filter(|k| table.model().row_of(*k).is_some());
    let mut keys: Vec<(RowKey, Option<Section>)> = Vec::new();
    table.selection().for_each(table.model(), &mut |row, key| {
        keys.push((key, table.model().rows.get(row).and_then(|r| r.section)));
    });
    if keys.len() < 2 {
        return;
    }
    let home = focus.map_or(keys[0].1, |f| section_of(table, f));
    if keys.iter().all(|k| k.1 == home) {
        return;
    }
    let kept: Vec<RowKey> = keys.iter().filter(|k| k.1 == home).map(|k| k.0).collect();
    let selection = match kept.as_slice() {
        [] => focus.map_or(Selection::None, Selection::Single),
        [only] => Selection::Single(*only),
        _ => {
            let mut set = KeySet::new();
            for k in &kept {
                set.insert(*k);
            }
            Selection::Keys(set)
        }
    };
    table.set_selection(cx, selection);
}
