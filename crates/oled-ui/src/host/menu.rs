//! Menu and list popup windows (K2a.9, KC2): non-modal owned layered windows; the owner keeps activation.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, WPARAM};

use super::window::WinHost;
use crate::command::CommandTable;
use crate::cx::{Anchor, Env, MenuId, MenuItem, Res};
use crate::harness::{Image, MenuSnapshot};
use crate::node::{NodeId, Text};

/// WM_ACTIVATEAPP: another app took the activation, the menus close. Until KC2: nothing.
pub(crate) fn on_activate_app(hwnd: HWND, wp: WPARAM) {
    let _ = (hwnd, wp);
}

/// The owner is destroyed: its menus go too. Until KC2: nothing.
pub(crate) fn on_owner_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// Effect::OpenMenu. Until KC2: none opened.
pub(crate) fn open(host: &mut WinHost, id: MenuId, items: Vec<MenuItem>, at: Anchor, opener: Option<NodeId>) {
    let _ = (host, id, items, at, opener);
}

/// Effect::OpenList. Until KC2: none opened.
pub(crate) fn open_list(host: &mut WinHost, id: MenuId, items: Vec<Text>, at: Anchor, highlight: Option<usize>) {
    let _ = (host, id, items, at, highlight);
}

/// Effect::UpdateList. Until KC2: nothing.
pub(crate) fn update_list(host: &mut WinHost, id: MenuId, items: Vec<Text>, highlight: Option<usize>) {
    let _ = (host, id, items, highlight);
}

/// Effect::CloseMenus. Until KC2: nothing.
pub(crate) fn close_all(host: &mut WinHost) {
    let _ = host;
}

/// Harness::menu_image: the menu as its window would draw it. Until KC2: None.
pub(crate) fn snapshot_image(snap: &MenuSnapshot, cmds: &CommandTable, env: Env<'_>, res: &mut Res) -> Option<Image> {
    let _ = (snap, cmds, env.scale, res);
    None
}
