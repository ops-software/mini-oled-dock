//! The details table's layout, paint and sorting (P3-files-app 2.2; FA-WIN fills it, F1a.8).

use oled_ui::model::{ColId, SortDir};
use oled_ui::{EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::listing::{Gen, SortDone, TabId};
use crate::window::FilesScreen;

/// The active tab's table in `r`. Filled by FA-WIN; until then nothing.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FA-WIN; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Rows up to 20,000 sort on the UI thread, more on the Bg Local lane. Filled by FA-WIN; until then nothing.
pub fn sort(s: &mut FilesScreen, cx: &mut EventCx, col: ColId, dir: SortDir) -> Reply {
    let _ = (s, cx, col, dir);
    Reply::None
}

/// A background sort finished. Filled by FA-WIN; until then ignored.
pub fn on_sorted(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, gen: Gen, done: SortDone) {
    let _ = (s, cx, tab, gen, done);
}

/// Filled by FA-WIN; until then nothing.
pub fn columns_changed(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}
