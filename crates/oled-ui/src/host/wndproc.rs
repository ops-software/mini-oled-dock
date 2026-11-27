//! The window procedure (P2-kit 2.13): K0.1b writes the whole match, one delegation per message to the module of the
//! group that handles it; KA2 owns the file and keeps every arm.
#![allow(dead_code)] // Registered with the window class by K1a.7.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{
    a11y, caption, def_proc, dpi, frame, hooks, ime, input, menu, placement, proxy, window, WM_OLED_A11Y, WM_OLED_DATA,
    WM_OLED_RETRY,
};

/// WM_MOUSELEAVE (winuser.h; the windows crate has it under UI::Controls only).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The window procedure of every kit window.
pub(crate) unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => window::on_nccreate(hwnd, msg, wp, lp),
        WM_NCDESTROY => window::on_ncdestroy(hwnd, msg, wp, lp),
        WM_SIZE => window::on_size(hwnd, msg, wp, lp),
        WM_ERASEBKGND => window::on_erasebkgnd(hwnd, msg, wp, lp),
        WM_OLED_RETRY => window::on_retry(hwnd, msg, wp, lp),
        WM_PAINT => crate::present::banded::on_paint(hwnd, msg, wp, lp),
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN
        | WM_RBUTTONUP | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK | WM_XBUTTONDOWN
        | WM_XBUTTONUP | WM_XBUTTONDBLCLK | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN
        | WM_SYSKEYUP | WM_CHAR | WM_SETFOCUS | WM_KILLFOCUS | WM_ACTIVATE | WM_CLOSE | WM_SETCURSOR
        | WM_CAPTURECHANGED | WM_TIMER => input::on_input(hwnd, msg, wp, lp),
        WM_DPICHANGED => dpi::on_dpichanged(hwnd, msg, wp, lp),
        WM_EXITSIZEMOVE => placement::on_exitsizemove(hwnd, msg, wp, lp),
        WM_OLED_DATA => proxy::drain(hwnd, msg, wp, lp),
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => frame::on_setting_change(hwnd, msg, wp, lp),
        WM_GETOBJECT => a11y::on_getobject(hwnd, msg, wp, lp).unwrap_or_else(|| def_proc(hwnd, msg, wp, lp)),
        WM_OLED_A11Y => a11y::on_action(hwnd, msg, wp, lp),
        WM_NCCALCSIZE | WM_NCHITTEST | WM_NCMOUSEMOVE | WM_NCMOUSELEAVE | WM_NCLBUTTONDOWN | WM_NCLBUTTONUP
        | WM_NCLBUTTONDBLCLK | WM_NCRBUTTONUP => {
            caption::on_nc(hwnd, msg, wp, lp).unwrap_or_else(|| def_proc(hwnd, msg, wp, lp))
        }
        WM_IME_SETCONTEXT | WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION | WM_IME_ENDCOMPOSITION => {
            ime::on_ime(hwnd, msg, wp, lp).unwrap_or_else(|| def_proc(hwnd, msg, wp, lp))
        }
        WM_ACTIVATEAPP => {
            menu::on_activate_app(hwnd, wp);
            def_proc(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            let _ = window::with_host(hwnd, |h| hooks::on_destroy(h, hwnd));
            def_proc(hwnd, msg, wp, lp)
        }
        _ => def_proc(hwnd, msg, wp, lp),
    }
}
