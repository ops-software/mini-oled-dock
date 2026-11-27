//! WM_DPICHANGED: the suggested rect, fonts rebuilt, glyphs and runs of the old DPI evicted (K1a.10, KA2).
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::def_proc;

/// Filled by K1a.10.
pub(crate) fn on_dpichanged(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}
