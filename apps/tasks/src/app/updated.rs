//! After an update: the installed it.lang (TS2) and the registered "mini-oled-dock.Updated" message with its notice
//! (TS3). Filled by TA-SUITE.
#![allow(dead_code, unused_variables)] // stub: removed by TA-SUITE

use std::path::Path;

use oled_ui::{CommandId, EventCx, Reply};

use crate::ui::notice::Notices;
use crate::ui::screen::TasksScreen;

/// <install dir>\lang\it.lang for the installed image with the suite version. TS2. Stub: nothing (the compiled
/// Italian table serves).
pub fn load_lang_file(install_dir: Option<&Path>, italian: bool) {}

#[derive(Default)]
pub struct UpdState;

/// A routed registered message (Screen::app_message, added by TK1). TS3. Stub: Reply::None.
pub fn on_message(cx: &mut EventCx, st: &mut UpdState, n: &mut Notices, msg: u32, wparam: usize) -> Reply {
    Reply::None
}

/// RESTART_TO_UPDATE. TS3. Stub: nothing.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {}
