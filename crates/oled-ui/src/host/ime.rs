//! IME glue (K2b.4, KD2): the composition window at the caret, ImmAssociateContext only off fields.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::window::WinHost;
use crate::geom::RectP;
use crate::node::NodeId;

/// After creation. Until KD2: nothing.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// The focus moved: the input context follows fields. Until KD2: nothing.
pub(crate) fn on_focus_changed(host: &mut WinHost, node: Option<NodeId>) {
    let _ = (host, node);
}

/// WM_DESTROY: the input context is restored. Until KD2: nothing.
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION. Until KD2: None
/// (DefWindowProcW).
pub(crate) fn on_ime(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = (hwnd, msg, wp, lp);
    None
}

/// Effect::SetCaret: the composition window and the accessibility caret. Until KD2: nothing.
pub(crate) fn set_caret(host: &mut WinHost, r: Option<RectP>) {
    let _ = (host, r);
}
