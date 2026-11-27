//! List mode: one row per process, flat. Filled by TA-T1a (T1.2); Tree and List together by TA-T3a (T3.2).

use std::cmp::Ordering;

use super::rows::{sort_value as value, RowEntry, RowKind};
use super::view::View;
use crate::app::resolver::NameBook;
use crate::model::view::ColumnId;

/// The rows of List mode: every process of the newest frame, in the order of the sort (ties by process key).
pub fn build(v: &View, names: &NameBook, rows: &mut Vec<RowEntry>) {
    let _ = names;
    let start = rows.len();
    rows.extend(v.procs().rows.iter().map(|p| v.proc_entry(p)));
    sort_rows(v, rows.get_mut(start..).unwrap_or(&mut []));
}

/// A stable sort by the view's column: the processes arrive in key order, so equal values stay in key order.
fn sort_rows(v: &View, rows: &mut [RowEntry]) {
    let s = v.sort();
    if s.column == ColumnId::Name {
        let mut keyed: Vec<(String, usize)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut name = String::new();
                if let RowKind::Proc(k) = r.kind {
                    v.proc_name(k, &mut name);
                }
                (name.to_lowercase(), i)
            })
            .collect();
        keyed.sort_by(|a, b| if s.descending { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) }.then(a.1.cmp(&b.1)));
        let order: Vec<usize> = keyed.into_iter().map(|(_, i)| i).collect();
        let mut taken: Vec<Option<RowEntry>> = rows.iter().cloned().map(Some).collect();
        for (slot, i) in rows.iter_mut().zip(order) {
            if let Some(r) = taken[i].take() {
                *slot = r;
            }
        }
        return;
    }
    rows.sort_by(|a, b| match (value(a, s.column), value(b, s.column)) {
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
    });
}
