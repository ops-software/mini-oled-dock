//! The updated notice and Reopen (P3-files-app 2.2; FD-INST fills it, F4.14): never restarts on its own.

use oled_trust::frame::files::Location;
use oled_ui::{EventCx, Reply};

use crate::session::Session;
use crate::window::FilesScreen;

/// "<name> was updated · Reopen". Filled by FD-INST; until then nothing.
pub fn on_updated(s: &mut FilesScreen, cx: &mut EventCx, version: u32) {
    let _ = (s, cx, version);
}

/// Filled by FD-INST; until then nothing.
pub fn reopen(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let _ = (s, cx);
    Reply::None
}

/// app.rs's Step::Reopen. Filled by FD-INST; until then none (the start opens its own target).
pub fn take_reopen(session: &mut Session) -> Vec<Vec<Location>> {
    let _ = session;
    Vec::new()
}
