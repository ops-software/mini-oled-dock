//! VirtualTable and ListTable (F-K2a, P2-kit 2.8): the frozen types; filled by K2a.5 and K2a.6 (KC1), rename by K2b.7.
//! Rows are never widgets: the model writes cells (TableModel::cell) and only visible rows are laid out.

use std::borrow::Cow;
use std::ops::Range;

use super::text_field::FieldKind;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::model::{
    CellKind, CellOut, ColId, Column, ListModel, RowKey, ScrollTo, Selection, SelectionMode, TableModel, TableOut,
    TableStyle,
};
use crate::node::WidgetId;
use crate::screen::Widget;

/// A virtual table over a model.
#[allow(dead_code)] // The fields are read by K2a.5 and K2a.6.
pub struct VirtualTable<M: TableModel> {
    id: WidgetId,
    model: M,
    columns: Vec<Column>,
    mode: SelectionMode,
    style: TableStyle,
    selection: Selection,
    focused: Option<RowKey>,
    viewport: RectP,
}

impl<M: TableModel> VirtualTable<M> {
    pub fn new(
        id: WidgetId,
        model: M,
        columns: Vec<Column>,
        mode: SelectionMode,
        style: TableStyle,
    ) -> VirtualTable<M> {
        VirtualTable {
            id,
            model,
            columns,
            mode,
            style,
            selection: Selection::None,
            focused: None,
            viewport: RectP::EMPTY,
        }
    }

    pub fn model(&self) -> &M {
        &self.model
    }

    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// After any model mutation; keeps the focused row's y. Filled by K2a.6; until then the selection drops the keys
    /// that left the model.
    pub fn model_changed(&mut self, cx: &mut EventCx, keep_anchor: bool) {
        let _ = (cx, keep_anchor);
        self.selection.retain_present(&self.model);
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn set_selection(&mut self, cx: &mut EventCx, s: Selection) {
        let _ = cx;
        self.selection = s;
    }

    pub fn focused(&self) -> Option<RowKey> {
        self.focused
    }

    /// Focuses the row of `k` and scrolls it into view (K2a.6).
    pub fn set_focused(&mut self, cx: &mut EventCx, k: RowKey, scroll: ScrollTo) {
        let _ = (cx, scroll);
        self.focused = Some(k);
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn set_columns(&mut self, cx: &mut EventCx, c: Vec<Column>) {
        let _ = cx;
        self.columns = c;
    }

    /// Opens the rename field over a cell (K2b.7); until then false.
    pub fn begin_rename(&mut self, cx: &mut EventCx, k: RowKey, col: ColId, kind: FieldKind) -> bool {
        let _ = (cx, k, col, kind);
        false
    }

    /// The model rows laid out now (K2a.5); until then none.
    pub fn visible_rows(&self) -> Range<usize> {
        0..0
    }

    /// A visible row's rect (K2a.5); until then None.
    pub fn row_rect(&self, row: usize) -> Option<RectP> {
        let _ = row;
        None
    }

    /// The rows' area (K2a.5); until then empty.
    pub fn viewport(&self) -> RectP {
        self.viewport
    }
}

impl<M: TableModel> Widget for VirtualTable<M> {
    type Out = TableOut;

    /// Filled by K2a.5 and K2a.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2a.5 and K2a.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2a.5 and K2a.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<TableOut> {
        let _ = (cx, ev);
        None
    }
}

/// A one-column table (ColId(0) = the label) over a list model.
pub struct ListTable<M: ListModel>(pub M);

impl<M: ListModel> ListModel for ListTable<M> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn key(&self, row: usize) -> RowKey {
        self.0.key(row)
    }

    fn row_of(&self, key: RowKey) -> Option<usize> {
        self.0.row_of(key)
    }

    fn label(&self, row: usize) -> Cow<'_, str> {
        self.0.label(row)
    }

    fn generation(&self) -> u64 {
        self.0.generation()
    }
}

impl<M: ListModel> TableModel for ListTable<M> {
    /// The label cell.
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        let _ = col;
        out.clear();
        out.set_text(CellKind::Text, &self.0.label(row));
    }
}

/// A virtual list: a table with one label column.
pub type VirtualList<M> = VirtualTable<ListTable<M>>;
