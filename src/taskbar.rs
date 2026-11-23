//! Native Windows taskbar: hide / restore / self-heal / on-disk state.
//! The watchdog is in `watchdog.rs`.
//!
//! THREADING:
//!  * `init`, `install_hook`, `remove_hook`, `on_winevent`, `on_safety_timer`, `on_taskbar_created`,
//!    `shutdown`: UI thread ONLY (the taskbar has no WinEvent hook of its own: `apps::winevent_proc` runs on the
//!    UI thread and hands the raw events to `on_raw_event`, which posts `WM_DOCK_TASKBAR_EVENT`).
//!  * `restore()` and `restore_force()`: callable from ANY thread/process (panic hook,
//!    watchdog, CLI `--restore`; Ctrl+C instead posts WM_CLOSE to the UI). They use only
//!    thread-safe Win32 APIs and do not touch thread-local state. `restore_at_session_end` (WM_ENDSESSION): UI thread.
//!  * On-disk state: `%ProgramData%\mini-oled-dock\taskbar-state` (text: the numeric `ABS_*` value),
//!    admin-only folder, atomic write (`datadir::write_atomic`), read without following reparse
//!    points. Without a usable data folder the state cannot be saved and the taskbar is never hidden.
//!  * The dock does NOT register an AppBar and does NOT reserve a work area.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_TIMEOUT, HANDLE, HWND, LPARAM, RECT, WIN32_ERROR, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, FindWindowExW, GetClassNameW, GetParent, GetPropW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, KillTimer, PostMessageW, RegisterWindowMessageW, RemovePropW,
    SendMessageTimeoutW, SendNotifyMessageW, SetPropW, SetTimer, SetWindowPos, ShowWindowAsync, EVENT_OBJECT_CREATE,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW, HWND_BOTTOM, HWND_BROADCAST, HWND_NOTOPMOST, HWND_TOPMOST, MSGFLT_ALLOW,
    OBJID_WINDOW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WM_COPYDATA,
};

use crate::config;
use crate::log;
use crate::msg::{TIMER_TRAY_REANNOUNCE, WM_DOCK_TASKBAR_EVENT};

/// Name of the window property that marks the taskbars hidden by us.
pub const PROP_DOCK_HIDDEN: windows::core::PCWSTR = w!("DockHidden");

/// true if this process hid the taskbar (and therefore must restore it).
static HIDDEN: AtomicBool = AtomicBool::new(false);
/// Set at the start of `restore`/`restore_force` and NEVER cleared: a process that has started
/// restoring must not hide the taskbar again (panic hook / safety timer race).
static RESTORING: AtomicBool = AtomicBool::new(false);
/// Set by `restore_at_session_end` and never cleared: `restore_force` keeps the state file for the next sign-in.
/// Only the dock sets it, so a watchdog restore after a crash still deletes the file.
static SESSION_END: AtomicBool = AtomicBool::new(false);
/// Set by `restored_externally` when the restore from outside was the watchdog's hang restore
/// (`after_outside_restore`): the next safety check hides the taskbar again (`hide_again`). UI thread.
static HIDE_AGAIN: AtomicBool = AtomicBool::new(false);
/// Set by `hide_again` when Explorer did not answer its read, cleared by `restored_externally` at each watchdog's
/// restore: the "state unreadable" line is logged once per restore, not at every retry (`after_rehide_read`).
/// UI thread.
static HIDE_AGAIN_UNREAD: AtomicBool = AtomicBool::new(false);
/// Id of the registered "TaskbarCreated" message (0 = not registered yet).
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);
/// true while the taskbar wants the raw WinEvents that `apps::winevent_proc` hands to `on_raw_event` (set by
/// `install_hook`, cleared by `remove_hook`): the taskbar has no WinEvent hook of its own.
static WANT_EVENTS: AtomicBool = AtomicBool::new(false);
/// Instant (ms since process start, +1) of the last `TaskbarCreated` broadcast sent by us.
static LAST_BROADCAST_MS: AtomicU64 = AtomicU64::new(0);
/// Explorer's primary `Shell_TrayWnd` as last seen by `hide_all` (0 = unknown): its destruction means
/// Explorer is going away (`explorer_gone`).
static EXPLORER_TRAY: AtomicIsize = AtomicIsize::new(0);
/// `now_ms()` when `explorer_gone` stopped our tray host (0 = not paused): it is started again once the
/// new Explorer has registered its desktop (`on_reannounce_timer`).
static HOST_PAUSED_AT: AtomicU64 = AtomicU64::new(0);
/// The window that held the taskbar's edge at the last hold-back (0 = no hold-back in effect): its DESTROY, or the
/// safety timer once it is no longer a window (`holder_left`), runs `hide_all` again. Cleared when the state is
/// sent again, when auto-hide reads on and on an Explorer restart, so a lasting conflict is logged once, not every
/// second.
static EDGE_HOLDER: AtomicIsize = AtomicIsize::new(0);
/// Taskbar state (`ABS_*`) of the last `ABM_GETSTATE` read by `hide_all`, or `STATE_UNREAD` / `STATE_UNKNOWN`:
/// a change is logged once, so the log tells whether Explorer turned auto-hide off by itself or kept it off.
static LAST_STATE: AtomicU32 = AtomicU32::new(STATE_UNREAD);
/// Times this process's safety check found a visible taskbar (`on_safety_timer`, `safety_logs`).
static SAFETY_FINDS: AtomicU32 = AtomicU32::new(0);
/// Monotonic time reference of the process.
static START: OnceLock<Instant> = OnceLock::new();

// SHAppBarMessage constants (documented values; they avoid depending on the type of the crate's constants).
const ABM_GETSTATE: u32 = 4;
const ABM_SETSTATE: u32 = 10;
const ABM_GETAUTOHIDEBAREX: u32 = 11;
const ABE_LEFT: u32 = 0;
const ABE_TOP: u32 = 1;
const ABE_RIGHT: u32 = 2;
const ABE_BOTTOM: u32 = 3;
const ABS_AUTOHIDE: u32 = 1;
const ABS_ALWAYSONTOP: u32 = 2;
const ABS_MASK: u32 = ABS_AUTOHIDE | ABS_ALWAYSONTOP;
/// `LAST_STATE` before the first read.
const STATE_UNREAD: u32 = u32::MAX;
/// `LAST_STATE` after a read that failed.
const STATE_UNKNOWN: u32 = u32::MAX - 1;

/// Timeout of the AppBar messages sent straight to Explorer (`appbar_direct`), in ms.
const APPBAR_TIMEOUT_MS: u32 = 2000;
/// Timeout of the advisory `ABM_GETAUTOHIDEBAREX` query, in ms: it blocks the UI thread before the auto-hide
/// request, and without an answer the request is sent anyway, so a busy Explorer costs at most this.
const EDGE_QUERY_TIMEOUT_MS: u32 = 500;
/// Quiet window during which our own `TaskbarCreated` broadcast is ignored.
const OWN_BROADCAST_WINDOW_MS: u64 = 2000;
/// Longest wait for a restarted Explorer's desktop before our tray host is started again anyway (ms).
const HOST_RESUME_MAX_MS: u64 = 60_000;
/// Maximum wait for `Shell_TrayWnd` at startup.
const WAIT_TRAY_MS: u64 = 30_000;

/// `<data root>\taskbar-state` (`None` without a usable data folder).
pub fn state_file_path() -> Option<PathBuf> {
    crate::datadir::state_path()
}

/// true if the state file exists (the name itself: a link is not followed).
pub fn state_file_exists() -> bool {
    state_file_path().is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

/// Id of the `TaskbarCreated` message (`RegisterWindowMessageW`, registered by `init`).
/// The `main` wnd_proc compares it with `msg` (if != 0) to call `on_taskbar_created()`.
pub fn taskbar_created_msg() -> u32 {
    TASKBAR_CREATED_MSG.load(Relaxed)
}

// ------------------------------------------------------------------ utilities

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Id of `TaskbarCreated`, registering it if needed (works in any process).
fn ensure_taskbar_created_msg() -> u32 {
    let cur = TASKBAR_CREATED_MSG.load(Relaxed);
    if cur != 0 {
        return cur;
    }
    // SAFETY: static NUL-terminated literal string.
    let id = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if id == 0 {
        log!("RegisterWindowMessageW(TaskbarCreated) failed: {:?}", windows::core::Error::from_thread());
    }
    TASKBAR_CREATED_MSG.store(id, Relaxed);
    id
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: `buf` is a writable buffer; the function truncates if too short.
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

fn is_taskbar_class(name: &str) -> bool {
    name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd"
}

/// true if the window belongs to this process (never touch our own windows).
fn is_ours(h: HWND) -> bool {
    let mut pid = 0u32;
    // SAFETY: `pid` is valid and writable; `h` may also no longer be valid (the call fails).
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    // SAFETY: no preconditions.
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

/// All the native taskbars (main + secondary) that are not ours.
fn native_taskbars() -> Vec<HWND> {
    let mut out = Vec::new();
    for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
        let mut prev: Option<HWND> = None;
        loop {
            // SAFETY: `class` is a static literal; `prev` is None or a window already returned
            // by the same enumeration (if it disappears in the meantime the search ends with an error).
            let Ok(h) = (unsafe { FindWindowExW(None, prev, class, PCWSTR::null()) }) else { break };
            if h.0.is_null() {
                break;
            }
            if !is_dock_tray_host(h) {
                out.push(h);
            }
            prev = Some(h);
        }
    }
    out
}

fn is_visible(h: HWND) -> bool {
    // SAFETY: no preconditions; for an invalid window it returns false.
    unsafe { IsWindowVisible(h) }.as_bool()
}

fn has_dock_prop(h: HWND) -> bool {
    // SAFETY: static literal property name; invalid handle = property absent.
    !unsafe { GetPropW(h, PROP_DOCK_HIDDEN) }.is_invalid()
}

/// `APPBARDATA` with 32-bit handles, as it travels inside `WM_COPYDATA`.
#[repr(C)]
#[derive(Clone, Copy)]
struct AppBarData32 {
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
struct AppBarMsg {
    abd: AppBarData32,
    message: u32,
    _pad0: u32,
    shared: u64,
    pid: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<AppBarData32>() == 0x28 && std::mem::size_of::<AppBarMsg>() == 0x40);

/// DIRECT send of an AppBar message without a shared block to Explorer's `Shell_TrayWnd`
/// (`hwnd`), so it does not depend on which `Shell_TrayWnd` is first in the Z-order (our tray
/// host is first). `Ok(result)` if the message was delivered, otherwise the send's error (`ERROR_TIMEOUT`: Explorer is
/// slow or hung).
fn appbar_direct(msg: u32, hwnd: HWND, lparam: u32) -> Result<usize, WIN32_ERROR> {
    appbar_direct_at(msg, hwnd, 0, [0; 4], lparam, APPBAR_TIMEOUT_MS)
}

/// `appbar_direct` with the APPBARDATA `uEdge` and `rc` filled in (`ABM_GETAUTOHIDEBAREX` needs both) and
/// a timeout of its own (ms). SMTO_BLOCK makes the timeout a true bound on the UI thread: without it the thread
/// dispatches the messages other processes send the dock while Explorer is slow, each one starts the timeout again
/// (live check V-14), and the heartbeat starves. Explorer has nothing to send the dock while it answers (the dock
/// registers no AppBar, so it gets no ABN_ notification); were it to, the send would end at its timeout and the
/// fallback would run, as for a slow Explorer. Nor does the wait serve the popup's low-level mouse hook, which Windows
/// calls through a message to the UI thread: the hook is removed for the wait (`popup::suspend_mouse_hook`, UI thread
/// only), or every mouse event would wait for Explorer and Windows would drop the hook.
fn appbar_direct_at(
    msg: u32,
    hwnd: HWND,
    edge: u32,
    rc: [i32; 4],
    lparam: u32,
    timeout_ms: u32,
) -> Result<usize, WIN32_ERROR> {
    let mut m = AppBarMsg {
        abd: AppBarData32 { cb_size: 0x28, hwnd: hwnd.0 as usize as u32, callback: 0, edge, rc, lparam: lparam as i64 },
        message: msg,
        _pad0: 0,
        shared: 0,
        pid: 0,
        _pad1: 0,
    };
    let cds = COPYDATASTRUCT {
        dwData: 0,
        cbData: std::mem::size_of::<AppBarMsg>() as u32,
        lpData: (&mut m as *mut AppBarMsg).cast::<c_void>(),
    };
    let mut res = 0usize;
    let hook = crate::popup::suspend_mouse_hook();
    // SAFETY: `cds` and `m` live for the whole call, which copies the data to Explorer; `res` is
    // writable; SMTO_ABORTIFHUNG avoids waits on a hung Explorer, SMTO_BLOCK keeps the timeout whole.
    let r = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(hwnd.0 as usize),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            timeout_ms,
            Some(&mut res),
        )
    };
    // SAFETY: no preconditions; read right after the send, before the hook's reinstall can set it.
    let err = unsafe { GetLastError() };
    crate::popup::resume_mouse_hook(hook);
    (r.0 != 0).then_some(res).ok_or(err)
}

/// AppBar message to Explorer: first a direct send, then `SHAppBarMessage` (which goes through the tray
/// host, which forwards it) where `takes_fallback` allows it, else 0. Returns the raw value.
fn appbar(msg: u32, hwnd: HWND, lparam: u32) -> usize {
    appbar_direct(msg, hwnd, lparam).unwrap_or_else(|e| appbar_fallback(msg, hwnd, lparam, e).unwrap_or(0))
}

/// Whether a direct AppBar send that failed with `err` falls back to `SHAppBarMessage`. Not on the UI thread after a
/// timeout: the fallback goes to the first `Shell_TrayWnd`, while the taskbar is hidden normally the dock's own tray
/// host, which forwards ABM_GETSTATE and ABM_SETSTATE to the same slow Explorer (`tray_host::forward_sync`, 3 s, no
/// SMTO_BLOCK, so each icon apps send the host meanwhile starts its wait again, as in live check V-14), and the UI
/// thread waits through it. Explorer still gets a timed-out send once it pumps again; only the answer is lost. A
/// refused send (a window gone) and the other threads (watchdog, `--restore`) keep the fallback. Pure.
fn takes_fallback(err: WIN32_ERROR, ui_thread: bool) -> bool {
    err != ERROR_TIMEOUT || !ui_thread
}

/// `SHAppBarMessage` after a direct send that failed with `err`, where `takes_fallback` allows it: its raw value, or
/// `None` when skipped.
fn appbar_fallback(msg: u32, hwnd: HWND, lparam: u32, err: WIN32_ERROR) -> Option<usize> {
    if !takes_fallback(err, crate::on_ui_thread()) {
        log!("taskbar: direct AppBar {msg} send timed out, no SHAppBarMessage fallback on the UI thread");
        return None;
    }
    log!("taskbar: direct AppBar {msg} send failed, falling back to SHAppBarMessage");
    Some(appbar_shell(msg, hwnd, lparam))
}

/// AppBar message through `SHAppBarMessage` (tray host -> Explorer): the fallback of a direct send that failed
/// (`appbar_fallback`). Returns the raw value.
fn appbar_shell(msg: u32, hwnd: HWND, lparam: u32) -> usize {
    let mut d = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        lParam: LPARAM(lparam as isize),
        ..Default::default()
    };
    // SAFETY: `d` is initialized with the correct `cbSize` and stays valid for the whole call.
    unsafe { SHAppBarMessage(msg, &mut d) }
}

fn get_state(tray: HWND) -> u32 {
    try_get_state(tray).0.unwrap_or(0)
}

/// `ABM_GETSTATE` like `get_state` (direct send, then `SHAppBarMessage`), but `None` when the read failed: the
/// direct send was not delivered and the fallback answered 0, which is also what a failed forward returns, or was
/// skipped (`takes_fallback`). The flag is true when the direct send answered: false means Explorer is slow right now.
fn try_get_state(tray: HWND) -> (Option<u32>, bool) {
    match appbar_direct(ABM_GETSTATE, tray, 0) {
        Ok(v) => (Some(v as u32 & ABS_MASK), true),
        Err(e) => {
            let v = appbar_fallback(ABM_GETSTATE, tray, 0, e).unwrap_or(0);
            ((v != 0).then_some(v as u32 & ABS_MASK), false)
        }
    }
}

/// The log line for a new state read by `hide_all` (`None` = the read failed) after `prev` (a `LAST_STATE`
/// value), `None` when nothing changed. Pure.
fn state_change_line(prev: u32, read: Option<u32>) -> Option<String> {
    if read.unwrap_or(STATE_UNKNOWN) == prev {
        return None;
    }
    Some(match (prev, read) {
        (_, None) => "taskbar: auto-hide state unknown (read failed)".to_string(),
        (STATE_UNREAD, Some(s)) => format!("taskbar: auto-hide state {s} (first read)"),
        (STATE_UNKNOWN, Some(s)) => format!("taskbar: auto-hide state unknown -> {s}"),
        (p, Some(s)) => format!("taskbar: auto-hide state {p} -> {s}"),
    })
}

/// How the hold-back and send lines of `hide_all` name a state read with auto-hide off (`None` = the read
/// failed, then counted as off). Pure.
fn autohide_off_text(read: Option<u32>) -> String {
    match read {
        Some(s) => format!("auto-hide off (state {s})"),
        None => "auto-hide state unknown (read failed)".to_string(),
    }
}

/// What the send line of `hide_all` adds about the edge check (`asked` from `autohide_edge_owner`): why the
/// edge was not asked, or that the question got no answer. Nothing only when Explorer answered. Pure.
fn edge_check_text(owner: EdgeOwner, asked: &Result<(u32, RECT), String>) -> String {
    match asked {
        Err(why) => format!(" (edge not asked: {why})"),
        Ok(_) if owner == EdgeOwner::NoAnswer => " (edge check unanswered)".to_string(),
        Ok(_) => String::new(),
    }
}

/// A rectangle as `(left, top)-(right, bottom)` for the log. Pure.
fn rect_text(r: RECT) -> String {
    format!("({}, {})-({}, {})", r.left, r.top, r.right, r.bottom)
}

/// Name of a screen edge (`ABE_*`) for the log. Pure.
fn edge_name(edge: u32) -> &'static str {
    match edge {
        ABE_LEFT => "left",
        ABE_TOP => "top",
        ABE_RIGHT => "right",
        ABE_BOTTOM => "bottom",
        _ => "unknown",
    }
}

/// Screen edge (`ABE_*`) a docked taskbar rectangle sits on within its monitor rectangle. Pure. A wide bar
/// is top or bottom, a tall one left or right, decided by which half of the monitor its centre is in. Only
/// for docked bars (`docked_inside`): an auto-hidden or moving bar can lie partly on another monitor.
fn edge_of(bar: RECT, mon: RECT) -> u32 {
    if bar.right - bar.left >= bar.bottom - bar.top {
        if bar.top + bar.bottom < mon.top + mon.bottom {
            ABE_TOP
        } else {
            ABE_BOTTOM
        }
    } else if bar.left + bar.right < mon.left + mon.right {
        ABE_LEFT
    } else {
        ABE_RIGHT
    }
}

/// true if `bar` is non-empty and lies fully inside `mon`: a docked bar, whose rectangle and monitor tell
/// its edge. Pure.
fn docked_inside(bar: RECT, mon: RECT) -> bool {
    bar.left < bar.right
        && bar.top < bar.bottom
        && bar.left >= mon.left
        && bar.top >= mon.top
        && bar.right <= mon.right
        && bar.bottom <= mon.bottom
}

/// The edge (`ABE_*`) and monitor rectangle to ask Explorer about for taskbar rectangle `bar` on monitor `mon`,
/// or why not to ask: only a bar docked inside its monitor tells its edge (`docked_inside`). Pure.
fn edge_to_ask(bar: RECT, mon: RECT) -> Result<(u32, RECT), String> {
    if !docked_inside(bar, mon) {
        return Err(format!("taskbar {} not inside monitor {}", rect_text(bar), rect_text(mon)));
    }
    Ok((edge_of(bar, mon), mon))
}

/// The window Explorer has registered as the auto-hide bar on the screen edge of taskbar `tray`, when that
/// window is NOT `tray`. `tray` must be Explorer's primary `Shell_TrayWnd`: the query is sent to it, and it is
/// the window that serves AppBar messages. Explorer allows one auto-hide bar per edge and per
/// monitor: asking it to auto-hide a taskbar on an edge another window already holds makes it show
/// "There is already an auto-hide toolbar on this side of your screen" and refuse. See `EdgeOwner`.
/// Also returns, for the log, the edge (`ABE_*`) and monitor rectangle the question named, or why it was not
/// asked.
fn autohide_edge_owner(tray: HWND) -> (EdgeOwner, Result<(u32, RECT), String>) {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    let mut bar = RECT::default();
    // SAFETY: `bar` is writable; for an invalid window the call fails.
    if unsafe { GetWindowRect(tray, &mut bar) }.is_err() {
        return (EdgeOwner::NoAnswer, Err("GetWindowRect failed".to_string()));
    }
    // SAFETY: MonitorFromWindow takes no pointers; for a taskbar destroyed since GetWindowRect it can
    // return NULL, and GetMonitorInfoW below then fails and is handled.
    let hm = unsafe { MonitorFromWindow(tray, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` has the right `cbSize` and is writable.
    if !unsafe { GetMonitorInfoW(hm, &mut mi) }.as_bool() {
        return (EdgeOwner::NoAnswer, Err("GetMonitorInfoW failed".to_string()));
    }
    // Edge and monitor come from the live window (MonitorFromWindow picks the largest intersection), while
    // Explorer uses its stored ones: a bar that is not docked inside its monitor (auto-hidden, moving, mid
    // display change) could name the wrong edge, so the edge then counts as free (fail open).
    let (edge, mon) = match edge_to_ask(bar, mi.rcMonitor) {
        Ok(q) => q,
        Err(why) => return (EdgeOwner::Free, Err(why)),
    };
    let rc = [mon.left, mon.top, mon.right, mon.bottom];
    let reply = appbar_direct_at(ABM_GETAUTOHIDEBAREX, tray, edge, rc, 0, EDGE_QUERY_TIMEOUT_MS).ok();
    // SAFETY: IsWindow has no preconditions; for a handle that is no longer a window it returns false.
    let owner =
        edge_owner_from_reply(reply, tray.0 as isize, |h| unsafe { IsWindow(Some(HWND(h as *mut c_void))) }.as_bool());
    (owner, Ok((edge, mon)))
}

/// Who holds the auto-hide slot on a taskbar's screen edge, as far as Explorer's answer tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeOwner {
    /// Nobody, the taskbar itself, a handle that is no longer a window, or a taskbar not docked inside its
    /// monitor (its edge is then not asked about: fail open).
    Free,
    /// Another live window (raw handle): asking for auto-hide would make Explorer refuse with its message box.
    Held(isize),
    /// The question could not be asked or got no answer in time: treated like `Free` (fail open, as 1.4.2).
    NoAnswer,
}

/// Decodes the reply to `ABM_GETAUTOHIDEBAREX` sent for taskbar `tray` (`None` = not delivered). Pure.
fn edge_owner_from_reply(reply: Option<usize>, tray: isize, is_window: impl Fn(isize) -> bool) -> EdgeOwner {
    let Some(raw) = reply else { return EdgeOwner::NoAnswer };
    // Window handles travel as 32-bit values and are sign-extended back.
    let owner = raw as u32 as i32 as isize;
    if owner != 0 && owner != tray && is_window(owner) {
        EdgeOwner::Held(owner)
    } else {
        EdgeOwner::Free
    }
}

/// Why `hide_all` does not ask who holds the edge of a taskbar in state `state`, with the owner that then counts, or
/// `None` to ask. `primary`: the first taskbar is Explorer's `Shell_TrayWnd`. `answered`: the direct ABM_GETSTATE
/// send just before was delivered; without it Explorer is slow, the question would most likely go unanswered after
/// another `EDGE_QUERY_TIMEOUT_MS` and wait in Explorer's queue ahead of ABM_SETSTATE, so the request goes out
/// unasked, as 1.4.2 did (fail open). Pure.
fn edge_not_asked(state: u32, primary: bool, answered: bool) -> Option<(EdgeOwner, String)> {
    if state & ABS_AUTOHIDE != 0 {
        Some((EdgeOwner::Free, "auto-hide on".to_string()))
    } else if !primary {
        Some((EdgeOwner::Free, "first taskbar is not Shell_TrayWnd".to_string()))
    } else if !answered {
        Some((EdgeOwner::NoAnswer, "state read failed".to_string()))
    } else {
        None
    }
}

/// What `hide_all` does about Explorer's auto-hide setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoHideAction {
    /// Auto-hide is already on.
    Nothing,
    /// Send `ABM_SETSTATE(ABS_AUTOHIDE)`.
    Send,
    /// Hold the request back: this window (raw handle) holds the edge and Explorer would refuse with its box.
    HoldBack(isize),
}

/// The auto-hide decision for taskbar state `state` (`ABS_*`) and the owner of its edge. Pure.
/// `NoAnswer` sends like `Free` (fail open): a slow Explorer at logon must still get auto-hide.
fn autohide_action(state: u32, owner: EdgeOwner) -> AutoHideAction {
    if state & ABS_AUTOHIDE != 0 {
        return AutoHideAction::Nothing;
    }
    match owner {
        EdgeOwner::Held(h) => AutoHideAction::HoldBack(h),
        EdgeOwner::Free | EdgeOwner::NoAnswer => AutoHideAction::Send,
    }
}

/// `ABM_SETSTATE`; true if the message reached Explorer. A delivered direct send is enough;
/// if it fails we fall back to `SHAppBarMessage` (which always returns TRUE, so it tells us nothing) and
/// confirm by re-reading the state with a direct send. On the UI thread a send that timed out has no fallback
/// (`takes_fallback`): false, so `restore_force` keeps the state file for the watchdog, which restores once the dock
/// has exited.
fn set_state(tray: HWND, state: u32) -> bool {
    let state = state & ABS_MASK;
    let Err(e) = appbar_direct(ABM_SETSTATE, tray, state) else { return true };
    if appbar_fallback(ABM_SETSTATE, tray, state, e).is_none() {
        return false;
    }
    matches!(appbar_direct(ABM_GETSTATE, tray, 0), Ok(v) if state_reads_as(v as u32, state))
}

/// True when a read-back state matches the wanted one on the auto-hide bit only: ABM_GETSTATE never
/// reports ABS_ALWAYSONTOP (live fact X2: after ABM_SETSTATE(2) it reads 0). Pure.
fn state_reads_as(read: u32, want: u32) -> bool {
    (read ^ want) & ABS_AUTOHIDE == 0
}

// ------------------------------------------------------------ state file

/// Parses a state file's content (decimal `ABS_*` value), masked to `ABS_MASK`. Pure.
fn parse_state(bytes: &[u8]) -> Option<u32> {
    let v = std::str::from_utf8(bytes).ok()?.trim().parse::<u32>().ok()?;
    Some(v & ABS_MASK)
}

/// Reads the saved state; `None` if the file is missing or unreadable/invalid.
fn read_state() -> Option<u32> {
    parse_state(&crate::datadir::read_file(&state_file_path()?, crate::datadir::STATE_MAX_BYTES, true).ok()?)
}

// ------------------------------------------------- legacy state (pre-ProgramData versions)

/// State file of the previous dock versions, `%LOCALAPPDATA%\Dock\taskbar-state`. If such a dock
/// exited uncleanly, Explorer keeps OUR autohide and only this file knows the original state.
/// It is only ever READ (medium-writable folder): never written or deleted by this process.
fn legacy_state_path() -> Option<PathBuf> {
    crate::datadir::legacy_dir().map(|d| d.join("taskbar-state"))
}

/// Marker in the admin-only data folder: the legacy state has been taken over (saved as our own
/// state or restored), so the stale legacy file is ignored from then on.
fn legacy_marker_path() -> Option<PathBuf> {
    state_file_path().map(|p| p.with_extension("legacy-done"))
}

/// The legacy state if it exists and has not been taken over yet. Without a writable data folder the
/// take-over could not be recorded, so it is ignored (it would otherwise override the user's
/// setting on every restore). Read without following reparse points, single link, size-capped;
/// the value is masked, and a medium process could set the same taskbar state itself anyway.
fn read_legacy_state() -> Option<u32> {
    if !crate::datadir::writable() || legacy_marker_path().is_none_or(|m| std::fs::symlink_metadata(m).is_ok()) {
        return None;
    }
    let bytes = crate::datadir::read_file(&legacy_state_path()?, crate::datadir::STATE_MAX_BYTES, true).ok()?;
    parse_state(&bytes)
}

/// Records that the legacy state has been taken over (the legacy file itself is left alone).
fn mark_legacy_consumed() {
    let Some(m) = legacy_marker_path() else { return };
    match crate::datadir::write_atomic(&m, b"1", crate::datadir::FileAcl::Inherit) {
        Ok(()) => log!(
            "taskbar: legacy state taken over; %LOCALAPPDATA%\\Dock\\taskbar-state is no longer used (delete it yourself)"
        ),
        Err(e) => log!("taskbar: recording the legacy state take-over failed: {e}"),
    }
}

/// Atomic write (temporary file + rename) in the admin-only data folder.
fn write_state(state: u32) -> Result<(), String> {
    if !crate::datadir::writable() {
        return Err("no writable data folder".into());
    }
    let dst = state_file_path().ok_or("no data folder")?;
    crate::datadir::write_atomic(&dst, state.to_string().as_bytes(), crate::datadir::FileAcl::Inherit)
}

/// saves the original state ONLY if the file is missing or unreadable.
/// Returns true if a valid state exists on file after the call.
/// `pre` is the state already read (and stabilized) by `init`, or read by `hide_again`; `None` = reads it here.
fn ensure_state_saved(pre: Option<u32>) -> bool {
    if let Some(s) = read_state() {
        log!("taskbar: original state {s} (state file)");
        return true;
    }
    let list = native_taskbars();
    let Some(&tray) = list.first() else {
        log!("taskbar: no Shell_TrayWnd, state cannot be saved");
        return false;
    };
    // A previous dock version that died with the taskbar hidden left the true original state in the
    // legacy file: Explorer's current state is then OUR autohide and must not become the "original".
    let legacy = read_legacy_state();
    let mut state = match legacy {
        Some(s) => {
            log!("taskbar: using the original state saved by the previous dock version ({s})");
            s
        }
        None => pre.unwrap_or_else(|| get_state(tray)),
    };
    // Heuristic: autohide active + our mark = the autohide is the work of a previous dead run.
    if state & ABS_AUTOHIDE != 0 && list.iter().any(|&h| has_dock_prop(h)) {
        state = ABS_ALWAYSONTOP;
    }
    match write_state(state) {
        Ok(()) => {
            log!("taskbar: original state saved ({state})");
            if legacy.is_some() {
                mark_legacy_consumed();
            }
            true
        }
        Err(e) => {
            log!("taskbar: saving state failed: {e}");
            false
        }
    }
}

/// Two ABM_GETSTATE reads ~500 ms apart must match (max ~2 s in total);
/// if they do not stabilize the last one wins. Keeps the startup heartbeat going. `None` if the taskbar disappears.
fn stable_state(last_beat: &mut Instant) -> Option<u32> {
    let t = Instant::now();
    let mut prev = get_state(*native_taskbars().first()?);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        crate::watchdog::startup_heartbeat();
        *last_beat = Instant::now();
        let cur = get_state(*native_taskbars().first()?);
        if cur == prev || t.elapsed() >= Duration::from_millis(2000) {
            return Some(cur);
        }
        prev = cur;
    }
}

/// if a taskbar is invisible but marked by us (dead previous run), put it back in place.
fn self_heal() {
    let mut healed = false;
    for h in native_taskbars() {
        if !is_visible(h) && has_dock_prop(h) {
            if !healed {
                appbar(ABM_SETSTATE, h, ABS_ALWAYSONTOP);
                healed = true;
            }
            // SAFETY: Explorer window; the asynchronous call does not block.
            let _ = unsafe { ShowWindowAsync(h, SW_SHOWNA) };
        }
    }
    if healed {
        log!("taskbar: self-heal (taskbar hidden by a previous run)");
        std::thread::sleep(Duration::from_millis(300));
    }
}

// ------------------------------------------------------------------ API

/// Startup sequence, AFTER the watchdog has started and BEFORE the
/// dock window exists: waits for `Shell_TrayWnd` up to 30 s (may block: never from the message loop),
/// registers `TaskbarCreated`, self-heal, `ABM_GETSTATE` + saves state to file (if that fails it does
/// NOT hide), then `hide_all()`. Returns `Ok(true)` if the taskbar was hidden, `Ok(false)` if
/// it was chosen not to hide it (config/`--keep-taskbar`/state not saveable). Does NOT install the
/// WinEventHook (see `install_hook`).
pub fn init() -> Result<bool, String> {
    if crate::args().keep_taskbar || !config::effective_hide_native_taskbar() {
        return Ok(false);
    }
    let start = Instant::now();
    let mut last_beat = Instant::now();
    while native_taskbars().is_empty() {
        if start.elapsed().as_millis() as u64 >= WAIT_TRAY_MS {
            return Err("Shell_TrayWnd did not appear within 30 s".into());
        }
        // The watchdog must not mistake this wait for a hang. "Startup" heartbeat: keeps
        // the watchdog alive without making it switch to the short tolerance before the message loop runs.
        if last_beat.elapsed() >= Duration::from_millis(500) {
            crate::watchdog::startup_heartbeat();
            last_beat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure_taskbar_created_msg();
    self_heal();
    // At logon Explorer may not have loaded the settings yet: ABM_GETSTATE read too
    // early would give a wrong state. Needed only if there is no saved state yet.
    let pre = if read_state().is_none() && read_legacy_state().is_none() { stable_state(&mut last_beat) } else { None };
    if !ensure_state_saved(pre) {
        log!("taskbar: original state not saveable, NOT hiding the taskbar");
        return Ok(false);
    }
    // From here on an exit must restore, even if hide_all failed halfway.
    HIDDEN.store(true, Relaxed);
    hide_all();
    Ok(true)
}

/// Arms the taskbar's share of the apps WinEvent hook (`apps::install_winevent_hook`, whose CREATE..HIDE range
/// covers CREATE, DESTROY and SHOW): `apps::winevent_proc` then hands every raw event to `on_raw_event`, which
/// filters the taskbar classes (also on the parent) and posts `WM_DOCK_TASKBAR_EVENT` to the dock window. Called
/// from the UI thread AFTER the creation of the window and after `ui::attach`. No-op if `HIDDEN` is false.
pub fn install_hook() -> Result<(), String> {
    if !HIDDEN.load(Relaxed) || WANT_EVENTS.load(Relaxed) {
        return Ok(());
    }
    if !crate::apps::hooks_installed() {
        return Err("apps WinEvent hooks not installed".into());
    }
    WANT_EVENTS.store(true, Relaxed);
    log!("taskbar: events armed (apps WinEvent hook)");
    allow_taskbar_created();
    Ok(())
}

/// true while `on_raw_event` wants the events of `apps::winevent_proc`.
pub(crate) fn wants_events() -> bool {
    WANT_EVENTS.load(Relaxed)
}

/// The dock runs elevated: without this exception UIPI would drop the `TaskbarCreated` broadcast
/// from Explorer (medium integrity) and `on_taskbar_created` would never fire.
fn allow_taskbar_created() {
    let ui = crate::hwnd();
    let msg = ensure_taskbar_created_msg();
    if ui.0.is_null() || msg == 0 {
        return;
    }
    // SAFETY: `ui` is the dock window (created by this thread); no optional structure.
    if let Err(e) = unsafe { ChangeWindowMessageFilterEx(ui, msg, MSGFLT_ALLOW, None) } {
        log!("taskbar: ChangeWindowMessageFilterEx(TaskbarCreated) failed: {e}");
    }
}

/// true if, while this process was hiding the taskbar, someone else (watchdog after missed
/// heartbeats, CLI `--restore` with the dock stuck) restored it: the state file is gone.
/// In that case it stops hiding it (`HIDDEN` = false), for good unless the restore was the watchdog's hang restore
/// (`after_outside_restore`): then the next safety check hides it again (`hide_again`), which saves the state first
/// and needs a live watchdog, so the taskbar is never hidden with no state file or no watchdog.
fn restored_externally() -> bool {
    if !HIDDEN.load(Relaxed) {
        return false;
    }
    if state_file_exists() {
        return false;
    }
    HIDDEN.store(false, Relaxed);
    match after_outside_restore(
        RESTORING.load(Relaxed),
        crate::watchdog::stop_requested(),
        config::effective_hide_native_taskbar(),
        crate::watchdog::alive(),
    ) {
        Outside::StopHiding => {
            // The tray host yields to Explorer: apps will re-register their icons with it.
            crate::tray_host::yield_now();
            log!("taskbar: state file gone (external restore), no longer hiding the taskbar");
        }
        Outside::HideAgain => {
            HIDE_AGAIN.store(true, Relaxed);
            HIDE_AGAIN_UNREAD.store(false, Relaxed);
            log!(
                "taskbar: state file gone, restored by the watchdog (no stop request): hiding it again at the next \
                 safety check"
            );
        }
    }
    true
}

/// What the dock does once it finds the taskbar restored from outside (`restored_externally`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outside {
    /// Stop hiding for good: an exit, a stop request, hiding no longer wanted, or no watchdog to guard a new hide.
    StopHiding,
    /// The watchdog's hang restore: hide again at the next safety check.
    HideAgain,
}

/// Pure: only the watchdog's hang restore is undone. A restore a user starts from outside signals the stop event
/// before it deletes the state file (`--restore`, `--install`, `--uninstall`), the in-process ones exit, and the
/// watchdog's wait-failed branch restores and exits; the hang restore does none of these and leaves the dock running.
fn after_outside_restore(exiting: bool, stop_requested: bool, hide_wanted: bool, watchdog_alive: bool) -> Outside {
    if !exiting && !stop_requested && hide_wanted && watchdog_alive {
        Outside::HideAgain
    } else {
        Outside::StopHiding
    }
}

/// Hides the taskbar again after the watchdog's hang restore (`HIDE_AGAIN`; `on_safety_timer` only, UI thread).
/// Its `TIMER_SECOND` tick comes after a heartbeat, so the watchdog has left its hang state and needs another 6.6 s of
/// silence to restore again. The restore set Explorer to the saved original and removed the marks, so the state saved
/// again here is the user's: read with `try_get_state`, since the restore deleted the file and `get_state` makes a
/// failed read 0 (a timed-out read gets no fallback on the UI thread), which would be saved as the original and set in
/// Explorer's saved setting by the next restore. A read Explorer does not answer saves nothing and does not hide: the
/// next safety check reads again (`after_rehide_read`), until Explorer answers or the first checks below end it.
fn hide_again() {
    if RESTORING.load(Relaxed) || crate::watchdog::stop_requested() || !config::effective_hide_native_taskbar() {
        log!("taskbar: not hiding again (exit or stop request)");
        return;
    }
    if !crate::watchdog::alive() {
        log!("taskbar: not hiding again (no watchdog)");
        return;
    }
    let Some(&tray) = native_taskbars().first() else {
        HIDE_AGAIN.store(true, Relaxed); // Explorer restarting: the next tick tries again
        return;
    };
    // The host the watchdog's yield ended, and its forwarder, are stopped first: `tray_sync` below then starts one host
    // and one forwarder. `stop` waits, so not under the popup's mouse hook.
    let hook = crate::popup::suspend_mouse_hook();
    crate::tray_host::stop();
    crate::popup::resume_mouse_hook(hook);
    let state = match after_rehide_read(try_get_state(tray).0, HIDE_AGAIN_UNREAD.load(Relaxed)) {
        Reread::Hide(s) => s,
        Reread::Retry { first } => {
            HIDE_AGAIN.store(true, Relaxed); // Explorer slow: the next tick reads again
            HIDE_AGAIN_UNREAD.store(true, Relaxed);
            if first {
                log!("taskbar: state unreadable after the watchdog's restore, not hiding yet, trying again");
            }
            return;
        }
    };
    if !ensure_state_saved(Some(state)) {
        log!("taskbar: state not saveable after the watchdog's restore, not hiding");
        return;
    }
    HIDDEN.store(true, Relaxed);
    hide_all();
    crate::ui::tray_sync(); // starts the host and announces it when show_tray
    log!("taskbar: hidden again after the watchdog's restore");
}

/// What `hide_again` does with its read of Explorer's state.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Reread {
    /// Explorer answered: this state is saved as the original (`ensure_state_saved`) and the taskbar hidden.
    Hide(u32),
    /// Explorer did not answer: nothing saved, the next safety check reads again; `first`: the first unanswered read
    /// since the watchdog's restore, the only one logged.
    Retry { first: bool },
}

/// Pure: `read` is `try_get_state`'s (`None` = unanswered), `failed_before` whether an earlier read since the
/// watchdog's restore went unanswered. A failed read is never saved as the original.
fn after_rehide_read(read: Option<u32>, failed_before: bool) -> Reread {
    match read {
        Some(s) => Reread::Hide(s),
        None => Reread::Retry { first: !failed_before },
    }
}

/// Stops asking for the raw events (the apps hook stays). Idempotent. UI thread.
pub fn remove_hook() {
    WANT_EVENTS.store(false, Relaxed);
}

/// What a DESTROY event of `hwnd` means for the taskbar.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum DestroyAct {
    /// Explorer's primary taskbar is gone (Explorer exits or crashes).
    ExplorerGone,
    /// The window that held the taskbar's edge is gone: auto-hide can be asked for again.
    HolderGone,
    Ignore,
}

fn destroy_act(hwnd: isize, explorer_tray: isize, holder: isize) -> DestroyAct {
    if hwnd == 0 {
        DestroyAct::Ignore
    } else if hwnd == explorer_tray {
        DestroyAct::ExplorerGone
    } else if hwnd == holder {
        DestroyAct::HolderGone
    } else {
        DestroyAct::Ignore
    }
}

/// true if a hold-back by `owner` is a new conflict (the previous holder was another window or none).
fn hold_back_is_news(prev: isize, owner: isize) -> bool {
    prev != owner
}

/// true if the window of the hold-back in effect (`holder`, 0 = none) is no longer a window, so the safety timer
/// asks for auto-hide again: its DESTROY event can arrive while its WM_DESTROY still runs, and a holder that lets go
/// of the edge in there after other cleanup still held it when `on_winevent` asked. `is_window` runs only while a
/// hold-back is in effect. Pure.
fn holder_left(holder: isize, is_window: impl Fn(isize) -> bool) -> bool {
    holder != 0 && !is_window(holder)
}

/// What a raw WinEvent means for the taskbar.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RawKind {
    Ignore,
    /// Post it as it is (a DESTROY of a window we track).
    Post,
    /// Post it if the window, or its parent, is of a taskbar class.
    CheckClass,
}

/// `tracked` = the window is `EXPLORER_TRAY` or `EDGE_HOLDER`.
fn raw_event_kind(event: u32, id_object: i32, tracked: bool) -> RawKind {
    if id_object != OBJID_WINDOW.0 {
        return RawKind::Ignore;
    }
    match event {
        EVENT_OBJECT_SHOW | EVENT_OBJECT_CREATE => RawKind::CheckClass,
        EVENT_OBJECT_DESTROY if tracked => RawKind::Post,
        _ => RawKind::Ignore,
    }
}

/// Raw WinEvent (UI thread, called by `apps::winevent_proc` while `wants_events`): filters the taskbar classes
/// (also on the parent) and posts `WM_DOCK_TASKBAR_EVENT` (wParam = event, lParam = HWND) to the dock window.
pub(crate) fn on_raw_event(event: u32, hwnd: HWND, id_object: i32) {
    if hwnd.0.is_null() {
        return;
    }
    let h = hwnd.0 as isize;
    let tracked = h == EXPLORER_TRAY.load(Relaxed) || h == EDGE_HOLDER.load(Relaxed);
    match raw_event_kind(event, id_object, tracked) {
        RawKind::Ignore => return,
        RawKind::Post => {}
        RawKind::CheckClass => {
            // SAFETY: GetParent has no preconditions; for invalid windows it fails.
            let parent = unsafe { GetParent(hwnd) }.ok();
            let hit = is_taskbar_class(&class_of(hwnd)) || parent.is_some_and(|p| is_taskbar_class(&class_of(p)));
            if !hit {
                return;
            }
        }
    }
    let ui = crate::hwnd();
    if ui.0.is_null() {
        return;
    }
    probe!(crate::probe::winevent_pass(crate::probe::Src::Apps, crate::probe::Stage::TaskbarEvent, event));
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
    let _ = unsafe { PostMessageW(Some(ui), WM_DOCK_TASKBAR_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)) };
}

/// for each `Shell_TrayWnd`/`Shell_SecondaryTrayWnd` that is not ours: `SetPropW(DockHidden)`,
/// `SHAppBarMessage(ABM_SETSTATE, ABS_AUTOHIDE)` (Explorer's HWND), `ShowWindowAsync(SW_HIDE)`,
/// `SetWindowPos(HWND_NOTOPMOST)` and `SetWindowPos(HWND_BOTTOM)` with
/// `SWP_NOACTIVATE|SWP_NOMOVE|SWP_NOSIZE|SWP_ASYNCWINDOWPOS`. Sets `HIDDEN`. UI thread.
pub fn hide_all() {
    // A process that has started restoring never hides again (re-checked before each action).
    if RESTORING.load(Relaxed) {
        return;
    }
    let list = native_taskbars();
    let Some(&first) = list.first() else { return };
    HIDDEN.store(true, Relaxed);
    let primary = class_of(first) == "Shell_TrayWnd";
    if primary {
        EXPLORER_TRAY.store(first.0 as isize, Relaxed);
    }
    for &h in &list {
        if RESTORING.load(Relaxed) {
            return;
        }
        // SAFETY: static name; the value 1 is only a marker, never dereferenced.
        if let Err(e) = unsafe { SetPropW(h, PROP_DOCK_HIDDEN, Some(HANDLE(std::ptr::dangling_mut::<c_void>()))) } {
            log!("taskbar: SetPropW failed: {e}");
        }
    }
    // Avoids repeating ABM_SETSTATE (and Explorer's AppBar recalculation) if autohide is already active.
    if RESTORING.load(Relaxed) {
        return;
    }
    let (read, answered) = try_get_state(first);
    if let Some(line) = state_change_line(LAST_STATE.swap(read.unwrap_or(STATE_UNKNOWN), Relaxed), read) {
        log!("{line}");
    }
    // A restore that began while the read waited (nested WM_ENDSESSION, watchdog) sends nothing more, not even the
    // edge question: return here, as the checks below would. Not a later check: `restored_externally` clears
    // `HIDDEN` and answers true once only.
    if RESTORING.load(Relaxed) || restored_externally() {
        return;
    }
    // A failed read counts as auto-hide off: the request is sent as 1.4.2 did (fail open).
    let state = read.unwrap_or(0);
    if state & ABS_AUTOHIDE != 0 {
        EDGE_HOLDER.store(0, Relaxed); // a conflict after this one is logged again
    }
    // Explorer refuses auto-hide on an edge another window already holds, and tells the user with a
    // modal box ("already an auto-hide toolbar on this side"). Seen right after a fullscreen app took
    // the foreground: the request is held back then. The taskbar is still hidden below with SW_HIDE, but a
    // taskbar without auto-hide keeps its work area reserved, so its strip stays empty until the holder is gone
    // (`on_winevent`, `on_safety_timer`) or the next re-show re-checks.
    // Only the primary taskbar's edge is asked about, and only when auto-hide is off: Explorer's primary
    // Shell_TrayWnd serves the AppBar messages, and ABM_SETSTATE registers the primary taskbar alone, so a
    // holder on a secondary's edge never holds the request back. Without the primary taskbar first in the
    // list nothing is asked, nor after a direct state read that failed (`edge_not_asked`). No answer sends the
    // request as 1.4.2 did (fail open).
    let (owner, asked) = match edge_not_asked(state, primary, answered) {
        Some((owner, why)) => (owner, Err(why)),
        None => autohide_edge_owner(first),
    };
    match autohide_action(state, owner) {
        AutoHideAction::Nothing => {}
        AutoHideAction::HoldBack(owner) => {
            if hold_back_is_news(EDGE_HOLDER.swap(owner, Relaxed), owner) {
                // `Held` comes only from an answered question, which named the edge and monitor.
                let (edge, mon) = asked.as_ref().map_or((u32::MAX, RECT::default()), |&q| q);
                let holder = HWND(owner as *mut c_void);
                let mut pid = 0u32;
                // SAFETY: `pid` is writable; for a handle that is no longer a window the call fails, pid stays 0.
                unsafe { GetWindowThreadProcessId(holder, Some(&mut pid)) };
                log!(
                    "taskbar: {} but window 0x{owner:X} (class {:?}, pid {pid}) already holds the {} edge of \
                     taskbar 0x{:X} on monitor ({}, {})-({}, {}), ABM_SETSTATE held back",
                    autohide_off_text(read),
                    class_of(holder),
                    edge_name(edge),
                    first.0 as isize,
                    mon.left,
                    mon.top,
                    mon.right,
                    mon.bottom
                );
            }
        }
        AutoHideAction::Send => {
            // A restore can have run while the messages above waited for Explorer: nested in this very call (the
            // direct sends block, but SHAppBarMessage, the fallback of a refused read, can handle incoming sent
            // messages while it waits, so WM_ENDSESSION can run begin_exit + restore inside it), or from the
            // watchdog, which deletes the state file. Auto-hide sent after that restore stays on in Explorer and
            // becomes the "original" at the next start.
            if RESTORING.load(Relaxed) || restored_externally() {
                return;
            }
            EDGE_HOLDER.store(0, Relaxed);
            log!("taskbar: {}{}, sending ABM_SETSTATE", autohide_off_text(read), edge_check_text(owner, &asked));
            // Send without verification: we are on the UI thread here and the confirmation re-read is not needed.
            // The direct send waits up to APPBAR_TIMEOUT_MS, so the same race is checked again before the
            // fallback: a restore run during that wait (the watchdog's) has already sent the original state, and the
            // fallback would put auto-hide back on after it.
            if let Err(e) = appbar_direct(ABM_SETSTATE, first, ABS_AUTOHIDE) {
                if RESTORING.load(Relaxed) || restored_externally() {
                    return;
                }
                appbar_fallback(ABM_SETSTATE, first, ABS_AUTOHIDE, e);
            }
        }
    }
    // The same race for the hide below: a restore that ran during the waits above has shown the taskbars and
    // removed their DockHidden mark; hiding them again would leave them hidden with no mark and no state file.
    if RESTORING.load(Relaxed) || restored_externally() {
        return;
    }
    let flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS;
    for &h in &list {
        if RESTORING.load(Relaxed) {
            return;
        }
        // SAFETY: Explorer windows (another process): only asynchronous variants, which do not block
        // the UI thread if Explorer is busy.
        unsafe {
            let _ = ShowWindowAsync(h, SW_HIDE);
            if let Err(e) = SetWindowPos(h, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags) {
                log!("taskbar: SetWindowPos(NOTOPMOST) failed: {e}");
            }
            if let Err(e) = SetWindowPos(h, Some(HWND_BOTTOM), 0, 0, 0, 0, flags) {
                log!("taskbar: SetWindowPos(BOTTOM) failed: {e}");
            }
        }
    }
}

/// Callback for `WM_DOCK_TASKBAR_EVENT` (wParam = event, lParam = HWND): if `hwnd` (or its parent)
/// is of a native taskbar class and visible, redoes `hide_all()`. The creation of Explorer's primary
/// `Shell_TrayWnd` is an Explorer restart (`explorer_restarted`).
pub fn on_winevent(event: u32, hwnd: HWND) {
    if !HIDDEN.load(Relaxed) || hwnd.0.is_null() || restored_externally() {
        return;
    }
    if event == EVENT_OBJECT_DESTROY {
        let h = hwnd.0 as isize;
        match destroy_act(h, EXPLORER_TRAY.load(Relaxed), EDGE_HOLDER.load(Relaxed)) {
            DestroyAct::ExplorerGone => {
                EXPLORER_TRAY.store(0, Relaxed);
                explorer_gone();
            }
            DestroyAct::HolderGone => {
                EDGE_HOLDER.store(0, Relaxed);
                log!("taskbar: window 0x{h:X} that held the taskbar's edge is gone, asking for auto-hide again");
                hide_all();
            }
            DestroyAct::Ignore => {}
        }
        return;
    }
    let class = class_of(hwnd);
    if event == EVENT_OBJECT_CREATE && class == "Shell_TrayWnd" && !is_dock_tray_host(hwnd) {
        explorer_restarted("new Shell_TrayWnd");
        return;
    }
    let target = if is_taskbar_class(&class) {
        hwnd
    } else {
        // SAFETY: GetParent has no preconditions; for invalid windows it fails.
        match unsafe { GetParent(hwnd) } {
            Ok(p) if is_taskbar_class(&class_of(p)) => p,
            _ => return,
        }
    };
    if is_dock_tray_host(target) || !is_visible(target) {
        return;
    }
    log!("taskbar: event 0x{event:X} on visible taskbar, hiding it again");
    hide_all();
    crate::tray_host::raise();
}

/// 3 s safety check (`TIMER_SECOND`): if a native taskbar is visible, or the window of the hold-back in effect is
/// no longer a window (`holder_left`), redoes `hide_all()`. A visible taskbar is logged for the first 20 times
/// (`safety_logs`): the event path should have hidden it already.
/// With `HIDDEN` false it only runs a pending re-hide after the watchdog's restore (`hide_again`); no-op if the
/// taskbar was restored from outside (see `restored_externally`).
pub fn on_safety_timer() {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    if !HIDDEN.load(Relaxed) {
        if HIDE_AGAIN.swap(false, Relaxed) {
            hide_again();
        }
        return;
    }
    if restored_externally() {
        return;
    }
    let holder = EDGE_HOLDER.load(Relaxed);
    // SAFETY: IsWindow has no preconditions; for a handle that is no longer a window it returns false.
    if holder_left(holder, |h| unsafe { IsWindow(Some(HWND(h as *mut c_void))) }.as_bool()) {
        EDGE_HOLDER.store(0, Relaxed);
        log!("taskbar: edge holder 0x{holder:X} is no longer a window, asking for auto-hide again");
        hide_all();
        return;
    }
    if native_taskbars().into_iter().any(is_visible) {
        let n = SAFETY_FINDS.fetch_add(1, Relaxed);
        if safety_logs(n) {
            log!("taskbar: safety check found a visible taskbar ({}), hiding it again", n + 1);
        }
        hide_all();
    }
}

/// Whether the safety check's find number `count` (from 0) is logged: the first 20 per process, so a taskbar that
/// stays visible (Explorer hung) does not log every 3 s.
fn safety_logs(count: u32) -> bool {
    count < 20
}

/// `TaskbarCreated` message (Explorer restarted): redoes self-heal + state saving (only if the
/// file is missing) + `hide_all()`. Ignores the broadcast if it is ours (sent by `restore_force()`
/// within the last 2 s). No-op if `HIDDEN` is false. If the state file is missing the restore was
/// done by another process (watchdog/CLI, whose broadcast is not recognizable as ours):
/// we stop hiding instead of re-saving the current state as "original".
pub fn on_taskbar_created() {
    if !HIDDEN.load(Relaxed) || restored_externally() {
        return;
    }
    let last = LAST_BROADCAST_MS.load(Relaxed);
    if last != 0 && now_ms().saturating_sub(last) < OWN_BROADCAST_WINDOW_MS {
        return;
    }
    explorer_restarted("TaskbarCreated");
}

/// Explorer's taskbar was destroyed (Explorer exited or crashed). Our tray host is a `Shell_TrayWnd` too:
/// if it exists while the new Explorer starts, Explorer never registers its desktop with ShellWindows
/// (every de-elevated launch then fails) nor broadcasts `TaskbarCreated`. So the host is stopped now
/// and started again by `on_reannounce_timer` once the new desktop is registered.
fn explorer_gone() {
    if !crate::tray_host::is_started() {
        return;
    }
    log!("taskbar: Explorer's taskbar destroyed, stopping our tray host until Explorer is back");
    let hook = crate::popup::suspend_mouse_hook();
    crate::tray_host::stop();
    crate::popup::resume_mouse_hook(hook);
    HOST_PAUSED_AT.store(now_ms(), Relaxed);
    arm_reannounce(1000);
}

/// (Re)arms the one-shot `TIMER_TRAY_REANNOUNCE` on the dock window.
fn arm_reannounce(ms: u32) {
    let ui = crate::hwnd();
    if !ui.0.is_null() {
        // SAFETY: `ui` is the dock window, created by this thread (UI thread).
        if unsafe { SetTimer(Some(ui), TIMER_TRAY_REANNOUNCE, ms, None) } == 0 {
            log!("taskbar: SetTimer(TIMER_TRAY_REANNOUNCE) failed: {:?}", windows::core::Error::from_thread());
        }
    }
}

/// Explorer restarted: self-heal, hide, and announce our tray again. Reached from `TaskbarCreated` and
/// from the creation of a new `Shell_TrayWnd` (`on_winevent`): while our tray host exists Explorer
/// does not broadcast `TaskbarCreated` on restart, so the window event is what actually fires.
fn explorer_restarted(why: &str) {
    log!("taskbar: Explorer restarted ({why}), redoing self-heal and hide");
    EDGE_HOLDER.store(0, Relaxed); // the new Explorer's conflicts are news
    self_heal();
    if !ensure_state_saved(None) {
        log!("taskbar: state not saveable after the Explorer restart, not hiding");
        HIDDEN.store(false, Relaxed);
        return;
    }
    hide_all();
    if HOST_PAUSED_AT.load(Relaxed) != 0 {
        return; // our host comes back (and is announced) once the new desktop is registered
    }
    if RESTORING.load(Relaxed) {
        return; // exit began: no host raise, no re-announcement
    }
    // Explorer's new Shell_TrayWnd is born topmost and would sit in front of ours: it is brought back to the
    // top and, after 300 ms, announced again so apps re-register their icons with us.
    crate::tray_host::raise_after("Explorer restart");
    arm_reannounce(300);
}

/// Marks the exit as begun: `RESTORING = true`, drops the Explorer-restart pause and kills
/// `TIMER_TRAY_REANNOUNCE`. Idempotent, UI thread, first thing in the explicit exit requests (quit request,
/// hotkey, close, end of session).
/// This is only the EARLY part of the exit: `restore()` / `restore_force()` remain the exit path that puts
/// the taskbar back (they set `RESTORING` themselves). `begin_exit` merely makes the exit visible sooner,
/// so that `pause_step`, `hide_all` and `explorer_restarted` stop acting before `restore()` runs, and it
/// cancels the pause and its timer, which `restore()` does not.
pub fn begin_exit() {
    RESTORING.store(true, Relaxed);
    HOST_PAUSED_AT.store(0, Relaxed);
    let ui = crate::hwnd();
    if !ui.0.is_null() {
        // SAFETY: our own id on the dock window (UI thread); fails harmlessly if the timer does not exist.
        let _ = unsafe { KillTimer(Some(ui), TIMER_TRAY_REANNOUNCE) };
    }
}

/// "Normal" restore by the dock on exit (WM_DESTROY/clean exit, WM_ENDSESSION wParam != 0,
/// panic hook). Idempotent and guarded: if this process hid nothing (`HIDDEN` false) and
/// the state file does not exist, it does nothing; after the session end's restore, nothing unless this process hid
/// the taskbar again (`restore_needed`). Otherwise it runs `restore_force()` and clears `HIDDEN`.
/// Callable from any thread.
pub fn restore() {
    RESTORING.store(true, Relaxed);
    let was_hidden = HIDDEN.swap(false, Relaxed);
    // Read only when this process hid nothing, as before.
    let state_left = !was_hidden && (state_file_exists() || read_legacy_state().is_some());
    if !restore_needed(was_hidden, state_left, session_ending()) {
        return;
    }
    restore_force();
}

/// Whether `restore()` has anything to do: this process hid the taskbar, or (outside a session end) a state file or
/// a legacy state is left. The session end's restore keeps the file for the next sign-in, so a later call of the same
/// process (WM_DESTROY, `cleanup`, the end of `main`) does not restore a second time. Pure.
fn restore_needed(was_hidden: bool, state_left: bool, session_ending: bool) -> bool {
    was_hidden || (state_left && !session_ending)
}

/// The session-end restore (`WM_ENDSESSION` with wParam != 0, UI thread): ends the watchdog first, so it cannot
/// restore again after the dock and delete the state file, then marks the session end (`restore_force` keeps the
/// file for the next sign-in's `init`) and restores. A restore that is not delivered keeps the file as always.
pub fn restore_at_session_end() {
    crate::watchdog::end_for_session_end();
    SESSION_END.store(true, Relaxed);
    restore();
}

/// true once `restore_at_session_end` ran in this process.
fn session_ending() -> bool {
    SESSION_END.load(Relaxed)
}

/// What `restore_force` does with the state file after its `ABM_SETSTATE`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum AfterRestore {
    /// Delivered: the file has done its job.
    Delete,
    /// Not delivered (Explorer hung or shutting down): kept, so the watchdog, the next run or `--restore` finish it.
    KeepUndelivered,
    /// Delivered at a session end: kept for the next sign-in's `init`, since Explorer may save its settings before
    /// this restore and does not keep a restored "not auto-hide".
    KeepForNextSignIn,
}

/// Pure decision of `restore_force` on the state file (`session_ending`: `restore_at_session_end` ran).
fn after_restore(delivered: bool, session_ending: bool) -> AfterRestore {
    match (delivered, session_ending) {
        (false, _) => AfterRestore::KeepUndelivered,
        (true, false) => AfterRestore::Delete,
        (true, true) => AfterRestore::KeepForNextSignIn,
    }
}

/// UNCONDITIONAL restore (watchdog, CLI `--restore`): reads the state from the file (default
/// `ABS_ALWAYSONTOP`, NEVER autohide), `ABM_SETSTATE`, for each taskbar `RemovePropW` +
/// `ShowWindowAsync(SW_SHOWNA)` + `SetWindowPos(HWND_TOPMOST, ... ASYNCWINDOWPOS)`, then
/// deletes the state file (kept at a session end: `after_restore`) and then
/// `SendNotifyMessageW(HWND_BROADCAST, TaskbarCreated)`. Without a file and
/// without the `DockHidden` mark it does nothing; without `Shell_TrayWnd` it keeps the file and does nothing.
/// Callable from any thread and from a process other than the dock.
pub fn restore_force() {
    RESTORING.store(true, Relaxed);
    HIDDEN.store(false, Relaxed);
    let saved = read_state();
    // No state of ours: a previous dock version may have died leaving its (legacy) state behind.
    let legacy = if saved.is_none() { read_legacy_state() } else { None };
    let list = native_taskbars();
    // Guard: without a state file and without our mark there is nothing to restore (avoids
    // forcing ALWAYSONTOP over the user's autohide setting when the watchdog fires after
    // an exit that was already restored). Does not depend on visibility: the dock's asynchronous SW_SHOWNA
    // may not have been executed by Explorer yet. `hide_all` always marks BEFORE hiding.
    if saved.is_none() && legacy.is_none() && !list.iter().any(|&h| has_dock_prop(h)) {
        return;
    }
    let state = saved.or(legacy).unwrap_or(ABS_ALWAYSONTOP);
    let Some(&first) = list.first() else {
        // Explorer absent or restarting: ABM_SETSTATE impossible. The file stays, so the next
        // run (or another `--restore`) restores the right original state instead of saving as
        // "original" the autohide set by us and persisted by Explorer.
        log!("taskbar: no Shell_TrayWnd, restore postponed (state file kept)");
        return;
    };
    // The tray host (of this process and of other dock processes) yields BEFORE the `TaskbarCreated`
    // broadcast and before deleting the state file: so apps re-register their icons
    // with Explorer. `yield_now` is asynchronous and does not block.
    crate::tray_host::yield_now();
    crate::tray_host::yield_foreign_hosts();
    let delivered = set_state(first, state);
    let flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS;
    for &h in &list {
        // SAFETY: Explorer windows: RemovePropW and the asynchronous variants do not block the caller.
        unsafe {
            let _ = RemovePropW(h, PROP_DOCK_HIDDEN);
            let _ = ShowWindowAsync(h, SW_SHOWNA);
            if let Err(e) = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) {
                log!("taskbar: SetWindowPos(TOPMOST) failed: {e}");
            }
        }
    }
    // Deletes the state file BEFORE the broadcast: a live dock handling TaskbarCreated sees
    // the file gone (`restored_externally`) and does not hide the taskbar again. If however ABM_SETSTATE
    // did not reach Explorer (session shutting down, hung) the file stays: the autohide set by
    // us has already been persisted by Explorer and without a file the next run would save it as "original".
    // This way the watchdog, the next run's self-heal or `--restore` complete the restore. At a session end the file
    // is kept too: the next sign-in's `init` reads it as the original.
    if delivered && legacy.is_some() {
        mark_legacy_consumed();
    }
    match after_restore(delivered, session_ending()) {
        AfterRestore::KeepUndelivered => log!("taskbar: ABM_SETSTATE not delivered, state file kept"),
        AfterRestore::KeepForNextSignIn => log!("taskbar: session ending, state file kept for the next sign-in"),
        AfterRestore::Delete => {
            if let Some(p) = state_file_path() {
                // Deletes the name itself (never a link target), inside the admin-only data folder.
                if let Err(e) = std::fs::remove_file(p) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        log!("taskbar: removing state file failed: {e}");
                    }
                }
            }
        }
    }
    // Explorer keeps a restored "not auto-hide" only until its next start: its saved setting is set back as well, also
    // when the file is kept for the next sign-in. After the shows above, so the registry I/O comes last.
    if wants_registry_fix(delivered, state, saved.or(legacy).is_some()) {
        persist_not_autohide(first);
    }
    broadcast_taskbar_created();
    log!("taskbar: restored (state {state})");
}

/// Like `restore_force()`, but first waits up to `max` for a `Shell_TrayWnd` to exist (Explorer
/// restarting). Only for watchdog and CLI `--restore`: blocks, NEVER from the UI thread.
pub fn restore_force_waiting(max: Duration) {
    let t = Instant::now();
    while native_taskbars().is_empty() && t.elapsed() < max {
        std::thread::sleep(Duration::from_millis(200));
    }
    restore_force();
}

// ------------------------------------------------- Explorer's saved setting (StuckRects3)

/// Explorer's saved taskbar setting, value "Settings", under `HKEY_USERS\<sid>`: Explorer saves ABM_SETSTATE's
/// auto-hide there and never a restored "not auto-hide", so after a restore its next start (an Explorer restart, the
/// next sign-in, a session without the dock) would come up auto-hide.
const STUCKRECTS3: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";
/// Bit of byte 8 of the "Settings" blob that holds auto-hide.
const STUCKRECTS_AUTOHIDE: u8 = 0x01;

/// What `saved_autohide_off` found in the "Settings" blob.
#[derive(Debug, PartialEq, Eq)]
enum SavedAutohide {
    /// Auto-hide saved: the blob with only that bit cleared, to write back.
    Clear(Vec<u8>),
    /// Already saved without auto-hide: nothing to write.
    AlreadyOff,
    /// Not the layout known (the blob starts with its own length as a u32, and has at least 12 bytes): left alone.
    Unknown,
}

/// The "Settings" blob with the auto-hide bit of byte 8 cleared. Pure, never panics.
fn saved_autohide_off(blob: &[u8]) -> SavedAutohide {
    let size = blob.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_le_bytes);
    if blob.len() < 12 || size != u32::try_from(blob.len()).ok() {
        return SavedAutohide::Unknown;
    }
    let mut out = blob.to_vec();
    match out.get_mut(8) {
        Some(b) if *b & STUCKRECTS_AUTOHIDE != 0 => {
            *b &= !STUCKRECTS_AUTOHIDE;
            SavedAutohide::Clear(out)
        }
        Some(_) => SavedAutohide::AlreadyOff,
        None => SavedAutohide::Unknown,
    }
}

/// Whether `restore_force` sets Explorer's saved setting back: only after a delivered restore to a state without
/// auto-hide that came from the state file (or the legacy one). The `ABS_ALWAYSONTOP` default of a restore without a
/// file is a guess, and writing it would overwrite an auto-hide the user saved. Pure.
fn wants_registry_fix(delivered: bool, state: u32, from_file: bool) -> bool {
    delivered && from_file && state & ABS_AUTOHIDE == 0
}

/// `<sid>\...\StuckRects3`, the key under `HKEY_USERS`. Pure.
fn stuckrects_subkey(sid: &str) -> String {
    format!(r"{sid}\{STUCKRECTS3}")
}

/// The SID of the user whose process owns window `h` (Explorer's taskbar): the token user, so under Administrator
/// protection it names the person at the keyboard, not the account the elevated dock runs as. The sequence of
/// `shell_thread::shell_user_sid`, from a given window.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn window_user_sid(h: HWND) -> Result<String, String> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    let mut pid = 0u32;
    // SAFETY: `pid` is writable for the call; a stale window only yields 0.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if pid == 0 {
        return Err("no taskbar process".into());
    }
    // SAFETY: a query-only open of that pid; the handle is owned (and closed) by `_process`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| format!("OpenProcess: {e}"))?;
    // SAFETY: fresh valid handle, owned from here on.
    let _process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let mut token = HANDLE::default();
    // SAFETY: `process` is valid; `token` is writable.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(|e| format!("OpenProcessToken: {e}"))?;
    // SAFETY: fresh valid token handle, owned from here on.
    let _token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut len = 0u32;
    // SAFETY: size query; `len` is writable. The expected "buffer too small" error is ignored.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if (len as usize) < std::mem::size_of::<TOKEN_USER>() {
        return Err("TokenUser: no size".into());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` writable, pointer-aligned bytes.
    unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }
        .map_err(|e| format!("TokenUser: {e}"))?;
    // SAFETY: the buffer starts with a TOKEN_USER whose SID points inside `buf`, alive until the end.
    let tu = unsafe { std::ptr::read(buf.as_ptr().cast::<TOKEN_USER>()) };
    let mut s = PWSTR::null();
    // SAFETY: `tu.User.Sid` points to a valid SID inside `buf`; `s` receives a LocalAlloc'd string.
    unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut s) }.map_err(|e| format!("ConvertSidToStringSidW: {e}"))?;
    // SAFETY: `s` is the NUL-terminated string returned above.
    let out = unsafe { s.to_string() };
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc; not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
    drop(buf);
    out.map_err(|e| format!("SID string: {e}"))
}

/// Sets Explorer's saved setting back to "not auto-hide" (`restore_force`, after a delivered restore from the state
/// file): StuckRects3 "Settings" of the user who owns `first`, opened under `HKEY_USERS` (never `HKEY_CURRENT_USER`,
/// never created), with only the auto-hide bit cleared. Writes nothing on any failure or an unknown layout. Any thread
/// and process.
fn persist_not_autohide(first: HWND) {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_USERS, KEY_QUERY_VALUE, KEY_SET_VALUE,
    };

    let sid = match window_user_sid(first) {
        Ok(s) => s,
        Err(e) => {
            log!("taskbar: StuckRects3 not updated: {e}");
            return;
        }
    };
    let mut key = HKEY::default();
    // SAFETY: the subkey is a NUL-terminated HSTRING for the call; `key` is a writable HKEY.
    let r = unsafe {
        RegOpenKeyExW(
            HKEY_USERS,
            &HSTRING::from(stuckrects_subkey(&sid)),
            None,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        )
    };
    if r != ERROR_SUCCESS {
        log!("taskbar: StuckRects3 not updated: opening the key: {}", windows::core::Error::from(r.to_hresult()));
        return;
    }
    let outcome = rewrite_saved_autohide(key);
    // SAFETY: `key` was opened above and is closed once, on every path.
    unsafe {
        let _ = RegCloseKey(key);
    }
    match outcome {
        Ok(Some(line)) => log!("{line}"),
        Ok(None) => {}
        Err(e) => log!("taskbar: StuckRects3 not updated: {e}"),
    }
}

/// Reads "Settings" from the open StuckRects3 `key` and writes it back without auto-hide. `Ok(Some(line))` is the
/// line to log, `Ok(None)` nothing to do (already off), `Err` a failed read or write.
fn rewrite_saved_autohide(key: windows::Win32::System::Registry::HKEY) -> Result<Option<String>, String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegQueryValueExW, RegSetValueExW, REG_BINARY, REG_VALUE_TYPE};

    let mut buf = [0u8; 256];
    let mut len = buf.len() as u32;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: `key` is open with KEY_QUERY_VALUE; `buf` holds `len` writable bytes; `kind` and `len` are writable.
    let r =
        unsafe { RegQueryValueExW(key, w!("Settings"), None, Some(&mut kind), Some(buf.as_mut_ptr()), Some(&mut len)) };
    if r != ERROR_SUCCESS {
        return Err(format!("reading Settings: {}", windows::core::Error::from(r.to_hresult())));
    }
    let blob = buf.get(..len as usize).unwrap_or_default();
    let unexpected =
        || Ok(Some(format!("taskbar: StuckRects3 has an unexpected layout ({} bytes), left alone", blob.len())));
    if kind != REG_BINARY {
        return unexpected();
    }
    match saved_autohide_off(blob) {
        SavedAutohide::AlreadyOff => Ok(None),
        SavedAutohide::Unknown => unexpected(),
        SavedAutohide::Clear(data) => {
            // SAFETY: `key` is open with KEY_SET_VALUE; the name is a static NUL-terminated literal and `data` lives
            // for the call.
            let r = unsafe { RegSetValueExW(key, w!("Settings"), None, REG_BINARY, Some(&data)) };
            if r != ERROR_SUCCESS {
                return Err(format!("writing Settings: {}", windows::core::Error::from(r.to_hresult())));
            }
            Ok(Some("taskbar: Explorer's saved setting set back to not auto-hide (StuckRects3)".into()))
        }
    }
}

// ------------------------------------------------------- tray host

/// true if this process hid the native taskbar (and has not restored yet).
/// `main` starts the tray host only in that case.
pub fn is_hidden() -> bool {
    HIDDEN.load(Relaxed)
}

/// true while our tray host is paused for an Explorer restart (`explorer_gone` until resume).
pub fn host_paused() -> bool {
    HOST_PAUSED_AT.load(Relaxed) != 0
}

/// true once the exit has begun (`RESTORING`, set by `begin_exit` / `restore()`).
pub fn exiting() -> bool {
    RESTORING.load(Relaxed)
}

/// true for a `Shell_TrayWnd` created by the tray host of ANY dock process (property
/// `tray_host::PROP_DOCK_TRAY_HOST`) or by this process. Used by `native_taskbars()`,
/// `on_winevent` and by the tray host to tell Explorer's taskbar apart.
pub fn is_dock_tray_host(h: HWND) -> bool {
    // SAFETY: static literal property name; invalid handle = property absent.
    !unsafe { GetPropW(h, crate::tray_host::PROP_DOCK_TRAY_HOST) }.is_invalid() || is_ours(h)
}

/// Announces our tray: `LAST_BROADCAST_MS = now` + `SendNotifyMessageW(HWND_BROADCAST,
/// TaskbarCreated)`, so apps re-register their icons with the first `Shell_TrayWnd` (our
/// host) and our `on_taskbar_created` ignores the broadcast. Called by `main` after a successful
/// `tray_host::start` and by `on_reannounce_timer`.
pub fn announce_tray() {
    if broadcast_taskbar_created() {
        log!("taskbar: tray announced (TaskbarCreated)");
    }
}

/// `LAST_BROADCAST_MS = now` + non-blocking `SendNotifyMessageW(HWND_BROADCAST, TaskbarCreated)`,
/// shared by `restore_force` and `announce_tray`. true if the broadcast was sent; failures are logged.
fn broadcast_taskbar_created() -> bool {
    let msg = ensure_taskbar_created_msg();
    if msg == 0 {
        return false;
    }
    LAST_BROADCAST_MS.store(now_ms(), Relaxed);
    // SAFETY: non-blocking broadcast (SendNotifyMessage); no pointers in the parameters.
    if let Err(e) = unsafe { SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0)) } {
        log!("taskbar: TaskbarCreated broadcast failed: {e}");
        return false;
    }
    true
}

/// What `on_reannounce_timer` does while our tray host is paused for an Explorer restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseStep {
    /// Keep waiting for the new Explorer's desktop.
    Wait,
    /// Start our tray host again (desktop registered, or `HOST_RESUME_MAX_MS` elapsed).
    Resume,
    /// The dock is exiting: drop the pause, never start a host.
    GiveUp,
}

/// Pure decision of the pause: `paused_at` / `now` from `now_ms()`, `exiting` = `RESTORING`.
fn pause_step(paused_at: u64, now: u64, desktop_ready: bool, exiting: bool) -> PauseStep {
    if exiting {
        PauseStep::GiveUp
    } else if !desktop_ready && now.saturating_sub(paused_at) < HOST_RESUME_MAX_MS {
        PauseStep::Wait
    } else {
        PauseStep::Resume
    }
}

/// `TIMER_TRAY_REANNOUNCE` (one-shot, already killed by `main`): second announcement after an Explorer
/// restart, if the taskbar is still hidden and the tray host is active.
pub fn on_reannounce_timer() {
    let paused = HOST_PAUSED_AT.load(Relaxed);
    if paused != 0 {
        // Waiting for the new Explorer: its desktop first, then our host (`HOST_RESUME_MAX_MS` at most).
        let now = now_ms();
        match pause_step(paused, now, crate::shell_thread::desktop_registered(), RESTORING.load(Relaxed)) {
            PauseStep::Wait => arm_reannounce(1000),
            PauseStep::GiveUp => HOST_PAUSED_AT.store(0, Relaxed),
            PauseStep::Resume => {
                HOST_PAUSED_AT.store(0, Relaxed);
                if !HIDDEN.load(Relaxed) {
                    return;
                }
                let waited = now.saturating_sub(paused);
                log!("taskbar: Explorer back after {waited} ms, syncing the tray host");
                // The current `show_tray` decides (it may have been switched off during the pause).
                crate::ui::tray_sync();
            }
        }
        return;
    }
    if HIDDEN.load(Relaxed) && crate::tray_host::is_running() {
        announce_tray();
    }
}

/// `remove_hook()` + `restore()`. UI thread, in `cleanup` (right away, before the rest).
pub fn shutdown() {
    remove_hook();
    restore();
}

/// CLI `--restore`: signals stop to the running instance (`watchdog::signal_stop_to_instance`),
/// waits up to 3 s for it to exit, then `restore_force()`. Returns the process exit code (0 ok).
pub fn run_restore_cli() -> i32 {
    let running = crate::watchdog::signal_stop_to_instance();
    if running {
        log!("--restore: instance running, exit request sent");
        // Wait for the instance to really exit: its single-instance mutex disappears with the process.
        // (Not the state file: an older dock keeps it in the legacy folder.) Then restore over a quiet
        // taskbar, so the exiting dock cannot re-hide it on our TaskbarCreated broadcast.
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) && crate::watchdog::instance_running() {
            std::thread::sleep(Duration::from_millis(100));
        }
        if crate::watchdog::instance_running() {
            log!("--restore: the instance did not exit within 5 s, restoring anyway");
        }
        // A moment to let the process shutdown finish.
        std::thread::sleep(Duration::from_millis(200));
    } else {
        log!("--restore: no running instance");
    }
    restore_force_waiting(Duration::from_secs(5));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wsA A6: a delivered restore deletes the state file, except at a session end, which keeps it for the next
    /// sign-in's `init` (Explorer may save its own setting before the restore); an undelivered one always keeps it.
    #[test]
    fn a_session_end_keeps_the_state_for_the_next_sign_in() {
        use AfterRestore::*;
        assert_eq!(after_restore(true, false), Delete);
        assert_eq!(after_restore(true, true), KeepForNextSignIn);
        assert_eq!(after_restore(false, false), KeepUndelivered);
        assert_eq!(after_restore(false, true), KeepUndelivered);
    }

    /// wsA A6: after the session end's restore (file kept) a later `restore()` of the same process (WM_DESTROY, the
    /// cleanup, the end of `main`) runs only if this process hid the taskbar again; outside a session end, as before.
    #[test]
    fn a_restore_after_the_session_end_restore_does_nothing() {
        //                     hidden left   session end
        assert!(restore_needed(true, false, false));
        assert!(restore_needed(true, true, false));
        assert!(restore_needed(false, true, false), "a state file or a legacy state left: restored, as before");
        assert!(!restore_needed(false, false, false), "nothing hidden, nothing left: as before");
        assert!(restore_needed(true, false, true), "the session end's own restore");
        assert!(restore_needed(true, true, true));
        assert!(!restore_needed(false, true, true), "the file kept for the next sign-in stays");
        assert!(!restore_needed(false, false, true));
    }

    /// wsD D2: Explorer's saved StuckRects3 blob with the auto-hide bit (byte 8, 0x01) cleared and nothing else; a blob
    /// of another layout (its first u32 is not its length, or under 12 bytes) is never touched.
    #[test]
    fn stuckrects_autohide_bit_cleared() {
        use SavedAutohide::*;
        let p1: [u8; 48] = [
            0x30, 0x00, 0x00, 0x00, 0xFE, 0xFF, 0xFF, 0xFF, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x90, 0x00,
            0x00, 0x00, 0x3C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x34, 0x08, 0x00, 0x00, 0x00, 0x0F, 0x00, 0x00,
            0x70, 0x08, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        let mut want = p1;
        want[8] = 0x02;
        assert_eq!(saved_autohide_off(&p1), Clear(want.to_vec()), "only byte 8 changes, 0x03 to 0x02");
        assert_eq!(saved_autohide_off(&want), AlreadyOff);
        let mut one = p1;
        one[8] = 0x01;
        let mut zero = p1;
        zero[8] = 0x00;
        assert_eq!(saved_autohide_off(&one), Clear(zero.to_vec()));
        assert_eq!(saved_autohide_off(&p1[..47]), Unknown, "47 bytes whose first u32 says 48");
        let mut other = p1;
        other[0] = 0x2C;
        assert_eq!(saved_autohide_off(&other), Unknown, "first u32 differs from the length");
        assert_eq!(saved_autohide_off(&[0x0B, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0]), Unknown, "11 bytes");
        assert_eq!(saved_autohide_off(&[]), Unknown);
    }

    /// wsD D2: the saved setting is set back only after a delivered restore to a state without auto-hide that came
    /// from the state file (or the legacy one), never from the ALWAYSONTOP default of a restore without a file.
    #[test]
    fn registry_fix_only_after_a_delivered_not_autohide_restore_from_the_file() {
        assert!(wants_registry_fix(true, 0, true));
        assert!(wants_registry_fix(true, 2, true));
        for (delivered, state, from_file) in
            [(true, 1, true), (true, 3, true), (false, 0, true), (false, 2, true), (true, 2, false), (true, 0, false)]
        {
            assert!(!wants_registry_fix(delivered, state, from_file), "({delivered}, {state}, {from_file})");
        }
    }

    /// wsD D2: StuckRects3 is opened under HKEY_USERS by the SID of Explorer's user, never under HKEY_CURRENT_USER.
    #[test]
    fn stuckrects_subkey_is_under_the_sid() {
        assert_eq!(
            stuckrects_subkey("S-1-5-21-1-2-3-1001"),
            r"S-1-5-21-1-2-3-1001\Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3"
        );
    }

    /// r151 N1: of the restores from outside, only the watchdog's hang restore is undone: not exiting, no stop request
    /// (--restore, --install, --uninstall), hiding still wanted and the watchdog process still running. All 16 rows.
    #[test]
    fn only_the_watchdogs_restore_is_undone() {
        let mut undone = 0;
        for row in 0u8..16 {
            let (exiting, stop, wanted, alive) = (row & 8 != 0, row & 4 != 0, row & 2 != 0, row & 1 != 0);
            let got = after_outside_restore(exiting, stop, wanted, alive);
            if (exiting, stop, wanted, alive) == (false, false, true, true) {
                assert_eq!(got, Outside::HideAgain);
                undone += 1;
            } else {
                assert_eq!(
                    got,
                    Outside::StopHiding,
                    "exiting {exiting}, stop {stop}, wanted {wanted}, watchdog {alive}"
                );
            }
        }
        assert_eq!(undone, 1, "one row of 16");
        assert_eq!(after_outside_restore(false, false, true, false), Outside::StopHiding, "no watchdog: never again");
    }

    /// r151 N1: the re-hide hides only with a state Explorer answered; an unanswered read is never saved as the
    /// original and is tried again at the next safety check, its line logged only for the first one after a restore.
    #[test]
    fn an_unanswered_read_is_tried_again() {
        assert_eq!(after_rehide_read(Some(ABS_AUTOHIDE), false), Reread::Hide(ABS_AUTOHIDE));
        assert_eq!(after_rehide_read(Some(0), true), Reread::Hide(0), "a 0 Explorer answered is the user's");
        assert_eq!(after_rehide_read(None, false), Reread::Retry { first: true });
        assert_eq!(after_rehide_read(None, true), Reread::Retry { first: false }, "not logged every 3 s");
    }

    /// wsC Q7: the safety check's line is logged for the first 20 hits of a process, not every 3 s.
    #[test]
    fn the_safety_line_is_capped() {
        assert!(safety_logs(0));
        assert!(safety_logs(19));
        assert!(!safety_logs(20));
    }

    /// Only a timed-out direct send on the UI thread skips SHAppBarMessage (review of 7232226: its forward through the
    /// dock's tray host restarts its 3 s at each icon apps send the host); a refused send and other threads keep it.
    #[test]
    fn a_timed_out_appbar_send_has_no_fallback_on_the_ui_thread() {
        use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_WINDOW_HANDLE};
        assert!(!takes_fallback(ERROR_TIMEOUT, true));
        assert!(takes_fallback(ERROR_TIMEOUT, false), "the watchdog and --restore keep it");
        for refused in [ERROR_INVALID_WINDOW_HANDLE, ERROR_ACCESS_DENIED, WIN32_ERROR(0)] {
            assert!(takes_fallback(refused, true) && takes_fallback(refused, false), "{refused:?}");
        }
        assert!(!crate::on_ui_thread(), "a test thread is not the dock's");
    }

    /// The direct send tells a timeout from a refusal, which `takes_fallback` reads: a second thread's message-only
    /// window that never pumps times out (well under 1 s for a 50 ms timeout, SMTO_BLOCK), a destroyed window refuses.
    /// Headless: message-only windows are never shown and never take the focus.
    #[test]
    fn a_direct_appbar_send_reports_its_timeout() {
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        };
        let message_only = || {
            // SAFETY: a message-only STATIC window of the calling thread, which destroys it before it ends.
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .expect("a message-only window")
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (done, wait_done) = std::sync::mpsc::channel::<()>();
        let silent = std::thread::spawn(move || {
            let h = message_only();
            tx.send(h.0 as isize).expect("the test thread waits");
            let _ = wait_done.recv_timeout(Duration::from_secs(4));
            // SAFETY: the window of this thread.
            let _ = unsafe { DestroyWindow(h) };
        });
        let target = HWND(rx.recv().expect("the silent thread starts") as *mut c_void);
        let t = Instant::now();
        let slow = appbar_direct_at(ABM_GETSTATE, target, 0, [0; 4], 0, 50);
        let waited = t.elapsed();
        let _ = done.send(());
        silent.join().expect("the silent thread ends");
        assert_eq!(slow, Err(ERROR_TIMEOUT));
        assert!(waited < Duration::from_secs(1), "the 50 ms send took {} ms", waited.as_millis());
        let gone = message_only();
        // SAFETY: the window of this thread.
        unsafe { DestroyWindow(gone) }.expect("the window is destroyed");
        let refused = appbar_direct(ABM_GETSTATE, gone, 0).expect_err("a destroyed window answers nothing");
        assert!(takes_fallback(refused, true), "{refused:?}");
    }

    #[test]
    fn raw_taskbar_events_filtered_as_before() {
        use windows::Win32::UI::WindowsAndMessaging::{
            EVENT_OBJECT_CLOAKED, EVENT_OBJECT_HIDE, EVENT_SYSTEM_FOREGROUND, OBJID_CURSOR,
        };
        use RawKind::*;
        let w = OBJID_WINDOW.0;
        for tracked in [false, true] {
            assert_eq!(raw_event_kind(EVENT_OBJECT_SHOW, w, tracked), CheckClass);
            assert_eq!(raw_event_kind(EVENT_OBJECT_CREATE, w, tracked), CheckClass);
            assert_eq!(raw_event_kind(EVENT_OBJECT_HIDE, w, tracked), Ignore);
            assert_eq!(raw_event_kind(EVENT_OBJECT_SHOW, OBJID_CURSOR.0, tracked), Ignore);
            assert_eq!(raw_event_kind(EVENT_OBJECT_DESTROY, OBJID_CURSOR.0, tracked), Ignore);
            assert_eq!(raw_event_kind(EVENT_SYSTEM_FOREGROUND, w, tracked), Ignore);
            assert_eq!(raw_event_kind(EVENT_OBJECT_CLOAKED, w, tracked), Ignore);
        }
        assert_eq!(raw_event_kind(EVENT_OBJECT_DESTROY, w, true), Post);
        assert_eq!(raw_event_kind(EVENT_OBJECT_DESTROY, w, false), Ignore);
    }

    #[test]
    fn edge_holder_bookkeeping() {
        use DestroyAct::*;
        assert_eq!(destroy_act(0, 0, 0), Ignore, "a null window is never news");
        assert_eq!(destroy_act(0, 5, 7), Ignore);
        assert_eq!(destroy_act(5, 5, 7), ExplorerGone);
        assert_eq!(destroy_act(7, 5, 7), HolderGone);
        assert_eq!(destroy_act(9, 5, 7), Ignore);
        assert_eq!(destroy_act(5, 5, 5), ExplorerGone, "Explorer's tray wins over the holder");
        assert_eq!(destroy_act(7, 0, 0), Ignore, "nothing tracked");
        assert!(hold_back_is_news(0, 7));
        assert!(hold_back_is_news(5, 7), "a different holder is news");
        assert!(!hold_back_is_news(7, 7), "the same holder is logged once");
    }

    #[test]
    fn a_holder_that_lets_go_after_its_destroy_event_is_rechecked() {
        let calls = std::cell::Cell::new(0);
        let alive = |up: bool| {
            let calls = &calls;
            move |_: isize| {
                calls.set(calls.get() + 1);
                up
            }
        };
        assert!(!holder_left(0, alive(false)), "no hold-back in effect: nothing to ask");
        assert_eq!(calls.get(), 0, "without a hold-back the check costs no call");
        assert!(!holder_left(7, alive(true)), "a holder that is still a window keeps the request held back");
        assert!(holder_left(7, alive(false)), "a holder that is no longer a window: ask again");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn fallback_confirms_only_the_auto_hide_bit() {
        for (r, w) in [(0, 2), (2, 2), (0, 0), (1, 1), (3, 1)] {
            assert!(state_reads_as(r, w), "{r} {w}");
        }
        for (r, w) in [(1, 0), (0, 1), (1, 2)] {
            assert!(!state_reads_as(r, w), "{r} {w}");
        }
    }

    #[test]
    fn state_parsing_masks_and_rejects() {
        assert_eq!(parse_state(b"2"), Some(ABS_ALWAYSONTOP));
        assert_eq!(parse_state(b" 3\r\n"), Some(ABS_AUTOHIDE | ABS_ALWAYSONTOP));
        // Unknown bits from a (medium-writable) legacy file are masked away.
        assert_eq!(parse_state(b"4294967295"), Some(ABS_MASK));
        assert_eq!(parse_state(b"autohide"), None);
        assert_eq!(parse_state(b""), None);
        assert_eq!(parse_state(&[0xFF, 0xFE]), None);
    }

    #[test]
    fn legacy_state_ignored_without_data_folder() {
        // Unit tests never initialize the data folder: nothing legacy is read or recorded.
        assert_eq!(read_legacy_state(), None);
        assert!(legacy_marker_path().is_none());
    }

    #[test]
    fn pause_state_machine() {
        use PauseStep::*;
        let max = HOST_RESUME_MAX_MS;
        assert_eq!(pause_step(1000, 1500, false, false), Wait, "desktop not registered yet");
        assert_eq!(pause_step(1000, 1500, true, false), Resume);
        assert_eq!(pause_step(1000, 1000 + max - 1, false, false), Wait);
        assert_eq!(pause_step(1000, 1000 + max, false, false), Resume, "stop waiting, start anyway");
        assert_eq!(pause_step(1000, 1500, true, true), GiveUp, "exit began: never start a host");
        assert_eq!(pause_step(1000, 1500, false, true), GiveUp);
    }

    #[test]
    fn taskbar_edge_from_rect() {
        let r = |left, top, right, bottom| RECT { left, top, right, bottom };
        let mon = r(0, 0, 2560, 1440);
        assert_eq!(edge_of(r(0, 1392, 2560, 1440), mon), ABE_BOTTOM);
        assert_eq!(edge_of(r(0, 0, 2560, 48), mon), ABE_TOP);
        assert_eq!(edge_of(r(0, 0, 62, 1440), mon), ABE_LEFT);
        assert_eq!(edge_of(r(2498, 0, 2560, 1440), mon), ABE_RIGHT);
        // Secondary monitor with a negative / offset origin.
        let side = r(3840, 287, 6400, 1727);
        assert_eq!(edge_of(r(3840, 1679, 6400, 1727), side), ABE_BOTTOM);
        assert_eq!(edge_of(r(3840, 287, 6400, 335), side), ABE_TOP);
        assert_eq!(edge_of(r(-62, 0, 0, 1080), r(-1920, 0, 0, 1080)), ABE_RIGHT);
    }

    #[test]
    fn edge_owner_reply_decoding() {
        let tray = 0x1_0040isize;
        let live = |h: isize| h != 0xDEAD;
        assert_eq!(edge_owner_from_reply(None, tray, live), EdgeOwner::NoAnswer);
        assert_eq!(edge_owner_from_reply(Some(0), tray, live), EdgeOwner::Free);
        assert_eq!(edge_owner_from_reply(Some(0x1_0040), tray, live), EdgeOwner::Free, "the taskbar itself");
        assert_eq!(edge_owner_from_reply(Some(0xDEAD), tray, live), EdgeOwner::Free, "a dead handle");
        assert_eq!(edge_owner_from_reply(Some(0x2_0080), tray, live), EdgeOwner::Held(0x2_0080));
        // A handle with the high bit set comes back sign-extended, whichever form the reply has, and it is
        // that value that is checked for being a window and compared with the taskbar.
        let high = 0x8001_0234u32 as i32 as isize;
        let only_high = move |h: isize| h == high;
        assert_eq!(edge_owner_from_reply(Some(0x8001_0234), tray, only_high), EdgeOwner::Held(high));
        assert_eq!(edge_owner_from_reply(Some(high as usize), tray, only_high), EdgeOwner::Held(high));
        assert_eq!(edge_owner_from_reply(Some(0x8001_0234), high, only_high), EdgeOwner::Free, "the taskbar itself");
    }

    #[test]
    fn autohide_action_for_every_state_and_owner() {
        use AutoHideAction::*;
        use EdgeOwner::{Free, Held, NoAnswer};
        let held = Held(0x2_0080);
        let cases = [
            (0, Free, Send),
            (0, held, HoldBack(0x2_0080)),
            (0, NoAnswer, Send),
            (ABS_ALWAYSONTOP, Free, Send),
            (ABS_ALWAYSONTOP, held, HoldBack(0x2_0080)),
            (ABS_ALWAYSONTOP, NoAnswer, Send),
            (ABS_AUTOHIDE, Free, Nothing),
            (ABS_AUTOHIDE, held, Nothing),
            (ABS_AUTOHIDE, NoAnswer, Nothing),
            (ABS_MASK, Free, Nothing),
            (ABS_MASK, held, Nothing),
            (ABS_MASK, NoAnswer, Nothing),
        ];
        for (state, owner, want) in cases {
            assert_eq!(autohide_action(state, owner), want, "state {state}, {owner:?}");
        }
    }

    #[test]
    fn docked_bar_lies_inside_its_monitor() {
        let r = |left, top, right, bottom| RECT { left, top, right, bottom };
        let mon = r(0, 0, 2560, 1440);
        assert!(docked_inside(r(0, 1392, 2560, 1440), mon));
        assert!(docked_inside(r(0, 0, 62, 1440), mon));
        assert!(docked_inside(mon, mon), "touching every side is still inside");
        // Auto-hidden or sliding: partly off its monitor.
        assert!(!docked_inside(r(0, 1438, 2560, 1486), mon));
        assert!(!docked_inside(r(0, -46, 2560, 2), mon));
        // On another monitor (mid display change), empty or inverted.
        assert!(!docked_inside(r(2560, 1392, 5120, 1440), mon));
        assert!(!docked_inside(r(0, 1440, 2560, 1440), mon), "empty");
        assert!(!docked_inside(r(0, 0, 0, 0), mon), "empty");
        assert!(!docked_inside(r(10, 10, 5, 20), mon), "inverted");
        // Monitors with an offset or negative origin.
        let side = r(3840, 287, 6400, 1727);
        assert!(docked_inside(r(3840, 1679, 6400, 1727), side));
        assert!(!docked_inside(r(3840, 1679, 6400, 1727), mon));
        assert!(docked_inside(r(-62, 0, 0, 1080), r(-1920, 0, 0, 1080)));
    }

    #[test]
    fn send_line_tells_why_the_edge_was_not_asked() {
        let r = |left, top, right, bottom| RECT { left, top, right, bottom };
        let mon = r(0, 0, 2560, 1440);
        assert_eq!(edge_to_ask(r(0, 1392, 2560, 1440), mon), Ok((ABE_BOTTOM, mon)));
        assert_eq!(edge_to_ask(r(0, 0, 62, 1440), mon), Ok((ABE_LEFT, mon)));
        let slid_off = edge_to_ask(r(0, 1438, 2560, 1486), mon);
        assert_eq!(
            slid_off.as_ref().unwrap_err(),
            "taskbar (0, 1438)-(2560, 1486) not inside monitor (0, 0)-(2560, 1440)"
        );
        assert_eq!(rect_text(r(-1920, -8, 0, 1080)), "(-1920, -8)-(0, 1080)");
        // Only an answered question that found the edge free adds nothing to the send line.
        let asked = Ok((ABE_BOTTOM, mon));
        assert_eq!(edge_check_text(EdgeOwner::Free, &asked), "");
        assert_eq!(edge_check_text(EdgeOwner::NoAnswer, &asked), " (edge check unanswered)");
        assert_eq!(
            edge_check_text(EdgeOwner::Free, &slid_off),
            " (edge not asked: taskbar (0, 1438)-(2560, 1486) not inside monitor (0, 0)-(2560, 1440))"
        );
        let not_primary = Err("first taskbar is not Shell_TrayWnd".to_string());
        assert_eq!(
            edge_check_text(EdgeOwner::Free, &not_primary),
            " (edge not asked: first taskbar is not Shell_TrayWnd)"
        );
        let no_rect = Err("GetWindowRect failed".to_string());
        assert_eq!(edge_check_text(EdgeOwner::NoAnswer, &no_rect), " (edge not asked: GetWindowRect failed)");
    }

    #[test]
    fn edge_is_asked_only_after_a_direct_state_read() {
        use EdgeOwner::{Free, NoAnswer};
        let not = |owner, why: &str| Some((owner, why.to_string()));
        assert_eq!(edge_not_asked(0, true, true), None);
        assert_eq!(edge_not_asked(ABS_ALWAYSONTOP, true, true), None);
        // The direct ABM_GETSTATE send failed: Explorer is slow, the request is sent unasked (fail open, as 1.4.2).
        assert_eq!(edge_not_asked(0, true, false), not(NoAnswer, "state read failed"));
        assert_eq!(edge_not_asked(ABS_ALWAYSONTOP, true, false), not(NoAnswer, "state read failed"));
        assert_eq!(edge_not_asked(ABS_AUTOHIDE, true, true), not(Free, "auto-hide on"));
        assert_eq!(edge_not_asked(ABS_AUTOHIDE, true, false), not(Free, "auto-hide on"));
        assert_eq!(edge_not_asked(ABS_MASK, true, false), not(Free, "auto-hide on"));
        assert_eq!(edge_not_asked(0, false, true), not(Free, "first taskbar is not Shell_TrayWnd"));
        assert_eq!(edge_not_asked(0, false, false), not(Free, "first taskbar is not Shell_TrayWnd"));
        // Whatever the reason, the decision is the one of 1171772 for that owner: a failed read still sends.
        for (state, primary, answered) in [(0, true, false), (ABS_ALWAYSONTOP, true, false), (0, false, false)] {
            let (owner, _) = edge_not_asked(state, primary, answered).unwrap();
            assert_eq!(autohide_action(state, owner), AutoHideAction::Send, "state {state}, {primary}, {answered}");
        }
        let failed = Err("state read failed".to_string());
        assert_eq!(edge_check_text(NoAnswer, &failed), " (edge not asked: state read failed)");
    }

    #[test]
    fn state_reads_are_logged_on_change_only() {
        let line = |prev, read| state_change_line(prev, read);
        assert_eq!(line(STATE_UNREAD, Some(2)).as_deref(), Some("taskbar: auto-hide state 2 (first read)"));
        assert_eq!(line(2, Some(2)), None, "an unchanged state is not logged again");
        assert_eq!(line(3, Some(2)).as_deref(), Some("taskbar: auto-hide state 3 -> 2"));
        assert_eq!(line(0, Some(1)).as_deref(), Some("taskbar: auto-hide state 0 -> 1"));
        assert_eq!(line(2, None).as_deref(), Some("taskbar: auto-hide state unknown (read failed)"));
        assert_eq!(line(STATE_UNREAD, None).as_deref(), Some("taskbar: auto-hide state unknown (read failed)"));
        assert_eq!(line(STATE_UNKNOWN, None), None, "a lasting read failure is logged once");
        assert_eq!(line(STATE_UNKNOWN, Some(3)).as_deref(), Some("taskbar: auto-hide state unknown -> 3"));
        // The text of the hold-back and send lines: never "state 0" for a failed read.
        assert_eq!(autohide_off_text(Some(0)), "auto-hide off (state 0)");
        assert_eq!(autohide_off_text(Some(ABS_ALWAYSONTOP)), "auto-hide off (state 2)");
        assert_eq!(autohide_off_text(None), "auto-hide state unknown (read failed)");
        let edges = [ABE_LEFT, ABE_TOP, ABE_RIGHT, ABE_BOTTOM, 7].map(edge_name);
        assert_eq!(edges, ["left", "top", "right", "bottom", "unknown"]);
    }
}
