//! The status line (P3-files-app 2.2; FA-WIN fills it, F1a.9): counts, the selection's size, free space, and the
//! notice every group shows through notice().

use oled_trust::frame::files::OpId;
use oled_ui::node::Text;
use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, Reply};

use crate::window::FilesScreen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeAct {
    Retry,
    OpenInExplorer,
    Reopen,
    Rest(OpId),
    Undo(OpId),
    OpenRecycleBin,
    Details(OpId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: Text,
    pub action: Option<(Text, NoticeAct)>,
    /// 6,000 by default.
    pub hold_ms: u32,
}

/// How long a notice stays by default.
pub const NOTICE_HOLD_MS: u32 = 6_000;

impl Notice {
    /// A notice with no action, held NOTICE_HOLD_MS.
    pub fn new(text: Text) -> Notice {
        Notice { text, action: None, hold_ms: NOTICE_HOLD_MS }
    }
}

/// The labels and the notice slot. Filled by FA-WIN.
#[derive(Debug, Default)]
pub struct Status;

impl Status {
    pub fn new() -> Status {
        Status
    }
}

/// Filled by FA-WIN; until then nothing.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FA-WIN; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Shows `n` in place of the items for its hold or until Esc. Filled by FA-WIN; until then one log line.
pub fn notice(s: &mut FilesScreen, cx: &mut EventCx, n: Notice) {
    let _ = (s, cx);
    oled_sys::log!("files: notice: {}", n.text);
}

/// Filled by FA-WIN; until then nothing.
pub fn selection_changed(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// The notice's action button: a click on it runs on_notice_action with the notice's NoticeAct. Filled by FA-WIN;
/// until then not taken.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// Timer kind 9. Filled by FA-WIN; until then nothing.
pub fn on_notice_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Retry -> window::cmd(Refresh) or the failed request's retry; OpenInExplorer -> open::hand_off_current; Reopen ->
/// notices::reopen; Rest -> ops_ui::on_rest; Undo -> ops_ui::cmd(Undo); OpenRecycleBin -> handoff::hand_off; Details ->
/// ops_ui::flyout_open. Filled by FA-WIN; until then nothing.
pub fn on_notice_action(s: &mut FilesScreen, cx: &mut EventCx, a: NoticeAct) -> Reply {
    let _ = (s, cx, a);
    Reply::None
}
