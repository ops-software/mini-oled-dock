//! The table's pure interaction model (K2a.2): keys, clicks, type-ahead, the header and the scroll offset, with no
//! painting and no host. The widget (K2a.6) feeds it events and applies the TableEffects it returns.

use std::ops::Range;

use crate::event::{Key, Mods, MouseButton};
use crate::model::{ColId, Column, ListModel, RowKey, ScrollTo, Selection, SelectionMode, SortDir, TableModel};
use crate::tokens::{COL_MIN_W, TYPEAHEAD_RESET};

/// What one input did, for the widget to apply and report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableEffects {
    pub selection_changed: bool,
    /// The new focused row, when the focus moved.
    pub focus: Option<RowKey>,
    /// A row to bring into view; the state's own offset is already moved.
    pub scroll_to: Option<(usize, ScrollTo)>,
    pub activate: Option<RowKey>,
    pub rename: Option<RowKey>,
    /// A context menu was asked for (the widget knows where).
    pub context: bool,
    pub sort: Option<(ColId, SortDir)>,
    /// A column width changed.
    pub columns_changed: bool,
    pub expand: Option<(RowKey, bool)>,
    pub header_menu: Option<ColId>,
    /// Ctrl+Plus: the widget measures every column and calls `autofit` for each.
    pub autofit_all: bool,
}

/// How far a move carries the selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Carry {
    /// The selection becomes the new row.
    Select,
    /// Only the focus moves.
    FocusOnly,
    /// The selection spans from the anchor to the new row (`true`: the rest kept).
    Extend(bool),
}

/// Extra room added to the widest cell by an auto-fit (logical px).
pub const AUTOFIT_PAD: i32 = 24;
/// One Shift+Left/Right resize step (logical px).
pub const RESIZE_STEP: i32 = 8;

/// The pure state of a virtual table.
#[derive(Clone, Debug)]
pub struct TableState {
    /// Scroll offset, physical px: i64 so that a million tall rows do not overflow.
    pub offset: i64,
    pub row_h: i32,
    pub viewport_h: i32,
    pub focused: Option<RowKey>,
    pub anchor: Option<RowKey>,
    /// The text typed so far and the time of the last character (ms).
    pub typeahead: (String, u64),
    /// The laid out width of each column, by column index (0 for a hidden one).
    pub widths: Vec<i32>,
    /// How many rows the user may select.
    pub mode: SelectionMode,
    /// The column the header keys act on.
    pub header_col: Option<ColId>,
}

fn min_w(c: &Column) -> i32 {
    c.min.max(COL_MIN_W)
}

impl TableState {
    pub fn new(row_h: i32, mode: SelectionMode) -> TableState {
        TableState {
            offset: 0,
            row_h: row_h.max(1),
            viewport_h: 0,
            focused: None,
            anchor: None,
            typeahead: (String::new(), 0),
            widths: Vec::new(),
            mode,
            header_col: None,
        }
    }

    // ---- scrolling

    fn max_offset(&self, len: usize) -> i64 {
        (len as i64 * i64::from(self.row_h) - i64::from(self.viewport_h)).max(0)
    }

    pub fn clamp_offset(&mut self, len: usize) {
        self.offset = self.offset.clamp(0, self.max_offset(len));
    }

    /// The rows that touch the viewport, in O(1).
    pub fn visible(&self, len: usize) -> Range<usize> {
        let h = i64::from(self.row_h);
        let first = (self.offset.max(0) / h) as usize;
        let last = ((self.offset.max(0) + i64::from(self.viewport_h) + h - 1) / h) as usize;
        first.min(len)..last.min(len)
    }

    /// The y of a row in the viewport, physical px (negative above it).
    pub fn row_y(&self, row: usize) -> i64 {
        row as i64 * i64::from(self.row_h) - self.offset
    }

    /// Whole rows that fit the viewport, at least 1.
    fn page(&self) -> usize {
        ((self.viewport_h / self.row_h).max(1)) as usize
    }

    pub fn scroll_to_row(&mut self, row: usize, how: ScrollTo, len: usize) {
        let top = row as i64 * i64::from(self.row_h);
        let h = i64::from(self.viewport_h);
        let rh = i64::from(self.row_h);
        match how {
            ScrollTo::Nearest => {
                if top < self.offset {
                    self.offset = top;
                } else if top + rh > self.offset + h {
                    self.offset = top + rh - h;
                }
            }
            ScrollTo::Center => self.offset = top - (h - rh) / 2,
            ScrollTo::Top => self.offset = top,
        }
        self.clamp_offset(len);
    }

    /// After a model change: the focused row, now at `new_row`, stays where it was (`old_y`).
    pub fn keep_anchor(&mut self, old_y: i64, new_row: usize, len: usize) {
        self.offset = new_row as i64 * i64::from(self.row_h) - old_y;
        self.clamp_offset(len);
    }

    // ---- rows

    fn cur_row(&self, m: &dyn ListModel) -> Option<usize> {
        self.focused.and_then(|k| m.row_of(k))
    }

    /// Moves the focus to `row` and carries the selection as asked.
    fn go(&mut self, m: &dyn ListModel, sel: &mut Selection, row: usize, carry: Carry, fx: &mut TableEffects) {
        let key = m.key(row);
        let before = sel.clone();
        let carry = match (carry, self.mode) {
            (_, SelectionMode::None) => Carry::FocusOnly,
            (Carry::Extend(_), SelectionMode::Single) => Carry::Select,
            (c, _) => c,
        };
        match carry {
            Carry::Select => {
                sel.click(key);
                self.anchor = Some(key);
            }
            Carry::FocusOnly => {}
            Carry::Extend(additive) => {
                let anchor = self.anchor.filter(|a| m.row_of(*a).is_some()).or(self.focused).unwrap_or(key);
                self.anchor = Some(anchor);
                sel.extend(m, anchor, key, additive);
            }
        }
        fx.selection_changed = *sel != before;
        if self.focused != Some(key) {
            self.focused = Some(key);
            fx.focus = Some(key);
        }
        self.scroll_to_row(row, ScrollTo::Nearest, m.len());
        fx.scroll_to = Some((row, ScrollTo::Nearest));
    }

    /// A key press on the rows (SPEC-platform 5.3.5, list and table). Printable characters go to `on_text`.
    pub fn on_key(&mut self, key: Key, mods: Mods, m: &dyn TableModel, sel: &mut Selection, _now: u64) -> TableEffects {
        let mut fx = TableEffects::default();
        let len = m.len();
        if len == 0 || mods.alt() || mods.win() {
            return fx;
        }
        let (ctrl, shift) = (mods.ctrl(), mods.shift());
        let carry = if shift {
            Carry::Extend(ctrl)
        } else if ctrl {
            Carry::FocusOnly
        } else {
            Carry::Select
        };
        let cur = self.cur_row(m);
        let last = len - 1;
        let page = self.page().saturating_sub(1).max(1);
        let target = match key {
            Key::UP => Some(cur.map_or(0, |r| r.saturating_sub(1))),
            Key::DOWN => Some(cur.map_or(0, |r| (r + 1).min(last))),
            Key::PAGE_UP => Some(cur.map_or(0, |r| r.saturating_sub(page))),
            Key::PAGE_DOWN => Some(cur.map_or(page.min(last), |r| (r + page).min(last))),
            Key::HOME => Some(0),
            Key::END => Some(last),
            _ => None,
        };
        if let Some(row) = target {
            self.go(m, sel, row, carry, &mut fx);
            return fx;
        }
        match key {
            Key::SPACE if ctrl && !shift => {
                if let (Some(row), true) = (cur, self.mode != SelectionMode::None) {
                    let k = m.key(row);
                    let before = sel.clone();
                    if self.mode == SelectionMode::Multi {
                        sel.toggle(k);
                    } else if sel.contains(k) {
                        *sel = Selection::None;
                    } else {
                        sel.click(k);
                    }
                    self.anchor = Some(k);
                    fx.selection_changed = *sel != before;
                }
            }
            Key(0x41) if ctrl && !shift => {
                if self.mode == SelectionMode::Multi {
                    let before = sel.clone();
                    sel.select_all();
                    fx.selection_changed = *sel != before;
                }
            }
            Key::ENTER if !ctrl && !shift => fx.activate = cur.map(|r| m.key(r)),
            Key::F2 if !ctrl && !shift => fx.rename = cur.map(|r| m.key(r)),
            Key::LEFT | Key::RIGHT if !ctrl && !shift => {
                if let Some(row) = cur {
                    self.tree_key(m, sel, row, key == Key::RIGHT, &mut fx);
                }
            }
            Key::OEM_PLUS | Key::ADD if ctrl => fx.autofit_all = true,
            _ => {}
        }
        fx
    }

    /// Right: expand, or step into the first child; Left: collapse, or step to the parent.
    fn tree_key(&mut self, m: &dyn TableModel, sel: &mut Selection, row: usize, right: bool, fx: &mut TableEffects) {
        let Some(info) = m.tree(row) else { return };
        let key = m.key(row);
        if right {
            if info.expandable && !info.expanded {
                fx.expand = Some((key, true));
            } else if info.expandable
                && info.expanded
                && row + 1 < m.len()
                && m.tree(row + 1).is_some_and(|c| c.depth > info.depth)
            {
                self.go(m, sel, row + 1, Carry::Select, fx);
            }
        } else if info.expandable && info.expanded {
            fx.expand = Some((key, false));
        } else if info.depth > 0 {
            if let Some(parent) = (0..row).rev().find(|&r| m.tree(r).is_some_and(|p| p.depth < info.depth)) {
                self.go(m, sel, parent, Carry::Select, fx);
            }
        }
    }

    /// A printable character: type-ahead. The text resets after TYPEAHEAD_RESET ms; the search starts at the row
    /// after the focused one (at the focused one itself while the text grows) and wraps.
    pub fn on_text(&mut self, c: char, m: &dyn TableModel, sel: &mut Selection, now: u64) -> TableEffects {
        let mut fx = TableEffects::default();
        let len = m.len();
        if len == 0 || c.is_control() {
            return fx;
        }
        if now.saturating_sub(self.typeahead.1) > u64::from(TYPEAHEAD_RESET) {
            self.typeahead.0.clear();
        }
        if c.is_whitespace() && self.typeahead.0.is_empty() {
            return fx;
        }
        self.typeahead.0.extend(c.to_lowercase());
        self.typeahead.1 = now;
        let cur = self.cur_row(m);
        let grows = self.typeahead.0.chars().count() > 1;
        let start = match cur {
            Some(r) if grows => r,
            Some(r) => (r + 1) % len,
            None => 0,
        };
        let found =
            (0..len).map(|i| (start + i) % len).find(|&r| m.label(r).to_lowercase().starts_with(&self.typeahead.0));
        if let Some(row) = found {
            self.go(m, sel, row, Carry::Select, &mut fx);
        }
        fx
    }

    /// A click on a row (`None`: the empty area below the rows).
    pub fn on_click(
        &mut self,
        row: Option<usize>,
        mods: Mods,
        clicks: u8,
        button: MouseButton,
        m: &dyn TableModel,
        sel: &mut Selection,
    ) -> TableEffects {
        let mut fx = TableEffects::default();
        let Some(row) = row.filter(|&r| r < m.len()) else {
            match button {
                MouseButton::Left if !mods.ctrl() && !mods.shift() && self.mode != SelectionMode::None => {
                    let before = sel.clone();
                    *sel = Selection::None;
                    fx.selection_changed = *sel != before;
                }
                MouseButton::Right => fx.context = true,
                _ => {}
            }
            return fx;
        };
        let key = m.key(row);
        match button {
            MouseButton::Left => {
                let carry = if self.mode == SelectionMode::Multi && mods.shift() {
                    Carry::Extend(mods.ctrl())
                } else if self.mode == SelectionMode::Multi && mods.ctrl() {
                    let before = sel.clone();
                    sel.toggle(key);
                    self.anchor = Some(key);
                    fx.selection_changed = *sel != before;
                    self.focus_only(m, row, &mut fx);
                    if clicks >= 2 {
                        fx.activate = Some(key);
                    }
                    return fx;
                } else {
                    Carry::Select
                };
                self.go(m, sel, row, carry, &mut fx);
                if clicks >= 2 {
                    fx.activate = Some(key);
                }
            }
            MouseButton::Right => {
                if !sel.contains(key) {
                    self.go(m, sel, row, Carry::Select, &mut fx);
                } else {
                    self.focus_only(m, row, &mut fx);
                }
                fx.context = true;
            }
            _ => {}
        }
        fx
    }

    fn focus_only(&mut self, m: &dyn ListModel, row: usize, fx: &mut TableEffects) {
        let key = m.key(row);
        if self.focused != Some(key) {
            self.focused = Some(key);
            fx.focus = Some(key);
        }
        self.scroll_to_row(row, ScrollTo::Nearest, m.len());
        fx.scroll_to = Some((row, ScrollTo::Nearest));
    }

    // ---- header

    /// The direction a click on `col` asks for: the sorted column flips, another starts at its first_sort.
    pub fn sort_dir(&self, cols: &[Column], col: ColId) -> SortDir {
        match cols.iter().find(|c| c.id == col) {
            Some(Column { sort: Some(SortDir::Asc), .. }) => SortDir::Desc,
            Some(Column { sort: Some(SortDir::Desc), .. }) => SortDir::Asc,
            Some(c) => c.first_sort,
            None => SortDir::Asc,
        }
    }

    /// A key on the focused header cell.
    pub fn on_header_key(&mut self, key: Key, mods: Mods, cols: &mut [Column]) -> TableEffects {
        let mut fx = TableEffects::default();
        if mods.alt() || mods.win() {
            return fx;
        }
        let visible: Vec<usize> = (0..cols.len()).filter(|&i| cols[i].visible).collect();
        let at = self.header_col.and_then(|id| visible.iter().position(|&i| cols[i].id == id));
        let cur = at.map(|p| visible[p]);
        match key {
            Key::LEFT | Key::RIGHT if mods.shift() && !mods.ctrl() => {
                if let Some(i) = cur {
                    let delta = if key == Key::LEFT { -RESIZE_STEP } else { RESIZE_STEP };
                    fx.columns_changed = self.resize(&mut cols[i], delta);
                }
            }
            Key::LEFT | Key::RIGHT if !mods.ctrl() => {
                let to = match (at, key == Key::LEFT) {
                    (None, _) => visible.first().copied(),
                    (Some(p), true) => Some(visible[p.saturating_sub(1)]),
                    (Some(p), false) => Some(visible[(p + 1).min(visible.len() - 1)]),
                };
                self.header_col = to.map(|i| cols[i].id);
            }
            Key::ENTER | Key::SPACE if !mods.ctrl() && !mods.shift() => {
                if let Some(i) = cur {
                    fx.sort = Some((cols[i].id, self.sort_dir(cols, cols[i].id)));
                }
            }
            Key::APPS if !mods.ctrl() => fx.header_menu = cur.map(|i| cols[i].id),
            Key::F10 if mods.shift() && !mods.ctrl() => fx.header_menu = cur.map(|i| cols[i].id),
            _ => {}
        }
        fx
    }

    /// A click on a header cell: left sorts, right opens the header menu.
    pub fn on_header_click(&mut self, col: ColId, button: MouseButton, cols: &[Column]) -> TableEffects {
        let mut fx = TableEffects::default();
        self.header_col = Some(col);
        match button {
            MouseButton::Left => fx.sort = Some((col, self.sort_dir(cols, col))),
            MouseButton::Right => fx.header_menu = Some(col),
            _ => {}
        }
        fx
    }

    // ---- column widths

    /// Grows or shrinks a column by `delta` logical px, never below its minimum. True when the width changed.
    pub fn resize(&mut self, col: &mut Column, delta: i32) -> bool {
        let w = (col.width + delta).max(min_w(col));
        let changed = w != col.width;
        col.width = w;
        changed
    }

    /// The double-click on a column's edge (and each column of Ctrl+Plus): the widest visible cell plus AUTOFIT_PAD.
    pub fn autofit(&mut self, col: &mut Column, widest_cell: i32) -> bool {
        let w = (widest_cell + AUTOFIT_PAD).max(min_w(col));
        let changed = w != col.width;
        col.width = w;
        changed
    }

    /// Lays the columns out in `total_w` logical px: hidden ones get 0, the others their width (not below the
    /// minimum), the flex one the rest (not below its own minimum).
    pub fn layout_widths(&mut self, cols: &[Column], total_w: i32) {
        self.widths.clear();
        let fixed: i32 = cols.iter().filter(|c| c.visible && !c.flex).map(|c| c.width.max(min_w(c))).sum();
        for c in cols {
            self.widths.push(if !c.visible {
                0
            } else if c.flex {
                (total_w - fixed).max(min_w(c))
            } else {
                c.width.max(min_w(c))
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellOut, TreeInfo};
    use std::borrow::Cow;

    /// Rows 0..n with the key = the row and the label "item N", plus an optional tree shape.
    struct Rows {
        keys: Vec<u64>,
        labels: Vec<String>,
        tree: Vec<TreeInfo>,
    }

    impl Rows {
        fn new(n: u64) -> Rows {
            Rows { keys: (0..n).collect(), labels: (0..n).map(|i| format!("item {i}")).collect(), tree: Vec::new() }
        }
        fn named(names: &[&str]) -> Rows {
            Rows {
                keys: (0..names.len() as u64).collect(),
                labels: names.iter().map(|s| s.to_string()).collect(),
                tree: Vec::new(),
            }
        }
    }

    impl ListModel for Rows {
        fn len(&self) -> usize {
            self.keys.len()
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(self.keys[row])
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            self.keys.iter().position(|&k| k == key.0)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Borrowed(&self.labels[row])
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Rows {
        fn cell(&self, _: usize, _: ColId, _: &mut CellOut) {}
        fn tree(&self, row: usize) -> Option<TreeInfo> {
            self.tree.get(row).copied()
        }
    }

    /// A million rows without storage.
    struct Huge;

    impl ListModel for Huge {
        fn len(&self) -> usize {
            1_000_000
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(row as u64)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            Some(key.0 as usize)
        }
        fn label(&self, _: usize) -> Cow<'_, str> {
            Cow::Borrowed("")
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Huge {
        fn cell(&self, _: usize, _: ColId, _: &mut CellOut) {}
    }

    fn state() -> TableState {
        let mut s = TableState::new(10, SelectionMode::Multi);
        s.viewport_h = 50; // 5 rows
        s
    }

    fn col(id: u16, w: i32, flex: bool) -> Column {
        Column {
            id: ColId(id),
            title: crate::node::Text::default(),
            width: w,
            min: 0,
            flex,
            align: crate::model::Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable: true,
            tip: None,
        }
    }

    fn press(s: &mut TableState, m: &Rows, sel: &mut Selection, key: Key, mods: Mods) -> TableEffects {
        s.on_key(key, mods, m, sel, 0)
    }

    fn selected(sel: &Selection, m: &Rows) -> Vec<usize> {
        let mut v = Vec::new();
        sel.for_each(m, &mut |r, _| v.push(r));
        v
    }

    fn at(s: &mut TableState, m: &Rows, sel: &mut Selection, row: u64) {
        s.focused = Some(RowKey(row));
        s.anchor = Some(RowKey(row));
        sel.click(RowKey(row));
        let _ = m;
    }

    #[test]
    fn up_and_down_move_with_the_selection() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        let fx = press(&mut s, &m, &mut sel, Key::DOWN, Mods::NONE);
        assert_eq!(
            (s.focused, sel.single(), fx.selection_changed, fx.focus),
            (Some(RowKey(0)), Some(RowKey(0)), true, Some(RowKey(0)))
        );
        press(&mut s, &m, &mut sel, Key::DOWN, Mods::NONE);
        assert_eq!((s.focused, sel.single()), (Some(RowKey(1)), Some(RowKey(1))));
        press(&mut s, &m, &mut sel, Key::UP, Mods::NONE);
        press(&mut s, &m, &mut sel, Key::UP, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(0)));
        for _ in 0..20 {
            press(&mut s, &m, &mut sel, Key::DOWN, Mods::NONE);
        }
        assert_eq!(s.focused, Some(RowKey(9)));
    }

    #[test]
    fn ctrl_up_and_ctrl_down_move_the_focus_only() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 3);
        let fx = press(&mut s, &m, &mut sel, Key::DOWN, Mods::CTRL);
        assert_eq!((s.focused, sel.single(), fx.selection_changed), (Some(RowKey(4)), Some(RowKey(3)), false));
        press(&mut s, &m, &mut sel, Key::UP, Mods::CTRL);
        press(&mut s, &m, &mut sel, Key::UP, Mods::CTRL);
        assert_eq!((s.focused, sel.single()), (Some(RowKey(2)), Some(RowKey(3))));
    }

    #[test]
    fn ctrl_space_toggles_the_focused_row() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 3);
        press(&mut s, &m, &mut sel, Key::DOWN, Mods::CTRL);
        let fx = press(&mut s, &m, &mut sel, Key::SPACE, Mods::CTRL);
        assert!(fx.selection_changed);
        assert_eq!(selected(&sel, &m), vec![3, 4]);
        press(&mut s, &m, &mut sel, Key::SPACE, Mods::CTRL);
        assert_eq!(selected(&sel, &m), vec![3]);
    }

    #[test]
    fn shift_up_and_shift_down_extend_from_the_anchor() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 4);
        press(&mut s, &m, &mut sel, Key::DOWN, Mods::SHIFT);
        press(&mut s, &m, &mut sel, Key::DOWN, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), vec![4, 5, 6]);
        press(&mut s, &m, &mut sel, Key::UP, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), vec![4, 5]);
        for _ in 0..3 {
            press(&mut s, &m, &mut sel, Key::UP, Mods::SHIFT);
        }
        assert_eq!(selected(&sel, &m), vec![2, 3, 4]);
        assert_eq!(s.anchor, Some(RowKey(4)));
    }

    #[test]
    fn shift_page_home_and_end_extend() {
        let m = Rows::new(30);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 10);
        press(&mut s, &m, &mut sel, Key::PAGE_DOWN, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), (10..=14).collect::<Vec<_>>());
        press(&mut s, &m, &mut sel, Key::PAGE_UP, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), vec![10]);
        press(&mut s, &m, &mut sel, Key::HOME, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), (0..=10).collect::<Vec<_>>());
        press(&mut s, &m, &mut sel, Key::END, Mods::SHIFT);
        assert_eq!(selected(&sel, &m), (10..30).collect::<Vec<_>>());
    }

    #[test]
    fn page_moves_visible_minus_one() {
        let m = Rows::new(30);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 0);
        press(&mut s, &m, &mut sel, Key::PAGE_DOWN, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(4)));
        press(&mut s, &m, &mut sel, Key::PAGE_DOWN, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(8)));
        press(&mut s, &m, &mut sel, Key::PAGE_UP, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(4)));
        s.viewport_h = 10; // one row visible: a page is still one step
        press(&mut s, &m, &mut sel, Key::PAGE_DOWN, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(5)));
    }

    #[test]
    fn home_and_end_jump_with_the_selection_and_ctrl_only_the_focus() {
        let m = Rows::new(30);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 10);
        press(&mut s, &m, &mut sel, Key::END, Mods::NONE);
        assert_eq!((s.focused, sel.single()), (Some(RowKey(29)), Some(RowKey(29))));
        assert_eq!(s.offset, 30 * 10 - 50);
        press(&mut s, &m, &mut sel, Key::HOME, Mods::CTRL);
        assert_eq!((s.focused, sel.single(), s.offset), (Some(RowKey(0)), Some(RowKey(29)), 0));
        press(&mut s, &m, &mut sel, Key::HOME, Mods::NONE);
        assert_eq!(sel.single(), Some(RowKey(0)));
    }

    #[test]
    fn ctrl_a_selects_all() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 2);
        let fx = press(&mut s, &m, &mut sel, Key(0x41), Mods::CTRL);
        assert!(fx.selection_changed);
        assert_eq!(sel.count(10), 10);
        assert_eq!(s.focused, Some(RowKey(2)));
        let mut single = state();
        single.mode = SelectionMode::Single;
        let mut one = Selection::None;
        assert!(!single.on_key(Key(0x41), Mods::CTRL, &m, &mut one, 0).selection_changed);
    }

    #[test]
    fn enter_activates_and_f2_renames() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        assert_eq!(press(&mut s, &m, &mut sel, Key::ENTER, Mods::NONE).activate, None);
        at(&mut s, &m, &mut sel, 6);
        assert_eq!(press(&mut s, &m, &mut sel, Key::ENTER, Mods::NONE).activate, Some(RowKey(6)));
        assert_eq!(press(&mut s, &m, &mut sel, Key::F2, Mods::NONE).rename, Some(RowKey(6)));
    }

    #[test]
    fn left_and_right_walk_a_tree() {
        let mut m = Rows::new(4);
        let t = |depth, expandable, expanded| TreeInfo { depth, expandable, expanded };
        m.tree = vec![t(0, true, false), t(0, true, true), t(1, false, false), t(0, false, false)];
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 0);
        assert_eq!(press(&mut s, &m, &mut sel, Key::RIGHT, Mods::NONE).expand, Some((RowKey(0), true)));
        at(&mut s, &m, &mut sel, 1);
        assert_eq!(press(&mut s, &m, &mut sel, Key::LEFT, Mods::NONE).expand, Some((RowKey(1), false)));
        press(&mut s, &m, &mut sel, Key::RIGHT, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(2)));
        press(&mut s, &m, &mut sel, Key::LEFT, Mods::NONE);
        assert_eq!(s.focused, Some(RowKey(1)));
        // A table without a tree ignores both.
        let flat = Rows::new(3);
        let fx = press(&mut s, &flat, &mut sel, Key::RIGHT, Mods::NONE);
        assert_eq!(fx, TableEffects::default());
    }

    #[test]
    fn ctrl_plus_asks_for_an_auto_fit_of_every_column() {
        let m = Rows::new(3);
        let (mut s, mut sel) = (state(), Selection::None);
        assert!(press(&mut s, &m, &mut sel, Key::OEM_PLUS, Mods::CTRL).autofit_all);
        assert!(press(&mut s, &m, &mut sel, Key::ADD, Mods::CTRL).autofit_all);
        assert!(!press(&mut s, &m, &mut sel, Key::OEM_PLUS, Mods::NONE).autofit_all);
        let mut c = col(0, 100, false);
        assert!(s.autofit(&mut c, 80));
        assert_eq!(c.width, 80 + AUTOFIT_PAD);
        assert!(s.autofit(&mut c, 0));
        assert_eq!(c.width, COL_MIN_W);
    }

    #[test]
    fn typeahead_wraps_and_resets() {
        let m = Rows::named(&["alpha", "beta", "Bravo", "charlie", "banana"]);
        let (mut s, mut sel) = (state(), Selection::None);
        s.on_text('b', &m, &mut sel, 100);
        assert_eq!(s.focused, Some(RowKey(1)));
        // The second 'b' within the window is the text "bb": no match, focus stays.
        s.on_text('b', &m, &mut sel, 300);
        assert_eq!(s.focused, Some(RowKey(1)));
        // After the reset a single 'b' moves to the next match from the focused row.
        s.on_text('b', &m, &mut sel, 300 + u64::from(TYPEAHEAD_RESET) + 1);
        assert_eq!(s.focused, Some(RowKey(2)));
        s.on_text('b', &m, &mut sel, 5000);
        assert_eq!(s.focused, Some(RowKey(4)));
        s.on_text('b', &m, &mut sel, 9000);
        assert_eq!(s.focused, Some(RowKey(1)), "wraps past the end");
        // Growing text stays on a row that still matches, case-insensitively.
        s.on_text('r', &m, &mut sel, 9100);
        assert_eq!(s.focused, Some(RowKey(2)));
        assert_eq!(sel.single(), Some(RowKey(2)));
    }

    #[test]
    fn right_click_selects_unselected_first() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        at(&mut s, &m, &mut sel, 1);
        sel.toggle(RowKey(2));
        let fx = s.on_click(Some(5), Mods::NONE, 1, MouseButton::Right, &m, &mut sel);
        assert!(fx.context && fx.selection_changed);
        assert_eq!((sel.single(), s.focused), (Some(RowKey(5)), Some(RowKey(5))));
        // On a selected row of a multi selection the selection is kept.
        sel.toggle(RowKey(6));
        let fx = s.on_click(Some(6), Mods::NONE, 1, MouseButton::Right, &m, &mut sel);
        assert!(fx.context && !fx.selection_changed);
        assert_eq!(selected(&sel, &m), vec![5, 6]);
        // The empty area asks for a context menu of no row.
        assert!(s.on_click(None, Mods::NONE, 1, MouseButton::Right, &m, &mut sel).context);
    }

    #[test]
    fn left_click_rows_follow_the_explorer_rules() {
        let m = Rows::new(10);
        let (mut s, mut sel) = (state(), Selection::None);
        s.on_click(Some(2), Mods::NONE, 1, MouseButton::Left, &m, &mut sel);
        s.on_click(Some(5), Mods::SHIFT, 1, MouseButton::Left, &m, &mut sel);
        assert_eq!(selected(&sel, &m), vec![2, 3, 4, 5]);
        s.on_click(Some(8), Mods::CTRL, 1, MouseButton::Left, &m, &mut sel);
        assert_eq!(selected(&sel, &m), vec![2, 3, 4, 5, 8]);
        let fx = s.on_click(Some(0), Mods::NONE, 2, MouseButton::Left, &m, &mut sel);
        assert_eq!((fx.activate, sel.single()), (Some(RowKey(0)), Some(RowKey(0))));
        assert!(s.on_click(None, Mods::NONE, 1, MouseButton::Left, &m, &mut sel).selection_changed);
        assert!(sel.is_empty());
    }

    #[test]
    fn header_left_and_right_move_between_visible_columns() {
        let mut cols = vec![col(0, 100, false), col(1, 100, false), col(2, 100, false)];
        cols[1].visible = false;
        let mut s = state();
        s.on_header_key(Key::RIGHT, Mods::NONE, &mut cols);
        assert_eq!(s.header_col, Some(ColId(0)));
        s.on_header_key(Key::RIGHT, Mods::NONE, &mut cols);
        assert_eq!(s.header_col, Some(ColId(2)));
        s.on_header_key(Key::RIGHT, Mods::NONE, &mut cols);
        assert_eq!(s.header_col, Some(ColId(2)));
        s.on_header_key(Key::LEFT, Mods::NONE, &mut cols);
        assert_eq!(s.header_col, Some(ColId(0)));
    }

    #[test]
    fn header_enter_and_space_sort() {
        let mut cols = vec![col(0, 100, false), col(1, 100, false)];
        cols[1].first_sort = SortDir::Desc;
        let mut s = state();
        s.header_col = Some(ColId(1));
        assert_eq!(s.on_header_key(Key::ENTER, Mods::NONE, &mut cols).sort, Some((ColId(1), SortDir::Desc)));
        cols[1].sort = Some(SortDir::Desc);
        assert_eq!(s.on_header_key(Key::SPACE, Mods::NONE, &mut cols).sort, Some((ColId(1), SortDir::Asc)));
        cols[1].sort = Some(SortDir::Asc);
        assert_eq!(s.sort_dir(&cols, ColId(1)), SortDir::Desc);
        assert_eq!(s.sort_dir(&cols, ColId(0)), SortDir::Asc);
    }

    #[test]
    fn header_shift_left_and_right_resize_by_eight() {
        let mut cols = vec![col(0, 100, false)];
        let mut s = state();
        s.header_col = Some(ColId(0));
        let fx = s.on_header_key(Key::RIGHT, Mods::SHIFT, &mut cols);
        assert!(fx.columns_changed);
        assert_eq!(cols[0].width, 108);
        s.on_header_key(Key::LEFT, Mods::SHIFT, &mut cols);
        s.on_header_key(Key::LEFT, Mods::SHIFT, &mut cols);
        assert_eq!(cols[0].width, 92);
        cols[0].width = COL_MIN_W + 3;
        s.on_header_key(Key::LEFT, Mods::SHIFT, &mut cols);
        assert_eq!(cols[0].width, COL_MIN_W);
        assert!(!s.on_header_key(Key::LEFT, Mods::SHIFT, &mut cols).columns_changed);
    }

    #[test]
    fn header_menu_key_and_right_click() {
        let mut cols = vec![col(0, 100, false), col(1, 100, false)];
        let mut s = state();
        s.header_col = Some(ColId(1));
        assert_eq!(s.on_header_key(Key::APPS, Mods::NONE, &mut cols).header_menu, Some(ColId(1)));
        assert_eq!(s.on_header_key(Key::F10, Mods::SHIFT, &mut cols).header_menu, Some(ColId(1)));
        assert_eq!(s.on_header_key(Key::F10, Mods::NONE, &mut cols).header_menu, None);
        let fx = s.on_header_click(ColId(0), MouseButton::Right, &cols);
        assert_eq!((fx.header_menu, fx.sort), (Some(ColId(0)), None));
        let fx = s.on_header_click(ColId(0), MouseButton::Left, &cols);
        assert_eq!((fx.header_menu, fx.sort), (None, Some((ColId(0), SortDir::Asc))));
        assert_eq!(s.header_col, Some(ColId(0)));
    }

    #[test]
    fn keep_anchor_holds_the_focused_row_y() {
        let mut s = state();
        s.offset = 70;
        let old_y = s.row_y(10);
        assert_eq!(old_y, 30);
        // Three rows appear above: the focused row is now row 13.
        s.keep_anchor(old_y, 13, 100);
        assert_eq!(s.row_y(13), old_y);
        // Clamped at the top and at the bottom.
        s.keep_anchor(old_y, 1, 100);
        assert_eq!(s.offset, 0);
        s.keep_anchor(-500, 99, 100);
        assert_eq!(s.offset, 100 * 10 - 50);
    }

    #[test]
    fn flex_column_never_below_min() {
        let mut flex = col(1, 0, true);
        flex.min = 120;
        let mut hidden = col(2, 90, false);
        hidden.visible = false;
        let cols = vec![col(0, 200, false), flex, hidden, col(3, 10, false)];
        let mut s = state();
        s.layout_widths(&cols, 600);
        assert_eq!(s.widths, vec![200, 600 - 200 - COL_MIN_W, 0, COL_MIN_W]);
        s.layout_widths(&cols, 300);
        assert_eq!(s.widths, vec![200, 120, 0, COL_MIN_W]);
    }

    #[test]
    fn offset_is_i64_at_1_000_000_rows() {
        let mut s = TableState::new(4000, SelectionMode::Multi);
        s.viewport_h = 800;
        let mut sel = Selection::None;
        let fx = s.on_key(Key::END, Mods::NONE, &Huge, &mut sel, 0);
        assert_eq!(fx.focus, Some(RowKey(999_999)));
        assert_eq!(s.offset, 1_000_000i64 * 4000 - 800);
        assert!(s.offset > i64::from(i32::MAX));
        assert_eq!(s.visible(1_000_000), 999_999..1_000_000);
        s.offset = 0;
        assert_eq!(s.visible(1_000_000), 0..1);
        s.keep_anchor(0, 500_000, 1_000_000);
        assert_eq!(s.offset, 2_000_000_000);
    }

    #[test]
    fn visible_covers_partial_rows() {
        let mut s = state();
        s.offset = 15;
        assert_eq!(s.visible(100), 1..7);
        assert_eq!(s.visible(4), 1..4);
        assert_eq!(s.visible(0), 0..0);
    }
}
