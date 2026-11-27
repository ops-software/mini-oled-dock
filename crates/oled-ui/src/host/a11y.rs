//! The AccessKit adapter's window side (K2b.9, KG): created before the first WM_GETOBJECT, never inside it.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::window::WinHost;
use crate::node::NodeId;

/// After creation. Until KG: nothing.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// After every layout: the tree update. Until KG: nothing.
pub(crate) fn after_layout(host: &mut WinHost) {
    let _ = host;
}

/// The focus moved. Until KG: nothing.
pub(crate) fn on_focus_changed(host: &mut WinHost, node: Option<NodeId>) {
    let _ = (host, node);
}

/// WM_DESTROY. Until KG: nothing.
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// WM_GETOBJECT. Until KG: None (DefWindowProcW).
pub(crate) fn on_getobject(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = (hwnd, msg, wp, lp);
    None
}

/// WM_OLED_A11Y: an action marshalled to the UI thread. Until KG: 0.
pub(crate) fn on_action(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (hwnd, msg, wp, lp);
    LRESULT(0)
}
