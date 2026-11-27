//! The banded presenter of opaque windows (K1a.9, KA2): bands of band_height rows, then hooks::after_paint_band in each.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use crate::host::def_proc;

/// WM_PAINT. Filled by K1a.9.
pub(crate) fn on_paint(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}
