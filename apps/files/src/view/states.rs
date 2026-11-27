//! The page states over the list (P3-files-app 2.2; FA-WIN fills it, F1a.9): empty, denied, offline, connecting and
//! the rest, with their banner buttons.

use oled_ui::{Event, EventCx, PaintCx, RectP, Reply};

use crate::nav::Page;
use crate::window::FilesScreen;

/// Filled by FA-WIN; until then nothing is drawn.
pub fn paint_state(s: &FilesScreen, page: &Page, r: RectP, cx: &mut PaintCx) {
    let _ = (s, page, r, cx);
}

/// The banners' buttons. Filled by FA-WIN; until then not taken.
pub fn state_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}
