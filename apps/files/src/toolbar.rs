//! The toolbar (P3-files-app 2.2; FA-WIN fills it, F1a.8): Back, Forward, Up, the location area and the right
//! cluster (New, Sort, View, More).

use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::window::FilesScreen;

/// Back, Forward, Up IconButtons, the location area (F1a: a TextLabel; F1b: pathline), the right cluster.
#[derive(Debug, Default)]
pub struct Toolbar;

impl Toolbar {
    pub fn new() -> Toolbar {
        Toolbar
    }
}

/// Calls pathline::layout for the location area once chrome::has_strip or pathline exists. Filled by FA-WIN; until
/// then nothing.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FA-WIN; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Back, Forward, Up -> keys::run; the right cluster -> toolmenus::open_*; a right click or the Menu key over Back or
/// Forward -> tab::history_menu. Filled by FA-WIN; until then not taken.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}
