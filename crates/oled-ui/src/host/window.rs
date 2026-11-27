//! The window: its state (WinHost), creation, size and the effects of an event (K1a.7-K1a.9, KA2). K0.1b writes
//! apply_effects's match over every Effect; each hand-off to another group is one call to a stub in that group's file.
//!
//! The borrow rules (P2-kit 6): a window's state lives in an `Rc<HostCell>` whose pointer GWLP_USERDATA holds from
//! WM_NCCREATE to WM_NCDESTROY. Every handler borrows it with try_borrow_mut; a message that meets a borrowed state is
//! queued and replayed in order by WM_OLED_RETRY. A call that can send messages back to the window (SetWindowPos,
//! ShowWindow, DestroyWindow, SetCapture, ReleaseCapture, UpdateLayeredWindowIndirect, any COM call) is never made while
//! the state is borrowed: it goes to `WinHost::deferred` and runs once the borrow has ended.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use windows::core::HSTRING;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow, HDC};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClassInfoExW, GetClientRect, GetCursorPos, GetWindowLongPtrW, LoadCursorW,
    PostMessageW, RegisterClassExW, SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, IDC_ARROW,
    MINMAXINFO, SHOW_WINDOW_CMD, SW_SHOWMAXIMIZED, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
use windows::Win32::UI::WindowsAndMessaging::{
    FlashWindowEx, IsIconic, SetForegroundWindow, SetWindowPos, FLASHWINFO, FLASHW_ALL, GWL_EXSTYLE, HWND_NOTOPMOST,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_RESTORE,
};

use super::placement::{self, Monitor};
use super::proxy::Mailbox;
use super::run::LoopShared;
use super::{
    def_proc, frame, hooks, ime, menu, pacing, tooltip, Caption, PlacementKey, UiError, WindowKind, WindowRef,
};
use super::{WindowSpec, WM_OLED_RETRY};
use crate::cx::{Env, FrontResult, PaintCx, Res};
use crate::dispatch::{Dispatcher, Effect, Input};
use crate::event::{FocusReason, Reply, TimerId};
use crate::geom::{PointP, RectP, Scale, SizeP};
use crate::host::clipboard::SysClipboard;
use crate::host::input;
use crate::node::{Cursor, NodeId};
use crate::present::banded::BandedPresenter;
use crate::present::layered::{self, LayeredPresenter, Present};
use crate::present::Damage;
use crate::screen::Screen;
use crate::text::GdiTextEngine;
use crate::theme::Tokens;
use crate::w3::{argb, div255};

/// One window's state, reached from its HWND through its HostCell.
pub(crate) struct WinHost {
    pub(crate) hwnd: HWND,
    pub(crate) screen: Box<dyn Screen>,
    pub(crate) disp: Dispatcher,
    pub(crate) res: Res,
    /// The loop the window belongs to: tokens, language, clock, the other windows.
    pub(crate) lp: Rc<LoopShared>,
    pub(crate) kind: WindowKind,
    /// The caption the spec asked for (the effective one follows host::caption::effective).
    pub(crate) caption: Caption,
    /// Where the window's placement is stored (None: it is not).
    pub(crate) placement: Option<PlacementKey>,
    pub(crate) scale: Scale,
    /// The client size in physical px.
    pub(crate) client: SizeP,
    /// The token generation of the last layout (None: never laid out).
    pub(crate) layout_gen: Option<u64>,
    pub(crate) needs_layout: bool,
    /// What the window repaints (window coordinates); Windows gets it when the handler ends.
    pub(crate) damage: Damage,
    pub(crate) presenter: Presenter,
    pub(crate) clipboard: SysClipboard,
    /// The calls that wait for the borrow to end, in order.
    pub(crate) deferred: Vec<Sys>,
    /// TrackMouseEvent(TME_LEAVE) is armed for this entry of the pointer.
    pub(crate) tracking_leave: bool,
    /// A WM_CHAR high surrogate waiting for its low half.
    pub(crate) high_surrogate: u16,
    /// The pointer buttons held, a bit each: the capture is held while any is.
    pub(crate) buttons: u8,
    /// The frame timer is armed (pacing).
    pub(crate) frame_timer: bool,
    /// The animated nodes: each frame repaints their halos (pacing).
    pub(crate) animating: Vec<NodeId>,
    /// The due time of the armed deadline timer.
    pub(crate) deadline: Option<u64>,
    /// The armed one-shot timers.
    pub(crate) app_timers: Vec<TimerId>,
    /// The messages for Screen::data (its proxies hold it too).
    pub(crate) mailbox: Arc<Mutex<Mailbox>>,
    /// The spec's icon resource, loaded by frame::apply.
    pub(crate) icon_resource: Option<u16>,
    /// The big and small icons frame::apply loaded, freed with the state.
    pub(crate) icons: [Option<HICON>; 2],
}

impl Drop for WinHost {
    fn drop(&mut self) {
        for i in self.icons.iter_mut().filter_map(Option::take) {
            // SAFETY: an icon frame::apply loaded (not shared); the window that showed it is gone.
            let _ = unsafe { DestroyIcon(i) };
        }
    }
}

/// A call made after the window's borrow has ended (it may send messages to the window).
pub(crate) enum Sys {
    Destroy,
    Show(SHOW_WINDOW_CMD),
    Capture,
    Release,
    /// A layered window's UpdateLayeredWindowIndirect.
    Present(Present),
    /// Another group's hand-off (DWM attributes, the icons, a popup window, a COM call).
    Call(Box<dyn FnOnce(HWND)>),
    /// EventCx::bring_to_front: the calls of bring_to_front, then the outcome stored for EventCx::front_result (TK1).
    Front,
    /// EventCx::set_topmost (TK1).
    Topmost(bool),
}

/// A window's presenter: banded for an opaque window, layered for a popup.
pub(crate) enum Presenter {
    Banded(BandedPresenter),
    Layered(LayeredPresenter),
}

impl Presenter {
    /// Drops the DIB (a DPI change): the next paint makes it again.
    fn reset(&mut self) {
        match self {
            Presenter::Banded(b) => b.reset(),
            Presenter::Layered(l) => l.reset(),
        }
    }
}

impl WinHost {
    /// The loop's current tokens (a cheap shared handle, so the contexts can borrow them).
    pub(crate) fn tokens(&self) -> Rc<Tokens> {
        Rc::clone(&self.lp.tokens.borrow())
    }

    /// What every context of this window reads.
    pub(crate) fn env<'a>(&self, tokens: &'a Tokens) -> Env<'a> {
        Env {
            scale: self.scale,
            tokens,
            lang: self.lp.lang.get(),
            now_ms: self.lp.now_ms(),
            caption: self.caption,
            caption_spike: self.lp.caption_spike.get(),
        }
    }

    /// Runs `s` once the borrow of this state has ended.
    pub(crate) fn defer(&mut self, s: Sys) {
        self.deferred.push(s);
    }

    pub(crate) fn client_rect(&self) -> RectP {
        RectP::wh(0, 0, self.client.w, self.client.h)
    }

    /// Adds `r` (clipped to the client) to what the window repaints; Windows gets it when the handler ends.
    pub(crate) fn add_damage(&mut self, r: RectP) {
        let client = self.client_rect();
        self.damage.add(r, client);
    }

    /// The whole client is repainted.
    pub(crate) fn redraw(&mut self) {
        let client = self.client_rect();
        self.damage.all(client);
    }

    /// A new client size: a relayout and a full repaint.
    pub(crate) fn resize(&mut self, size: SizeP) {
        if size != self.client {
            self.client = size;
            self.needs_layout = true;
            self.redraw();
        }
    }

    /// WM_DPICHANGED: the new scale. A window has one DPI at a time, so the fonts and runs of the old one go with a
    /// fresh text engine (its Gdi too); the glyph cache is emptied, the band DIB is made again at the next paint, then a
    /// relayout and a full repaint.
    pub(crate) fn dpi_changed(&mut self, dpi: u32) {
        if dpi == 0 || dpi == self.scale.dpi {
            return;
        }
        self.scale = Scale { dpi };
        if let Ok(t) = GdiTextEngine::new() {
            self.res.text = Box::new(t);
        }
        self.res.glyphs.clear();
        self.presenter.reset();
        self.needs_layout = true;
        self.redraw();
    }

    /// Lays the screen out at the client size, applies what the layout asked for, then hooks::after_layout.
    pub(crate) fn layout(&mut self) {
        let tokens = self.tokens();
        let env = self.env(&tokens);
        let avail = self.client;
        let before = self.disp.state.hot;
        let WinHost { screen, disp, res, .. } = self;
        let size = disp.layout(&mut **screen, env, res, avail);
        self.needs_layout = false;
        self.layout_gen = Some(tokens.generation);
        // A layered window is sized to its layout.
        let layered = matches!(self.kind, WindowKind::Layered { .. });
        if layered && size != self.client && size.w > 0 && size.h > 0 {
            self.client = size;
            self.redraw();
        }
        self.apply_pending();
        pacing::note_hot(self, before);
        self.after_dispatch();
        hooks::after_layout(self);
    }

    /// One input through the dispatcher, then its reply and effects, the deadline timer and the frame clock.
    pub(crate) fn input(&mut self, inp: Input) -> Reply {
        let tokens = self.tokens();
        let env = self.env(&tokens);
        let before = self.disp.state.hot;
        let WinHost { screen, disp, res, clipboard, .. } = self;
        let reply = disp.input(&mut **screen, inp, env, res, clipboard);
        self.reply(reply);
        self.apply_pending();
        pacing::note_hot(self, before);
        self.after_dispatch();
        reply
    }

    /// A WM_COPYDATA's tag and bytes through the dispatcher to Screen::copy_data: the screen's answer.
    pub(crate) fn copy_data(&mut self, tag: usize, bytes: Vec<u8>) -> bool {
        self.disp.copied = false;
        self.input(Input::CopyData(tag, bytes));
        std::mem::take(&mut self.disp.copied)
    }

    /// What a screen's reply asks of the host.
    pub(crate) fn reply(&mut self, r: Reply) {
        match r {
            Reply::None => {}
            Reply::Damage(d) => self.add_damage(d),
            Reply::Redraw => self.redraw(),
            Reply::Relayout => self.needs_layout = true,
            Reply::Close => self.defer(Sys::Destroy),
        }
    }

    /// After every dispatch: the deadline timer follows the dispatcher, the frame clock runs while a tween lives.
    pub(crate) fn after_dispatch(&mut self) {
        input::sync_deadline(self);
        pacing::keep_alive(self);
    }

    /// The focus ring and its gap around `r`: what a state change of a node repaints (the dispatcher's halo).
    pub(crate) fn halo(&self, r: RectP) -> RectP {
        let g = self.scale.px(self.lp.tokens.borrow().size.focus_gap) + self.scale.ring();
        r.inset(-g, -g)
    }

    /// The cursor over the client: the captured node's while a button is down, else the hot node's.
    pub(crate) fn cursor(&self) -> Cursor {
        let id = self.disp.state.captured.or(self.disp.state.hot);
        id.and_then(|i| self.disp.frame.get(i)).map_or(Cursor::Arrow, |n| n.cursor)
    }

    /// The Win32 timers this window holds.
    pub(crate) fn live_timers(&self) -> u32 {
        self.app_timers.len() as u32 + u32::from(self.deadline.is_some()) + u32::from(self.frame_timer)
    }

    /// The dispatcher's waiting effects, in rounds (an applied effect may queue more), as the Harness does.
    pub(crate) fn apply_pending(&mut self) {
        for _round in 0..16 {
            let effects = std::mem::take(&mut self.disp.effects);
            if effects.is_empty() {
                break;
            }
            apply_effects(self, effects);
        }
    }

    /// WM_PAINT: `rects` (window coordinates) in bands, after a relayout if one is due.
    pub(crate) fn paint(&mut self, hdc: HDC, rects: &[RectP]) {
        if self.needs_layout {
            self.layout();
        }
        let tokens = self.tokens();
        let env = self.env(&tokens);
        let bg = tokens.color.bg;
        let bg_px = argb(bg.a, div255(bg.rgb.0 * bg.a), div255(bg.rgb.1 * bg.a), div255(bg.rgb.2 * bg.a));
        let client = self.client;
        let WinHost { screen, disp, res, presenter, .. } = self;
        // A layered window paints in flush_damage; it gets no WM_PAINT of its own.
        let Presenter::Banded(banded) = presenter else { return };
        banded.paint(hdc, client, rects, bg_px, &mut |canvas, origin, clip| {
            let mut cx = PaintCx::new(env, canvas, origin, clip, disp, res, None);
            screen.paint(&mut cx);
            hooks::after_paint_band(&mut cx);
        });
    }

    /// A layered window's damage painted into its DIB: what to present once the borrow has ended.
    fn render_layered(&mut self, damage: &Damage) -> Option<Present> {
        let tokens = self.tokens();
        let env = self.env(&tokens);
        let client = self.client;
        let WinHost { screen, disp, res, presenter, .. } = self;
        let Presenter::Layered(l) = presenter else { return None };
        l.render(client, damage, &mut |canvas, origin, clip| {
            let mut cx = PaintCx::new(env, canvas, origin, clip, disp, res, None);
            screen.paint(&mut cx);
            hooks::after_paint_band(&mut cx);
        })
    }

    /// The idle pass of this window: a relayout if one is due (asked for, or new tokens); HostCell::run then hands the
    /// damage to Windows.
    pub(crate) fn idle(&mut self) {
        if self.layout_gen != Some(self.lp.tokens.borrow().generation) {
            if self.layout_gen.is_some() {
                // New tokens: the system caption takes the new background too.
                frame::apply(self, self.hwnd);
            }
            self.needs_layout = true;
            self.redraw();
        }
        if self.needs_layout {
            self.layout();
        }
    }

    /// Hands the damage to Windows (InvalidateRect sends no message, so it runs inside the borrow). Every handler ends
    /// with it, so a modal loop (a live resize, a drag of the caption) still repaints what changed.
    /// A layered window paints its damage here instead (it gets no WM_PAINT) and presents it after the borrow.
    pub(crate) fn flush_damage(&mut self) {
        if self.damage.is_empty() {
            return;
        }
        if matches!(self.presenter, Presenter::Layered(_)) {
            if self.needs_layout {
                self.layout();
            }
            let d = self.damage.take();
            if let Some(p) = self.render_layered(&d) {
                self.defer(Sys::Present(p));
            }
            return;
        }
        for r in self.damage.take().rects() {
            let rc = RECT::from(*r);
            // SAFETY: our window; the rect lives for the call.
            let _ = unsafe { InvalidateRect(Some(self.hwnd), Some(&rc), false) };
        }
    }
}

/// The messages a window could not take while its state was borrowed, replayed in order by WM_OLED_RETRY.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Retry {
    Msg {
        msg: u32,
        wp: usize,
        lp: isize,
    },
    /// A WM_PAINT that met a borrowed state: its rect is invalidated again.
    Repaint(RectP),
    /// A WM_COPYDATA that met a borrowed state: its bytes, copied, go to Screen::copy_data (TK1).
    CopyData {
        tag: usize,
        bytes: Vec<u8>,
    },
}

/// The retry queue: FIFO, bounded, with the one WM_OLED_RETRY in flight.
#[derive(Debug, Default)]
pub(crate) struct RetryQueue {
    items: VecDeque<Retry>,
    /// A WM_OLED_RETRY is posted and not yet handled.
    posted: bool,
    /// WM_OLED_RETRY is replaying: the messages it sends are not queued behind the ones it took.
    replaying: bool,
    /// Messages dropped because the queue was full.
    pub(crate) dropped: u32,
}

impl RetryQueue {
    /// At most this many messages wait; later ones are dropped (a window stuck that long has lost them anyway).
    pub(crate) const CAP: usize = 1024;

    pub(crate) fn push(&mut self, r: Retry) {
        if self.items.len() >= Self::CAP {
            self.dropped += 1;
        } else {
            self.items.push_back(r);
        }
    }

    /// True while queued messages wait: a new message goes behind them.
    pub(crate) fn blocks(&self) -> bool {
        !self.items.is_empty() && !self.replaying
    }

    /// True when a WM_OLED_RETRY must be posted now (messages wait and none is in flight); marks it posted.
    pub(crate) fn want_post(&mut self) -> bool {
        if self.items.is_empty() || self.posted {
            return false;
        }
        self.posted = true;
        true
    }

    /// WM_OLED_RETRY: every waiting message, oldest first.
    pub(crate) fn take(&mut self) -> Vec<Retry> {
        self.posted = false;
        self.items.drain(..).collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
}

/// What a window keeps behind its HWND.
pub(crate) struct HostCell {
    pub(crate) lp: Rc<LoopShared>,
    pub(crate) host: RefCell<WinHost>,
    pub(crate) retry: RefCell<RetryQueue>,
    /// WM_NCCREATE stored the pointer: from then on WM_NCDESTROY owns the reference GWLP_USERDATA holds.
    attached: Cell<bool>,
}

impl HostCell {
    /// Runs `f` on the state if it can be borrowed now, then the calls it deferred, then posts WM_OLED_RETRY if
    /// messages were queued meanwhile. None when the state is borrowed.
    pub(crate) fn run<R>(&self, hwnd: HWND, f: impl FnOnce(&mut WinHost) -> R) -> Option<R> {
        let (r, deferred) = {
            let mut h = self.host.try_borrow_mut().ok()?;
            let r = f(&mut h);
            h.flush_damage();
            (r, std::mem::take(&mut h.deferred))
        };
        run_deferred(hwnd, deferred);
        self.kick(hwnd);
        Some(r)
    }

    /// Posts WM_OLED_RETRY when messages wait, none is in flight and the state is free (a borrow in progress posts it
    /// when it ends, so a modal loop inside a handler never spins on retries).
    fn kick(&self, hwnd: HWND) {
        if self.host.try_borrow_mut().is_err() {
            return;
        }
        if self.retry.borrow_mut().want_post() {
            // SAFETY: posting to our own window; the message carries no pointer.
            if unsafe { PostMessageW(Some(hwnd), WM_OLED_RETRY, WPARAM(0), LPARAM(0)) }.is_err() {
                self.retry.borrow_mut().posted = false;
            }
        }
    }
}

/// The window's cell, with a strong reference of its own, so the state outlives a destruction inside the handler.
pub(crate) fn cell_of(hwnd: HWND) -> Option<Rc<HostCell>> {
    // SAFETY: reading a window's own long has no precondition.
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const HostCell;
    if p.is_null() {
        return None;
    }
    // SAFETY: a non-null GWLP_USERDATA of a kit window is the pointer Rc::into_raw gave at WM_NCCREATE, and the reference
    // it stands for is released only at WM_NCDESTROY, which clears the slot first; the count added here is released by
    // the Rc returned.
    unsafe {
        Rc::increment_strong_count(p);
        Some(Rc::from_raw(p))
    }
}

/// Runs `f` on the window's state if it can be borrowed now (None: no state, or a re-entrant message to replay).
pub(crate) fn with_host<R>(hwnd: HWND, f: impl FnOnce(&mut WinHost) -> R) -> Option<R> {
    cell_of(hwnd)?.run(hwnd, f)
}

/// What became of a message that keeps its order.
pub(crate) enum Outcome<R> {
    Done(R),
    /// The state was borrowed, or earlier messages wait: it is queued for WM_OLED_RETRY.
    Queued,
    /// The window has no state (before WM_NCCREATE or after WM_NCDESTROY): DefWindowProcW.
    NoState,
}

/// A message that keeps its order: `f` now, or queued behind the messages that wait.
pub(crate) fn handle<R>(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, f: impl FnOnce(&mut WinHost) -> R) -> Outcome<R> {
    let Some(cell) = cell_of(hwnd) else { return Outcome::NoState };
    if !cell.retry.borrow().blocks() {
        if let Some(r) = cell.run(hwnd, f) {
            return Outcome::Done(r);
        }
    }
    cell.retry.borrow_mut().push(Retry::Msg { msg, wp: wp.0, lp: lp.0 });
    cell.kick(hwnd);
    Outcome::Queued
}

/// The largest WM_COPYDATA payload a screen gets (TK1): a larger one is refused before any call.
pub(crate) const COPY_DATA_MAX: usize = 64 * 1024;

/// True when a WM_COPYDATA payload of `len` bytes may reach the screen.
pub(crate) fn copy_data_fits(len: usize) -> bool {
    len <= COPY_DATA_MAX
}

/// A WM_COPYDATA's bytes to Screen::copy_data in order with the window's other messages: Done with the screen's answer
/// now, or Queued behind the messages that wait (the bytes are the window's own copy).
pub(crate) fn copy_data(hwnd: HWND, tag: usize, bytes: Vec<u8>) -> Outcome<bool> {
    let Some(cell) = cell_of(hwnd) else { return Outcome::NoState };
    let mut slot = Some(bytes);
    if !cell.retry.borrow().blocks() {
        if let Some(taken) = cell.run(hwnd, |h| h.copy_data(tag, slot.take().unwrap_or_default())) {
            return Outcome::Done(taken);
        }
    }
    cell.retry.borrow_mut().push(Retry::CopyData { tag, bytes: slot.take().unwrap_or_default() });
    cell.kick(hwnd);
    Outcome::Queued
}

/// WM_COPYDATA (TK1): dwData and a copy of the payload to Screen::copy_data. The sender gets the screen's answer, TRUE
/// when the bytes wait for WM_OLED_RETRY (the state was borrowed: accepted), FALSE for a payload above COPY_DATA_MAX
/// (unread, no call).
pub(crate) fn on_copydata(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if lp.0 == 0 {
        return LRESULT(0);
    }
    // SAFETY: for WM_COPYDATA lParam points to a COPYDATASTRUCT that the system keeps valid while the message is handled.
    let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
    let len = cds.cbData as usize;
    if !copy_data_fits(len) || (len > 0 && cds.lpData.is_null()) {
        return LRESULT(0);
    }
    let bytes = if len == 0 {
        Vec::new()
    } else {
        // SAFETY: lpData points to cbData bytes the system copied into this process, valid while the message is handled;
        // they are copied out at once.
        unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, len) }.to_vec()
    };
    match copy_data(hwnd, cds.dwData, bytes) {
        Outcome::Done(taken) => LRESULT(isize::from(taken)),
        Outcome::Queued => LRESULT(1),
        Outcome::NoState => def_proc(hwnd, msg, wp, lp),
    }
}

/// True when the app routed `msg` to this window's screen (EventLoop::route_message); a system message never is.
pub(crate) fn is_routed(hwnd: HWND, msg: u32) -> bool {
    super::run::routable(msg) && cell_of(hwnd).is_some_and(|c| c.lp.routes(hwnd, msg))
}

/// A routed message to Screen::app_message, in order with the window's other messages (TK1).
pub(crate) fn on_routed(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let routed = Input::Routed(msg, wp.0, lp.0);
    match handle(hwnd, msg, wp, lp, |h| h.input(routed)) {
        Outcome::NoState => def_proc(hwnd, msg, wp, lp),
        Outcome::Done(_) | Outcome::Queued => LRESULT(0),
    }
}

/// What a bring_to_front achieved: the foreground, else the raise above the other windows, else nothing.
pub(crate) fn front_outcome(foreground: bool, raised: bool) -> FrontResult {
    match (foreground, raised) {
        (true, _) => FrontResult::Foreground,
        (false, true) => FrontResult::Topmost,
        (false, false) => FrontResult::None,
    }
}

/// EventCx::bring_to_front once the borrow has ended: a minimised window is restored, then made the foreground window.
/// When Windows refuses that (another process holds the foreground), the window goes up into the topmost band and back
/// down to the top of the others, without taking the activation, and its taskbar button flashes three times. A window
/// kept topmost (set_topmost) is only raised and stays topmost.
fn bring_to_front(hwnd: HWND) -> FrontResult {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // SAFETY: our window, on its thread, with no state borrowed: the messages these calls send re-enter its procedure
    // safely; FLASHWINFO lives for the call.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() {
            return FrontResult::Foreground;
        }
        let pinned = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize) != 0;
        let mut raised = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags).is_ok();
        if !pinned {
            raised &= SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags).is_ok();
        }
        let flash = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL,
            uCount: 3,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&flash);
        front_outcome(false, raised)
    }
}

/// EventCx::set_topmost once the borrow has ended.
fn set_topmost(hwnd: HWND, on: bool) {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: our window, on its thread, with no state borrowed.
    let _ = unsafe { SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
}

/// A WM_PAINT that met a borrowed state: its rect is invalidated again by WM_OLED_RETRY.
pub(crate) fn queue_repaint(hwnd: HWND, r: RectP) {
    if let Some(cell) = cell_of(hwnd) {
        cell.retry.borrow_mut().push(Retry::Repaint(r));
        cell.kick(hwnd);
    }
}

/// The calls a handler deferred, in order, with no borrow held.
pub(crate) fn run_deferred(hwnd: HWND, calls: Vec<Sys>) {
    for c in calls {
        match c {
            Sys::Destroy => {
                // SAFETY: our window, destroyed on its own thread with no state borrowed.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            Sys::Show(cmd) => {
                // SAFETY: our window; no state is borrowed while they send their messages. UpdateWindow paints the
                // window now, before the messages the show queued (activation, input) are pumped.
                unsafe {
                    let _ = ShowWindow(hwnd, cmd);
                    let _ = UpdateWindow(hwnd);
                }
            }
            Sys::Capture => {
                // SAFETY: our window, on its thread; WM_CAPTURECHANGED reaches the old owner with no state borrowed.
                unsafe { SetCapture(hwnd) };
            }
            Sys::Release => {
                // SAFETY: releases this thread's capture; WM_CAPTURECHANGED arrives with no state borrowed.
                let _ = unsafe { ReleaseCapture() };
            }
            Sys::Present(p) => {
                let _ = layered::present(hwnd, &p);
            }
            Sys::Call(f) => f(hwnd),
            Sys::Front => {
                let r = bring_to_front(hwnd);
                let _ = with_host(hwnd, |h| h.disp.front = Some(r));
            }
            Sys::Topmost(on) => set_topmost(hwnd, on),
        }
    }
}

pub(crate) fn on_nccreate(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: for WM_NCCREATE, lParam points to the CREATESTRUCTW of the CreateWindowExW call in progress.
    let raw = unsafe { (*(lp.0 as *const CREATESTRUCTW)).lpCreateParams } as *const HostCell;
    if !raw.is_null() {
        // SAFETY: lpCreateParams is the pointer `open` got from Rc::into_raw; `open` holds another strong reference
        // until CreateWindowExW returns, so the cell is alive.
        let cell = unsafe { &*raw };
        cell.attached.set(true);
        // SAFETY: our window's own long; from here GWLP_USERDATA owns the reference into_raw made.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        if let Ok(mut h) = cell.host.try_borrow_mut() {
            h.hwnd = hwnd;
            h.clipboard.hwnd = hwnd;
        }
    }
    def_proc(hwnd, msg, wp, lp)
}

pub(crate) fn on_ncdestroy(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: our window's own long; the slot is cleared before its reference is released, so nothing reads it again.
    let raw = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *const HostCell;
    if !raw.is_null() {
        // SAFETY: the reference GWLP_USERDATA held since WM_NCCREATE (Rc::into_raw), released once.
        let cell = unsafe { Rc::from_raw(raw) };
        // The window's timers died with it.
        if let Ok(h) = cell.host.try_borrow() {
            let n = h.live_timers();
            cell.lp.count(|s| s.timers_live = s.timers_live.saturating_sub(n));
        }
        cell.lp.forget_window(hwnd);
    }
    def_proc(hwnd, msg, wp, lp)
}

pub(crate) fn on_size(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    /// SIZE_MINIMIZED: the client is 0 x 0 until restored; the layout keeps its size.
    const SIZE_MINIMIZED: usize = 1;
    if wp.0 == SIZE_MINIMIZED {
        tooltip::on_owner_message(hwnd, msg, wp, lp);
        return LRESULT(0);
    }
    let size = SizeP { w: (lp.0 & 0xFFFF) as i32, h: ((lp.0 >> 16) & 0xFFFF) as i32 };
    match handle(hwnd, msg, wp, lp, |h| h.resize(size)) {
        Outcome::NoState => def_proc(hwnd, msg, wp, lp),
        Outcome::Done(()) | Outcome::Queued => LRESULT(0),
    }
}

/// Never erased: the presenter paints every pixel.
pub(crate) fn on_erasebkgnd(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (hwnd, msg, wp, lp);
    LRESULT(1)
}

/// WM_OLED_RETRY: replays, in order, the messages that met a borrowed state (K1a.7).
pub(crate) fn on_retry(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _ = (msg, wp, lp);
    let Some(cell) = cell_of(hwnd) else { return LRESULT(0) };
    let items = {
        let mut q = cell.retry.borrow_mut();
        q.posted = false;
        if cell.host.try_borrow_mut().is_err() {
            // Still borrowed (a modal loop inside a handler): the borrow posts again when it ends.
            return LRESULT(0);
        }
        q.replaying = true;
        q.take()
    };
    for r in items {
        match r {
            // SAFETY: the window procedure of this very window, called on its thread with the arguments it queued.
            Retry::Msg { msg, wp, lp } => unsafe {
                super::wndproc::wndproc(hwnd, msg, WPARAM(wp), LPARAM(lp));
            },
            Retry::Repaint(r) => {
                let rc = RECT::from(r);
                // SAFETY: our window; the rect lives for the call.
                let _ = unsafe { InvalidateRect(Some(hwnd), Some(&rc), false) };
            }
            Retry::CopyData { tag, bytes } => {
                let _ = copy_data(hwnd, tag, bytes);
            }
        }
    }
    cell.retry.borrow_mut().replaying = false;
    cell.kick(hwnd);
    LRESULT(0)
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
            // KA2's own arms (K1a.7-K1a.9).
            Effect::Damage(r) => host.add_damage(r),
            Effect::Redraw => host.redraw(),
            Effect::Relayout => host.needs_layout = true,
            Effect::CloseWindow => host.defer(Sys::Destroy),
            Effect::SetFocus(id) => {
                let tokens = host.tokens();
                let env = host.env(&tokens);
                let WinHost { screen, disp, res, clipboard, .. } = host;
                disp.set_focus(&mut **screen, env, res, clipboard, Some(id), FocusReason::Programmatic);
            }
            Effect::Capture => {
                host.disp.state.captured = host.disp.state.pressed.or(host.disp.state.hot);
                host.defer(Sys::Capture);
            }
            Effect::Release => {
                host.disp.state.captured = None;
                if host.buttons == 0 {
                    host.defer(Sys::Release);
                }
            }
            Effect::SetTimer(id, after_ms) => input::set_timer(host, id, after_ms),
            Effect::CancelTimer(id) => input::cancel_timer(host, id),
            // TK1: they send messages to the window, so they run after the borrow.
            Effect::BringToFront => host.defer(Sys::Front),
            Effect::Topmost(on) => host.defer(Sys::Topmost(on)),
        }
    }
}

/// The client size of a window in physical px.
pub(crate) fn client_size(hwnd: HWND) -> SizeP {
    let mut r = RECT::default();
    // SAFETY: our window; `r` is writable.
    if unsafe { GetClientRect(hwnd, &mut r) }.is_err() {
        return SizeP::default();
    }
    SizeP { w: r.right - r.left, h: r.bottom - r.top }
}

/// The styles of a window kind.
fn styles(kind: &WindowKind) -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    match *kind {
        WindowKind::Opaque { .. } => (WS_OVERLAPPEDWINDOW, WINDOW_EX_STYLE(0)),
        WindowKind::Layered { activate, topmost } => {
            let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
            if !activate {
                ex |= WS_EX_NOACTIVATE;
            }
            if topmost {
                ex |= WS_EX_TOPMOST;
            }
            (WS_POPUP, ex)
        }
    }
}

/// The module handle of this executable (the class and the windows belong to it).
pub(crate) fn instance() -> HINSTANCE {
    // SAFETY: GetModuleHandleW(None) returns the executable's handle and takes no reference.
    unsafe { GetModuleHandleW(None) }.map(|m| HINSTANCE(m.0)).unwrap_or_default()
}

/// Registers the class once per loop (or finds it registered by another loop of this process).
fn register_class(lp: &LoopShared, name: &'static str, hinst: HINSTANCE) -> Result<HSTRING, UiError> {
    let wide = HSTRING::from(name);
    if lp.classes.borrow().contains(&name) {
        return Ok(wide);
    }
    let mut wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, ..Default::default() };
    // SAFETY: `wc` is a writable WNDCLASSEXW with cbSize set; the name outlives the call.
    let known = unsafe { GetClassInfoExW(Some(hinst), &wide, &mut wc) }.is_ok();
    if !known {
        // SAFETY: IDC_ARROW is a system cursor; the handle is shared and never freed.
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(super::wndproc::wndproc),
            hInstance: hinst,
            hCursor: cursor,
            lpszClassName: windows::core::PCWSTR(wide.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is complete and its class name outlives the call (RegisterClassExW copies it).
        if unsafe { RegisterClassExW(&wc) } == 0 {
            return Err(UiError::Win32(windows::core::Error::from_thread()));
        }
    }
    lp.classes.borrow_mut().push(name);
    Ok(wide)
}

/// The frame around a client area at `dpi`: the insets AdjustWindowRectExForDpi adds (left, top, right, bottom >= 0).
pub(crate) fn frame_insets(style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, dpi: u32) -> RECT {
    let mut r = RECT::default();
    // SAFETY: `r` is writable.
    if unsafe { AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi) }.is_err() {
        return RECT::default();
    }
    RECT { left: -r.left, top: -r.top, right: r.right, bottom: r.bottom }
}

/// The frame of a window kind at `dpi` (the insets around its client area).
pub(crate) fn frame_for(kind: &WindowKind, dpi: u32) -> RECT {
    let (style, ex) = styles(kind);
    frame_insets(style, ex, dpi)
}

/// The pointer's position (screen coordinates; the primary monitor's corner when Windows does not say).
fn cursor() -> PointP {
    let mut p = POINT::default();
    // SAFETY: `p` is writable; on failure it stays (0, 0).
    let _ = unsafe { GetCursorPos(&mut p) };
    PointP { x: p.x, y: p.y }
}

/// WM_GETMINMAXINFO: an opaque window's minimum client size, with its frame, at its DPI.
pub(crate) fn on_getminmaxinfo(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let min = with_host(hwnd, |h| match h.kind {
        WindowKind::Opaque { min, .. } => {
            let ins = frame_for(&h.kind, h.scale.dpi);
            let s = h.scale;
            Some((s.px(min.w) + ins.left + ins.right, s.px(min.h) + ins.top + ins.bottom))
        }
        WindowKind::Layered { .. } => None,
    });
    let Some((w, h)) = min.flatten() else { return def_proc(hwnd, msg, wp, lp) };
    // SAFETY: for WM_GETMINMAXINFO lParam points to the MINMAXINFO the system prefilled, valid during the message.
    let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
    mmi.ptMinTrackSize = POINT { x: w, y: h };
    LRESULT(0)
}

/// EventLoop::open: the class, the state, the window, after_create, the first layout, then the show.
pub(crate) fn open(lp: &Rc<LoopShared>, spec: WindowSpec, screen: Box<dyn Screen>) -> Result<WindowRef, UiError> {
    // The first paint draws text: the text system starts on a worker while this thread makes and shows the window.
    crate::text::warm::start();
    let hinst = instance();
    let class = register_class(lp, spec.class, hinst)?;
    let (style, ex) = styles(&spec.kind);
    let mut mons = placement::monitors();
    if mons.is_empty() {
        let any = RectP::wh(0, 0, 1024, 768);
        mons.push(Monitor { device: String::new(), rect: any, work: any, dpi: 96 });
    }
    let saved = spec.placement.and_then(|k| lp.store.borrow().as_ref().and_then(|s| s.load(k)));
    let placed = placement::place(saved.as_ref(), &mons, cursor(), &spec, &|dpi| frame_insets(style, ex, dpi));
    let dpi = mons.get(placed.monitor).map_or(96, |m| m.dpi);
    let scale = Scale { dpi };
    let client = SizeP { w: placed.rect.w(), h: placed.rect.h() };
    let ins = frame_insets(style, ex, dpi);
    let rect = RECT {
        left: placed.rect.l - ins.left,
        top: placed.rect.t - ins.top,
        right: placed.rect.r + ins.right,
        bottom: placed.rect.b + ins.bottom,
    };
    let (caption, opaque) = match spec.kind {
        WindowKind::Opaque { caption, .. } => (caption, true),
        WindowKind::Layered { .. } => (Caption::System, false),
    };
    let show = match spec.kind {
        WindowKind::Layered { activate: false, .. } => SW_SHOWNOACTIVATE,
        WindowKind::Opaque { .. } if placed.maximized => SW_SHOWMAXIMIZED,
        _ => SW_SHOWNORMAL,
    };
    let mut disp = Dispatcher::new();
    disp.esc_closes = spec.esc_closes;
    let mailbox = Arc::new(Mutex::new(Mailbox::default()));
    let id = lp.next_window_ref();
    let host = WinHost {
        hwnd: HWND::default(),
        screen,
        disp,
        res: Res::new(Box::new(GdiTextEngine::new()?)),
        lp: Rc::clone(lp),
        kind: spec.kind,
        caption,
        placement: spec.placement,
        scale,
        client,
        layout_gen: None,
        needs_layout: true,
        damage: Damage::default(),
        presenter: if opaque {
            Presenter::Banded(BandedPresenter::new())
        } else {
            Presenter::Layered(LayeredPresenter::new())
        },
        clipboard: SysClipboard { hwnd: HWND::default() },
        deferred: Vec::new(),
        tracking_leave: false,
        high_surrogate: 0,
        buttons: 0,
        frame_timer: false,
        animating: Vec::new(),
        deadline: None,
        app_timers: Vec::new(),
        mailbox: Arc::clone(&mailbox),
        icon_resource: spec.icon_resource,
        icons: [None, None],
    };
    let cell = Rc::new(HostCell {
        lp: Rc::clone(lp),
        host: RefCell::new(host),
        retry: RefCell::new(RetryQueue::default()),
        attached: Cell::new(false),
    });
    let raw = Rc::into_raw(Rc::clone(&cell));
    let title = HSTRING::from(spec.title.as_ref());
    // SAFETY: the class is registered with this instance; `raw` stays valid (we hold `cell`) and is taken over by
    // WM_NCCREATE; no state is borrowed while the creation messages arrive.
    let created = unsafe {
        CreateWindowExW(
            ex,
            &class,
            &title,
            style,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            spec.owner,
            None,
            Some(hinst),
            Some(raw.cast()),
        )
    };
    let hwnd = match created {
        Ok(h) if cell.attached.get() => h,
        Ok(h) => {
            // SAFETY: WM_NCCREATE never took the reference into_raw made: it is released here, once; the window,
            // created without our state, is ours to destroy.
            unsafe {
                drop(Rc::from_raw(raw));
                let _ = DestroyWindow(h);
            }
            return Err(UiError::NoSuchWindow);
        }
        Err(e) => {
            if !cell.attached.get() {
                // SAFETY: as above: the reference into_raw made was never taken.
                drop(unsafe { Rc::from_raw(raw) });
            }
            return Err(UiError::Win32(e));
        }
    };
    lp.remember_window(id, hwnd, Rc::downgrade(&cell), opaque, mailbox);
    cell.run(hwnd, |h| {
        hooks::after_create(h, hwnd);
        h.client = client_size(hwnd);
        h.layout();
        h.defer(Sys::Show(show));
    });
    Ok(id)
}

#[cfg(test)]
impl WinHost {
    /// The state of an opaque window at `dpi` with a `client` of that size and no window behind it (a null HWND): the
    /// tests run the host's functions on it. The calls it defers must never run.
    pub(crate) fn headless(lp: &Rc<LoopShared>, screen: Box<dyn Screen>, dpi: u32, client: SizeP) -> WinHost {
        WinHost {
            hwnd: HWND::default(),
            screen,
            disp: Dispatcher::new(),
            res: Res::new(Box::new(GdiTextEngine::new().expect("a text engine"))),
            lp: Rc::clone(lp),
            kind: WindowKind::Opaque { min: crate::geom::SizeL { w: 1, h: 1 }, caption: Caption::System },
            caption: Caption::System,
            placement: None,
            scale: Scale { dpi },
            client,
            layout_gen: None,
            needs_layout: true,
            damage: Damage::default(),
            presenter: Presenter::Banded(BandedPresenter::new()),
            clipboard: SysClipboard { hwnd: HWND::default() },
            deferred: Vec::new(),
            tracking_leave: false,
            high_surrogate: 0,
            buttons: 0,
            frame_timer: false,
            animating: Vec::new(),
            deadline: None,
            app_timers: Vec::new(),
            mailbox: Arc::new(Mutex::new(Mailbox::default())),
            icon_resource: None,
            icons: [None, None],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_queue_keeps_order() {
        let mut q = RetryQueue::default();
        assert!(!q.blocks() && !q.want_post(), "an empty queue blocks nothing and posts nothing");
        for i in 0..5 {
            q.push(Retry::Msg { msg: 0x200 + i, wp: i as usize, lp: -(i as isize) });
        }
        q.push(Retry::Repaint(RectP::wh(1, 2, 3, 4)));
        assert!(q.blocks(), "a new message goes behind the waiting ones");
        assert!(q.want_post() && !q.want_post(), "one WM_OLED_RETRY in flight");
        let got = q.take();
        assert_eq!(got.len(), 6);
        for (i, r) in got.iter().take(5).enumerate() {
            assert_eq!(*r, Retry::Msg { msg: 0x200 + i as u32, wp: i, lp: -(i as isize) }, "oldest first");
        }
        assert_eq!(got[5], Retry::Repaint(RectP::wh(1, 2, 3, 4)));
        assert!(!q.blocks() && q.len() == 0);
        q.push(Retry::Msg { msg: 1, wp: 0, lp: 0 });
        assert!(q.want_post(), "the next message after a replay posts again");
    }

    #[test]
    fn retry_queue_keeps_copy_data_order() {
        let mut q = RetryQueue::default();
        let payloads: [&[u8]; 3] = [b"first", b"", b"third payload"];
        for (i, p) in payloads.iter().enumerate() {
            q.push(Retry::CopyData { tag: 0x5449_0000 + i, bytes: p.to_vec() });
            if i == 0 {
                q.push(Retry::Msg { msg: 0xC123, wp: 1, lp: 2 });
            }
        }
        assert!(q.blocks() && q.want_post());
        let got = q.take();
        let want = [
            Retry::CopyData { tag: 0x5449_0000, bytes: b"first".to_vec() },
            Retry::Msg { msg: 0xC123, wp: 1, lp: 2 },
            Retry::CopyData { tag: 0x5449_0001, bytes: Vec::new() },
            Retry::CopyData { tag: 0x5449_0002, bytes: b"third payload".to_vec() },
        ];
        assert_eq!(got, want, "three payloads pushed while borrowed replay in order, with the messages between them");
    }

    #[test]
    fn copy_data_over_64_kib_is_refused() {
        assert_eq!(COPY_DATA_MAX, 65_536);
        assert!(copy_data_fits(0) && copy_data_fits(1) && copy_data_fits(64 * 1024), "up to 64 KiB reaches the screen");
        assert!(!copy_data_fits(64 * 1024 + 1) && !copy_data_fits(usize::MAX), "above 64 KiB is refused");
    }

    #[test]
    fn front_outcome_table() {
        assert_eq!(front_outcome(true, false), FrontResult::Foreground);
        assert_eq!(front_outcome(true, true), FrontResult::Foreground);
        assert_eq!(front_outcome(false, true), FrontResult::Topmost);
        assert_eq!(front_outcome(false, false), FrontResult::None);
    }

    #[test]
    fn retry_queue_is_bounded() {
        let mut q = RetryQueue::default();
        for _ in 0..RetryQueue::CAP + 3 {
            q.push(Retry::Msg { msg: 1, wp: 0, lp: 0 });
        }
        assert_eq!((q.len(), q.dropped), (RetryQueue::CAP, 3));
    }

    #[test]
    fn replaying_does_not_block() {
        let mut q = RetryQueue::default();
        q.push(Retry::Msg { msg: 1, wp: 0, lp: 0 });
        q.replaying = true;
        assert!(!q.blocks(), "the messages a replayed one sends run at once");
    }
}
