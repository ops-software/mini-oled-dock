//! Per-monitor DPI (K1a.10, KA2): the process's awareness, and WM_DPICHANGED: the suggested rect, a new scale, the
//! fonts, runs and glyphs of the old DPI dropped, the band DIB made again at the next paint, a relayout and a full
//! repaint.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

use super::def_proc;
use super::window::{self, Outcome};

/// Makes the process per-monitor DPI aware (v2) unless the thread already is (the manifest's normal case), as the
/// dock's main does; a refusal (the awareness was fixed already) is not an error.
pub(crate) fn ensure_per_monitor_v2() {
    // SAFETY: both calls have no precondition; the constant is a valid context.
    let already = unsafe {
        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    };
    if !already.as_bool() {
        // SAFETY: called by EventLoop::new, before the loop creates a window; the constant is valid.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    }
}

/// The suggested window rect of a WM_DPICHANGED (None for a replay from the retry queue, which carries no pointer: the
/// rect was applied when the message came).
fn suggested(lp: LPARAM) -> Option<RECT> {
    // SAFETY: for WM_DPICHANGED lParam points to the suggested window rect, valid during the message; a replay passes 0.
    (lp.0 != 0).then(|| unsafe { *(lp.0 as *const RECT) })
}

/// WM_DPICHANGED (as the Settings window does): the state takes the new DPI, then the window takes the suggested rect
/// with no state borrowed.
pub(crate) fn on_dpichanged(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let dpi = (wp.0 & 0xFFFF) as u32;
    let rect = suggested(lp);
    if let Outcome::NoState = window::handle(hwnd, msg, wp, LPARAM(0), |h| h.dpi_changed(dpi)) {
        return def_proc(hwnd, msg, wp, lp);
    }
    if let Some(r) = rect {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        // SAFETY: our window; this handler holds no borrow while SetWindowPos sends its messages.
        let _ = unsafe { SetWindowPos(hwnd, None, r.left, r.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replay_carries_no_rect() {
        assert_eq!(suggested(LPARAM(0)), None);
        let r = RECT { left: -10, top: 5, right: 630, bottom: 485 };
        assert_eq!(suggested(LPARAM(&r as *const RECT as isize)), Some(r));
    }
}
