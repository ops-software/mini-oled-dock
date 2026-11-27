//! About (P3-files-app 2.2; FD-INST fills it, F4.15).

use oled_ui::{EventCx, Reply};

use crate::keys::Cmd;
use crate::window::FilesScreen;

/// About. Filled by FD-INST; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
