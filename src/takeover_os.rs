//! Takeover delivery on the shell thread (dock-suite 2.4, D1-D3): every swallowed Ctrl+Shift+Esc ends in Tasks in
//! front, Tasks on screen, or the fallback the machine allows (Windows Task Manager with Administrator protection
//! off, else the re-injected chord when proven, else nothing). `deliver` decides over `TakeoverOs`, `RealOs` is the
//! shell thread's OS, `on_message` and `run_lost` are the entry points, `copydata_send` the plan's one timed send.
//!
//! Declared as a child module of `ui` (`#[path]`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::Cell;
use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering::Relaxed};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use oled_sys::ident::same_image;
use oled_trust::frame::tasks::{copydata_tag, InstanceCmd, Show, WINDOW_CLASS};
use oled_trust::frame::{encode, FrameError};
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_TIMEOUT, FILETIME, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, FlashWindowEx, GetForegroundWindow, GetShellWindow,
    GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, SendMessageTimeoutW, SetWindowPos, FLASHWINFO,
    FLASHW_ALL, FLASHW_TIMERNOFG, HWND_NOTOPMOST, HWND_TOPMOST, SMTO_ABORTIFHUNG, SMTO_BLOCK, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNORMAL, WM_COPYDATA,
};

use crate::hook::TakeoverTarget;
use crate::log;
use crate::shell_thread::{desktop_shell_execute, system_dir, Verb};
use crate::ui::incidents::{self, IncidentKind};
use crate::ui::suite_links::{self, App};
use crate::ui::takeover::{self, ApMode};
use crate::watchdog::spawn_breakaway;

/// What a delivery did (logged as its kind and milliseconds; FellBack counts for the breaker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Activated,
    Launched,
    ShownNotForeground,
    FellBack(Why, Fallback),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    Stale,
    ExeMissing,
    InstanceNoAnswer,
    InstanceHung,
    NoShell,
    LaunchFailed,
    NotReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    Taskmgr,
    Reinject,
    Nothing,
}

/// How long a running Tasks has to answer the WM_COPYDATA.
pub(crate) const COPYDATA_TIMEOUT_MS: u32 = 300;

/// What a WM_COPYDATA to a Tasks window gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Accepted,
    Refused,
    Timeout,
}

/// The plan's only SendMessageTimeoutW (lint 19 lists `fn copydata_send(`, blocks = true): a non-zero return with
/// result 1 is Accepted; zero with ERROR_TIMEOUT is Timeout; anything else is Refused. A free function, so the lint's
/// body search never lands on a trait declaration.
pub(crate) fn copydata_send(to: HWND, sender: HWND, cds: &COPYDATASTRUCT) -> Answer {
    let mut result = 0usize;
    // SAFETY: `cds` lives for the whole call, which copies the data to the receiver's process; `result` is writable;
    // SMTO_ABORTIFHUNG avoids a wait on a hung window and SMTO_BLOCK keeps the timeout whole.
    let r = unsafe {
        SendMessageTimeoutW(
            to,
            WM_COPYDATA,
            WPARAM(sender.0 as usize),
            LPARAM(cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            COPYDATA_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    // SAFETY: no preconditions; read right after the send.
    let err = unsafe { GetLastError() };
    answer_of(r.0 != 0, result, err == ERROR_TIMEOUT)
}

/// The mapping of a send's outcome to an Answer. Pure.
fn answer_of(delivered: bool, result: usize, timed_out: bool) -> Answer {
    if delivered && result == 1 {
        Answer::Accepted
    } else if !delivered && timed_out {
        Answer::Timeout
    } else {
        Answer::Refused
    }
}

// ------------------------------------------------------------------ the decision (D1)
//
// `deliver` is the whole decision of one swallowed press, pure over `TakeoverOs`: the shell thread runs it over the
// real OS, the tests over a scripted fake. One deadline bounds each path; every wait polls every POLL_MS and reads the
// instances again at each poll, so visibility, hang state and existence are always the current ones.

/// A request older than this goes straight to the fallback.
pub(crate) const STALE_MS: u32 = 1500;
/// How long the desktop shell has to start Tasks (T 5.6 step 2).
pub(crate) const LAUNCH_WATCHDOG_MS: u32 = 400;
/// How long Tasks has to show its window (X10).
pub(crate) const READY_WAIT_MS: u32 = 1500;
/// When a visible Tasks window that is not in front yet is raised (T 5.6 step 3).
pub(crate) const LADDER_AFTER_MS: u32 = 300;
/// The period of every poll.
pub(crate) const POLL_MS: u32 = 25;
/// Tick granularity between `kb.time` and a process creation time.
pub(crate) const CREATE_SLACK_MS: u64 = 50;
/// Two fallbacks within this window trip the breaker ...
pub(crate) const BREAKER_WINDOW_MS: u32 = 600_000;
/// ... which pauses the takeover this long.
pub(crate) const BREAKER_PAUSE_MS: u32 = 600_000;

/// A top-level window of class WINDOW_CLASS whose process image is the installed tasks.exe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Instance {
    pub hwnd: isize,
    pub pid: u32,
    /// The creation time of its process, ms since 1601.
    pub created_ms: u64,
    pub visible: bool,
    pub hung: bool,
}

/// What a start of tasks.exe through the desktop shell gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Launch {
    Started,
    Failed,
    Watchdog,
}

/// The OS as a delivery sees it: the real one on the shell thread, a scripted fake in the tests.
pub(crate) trait TakeoverOs {
    /// GetTickCount.
    fn now(&self) -> u32;
    /// The wall clock (ms since 1601) of a past tick.
    fn wall_ms(&self, tick: u32) -> u64;
    fn sleep(&mut self, ms: u32);
    /// The installed tasks.exe, if it is a file.
    fn tasks_exe(&self) -> Option<PathBuf>;
    /// The top-level windows of class WINDOW_CLASS whose process image is `exe`.
    fn instances(&self, exe: &Path) -> Vec<Instance>;
    /// AllowSetForegroundWindow, then the Show frame through `copydata_send` (COPYDATA_TIMEOUT_MS).
    fn send_show(&mut self, w: &Instance, frame: &[u8]) -> Answer;
    /// GetShellWindow is not null and not hung.
    fn shell_ok(&self) -> bool;
    /// `exe --from-chord` through the desktop shell, given up after LAUNCH_WATCHDOG_MS.
    fn launch(&mut self, exe: &Path) -> Launch;
    fn foreground_pid(&self) -> Option<u32>;
    /// The ladder's step (b): topmost then not topmost, and a flash.
    fn raise(&mut self, w: &Instance);
    /// <System32>\Taskmgr.exe with no arguments.
    fn start_taskmgr(&mut self) -> bool;
    /// `hook::inject_ctrl_shift_esc`.
    fn reinject(&mut self) -> bool;
}

/// One swallowed press: its sequence and its `kb.time`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Req {
    pub seq: u16,
    pub press: u32,
}

/// How a wait for the foreground ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Front {
    Foreground,
    /// Visible at the deadline but not in front: Tasks is on screen.
    NotForeground,
    /// Destroyed or hidden at the deadline: Tasks crashed or closed after it accepted.
    Gone,
}

/// The delivery of one press (dock-suite D1): activate a running Tasks, await a starting one, or start it; anything
/// else, or a Tasks that does not show within its deadline, ends in the fallback. An empty `frame` is one that could
/// not be encoded: a running Tasks is then not sent anything and the press falls back.
pub(crate) fn deliver(os: &mut dyn TakeoverOs, req: Req, ap: ApMode, reinject_ok: bool, frame: &[u8]) -> Outcome {
    let kind = fallback_kind(ap, reinject_ok);
    // (0) A press the shell thread reached late (a busy pump, a dropped message).
    if os.now().wrapping_sub(req.press) > STALE_MS {
        return fallback(os, Why::Stale, kind);
    }
    // (1)
    let Some(exe) = os.tasks_exe() else {
        return fallback(os, Why::ExeMissing, kind);
    };
    // (2)
    let seen = os.instances(&exe);
    if let Some(w) = seen.iter().copied().find(ready) {
        // (2a) Running.
        let answer = if frame.is_empty() { Answer::Refused } else { os.send_show(&w, frame) };
        if answer != Answer::Accepted {
            return fallback(os, Why::InstanceNoAnswer, kind);
        }
        let deadline = os.now().wrapping_add(READY_WAIT_MS);
        let front = wait_foreground(os, &exe, w, deadline, seen);
        return settle(os, front, Outcome::Activated, kind);
    }
    if seen.iter().any(|i| !i.hung) {
        // (2b) Starting: tasks.exe creates its window hidden and shows it after its first frame, so it shows itself;
        // no frame and no launch (a second start would only be forwarded to it).
        let deadline = os.now().wrapping_add(READY_WAIT_MS);
        return match await_window(os, &exe, deadline, &|_| true) {
            Some(front) => settle(os, front, Outcome::Activated, kind),
            None => fallback(os, Why::NotReady, kind),
        };
    }
    if !seen.is_empty() {
        // (2c) Only hung instances: single-instance forwarding would reach a hung one.
        return fallback(os, Why::InstanceHung, kind);
    }
    // (3) Fresh.
    if !os.shell_ok() {
        return fallback(os, Why::NoShell, kind);
    }
    let since = os.wall_ms(req.press).saturating_sub(CREATE_SLACK_MS);
    if os.launch(&exe) != Launch::Started {
        return fallback(os, Why::LaunchFailed, kind);
    }
    let deadline = os.now().wrapping_add(READY_WAIT_MS);
    // R6: never an old window. A window counts only when it was not visible at the press and its process started
    // after the press.
    let fresh = |i: &Instance| !seen.iter().any(|s| s.hwnd == i.hwnd && s.visible) && i.created_ms >= since;
    match await_window(os, &exe, deadline, &fresh) {
        Some(front) => settle(os, front, Outcome::Launched, kind),
        None => fallback(os, Why::NotReady, kind),
    }
}

/// The fallback the machine allows: Windows Task Manager only with Administrator protection proven off; otherwise the
/// tagged re-injection when it was proven to open it, else nothing.
pub(crate) fn fallback_kind(ap: ApMode, reinject_ok: bool) -> Fallback {
    match ap {
        ApMode::Off => Fallback::Taskmgr,
        ApMode::On | ApMode::Unknown if reinject_ok => Fallback::Reinject,
        ApMode::On | ApMode::Unknown => Fallback::Nothing,
    }
}

/// The breaker over the fallbacks: two within BREAKER_WINDOW_MS pause the takeover for BREAKER_PAUSE_MS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Breaker {
    /// The ticks of the previous and of the latest fallback; 0 = none (a fallback at tick 0 exactly is not counted).
    first: u32,
    second: u32,
    /// The tick the last pause lasts until, `| 1` (0 = never tripped).
    open_until: u32,
}

impl Breaker {
    pub(crate) const CLOSED: Breaker = Breaker { first: 0, second: 0, open_until: 0 };

    /// One more fallback at `now`: true when it is the second within BREAKER_WINDOW_MS (wrapping), which trips the
    /// breaker and starts the count again.
    pub(crate) fn on_fallback(&mut self, now: u32) -> bool {
        self.first = self.second;
        self.second = now;
        if self.first != 0 && now.wrapping_sub(self.first) <= BREAKER_WINDOW_MS {
            self.open_until = now.wrapping_add(BREAKER_PAUSE_MS) | 1;
            self.first = 0;
            self.second = 0;
            return true;
        }
        false
    }

    /// The tick the last pause lasts until, never 0 once tripped.
    pub(crate) fn open_until(&self) -> u32 {
        self.open_until
    }
}

/// Visible and not hung: a Tasks window that can take the frame or the foreground.
fn ready(i: &Instance) -> bool {
    i.visible && !i.hung
}

/// `now` is at or past `deadline` (wrapping).
fn reached(now: u32, deadline: u32) -> bool {
    (now.wrapping_sub(deadline) as i32) >= 0
}

/// Polls until a ready instance that `accept`s shows, then waits for it to come to the front, under one deadline.
/// None: no such window by the deadline.
fn await_window(
    os: &mut dyn TakeoverOs,
    exe: &Path,
    deadline: u32,
    accept: &dyn Fn(&Instance) -> bool,
) -> Option<Front> {
    loop {
        if reached(os.now(), deadline) {
            return None;
        }
        os.sleep(POLL_MS);
        let list = os.instances(exe);
        if let Some(w) = list.iter().copied().find(|i| ready(i) && accept(i)) {
            return Some(wait_foreground(os, exe, w, deadline, list));
        }
    }
}

/// Waits until `w`'s process owns the foreground or the deadline passes. `list` is this poll's read of the instances;
/// every later poll reads them again. `w` is raised once, LADDER_AFTER_MS after the wait began or at the last poll
/// before the deadline if that comes first, and only while it is visible.
fn wait_foreground(os: &mut dyn TakeoverOs, exe: &Path, w: Instance, deadline: u32, mut list: Vec<Instance>) -> Front {
    let began = os.now();
    let mut raised = false;
    loop {
        if os.foreground_pid() == Some(w.pid) {
            return Front::Foreground;
        }
        let visible = list.iter().any(|i| i.hwnd == w.hwnd && i.visible);
        let now = os.now();
        if reached(now, deadline) {
            return if visible { Front::NotForeground } else { Front::Gone };
        }
        let due = now.wrapping_sub(began) >= LADDER_AFTER_MS || reached(now.wrapping_add(POLL_MS), deadline);
        if visible && due && !raised {
            os.raise(&w);
            raised = true;
        }
        os.sleep(POLL_MS);
        list = os.instances(exe);
    }
}

/// The outcome of a wait: in front is `on_front`; on screen is no fallback; gone is NotReady.
fn settle(os: &mut dyn TakeoverOs, front: Front, on_front: Outcome, kind: Fallback) -> Outcome {
    match front {
        Front::Foreground => on_front,
        Front::NotForeground => Outcome::ShownNotForeground,
        Front::Gone => fallback(os, Why::NotReady, kind),
    }
}

/// Runs the fallback `kind` (Nothing is unreachable through the gate; the caller logs it).
fn fallback(os: &mut dyn TakeoverOs, why: Why, kind: Fallback) -> Outcome {
    match kind {
        Fallback::Taskmgr => {
            os.start_taskmgr();
        }
        Fallback::Reinject => {
            os.reinject();
        }
        Fallback::Nothing => {}
    }
    Outcome::FellBack(why, kind)
}

// ------------------------------------------------------------------ the real OS (D2)

/// At most this many windows of the Tasks class are looked at per poll.
const MAX_WINDOWS: usize = 32;
/// The Show frame is a few bytes; a longer one is not the contract's.
const SHOW_FRAME_MAX: usize = 64;
/// The stack of the launch thread: COM and one ShellExecute through the desktop shell.
const LAUNCH_STACK: usize = 256 * 1024;

/// The Tasks window class as FindWindowExW takes it: F-T1's WINDOW_CLASS, NUL-terminated, built once, by the compiler.
static CLASS_W: [u16; WINDOW_CLASS.len() + 1] = ascii_wide(WINDOW_CLASS);

fn class_w() -> &'static [u16] {
    &CLASS_W
}

/// The UTF-16 of an ASCII name and its NUL, at compile time (a name of another length or not ASCII fails the build).
#[allow(clippy::indexing_slicing)] // evaluated by the compiler only: an index out of bounds fails the build
const fn ascii_wide<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    assert!(b.len() + 1 == N, "one unit per byte, then the NUL");
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii(), "an ASCII class name");
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

/// The one frame a running Tasks is sent (F-T1): InstanceCmd::Show with from_chord and the defaults (page Processes,
/// no selection, sort Auto), at most SHOW_FRAME_MAX bytes.
pub(crate) fn show_frame() -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::new();
    encode(&InstanceCmd::Show(Show { from_chord: true, ..Default::default() }), &mut out)?;
    if out.len() > SHOW_FRAME_MAX {
        let len = u32::try_from(out.len()).unwrap_or(u32::MAX);
        return Err(FrameError::TooLong { len, max: SHOW_FRAME_MAX as u32 });
    }
    Ok(out)
}

/// The WM_COPYDATA parameters of `frame`: dwData is F-T1's tag; lpData points into `frame`, which outlives the send.
fn cds_of(frame: &[u8]) -> COPYDATASTRUCT {
    COPYDATASTRUCT { dwData: copydata_tag(), cbData: frame.len() as u32, lpData: frame.as_ptr().cast_mut().cast() }
}

/// A FILETIME as ms since 1601.
fn filetime_to_ms(ft: FILETIME) -> u64 {
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) / 10_000
}

/// The wall clock of `tick`, given the wall clock and the tick of now (wrapping ticks).
fn wall_of(wall_now_ms: u64, tick_now: u32, tick: u32) -> u64 {
    wall_now_ms.saturating_sub(u64::from(tick_now.wrapping_sub(tick)))
}

/// The OS of a delivery on the shell thread.
pub(crate) struct RealOs {
    /// Windows of the Tasks class not proven to be tasks.exe's (another image, or a process that cannot be read): the
    /// most seen in one poll, for the log line.
    squatters: Cell<u32>,
}

impl RealOs {
    pub(crate) fn new() -> RealOs {
        RealOs { squatters: Cell::new(0) }
    }

    pub(crate) fn squatters(&self) -> u32 {
        self.squatters.get()
    }
}

/// The instance behind `h` when its process image is `exe` (same_image: volume and file id); None for another image or
/// a process that cannot be read.
fn instance_of(h: HWND, exe: &Path) -> Option<Instance> {
    let mut pid = 0u32;
    // SAFETY: `pid` is writable for the call; a window gone meanwhile yields 0.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: a plain open by id; the handle is owned below.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // SAFETY: a fresh handle, owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let handle = HANDLE(process.as_raw_handle());
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has `len` writable elements; `len` is in (capacity) and out (length written, no NUL).
    unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let image = PathBuf::from(OsString::from_wide(buf.get(..len as usize)?));
    if !same_image(&image, exe) {
        return None;
    }
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = Default::default();
    // SAFETY: the handle is valid while `process` lives; the four FILETIMEs are writable.
    let timed = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }.is_ok();
    // A creation time that cannot be read is older than any press: such a window is never taken as a fresh one.
    let created_ms = if timed { filetime_to_ms(created) } else { 0 };
    // SAFETY: plain queries on a window handle; a stale one answers false.
    let (visible, hung) = unsafe { (IsWindowVisible(h).as_bool(), IsHungAppWindow(h).as_bool()) };
    Some(Instance { hwnd: h.0 as isize, pid, created_ms, visible, hung })
}

impl TakeoverOs for RealOs {
    fn now(&self) -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetTickCount() }
    }

    fn wall_ms(&self, tick: u32) -> u64 {
        // SAFETY: no preconditions, both read the clocks.
        let (wall, now) = unsafe { (GetSystemTimeAsFileTime(), GetTickCount()) };
        wall_of(filetime_to_ms(wall), now, tick)
    }

    fn sleep(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }

    fn tasks_exe(&self) -> Option<PathBuf> {
        suite_links::exe_path(App::Tasks).filter(|p| p.is_file())
    }

    fn instances(&self, exe: &Path) -> Vec<Instance> {
        let mut out = Vec::new();
        let mut squatters = 0u32;
        let mut after: Option<HWND> = None;
        for _ in 0..MAX_WINDOWS {
            // SAFETY: the class is a NUL-terminated buffer that lives for the program and a null name matches any
            // window; `after` is a window this loop found (one destroyed meanwhile ends the search with an error).
            let found = unsafe { FindWindowExW(None, after, PCWSTR(class_w().as_ptr()), PCWSTR::null()) };
            let Ok(h) = found else { break };
            after = Some(h);
            match instance_of(h, exe) {
                Some(i) => out.push(i),
                None => squatters += 1,
            }
        }
        self.squatters.set(self.squatters.get().max(squatters));
        out
    }

    fn send_show(&mut self, w: &Instance, frame: &[u8]) -> Answer {
        // SAFETY: no preconditions; a process gone meanwhile only fails, and the send decides.
        let _ = unsafe { AllowSetForegroundWindow(w.pid) };
        let cds = cds_of(frame);
        copydata_send(HWND(w.hwnd as *mut c_void), crate::hwnd(), &cds)
    }

    fn shell_ok(&self) -> bool {
        // SAFETY: no preconditions; a stale window answers false.
        unsafe {
            let shell = GetShellWindow();
            !shell.0.is_null() && !IsHungAppWindow(shell).as_bool()
        }
    }

    fn launch(&mut self, exe: &Path) -> Launch {
        let Some(file) = exe.to_str().map(str::to_owned) else {
            return Launch::Failed;
        };
        let args = suite_links::tasks_params(&Show { from_chord: true, ..Default::default() });
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<(), HRESULT>>(1);
        let body = move || {
            // SAFETY: the first COM call of this new thread; balanced below.
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            let r = match com.ok() {
                Ok(()) => desktop_shell_execute(&file, args.as_deref(), None, Verb::Open, SW_SHOWNORMAL.0)
                    .map_err(|e| e.code()),
                Err(e) => Err(e.code()),
            };
            if com.is_ok() {
                // SAFETY: balances the successful CoInitializeEx of this thread; the shell's objects are released.
                unsafe { CoUninitialize() };
            }
            // A shell thread that stopped waiting (the watchdog) is gone: nothing to tell.
            let _ = tx.send(r);
        };
        let spawned = std::thread::Builder::new().name("takeover-launch".into()).stack_size(LAUNCH_STACK).spawn(body);
        if spawned.is_err() {
            log!("takeover: no launch thread");
            return Launch::Failed;
        }
        match rx.recv_timeout(Duration::from_millis(u64::from(LAUNCH_WATCHDOG_MS))) {
            Ok(Ok(())) => Launch::Started,
            Ok(Err(hr)) => {
                log!("takeover: the desktop shell refused the launch ({:#010x})", hr.0);
                Launch::Failed
            }
            Err(RecvTimeoutError::Timeout) => {
                // The thread ends by itself when its call returns.
                log!("takeover: the desktop shell did not answer within {LAUNCH_WATCHDOG_MS} ms");
                Launch::Watchdog
            }
            Err(RecvTimeoutError::Disconnected) => Launch::Failed,
        }
    }

    fn foreground_pid(&self) -> Option<u32> {
        // SAFETY: no preconditions.
        let h = unsafe { GetForegroundWindow() };
        if h.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        // SAFETY: `pid` is writable for the call; a stale window yields 0.
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        (pid != 0).then_some(pid)
    }

    fn raise(&mut self, w: &Instance) {
        let h = HWND(w.hwnd as *mut c_void);
        // Posted to Tasks' own thread (SWP_ASYNCWINDOWPOS): the shell thread never waits on another process's window.
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
        let flash = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: h,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 3,
            dwTimeout: 0,
        };
        // SAFETY: plain calls on a window handle; a stale one only fails; `flash` lives for the call.
        unsafe {
            let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            let _ = SetWindowPos(h, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
            let _ = FlashWindowEx(&flash);
        }
    }

    fn start_taskmgr(&mut self) -> bool {
        // System32 from GetSystemDirectoryW, never from the environment.
        let Some(dir) = system_dir() else {
            log!("takeover: no system folder for Windows Task Manager");
            return false;
        };
        match spawn_breakaway(&dir.join("Taskmgr.exe"), "") {
            Ok(_process) => true,
            Err(_) => {
                log!("takeover: Windows Task Manager did not start");
                false
            }
        }
    }

    fn reinject(&mut self) -> bool {
        crate::hook::inject_ctrl_shift_esc()
    }
}

// ------------------------------------------------------------------ the shell-thread entry points (D3)

/// The sequence of the newest press this thread took for delivery (shell thread only).
static HANDLED: AtomicU16 = AtomicU16::new(0);
/// The breaker over the fallbacks (shell thread only).
static BREAKER: Mutex<Breaker> = Mutex::new(Breaker::CLOSED);

/// The WM_SHELL_TAKEOVER handling: the post is acknowledged first, then the press is delivered once.
pub(crate) fn on_message(wparam: usize, lparam: isize) {
    let Some((target, seq)) = crate::hook::decode_takeover(wparam) else {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Relaxed) {
            log!("takeover: a message with no known target, ignored");
        }
        return;
    };
    run(target, Req { seq, press: lparam as u32 });
}

/// After every shell pump: a press whose thread message a nested COM loop dropped is run now, whatever its age (a
/// stale one ends in the fallback, `deliver`'s step 0). Two atomic loads when nothing is lost.
pub(crate) fn run_lost() {
    let (sent, time) = crate::hook::takeover_last_sent();
    if let Some(seq) = lost_to_run(sent, HANDLED.load(Relaxed)) {
        run(TakeoverTarget::Tasks, Req { seq, press: time });
    }
}

/// The newest posted press when it is after the newest one taken (wrapping): it was swallowed and must be delivered.
fn lost_to_run(sent: u16, handled: u16) -> Option<u16> {
    ((sent.wrapping_sub(handled) as i16) > 0).then_some(sent)
}

/// Takes `seq` for delivery unless it is at or before `handled`, so each press is delivered once.
fn claim(handled: &mut u16, seq: u16) -> bool {
    let fresh = lost_to_run(seq, *handled).is_some();
    if fresh {
        *handled = seq;
    }
    fresh
}

/// One press, from either entry point.
fn run(target: TakeoverTarget, req: Req) {
    crate::hook::ack_takeover(req.seq);
    let frame = show_frame().unwrap_or_else(|_| {
        log!("takeover: the Show frame does not encode");
        Vec::new()
    });
    let mut os = RealOs::new();
    let (ap, reinject_ok) = (takeover::ap_cached(), takeover::REINJECT_OPENS_TASKMGR);
    let ran = run_with(&mut os, &HANDLED, &BREAKER, target, req, ap, reinject_ok, &frame);
    let Some((outcome, ms, paused)) = ran else {
        return;
    };
    log!("{}", outcome_line(outcome, ms, os.squatters()));
    if let Outcome::FellBack(_, kind) = outcome {
        if kind == Fallback::Nothing {
            log!("takeover: error: no fallback is allowed here, the gate should have passed the key");
        }
        incidents::push(IncidentKind::TakeoverFellBack);
    }
    if let Some(until) = paused {
        crate::hook::set_takeover_breaker(until);
        incidents::push(IncidentKind::TakeoverPaused);
        log!("takeover: paused 10 min after two fallbacks");
    }
}

/// `run` over `os`, `handled` and `breaker` (HANDLED and BREAKER on the shell thread): None when `req.seq` was
/// taken already, so each press is delivered once, or when its target is not built (the seq is taken all the same);
/// else `delivery`'s outcome, milliseconds and pause, with the breaker's new state stored back into `breaker`.
#[allow(clippy::too_many_arguments)] // run's glue: one call site and the tests
fn run_with(
    os: &mut dyn TakeoverOs,
    handled: &AtomicU16,
    breaker: &Mutex<Breaker>,
    target: TakeoverTarget,
    req: Req,
    ap: ApMode,
    reinject_ok: bool,
    frame: &[u8],
) -> Option<(Outcome, u32, Option<u32>)> {
    let mut taken = handled.load(Relaxed);
    if !claim(&mut taken, req.seq) {
        return None;
    }
    handled.store(taken, Relaxed);
    if target != TakeoverTarget::Tasks {
        // Reserved for the Files takeover: the hook never swallows it in this release.
        log!("takeover: this target is not built");
        return None;
    }
    let mut state = *breaker.lock().unwrap_or_else(PoisonError::into_inner);
    let ran = delivery(os, &mut state, req, ap, reinject_ok, frame);
    *breaker.lock().unwrap_or_else(PoisonError::into_inner) = state;
    Some(ran)
}

/// One delivery and its bookkeeping: the outcome, its milliseconds and, when this fallback tripped the breaker, the
/// tick the pause lasts until (bit 0 set). Pure over `os` and `breaker`.
fn delivery(
    os: &mut dyn TakeoverOs,
    breaker: &mut Breaker,
    req: Req,
    ap: ApMode,
    reinject_ok: bool,
    frame: &[u8],
) -> (Outcome, u32, Option<u32>) {
    let t0 = os.now();
    let outcome = deliver(os, req, ap, reinject_ok, frame);
    let now = os.now();
    let fell_back = matches!(outcome, Outcome::FellBack(..));
    let paused = (fell_back && breaker.on_fallback(now)).then(|| breaker.open_until());
    (outcome, now.wrapping_sub(t0), paused)
}

/// The one log line of a delivery: its kind and milliseconds, never a path, a title or a name.
fn outcome_line(o: Outcome, ms: u32, squatters: u32) -> String {
    let (kind, sep, why, fallback) = match o {
        Outcome::Activated => ("activated", "", "", ""),
        Outcome::Launched => ("launched", "", "", ""),
        Outcome::ShownNotForeground => ("shown-not-foreground", "", "", ""),
        Outcome::FellBack(w, f) => ("fellback", " ", why_name(w), fallback_name(f)),
    };
    let line = format!("takeover: {kind}{sep}{why}{sep}{fallback} in {ms} ms");
    if squatters == 0 {
        return line;
    }
    format!("{line}, {squatters} squatter")
}

fn why_name(w: Why) -> &'static str {
    match w {
        Why::Stale => "stale",
        Why::ExeMissing => "exe-missing",
        Why::InstanceNoAnswer => "instance-no-answer",
        Why::InstanceHung => "instance-hung",
        Why::NoShell => "no-shell",
        Why::LaunchFailed => "launch-failed",
        Why::NotReady => "not-ready",
    }
}

fn fallback_name(f: Fallback) -> &'static str {
    match f {
        Fallback::Taskmgr => "taskmgr",
        Fallback::Reinject => "reinject",
        Fallback::Nothing => "nothing",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The one timed send of the takeover sits in `fn copydata_send`, which waits with SMTO_BLOCK (lint 19 lists it).
    #[test]
    fn copydata_send_is_the_listed_send() {
        let src = include_str!("takeover_os.rs");
        let code = &src[..src.find("#[cfg(test)]").expect("tests start")];
        let sends: Vec<_> =
            code.match_indices("SendMessageTimeoutW(").filter(|(i, _)| !code[..*i].ends_with("// ")).collect();
        assert_eq!(sends.len(), 1, "exactly one timed send");
        let from = code.find("fn copydata_send(").expect("the function exists");
        let body = &code[from..code.find("/// The mapping of a send").expect("the next item")];
        assert!(body.contains("SendMessageTimeoutW("), "the send is inside copydata_send");
        assert!(body.contains("SMTO_BLOCK") && body.contains("SMTO_ABORTIFHUNG"));
    }

    #[test]
    fn answers_map_as_written() {
        assert_eq!(answer_of(true, 1, false), Answer::Accepted);
        assert_eq!(answer_of(true, 0, false), Answer::Refused);
        assert_eq!(answer_of(false, 0, true), Answer::Timeout);
        assert_eq!(answer_of(false, 0, false), Answer::Refused);
    }

    // ---- D1: the decision over a scripted OS

    /// The fake tick of the press, and the wall clock (ms since 1601) of tick 0.
    const T0: u32 = 50_000;
    const WALL0: u64 = 13_400_000_000_000;
    const FRAME: &[u8] = &[1, 2, 3];
    /// A Tasks that ran before the press, and one whose process started after it.
    const RUNNING: Instance =
        Instance { hwnd: 0x10, pid: 10, created_ms: WALL0 + T0 as u64 - 60_000, visible: true, hung: false };
    const FRESH: Instance =
        Instance { hwnd: 0x20, pid: 20, created_ms: WALL0 + T0 as u64 + 30, visible: true, hung: false };

    fn with(i: Instance, visible: bool, hung: bool) -> Instance {
        Instance { visible, hung, ..i }
    }

    /// An OS whose windows and foreground are scripted per poll (a poll is one sleep), with a clock that advances by
    /// every sleep, send and launch, and a record of every call.
    struct FakeOs {
        t: u32,
        polls: u32,
        exe: bool,
        script: Box<dyn Fn(u32) -> Vec<Instance>>,
        front: Box<dyn Fn(u32) -> Option<u32>>,
        answer: Answer,
        send_ms: u32,
        shell: bool,
        launch: Launch,
        launch_ms: u32,
        /// The poll of every instances() call.
        reads: RefCell<Vec<u32>>,
        sends: u32,
        launches: u32,
        /// The tick of every raise and of every start of Windows Task Manager.
        raises: Vec<u32>,
        taskmgr: Vec<u32>,
        reinjects: u32,
    }

    impl FakeOs {
        fn new() -> FakeOs {
            FakeOs {
                t: T0,
                polls: 0,
                exe: true,
                script: Box::new(|_| Vec::new()),
                front: Box::new(|_| None),
                answer: Answer::Accepted,
                send_ms: 5,
                shell: true,
                launch: Launch::Started,
                launch_ms: 50,
                reads: RefCell::new(Vec::new()),
                sends: 0,
                launches: 0,
                raises: Vec::new(),
                taskmgr: Vec::new(),
                reinjects: 0,
            }
        }

        fn windows(mut self, f: impl Fn(u32) -> Vec<Instance> + 'static) -> FakeOs {
            self.script = Box::new(f);
            self
        }

        fn front_from(mut self, poll: u32, pid: u32) -> FakeOs {
            self.front = Box::new(move |p| (p >= poll).then_some(pid));
            self
        }

        fn spent(&self) -> u32 {
            self.t - T0
        }

        fn fell_back(&self) -> usize {
            self.taskmgr.len() + self.reinjects as usize
        }
    }

    impl TakeoverOs for FakeOs {
        fn now(&self) -> u32 {
            self.t
        }
        fn wall_ms(&self, tick: u32) -> u64 {
            WALL0 + u64::from(tick)
        }
        fn sleep(&mut self, ms: u32) {
            self.t = self.t.wrapping_add(ms);
            self.polls += 1;
        }
        fn tasks_exe(&self) -> Option<PathBuf> {
            self.exe.then(|| PathBuf::from("tasks.exe"))
        }
        fn instances(&self, _exe: &Path) -> Vec<Instance> {
            self.reads.borrow_mut().push(self.polls);
            (self.script)(self.polls)
        }
        fn send_show(&mut self, _w: &Instance, frame: &[u8]) -> Answer {
            assert_eq!(frame, FRAME);
            self.sends += 1;
            self.t += self.send_ms;
            self.answer
        }
        fn shell_ok(&self) -> bool {
            self.shell
        }
        fn launch(&mut self, _exe: &Path) -> Launch {
            self.launches += 1;
            self.t += self.launch_ms;
            self.launch
        }
        fn foreground_pid(&self) -> Option<u32> {
            (self.front)(self.polls)
        }
        fn raise(&mut self, _w: &Instance) {
            self.raises.push(self.t);
        }
        fn start_taskmgr(&mut self) -> bool {
            self.taskmgr.push(self.t);
            true
        }
        fn reinject(&mut self) -> bool {
            self.reinjects += 1;
            true
        }
    }

    fn go(os: &mut FakeOs) -> Outcome {
        deliver(os, Req { seq: 1, press: T0 }, ApMode::Off, false, FRAME)
    }

    #[test]
    fn fresh_launch_foreground() {
        let mut os = FakeOs::new().windows(|p| if p >= 4 { vec![FRESH] } else { vec![] }).front_from(6, FRESH.pid);
        assert_eq!(go(&mut os), Outcome::Launched);
        assert_eq!((os.launches, os.sends, os.raises.len(), os.taskmgr.len()), (1, 0, 0, 0));
    }

    #[test]
    fn running_instance_activated() {
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(2, RUNNING.pid);
        assert_eq!(go(&mut os), Outcome::Activated);
        assert_eq!((os.sends, os.launches, os.fell_back()), (1, 0, 0));
    }

    #[test]
    fn running_but_hung_falls_back() {
        for visible in [true, false] {
            let mut os = FakeOs::new().windows(move |_| vec![with(RUNNING, visible, true)]);
            assert_eq!(go(&mut os), Outcome::FellBack(Why::InstanceHung, Fallback::Taskmgr), "visible {visible}");
            assert_eq!((os.sends, os.launches, os.taskmgr.len()), (0, 0, 1));
        }
    }

    #[test]
    fn copydata_timeout_falls_back() {
        for answer in [Answer::Timeout, Answer::Refused] {
            let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
            os.answer = answer;
            os.send_ms = COPYDATA_TIMEOUT_MS;
            assert_eq!(go(&mut os), Outcome::FellBack(Why::InstanceNoAnswer, Fallback::Taskmgr), "{answer:?}");
            assert_eq!((os.sends, os.launches, os.taskmgr.clone()), (1, 0, vec![T0 + COPYDATA_TIMEOUT_MS]));
        }
        // A frame that could not be encoded is never sent.
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
        let outcome = deliver(&mut os, Req { seq: 1, press: T0 }, ApMode::Off, false, &[]);
        assert_eq!(outcome, Outcome::FellBack(Why::InstanceNoAnswer, Fallback::Taskmgr));
        assert_eq!(os.sends, 0);
    }

    #[test]
    fn exe_missing() {
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]);
        os.exe = false;
        assert_eq!(go(&mut os), Outcome::FellBack(Why::ExeMissing, Fallback::Taskmgr));
        assert!(os.reads.borrow().is_empty());
        assert_eq!((os.sends, os.launches, os.taskmgr.clone()), (0, 0, vec![T0]));
    }

    #[test]
    fn stale_request() {
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
        let outcome = deliver(&mut os, Req { seq: 1, press: T0 - STALE_MS - 1 }, ApMode::Off, false, FRAME);
        assert_eq!(outcome, Outcome::FellBack(Why::Stale, Fallback::Taskmgr));
        assert!(os.reads.borrow().is_empty());
        assert_eq!((os.sends, os.launches, os.taskmgr.len()), (0, 0, 1));
        // STALE_MS old exactly is still delivered.
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
        let outcome = deliver(&mut os, Req { seq: 1, press: T0 - STALE_MS }, ApMode::Off, false, FRAME);
        assert_eq!(outcome, Outcome::Activated);
        // The age is taken across a tick wrap.
        for (age, stale) in [(STALE_MS, false), (STALE_MS + 1, true)] {
            let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
            os.t = 100;
            let outcome = deliver(&mut os, Req { seq: 1, press: 100u32.wrapping_sub(age) }, ApMode::Off, false, FRAME);
            assert_eq!(outcome == Outcome::FellBack(Why::Stale, Fallback::Taskmgr), stale, "{age} ms");
        }
    }

    #[test]
    fn no_shell() {
        let mut os = FakeOs::new();
        os.shell = false;
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NoShell, Fallback::Taskmgr));
        assert_eq!((os.launches, os.taskmgr.len()), (0, 1));
    }

    #[test]
    fn launch_watchdog() {
        for (launch, ms) in [(Launch::Watchdog, LAUNCH_WATCHDOG_MS), (Launch::Failed, 20)] {
            let mut os = FakeOs::new();
            os.launch = launch;
            os.launch_ms = ms;
            assert_eq!(go(&mut os), Outcome::FellBack(Why::LaunchFailed, Fallback::Taskmgr), "{launch:?}");
            assert_eq!((os.launches, os.taskmgr.clone()), (1, vec![T0 + ms]));
        }
    }

    #[test]
    fn never_ready() {
        let mut os = FakeOs::new();
        os.launch_ms = LAUNCH_WATCHDOG_MS;
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert!(os.spent() <= LAUNCH_WATCHDOG_MS + READY_WAIT_MS + 2 * POLL_MS, "{} ms", os.spent());
        assert_eq!(os.launches, 1);
    }

    #[test]
    fn old_window_is_not_ready() {
        // Not seen at the press, visible from poll 2, but its process is older than the press: never accepted.
        let mut os = FakeOs::new().windows(|p| if p >= 2 { vec![RUNNING] } else { vec![] }).front_from(3, RUNNING.pid);
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert!(os.raises.is_empty());
        // A process created up to CREATE_SLACK_MS before the press counts as new (tick granularity), not 1 ms more.
        for (slack, outcome) in [
            (CREATE_SLACK_MS, Outcome::Launched),
            (CREATE_SLACK_MS + 1, Outcome::FellBack(Why::NotReady, Fallback::Taskmgr)),
        ] {
            let w = Instance { created_ms: WALL0 + u64::from(T0) - slack, ..FRESH };
            let mut os = FakeOs::new().windows(move |p| if p >= 2 { vec![w] } else { vec![] }).front_from(3, w.pid);
            assert_eq!(go(&mut os), outcome, "{slack} ms before the press");
        }
    }

    #[test]
    fn starting_instance_is_awaited() {
        let mut os = FakeOs::new().windows(|p| vec![with(RUNNING, p >= 8, false)]).front_from(10, RUNNING.pid);
        assert_eq!(go(&mut os), Outcome::Activated);
        assert_eq!((os.sends, os.launches, os.fell_back()), (0, 0, 0));
    }

    #[test]
    fn starting_instance_never_shows() {
        let mut os = FakeOs::new().windows(|_| vec![with(RUNNING, false, false)]);
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert!(os.spent() <= READY_WAIT_MS + 2 * POLL_MS, "{} ms", os.spent());
        assert_eq!((os.sends, os.launches), (0, 0));
    }

    #[test]
    fn a_hung_and_a_starting_instance() {
        let hung = with(RUNNING, true, true);
        let mut os = FakeOs::new().windows(move |p| vec![hung, with(FRESH, p >= 3, false)]).front_from(4, FRESH.pid);
        assert_eq!(go(&mut os), Outcome::Activated);
        assert_eq!((os.sends, os.launches, os.fell_back()), (0, 0, 0));
        // Never shown: the starting path's NotReady, not InstanceHung.
        let mut os = FakeOs::new().windows(move |_| vec![hung, with(FRESH, false, false)]);
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert_eq!((os.sends, os.launches), (0, 0));
    }

    #[test]
    fn window_gone_after_accept() {
        let mut os = FakeOs::new().windows(|p| if p < 3 { vec![RUNNING] } else { vec![] });
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert_eq!(os.taskmgr, vec![T0 + os.send_ms + READY_WAIT_MS], "at the deadline");
    }

    #[test]
    fn hidden_after_accept() {
        let mut os = FakeOs::new().windows(|p| vec![with(RUNNING, p < 3, false)]);
        assert_eq!(go(&mut os), Outcome::FellBack(Why::NotReady, Fallback::Taskmgr));
        assert!(os.raises.is_empty(), "a hidden window is not raised");
    }

    #[test]
    fn instances_reread_every_poll() {
        let cases = [
            ("running", FakeOs::new().windows(|_| vec![RUNNING])),
            ("starting", FakeOs::new().windows(|p| vec![with(RUNNING, p >= 20, false)])),
            ("starting, never shown", FakeOs::new().windows(|_| vec![with(RUNNING, false, false)])),
            ("fresh", FakeOs::new().windows(|p| if p >= 7 { vec![FRESH] } else { vec![] })),
            ("fresh, never shown", FakeOs::new()),
        ];
        for (name, mut os) in cases {
            go(&mut os);
            assert_eq!(*os.reads.borrow(), (0..=os.polls).collect::<Vec<_>>(), "{name}: one read per poll");
            assert!(os.polls >= READY_WAIT_MS / POLL_MS, "{name}: the wait ran to its deadline");
        }
    }

    #[test]
    fn ladder_once() {
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(900 / POLL_MS, RUNNING.pid);
        assert_eq!(go(&mut os), Outcome::Activated);
        assert_eq!(os.raises, vec![T0 + os.send_ms + LADDER_AFTER_MS], "once, at 300 ms");
        // A window found at the last poll before the deadline is raised there.
        let last = READY_WAIT_MS / POLL_MS - 1;
        let mut os = FakeOs::new().windows(move |p| vec![with(RUNNING, p >= last, false)]);
        assert_eq!(go(&mut os), Outcome::ShownNotForeground);
        assert_eq!(os.raises, vec![T0 + last * POLL_MS]);
    }

    #[test]
    fn shown_not_foreground_is_no_fallback() {
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]);
        assert_eq!(go(&mut os), Outcome::ShownNotForeground);
        assert_eq!((os.fell_back(), os.raises.len()), (0, 1));
    }

    /// One script per fallback reason, with the press it is delivered for.
    fn fallback_cases() -> Vec<(Why, u32, FakeOs)> {
        let mut missing = FakeOs::new();
        missing.exe = false;
        let mut no_answer = FakeOs::new().windows(|_| vec![RUNNING]);
        no_answer.answer = Answer::Timeout;
        let mut no_shell = FakeOs::new();
        no_shell.shell = false;
        let mut failed = FakeOs::new();
        failed.launch = Launch::Watchdog;
        vec![
            (Why::Stale, T0 - STALE_MS - 1, FakeOs::new()),
            (Why::ExeMissing, T0, missing),
            (Why::InstanceNoAnswer, T0, no_answer),
            (Why::InstanceHung, T0, FakeOs::new().windows(|_| vec![with(RUNNING, true, true)])),
            (Why::NoShell, T0, no_shell),
            (Why::LaunchFailed, T0, failed),
            (Why::NotReady, T0, FakeOs::new()),
        ]
    }

    #[test]
    fn ap_on_reinjects_only_when_proven() {
        for (why, press, mut os) in fallback_cases() {
            let outcome = deliver(&mut os, Req { seq: 1, press }, ApMode::On, false, FRAME);
            assert_eq!(outcome, Outcome::FellBack(why, Fallback::Nothing));
            assert_eq!((os.taskmgr.len(), os.reinjects), (0, 0), "{why:?}");
        }
        for (why, press, mut os) in fallback_cases() {
            let outcome = deliver(&mut os, Req { seq: 1, press }, ApMode::On, true, FRAME);
            assert_eq!(outcome, Outcome::FellBack(why, Fallback::Reinject));
            assert_eq!((os.taskmgr.len(), os.reinjects), (0, 1), "{why:?}");
        }
        assert_eq!(fallback_kind(ApMode::Off, false), Fallback::Taskmgr);
        assert_eq!(fallback_kind(ApMode::Off, true), Fallback::Taskmgr);
        assert_eq!(fallback_kind(ApMode::On, false), Fallback::Nothing);
        assert_eq!(fallback_kind(ApMode::On, true), Fallback::Reinject);
    }

    #[test]
    fn unknown_ap_never_starts_taskmgr() {
        for reinject_ok in [false, true] {
            for (why, press, mut os) in fallback_cases() {
                let kind = if reinject_ok { Fallback::Reinject } else { Fallback::Nothing };
                let outcome = deliver(&mut os, Req { seq: 1, press }, ApMode::Unknown, reinject_ok, FRAME);
                assert_eq!(outcome, Outcome::FellBack(why, kind));
                assert!(os.taskmgr.is_empty(), "{why:?}");
                assert_eq!(fallback_kind(ApMode::Unknown, reinject_ok), kind);
            }
        }
    }

    #[test]
    fn breaker_trips_on_the_second_fallback_within_10_min() {
        let mut b = Breaker::default();
        assert!(!b.on_fallback(1_000));
        assert!(b.on_fallback(1_000 + BREAKER_WINDOW_MS), "10 min exactly is within");
        assert_eq!(b.open_until(), (1_000 + BREAKER_WINDOW_MS + BREAKER_PAUSE_MS) | 1);
        // Tripping starts the count again.
        assert!(!b.on_fallback(1_000 + BREAKER_WINDOW_MS + 5));
        assert!(b.on_fallback(1_000 + BREAKER_WINDOW_MS + 9));
        // Across a tick wrap.
        let mut b = Breaker::CLOSED;
        assert!(!b.on_fallback(u32::MAX - 100));
        assert!(b.on_fallback(200));
        assert_eq!(b.open_until(), (200 + BREAKER_PAUSE_MS) | 1);
    }

    #[test]
    fn breaker_does_not_trip_after_10_min_and_1_ms() {
        let mut b = Breaker::default();
        assert!(!b.on_fallback(1_000));
        assert!(!b.on_fallback(1_000 + BREAKER_WINDOW_MS + 1));
        assert_eq!(b.open_until(), 0, "never tripped");
        // That one and the next within 10 min of it do.
        assert!(b.on_fallback(1_000 + 2 * BREAKER_WINDOW_MS));
    }

    #[test]
    fn every_path_ends() {
        let answers = [Answer::Accepted, Answer::Refused, Answer::Timeout];
        let launches = [Launch::Started, Launch::Failed, Launch::Watchdog];
        for n in 0..(1u32 << 7) * 9 {
            let bit = |i: u32| (n >> i) & 1 == 1;
            let (exe, at_press, visible, hung, shell) = (bit(0), bit(1), bit(2), bit(3), bit(6));
            let shows = bit(4).then_some(5);
            let w = if at_press { RUNNING } else { FRESH };
            let mut os = FakeOs::new().windows(move |p| {
                if shows.is_some_and(|s| p >= s) {
                    vec![with(w, true, false)]
                } else if at_press {
                    vec![with(w, visible, hung)]
                } else {
                    vec![]
                }
            });
            if bit(5) {
                os = os.front_from(9, w.pid);
            }
            os.exe = exe;
            os.shell = shell;
            os.answer = answers[(n >> 7) as usize % 3];
            os.send_ms = COPYDATA_TIMEOUT_MS;
            os.launch = launches[(n >> 7) as usize / 3];
            os.launch_ms = LAUNCH_WATCHDOG_MS;
            let outcome = go(&mut os);
            assert!(os.spent() <= 2_000, "case {n}: {outcome:?} after {} ms", os.spent());
            let fell_back = matches!(outcome, Outcome::FellBack(_, Fallback::Taskmgr));
            assert_eq!(os.taskmgr.len(), usize::from(fell_back), "case {n}: {outcome:?}");
        }
    }

    // ---- D2: the pure parts of the real OS

    #[test]
    fn filetime_to_ms_of_a_known_time() {
        let ft = |v: u64| FILETIME { dwLowDateTime: v as u32, dwHighDateTime: (v >> 32) as u32 };
        // 1970-01-01 00:00:00 UTC, 11,644,473,600 s after 1601-01-01, in 100 ns units.
        let unix = 116_444_736_000_000_000u64;
        assert_eq!(filetime_to_ms(ft(unix)), 11_644_473_600_000);
        assert_eq!(filetime_to_ms(ft(unix + 12_345_678)), 11_644_473_601_234, "truncated to the ms");
        assert_eq!(filetime_to_ms(ft(0)), 0);
    }

    #[test]
    fn wall_ms_of_a_past_tick() {
        assert_eq!(wall_of(1_000_000, 5_000, 4_000), 999_000);
        assert_eq!(wall_of(1_000_000, 100, u32::MAX - 899), 999_000, "across a tick wrap");
        assert_eq!(wall_of(500, 5_000, 0), 0, "never before 1601");
    }

    #[test]
    fn cds_of_points_at_the_frame() {
        let frame = [7u8, 8, 9, 10];
        let cds = cds_of(&frame);
        assert_eq!(cds.dwData, oled_trust::frame::tasks::copydata_tag());
        assert_eq!(cds.cbData, 4);
        assert_eq!(cds.lpData.cast_const().cast::<u8>(), frame.as_ptr());
    }

    #[test]
    fn class_matches_the_contract() {
        let class = class_w();
        assert_eq!(class.last(), Some(&0));
        assert_eq!(class.iter().filter(|&&c| c == 0).count(), 1, "one NUL, at the end");
        assert_eq!(String::from_utf16(&class[..class.len() - 1]).unwrap(), oled_trust::frame::tasks::WINDOW_CLASS);
        assert!(std::ptr::eq(class, class_w()), "built once");
    }

    #[test]
    fn show_frame_is_the_contract() {
        let frame = show_frame().unwrap();
        assert!(frame.len() <= 64, "{} bytes", frame.len());
        let (cmd, used) = oled_trust::frame::decode::<InstanceCmd>(&frame).unwrap();
        assert_eq!(cmd, InstanceCmd::Show(Show { from_chord: true, ..Default::default() }));
        assert_eq!(used, frame.len(), "one frame, every byte");
    }

    /// The part of this file before its tests.
    fn code() -> &'static str {
        let src = include_str!("takeover_os.rs");
        &src[..src.find("#[cfg(test)]").expect("tests start")]
    }

    #[test]
    fn no_environment_paths() {
        let code = code();
        for needle in ["var(\"SystemRoot\")", "%SystemRoot%", "var(\"windir\")", "%windir%", "env::var"] {
            assert!(!code.contains(needle), "{needle}");
        }
        assert!(code.contains("system_dir()"), "System32 comes from GetSystemDirectoryW");
    }

    #[test]
    fn send_show_uses_copydata_send() {
        let code = code();
        let code = code.replace("\r\n", "\n"); // a CRLF checkout (core.autocrlf) reads as LF
        let real = code.find("impl TakeoverOs for RealOs").expect("the real OS");
        let from = real + code[real..].find("fn send_show(").expect("its send_show");
        let body = &code[from..from + code[from..].find("\n    }\n").expect("its end")];
        assert!(body.contains("copydata_send("), "{body}");
        assert!(!body.contains("SendMessageTimeoutW"), "no send of its own: {body}");
    }

    // ---- D3: the entry points

    #[test]
    fn lost_to_run_wraps() {
        assert_eq!(lost_to_run(0, 0xFFFF), Some(0));
        assert_eq!(lost_to_run(5, 5), None, "equal");
        assert_eq!(lost_to_run(4, 5), None, "before handled");
        assert_eq!(lost_to_run(6, 5), Some(6));
        assert_eq!(lost_to_run(0xFFFF, 0), None, "behind across the wrap");
        assert_eq!(lost_to_run(0, 0), None, "nothing posted yet");
    }

    /// `run`'s path over `os`: `run_with` with the test's HANDLED and BREAKER, target Tasks, Administrator protection
    /// off, no re-injection.
    fn run_tasks(
        os: &mut FakeOs,
        handled: &AtomicU16,
        breaker: &Mutex<Breaker>,
        req: Req,
    ) -> Option<(Outcome, u32, Option<u32>)> {
        run_with(os, handled, breaker, TakeoverTarget::Tasks, req, ApMode::Off, false, FRAME)
    }

    #[test]
    fn lost_message_runs_once() {
        let (handled, breaker) = (AtomicU16::new(4), Mutex::new(Breaker::CLOSED));
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(1, RUNNING.pid);
        // run_lost: the newest post (5) never reached the thread.
        let seq = lost_to_run(5, handled.load(Relaxed)).expect("lost");
        let ran = run_tasks(&mut os, &handled, &breaker, Req { seq, press: T0 });
        assert_eq!(ran.map(|(outcome, _, paused)| (outcome, paused)), Some((Outcome::Activated, None)));
        assert_eq!(handled.load(Relaxed), 5, "taken");
        // Its thread message arrives after all (on_message's decode): nothing more, and a second run_lost does nothing.
        let wparam = crate::hook::encode_takeover(TakeoverTarget::Tasks, 5);
        let (target, seq) = crate::hook::decode_takeover(wparam).expect("decodes");
        let again = run_with(&mut os, &handled, &breaker, target, Req { seq, press: T0 }, ApMode::Off, false, FRAME);
        assert_eq!(again, None);
        assert_eq!(lost_to_run(5, handled.load(Relaxed)), None);
        assert_eq!((os.sends, os.launches, os.fell_back()), (1, 0, 0));
        // A post of a target not built is taken all the same and delivers nothing.
        let req = Req { seq: 6, press: os.t };
        let files = run_with(&mut os, &handled, &breaker, TakeoverTarget::Files, req, ApMode::Off, false, FRAME);
        assert_eq!((files, handled.load(Relaxed)), (None, 6));
        assert_eq!((os.sends, os.launches, os.fell_back()), (1, 0, 0));
        assert_eq!(*breaker.lock().unwrap(), Breaker::CLOSED, "no fallback counted");
    }

    #[test]
    fn lost_and_stale_still_falls_back() {
        let (handled, breaker) = (AtomicU16::new(4), Mutex::new(Breaker::CLOSED));
        let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
        let seq = lost_to_run(5, handled.load(Relaxed)).expect("lost, whatever its age");
        let ran = run_tasks(&mut os, &handled, &breaker, Req { seq, press: T0 - 3_000 });
        assert_eq!(
            ran.map(|(outcome, _, paused)| (outcome, paused)),
            Some((Outcome::FellBack(Why::Stale, Fallback::Taskmgr), None))
        );
        assert_eq!((os.taskmgr.len(), os.launches, os.sends), (1, 0, 0));
        assert_ne!(*breaker.lock().unwrap(), Breaker::CLOSED, "the fallback is counted in the stored breaker");
        assert_eq!(lost_to_run(5, handled.load(Relaxed)), None, "a second run_lost does nothing");
    }

    #[test]
    fn outcome_log_has_no_path() {
        let whys = [
            Why::Stale,
            Why::ExeMissing,
            Why::InstanceNoAnswer,
            Why::InstanceHung,
            Why::NoShell,
            Why::LaunchFailed,
            Why::NotReady,
        ];
        let mut all = vec![Outcome::Activated, Outcome::Launched, Outcome::ShownNotForeground];
        for why in whys {
            for kind in [Fallback::Taskmgr, Fallback::Reinject, Fallback::Nothing] {
                all.push(Outcome::FellBack(why, kind));
            }
        }
        for o in all {
            for squatters in [0, 3] {
                let line = outcome_line(o, 1234, squatters);
                let rest = line.strip_prefix("takeover: ").expect("the prefix");
                assert!(!rest.contains(':') && !rest.contains('\\'), "{line}");
                assert!(rest.contains(" in 1234 ms"), "{line}");
            }
        }
        let fell = Outcome::FellBack(Why::ExeMissing, Fallback::Taskmgr);
        assert_eq!(outcome_line(fell, 7, 0), "takeover: fellback exe-missing taskmgr in 7 ms");
        assert_eq!(
            outcome_line(Outcome::ShownNotForeground, 1800, 1),
            "takeover: shown-not-foreground in 1800 ms, 1 squatter"
        );
    }

    #[test]
    fn a_tripped_breaker_publishes_bit_0() {
        for now in [2u32, 1_000, 599_998, u32::MAX - 10] {
            let mut b = Breaker::CLOSED;
            assert!(!b.on_fallback(now.wrapping_sub(60_000)));
            assert!(b.on_fallback(now));
            assert_eq!(b.open_until() & 1, 1, "{now}");
            assert_eq!(b.open_until() | 1, now.wrapping_add(BREAKER_PAUSE_MS) | 1);
        }
        // Through run's path: two fallbacks a minute apart, each over its own OS and the one stored breaker, publish
        // the pause on the second; deliveries that did not fall back publish nothing.
        let (handled, breaker) = (AtomicU16::new(0), Mutex::new(Breaker::CLOSED));
        for (i, paused) in [(0u32, false), (1, true)] {
            let mut os = FakeOs::new();
            os.exe = false;
            os.t = T0 + i * 60_000;
            let req = Req { seq: i as u16 + 1, press: os.t };
            let ran = run_tasks(&mut os, &handled, &breaker, req);
            let (outcome, _, published) = ran.expect("a fresh seq runs");
            assert_eq!(outcome, Outcome::FellBack(Why::ExeMissing, Fallback::Taskmgr));
            assert_eq!(published.is_some(), paused);
            assert!(published.is_none_or(|until| until & 1 == 1 && until == (T0 + 60_000 + BREAKER_PAUSE_MS) | 1));
        }
        let tripped = Breaker { open_until: (T0 + 60_000 + BREAKER_PAUSE_MS) | 1, ..Breaker::CLOSED };
        assert_eq!(*breaker.lock().unwrap(), tripped, "the trip is stored back and the count starts again");
        let (handled, breaker) = (AtomicU16::new(0), Mutex::new(Breaker::CLOSED));
        for seq in 1..=3 {
            let mut os = FakeOs::new().windows(|_| vec![RUNNING]).front_from(0, RUNNING.pid);
            let ran = run_tasks(&mut os, &handled, &breaker, Req { seq, press: T0 });
            assert_eq!(ran, Some((Outcome::Activated, os.send_ms, None)));
        }
        assert_eq!(*breaker.lock().unwrap(), Breaker::CLOSED);
    }

    #[test]
    fn ack_comes_first() {
        let code = code();
        let from = code.find("fn run(target: TakeoverTarget, req: Req) {").expect("run");
        let first = code[from..].lines().nth(1).expect("a body").trim();
        assert_eq!(first, "crate::hook::ack_takeover(req.seq);");
    }
}
