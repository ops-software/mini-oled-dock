//! The column header of the virtual table (K2a.5, menu K2a.6): cell geometry, painting and the header menu's items.

use crate::command::CommandId;
use crate::cx::{MenuItem, PaintCx};
use crate::geom::{RectP, Scale};
use crate::glyph::Glyph;
use crate::model::{Align, Column, SortDir};
use crate::node::{NodeId, WidgetId};
use crate::strings::{k, UiLang};
use crate::text::Trim;
use crate::theme::TypeRole;

/// A header cell's node slot is this plus the column's index in the columns Vec.
pub(crate) const CELL_SLOT: u32 = 0x4000_0000;
/// A column's resize handle's node slot is this plus the column's index.
pub(crate) const HANDLE_SLOT: u32 = 0x6000_0000;

/// Header menu items: the kit command numbers.
pub(crate) const CMD_SORT_ASC: u16 = 1;
pub(crate) const CMD_SORT_DESC: u16 = 2;
pub(crate) const CMD_HIDE: u16 = 3;
pub(crate) const CMD_LEFT: u16 = 4;
pub(crate) const CMD_RIGHT: u16 = 5;
/// The Columns submenu's items are this plus the column's index.
pub(crate) const CMD_COLUMN_BASE: u16 = 16;

/// A visible column's header cell.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeaderCell {
    /// The column's index in the columns Vec.
    pub col: usize,
    pub rect: RectP,
    /// The resize zone: COL_RESIZE_HIT wide, centred on the cell's right edge.
    pub handle: RectP,
}

/// The cells of the visible columns laid left to right from `area.l`; `widths` are the columns' physical widths (0 for a
/// hidden one).
pub(crate) fn cells(widths: &[i32], area: RectP, hit: i32) -> Vec<HeaderCell> {
    let mut x = area.l;
    let mut out = Vec::new();
    for (col, w) in widths.iter().enumerate() {
        if *w <= 0 {
            continue;
        }
        let rect = RectP { l: x, t: area.t, r: x + w, b: area.b };
        let handle = RectP { l: rect.r - hit / 2, t: area.t, r: rect.r + hit / 2, b: area.b };
        out.push(HeaderCell { col, rect, handle });
        x += w;
    }
    out
}

pub(crate) fn cell_id(owner: WidgetId, col: usize) -> NodeId {
    NodeId::new(owner, CELL_SLOT + col as u32)
}

pub(crate) fn handle_id(owner: WidgetId, col: usize) -> NodeId {
    NodeId::new(owner, HANDLE_SLOT + col as u32)
}

/// What a header node's slot is: Some(Ok(col)) a cell, Some(Err(col)) a resize handle.
pub(crate) fn slot_col(slot: u32) -> Option<Result<usize, usize>> {
    if (HANDLE_SLOT..HANDLE_SLOT + 0x1000_0000).contains(&slot) {
        Some(Err((slot - HANDLE_SLOT) as usize))
    } else if (CELL_SLOT..CELL_SLOT + 0x1000_0000).contains(&slot) {
        Some(Ok((slot - CELL_SLOT) as usize))
    } else {
        None
    }
}

/// Paints the header: a plate per hot or pressed cell, the titles, the sort chevron, the column separators while the
/// header is hovered, and the line under it.
pub(crate) fn paint(
    cx: &mut PaintCx,
    owner: WidgetId,
    cells: &[HeaderCell],
    columns: &[Column],
    header: RectP,
    focus_col: Option<usize>,
) {
    let scale: Scale = cx.scale();
    let radius = cx.px(cx.tokens().radius.s);
    let c = cx.tokens().color.clone();
    let hovered = cells.iter().any(|h| cx.hot(cell_id(owner, h.col)) || cx.hot(handle_id(owner, h.col)));
    for h in cells {
        let col = &columns[h.col];
        let id = cell_id(owner, h.col);
        let (pressed, hot) = (cx.pressed(id), cx.hot(id));
        if pressed {
            cx.fill(h.rect, radius, c.fill_press);
        } else if hot {
            cx.fill(h.rect, radius, c.fill_hover);
        }
        // Under high contrast the hover and press plates are the highlight colour: its text is the highlight text.
        let ink = if cx.tokens().high_contrast && (pressed || hot) { c.fg_hot } else { c.fg_secondary };
        let pad = cx.px(12);
        let sorted = col.sort.is_some();
        let chev = scale.px(8);
        let gap = scale.px(4);
        let reserve = if sorted { chev + gap } else { 0 };
        let text_r =
            RectP { l: h.rect.l + pad, t: h.rect.t, r: (h.rect.r - pad - reserve).max(h.rect.l + pad), b: h.rect.b };
        let drawn = cx.text_in(&col.title, TypeRole::Caption, text_r, Align::Start, Trim::End, ink);
        if let Some(dir) = col.sort {
            let x = (text_r.l + drawn + gap).min(h.rect.r - pad - chev);
            let g = RectP::wh(x, h.rect.t + (h.rect.h() - chev) / 2, chev, chev);
            cx.glyph(g, if dir == SortDir::Asc { Glyph::SortUp } else { Glyph::SortDown }, ink);
        }
        if focus_col == Some(h.col) && cx.focused(id) {
            cx.focus_ring(h.rect, radius, true);
        }
    }
    if hovered {
        let inset = cx.px(6);
        for h in cells.iter().filter(|h| h.rect.r < header.r) {
            cx.vline(h.rect.r - 1, header.t + inset, header.b - inset, c.separator);
        }
    }
    cx.hline(header.l, header.r, header.b - 1, c.separator);
}

/// The header menu of column `col` (an index in `columns`): sort, hide, move, and the Columns submenu.
pub(crate) fn menu_items(owner: WidgetId, columns: &[Column], col: usize, lang: UiLang) -> Vec<MenuItem> {
    let visible: Vec<usize> = (0..columns.len()).filter(|&i| columns[i].visible).collect();
    let pos = visible.iter().position(|&i| i == col);
    let id = |n: u16| CommandId::kit(owner, n);
    let item = |n: u16, label: crate::node::Text, enabled: bool, checked: Option<bool>| MenuItem::Custom {
        id: id(n),
        label,
        glyph: None,
        chord: None,
        enabled,
        checked,
        note: None,
        danger: false,
    };
    let can_hide = columns[col].hideable && visible.len() > 1;
    let mut columns_menu = Vec::new();
    for (i, c) in columns.iter().enumerate().filter(|(_, c)| c.hideable) {
        let last = c.visible && visible.len() <= 1;
        columns_menu.push(item(CMD_COLUMN_BASE + i as u16, c.title.clone(), !last, Some(c.visible)));
    }
    vec![
        item(CMD_SORT_ASC, lang.kit(k::KIT_SORT_ASCENDING), true, None),
        item(CMD_SORT_DESC, lang.kit(k::KIT_SORT_DESCENDING), true, None),
        MenuItem::Separator,
        item(CMD_HIDE, lang.kit(k::KIT_HIDE_COLUMN), can_hide, None),
        item(CMD_LEFT, lang.kit(k::KIT_MOVE_LEFT), pos.is_some_and(|p| p > 0), None),
        item(CMD_RIGHT, lang.kit(k::KIT_MOVE_RIGHT), pos.is_some_and(|p| p + 1 < visible.len()), None),
        MenuItem::Separator,
        MenuItem::Submenu { label: lang.kit(k::KIT_COLUMNS), glyph: None, items: columns_menu },
    ]
}
