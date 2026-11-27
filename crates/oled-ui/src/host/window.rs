//! The window: its state (WinHost), creation, size and the effects of an event (K1a.7-K1a.9, KA2). K0.1b writes
//! apply_effects's match over every Effect; each hand-off to another group is one call to a stub in that group's file.
#![allow(dead_code)] // The skeleton is wired by K1a.7 (window creation); nothing creates a window before it.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::{def_proc, hooks, ime, menu, pacing, tooltip};
use crate::cx::Res;
use crate::dispatch::{Dispatcher, Effect};
use crate::screen::Screen;

/// One window's state, reached from its HWND (K1a.7 decides how: the borrow rules of P2-kit 6 apply).
pub(crate) struct WinHost {
    pub(crate) hwnd: HWND,
    pub(crate) screen: Box<dyn Screen>,
    pub(crate) disp: Dispatcher,
    pub(crate) res: Res,
}

/// Runs `f` on the window's state if it can be borrowed now (None: no state, or a re-entrant message to replay).
/// Filled by K1a.7.
pub(crate) fn with_host<R>(hwnd: HWND, f: impl FnOnce(&mut WinHost) -> R) -> Option<R> {
    let _ = (hwnd, f);
    None
}

pub(crate) fn on_nccreate(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}

pub(crate) fn on_ncdestroy(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}

pub(crate) fn on_size(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}

/// Never erased: the presenter paints every pixel.
pub(crate) fn on_erasebkgnd(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (hwnd, msg, wp, lp);
    LRESULT(1)
}

/// WM_OLED_RETRY: replays, in order, the messages that met a borrowed state (K1a.7).
pub(crate) fn on_retry(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}

/// Applies an event's effects once the dispatcher's borrow has ended.
pub(crate) fn apply_effects(host: &mut WinHost, effects: Vec<Effect>) {
    for e in effects {
        match e {
            Effect::OpenMenu { id, items, at, opener } => menu::open(host, id, items, at, opener),
            Effect::OpenList { id, items, at, highlight } => menu::open_list(host, id, items, at, highlight),
            Effect::UpdateList { id, items, highlight } => menu::update_list(host, id, items, highlight),
            Effect::CloseMenus => menu::close_all(host),
            Effect::Scroll { viewport, dy } => crate::present::scroll::apply(host, viewport, dy),
            Effect::SetCaret(r) => ime::set_caret(host, r),
            Effect::ShowTip { node, rect, text, chord } => tooltip::show(host, node, rect, text, chord),
            Effect::HideTip => tooltip::hide(host),
            Effect::Animate { id, to, dur_ms, ease } => pacing::arm(host, id, to, dur_ms, ease),
            Effect::FocusChanged(node) => hooks::on_focus_changed(host, node),
            // KA2's own arms (K1a.8, K1a.9).
            Effect::Damage(_)
            | Effect::Redraw
            | Effect::Relayout
            | Effect::SetFocus(_)
            | Effect::Capture
            | Effect::Release
            | Effect::SetTimer(..)
            | Effect::CancelTimer(_)
            | Effect::CloseWindow => {}
        }
    }
}
