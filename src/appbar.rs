//! AppBar: while the bar is visible, `config.reserve_space` is on and auto-hide is off (`wanted`), the dock reserves
//! its screen strip like the Windows taskbar does, so maximized windows end above it instead of under it. With auto-hide
//! on nothing is reserved: the bar shows and hides all day, each show would resize every maximized window of the
//! display twice, and the taskbar itself reserves nothing while it auto-hides.
//!
//! Protocol (AppBar messages sent straight to Explorer's `Shell_TrayWnd`, see `call`): `ABM_NEW` (callback `WM_DOCK_APPBAR`) -> `ABM_QUERYPOS` -> the thickness is
//! re-applied on the side that touches the screen edge -> `ABM_SETPOS` -> `ABM_ACTIVATE`; `ABM_REMOVE` frees the
//! strip. The strip is registered only while the bar is visible, on the edge (top / bottom) and display the bar sits
//! on, and it is released when the bar hides, over a fullscreen app detected by the dock itself, when the option goes
//! off and at exit.
//!
//! `ABN_FULLSCREENAPP` parks the strip instead of releasing it: the registration stays, at 0 px, so the "fullscreen
//! app gone" notification still reaches the dock and the strip comes back by itself (Windows sends `ABN_*` only to
//! registered bars: a released strip would never hear the end of the fullscreen app). The notification does not say
//! which display it is about: `ui` tells where the fullscreen window in front is (`FsWhere`), and one on another
//! display (a game on the secondary) leaves the strip as it is (`fullscreen_after_notify`).
//! Seen on Windows 11 with two displays: Windows already sends `ABN_FULLSCREENAPP` only to the bars on
//! the display of the registered window that the fullscreen window covers; the check in `ui` is a second guard for
//! builds or shells that broadcast it. An `ABN_*` handled while nothing is registered is stale or forged and is
//! ignored (`apply_notify`).
//!
//! Display changes (`WM_DISPLAYCHANGE`, resume): `ui` recomputes the bar's geometry, then `sync` with `force` sets the
//! strip again on the display and edge the bar sits on now. Until then the strip granted on the old display rectangle
//! may still cut the work area: `unreserved_work` gives it back to the bar's geometry as long as it still lies on the
//! bar's display, even when that display changed size or position.
//!
//! The dock never uses `ABM_SETAUTOHIDEBAR`: Windows admits one auto-hide bar per edge and display, and the native
//! taskbar's own registration (`taskbar.rs`) must not meet a second one.
//!
//! Release guarantees: `release_all` runs at `ui::shutdown`, in `taskbar::restore` / `restore_force` (normal exit,
//! panic hook, watchdog, `--restore`) and, for a dock that died, from the marker file `appbar-state` next to
//! `taskbar-state` (the registered window handle): the watchdog or the next start sends the `ABM_REMOVE`. The marker
//! is written and read like `taskbar-state` (admin-only folder, atomic write, read without following reparse points).
//! A marker whose handle now names a window that is not a dock's (the handle was reused) removes nothing
//! (`marker_removes`). The watchdog's give-up gives the taskbar back through `taskbar::restore` while the dock runs on:
//! that frees the strip too, and the give-up sets it again (`ui::appbar_resync`; `dock_exiting` tells it from an exit).
//! An `ABM_REMOVE` that does not reach Explorer (busy past the send's bound, flagged as hung, or away) keeps the
//! marker, as `taskbar-state` is kept (`after_remove`): this process sends it again from the 3 s tick
//! (`retry_removal`), and the watchdog after a clean exit or the next start send it from the marker. An `ABM_NEW` that
//! Explorer refuses because it still holds the window (a registration the dock lost track of) is followed by
//! `ABM_REMOVE` and one more `ABM_NEW` (`register_with`).
//!
//! A registration can also be lost without the dock doing anything: the watchdog removes it while the dock hangs (and
//! deletes the marker), or Explorer restarts and the new one never heard of it. The 3 s safety tick (`ui::appbar_tick`)
//! asks `take_lost`, which sees either (`lost`) and forgets the registration, and the strip is set again at once.
//!
//! `ABM_ACTIVATE` and `ABM_WINDOWPOSCHANGED` follow the documented protocol, the second only for a real move or resize
//! (`position_changed`): the bar re-asserts its topmost place every 500 ms while it is shown, and each re-assertion
//! would otherwise be one more synchronous send to Explorer.
//!
//! The pure part (rectangles, decisions) is separate from the Win32 calls and tested. UI thread, except
//! `release_all` (any thread).

use std::sync::{Mutex, MutexGuard};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{SHAllocShared, SHFreeShared, SHLockShared, SHUnlockShared, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, FindWindowExW, GetWindowThreadProcessId, IsWindow, SendMessageTimeoutW, MSGFLT_ALLOW,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_COPYDATA,
};

use crate::log;
use crate::msg::WM_DOCK_APPBAR;

// Documented `SHAppBarMessage` values (as in `taskbar.rs`: no dependence on the crate's constant types).
const ABM_NEW: u32 = 0;
const ABM_REMOVE: u32 = 1;
const ABM_QUERYPOS: u32 = 2;
const ABM_SETPOS: u32 = 3;
const ABM_ACTIVATE: u32 = 6;
const ABM_WINDOWPOSCHANGED: u32 = 9;
/// Notifications (wParam of `WM_DOCK_APPBAR`).
const ABN_POSCHANGED: usize = 1;
const ABN_FULLSCREENAPP: usize = 2;
/// `ABM_WINDOWPOSCHANGED` is sent at most this often (ms): the show / hide animation moves the window every frame.
const WPC_MIN_MS: u64 = 500;
/// An `ABN_POSCHANGED` this soon after our own `ABM_SETPOS` (ms) is its echo, not a change by another bar.
const ECHO_MS: u64 = 300;
/// Name of the marker file, next to `taskbar-state`.
const MARKER_NAME: &str = "appbar-state";
/// Window class of the dock's window (`main`): a marker naming a live window of another class removes nothing.
const DOCK_WINDOW_CLASS: &str = "DockWindow";

/// `[left, top, right, bottom]`, screen pixels.
pub type Rc = [i32; 4];

/// Edge of the display the strip sits on (the dock is only ever at the top or at the bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

impl Edge {
    pub fn from_top(top: bool) -> Edge {
        if top {
            Edge::Top
        } else {
            Edge::Bottom
        }
    }

    /// `ABE_TOP` / `ABE_BOTTOM`.
    fn abe(self) -> u32 {
        match self {
            Edge::Top => 1,
            Edge::Bottom => 3,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
    }
}

/// What the dock asks Windows to reserve: `thickness` px along `edge` of the display `mon` (its full rectangle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub mon: Rc,
    pub edge: Edge,
    pub thickness: i32,
}

/// What Windows granted (after `ABM_QUERYPOS` / `ABM_SETPOS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    edge: Edge,
    rect: Rc,
}

/// What `sync` must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Register if needed, then query and set the position.
    Reserve,
    Release,
}

// ---------------------------------------------------------------- pure logic

/// Thickness of the strip: the bar plus its margin on both sides (the bar sits `margin` away from the edge and
/// keeps the same distance from the windows above it).
pub fn strip_thickness(bar_h: i32, margin: i32) -> i32 {
    bar_h.max(0) + 2 * margin.max(0)
}

/// The strip asked for: full width of the display, `thickness` px (at most the display's height) on `edge`.
pub fn strip_rect(req: &Request) -> Rc {
    let [l, t, r, b] = req.mon;
    let th = req.thickness.clamp(0, (b - t).max(0));
    match req.edge {
        Edge::Top => [l, t, r, t + th],
        Edge::Bottom => [l, b - th, r, b],
    }
}

/// The rectangle after `ABM_QUERYPOS` (which may have moved it away from other bars): the thickness is applied again
/// on the side that does not touch the edge, as the Microsoft documentation prescribes.
pub fn fit_after_query(rc: Rc, edge: Edge, thickness: i32) -> Rc {
    match edge {
        Edge::Top => [rc[0], rc[1], rc[2], rc[1] + thickness],
        Edge::Bottom => [rc[0], rc[3] - thickness, rc[2], rc[3]],
    }
}

/// A rectangle with an area.
pub fn is_usable(rc: Rc) -> bool {
    rc[2] > rc[0] && rc[3] > rc[1]
}

/// true when the granted strip `rect` (on `edge`) still lies on display `mon`: it overlaps the display horizontally
/// and its inner side (the one facing the windows) is inside the display. After a display change the strip keeps the
/// coordinates it was granted with until the next `ABM_SETPOS`: a display that changed size keeps it, one that moved
/// away or was unplugged does not.
pub fn strip_on_display(rect: Rc, edge: Edge, mon: Rc) -> bool {
    let inner = match edge {
        Edge::Top => rect[3],
        Edge::Bottom => rect[1],
    };
    rect[0] < mon[2] && rect[2] > mon[0] && inner > mon[1] && inner < mon[3]
}

/// true when the strip must be reserved: option on, bar visible, no fullscreen app, dock not exiting.
pub fn should_reserve(option: bool, visible: bool, fullscreen: bool, exiting: bool) -> bool {
    option && visible && !fullscreen && !exiting
}

/// true when a registered strip must be parked at 0 px rather than released: the only thing keeping it from being
/// reserved is `ABN_FULLSCREENAPP` (`fs_app`). The registration is kept so the "fullscreen app gone" notification
/// still arrives; for anything else (option off, bar hidden, the dock's own fullscreen detection `fs_dock`, exit) the
/// strip is released as usual.
pub fn park_for_fullscreen_app(option: bool, visible: bool, fs_dock: bool, fs_app: bool, exiting: bool) -> bool {
    fs_app && should_reserve(option, visible, fs_dock, exiting)
}

/// Where the fullscreen window in front is when `ABN_FULLSCREENAPP` arrives (the notification names no display).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsWhere {
    /// A fullscreen / borderless window has the foreground on the bar's display.
    DockDisplay,
    /// The fullscreen window in front is on another display.
    OtherDisplay,
    /// No fullscreen window recognised in front (not the foreground yet, or one with a title bar).
    Unknown,
}

impl FsWhere {
    fn name(self) -> &'static str {
        match self {
            FsWhere::DockDisplay => "on the bar's display",
            FsWhere::OtherDisplay => "on another display",
            FsWhere::Unknown => "display unknown",
        }
    }
}

/// The `ABN_FULLSCREENAPP` state kept for the bar's display after a notification (`on` = its lParam). A fullscreen
/// app on another display (a game on the secondary) leaves the state as it is. When the display cannot be told, the
/// notification is believed: parking for nothing only frees the strip for a while, not parking would keep pushing the
/// windows under a game. An "off" while a fullscreen window still holds the bar's display ended an app elsewhere: the
/// strip stays parked.
pub fn fullscreen_after_notify(on: bool, at: FsWhere, prev: bool) -> bool {
    match (on, at) {
        (true, FsWhere::OtherDisplay) => prev,
        (true, _) => true,
        (false, FsWhere::DockDisplay) => true,
        (false, _) => false,
    }
}

/// Decision of `sync`. `last` is the request already granted (if registered), `force` repeats `ABM_SETPOS` even when
/// it is unchanged (display change, `ABN_POSCHANGED`, resume).
pub fn decide(want: bool, registered: bool, last: Option<Request>, req: Request, force: bool) -> Step {
    match (want, registered) {
        (true, true) if !force && last == Some(req) => Step::Nothing,
        (true, _) => Step::Reserve,
        (false, true) => Step::Release,
        (false, false) => Step::Nothing,
    }
}

/// true when our registration was released from outside (the watchdog sent `ABM_REMOVE` and deleted the marker
/// while the dock was hung): only when we did write the marker, so a marker that could not be written (no data
/// folder, write error) never makes the dock register again on every `sync`.
pub fn released_from_outside(registered: bool, marker_written: bool, marker_present: bool) -> bool {
    registered && marker_written && !marker_present
}

/// true when the option counts: `config.reserve_space` with auto-hide off (`config::effective_autohide`).
pub fn wanted(option: bool, autohide: bool) -> bool {
    option && !autohide
}

/// `Input::exiting`: the taskbar's restore has begun (`taskbar::exiting`) and it was not the watchdog's give-up
/// (`watchdog::gave_up`), after which the dock runs on.
pub fn dock_exiting(restoring: bool, gave_up: bool) -> bool {
    restoring && !gave_up
}

/// Why a registration this process holds is no longer Explorer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// The watchdog removed it while the dock hung (it deleted the marker we wrote).
    Outside,
    /// Explorer restarted: its taskbar window is a new one, which never heard of the strip.
    NewExplorer,
}

/// Whether the registration is lost: `with` = Explorer's taskbar window at `ABM_NEW` (0 = unknown), `now` = the one
/// there now (`None` while Explorer is away: decided once it is back).
pub fn lost(
    registered: bool,
    marker_written: bool,
    marker_present: bool,
    with: isize,
    now: Option<isize>,
) -> Option<Lost> {
    if released_from_outside(registered, marker_written, marker_present) {
        return Some(Lost::Outside);
    }
    match now {
        Some(h) if registered && with != 0 && h != with => Some(Lost::NewExplorer),
        _ => None,
    }
}

/// Outcome of `register_with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Registered {
    /// `ABM_NEW` was taken.
    New,
    /// Explorer refused `ABM_NEW` because it still held the window: taken after an `ABM_REMOVE`.
    AfterRemove,
    /// Not delivered, or refused again: nothing is registered.
    No,
}

/// `ABM_NEW` through `send` (Explorer's answer, `None` = not delivered). Explorer refuses it for a window it already
/// holds (documented): a registration the dock lost track of (an `ABM_REMOVE` that never arrived, an `ABM_NEW` that
/// arrived after its send timed out, a marker deleted by another dock). That record is removed and `ABM_NEW` sent once
/// more, so it heals itself instead of leaving the strip held with nothing left to release it.
fn register_with(mut send: impl FnMut(u32) -> Option<usize>) -> Registered {
    match send(ABM_NEW) {
        None => Registered::No,
        Some(0) => {
            let again = send(ABM_REMOVE).is_some() && send(ABM_NEW).is_some_and(|r| r != 0);
            if again {
                Registered::AfterRemove
            } else {
                Registered::No
            }
        }
        Some(_) => Registered::New,
    }
}

/// What a release does with the marker file after its `ABM_REMOVE`, as `taskbar::after_restore` does with
/// `taskbar-state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterRemove {
    /// Delivered: Explorer no longer holds the strip, the marker has done its job.
    Delete,
    /// Not delivered (Explorer busy past `SEND_TIMEOUT_MS`, flagged as hung, or away): Explorer may still hold the
    /// strip, so the marker stays for the watchdog and the next start.
    KeepUndelivered,
}

/// Pure decision of a release on the marker file (`delivered`: the `ABM_REMOVE` reached Explorer).
fn after_remove(delivered: bool) -> AfterRemove {
    if delivered {
        AfterRemove::Delete
    } else {
        AfterRemove::KeepUndelivered
    }
}

/// Whether a marker left by another process sends `ABM_REMOVE` for its window: when that window is gone (Explorer still
/// holds its strip) or is a dock's (a hung one), not when the handle now names another app's window.
fn marker_removes(window_exists: bool, dock_class: bool) -> bool {
    !window_exists || dock_class
}

/// `WINDOWPOS::flags` of a real move or resize, not of a z-order change alone.
fn position_changed(flags: u32) -> bool {
    let still = SWP_NOMOVE.0 | SWP_NOSIZE.0;
    flags & still != still
}

/// The work area `wa` of display `mon` as if our own strip were not reserved: the dock places itself on the work
/// area, and without this it would climb by its own strip each time it is reserved. Only when the work area ends
/// exactly where the granted strip begins and the strip still lies on `mon` (`strip_on_display`): after a display
/// change `mon` may differ from the rectangle the strip was granted on while the old strip still cuts the work area.
/// The edge given back never goes past the display.
fn unreserved(wa: Rc, mon: Rc, held: Option<Held>) -> Rc {
    let Some(h) = held else { return wa };
    if !strip_on_display(h.rect, h.edge, mon) {
        return wa;
    }
    match h.edge {
        Edge::Top if wa[1] == h.rect[3] => [wa[0], h.rect[1].max(mon[1]), wa[2], wa[3]],
        Edge::Bottom if wa[3] == h.rect[1] => [wa[0], wa[1], wa[2], h.rect[3].min(mon[3])],
        _ => wa,
    }
}

/// Content of the marker file for the registered window.
fn marker_text(hwnd: isize) -> String {
    hwnd.to_string()
}

/// The window handle in the marker file (`None` if empty or not a number).
fn parse_marker(s: &str) -> Option<isize> {
    s.trim().parse::<isize>().ok().filter(|&h| h != 0)
}

// --------------------------------------------------------------------- state

#[derive(Clone, Copy)]
struct State {
    /// Registered window (`HWND` as isize), 0 = not registered.
    hwnd: isize,
    /// The marker file was written for this registration.
    marker: bool,
    /// Explorer's taskbar window at `ABM_NEW` (as isize, 0 = unknown): another one means Explorer restarted.
    explorer: isize,
    last: Option<Request>,
    held: Option<Held>,
    /// `ABN_FULLSCREENAPP` says a fullscreen app is running (only meaningful while registered).
    fullscreen_app: bool,
    /// The registration is kept at 0 px because of `fullscreen_app`.
    parked: bool,
    /// Tick of the last `ABM_WINDOWPOSCHANGED`.
    last_wpc: u64,
    /// Tick of the last `ABM_SETPOS` of ours.
    last_set: u64,
    /// This process' window whose `ABM_REMOVE` did not reach Explorer (0 = none): sent again by the 3 s tick.
    pending: isize,
}

/// Nothing registered: the state at start.
const UNREGISTERED: State = State {
    hwnd: 0,
    marker: false,
    explorer: 0,
    last: None,
    held: None,
    fullscreen_app: false,
    parked: false,
    last_wpc: 0,
    last_set: 0,
    pending: 0,
};

static STATE: Mutex<State> = Mutex::new(UNREGISTERED);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears the registration and what hangs on it (the `ABN_FULLSCREENAPP` state too: an unregistered bar is never told).
fn forget(st: &mut State) {
    st.hwnd = 0;
    st.marker = false;
    st.explorer = 0;
    st.last = None;
    st.held = None;
    st.fullscreen_app = false;
    st.parked = false;
}

/// `ABN_*` applied to the state: `None` while nothing is registered. Explorer notifies registered bars alone, so one
/// handled after the release is stale (posted before it: the dock's own fullscreen detection may hide the bar and
/// release the strip before Explorer's `ABN_FULLSCREENAPP` is dispatched) or forged (UIPI lets `WM_DOCK_APPBAR`
/// through). Taken, an "on" would keep `sync` from reserving until the dock restarts: the "off" goes to registered
/// bars only.
fn apply_notify(st: &mut State, wp: usize, lp: isize, now: u64, at: FsWhere) -> Option<Notify> {
    if st.hwnd == 0 {
        return None;
    }
    let n = notify_decision(wp, lp, now.saturating_sub(st.last_set), st.fullscreen_app, at);
    if let Some(on) = n.fullscreen {
        st.fullscreen_app = on;
    }
    Some(n)
}

/// After the `ABM_REMOVE` of this process' window `hwnd`: not delivered, it stays pending, sent again by the 3 s tick
/// (`retry_removal`) and by `release_all`; delivered, nothing is pending for it any more.
fn removal_sent(st: &mut State, hwnd: isize, delivered: bool) -> AfterRemove {
    if !delivered {
        st.pending = hwnd;
    } else if st.pending == hwnd {
        st.pending = 0;
    }
    after_remove(delivered)
}

/// Records the registration of `hwnd` (`marker`: the marker file was written, `explorer`: Explorer's taskbar window
/// then). A removal still pending for the window is dropped: Explorer took the new `ABM_NEW`, and sent later the old
/// removal would end the new registration.
fn on_registered(st: &mut State, hwnd: isize, marker: bool, explorer: isize) {
    st.hwnd = hwnd;
    st.marker = marker;
    st.explorer = explorer;
    st.fullscreen_app = false;
    st.parked = false;
    if st.pending == hwnd {
        st.pending = 0;
    }
}

/// The registration, if Explorer no longer holds it (`lost`): forgotten without an `ABM_REMOVE` (nothing is left to
/// remove), so the next `sync` registers again. `sync` and the 3 s tick (`ui::appbar_tick`); cheap while nothing is
/// registered.
pub fn take_lost() -> Option<Lost> {
    let (registered, written, with) = {
        let st = state();
        (st.hwnd != 0, st.marker, st.explorer)
    };
    if !registered {
        return None;
    }
    let why = lost(registered, written, marker_exists(), with, explorer_tray().map(|h| h.0 as isize))?;
    forget(&mut state());
    match why {
        Lost::Outside => log!("appbar: the registration was removed from outside (the watchdog, while the dock hung)"),
        Lost::NewExplorer => log!("appbar: Explorer restarted, the registration went with the old one"),
    }
    Some(why)
}

/// `ABN_FULLSCREENAPP` state.
pub fn fullscreen_app() -> bool {
    state().fullscreen_app
}

/// `wa` (the work area of display `mon`) without our own strip: for the dock's own geometry.
pub fn unreserved_work(wa: RECT, mon: RECT) -> RECT {
    let held = state().held;
    let r = unreserved(from_rect(wa), from_rect(mon), held);
    to_rect(r)
}

fn from_rect(r: RECT) -> Rc {
    [r.left, r.top, r.right, r.bottom]
}

fn to_rect(r: Rc) -> RECT {
    RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] }
}

/// What `sync` needs from the UI.
pub struct Input {
    /// `config.reserve_space` with auto-hide off (`wanted`).
    pub option: bool,
    pub visible: bool,
    /// The dock was shown over a fullscreen app.
    pub fullscreen: bool,
    /// The dock is exiting (`dock_exiting`).
    pub exiting: bool,
    /// Repeat the position even when unchanged.
    pub force: bool,
    /// `rcMonitor` of the bar's display.
    pub mon: Rc,
    pub top: bool,
    /// Bar height and its edge margin (physical px).
    pub bar_h: i32,
    pub margin: i32,
}

// --------------------------------------------------------------------- Win32

fn data(h: HWND, edge: Edge, rc: Rc) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: h,
        uCallbackMessage: WM_DOCK_APPBAR,
        uEdge: edge.abe(),
        rc: to_rect(rc),
        lParam: LPARAM(0),
    }
}

/// `APPBARDATA` with 32-bit handles, as it travels inside `WM_COPYDATA` (the layout `taskbar.rs` sends too).
#[repr(C)]
#[derive(Clone, Copy)]
struct WireData {
    cb_size: u32,
    hwnd: u32,
    callback: u32,
    edge: u32,
    rc: [i32; 4],
    lparam: i64,
}

/// `APPBARMSGDATAV3` (64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct WireMsg {
    abd: WireData,
    message: u32,
    _pad0: u32,
    shared: u64,
    pid: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<WireData>() == 0x28 && std::mem::size_of::<WireMsg>() == 0x40);

/// Bound of one AppBar message to Explorer (ms).
const SEND_TIMEOUT_MS: u32 = 1000;

/// Explorer's primary `Shell_TrayWnd`: the first one that is not a dock's tray host.
fn explorer_tray() -> Option<HWND> {
    let mut prev: Option<HWND> = None;
    loop {
        // SAFETY: static class literal; `prev` is None or a window returned by the same enumeration.
        let h = unsafe { FindWindowExW(None, prev, w!("Shell_TrayWnd"), PCWSTR::null()) }.ok()?;
        if h.0.is_null() {
            return None;
        }
        if !crate::taskbar::is_dock_tray_host(h) {
            return Some(h);
        }
        prev = Some(h);
    }
}

/// The AppBar messages whose APPBARDATA travels in a shared block that Explorer answers into. Pure.
fn needs_block(msg: u32) -> bool {
    matches!(msg, ABM_QUERYPOS | ABM_SETPOS)
}

/// One AppBar message, sent straight to Explorer's `Shell_TrayWnd` with `WM_COPYDATA`. Not `SHAppBarMessage`: that
/// goes to the first `Shell_TrayWnd`, which is the dock's own tray host, and within one process shell32 passes the
/// block as a heap pointer, which the tray host's gate rejects (seen live: "AppBar 2 rejected ... is not a shell
/// shared-memory atom", and the work area never changed). For `ABM_QUERYPOS` / `ABM_SETPOS` the APPBARDATA travels
/// in a block made for Explorer's pid, as `tray_host::forward_with_shared` does, and Explorer's answer (the
/// rectangle) is copied back into `d`. Returns Explorer's result, `None` when the message did not arrive (no
/// Explorer, the send failed or timed out).
fn call(msg: u32, d: &mut APPBARDATA) -> Option<usize> {
    let Some(ex) = explorer_tray() else {
        log!("appbar: no Explorer taskbar, AppBar {msg} not sent");
        return None;
    };
    let mut ex_pid = 0u32;
    // SAFETY: `ex_pid` is writable; for a window that is gone it stays 0.
    unsafe { GetWindowThreadProcessId(ex, Some(&mut ex_pid)) };
    if ex_pid == 0 {
        log!("appbar: Explorer taskbar gone, AppBar {msg} not sent");
        return None;
    }
    let abd = WireData {
        cb_size: std::mem::size_of::<WireData>() as u32,
        hwnd: d.hWnd.0 as usize as u32,
        callback: d.uCallbackMessage,
        edge: d.uEdge,
        rc: from_rect(d.rc),
        lparam: d.lParam.0 as i64,
    };
    let block = if needs_block(msg) {
        // SAFETY: `abd` is a local of the declared size; the block is made for Explorer's pid.
        let h = unsafe {
            SHAllocShared(Some((&abd as *const WireData).cast()), std::mem::size_of::<WireData>() as u32, ex_pid)
        };
        if h.is_invalid() {
            log!("appbar: SHAllocShared failed, AppBar {msg} not sent");
            return None;
        }
        Some(h)
    } else {
        None
    };
    // SAFETY: no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    let mut m = WireMsg {
        abd,
        message: msg,
        _pad0: 0,
        shared: block.map_or(0, |h| h.0 as usize as u64),
        pid: if block.is_some() { ex_pid } else { own },
        _pad1: 0,
    };
    let cds = COPYDATASTRUCT {
        dwData: 0,
        cbData: std::mem::size_of::<WireMsg>() as u32,
        lpData: (&mut m as *mut WireMsg).cast(),
    };
    // As in `taskbar::appbar_direct_at`: SMTO_BLOCK keeps the bound whole on the UI thread, and the popup's mouse
    // hook is lifted for the wait (UI thread only).
    let ui = crate::on_ui_thread();
    let hook = if ui { crate::popup::suspend_mouse_hook() } else { false };
    let mut res = 0usize;
    // SAFETY: `cds` and `m` live for the whole call, which copies the data to Explorer; `res` is writable.
    let sent = unsafe {
        SendMessageTimeoutW(
            ex,
            WM_COPYDATA,
            WPARAM(d.hWnd.0 as usize),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            SEND_TIMEOUT_MS,
            Some(&mut res),
        )
    }
    .0 != 0;
    if ui {
        crate::popup::resume_mouse_hook(hook);
    }
    if !sent {
        // A block of a timed-out send is left in place: Explorer may still read it once it pumps again.
        log!("appbar: AppBar {msg} to Explorer failed or timed out");
        return None;
    }
    if let Some(h) = block {
        // SAFETY: block made above by the dock for Explorer's pid (not a value received from outside).
        let back = unsafe { SHLockShared(h, ex_pid) };
        if !back.is_null() {
            // SAFETY: `back` holds the WireData allocated above, with Explorer's answer written in place.
            let ans = unsafe { back.cast::<WireData>().read_unaligned() };
            d.rc = to_rect(ans.rc);
            // SAFETY: `back` obtained from SHLockShared above.
            let _ = unsafe { SHUnlockShared(back) };
        }
        // SAFETY: handle made by SHAllocShared for `ex_pid`, freed exactly once.
        let _ = unsafe { SHFreeShared(h, ex_pid) };
    }
    Some(res)
}

fn marker_path() -> Option<std::path::PathBuf> {
    crate::datadir::state_path().map(|p| p.with_file_name(MARKER_NAME))
}

/// The marker file is there: a registration was not freed yet (`watchdog` checks it after a clean exit).
pub fn marker_exists() -> bool {
    marker_path().is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

/// Writes the marker like `taskbar-state` (atomic, inherited admin-only ACL). true = written.
fn write_marker(hwnd: isize) -> bool {
    let Some(p) = marker_path() else { return false };
    let bytes = marker_text(hwnd);
    match crate::datadir::write_atomic(&p, bytes.as_bytes(), crate::datadir::FileAcl::Inherit) {
        Ok(()) => true,
        Err(e) => {
            log!("appbar: writing the marker file failed: {e}");
            false
        }
    }
}

/// The window handle in the marker file, read without following reparse points.
fn read_marker() -> Option<isize> {
    let p = marker_path()?;
    let bytes = crate::datadir::read_file(&p, crate::datadir::STATE_MAX_BYTES, true).ok()?;
    parse_marker(&String::from_utf8_lossy(&bytes))
}

fn remove_marker() {
    if let Some(p) = marker_path() {
        if let Err(e) = std::fs::remove_file(p) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log!("appbar: removing the marker file failed: {e}");
            }
        }
    }
}

fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut std::ffi::c_void)
}

fn fmt_rc(r: Rc) -> String {
    format!("{},{}-{},{}", r[0], r[1], r[2], r[3])
}

fn now_ms() -> u64 {
    // SAFETY: GetTickCount64 has no preconditions.
    unsafe { GetTickCount64() }
}

/// Applies the option / visibility to the registration: reserves (or moves) the strip, parks it at 0 px under a
/// fullscreen app, or releases it. `reason` is only for the log.
pub fn sync(h: HWND, reason: &str, inp: &Input) {
    let fs_app = fullscreen_app();
    let want = should_reserve(inp.option, inp.visible, inp.fullscreen || fs_app, inp.exiting);
    let req =
        Request { mon: inp.mon, edge: Edge::from_top(inp.top), thickness: strip_thickness(inp.bar_h, inp.margin) };
    if take_lost().is_some() {
        log!("appbar: registering again ({reason})");
    }
    let registered = state().hwnd != 0;
    if registered && park_for_fullscreen_app(inp.option, inp.visible, inp.fullscreen, fs_app, inp.exiting) {
        park(h, reason, req, inp.force);
        return;
    }
    let last = state().last;
    match decide(want, registered, last, req, inp.force) {
        Step::Nothing => {}
        Step::Release => release_own(reason),
        Step::Reserve => reserve(h, reason, req),
    }
}

fn reserve(h: HWND, reason: &str, req: Request) {
    if state().hwnd == 0 {
        // Explorer (medium integrity) notifies the bar with `WM_DOCK_APPBAR` (ABN_FULLSCREENAPP, ABN_POSCHANGED); the
        // dock runs elevated, so UIPI must let that message through to this window.
        // SAFETY: `h` is the dock's own window.
        if let Err(e) = unsafe { ChangeWindowMessageFilterEx(h, WM_DOCK_APPBAR, MSGFLT_ALLOW, None) } {
            log!("appbar: letting ABN_* notifications through UIPI failed: {e}");
        }
        let mut d = data(h, req.edge, [0; 4]);
        match register_with(|msg| call(msg, &mut d)) {
            Registered::No => {
                log!("appbar: ABM_NEW refused or not delivered, no space reserved ({reason})");
                return;
            }
            Registered::AfterRemove => log!("appbar: Explorer still held the window: removed, registered again"),
            Registered::New => {}
        }
        let written = write_marker(h.0 as isize);
        let explorer = explorer_tray().map_or(0, |e| e.0 as isize);
        on_registered(&mut state(), h.0 as isize, written, explorer);
        log!("appbar: registered ({reason})");
    }
    let mut d = data(h, req.edge, strip_rect(&req));
    call(ABM_QUERYPOS, &mut d);
    let rc = fit_after_query(from_rect(d.rc), req.edge, req.thickness);
    d.rc = to_rect(rc);
    call(ABM_SETPOS, &mut d);
    let granted = from_rect(d.rc);
    if !is_usable(granted) {
        log!("appbar: ABM_SETPOS gave an unusable rectangle {}, releasing ({reason})", fmt_rc(granted));
        release_own("unusable rectangle");
        return;
    }
    {
        let mut st = state();
        st.held = Some(Held { edge: req.edge, rect: granted });
        st.last = Some(req);
        st.parked = false;
        st.last_set = now_ms();
    }
    log!(
        "appbar: reserved {} strip {} ({} px) on display {} ({reason})",
        req.edge.name(),
        fmt_rc(granted),
        req.thickness,
        fmt_rc(req.mon)
    );
    call(ABM_ACTIVATE, &mut d);
    verify_work_area(req, granted);
}

/// Keeps the registration but sets a 0 px strip on the bar's edge: nothing is reserved under a fullscreen app, and
/// the end of the fullscreen app is still notified (`ABN_FULLSCREENAPP` off), which reserves the strip again.
fn park(h: HWND, reason: &str, req: Request, force: bool) {
    if state().parked && !force {
        return;
    }
    let zero = Request { thickness: 0, ..req };
    let mut d = data(h, req.edge, strip_rect(&zero));
    call(ABM_QUERYPOS, &mut d);
    d.rc = to_rect(fit_after_query(from_rect(d.rc), req.edge, 0));
    call(ABM_SETPOS, &mut d);
    {
        let mut st = state();
        st.held = None;
        st.last = None;
        st.parked = true;
        st.last_set = now_ms();
    }
    log!("appbar: fullscreen app, strip parked at 0 px, registration kept ({reason})");
}

/// After `ABM_SETPOS` the work area of the display must end where the strip begins; if not, Windows did not take
/// the reservation (for instance the request was rejected on the way): written to the log.
fn verify_work_area(req: Request, granted: Rc) {
    let p = windows::Win32::Foundation::POINT { x: (req.mon[0] + req.mon[2]) / 2, y: (req.mon[1] + req.mon[3]) / 2 };
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` has cbSize initialized and is writable; MONITOR_DEFAULTTONEAREST always returns a monitor.
    let ok = unsafe { GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi) }.as_bool();
    if !ok {
        return;
    }
    let wa = from_rect(mi.rcWork);
    let applied = match req.edge {
        Edge::Top => wa[1] >= granted[3],
        Edge::Bottom => wa[3] <= granted[1],
    };
    if !applied {
        log!("appbar: WARNING the work area {} does not reflect the strip {}", fmt_rc(wa), fmt_rc(granted));
    }
}

/// `ABM_REMOVE` for the window registered by this process (no-op when none). The registration is forgotten whatever
/// the send does, with the `ABN_FULLSCREENAPP` state: an unregistered bar is never told when the fullscreen app ends.
/// A removal that did not reach Explorer keeps the marker and stays pending (`removal_sent`).
fn release_own(reason: &str) {
    let hwnd = {
        let mut st = state();
        let h = st.hwnd;
        forget(&mut st);
        h
    };
    if hwnd != 0 {
        remove_own(hwnd, reason);
    }
}

/// `ABM_REMOVE` for this process' window `hwnd`, then the marker and the pending removal as `removal_sent` says.
fn remove_own(hwnd: isize, reason: &str) {
    let delivered = send_remove(hwnd);
    let after = removal_sent(&mut state(), hwnd, delivered);
    settle_marker(after, reason);
    if delivered {
        log!("appbar: released ({reason})");
    }
}

/// `ABM_REMOVE` for `hwnd`: true = it reached Explorer.
fn send_remove(hwnd: isize) -> bool {
    call(ABM_REMOVE, &mut data(hwnd_of(hwnd), Edge::Bottom, [0; 4])).is_some()
}

/// The marker file after an `ABM_REMOVE` (`after_remove`).
fn settle_marker(after: AfterRemove, reason: &str) {
    match after {
        AfterRemove::Delete => remove_marker(),
        AfterRemove::KeepUndelivered => log!("appbar: ABM_REMOVE not delivered, the marker is kept ({reason})"),
    }
}

/// The 3 s tick (`ui::appbar_tick`): sends again this process' `ABM_REMOVE` that did not reach Explorer. Cheap while
/// none is pending.
pub fn retry_removal() {
    let pending = state().pending;
    if pending != 0 {
        remove_own(pending, "sent again");
    }
}

/// Frees the strip whoever registered it: this process' registration or its removal still pending, or the one a dead
/// dock left in the marker file (a watchdog or a new start). The marker goes only once the `ABM_REMOVE` reached
/// Explorer (`after_remove`). Idempotent, any thread: called from `taskbar::restore` / `restore_force`, `ui::shutdown`
/// and `ui::attach`.
pub fn release_all(reason: &str) {
    if state().hwnd != 0 {
        release_own(reason);
        return;
    }
    let pending = state().pending;
    if pending != 0 {
        remove_own(pending, reason);
        return;
    }
    if let Some(hwnd) = read_marker() {
        let h = hwnd_of(hwnd);
        // SAFETY: IsWindow has no preconditions; a handle that is no longer a window reads false.
        let exists = unsafe { IsWindow(Some(h)) }.as_bool();
        if marker_removes(exists, exists && crate::ui::window_class(h) == DOCK_WINDOW_CLASS) {
            let delivered = send_remove(hwnd);
            if delivered {
                log!("appbar: released a registration left by a dock that is gone ({reason})");
            }
            settle_marker(after_remove(delivered), reason);
            return;
        }
        log!("appbar: the marker names another app's window now, nothing removed ({reason})");
    }
    remove_marker();
}

/// Outcome of an `ABN_*` notification (pure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notify {
    /// The strip must be recomputed (`sync` with `force`).
    pub recompute: bool,
    /// New `ABN_FULLSCREENAPP` state, if the notification carries one.
    pub fullscreen: Option<bool>,
}

/// Decision for `ABN_*`: `since_set_ms` = time since our own last `ABM_SETPOS`, `prev_fullscreen` = the state we hold,
/// `at` = where the fullscreen window in front is. `ABN_POSCHANGED` right after our own `ABM_SETPOS` is its echo and is
/// ignored; `ABN_FULLSCREENAPP` recomputes only when the state of the bar's display changes
/// (`fullscreen_after_notify`; on: the strip is parked at 0 px, off: it is reserved again if the bar is visible).
pub fn notify_decision(wp: usize, lp: isize, since_set_ms: u64, prev_fullscreen: bool, at: FsWhere) -> Notify {
    match wp {
        ABN_POSCHANGED => Notify { recompute: since_set_ms >= ECHO_MS, fullscreen: None },
        ABN_FULLSCREENAPP => {
            let on = fullscreen_after_notify(lp != 0, at, prev_fullscreen);
            Notify { recompute: on != prev_fullscreen, fullscreen: Some(on) }
        }
        _ => Notify { recompute: false, fullscreen: None },
    }
}

/// `WM_DOCK_APPBAR`: `wp` = `ABN_*`, `lp` = its value, `at` = where the fullscreen window in front is. true = the
/// strip must be recomputed (`sync` with `force`).
pub fn on_notify(wp: usize, lp: isize, at: FsWhere) -> bool {
    let applied = apply_notify(&mut state(), wp, lp, now_ms(), at);
    let Some(n) = applied else {
        log!("appbar: ABN {wp} ignored, nothing is registered");
        return false;
    };
    if let Some(on) = n.fullscreen {
        let what = if lp != 0 { "on" } else { "off" };
        let effect = match (n.recompute, on) {
            (true, true) => "strip parked",
            (true, false) => "strip reserved again",
            (false, _) => "strip unchanged",
        };
        log!("appbar: ABN_FULLSCREENAPP {what} ({}): {effect}", at.name());
    }
    if wp == ABN_POSCHANGED {
        if n.recompute {
            log!("appbar: ABN_POSCHANGED, recomputing the strip");
        } else {
            log!("appbar: ABN_POSCHANGED ignored (echo of our own ABM_SETPOS)");
        }
    }
    n.recompute
}

/// `WM_ACTIVATE` of the registered window: `ABM_ACTIVATE`.
pub fn on_activate(h: HWND) {
    let registered = state().hwnd == h.0 as isize;
    if registered {
        call(ABM_ACTIVATE, &mut data(h, Edge::Bottom, [0; 4]));
    }
}

/// `WM_WINDOWPOSCHANGED` of the registered window: `ABM_WINDOWPOSCHANGED` for a real move or resize
/// (`position_changed`), at most every `WPC_MIN_MS`. `lp` = the message's lParam, its `WINDOWPOS`.
pub fn on_window_pos_changed(h: HWND, lp: LPARAM) {
    if lp.0 == 0 {
        return;
    }
    // SAFETY: for WM_WINDOWPOSCHANGED lParam points to the WINDOWPOS of the change, valid while the message is handled.
    if !position_changed(unsafe { (*(lp.0 as *const WINDOWPOS)).flags.0 }) {
        return;
    }
    let now = now_ms();
    {
        let mut st = state();
        if st.hwnd != h.0 as isize || now.saturating_sub(st.last_wpc) < WPC_MIN_MS {
            return;
        }
        st.last_wpc = now;
    }
    call(ABM_WINDOWPOSCHANGED, &mut data(h, Edge::Bottom, [0; 4]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_position_messages_carry_a_block() {
        for msg in [ABM_QUERYPOS, ABM_SETPOS] {
            assert!(needs_block(msg), "{msg}");
        }
        for msg in [ABM_NEW, ABM_REMOVE, ABM_ACTIVATE, ABM_WINDOWPOSCHANGED] {
            assert!(!needs_block(msg), "{msg}");
        }
    }

    const PRIMARY: Rc = [0, 0, 2560, 1440];
    const MIDDLE: Rc = [2560, 0, 3840, 1080];
    const SECOND: Rc = [3840, 0, 6400, 1440];

    fn req(mon: Rc, edge: Edge, thickness: i32) -> Request {
        Request { mon, edge, thickness }
    }

    #[test]
    fn thickness_is_bar_plus_both_margins() {
        assert_eq!(strip_thickness(48, 8), 64);
        assert_eq!(strip_thickness(48, 0), 48);
        assert_eq!(strip_thickness(-5, -3), 0);
    }

    #[test]
    fn strip_sits_on_the_edge_of_the_chosen_display() {
        assert_eq!(strip_rect(&req(PRIMARY, Edge::Bottom, 64)), [0, 1376, 2560, 1440]);
        assert_eq!(strip_rect(&req(PRIMARY, Edge::Top, 64)), [0, 0, 2560, 64]);
        // Displays that do not start at 0,0: the strip follows their own rectangle.
        assert_eq!(strip_rect(&req(MIDDLE, Edge::Bottom, 64)), [2560, 1016, 3840, 1080]);
        assert_eq!(strip_rect(&req(SECOND, Edge::Top, 70)), [3840, 0, 6400, 70]);
    }

    #[test]
    fn strip_never_exceeds_the_display() {
        assert_eq!(strip_rect(&req(PRIMARY, Edge::Bottom, 5000)), [0, 0, 2560, 1440]);
        assert_eq!(strip_rect(&req(PRIMARY, Edge::Top, -4)), [0, 0, 2560, 0]);
    }

    #[test]
    fn query_result_keeps_the_thickness_on_the_free_side() {
        // Another bar already holds the bottom 40 px: the query moved our strip up.
        assert_eq!(fit_after_query([0, 1376, 2560, 1400], Edge::Bottom, 64), [0, 1336, 2560, 1400]);
        // Top: the bottom side is recomputed from the top.
        assert_eq!(fit_after_query([0, 40, 2560, 64], Edge::Top, 64), [0, 40, 2560, 104]);
    }

    #[test]
    fn usable_needs_an_area() {
        assert!(is_usable([0, 0, 10, 10]));
        assert!(!is_usable([0, 0, 0, 10]));
        assert!(!is_usable([0, 10, 10, 10]));
    }

    #[test]
    fn reserve_only_when_option_visible_and_nothing_else_blocks() {
        assert!(should_reserve(true, true, false, false));
        assert!(!should_reserve(false, true, false, false), "option off");
        assert!(!should_reserve(true, false, false, false), "bar hidden");
        assert!(!should_reserve(true, true, true, false), "fullscreen app");
        assert!(!should_reserve(true, true, false, true), "exiting");
    }

    #[test]
    fn fullscreen_app_parks_instead_of_releasing() {
        assert!(park_for_fullscreen_app(true, true, false, true, false), "only ABN_FULLSCREENAPP blocks");
        assert!(!park_for_fullscreen_app(true, true, false, false, false), "no fullscreen app: reserve");
        assert!(!park_for_fullscreen_app(false, true, false, true, false), "option off: release");
        assert!(!park_for_fullscreen_app(true, false, false, true, false), "bar hidden: release");
        assert!(!park_for_fullscreen_app(true, true, true, true, false), "dock's own detection: release");
        assert!(!park_for_fullscreen_app(true, true, false, true, true), "exiting: release");
    }

    #[test]
    fn parked_strip_has_no_thickness_on_its_edge() {
        let bottom = strip_rect(&req(PRIMARY, Edge::Bottom, 0));
        assert_eq!(bottom, [0, 1440, 2560, 1440]);
        assert!(!is_usable(bottom));
        assert_eq!(fit_after_query([0, 1400, 2560, 1400], Edge::Bottom, 0), [0, 1400, 2560, 1400]);
        assert_eq!(strip_rect(&req(SECOND, Edge::Top, 0)), [3840, 0, 6400, 0]);
        // A 0 px strip is never given back to the dock's geometry: the parked state holds no strip.
        assert_eq!(unreserved([0, 0, 2560, 1440], PRIMARY, None), [0, 0, 2560, 1440]);
    }

    #[test]
    fn decision_table() {
        let r = req(PRIMARY, Edge::Bottom, 64);
        let other = req(SECOND, Edge::Bottom, 64);
        assert_eq!(decide(true, false, None, r, false), Step::Reserve);
        assert_eq!(decide(true, true, Some(r), r, false), Step::Nothing, "already granted, unchanged");
        assert_eq!(decide(true, true, Some(r), r, true), Step::Reserve, "forced");
        assert_eq!(decide(true, true, Some(r), other, false), Step::Reserve, "moved to another display");
        assert_eq!(decide(true, true, Some(r), req(PRIMARY, Edge::Top, 64), false), Step::Reserve, "other edge");
        assert_eq!(decide(true, true, Some(r), req(PRIMARY, Edge::Bottom, 70), false), Step::Reserve, "new size");
        assert_eq!(decide(true, true, None, r, false), Step::Reserve, "back from parked");
        assert_eq!(decide(false, true, Some(r), r, false), Step::Release);
        assert_eq!(decide(false, false, None, r, false), Step::Nothing);
        assert_eq!(decide(false, false, None, r, true), Step::Nothing);
    }

    #[test]
    fn outside_release_needs_a_marker_we_wrote() {
        assert!(released_from_outside(true, true, false), "watchdog removed the marker");
        assert!(!released_from_outside(true, true, true), "marker still there");
        assert!(!released_from_outside(true, false, false), "marker never written: no re-register loop");
        assert!(!released_from_outside(false, false, false), "not registered");
    }

    #[test]
    fn own_strip_is_given_back_to_the_dock_geometry() {
        let held = Some(Held { edge: Edge::Bottom, rect: [0, 1376, 2560, 1440] });
        // Work area cut exactly by the strip: the dock sees the display as if it were not reserved.
        assert_eq!(unreserved([0, 0, 2560, 1376], PRIMARY, held), [0, 0, 2560, 1440]);
        // Other display, or work area not cut by us: untouched.
        assert_eq!(unreserved([2560, 0, 3840, 1080], MIDDLE, held), [2560, 0, 3840, 1080]);
        assert_eq!(unreserved([0, 0, 2560, 1400], PRIMARY, held), [0, 0, 2560, 1400]);
        assert_eq!(unreserved([0, 0, 2560, 1376], PRIMARY, None), [0, 0, 2560, 1376]);
        let top = Some(Held { edge: Edge::Top, rect: [3840, 0, 6400, 64] });
        assert_eq!(unreserved([3840, 64, 6400, 1440], SECOND, top), [3840, 0, 6400, 1440]);
        // Strip above a visible taskbar: the work area ends where our strip starts, the taskbar's top is given back.
        let above = Some(Held { edge: Edge::Bottom, rect: [0, 1336, 2560, 1400] });
        assert_eq!(unreserved([0, 0, 2560, 1336], PRIMARY, above), [0, 0, 2560, 1400]);
    }

    #[test]
    fn strip_lies_on_a_display_by_its_inner_side() {
        let bottom = [0, 1376, 2560, 1440];
        assert!(strip_on_display(bottom, Edge::Bottom, PRIMARY));
        assert!(strip_on_display(bottom, Edge::Bottom, [0, 0, 1920, 1440]), "narrower display, same height");
        assert!(!strip_on_display(bottom, Edge::Bottom, [0, 0, 1920, 1080]), "display shorter than the strip's top");
        assert!(!strip_on_display(bottom, Edge::Bottom, MIDDLE), "display beside it");
        let top = [3840, 0, 6400, 64];
        assert!(strip_on_display(top, Edge::Top, SECOND));
        assert!(!strip_on_display(top, Edge::Top, [-2560, 0, 0, 1440]), "display moved to the left");
    }

    #[test]
    fn display_change_still_gives_back_the_old_strip() {
        // Primary went from 2560 to 1920 wide: the strip granted on the old rectangle still cuts the work area until
        // the next ABM_SETPOS, and the bar must not climb by it in the meantime.
        let held = Some(Held { edge: Edge::Bottom, rect: [0, 1376, 2560, 1440] });
        let narrower = [0, 0, 1920, 1440];
        assert_eq!(unreserved([0, 0, 1920, 1376], narrower, held), [0, 0, 1920, 1440]);
        // Display shrank below the strip: Windows already recomputed the work area, nothing to give back.
        let shorter = [0, 0, 1920, 1080];
        assert_eq!(unreserved([0, 0, 1920, 1080], shorter, held), [0, 0, 1920, 1080]);
        // Display moved away (rearranged): the old strip is elsewhere, the work area is left as it is.
        let top = Some(Held { edge: Edge::Top, rect: [3840, 0, 6400, 64] });
        let moved = [-2560, 0, 0, 1440];
        assert_eq!(unreserved([-2560, 64, 0, 1440], moved, top), [-2560, 64, 0, 1440]);
        // Top strip on a display that grew taller: given back, never past the display's top.
        let grown = [3840, 0, 6400, 2160];
        assert_eq!(unreserved([3840, 64, 6400, 2160], grown, top), [3840, 0, 6400, 2160]);
    }

    #[test]
    fn notify_poschanged_ignores_our_own_echo() {
        let u = FsWhere::Unknown;
        assert!(!notify_decision(ABN_POSCHANGED, 0, 50, false, u).recompute, "echo");
        assert!(notify_decision(ABN_POSCHANGED, 0, ECHO_MS, false, u).recompute, "another bar moved");
        assert_eq!(notify_decision(ABN_POSCHANGED, 0, 5000, false, u).fullscreen, None);
    }

    #[test]
    fn notify_fullscreen_recomputes_only_on_change() {
        let d = FsWhere::DockDisplay;
        let u = FsWhere::Unknown;
        let on = notify_decision(ABN_FULLSCREENAPP, 1, 0, false, d);
        assert_eq!(on, Notify { recompute: true, fullscreen: Some(true) });
        assert!(!notify_decision(ABN_FULLSCREENAPP, 1, 0, true, d).recompute, "already on");
        let off = notify_decision(ABN_FULLSCREENAPP, 0, 0, true, u);
        assert_eq!(off, Notify { recompute: true, fullscreen: Some(false) });
        assert!(!notify_decision(ABN_FULLSCREENAPP, 0, 0, false, u).recompute, "already off");
        // Fullscreen on parks, off reserves again (option on, bar visible).
        assert!(!should_reserve(true, true, true, false));
        assert!(should_reserve(true, true, false, false));
    }

    #[test]
    fn notify_other_codes_are_ignored() {
        let u = FsWhere::Unknown;
        assert_eq!(notify_decision(0, 0, 9999, false, u), Notify { recompute: false, fullscreen: None });
        assert_eq!(notify_decision(3, 1, 9999, true, u), Notify { recompute: false, fullscreen: None });
    }

    #[test]
    fn a_notification_while_nothing_is_registered_is_ignored() {
        let d = FsWhere::DockDisplay;
        // The dock's own fullscreen detection hid the bar and released the strip before Explorer's posted
        // ABN_FULLSCREENAPP(1) was dispatched, or the message was forged: taken, it would block every later
        // reservation, since Explorer sends the "off" to registered bars only.
        let mut st = UNREGISTERED;
        assert_eq!(apply_notify(&mut st, ABN_FULLSCREENAPP, 1, 5000, d), None);
        assert!(!st.fullscreen_app);
        assert!(should_reserve(true, true, st.fullscreen_app, false), "the next show reserves");
        assert_eq!(apply_notify(&mut st, ABN_POSCHANGED, 0, 5000, d), None);
        // Registered: the notification counts.
        st.hwnd = 0x3_0030;
        let on = apply_notify(&mut st, ABN_FULLSCREENAPP, 1, 5000, d);
        assert_eq!(on, Some(Notify { recompute: true, fullscreen: Some(true) }));
        assert!(st.fullscreen_app);
    }

    #[test]
    fn fullscreen_app_on_another_display_leaves_the_strip() {
        let other = FsWhere::OtherDisplay;
        // A game on the secondary display: the strip on the bar's display stays reserved.
        assert!(!fullscreen_after_notify(true, other, false));
        let start = notify_decision(ABN_FULLSCREENAPP, 1, 0, false, other);
        assert_eq!(start, Notify { recompute: false, fullscreen: Some(false) });
        // Its end changes nothing either.
        let end = notify_decision(ABN_FULLSCREENAPP, 0, 0, false, other);
        assert_eq!(end, Notify { recompute: false, fullscreen: Some(false) });
        // Already parked for the bar's display: an app starting elsewhere keeps it parked.
        assert!(fullscreen_after_notify(true, other, true));
        assert!(!notify_decision(ABN_FULLSCREENAPP, 1, 0, true, other).recompute);
    }

    #[test]
    fn fullscreen_app_on_the_bar_display_parks() {
        assert!(fullscreen_after_notify(true, FsWhere::DockDisplay, false));
        assert!(fullscreen_after_notify(true, FsWhere::Unknown, false), "display not known: believe it");
        assert!(!fullscreen_after_notify(false, FsWhere::Unknown, true), "app gone");
        assert!(!fullscreen_after_notify(false, FsWhere::OtherDisplay, true), "the foreground went elsewhere");
        // An app elsewhere ended while ours still covers the bar's display: stays parked, nothing recomputed.
        assert!(fullscreen_after_notify(false, FsWhere::DockDisplay, true));
        assert!(!notify_decision(ABN_FULLSCREENAPP, 0, 0, true, FsWhere::DockDisplay).recompute);
    }

    #[test]
    fn strip_on_every_display_and_edge() {
        // (display, edge, expected strip) for three displays side by side, of two sizes, 64 px strip.
        let cases = [
            (PRIMARY, Edge::Top, [0, 0, 2560, 64]),
            (PRIMARY, Edge::Bottom, [0, 1376, 2560, 1440]),
            (MIDDLE, Edge::Top, [2560, 0, 3840, 64]),
            (MIDDLE, Edge::Bottom, [2560, 1016, 3840, 1080]),
            (SECOND, Edge::Top, [3840, 0, 6400, 64]),
            (SECOND, Edge::Bottom, [3840, 1376, 6400, 1440]),
        ];
        for (mon, edge, want) in cases {
            let rc = strip_rect(&req(mon, edge, 64));
            assert_eq!(rc, want, "{mon:?} {edge:?}");
            assert!(is_usable(rc));
            assert!(strip_on_display(rc, edge, mon), "the strip lies on its own display");
            // Windows leaves it as asked when no other bar is in the way.
            assert_eq!(fit_after_query(rc, edge, 64), rc);
            // It never lies on a neighbouring display.
            for other in [PRIMARY, MIDDLE, SECOND] {
                if other != mon {
                    assert!(!strip_on_display(rc, edge, other), "{mon:?} {edge:?} on {other:?}");
                }
            }
        }
    }

    #[test]
    fn reserve_and_fullscreen_truth_tables() {
        for bits in 0..32u8 {
            let (option, visible, fs_dock, fs_app, exiting) =
                (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0, bits & 16 != 0);
            let reserve = should_reserve(option, visible, fs_dock, exiting);
            assert_eq!(reserve, option && visible && !fs_dock && !exiting, "bits {bits:#07b}");
            // Parked only when the fullscreen notification alone stands in the way.
            assert_eq!(park_for_fullscreen_app(option, visible, fs_dock, fs_app, exiting), fs_app && reserve);
            // A strip that is wanted is reserved; one that is not wanted is released only if registered.
            let r = req(PRIMARY, Edge::Bottom, 64);
            assert_eq!(decide(reserve, false, None, r, false) == Step::Reserve, reserve);
            assert_eq!(decide(reserve, true, None, r, false) == Step::Release, !reserve);
            assert_eq!(decide(reserve, false, None, r, true) == Step::Nothing, !reserve);
        }
        // `ABN_FULLSCREENAPP` on / off, by where the fullscreen window is, from either previous state.
        for prev in [false, true] {
            assert!(fullscreen_after_notify(true, FsWhere::DockDisplay, prev));
            assert!(fullscreen_after_notify(true, FsWhere::Unknown, prev));
            assert_eq!(fullscreen_after_notify(true, FsWhere::OtherDisplay, prev), prev);
            assert!(fullscreen_after_notify(false, FsWhere::DockDisplay, prev));
            assert!(!fullscreen_after_notify(false, FsWhere::Unknown, prev));
            assert!(!fullscreen_after_notify(false, FsWhere::OtherDisplay, prev));
        }
    }

    #[test]
    fn marker_roundtrip() {
        assert_eq!(parse_marker(&marker_text(197_450)), Some(197_450));
        assert_eq!(parse_marker(" 42\r\n"), Some(42));
        assert_eq!(parse_marker(""), None);
        assert_eq!(parse_marker("0"), None);
        assert_eq!(parse_marker("abc"), None);
    }

    #[test]
    fn the_strip_is_wanted_only_while_auto_hide_is_off() {
        // With auto-hide the bar shows and hides all day, and each show would move every maximized window of the
        // display twice; the Windows taskbar reserves nothing while it auto-hides either.
        assert!(wanted(true, false), "option on, auto-hide off");
        assert!(!wanted(true, true), "auto-hide on: nothing is reserved");
        assert!(!wanted(false, false), "option off");
        assert!(!wanted(false, true));
    }

    #[test]
    fn a_registration_is_lost_to_the_watchdog_or_to_a_new_explorer() {
        const OLD: isize = 0x1_0010;
        const NEW: isize = 0x2_0020;
        // (registered, marker written, marker present, Explorer it was registered with, Explorer now, lost)
        let cases = [
            (true, true, true, OLD, Some(OLD), None),
            (true, true, false, OLD, Some(OLD), Some(Lost::Outside)),
            (true, false, false, OLD, Some(OLD), None),
            (true, true, false, OLD, Some(NEW), Some(Lost::Outside)),
            (true, true, true, OLD, Some(NEW), Some(Lost::NewExplorer)),
            (true, true, true, OLD, None, None),
            (true, true, true, 0, Some(NEW), None),
            (false, false, false, 0, Some(NEW), None),
        ];
        for (registered, written, present, with, now, want) in cases {
            let got = lost(registered, written, present, with, now);
            assert_eq!(got, want, "{registered} {written} {present} {with:#x} {now:?}");
        }
    }

    #[test]
    fn a_stale_marker_removes_only_a_dead_or_a_dock_window() {
        assert!(marker_removes(false, false), "the window is gone: Explorer still holds its strip");
        assert!(marker_removes(true, true), "the window of a hung dock");
        assert!(!marker_removes(true, false), "the handle was reused by another app's window");
    }

    /// Explorer as documented: `ABM_NEW` is refused for a window it already holds, `ABM_REMOVE` is always taken.
    struct Explorer {
        holds: bool,
        delivers: bool,
        sent: Vec<u32>,
    }

    impl Explorer {
        fn new(holds: bool, delivers: bool) -> Explorer {
            Explorer { holds, delivers, sent: Vec::new() }
        }

        fn send(&mut self, msg: u32) -> Option<usize> {
            self.sent.push(msg);
            if !self.delivers {
                return None;
            }
            match msg {
                ABM_NEW => {
                    let taken = !self.holds;
                    self.holds = true;
                    Some(usize::from(taken))
                }
                ABM_REMOVE => {
                    self.holds = false;
                    Some(1)
                }
                _ => Some(1),
            }
        }
    }

    #[test]
    fn a_refused_abm_new_is_sent_again_after_a_remove() {
        // Explorer still holds the window (an ABM_REMOVE that never arrived, a marker deleted by another dock): the
        // record is removed and the window registered again, instead of the strip staying held with nothing to free it.
        let mut ex = Explorer::new(true, true);
        assert_eq!(register_with(|m| ex.send(m)), Registered::AfterRemove);
        assert_eq!(ex.sent, [ABM_NEW, ABM_REMOVE, ABM_NEW]);
        assert!(ex.holds);
        // Nothing held: one ABM_NEW.
        let mut ex = Explorer::new(false, true);
        assert_eq!(register_with(|m| ex.send(m)), Registered::New);
        assert_eq!(ex.sent, [ABM_NEW]);
        // Not delivered: nothing more is sent, nothing is registered.
        let mut ex = Explorer::new(true, false);
        assert_eq!(register_with(|m| ex.send(m)), Registered::No);
        assert_eq!(ex.sent, [ABM_NEW]);
        // Refused again after the remove: one more try only.
        let mut sent = Vec::new();
        let refuses = |m: u32| {
            sent.push(m);
            Some(0)
        };
        assert_eq!(register_with(refuses), Registered::No);
        assert_eq!(sent, [ABM_NEW, ABM_REMOVE, ABM_NEW]);
    }

    #[test]
    fn a_remove_that_did_not_arrive_keeps_the_marker_and_is_sent_again() {
        const H: isize = 0x3_0030;
        // As `taskbar::after_restore`: only a delivered ABM_REMOVE lets the marker go.
        assert_eq!(after_remove(true), AfterRemove::Delete);
        assert_eq!(after_remove(false), AfterRemove::KeepUndelivered);
        let mut st = UNREGISTERED;
        on_registered(&mut st, H, true, 0x1_0010);
        forget(&mut st);
        // Explorer busy past the send's bound or flagged as hung: the removal stays pending for the 3 s tick.
        assert_eq!(removal_sent(&mut st, H, false), AfterRemove::KeepUndelivered);
        assert_eq!(st.pending, H);
        // Delivered on a later try: nothing is pending, the marker goes.
        assert_eq!(removal_sent(&mut st, H, true), AfterRemove::Delete);
        assert_eq!(st.pending, 0);
        // Registered again first: the old removal is dropped, or the tick would end the new registration.
        assert_eq!(removal_sent(&mut st, H, false), AfterRemove::KeepUndelivered);
        on_registered(&mut st, H, true, 0x1_0010);
        assert_eq!((st.hwnd, st.pending), (H, 0));
        // The tick sends a pending removal again.
        let ui = include_str!("ui.rs");
        let tick = ui.find("fn appbar_tick() {").expect("the tick");
        let end = ui[tick..].find("fn appbar_resync(").expect("the next function") + tick;
        assert!(ui[tick..end].contains("appbar::retry_removal();"), "{}", &ui[tick..end]);
    }

    #[test]
    fn only_a_move_or_a_resize_is_a_position_change() {
        use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE};
        let (nomove, nosize, noactivate) = (SWP_NOMOVE.0, SWP_NOSIZE.0, SWP_NOACTIVATE.0);
        assert!(!position_changed(nomove | nosize | noactivate), "the topmost re-assertion every 500 ms");
        assert!(position_changed(nosize | noactivate), "the slide moves the bar");
        assert!(position_changed(nomove), "a resize");
        assert!(position_changed(0));
    }

    #[test]
    fn a_watchdog_give_up_is_not_an_exit() {
        assert!(dock_exiting(true, false), "the exit's restore");
        assert!(!dock_exiting(true, true), "the give-up gives the taskbar back and the dock runs on");
        assert!(!dock_exiting(false, false));
        assert!(!dock_exiting(false, true));
    }

    #[test]
    fn the_marker_window_class_is_the_dock_window_class() {
        assert!(include_str!("main.rs").contains(&format!("w!(\"{DOCK_WINDOW_CLASS}\")")));
    }

    /// The 3 s safety tick looks for a registration Explorer no longer holds (`take_lost`), and the watchdog's
    /// give-up, whose taskbar restore frees the strip, puts the strip back while the dock runs on.
    #[test]
    fn a_lost_or_freed_strip_is_put_back() {
        let main = include_str!("main.rs");
        let tick = main.find("TIMER_SECOND => {").expect("the safety tick");
        let end = main[tick..].find("LRESULT(0)").expect("the arm ends") + tick;
        assert!(main[tick..end].contains("ui::appbar_tick();"), "{}", &main[tick..end]);
        let wd = include_str!("watchdog.rs");
        let give_up = wd.find("AfterExit::GiveUp => {").expect("the give-up");
        let end = wd[give_up..].find("enum GiveUpRestore").expect("the arm ends") + give_up;
        let arm = &wd[give_up..end];
        let restore = arm.find("crate::taskbar::restore();").expect("the taskbar is given back");
        let back = arm.find("crate::ui::appbar_resync(").expect("the strip is put back");
        assert!(arm.find("GAVE_UP.store(true").expect("the give-up is marked first") < restore && restore < back);
        assert!(include_str!("ui.rs").contains("if appbar::take_lost().is_some() {"));
    }
}
