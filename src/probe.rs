//! Wake probe: an instrumented build that says what wakes each thread of the dock, above all while the bar is
//! hidden. Diagnostic only (ROADMAP 9.2 row "wakeups.dock_hidden": 0.25 context switches per second).
//!
//! SWITCH, read ONCE by `init` at start: the environment variable `DOCK_WAKE_PROBE=1`, or the marker file
//! `wakeprobe.on` in the data folder (`%ProgramData%\mini-oled-dock`, next to config.json and dock.log). The
//! marker is the practical way for a dock started by the scheduled task, which does not inherit a shell's
//! environment. Off (the default), every call below is one load of a null pointer and a branch, the UI thread
//! runs the release's own `GetMessageW` loop, and nothing is allocated, installed or started.
//!
//! STALL SWITCH (wsC Q7, live check L1 B9 (iii) and (iv)), only while the probe is on: the marker `wakeprobe.stall` in
//! the data folder holds a decimal number of ms (at most `STALL_MAX_MS`; anything else is off), read once by `init`.
//! More than 60 s after start, the first message whose beat is not due (the last beat under 1 s old) sleeps that long
//! before its dispatch, once per process, then logs "wake probe: stalled N ms (beat age A ms)".
//!
//! ON, this module counts, with relaxed atomics in one heap block (about 50 KB, allocated once) and a few
//! thread-locals, never a lock or an allocation on a counted path:
//!  * UI thread: the release loop is replaced by `MsgWaitForMultipleObjectsEx` + `PeekMessageW(PM_REMOVE)`, so
//!    every return from the wait is a counted wake, separately from the posted messages it then drains (by id,
//!    `WM_TIMER` by timer id, `SHELLHOOK` by HSHELL code, with the time spent dispatching each). A
//!    thread-local cause mask, set by the WinEvent callback, the sent-message hook and the mouse hook while a
//!    wake is served, tells which wakes carried no posted message at all. Sent messages (from other threads and
//!    processes, and from the thread itself) are counted by a thread-local `WH_CALLWNDPROC` hook.
//!  * WinEvent callbacks (`apps`, the tray placement hook): by event, by owner of the window (dock, Explorer,
//!    another process, no window) and by owner image file name, and how many pass each module's filter.
//!  * Keyboard (hook thread) and mouse (UI thread, popup open) low-level hooks: calls by message and the
//!    messages each one posts to the UI.
//!  * hook, shell and tray-host threads: every `GetMessageW` return by message; shell and tray host also get the
//!    sent-message hook; the tray host's `WM_COPYDATA` calls by kind (NIM op, AppBar, icon rect) and sender
//!    image name. tray-fwd, notif, notif-icons, log, stop-watch, audio callbacks, the config watch callback,
//!    transient workers: counted where they wait.
//!  * A `wake-probe` thread waits 30 s on an event, then reads every thread of the process from the system
//!    (`NtQuerySystemInformation(SystemProcessInformation)`: context switches, CPU, state) and appends one
//!    compact JSON line to `wakeprobe.jsonl` next to dock.log (rotated to `wakeprobe.1.jsonl` at 16 MiB), with
//!    the bar state (hidden, shown, popup, settings) and its time in each state during the interval.
//!
//! WHAT THE PROBE ITSELF ADDS (on): one wake every 30 s on its own thread (plus about 1-3 ms of CPU for the
//! system snapshot and the JSON line); a transient VirtualAlloc of 0.5-2 MiB for that snapshot, released at
//! once; per counted event 1-3 uncontended atomic adds; per WinEvent and tray call a `GetWindowThreadProcessId`
//! (the owner's image name is resolved with OpenProcess/QueryFullProcessImageNameW once per pid per 30 s); per UI
//! wake one more `PeekMessageW` (the final empty one) and a few `Instant::now` reads; one `WH_CALLWNDPROC` call
//! per message sent to the UI, shell or tray-host thread; one config watch callback per JSON line (wakeprobe.jsonl
//! sits in the data folder the watch observes). None of it posts a message or arms a timer.

use std::cell::{Cell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicIsize, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering::Acquire, Ordering::Relaxed, Ordering::Release,
};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Instant;

use serde_json::{json, Map, Value};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, HANDLE, HLOCAL, HMODULE, HWND, LPARAM, LRESULT, WAIT_FAILED, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::System::DataExchange::{GetClipboardFormatNameW, COPYDATASTRUCT};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Memory::{VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount64};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, GetCurrentThreadId, GetThreadDescription, OpenProcess, OpenThread,
    QueryFullProcessImageNameW, SetEvent, WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetWindowThreadProcessId, MsgWaitForMultipleObjectsEx, PeekMessageW,
    RegisterWindowMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, CWPSTRUCT, HHOOK, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WH_CALLWNDPROC, WM_COPYDATA, WM_QUIT, WM_TIMER,
};

use crate::log;

/// Environment variable that switches the probe on (value `1`).
pub const ENV_SWITCH: &str = "DOCK_WAKE_PROBE";
/// Marker file in the data folder that switches the probe on (any content).
pub const MARKER: &str = "wakeprobe.on";
/// Report file in the data folder (one JSON object per line).
pub const REPORT: &str = "wakeprobe.jsonl";
const REPORT_OLD: &str = "wakeprobe.1.jsonl";
const PERIOD_MS: u32 = 30_000;
const ROTATE_BYTES: u64 = 16 * 1024 * 1024;

// ------------------------------------------------------------------ counters

macro_rules! counters {
    ($($id:ident = $name:literal),* $(,)?) => {
        /// Scalar counters (one relaxed atomic each); the report shows the non-zero ones by name.
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum C { $($id),* }
        const C_NAMES: &[&str] = &[$($name),*];
    };
}

counters! {
    UiWakes = "ui.wakes",
    UiPosted = "ui.posted",
    UiEmptyWakes = "ui.wakes_without_posted",
    UiPeekUs = "ui.peek_us",
    UiDispatchUs = "ui.dispatch_us",
    UiSentX = "ui.sent_from_other_threads",
    UiSentL = "ui.sent_same_thread",
    Heartbeat = "ui.heartbeat_setevent",
    WeCalls = "winevent.calls",
    WeUs = "winevent.us",
    MouseCalls = "mouse_ll.calls",
    HookWakes = "hook.getmessage_returns",
    KbCalls = "hook.kb_calls",
    KbSwallowed = "hook.kb_swallowed",
    KbInjected = "hook.kb_injected",
    ShellWakes = "shell.getmessage_returns",
    ShellSentX = "shell.sent_from_other_threads",
    TrayWakes = "tray.getmessage_returns",
    TraySentX = "tray.sent_from_other_threads",
    TrayCopyData = "tray.copydata_calls",
    FwdItem = "tray_fwd.wake_signal",
    NotifTimeout = "notif.wake_timeout",
    NotifCmd = "notif.wake_command",
    NotifChanged = "notif.changed_callbacks",
    IconWakes = "notif_icons.wakes",
    LogLines = "log.lines",
    StopWatch = "stop_watch.wakes",
    AudioVolume = "audio.volume_callbacks",
    AudioDevice = "audio.device_callbacks",
    Spawned = "transient_threads_started",
    ImportPolls = "start_import.poll_wakes",
    OwnerExited = "launcher.owner_exited_waits",
    ConfigWatch = "config.watch_callbacks",
    Reports = "probe.reports",
}

const C_COUNT: usize = C_NAMES.len();

/// Thread whose message loop a per-thread table belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Th {
    Ui = 0,
    Hook = 1,
    Shell = 2,
    Tray = 3,
}

const TH_ALL: [Th; 4] = [Th::Ui, Th::Hook, Th::Shell, Th::Tray];

impl Th {
    fn name(self) -> &'static str {
        match self {
            Th::Ui => "ui",
            Th::Hook => "hook",
            Th::Shell => "shell",
            Th::Tray => "tray",
        }
    }
    fn wakes(self) -> C {
        match self {
            Th::Ui => C::UiWakes,
            Th::Hook => C::HookWakes,
            Th::Shell => C::ShellWakes,
            Th::Tray => C::TrayWakes,
        }
    }
    fn sent_x(self) -> C {
        match self {
            Th::Ui => C::UiSentX,
            Th::Shell => C::ShellSentX,
            // The hook thread has no windows (no sent-message hook there).
            Th::Hook | Th::Tray => C::TraySentX,
        }
    }
}

/// WinEvent hook that delivered an event.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Src {
    /// `apps::winevent_proc`: the three hooks of the UI thread (the taskbar's share included).
    Apps = 1,
    /// `tray_host::place_winevent_proc` (tray-host thread, only while a tray menu is being placed).
    TrayPlace = 2,
}

/// Filter a WinEvent passed: the post that follows it.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Stage {
    /// `WM_DOCK_APPS_DIRTY` posted by `apps::winevent_proc`.
    AppsDirty = 1,
    /// `WM_DOCK_TASKBAR_EVENT` posted by `taskbar::on_raw_event`.
    TaskbarEvent = 2,
    /// `WM_HOST_PLACE_MOVE` posted by `tray_host::place_winevent_proc`.
    PlaceMove = 3,
}

/// Who posted a message counted with `post_by`.
pub const BY_KB_HOOK: u32 = 1;
pub const BY_MOUSE_HOOK: u32 = 2;

// Keys of the per-thread tables: tag in the top 4 bits, never 0.
const K_POSTED: u32 = 1 << 28;
const K_TIMER: u32 = 2 << 28;
const K_SENTX: u32 = 3 << 28;
const K_SENTL: u32 = 4 << 28;
const K_CAUSE: u32 = 5 << 28;
const K_SHELLHOOK: u32 = 6 << 28;
const K_KB: u32 = 7 << 28;
const K_POSTBY: u32 = 8 << 28;
const K_MOUSE: u32 = 9 << 28;
const K_COPYDATA: u32 = 10 << 28;
const TIMER_THREAD_FLAG: u32 = 1 << 24;
// Keys of the WinEvent table: tag | src << 24 | class, slot or stage << 16 | event (16 bits).
const W_CLASS: u32 = 1 << 28;
const W_PROC: u32 = 2 << 28;
const W_PASS: u32 = 3 << 28;

// Wake causes (thread-local mask of the UI thread, reset at every wake).
const CAUSE_POSTED: u8 = 1;
const CAUSE_WINEVENT: u8 = 2;
const CAUSE_SENT: u8 = 4;
const CAUSE_MOUSE: u8 = 8;

// Owner classes of a WinEvent window.
const CLASS_DOCK: u32 = 0;
const CLASS_EXPLORER: u32 = 1;
const CLASS_OTHER: u32 = 2;
const CLASS_NOWIN: u32 = 3;
const CLASS_NAMES: [&str; 4] = ["dock", "explorer", "other", "no_window"];

// Process-name slots: fixed ones, then one per image name met.
const PROCS: usize = 128;
const NAME_MAX: usize = 62;
const SLOT_DOCK: u8 = 0;
const SLOT_NOWIN: u8 = 1;
const SLOT_DENIED: u8 = 2;
const SLOT_FULL: u8 = 3;
const FIXED_SLOTS: [&str; 4] = ["(dock)", "(no window)", "(denied)", "(table full)"];

const TABLE: usize = 512;

#[derive(Default)]
struct Slot {
    key: AtomicU32,
    n: AtomicU32,
    us: AtomicU64,
}

/// Open-addressing counter table: keys are only ever added, counts are drained (swap to 0) by the report.
struct Table {
    slots: [Slot; TABLE],
    overflow: AtomicU32,
}

impl Table {
    fn bump(&self, key: u32, us: u64) {
        let mut i = (key.wrapping_mul(0x9E37_79B1) >> 16) as usize % TABLE;
        for _ in 0..TABLE {
            let s = &self.slots[i];
            let k = s.key.load(Relaxed);
            let mine = k == key
                || (k == 0
                    && match s.key.compare_exchange(0, key, Relaxed, Relaxed) {
                        Ok(_) => true,
                        Err(cur) => cur == key,
                    });
            if mine {
                s.n.fetch_add(1, Relaxed);
                if us != 0 {
                    s.us.fetch_add(us, Relaxed);
                }
                return;
            }
            i = (i + 1) % TABLE;
        }
        self.overflow.fetch_add(1, Relaxed);
    }

    /// (key, count, µs) of every key counted since the last drain; resets the counts.
    fn drain(&self) -> Vec<(u32, u32, u64)> {
        let mut out = Vec::new();
        for s in &self.slots {
            let k = s.key.load(Relaxed);
            if k == 0 {
                continue;
            }
            let n = s.n.swap(0, Relaxed);
            let us = s.us.swap(0, Relaxed);
            if n != 0 || us != 0 {
                out.push((k, n, us));
            }
        }
        out
    }
}

struct ProcSlot {
    ready: AtomicU8,
    len: AtomicU8,
    name: UnsafeCell<[u8; NAME_MAX]>,
}

struct Probe {
    c: [AtomicU64; C_COUNT],
    th: [Table; 4],
    we: Table,
    procs: [ProcSlot; PROCS],
    proc_next: AtomicU32,
}

// SAFETY: every field is an atomic except the slot names, each written once by the thread that claimed the slot
// (`proc_next.fetch_add`) before its `ready` flag is set with Release, and only read after an Acquire of it.
unsafe impl Sync for Probe {}

impl Probe {
    fn new_boxed() -> Box<Probe> {
        let layout = std::alloc::Layout::new::<Probe>();
        // SAFETY: `Probe` is not zero-sized and all-zero bytes are a valid value of every field (atomics, byte arrays
        // in UnsafeCell); the block comes from the global allocator with `Probe`'s layout, as `Box::from_raw` needs.
        let p = unsafe {
            let raw = std::alloc::alloc_zeroed(layout).cast::<Probe>();
            if raw.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(raw)
        };
        for (i, n) in FIXED_SLOTS.iter().enumerate() {
            p.publish(i, n.as_bytes());
        }
        p.proc_next.store(FIXED_SLOTS.len() as u32, Relaxed);
        p
    }

    fn add(&self, c: C, v: u64) {
        self.c[c as usize].fetch_add(v, Relaxed);
    }

    fn publish(&self, i: usize, name: &[u8]) {
        let s = &self.procs[i];
        let n = name.len().min(NAME_MAX);
        // SAFETY: slot `i` belongs to the caller alone (a fixed slot at creation, or one just claimed with
        // `proc_next.fetch_add`); readers wait for `ready`.
        unsafe { (&mut *s.name.get())[..n].copy_from_slice(&name[..n]) };
        s.len.store(n as u8, Relaxed);
        s.ready.store(1, Release);
    }

    fn proc_name(&self, i: usize) -> Option<&[u8]> {
        let s = self.procs.get(i)?;
        if s.ready.load(Acquire) == 0 {
            return None;
        }
        let n = s.len.load(Relaxed) as usize;
        // SAFETY: written once before `ready` (Release, read above with Acquire), never written again.
        Some(unsafe { &(&*s.name.get())[..n] })
    }

    /// Slot of an image name (lowercase file name), adding it if new.
    fn slot_for(&self, name: &[u8]) -> u8 {
        let used = (self.proc_next.load(Acquire) as usize).min(PROCS);
        for i in 0..used {
            if self.proc_name(i) == Some(name) {
                return i as u8;
            }
        }
        let i = self.proc_next.fetch_add(1, Relaxed) as usize;
        if i >= PROCS {
            return SLOT_FULL;
        }
        self.publish(i, name);
        i as u8
    }

    fn class_of(&self, slot: u8) -> u32 {
        match slot {
            SLOT_DOCK => CLASS_DOCK,
            SLOT_NOWIN => CLASS_NOWIN,
            s if self.proc_name(s as usize) == Some(b"explorer.exe".as_slice()) => CLASS_EXPLORER,
            _ => CLASS_OTHER,
        }
    }

    fn slot_label(&self, slot: u32) -> String {
        self.proc_name(slot as usize).map_or_else(|| format!("slot{slot}"), |n| String::from_utf8_lossy(n).into_owned())
    }
}

static PROBE: AtomicPtr<Probe> = AtomicPtr::new(std::ptr::null_mut());
static OWN_PID: AtomicU32 = AtomicU32::new(0);
static UI_TID: AtomicU32 = AtomicU32::new(0);
static SHELLHOOK: AtomicU32 = AtomicU32::new(0);
static STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
static REPORTER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
/// The stall switch: ms of the one stall still pending, 0 = off or done.
static STALL_MS: AtomicU32 = AtomicU32::new(0);
/// `GetTickCount64` at `init`: the stall comes only after `STALL_AFTER_MS`.
static STARTED_MS: AtomicU64 = AtomicU64::new(0);

/// Marker of the stall switch, next to `MARKER`.
pub const STALL_MARKER: &str = "wakeprobe.stall";
const STALL_AFTER_MS: u64 = 60_000;
/// The longest stall the switch accepts (ms).
const STALL_MAX_MS: u32 = 60_000;

#[inline]
fn get() -> Option<&'static Probe> {
    let p = PROBE.load(Acquire);
    // SAFETY: null, or a Box leaked by `init` and never freed.
    unsafe { p.as_ref() }
}

thread_local! {
    static CAUSE: Cell<u8> = const { Cell::new(0) };
    static CUR_TH: Cell<u8> = const { Cell::new(0) };
    static PID_KEYS: [Cell<u32>; PID_CACHE] = const { [const { Cell::new(0) }; PID_CACHE] };
    static PID_VALS: [Cell<u64>; PID_CACHE] = const { [const { Cell::new(0) }; PID_CACHE] };
}

const PID_CACHE: usize = 64;
const PID_TTL_MS: u64 = 30_000;

#[inline]
fn mark(bit: u8) {
    CAUSE.with(|c| c.set(c.get() | bit));
}

fn us_since(t: Instant) -> u64 {
    t.elapsed().as_micros() as u64
}

fn m16(m: u32) -> u32 {
    m.min(0xFFFF)
}

fn ev16(e: u32) -> u32 {
    e.min(0xFFFF)
}

fn msg_key(msg: &MSG) -> u32 {
    if msg.message == WM_TIMER {
        let thread = if msg.hwnd.0.is_null() { TIMER_THREAD_FLAG } else { 0 };
        K_TIMER | thread | (msg.wParam.0 as u32 & 0xFF_FFFF)
    } else {
        K_POSTED | m16(msg.message)
    }
}

/// Image-name slot of a process, through a small per-thread cache (30 s per pid).
fn slot_of_pid(p: &Probe, pid: u32) -> u8 {
    if pid == 0 {
        return SLOT_NOWIN;
    }
    if pid == OWN_PID.load(Relaxed) {
        return SLOT_DOCK;
    }
    let i = (pid >> 2) as usize % PID_CACHE;
    // SAFETY: no preconditions.
    let now = unsafe { GetTickCount64() };
    let cached = PID_KEYS.with(|k| k[i].get()) == pid;
    let val = PID_VALS.with(|v| v[i].get());
    if cached && (val >> 8) > now {
        return val as u8;
    }
    let slot = resolve_name(p, pid);
    PID_KEYS.with(|k| k[i].set(pid));
    PID_VALS.with(|v| v[i].set(((now + PID_TTL_MS) << 8) | u64::from(slot)));
    slot
}

fn resolve_name(p: &Probe, pid: u32) -> u8 {
    // SAFETY: plain query of another process; the handle is closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return SLOT_DENIED;
    };
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; `h` is a valid process handle.
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    // SAFETY: handle opened above.
    let _ = unsafe { CloseHandle(h) };
    if !ok {
        return SLOT_DENIED;
    }
    let name = image_file_name(&buf[..(len as usize).min(buf.len())]);
    p.slot_for(&name)
}

/// Lowercase ASCII file name of a path (non-ASCII as '?'), at most NAME_MAX bytes.
fn image_file_name(path: &[u16]) -> Vec<u8> {
    let start = path.iter().rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/')).map_or(0, |i| i + 1);
    path[start..].iter().take(NAME_MAX).map(|&c| if c < 0x80 { (c as u8).to_ascii_lowercase() } else { b'?' }).collect()
}

// ------------------------------------------------------------------ start / stop

/// Reads the switch (environment, then the marker file) once; if on, allocates the counters and starts the
/// `wake-probe` reporter thread. Called once, after the data folder and the logger are up.
pub fn init() {
    let env_on = std::env::var_os(ENV_SWITCH).is_some_and(|v| v == "1");
    let data = crate::datadir::config_path().and_then(|p| p.parent().map(Path::to_path_buf));
    let marker_on = !env_on && data.as_ref().is_some_and(|d| d.join(MARKER).is_file());
    if !env_on && !marker_on {
        return;
    }
    let Some(dir) = crate::datadir::log_path().and_then(|p| p.parent().map(Path::to_path_buf)) else {
        log!("wake probe: requested, but the data folder is not writable: probe stays off");
        return;
    };
    let report = dir.join(REPORT);
    // SAFETY: no preconditions.
    OWN_PID.store(unsafe { GetCurrentProcessId() }, Relaxed);
    // SAFETY: static NUL-terminated name; the same id `ui` registers for its shell hook.
    SHELLHOOK.store(unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) }, Relaxed);
    // SAFETY: unnamed manual-reset event, owned by this module for the life of the process.
    let stop = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(h) => h,
        Err(e) => {
            log!("wake probe: CreateEventW failed ({e}): probe stays off");
            return;
        }
    };
    STOP_EVENT.store(stop.0 as isize, Relaxed);
    let p: &'static Probe = Box::leak(Probe::new_boxed());
    PROBE.store(std::ptr::from_ref(p).cast_mut(), Release);
    let switch = if env_on { "env" } else { "marker" };
    // SAFETY: no preconditions.
    STARTED_MS.store(unsafe { GetTickCount64() }, Relaxed);
    let stall = data.as_deref().map_or(0, read_stall);
    if stall != 0 {
        STALL_MS.store(stall, Relaxed);
        log!("wake probe: one stall of {stall} ms armed, more than {} s after start", STALL_AFTER_MS / 1000);
    }
    let path = report.clone();
    let spawned = std::thread::Builder::new()
        .name("wake-probe".into())
        .stack_size(256 * 1024)
        .spawn(move || reporter_main(p, &path, switch));
    match spawned {
        Ok(j) => *REPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(j),
        Err(e) => log!("wake probe: reporter thread not started: {e} (counters run, no report)"),
    }
    log!("wake probe ON ({switch}): a JSON line every {} s to {}", PERIOD_MS / 1000, report.display());
}

/// The stall switch's marker in `dir`, read once and bounded: 0 (off) when absent or unreadable.
fn read_stall(dir: &Path) -> u32 {
    use std::io::Read;
    let mut text = String::new();
    match std::fs::File::open(dir.join(STALL_MARKER)).and_then(|f| f.take(32).read_to_string(&mut text)) {
        Ok(_) => stall_ms(&text),
        Err(_) => 0,
    }
}

/// The stall switch's value: a decimal number of ms from 1 to `STALL_MAX_MS`; anything else is off (0).
fn stall_ms(text: &str) -> u32 {
    text.trim().parse::<u32>().ok().filter(|&n| n <= STALL_MAX_MS).unwrap_or(0)
}

/// The UI loop's heartbeat for a message (wsC Q7), before its dispatch: as the release loop, only for messages not
/// addressed to the dock window (those beat in `wnd_proc`, so no message beats twice). While the stall switch is pending
/// and more than 60 s have passed since start, the one stall goes on the first message whose beat is not due (the last
/// beat under 1 s old): it begins as close after a beat as a real stall can, the case of the 5.6 s tolerance.
fn beat_or_stall(msg: &MSG) {
    let beat = msg.hwnd != crate::hwnd() && crate::watchdog::beat_if_due();
    let stall = STALL_MS.load(Relaxed);
    // SAFETY: no preconditions.
    if stall == 0 || beat || unsafe { GetTickCount64() }.saturating_sub(STARTED_MS.load(Relaxed)) <= STALL_AFTER_MS {
        return;
    }
    let age = crate::watchdog::beat_age_ms();
    if age >= crate::watchdog::BEAT_MIN_GAP_MS {
        return; // a message to the dock window, whose beat `wnd_proc` gives
    }
    STALL_MS.store(0, Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(u64::from(stall)));
    log!("wake probe: stalled {stall} ms (beat age {age} ms)");
}

/// Writes the last (partial) report and stops the reporter. No-op when the probe is off.
pub fn stop() {
    if get().is_none() {
        return;
    }
    let raw = STOP_EVENT.load(Relaxed);
    if raw != 0 {
        // SAFETY: event created by `init`, never closed.
        let _ = unsafe { SetEvent(HANDLE(raw as *mut c_void)) };
    }
    let j = REPORTER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(j) = j {
        let _ = j.join();
    }
}

// ------------------------------------------------------------------ counting points

/// One more on a scalar counter.
#[inline]
pub fn bump(c: C) {
    if let Some(p) = get() {
        p.add(c, 1);
    }
}

/// `GetMessageW` of the hook, shell or tray-host thread returned `msg`.
#[inline]
pub fn thread_msg(th: Th, msg: &MSG) {
    if let Some(p) = get() {
        p.add(th.wakes(), 1);
        p.th[th as usize].bump(msg_key(msg), 0);
    }
}

/// A hook or callback of thread `th` posted `msg` (to the UI): `by` = `BY_*`.
#[inline]
pub fn post_by(th: Th, by: u32, msg: u32) {
    if let Some(p) = get() {
        p.th[th as usize].bump(K_POSTBY | (by & 0xFF) << 16 | m16(msg), 0);
    }
}

/// Low-level keyboard hook callback (hook thread): message, KBDLLHOOKSTRUCT flags, swallowed or not.
#[inline]
pub fn kb(wm: u32, flags: u32, swallowed: bool) {
    let Some(p) = get() else { return };
    p.add(C::KbCalls, 1);
    if swallowed {
        p.add(C::KbSwallowed, 1);
    }
    if flags & 0x10 != 0 {
        p.add(C::KbInjected, 1); // LLKHF_INJECTED
    }
    p.th[Th::Hook as usize].bump(K_KB | m16(wm), 0);
}

/// Low-level mouse hook callback of the popups (UI thread).
#[inline]
pub fn mouse(wm: u32) {
    let Some(p) = get() else { return };
    mark(CAUSE_MOUSE);
    p.add(C::MouseCalls, 1);
    p.th[Th::Ui as usize].bump(K_MOUSE | m16(wm), 0);
}

/// Times a WinEvent callback; its drop counts it with the time spent.
pub struct Span(Option<(Instant, u32)>);

impl Drop for Span {
    fn drop(&mut self) {
        if let (Some((t, key)), Some(p)) = (self.0, get()) {
            let us = us_since(t);
            p.we.bump(key, us);
            p.add(C::WeUs, us);
        }
    }
}

/// Start of a WinEvent callback: counts it by event and owner of the window. Keep the guard alive to the end.
#[inline]
pub fn winevent(src: Src, event: u32, hwnd: HWND) -> Span {
    let Some(p) = get() else { return Span(None) };
    mark(CAUSE_WINEVENT);
    p.add(C::WeCalls, 1);
    let mut pid = 0u32;
    if !hwnd.0.is_null() {
        // SAFETY: accepts any HWND; a window already gone leaves pid at 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    }
    let slot = slot_of_pid(p, pid);
    let (s, ev) = ((src as u32) << 24, ev16(event));
    p.we.bump(W_PROC | s | u32::from(slot) << 16 | ev, 0);
    Span(Some((Instant::now(), W_CLASS | s | p.class_of(slot) << 16 | ev)))
}

/// A WinEvent passed the filter of `stage` (the module is about to post for it).
#[inline]
pub fn winevent_pass(src: Src, stage: Stage, event: u32) {
    if let Some(p) = get() {
        p.we.bump(W_PASS | (src as u32) << 24 | (stage as u32) << 16 | ev16(event), 0);
    }
}

/// Installs the sent-message counter (thread-local `WH_CALLWNDPROC`) on the calling thread. 0 when off.
pub fn install_send_hook(th: Th) -> isize {
    if get().is_none() {
        return 0;
    }
    CUR_TH.with(|c| c.set(th as u8));
    // SAFETY: thread-specific hook of the calling thread, whose procedure lives in this module: no module needed.
    match unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(send_hook_proc), None, GetCurrentThreadId()) } {
        Ok(h) => h.0 as isize,
        Err(e) => {
            log!("wake probe: WH_CALLWNDPROC on the {} thread failed: {e}", th.name());
            0
        }
    }
}

/// Removes what `install_send_hook` installed (0 = nothing).
pub fn remove_send_hook(raw: isize) {
    if raw != 0 {
        // SAFETY: handle returned by SetWindowsHookExW on this thread, removed once.
        let _ = unsafe { UnhookWindowsHookEx(HHOOK(raw as *mut c_void)) };
    }
}

unsafe extern "system" fn send_hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && lp.0 != 0 {
        if let Some(p) = get() {
            // SAFETY: for WH_CALLWNDPROC lParam points to a CWPSTRUCT valid for the duration of the call.
            let cwp = unsafe { &*(lp.0 as *const CWPSTRUCT) };
            let th = TH_ALL[CUR_TH.with(Cell::get) as usize & 3];
            let t = &p.th[th as usize];
            if wp.0 == 0 {
                // Sent by another thread (or process).
                t.bump(K_SENTX | m16(cwp.message), 0);
                mark(CAUSE_SENT);
                p.add(th.sent_x(), 1);
                if th == Th::Tray && cwp.message == WM_COPYDATA {
                    copydata(p, cwp.wParam, cwp.lParam);
                }
            } else {
                t.bump(K_SENTL | m16(cwp.message), 0);
                if th == Th::Ui {
                    p.add(C::UiSentL, 1);
                }
            }
        }
    }
    // SAFETY: unchanged forwarding to the hook chain.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// A `WM_COPYDATA` to the tray host: by kind (dwData, NIM op) and sender image (wParam = sender window).
fn copydata(p: &Probe, wp: WPARAM, lp: LPARAM) {
    if lp.0 == 0 {
        return;
    }
    // SAFETY: WM_COPYDATA's lParam points to a COPYDATASTRUCT valid for the duration of the message.
    let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
    let dw = cds.dwData.min(15) as u32;
    let op = if cds.dwData == 1 && cds.cbData >= 8 && !cds.lpData.is_null() {
        // SAFETY: at least 8 bytes at lpData, valid for the message; unaligned read of the second DWORD.
        unsafe { (cds.lpData as *const u8).add(4).cast::<u32>().read_unaligned() }.min(15)
    } else {
        0
    };
    let mut pid = 0u32;
    if wp.0 != 0 {
        // SAFETY: accepts any value; a non-window leaves pid at 0.
        unsafe { GetWindowThreadProcessId(HWND(wp.0 as *mut c_void), Some(&mut pid)) };
    }
    let slot = slot_of_pid(p, pid);
    p.add(C::TrayCopyData, 1);
    p.th[Th::Tray as usize].bump(K_COPYDATA | dw << 12 | op << 8 | u32::from(slot), 0);
}

// ------------------------------------------------------------------ UI loop

/// Bar state: 0 hidden, 1 shown, 2 popup open, 3 Settings open, 4 not in the loop yet.
const STATE_NAMES: [&str; 5] = ["hidden", "shown", "popup", "settings", "starting"];

struct Bar {
    cur: usize,
    since: Option<Instant>,
    acc_ms: [u64; 5],
    changes: u32,
}

static BAR: Mutex<Bar> = Mutex::new(Bar { cur: 4, since: None, acc_ms: [0; 5], changes: 0 });

fn bar_state() -> usize {
    if crate::popup::is_open() {
        2
    } else if crate::settings_window::is_open() {
        3
    } else if crate::ui::is_visible() {
        1
    } else {
        0
    }
}

/// State change (UI thread, rare): the only lock of the counted side.
fn set_bar(s: usize) {
    let now = Instant::now();
    let mut b = BAR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = b.since {
        let cur = b.cur;
        b.acc_ms[cur] += now.duration_since(t).as_millis() as u64;
    }
    if b.cur != 4 {
        b.changes += 1;
    }
    b.cur = s;
    b.since = Some(now);
}

/// The UI thread's message loop while the probe is on; false (at once) when it is off, or if the wait fails,
/// and the caller then runs the release loop. Returns true on WM_QUIT.
pub fn ui_loop() -> bool {
    let Some(p) = get() else { return false };
    // SAFETY: no preconditions.
    UI_TID.store(unsafe { GetCurrentThreadId() }, Relaxed);
    let hook = install_send_hook(Th::Ui);
    let shellhook = SHELLHOOK.load(Relaxed);
    let ui = &p.th[Th::Ui as usize];
    let mut msg = MSG::default();
    let mut last_state = usize::MAX;
    let quit = loop {
        let s = bar_state();
        if s != last_state {
            set_bar(s);
            last_state = s;
        }
        // SAFETY: no handles; wakes for any message, sent message, input or event, as GetMessageW would.
        let r = unsafe { MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if r == WAIT_FAILED {
            log!(
                "wake probe: MsgWaitForMultipleObjectsEx failed ({:?}): release loop",
                windows::core::Error::from_thread()
            );
            break false;
        }
        p.add(C::UiWakes, 1);
        CAUSE.with(|c| c.set(0));
        let mut cause: Option<u8> = None;
        let mut quit = false;
        loop {
            let t = Instant::now();
            // SAFETY: `msg` valid and writable; no filter, as the release loop's GetMessageW.
            let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool();
            p.add(C::UiPeekUs, us_since(t));
            if cause.is_none() {
                cause = Some(CAUSE.with(Cell::get) | if got { CAUSE_POSTED } else { 0 });
            }
            if !got {
                break;
            }
            if msg.message == WM_QUIT {
                quit = true;
                break;
            }
            p.add(C::UiPosted, 1);
            if shellhook != 0 && msg.message == shellhook {
                ui.bump(K_SHELLHOOK | (msg.wParam.0 as u32 & 0xFFFF), 0);
            }
            beat_or_stall(&msg);
            let key = msg_key(&msg);
            let t = Instant::now();
            // SAFETY: `msg` was just filled by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let us = us_since(t);
            ui.bump(key, us);
            p.add(C::UiDispatchUs, us);
        }
        let c = cause.unwrap_or(0);
        if c & CAUSE_POSTED == 0 {
            p.add(C::UiEmptyWakes, 1);
        }
        ui.bump(K_CAUSE | u32::from(c), 0);
        if quit {
            break true;
        }
    };
    remove_send_hook(hook);
    quit
}

// ------------------------------------------------------------------ system snapshot

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
    fn NtQueryInformationThread(thread: HANDLE, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

const SYSTEM_PROCESS_INFORMATION: u32 = 5;
const THREAD_QUERY_SET_WIN32_START_ADDRESS: u32 = 9;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

/// `SYSTEM_PROCESS_INFORMATION` (x64, 0x100 bytes), followed by `nthreads` `SYSTEM_THREAD_INFORMATION`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SpInfo {
    next: u32,
    nthreads: u32,
    ws_private: i64,
    hard_faults: u32,
    threads_hwm: u32,
    cycles: u64,
    create: i64,
    user: i64,
    kernel: i64,
    name_len: u16,
    name_max: u16,
    _pad0: u32,
    name_ptr: usize,
    base_prio: i32,
    _pad1: u32,
    pid: usize,
    ppid: usize,
    handles: u32,
    session: u32,
    key: usize,
    peak_vsize: usize,
    vsize: usize,
    page_faults: u32,
    _pad2: u32,
    peak_ws: usize,
    ws: usize,
    quota: [usize; 4],
    pagefile: usize,
    peak_pagefile: usize,
    private: usize,
    io: [i64; 6],
}

/// `SYSTEM_THREAD_INFORMATION` (x64, 0x50 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct StInfo {
    kernel: i64,
    user: i64,
    create: i64,
    wait_time: u32,
    _pad0: u32,
    start: usize,
    pid: usize,
    tid: usize,
    prio: i32,
    base_prio: i32,
    cs: u32,
    state: u32,
    wait_reason: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<SpInfo>() == 0x100);
const _: () = assert!(std::mem::size_of::<StInfo>() == 0x50);

struct Snap {
    proc_time: u64,
    handles: u32,
    private: usize,
    ws: usize,
    threads: Vec<StInfo>,
}

fn snapshot(own: u32) -> Option<Snap> {
    let mut size: usize = 1024 * 1024;
    for _ in 0..5 {
        // SAFETY: fresh private allocation, released below in every path.
        let buf = unsafe { VirtualAlloc(None, size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        if buf.is_null() {
            return None;
        }
        let mut need = 0u32;
        // SAFETY: `buf` is `size` writable bytes.
        let st = unsafe { NtQuerySystemInformation(SYSTEM_PROCESS_INFORMATION, buf, size as u32, &mut need) };
        let out = if st >= 0 {
            // SAFETY: the system wrote a valid list into the first `size` bytes.
            parse_snapshot(unsafe { std::slice::from_raw_parts(buf as *const u8, size) }, own)
        } else {
            None
        };
        // SAFETY: allocated above, released once.
        let _ = unsafe { VirtualFree(buf, 0, MEM_RELEASE) };
        if st == STATUS_INFO_LENGTH_MISMATCH {
            size = (need as usize).max(size * 2) + 256 * 1024;
            continue;
        }
        return out;
    }
    None
}

fn parse_snapshot(b: &[u8], own: u32) -> Option<Snap> {
    let mut off = 0usize;
    loop {
        if off + std::mem::size_of::<SpInfo>() > b.len() {
            return None;
        }
        // SAFETY: bounds checked above; unaligned read of a plain-old-data struct.
        let sp = unsafe { b.as_ptr().add(off).cast::<SpInfo>().read_unaligned() };
        if sp.pid == own as usize {
            let mut threads = Vec::with_capacity(sp.nthreads as usize);
            for i in 0..sp.nthreads as usize {
                let at = off + std::mem::size_of::<SpInfo>() + i * std::mem::size_of::<StInfo>();
                if at + std::mem::size_of::<StInfo>() > b.len() {
                    break;
                }
                // SAFETY: bounds checked above.
                threads.push(unsafe { b.as_ptr().add(at).cast::<StInfo>().read_unaligned() });
            }
            return Some(Snap {
                proc_time: (sp.user + sp.kernel).max(0) as u64,
                handles: sp.handles,
                private: sp.private,
                ws: sp.ws,
                threads,
            });
        }
        if sp.next == 0 {
            return None;
        }
        off += sp.next as usize;
    }
}

fn wait_reason_name(w: u32) -> String {
    let n = match w {
        0 => "Executive",
        4 => "DelayExecution",
        5 => "Suspended",
        6 => "UserRequest",
        7 => "WrExecutive",
        11 => "WrDelayExecution",
        13 => "WrUserRequest",
        15 => "WrQueue",
        16 => "WrLpcReceive",
        17 => "WrLpcReply",
        21 => "WrKeyedEvent",
        27 => "WrResource",
        28 => "WrPushLock",
        29 => "WrMutex",
        37 => "WrAlertByThreadId",
        _ => return format!("wr{w}"),
    };
    n.into()
}

fn thread_state_name(s: u32, w: u32) -> String {
    match s {
        5 => format!("wait:{}", wait_reason_name(w)),
        2 => "running".into(),
        1 => "ready".into(),
        4 => "terminated".into(),
        _ => format!("state{s}"),
    }
}

/// Name of a thread of this process: the UI thread, its description (Rust thread names), or the module and
/// offset of its start address (system and COM threads).
fn thread_name(tid: u32) -> String {
    if tid == UI_TID.load(Relaxed) {
        return "ui (main)".into();
    }
    // SAFETY: plain query of a thread of this process; closed below.
    let Ok(h) = (unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION | THREAD_QUERY_INFORMATION, false, tid) }) else {
        return "?".into();
    };
    let mut name = String::new();
    // SAFETY: `h` valid; the returned string is LocalAlloc'ed and freed below.
    if let Ok(d) = unsafe { GetThreadDescription(h) } {
        if !d.is_null() {
            // SAFETY: NUL-terminated string returned by the system.
            name = unsafe { d.to_string() }.unwrap_or_default();
            // SAFETY: allocated by GetThreadDescription.
            let _ = unsafe { LocalFree(Some(HLOCAL(d.0.cast()))) };
        }
    }
    if name.is_empty() {
        let mut start = 0usize;
        // SAFETY: `start` is a pointer-sized out value.
        let st = unsafe {
            NtQueryInformationThread(
                h,
                THREAD_QUERY_SET_WIN32_START_ADDRESS,
                (&mut start as *mut usize).cast(),
                std::mem::size_of::<usize>() as u32,
                std::ptr::null_mut(),
            )
        };
        if st >= 0 && start != 0 {
            name = module_and_offset(start);
        }
    }
    // SAFETY: opened above.
    let _ = unsafe { CloseHandle(h) };
    if name.is_empty() {
        "?".into()
    } else {
        name
    }
}

fn module_and_offset(addr: usize) -> String {
    let mut m = HMODULE::default();
    // SAFETY: FROM_ADDRESS reads `addr` as an address inside a module; UNCHANGED_REFCOUNT takes no reference.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr as *const u16),
            &mut m,
        )
    }
    .is_ok();
    if !ok {
        return format!("start@0x{addr:x}");
    }
    let mut buf = [0u16; 520];
    // SAFETY: `m` is a module of this process, `buf` writable.
    let n = unsafe { GetModuleFileNameW(Some(m), &mut buf) } as usize;
    let file = String::from_utf8_lossy(&image_file_name(&buf[..n.min(buf.len())])).into_owned();
    format!("{file}+0x{:x}", addr.wrapping_sub(m.0 as usize))
}

// ------------------------------------------------------------------ names

fn sys_msg_name(m: u32) -> Option<&'static str> {
    Some(match m {
        0x0000 => "WM_NULL",
        0x0001 => "WM_CREATE",
        0x0002 => "WM_DESTROY",
        0x0003 => "WM_MOVE",
        0x0005 => "WM_SIZE",
        0x0006 => "WM_ACTIVATE",
        0x0007 => "WM_SETFOCUS",
        0x0008 => "WM_KILLFOCUS",
        0x000C => "WM_SETTEXT",
        0x000D => "WM_GETTEXT",
        0x000E => "WM_GETTEXTLENGTH",
        0x000F => "WM_PAINT",
        0x0010 => "WM_CLOSE",
        0x0011 => "WM_QUERYENDSESSION",
        0x0014 => "WM_ERASEBKGND",
        0x0015 => "WM_SYSCOLORCHANGE",
        0x0016 => "WM_ENDSESSION",
        0x0018 => "WM_SHOWWINDOW",
        0x001A => "WM_SETTINGCHANGE",
        0x001C => "WM_ACTIVATEAPP",
        0x001D => "WM_FONTCHANGE",
        0x001E => "WM_TIMECHANGE",
        0x0020 => "WM_SETCURSOR",
        0x0021 => "WM_MOUSEACTIVATE",
        0x0024 => "WM_GETMINMAXINFO",
        0x003D => "WM_GETOBJECT",
        0x0046 => "WM_WINDOWPOSCHANGING",
        0x0047 => "WM_WINDOWPOSCHANGED",
        0x004A => "WM_COPYDATA",
        0x0051 => "WM_INPUTLANGCHANGE",
        0x007B => "WM_CONTEXTMENU",
        0x007C => "WM_STYLECHANGING",
        0x007D => "WM_STYLECHANGED",
        0x007E => "WM_DISPLAYCHANGE",
        0x007F => "WM_GETICON",
        0x0081 => "WM_NCCREATE",
        0x0082 => "WM_NCDESTROY",
        0x0083 => "WM_NCCALCSIZE",
        0x0084 => "WM_NCHITTEST",
        0x0085 => "WM_NCPAINT",
        0x0086 => "WM_NCACTIVATE",
        0x0088 => "WM_SYNCPAINT",
        0x0090 => "WM_UAHDESTROYWINDOW",
        0x00A0 => "WM_NCMOUSEMOVE",
        0x00FF => "WM_INPUT",
        0x0100 => "WM_KEYDOWN",
        0x0101 => "WM_KEYUP",
        0x0102 => "WM_CHAR",
        0x0104 => "WM_SYSKEYDOWN",
        0x0105 => "WM_SYSKEYUP",
        0x0111 => "WM_COMMAND",
        0x0112 => "WM_SYSCOMMAND",
        0x0113 => "WM_TIMER",
        0x0118 => "WM_SYSTIMER",
        0x0200 => "WM_MOUSEMOVE",
        0x0201 => "WM_LBUTTONDOWN",
        0x0202 => "WM_LBUTTONUP",
        0x0204 => "WM_RBUTTONDOWN",
        0x0205 => "WM_RBUTTONUP",
        0x0207 => "WM_MBUTTONDOWN",
        0x0208 => "WM_MBUTTONUP",
        0x020A => "WM_MOUSEWHEEL",
        0x020B => "WM_XBUTTONDOWN",
        0x020C => "WM_XBUTTONUP",
        0x020E => "WM_MOUSEHWHEEL",
        0x0215 => "WM_CAPTURECHANGED",
        0x0218 => "WM_POWERBROADCAST",
        0x0219 => "WM_DEVICECHANGE",
        0x0281 => "WM_IME_SETCONTEXT",
        0x0282 => "WM_IME_NOTIFY",
        0x02A1 => "WM_MOUSEHOVER",
        0x02A3 => "WM_MOUSELEAVE",
        0x02B1 => "WM_WTSSESSION_CHANGE",
        0x02E0 => "WM_DPICHANGED",
        0x0312 => "WM_HOTKEY",
        0x031A => "WM_THEMECHANGED",
        0x031E => "WM_DWMCOMPOSITIONCHANGED",
        0x031F => "WM_DWMNCRENDERINGCHANGED",
        0x0320 => "WM_DWMCOLORIZATIONCOLORCHANGED",
        0x0321 => "WM_DWMWINDOWMAXIMIZEDCHANGE",
        _ => return None,
    })
}

fn app_msg_name(th: Th, m: u32) -> Option<&'static str> {
    use crate::msg::*;
    Some(match (th, m) {
        (Th::Ui, WM_DOCK_TOGGLE) => "WM_DOCK_TOGGLE",
        (Th::Ui, WM_DOCK_WINKEY) => "WM_DOCK_WINKEY",
        (Th::Ui, WM_DOCK_DIAG) => "WM_DOCK_DIAG",
        (Th::Ui, WM_DOCK_APPS_DIRTY) => "WM_DOCK_APPS_DIRTY",
        (Th::Ui, WM_DOCK_TASKBAR_EVENT) => "WM_DOCK_TASKBAR_EVENT",
        (Th::Ui, WM_DOCK_SHELL_READY) => "WM_DOCK_SHELL_READY",
        (Th::Ui, WM_DOCK_VOLUME) => "WM_DOCK_VOLUME",
        (Th::Ui, WM_DOCK_QUIT_REQUEST) => "WM_DOCK_QUIT_REQUEST",
        (Th::Ui, WM_DOCK_TRAY_CHANGED) => "WM_DOCK_TRAY_CHANGED",
        (Th::Ui, WM_DOCK_NOTIF) => "WM_DOCK_NOTIF",
        (Th::Ui, WM_DOCK_POPUP_CLOSED) => "WM_DOCK_POPUP_CLOSED",
        (Th::Ui, WM_DOCK_APP_MENU_CMD) => "WM_DOCK_APP_MENU_CMD",
        (Th::Ui, WM_POPUP_OUTSIDE) => "WM_POPUP_OUTSIDE",
        (Th::Ui, WM_DOCK_CONFIG_APPLY) => "WM_DOCK_CONFIG_APPLY",
        (Th::Ui, WM_DOCK_CONFIG_CHANGED) => "WM_DOCK_CONFIG_CHANGED",
        (Th::Ui, WM_DOCK_TRIM_KICK) => "WM_DOCK_TRIM_KICK",
        (Th::Ui, WM_DOCK_START_MENU) => "WM_DOCK_START_MENU",
        (Th::Ui, WM_DOCK_OPEN_SETTINGS) => "WM_DOCK_OPEN_SETTINGS",
        (Th::Ui, WM_DOCK_UPDATE) => "WM_DOCK_UPDATE",
        (Th::Ui, WM_DOCK_WATCHDOG_EXITED) => "WM_DOCK_WATCHDOG_EXITED",
        (Th::Ui, WM_DOCK_CHORD) => "WM_DOCK_CHORD",
        (Th::Ui, WM_DOCK_PEEK) => "WM_DOCK_PEEK",
        (Th::Ui, WM_SETTINGS_SUITE_DATA) => "WM_SETTINGS_SUITE_DATA",
        // Labels without the launcher's probe-name prefix: launcher lint 14 rejects it in any string literal.
        (Th::Ui, WM_DOCK_LAUNCHER) => "launcher",
        (Th::Ui, WM_DOCK_LAUNCHER_YIELDED) => "launcher_yielded",
        (Th::Ui, WM_DOCK_LAUNCHER_CLOSE) => "launcher_close",
        (Th::Ui, WM_DOCK_LAUNCHER_PASSED) => "launcher_passed",
        (Th::Ui, WM_DOCK_LAUNCHER_PT) => "launcher_pt",
        (Th::Ui, WM_DOCK_LAUNCHER_ALIAS) => "launcher_alias",
        (Th::Hook, 0x800A) => "WM_HOOK_REINSTALL",
        (Th::Hook, 0x8013) => "WM_HOOK_NOTE",
        (Th::Shell, WM_SHELL_WAKE) => "WM_SHELL_WAKE",
        (Th::Shell, WM_SHELL_AUDIO_DEVICE_CHANGED) => "WM_SHELL_AUDIO_DEVICE_CHANGED",
        (Th::Shell, WM_SHELL_SPATIAL_CHANGED) => "WM_SHELL_SPATIAL_CHANGED",
        (Th::Shell, WM_SHELL_TAKEOVER) => "WM_SHELL_TAKEOVER",
        (Th::Tray, 0x8001) => "WM_HOST_RESIZE",
        (Th::Tray, 0x8002) => "WM_HOST_RAISE",
        (Th::Tray, 0x8003) => "WM_HOST_RESCALE",
        (Th::Tray, 0x8004) => "WM_HOST_PLACE_ARM",
        (Th::Tray, 0x8005) => "WM_HOST_PLACE_MOVE",
        (Th::Tray, 0x8006) => "WM_HOST_BAR",
        _ => return None,
    })
}

fn msg_name(th: Th, m: u32) -> String {
    if let Some(n) = app_msg_name(th, m).or_else(|| sys_msg_name(m)) {
        return n.into();
    }
    if (0xC000..=0xFFFF).contains(&m) {
        let mut buf = [0u16; 128];
        // SAFETY: `buf` writable; registered window messages share the clipboard-format atom table.
        let n = unsafe { GetClipboardFormatNameW(m, &mut buf) };
        if n > 0 {
            return format!("R:{}", String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]));
        }
    }
    format!("0x{m:04X}")
}

fn timer_name(th: Th, id: u32, thread_timer: bool) -> String {
    use crate::msg::*;
    if thread_timer {
        if th == Th::Hook {
            // The hook thread's thread timers: the Peek timer and the heartbeat, each counted under one fixed id
            // (`hook::is_peek_timer`, `hook::is_heartbeat_timer`), and the reinstall tick (wsC Q6), whose id changes at
            // each arming.
            let name = if crate::hook::is_peek_timer(id) {
                "PEEK_TIMER"
            } else if crate::hook::is_heartbeat_timer(id) {
                "HEARTBEAT"
            } else {
                "REINSTALL_TICK"
            };
            return name.into();
        }
        return format!("thread_timer#{id:X}");
    }
    let n = match (th, id as usize) {
        (Th::Ui, TIMER_POLL) => "TIMER_POLL",
        (Th::Ui, TIMER_HOUSEKEEPING) => "TIMER_HOUSEKEEPING",
        (Th::Ui, TIMER_RAM) => "TIMER_RAM",
        (Th::Ui, TIMER_SECOND) => "TIMER_SECOND",
        (Th::Ui, TIMER_RECONCILE) => "TIMER_RECONCILE",
        (Th::Ui, TIMER_TRIM) => "TIMER_TRIM",
        (Th::Ui, TIMER_CONFIG_COALESCE) => "TIMER_CONFIG_COALESCE",
        (Th::Ui, TIMER_APPS_DEBOUNCE) => "TIMER_APPS_DEBOUNCE",
        (Th::Ui, TIMER_ANIM) => "TIMER_ANIM",
        (Th::Ui, TIMER_CLOCK) => "TIMER_CLOCK",
        (Th::Ui, TIMER_TRAY_REANNOUNCE) => "TIMER_TRAY_REANNOUNCE",
        (Th::Ui, TIMER_SYSMON) => "TIMER_SYSMON",
        (Th::Ui, TIMER_HOVER_PICKER) => "TIMER_HOVER_PICKER",
        (Th::Ui, TIMER_PICKER_LEAVE) => "TIMER_PICKER_LEAVE",
        (Th::Ui, TIMER_BATTERY) => "TIMER_BATTERY",
        (Th::Ui, TIMER_UPDATE_CHECK) => "TIMER_UPDATE_CHECK",
        (Th::Ui, TIMER_POPUP_ANIM) => "TIMER_POPUP_ANIM",
        (Th::Ui, TIMER_POPUP_TICK) => "TIMER_POPUP_TICK",
        (Th::Ui, TIMER_POPUP_FOCUS) => "TIMER_POPUP_FOCUS",
        (Th::Ui, TIMER_POPUP_START_RELEASE) => "TIMER_POPUP_START_RELEASE",
        (Th::Ui, TIMER_LAUNCHER_SHRINK) => "TIMER_LAUNCHER_SHRINK",
        (Th::Ui, TIMER_LAUNCHER_WARM) => "TIMER_LAUNCHER_WARM",
        (Th::Ui, TIMER_LAUNCHER_SAVE) => "TIMER_LAUNCHER_SAVE",
        (Th::Ui, TIMER_LAUNCHER_PT) => "TIMER_LAUNCHER_PT",
        (Th::Ui, TIMER_POPUP_COM_FREE) => "TIMER_POPUP_COM_FREE",
        (Th::Ui, 40) => "TIMER_WIDTH",
        (Th::Ui, 41) => "TIMER_ATTN",
        (Th::Ui, 42) => "TIMER_VIS",
        (Th::Ui, TIMER_SETTINGS_TICK) => "TIMER_SETTINGS_TICK",
        (Th::Ui, TIMER_SETTINGS_APPLY) => "TIMER_SETTINGS_APPLY",
        (Th::Ui, TIMER_SETTINGS_BAR) => "TIMER_SETTINGS_BAR",
        (Th::Tray, 1) => "TIMER_ZORDER",
        (Th::Tray, 2) => "TIMER_DEAD",
        (Th::Tray, 3) => "TIMER_PLACE",
        _ => return format!("timer#{id}"),
    };
    n.into()
}

fn event_name(e: u32) -> String {
    let n = match e {
        0x0003 => "SYSTEM_FOREGROUND",
        0x0006 => "SYSTEM_MENUPOPUPSTART",
        0x0007 => "SYSTEM_MENUPOPUPEND",
        0x8000 => "OBJECT_CREATE",
        0x8001 => "OBJECT_DESTROY",
        0x8002 => "OBJECT_SHOW",
        0x8003 => "OBJECT_HIDE",
        0x800B => "OBJECT_LOCATIONCHANGE",
        0x8017 => "OBJECT_CLOAKED",
        0x8018 => "OBJECT_UNCLOAKED",
        _ => return format!("0x{e:04X}"),
    };
    n.into()
}

fn hshell_name(code: u32) -> String {
    let n = match code {
        1 => "WINDOWCREATED",
        2 => "WINDOWDESTROYED",
        3 => "ACTIVATESHELLWINDOW",
        4 => "WINDOWACTIVATED",
        5 => "GETMINRECT",
        6 => "REDRAW",
        7 => "TASKMAN",
        8 => "LANGUAGE",
        9 => "SYSMENU",
        10 => "ENDTASK",
        11 => "ACCESSIBILITYSTATE",
        12 => "APPCOMMAND",
        13 => "WINDOWREPLACED",
        14 => "WINDOWREPLACING",
        16 => "MONITORCHANGED",
        0x8004 => "RUDEAPPACTIVATED",
        0x8006 => "FLASH",
        _ => return format!("hshell#{code:X}"),
    };
    n.into()
}

fn cause_name(mask: u32) -> String {
    let parts: Vec<&str> =
        [(CAUSE_POSTED, "posted"), (CAUSE_WINEVENT, "winevent"), (CAUSE_SENT, "sent"), (CAUSE_MOUSE, "mouse_ll")]
            .iter()
            .filter(|(b, _)| mask & u32::from(*b) != 0)
            .map(|(_, n)| *n)
            .collect();
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join("+")
    }
}

fn copydata_kind(dw: u32, op: u32) -> String {
    match dw {
        0 => "appbar".into(),
        1 => match op {
            0 => "nim_add".into(),
            1 => "nim_modify".into(),
            2 => "nim_delete".into(),
            3 => "nim_setfocus".into(),
            4 => "nim_setversion".into(),
            _ => format!("nim_op{op}"),
        },
        3 => "icon_rect".into(),
        _ => format!("dwdata{dw}"),
    }
}

// ------------------------------------------------------------------ report

fn add_n(m: &mut Map<String, Value>, k: String, n: u64) {
    let v = m.entry(k).or_insert(json!(0));
    *v = json!(v.as_u64().unwrap_or(0) + n);
}

fn add_pair(m: &mut Map<String, Value>, k: String, n: u64, us: u64) {
    let v = m.entry(k).or_insert(json!([0, 0]));
    let (a, b) = (v[0].as_u64().unwrap_or(0), v[1].as_u64().unwrap_or(0));
    *v = json!([a + n, b + us]);
}

fn sub_map<'a>(m: &'a mut Map<String, Value>, k: &str) -> &'a mut Map<String, Value> {
    let v = m.entry(k.to_string()).or_insert_with(|| Value::Object(Map::new()));
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("object just ensured")
}

/// One thread table as JSON: posted and timers as [count, µs dispatching] on the UI thread, plain counts elsewhere.
fn thread_json(p: &Probe, th: Th, rows: &[(u32, u32, u64)]) -> Value {
    let mut o = Map::new();
    for &(key, n, us) in rows {
        let (n, low) = (u64::from(n), key & 0x0FFF_FFFF);
        match key & 0xF000_0000 {
            K_POSTED if th == Th::Ui => add_pair(sub_map(&mut o, "posted"), msg_name(th, low), n, us),
            K_POSTED => add_n(sub_map(&mut o, "posted"), msg_name(th, low), n),
            K_TIMER => {
                let name = timer_name(th, low & 0xFF_FFFF, low & TIMER_THREAD_FLAG != 0);
                if th == Th::Ui {
                    add_pair(sub_map(&mut o, "timers"), name, n, us);
                } else {
                    add_n(sub_map(&mut o, "timers"), name, n);
                }
            }
            K_SENTX => add_n(sub_map(&mut o, "sent"), msg_name(th, low), n),
            K_SENTL => add_n(sub_map(&mut o, "sent_same_thread"), msg_name(th, low), n),
            K_CAUSE => add_n(sub_map(&mut o, "wake_cause"), cause_name(low), n),
            K_SHELLHOOK => add_n(sub_map(&mut o, "shellhook"), hshell_name(low), n),
            K_KB => add_n(sub_map(&mut o, "kb"), msg_name(Th::Hook, low), n),
            K_MOUSE => add_n(sub_map(&mut o, "mouse_ll"), msg_name(Th::Ui, low), n),
            K_POSTBY => {
                let by = if (low >> 16) & 0xFF == BY_KB_HOOK { "kb_hook" } else { "mouse_hook" };
                add_n(sub_map(&mut o, "posts_to_ui"), format!("{by}:{}", msg_name(Th::Ui, low & 0xFFFF)), n);
            }
            K_COPYDATA => {
                let (dw, op, slot) = ((low >> 12) & 0xF, (low >> 8) & 0xF, low & 0xFF);
                let senders = sub_map(&mut o, "copydata");
                add_n(sub_map(senders, &p.slot_label(slot)), copydata_kind(dw, op), n);
            }
            _ => add_n(sub_map(&mut o, "unknown_keys"), format!("0x{key:08X}"), n),
        }
    }
    Value::Object(o)
}

fn winevent_json(p: &Probe, rows: &[(u32, u32, u64)]) -> Value {
    let mut o = Map::new();
    for &(key, n, us) in rows {
        let n = u64::from(n);
        let src = if (key >> 24) & 0xF == Src::Apps as u32 { "apps" } else { "tray_place" };
        let (mid, ev) = ((key >> 16) & 0xFF, event_name(key & 0xFFFF));
        let s = sub_map(&mut o, src);
        match key & 0xF000_0000 {
            W_CLASS => {
                let by = sub_map(sub_map(s, "by_event"), &ev);
                add_pair(by, CLASS_NAMES[mid as usize & 3].into(), n, us);
            }
            W_PROC => add_n(sub_map(sub_map(s, "by_process"), &p.slot_label(mid)), ev, n),
            W_PASS => {
                let stage = match mid {
                    1 => "pass_apps_post",
                    2 => "pass_taskbar_post",
                    _ => "pass_place_post",
                };
                add_n(sub_map(sub_map(s, "by_event"), &ev), stage.into(), n);
            }
            _ => {}
        }
    }
    Value::Object(o)
}

fn now_text() -> String {
    // SAFETY: no preconditions.
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

struct Reporter {
    start: Instant,
    last: Instant,
    /// (tid, create time) -> (context switches, 100 ns of CPU) at the previous report.
    prev: HashMap<(u32, i64), (u32, u64)>,
    names: HashMap<(u32, i64), String>,
    prev_proc_time: u64,
    report_us: u64,
}

impl Reporter {
    fn new(start: Instant) -> Reporter {
        Reporter { start, last: start, prev: HashMap::new(), names: HashMap::new(), prev_proc_time: 0, report_us: 0 }
    }

    fn baseline(&mut self, s: &Snap) {
        self.prev_proc_time = s.proc_time;
        for t in &s.threads {
            self.prev.insert((t.tid as u32, t.create), (t.cs, (t.kernel + t.user).max(0) as u64));
        }
    }

    fn sys_json(&mut self, s: Option<Snap>) -> Value {
        let Some(s) = s else { return json!({"error": "NtQuerySystemInformation failed"}) };
        let mut rows: Vec<(u64, Value)> = Vec::new();
        let mut next = HashMap::new();
        let mut cs_sum = 0u64;
        for t in &s.threads {
            let id = (t.tid as u32, t.create);
            let time = (t.kernel + t.user).max(0) as u64;
            let (pcs, ptime) = self.prev.get(&id).copied().unwrap_or((0, 0));
            let dcs = u64::from(t.cs.wrapping_sub(pcs));
            let dtime = time.saturating_sub(ptime);
            next.insert(id, (t.cs, time));
            cs_sum += dcs;
            let name = self.names.entry(id).or_insert_with(|| thread_name(id.0)).clone();
            let cpu_ms = (dtime / 1000) as f64 / 10.0;
            rows.push((
                dcs,
                json!({"n": name, "tid": id.0, "cs": dcs, "cpu_ms": cpu_ms, "st": thread_state_name(t.state, t.wait_reason), "new": !self.prev.contains_key(&id)}),
            ));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        self.names.retain(|k, _| next.contains_key(k));
        self.prev = next;
        let dproc = s.proc_time.saturating_sub(self.prev_proc_time);
        self.prev_proc_time = s.proc_time;
        json!({
            "proc": {
                "cs": cs_sum,
                "cpu_ms": (dproc / 1000) as f64 / 10.0,
                "threads": s.threads.len(),
                "handles": s.handles,
                "private_kb": s.private / 1024,
                "ws_kb": s.ws / 1024,
            },
            "threads": rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
        })
    }

    fn report(&mut self, p: &Probe, kind: &str) -> String {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_millis() as u64;
        self.last = now;
        p.add(C::Reports, 1);
        // Bar state: close the running interval at "now".
        let (cur, ms, changes) = {
            let mut b = BAR.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = b.since {
                let cur = b.cur;
                b.acc_ms[cur] += now.duration_since(t).as_millis() as u64;
                b.since = Some(now);
            }
            let out = (b.cur, b.acc_ms, b.changes);
            b.acc_ms = [0; 5];
            b.changes = 0;
            out
        };
        let mut ms_map = Map::new();
        for (i, v) in ms.iter().enumerate() {
            if *v != 0 {
                ms_map.insert(STATE_NAMES[i].into(), json!(v));
            }
        }
        let pure = (changes == 0 && ms.iter().filter(|v| **v != 0).count() == 1).then(|| STATE_NAMES[cur]);
        let mut c = Map::new();
        for (i, a) in p.c.iter().enumerate() {
            let v = a.swap(0, Relaxed);
            if v != 0 {
                c.insert(C_NAMES[i].into(), json!(v));
            }
        }
        let mut overflow = 0u64;
        let mut threads = Map::new();
        for th in TH_ALL {
            let t = &p.th[th as usize];
            overflow += u64::from(t.overflow.swap(0, Relaxed));
            let v = thread_json(p, th, &t.drain());
            if v.as_object().is_some_and(|o| !o.is_empty()) {
                threads.insert(th.name().into(), v);
            }
        }
        overflow += u64::from(p.we.overflow.swap(0, Relaxed));
        let we = winevent_json(p, &p.we.drain());
        let sys = self.sys_json(snapshot(OWN_PID.load(Relaxed)));
        let mut line = json!({
            "v": 1,
            "kind": kind,
            "ts": now_text(),
            "up_s": now.duration_since(self.start).as_secs(),
            "dt_ms": dt,
            "pid": OWN_PID.load(Relaxed),
            "ui_tid": UI_TID.load(Relaxed),
            "bar": {"now": STATE_NAMES[cur], "ms": ms_map, "changes": changes, "pure": pure},
            "c": c,
            "threads_msg": threads,
            "winevent": we,
            "sys": sys,
            "probe": {
                "prev_report_us": self.report_us,
                "table_overflow": overflow,
                "process_names": (p.proc_next.load(Relaxed) as usize).min(PROCS),
            },
        });
        if let Some(o) = line.as_object_mut() {
            o.retain(|_, v| !v.is_null());
        }
        line.to_string()
    }
}

fn write_line(path: &Path, line: &str) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= ROTATE_BYTES) {
        let old: PathBuf = path.with_file_name(REPORT_OLD);
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
    if let Some(mut f) = crate::datadir::open_append(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn reporter_main(p: &'static Probe, path: &Path, switch: &str) {
    let start = Instant::now();
    let mut r = Reporter::new(start);
    if let Some(s) = snapshot(OWN_PID.load(Relaxed)) {
        r.baseline(&s);
    }
    let head = json!({
        "v": 1,
        "kind": "start",
        "ts": now_text(),
        "pid": OWN_PID.load(Relaxed),
        "version": env!("CARGO_PKG_VERSION"),
        "switch": switch,
        "period_ms": PERIOD_MS,
    });
    write_line(path, &head.to_string());
    let stop = HANDLE(STOP_EVENT.load(Relaxed) as *mut c_void);
    loop {
        // SAFETY: event created by `init`, never closed.
        let w = unsafe { WaitForSingleObject(stop, PERIOD_MS) };
        let last = w != WAIT_TIMEOUT;
        let t = Instant::now();
        let line = r.report(p, if last { "final" } else { "report" });
        write_line(path, &line);
        r.report_us = us_since(t);
        if last {
            break;
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_counts_and_drains() {
        let p = Probe::new_boxed();
        let t = &p.th[0];
        t.bump(K_POSTED | 0x113, 5);
        t.bump(K_POSTED | 0x113, 7);
        t.bump(K_TIMER | 1, 3);
        let mut rows = t.drain();
        rows.sort();
        assert_eq!(rows, vec![(K_POSTED | 0x113, 2, 12), (K_TIMER | 1, 1, 3)]);
        assert!(t.drain().is_empty(), "a drain resets the counts");
        t.bump(K_TIMER | 1, 0);
        assert_eq!(t.drain(), vec![(K_TIMER | 1, 1, 0)]);
    }

    #[test]
    fn table_overflow_is_counted_not_lost_silently() {
        let p = Probe::new_boxed();
        let t = &p.th[1];
        for k in 0..(TABLE as u32 + 3) {
            t.bump(K_POSTED | k, 0);
        }
        assert_eq!(t.overflow.load(Relaxed), 3);
        assert_eq!(t.drain().len(), TABLE);
    }

    #[test]
    fn process_slots_are_shared_by_name() {
        let p = Probe::new_boxed();
        let a = p.slot_for(b"explorer.exe");
        let b = p.slot_for(b"chrome.exe");
        assert_eq!(p.slot_for(b"explorer.exe"), a);
        assert_ne!(a, b);
        assert!(a as usize >= FIXED_SLOTS.len());
        assert_eq!(p.class_of(a), CLASS_EXPLORER);
        assert_eq!(p.class_of(b), CLASS_OTHER);
        assert_eq!(p.class_of(SLOT_DOCK), CLASS_DOCK);
        assert_eq!(p.class_of(SLOT_NOWIN), CLASS_NOWIN);
        assert_eq!(p.slot_label(u32::from(SLOT_DENIED)), "(denied)");
    }

    #[test]
    fn image_names_are_lowercase_file_names() {
        let w: Vec<u16> = r"C:\Program Files\Foo\Bar.EXE".encode_utf16().collect();
        assert_eq!(image_file_name(&w), b"bar.exe".to_vec());
        let w: Vec<u16> = "plain.exe".encode_utf16().collect();
        assert_eq!(image_file_name(&w), b"plain.exe".to_vec());
    }

    #[test]
    fn timer_keys_keep_window_and_thread_timers_apart() {
        let mut m = MSG { message: WM_TIMER, ..Default::default() };
        m.wParam = WPARAM(crate::msg::TIMER_POLL);
        m.hwnd = HWND(8 as *mut c_void);
        assert_eq!(msg_key(&m), K_TIMER | 1);
        m.hwnd = HWND::default();
        assert_eq!(msg_key(&m), K_TIMER | TIMER_THREAD_FLAG | 1);
        m.message = crate::msg::WM_DOCK_APPS_DIRTY;
        assert_eq!(msg_key(&m), K_POSTED | crate::msg::WM_DOCK_APPS_DIRTY);
    }

    #[test]
    fn names_of_messages_timers_and_causes() {
        assert_eq!(msg_name(Th::Ui, crate::msg::WM_DOCK_TOGGLE), "WM_DOCK_TOGGLE");
        assert_eq!(msg_name(Th::Tray, 0x8001), "WM_HOST_RESIZE");
        assert_eq!(msg_name(Th::Tray, 0x8006), "WM_HOST_BAR");
        assert_eq!(msg_name(Th::Hook, 0x800A), "WM_HOOK_REINSTALL");
        assert_eq!(msg_name(Th::Shell, 0x0113), "WM_TIMER");
        assert_eq!(msg_name(Th::Ui, 0x0401), "0x0401");
        assert_eq!(timer_name(Th::Ui, 1, false), "TIMER_POLL");
        assert_eq!(timer_name(Th::Ui, crate::msg::TIMER_TRIM as u32, false), "TIMER_TRIM");
        assert_eq!(timer_name(Th::Ui, crate::msg::TIMER_CONFIG_COALESCE as u32, false), "TIMER_CONFIG_COALESCE");
        assert_eq!(msg_name(Th::Ui, crate::msg::WM_DOCK_CONFIG_CHANGED), "WM_DOCK_CONFIG_CHANGED");
        assert_eq!(msg_name(Th::Ui, crate::msg::WM_DOCK_TRIM_KICK), "WM_DOCK_TRIM_KICK");
        assert_eq!(timer_name(Th::Tray, 1, false), "TIMER_ZORDER");
        assert_eq!(timer_name(Th::Shell, 0x7FFF, true), "thread_timer#7FFF");
        assert_eq!(timer_name(Th::Hook, 0x7FFE, true), "REINSTALL_TICK");
        assert_eq!(timer_name(Th::Hook, 0xFF_FFFF, true), "PEEK_TIMER");
        assert_eq!(timer_name(Th::Hook, 0xFF_FFFE, true), "HEARTBEAT");
        assert_eq!(cause_name(0), "none");
        assert_eq!(cause_name(u32::from(CAUSE_POSTED | CAUSE_WINEVENT)), "posted+winevent");
        assert_eq!(copydata_kind(1, 1), "nim_modify");
        assert_eq!(event_name(0x800B), "OBJECT_LOCATIONCHANGE");
    }

    #[test]
    fn the_stall_switch_reads_a_bounded_number_of_ms() {
        assert_eq!(stall_ms("7000"), 7000);
        assert_eq!(stall_ms(" 4500\r\n"), 4500);
        assert_eq!(stall_ms(&STALL_MAX_MS.to_string()), STALL_MAX_MS);
        for off in ["", "0", "-1", "7 s", "abc", "60001", "99999999999"] {
            assert_eq!(stall_ms(off), 0, "{off:?}");
        }
    }

    #[test]
    fn thread_and_winevent_json_shapes() {
        let p = Probe::new_boxed();
        let chrome = p.slot_for(b"chrome.exe");
        let ui = &p.th[Th::Ui as usize];
        ui.bump(K_TIMER | 1, 40);
        ui.bump(K_CAUSE | u32::from(CAUSE_WINEVENT), 0);
        ui.bump(K_SENTX | 0x001A, 0);
        let v = thread_json(&p, Th::Ui, &ui.drain());
        assert_eq!(v["timers"]["TIMER_POLL"], json!([1, 40]));
        assert_eq!(v["wake_cause"]["winevent"], json!(1));
        assert_eq!(v["sent"]["WM_SETTINGCHANGE"], json!(1));
        let tray = &p.th[Th::Tray as usize];
        tray.bump(K_COPYDATA | 1 << 12 | 1 << 8 | u32::from(chrome), 0);
        let v = thread_json(&p, Th::Tray, &tray.drain());
        assert_eq!(v["copydata"]["chrome.exe"]["nim_modify"], json!(1));
        let s = (Src::Apps as u32) << 24;
        p.we.bump(W_CLASS | s | CLASS_OTHER << 16 | 0x800B, 9);
        p.we.bump(W_PROC | s | u32::from(chrome) << 16 | 0x800B, 0);
        p.we.bump(W_PASS | s | (Stage::AppsDirty as u32) << 16 | 0x800B, 0);
        let v = winevent_json(&p, &p.we.drain());
        assert_eq!(v["apps"]["by_event"]["OBJECT_LOCATIONCHANGE"]["other"], json!([1, 9]));
        assert_eq!(v["apps"]["by_event"]["OBJECT_LOCATIONCHANGE"]["pass_apps_post"], json!(1));
        assert_eq!(v["apps"]["by_process"]["chrome.exe"]["OBJECT_LOCATIONCHANGE"], json!(1));
    }

    #[test]
    fn struct_layouts_match_the_system() {
        assert_eq!(std::mem::offset_of!(SpInfo, user), 0x28);
        assert_eq!(std::mem::offset_of!(SpInfo, kernel), 0x30);
        assert_eq!(std::mem::offset_of!(SpInfo, pid), 0x50);
        assert_eq!(std::mem::offset_of!(SpInfo, handles), 0x60);
        assert_eq!(std::mem::offset_of!(SpInfo, ws), 0x90);
        assert_eq!(std::mem::offset_of!(SpInfo, private), 0xC8);
        assert_eq!(std::mem::offset_of!(StInfo, start), 0x20);
        assert_eq!(std::mem::offset_of!(StInfo, tid), 0x30);
        assert_eq!(std::mem::offset_of!(StInfo, cs), 0x40);
        assert_eq!(std::mem::offset_of!(StInfo, wait_reason), 0x48);
    }

    #[test]
    fn the_snapshot_finds_this_test_process() {
        // SAFETY: no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        let s = snapshot(pid).expect("SystemProcessInformation lists this process");
        // SAFETY: no preconditions.
        let me = unsafe { GetCurrentThreadId() };
        assert!(s.threads.iter().any(|t| t.tid as u32 == me && t.pid as u32 == pid));
        assert!(s.handles > 0);
    }

    #[test]
    fn a_report_line_is_one_json_object_with_threads_and_counters() {
        // SAFETY: no preconditions.
        OWN_PID.store(unsafe { GetCurrentProcessId() }, Relaxed);
        let p = Probe::new_boxed();
        let mut r = Reporter::new(Instant::now());
        if let Some(s) = snapshot(OWN_PID.load(Relaxed)) {
            r.baseline(&s);
        }
        p.add(C::UiWakes, 3);
        p.th[Th::Ui as usize].bump(K_TIMER | 4, 11);
        let line = r.report(&p, "report");
        assert!(!line.contains('\n'), "one line per report");
        let v: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(v["kind"], json!("report"));
        assert_eq!(v["c"]["ui.wakes"], json!(3));
        assert_eq!(v["threads_msg"]["ui"]["timers"]["TIMER_SECOND"], json!([1, 11]));
        assert!(v["sys"]["proc"]["threads"].as_u64().unwrap_or(0) >= 1);
        assert!(v["sys"]["threads"].as_array().is_some_and(|t| !t.is_empty()));
        let again: Value = serde_json::from_str(&r.report(&p, "report")).expect("valid JSON");
        assert!(again["c"].get("ui.wakes").is_none(), "counters are per interval");
    }

    #[test]
    fn msgwait_wakes_for_each_out_of_context_winevent() {
        use windows::Win32::UI::Accessibility::{NotifyWinEvent, SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINEVENT_OUTOFCONTEXT, WS_POPUP,
        };
        const EV: u32 = 0xA0F1; // application range: nothing else raises it
        static HITS: AtomicU32 = AtomicU32::new(0);
        unsafe extern "system" fn cb(_h: HWINEVENTHOOK, ev: u32, _w: HWND, _o: i32, _c: i32, _t: u32, _ms: u32) {
            if ev == EV {
                HITS.fetch_add(1, Relaxed);
            }
        }
        // SAFETY: no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        // SAFETY: 'static callback; out of context, delivered to this thread while it waits below.
        let hook = unsafe { SetWinEventHook(EV, EV, None, Some(cb), pid, 0, WINEVENT_OUTOFCONTEXT) };
        assert!(!hook.is_invalid());
        let fire = std::thread::spawn(|| {
            // SAFETY: plain hidden popup of a system class, destroyed below on the same thread.
            let h = unsafe {
                CreateWindowExW(Default::default(), w!("STATIC"), w!(""), WS_POPUP, 0, 0, 1, 1, None, None, None, None)
            }
            .expect("window");
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(30));
                // SAFETY: own window; the event id is in the application range.
                unsafe { NotifyWinEvent(EV, h, 0, 0) };
            }
            // SAFETY: created above by this thread.
            let _ = unsafe { DestroyWindow(h) };
        });
        let t0 = Instant::now();
        let (mut wakes, mut timeouts) = (0, 0);
        let mut msg = MSG::default();
        while HITS.load(Relaxed) < 5 && t0.elapsed() < std::time::Duration::from_secs(5) {
            // SAFETY: as in `ui_loop`, with a 1 s timeout so a missed wake shows as a timeout.
            let r = unsafe { MsgWaitForMultipleObjectsEx(None, 1000, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            if r == WAIT_TIMEOUT {
                timeouts += 1;
            } else {
                wakes += 1;
            }
            // SAFETY: `msg` valid; delivers the pending WinEvent callbacks.
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                // SAFETY: just filled by PeekMessageW.
                unsafe { DispatchMessageW(&msg) };
            }
        }
        fire.join().expect("firing thread");
        // SAFETY: hook installed above on this thread.
        let _ = unsafe { UnhookWinEvent(hook) };
        assert_eq!(HITS.load(Relaxed), 5);
        assert!(wakes >= 1);
        assert_eq!(timeouts, 0, "every event must wake the wait by itself");
    }

    #[test]
    fn probe_is_off_by_default() {
        assert!(get().is_none());
        assert!(!ui_loop());
        assert_eq!(install_send_hook(Th::Ui), 0);
        bump(C::UiWakes);
        let _span = winevent(Src::Apps, 0x8002, HWND::default());
    }
}
