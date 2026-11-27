//! Path completion (P3-files-app 2.2; FD-PATH fills it, F4.5): children enumerated off the UI thread.

use oled_ui::EventCx;

use crate::listing::WidePath;
use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct Completer;

impl Completer {
    pub fn new() -> Completer {
        Completer
    }
}

/// Filled by FD-PATH; until then nothing.
pub fn request(s: &mut FilesScreen, cx: &mut EventCx, text: &str) {
    let _ = (s, cx, text);
}

/// Filled by FD-PATH; until then false (not ours).
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
