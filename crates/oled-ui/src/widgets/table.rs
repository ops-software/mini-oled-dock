//! VirtualTable and ListTable (F-K2a, P2-kit 2.8): painting (K2a.5) and input, the header menu and the accessibility
//! nodes (K2a.6); rename by K2b.7. Rows are never widgets: the model writes cells (TableModel::cell) and only the visible
//! rows are laid out and painted.

use std::borrow::Cow;
use std::ops::Range;

use super::header::{self, HeaderCell};
use super::paint::fg_ink;
use super::table_state::TableState;
use super::text_field::{FieldKind, FieldOut, TextField};
use crate::cx::{Anchor, EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, Mods, MouseButton, TimerId, WheelDelta};
use crate::geom::{PointP, RectP, Scale};
use crate::glyph::Glyph;
use crate::model::{
    Align, Badge, CellKind, CellOut, ColId, Column, FgRole, HeatStep, ListModel, RowKey, RowMotion, RowOut, ScrollTo,
    Selection, SelectionLook, SelectionMode, TableModel, TableOut, TableStyle, TreeInfo,
};
use crate::node::{Actions, Cursor, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// The wheel animation's timer (TimerId::kit slot).
const TIMER_SCROLL: u8 = 1;
/// The rename error line's timer (TimerId::kit slot).
const TIMER_RENAME_ERR: u8 = 2;
/// One animation frame.
const FRAME_MS: u32 = 16;
/// How long a refused character's error line stays, ms.
const RENAME_ERR_MS: u32 = 3000;
/// A slow second click on a selected name starts a rename between these two delays after the first, ms.
const SLOW_CLICK_MS: std::ops::RangeInclusive<u64> = 500..=1500;
/// The rename field's widget id is the table's with this bit flipped (its timers stay apart from the table's).
const RENAME_ID_BIT: u32 = 0x40_0000;

/// The rename in progress: the row, the column (index in the columns) and the field over the cell.
struct Rename {
    key: RowKey,
    col: usize,
    field: TextField,
}

/// A wheel scroll in flight.
#[derive(Clone, Copy, Debug)]
struct ScrollAnim {
    from: i64,
    to: i64,
    start_ms: u64,
}

/// A column resize in flight (the drag of a handle).
#[derive(Clone, Copy, Debug)]
struct Resize {
    col: usize,
    start_x: i32,
    start_w: i32,
}

/// What a node of the table is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Table,
    Row(usize),
    Header(usize),
    Handle(usize),
}

/// A virtual table over a model.
pub struct VirtualTable<M: TableModel> {
    id: WidgetId,
    model: M,
    columns: Vec<Column>,
    style: TableStyle,
    scale: Scale,
    /// The style's row inset in physical px, and the row band's x range (set at layout).
    inset_px: i32,
    selection: Selection,
    state: TableState,
    label: Option<Text>,
    /// The whole table (header and rows), and its parts.
    rect: RectP,
    header: RectP,
    viewport: RectP,
    cells: Vec<HeaderCell>,
    /// The rows that touch the viewport, and those that have nodes (one screen more each way).
    visible: Range<usize>,
    nodes: Range<usize>,
    /// The row the focused key had at the last layout (model_changed keeps its y).
    last_focus_row: Option<usize>,
    /// The column the open header menu belongs to.
    menu_col: Option<usize>,
    scroll: Option<ScrollAnim>,
    resize: Option<Resize>,
    /// The row whose action was first shown, and when.
    action_seen: Option<(RowKey, u64)>,
    extra: Vec<TableOut>,
    /// The rename field over a cell, the text a refused character shows, and the last plain click (for the slow one).
    rename: Option<Box<Rename>>,
    refusal: Option<Text>,
    last_click: Option<(RowKey, u64)>,
    /// The row height set by set_row_height, logical px; None = the token.
    row_h_logical: Option<i32>,
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
            style,
            scale: Scale { dpi: 96 },
            inset_px: 0,
            selection: Selection::None,
            state: TableState::new(28, mode),
            label: None,
            rect: RectP::EMPTY,
            header: RectP::EMPTY,
            viewport: RectP::EMPTY,
            cells: Vec::new(),
            visible: 0..0,
            nodes: 0..0,
            last_focus_row: None,
            menu_col: None,
            scroll: None,
            resize: None,
            action_seen: None,
            extra: Vec::new(),
            rename: None,
            refusal: None,
            last_click: None,
            row_h_logical: None,
        }
    }

    pub fn model(&self) -> &M {
        &self.model
    }

    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// The table with this selection (a page builds its tables selected).
    pub fn with_selection(mut self, s: Selection) -> VirtualTable<M> {
        self.selection = s;
        self
    }

    /// The table with this row focused (no scrolling: a page builds its tables at the top).
    pub fn with_focused(mut self, k: RowKey) -> VirtualTable<M> {
        self.state.focused = Some(k);
        self
    }

    /// The table's accessible name (default: the first visible column's title).
    pub fn set_label(&mut self, t: Text) {
        self.label = Some(t);
    }

    /// The outputs of the last event beyond the one it returned (an event can change the selection and ask for a context
    /// menu at once); the screen drains them after each event it forwards.
    pub fn take_extra(&mut self) -> Vec<TableOut> {
        std::mem::take(&mut self.extra)
    }

    /// After any model mutation: the selection drops the keys that left the model; the focused row keeps its y
    /// (keep_anchor) wherever the model put it; when the table holds the focus, the focus node follows the row.
    pub fn model_changed(&mut self, cx: &mut EventCx, keep_anchor: bool) {
        self.selection.retain_present(&self.model);
        let len = self.model.len();
        let key = self.state.focused;
        let new_row = key.and_then(|k| self.model.row_of(k));
        if key.is_some() && new_row.is_none() {
            // The focused row is gone: focus the row now at its old place, or the table itself.
            let at = self.last_focus_row.unwrap_or(0).min(len.saturating_sub(1));
            self.state.focused = (len > 0).then(|| self.model.key(at));
        }
        let new_row = self.state.focused.and_then(|k| self.model.row_of(k));
        if let (true, Some(old), Some(new)) = (keep_anchor, self.last_focus_row, new_row) {
            let old_y = self.state.row_y(old);
            self.state.keep_anchor(old_y, new, len);
        } else {
            self.state.clamp_offset(len);
        }
        self.last_focus_row = new_row;
        if cx
            .focused()
            .is_some_and(|n| n.owner() == self.id && !matches!(self.part(n), Part::Header(_) | Part::Handle(_)))
        {
            let to = match new_row {
                Some(r) => self.row_id(r),
                None => self.table_id(),
            };
            cx.set_focus(to);
        }
        self.dirty(cx);
    }

    /// Sets the row height (logical px; None = tokens.size.row_table). The focused row keeps its y; without a focused
    /// row the first row in view stays put.
    pub fn set_row_height(&mut self, cx: &mut EventCx, logical: Option<i32>) {
        if self.row_h_logical == logical {
            return;
        }
        self.row_h_logical = logical;
        let new_h = cx.px(logical.unwrap_or(cx.tokens().size.row_table)).max(1);
        let old_h = self.state.row_h.max(1);
        let len = self.model.len();
        let focus_row = self.state.focused.and_then(|k| self.model.row_of(k));
        let old_y = focus_row.map(|r| self.state.row_y(r));
        self.state.row_h = new_h;
        match (focus_row, old_y) {
            (Some(row), Some(y)) => self.state.keep_anchor(y, row, len),
            _ => {
                self.state.offset = self.state.offset * i64::from(new_h) / i64::from(old_h);
                self.state.clamp_offset(len);
            }
        }
        self.dirty(cx);
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn set_selection(&mut self, cx: &mut EventCx, s: Selection) {
        self.selection = s;
        self.dirty(cx);
    }

    pub fn focused(&self) -> Option<RowKey> {
        self.state.focused
    }

    /// Moves the anchor of the next Shift+arrow or Shift+click range (None: the focused row).
    pub fn set_anchor(&mut self, k: Option<RowKey>) {
        self.state.anchor = k;
    }

    /// Focuses the row of `k` and scrolls it into view.
    pub fn set_focused(&mut self, cx: &mut EventCx, k: RowKey, scroll: ScrollTo) {
        let Some(row) = self.model.row_of(k) else { return };
        self.state.focused = Some(k);
        self.state.scroll_to_row(row, scroll, self.model.len());
        self.scroll = None;
        self.last_focus_row = Some(row);
        if cx.focused().is_some_and(|n| n.owner() == self.id) {
            cx.set_focus(self.row_id(row));
        }
        self.dirty(cx);
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn set_columns(&mut self, cx: &mut EventCx, c: Vec<Column>) {
        self.columns = c;
        self.dirty(cx);
    }

    /// Opens the rename field over a cell: the row scrolls into view and a field of `kind` sits exactly over the cell's
    /// text (same x, same baseline) holding its text, the stem selected for a Rename kind. False when the row or the
    /// column is not there. The field answers with RenameCommitted (Enter, focus loss, Tab), RenameCancelled (Esc).
    pub fn begin_rename(&mut self, cx: &mut EventCx, k: RowKey, col: ColId, kind: FieldKind) -> bool {
        let Some(row) = self.model.row_of(k) else { return false };
        let Some(ci) = self.columns.iter().position(|c| c.id == col && c.visible) else { return false };
        self.state.scroll_to_row(row, ScrollTo::Nearest, self.model.len());
        self.scroll = None;
        let mut out = CellOut::default();
        self.model.cell(row, col, &mut out);
        let mut field = TextField::new(self.rename_id(), kind);
        field.set_text(cx, &out.text, true);
        let node = field.node_id();
        self.state.focused = Some(k);
        self.last_focus_row = Some(row);
        self.rename = Some(Box::new(Rename { key: k, col: ci, field }));
        self.dirty(cx);
        cx.set_focus(node);
        true
    }

    /// True while a rename field is open.
    pub fn is_renaming(&self) -> bool {
        self.rename.is_some()
    }

    /// The open rename field, for the app to set its filter (files: \ / : * ? " < > |) and its limit.
    pub fn rename_field_mut(&mut self) -> Option<&mut TextField> {
        self.rename.as_mut().map(|r| &mut r.field)
    }

    /// The line a refused character shows under the rename field for 3 s (files: "A name can't contain ..."); None:
    /// no line.
    pub fn set_rename_refusal(&mut self, text: Option<Text>) {
        self.refusal = text;
    }

    fn rename_id(&self) -> WidgetId {
        WidgetId(self.id.0 ^ RENAME_ID_BIT)
    }

    /// The model rows laid out now (the ones that touch the viewport).
    pub fn visible_rows(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// A visible row's plate, physical px; None for a row outside the viewport.
    pub fn row_rect(&self, row: usize) -> Option<RectP> {
        self.visible.contains(&row).then(|| self.plate(row))
    }

    /// The rows' area.
    pub fn viewport(&self) -> RectP {
        self.viewport
    }

    // ---- geometry

    fn table_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn row_id(&self, row: usize) -> NodeId {
        NodeId::new(self.id, row as u32 + 1)
    }

    fn part(&self, n: NodeId) -> Part {
        match n.slot() {
            0 => Part::Table,
            s => match header::slot_col(s) {
                Some(Ok(c)) => Part::Header(c),
                Some(Err(c)) => Part::Handle(c),
                None => Part::Row(s as usize - 1),
            },
        }
    }

    /// A row's plate: the row's band less the style's inset.
    fn plate(&self, row: usize) -> RectP {
        let inset = self.inset_px;
        let y = self.viewport.t as i64 + self.state.row_y(row);
        let t = y.clamp(-1_000_000_000, 1_000_000_000) as i32;
        RectP { l: self.rect.l + inset, t, r: self.rect.r - inset, b: t + self.state.row_h }
    }

    /// Marks the table dirty after a change that moves nodes or pixels.
    fn dirty(&self, cx: &mut EventCx) {
        cx.relayout();
        cx.damage(self.rect);
    }
}

impl<M: TableModel> VirtualTable<M> {
    fn first_col(&self) -> Option<usize> {
        self.cells.first().map(|c| c.col)
    }

    fn cell_rect(&self, col: usize) -> Option<RectP> {
        self.cells.iter().find(|c| c.col == col).map(|c| c.rect)
    }

    /// The chevron's 16 px box of a tree row.
    fn chevron_box(&self, plate: RectP, depth: u16) -> RectP {
        let s = self.scale;
        let x = self.rect.l + s.px(self.style.row_inset + 4) + s.px(16) * i32::from(depth);
        RectP::wh(x, plate.t + (plate.h() - s.px(16)) / 2, s.px(16), s.px(16))
    }

    /// The hover action's rect in the note slot of the first column.
    fn action_rect(&self, plate: RectP, label_w: i32) -> Option<RectP> {
        let cell = self.cell_rect(self.first_col()?)?;
        let s = self.scale;
        let (h, w) = (s.px(22), label_w + s.px(16));
        Some(RectP::wh(cell.r - s.px(12) - w, plate.t + (plate.h() - h) / 2, w, h))
    }

    /// The x where the first column's name text starts in `row` (paint_cell's `l` for the first column).
    fn name_left(&self, row: usize, cell: RectP, out: &CellOut) -> i32 {
        let s = self.scale;
        let tree = self.model.tree(row);
        let indent = tree.map_or(0, |t| s.px(16) * i32::from(t.depth));
        let icon_l = self.rect.l + s.px(self.style.icon_x) + indent;
        if out.icon.is_some() {
            icon_l + s.px(16) + s.px(8)
        } else if tree.is_some() {
            icon_l
        } else {
            cell.l + s.px(12)
        }
    }

    /// Where the rename field goes: its text starts exactly where the cell's text does (the field's own text inset is
    /// 8 px), and it is two px inside the row's plate above and below, so the baselines are the same.
    fn rename_rect(&self, row: usize, ci: usize) -> Option<RectP> {
        let cell = self.cells.iter().find(|c| c.col == ci)?.rect;
        let plate = self.plate(row);
        let s = self.scale;
        let mut out = CellOut::default();
        self.model.cell(row, self.columns.get(ci)?.id, &mut out);
        let text_l = if self.first_col() == Some(ci) { self.name_left(row, cell, &out) } else { cell.l + s.px(12) };
        let l = text_l - s.px(8);
        let r = (cell.r - s.px(12) + s.px(8)).max(l + s.px(40));
        Some(RectP { l, t: plate.t + s.px(2), r, b: plate.b - s.px(2) })
    }

    /// The first column's cell of `row`, for its hover action.
    fn action_of(&self, row: usize, out: &mut CellOut) -> Option<crate::model::RowAction> {
        let col = self.columns.get(self.first_col()?)?.id;
        out.clear();
        self.model.cell(row, col, out);
        out.action.take()
    }

    fn focus_center(&self) -> PointP {
        let r = self.state.focused.and_then(|k| self.model.row_of(k)).map_or(self.viewport, |r| self.plate(r));
        PointP { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
    }

    /// The index in the columns Vec of the header's current cell.
    fn header_index(&self) -> Option<usize> {
        let id = self.state.header_col?;
        let at = self.columns.iter().position(|c| c.id == id)?;
        self.columns[at].visible.then_some(at)
    }

    fn table_focused(&self, cx: &PaintCx) -> bool {
        cx.disp.state.focused.is_some_and(|n| n.owner() == self.id)
    }

    // ---- painting

    fn paint_rows(&self, cx: &mut PaintCx, out: &mut CellOut) {
        let t = cx.tokens();
        let c = t.color.clone();
        let hc = t.high_contrast;
        let radius = cx.px(t.radius.s);
        let active = cx.window_active() && self.table_focused(cx);
        let rows: Vec<usize> = self.visible.clone().collect();
        let sel: Vec<bool> = rows.iter().map(|r| self.selection.contains(self.model.key(*r))).collect();
        // The motion of each visible row, asked once; a moving row is never part of a merged run (its plate moves alone).
        let motions: Vec<RowMotion> = rows.iter().map(|r| self.model.motion(*r)).collect();
        let run_sel: Vec<bool> = sel.iter().zip(&motions).map(|(s, m)| *s && *m == RowMotion::NONE).collect();
        let hot_of = |cx: &PaintCx, r: usize| cx.hot(self.row_id(r));
        let merged = self.style.selection == SelectionLook::Plate && self.style.merge_runs;
        if merged {
            let mut i = 0;
            while i < rows.len() {
                if !run_sel[i] {
                    i += 1;
                    continue;
                }
                let mut j = i;
                while j + 1 < rows.len() && run_sel[j + 1] {
                    j += 1;
                }
                // The run is one plate at the active or inactive fill; a hovered row is its own piece at the hot fill
                // (hot wins over inactive), square on the sides that border the rest of the run.
                let base = if active { c.fill_row_selected } else { c.fill_row_selected_inactive };
                let mut k = i;
                while k <= j {
                    let hot = hot_of(cx, rows[k]);
                    let mut m = k;
                    while m < j && hot_of(cx, rows[m + 1]) == hot {
                        m += 1;
                    }
                    let rect = self.plate(rows[k]).union(self.plate(rows[m]));
                    // Past the clip the rounded corners of a taller rect fall outside it: square.
                    let (up, down) = (if k > i { radius } else { 0 }, if m < j { radius } else { 0 });
                    let ink = if hot { c.fill_row_selected_hot } else { base };
                    cx.with_clip(rect, &mut |cx| {
                        cx.fill(RectP { t: rect.t - up, b: rect.b + down, ..rect }, radius, ink);
                    });
                    k = m + 1;
                }
                i = j + 1;
            }
        }
        for (i, &row) in rows.iter().enumerate() {
            let motion = motions[i];
            let plate = {
                let p = self.plate(row);
                let dy = cx.px(i32::from(motion.dy));
                RectP { t: p.t + dy, b: p.b + dy, ..p }
            };
            let op = motion.opacity;
            let fade = |i: Ink| if op == 255 { i } else { i.faded(f32::from(op) / 255.0) };
            let (hot, pressed) = (hot_of(cx, row), cx.pressed(self.row_id(row)));
            let selected = sel[i];
            let mut row_out = RowOut::default();
            self.model.row(row, &mut row_out);
            if selected && (!merged || motion != RowMotion::NONE) {
                match self.style.selection {
                    SelectionLook::Bar => {
                        let ink = if hot {
                            c.fill_press
                        } else if !active {
                            c.fill_hover
                        } else {
                            c.fill_control_hot
                        };
                        cx.fill(plate, radius, fade(ink));
                        if active {
                            let (w, h) = (cx.px(2), cx.px(12));
                            let bar = RectP::wh(self.rect.l + cx.px(10), plate.t + (plate.h() - h) / 2, w, h);
                            cx.fill(bar, w / 2, fade(c.fg_primary));
                        }
                    }
                    SelectionLook::Plate => {
                        let ink = if hot {
                            c.fill_row_selected_hot
                        } else if !active {
                            c.fill_row_selected_inactive
                        } else {
                            c.fill_row_selected
                        };
                        cx.fill(plate, radius, fade(ink));
                    }
                }
            } else if !selected && !row_out.section {
                if pressed {
                    cx.fill(plate, radius, fade(c.fill_press));
                } else if hot {
                    cx.fill(plate, radius, fade(c.fill_hover));
                    if self.style.hover_rim {
                        cx.stroke(plate, radius, 1, fade(c.rim_hover));
                    }
                }
            }
            if row_out.fresh {
                let (w, m) = (cx.px(2), cx.px(6));
                cx.fill(RectP { l: plate.l, t: plate.t + m, r: plate.l + w, b: plate.b - m }, 0, fade(c.rim_focus));
            }
            let plate_text_hot = match self.style.selection {
                SelectionLook::Plate => selected,
                SelectionLook::Bar => selected || hot,
            };
            // The secondary cells and the extension go hot on a selected plate (not on a hovered bar row).
            let sec_hot = selected && self.style.selection == SelectionLook::Plate;
            // Under high contrast a selected, hovered or pressed row is the highlight colour and its text the highlight
            // text (a section row has no plate).
            let flat_hot = hc && (selected || ((hot || pressed) && !row_out.section));
            for cell in &self.cells {
                if !cx.visible(RectP { l: cell.rect.l, t: plate.t, r: cell.rect.r, b: plate.b }) {
                    continue;
                }
                out.clear();
                self.model.cell(row, self.columns[cell.col].id, out);
                let looks = [plate_text_hot, sec_hot, flat_hot];
                self.paint_cell(cx, row, cell, plate, out, &row_out, looks, hot, hc, &c, op);
            }
            if cx.focused(self.row_id(row)) {
                if op == 255 {
                    cx.focus_ring(plate, radius, true);
                } else if cx.focus_visible() && cx.window_active() {
                    let ring = crate::geom::focus_ring_inset(plate, radius, cx.scale());
                    cx.stroke(ring.rect, ring.radius, ring.width, fade(c.rim_focus));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_cell(
        &self,
        cx: &mut PaintCx,
        row: usize,
        cell: &HeaderCell,
        plate: RectP,
        out: &CellOut,
        row_out: &RowOut,
        [plate_text_hot, sec_hot, flat_hot]: [bool; 3],
        hot: bool,
        hc: bool,
        c: &crate::theme::ColorTokens,
        op: u8,
    ) {
        let s = cx.scale();
        let fade = |i: Ink| if op == 255 { i } else { i.faded(f32::from(op) / 255.0) };
        let fg = if row_out.section {
            FgRole::Secondary
        } else if row_out.ghost {
            FgRole::Disabled
        } else if out.cut {
            FgRole::Tertiary
        } else if plate_text_hot && out.fg == FgRole::Primary {
            FgRole::Hot
        } else {
            out.fg
        };
        let ink = if flat_hot {
            c.fg_hot
        } else if sec_hot && fg == FgRole::Secondary {
            c.fg_secondary_hot
        } else {
            fg_ink(fg, cx.tokens())
        };
        let unit_ink = if flat_hot {
            c.fg_hot
        } else if sec_hot {
            c.fg_secondary_hot
        } else {
            c.fg_secondary
        };
        let (ink, unit_ink) = (fade(ink), fade(unit_ink));
        let mut role = if row_out.section { TypeRole::Section } else { out.role };
        let heat = out.heat.map(|HeatStep(n)| n.clamp(1, 4));
        let col_rect = RectP { l: cell.rect.l, t: plate.t, r: cell.rect.r, b: plate.b };
        if let Some(n) = heat {
            if hc {
                let w = s.px(2);
                let bar = RectP { l: col_rect.l, t: col_rect.t + 1, r: col_rect.l + w, b: col_rect.b - 1 };
                cx.fill(bar, 0, fade(c.rim_focus));
                if n >= 3 {
                    role = TypeRole::BodyStrong;
                }
            } else {
                cx.fill(col_rect.inset(1, 1), 0, fade(c.heat[usize::from(n) - 1]));
            }
        }
        let pad = s.px(12);
        let (mut l, mut r) = (cell.rect.l + pad, cell.rect.r - pad);
        let line_rect = |l: i32, r: i32| RectP { l, t: plate.t, r, b: plate.b };
        let is_first = self.first_col() == Some(cell.col);
        let mut name_badge = None;
        if is_first {
            let tree: Option<TreeInfo> = self.model.tree(row);
            let indent = tree.map_or(0, |t| s.px(16) * i32::from(t.depth));
            if let Some(t) = tree.filter(|t| t.expandable) {
                let g = if t.expanded { Glyph::ChevronDown } else { Glyph::ChevronRight };
                let b = self.chevron_box(plate, t.depth);
                let gl = RectP::wh(b.l + s.px(2), b.t + s.px(2), s.px(12), s.px(12));
                cx.glyph(gl, g, fade(c.fg_secondary));
            }
            let icon_l = self.rect.l + s.px(self.style.icon_x) + indent;
            let size = s.px(16);
            if let Some(ic) = out.icon {
                if let Some(bmp) = self.model.icon(ic, size) {
                    let at = PointP { x: icon_l, y: plate.t + (plate.h() - size) / 2 };
                    let icon_op = if out.cut || out.dim_icon { 128 } else { 255 };
                    cx.image(bmp, at, size, (icon_op * u32::from(op) + 127) / 255);
                    if out.badge == Some(Badge::Link) {
                        let b = s.px(8);
                        cx.glyph(RectP::wh(icon_l, at.y + size - b, b, b), Glyph::Link, fade(c.fg_primary));
                    }
                }
                l = icon_l + size + s.px(8);
            } else if tree.is_some() {
                l = icon_l;
            }
            // The note slot, from the right: the error or lock mark, then the hover action or the note.
            let mark = |cx: &mut PaintCx, g: Glyph, ink: Ink, r: &mut i32| {
                let b = s.px(12);
                cx.glyph(RectP::wh(*r - b, plate.t + (plate.h() - b) / 2, b, b), g, ink);
                *r -= b + s.px(6);
            };
            if row_out.error || out.badge == Some(Badge::Error) {
                mark(cx, Glyph::Error, fade(c.accent_danger), &mut r);
            } else if let Some(b) = out.badge.filter(|b| matches!(b, Badge::Shield | Badge::Lock)) {
                mark(cx, if b == Badge::Shield { Glyph::Shield } else { Glyph::Lock }, fade(c.fg_secondary), &mut r);
            }
            let action = out.action.as_ref().filter(|_| hot);
            if let Some(a) = action {
                let font = cx.res.text.font(TypeRole::Caption, None, s, cx.tokens().text_scale);
                let w = cx.res.text.width(&a.label, font);
                if let Some(rect) = self.action_rect(plate, w) {
                    let over = cx.disp.state.last_pointer.is_some_and(|p| rect.contains(p));
                    let (plate_ink, text_ink) =
                        if over { (c.fill_control_hot, c.fg_hot) } else { (c.fill_control, c.fg_secondary) };
                    let (plate_ink, text_ink) = (fade(plate_ink), fade(text_ink));
                    cx.fill(rect, cx.px(cx.tokens().radius.s), plate_ink);
                    cx.text_in(
                        &a.label,
                        TypeRole::Caption,
                        RectP { l: rect.l + s.px(8), ..rect },
                        Align::Start,
                        Trim::None,
                        text_ink,
                    );
                    r = rect.l - s.px(8);
                }
            } else if let Some((note, nfg)) = &out.note {
                let nink = fade(fg_ink(*nfg, cx.tokens()));
                let font = cx.res.text.font(TypeRole::Caption, None, s, cx.tokens().text_scale);
                let w = cx.res.text.width(note, font).min((r - l).max(0));
                cx.text_in(note, TypeRole::Caption, line_rect(r - w, r), Align::End, Trim::End, nink);
                r -= w + s.px(8);
            }
            name_badge = out.badge.filter(|b| matches!(b, Badge::CloudOnline | Badge::CloudLocal | Badge::CloudPinned));
        }
        if r <= l {
            return;
        }
        match out.kind {
            CellKind::Num | CellKind::Bytes | CellKind::Percent => {
                let align = if out.align != Align::Start { out.align } else { self.columns[cell.col].align };
                let line = s.px(role.spec().line);
                let at = PointP { x: r, y: plate.t + (plate.h() - line) / 2 };
                // A figure wider than its cell is cut at the cell, never drawn over the neighbour.
                let cell_clip = RectP { l: cell.rect.l, t: plate.t, r: cell.rect.r, b: plate.b };
                cx.with_clip(cell_clip, &mut |cx| {
                    cx.digits(&out.text, at, role, ink, unit_ink, align == Align::Decimal);
                });
            }
            CellKind::Icon | CellKind::Spark => {}
            CellKind::Text => {
                // The rename field is drawn over this cell: its text is the field's.
                if self.rename.as_ref().is_some_and(|r| r.col == cell.col && self.model.key(row) == r.key) {
                    return;
                }
                let align = if out.align != Align::Start { out.align } else { self.columns[cell.col].align };
                let size = s.px(12);
                // The cloud badge has its own room at the end of the name: the text trims to leave it.
                let text_r = if name_badge.is_some() { (r - size - s.px(4)).max(l) } else { r };
                let rect = line_rect(l, text_r);
                let w = self.draw_run(cx, out, role, ink, rect, align, !flat_hot, sec_hot, op);
                if let (Some(b), true) = (name_badge, w > 0) {
                    let x = (l + w + s.px(4)).min(r - size);
                    let ink = match b {
                        Badge::CloudPinned => c.fg_primary,
                        Badge::CloudLocal => c.fg_tertiary,
                        _ => c.fg_secondary,
                    };
                    let ink = fade(ink);
                    cx.glyph(RectP::wh(x, plate.t + (plate.h() - size) / 2, size, size), Glyph::Cloud, ink);
                }
            }
        }
    }

    /// One text run of a cell: its spans in their inks when it fits, else trimmed in one ink. Returns the width drawn.
    #[allow(clippy::too_many_arguments)]
    fn draw_run(
        &self,
        cx: &mut PaintCx,
        out: &CellOut,
        role: TypeRole,
        ink: Ink,
        rect: RectP,
        align: Align,
        coloured_spans: bool,
        sec_hot: bool,
        op: u8,
    ) -> i32 {
        let s = cx.scale();
        let spans = if coloured_spans { &out.spans[..usize::from(out.n_spans)] } else { &[][..] };
        let font = cx.res.text.font(role, None, s, cx.tokens().text_scale);
        let total = cx.res.text.width(&out.text, font);
        if spans.is_empty() {
            return cx.text_in(&out.text, role, rect, align, out.trim, ink);
        }
        // The text as drawn, and the spans in its bytes. A trimmed name keeps the spans that fall inside the verbatim
        // tail of Trim::Middle (the extension), re-based from the end of the trimmed text; the others are dropped.
        let mut text: Cow<str> = Cow::Borrowed(&out.text);
        let mut ranges: Vec<(usize, usize, FgRole)> =
            spans.iter().map(|sp| (sp.start as usize, (sp.end as usize).min(out.text.len()), sp.fg)).collect();
        let mut total = total;
        if total > rect.w() {
            let Trim::Middle { tail } = out.trim else {
                return cx.text_in(&out.text, role, rect, align, out.trim, ink);
            };
            let max_w = rect.w().max(0);
            let Some(trimmed) =
                crate::text::trim::trim_to(&out.text, out.trim, max_w, &mut |t| cx.res.text.width(t, font))
            else {
                return cx.text_in(&out.text, role, rect, align, out.trim, ink);
            };
            let n = out.text.chars().count();
            let from = out.text.char_indices().nth(n - usize::from(tail).min(n)).map_or(out.text.len(), |(i, _)| i);
            let new_from = trimmed.len() - (out.text.len() - from);
            ranges = ranges
                .into_iter()
                .filter(|(_, b, _)| *b > from)
                .map(|(a, b, fg)| (a.max(from) - from + new_from, b - from + new_from, fg))
                .collect();
            total = cx.res.text.width(&trimmed, font);
            text = Cow::Owned(trimmed);
        }
        let mut x = if align == Align::Start { rect.l } else { rect.r - total };
        let mut at = 0usize;
        let mut pieces: Vec<(usize, usize, FgRole)> = Vec::new();
        for (a, b, fg) in ranges {
            let b = b.min(text.len());
            if a < at || a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                continue;
            }
            if a > at {
                pieces.push((at, a, FgRole::Primary));
            }
            pieces.push((a, b, fg));
            at = b;
        }
        if at < text.len() {
            pieces.push((at, text.len(), FgRole::Primary));
        }
        for (a, b, fg) in pieces {
            let piece = &text[a..b];
            let w = cx.res.text.width(piece, font);
            // A piece keeps the row's colour unless the span names another role than the base.
            let piece_ink = if fg == FgRole::Primary {
                ink
            } else {
                let raw = if sec_hot && fg == FgRole::Secondary {
                    cx.tokens().color.fg_secondary_hot
                } else {
                    fg_ink(fg, cx.tokens())
                };
                if op == 255 {
                    raw
                } else {
                    raw.faded(f32::from(op) / 255.0)
                }
            };
            let r = RectP { l: x, t: rect.t, r: x + w, b: rect.b };
            cx.text_in(piece, role, r, Align::Start, Trim::None, piece_ink);
            x += w;
        }
        total
    }
}

impl<M: TableModel> Widget for VirtualTable<M> {
    type Out = TableOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let s = cx.scale();
        let (row_h, head_h, hit) = {
            let t = cx.tokens();
            let head = if self.style.header { cx.px(t.size.header_h) } else { 0 };
            (cx.px(self.row_h_logical.unwrap_or(t.size.row_table)).max(1), head, cx.px(t.size.col_resize_hit))
        };
        self.scale = s;
        self.inset_px = cx.px(self.style.row_inset);
        self.rect = rect;
        self.header = RectP { l: rect.l, t: rect.t, r: rect.r, b: (rect.t + head_h).min(rect.b) };
        self.viewport = RectP { l: rect.l, t: self.header.b, r: rect.r, b: rect.b };
        self.state.row_h = row_h;
        self.state.viewport_h = self.viewport.h().max(0);
        self.state.layout_widths(&self.columns, s.logical(rect.w()));
        let widths: Vec<i32> = self.state.widths.iter().map(|w| if *w > 0 { cx.px(*w) } else { 0 }).collect();
        self.cells = header::cells(&widths, RectP { b: self.header.b.max(rect.t + 1), ..self.header }, hit);
        let len = self.model.len();
        self.state.clamp_offset(len);
        self.visible = self.state.visible(len);
        let page = ((self.state.viewport_h / row_h).max(1)) as usize;
        self.nodes = self.visible.start.saturating_sub(page)..(self.visible.end + page).min(len);
        let focus_row = self.state.focused.and_then(|k| self.model.row_of(k));
        self.last_focus_row = focus_row;
        let focus_in_nodes = focus_row.is_some_and(|r| self.nodes.contains(&r));
        if self.style.header {
            let current = self.header_index().or(self.first_col());
            for h in &self.cells {
                let col = &self.columns[h.col];
                let focus = if Some(h.col) == current { Focusable::Tab } else { Focusable::Roving };
                cx.push(
                    Node::new(header::cell_id(self.id, h.col), Role::ColumnHeader, h.rect.intersect(self.header))
                        .focus(focus)
                        .label(col.title.clone())
                        .actions(Actions::CLICK | Actions::CONTEXT_MENU | Actions::FOCUS),
                );
            }
            for h in &self.cells {
                cx.push(
                    Node::new(header::handle_id(self.id, h.col), Role::Separator, h.handle.intersect(self.header))
                        .flags(NodeFlags::A11Y_HIDDEN)
                        .label(self.columns[h.col].title.clone())
                        .cursor(Cursor::SizeWE),
                );
            }
        }
        // The table itself: a Tab stop only while no row node holds the focus stop.
        let name = self
            .label
            .clone()
            .or_else(|| self.cells.first().map(|h| self.columns[h.col].title.clone()))
            .unwrap_or(Cow::Borrowed(""));
        cx.push(
            Node::new(self.table_id(), Role::Table, self.viewport)
                .label(name)
                .value(NodeValue::Position { index: focus_row.map_or(0, |r| r as u32 + 1), count: len as u32 })
                // A table with no visible column has nothing to focus (and no name to read).
                .focus(if focus_in_nodes || self.cells.is_empty() { Focusable::No } else { Focusable::Tab })
                .actions(if self.cells.is_empty() {
                    Actions::default()
                } else {
                    Actions::FOCUS | Actions::CONTEXT_MENU
                }),
        );
        for row in self.nodes.clone() {
            let key = self.model.key(row);
            let tree = self.model.tree(row);
            let mut flags = NodeFlags::default();
            if self.selection.contains(key) {
                flags = flags | NodeFlags::SELECTED;
            }
            let mut actions = Actions::CLICK | Actions::FOCUS | Actions::CONTEXT_MENU | Actions::SCROLL_INTO_VIEW;
            if let Some(t) = tree.filter(|t| t.expandable) {
                flags = flags | NodeFlags::EXPANDABLE;
                if t.expanded {
                    flags = flags | NodeFlags::EXPANDED;
                    actions = actions | Actions::COLLAPSE;
                } else {
                    actions = actions | Actions::EXPAND;
                }
            }
            let focus = if self.state.focused == Some(key) { Focusable::Tab } else { Focusable::Roving };
            let label: Text = Cow::Owned(self.model.label(row).into_owned());
            cx.push(
                Node::new(self.row_id(row), Role::Row, self.plate(row).intersect(self.viewport))
                    .parent(self.table_id())
                    .flags(flags)
                    .focus(focus)
                    .label(label)
                    .value(NodeValue::Position { index: row as u32 + 1, count: len as u32 })
                    .actions(actions),
            );
        }
        // The rename field goes over its cell, after the rows (it is hit first); a row scrolled out hides it.
        let place = self
            .rename
            .as_ref()
            .and_then(|r| self.model.row_of(r.key).filter(|row| self.visible.contains(row)).map(|row| (row, r.col)))
            .and_then(|(row, ci)| self.rename_rect(row, ci));
        if let (Some(rect), Some(r)) = (place, self.rename.as_mut()) {
            r.field.layout(cx, rect);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        if self.rect.is_empty() || !cx.visible(self.rect) {
            return;
        }
        if self.style.header {
            let focus_col = self.header_index();
            let header = self.header;
            if cx.visible(header) {
                cx.with_clip(header, &mut |cx| {
                    header::paint(cx, self.id, &self.cells, &self.columns, header, focus_col);
                });
            }
        }
        let mut out = CellOut::default();
        let vp = self.viewport;
        cx.with_clip(vp, &mut |cx| self.paint_rows(cx, &mut out));
        if let Some(r) = &self.rename {
            cx.with_clip(vp, &mut |cx| r.field.paint(cx));
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<TableOut> {
        if self.rename.is_some() {
            if let Some(out) = self.rename_event(cx, ev) {
                return out;
            }
        }
        match ev {
            Event::Timer(t) if *t == TimerId::kit(self.id, TIMER_SCROLL) => return self.scroll_step(cx),
            Event::Command(id) if id.is_kit() && ((id.0 >> 16) & 0x7FFF) == (self.id.0 & 0x7FFF) => {
                return self.header_command(cx, (id.0 & 0xFFFF) as u16);
            }
            _ => {}
        }
        let node = cx.target().filter(|n| n.owner() == self.id)?;
        let part = self.part(node);
        match (ev, part) {
            (Event::PointerMove { .. }, Part::Row(row)) => {
                if let Some(key) = (row < self.model.len()).then(|| self.model.key(row)) {
                    if self.action_seen.map(|a| a.0) != Some(key) {
                        self.action_seen = Some((key, cx.now_ms()));
                    }
                }
                None
            }
            (Event::PointerLeave, _) => {
                self.action_seen = None;
                None
            }
            (Event::PointerDown { pos, button: MouseButton::Left, .. }, Part::Handle(col)) => {
                let start_w = self.columns.get(col)?.width;
                self.resize = Some(Resize { col, start_x: pos.x, start_w });
                None
            }
            (Event::DragMove { pos, .. }, Part::Handle(_)) => {
                let r = self.resize?;
                let col = self.columns.get_mut(r.col)?;
                let min = col.min.max(crate::tokens::COL_MIN_W);
                let delta = self.scale.logical(pos.x - r.start_x);
                let w = (r.start_w + delta).max(min);
                if col.width != w {
                    col.width = w;
                    self.dirty(cx);
                }
                None
            }
            (Event::DragEnd { .. }, Part::Handle(_)) => {
                let r = self.resize.take()?;
                (self.columns.get(r.col)?.width != r.start_w).then_some(TableOut::ColumnsChanged)
            }
            (Event::Click { pos, button, clicks, mods }, _) => self.on_click(cx, part, *pos, *button, *clicks, *mods),
            (Event::Key { key, down: true, mods, .. }, _) => self.on_key(cx, part, *key, *mods),
            (Event::Text(ch), Part::Row(_) | Part::Table) => {
                let now = cx.now_ms();
                let fx = self.state.on_text(*ch, &self.model, &mut self.selection, now);
                self.apply(cx, fx, None, None)
            }
            (Event::Wheel { delta, horizontal: false, .. }, _) => {
                self.on_wheel(cx, *delta);
                None
            }
            (Event::FocusIn(_), Part::Table) => {
                let len = self.model.len();
                if len > 0 {
                    let row = self
                        .state
                        .focused
                        .and_then(|k| self.model.row_of(k))
                        .unwrap_or(self.visible.start.min(len - 1));
                    self.state.focused = Some(self.model.key(row));
                    self.state.scroll_to_row(row, ScrollTo::Nearest, len);
                    self.last_focus_row = Some(row);
                    cx.set_focus(self.row_id(row));
                }
                self.dirty(cx);
                None
            }
            (Event::FocusIn(_), Part::Header(col)) => {
                self.state.header_col = Some(self.columns.get(col)?.id);
                self.dirty(cx);
                None
            }
            (Event::FocusIn(_) | Event::FocusOut, _) => {
                cx.damage(self.viewport);
                None
            }
            (Event::A11y(_, action), _) => self.on_a11y(cx, part, action),
            _ => None,
        }
    }
}

impl<M: TableModel> VirtualTable<M> {
    /// What the open rename field's events mean: Some(output) when the field took the event.
    fn rename_event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<Option<TableOut>> {
        let err_timer = TimerId::kit(self.id, TIMER_RENAME_ERR);
        let rn = self.rename.as_mut()?;
        let owner = rn.field.node_id().owner();
        let mine = match ev {
            Event::Timer(t) if *t == err_timer => {
                rn.field.set_error(cx, None);
                return Some(None);
            }
            Event::Timer(t) => t.is_kit() && (t.0 >> 8) & 0x7F_FFFF == owner.0 & 0x7F_FFFF,
            Event::ListPicked { .. } | Event::MenuClosed(_) => true,
            _ => cx.target().is_some_and(|n| n.owner() == owner),
        };
        if !mine {
            return None;
        }
        let key = rn.key;
        let out = rn.field.event(cx, ev);
        let text = rn.field.text().to_string();
        match (ev, out) {
            // Focus left the field: the name is taken as it stands.
            (Event::FocusOut, _) => Some(self.finish_rename(cx, Some(text), false)),
            (_, Some(FieldOut::Committed(t))) => Some(self.finish_rename(cx, Some(t), true)),
            (_, Some(FieldOut::Cancelled)) => Some(self.finish_rename(cx, None, true)),
            (_, Some(FieldOut::CommitAndMove { text, back })) => {
                let to = self.model.row_of(key).and_then(|row| {
                    let n = if back { row.checked_sub(1)? } else { row + 1 };
                    (n < self.model.len()).then(|| self.model.key(n))
                });
                let committed = self.finish_rename(cx, Some(text), true);
                if let Some(next) = to {
                    self.set_focused(cx, next, ScrollTo::Nearest);
                    self.extra.push(TableOut::RenameRequested(next));
                }
                Some(committed)
            }
            (_, Some(FieldOut::Refused(_))) => {
                if let Some(t) = self.refusal.clone() {
                    if let Some(rn) = self.rename.as_mut() {
                        rn.field.set_error(cx, Some(t));
                    }
                    cx.set_timer(err_timer, RENAME_ERR_MS);
                }
                Some(None)
            }
            _ => Some(None),
        }
    }

    /// Closes the rename field: the name (None: cancelled) is the output; the focus goes back to the row when asked.
    fn finish_rename(&mut self, cx: &mut EventCx, name: Option<String>, restore_focus: bool) -> Option<TableOut> {
        let mut rn = self.rename.take()?;
        rn.field.release(cx);
        cx.cancel_timer(TimerId::kit(self.id, TIMER_RENAME_ERR));
        self.dirty(cx);
        if restore_focus {
            if let Some(row) = self.model.row_of(rn.key) {
                cx.set_focus(self.row_id(row));
            }
        }
        Some(match name {
            Some(t) => TableOut::RenameCommitted(rn.key, t),
            None => TableOut::RenameCancelled(rn.key),
        })
    }

    /// Picks the output an event returns (the most significant) and queues the rest for take_extra.
    fn pick(&mut self, mut outs: Vec<TableOut>) -> Option<TableOut> {
        fn rank(o: &TableOut) -> u8 {
            match o {
                TableOut::Activated(_) => 0,
                TableOut::SortRequested(..) => 1,
                TableOut::ContextMenu(..) => 2,
                TableOut::ColumnsChanged => 3,
                TableOut::ExpandRequested(..) => 4,
                TableOut::RowAction(..) => 5,
                TableOut::RenameRequested(_) => 6,
                TableOut::SelectionChanged => 7,
                _ => 8,
            }
        }
        outs.sort_by_key(rank);
        let mut it = outs.into_iter();
        let first = it.next();
        self.extra.extend(it);
        first
    }

    /// Applies what TableState reported: the outputs, the focus node, the redraw and the header menu.
    fn apply(
        &mut self,
        cx: &mut EventCx,
        fx: super::table_state::TableEffects,
        at: Option<PointP>,
        ctx_key: Option<RowKey>,
    ) -> Option<TableOut> {
        let mut outs = Vec::new();
        if let Some(k) = fx.activate {
            outs.push(TableOut::Activated(k));
        }
        if let Some((col, dir)) = fx.sort {
            outs.push(TableOut::SortRequested(col, dir));
        }
        if fx.context {
            outs.push(TableOut::ContextMenu(ctx_key, at.unwrap_or_else(|| self.focus_center())));
        }
        if fx.columns_changed {
            outs.push(TableOut::ColumnsChanged);
        }
        if let Some((k, e)) = fx.expand {
            outs.push(TableOut::ExpandRequested(k, e));
        }
        if let Some(k) = fx.rename {
            outs.push(TableOut::RenameRequested(k));
        }
        if fx.selection_changed {
            outs.push(TableOut::SelectionChanged);
        }
        if let Some(k) = fx.focus {
            outs.push(TableOut::FocusChanged(Some(k)));
        }
        let len = self.model.len();
        if fx.autofit_all {
            let mut changed = false;
            for i in 0..self.columns.len() {
                if self.columns[i].visible {
                    let w = self.widest(cx, i);
                    let mut col = self.columns[i].clone();
                    changed |= self.state.autofit(&mut col, w);
                    self.columns[i] = col;
                }
            }
            if changed {
                outs.push(TableOut::ColumnsChanged);
            }
        }
        if fx.focus.is_some() || fx.scroll_to.is_some() {
            if let Some(row) = self.state.focused.and_then(|k| self.model.row_of(k)) {
                self.last_focus_row = Some(row);
                self.state.clamp_offset(len);
                cx.set_focus(self.row_id(row));
            }
            self.dirty(cx);
        } else if fx.selection_changed || fx.columns_changed || fx.autofit_all || fx.expand.is_some() {
            self.dirty(cx);
        }
        if let Some(col) = fx.header_menu {
            self.open_header_menu(cx, col, at);
        }
        self.pick(outs)
    }

    /// Opens the header menu of `col`: at the pointer for a click, below the cell for the keys.
    fn open_header_menu(&mut self, cx: &mut EventCx, col: ColId, at: Option<PointP>) {
        let Some(i) = self.columns.iter().position(|c| c.id == col) else { return };
        let Some(cell) = self.cell_rect(i) else { return };
        self.menu_col = Some(i);
        let items = header::menu_items(self.id, &self.columns, i, cx.lang());
        let anchor = at.map_or(Anchor::Below(cell), Anchor::Point);
        cx.open_menu(items, anchor);
    }

    fn on_click(
        &mut self,
        cx: &mut EventCx,
        part: Part,
        pos: PointP,
        button: MouseButton,
        clicks: u8,
        mods: Mods,
    ) -> Option<TableOut> {
        match part {
            Part::Header(col) => {
                let id = self.columns.get(col)?.id;
                let fx = self.state.on_header_click(id, button, &self.columns);
                self.dirty(cx);
                self.apply(cx, fx, Some(pos), None)
            }
            Part::Handle(col) => {
                self.resize = None;
                if col >= self.columns.len() {
                    return None;
                }
                if button == MouseButton::Left && clicks >= 2 {
                    let w = self.widest(cx, col);
                    let mut c = self.columns[col].clone();
                    let changed = self.state.autofit(&mut c, w);
                    self.columns[col] = c;
                    if changed {
                        self.dirty(cx);
                        return Some(TableOut::ColumnsChanged);
                    }
                }
                None
            }
            Part::Table => {
                let fx = self.state.on_click(None, mods, clicks, button, &self.model, &mut self.selection);
                self.apply(cx, fx, Some(pos), None)
            }
            Part::Row(row) => {
                if row >= self.model.len() {
                    return None;
                }
                let key = self.model.key(row);
                let plate = self.plate(row);
                if button == MouseButton::Left {
                    if let Some(t) = self.model.tree(row).filter(|t| t.expandable) {
                        if self.chevron_box(plate, t.depth).contains(pos) {
                            self.state.focused = Some(key);
                            return Some(TableOut::ExpandRequested(key, !t.expanded));
                        }
                    }
                    let delay = cx.tokens().motion.dur_fast;
                    let ts = cx.tokens().text_scale;
                    let shown = self
                        .action_seen
                        .is_some_and(|(k, t)| k == key && cx.now_ms().saturating_sub(t) >= u64::from(delay));
                    if shown {
                        let mut tmp = CellOut::default();
                        if let Some(a) = self.action_of(row, &mut tmp) {
                            let font = cx.text().font(TypeRole::Caption, None, self.scale, ts);
                            let w = cx.text().width(&a.label, font);
                            if self.action_rect(plate, w).is_some_and(|r| r.contains(pos)) {
                                return Some(TableOut::RowAction(key, a.command));
                            }
                        }
                    }
                }
                let was_only = self.selection.single() == Some(key);
                let mut fx = self.state.on_click(Some(row), mods, clicks, button, &self.model, &mut self.selection);
                if button == MouseButton::Left && clicks == 1 && !mods.ctrl() && !mods.shift() {
                    let now = cx.now_ms();
                    let slow = was_only
                        && self
                            .last_click
                            .is_some_and(|(k, t)| k == key && SLOW_CLICK_MS.contains(&now.saturating_sub(t)));
                    if slow && self.on_name(cx, row, pos) {
                        fx.rename = Some(key);
                    }
                    self.last_click = Some((key, now));
                } else if clicks > 1 {
                    self.last_click = None;
                }
                let ctx = fx.context.then_some(key);
                self.apply(cx, fx, Some(pos), ctx)
            }
        }
    }

    /// True when `pos` is on the name text of `row` (the first column's text, from where it starts to where it ends).
    fn on_name(&mut self, cx: &mut EventCx, row: usize, pos: PointP) -> bool {
        let Some(ci) = self.first_col() else { return false };
        let Some(cell) = self.cell_rect(ci) else { return false };
        let mut out = CellOut::default();
        self.model.cell(row, self.columns[ci].id, &mut out);
        let l = self.name_left(row, cell, &out);
        let (scale, ts) = (self.scale, cx.tokens().text_scale);
        let font = cx.text().font(out.role, None, scale, ts);
        let w = cx.text().width(&out.text, font).min((cell.r - self.scale.px(12) - l).max(0));
        let plate = self.plate(row);
        RectP { l, t: plate.t, r: l + w, b: plate.b }.contains(pos)
    }

    fn on_key(&mut self, cx: &mut EventCx, part: Part, key: Key, mods: Mods) -> Option<TableOut> {
        if let Part::Header(_) = part {
            let menu = key == Key::APPS || (key == Key::F10 && mods.shift());
            let before = self.state.header_col;
            let fx = self.state.on_header_key(key, mods, &mut self.columns);
            let handled =
                menu || matches!(key, Key::LEFT | Key::RIGHT | Key::ENTER | Key::SPACE) && !mods.alt() && !mods.ctrl();
            if handled {
                cx.consume();
            }
            if self.state.header_col != before {
                if let Some(i) = self.header_index() {
                    cx.set_focus(header::cell_id(self.id, i));
                }
                self.dirty(cx);
            }
            if fx.columns_changed {
                self.dirty(cx);
            }
            return self.apply(cx, fx, None, None);
        }
        if matches!(part, Part::Handle(_)) {
            return None;
        }
        let handled = match key {
            Key::UP | Key::DOWN | Key::PAGE_UP | Key::PAGE_DOWN | Key::HOME | Key::END => !mods.alt() && !mods.win(),
            Key::ENTER | Key::F2 | Key::LEFT | Key::RIGHT => {
                !mods.alt() && !mods.win() && !mods.ctrl() && !mods.shift()
            }
            Key::SPACE => mods.ctrl() && !mods.shift(),
            Key::OEM_PLUS | Key::ADD => mods.ctrl(),
            Key(0x41) => mods.ctrl() && !mods.shift() && self.state.mode == SelectionMode::Multi,
            _ => false,
        };
        if handled {
            cx.consume();
        }
        let before = self.state.offset;
        let fx = self.state.on_key(key, mods, &self.model, &mut self.selection, cx.now_ms());
        let moved = self.state.offset != before;
        let out = self.apply(cx, fx, None, None);
        if moved {
            self.scroll = None;
        }
        out
    }

    fn on_a11y(&mut self, cx: &mut EventCx, part: Part, action: &A11yAction) -> Option<TableOut> {
        match (part, action) {
            (Part::Row(row), A11yAction::Click) => {
                let at = self.plate(row);
                let pos = PointP { x: (at.l + at.r) / 2, y: (at.t + at.b) / 2 };
                self.on_click(cx, part, pos, MouseButton::Left, 1, Mods::NONE)
            }
            (Part::Row(row), A11yAction::Focus) if row < self.model.len() => {
                let fx =
                    self.state.on_click(Some(row), Mods::CTRL, 1, MouseButton::Left, &self.model, &mut self.selection);
                let mut fx = fx;
                fx.selection_changed = false;
                self.apply(cx, fx, None, None)
            }
            (Part::Row(row), A11yAction::ScrollIntoView) if row < self.model.len() => {
                self.state.scroll_to_row(row, ScrollTo::Nearest, self.model.len());
                self.dirty(cx);
                None
            }
            (Part::Row(row), A11yAction::Expand | A11yAction::Collapse) if row < self.model.len() => {
                Some(TableOut::ExpandRequested(self.model.key(row), *action == A11yAction::Expand))
            }
            (Part::Row(row), A11yAction::ShowContextMenu) if row < self.model.len() => {
                let at = self.plate(row);
                let pos = PointP { x: (at.l + at.r) / 2, y: (at.t + at.b) / 2 };
                Some(TableOut::ContextMenu(Some(self.model.key(row)), pos))
            }
            (Part::Header(col), A11yAction::Click) => {
                let id = self.columns.get(col)?.id;
                let fx = self.state.on_header_click(id, MouseButton::Left, &self.columns);
                self.apply(cx, fx, None, None)
            }
            (Part::Header(col), A11yAction::ShowContextMenu) => {
                let id = self.columns.get(col)?.id;
                self.open_header_menu(cx, id, None);
                None
            }
            _ => None,
        }
    }

    /// The widest visible cell of column `col` and its title, in logical px (the auto-fit width before padding).
    fn widest(&self, cx: &mut EventCx, col: usize) -> i32 {
        let s = self.scale;
        let ts = cx.tokens().text_scale;
        let mut widest = {
            let f = cx.text().font(TypeRole::Caption, None, s, ts);
            cx.text().width(&self.columns[col].title, f) + s.px(24)
        };
        let mut out = CellOut::default();
        let first = self.first_col() == Some(col);
        for row in self.visible.clone() {
            out.clear();
            self.model.cell(row, self.columns[col].id, &mut out);
            let f = cx.text().font(out.role, None, s, ts);
            let mut w = cx.text().width(&out.text, f);
            if first {
                let depth = self.model.tree(row).map_or(0, |t| i32::from(t.depth));
                w += s.px(self.style.icon_x - 12) + s.px(16) * depth + if out.icon.is_some() { s.px(24) } else { 0 };
            }
            widest = widest.max(w);
        }
        s.logical(widest)
    }

    /// A header menu pick.
    fn header_command(&mut self, cx: &mut EventCx, n: u16) -> Option<TableOut> {
        let col = self.menu_col.filter(|c| *c < self.columns.len())?;
        let visible: Vec<usize> = (0..self.columns.len()).filter(|&i| self.columns[i].visible).collect();
        let out = match n {
            header::CMD_SORT_ASC => Some(TableOut::SortRequested(self.columns[col].id, crate::model::SortDir::Asc)),
            header::CMD_SORT_DESC => Some(TableOut::SortRequested(self.columns[col].id, crate::model::SortDir::Desc)),
            header::CMD_HIDE => self.set_visible(col, false, &visible),
            header::CMD_LEFT | header::CMD_RIGHT => {
                let p = visible.iter().position(|&i| i == col)?;
                let q =
                    if n == header::CMD_LEFT { p.checked_sub(1)? } else { (p + 1 < visible.len()).then_some(p + 1)? };
                self.columns.swap(visible[p], visible[q]);
                self.menu_col = Some(visible[q]);
                Some(TableOut::ColumnsChanged)
            }
            n if n >= header::CMD_COLUMN_BASE => {
                let i = usize::from(n - header::CMD_COLUMN_BASE);
                let show = !self.columns.get(i)?.visible;
                self.set_visible(i, show, &visible)
            }
            _ => None,
        };
        if matches!(out, Some(TableOut::ColumnsChanged)) {
            self.dirty(cx);
        }
        out
    }

    /// Hides or shows a column when its flag and the others allow it.
    fn set_visible(&mut self, i: usize, show: bool, visible: &[usize]) -> Option<TableOut> {
        let c = &self.columns[i];
        if !c.hideable || c.visible == show || (!show && visible.len() <= 1) {
            return None;
        }
        if !show && self.state.header_col == Some(c.id) {
            self.state.header_col = None;
        }
        self.columns[i].visible = show;
        Some(TableOut::ColumnsChanged)
    }

    fn on_wheel(&mut self, cx: &mut EventCx, delta: WheelDelta) {
        let len = self.model.len();
        match delta {
            WheelDelta::Pixels(px) => {
                self.scroll = None;
                let to = self.state.offset - px.round() as i64;
                self.set_offset(cx, to);
            }
            WheelDelta::Lines(lines) => {
                let base = self.scroll.map_or(self.state.offset, |a| a.to);
                let max = (len as i64 * i64::from(self.state.row_h) - i64::from(self.state.viewport_h)).max(0);
                let to = (base - (lines * self.state.row_h as f32).round() as i64).clamp(0, max);
                if to == base {
                    return; // already at the end: nothing to animate, no timer
                }
                if cx.tokens().motion.dur_normal == 0 {
                    // Reduced motion: every duration is 0, the wheel jumps and arms no timer.
                    self.scroll = None;
                    self.set_offset(cx, to);
                    return;
                }
                self.scroll = Some(ScrollAnim { from: self.state.offset, to, start_ms: cx.now_ms() });
                cx.set_timer(TimerId::kit(self.id, TIMER_SCROLL), FRAME_MS);
            }
        }
    }

    /// Moves the offset and tells the host: the viewport's pixels move by the difference (retained scroll) and the
    /// nodes are rebuilt.
    fn set_offset(&mut self, cx: &mut EventCx, to: i64) {
        let before = self.state.offset;
        self.state.offset = to;
        self.state.clamp_offset(self.model.len());
        let dy = (before - self.state.offset).clamp(-1_000_000, 1_000_000) as i32;
        if dy != 0 {
            cx.scroll(self.viewport, dy);
            cx.relayout();
        }
    }

    fn scroll_step(&mut self, cx: &mut EventCx) -> Option<TableOut> {
        let a = self.scroll?;
        let dur = cx.tokens().motion.dur_normal.max(1);
        let t = (cx.now_ms().saturating_sub(a.start_ms) as f32 / dur as f32).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3); // Ease::OutCubic
        let to = a.from + ((a.to - a.from) as f64 * f64::from(eased)).round() as i64;
        self.set_offset(cx, to);
        if t >= 1.0 {
            self.scroll = None;
        } else {
            cx.set_timer(TimerId::kit(self.id, TIMER_SCROLL), FRAME_MS);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandId, CommandTable};
    use crate::cx::MenuItem;
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::model::{SortDir, TableModel};
    use crate::screen::Screen;
    use crate::theme::Tokens;
    use std::cell::Cell;

    /// Rows with the keys `order` (labels "item K"); counts the calls the layout and the paint make.
    struct T {
        order: Vec<u64>,
        cells: Cell<u32>,
        labels: Cell<u32>,
        /// The row that moves, and how.
        motion: (usize, RowMotion),
    }

    impl T {
        fn new(n: u64) -> T {
            T {
                order: (0..n).collect(),
                cells: Cell::new(0),
                labels: Cell::new(0),
                motion: (usize::MAX, RowMotion::NONE),
            }
        }
    }

    impl ListModel for T {
        fn len(&self) -> usize {
            self.order.len()
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(self.order[row])
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            self.order.iter().position(|k| *k == key.0)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            self.labels.set(self.labels.get() + 1);
            Cow::Owned(format!("item {}", self.order[row]))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for T {
        fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
            self.cells.set(self.cells.get() + 1);
            let k = self.order[row];
            match col.0 {
                0 => out.set_text(CellKind::Text, &format!("item {k}")),
                1 => out.set_text(CellKind::Bytes, &format!("{} KB", k)),
                _ => out.set_text(CellKind::Percent, &format!("{}.0%", k % 100)),
            };
            if col.0 == 2 && self.motion.0 == row {
                out.heat = Some(HeatStep(2));
            }
        }
        fn motion(&self, row: usize) -> RowMotion {
            if row == self.motion.0 {
                self.motion.1
            } else {
                RowMotion::NONE
            }
        }
    }

    /// A million rows without storage; counts the calls.
    struct Huge {
        cells: Cell<u32>,
        labels: Cell<u32>,
    }

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
        fn label(&self, row: usize) -> Cow<'_, str> {
            self.labels.set(self.labels.get() + 1);
            Cow::Owned(format!("row {row}"))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Huge {
        fn cell(&self, row: usize, _: ColId, out: &mut CellOut) {
            self.cells.set(self.cells.get() + 1);
            out.set_text(CellKind::Text, &format!("row {row}"));
        }
    }

    fn select(keys: &[u64]) -> Selection {
        let mut s = Selection::None;
        for (i, k) in keys.iter().enumerate() {
            if i == 0 {
                s.click(RowKey(*k));
            } else {
                s.toggle(RowKey(*k));
            }
        }
        s
    }

    fn cols(all_hideable: bool) -> Vec<Column> {
        let c = |id: u16, title: &'static str, width, flex, hideable| Column {
            id: ColId(id),
            title: Cow::Borrowed(title),
            width,
            min: 48,
            flex,
            align: Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable,
            tip: None,
        };
        vec![c(0, "Name", 200, true, all_hideable), c(1, "Size", 100, false, true), c(2, "CPU", 100, false, true)]
    }

    type Op = Box<dyn FnOnce(&mut VirtualTable<T>, &mut EventCx) -> Option<TableOut> + Send>;

    /// The table on its own, with every output kept and a way to run a closure with an EventCx (Harness::send).
    struct Sc {
        t: VirtualTable<T>,
        outs: Vec<TableOut>,
        cmds: CommandTable,
    }

    impl Screen for Sc {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.t.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(300)));
            avail
        }
        fn paint(&self, cx: &mut PaintCx) {
            self.t.paint(cx);
        }
        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> crate::event::Reply {
            if let Some(o) = self.t.event(cx, ev) {
                self.outs.push(o);
            }
            self.outs.extend(self.t.take_extra());
            crate::event::Reply::None
        }
        fn data(&mut self, cx: &mut EventCx, msg: crate::host::AppMsg) -> crate::event::Reply {
            if let Ok(op) = msg.downcast::<Op>() {
                if let Some(o) = (*op)(&mut self.t, cx) {
                    self.outs.push(o);
                }
            }
            crate::event::Reply::None
        }
        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn make(n: u64, style: TableStyle, all_hideable: bool) -> Harness<Sc> {
        of(VirtualTable::new(WidgetId(1), T::new(n), cols(all_hideable), SelectionMode::Multi, style))
    }

    fn of(t: VirtualTable<T>) -> Harness<Sc> {
        let sc = Sc { t, outs: Vec::new(), cmds: CommandTable::new() };
        Harness::new(sc, SizeL { w: 400, h: 300 }, 96, Tokens::oled())
    }

    fn run(
        h: &mut Harness<Sc>,
        f: impl FnOnce(&mut VirtualTable<T>, &mut EventCx) -> Option<TableOut> + Send + 'static,
    ) {
        h.send(Box::new(Box::new(f) as Op));
    }

    fn t(h: &Harness<Sc>) -> &VirtualTable<T> {
        &h.screen().t
    }

    fn outs(h: &mut Harness<Sc>) -> Vec<TableOut> {
        std::mem::take(&mut h.screen_mut().outs)
    }

    /// The centre of a row (96 DPI, FILES style: 28 header, 28 rows, no scroll).
    fn row_pt(row: i32) -> PointL {
        PointL { x: 250, y: 28 + row * 28 + 14 }
    }

    fn px(img: &crate::harness::Image, x: u32, y: u32) -> u32 {
        img.px[(y * img.w + x) as usize] & 0xFF
    }

    fn focus_body(h: &mut Harness<Sc>) {
        h.key("Tab").key("Tab");
    }

    #[test]
    fn only_visible_rows_are_laid_out() {
        let model = Huge { cells: Cell::new(0), labels: Cell::new(0) };
        let table = VirtualTable::new(WidgetId(1), model, cols(false), SelectionMode::Multi, TableStyle::FILES);
        struct H(VirtualTable<Huge>, CommandTable);
        impl Screen for H {
            fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
                self.0.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(300)));
                avail
            }
            fn paint(&self, cx: &mut PaintCx) {
                self.0.paint(cx);
            }
            fn event(&mut self, _: &mut EventCx, _: &Event) -> crate::event::Reply {
                crate::event::Reply::None
            }
            fn commands(&self) -> &CommandTable {
                &self.1
            }
        }
        let mut h = Harness::new(H(table, CommandTable::new()), SizeL { w: 400, h: 300 }, 96, Tokens::oled());
        h.render();
        let m = h.screen().0.model();
        // 272 px of rows = 10 whole rows (+1 partial): painted cells = 3 columns of them at most; the labels (a11y nodes)
        // cover the visible rows and one screen each way.
        assert!(h.screen().0.visible_rows().len() <= 11);
        assert!(m.cells.get() <= 11 * 3, "cells: {}", m.cells.get());
        assert!(m.labels.get() <= 11 + 2 * 10, "labels: {}", m.labels.get());
        assert!(m.cells.get() > 0 && m.labels.get() > 0);
    }

    #[test]
    fn merged_run_has_two_round_ends() {
        let mut h =
            of(VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES)
                .with_selection(select(&[1, 2, 3]))
                .with_focused(RowKey(2)));
        h.key("Tab").key("Tab");
        let img = h.render();
        // The run covers rows 1-3: y 56..140 at x 4.. (the plate inset). Inside, the boundary between rows 1 and 2.
        assert_eq!(px(&img, 6, 83), 48, "no gap between the rows");
        assert_eq!(px(&img, 4, 83), 48, "no rounding at the inner boundary");
        assert_eq!(px(&img, 4, 84), 48);
        assert!(px(&img, 4, 56) < 12, "the run's top left corner is rounded");
        assert!(px(&img, 4, 139) < 12, "the run's bottom left corner is rounded");
        assert_eq!(px(&img, 6, 70), 48, "the plate is FILL_ROW_SELECTED");
    }

    #[test]
    fn inactive_selection_alpha() {
        let mut h =
            of(VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES)
                .with_selection(select(&[1, 2]))
                .with_focused(RowKey(1)));
        h.key("Tab").key("Tab");
        // Row 2 is selected too and holds no focus ring.
        assert_eq!(px(&h.render(), 6, 98), 48);
        h.set_active(false);
        assert_eq!(px(&h.render(), 6, 98), 28, "inactive: 28");
    }

    #[test]
    fn bar_look_matches_tasks_mockup() {
        let mut h =
            of(VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::TASKS)
                .with_selection(select(&[1]))
                .with_focused(RowKey(1)));
        // Focused by the pointer (no ring), the pointer then parked on the empty area below the rows.
        h.click(PointL { x: 150, y: 42 }).hover(PointL { x: 390, y: 290 });
        let img = h.render();
        // No header: row 1 is y 28..56, middle 42. The plate (inset 8) is FILL_CONTROL_HOT 28; the bar is 2 x 12 at x 10.
        assert_eq!(px(&img, 30, 36), 28, "plate");
        // FG_PRIMARY (230) over the plate's 28.
        assert_eq!(px(&img, 10, 42), 233, "bar");
        assert_eq!(px(&img, 11, 42), 233);
        assert_eq!(px(&img, 10, 37), 233, "inside the bar's top (its end is a rounded pill)");
        assert_eq!(px(&img, 10, 34), 28, "above the bar: the plate only");
        assert_eq!(px(&img, 10, 46), 233);
        assert_eq!(px(&img, 10, 49), 28, "below the bar");
        assert_eq!(px(&img, 12, 42), 28, "right of the bar");
        // Inactive: FILL_HOVER 20 and no bar.
        h.set_active(false);
        let img = h.render();
        assert_eq!((px(&img, 30, 36), px(&img, 10, 42)), (20, 20));
    }

    #[test]
    fn key_script_matches_table_state() {
        let mut h = make(100, TableStyle::FILES, false);
        focus_body(&mut h);
        let model = T::new(100);
        let mut st = TableState::new(28, SelectionMode::Multi);
        st.viewport_h = 272;
        st.focused = Some(RowKey(0));
        let mut sel = Selection::None;
        let script = [
            ("Down", Key::DOWN, Mods::NONE),
            ("Down", Key::DOWN, Mods::NONE),
            ("Shift+Down", Key::DOWN, Mods::SHIFT),
            ("Shift+Down", Key::DOWN, Mods::SHIFT),
            ("Ctrl+Up", Key::UP, Mods::CTRL),
            ("Ctrl+Space", Key::SPACE, Mods::CTRL),
            ("PgDn", Key::PAGE_DOWN, Mods::NONE),
            ("End", Key::END, Mods::NONE),
            ("Home", Key::HOME, Mods::NONE),
            ("Ctrl+A", Key(0x41), Mods::CTRL),
            ("Up", Key::UP, Mods::NONE),
        ];
        for (name, key, mods) in script {
            h.key(name);
            st.on_key(key, mods, &model, &mut sel, 0);
            assert_eq!(t(&h).focused(), st.focused, "focus after {name}");
            assert_eq!(*t(&h).selection(), sel, "selection after {name}");
            assert_eq!(t(&h).state.offset, st.offset, "offset after {name}");
        }
        // The focus node follows the key.
        let row = model.row_of(st.focused.unwrap()).unwrap();
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), row as u32 + 1)));
    }

    #[test]
    fn right_click_then_context_menu_out() {
        let mut h = make(20, TableStyle::FILES, false);
        h.right_click(row_pt(3));
        let o = outs(&mut h);
        let at = o.iter().find_map(|o| match o {
            TableOut::ContextMenu(k, p) => Some((*k, *p)),
            _ => None,
        });
        let (k, p) = at.expect("a ContextMenu output");
        assert_eq!(k, Some(RowKey(3)));
        assert_eq!((p.x, p.y), (250, 28 + 3 * 28 + 14));
        assert!(o.contains(&TableOut::SelectionChanged), "an unselected row is selected first");
        assert_eq!(t(&h).selection().single(), Some(RowKey(3)));
        // The empty area: no row.
        let mut h = make(2, TableStyle::FILES, false);
        h.right_click(PointL { x: 250, y: 200 });
        assert!(outs(&mut h).iter().any(|o| matches!(o, TableOut::ContextMenu(None, _))));
    }

    #[test]
    fn sort_click_emits_request_only() {
        let mut h = make(20, TableStyle::FILES, false);
        h.click(PointL { x: 260, y: 14 });
        let o = outs(&mut h);
        assert_eq!(o, [TableOut::SortRequested(ColId(1), SortDir::Asc)]);
        // The table never sorts: neither the model nor the columns changed.
        assert_eq!(t(&h).model().order, (0..20).collect::<Vec<u64>>());
        assert!(t(&h).columns().iter().all(|c| c.sort.is_none()));
        // A sorted column asks for the other direction.
        run(&mut h, |t, cx| {
            let mut c = t.columns().to_vec();
            c[1].sort = Some(SortDir::Asc);
            t.set_columns(cx, c);
            None
        });
        h.click(PointL { x: 260, y: 14 });
        assert_eq!(outs(&mut h), [TableOut::SortRequested(ColId(1), SortDir::Desc)]);
    }

    #[test]
    fn model_changed_keeps_focus_and_y() {
        let mut h = make(100, TableStyle::FILES, false);
        run(&mut h, |t, cx| {
            t.set_focused(cx, RowKey(40), ScrollTo::Center);
            None
        });
        h.render();
        let old_y = t(&h).row_rect(40).expect("row 40 is in view").t;
        run(&mut h, |t, cx| {
            t.model_mut().order.reverse();
            t.model_changed(cx, true);
            None
        });
        h.render();
        let new_row = t(&h).model().row_of(RowKey(40)).unwrap();
        assert_eq!(new_row, 59);
        assert_eq!(t(&h).focused(), Some(RowKey(40)));
        assert_eq!(t(&h).row_rect(new_row).map(|r| r.t), Some(old_y), "the focused row keeps its y");
        // Without keep_anchor the offset stays where it was.
        let before = t(&h).state.offset;
        run(&mut h, |t, cx| {
            t.model_mut().order.reverse();
            t.model_changed(cx, false);
            None
        });
        assert_eq!(t(&h).state.offset, before);
    }

    #[test]
    fn focus_node_follows_key_after_reorder() {
        let mut h = make(50, TableStyle::FILES, false);
        focus_body(&mut h);
        for _ in 0..5 {
            h.key("Down");
        }
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 6)), "row 5 is node 6");
        run(&mut h, |t, cx| {
            t.model_mut().order.reverse();
            t.model_changed(cx, true);
            None
        });
        h.render();
        let new_row = t(&h).model().row_of(RowKey(5)).unwrap();
        assert_eq!(new_row, 44);
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), new_row as u32 + 1)));
        let dump = h.a11y_dump();
        let line = dump.lines().find(|l| l.contains("\"item 5\"")).expect("the row is in the dump");
        assert!(line.contains("45 of 50"), "{line}");
        // A key that left the model: the focus falls back to a row (or the table itself).
        run(&mut h, |t, cx| {
            t.model_mut().order.retain(|k| *k != 5);
            t.model_changed(cx, true);
            None
        });
        h.render();
        assert!(h.focused().is_some_and(|n| n.owner() == WidgetId(1)));
    }

    fn open_header_menu(h: &mut Harness<Sc>, x: i32) -> Vec<MenuItem> {
        h.right_click(PointL { x, y: 14 });
        h.menus().last().expect("a menu opened").items.clone()
    }

    fn enabled(items: &[MenuItem], n: u16) -> Option<bool> {
        items.iter().find_map(|i| match i {
            MenuItem::Custom { id, enabled, .. } if id.0 & 0xFFFF == u32::from(n) => Some(*enabled),
            _ => None,
        })
    }

    /// A pick of the open header menu: the menu closes (K2a.9's real menus would swallow the next right click otherwise)
    /// and the command reaches the table.
    fn pick(h: &mut Harness<Sc>, n: u16) {
        run(h, move |t, cx| {
            cx.close_menus();
            t.event(cx, &Event::Command(CommandId::kit(WidgetId(1), n)))
        });
    }

    #[test]
    fn header_menu_hides_column() {
        let mut h = make(10, TableStyle::FILES, false);
        let items = open_header_menu(&mut h, 260);
        assert_eq!(items.len(), 8);
        assert_eq!(enabled(&items, 3), Some(true));
        assert!(matches!(&items[7], MenuItem::Submenu { items, .. } if items.len() == 2));
        pick(&mut h, 3);
        assert!(!t(&h).columns()[1].visible);
        assert!(outs(&mut h).contains(&TableOut::ColumnsChanged));
        let dump = h.a11y_dump();
        assert!(!dump.contains("\"Size\"") && dump.contains("\"CPU\""), "{dump}");
    }

    #[test]
    fn non_hideable_is_disabled() {
        let mut h = make(10, TableStyle::FILES, false);
        let items = open_header_menu(&mut h, 100);
        assert_eq!(enabled(&items, 3), Some(false), "the name column cannot hide");
        assert_eq!(enabled(&items, 4), Some(false), "and it is the first: no Move left");
        assert_eq!(enabled(&items, 5), Some(true));
        pick(&mut h, 3);
        assert!(t(&h).columns()[0].visible);
        assert!(!outs(&mut h).contains(&TableOut::ColumnsChanged));
    }

    #[test]
    fn menu_key_on_header_opens_chooser() {
        let mut h = make(10, TableStyle::FILES, false);
        h.key("Tab");
        assert_eq!(h.focused(), Some(header::cell_id(WidgetId(1), 0)));
        h.key("Right");
        assert_eq!(h.focused(), Some(header::cell_id(WidgetId(1), 1)));
        h.key("Apps");
        let m = h.menus().last().expect("the chooser opened");
        let cell = t(&h).cell_rect(1).unwrap();
        assert_eq!(m.at, Anchor::Below(cell));
        assert!(m.items.iter().any(|i| matches!(i, MenuItem::Submenu { items, .. } if items.len() == 2)));
        // Shift+Tab from the rows lands on the header's current cell.
        let mut h = make(10, TableStyle::FILES, false);
        h.key("Tab").key("Tab");
        h.key("Shift+Tab");
        assert_eq!(h.focused(), Some(header::cell_id(WidgetId(1), 0)));
    }

    #[test]
    fn columns_submenu_toggles_visibility() {
        let mut h = make(10, TableStyle::FILES, false);
        open_header_menu(&mut h, 100);
        pick(&mut h, header::CMD_COLUMN_BASE + 2);
        assert!(!t(&h).columns()[2].visible);
        pick(&mut h, header::CMD_COLUMN_BASE + 2);
        assert!(t(&h).columns()[2].visible);
        assert_eq!(outs(&mut h).iter().filter(|o| **o == TableOut::ColumnsChanged).count(), 2);
    }

    #[test]
    fn move_right_reorders_columns() {
        let mut h = make(10, TableStyle::FILES, false);
        open_header_menu(&mut h, 260);
        pick(&mut h, header::CMD_RIGHT);
        let ids: Vec<u16> = t(&h).columns().iter().map(|c| c.id.0).collect();
        assert_eq!(ids, [0, 2, 1]);
        assert!(outs(&mut h).contains(&TableOut::ColumnsChanged));
        open_header_menu(&mut h, 350);
        pick(&mut h, header::CMD_LEFT);
        let ids: Vec<u16> = t(&h).columns().iter().map(|c| c.id.0).collect();
        assert_eq!(ids, [0, 1, 2], "Move left swaps back");
    }

    #[test]
    fn last_visible_column_cannot_hide() {
        let mut h = make(10, TableStyle::FILES, true);
        open_header_menu(&mut h, 260);
        pick(&mut h, 3);
        let items = open_header_menu(&mut h, 100);
        // Name and CPU are visible: Name may hide.
        assert_eq!(enabled(&items, 3), Some(true));
        pick(&mut h, 3);
        assert_eq!(t(&h).columns().iter().filter(|c| c.visible).count(), 1);
        let items = open_header_menu(&mut h, 50);
        assert_eq!(enabled(&items, 3), Some(false), "the last visible column stays");
        let Some(MenuItem::Submenu { items: sub, .. }) = items.last() else { panic!("no Columns submenu") };
        let last_on =
            sub.iter().filter(|i| matches!(i, MenuItem::Custom { checked: Some(true), enabled: false, .. })).count();
        assert_eq!(last_on, 1, "its Columns item is disabled");
        pick(&mut h, 3);
        assert_eq!(t(&h).columns().iter().filter(|c| c.visible).count(), 1);
    }

    #[test]
    fn a11y_rows_carry_position() {
        let mut h = make(300, TableStyle::FILES, false);
        let dump = h.a11y_dump();
        assert!(dump.contains("Table \"Name\""), "{dump}");
        assert!(dump.contains("Row \"item 0\"") && dump.contains("1 of 300"));
        assert!(dump.contains("ColumnHeader \"Size\""));
        h.click(row_pt(5));
        let dump = h.a11y_dump();
        let line = dump.lines().find(|l| l.contains("\"item 5\"")).unwrap();
        assert!(line.contains("selected") && line.contains("6 of 300"), "{line}");
        h.assert_named();
    }

    #[test]
    fn wheel_scrolls_animated_and_is_idle_after() {
        let mut h = make(100, TableStyle::FILES, false);
        h.wheel(row_pt(2), -3.0);
        h.advance(40);
        let mid = t(&h).state.offset;
        assert!(mid > 0 && mid < 84, "in flight: {mid}");
        h.advance(300);
        assert_eq!(t(&h).state.offset, 84, "three rows");
        assert!(h.is_idle());
        h.wheel(row_pt(2), 100.0);
        h.advance(300);
        assert_eq!(t(&h).state.offset, 0, "clamped at the top");
        h.wheel(row_pt(2), 5.0);
        assert!(h.is_idle(), "a wheel that cannot move anything arms no timer");
    }

    #[test]
    fn dragging_a_header_edge_resizes() {
        let mut h = make(10, TableStyle::FILES, false);
        // The Name column is flex; Size ends at x 100 + 200 = 300 in a 400-wide table (Name 200).
        h.drag(PointL { x: 300, y: 14 }, PointL { x: 320, y: 14 });
        assert_eq!(t(&h).columns()[1].width, 120, "the Size column grew by the drag");
        assert!(outs(&mut h).contains(&TableOut::ColumnsChanged));
        // Keyboard: Shift+Right on the focused header cell widens by 8.
        let mut h = make(10, TableStyle::FILES, false);
        h.key("Tab").key("Right");
        let before = t(&h).columns()[1].width;
        h.key("Shift+Right");
        assert_eq!(t(&h).columns()[1].width, before + 8);
    }
    /// A model that gives the first column a hover action and a plain number column.
    struct Act;
    impl ListModel for Act {
        fn len(&self) -> usize {
            5
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(row as u64)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            (key.0 < 5).then_some(key.0 as usize)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Owned(format!("app {row}"))
        }
        fn generation(&self) -> u64 {
            0
        }
    }
    impl TableModel for Act {
        fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
            if col.0 == 0 {
                out.set_text(CellKind::Text, &format!("app {row}"));
                out.action = Some(crate::model::RowAction { label: Cow::Borrowed("Close"), command: CommandId(77) });
            } else {
                out.set_text(CellKind::Text, "x");
            }
        }
    }

    struct Sa {
        t: VirtualTable<Act>,
        outs: Vec<TableOut>,
        cmds: CommandTable,
    }
    impl Screen for Sa {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.t.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(300)));
            avail
        }
        fn paint(&self, cx: &mut PaintCx) {
            self.t.paint(cx);
        }
        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> crate::event::Reply {
            if let Some(o) = self.t.event(cx, ev) {
                self.outs.push(o);
            }
            crate::event::Reply::None
        }
        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    #[test]
    fn row_action_clickable_after_dur_fast() {
        let t = VirtualTable::new(WidgetId(1), Act, cols(false)[..2].to_vec(), SelectionMode::Multi, TableStyle::TASKS);
        let mut h = Harness::new(
            Sa { t, outs: Vec::new(), cmds: CommandTable::new() },
            SizeL { w: 400, h: 300 },
            96,
            Tokens::oled(),
        );
        h.render();
        // TASKS: no header, rows of 28. The first column ends at x 300 (Name flex 200 + Size 100): the button sits at its right end.
        let cell = h.screen().t.cell_rect(0).unwrap();
        let font_w = {
            let r = h.screen().t.row_rect(1).unwrap();
            (r.t, r.b)
        };
        let at = PointL { x: cell.r - 12 - 8 - 12, y: (font_w.0 + font_w.1) / 2 };
        h.hover(at);
        h.advance(50);
        h.click(at);
        assert!(h.screen().outs.iter().all(|o| !matches!(o, TableOut::RowAction(..))), "{:?}", h.screen().outs);
        assert_eq!(h.screen().t.selection().single(), Some(RowKey(1)), "the early click selects the row");
        h.screen_mut().outs.clear();
        h.advance(100);
        h.click(at);
        assert!(h.screen().outs.contains(&TableOut::RowAction(RowKey(1), CommandId(77))), "{:?}", h.screen().outs);
    }

    #[test]
    fn reduced_motion_wheel_jumps_and_is_idle() {
        let mut tk = Tokens::oled();
        tk.motion.dur_fast = 0;
        tk.motion.dur_normal = 0;
        tk.motion.reduced = true;
        let table = VirtualTable::new(WidgetId(1), T::new(100), cols(false), SelectionMode::Multi, TableStyle::FILES);
        let sc = Sc { t: table, outs: Vec::new(), cmds: CommandTable::new() };
        let mut h = Harness::new(sc, SizeL { w: 400, h: 300 }, 96, tk);
        h.wheel(row_pt(2), -3.0);
        assert_eq!(t(&h).state.offset, 84, "one notch reaches its offset at once");
        assert!(h.is_idle(), "and arms no timer");
    }

    #[test]
    fn stale_header_nodes_do_not_panic() {
        let mut h = make(10, TableStyle::FILES, false);
        h.render();
        run(&mut h, |t, cx| {
            // The columns shrink (a page switch) while nodes of the old layout may still be targeted.
            let one = t.columns()[..1].to_vec();
            t.set_columns(cx, one);
            None
        });
        let id = WidgetId(1);
        for action in [A11yAction::Click, A11yAction::ShowContextMenu, A11yAction::Focus] {
            h.input(crate::dispatch::Input::A11y(header::cell_id(id, 2), action));
        }
        h.input(crate::dispatch::Input::A11y(header::handle_id(id, 2), A11yAction::Click));
        assert!(outs(&mut h).is_empty(), "nothing happens for a column that is gone");
        // The pointer on the place the old handles were: no panic.
        h.drag(PointL { x: 300, y: 14 }, PointL { x: 320, y: 14 });
        h.double_click(PointL { x: 300, y: 14 });
    }

    #[test]
    fn set_selection_updates_the_nodes() {
        let mut h = make(10, TableStyle::FILES, false);
        h.render();
        run(&mut h, |t, cx| {
            t.set_selection(cx, select(&[3]));
            None
        });
        h.render();
        let dump = h.a11y_dump();
        let line = dump.lines().find(|l| l.contains("\"item 3\"")).unwrap();
        assert!(line.contains("selected"), "{line}");
    }

    #[test]
    fn hovering_one_row_of_a_run_lights_only_that_row() {
        let mut h =
            of(VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES)
                .with_selection(select(&[1, 2, 3]))
                .with_focused(RowKey(1)));
        h.key("Tab").key("Tab");
        h.hover(row_pt(2));
        let img = h.render();
        // Row 1 holds the focus ring at its left edge: probe at the right edge of the plates.
        assert_eq!(px(&img, 392, 98), 56, "the hovered row");
        assert_eq!(px(&img, 392, 70), 48, "the row above stays at the run's fill");
        assert_eq!(px(&img, 392, 126), 48, "and the row below");
        assert_eq!(px(&img, 395, 84), 56, "square where the hovered row meets the run");
        // An unfocused table: hot still wins over inactive.
        h.set_active(false);
        let img = h.render();
        assert_eq!(px(&img, 392, 98), 56);
        assert_eq!(px(&img, 392, 70), 28);
    }
    #[test]
    fn hover_and_pressed_fills() {
        let mk =
            || of(VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::TASKS));
        // No header: row 1 is y 28..56, its plate inset 8 (x 8..392).
        let mut h = mk();
        h.hover(PointL { x: 150, y: 42 });
        let img = h.render();
        assert_eq!(px(&img, 385, 36), 20, "FILL_HOVER");
        assert!(px(&img, 385, 28) > 20, "the hover rim (RIM_HOVER 18) on top of the fill at the plate edge");
        assert_eq!(px(&img, 385, 100), 0, "a row that is not hot has no plate");
        let mut h = mk();
        h.hover(PointL { x: 150, y: 42 });
        h.input(crate::dispatch::Input::Down(PointP { x: 150, y: 42 }, MouseButton::Left, 1, Mods::NONE));
        assert_eq!(px(&h.render(), 385, 36), 36, "FILL_PRESS while the button is down");
    }

    #[test]
    fn high_contrast_hot_row_text_is_highlight_text() {
        use crate::theme::{HcColors, SysOverlay, ThemeSpec};
        let hc = HcColors {
            window: (0, 0, 0),
            window_text: (255, 255, 255),
            highlight: (26, 235, 255),
            highlight_text: (0, 0, 0),
            gray_text: (63, 242, 63),
        };
        let sys = SysOverlay { high_contrast: Some(hc), reduced_motion: false, text_scale: 1.0 };
        let tokens = Tokens::resolve(&ThemeSpec::oled(), &sys);
        let table = VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES);
        let mut h = Harness::new(
            Sc { t: table, outs: Vec::new(), cmds: CommandTable::new() },
            SizeL { w: 400, h: 300 },
            96,
            tokens,
        );
        h.hover(row_pt(1));
        let img = h.render();
        // Row 1 is y 56..84: the plate is the highlight (blue 255), its text the highlight text (black), not WindowText.
        let dark = (60..80).flat_map(|y| (12..80).map(move |x| (x, y))).filter(|&(x, y)| px(&img, x, y) < 100).count();
        assert!(dark > 20, "the hovered row's text is dark on the highlight plate: {dark}");
        assert_eq!(px(&img, 390, 70), 255, "the plate");
    }

    // ---- rename in place (K2b.7)

    fn begin(h: &mut Harness<Sc>, key: u64, kind: FieldKind) {
        run(h, move |t, cx| {
            assert!(t.begin_rename(cx, RowKey(key), ColId(0), kind));
            None
        });
    }

    #[test]
    fn rename_field_sits_on_the_cell() {
        for dpi in [96, 144] {
            let table =
                VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES);
            let sc = Sc { t: table, outs: Vec::new(), cmds: CommandTable::new() };
            let mut h = Harness::new(sc, SizeL { w: 400, h: 300 }, dpi, Tokens::oled());
            let band = |h: &mut Harness<Sc>| {
                let img = h.render();
                let s = |v: i32| (v * dpi as i32 + 48) / 96;
                let (top, bottom) = (s(28 + 3 * 28), s(28 + 4 * 28));
                img.text_rects
                    .iter()
                    .copied()
                    .filter(|r| r.t >= top && r.b <= bottom && r.l < s(200))
                    .collect::<Vec<_>>()
            };
            let before = band(&mut h);
            assert_eq!(before.len(), 1, "the name is drawn once: {before:?}");
            begin(&mut h, 3, FieldKind::Plain);
            let after = band(&mut h);
            assert_eq!(after, before, "the field's text origin is the cell's text origin ({dpi} dpi)");
            // The same on a table that was never painted before the rename began.
            let table =
                VirtualTable::new(WidgetId(1), T::new(20), cols(false), SelectionMode::Multi, TableStyle::FILES);
            let sc = Sc { t: table, outs: Vec::new(), cmds: CommandTable::new() };
            let mut cold = Harness::new(sc, SizeL { w: 400, h: 300 }, dpi, Tokens::oled());
            begin(&mut cold, 3, FieldKind::Plain);
            assert_eq!(band(&mut cold), before, "cold start, {dpi} dpi");
        }
    }

    #[test]
    fn f2_then_enter_commits() {
        let mut h = make(20, TableStyle::FILES, false);
        focus_body(&mut h);
        h.key("Down").key("Down").key("F2");
        assert_eq!(outs(&mut h).last(), Some(&TableOut::RenameRequested(RowKey(2))));
        begin(&mut h, 2, FieldKind::Plain);
        assert!(t(&h).is_renaming());
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1 ^ RENAME_ID_BIT), 0)));
        h.type_text("renamed").key("Enter");
        assert_eq!(outs(&mut h), [TableOut::RenameCommitted(RowKey(2), "renamed".into())]);
        assert!(!t(&h).is_renaming());
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 3)), "the focus is back on the row");
    }

    #[test]
    fn esc_cancels() {
        let mut h = make(20, TableStyle::FILES, false);
        begin(&mut h, 4, FieldKind::Rename { stem_end: 4 });
        h.type_text("zzz").key("Esc");
        assert_eq!(outs(&mut h), [TableOut::RenameCancelled(RowKey(4))]);
        assert!(!t(&h).is_renaming());
        assert!(h.is_idle(), "no timer of the field survives");
    }

    #[test]
    fn focus_loss_commits() {
        let mut h = make(20, TableStyle::FILES, false);
        begin(&mut h, 4, FieldKind::Plain);
        h.click(row_pt(8));
        let o = outs(&mut h);
        assert!(o.contains(&TableOut::RenameCommitted(RowKey(4), "item 4".into())), "{o:?}");
        assert!(!t(&h).is_renaming());
    }

    #[test]
    fn tab_moves_to_next_row() {
        let mut h = make(20, TableStyle::FILES, false);
        begin(&mut h, 1, FieldKind::Rename { stem_end: 4 });
        h.key("Tab");
        assert_eq!(
            outs(&mut h),
            [TableOut::RenameCommitted(RowKey(1), "item 1".into()), TableOut::RenameRequested(RowKey(2))]
        );
        begin(&mut h, 2, FieldKind::Rename { stem_end: 4 });
        h.key("Shift+Tab");
        assert_eq!(
            outs(&mut h),
            [TableOut::RenameCommitted(RowKey(2), "item 2".into()), TableOut::RenameRequested(RowKey(1))]
        );
        // At the first row, Shift+Tab commits and asks for nothing more.
        begin(&mut h, 0, FieldKind::Rename { stem_end: 4 });
        h.key("Shift+Tab");
        assert_eq!(outs(&mut h), [TableOut::RenameCommitted(RowKey(0), "item 0".into())]);
    }

    #[test]
    fn slow_second_click() {
        let name = |row: i32| PointL { x: 24, y: 28 + row * 28 + 14 };
        let mut h = make(20, TableStyle::FILES, false);
        h.click(name(3));
        outs(&mut h);
        h.advance(800);
        h.click(name(3));
        assert!(outs(&mut h).contains(&TableOut::RenameRequested(RowKey(3))), "a slow second click on the name");
        // Too quick, too slow, or off the name: nothing.
        let mut h = make(20, TableStyle::FILES, false);
        h.click(name(3));
        h.advance(200);
        h.click(name(3));
        assert!(!outs(&mut h).iter().any(|o| matches!(o, TableOut::RenameRequested(_))));
        let mut h = make(20, TableStyle::FILES, false);
        h.click(name(3));
        h.advance(2000);
        h.click(name(3));
        assert!(!outs(&mut h).iter().any(|o| matches!(o, TableOut::RenameRequested(_))));
        let mut h = make(20, TableStyle::FILES, false);
        h.click(name(3));
        h.advance(800);
        h.click(PointL { x: 180, y: 28 + 3 * 28 + 14 });
        assert!(!outs(&mut h).iter().any(|o| matches!(o, TableOut::RenameRequested(_))), "off the name text");
        let mut h = make(20, TableStyle::FILES, false);
        h.click(name(3));
        h.advance(800);
        h.click(name(4));
        assert!(!outs(&mut h).iter().any(|o| matches!(o, TableOut::RenameRequested(_))), "another row");
    }

    #[test]
    fn a_refused_character_shows_the_line_for_three_seconds() {
        let mut h = make(20, TableStyle::FILES, false);
        run(&mut h, |t, _| {
            t.set_rename_refusal(Some(Cow::Borrowed("A name can't contain / here")));
            None
        });
        begin(&mut h, 4, FieldKind::Plain);
        run(&mut h, |t, _| {
            if let Some(f) = t.rename_field_mut() {
                f.set_filter(Some(|c| c == '/'));
            }
            None
        });
        h.type_text("a/b");
        assert!(h.a11y_dump().contains("invalid"), "{}", h.a11y_dump());
        h.advance(2900);
        assert!(h.a11y_dump().contains("invalid"));
        h.advance(300);
        assert!(!h.a11y_dump().contains("invalid"), "the line is gone after 3 s");
        assert!(t(&h).is_renaming());
    }

    #[test]
    fn rename_scrolls_the_row_into_view() {
        let mut h = make(200, TableStyle::FILES, false);
        begin(&mut h, 150, FieldKind::Plain);
        let band = h.dispatcher().frame.iter().find(|n| n.role == Role::TextInput).map(|n| n.rect);
        let r = band.expect("the field has a node over the visible row");
        assert!(r.t >= 28 && r.b <= 300, "{r:?}");
    }

    #[test]
    fn rename_golden_look() {
        let mut h = make(20, TableStyle::FILES, false);
        run(&mut h, |t, cx| {
            let _ = cx;
            t.selection = Selection::Single(RowKey(3));
            None
        });
        begin(&mut h, 3, FieldKind::Rename { stem_end: 4 });
        let img = h.render();
        let r = h.dispatcher().frame.iter().find(|n| n.role == Role::TextInput).map(|n| n.rect).unwrap();
        // The field's plate is opaque FILL_CONTROL over the window colour, not the row's selection plate.
        let inside = img.px[((r.t + r.h() / 2) as u32 * img.w + (r.r - 6) as u32) as usize];
        assert!(inside & 0xFF < 40, "dark field plate over the row: {inside:#x}");
        let rim = img.px[((r.t + r.h() / 2) as u32 * img.w + r.l as u32) as usize] & 0xFF;
        assert!(rim > 100, "the focus rim on the left edge: {rim}");
    }

    /// Row 3 selected and moving by `m`.
    fn moving(m: RowMotion) -> Harness<Sc> {
        let mut h = make(20, TableStyle::FILES, false);
        run(&mut h, move |t, _cx| {
            t.selection = Selection::Single(RowKey(3));
            t.model_mut().motion = (3, m);
            None
        });
        h
    }

    /// The first y at or below the header where column x differs from the empty window colour at the bottom.
    fn plate_top(img: &crate::harness::Image, x: u32) -> u32 {
        let bg = px(img, x, 299);
        (28..299).find(|y| px(img, x, *y) != bg).expect("a plate")
    }

    /// The sum of the blue channel over a rectangle.
    fn ink_sum(img: &crate::harness::Image, x0: u32, x1: u32, y0: u32, y1: u32) -> u64 {
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| u64::from(px(img, x, y))).sum()
    }

    #[test]
    fn dy_moves_paint_not_hit() {
        let mut h = moving(RowMotion::NONE);
        let base = plate_top(&h.render(), 390);
        assert_eq!(base, 28 + 3 * 28);
        let mut h = moving(RowMotion { dy: 10, opacity: 255 });
        let img = h.render();
        assert_eq!(plate_top(&img, 390), base + 10, "the plate is painted 10 px lower");
        assert_eq!(t(&h).row_rect(3).map(|r| r.t), Some(28 + 3 * 28), "the node rect is unmoved");
        // The strip the plate left is row 3's: a click there still targets row 3; the strip it moved over is row 4's.
        h.click(PointL { x: 250, y: 28 + 3 * 28 + 4 });
        assert!(!t(&h).selection().contains(RowKey(4)));
        assert!(t(&h).selection().contains(RowKey(3)));
        h.click(PointL { x: 250, y: 28 + 4 * 28 - 4 });
        assert!(t(&h).selection().contains(RowKey(3)), "the painted plate does not take the click");
        h.click(PointL { x: 250, y: 28 + 4 * 28 + 4 });
        assert!(t(&h).selection().contains(RowKey(4)));
    }

    #[test]
    fn opacity_scales_every_ink() {
        let full = moving(RowMotion::NONE).render();
        let half = moving(RowMotion { dy: 0, opacity: 128 }).render();
        let y = 28 + 3 * 28 + 4;
        let (a, b) = (px(&full, 390, y), px(&half, 390, y));
        assert!(a > 0 && (i64::from(b) - (i64::from(a) * 128 + 127) / 255).abs() <= 1, "plate {a} -> {b}");
        // Text (name cell) and heat (CPU cell): the ink over the plate halves, within 3 %.
        for (x0, x1) in [(30u32, 190u32), (300, 398)] {
            let (rows0, rows1) = (28 + 3 * 28, 28 + 4 * 28);
            let (sf, sh) = (ink_sum(&full, x0, x1, rows0, rows1), ink_sum(&half, x0, x1, rows0, rows1));
            let want = sf * 128 / 255;
            assert!(sh.abs_diff(want) * 100 <= want * 3, "cells {x0}..{x1}: {sf} -> {sh}, want about {want}");
        }
        // Rows that do not move are untouched.
        assert_eq!(ink_sum(&full, 0, 400, 28, 28 + 3 * 28), ink_sum(&half, 0, 400, 28, 28 + 3 * 28));
    }

    #[test]
    fn row_height_keeps_focus_y() {
        let mut h = make(100, TableStyle::FILES, false);
        run(&mut h, |t, cx| {
            t.set_focused(cx, RowKey(40), ScrollTo::Center);
            None
        });
        h.render();
        let y0 = t(&h).row_rect(40).expect("row 40 is in view").t;
        run(&mut h, |t, cx| {
            t.set_row_height(cx, Some(24));
            None
        });
        h.render();
        let r = t(&h).row_rect(40).expect("row 40 is in view");
        assert_eq!((r.t, r.h()), (y0, 24), "the focused row keeps its y at the new height");
        run(&mut h, |t, cx| {
            t.set_row_height(cx, None);
            None
        });
        h.render();
        let r = t(&h).row_rect(40).expect("row 40 is in view");
        assert_eq!((r.t, r.h()), (y0, 28));
    }

    /// A table of eight rows with icons; row 5 dims its icon. Its motion is the new path, always unmoved.
    struct Pr {
        icon: crate::w3::IconBitmap,
    }

    impl ListModel for Pr {
        fn len(&self) -> usize {
            8
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(row as u64)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            (key.0 < 8).then_some(key.0 as usize)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Owned(format!("item {row}"))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Pr {
        fn icon(&self, _: crate::model::IconRef, _: i32) -> Option<&crate::w3::IconBitmap> {
            Some(&self.icon)
        }
        fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
            if col.0 == 0 {
                out.set_text(CellKind::Text, "");
                out.icon = Some(crate::model::IconRef(0));
                out.dim_icon = row == 5;
            }
        }
        fn motion(&self, _row: usize) -> RowMotion {
            RowMotion::NONE
        }
    }

    struct Sp {
        t: VirtualTable<Pr>,
        cmds: CommandTable,
    }

    impl Screen for Sp {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.t.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(300)));
            avail
        }
        fn paint(&self, cx: &mut PaintCx) {
            self.t.paint(cx)
        }
        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> crate::event::Reply {
            let _ = self.t.event(cx, ev);
            crate::event::Reply::None
        }
        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    /// An unmoved table, probed pixel by pixel with the values the code painted before row motion existed: the
    /// merged selection run of three rows (ends and middle), the hovered row, a dimmed and a lit icon, the focus ring.
    #[test]
    fn unmoved_table_paints_as_before_motion() {
        let icon = crate::w3::IconBitmap { width: 16, height: 16, pixels: vec![0xFFFF_FFFF; 256] };
        let t = VirtualTable::new(WidgetId(1), Pr { icon }, cols(false), SelectionMode::Multi, TableStyle::TASKS)
            .with_selection(select(&[2, 3, 4]))
            .with_focused(RowKey(3));
        let mut h = Harness::new(Sp { t, cmds: CommandTable::new() }, SizeL { w: 400, h: 300 }, 96, Tokens::oled());
        // Tab shows the focus ring on row 3; the pointer rests on row 6.
        h.key("Tab").hover(PointL { x: 150, y: 6 * 28 + 14 });
        let img = h.render();
        // The run of rows 2..4 (y 56..140) is one plate: the same fill at its top, middle and bottom row, nothing outside.
        assert_eq!((px(&img, 30, 55), px(&img, 30, 56), px(&img, 30, 70)), (0, 28, 28), "run: top end");
        assert_eq!((px(&img, 30, 111), px(&img, 30, 125), px(&img, 30, 139)), (28, 28, 28), "run: bottom row");
        assert_eq!(px(&img, 30, 140), 0, "run: below its end");
        assert_eq!((px(&img, 12, 56), px(&img, 12, 57), px(&img, 12, 139)), (15, 28, 15), "run: rounded ends");
        // The ring on row 3 (y 84..112) is drawn over the run, and the row inside it keeps the run's fill.
        assert_eq!((px(&img, 30, 85), px(&img, 30, 86), px(&img, 30, 95)), (170, 170, 28), "ring: top edge");
        assert_eq!((px(&img, 9, 98), px(&img, 10, 98)), (170, 247), "ring: left edge");
        // The hovered row 6 is its own plate at the hover fill.
        assert_eq!((px(&img, 30, 167), px(&img, 30, 180)), (0, 20), "hovered row");
        // The icon of row 5 is dimmed to half; a lit icon is not.
        assert_eq!(px(&img, 40, 154), 128, "dimmed icon");
        assert_eq!((px(&img, 40, 70), px(&img, 40, 126), px(&img, 40, 182)), (255, 255, 255), "lit icons");
    }
}
