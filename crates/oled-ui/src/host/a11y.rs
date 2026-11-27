//! The AccessKit adapter's window side (K2b.9, KG): each hook hands over to the bridge (a11y/bridge.rs) when the a11y
//! feature is on. The adapter is made right after CreateWindowExW returns (hooks::after_create), before the first
//! WM_GETOBJECT can be handled and never inside it. Without the feature every hook does nothing and WM_GETOBJECT falls
//! through to DefWindowProcW.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::window::WinHost;
#[cfg(feature = "a11y")]
use crate::a11y::bridge;
use crate::node::NodeId;

/// After creation: the window's adapter, lazy until a client asks.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = hwnd;
    #[cfg(feature = "a11y")]
    bridge::on_created(host);
    #[cfg(not(feature = "a11y"))]
    let _ = host;
}

/// After every layout: the client's update, when a client is connected and a node changed.
pub(crate) fn after_layout(host: &mut WinHost) {
    #[cfg(feature = "a11y")]
    bridge::after_layout(host);
    #[cfg(not(feature = "a11y"))]
    let _ = host;
}

/// The focus moved: the client's update (the bridge reads the focus from the dispatcher).
pub(crate) fn on_focus_changed(host: &mut WinHost, node: Option<NodeId>) {
    let _ = node;
    #[cfg(feature = "a11y")]
    bridge::on_focus_changed(host);
    #[cfg(not(feature = "a11y"))]
    let _ = host;
}

/// WM_DESTROY: the adapter goes with the window.
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    #[cfg(feature = "a11y")]
    bridge::on_destroy(host, hwnd);
    #[cfg(not(feature = "a11y"))]
    let _ = (host, hwnd);
}

/// WM_GETOBJECT: the client's root provider; None (DefWindowProcW) for another object id or for a nested message while
/// the bridge is busy.
#[cfg(feature = "a11y")]
pub(crate) fn on_getobject(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = msg;
    bridge::on_getobject(hwnd, wp, lp)
}

/// WM_GETOBJECT without the feature: DefWindowProcW.
#[cfg(not(feature = "a11y"))]
pub(crate) fn on_getobject(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = (hwnd, msg, wp, lp);
    None
}

/// WM_OLED_A11Y: a client's action requests, marshalled to the UI thread, become Event::A11y.
#[cfg(feature = "a11y")]
pub(crate) fn on_action(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    bridge::on_action(hwnd, msg, wp, lp)
}

/// WM_OLED_A11Y without the feature: nothing posts it.
#[cfg(not(feature = "a11y"))]
pub(crate) fn on_action(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (hwnd, msg, wp, lp);
    LRESULT(0)
}
