//! IME glue (K2b.4, KD2): the composition window at the caret, ImmAssociateContext only off fields.
//!
//! A kit window draws the composition inline in its TextField, so the system composition window stays hidden
//! (WM_IME_SETCONTEXT drops ISC_SHOWUICOMPOSITIONWINDOW) and the messages are not passed to DefWindowProc while the
//! field handles them. While the focus is off a text field the window's input context is released (ImmAssociateContext
//! with a null context), so the IME stays away from a button or a table; it is restored on the next field and at
//! WM_DESTROY. The released contexts wait in a process-wide list keyed by window, because the window state has no room
//! for them.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::Ime::{
    ImmAssociateContext, ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow,
    ImmSetCompositionWindow, CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, HIMC, IME_COMPOSITION_STRING,
};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION,
};

use super::window::{self, WinHost};
use crate::dispatch::Input;
use crate::event::ImeEvent;
use crate::geom::RectP;
use crate::node::{NodeId, Role};

/// ISC_SHOWUICOMPOSITIONWINDOW (imm.h): the system draws its own composition window.
const ISC_SHOWUICOMPOSITIONWINDOW: isize = 0x8000_0000_u32 as isize;

/// The input contexts taken from windows that have no text field focused: (window, context), as integers.
static RELEASED: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

/// True when the focus is on a node that takes typed text: only then the window keeps an input context.
pub(crate) fn wants_context(role: Option<Role>) -> bool {
    matches!(role, Some(Role::TextInput | Role::SearchInput))
}

/// The byte offset in \`s\` of UTF-16 unit \`units\` (clamped to the end).
pub(crate) fn utf16_to_byte(s: &str, units: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= units {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

/// Takes the window's input context away (the IME is off for it). Called with the window state borrowed: the
/// WM_IME_SETCONTEXT it sends goes to on_ime, which never reaches the state.
fn release(hwnd: HWND) {
    let Ok(mut list) = RELEASED.lock() else { return };
    if list.iter().any(|(w, _)| *w == hwnd.0 as isize) {
        return;
    }
    // SAFETY: hwnd is this window; a null context detaches the current one and returns it, which is kept to give back.
    let prev = unsafe { ImmAssociateContext(hwnd, HIMC::default()) };
    if !prev.is_invalid() {
        list.push((hwnd.0 as isize, prev.0 as isize));
    }
}

/// Gives the window its input context back, if it was taken away.
fn restore(hwnd: HWND) {
    let taken = {
        let Ok(mut list) = RELEASED.lock() else { return };
        list.iter().position(|(w, _)| *w == hwnd.0 as isize).map(|i| list.swap_remove(i).1)
    };
    if let Some(himc) = taken {
        // SAFETY: the context this window had; nobody else holds it.
        let _ = unsafe { ImmAssociateContext(hwnd, HIMC(himc as *mut c_void)) };
    }
}

/// After creation: no field has the focus yet, so the IME stays away.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = host;
    release(hwnd);
}

/// The focus moved: the input context follows fields.
pub(crate) fn on_focus_changed(host: &mut WinHost, node: Option<NodeId>) {
    let role = node.and_then(|n| host.disp.frame.get(n)).map(|n| n.role);
    if wants_context(role) {
        restore(host.hwnd);
    } else {
        release(host.hwnd);
    }
}

/// WM_DESTROY: the input context is restored (the system frees it with the window).
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = host;
    restore(hwnd);
}

/// A composition string of the context: its UTF-16 units as text.
fn comp_string(himc: HIMC, which: IME_COMPOSITION_STRING) -> Option<String> {
    // SAFETY: a context from ImmGetContext; the first call only asks for the size in bytes.
    let bytes = unsafe { ImmGetCompositionStringW(himc, which, None, 0) };
    if bytes <= 0 {
        return None;
    }
    let mut buf = vec![0u16; bytes as usize / 2];
    // SAFETY: buf holds bytes bytes; the call writes at most that many.
    let got = unsafe { ImmGetCompositionStringW(himc, which, Some(buf.as_mut_ptr().cast::<c_void>()), bytes as u32) };
    if got <= 0 {
        return None;
    }
    buf.truncate(got as usize / 2);
    Some(String::from_utf16_lossy(&buf))
}

/// The events the IME messages make, read from the window's context.
fn read_events(hwnd: HWND, lp: LPARAM) -> Vec<ImeEvent> {
    let mut out = Vec::new();
    // SAFETY: hwnd is this window; the context is released below.
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.is_invalid() {
        return out;
    }
    let flags = lp.0 as u32;
    if flags & GCS_RESULTSTR.0 != 0 {
        if let Some(r) = comp_string(himc, GCS_RESULTSTR) {
            out.push(ImeEvent::Result(r));
        }
    }
    if flags & GCS_COMPSTR.0 != 0 {
        let text = comp_string(himc, GCS_COMPSTR).unwrap_or_default();
        // SAFETY: GCS_CURSORPOS returns the position itself (in UTF-16 units), no buffer.
        let pos = unsafe { ImmGetCompositionStringW(himc, GCS_CURSORPOS, None, 0) };
        let caret = utf16_to_byte(&text, pos.max(0) as usize);
        out.push(ImeEvent::Composition { text, caret });
    }
    // SAFETY: the context ImmGetContext returned for this window.
    let _ = unsafe { ImmReleaseContext(hwnd, himc) };
    out
}

/// WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION. None: DefWindowProcW.
pub(crate) fn on_ime(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        // Our field draws the composition: the system's own window is not shown. The call is made with the window
        // state possibly borrowed, so it never touches the state.
        WM_IME_SETCONTEXT => {
            let lp = LPARAM(lp.0 & !ISC_SHOWUICOMPOSITIONWINDOW);
            Some(super::def_proc(hwnd, msg, wp, lp))
        }
        WM_IME_STARTCOMPOSITION => {
            let _ = window::handle(hwnd, msg, wp, lp, |h| h.input(Input::Ime(ImeEvent::Start)));
            Some(LRESULT(0))
        }
        WM_IME_COMPOSITION => {
            for e in read_events(hwnd, lp) {
                let _ = window::handle(hwnd, msg, wp, lp, |h| h.input(Input::Ime(e)));
            }
            Some(LRESULT(0))
        }
        WM_IME_ENDCOMPOSITION => {
            let _ = window::handle(hwnd, msg, wp, lp, |h| h.input(Input::Ime(ImeEvent::End)));
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// Effect::SetCaret: the composition and candidate windows go to the caret's bottom-left corner.
pub(crate) fn set_caret(host: &mut WinHost, r: Option<RectP>) {
    let Some(r) = r else { return };
    let hwnd = host.hwnd;
    // SAFETY: hwnd is this window; the context (null while the IME is off) is released below.
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.is_invalid() {
        return;
    }
    let at = POINT { x: r.l, y: r.b };
    let comp = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: at, ..Default::default() };
    let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_CANDIDATEPOS, ptCurrentPos: at, ..Default::default() };
    // SAFETY: himc is the window's context and both forms live through the calls.
    unsafe {
        let _ = ImmSetCompositionWindow(himc, &comp);
        let _ = ImmSetCandidateWindow(himc, &cand);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_released_off_fields() {
        assert!(wants_context(Some(Role::TextInput)));
        assert!(wants_context(Some(Role::SearchInput)));
        for r in [Role::Button, Role::Table, Role::Row, Role::Cell, Role::ListItem, Role::Window, Role::TabList] {
            assert!(!wants_context(Some(r)), "{r:?}");
        }
        assert!(!wants_context(None), "no focus: the IME stays away");
    }

    #[test]
    fn composition_caret_counts_utf16_units() {
        assert_eq!(utf16_to_byte("abc", 2), 2);
        assert_eq!(utf16_to_byte("a\u{1F600}b", 1), 1);
        assert_eq!(utf16_to_byte("a\u{1F600}b", 3), 5, "a pair is two units and four bytes");
        assert_eq!(utf16_to_byte("a\u{e8}", 9), 3, "past the end clamps");
        assert_eq!(utf16_to_byte("", 0), 0);
    }
}
