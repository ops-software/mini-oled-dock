//! The EventLoop and LoopCx (P2-kit 2.5; K1a.7, K1a.11, KA2): one loop per UI thread, its windows, the handles it
//! waits on and the theme it follows. The loop never polls and never sleeps: it waits in MsgWaitForMultipleObjectsEx
//! with no timeout, then dispatches every message, then does the idle work of each window.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject, INFINITE, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, IsIconic, IsWindowVisible, MsgWaitForMultipleObjectsEx, PeekMessageW,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOW, WM_QUIT,
};

use super::proxy::{self, Mailbox};
use super::tooltip::Tip;
use super::window::{self, HostCell};
use super::{hooks, AppId, HandleId, PlacementStore, Proxy, UiError, WindowRef, WindowSpec};
use super::{WM_OLED_A11Y, WM_OLED_DATA, WM_OLED_RETRY};
use crate::geom::PointP;
use crate::screen::Screen;
use crate::strings::{Lang, UiLang};
use crate::theme::{SysOverlay, ThemeFile, ThemeSpec, Tokens};

/// At most this many handles of the app's (MAXIMUM_WAIT_OBJECTS is 64, minus room for the kit's own).
pub(crate) const MAX_HANDLES: usize = 56;

/// Counters of the loop, read by the gallery's live measurements (`--measure idle`).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopStats {
    /// MsgWaitForMultipleObjectsEx returns.
    pub wakes: u64,
    /// Win32 timers the kit armed (frame, deadline and app timers).
    pub timers_armed: u64,
    /// Kit timers armed now.
    pub timers_live: u32,
}

/// One window of the loop.
pub(crate) struct WinEntry {
    pub(crate) id: WindowRef,
    pub(crate) hwnd: HWND,
    pub(crate) cell: Weak<HostCell>,
    pub(crate) opaque: bool,
    pub(crate) mailbox: Arc<Mutex<Mailbox>>,
    /// The messages the app routed to the screen (EventLoop::route_message, TK1).
    pub(crate) routed: Vec<u32>,
}

/// What the loop and its windows share (one UI thread).
pub(crate) struct LoopShared {
    #[allow(dead_code)] // The app's id: the a11y bridge (KG) names the window's app with it.
    pub(crate) app: AppId,
    pub(crate) lang: Cell<UiLang>,
    pub(crate) spec: RefCell<ThemeSpec>,
    pub(crate) sys: Cell<SysOverlay>,
    pub(crate) tokens: RefCell<Rc<Tokens>>,
    pub(crate) windows: RefCell<Vec<WinEntry>>,
    next_window: Cell<u32>,
    pub(crate) quit: Cell<Option<i32>>,
    pub(crate) store: RefCell<Option<Box<dyn PlacementStore>>>,
    pub(crate) caption_spike: Cell<bool>,
    /// The window classes this loop registered (or found registered).
    pub(crate) classes: RefCell<Vec<&'static str>>,
    start: Instant,
    pub(crate) stats: Cell<LoopStats>,
    /// Lines per wheel notch, vertical and horizontal (SPI_GETWHEELSCROLLLINES, SPI_GETWHEELSCROLLCHARS).
    pub(crate) wheel_lines: Cell<u32>,
    pub(crate) wheel_chars: Cell<u32>,
    /// The tooltip window of the loop's windows.
    pub(crate) tip: RefCell<Tip>,
}

impl LoopShared {
    /// Milliseconds since the loop was made: the clock of every context.
    pub(crate) fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub(crate) fn next_window_ref(&self) -> WindowRef {
        let n = self.next_window.get().wrapping_add(1).max(1);
        self.next_window.set(n);
        WindowRef(n)
    }

    pub(crate) fn remember_window(
        &self,
        id: WindowRef,
        hwnd: HWND,
        cell: Weak<HostCell>,
        opaque: bool,
        mailbox: Arc<Mutex<Mailbox>>,
    ) {
        self.windows.borrow_mut().push(WinEntry { id, hwnd, cell, opaque, mailbox, routed: Vec::new() });
    }

    /// True when the app routed `msg` to the screen of the window `hwnd` (EventLoop::route_message).
    pub(crate) fn routes(&self, hwnd: HWND, msg: u32) -> bool {
        self.windows.try_borrow().is_ok_and(|w| w.iter().any(|e| e.hwnd == hwnd && e.routed.contains(&msg)))
    }

    /// WM_NCDESTROY: the window leaves the loop; its mailbox closes (a later send returns false).
    pub(crate) fn forget_window(&self, hwnd: HWND) {
        let mut w = self.windows.borrow_mut();
        for e in w.iter().filter(|e| e.hwnd == hwnd) {
            proxy::lock(&e.mailbox).close();
        }
        w.retain(|e| e.hwnd != hwnd);
    }

    /// Resolves the spec over the system state; the tokens change only with the generation (an unchanged theme
    /// repaints nothing).
    pub(crate) fn resolve(&self) {
        let t = Tokens::resolve(&self.spec.borrow(), &self.sys.get());
        if t.generation != self.tokens.borrow().generation {
            *self.tokens.borrow_mut() = Rc::new(t);
        }
    }

    /// WM_SETTINGCHANGE and its kin: the system's accessibility state and wheel settings read again, then resolve().
    pub(crate) fn read_system(&self) {
        self.sys.set(SysOverlay::read());
        let (lines, chars) = super::input::read_wheel();
        self.wheel_lines.set(lines);
        self.wheel_chars.set(chars);
        self.resolve();
    }

    pub(crate) fn opaque_count(&self) -> usize {
        self.windows.borrow().iter().filter(|e| e.opaque).count()
    }

    /// The windows as (hwnd, cell) pairs, so none of them is borrowed while the list is.
    pub(crate) fn window_cells(&self) -> Vec<(HWND, Rc<HostCell>)> {
        self.windows.borrow().iter().filter_map(|e| e.cell.upgrade().map(|c| (e.hwnd, c))).collect()
    }

    pub(crate) fn count(&self, f: impl FnOnce(&mut LoopStats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }
}

/// True for a message an app may route to a screen (TK1): WM_APP..=0xBFFF, the private range, less the kit's own
/// messages, and 0xC000..=0xFFFF, the registered ones (RegisterWindowMessageW). No system message is routable.
pub(crate) fn routable(msg: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::WM_APP;
    let kit = [WM_OLED_RETRY, WM_OLED_DATA, WM_OLED_A11Y];
    ((WM_APP..=0xBFFF).contains(&msg) && !kit.contains(&msg)) || (0xC000..=0xFFFF).contains(&msg)
}

/// What runs when a handle is signalled.
type OnSignal = Box<dyn FnMut(&mut LoopCx)>;

/// A handle the loop waits on and what runs when it is signalled.
struct HandleSlot {
    id: HandleId,
    h: HANDLE,
    /// Taken out while it runs.
    cb: Option<OnSignal>,
}

/// The UI thread's loop. Not Send: one per UI thread.
pub struct EventLoop {
    shared: Rc<LoopShared>,
    handles: Vec<HandleSlot>,
    next_handle: u32,
    _ui_thread: PhantomData<*const ()>,
}

impl EventLoop {
    /// A loop for `app` speaking `lang` (resolved by UiLang::from_lang) with `theme` resolved over the system's
    /// accessibility state (SysOverlay::read); the process is made per-monitor DPI aware (v2) if it is not yet.
    pub fn new(app: AppId, lang: Lang, theme: ThemeSpec) -> Result<EventLoop, UiError> {
        super::dpi::ensure_per_monitor_v2();
        let sys = SysOverlay::read();
        let tokens = Tokens::resolve(&theme, &sys);
        let (wheel_lines, wheel_chars) = super::input::read_wheel();
        let shared = LoopShared {
            app,
            lang: Cell::new(UiLang::from_lang(lang)),
            spec: RefCell::new(theme),
            sys: Cell::new(sys),
            tokens: RefCell::new(Rc::new(tokens)),
            windows: RefCell::new(Vec::new()),
            next_window: Cell::new(0),
            quit: Cell::new(None),
            store: RefCell::new(None),
            caption_spike: Cell::new(false),
            classes: RefCell::new(Vec::new()),
            start: Instant::now(),
            stats: Cell::new(LoopStats::default()),
            wheel_lines: Cell::new(wheel_lines),
            wheel_chars: Cell::new(wheel_chars),
            tip: RefCell::new(Tip::new()),
        };
        Ok(EventLoop { shared: Rc::new(shared), handles: Vec::new(), next_handle: 0, _ui_thread: PhantomData })
    }

    pub fn set_placement_store(&mut self, s: Box<dyn PlacementStore>) {
        *self.shared.store.borrow_mut() = Some(s);
    }

    /// Opens a window with `screen`: laid out at once, shown, painted by the loop.
    pub fn open<S: Screen + 'static>(&mut self, spec: WindowSpec, screen: S) -> Result<WindowRef, UiError> {
        window::open(&self.shared, spec, Box::new(screen))
    }

    /// The window's proxy (one with no window behind it once the window is gone).
    pub fn proxy(&self, w: WindowRef) -> Proxy {
        let windows = self.shared.windows.borrow();
        windows.iter().find(|e| e.id == w).map_or_else(Proxy::detached, |e| Proxy::new(e.hwnd, Arc::clone(&e.mailbox)))
    }

    /// Waits on `h` too (at most 56 handles); `on_signal` runs on this thread when it is signalled. The handle stays
    /// the caller's: it must stay open while it is in the set, and a manual-reset one must be reset by `on_signal`.
    pub fn add_handle(&mut self, h: HANDLE, on_signal: Box<dyn FnMut(&mut LoopCx)>) -> Result<HandleId, UiError> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(UiError::TooManyHandles);
        }
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        let id = HandleId(self.next_handle);
        self.handles.push(HandleSlot { id, h, cb: Some(on_signal) });
        Ok(id)
    }

    /// Stops waiting on a handle (it stays open: it is the caller's).
    pub fn remove_handle(&mut self, id: HandleId) {
        self.handles.retain(|s| s.id != id);
    }

    /// Resolves the theme over the current system state; if the generation changed, every window relayouts and repaints
    /// at the idle pass.
    pub fn set_theme(&mut self, spec: ThemeSpec) {
        *self.shared.spec.borrow_mut() = spec;
        self.shared.resolve();
    }

    /// theme.json -> set_theme(ThemeSpec::from(file)) -> on_change (the app's language and the rest). The watch's
    /// change handle joins the wait set (none: nothing to follow, Ok); a signal arms one 60 ms one-shot waitable timer,
    /// in the wait set only while armed (SPEC-platform 5.3.3's coalescing, no polling); when it fires the file is
    /// reloaded once.
    #[allow(clippy::type_complexity)]
    pub fn follow_theme_file(
        &mut self,
        watch: oled_config::theme::ThemeWatch,
        on_change: Box<dyn FnMut(&mut LoopCx, &ThemeFile)>,
    ) -> Result<HandleId, UiError> {
        let Some(change) = watch.change_handle() else { return Ok(HandleId(0)) };
        // SAFETY: an unnamed synchronization timer with default security; Follow's drop closes it.
        let timer = unsafe { CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0) }.map_err(UiError::Win32)?;
        let f = Rc::new(RefCell::new(Follow { watch, timer, slot: None, coalesce: Coalesce::default(), on_change }));
        self.add_handle(change, Box::new(move |cx: &mut LoopCx| on_theme_signal(cx, &f)))
    }

    /// The loop's counters (the gallery's live measurements).
    #[doc(hidden)]
    pub fn stats(&self) -> LoopStats {
        self.shared.stats.get()
    }

    /// The language of every window; they lay out again.
    pub fn set_lang(&mut self, lang: Lang) {
        self.shared.lang.set(UiLang::from_lang(lang));
        for (hwnd, cell) in self.shared.window_cells() {
            cell.run(hwnd, |h| {
                h.needs_layout = true;
                h.redraw();
            });
        }
    }

    /// The window's HWND, while it exists.
    pub fn hwnd(&self, w: WindowRef) -> Option<HWND> {
        self.shared.windows.borrow().iter().find(|e| e.id == w).map(|e| e.hwnd)
    }

    /// Delivers `msg`, posted or sent to the window, to its Screen::app_message, in order with the window's other
    /// messages (TK1). Only WM_APP..=0xBFFF (less the kit's own messages) and 0xC000..=0xFFFF (RegisterWindowMessageW)
    /// can be routed: false for any other message (0 included, a failed registration) or a window that is gone.
    pub fn route_message(&mut self, w: WindowRef, msg: u32) -> bool {
        if !routable(msg) {
            return false;
        }
        let mut windows = self.shared.windows.borrow_mut();
        let Some(e) = windows.iter_mut().find(|e| e.id == w) else { return false };
        if !e.routed.contains(&msg) {
            e.routed.push(msg);
        }
        true
    }

    /// S-CAPTION's rig only (K3.5): Custom takes effect for this loop's windows while CUSTOM_CAPTION is false. An app
    /// that calls it fails review.
    #[doc(hidden)]
    pub fn caption_spike(&mut self) {
        self.shared.caption_spike.set(true);
    }

    /// The gallery's empty-window measure without text only: the windows of this process open without the text system's
    /// warm-up, so a window that draws no text loads none. An app that calls it fails review.
    #[doc(hidden)]
    pub fn no_text_warm_up(&mut self) {
        crate::text::warm::skip();
    }

    /// What the loop shares with its windows (tests that build a window's state with no window).
    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Rc<LoopShared> {
        &self.shared
    }

    /// Runs until the last opaque window is destroyed (0) or quit(code); with no opaque window it returns 0 at once.
    /// The windows still open when it returns are destroyed.
    pub fn run(mut self) -> i32 {
        let code = loop {
            if let Some(c) = self.shared.quit.get() {
                break c;
            }
            if self.shared.opaque_count() == 0 {
                break 0;
            }
            let mut set: Vec<HANDLE> = self.handles.iter().map(|s| s.h).collect();
            let own = set.len();
            hooks::wait_handles(&mut set);
            // SAFETY: every handle in `set` is a waitable handle its owner keeps open while it is in the set.
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&set), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            self.shared.count(|s| s.wakes += 1);
            let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if r == WAIT_FAILED {
                self.prune_handles();
            } else if i < own {
                self.signal(i);
            } else if i < set.len() {
                self.pacing_signal();
            }
            if let Some(c) = self.pump() {
                self.shared.quit.set(Some(c));
            }
            self.idle();
        };
        self.close_all();
        code
    }

    /// Every waiting message to its window; Some(code) on WM_QUIT.
    fn pump(&mut self) -> Option<i32> {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a writable MSG; the messages are this thread's.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return Some(msg.wParam.0 as i32);
            }
            // SAFETY: `msg` was just filled by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        None
    }

    /// The idle work of every window: relayout where due, then the damage handed to Windows.
    fn idle(&mut self) {
        for (hwnd, cell) in self.shared.window_cells() {
            cell.run(hwnd, |h| h.idle());
        }
    }

    /// Handle `i` was signalled: its callback runs with a LoopCx (and goes back unless it was removed meanwhile).
    fn signal(&mut self, i: usize) {
        let id = self.handles[i].id;
        let Some(mut cb) = self.handles[i].cb.take() else { return };
        cb(&mut LoopCx { lp: self });
        if let Some(s) = self.handles.iter_mut().find(|s| s.id == id) {
            s.cb.get_or_insert(cb);
        }
    }

    /// A handle of hooks::wait_handles was signalled (the frame pacing's): every window takes a frame.
    fn pacing_signal(&mut self) {
        for (hwnd, cell) in self.shared.window_cells() {
            cell.run(hwnd, super::pacing::on_frame_timer);
        }
    }

    /// MsgWait failed: each handle is probed with a zero-timeout wait. The ones that cannot be waited on (closed by
    /// their owner) leave the set. A probe that is satisfied consumes the signal (an auto-reset event, a semaphore, a
    /// synchronization timer) or takes ownership (a mutex), as MsgWait would have: that handle's callback runs.
    fn prune_handles(&mut self) {
        let mut fired = Vec::new();
        self.handles.retain(|s| {
            // SAFETY: a zero-timeout wait on a handle of the set: an invalid one returns WAIT_FAILED; a satisfied one
            // changes the object's state like MsgWait does, so its callback runs below.
            let r = unsafe { WaitForSingleObject(s.h, 0) };
            if r == WAIT_OBJECT_0 {
                fired.push(s.id);
            }
            r != WAIT_FAILED
        });
        for id in fired {
            if let Some(i) = self.handles.iter().position(|s| s.id == id) {
                self.signal(i);
            }
        }
    }

    /// Destroys the windows still open (run's end, or a loop dropped without running).
    fn close_all(&mut self) {
        let open: Vec<HWND> = self.shared.windows.borrow().iter().map(|e| e.hwnd).collect();
        for hwnd in open {
            // SAFETY: a window of this thread's loop; no state is borrowed at the loop's level.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
    }
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// The coalescing of theme.json's change notifications (SPEC-platform 5.3.3): the first signal arms the 60 ms timer, the
/// next ones while it is armed change nothing; when it fires the file is reloaded once.
#[derive(Debug, Default)]
pub(crate) struct Coalesce {
    armed: bool,
}

impl Coalesce {
    pub(crate) const DELAY_MS: u64 = 60;

    /// A change signal: true when the timer must be armed now.
    pub(crate) fn signal(&mut self) -> bool {
        !std::mem::replace(&mut self.armed, true)
    }

    /// The timer fired (or could not be armed): the next signal arms it again.
    pub(crate) fn fire(&mut self) {
        self.armed = false;
    }
}

/// The theme a reload sets: that of the file; none when the reload gave nothing (an invalid, missing or unchanged file
/// keeps the current theme).
pub(crate) fn reloaded(file: Option<&ThemeFile>) -> Option<ThemeSpec> {
    file.map(ThemeSpec::from)
}

/// What the app runs after a theme.json change.
type OnThemeChange = Box<dyn FnMut(&mut LoopCx, &ThemeFile)>;

/// theme.json's follower: the watch, the coalescing timer and the app's callback.
struct Follow {
    watch: oled_config::theme::ThemeWatch,
    timer: HANDLE,
    /// The timer's slot in the wait set, while it is armed.
    slot: Option<HandleId>,
    coalesce: Coalesce,
    on_change: OnThemeChange,
}

impl Drop for Follow {
    fn drop(&mut self) {
        // SAFETY: the timer follow_theme_file created; nothing else closes it.
        let _ = unsafe { CloseHandle(self.timer) };
    }
}

/// The change handle was signalled: the notification is re-armed, and the 60 ms timer armed and put in the wait set
/// unless it already is.
fn on_theme_signal(cx: &mut LoopCx, f: &Rc<RefCell<Follow>>) {
    let (arm, timer) = {
        let mut s = f.borrow_mut();
        s.watch.on_signaled();
        (s.coalesce.signal(), s.timer)
    };
    if !arm {
        return;
    }
    let due = -(Coalesce::DELAY_MS as i64 * 10_000);
    // SAFETY: the follower's timer; `due` lives for the call; no completion routine.
    let armed = unsafe { SetWaitableTimer(timer, &due, 0, None, None, false) }.is_ok();
    let follow = Rc::clone(f);
    let slot = if armed {
        cx.lp.add_handle(timer, Box::new(move |cx: &mut LoopCx| on_theme_timer(cx, &follow))).ok()
    } else {
        None
    };
    match slot {
        Some(id) => f.borrow_mut().slot = Some(id),
        // No timer, or no room in the wait set: the reload happens now.
        None => on_theme_timer(cx, f),
    }
}

/// The 60 ms timer fired: it leaves the wait set; the file is reloaded, the theme set and the app told.
fn on_theme_timer(cx: &mut LoopCx, f: &Rc<RefCell<Follow>>) {
    let slot = {
        let mut s = f.borrow_mut();
        s.coalesce.fire();
        s.slot.take()
    };
    if let Some(id) = slot {
        cx.lp.remove_handle(id);
    }
    let file = f.borrow_mut().watch.reload().cloned();
    let (Some(spec), Some(file)) = (reloaded(file.as_ref()), file.as_ref()) else { return };
    cx.set_theme(spec);
    (f.borrow_mut().on_change)(cx, file);
}

/// What LoopCx::show does to a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// SW_HIDE.
    Hide,
    /// SW_RESTORE if iconic, SW_SHOW if hidden, then SetForegroundWindow.
    Front,
}

/// One Win32 call of LoopCx::show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowCall {
    Hide,
    Restore,
    Reveal,
    Foreground,
}

/// The calls that bring `how` about for a window that is (or is not) iconic and visible, in order.
pub(crate) fn show_calls(how: Show, iconic: bool, visible: bool) -> &'static [ShowCall] {
    match how {
        Show::Hide => &[ShowCall::Hide],
        Show::Front if iconic => &[ShowCall::Restore, ShowCall::Foreground],
        Show::Front if !visible => &[ShowCall::Reveal, ShowCall::Foreground],
        Show::Front => &[ShowCall::Foreground],
    }
}

/// What a handle callback or the theme follower may do to the loop.
pub struct LoopCx<'a> {
    lp: &'a mut EventLoop,
}

impl LoopCx<'_> {
    /// Screen::data of window `w`, through its mailbox (dropped when the window is gone).
    pub fn send(&mut self, w: WindowRef, msg: super::AppMsg) {
        let _ = self.lp.proxy(w).send(msg);
    }

    /// The loop's counters (the gallery's live measurements).
    #[doc(hidden)]
    pub fn stats(&self) -> LoopStats {
        self.lp.stats()
    }

    pub fn set_theme(&mut self, spec: ThemeSpec) {
        self.lp.set_theme(spec);
    }

    pub fn open<S: Screen + 'static>(&mut self, spec: WindowSpec, screen: S) -> Result<WindowRef, UiError> {
        self.lp.open(spec, screen)
    }

    /// Ends run() with `code`.
    pub fn quit(&mut self, code: i32) {
        self.lp.shared.quit.set(Some(code));
    }

    /// The window's proxy; None once the window is gone.
    pub fn proxy(&self, w: WindowRef) -> Option<Proxy> {
        self.lp.hwnd(w).map(|_| self.lp.proxy(w))
    }

    /// The window's HWND, while it exists.
    pub fn hwnd(&self, w: WindowRef) -> Option<HWND> {
        self.lp.hwnd(w)
    }

    /// The language of every window: the kit's strings follow and every window lays out and repaints.
    pub fn set_lang(&mut self, lang: Lang) {
        self.lp.set_lang(lang);
    }

    /// Hides the window or brings it to the front. The loop holds no window borrow while a callback runs, so the
    /// messages these calls send re-enter the window's procedure safely. False for a window that is gone.
    pub fn show(&mut self, w: WindowRef, how: Show) -> bool {
        let Some(hwnd) = self.lp.hwnd(w) else { return false };
        // SAFETY: `hwnd` belongs to this loop's thread and was alive a line ago; the queries only read.
        let (iconic, visible) = unsafe { (IsIconic(hwnd).as_bool(), IsWindowVisible(hwnd).as_bool()) };
        for call in show_calls(how, iconic, visible) {
            // SAFETY: as above; no window borrow is held here (the loop released them before the callback).
            unsafe {
                match call {
                    ShowCall::Hide => drop(ShowWindow(hwnd, SW_HIDE)),
                    ShowCall::Restore => drop(ShowWindow(hwnd, SW_RESTORE)),
                    ShowCall::Reveal => drop(ShowWindow(hwnd, SW_SHOW)),
                    ShowCall::Foreground => drop(SetForegroundWindow(hwnd)),
                }
            }
        }
        true
    }

    /// Moves the window's top-left corner to `at` (physical screen coordinates) without sizing, re-stacking or
    /// activating it. False for a window that is gone or a move Windows refused.
    pub fn move_to(&mut self, w: WindowRef, at: PointP) -> bool {
        let Some(hwnd) = self.lp.hwnd(w) else { return false };
        // SAFETY: a live window of this thread; no window borrow is held.
        unsafe { SetWindowPos(hwnd, None, at.x, at.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Effect;
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;

    /// One node at a fixed place.
    struct One(crate::command::CommandTable);

    impl Screen for One {
        fn layout(&mut self, cx: &mut crate::cx::LayoutCx, avail: crate::geom::SizeP) -> crate::geom::SizeP {
            use crate::node::{Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
            let n = Node::new(NodeId::new(WidgetId(1), 0), Role::Button, crate::geom::RectP::wh(10, 10, 80, 28));
            cx.push(n.label(Text::from("one")).focus(Focusable::Tab).flags(NodeFlags::HOVER_FADE));
            avail
        }

        fn paint(&self, cx: &mut crate::cx::PaintCx) {
            let _ = cx;
        }

        fn event(&mut self, cx: &mut crate::cx::EventCx, ev: &crate::event::Event) -> crate::event::Reply {
            let _ = (cx, ev);
            crate::event::Reply::None
        }

        fn commands(&self) -> &crate::command::CommandTable {
            &self.0
        }
    }

    #[test]
    fn show_mapping_table() {
        use ShowCall::*;
        for (iconic, visible) in [(false, false), (false, true), (true, true), (true, false)] {
            assert_eq!(show_calls(Show::Hide, iconic, visible), &[Hide]);
        }
        assert_eq!(show_calls(Show::Front, true, true), &[Restore, Foreground]);
        assert_eq!(show_calls(Show::Front, true, false), &[Restore, Foreground]);
        assert_eq!(show_calls(Show::Front, false, false), &[Reveal, Foreground]);
        assert_eq!(show_calls(Show::Front, false, true), &[Foreground]);
    }

    #[test]
    fn theme_unchanged_does_not_repaint() {
        let mut lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let before = Rc::clone(&lp.shared.tokens.borrow());
        lp.set_theme(ThemeSpec::oled());
        assert!(Rc::ptr_eq(&before, &lp.shared.tokens.borrow()), "the same generation: no window sees new tokens");
        let mut grey = ThemeSpec::oled();
        grey.look.seed.bg = crate::theme::Rgb(0x1C, 0x1C, 0x1E);
        lp.set_theme(grey);
        assert_ne!(before.generation, lp.shared.tokens.borrow().generation, "a new generation repaints every window");
        // A relayout with the same tokens (what a window does at the idle pass) damages nothing.
        let mut h =
            Harness::new(One(crate::command::CommandTable::new()), SizeL { w: 200, h: 100 }, 96, Tokens::oled());
        h.hover(PointL { x: 20, y: 20 }).advance(300);
        let _ = h.render();
        h.dispatcher().effects.clear();
        h.set_tokens(Tokens::oled());
        let damage = h.dispatcher().effects.iter().any(|e| matches!(e, Effect::Damage(_) | Effect::Redraw));
        assert!(!damage, "{:?}", h.dispatcher().effects);
    }

    #[test]
    fn theme_follow_coalesces() {
        let mut c = Coalesce::default();
        let arms = (0..3).filter(|_| c.signal()).count();
        assert_eq!(arms, 1, "three signals within the delay arm one timer: one reload");
        c.fire();
        assert!(c.signal(), "after the reload the next signal arms it again");
        assert_eq!(Coalesce::DELAY_MS, 60);
        assert_eq!(reloaded(None), None, "a reload that gives nothing sets no theme");
        let file = ThemeFile::default();
        assert_eq!(reloaded(Some(&file)), Some(ThemeSpec::from(&file)));
    }

    #[test]
    fn handle_limit() {
        let mut lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        for i in 0..MAX_HANDLES {
            let id = lp.add_handle(HANDLE(std::ptr::null_mut()), Box::new(|_: &mut LoopCx| {})).unwrap();
            assert_eq!(id, HandleId(i as u32 + 1), "ids are given in order");
        }
        let full = lp.add_handle(HANDLE(std::ptr::null_mut()), Box::new(|_: &mut LoopCx| {}));
        assert!(matches!(full, Err(UiError::TooManyHandles)), "the 57th handle is refused");
        lp.remove_handle(HandleId(3));
        assert!(lp.add_handle(HANDLE(std::ptr::null_mut()), Box::new(|_: &mut LoopCx| {})).is_ok(), "room again");
        assert_eq!(lp.handles.len(), MAX_HANDLES);
    }

    #[test]
    fn prune_runs_what_its_probe_consumed() {
        use windows::core::PCWSTR;
        use windows::Win32::System::Threading::CreateEventW;
        let mut lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        // SAFETY: an unnamed auto-reset event, signalled; closed at the end of the test.
        let ev = unsafe { CreateEventW(None, false, true, PCWSTR::null()) }.unwrap();
        let runs = Rc::new(Cell::new(0));
        let seen = Rc::clone(&runs);
        let _gone = lp.add_handle(HANDLE(std::ptr::null_mut()), Box::new(|_: &mut LoopCx| {})).unwrap();
        let kept = lp.add_handle(ev, Box::new(move |_: &mut LoopCx| seen.set(seen.get() + 1))).unwrap();
        lp.prune_handles();
        let ids: Vec<HandleId> = lp.handles.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![kept], "the handle that cannot be waited on leaves the set");
        assert_eq!(runs.get(), 1, "the probe consumed the auto-reset event's signal: its callback ran");
        lp.prune_handles();
        assert_eq!(runs.get(), 1, "a handle that is not signalled stays without running");
        drop(lp);
        // SAFETY: the event created above, in no wait set any more.
        let _ = unsafe { CloseHandle(ev) };
    }

    #[test]
    fn routed_message_only_in_app_ranges() {
        use windows::Win32::UI::WindowsAndMessaging::{WM_APP, WM_COPYDATA, WM_PAINT, WM_USER};
        let mut lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        // A window entry with no window behind it (no test creates one); it leaves the loop before the loop drops.
        let (w, hwnd) = (WindowRef(7), HWND::default());
        lp.shared.remember_window(w, hwnd, Weak::new(), true, Arc::new(Mutex::new(Mailbox::default())));
        assert!(!lp.route_message(w, WM_PAINT), "a system message");
        assert!(lp.route_message(w, WM_APP + 5), "the app's range");
        assert!(lp.route_message(w, 0xC123), "a registered message");
        assert!(lp.route_message(w, 0xC123), "routing twice is harmless");
        assert!(!lp.route_message(WindowRef(8), WM_APP + 5), "no such window");
        for refused in [0, WM_COPYDATA, WM_USER, WM_APP - 1, WM_OLED_RETRY, WM_OLED_DATA, WM_OLED_A11Y, 0x1_0000] {
            assert!(!lp.route_message(w, refused), "{refused:#x} is not routable");
        }
        for taken in [WM_APP, WM_APP + 4, 0xBFFF, 0xC000, 0xFFFF] {
            assert!(routable(taken), "{taken:#x} is routable");
        }
        assert!(lp.shared.routes(hwnd, WM_APP + 5) && lp.shared.routes(hwnd, 0xC123));
        assert!(!lp.shared.routes(hwnd, WM_PAINT) && !lp.shared.routes(hwnd, WM_APP + 6));
        assert_eq!(lp.shared.windows.borrow()[0].routed, [WM_APP + 5, 0xC123], "each message once");
        lp.shared.forget_window(hwnd);
        assert!(!lp.shared.routes(hwnd, WM_APP + 5), "a window that left the loop routes nothing");
    }

    #[test]
    fn a_loop_without_a_window_returns_at_once() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        assert_eq!(lp.run(), 0);
    }
}
