//! The toolbar's menus (P3-files-app 2.2; FB-MENU fills it, F2.6): New, Sort, View and More.

use oled_ui::{Anchor, EventCx, Reply};

use crate::keys::Cmd;
use crate::window::FilesScreen;

/// Filled by FB-MENU; until then no menu.
pub fn open_new(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let _ = (s, cx, at);
    Reply::None
}

/// Filled by FB-MENU; until then no menu.
pub fn open_sort(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let _ = (s, cx, at);
    Reply::None
}

/// Filled by FB-MENU; until then no menu.
pub fn open_view(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let _ = (s, cx, at);
    Reply::None
}

/// Filled by FB-MENU; until then no menu.
pub fn open_more(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let _ = (s, cx, at);
    Reply::None
}

/// HiddenItems. Filled by FB-MENU; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
