//! The inline bar that asks before a permanent delete (P3-files-app 2.2; FC-OPS fills it, F3.2, U3).

use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct RecycleBar;

impl RecycleBar {
    pub fn new() -> RecycleBar {
        RecycleBar
    }

    /// Filled by FC-OPS; until then false.
    pub fn is_open(&self) -> bool {
        false
    }
}

/// Filled by FC-OPS; until then nothing.
pub fn bar_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FC-OPS; until then nothing is drawn.
pub fn bar_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Takes the keyboard while open. Filled by FC-OPS; until then not taken.
pub fn bar_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}
