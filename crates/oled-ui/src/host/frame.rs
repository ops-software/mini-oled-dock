//! The window frame (DWM attributes, the system caption) and the live system settings (K1a.11, KA2).
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::def_proc;
use super::window::WinHost;

/// After creation: the frame's DWM attributes. Filled by K1a.11.
pub(crate) fn apply(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED: SysOverlay::read and a new resolve. Filled by K1a.11.
pub(crate) fn on_setting_change(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}
