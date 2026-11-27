//! Hand-offs to File Explorer (P3-files-app 2.2; FD-INST fills it, F4.13): locations this window does not list.

use oled_ui::{EventCx, Reply};

use crate::listing::WidePath;
use crate::status::{self, Notice};
use crate::tr::{self, k};
use crate::window::FilesScreen;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    RecycleBin,
    Network,
    ThisPc,
    Other(String),
    Folder(WidePath),
}

/// Filled by FD-INST; until then the not_in_beta notice (as open::hand_off_current).
pub fn hand_off(s: &mut FilesScreen, cx: &mut EventCx, t: Target, typed: bool) -> Reply {
    let _ = (t, typed);
    let text = tr::t(k::FILES_MENU_NOT_IN_BETA, cx.lang());
    status::notice(s, cx, Notice::new(text));
    Reply::Redraw
}
