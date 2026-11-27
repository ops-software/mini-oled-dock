//! The custom caption (K3.5, KE2): CUSTOM_CAPTION stays false until S-CAPTION passed, and only a separate one-line
//! commit flips it. Until then every window draws the system caption.
#![allow(dead_code)] // on_created and on_nc are reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::window::WinHost;
use super::Caption;
use crate::cx::PaintCx;

/// Decided by S-CAPTION (K3.5): false until it passes; only a separate one-line commit sets it true.
pub(crate) const CUSTOM_CAPTION: bool = false;

/// The caption a window really gets: Custom only when `custom_on` (CUSTOM_CAPTION, or S-CAPTION's rig through
/// EventLoop::caption_spike).
pub(crate) fn effective(requested: Caption, custom_on: bool) -> Caption {
    if custom_on {
        requested
    } else {
        Caption::System
    }
}

/// After creation. Until KE2: nothing.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// The non-client messages. Until KE2: None (DefWindowProcW: the system caption).
pub(crate) fn on_nc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let _ = (hwnd, msg, wp, lp);
    None
}

/// The caption buttons in a band (hooks::after_paint_band). Until KE2: nothing.
pub(crate) fn paint(cx: &mut PaintCx<'_>) {
    let _ = cx;
}
