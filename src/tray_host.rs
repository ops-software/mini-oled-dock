//! Tray host: the notification-area icons of OTHER apps, hosted in the dock.
//! Code written from scratch; lightbar/ManagedShell/RetroBar (MIT) only as inspiration.
//!
//! THREAD
//!  * `tray-host`: HIDDEN top-level window of class `Shell_TrayWnd` (WS_POPUP, WS_EX_TOPMOST |
//!    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, never visible), property `DockTrayHost`,
//!    `ChangeWindowMessageFilterEx(WM_COPYDATA | WM_COMMAND | DockTrayYield, MSGFLT_ALLOW)`, child
//!    `TrayNotifyWnd`, copy of `TaskbandHWND` from Explorer. Receives `WM_COPYDATA` (dwData 1 = NIM_*,
//!    0 = AppBar forwarded to Explorer except GETTASKBARPOS, 3 = icon rect). 1 s timer (stays
//!    first in the Z-order unless `YIELDED`), 5 s timer (dead owners).
//!  * `tray-fwd`: queue (max 256) of NIM_* copies forwarded to Explorer with
//!    `SendMessageTimeoutW(SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT, 2000)`, only if `taskbar::is_hidden()`.
//!    Icon state lives in a `Mutex` with short locks (never held during SendMessage* or bitmap conversions). The UI
//!    reads `snapshot()` when `WM_DOCK_TRAY_CHANGED` arrives. UI calls (`click`, `hover`,
//!    `set_*`) do not block: `SendNotifyMessageW` / `PostMessageW`.
//!
//! RECOVERY: `yield_now()` (any thread, asynchronous) sends the window to the bottom and stops
//! repositioning; `stop()` destroys it. Both run BEFORE the restore's `TaskbarCreated` broadcast
//! (see `taskbar::restore_force`), so apps re-register their icons with Explorer.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_SUCCESS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, RECT, UNICODE_STRING, WPARAM,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{GlobalGetAtomNameW, COPYDATASTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, SECTION_MAP_READ,
    SECTION_MAP_WRITE, SECTION_QUERY,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE,
    KEY_WOW64_64KEY, REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD, RRF_SUBKEY_WOW6464KEY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SHAllocShared, SHFreeShared, SHLockShared, SHUnlockShared};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CopyIcon, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyWindow, DispatchMessageW, EnumWindows, FindWindowExW, FindWindowW, GetMessageW, GetPropW, GetWindowLongW,
    GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SendMessageTimeoutW, SendNotifyMessageW, SetPropW, SetTimer,
    SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW, ASFW_ANY, CS_DBLCLKS, GWL_STYLE, HICON, HWND_BOTTOM,
    HWND_TOPMOST, MSG, MSGFLT_ALLOW, SMTO_ABORTIFHUNG, SMTO_ERRORONEXIT, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, WINDOWPOS, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CONTEXTMENU, WM_COPYDATA, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_USER, WM_WINDOWPOSCHANGED, WNDCLASSEXW,
    WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use crate::icons::{self, IconBitmap};
use crate::log;
use crate::msg::WM_DOCK_TRAY_CHANGED;

/// Window property that marks OUR `Shell_TrayWnd` (also read by the watchdog process and
/// by the `--restore` CLI through `taskbar::is_dock_tray_host`).
pub const PROP_DOCK_TRAY_HOST: PCWSTR = w!("DockTrayHost");
/// Registered message (`RegisterWindowMessageW`) that another dock process (watchdog/`--restore`)
/// sends to our host to make it yield: `yield_now()` + `DestroyWindow`.
pub const YIELD_MSG_NAME: PCWSTR = w!("DockTrayYield");

/// A tray icon to draw (only visible icons, not hidden by `config.tray_hidden_guids`, in
/// registration order).
#[derive(Clone, Debug)]
pub struct TrayView {
    /// Stable id, never reused (goes into `render::ItemId::Tray`).
    pub id: u64,
    /// Premultiplied bitmap ALREADY scaled to `set_icon_px` (None = conversion failed: do not draw).
    pub bitmap: Option<Arc<IconBitmap>>,
    /// Stable key for the `tray_hidden` config (see `item_key`).
    pub key: String,
    /// Short app name (the exe's FileDescription; fallback: first tooltip line, exe name).
    pub name: String,
    /// true if the user chose not to show the icon in the bar (`config.tray_hidden`).
    pub user_hidden: bool,
}

/// Dock geometry as seen by the apps (screen coordinates, physical px).
#[derive(Clone, Copy, Debug, Default)]
pub struct TrayGeometry {
    /// Rectangle of the SHOWN bar.
    pub bar: RECT,
    /// Rectangle of the tray area within the bar (for `ABM_GETTASKBARPOS` and for rect requests
    /// of icons not laid out yet).
    pub tray_area: RECT,
    /// `ABE_BOTTOM` (3) o `ABE_TOP` (1).
    pub edge: u32,
}

/// Kind of click forwarded to the owning app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayClick {
    Left,
    Right,
    Middle,
    /// Second left click within `GetDoubleClickTime` on the same icon (the first one was already
    /// sent as `Left`).
    Double,
}

// ------------------------------------------------------------------ wire format

/// Signature of `SHELLTRAYDATA` / rect requests.
const TRAY_SIGNATURE: u32 = 0x3475_3423;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETFOCUS: u32 = 3;
const NIM_SETVERSION: u32 = 4;

const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x1;

const NIN_SELECT: u32 = 0x400;

const ABM_QUERYPOS: u32 = 2;
const ABM_SETPOS: u32 = 3;
const ABM_GETTASKBARPOS: u32 = 5;

/// 32-bit-handle `NOTIFYICONDATA`, as it travels inside `WM_COPYDATA`.
/// Layout adapted from lightbar (MIT, Copyright (c) 2026 SauerExe), `src/tray.rs`; see THIRD_PARTY_NOTICES.md.
#[repr(C)]
#[derive(Clone, Copy)]
struct NotifyIconData32 {
    cb_size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback: u32,
    hicon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: GUID,
    balloon_icon: u32,
}

/// `SHELLTRAYDATA`: header + `NotifyIconData32`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ShellTrayData {
    signature: u32,
    message: u32,
    nid: NotifyIconData32,
}

/// 32-bit-handle `APPBARDATA`.
/// Adapted from lightbar (MIT, Copyright (c) 2026 SauerExe), `src/bar.rs`; see THIRD_PARTY_NOTICES.md.
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

/// `APPBARMSGDATAV3` (64 bytes). Adapted from lightbar (MIT), like `AppBarData32`.
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

const _: () = assert!(
    std::mem::size_of::<NotifyIconData32>() == 956
        && std::mem::size_of::<ShellTrayData>() == 964
        && std::mem::offset_of!(NotifyIconData32, tip) == 24
        && std::mem::offset_of!(NotifyIconData32, info) == 288
        && std::mem::offset_of!(NotifyIconData32, version) == 800
        && std::mem::offset_of!(NotifyIconData32, guid) == 936
        && std::mem::size_of::<AppBarData32>() == 0x28
        && std::mem::size_of::<AppBarMsg>() == 0x40
        && std::mem::offset_of!(AppBarMsg, message) == 40
        && std::mem::offset_of!(AppBarMsg, shared) == 48
        && std::mem::offset_of!(AppBarMsg, pid) == 56
);

/// Copies at most `bytes.len()` bytes into a `T` struct; the rest stays zero (old senders send
/// shorter structs). `T` must be a `repr(C)` type made only of integers/integer arrays/GUIDs.
fn from_prefix<T: Copy>(bytes: &[u8]) -> T {
    // SAFETY: for the only `T`s used here (repr(C) structs of integers and GUIDs) the all-zero bit pattern
    // is a valid value.
    let mut v: T = unsafe { std::mem::zeroed() };
    let n = bytes.len().min(std::mem::size_of::<T>());
    // SAFETY: copies `n <= size_of::<T>()` bytes from `bytes` (at least `n` long) into `v`, which does not
    // overlap the slice; every bit pattern is valid for `T`.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (&mut v as *mut T).cast::<u8>(), n) };
    v
}

/// Decodes a `WM_COPYDATA` with `dwData == 1`; `None` if too short or with a wrong signature.
fn parse_shell_tray(bytes: &[u8]) -> Option<ShellTrayData> {
    if bytes.len() < 8 + 24 {
        return None;
    }
    let d: ShellTrayData = from_prefix(bytes);
    (d.signature == TRAY_SIGNATURE).then_some(d)
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).unwrap_or(0)
}

/// Sign-extends a 32-bit handle.
fn ext(h: u32) -> isize {
    h as i32 as isize
}

fn hwnd_of(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn guid_is_zero(g: &GUID) -> bool {
    *g == GUID::zeroed()
}

/// Lowercase GUID without braces (format of `config.tray_hidden_guids`).
fn guid_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

/// UTF-16 string up to the first NUL (the buffer may not be terminated).
fn utf16_until_nul(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

// ------------------------------------------------------------------ state

struct TrayItem {
    id: u64,
    owner: isize,
    pid: u32,
    uid: u32,
    guid: Option<GUID>,
    callback: u32,
    version: u32,
    hidden: bool,
    tip: String,
    /// Native-size source (needed to rescale on DPI change).
    src: Option<Arc<IconBitmap>>,
    /// Version scaled to `ICON_PX`.
    bitmap: Option<Arc<IconBitmap>>,
    /// Owner's exe name (diagnostics and `tray_hidden` key).
    exe: String,
    /// Full image path of the owner process when the icon was registered (empty if unknown); an
    /// elevated owner is trusted only while its process still runs this image.
    image: String,
    /// Short app name (FileDescription), empty if unavailable.
    name: String,
}

struct TrayState {
    items: Vec<TrayItem>,
    next_id: u64,
    geom: TrayGeometry,
    rects: Vec<(u64, RECT)>,
}

const EMPTY_RECT: RECT = RECT { left: 0, top: 0, right: 0, bottom: 0 };

static TRAY: Mutex<TrayState> = Mutex::new(TrayState {
    items: Vec::new(),
    next_id: 1,
    geom: TrayGeometry { bar: EMPTY_RECT, tray_area: EMPTY_RECT, edge: 3 },
    rects: Vec::new(),
});

/// Tray area to show to the apps; if the UI has not reported it yet (empty rect) the bar is used.
fn area_rect(g: &TrayGeometry) -> RECT {
    let a = g.tray_area;
    if a.right > a.left && a.bottom > a.top {
        a
    } else {
        g.bar
    }
}

fn state() -> MutexGuard<'static, TrayState> {
    TRAY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Raw HWND of the host window (0 = absent).
static HOST_HWND: AtomicIsize = AtomicIsize::new(0);
/// Raw HWND of the dock's UI window.
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
/// true from the creation of the host window until its destruction.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Permanent flag: the host has yielded to Explorer.
static YIELDED: AtomicBool = AtomicBool::new(false);
/// Side in physical px to scale the bitmaps to.
static ICON_PX: AtomicU32 = AtomicU32::new(16);
/// Id of the registered message `DockTrayYield` (0 = not registered yet).
static YIELD_MSG: AtomicU32 = AtomicU32::new(0);

/// Module threads.
type ThreadPair = (Option<JoinHandle<()>>, Option<JoinHandle<()>>);
static THREADS: Mutex<ThreadPair> = Mutex::new((None, None));

/// Forwarding queue towards Explorer: (wParam, original bytes).
static FWD_QUEUE: Mutex<VecDeque<(usize, Vec<u8>)>> = Mutex::new(VecDeque::new());
static FWD_CV: Condvar = Condvar::new();
static FWD_STOP: AtomicBool = AtomicBool::new(false);
static FWD_OVERFLOW_LOGGED: AtomicBool = AtomicBool::new(false);
const FWD_QUEUE_MAX: usize = 256;

/// Total cap on hosted tray icons and cap per owning process.
const MAX_ITEMS: usize = 256;
const MAX_ITEMS_PER_PID: usize = 32;
static CAP_LOGGED: AtomicBool = AtomicBool::new(false);

// Private messages of the host window.
const WM_HOST_RESIZE: u32 = WM_APP + 1;
const WM_HOST_RAISE: u32 = WM_APP + 2;
const WM_HOST_RESCALE: u32 = WM_APP + 3;

const TIMER_ZORDER: usize = 1;
const TIMER_DEAD: usize = 2;

fn yield_msg_id() -> u32 {
    let cur = YIELD_MSG.load(Relaxed);
    if cur != 0 {
        return cur;
    }
    // SAFETY: static NUL-terminated string literal.
    let id = unsafe { RegisterWindowMessageW(YIELD_MSG_NAME) };
    if id == 0 {
        log!("tray: RegisterWindowMessageW(DockTrayYield) failed: {:?}", windows::core::Error::from_thread());
    }
    YIELD_MSG.store(id, Relaxed);
    id
}

fn post_ui(kind: usize) {
    let ui = UI_HWND.load(Relaxed);
    if ui == 0 {
        return;
    }
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it merely fails.
    if let Err(e) = unsafe { PostMessageW(Some(hwnd_of(ui)), WM_DOCK_TRAY_CHANGED, WPARAM(kind), LPARAM(0)) } {
        log!("tray: PostMessageW(WM_DOCK_TRAY_CHANGED) failed: {e}");
    }
}

fn post_host(msg: u32) {
    let h = HOST_HWND.load(Relaxed);
    if h == 0 {
        return;
    }
    // SAFETY: PostMessageW is thread-safe; if the window is gone it merely fails.
    let _ = unsafe { PostMessageW(Some(hwnd_of(h)), msg, WPARAM(0), LPARAM(0)) };
}

// ------------------------------------------------------------------ pure NIM_* logic

/// What to do with the icon on a `NIM_ADD`/`NIM_MODIFY` with `NIF_ICON`.
enum IconUpdate {
    Keep,
    Clear,
    /// (source, scaled)
    Set(Arc<IconBitmap>, Arc<IconBitmap>),
}

struct NimResult {
    /// Return value of the `WM_COPYDATA` (true = 1).
    ret: bool,
    /// 0 nothing, 1 bitmap/tooltip only, 2 the set of visible icons changes.
    change: usize,
}

fn is_visible_item(t: &TrayItem) -> bool {
    !t.hidden && t.bitmap.is_some()
}

fn visible_ids(items: &[TrayItem]) -> Vec<u64> {
    items.iter().filter(|t| is_visible_item(t)).map(|t| t.id).collect()
}

/// Identity: GUID if present, otherwise `(hWnd, uID)`.
fn find_item(items: &[TrayItem], owner: isize, uid: u32, guid: Option<GUID>) -> Option<usize> {
    match guid {
        Some(g) => items.iter().position(|t| t.guid == Some(g)),
        None => items.iter().position(|t| t.owner == owner && t.uid == uid),
    }
}

/// Applies the fields indicated by the flags; returns true if the bitmap or tooltip changed.
fn apply_fields(t: &mut TrayItem, nid: &NotifyIconData32, icon: IconUpdate) -> bool {
    let mut changed = false;
    if nid.flags & NIF_MESSAGE != 0 {
        t.callback = nid.callback;
    }
    if nid.flags & NIF_STATE != 0 && nid.state_mask & NIS_HIDDEN != 0 {
        t.hidden = nid.state & NIS_HIDDEN != 0;
    }
    if nid.flags & NIF_TIP != 0 {
        let tip = utf16_until_nul(&nid.tip);
        if tip != t.tip {
            t.tip = tip;
            changed = true;
        }
    }
    match icon {
        IconUpdate::Keep => {}
        IconUpdate::Clear => {
            changed |= t.bitmap.is_some();
            t.src = None;
            t.bitmap = None;
        }
        IconUpdate::Set(src, scaled) => {
            t.src = Some(src);
            t.bitmap = Some(scaled);
            changed = true;
        }
    }
    changed
}

/// Core of `dwData == 1` (pure, tested). `owner_pid` = owner's pid if the window exists; `image` = full
/// image path of that process (read for `NIM_ADD` only, empty if unknown).
fn apply_nim(
    items: &mut Vec<TrayItem>,
    next_id: &mut u64,
    d: &ShellTrayData,
    owner_pid: Option<u32>,
    icon: IconUpdate,
    image: &str,
) -> NimResult {
    let exe = file_name_of(image);
    let nid = &d.nid;
    let owner = ext(nid.hwnd);
    let guid = (nid.flags & NIF_GUID != 0 && !guid_is_zero(&nid.guid)).then_some(nid.guid);
    let before = visible_ids(items);
    let idx = find_item(items, owner, nid.uid, guid);
    let mut touched = false;
    let ret = match d.message {
        NIM_ADD => match (owner_pid, idx) {
            (None, _) => false,
            // Same GUID but different owner (app restarted without DELETE): replace the owner.
            (Some(pid), Some(i)) if guid.is_some() && items[i].owner != owner => {
                let t = &mut items[i];
                t.owner = owner;
                t.pid = pid;
                t.uid = nid.uid;
                t.exe = exe;
                t.image = image.to_string();
                touched = apply_fields(t, nid, icon);
                true
            }
            (Some(_), Some(_)) => false,
            // Caps: each icon holds two bitmaps in the elevated process; a sender looping over
            // different uIDs must not make memory grow without bound.
            (Some(pid), None)
                if items.len() >= MAX_ITEMS || items.iter().filter(|t| t.pid == pid).count() >= MAX_ITEMS_PER_PID =>
            {
                if !CAP_LOGGED.swap(true, Relaxed) {
                    log!("tray: icon limit reached ({MAX_ITEMS} total / {MAX_ITEMS_PER_PID} per process), NIM_ADD rejected");
                }
                false
            }
            (Some(pid), None) => {
                let mut t = TrayItem {
                    id: *next_id,
                    owner,
                    pid,
                    uid: nid.uid,
                    guid,
                    callback: 0,
                    version: 0,
                    hidden: false,
                    tip: String::new(),
                    src: None,
                    bitmap: None,
                    exe,
                    image: image.to_string(),
                    name: String::new(),
                };
                *next_id += 1;
                touched = apply_fields(&mut t, nid, icon);
                items.push(t);
                true
            }
        },
        NIM_MODIFY => match idx {
            Some(i) => {
                touched = apply_fields(&mut items[i], nid, icon);
                true
            }
            None => false,
        },
        NIM_DELETE => match idx {
            Some(i) => {
                items.remove(i);
                true
            }
            None => false,
        },
        NIM_SETFOCUS => true,
        NIM_SETVERSION => match idx {
            Some(i) if nid.version <= 4 => {
                items[i].version = nid.version;
                true
            }
            _ => false,
        },
        _ => idx.is_some(),
    };
    let change = if visible_ids(items) != before {
        2
    } else if touched {
        1
    } else {
        0
    };
    NimResult { ret, change }
}

// ------------------------------------------------------------------ callbacks to the apps

/// Encodes (wParam, lParam) for the icon version. Adapted from lightbar (MIT, `src/tray.rs`). In v4 the coordinates are signed 16
/// bit (the receiver sign-extends them again with `GET_X_LPARAM`).
fn encode(version: u32, uid: u32, ax: i32, ay: i32, ev: u32) -> (usize, isize) {
    if version >= 4 {
        let wp = (ax as u16 as usize) | ((ay as u16 as usize) << 16);
        let lp = ((ev & 0xFFFF) as isize) | (((uid & 0xFFFF) as isize) << 16);
        (wp, lp)
    } else {
        (uid as usize, ev as isize)
    }
}

/// Sequence of events sent for a click, as Explorer does.
fn click_events(kind: TrayClick, version: u32) -> Vec<u32> {
    let new_style = version >= 3;
    let mut v = match kind {
        TrayClick::Left => vec![WM_LBUTTONDOWN, WM_LBUTTONUP],
        TrayClick::Right => vec![WM_RBUTTONDOWN, WM_RBUTTONUP],
        TrayClick::Middle => vec![WM_MBUTTONDOWN, WM_MBUTTONUP],
        TrayClick::Double => vec![WM_LBUTTONDBLCLK, WM_LBUTTONUP],
    };
    if new_style {
        match kind {
            TrayClick::Left | TrayClick::Double => v.push(NIN_SELECT),
            TrayClick::Right => v.push(WM_CONTEXTMENU),
            TrayClick::Middle => {}
        }
    }
    v
}

struct Target {
    id: u64,
    owner: isize,
    pid: u32,
    uid: u32,
    callback: u32,
    version: u32,
    exe: String,
    image: String,
}

fn target_of(id: u64) -> Option<Target> {
    let st = state();
    st.items.iter().find(|t| t.id == id).map(|t| Target {
        id: t.id,
        owner: t.owner,
        pid: t.pid,
        uid: t.uid,
        callback: t.callback,
        version: t.version,
        exe: t.exe.clone(),
        image: t.image.clone(),
    })
}

/// true if the owning window still exists and belongs to the same registered process.
fn owner_alive(owner: isize, pid: u32) -> bool {
    let h = hwnd_of(owner);
    // SAFETY: no preconditions; returns false for an invalid handle.
    if !unsafe { IsWindow(Some(h)) }.as_bool() {
        return false;
    }
    window_pid(h) == pid
}

fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` is valid and writable; for an invalid handle the call fails leaving 0.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Removes icon `id` (dead owner) and notifies the UI if it was visible.
fn remove_dead(id: u64, exe: &str) {
    let was_visible = {
        let mut st = state();
        match st.items.iter().position(|t| t.id == id) {
            Some(i) => {
                let v = is_visible_item(&st.items[i]);
                st.items.remove(i);
                Some(v)
            }
            None => None,
        }
    };
    if let Some(v) = was_visible {
        log!("tray: owner of #{id} ({exe}) no longer valid, icon removed");
        if v {
            post_ui(2);
        }
    }
}

fn allow_foreground(pid: u32) {
    // SAFETY: no preconditions; `pid` is the icon owner's.
    if let Err(e) = unsafe { AllowSetForegroundWindow(pid) } {
        log!("tray: AllowSetForegroundWindow({pid}) failed: {e}, falling back to ASFW_ANY");
        // SAFETY: no preconditions.
        if let Err(e2) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
            log!("tray: AllowSetForegroundWindow(ASFW_ANY) failed: {e2}");
        }
    }
}

/// Facts about an icon's owner gathered when the dock is about to send it a message.
#[derive(Clone, Copy, Debug)]
struct OwnerFacts<'a> {
    /// The owner window still exists and belongs to the pid recorded for the icon.
    same_process: bool,
    /// That process runs at integrity not higher than medium.
    at_most_medium: bool,
    /// Image path recorded when the icon was registered (empty if it could not be read).
    recorded_image: &'a str,
    /// Image path of the process now (`None` if not read or unreadable).
    current_image: Option<&'a str>,
}

/// Outcome of `callback_verdict`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Allowed,
    /// The window is gone or belongs to another process: nothing is sent (no log).
    NotOwner,
    /// Elevated owner and the admin switch is off.
    ElevatedNotTrusted,
    /// Elevated owner, switch on, but the callback is not an application message.
    SystemMessage,
    /// Elevated owner, switch on, but its process no longer runs the image recorded at registration.
    ImageMismatch,
}

/// true if `callback` is in the ranges a tray icon legitimately uses: private and `WM_APP` messages
/// (`WM_USER..=0xBFFF`) and `RegisterWindowMessage` strings (`0xC000..=0xFFFF`). Never a system message
/// below `WM_USER` (e.g. WM_COMMAND, WM_SYSCOMMAND), never the reserved range above 0xFFFF.
fn is_app_message(callback: u32) -> bool {
    (WM_USER..=0xFFFF).contains(&callback)
}

/// Pure gating decision for any message the dock sends on behalf of a tray registration
/// (callback clicks, hover `WM_MOUSEMOVE`, `NIN_*`, `WM_CONTEXTMENU`).
///
/// UIPI: any medium-integrity process can register an icon whose owner HWND belongs to an elevated
/// process (a registration is a WM_COPYDATA whose sender cannot be identified, and it names the owner
/// window and the callback message itself). Our elevated dock would then deliver attacker-chosen
/// messages across the integrity boundary. So owners above medium (or of unknown integrity) are
/// refused unless an administrator opted in (`trust_elevated`, HKLM registry), and even then only
/// application messages (`is_app_message`) are sent, and only while the owner process still runs the
/// image recorded when the icon was registered. Owners at most medium are not affected.
fn callback_verdict(o: &OwnerFacts<'_>, callback: u32, trust_elevated: bool) -> Verdict {
    if !o.same_process {
        return Verdict::NotOwner;
    }
    if o.at_most_medium {
        return Verdict::Allowed;
    }
    if !trust_elevated {
        return Verdict::ElevatedNotTrusted;
    }
    if !is_app_message(callback) {
        return Verdict::SystemMessage;
    }
    match o.current_image {
        Some(now) if !o.recorded_image.is_empty() && now.eq_ignore_ascii_case(o.recorded_image) => Verdict::Allowed,
        _ => Verdict::ImageMismatch,
    }
}

/// Admin-only opt-in: `HKLM\SOFTWARE\mini-oled-dock`, DWORD `TrustElevatedTrayIcons` (1 = trust).
/// Deliberately NOT in config.json, which any medium-integrity process can write (the gate would
/// fail open). HKLM\SOFTWARE is writable only by administrators. The Settings window (elevated, like
/// the whole dock) writes it through `set_trust_elevated_tray_icons`.
const TRUST_KEY: PCWSTR = w!("SOFTWARE\\mini-oled-dock");
const TRUST_VALUE: PCWSTR = w!("TrustElevatedTrayIcons");
/// Maximum age of the cached registry value.
const TRUST_CACHE_TTL: Duration = Duration::from_secs(10);
/// Cached (read time, value) of the opt-in; `None` = never read or invalidated.
static TRUST_CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// Reads the opt-in from the 64-bit registry view. Default false on any error or if missing.
fn read_trust_elevated_tray_icons() -> bool {
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `data`/`size` are valid writable locals of the declared size; key/value names are
    // static NUL-terminated wide strings. RRF_RT_REG_DWORD guarantees at most 4 bytes are written.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            TRUST_KEY,
            TRUST_VALUE,
            RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
            None,
            Some((&mut data as *mut u32).cast::<c_void>()),
            Some(&mut size),
        )
    };
    r == ERROR_SUCCESS && data == 1
}

/// Cached opt-in, re-read from the registry at most every `TRUST_CACHE_TTL` (or after
/// `invalidate_trust_cache`).
pub fn trust_elevated_tray_icons() -> bool {
    let mut c = TRUST_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((t, v)) = *c {
        if t.elapsed() < TRUST_CACHE_TTL {
            return v;
        }
    }
    let v = read_trust_elevated_tray_icons();
    if c.is_none_or(|(_, old)| old != v) {
        log!("tray: TrustElevatedTrayIcons (HKLM) = {v}");
        // A new value can refuse for a new reason: log blocked owners again.
        BLOCKED_LOGGED.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
    *c = Some((Instant::now(), v));
    v
}

/// Forces the next check to re-read the registry (called on config reload).
pub fn invalidate_trust_cache() {
    let mut c = TRUST_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    // Keep the last value (for change logging) but mark it as expired; if the clock cannot go
    // back that far, simply forget it.
    *c = match (*c, Instant::now().checked_sub(TRUST_CACHE_TTL)) {
        (Some((_, v)), Some(expired)) => Some((expired, v)),
        _ => None,
    };
}

/// Writes the opt-in (1 or 0) to the 64-bit view of HKLM, creating the key if needed, and makes it
/// take effect at once. Needs administrator rights (the dock runs elevated); returns the error text
/// otherwise. Called from the Settings window only.
pub fn set_trust_elevated_tray_icons(on: bool) -> Result<(), String> {
    let mut key = HKEY::default();
    // SAFETY: `TRUST_KEY` is a static NUL-terminated wide string; `key` is a writable HKEY, closed below.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            TRUST_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &mut key,
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(format!("opening HKLM\\SOFTWARE\\mini-oled-dock: {}", windows::core::Error::from(r.to_hresult())));
    }
    let data = u32::from(on).to_le_bytes();
    // SAFETY: `key` is open with KEY_SET_VALUE; the value name is a static NUL-terminated wide string
    // and `data` (4 bytes, REG_DWORD) lives for the call.
    let w = unsafe { RegSetValueExW(key, TRUST_VALUE, None, REG_DWORD, Some(&data)) };
    // SAFETY: `key` was opened above and is closed exactly once.
    let _ = unsafe { RegCloseKey(key) };
    if w != ERROR_SUCCESS {
        return Err(format!("writing TrustElevatedTrayIcons: {}", windows::core::Error::from(w.to_hresult())));
    }
    invalidate_trust_cache();
    trust_elevated_tray_icons();
    Ok(())
}

/// Owners for which a blocked callback was already logged (log once per owner). Bounded.
static BLOCKED_LOGGED: Mutex<Vec<isize>> = Mutex::new(Vec::new());
const BLOCKED_LOGGED_CAP: usize = 256;

/// Log text of a refusal, after "owner runs above medium integrity".
const NOT_TRUSTED_HINT: &str = " (allow it in Settings > Modules > Tray, \"Icons of apps run as administrator\", \
                                or set HKLM\\SOFTWARE\\mini-oled-dock TrustElevatedTrayIcons = 1)";

/// true if the dock may send messages on behalf of icon `t` (`callback_verdict`). Logs once per owner
/// when blocked.
fn callback_allowed(t: &Target) -> bool {
    let same_process = owner_alive(t.owner, t.pid);
    let at_most_medium = same_process && pid_at_most_medium(t.pid);
    let trust = same_process && !at_most_medium && trust_elevated_tray_icons();
    // The image is read only for an elevated owner the switch may allow.
    let current = if trust && is_app_message(t.callback) { crate::apps::exe_path_of_pid(t.pid) } else { None };
    let facts =
        OwnerFacts { same_process, at_most_medium, recorded_image: &t.image, current_image: current.as_deref() };
    let why = match callback_verdict(&facts, t.callback, trust) {
        Verdict::Allowed => return true,
        Verdict::NotOwner => return false,
        Verdict::ElevatedNotTrusted => NOT_TRUSTED_HINT.to_string(),
        Verdict::SystemMessage => " and the callback is not an application message".to_string(),
        Verdict::ImageMismatch => format!(
            " and its process image ({}) is not the one recorded at registration ({})",
            current.as_deref().unwrap_or("?"),
            if t.image.is_empty() { "?" } else { &t.image }
        ),
    };
    let mut logged = BLOCKED_LOGGED.lock().unwrap_or_else(|p| p.into_inner());
    if !logged.contains(&t.owner) {
        if logged.len() >= BLOCKED_LOGGED_CAP {
            logged.clear();
        }
        logged.push(t.owner);
        log!(
            "tray: messages of icon #{} ({}, callback 0x{:X}) to owner 0x{:X} (pid {}) blocked: owner runs above medium integrity{why}",
            t.id,
            t.exe,
            t.callback,
            t.owner,
            t.pid,
        );
    }
    false
}

fn send_callback(t: &Target, ev: u32, anchor: POINT) {
    if !callback_allowed(t) {
        return;
    }
    let (wp, lp) = encode(t.version, t.uid, anchor.x, anchor.y, ev);
    // SAFETY: SendNotifyMessageW to another window does not block; if the window disappears it merely fails.
    if let Err(e) = unsafe { SendNotifyMessageW(hwnd_of(t.owner), t.callback, WPARAM(wp), LPARAM(lp)) } {
        log!("tray: SendNotifyMessageW(#{}, 0x{ev:X}) failed: {e}", t.id);
    }
}

// ------------------------------------------------------------------ public API

/// Starts the `tray-host` thread (creates the window, marker, UIPI filter, timers) and waits up to 2 s
/// for it to be ready. `ui_hwnd` receives `WM_DOCK_TRAY_CHANGED`. Called by `main` ONLY if
/// `taskbar::is_hidden()` and `config.show_tray`; on success `main` calls
/// `taskbar::announce_tray()`. Error = no tray in the dock (the icons stay with Explorer).
pub fn start(ui_hwnd: HWND) -> Result<(), String> {
    if RUNNING.load(Relaxed) {
        return Ok(());
    }
    YIELDED.store(false, Relaxed);
    FWD_STOP.store(false, Relaxed);
    FWD_OVERFLOW_LOGGED.store(false, Relaxed);
    UI_HWND.store(ui_hwnd.0 as isize, Relaxed);
    yield_msg_id();
    {
        let mut st = state();
        st.items.clear();
        st.rects.clear();
    }
    FWD_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).clear();

    let fwd = std::thread::Builder::new()
        .name("tray-fwd".into())
        .stack_size(128 * 1024)
        .spawn(forward_main)
        .map_err(|e| format!("thread tray-fwd not started: {e}"))?;
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let host = match std::thread::Builder::new().name("tray-host".into()).spawn(move || host_main(tx)) {
        Ok(h) => h,
        Err(e) => {
            stop_forwarder(Some(fwd));
            return Err(format!("thread tray-host not started: {e}"));
        }
    };
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {
            let mut th = THREADS.lock().unwrap_or_else(|e| e.into_inner());
            *th = (Some(host), Some(fwd));
            log!("tray: host started");
            Ok(())
        }
        Ok(Err(e)) => {
            stop_forwarder(Some(fwd));
            let _ = host.join();
            Err(e)
        }
        Err(_) => {
            // The thread might complete later: ask it to close and give up.
            post_host(WM_CLOSE);
            stop_forwarder(Some(fwd));
            Err("tray host not ready within 2 s".into())
        }
    }
}

/// Stops the host thread: `WM_CLOSE` to the window, `DestroyWindow`, join for at most 1500 ms; on timeout
/// `yield_now()`. Idempotent. In `cleanup` BEFORE `taskbar::shutdown()`.
pub fn stop() {
    let (host, fwd) = {
        let mut th = THREADS.lock().unwrap_or_else(|e| e.into_inner());
        (th.0.take(), th.1.take())
    };
    post_host(WM_CLOSE);
    if let Some(h) = host {
        let t = Instant::now();
        while !h.is_finished() && t.elapsed() < Duration::from_millis(1500) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            if h.join().is_err() {
                log!("tray: tray-host thread terminated with a panic");
            }
        } else {
            log!("tray: host did not close within 1500 ms, yielding to Explorer");
            yield_now();
        }
    }
    stop_forwarder(fwd);
}

fn stop_forwarder(fwd: Option<JoinHandle<()>>) {
    FWD_STOP.store(true, Relaxed);
    FWD_CV.notify_all();
    if let Some(h) = fwd {
        let t = Instant::now();
        while !h.is_finished() && t.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            log!("tray: tray-fwd thread busy, leaving it to the process");
        }
    }
}

/// true if the host thread is running (even if it yielded to Explorer): `stop` has something to stop.
pub fn is_started() -> bool {
    RUNNING.load(Relaxed)
}

/// true if the host window exists and is active (not yielded).
pub fn is_running() -> bool {
    RUNNING.load(Relaxed) && !YIELDED.load(Relaxed)
}

/// Stable key of an icon for `config.tray_hidden`: lowercase GUID without braces if present,
/// otherwise `<lowercase exe name>#<uID>` (unknown exe = "?").
fn item_key(t: &TrayItem) -> String {
    match &t.guid {
        Some(g) => guid_string(g),
        None => {
            let exe = if t.exe.is_empty() { "?".to_string() } else { t.exe.to_lowercase() };
            format!("{exe}#{}", t.uid)
        }
    }
}

/// Name to show for the icon: FileDescription, then first tooltip line, then exe without extension.
fn display_name(t: &TrayItem) -> String {
    if !t.name.trim().is_empty() {
        return t.name.trim().to_string();
    }
    if let Some(l) = t.tip.lines().next().map(str::trim).filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    let stem = t.exe.rsplit_once('.').map_or(t.exe.as_str(), |(a, _)| a);
    if stem.is_empty() {
        "App".to_string()
    } else {
        stem.to_string()
    }
}

fn view_of(t: &TrayItem, user_hidden: bool) -> TrayView {
    TrayView { id: t.id, bitmap: t.bitmap.clone(), key: item_key(t), name: display_name(t), user_hidden }
}

fn user_hidden_in(cfg: &crate::config::Config, key: &str) -> bool {
    cfg.tray_hidden.iter().any(|h| h.eq_ignore_ascii_case(key))
}

/// All icons with a bitmap (excluding only the system ones in `tray_hidden_guids`), including those
/// hidden by the user (`user_hidden == true`). For the chevron popup.
pub fn snapshot_all() -> Vec<TrayView> {
    collect(true)
}

/// true if at least one icon that would otherwise be shown is hidden by the user (the bar shows the
/// "hidden icons" chevron for it). Same filters as `collect`; allocates (keys), so call it on state changes, not per frame.
pub fn any_user_hidden() -> bool {
    let cfg = crate::config::get();
    if cfg.tray_hidden.is_empty() {
        return false;
    }
    let st = state();
    st.items.iter().filter(|t| is_visible_item(t) && !guid_hidden(&cfg, t)).any(|t| user_hidden_in(&cfg, &item_key(t)))
}

fn guid_hidden(cfg: &crate::config::Config, t: &TrayItem) -> bool {
    match &t.guid {
        Some(g) => {
            let s = guid_string(g);
            cfg.tray_hidden_guids.iter().any(|h| h.eq_ignore_ascii_case(&s))
        }
        None => false,
    }
}

/// Builds the views; with `include_hidden == false` user-hidden icons are skipped BEFORE the
/// view is built (no bitmap Arc clone / String copies for them). No cap on the count.
fn collect(include_hidden: bool) -> Vec<TrayView> {
    let cfg = crate::config::get();
    let st = state();
    st.items
        .iter()
        .filter(|t| is_visible_item(t))
        .filter(|t| !guid_hidden(&cfg, t))
        .filter_map(|t| {
            let hidden = user_hidden_in(&cfg, &item_key(t));
            (include_hidden || !hidden).then(|| view_of(t, hidden))
        })
        .collect()
}

/// Candidate icons for the bar: `snapshot_all()` without those hidden by the user, in stable order.
/// Hidden ones are reachable only from the chevron popup (`snapshot_all`).
pub fn snapshot_visible() -> Vec<TrayView> {
    collect(false)
}

/// Same as `snapshot_visible()` (historical name, used by `ui`).
pub fn snapshot() -> Vec<TrayView> {
    snapshot_visible()
}

/// Sets whether icon `key` is hidden by the user: atomic config write and
/// `WM_DOCK_TRAY_CHANGED` to the UI (the bar updates immediately). Callable from the UI thread: a
/// small file write.
pub fn set_user_hidden(key: &str, hidden: bool) {
    let r = crate::config::modify(|c| {
        c.tray_hidden.retain(|h| !h.eq_ignore_ascii_case(key));
        if hidden {
            c.tray_hidden.push(key.to_string());
        }
    });
    if let Err(e) = r {
        log!("tray: saving tray_hidden failed: {e}");
        return;
    }
    post_ui(2);
}

/// Side (physical px) to scale the bitmaps to (`Metrics::px(16)`); on change the host thread rescales from the
/// sources and posts `WM_DOCK_TRAY_CHANGED` (1). Called by the UI at startup and on every DPI change.
pub fn set_icon_px(px: u32) {
    let px = px.max(1);
    if ICON_PX.swap(px, Relaxed) != px {
        post_host(WM_HOST_RESCALE);
    }
}

/// Forwards a click to the app owning icon `id` (encode for `uVersion`, `AllowSetForegroundWindow`,
/// `SendNotifyMessageW`). `anchor` = cursor point in physical screen coordinates. Dead owner =
/// icon removed + `WM_DOCK_TRAY_CHANGED` (2). Does not block.
pub fn click(id: u64, kind: TrayClick, anchor: POINT) {
    let Some(t) = target_of(id) else { return };
    if !owner_alive(t.owner, t.pid) {
        remove_dead(t.id, &t.exe);
        return;
    }
    if t.callback == 0 {
        log!("tray: icon #{id} ({}) has no callback message", t.exe);
        return;
    }
    if !callback_allowed(&t) {
        return;
    }
    allow_foreground(t.pid);
    let anchor = if kind == TrayClick::Middle { anchor } else { menu_anchor(id, anchor) };
    if kind != TrayClick::Middle {
        note_click(t.pid);
    }
    if kind == TrayClick::Right {
        arm_placement(id, t.pid);
    }
    for ev in click_events(kind, t.version) {
        send_callback(&t, ev, anchor);
    }
}

// ------------------------------------------------------------------ app context menus

/// Duration (ms) after a click during which the app is assumed to be opening a menu/flyout.
const MENU_GRACE_MS: u64 = 1200;
/// Maximum duration (ms) for which an app popup window covering the bar counts as an open menu.
const MENU_WINDOW_MS: u64 = 15_000;

/// Last forwarded click: milliseconds since start (0 = never) and the app's pid.
static LAST_CLICK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_CLICK_PID: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

fn note_click(pid: u32) {
    LAST_CLICK_PID.store(pid, Relaxed);
    LAST_CLICK_MS.store(now_ms(), Relaxed);
}

/// Point passed to the app as the menu anchor (v4: `wParam`): horizontally the CENTER of the clicked
/// icon (cursor x if its rect is unknown), vertically just outside the bar (above it at the bottom,
/// below it at the top), so menus open next to the bar and never over the icons.
fn menu_anchor(id: u64, p: POINT) -> POINT {
    let st = state();
    let g = st.geom;
    let x = st.rects.iter().find(|(i, _)| *i == id).map_or(p.x, |(_, r)| (r.left + r.right) / 2);
    anchor_point(&g, x, p)
}

/// Pure part of `menu_anchor` (tested).
fn anchor_point(g: &TrayGeometry, x: i32, p: POINT) -> POINT {
    if g.bar.bottom <= g.bar.top {
        return p;
    }
    if g.edge == 1 {
        POINT { x, y: g.bar.bottom + 1 }
    } else {
        POINT { x, y: g.bar.top - 1 }
    }
}

// ------------------------------------------------------------------ placing the app's own menu

/// How long (ms) after a right click a window of the app is considered its context menu.
const PLACE_WINDOW_MS: u64 = 3000;
/// Maximum re-placements of the same window (an app that keeps moving it back wins).
const PLACE_MAX_MOVES: u32 = 6;
const TIMER_PLACE: usize = 3;
const WM_HOST_PLACE_ARM: u32 = WM_APP + 4;
/// Posted by the WinEvent callback: wparam = window, lparam = event id.
const WM_HOST_PLACE_MOVE: u32 = WM_APP + 5;

/// Pending placement: the next context menu / flyout shown by `pid` is moved so that it is centered
/// on the clicked icon, `POPUP_GAP` above (or below) the bar, exactly like the dock's own popups.
#[derive(Clone, Copy)]
struct Placement {
    pid: u32,
    item: RECT,
    bar: RECT,
    top: bool,
    until_ms: u64,
    /// Window already placed (0 = none yet): only the FIRST one is taken (submenus stay native).
    placed: isize,
    moves: u32,
}

static PLACEMENT: Mutex<Option<Placement>> = Mutex::new(None);
/// Raw `HWINEVENTHOOK`s installed by the host thread for the current placement.
static PLACE_HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn placement() -> MutexGuard<'static, Option<Placement>> {
    PLACEMENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called by `click` on a right click: records icon/bar and asks the host thread to hook the
/// owner's window events (filtered by pid, only for `PLACE_WINDOW_MS`).
fn arm_placement(id: u64, pid: u32) {
    let (item, g) = {
        let st = state();
        (st.rects.iter().find(|(i, _)| *i == id).map(|(_, r)| *r), st.geom)
    };
    let Some(item) = item.filter(|r| r.right > r.left) else { return };
    if g.bar.bottom <= g.bar.top {
        return;
    }
    *placement() = Some(Placement {
        pid,
        item,
        bar: g.bar,
        top: g.edge == 1,
        until_ms: now_ms() + PLACE_WINDOW_MS,
        placed: 0,
        moves: 0,
    });
    let h = HOST_HWND.load(Relaxed);
    if h != 0 {
        // SAFETY: PostMessageW is thread-safe; if the window is gone it merely fails.
        let _ = unsafe { PostMessageW(Some(hwnd_of(h)), WM_HOST_PLACE_ARM, WPARAM(pid as usize), LPARAM(0)) };
    }
}

/// Host thread: (re)installs the out-of-context hooks for `pid` and the disarm timer.
fn place_arm(host: HWND, pid: u32) {
    use windows::Win32::UI::Accessibility::SetWinEventHook;
    use windows::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_MENUPOPUPSTART, WINEVENT_OUTOFCONTEXT,
    };
    place_disarm(host);
    let mut hooks = PLACE_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    for ev in [EVENT_SYSTEM_MENUPOPUPSTART, EVENT_OBJECT_SHOW, EVENT_OBJECT_LOCATIONCHANGE] {
        // SAFETY: `place_winevent_proc` is a 'static extern "system" fn with the WINEVENTPROC
        // signature; out-of-context (no module); this thread runs a message loop.
        let hk = unsafe { SetWinEventHook(ev, ev, None, Some(place_winevent_proc), pid, 0, WINEVENT_OUTOFCONTEXT) };
        if hk.is_invalid() {
            log!("tray: SetWinEventHook(0x{ev:X}, pid {pid}) failed: {:?}", windows::core::Error::from_thread());
        } else {
            hooks.push(hk.0 as isize);
        }
    }
    // SAFETY: own window, same thread.
    unsafe { SetTimer(Some(host), TIMER_PLACE, PLACE_WINDOW_MS as u32, None) };
}

/// Host thread: removes the placement hooks and timer. Idempotent.
fn place_disarm(host: HWND) {
    use windows::Win32::UI::Accessibility::{UnhookWinEvent, HWINEVENTHOOK};
    let hooks = std::mem::take(&mut *PLACE_HOOKS.lock().unwrap_or_else(|e| e.into_inner()));
    for h in hooks {
        // SAFETY: handle obtained from SetWinEventHook on this thread and removed from the list above.
        let _ = unsafe { UnhookWinEvent(HWINEVENTHOOK(h as *mut c_void)) };
    }
    // SAFETY: own window, same thread; fails harmlessly if the timer does not exist.
    let _ = unsafe { KillTimer(Some(host), TIMER_PLACE) };
}

/// true if `h` looks like a context menu / flyout: visible top-level, no title bar, not a child,
/// sensible size (not a frameless main window).
fn is_menu_candidate(h: HWND, work: &RECT) -> Option<RECT> {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetDesktopWindow, GA_PARENT};
    // SAFETY: functions without preconditions; invalid handles just fail.
    unsafe {
        if !IsWindowVisible(h).as_bool() || GetAncestor(h, GA_PARENT) != GetDesktopWindow() {
            return None;
        }
        let style = GetWindowLongW(h, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 || style & WS_CHILD.0 != 0 {
            return None;
        }
        let mut rc = RECT::default();
        GetWindowRect(h, &mut rc).ok()?;
        let (w, ht) = (rc.right - rc.left, rc.bottom - rc.top);
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        (w >= 24 && ht >= 16 && w * 10 <= ww * 6 && ht <= wh).then_some(rc)
    }
}

/// Target top-left of a `w` x `h` menu for placement `p` (same rule as the dock's popups).
fn place_target(p: &Placement, w: i32, h: i32) -> (i32, i32) {
    let ui = UI_HWND.load(Relaxed);
    // SAFETY: no preconditions; 0 for an invalid window.
    let dpi = if ui == 0 { 96 } else { unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd_of(ui)) } };
    let a = crate::popup::Anchor { item: p.item, bar: p.bar, top: p.top, dpi: dpi.max(96) };
    crate::popup::compute_pos(&a, w, h)
}

/// Pure filter for a placement event: false when the armed window expired, the window belongs to another
/// process, or the event is not the one this placement still wants (see `Placement::placed`).
fn place_should_handle(p: &Placement, event: u32, raw_hwnd: isize, pid_matches: bool, now: u64) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::EVENT_OBJECT_LOCATIONCHANGE;
    if now > p.until_ms || !pid_matches {
        return false;
    }
    if event == EVENT_OBJECT_LOCATIONCHANGE {
        raw_hwnd == p.placed && p.moves < PLACE_MAX_MOVES
    } else {
        p.placed == 0
    }
}

/// Out-of-context hook callback: it runs on the host thread, so it only forwards the event; the real work is
/// in `place_move` (a foreign window call here could block `Shell_NotifyIcon` / AppBar handling).
unsafe extern "system" fn place_winevent_proc(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    use windows::Win32::UI::WindowsAndMessaging::{CHILDID_SELF, EVENT_SYSTEM_MENUPOPUPSTART, OBJID_WINDOW};
    if hwnd.0.is_null() {
        return;
    }
    if event != EVENT_SYSTEM_MENUPOPUPSTART && (id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32) {
        return;
    }
    let h = HOST_HWND.load(Relaxed);
    if h != 0 {
        // SAFETY: PostMessageW is thread-safe; if the window is gone it merely fails.
        let _ = unsafe {
            PostMessageW(Some(hwnd_of(h)), WM_HOST_PLACE_MOVE, WPARAM(hwnd.0 as usize), LPARAM(event as isize))
        };
    }
}

/// Host thread: handles one forwarded placement event and moves the foreign window asynchronously.
fn place_move(event: u32, hwnd: HWND) {
    let mut guard = placement();
    let Some(p) = guard.as_mut() else { return };
    let raw = hwnd.0 as isize;
    if !place_should_handle(p, event, raw, window_pid(hwnd) == p.pid, now_ms()) {
        return;
    }
    let work = crate::popup::work_area(&p.bar);
    let Some(rc) = is_menu_candidate(hwnd, &work) else { return };
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let (x, y) = place_target(p, w, h);
    p.placed = raw;
    if (rc.left, rc.top) == (x, y) {
        return;
    }
    p.moves += 1;
    let p_copy = *p;
    drop(guard);
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    // SAFETY: moving another process's top-level window; no size/Z-order/activation change. ASYNCWINDOWPOS
    // queues the move so a hung owner cannot stall this thread.
    if let Err(e) = unsafe { SetWindowPos(hwnd, None, x, y, 0, 0, flags) } {
        log!("tray: placing menu of pid {} failed: {e}", p_copy.pid);
    } else if p_copy.moves == 1 {
        log!("tray: menu of pid {} placed at ({x},{y}) {w}x{h} (was {},{})", p_copy.pid, rc.left, rc.top);
    }
}

/// true while the window placed for the last right click is still visible.
fn placed_menu_visible() -> bool {
    let Some(p) = *placement() else { return false };
    // SAFETY: no preconditions; false for a destroyed window.
    p.placed != 0 && unsafe { IsWindowVisible(hwnd_of(p.placed)) }.as_bool()
}

fn rects_overlap(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// true if a visible system menu window (`#32768`) exists.
fn system_menu_visible() -> bool {
    let mut prev: Option<HWND> = None;
    loop {
        // SAFETY: static class literal; `prev` is None or a window returned by the same enumeration.
        let Ok(h) = (unsafe { FindWindowExW(None, prev, w!("#32768"), PCWSTR::null()) }) else { return false };
        if h.0.is_null() {
            return false;
        }
        // SAFETY: no preconditions.
        if unsafe { IsWindowVisible(h) }.as_bool() {
            return true;
        }
        prev = Some(h);
    }
}

struct PopupScan {
    pid: u32,
    bar: RECT,
    found: bool,
}

unsafe extern "system" fn scan_popup_proc(h: HWND, lp: LPARAM) -> windows::core::BOOL {
    // SAFETY: `lp` is the pointer to `PopupScan` passed by `owner_popup_over_bar`, alive for the whole
    // synchronous enumeration.
    let scan = unsafe { &mut *(lp.0 as *mut PopupScan) };
    // SAFETY: no preconditions.
    if window_pid(h) == scan.pid && unsafe { IsWindowVisible(h) }.as_bool() {
        // SAFETY: no preconditions.
        let style = unsafe { GetWindowLongW(h, GWL_STYLE) } as u32;
        let mut rc = RECT::default();
        // SAFETY: `rc` is writable; fails for an invalid handle.
        if style & WS_CAPTION.0 != WS_CAPTION.0
            && unsafe { GetWindowRect(h, &mut rc) }.is_ok()
            && rects_overlap(&rc, &scan.bar)
        {
            scan.found = true;
            return false.into();
        }
    }
    true.into()
}

/// true if process `pid` has a visible window without a title bar (menu/flyout) on the bar.
fn owner_popup_over_bar(pid: u32, bar: RECT) -> bool {
    let mut scan = PopupScan { pid, bar, found: false };
    // SAFETY: `scan` outlives the call (synchronous enumeration); the callback uses it only there.
    let _ = unsafe { EnumWindows(Some(scan_popup_proc), LPARAM(&mut scan as *mut PopupScan as isize)) };
    scan.found
}

/// true while a context menu (or flyout) of a tray app is open or about to open: the UI must
/// suspend the `HWND_TOPMOST` re-assertion and auto-hide, otherwise the dock covers the menu.
/// True (a) for ~1.2 s after a forwarded click, (b) as long as a visible system menu `#32768`
/// exists, (c) up to 15 s after the click while the app has a title-less window on the bar.
/// Cheap (a few Win32 calls): meant for the UI thread's 500 ms poll.
pub fn foreign_menu_active() -> bool {
    let last = LAST_CLICK_MS.load(Relaxed);
    let since = if last == 0 { u64::MAX } else { now_ms().saturating_sub(last) };
    if since < MENU_GRACE_MS {
        return true;
    }
    if system_menu_visible() {
        return true;
    }
    if since < MENU_WINDOW_MS && placed_menu_visible() {
        return true;
    }
    if since < MENU_WINDOW_MS {
        let bar = state().geom.bar;
        return owner_popup_over_bar(LAST_CLICK_PID.load(Relaxed), bar);
    }
    false
}

/// Mouse over icon `id` (`WM_MOUSEMOVE`; in v4 without NIF_SHOWTIP also `NIN_POPUPOPEN`).
/// The UI calls it at most once per entry onto the icon. Does not block.
pub fn hover(id: u64, anchor: POINT) {
    let Some(t) = target_of(id) else { return };
    if t.callback == 0 || !owner_alive(t.owner, t.pid) {
        return;
    }
    send_callback(&t, WM_MOUSEMOVE, anchor);
}

/// Dock geometry (shown bar, tray area, edge). Called by the UI on every redraw that changes
/// the geometry. The host window is resized to `tray_area` by the host thread.
pub fn set_geometry(g: TrayGeometry) {
    state().geom = g;
    post_host(WM_HOST_RESIZE);
}

/// SCREEN rectangles of the icons visible in the bar (for `Shell_NotifyIconGetRect`, dwData 3).
/// Overflow icons use the chevron's rect (the UI passes it for their ids).
pub fn set_icon_rects(rects: &[(u64, RECT)]) {
    let mut st = state();
    st.rects.clear();
    st.rects.extend_from_slice(rects);
}

/// Immediately brings the host back to the top (after `taskbar::hide_all` for an Explorer event/restart), if not
/// yielded. Does not block (private message to the host thread).
pub fn raise() {
    post_host(WM_HOST_RAISE);
}

/// Yields to Explorer: permanent `YIELDED` flag, `SetWindowPos(HWND_BOTTOM, SWP_ASYNCWINDOWPOS)`,
/// no more forwarding. Idempotent, callable from ANY thread (panic hook, WM_ENDSESSION,
/// `taskbar::restore_force`, `restored_externally`). No-op if the host does not exist in this process.
pub fn yield_now() {
    let raw = HOST_HWND.load(Relaxed);
    if raw == 0 {
        return;
    }
    YIELDED.store(true, Relaxed);
    // SAFETY: asynchronous variant: does not block the caller even if the host thread were hung; if the
    // window has already been destroyed the call merely fails.
    let r = unsafe {
        SetWindowPos(
            hwnd_of(raw),
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS,
        )
    };
    if let Err(e) = r {
        log!("tray: yield_now, SetWindowPos(BOTTOM) failed: {e}");
    }
}

/// For processes that do NOT own the host (watchdog, `--restore`): finds every window with
/// `PROP_DOCK_TRAY_HOST` and sends it `DockTrayYield` (`SendMessageTimeoutW`, `SMTO_ABORTIFHUNG`, 500 ms);
/// if it does not respond, `SetWindowPos(HWND_BOTTOM, SWP_ASYNCWINDOWPOS)` as best effort. Returns true if
/// at least one host was left unresponsive (the watchdog decides the policy).
pub fn yield_foreign_hosts() -> bool {
    let msg = yield_msg_id();
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    let mut unanswered = false;
    let mut prev: Option<HWND> = None;
    loop {
        // SAFETY: static class literal; `prev` is None or a window returned by the same
        // enumeration (if it disappears the search ends with an error).
        let Ok(h) = (unsafe { FindWindowExW(None, prev, w!("Shell_TrayWnd"), PCWSTR::null()) }) else { break };
        if h.0.is_null() {
            break;
        }
        prev = Some(h);
        // SAFETY: static property name; invalid handle = property absent.
        if unsafe { GetPropW(h, PROP_DOCK_TRAY_HOST) }.is_invalid() || window_pid(h) == me {
            continue;
        }
        let mut res = 0usize;
        // SAFETY: registered message without pointers; `res` valid; SMTO_ABORTIFHUNG avoids waiting on hung hosts.
        let r = unsafe { SendMessageTimeoutW(h, msg, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 500, Some(&mut res)) };
        if r.0 == 0 {
            log!("tray: host {:?} did not respond to DockTrayYield, sending it to the bottom", h.0);
            unanswered = true;
            // SAFETY: asynchronous variant, does not block.
            let _ = unsafe {
                SetWindowPos(
                    h,
                    Some(HWND_BOTTOM),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS,
                )
            };
        } else {
            log!("tray: host {:?} yielded", h.0);
        }
    }
    unanswered
}

// ------------------------------------------------------------------ host thread

fn host_main(ready: mpsc::Sender<Result<(), String>>) {
    match create_host() {
        Ok(()) => {
            let _ = ready.send(Ok(()));
            let mut msg = MSG::default();
            loop {
                // SAFETY: `msg` valid and writable; message loop of the thread that created the window.
                let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if r.0 <= 0 {
                    break;
                }
                // SAFETY: `msg` just filled by GetMessageW.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            HOST_HWND.store(0, Relaxed);
            RUNNING.store(false, Relaxed);
            // SAFETY: classes registered by `create_host` with the same instance.
            unsafe {
                let inst = GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).ok();
                let _ = UnregisterClassW(w!("TrayNotifyWnd"), inst);
                let _ = UnregisterClassW(w!("Shell_TrayWnd"), inst);
            }
            let mut st = state();
            st.items.clear();
            st.rects.clear();
            log!("tray: host terminated");
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }
}

/// Registers the classes and creates `Shell_TrayWnd` + `TrayNotifyWnd`, marker, UIPI filters, timers.
fn create_host() -> Result<(), String> {
    // SAFETY: no preconditions.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let inst = HINSTANCE(module.0);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(host_wnd_proc),
        hInstance: inst,
        lpszClassName: w!("Shell_TrayWnd"),
        ..Default::default()
    };
    // SAFETY: `wc` fully initialized; static class name literal.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(format!("RegisterClassExW(Shell_TrayWnd): {:?}", windows::core::Error::from_thread()));
    }
    let wc_child = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(child_wnd_proc),
        hInstance: inst,
        lpszClassName: w!("TrayNotifyWnd"),
        ..Default::default()
    };
    // SAFETY: as above.
    if unsafe { RegisterClassExW(&wc_child) } == 0 {
        log!("tray: RegisterClassExW(TrayNotifyWnd) failed: {:?}", windows::core::Error::from_thread());
    }
    let geom = state().geom;
    let g = host_rect(&geom, monitor_rect_of(&geom.bar));
    let (x, y) = (g.left, g.top);
    let (cw, ch) = ((g.right - g.left).max(1), (g.bottom - g.top).max(1));
    // SAFETY: class registered above; static PCWSTR literals; the window is never shown.
    let host = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("Shell_TrayWnd"),
            w!(""),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            x,
            y,
            cw,
            ch,
            None,
            None,
            Some(inst),
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW(Shell_TrayWnd): {e}"))?;

    // Marker: lets the watchdog/`--restore` avoid mistaking it for Explorer's taskbar.
    // SAFETY: static name; the value is only a marker, never dereferenced.
    if let Err(e) = unsafe { SetPropW(host, PROP_DOCK_TRAY_HOST, Some(HANDLE(std::ptr::dangling_mut::<c_void>()))) } {
        // SAFETY: window just created by this thread.
        let _ = unsafe { DestroyWindow(host) };
        return Err(format!("SetPropW(DockTrayHost): {e}"));
    }
    // SAFETY: `host` is the window just created by this thread; no optional struct.
    if let Err(e) = unsafe { ChangeWindowMessageFilterEx(host, WM_COPYDATA, MSGFLT_ALLOW, None) } {
        log!(
            "tray: WARNING ChangeWindowMessageFilterEx(WM_COPYDATA) failed: {e} (UIPI will block the icons of medium-integrity apps!)"
        );
    }
    // SAFETY: as above.
    if let Err(e) = unsafe { ChangeWindowMessageFilterEx(host, WM_COMMAND, MSGFLT_ALLOW, None) } {
        log!("tray: ChangeWindowMessageFilterEx(WM_COMMAND) failed: {e}");
    }
    let ym = yield_msg_id();
    if ym != 0 {
        // SAFETY: as above.
        if let Err(e) = unsafe { ChangeWindowMessageFilterEx(host, ym, MSGFLT_ALLOW, None) } {
            log!("tray: ChangeWindowMessageFilterEx(DockTrayYield) failed: {e}");
        }
    }
    // Child TrayNotifyWnd: some apps look for it to compute popup positions.
    // SAFETY: class registered above; `host` valid; child window never visible.
    if let Err(e) = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("TrayNotifyWnd"),
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            cw,
            ch,
            Some(host),
            None,
            Some(inst),
            None,
        )
    } {
        log!("tray: CreateWindowExW(TrayNotifyWnd) failed: {e}");
    }
    copy_taskband(host);
    // SAFETY: `host` valid; timers handled by `host_wnd_proc` on the same thread.
    unsafe {
        if SetTimer(Some(host), TIMER_ZORDER, 1000, None) == 0 {
            log!("tray: SetTimer(1 s) failed: {:?}", windows::core::Error::from_thread());
        }
        if SetTimer(Some(host), TIMER_DEAD, 5000, None) == 0 {
            log!("tray: SetTimer(5 s) failed: {:?}", windows::core::Error::from_thread());
        }
    }
    HOST_HWND.store(host.0 as isize, Relaxed);
    RUNNING.store(true, Relaxed);
    // SAFETY: valid window; no SWP_SHOWWINDOW: it stays invisible.
    if let Err(e) =
        unsafe { SetWindowPos(host, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) }
    {
        log!("tray: initial SetWindowPos(TOPMOST) failed: {e}");
    }
    Ok(())
}

/// First `Shell_TrayWnd` that does not belong to a dock host (i.e. Explorer's).
fn find_explorer_tray() -> Option<HWND> {
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

/// Copies `TaskbandHWND` from Explorer's `Shell_TrayWnd` (for `ITaskbarList3`); acceptable degradation if missing.
fn copy_taskband(host: HWND) {
    let Some(ex) = find_explorer_tray() else { return };
    // SAFETY: static property name; invalid handle = property absent.
    let v = unsafe { GetPropW(ex, w!("TaskbandHWND")) };
    if v.is_invalid() {
        log!("tray: TaskbandHWND missing on Explorer (no progress on the dock)");
        return;
    }
    // SAFETY: `host` is our window; the value is an opaque handle copied from Explorer.
    if let Err(e) = unsafe { SetPropW(host, w!("TaskbandHWND"), Some(v)) } {
        log!("tray: SetPropW(TaskbandHWND) failed: {e}");
    }
}

unsafe extern "system" fn child_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: forwards the parameters received from the system to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

unsafe extern "system" fn host_wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => on_copydata(wp, lp),
        WM_TIMER => {
            match wp.0 {
                TIMER_ZORDER => keep_first(hwnd),
                TIMER_DEAD => sweep_dead(),
                TIMER_PLACE => place_disarm(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            // SAFETY: for WM_WINDOWPOSCHANGED lParam points to a WINDOWPOS valid for the duration of the message.
            let flags = unsafe { (*(lp.0 as *const WINDOWPOS)).flags };
            if flags.0 & SWP_SHOWWINDOW.0 != 0 {
                // The host must never be visible.
                // SAFETY: own window, same thread.
                let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            }
            // SAFETY: forwards to the default procedure.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_COMMAND => {
            forward_post(msg, wp, lp);
            LRESULT(0)
        }
        m if (WM_USER..WM_APP).contains(&m) => {
            forward_post(msg, wp, lp);
            LRESULT(0)
        }
        WM_HOST_RESIZE => {
            resize_host(hwnd);
            LRESULT(0)
        }
        WM_HOST_RAISE => {
            if !YIELDED.load(Relaxed) {
                copy_taskband(hwnd);
                set_topmost(hwnd);
            }
            LRESULT(0)
        }
        WM_HOST_RESCALE => {
            rescale_all();
            LRESULT(0)
        }
        WM_HOST_PLACE_ARM => {
            place_arm(hwnd, wp.0 as u32);
            LRESULT(0)
        }
        WM_HOST_PLACE_MOVE => {
            place_move(lp.0 as u32, hwnd_of(wp.0 as isize));
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: own window, same thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            place_disarm(hwnd);
            // SAFETY: timers created by this thread on this window.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ZORDER);
                let _ = KillTimer(Some(hwnd), TIMER_DEAD);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m != 0 && m == YIELD_MSG.load(Relaxed) => {
            yield_now();
            // SAFETY: own window, same thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(1)
        }
        // SAFETY: forwards to the default procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn set_topmost(hwnd: HWND) {
    // SAFETY: the window is invisible and ours: making it topmost covers nothing and does not show it.
    if let Err(e) =
        unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) }
    {
        log!("tray: SetWindowPos(TOPMOST) failed: {e}");
    }
}

/// 1 s timer: if we are not the first `Shell_TrayWnd` (and have not yielded) climb back up.
fn keep_first(hwnd: HWND) {
    if YIELDED.load(Relaxed) {
        return;
    }
    // SAFETY: static class literal.
    let first = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.unwrap_or_default();
    if first != hwnd {
        set_topmost(hwnd);
    }
    // SAFETY: no preconditions.
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        // SAFETY: own window, same thread.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

/// 5 s timer: removes icons whose owner is dead.
fn sweep_dead() {
    let cands: Vec<(u64, isize, u32, String)> =
        state().items.iter().map(|t| (t.id, t.owner, t.pid, t.exe.clone())).collect();
    for (id, owner, pid, exe) in cands {
        if !owner_alive(owner, pid) {
            remove_dead(id, &exe);
        }
    }
}

/// Rectangle given to the hidden `Shell_TrayWnd`. Apps that position their own flyouts from
/// `FindWindow("Shell_TrayWnd")` + `GetWindowRect` (e.g. Logitech G HUB's WinUI tray) infer the
/// taskbar edge by comparing that rect with the monitor: a small floating rect matches no edge and
/// they fall back to (0,0). So, like Explorer's, the window spans the monitor width and touches the
/// screen edge, covering the bar's height. Pure (tested); `mon` = the bar's monitor rect.
fn host_rect(g: &TrayGeometry, mon: RECT) -> RECT {
    let b = g.bar;
    if b.right <= b.left || b.bottom <= b.top || mon.right <= mon.left || mon.bottom <= mon.top {
        return area_rect(g);
    }
    // Right edge = right end of the dock's tray area: such apps right-align their flyout to the
    // taskbar's right end (where Explorer keeps the notification area), so it lands over our icons.
    let right = area_rect(g).right.clamp(mon.left + 1, mon.right);
    if g.edge == 1 {
        RECT { left: mon.left, top: mon.top, right, bottom: b.bottom.clamp(mon.top + 1, mon.bottom) }
    } else {
        RECT { left: mon.left, top: b.top.clamp(mon.top, mon.bottom - 1), right, bottom: mon.bottom }
    }
}

/// Monitor rectangle of `r` (nearest monitor); empty on failure.
fn monitor_rect_of(r: &RECT) -> RECT {
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
    // SAFETY: `r` is a valid RECT; MONITOR_DEFAULTTONEAREST always returns a monitor.
    let mon = unsafe { MonitorFromRect(r, MONITOR_DEFAULTTONEAREST) };
    crate::ui::monitor_info(mon).map_or(EMPTY_RECT, |mi| mi.rcMonitor)
}

fn resize_host(hwnd: HWND) {
    let geom = state().geom;
    let g = host_rect(&geom, monitor_rect_of(&geom.bar));
    let (w, h) = ((g.right - g.left).max(1), (g.bottom - g.top).max(1));
    // SAFETY: own window, same thread; without SWP_SHOWWINDOW it stays hidden.
    if let Err(e) = unsafe { SetWindowPos(hwnd, None, g.left, g.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE) } {
        log!("tray: SetWindowPos(resize) failed: {e}");
    }
}

/// Rescales all bitmaps from the sources (host thread, outside the lock).
fn rescale_all() {
    let px = ICON_PX.load(Relaxed);
    let srcs: Vec<(u64, Arc<IconBitmap>)> =
        state().items.iter().filter_map(|t| t.src.clone().map(|s| (t.id, s))).collect();
    let scaled: Vec<(u64, Arc<IconBitmap>, Arc<IconBitmap>)> = srcs
        .into_iter()
        .map(|(id, s)| {
            let b = Arc::new(icons::resize_area(&s, px));
            (id, s, b)
        })
        .collect();
    {
        let mut st = state();
        for (id, s, b) in scaled {
            if b.is_empty() {
                continue;
            }
            if let Some(t) = st.items.iter_mut().find(|t| t.id == id) {
                // Only if the source has not changed in the meantime.
                if t.src.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &s)) {
                    t.bitmap = Some(b);
                }
            }
        }
    }
    post_ui(1);
}

fn forward_post(msg: u32, wp: WPARAM, lp: LPARAM) {
    if let Some(ex) = find_explorer_tray() {
        // SAFETY: PostMessageW does not block; if Explorer disappears it merely fails.
        let _ = unsafe { PostMessageW(Some(ex), msg, wp, lp) };
    }
}

// ------------------------------------------------------------------ WM_COPYDATA

/// Bytes of the `WM_COPYDATA` (at most 64 KiB; empty if absent).
fn cds_bytes(cds: &COPYDATASTRUCT) -> &[u8] {
    if cds.lpData.is_null() || cds.cbData == 0 {
        return &[];
    }
    let n = (cds.cbData as usize).min(1 << 16);
    // SAFETY: by the WM_COPYDATA contract `lpData` points to `cbData` bytes valid for the whole duration
    // of the message; `n <= cbData`.
    unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, n) }
}

fn on_copydata(wp: WPARAM, lp: LPARAM) -> LRESULT {
    if lp.0 == 0 {
        return LRESULT(0);
    }
    // SAFETY: WM_COPYDATA's lParam is a pointer to a COPYDATASTRUCT valid for the whole duration of the message.
    let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
    let bytes = cds_bytes(cds);
    match cds.dwData {
        1 => handle_nim(wp, lp, bytes),
        0 => handle_appbar(wp, lp, bytes),
        3 => handle_icon_rect(bytes),
        _ => forward_sync(wp, lp),
    }
}

/// Synchronous forwarding (3 s timeout) of the original `WM_COPYDATA` to Explorer; 0 if absent.
fn forward_sync(wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(ex) = find_explorer_tray() else { return LRESULT(0) };
    let mut res = 0usize;
    // SAFETY: `lp` is the original COPYDATASTRUCT, valid for the whole duration of our handler; the
    // call marshals the data to Explorer; `res` valid.
    let r = unsafe {
        SendMessageTimeoutW(ex, WM_COPYDATA, wp, lp, SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT, 3000, Some(&mut res))
    };
    if r.0 == 0 {
        return LRESULT(0);
    }
    LRESULT(res as isize)
}

/// File name of the exe from a full path.
fn file_name_of(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or("").to_string()
}

/// `file_description` with a per-exe-path cache (many apps register together at login).
fn cached_file_description(path: &str) -> String {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(c) = cache.lock() {
        if let Some(v) = c.get(path) {
            return v.clone();
        }
    }
    let name = file_description(path);
    if let Ok(mut c) = cache.lock() {
        c.insert(path.to_string(), name.clone());
    }
    name
}

/// `FileDescription` of the exe (empty if absent or unreadable), read like the bar's app names
/// (`shell_thread::FileInfo`, `IShellItem2`). Runs on the short-lived `tray-name` thread, so it
/// initializes COM for the duration of the read.
fn file_description(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    // SAFETY: COM STA for this thread (FileInfo::read uses IShellItem2), paired with CoUninitialize below.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let name = crate::shell_thread::FileInfo::read(path).description.unwrap_or_default();
    if hr.is_ok() {
        // SAFETY: paired with the successful CoInitializeEx above; the shell item was dropped by `read`.
        unsafe { CoUninitialize() };
    }
    name
}

/// RID of the "medium" integrity level (`SECURITY_MANDATORY_MEDIUM_RID`).
const MEDIUM_RID: u32 = 0x2000;

/// true if process `pid` runs at integrity NOT higher than medium (or is this process).
/// Used so the elevated dock does not cross the UIPI boundary on behalf of a medium sender:
/// when in doubt (process cannot be opened, token unreadable) returns false.
fn pid_at_most_medium(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: no preconditions.
    if pid == unsafe { GetCurrentProcessId() } {
        return true;
    }
    // SAFETY: no preconditions; if the process cannot be opened it returns Err.
    let Ok(proc_h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let medium = process_at_most_medium(proc_h);
    // SAFETY: handle opened above, closed exactly once.
    let _ = unsafe { CloseHandle(proc_h) };
    medium
}

/// true if the process behind `proc_h` (opened with at least `PROCESS_QUERY_LIMITED_INFORMATION`) runs at
/// integrity NOT higher than medium; false when the token cannot be read.
fn process_at_most_medium(proc_h: HANDLE) -> bool {
    let mut token = HANDLE::default();
    // SAFETY: `proc_h` is a valid process handle of the caller; `token` writable, closed below.
    let opened = unsafe { OpenProcessToken(proc_h, TOKEN_QUERY, &mut token) }.is_ok();
    if !opened {
        return false;
    }
    // TOKEN_MANDATORY_LABEL + SID: 64 bytes are enough (integrity SID = 1 sub-authority); u64
    // buffer for pointer alignment.
    let mut buf = [0u64; 16];
    let mut ret = 0u32;
    // SAFETY: `buf` writable for the declared 128 bytes; `token` valid.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            std::mem::size_of_val(&buf) as u32,
            &mut ret,
        )
    }
    .is_ok();
    // SAFETY: handle opened above and no longer used.
    let _ = unsafe { CloseHandle(token) };
    if !ok {
        return false;
    }
    // SAFETY: GetTokenInformation succeeded: `buf` starts with a TOKEN_MANDATORY_LABEL whose `Sid`
    // points inside `buf` itself, still alive; the SID is valid, so count and sub-authorities are readable.
    let rid = unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return false;
        }
        *GetSidSubAuthority(sid, u32::from(count) - 1)
    };
    rid <= MEDIUM_RID
}

/// Converts the sender's HICON: copy, bitmap, destruction of the copy (all outside the lock).
fn make_icon_update(hicon: u32) -> IconUpdate {
    if hicon == 0 {
        return IconUpdate::Clear;
    }
    let raw = HICON(ext(hicon) as *mut c_void);
    // SAFETY: the HICON is a session-global USER handle; the sender is blocked in SendMessage and
    // therefore keeps it alive; CopyIcon fails with Err if it is not valid.
    let copy = match unsafe { CopyIcon(raw) } {
        Ok(c) => c,
        Err(e) => {
            log!("tray: CopyIcon failed: {e}");
            return IconUpdate::Keep;
        }
    };
    let bmp = icons::hicon_to_bitmap(copy);
    // SAFETY: `copy` is our own copy, destroyed exactly once.
    let _ = unsafe { DestroyIcon(copy) };
    let Some(bmp) = bmp else {
        log!("tray: HICON conversion failed");
        return IconUpdate::Keep;
    };
    let px = ICON_PX.load(Relaxed);
    // Already display size with nothing to crop: share one allocation between `src` and `scaled`
    // (same result as `resize_area`, so `rescale_all` agrees).
    if icons::resize_is_identity(&bmp, px) {
        let one = Arc::new(bmp);
        return IconUpdate::Set(Arc::clone(&one), one);
    }
    let scaled = icons::resize_area(&bmp, px);
    if scaled.is_empty() {
        log!("tray: icon empty after resizing");
        return IconUpdate::Keep;
    }
    IconUpdate::Set(Arc::new(bmp), Arc::new(scaled))
}

fn handle_nim(wp: WPARAM, lp: LPARAM, bytes: &[u8]) -> LRESULT {
    // Host that has yielded but is still the first `Shell_TrayWnd` (asynchronous SetWindowPos not
    // applied yet, or stop() timed out): the NIM goes to Explorer, otherwise the icon would be lost.
    if YIELDED.load(Relaxed) {
        return forward_sync(wp, lp);
    }
    let Some(d) = parse_shell_tray(bytes) else {
        // Not one of our NIMs (signature/length): Explorer decides.
        return forward_sync(wp, lp);
    };
    let nid = &d.nid;
    let owner = ext(nid.hwnd);
    let pid = window_pid(hwnd_of(owner));
    let msg = d.message;
    let icon = if (msg == NIM_ADD || msg == NIM_MODIFY) && nid.flags & NIF_ICON != 0 {
        make_icon_update(nid.hicon)
    } else {
        IconUpdate::Keep
    };
    let exe_path = if msg == NIM_ADD && pid != 0 { crate::apps::exe_path_of_pid(pid).unwrap_or_default() } else { String::new() };
    let res = {
        let mut st = state();
        let TrayState { items, next_id, .. } = &mut *st;
        apply_nim(items, next_id, &d, (pid != 0).then_some(pid), icon, &exe_path)
    };
    if msg == NIM_ADD && res.ret && !exe_path.is_empty() {
        // Short name for the chooser: reading version resources is disk I/O, so it stays outside
        // the host thread (the app is still blocked in its Shell_NotifyIcon). Reply immediately.
        let guid = (nid.flags & NIF_GUID != 0 && !guid_is_zero(&nid.guid)).then_some(nid.guid);
        let (uid, path) = (nid.uid, exe_path.clone());
        if let Err(e) = std::thread::Builder::new().name("tray-name".into()).stack_size(512 * 1024).spawn(move || {
            let name = cached_file_description(&path);
            if name.is_empty() {
                return;
            }
            let changed = {
                let mut st = state();
                match find_item(&st.items, owner, uid, guid) {
                    Some(i) if st.items[i].name != name => {
                        st.items[i].name = name;
                        true
                    }
                    _ => false,
                }
            };
            if changed {
                post_ui(1);
            }
        }) {
            log!("tray: app-name thread not started: {e}");
        }
    }
    if msg == NIM_ADD {
        let exe = file_name_of(&exe_path);
        let guid = if nid.flags & NIF_GUID != 0 { guid_string(&nid.guid) } else { "-".into() };
        log!(
            "tray: NIM_ADD exe={} guid={guid} tip=\"{}\" hwnd=0x{:X} uid={} ret={}",
            if exe.is_empty() { "?" } else { &exe },
            utf16_until_nul(&nid.tip),
            owner,
            nid.uid,
            res.ret
        );
    }
    enqueue_forward(wp.0, bytes);
    if res.change != 0 {
        post_ui(res.change);
    }
    LRESULT(res.ret as isize)
}

fn handle_icon_rect(bytes: &[u8]) -> LRESULT {
    if bytes.len() < 24 || rd_u32(bytes, 0) != TRAY_SIGNATURE {
        return LRESULT(0);
    }
    let which = rd_u32(bytes, 4);
    let owner = ext(rd_u32(bytes, 16));
    let uid = rd_u32(bytes, 20);
    let guid = if bytes.len() >= 40 {
        let mut d4 = [0u8; 8];
        d4.copy_from_slice(&bytes[32..40]);
        let g = GUID::from_values(rd_u32(bytes, 24), rd_u32(bytes, 28) as u16, (rd_u32(bytes, 28) >> 16) as u16, d4);
        (!guid_is_zero(&g)).then_some(g)
    } else {
        None
    };
    let st = state();
    let Some(i) = find_item(&st.items, owner, uid, guid) else { return LRESULT(0) };
    let id = st.items[i].id;
    let r = st.rects.iter().find(|(rid, _)| *rid == id).map(|(_, r)| *r).unwrap_or_else(|| area_rect(&st.geom));
    let (x, y) = if which == 2 { (r.right, r.bottom) } else { (r.left, r.top) };
    LRESULT((((y as u16 as u32) << 16) | (x as u16 as u32)) as i32 as isize)
}

fn handle_appbar(wp: WPARAM, lp: LPARAM, bytes: &[u8]) -> LRESULT {
    if bytes.len() < std::mem::size_of::<AppBarMsg>() {
        return forward_sync(wp, lp);
    }
    let m: AppBarMsg = from_prefix(bytes);
    if !matches!(m.message, ABM_GETTASKBARPOS | ABM_QUERYPOS | ABM_SETPOS) {
        return forward_sync(wp, lp);
    }
    // SAFETY: no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    let elevated = crate::shell_thread::elevation();
    // Sender windows named by the message (`wParam` and the APPBARDATA `hWnd`); windows of the dock itself
    // (e.g. `FindWindow("Shell_TrayWnd")` passed to `ABM_GETTASKBARPOS`) say nothing about the sender.
    let senders: Vec<u32> = [wp.0 as isize, ext(m.abd.hwnd)]
        .into_iter()
        .filter(|&w| w != 0)
        .map(|w| window_pid(hwnd_of(w)))
        .filter(|&p| p != own)
        .collect();
    let gate = appbar_gate(
        &m,
        &senders,
        own,
        elevated,
        pid_at_most_medium,
        resolve_shared_atom,
        open_holder,
        |p: &HolderProcess| process_at_most_medium(p.0),
        dup_section,
        map_block,
    );
    let block = match gate {
        Ok(b) => b,
        Err(text) => {
            log!("{text}");
            return LRESULT(0);
        }
    };
    if m.message == ABM_GETTASKBARPOS {
        get_taskbar_pos(&block)
    } else {
        forward_with_shared(wp, m, &block)
    }
}

/// Gate for the AppBar messages whose shared block the dock reads and writes (`ABM_GETTASKBARPOS`,
/// `ABM_QUERYPOS`, `ABM_SETPOS`). `Ok` = the block, mapped by the dock; `Err(text)` = reject, `text` is the
/// log line (claimed pid, holder pid, reason). Acceptance rule: the message's `shared` value is resolved
/// exactly once (`resolve`), as a global atom whose name decodes into a section handle and the pid of the
/// process holding it. The holder must not be the dock itself (allowed only while the dock is not elevated,
/// i.e. a `DOCK_NO_MANIFEST=1` developer build; unknown elevation counts as elevated). When the message names
/// sender windows (`senders`: the pids of `wParam` and of the APPBARDATA `hWnd`, windows of the dock left
/// out), each must be alive (pid != 0) and its process must run at most at medium (`sender_medium`; the
/// holder's own windows are covered by the holder check). Like the real taskbar, a window of another
/// process than the holder is allowed (shell window, frame host, helper). The holder
/// process is opened once (`open`), and that
/// same process handle is used for the integrity check (at most medium, `medium`) and for the duplication of
/// its section handle with `DUPLICATE_SAME_ACCESS` (`dup`), so no right is added that the holder did not have;
/// the access granted to that handle must include `SECTION_QUERY | SECTION_MAP_READ | SECTION_MAP_WRITE`
/// (all three messages get a reply written into the block), and the section must be a Section with
/// `AllocationAttributes == SEC_COMMIT` and `MaximumSize == 0x1000`. Then that same duplicate is mapped
/// (`map`, read/write, no more than granted) and its header must give a payload size in `0x28..=0xFF0`.
/// Without any sender window (`ABM_GETTASKBARPOS` with a NULL `hWnd`, or the dock's own window) the holder
/// rule alone applies. In every case the dock writes, for
/// `ABM_GETTASKBARPOS`, only `edge` (view bytes 0x1C..0x20) and `rc` (0x20..0x30), and for
/// `ABM_QUERYPOS` / `ABM_SETPOS` Explorer's 0x28-byte APPBARDATA answer at 0x10..0x38, into a block whose
/// holder had map-write access to it already. Pure: the Win32 steps are injected for tests.
#[allow(clippy::too_many_arguments)]
fn appbar_gate<P, B>(
    m: &AppBarMsg,
    senders: &[u32],
    own: u32,
    own_elevated: Option<bool>,
    sender_medium: impl Fn(u32) -> bool,
    resolve: impl Fn(u64) -> Result<(u64, u32), String>,
    open: impl Fn(u32) -> Result<P, String>,
    medium: impl Fn(&P) -> bool,
    dup: impl Fn(&P, u64) -> Result<(B, SharedBlock), String>,
    map: impl Fn(B) -> Result<B, String>,
) -> Result<B, String> {
    let reject = |holder: Option<u32>, why: &str| {
        let holder = holder.map_or_else(|| "-".to_string(), |p| p.to_string());
        let (msg, pid, shared) = (m.message, m.pid, m.shared);
        format!("tray: AppBar {msg} rejected (claimed pid {pid}, holder pid {holder}, shared 0x{shared:X}: {why})")
    };
    let (handle, holder) = resolve(m.shared).map_err(|e| reject(None, &e))?;
    let fail = |why: &str| reject(Some(holder), why);
    // Unknown elevation (token unreadable) counts as elevated: fail closed.
    if holder == own && own_elevated != Some(false) {
        return Err(fail("block held by the dock itself"));
    }
    for &s in senders {
        if s == 0 {
            return Err(fail("sender window's process is gone"));
        }
        if s != holder && !sender_medium(s) {
            return Err(fail(&format!("sender window's process {s} runs above medium integrity or cannot be checked")));
        }
    }
    let p = open(holder).map_err(|e| fail(&e))?;
    if holder != own && !medium(&p) {
        return Err(fail("holder runs above medium integrity or cannot be checked"));
    }
    let (b, shape) = dup(&p, handle).map_err(|e| fail(&e))?;
    drop(p);
    shared_block_authentic(&shape).map_err(fail)?;
    map(b).map_err(|e| fail(&e))
}

/// Access to the sender's section that the dock needs and that the holder's own handle must already grant.
const SECTION_NEEDED: u32 = SECTION_QUERY.0 | SECTION_MAP_READ.0 | SECTION_MAP_WRITE.0;

/// Shape of a sender's section, read from the dock's duplicate without mapping it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SharedBlock {
    /// Access granted to the duplicate, i.e. to the holder's own handle (`DUPLICATE_SAME_ACCESS`).
    granted: u32,
    /// The handle's object type is `Section`.
    is_section: bool,
    /// `SECTION_BASIC_INFORMATION.AllocationAttributes` and `.MaximumSize`.
    attrs: u32,
    size: i64,
}

/// Only attribute of a pagefile-backed section (`CreateFileMapping(INVALID_HANDLE_VALUE)`); an image
/// mapping carries `SEC_IMAGE` (0x0100_0000), a data-file mapping `SEC_FILE` (0x0080_0000).
const SEC_COMMIT: u32 = 0x0800_0000;
/// shell32 sections hold a 16-byte header plus the 0x28-byte APPBARDATA, rounded up to one 4 KiB page
/// (measured: any payload of 1..=0xFF0 bytes gives 0x1000, 0xFF1 gives 0x2000).
const APPBAR_SECTION_SIZE: i64 = 0x1000;
/// Header of a shell32 block, which `SHLockShared` checked and skipped: its first u32 is the payload size.
const BLOCK_HEADER: usize = 0x10;
/// Payload sizes accepted in that header: at least one 32-bit APPBARDATA, at most the rest of the page.
const PAYLOAD_MIN: u32 = std::mem::size_of::<AppBarData32>() as u32;
const PAYLOAD_MAX: u32 = APPBAR_SECTION_SIZE as u32 - BLOCK_HEADER as u32;
const _: () = assert!(PAYLOAD_MIN == 0x28 && PAYLOAD_MAX == 0xFF0);

/// Whether a section is what shell32 creates for an AppBar message, reached through a handle of the holder
/// that already grants the dock's needs (query, map read, map write): pagefile-backed only and exactly one
/// page long. Pure.
fn shared_block_authentic(b: &SharedBlock) -> Result<(), &'static str> {
    if b.granted & SECTION_NEEDED != SECTION_NEEDED {
        return Err("holder's handle lacks query, map-read or map-write access");
    }
    if !b.is_section {
        return Err("handle is not a section");
    }
    if b.attrs != SEC_COMMIT {
        return Err("section is not plain pagefile-backed memory");
    }
    if b.size != APPBAR_SECTION_SIZE {
        return Err("section size is not that of an AppBar block");
    }
    Ok(())
}

/// Decodes the name of the global atom that shell32 hands out as an `SHAllocShared` value between
/// processes: 32 characters of 6 bits each (`' '..='`'`, least significant first) -> 24 bytes =
/// `"MARC"`, 4 bytes not used here, the section handle (u64) and the pid of the process holding it (u64).
/// Returns `(handle, pid)`; `None` for any other name. Pure.
fn decode_shared_atom(name: &[u16]) -> Option<(u64, u32)> {
    if name.len() != 32 {
        return None;
    }
    let mut b = [0u8; 24];
    for (i, quad) in name.chunks_exact(4).enumerate() {
        let mut v = 0u32;
        for (j, &c) in quad.iter().enumerate() {
            if !(0x20..=0x60).contains(&c) {
                return None;
            }
            v |= ((u32::from(c) - 0x20) & 63) << (6 * j);
        }
        b[i * 3..i * 3 + 3].copy_from_slice(&v.to_le_bytes()[..3]);
    }
    let handle = u64::from_le_bytes(b[8..16].try_into().ok()?);
    let pid = u32::try_from(u64::from_le_bytes(b[16..24].try_into().ok()?)).ok()?;
    (&b[..4] == b"MARC" && handle != 0 && handle % 4 == 0 && pid != 0).then_some((handle, pid))
}

/// ntdll is always loaded, so no `DELAY_LOAD` entry is needed. Not in the `windows` crate: `NtQuerySection`.
mod nt {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HANDLE;
    windows_core::link!("ntdll.dll" "system" fn NtQueryObject(
        h: HANDLE, class: u32, info: *mut c_void, len: u32, ret: *mut u32
    ) -> i32);
    windows_core::link!("ntdll.dll" "system" fn NtQuerySection(
        h: HANDLE, class: u32, info: *mut c_void, len: usize, ret: *mut usize
    ) -> i32);
}

/// Win32 side of `appbar_gate`: reads the name of atom `shared` once and decodes `(section handle, holder
/// pid)` from it. On Windows 11 the value is a global atom (`decode_shared_atom`); anything outside the
/// string-atom range would be taken by `SHLockShared` as a raw pointer, so it is an error (fail closed).
fn resolve_shared_atom(shared: u64) -> Result<(u64, u32), String> {
    if !(0xC000..=0xFFFF).contains(&shared) {
        return Err(format!("value 0x{shared:X} is not a shell shared-memory atom"));
    }
    let mut name = [0u16; 64];
    // SAFETY: `name` is writable for its whole length; an unknown atom returns 0.
    let n = unsafe { GlobalGetAtomNameW(shared as u16, &mut name) } as usize;
    decode_shared_atom(&name[..n.min(name.len())])
        .ok_or_else(|| format!("atom 0x{shared:X} is not a shell shared block"))
}

/// A sender's AppBar block as the dock holds it: its own duplicate of the holder's section handle and, once
/// mapped (`map_block`), its own view of that same duplicate. Both are released on drop, on every path.
struct SenderBlock {
    dup: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl SenderBlock {
    /// The 32-bit APPBARDATA at the start of the payload. Only valid once `map_block` has accepted the view:
    /// `BLOCK_HEADER + PAYLOAD_MIN <= APPBAR_SECTION_SIZE`, the length of the view.
    fn payload(&self) -> *mut AppBarData32 {
        // SAFETY: offset `BLOCK_HEADER` lies inside the mapped page (checked by `map_block` before it returns).
        unsafe { self.view.Value.cast::<u8>().add(BLOCK_HEADER).cast::<AppBarData32>() }
    }
}

impl Drop for SenderBlock {
    fn drop(&mut self) {
        if !self.view.Value.is_null() {
            // SAFETY: view mapped by `map_block`, unmapped exactly once.
            let _ = unsafe { UnmapViewOfFile(self.view) };
        }
        // SAFETY: duplicate created by `dup_section`, closed exactly once.
        let _ = unsafe { CloseHandle(self.dup) };
    }
}

/// The holder process, opened once with `PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION`: the same
/// handle serves the integrity check and the duplication. Closed on drop.
struct HolderProcess(HANDLE);

impl Drop for HolderProcess {
    fn drop(&mut self) {
        // SAFETY: process handle opened by `open_holder`, closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Win32 side of `appbar_gate`: opens the holder process once. Failure is an `Err` (fail closed).
fn open_holder(pid: u32) -> Result<HolderProcess, String> {
    let access = PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION;
    // SAFETY: no preconditions; failure is an Err.
    let h = unsafe { OpenProcess(access, false, pid) }.map_err(|e| format!("holder process not opened: {e}"))?;
    Ok(HolderProcess(h))
}

/// Win32 side of `appbar_gate`: duplicates section handle `handle` of the holder into the dock with
/// `DUPLICATE_SAME_ACCESS`, so the dock never gets a right the holder's handle did not have, reads the access
/// granted to it (`NtQueryObject(ObjectBasicInformation)`) and, only when that includes `SECTION_NEEDED`, the
/// shape of the section, without mapping it. Every failure is an `Err`; the duplicate is closed on drop.
fn dup_section(p: &HolderProcess, handle: u64) -> Result<(SenderBlock, SharedBlock), String> {
    let mut local = HANDLE::default();
    // SAFETY: `p.0` is a process handle with PROCESS_DUP_HANDLE; `local` writable; same access as the source.
    unsafe {
        let src = HANDLE(handle as usize as *mut c_void);
        DuplicateHandle(p.0, src, GetCurrentProcess(), &mut local, 0, false, DUPLICATE_SAME_ACCESS)
    }
    .map_err(|e| format!("handle 0x{handle:X} not duplicated: {e}"))?;
    let b = SenderBlock { dup: local, view: MEMORY_MAPPED_VIEW_ADDRESS::default() };
    // PUBLIC_OBJECT_BASIC_INFORMATION: Attributes, GrantedAccess, HandleCount, PointerCount, Reserved[10].
    let mut info = [0u32; 14];
    // SAFETY: `info` is writable for the declared 56 bytes; `b.dup` is our own handle; no wait.
    let st = unsafe {
        let out = info.as_mut_ptr().cast();
        nt::NtQueryObject(b.dup, 0 /* ObjectBasicInformation */, out, 56, std::ptr::null_mut())
    };
    if st < 0 {
        return Err(format!("granted access not read (NTSTATUS 0x{:08X})", st as u32));
    }
    let granted = info[1];
    if granted & SECTION_NEEDED != SECTION_NEEDED {
        // Rejected by `shared_block_authentic`; the section is not queried through a handle lacking the rights.
        return Ok((b, SharedBlock { granted, is_section: false, attrs: 0, size: 0 }));
    }
    let (is_section, attrs, size) = query_section(b.dup)?;
    Ok((b, SharedBlock { granted, is_section, attrs, size }))
}

/// Win32 side of `appbar_gate`: maps the duplicate that passed the shape check (the same handle, no new
/// lookup) for one page and checks its header as `SHLockShared` did: the payload size at offset 0 must be in
/// `PAYLOAD_MIN..=PAYLOAD_MAX`, so the payload (from offset `BLOCK_HEADER`) lies inside the view.
fn map_block(mut b: SenderBlock) -> Result<SenderBlock, String> {
    #[cfg(test)]
    SENDER_MAPS.with(|c| c.set(c.get() + 1));
    // SAFETY: `b.dup` is a one-page section handle already granted map read/write (`SECTION_NEEDED`); the
    // view asks for no more than that and is unmapped on drop.
    b.view = unsafe { MapViewOfFile(b.dup, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, APPBAR_SECTION_SIZE as usize) };
    if b.view.Value.is_null() {
        return Err(format!("section not mapped: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the view is page-aligned and APPBAR_SECTION_SIZE bytes long, so offset 0 holds a readable u32;
    // volatile because the sender can change it, and it is read only once.
    let n = unsafe { b.view.Value.cast::<u32>().read_volatile() };
    if !(PAYLOAD_MIN..=PAYLOAD_MAX).contains(&n) {
        return Err(format!("payload size 0x{n:X} in the block header is out of range"));
    }
    Ok(b)
}

/// `(is a section, AllocationAttributes, MaximumSize)` of a handle of ours, from its object type and
/// `NtQuerySection(SectionBasicInformation)`; neither maps the object. Errors are `Err`.
fn query_section(h: HANDLE) -> Result<(bool, u32, i64), String> {
    // OBJECT_TYPE_INFORMATION starts with the type name (UNICODE_STRING) whose buffer follows it.
    let mut buf = [0u64; 64];
    let mut ret = 0u32;
    // SAFETY: `buf` is writable for the declared 512 bytes; any handle value is safe to query (no wait).
    let st = unsafe { nt::NtQueryObject(h, 2 /* ObjectTypeInformation */, buf.as_mut_ptr().cast(), 512, &mut ret) };
    if st < 0 {
        return Err(format!("object type not read (NTSTATUS 0x{:08X})", st as u32));
    }
    // SAFETY: `buf` is 8-aligned and starts with the UNICODE_STRING written by the call.
    let name = unsafe { &*buf.as_ptr().cast::<UNICODE_STRING>() };
    let (len, ptr) = (usize::from(name.Length) / 2, name.Buffer.0.cast_const());
    let (start, end) = (buf.as_ptr() as usize, buf.as_ptr() as usize + 512);
    if (ptr as usize) < start || (ptr as usize) + len * 2 > end {
        return Err("object type name outside the buffer".into());
    }
    // SAFETY: `ptr..ptr + len` lies inside `buf` (checked above) and was written by the call.
    let type_name = unsafe { std::slice::from_raw_parts(ptr, len) };
    if type_name != "Section".encode_utf16().collect::<Vec<u16>>().as_slice() {
        return Ok((false, 0, 0));
    }
    // SECTION_BASIC_INFORMATION: BaseAddress, AllocationAttributes, MaximumSize.
    #[repr(C)]
    struct BasicInfo {
        base: *mut c_void,
        attrs: u32,
        size: i64,
    }
    let mut info = BasicInfo { base: std::ptr::null_mut(), attrs: 0, size: 0 };
    let n = std::mem::size_of::<BasicInfo>();
    // SAFETY: `info` is writable for `n` bytes; the handle is a section (checked above).
    let st = unsafe {
        let out = (&mut info as *mut BasicInfo).cast();
        nt::NtQuerySection(h, 0 /* SectionBasicInformation */, out, n, std::ptr::null_mut())
    };
    if st < 0 {
        return Err(format!("section not queried (NTSTATUS 0x{:08X})", st as u32));
    }
    Ok((true, info.attrs, info.size))
}

// Test probe: counts maps of a sender's section (`map_block`), per thread (tests run in parallel).
#[cfg(test)]
thread_local! {
    static SENDER_MAPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `ABM_GETTASKBARPOS`: answers with the dock's tray area and edge, written into the sender's block.
fn get_taskbar_pos(b: &SenderBlock) -> LRESULT {
    let g = state().geom;
    let a = area_rect(&g);
    let p = b.payload();
    // SAFETY: `p` is the APPBARDATA inside the dock's own mapped view (`SenderBlock::payload`); volatile writes
    // to memory the sender shares, updated in place.
    unsafe {
        std::ptr::addr_of_mut!((*p).rc).write_volatile([a.left, a.top, a.right, a.bottom]);
        std::ptr::addr_of_mut!((*p).edge).write_volatile(g.edge);
    }
    LRESULT(1)
}

/// `ABM_QUERYPOS`/`ABM_SETPOS`: copies the sender's APPBARDATA into a block of the dock's own for Explorer's
/// PID, forwards, and copies Explorer's answer back into the sender's block.
fn forward_with_shared(wp: WPARAM, m: AppBarMsg, b: &SenderBlock) -> LRESULT {
    let Some(ex) = find_explorer_tray() else { return LRESULT(0) };
    let ex_pid = window_pid(ex);
    if ex_pid == 0 {
        return LRESULT(0);
    }
    let old = b.payload();
    // SAFETY: `old` is the APPBARDATA inside the dock's own mapped view; read once into a local copy.
    let copy = unsafe { old.read_volatile() };
    // SAFETY: `copy` is a local 32-bit APPBARDATA of the declared size; the new block is destined for Explorer.
    let new_h = unsafe {
        SHAllocShared(Some((&copy as *const AppBarData32).cast()), std::mem::size_of::<AppBarData32>() as u32, ex_pid)
    };
    if new_h.is_invalid() {
        log!("tray: AppBar, SHAllocShared failed");
        return LRESULT(0);
    }
    let mut m2 = m;
    m2.shared = new_h.0 as usize as u64;
    m2.pid = ex_pid;
    let cds = COPYDATASTRUCT {
        dwData: 0,
        cbData: std::mem::size_of::<AppBarMsg>() as u32,
        lpData: (&mut m2 as *mut AppBarMsg).cast::<c_void>(),
    };
    let mut res = 0usize;
    // SAFETY: `cds` and `m2` live for the whole call, which marshals the data to Explorer.
    let sent = unsafe {
        SendMessageTimeoutW(
            ex,
            WM_COPYDATA,
            wp,
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
            3000,
            Some(&mut res),
        )
    };
    let ok = sent.0 != 0;
    if ok {
        // SAFETY: block created above by the dock for Explorer's PID (not a value received from outside).
        let back = unsafe { SHLockShared(new_h, ex_pid) };
        if !back.is_null() {
            // SAFETY: `back` holds the 32-bit APPBARDATA allocated above; `old` lies inside the dock's view.
            unsafe { old.write_volatile(back.cast::<AppBarData32>().read_unaligned()) };
            // SAFETY: `back` obtained from SHLockShared above.
            let _ = unsafe { SHUnlockShared(back) };
        }
        // SAFETY: handle created by SHAllocShared for `ex_pid`, freed exactly once.
        let _ = unsafe { SHFreeShared(new_h, ex_pid) };
    } else {
        log!("tray: AppBar, forwarding to Explorer failed (block left in place, it may still be needed)");
    }
    if ok {
        LRESULT(res as isize)
    } else {
        LRESULT(0)
    }
}

// ------------------------------------------------------------------ tray-fwd thread

fn enqueue_forward(wp: usize, bytes: &[u8]) {
    if !crate::taskbar::is_hidden() || YIELDED.load(Relaxed) || bytes.is_empty() {
        return;
    }
    {
        let mut q = FWD_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= FWD_QUEUE_MAX {
            q.pop_front();
            if !FWD_OVERFLOW_LOGGED.swap(true, Relaxed) {
                log!("tray: Explorer forwarding queue full ({FWD_QUEUE_MAX}), dropping the oldest messages");
            }
        }
        q.push_back((wp, bytes.to_vec()));
    }
    FWD_CV.notify_one();
}

fn forward_main() {
    loop {
        let item = {
            let mut q = FWD_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if FWD_STOP.load(Relaxed) {
                    return;
                }
                if let Some(it) = q.pop_front() {
                    break it;
                }
                q = match FWD_CV.wait_timeout(q, Duration::from_millis(500)) {
                    Ok((g, _)) => g,
                    Err(e) => e.into_inner().0,
                };
            }
        };
        if !crate::taskbar::is_hidden() || YIELDED.load(Relaxed) {
            continue;
        }
        let Some(ex) = find_explorer_tray() else { continue };
        let (wp, mut data) = item;
        let cds = COPYDATASTRUCT { dwData: 1, cbData: data.len() as u32, lpData: data.as_mut_ptr().cast::<c_void>() };
        let mut res = 0usize;
        // SAFETY: `cds` and `data` live for the whole call; the system copies the bytes to Explorer.
        let r = unsafe {
            SendMessageTimeoutW(
                ex,
                WM_COPYDATA,
                WPARAM(wp),
                LPARAM(&cds as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                2000,
                Some(&mut res),
            )
        };
        if r.0 == 0 {
            log!("tray: forwarding to Explorer failed or timed out");
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::DUPLICATE_HANDLE_OPTIONS;

    #[test]
    fn placement_events_are_filtered_before_any_window_call() {
        use windows::Win32::UI::WindowsAndMessaging::{
            EVENT_OBJECT_LOCATIONCHANGE as LOC, EVENT_OBJECT_SHOW as SHOW,
        };
        let rc = RECT { left: 0, top: 0, right: 10, bottom: 10 };
        let p = Placement { pid: 7, item: rc, bar: rc, top: false, until_ms: 5_000, placed: 0, moves: 0 };
        assert!(place_should_handle(&p, SHOW, 0x10, true, 1_000));
        assert!(!place_should_handle(&p, SHOW, 0x10, true, 5_001), "armed window expired");
        assert!(!place_should_handle(&p, SHOW, 0x10, false, 1_000), "other process");
        assert!(!place_should_handle(&p, LOC, 0x10, true, 1_000), "location change before any placement");
        let placed = Placement { placed: 0x10, moves: 1, ..p };
        assert!(place_should_handle(&placed, LOC, 0x10, true, 1_000));
        assert!(!place_should_handle(&placed, LOC, 0x20, true, 1_000), "a different window");
        assert!(!place_should_handle(&placed, SHOW, 0x20, true, 1_000), "only the first menu is taken");
        assert!(!place_should_handle(&Placement { moves: PLACE_MAX_MOVES, ..placed }, LOC, 0x10, true, 1_000));
    }

    #[test]
    fn elevated_owner_gating() {
        const AB: &str = r"C:\Program Files (x86)\MSI Afterburner\MSIAfterburner.exe";
        let facts = |same_process, at_most_medium, recorded_image, current_image| OwnerFacts {
            same_process,
            at_most_medium,
            recorded_image,
            current_image,
        };
        let elevated = facts(true, false, AB, Some(AB));
        // Medium (or lower) owners: always allowed, system callbacks included, image never consulted.
        let medium = facts(true, true, "", None);
        for trust in [false, true] {
            assert_eq!(callback_verdict(&medium, WM_USER + 1, trust), Verdict::Allowed);
            assert_eq!(callback_verdict(&medium, 0x0111, trust), Verdict::Allowed);
            assert_eq!(callback_verdict(&medium, 0x1_0000, trust), Verdict::Allowed);
        }
        // The window no longer belongs to the registered process: nothing, whatever the rest says.
        for trust in [false, true] {
            assert_eq!(callback_verdict(&facts(false, true, AB, Some(AB)), 0x8001, trust), Verdict::NotOwner);
            assert_eq!(callback_verdict(&facts(false, false, AB, Some(AB)), 0x8001, trust), Verdict::NotOwner);
        }
        // Elevated / unknown owners: blocked while the switch is off.
        assert_eq!(callback_verdict(&elevated, 0x8001, false), Verdict::ElevatedNotTrusted);
        assert_eq!(callback_verdict(&elevated, 0x0111, false), Verdict::ElevatedNotTrusted);
        // Switch on: application ranges only (private, WM_APP, RegisterWindowMessage).
        for cb in [WM_USER, 0x7FFF, 0x8000, 0x8001, 0xBFFF, 0xC000, 0xC123, 0xFFFF] {
            assert_eq!(callback_verdict(&elevated, cb, true), Verdict::Allowed, "0x{cb:X}");
        }
        for cb in [0, 0x0111, 0x0112, 0x007B, WM_USER - 1, 0x1_0000, u32::MAX] {
            assert_eq!(callback_verdict(&elevated, cb, true), Verdict::SystemMessage, "0x{cb:X}");
        }
        // Switch on: the process must still run the image recorded at registration (case-insensitive).
        let upper = AB.to_uppercase();
        assert_eq!(callback_verdict(&facts(true, false, AB, Some(&upper)), 0x8001, true), Verdict::Allowed);
        let other = facts(true, false, AB, Some(r"C:\Users\x\AppData\Local\Temp\MSIAfterburner.exe"));
        assert_eq!(callback_verdict(&other, 0x8001, true), Verdict::ImageMismatch);
        assert_eq!(callback_verdict(&facts(true, false, AB, None), 0x8001, true), Verdict::ImageMismatch);
        assert_eq!(callback_verdict(&facts(true, false, "", Some(AB)), 0x8001, true), Verdict::ImageMismatch);
        assert_eq!(callback_verdict(&facts(true, false, "", Some("")), 0x8001, true), Verdict::ImageMismatch);
        // The refusal hint names the Settings option and the registry value.
        assert!(NOT_TRUSTED_HINT.contains("Settings > Modules > Tray"));
        assert!(NOT_TRUSTED_HINT.contains("TrustElevatedTrayIcons"));
    }

    #[test]
    fn nim_add_records_the_owner_image() {
        let mut items = Vec::new();
        let mut next = 1;
        let d = nid(0x10, 1, NIF_MESSAGE);
        let path = r"C:\Program Files (x86)\MSI Afterburner\MSIAfterburner.exe";
        assert!(apply_nim(&mut items, &mut next, &d, Some(10), IconUpdate::Keep, path).ret);
        assert_eq!((items[0].exe.as_str(), items[0].image.as_str()), ("MSIAfterburner.exe", path));
    }

    fn nid(hwnd: u32, uid: u32, flags: u32) -> ShellTrayData {
        let mut d: ShellTrayData = from_prefix(&[]);
        d.signature = TRAY_SIGNATURE;
        d.nid.hwnd = hwnd;
        d.nid.uid = uid;
        d.nid.flags = flags;
        d
    }

    fn bmp() -> IconUpdate {
        IconUpdate::Set(Arc::new(IconBitmap::new(16, 16)), Arc::new(IconBitmap::new(16, 16)))
    }

    #[test]
    fn layout_e_firma() {
        assert_eq!(std::mem::size_of::<ShellTrayData>(), 964);
        assert!(parse_shell_tray(&[0u8; 100]).is_none());
        let mut raw = vec![0u8; 100];
        raw[..4].copy_from_slice(&TRAY_SIGNATURE.to_le_bytes());
        assert!(parse_shell_tray(&raw).is_some());
        assert!(parse_shell_tray(&raw[..20]).is_none());
    }

    #[test]
    fn encode_per_version() {
        assert_eq!(encode(0, 7, 100, 200, 0x202), (7, 0x202));
        assert_eq!(encode(3, 7, 100, 200, NIN_SELECT), (7, NIN_SELECT as isize));
        let (wp, lp) = encode(4, 7, 100, 200, 0x202);
        assert_eq!(wp & 0xFFFF, 100);
        assert_eq!(wp >> 16, 200);
        assert_eq!(lp & 0xFFFF, 0x202);
        assert_eq!(lp >> 16, 7);
        // negative coordinates: signed 16 bit
        let (wp, _) = encode(4, 1, -5, -9, 0x200);
        assert_eq!((wp & 0xFFFF) as u16 as i16, -5);
        assert_eq!(((wp >> 16) & 0xFFFF) as u16 as i16, -9);
    }

    #[test]
    fn sequenze_clic() {
        assert_eq!(click_events(TrayClick::Left, 0), vec![WM_LBUTTONDOWN, WM_LBUTTONUP]);
        assert_eq!(click_events(TrayClick::Left, 4), vec![WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT]);
        assert_eq!(click_events(TrayClick::Right, 3), vec![WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU]);
        assert_eq!(click_events(TrayClick::Middle, 4), vec![WM_MBUTTONDOWN, WM_MBUTTONUP]);
        assert_eq!(click_events(TrayClick::Double, 0), vec![WM_LBUTTONDBLCLK, WM_LBUTTONUP]);
        assert_eq!(click_events(TrayClick::Double, 3), vec![WM_LBUTTONDBLCLK, WM_LBUTTONUP, NIN_SELECT]);
    }

    #[test]
    fn menu_anchor_on_icon_center_outside_bar() {
        let bar = RECT { left: 1000, top: 2065, right: 2800, bottom: 2146 };
        let cursor = POINT { x: 2210, y: 2130 };
        let bottom = TrayGeometry { bar, tray_area: bar, edge: 3 };
        let p = anchor_point(&bottom, 2200, cursor);
        assert_eq!((p.x, p.y), (2200, 2064));
        let top = TrayGeometry { bar, tray_area: bar, edge: 1 };
        let p = anchor_point(&top, 2200, cursor);
        assert_eq!((p.x, p.y), (2200, 2147));
        let none = TrayGeometry::default();
        let p = anchor_point(&none, 2200, cursor);
        assert_eq!((p.x, p.y), (cursor.x, cursor.y));
    }

    #[test]
    fn ciclo_nim() {
        let mut items = Vec::new();
        let mut next = 1u64;
        let mut d = nid(0x1234, 1, NIF_ICON | NIF_MESSAGE);
        d.nid.callback = 0x8001;
        d.message = NIM_ADD;
        let r = apply_nim(&mut items, &mut next, &d, Some(10), bmp(), "a.exe");
        assert!(r.ret);
        assert_eq!(r.change, 2);
        assert_eq!(items.len(), 1);
        // double ADD = 0
        let r = apply_nim(&mut items, &mut next, &d, Some(10), bmp(), "a.exe");
        assert!(!r.ret);
        // MODIFY of unknown icon = 0
        let mut m = nid(0x9999, 1, NIF_TIP);
        m.message = NIM_MODIFY;
        assert!(!apply_nim(&mut items, &mut next, &m, Some(10), IconUpdate::Keep, "").ret);
        // MODIFY of known icon: tooltip
        let mut m = nid(0x1234, 1, NIF_TIP);
        m.message = NIM_MODIFY;
        m.nid.tip[0] = b'A' as u16;
        let r = apply_nim(&mut items, &mut next, &m, Some(10), IconUpdate::Keep, "");
        assert!(r.ret);
        assert_eq!(r.change, 1);
        assert_eq!(items[0].tip, "A");
        // hide
        let mut m = nid(0x1234, 1, NIF_STATE);
        m.message = NIM_MODIFY;
        m.nid.state = NIS_HIDDEN;
        m.nid.state_mask = NIS_HIDDEN;
        assert_eq!(apply_nim(&mut items, &mut next, &m, Some(10), IconUpdate::Keep, "").change, 2);
        // SETVERSION
        let mut v = nid(0x1234, 1, 0);
        v.message = NIM_SETVERSION;
        v.nid.version = 4;
        assert!(apply_nim(&mut items, &mut next, &v, Some(10), IconUpdate::Keep, "").ret);
        assert_eq!(items[0].version, 4);
        v.nid.version = 5;
        assert!(!apply_nim(&mut items, &mut next, &v, Some(10), IconUpdate::Keep, "").ret);
        // SETFOCUS always 1
        let mut f = nid(0x7777, 9, 0);
        f.message = NIM_SETFOCUS;
        assert!(apply_nim(&mut items, &mut next, &f, None, IconUpdate::Keep, "").ret);
        // DELETE
        let mut x = nid(0x1234, 1, 0);
        x.message = NIM_DELETE;
        assert!(apply_nim(&mut items, &mut next, &x, Some(10), IconUpdate::Keep, "").ret);
        assert!(items.is_empty());
        assert!(!apply_nim(&mut items, &mut next, &x, Some(10), IconUpdate::Keep, "").ret);
    }

    #[test]
    fn add_con_owner_morto_e_guid() {
        let mut items = Vec::new();
        let mut next = 1u64;
        let mut d = nid(0x10, 1, NIF_ICON | NIF_GUID);
        d.nid.guid = GUID::from_u128(0x7820ae73_23e3_4229_82c1_e41cb67d5b9c);
        d.message = NIM_ADD;
        assert!(!apply_nim(&mut items, &mut next, &d, None, bmp(), "").ret);
        assert!(apply_nim(&mut items, &mut next, &d, Some(5), bmp(), "").ret);
        // same GUID, different hWnd: replaces the owner
        d.nid.hwnd = 0x20;
        assert!(apply_nim(&mut items, &mut next, &d, Some(6), bmp(), "").ret);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].owner, 0x20);
        assert_eq!(items[0].pid, 6);
        assert_eq!(guid_string(&items[0].guid.unwrap_or(GUID::zeroed())), "7820ae73-23e3-4229-82c1-e41cb67d5b9c");
    }

    fn crafted(message: u32, pid: u32) -> AppBarMsg {
        let mut m: AppBarMsg = from_prefix(&[]); // all zero
        m.message = message;
        m.shared = 0x1234; // a guessed handle value: must never be locked
        m.pid = pid;
        m
    }

    fn appbar_bytes(m: &AppBarMsg) -> Vec<u8> {
        // SAFETY: AppBarMsg is repr(C), Copy and made only of integers (explicit _pad fields, size asserted 0x40).
        unsafe { std::slice::from_raw_parts((m as *const AppBarMsg).cast::<u8>(), std::mem::size_of::<AppBarMsg>()) }
            .to_vec()
    }

    /// Shell window (Explorer, medium) for the tests that need a real window of another process.
    fn shell_window() -> Option<(WPARAM, u32)> {
        // SAFETY: no preconditions.
        let h = unsafe { windows::Win32::UI::WindowsAndMessaging::GetShellWindow() };
        let pid = window_pid(h);
        (!h.0.is_null() && pid != 0).then_some((WPARAM(h.0 as usize), pid))
    }

    /// Inverse of `decode_shared_atom`: the atom name shell32 builds for `handle` held by process `pid`.
    fn shell_atom_name(sig: &[u8; 4], handle: u64, pid: u64) -> Vec<u16> {
        let mut b = sig.to_vec();
        b.extend([0u8; 4].iter().chain(&handle.to_le_bytes()).chain(&pid.to_le_bytes()));
        b.chunks(3)
            .flat_map(|t| {
                let v = u32::from(t[0]) | u32::from(t[1]) << 8 | u32::from(t[2]) << 16;
                (0..4).map(move |j| match (v >> (6 * j)) & 63 {
                    0 => u16::from(b'`'),
                    c => c as u16 + 0x20,
                })
            })
            .collect()
    }

    /// Registers a shell32-style atom naming `handle` held by `pid`; the caller deletes it.
    fn add_shell_atom(handle: u64, pid: u32) -> u16 {
        let mut name = shell_atom_name(b"MARC", handle, u64::from(pid));
        name.push(0);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let atom = unsafe { windows::Win32::System::DataExchange::GlobalAddAtomW(PCWSTR(name.as_ptr())) };
        assert_ne!(atom, 0);
        atom
    }

    // Entry-point tests use only ABM_GETTASKBARPOS: an accepted ABM_QUERYPOS / ABM_SETPOS would be forwarded to
    // Explorer's tray window, which tests must never do. The pure table covers all three messages.
    #[test]
    fn forged_appbar_values_never_reach_the_map() {
        // SAFETY: no preconditions.
        let own = unsafe { GetCurrentProcessId() };
        // SAFETY: the name is a valid NUL-terminated string; the atom is deleted below.
        let foreign_atom = unsafe { windows::Win32::System::DataExchange::GlobalAddAtomW(w!("dock-test-not-shell")) };
        assert_ne!(foreign_atom, 0);
        // Decodes, but names a handle this process does not have.
        let dangling = add_shell_atom(0x7FFF_FFFC, own);
        // A real window of another process: a window must not be what lets a value through.
        let mut wps = vec![WPARAM(0)];
        match shell_window() {
            Some((wp, _)) => wps.push(wp),
            None => eprintln!("no shell window: forged values behind a real window not exercised"),
        }
        let m = crafted(ABM_GETTASKBARPOS, own);
        for wp in wps {
            // A raw value (SHLockShared would take it as a pointer), none, a foreign atom, a dangling handle.
            for shared in [m.shared, 0, u64::from(foreign_atom), u64::from(dangling)] {
                let maps = SENDER_MAPS.with(|c| c.get());
                let bytes = appbar_bytes(&AppBarMsg { shared, ..m });
                assert_eq!(handle_appbar(wp, LPARAM(0), &bytes).0, 0, "0x{shared:X}");
                assert_eq!(SENDER_MAPS.with(|c| c.get()), maps, "0x{shared:X} reached the map");
            }
        }
        // SAFETY: atoms added above.
        unsafe {
            windows::Win32::System::DataExchange::GlobalDeleteAtom(foreign_atom);
            windows::Win32::System::DataExchange::GlobalDeleteAtom(dangling);
        }
    }

    #[test]
    fn appbar_gate_rule_table() {
        const OWN: u32 = 100;
        const APP: u32 = 8120;
        const ELEV: u32 = 666;
        const OTHER: u32 = 9000;
        // The "process handle" of the tests is the pid itself.
        let medium = |p: &u32| *p != 0 && *p != ELEV;
        let sender_medium = |p: u32| p != 0 && p != ELEV;
        let good = SharedBlock { granted: 0x000F_001F, is_section: true, attrs: SEC_COMMIT, size: APPBAR_SECTION_SIZE };
        let rights = |granted: u32| SharedBlock { granted, ..good };
        // (holder, dock elevated, sender window pids, section, None = accept / Some(reason))
        let rows: [(u32, bool, &[u32], SharedBlock, Option<&str>); 24] = [
            (OWN, true, &[], good, Some("held by the dock itself")),
            (OWN, true, &[APP], good, Some("held by the dock itself")),
            // Unelevated developer build (DOCK_NO_MANIFEST=1): shell32 makes the dock the holder.
            (OWN, false, &[], good, None),
            (OWN, false, &[APP], good, None),
            (OWN, false, &[ELEV], good, Some("sender window's process 666 runs above medium")),
            (ELEV, true, &[], good, Some("above medium")),
            (ELEV, true, &[ELEV], good, Some("above medium")),
            // No sender window: the holder rule alone.
            (APP, true, &[], good, None),
            // Sender known: alive and at most medium; it need not be the holder (shell window, frame host).
            (APP, true, &[APP], good, None),
            (APP, true, &[APP, APP], good, None),
            (APP, true, &[OTHER], good, None),
            (APP, true, &[APP, OTHER], good, None),
            (APP, true, &[0], good, Some("sender window's process is gone")),
            (APP, true, &[APP, 0], good, Some("sender window's process is gone")),
            (APP, true, &[ELEV], good, Some("sender window's process 666 runs above medium")),
            (APP, true, &[OTHER, ELEV], good, Some("sender window's process 666 runs above medium")),
            // Only the access the holder's own handle was granted counts.
            (APP, true, &[], rights(SECTION_NEEDED), None),
            (APP, true, &[], rights(SECTION_QUERY.0 | SECTION_MAP_READ.0), Some("lacks query, map-read or map-write")),
            (APP, true, &[APP], rights(SECTION_QUERY.0 | SECTION_MAP_WRITE.0), Some("lacks query, map-read")),
            (APP, true, &[], rights(SECTION_MAP_READ.0 | SECTION_MAP_WRITE.0), Some("lacks query, map-read")),
            (APP, true, &[APP], SharedBlock { is_section: false, ..good }, Some("not a section")),
            (APP, true, &[], SharedBlock { attrs: 0x0100_0000 | SEC_COMMIT, ..good }, Some("not plain pagefile")),
            (APP, true, &[APP], SharedBlock { attrs: 0x0080_0000, ..good }, Some("not plain pagefile")),
            (APP, true, &[], SharedBlock { size: 0x2000, ..good }, Some("size is not that of an AppBar block")),
        ];
        for msg in [ABM_GETTASKBARPOS, ABM_QUERYPOS, ABM_SETPOS] {
            for (holder, elevated, senders, shape, want) in rows {
                let m = crafted(msg, OWN);
                let (opens, dups, maps) = (std::cell::Cell::new(0), std::cell::Cell::new(0), std::cell::Cell::new(0));
                let got = appbar_gate(
                    &m,
                    senders,
                    OWN,
                    Some(elevated),
                    sender_medium,
                    |s| {
                        assert_eq!(s, 0x1234, "the value received is resolved, and only once");
                        Ok((0x9B0, holder))
                    },
                    |p| {
                        opens.set(opens.get() + 1);
                        Ok(p)
                    },
                    medium,
                    |p, h| {
                        assert_eq!((*p, h), (holder, 0x9B0), "the decoded handle is duplicated from the opened holder");
                        dups.set(dups.get() + 1);
                        Ok((7u8, shape))
                    },
                    |b| {
                        maps.set(maps.get() + 1);
                        Ok(b)
                    },
                );
                let row = format!("msg {msg}, holder {holder}, elevated {elevated}, senders {senders:?}, {shape:?}");
                match want {
                    None => {
                        assert_eq!(got, Ok(7), "{row}");
                        assert_eq!((opens.get(), dups.get(), maps.get()), (1, 1, 1), "{row}");
                    }
                    Some(why) => {
                        let text = got.expect_err(&row);
                        assert!(text.contains(why), "{row}: {text}");
                        let pids = text.contains("claimed pid 100") && text.contains(&format!("holder pid {holder}"));
                        assert!(pids, "{text}");
                        assert_eq!(maps.get(), 0, "{row}: rejected blocks are never mapped");
                        let early = why.contains("dock itself") || why.contains("sender window");
                        assert_eq!(opens.get(), usize::from(!early), "{row}: holder process opened only when needed");
                        let pre_dup = early || why.contains("medium");
                        assert_eq!(dups.get(), usize::from(!pre_dup), "{row}: integrity checked before duplicating");
                    }
                }
            }
        }
        // Every failed Win32 step rejects, with the holder pid once it is known.
        let m = crafted(ABM_GETTASKBARPOS, OWN);
        // Elevation unknown (token unreadable) + block held by the dock: rejected like an elevated dock.
        let never = |_: u32| -> Result<u32, String> { panic!("the holder is not opened") };
        let dock_block = |_: u64| Ok((0x9B0, OWN));
        let text =
            appbar_gate(&m, &[], OWN, None, sender_medium, dock_block, never, medium, |_, _| Ok((7u8, good)), |b| Ok(b));
        assert!(text.is_err_and(|t| t.contains("held by the dock itself")), "unknown elevation fails closed");
        let ok_resolve = |_: u64| Ok((0x9B0, APP));
        let ok_open = |p: u32| Ok(p);
        let ok_dup = |_: &u32, _: u64| Ok((7u8, good));
        let ok_map = |b: u8| Ok(b);
        let gate = |resolve: &dyn Fn(u64) -> Result<(u64, u32), String>,
                    open: &dyn Fn(u32) -> Result<u32, String>,
                    dup: &dyn Fn(&u32, u64) -> Result<(u8, SharedBlock), String>,
                    map: &dyn Fn(u8) -> Result<u8, String>| {
            appbar_gate(&m, &[], OWN, Some(true), sender_medium, resolve, open, medium, dup, map).unwrap_err()
        };
        let text = gate(&|_| Err("no atom".into()), &ok_open, &ok_dup, &ok_map);
        assert!(text.contains("holder pid -") && text.contains("no atom"), "{text}");
        let text = gate(&ok_resolve, &|_| Err("not opened".into()), &ok_dup, &ok_map);
        assert!(text.contains("holder pid 8120") && text.contains("not opened"), "{text}");
        let text = gate(&ok_resolve, &ok_open, &|_, _| Err("no dup".into()), &ok_map);
        assert!(text.contains("holder pid 8120") && text.contains("no dup"), "{text}");
        let text = gate(&ok_resolve, &ok_open, &ok_dup, &|_| Err("bad header".into()));
        assert!(text.contains("holder pid 8120") && text.contains("bad header"), "{text}");
    }

    #[test]
    fn shell_shared_atom_names_decode() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let enc = shell_atom_name;
        // Captured on Windows 11: a medium sender's block for a medium receiver 0x8F0C, and one for Explorer.
        assert_eq!(decode_shared_atom(&u("-%D4#!`````L)````````PPC````````")), Some((0x9B0, 0x8F0C)));
        assert_eq!(decode_shared_atom(&u("-%D4#!`````9J!```````0Q2````````")), Some((0x6A64, 0x4B14)));
        assert_eq!(enc(b"MARC", 0x9B0, 0x8F0C), u("-%D4#!`````L)````````PPC````````"), "encoder matches");
        assert_eq!(decode_shared_atom(&u("dock-test-not-shell")), None, "wrong length");
        assert_eq!(decode_shared_atom(&enc(b"MARD", 0x9B0, 0x8F0C)), None, "wrong signature");
        assert_eq!(decode_shared_atom(&u("-%D4#!`````L)````````PPCa```````")), None, "character out of range");
        assert_eq!(decode_shared_atom(&enc(b"MARC", 0, 0x8F0C)), None, "null handle");
        assert_eq!(decode_shared_atom(&enc(b"MARC", 0x9B2, 0x8F0C)), None, "not a handle value");
        assert_eq!(decode_shared_atom(&enc(b"MARC", 0x9B0, 0)), None, "no pid");
        assert_eq!(decode_shared_atom(&enc(b"MARC", 0x9B0, 1 << 32)), None, "pid does not fit 32 bits");
    }

    #[test]
    fn shell32_blocks_are_mapped_through_the_duplicate_and_pointers_are_not() {
        // SAFETY: no preconditions.
        let own = unsafe { GetCurrentProcessId() };
        // Within one process SHAllocShared returns a heap pointer: never a shell atom.
        let data = [0u8; 0x28];
        // SAFETY: `data` is readable for the declared size; the block is freed right after.
        let local = unsafe { SHAllocShared(Some(data.as_ptr().cast()), data.len() as u32, own) };
        assert!(resolve_shared_atom(local.0 as usize as u64).is_err());
        // SAFETY: block allocated above for our pid, freed once.
        let _ = unsafe { SHFreeShared(local, own) };
        let Some((_, ex)) = shell_window() else {
            eprintln!("no shell window: real cross-process block not exercised");
            return;
        };
        // A block for Explorer, as an AppBar app would send it: resolved, duplicated with the holder's own access
        // and accepted; a bigger one is a section too, but not an AppBar block (and is not mapped).
        for (n, ok) in [(0x28usize, true), (0x2000, false)] {
            let buf: Vec<u8> = (0..n).map(|i| (i as u8) ^ 0x5A).collect();
            // SAFETY: `buf` is readable for `n` bytes; the block is freed right after.
            let h = unsafe { SHAllocShared(Some(buf.as_ptr().cast()), n as u32, ex) };
            assert!(!h.is_invalid());
            let (handle, holder) = resolve_shared_atom(h.0 as usize as u64).expect("a shell32 block is an atom");
            assert_eq!(holder, ex, "n = {n:#X}");
            let p = open_holder(holder).expect("holder opened");
            if !process_at_most_medium(p.0) {
                // A CI runner (UAC off) runs its shell above medium: no medium holder to exercise there.
                // SAFETY: block allocated above for the shell's pid, freed once.
                let _ = unsafe { SHFreeShared(h, ex) };
                eprintln!("the shell runs above medium integrity: real cross-process block not exercised");
                return;
            }
            let (b, shape) = dup_section(&p, handle).expect("duplicated with the same access");
            assert_eq!((shape.is_section, shape.attrs), (true, SEC_COMMIT), "n = {n:#X}");
            // Evidence for the access rule: shell32's handle in the holder grants query, map read and map write.
            assert_eq!(shape.granted & SECTION_NEEDED, SECTION_NEEDED, "n = {n:#X}: granted {:#X}", shape.granted);
            assert_eq!(shared_block_authentic(&shape).is_ok(), ok, "n = {n:#X}: {shape:?}");
            if ok {
                let b = map_block(b).expect("a real shell32 header is accepted");
                // Layout of the real block: payload size at offset 0, payload from BLOCK_HEADER.
                // SAFETY: the view is one page long (`map_block`); both ranges lie inside it.
                let (size, payload) = unsafe {
                    let base = b.view.Value.cast::<u8>();
                    (base.cast::<u32>().read(), std::slice::from_raw_parts(base.add(BLOCK_HEADER), n).to_vec())
                };
                assert_eq!((size, payload), (n as u32, buf), "header and payload of a real shell32 block");
            }
            // SAFETY: block allocated above for Explorer's pid, freed once.
            let _ = unsafe { SHFreeShared(h, ex) };
        }
    }

    /// A pagefile-backed section of this process (`size` bytes, `PAGE_READWRITE | flags`).
    fn own_section(size: u32, flags: windows::Win32::System::Memory::PAGE_PROTECTION_FLAGS) -> HANDLE {
        use windows::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE};
        // SAFETY: page-file backed (INVALID_HANDLE_VALUE), unnamed, default security; closed by the caller.
        unsafe {
            CreateFileMappingW(
                windows::Win32::Foundation::INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE | flags,
                0,
                size,
                PCWSTR::null(),
            )
        }
        .expect("section created")
    }

    #[test]
    fn sections_of_this_process_through_the_entry_point() {
        use windows::Win32::System::Memory::{SEC_COMMIT as COMMIT, SEC_RESERVE};
        // SAFETY: no preconditions.
        let own = unsafe { GetCurrentProcessId() };
        // A block held by this process is allowed only while it is not elevated (developer build rule).
        let holder_ok = !crate::shell_thread::is_elevated();
        use windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE;
        // A message-only window of this process, i.e. of the holder (not a tray window class).
        // SAFETY: plain STATIC message-only window, destroyed below.
        let mine = unsafe {
            CreateWindowExW(Default::default(), w!("STATIC"), w!(""), Default::default(), 0, 0, 0, 0,
                Some(HWND_MESSAGE), None, None, None)
        }
        .expect("message-only window");
        // (wParam, sender allowed): no window, the holder's own window, another process's window (allowed when
        // that process runs at most at medium, as Explorer normally does).
        let mut wps = vec![(WPARAM(0), true), (WPARAM(mine.0 as usize), true)];
        match shell_window() {
            Some((wp, pid)) => wps.push((wp, pid_at_most_medium(pid))),
            None => eprintln!("no shell window: a sender window of another process not exercised"),
        }
        let sentinel = AppBarData32 { cb_size: 0x28, hwnd: 0x10, callback: 0x20, edge: 0xEE, rc: [-7; 4], lparam: 9 };
        // (section size, flags, header size field, accepted, mapped)
        let cases = [
            (0x1000, COMMIT, Some(0x28), true, true),
            (0x1000, COMMIT, Some(0xFF0), true, true),
            (0x1000, COMMIT, Some(0x27), false, true),
            (0x1000, COMMIT, Some(0xFF1), false, true),
            (0x1000, COMMIT, Some(0), false, true),
            (0x2000, COMMIT, Some(0x28), false, false),
            (0x1000, SEC_RESERVE, None, false, false),
        ];
        for (wp, allowed) in wps {
            for (size, flags, header, accepted, mapped) in cases {
                let row = format!("size {size:#X}, flags {:#X}, header {header:?}, wp {:#X}", flags.0, wp.0);
                let h = own_section(size, flags);
                // SAFETY: `h` is our own section of at least one page; the view is unmapped below.
                let view = unsafe { MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0x1000) };
                assert!(!view.Value.is_null(), "{row}");
                let p = view.Value.cast::<u8>();
                if let Some(n) = header {
                    // SAFETY: committed one-page view: offset 0 and the payload at BLOCK_HEADER lie inside it.
                    unsafe {
                        p.cast::<u32>().write(n);
                        p.add(BLOCK_HEADER).cast::<AppBarData32>().write(sentinel);
                    }
                }
                let atom = add_shell_atom(h.0 as usize as u64, own);
                let maps = SENDER_MAPS.with(|c| c.get());
                let bytes = appbar_bytes(&AppBarMsg { shared: u64::from(atom), ..crafted(ABM_GETTASKBARPOS, own) });
                let ret = handle_appbar(wp, LPARAM(0), &bytes).0;
                assert_eq!(ret, isize::from(accepted && holder_ok && allowed), "{row}");
                assert_eq!(SENDER_MAPS.with(|c| c.get()) - maps, usize::from(mapped && holder_ok && allowed), "{row}");
                if header.is_some() {
                    // SAFETY: as above.
                    let got = unsafe { p.add(BLOCK_HEADER).cast::<AppBarData32>().read() };
                    let g = state().geom;
                    let a = area_rect(&g);
                    let written = ([a.left, a.top, a.right, a.bottom], g.edge);
                    let (rc, edge) = if ret == 1 { written } else { ([-7; 4], 0xEE) };
                    assert_eq!((got.rc, got.edge), (rc, edge), "{row}: only an accepted block is written");
                    assert_eq!((got.cb_size, got.hwnd, got.callback, got.lparam), (0x28, 0x10, 0x20, 9), "{row}");
                }
                // SAFETY: atom, view and section created above, released once.
                unsafe {
                    windows::Win32::System::DataExchange::GlobalDeleteAtom(atom);
                    let _ = UnmapViewOfFile(view);
                    let _ = CloseHandle(h);
                }
            }
        }
        // A handle that is not a section: rejected before any map.
        // SAFETY: no preconditions; the event is closed below.
        let ev = unsafe { windows::Win32::System::Threading::CreateEventW(None, true, false, PCWSTR::null()) }
            .expect("event created");
        let atom = add_shell_atom(ev.0 as usize as u64, own);
        let maps = SENDER_MAPS.with(|c| c.get());
        let bytes = appbar_bytes(&AppBarMsg { shared: u64::from(atom), ..crafted(ABM_GETTASKBARPOS, own) });
        assert_eq!(handle_appbar(WPARAM(0), LPARAM(0), &bytes).0, 0);
        assert_eq!(SENDER_MAPS.with(|c| c.get()), maps, "an event is never mapped");
        // SAFETY: atom and event created above, released once.
        unsafe {
            windows::Win32::System::DataExchange::GlobalDeleteAtom(atom);
            let _ = CloseHandle(ev);
        }
        // A good one-page section, but the holder's handle grants only query + map read: rejected before any map,
        // never widened to map write. The reduced handle is made by this process from its own full one.
        let h = own_section(0x1000, COMMIT);
        // SAFETY: `h` is our own section; the view is unmapped below.
        let view = unsafe { MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0x1000) };
        assert!(!view.Value.is_null());
        // SAFETY: committed one-page view: offset 0 lies inside it.
        unsafe { view.Value.cast::<u32>().write(0x28) };
        let mut ro = HANDLE::default();
        // SAFETY: own process and own handle; the requested access is a subset of the source's.
        unsafe {
            let me = GetCurrentProcess();
            let access = (SECTION_QUERY | SECTION_MAP_READ).0;
            DuplicateHandle(me, h, me, &mut ro, access, false, DUPLICATE_HANDLE_OPTIONS(0))
        }
        .expect("read-only duplicate");
        let atom = add_shell_atom(ro.0 as usize as u64, own);
        let maps = SENDER_MAPS.with(|c| c.get());
        let bytes = appbar_bytes(&AppBarMsg { shared: u64::from(atom), ..crafted(ABM_GETTASKBARPOS, own) });
        assert_eq!(handle_appbar(WPARAM(0), LPARAM(0), &bytes).0, 0, "a handle without map write is rejected");
        assert_eq!(SENDER_MAPS.with(|c| c.get()), maps, "a handle without map write is never mapped");
        // SAFETY: atom, view, handles and window created above, released once.
        unsafe {
            windows::Win32::System::DataExchange::GlobalDeleteAtom(atom);
            let _ = UnmapViewOfFile(view);
            let _ = CloseHandle(ro);
            let _ = CloseHandle(h);
            let _ = DestroyWindow(mine);
        }
    }

    #[test]
    fn host_rect_spans_monitor_edge() {
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let bar = RECT { left: 1200, top: 2065, right: 2640, bottom: 2146 };
        let g = TrayGeometry { bar, tray_area: RECT { left: 2198, top: 2065, right: 2582, bottom: 2146 }, edge: 3 };
        let r = host_rect(&g, mon);
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 2065, 2582, 2160));
        let top_bar = RECT { left: 1200, top: 12, right: 2640, bottom: 93 };
        let g = TrayGeometry { bar: top_bar, tray_area: top_bar, edge: 1 };
        let r = host_rect(&g, mon);
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 2640, 93));
        // no geometry yet: falls back to the tray area / bar
        let g = TrayGeometry { bar: EMPTY_RECT, tray_area: EMPTY_RECT, edge: 3 };
        assert_eq!(host_rect(&g, mon).right, 0);
    }

    #[test]
    fn handle_a_32_bit_con_segno() {
        assert_eq!(ext(0xFFFF_FFFE), -2);
        assert_eq!(ext(0x1234), 0x1234);
    }
}
