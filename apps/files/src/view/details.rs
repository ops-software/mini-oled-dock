//! The details table's layout, paint and sorting (P3-files-app 2.2; FA-WIN, F1a.8).

use oled_ui::model::{ColId, Column, ListModel, SortDir, TableModel};
use oled_ui::theme::TypeRole;
use oled_ui::{EventCx, LayoutCx, PaintCx, RectP, Reply, Widget};

use crate::bg::Lane;
use crate::listing::{Gen, SortDone, TabId};
use crate::model::ColKind;
use crate::msg::JobDone;
use crate::view::columns::sort_key;
use crate::window::FilesScreen;

/// Rows up to this many sort on the UI thread.
pub const SORT_INLINE_MAX: u32 = 20_000;
/// Logical px added to the widest cell when a column is sized to fit.
const FIT_PAD: i32 = 24;

fn kind_of(c: ColId) -> Option<ColKind> {
    [
        ColKind::Name,
        ColKind::Modified,
        ColKind::Type,
        ColKind::Size,
        ColKind::Created,
        ColKind::Attributes,
        ColKind::Status,
    ]
    .into_iter()
    .find(|k| *k as u16 == c.0)
}

/// The listing's sort direction of the kit's.
pub fn dir_of(d: SortDir) -> crate::listing::SortDir {
    match d {
        SortDir::Asc => crate::listing::SortDir::Asc,
        SortDir::Desc => crate::listing::SortDir::Desc,
    }
}

/// The columns with the sort chevron on `kind` only.
pub fn columns_with_sort(cols: &[Column], kind: ColKind, dir: SortDir) -> Vec<Column> {
    cols.iter().map(|c| Column { sort: (c.id.0 == kind as u16).then_some(dir), ..c.clone() }).collect()
}

/// The active tab's table in `r`.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    s.tabs.active_mut().table.layout(cx, r);
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    s.tabs.active().table.paint(cx);
}

/// Rows up to 20,000 sort on the UI thread, more on the Bg Local lane.
pub fn sort(s: &mut FilesScreen, cx: &mut EventCx, col: ColId, dir: SortDir) -> Reply {
    let Some(kind) = kind_of(col) else { return Reply::None };
    let Some(key) = sort_key(kind) else { return Reply::None };
    let win = s.win;
    let id = s.tabs.active().id;
    s.sort_chosen.insert(id);
    let t = s.tabs.active_mut();
    t.sort = (kind, dir);
    let cols = columns_with_sort(t.table.columns(), kind, dir);
    if t.table.model().listing().len() <= SORT_INLINE_MAX {
        t.table.model_mut().listing_mut().sort(key, dir_of(dir));
        t.table.set_columns(cx, cols);
        t.table.model_changed(cx, true);
        return Reply::Relayout;
    }
    // The old order stays drawn while the job runs.
    let job = t.table.model().listing().sort_job(key, dir_of(dir));
    let (tab, gen) = (t.id, t.table.model().listing().gen());
    t.table.set_columns(cx, cols);
    s.shared.services.bg.run(win, Lane::Local, Box::new(move || Some(JobDone::Sorted { tab, gen, done: job.run() })));
    Reply::Relayout
}

/// A background sort finished: adopted when the listing is still the one it was cut from.
pub fn on_sorted(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, gen: Gen, done: SortDone) {
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    if t.table.model().listing().gen() != gen {
        return;
    }
    t.table.model_mut().listing_mut().adopt(done);
    t.table.model_changed(cx, true);
}

/// The user resized, hid or reordered a column: nothing to keep beyond the table's own state in F1a.
pub fn columns_changed(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Each visible column's width becomes the widest cell text of the rows now in view plus a padding.
pub fn fit_columns(s: &mut FilesScreen, cx: &mut EventCx) {
    let scale = cx.scale();
    let font = cx.text().font(TypeRole::Body, None, scale, 1.0);
    let t = s.tabs.active_mut();
    let rows = t.table.visible_rows();
    let mut cols = t.table.columns().to_vec();
    let mut out = oled_ui::model::CellOut::default();
    for c in cols.iter_mut().filter(|c| c.visible) {
        let mut widest = cx.text().width(&c.title, font);
        for row in rows.clone() {
            if row >= ListModel::len(t.table.model()) {
                break;
            }
            out.clear();
            t.table.model().cell(row, c.id, &mut out);
            widest = widest.max(cx.text().width(&out.text, font));
        }
        c.width = (scale.logical(widest) + FIT_PAD).max(c.min);
    }
    t.table.set_columns(cx, cols);
}
