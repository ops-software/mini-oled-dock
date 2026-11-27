//! List mode: one row per process, flat. Filled by TA-T1a (T1.2); Tree and List together by TA-T3a (T3.2).

use std::cmp::Ordering;

use oled_sysmon::ProcRow;

use super::rows::{sort_value as value, Decor, RowEntry, RowKind};
use super::view::{SortState, View};
use crate::app::resolver::NameBook;
use crate::model::view::ColumnId;

/// The separator between the leading name of a row and the tertiary text after it.
pub const SEP: &str = " \u{b7} ";

/// A label of `head` and a tertiary `tail` after the separator.
pub fn tailed(head: &str, tail: &str) -> Decor {
    Decor { head: head.to_string(), tail: format!("{SEP}{tail}"), hung: false }
}

/// The label of a process row of Tree or List: its image name and, in tertiary after it, the name the resolver gave
/// its program. None while no name is known (the row shows its image name).
pub fn named_decor(v: &View, names: &NameBook, index: usize, p: &ProcRow) -> Option<Decor> {
    let exe = v.info_rows().get(index).filter(|i| i.key == p.key)?.exe?;
    let friendly = names.name_of_exe(exe)?;
    let mut image = String::new();
    v.proc_name(p.key, &mut image);
    if image.eq_ignore_ascii_case(friendly) {
        return None;
    }
    Some(tailed(&image, friendly))
}

/// The rows of List mode: every process of the newest frame, in the order of the sort (ties by process key).
pub fn build(v: &View, names: &NameBook, rows: &mut Vec<RowEntry>) {
    v.clear_decor();
    let start = rows.len();
    for (i, p) in v.procs().rows.iter().enumerate() {
        let e = v.proc_entry(p);
        if let Some(d) = named_decor(v, names, i, p) {
            v.set_decor(e.key, d);
        }
        rows.push(e);
    }
    sort_rows(v, rows.get_mut(start..).unwrap_or(&mut []));
}

/// The order of two rows under a sort; `name_a` and `name_b` are their lower-cased image names when the sort is by
/// name. A row without the value sorts last, whatever the direction.
pub fn cmp_by_sort(s: SortState, a: &RowEntry, b: &RowEntry, name_a: Option<&str>, name_b: Option<&str>) -> Ordering {
    if s.column == ColumnId::Name {
        let o = name_a.unwrap_or("").cmp(name_b.unwrap_or(""));
        return if s.descending { o.reverse() } else { o };
    }
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
    rows.sort_by(|a, b| cmp_by_sort(s, a, b, None, None));
}
