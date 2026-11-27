//! Pointer, keyboard, focus, activation, close, cursor and timer messages into the dispatcher's Input (K1a.8, KA2).
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::def_proc;

/// Every input message of the window procedure. Filled by K1a.8.
pub(crate) fn on_input(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}
