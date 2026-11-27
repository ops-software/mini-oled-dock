//! Our context menu and the system menu (P3-files-app 2.2; FB-MENU fills it, F2.5): our first level never waits on
//! the shell host; the system menu is requested only from Shift+F10, Shift+right-click or "Show more options".

use oled_ui::model::RowKey;
use oled_ui::{CommandId, EventCx, PointP, Reply};

use crate::keys::Cmd;
use crate::shell_client::ShellReply;
use crate::window::FilesScreen;

/// Filled by FB-MENU; until then no menu.
pub fn open_context(s: &mut FilesScreen, cx: &mut EventCx, row: Option<RowKey>, at: PointP) -> Reply {
    let _ = (s, cx, row, at);
    Reply::None
}

/// Filled by FB-MENU; until then no menu.
pub fn open_system(s: &mut FilesScreen, cx: &mut EventCx, at: PointP) -> Reply {
    let _ = (s, cx, at);
    Reply::None
}

/// A pick of our menus that is not a key-table command. Filled by FB-MENU; until then nothing.
pub fn on_pick(s: &mut FilesScreen, cx: &mut EventCx, id: CommandId) -> Reply {
    let _ = (s, cx, id);
    Reply::None
}

/// Filled by FB-MENU; until then nothing.
pub fn on_menu_reply(s: &mut FilesScreen, cx: &mut EventCx, reply: &ShellReply) {
    let _ = (s, cx, reply);
}

/// OurMenu, SystemMenu, CopyPath. Filled by FB-MENU; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
