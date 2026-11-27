//! Tree mode: processes under their parents. Filled by TA-T3a (T3.2).

use std::cmp::Ordering;

use super::list::{cmp_by_sort, named_decor};
use super::rows::{proc_key, RowEntry};
use super::view::View;
use crate::app::resolver::NameBook;
use crate::model::tree::{forest, Forest, NONE};
use crate::model::view::ColumnId;

/// The scratch of the build (the collapsed nodes live in the window's state, for the session).
#[derive(Default)]
pub struct TreeState {
    forest: Forest,
    stack: Vec<(u32, u16)>,
    kids: Vec<u32>,
    /// The lower-cased image names of the rows, when the sort is by name.
    lower: Vec<String>,
    /// A search stands: every node is built open (the search filter keeps what matches).
    query_open: bool,
}

impl TreeState {
    /// While a search stands every node is built open, so every process is a row (T5.2).
    pub fn set_query_open(&mut self, on: bool) {
        self.query_open = on;
    }
}

/// The rows of Tree mode: every process under its parent (a process whose parent is gone is a root), siblings in the
/// order of the sort, depth first. A collapsed process hides its descendants.
pub fn build(v: &View, names: &NameBook, st: &mut TreeState, rows: &mut Vec<RowEntry>) {
    v.clear_decor();
    let procs = v.procs();
    forest(procs, &mut st.forest);
    let sort = v.sort();
    let mut ents: Vec<Option<RowEntry>> = procs.rows.iter().map(|p| Some(v.proc_entry(p))).collect();
    st.lower.clear();
    if sort.column == ColumnId::Name {
        for p in &procs.rows {
            let mut s = String::new();
            v.proc_name(p.key, &mut s);
            s.make_ascii_lowercase();
            st.lower.push(s);
        }
    }
    let order = |a: u32, b: u32, ents: &[Option<RowEntry>], lower: &[String]| -> Ordering {
        let (ea, eb) = (&ents[a as usize], &ents[b as usize]);
        match (ea, eb) {
            (Some(x), Some(y)) => cmp_by_sort(
                sort,
                x,
                y,
                lower.get(a as usize).map(String::as_str),
                lower.get(b as usize).map(String::as_str),
            ),
            _ => Ordering::Equal,
        }
    };
    let TreeState { forest, stack, kids, lower, query_open } = st;
    let query_open = *query_open;
    kids.clear();
    kids.extend(forest.roots.iter().copied());
    kids.sort_by(|&a, &b| order(a, b, &ents, lower));
    stack.clear();
    stack.extend(kids.iter().rev().map(|&i| (i, 0u16)));
    let state = v.state();
    while let Some((i, depth)) = stack.pop() {
        let Some(mut e) = ents[i as usize].take() else { continue };
        let row = &procs.rows[i as usize];
        e.depth = depth;
        e.parent = row.parent.filter(|k| procs.index_of(*k).is_some()).map(proc_key);
        let has_kids = forest.first_child[i as usize] != NONE;
        let collapsed = !query_open && state.is_some_and(|s| s.tree_collapsed(e.key.0));
        e.expandable = has_kids;
        e.expanded = has_kids && !collapsed;
        if let Some(d) = named_decor(v, names, i as usize, row) {
            v.set_decor(e.key, d);
        }
        rows.push(e);
        if has_kids && !collapsed {
            kids.clear();
            let mut c = forest.first_child[i as usize];
            while c != NONE {
                kids.push(c);
                c = forest.next_sibling[c as usize];
            }
            kids.sort_by(|&a, &b| order(a, b, &ents, lower));
            stack.extend(kids.iter().rev().map(|&k| (k, depth.saturating_add(1))));
        }
    }
}
