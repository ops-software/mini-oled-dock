//! Rename in place and new folder (P3-files-app 2.2; FC-RENAME fills it, F3.7, F3.8): the field sits on the cell;
//! every change goes through ops_ui::request.

use oled_trust::frame::files::OpId;
use oled_ui::model::RowKey;
use oled_ui::{Event, EventCx, Reply};

use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct RenameState;

impl RenameState {
    pub fn new() -> RenameState {
        RenameState
    }

    /// Filled by FC-RENAME; until then false.
    pub fn active(&self) -> bool {
        false
    }
}

/// Filled by FC-RENAME; until then nothing (the kit opens no field).
pub fn begin(s: &mut FilesScreen, cx: &mut EventCx, k: RowKey) -> Reply {
    let _ = (s, cx, k);
    Reply::None
}

/// The menu's Rename and an intercepted Rename. Filled by FC-RENAME; until then nothing.
pub fn begin_path(s: &mut FilesScreen, cx: &mut EventCx, p: &WidePath) -> Reply {
    let _ = (s, cx, p);
    Reply::None
}

/// Filled by FC-RENAME; until then nothing.
pub fn commit(s: &mut FilesScreen, cx: &mut EventCx, k: RowKey, text: String) -> Reply {
    let _ = (s, cx, k, text);
    Reply::None
}

/// Filled by FC-RENAME; until then nothing.
pub fn cancel(s: &mut FilesScreen, cx: &mut EventCx, k: RowKey) -> Reply {
    let _ = (s, cx, k);
    Reply::None
}

/// F2 cycling while a field is open. Filled by FC-RENAME; until then not taken.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// Filled by FC-RENAME; until then nothing.
pub fn new_folder(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let _ = (s, cx);
    Reply::None
}

/// A Created change of this window's new-folder operation: select, scroll, begin its rename. Filled by FC-RENAME;
/// until then false.
pub fn on_created(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, path: &WidePath) -> bool {
    let _ = (s, cx, op, path);
    false
}

/// Rename, NewFolder. Filled by FC-RENAME; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
