//! The sidebar (P3-files-app 2.2; FD-SIDE fills it, F4.9): pins, This PC, drives, the folder tree; it owns its
//! SplitView.

use oled_trust::frame::files::Location;
use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::window::{FilesScreen, Shared};

#[derive(Debug, Default)]
pub struct Sidebar;

impl Sidebar {
    pub fn new() -> Sidebar {
        Sidebar
    }
}

/// The body rect beside the sidebar (folded below 760 px or when hidden). Filled by FD-SIDE; until then `r` (the
/// sidebar takes no width).
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    let _ = (s, cx);
    r
}

/// Filled by FD-SIDE; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// SidebarOut. Filled by FD-SIDE; until then not taken.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// The tree's children. Filled by FD-SIDE; until then false (not ours).
pub fn on_children(
    s: &mut FilesScreen,
    cx: &mut EventCx,
    req: u64,
    parent: &WidePath,
    dirs: &[Box<str>],
    done: bool,
) -> bool {
    let _ = (s, cx, req, parent, dirs, done);
    false
}

/// network_recent for a UNC path, the current row. Filled by FD-SIDE; until then nothing.
pub fn on_navigated(shared: &Shared, to: &Location) {
    let _ = (shared, to);
}

/// Drives or pins changed. Filled by FD-SIDE; until then nothing.
pub fn on_bump(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// ExpandTree, Sidebar. Filled by FD-SIDE; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
