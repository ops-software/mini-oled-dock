//! The linger (P3-files-app 2.2; FD-INST fills it, F4.12): the last window hides instead of closing, holds no
//! listing and one 10-minute one-shot timer.

use oled_trust::frame::files::InstanceCmd;
use oled_ui::{EventCx, Reply};

use crate::window::FilesScreen;

/// This is the last window. Filled by FD-INST; until then false (it closes).
pub fn on_last_close(s: &mut FilesScreen, cx: &mut EventCx) -> bool {
    let _ = (s, cx);
    false
}

/// Timer kind 7. Filled by FD-INST; until then nothing.
pub fn on_linger_timer(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let _ = (s, cx);
    Reply::None
}

/// Filled by FD-INST; until then false.
pub fn on_instance(s: &mut FilesScreen, cx: &mut EventCx, cmd: &InstanceCmd) -> bool {
    let _ = (s, cx, cmd);
    false
}
