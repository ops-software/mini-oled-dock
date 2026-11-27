//! Home (P3-files-app 2.2; FD-SIDE fills it, F4.10): drives, network locations and pins as one page.

use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::keys::Cmd;
use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct Home;

impl Home {
    pub fn new() -> Home {
        Home
    }
}

/// Filled by FD-SIDE; until then an empty page.
pub fn home_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FD-SIDE; until then an empty page.
pub fn home_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Filled by FD-SIDE; until then not taken.
pub fn home_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// Home. Filled by FD-SIDE; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
