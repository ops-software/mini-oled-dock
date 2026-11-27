//! The path line (P3-files-app 2.2; FB-CHROME the breadcrumb, FD-PATH the edit, F1b.2 and F4.4-F4.6).

use oled_trust::frame::files::Location;
use oled_ui::node::Text;
use oled_ui::widgets::PathOut;
use oled_ui::{EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::status::{self, Notice};
use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct PathLine;

impl PathLine {
    pub fn new() -> PathLine {
        PathLine
    }
}

/// Filled by FB-CHROME; until then nothing.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FB-CHROME; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Filled by FB-CHROME; until then nothing.
pub fn set_location(s: &mut FilesScreen, cx: &mut EventCx, at: &Location) {
    let _ = (s, cx, at);
}

/// FA-WIN's NotFound. Filled by FD-PATH; until then the text as a notice.
pub fn set_error(s: &mut FilesScreen, cx: &mut EventCx, msg: Option<Text>) {
    if let Some(text) = msg {
        status::notice(s, cx, Notice::new(text));
    }
}

/// Filled by FB-CHROME; until then nothing.
pub fn on_out(s: &mut FilesScreen, cx: &mut EventCx, out: PathOut) -> Reply {
    let _ = (s, cx, out);
    Reply::None
}

/// "\\ / :" typed in the list (F4.6). Filled by FD-PATH; until then false.
pub fn type_into(s: &mut FilesScreen, cx: &mut EventCx, c: char) -> bool {
    let _ = (s, cx, c);
    false
}

/// Filled by FD-PATH; until then false (not ours).
pub fn on_siblings(s: &mut FilesScreen, cx: &mut EventCx, req: u64, dirs: Vec<Box<str>>) -> bool {
    let _ = (s, cx, req, dirs);
    false
}

/// Filled by FD-PATH; until then false.
pub fn on_stat(s: &mut FilesScreen, cx: &mut EventCx, req: u64, path: WidePath, found: Option<bool>) -> bool {
    let _ = (s, cx, req, path, found);
    false
}

/// EditPath, EditPathHistory. Filled by FD-PATH; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
