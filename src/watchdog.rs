//! Watchdog: the same exe launched with `--watchdog <pid> <heartbeat-event-name>`, plus the client
//! side in the dock (launch, heartbeat) and the named stop event used by `--restore`.
//!
//! PROTOCOL
//!  * Heartbeat event: name `Local\DockHeartbeat-<pid>` (auto-reset), created by the dock with
//!    `CreateEventW`; the dock signals it at most once a second while it handles messages (`beat_if_due`, at the
//!    start of each message), and every 3 s when idle (`TIMER_SECOND`, `HEARTBEAT_MS`).
//!  * The watchdog opens the dock process (`SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION`) and
//!    the event, then waits (`WaitForMultipleObjects`) on the process and the event with a 3.3 s timeout
//!    (sized for the 3 s keepalive; while the dock handles messages each wait returns at a beat). 2 timeouts
//!    in a row (6.6 s without a beat) are a hang; before the first beat, 10 (33 s). On a hang it runs
//!    `taskbar::restore_force()` but stays alive as long as the dock lives (a stuck dock can recover: the dock hides
//!    the taskbar again once it handles messages again, unless a stop was requested (`--restore`, `--install`,
//!    `--uninstall`), see `stop_requested` and `alive`). If the process ends with
//!    exit code != 0 (including abort/crash) it restores and exits; with code 0 it exits doing nothing (the
//!    dock has already restored). It does NOT relaunch the dock. Before restoring it waits up to 30 s for a
//!    `Shell_TrayWnd` (Explorer restarting).
//!  * Startup heartbeat event: `Local\DockHeartbeat-<pid>-startup` (auto-reset), signaled by
//!    `taskbar::init` while waiting for `Shell_TrayWnd`: it resets the count but does NOT move the
//!    watchdog from the startup tolerance (10) to the steady-state one (2), which starts at the first
//!    real heartbeat (`heartbeat`).
//!  * Single instance: named mutex `Local\DockInstance`, taken by `main` right after parsing the
//!    arguments, before the data folder (`datadir::init`), the logger and anything else. The watchdog,
//!    `--restore` and `--install` never take it; `--restore` polls it (`instance_running`) to wait
//!    for a running dock to exit.
//!  * Stop event: fixed name `Local\DockStop`, created by the dock at startup (manual-reset); a
//!    `stop-watch` thread waits on it and, if it fires, posts `WM_DOCK_QUIT_REQUEST` to the window.
//!    `--restore` signals it with `signal_stop_to_instance()`.
//!
//! THREADING: `spawn`, `heartbeat`, `beat_if_due`, `start_stop_watcher`, `shutdown`, `end_for_session_end` from the
//! main/UI thread;
//! `run_watchdog` is the main of the watchdog process (no window, no hook, no COM).

use std::ffi::c_void;
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering::Relaxed};
use std::thread::JoinHandle;

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, E_ACCESSDENIED, HANDLE, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, CreateProcessW, GetCurrentProcess, GetExitCodeProcess, OpenEventW, OpenMutexW,
    OpenProcess, ProcessMemoryPriority, ResetEvent, SetEvent, SetProcessInformation, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, EVENT_MODIFY_STATE,
    INFINITE, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_VERY_LOW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::log;
use crate::msg::WM_DOCK_QUIT_REQUEST;

/// Number of consecutive wait timeouts (`WAIT_MS` each) without a heartbeat after which the watchdog
/// restores: 6.6 s of silence.
pub const MAX_MISSED_HEARTBEATS: u32 = 2;
/// Name of the stop event (manual-reset) shared by the dock and `--restore`.
pub const STOP_EVENT_NAME: &str = "Local\\DockStop";
/// Name of the dock's single-instance mutex.
const INSTANCE_MUTEX_NAME: &str = "Local\\DockInstance";
/// Maximum wait for `Shell_TrayWnd` before a restore by the watchdog.
pub(crate) const RESTORE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Timeout of each watchdog wait: 10 % above the longest heartbeat period it is sized for (3 s).
const WAIT_MS: u32 = 3300;
/// Period (ms) of `TIMER_SECOND`: the keepalive heartbeat of an idle dock (and the taskbar safety check).
pub const HEARTBEAT_MS: u32 = 3000;
/// While the dock handles messages it beats at the start of a message when the last beat is at least this old (ms),
/// so a stall always begins at most 1 s after a beat: 5.6 s of it are tolerated, as with the old 1 s timer.
pub(crate) const BEAT_MIN_GAP_MS: u64 = 1000;
/// Before the FIRST heartbeat the dock may still be starting (waiting for Shell_TrayWnd, ...):
/// a much wider tolerance (10 timeouts, 33 s).
const MAX_MISSED_BEFORE_FIRST: u32 = 10;
/// SYNCHRONIZE access right (waiting on kernel objects).
const SYNCHRONIZE: u32 = 0x0010_0000;

/// Raw handles (isize, 0 = absent) of the dock-side kernel objects.
static HEARTBEAT_EVENT: AtomicIsize = AtomicIsize::new(0);
static STARTUP_EVENT: AtomicIsize = AtomicIsize::new(0);
static STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
static SHUTDOWN_EVENT: AtomicIsize = AtomicIsize::new(0);
/// The watchdog process (`launch_watchdog`), kept so the dock can end it at a session end (`end_for_session_end`);
/// closing it does not end the watchdog, which still sees exit code 0 at a normal exit.
static WATCHDOG_PROCESS: AtomicIsize = AtomicIsize::new(0);
/// `GetTickCount64` of the last heartbeat signalled (0 = none yet). UI thread.
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);

/// Name of the heartbeat event for the dock with `pid`: `Local\DockHeartbeat-<pid>`.
pub fn heartbeat_event_name(pid: u32) -> String {
    format!("Local\\DockHeartbeat-{pid}")
}

/// Name of the startup heartbeat event, derived from the heartbeat one.
fn startup_event_name(heartbeat_name: &str) -> String {
    format!("{heartbeat_name}-startup")
}

/// Single instance: creates the named mutex and keeps it for the whole life of the process (the
/// kernel closes it on exit). Returns false if another dock is already running: in that case the caller
/// must exit WITHOUT touching the taskbar (the other instance handles it). If creation fails it
/// logs and carries on (true). Main thread, before `spawn`.
pub fn acquire_single_instance() -> bool {
    // SAFETY: HSTRING name valid for the call; no security attributes, not owned.
    match unsafe { CreateMutexW(None, false, &HSTRING::from(INSTANCE_MUTEX_NAME)) } {
        // The handle is NOT closed: it must stay open as long as the process lives.
        // SAFETY: GetLastError right after CreateMutexW on the same thread.
        Ok(_h) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        Err(e) => {
            // Called before the logger starts: the debugger output is the only sink.
            crate::datadir::debug_out(&format!(
                "watchdog: CreateMutexW(instance) failed: {e} (single-instance check skipped)"
            ));
            true
        }
    }
}

fn to_handle(raw: isize) -> HANDLE {
    HANDLE(raw as *mut c_void)
}

/// Closes the handle stored in `slot` (only once) and zeroes the slot.
fn close_slot(slot: &AtomicIsize) {
    let raw = slot.swap(0, Relaxed);
    if raw != 0 {
        // SAFETY: handle opened in this process (CreateEventW, CreateProcessW); the swap guarantees a single close.
        let _ = unsafe { CloseHandle(to_handle(raw)) };
    }
}

// ----------------------------------------------------------- dock side

/// Creates the named events (heartbeat and stop) and launches `current_exe --watchdog <pid> <event>`
/// with `CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW` (fallback to `CREATE_NO_WINDOW` if
/// breakaway is denied). MUST be called BEFORE touching the taskbar. If it fails the dock can
/// continue, but `main` must log it and NOT hide the taskbar (no safety net).
pub fn spawn() -> Result<(), String> {
    // SAFETY: no preconditions.
    let pid = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
    let ev_name = heartbeat_event_name(pid);

    // SAFETY: HSTRING name valid for the call; auto-reset, not signaled.
    let hb = unsafe { CreateEventW(None, false, false, &HSTRING::from(ev_name.as_str())) }
        .map_err(|e| format!("CreateEventW(heartbeat): {e}"))?;
    HEARTBEAT_EVENT.store(hb.0 as isize, Relaxed);

    // Startup heartbeat event (optional: without it, `startup_heartbeat` is a no-op).
    let st_name = startup_event_name(&ev_name);
    // SAFETY: valid HSTRING name; auto-reset, not signaled.
    match unsafe { CreateEventW(None, false, false, &HSTRING::from(st_name.as_str())) } {
        Ok(h) => STARTUP_EVENT.store(h.0 as isize, Relaxed),
        Err(e) => log!("watchdog: CreateEventW(startup) failed: {e}"),
    }

    // SAFETY: valid name; manual-reset, not signaled.
    let stop = match unsafe { CreateEventW(None, true, false, &HSTRING::from(STOP_EVENT_NAME)) } {
        Ok(h) => h,
        Err(e) => {
            close_slot(&HEARTBEAT_EVENT);
            close_slot(&STARTUP_EVENT);
            return Err(format!("CreateEventW(stop): {e}"));
        }
    };
    // SAFETY: GetLastError right after CreateEventW on the same thread.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if existed {
        log!("watchdog: stop event already exists (other instance?)");
    }
    // A possible leftover signal must not make this instance exit immediately.
    // SAFETY: handle just created.
    let _ = unsafe { ResetEvent(stop) };
    STOP_EVENT.store(stop.0 as isize, Relaxed);

    // Internal event (anonymous, manual-reset) to stop the stop-watch in `shutdown`.
    // SAFETY: no name, no security attributes.
    match unsafe { CreateEventW(None, true, false, None) } {
        Ok(h) => SHUTDOWN_EVENT.store(h.0 as isize, Relaxed),
        Err(e) => {
            close_slot(&HEARTBEAT_EVENT);
            close_slot(&STARTUP_EVENT);
            close_slot(&STOP_EVENT);
            return Err(format!("CreateEventW(shutdown): {e}"));
        }
    }

    let result = launch_watchdog(pid, &ev_name);
    if let Err(e) = result {
        close_slot(&HEARTBEAT_EVENT);
        close_slot(&STARTUP_EVENT);
        close_slot(&STOP_EVENT);
        close_slot(&SHUTDOWN_EVENT);
        close_slot(&WATCHDOG_PROCESS);
        return Err(e);
    }
    log!("watchdog: started (event {ev_name})");
    Ok(())
}

/// `CreateProcessW` of `current_exe --watchdog <pid> <event>`, with a fallback without breakaway.
fn launch_watchdog(pid: u32, ev_name: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // The process handle is kept for `end_for_session_end`; `shutdown` closes it.
    let process = spawn_breakaway(&exe, &format!("--watchdog {pid} {ev_name}"))?;
    WATCHDOG_PROCESS.store(process.into_raw_handle() as isize, Relaxed);
    Ok(())
}

/// `CreateProcessW` of `"<exe>" <args>` with `CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW`, falling
/// back to `CREATE_NO_WINDOW` if breakaway is denied. Breakaway lets the child outlive the dock when
/// the dock runs inside the Task Scheduler job (the watchdog, the updater's installer). Returns the
/// process handle; the thread handle is closed at once.
pub(crate) fn spawn_breakaway(exe: &std::path::Path, args: &str) -> Result<OwnedHandle, String> {
    let exe_str = exe.to_string_lossy().into_owned();
    let cmdline = format!("\"{exe_str}\" {args}");
    let app = HSTRING::from(exe_str.as_str());

    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut breakaway_denied = false;
    for (flags, label) in
        [(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW, "breakaway"), (CREATE_NO_WINDOW, "without breakaway")]
    {
        // CreateProcessW may modify the command line: mutable buffer for each attempt.
        let mut buf: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: `app` and `buf` (NUL-terminated, mutable) live for the whole call; `si` and `pi`
        // are valid structures; no inherited handles, environment and current folder of the parent.
        let r = unsafe {
            CreateProcessW(
                &app,
                Some(PWSTR(buf.as_mut_ptr())),
                None,
                None,
                false,
                flags as PROCESS_CREATION_FLAGS,
                None,
                None,
                &si,
                &mut pi,
            )
        };
        match r {
            Ok(()) => {
                // SAFETY: both handles were just returned by CreateProcessW and are owned here; the
                // thread handle is closed once, the process handle is moved into the OwnedHandle.
                let process = unsafe {
                    let _ = CloseHandle(pi.hThread);
                    OwnedHandle::from_raw_handle(pi.hProcess.0)
                };
                if breakaway_denied {
                    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    log!("spawn: {name} started inside this process's job (breakaway not allowed)");
                }
                return Ok(process);
            }
            Err(e) => {
                match attempt_note(label, &e, &exe_str) {
                    Some(note) => log!("{note}"),
                    None => breakaway_denied = true,
                }
                if flags == CREATE_NO_WINDOW {
                    return Err(format!("CreateProcessW: {e}"));
                }
            }
        }
    }
    Err("CreateProcessW: no attempt succeeded".into())
}

/// The log line of a failed `CreateProcessW` attempt. A denied breakaway is the normal case inside the Task
/// Scheduler job (the plain attempt follows), so it is not an error line: `None`.
fn attempt_note(label: &str, e: &windows::core::Error, exe: &str) -> Option<String> {
    if label == "breakaway" && e.code() == E_ACCESSDENIED {
        return None;
    }
    Some(format!("spawn: CreateProcessW of {exe} ({label}) failed: {e}"))
}

/// Signals the heartbeat (`SetEvent`) and remembers when. Does not block; no-op if `spawn` did not succeed.
/// UI thread: `beat_if_due` and `TIMER_SECOND`.
pub fn heartbeat() {
    let raw = HEARTBEAT_EVENT.load(Relaxed);
    if raw == 0 {
        return;
    }
    // SAFETY: no preconditions.
    LAST_BEAT_MS.store(unsafe { GetTickCount64() }, Relaxed);
    // SAFETY: handle valid until `shutdown` closes it (same UI thread).
    let _ = unsafe { SetEvent(to_handle(raw)) };
    probe!(crate::probe::bump(crate::probe::C::Heartbeat));
}

/// true when a beat is due: the last one is at least `BEAT_MIN_GAP_MS` old (a clock read before it is not).
fn beat_due(now_ms: u64, last_ms: u64) -> bool {
    now_ms.saturating_sub(last_ms) >= BEAT_MIN_GAP_MS
}

/// The heartbeat at the start of a message (UI thread: `wnd_proc`, the message loop): beats when the last beat is at
/// least 1 s old. true when it called `heartbeat` (which stays a no-op when `spawn` did not succeed).
pub fn beat_if_due() -> bool {
    // SAFETY: no preconditions.
    let now = unsafe { GetTickCount64() };
    if beat_due(now, LAST_BEAT_MS.load(Relaxed)) {
        heartbeat();
        true
    } else {
        false
    }
}

/// Age (ms) of the last heartbeat, for the wake probe's stall line.
#[cfg(feature = "wake-probe")]
pub fn beat_age_ms() -> u64 {
    // SAFETY: no preconditions.
    unsafe { GetTickCount64() }.saturating_sub(LAST_BEAT_MS.load(Relaxed))
}

/// Startup heartbeat (`taskbar::init`, waiting for `Shell_TrayWnd`): resets the watchdog count
/// without enabling the steady-state tolerance. Does not block; no-op if the event does not exist.
pub fn startup_heartbeat() {
    let raw = STARTUP_EVENT.load(Relaxed);
    if raw == 0 {
        return;
    }
    // SAFETY: handle valid until `shutdown` closes it (same thread).
    let _ = unsafe { SetEvent(to_handle(raw)) };
}

/// Starts the `stop-watch` thread: waits on `STOP_EVENT_NAME` and posts `WM_DOCK_QUIT_REQUEST`
/// to `ui_hwnd`. Returns the handle (the thread exits when `shutdown` signals the internal stop).
pub fn start_stop_watcher(ui_hwnd: HWND) -> Option<JoinHandle<()>> {
    let stop = STOP_EVENT.load(Relaxed);
    let quit = SHUTDOWN_EVENT.load(Relaxed);
    if stop == 0 || quit == 0 {
        log!("watchdog: stop-watch not started (events missing)");
        return None;
    }
    // HANDLE/HWND are not Send: they are passed as integers.
    let ui = ui_hwnd.0 as isize;
    match std::thread::Builder::new().name("stop-watch".into()).stack_size(64 * 1024).spawn(move || {
        let handles = [to_handle(stop), to_handle(quit)];
        // SAFETY: the handles stay open until `shutdown` has joined this thread.
        let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        probe!(crate::probe::bump(crate::probe::C::StopWatch));
        if r == WAIT_OBJECT_0 {
            // SAFETY: PostMessageW is thread-safe; if the window is already gone it merely fails.
            let posted =
                unsafe { PostMessageW(Some(HWND(ui as *mut c_void)), WM_DOCK_QUIT_REQUEST, WPARAM(0), LPARAM(0)) };
            if let Err(e) = posted {
                log!("watchdog: PostMessageW(QUIT_REQUEST) failed: {e}");
            }
        } else if r == WAIT_FAILED {
            log!("watchdog: stop-watch wait failed: {:?}", windows::core::Error::from_thread());
        }
    }) {
        Ok(h) => Some(h),
        Err(e) => {
            log!("watchdog: stop-watch thread not started: {e}");
            None
        }
    }
}

/// Normal dock exit: stops the `stop-watch`, closes the handles. The watchdog sees the
/// process end with code 0 and leaves. Call AFTER `taskbar::shutdown()`.
pub fn shutdown(stop_watcher: Option<JoinHandle<()>>) {
    let quit = SHUTDOWN_EVENT.load(Relaxed);
    if quit != 0 {
        // SAFETY: valid handle (closed only further below).
        if let Err(e) = unsafe { SetEvent(to_handle(quit)) } {
            log!("watchdog: SetEvent(shutdown) failed: {e}");
        }
    }
    if let Some(h) = stop_watcher {
        if h.join().is_err() {
            log!("watchdog: stop-watch thread terminated with a panic");
        }
    }
    close_slot(&SHUTDOWN_EVENT);
    close_slot(&STOP_EVENT);
    close_slot(&HEARTBEAT_EVENT);
    close_slot(&STARTUP_EVENT);
    close_slot(&WATCHDOG_PROCESS);
}

/// true when a stop was requested: `--restore`, `--install` and `--uninstall` signal `Local\DockStop` (manual-reset,
/// so it stays set) before they restore. Also true when it cannot be told (no stop event): the dock then stops hiding
/// after an outside restore, as before. UI thread (`shutdown` closes the event on it).
pub fn stop_requested() -> bool {
    let raw = STOP_EVENT.load(Relaxed);
    raw == 0 || is_set(to_handle(raw))
}

/// true only while the watchdog process this dock started runs: false without one (not started, ended for the
/// session end, closed by `shutdown`) and once it has exited (its wait-failed branch restores and exits). UI thread
/// (the slot is emptied and closed on it).
pub(crate) fn alive() -> bool {
    let raw = WATCHDOG_PROCESS.load(Relaxed);
    raw != 0 && unsignalled(to_handle(raw))
}

/// true when `h` (an event, a process) is signalled now; a failed wait reads as false. Never blocks.
fn is_set(h: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait; an invalid handle only makes it fail.
    (unsafe { WaitForSingleObject(h, 0) }) == WAIT_OBJECT_0
}

/// true when `h` is not signalled now (a process that still runs); a failed wait reads as false. Never blocks.
fn unsignalled(h: HANDLE) -> bool {
    // SAFETY: a zero-timeout wait; an invalid handle only makes it fail.
    (unsafe { WaitForSingleObject(h, 0) }) == WAIT_TIMEOUT
}

/// Session end (`taskbar::restore_at_session_end`, UI thread): ends the watchdog process before the dock's own
/// restore, so it cannot restore again after the dock exits and delete the state file kept for the next sign-in.
/// No-op without a watchdog (never started, or already ended).
pub(crate) fn end_for_session_end() {
    let raw = WATCHDOG_PROCESS.swap(0, Relaxed);
    if raw == 0 {
        return;
    }
    let h = to_handle(raw);
    // SAFETY: the process handle CreateProcessW returned (full access), owned by the slot until the swap above.
    match unsafe { TerminateProcess(h, 0) } {
        Ok(()) => log!("watchdog: ended for the session end (the dock restores and keeps the state file)"),
        Err(e) => log!("watchdog: ending it for the session end failed: {e}"),
    }
    // SAFETY: the same handle, closed once (the slot was emptied by the swap).
    let _ = unsafe { CloseHandle(h) };
}

// ------------------------------------------------------ `--restore` side

/// true while a dock instance holds the single-instance mutex (it exists as long as the dock
/// process lives: the kernel closes it on exit). Works for older dock versions too, unlike the
/// state file, which moved.
pub fn instance_running() -> bool {
    // SAFETY: valid HSTRING name; only SYNCHRONIZE is requested and the handle is closed below.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(INSTANCE_MUTEX_NAME)) } {
        Ok(h) => {
            // SAFETY: handle just opened, closed once.
            let _ = unsafe { CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

/// Opens `STOP_EVENT_NAME` and signals it. Returns true if an instance was running (event
/// existing), false otherwise.
pub fn signal_stop_to_instance() -> bool {
    // SAFETY: valid HSTRING name; if the event does not exist it fails (no instance).
    let ev = match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(STOP_EVENT_NAME)) } {
        Ok(h) => h,
        Err(_) => return false,
    };
    // SAFETY: handle just opened with EVENT_MODIFY_STATE.
    let ok = match unsafe { SetEvent(ev) } {
        Ok(()) => true,
        Err(e) => {
            log!("watchdog: SetEvent(stop) failed: {e}");
            false
        }
    };
    // SAFETY: handle opened above, closed only once.
    let _ = unsafe { CloseHandle(ev) };
    ok
}

// ---------------------------------------------------- watchdog process side

/// Main of the `--watchdog <pid> <event>` mode: returns the process exit code.
/// See PROTOCOL in the module doc.
pub fn run_watchdog(dock_pid: u32, event_name: &str) -> i32 {
    // SAFETY: no preconditions; the PID is the one passed by the dock.
    let proc = match unsafe {
        OpenProcess(
            windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION.0),
            false,
            dock_pid,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            // Dock already gone (or inaccessible): better to restore, it is idempotent and protected.
            log!("watchdog: OpenProcess({dock_pid}) failed: {e}, restoring");
            crate::taskbar::restore_force_waiting(RESTORE_WAIT);
            return 1;
        }
    };
    // SAFETY: valid HSTRING name; only SYNCHRONIZE is needed to wait on the event.
    let ev = match unsafe {
        OpenEventW(windows::Win32::System::Threading::SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(event_name))
    } {
        Ok(h) => Some(h),
        Err(e) => {
            log!("watchdog: OpenEventW({event_name}) failed: {e} (no heartbeat check)");
            None
        }
    };
    // Startup heartbeat event (optional: if missing, only the real heartbeat is counted).
    let startup = ev.and_then(|_| {
        let name = startup_event_name(event_name);
        // SAFETY: valid HSTRING name; only SYNCHRONIZE is needed to wait on the event.
        unsafe {
            OpenEventW(
                windows::Win32::System::Threading::SYNCHRONIZATION_SYNCHRONIZE,
                false,
                &HSTRING::from(name.as_str()),
            )
        }
        .ok()
    });
    log!("watchdog: watching dock pid={dock_pid}");

    lower_footprint();
    let restore = || {
        crate::taskbar::restore_force_waiting(RESTORE_WAIT);
        trim_working_set();
    };
    // Indices in the wait array: 0 = process, 1 = heartbeat (if present), 2 = startup (if present).
    let mut handles = vec![proc];
    handles.extend(ev);
    handles.extend(startup);
    let mut missed = 0u32;
    let mut seen_beat = false;
    // true after a restore due to missed heartbeats, until a heartbeat comes back (only one
    // restore per hang).
    let mut stalled = false;
    let code = loop {
        // Without the event only the process is awaited (no timeout: there are no heartbeats to count).
        let timeout = if ev.is_some() { WAIT_MS } else { INFINITE };
        // SAFETY: handles valid for the whole duration of the loop (closed only at the end).
        let r = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
        if r == WAIT_OBJECT_0 {
            let mut exit = 0u32;
            // SAFETY: `proc` opened with PROCESS_QUERY_LIMITED_INFORMATION; `exit` is writable.
            let exit_code = match unsafe { GetExitCodeProcess(proc, &mut exit) } {
                Ok(()) => Some(exit),
                Err(e) => {
                    log!("watchdog: GetExitCodeProcess failed: {e}, restoring");
                    None
                }
            };
            let state_kept = crate::taskbar::state_file_exists();
            match exit_code {
                Some(0) => {
                    log!("watchdog: the dock exited normally");
                    // Exit with Explorer absent: restore_force kept the state file, so the
                    // taskbar is still in autohide. Wait for Explorer and restore.
                    if state_kept {
                        log!("watchdog: state file still present after exit, restoring the taskbar");
                    }
                }
                Some(n) => log!("watchdog: the dock ended with code 0x{n:X}, restoring the taskbar"),
                None => {}
            }
            if exit_needs_restore(exit_code, state_kept) {
                restore();
            }
            break 0;
        } else if r.0 == WAIT_OBJECT_0.0 + 1 && handles.len() >= 2 {
            if stalled {
                log!("watchdog: heartbeat resumed after the hang");
            }
            missed = 0;
            seen_beat = true;
            stalled = false;
        } else if r.0 == WAIT_OBJECT_0.0 + 2 && handles.len() == 3 {
            // Startup heartbeat: the dock is alive but not yet at steady state.
            missed = 0;
        } else if r == WAIT_TIMEOUT {
            missed += 1;
            if restore_due(missed, seen_beat) && !stalled {
                log!("watchdog: {missed} missed heartbeats, restoring the taskbar (still waiting for the dock)");
                // Before restoring, the dock tray host must yield to Explorer. If it does not
                // respond (hung host) the dock must be terminated BEFORE the restore: a hung host
                // stays the first `Shell_TrayWnd` and apps, on the `TaskbarCreated` broadcast, would
                // register their icons with it (timing out) instead of with Explorer.
                if crate::tray_host::yield_foreign_hosts() {
                    terminate_dock(dock_pid);
                    // SAFETY: `proc` opened with SYNCHRONIZE, valid until closed at the end.
                    let w = unsafe { WaitForSingleObject(proc, 2000) };
                    if w != WAIT_OBJECT_0 {
                        log!("watchdog: dock not yet terminated after 2 s ({w:?}), restoring anyway");
                    }
                }
                restore();
                stalled = true;
            }
        } else {
            log!("watchdog: wait failed ({:?}), restoring", windows::core::Error::from_thread());
            restore();
            break 1;
        }
    };
    // SAFETY: handles opened above, closed only once.
    unsafe {
        let _ = CloseHandle(proc);
        for h in ev.into_iter().chain(startup) {
            let _ = CloseHandle(h);
        }
    }
    code
}

/// Marks this process' pages as the first to be repurposed by the memory manager and trims what is
/// resident. Memory priority does not affect CPU scheduling, so restore latency is unchanged.
fn lower_footprint() {
    let info = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_VERY_LOW };
    // SAFETY: pseudo-handle of the current process; `info` is a valid struct of the stated size.
    let r = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessMemoryPriority,
            &info as *const MEMORY_PRIORITY_INFORMATION as *const c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    };
    if let Err(e) = r {
        log!("watchdog: SetProcessInformation(memory priority) failed: {e}");
    }
    trim_working_set();
}

/// Empties the working set of the watchdog process (it sleeps almost all the time).
fn trim_working_set() {
    // SAFETY: pseudo-handle of the current process, always valid.
    if let Err(e) = unsafe { EmptyWorkingSet(GetCurrentProcess()) } {
        log!("watchdog: EmptyWorkingSet failed: {e}");
    }
}

/// Terminates the dock process (`TerminateProcess(.., 1)`): used when its tray host does not respond
/// and cannot yield to Explorer. Watchdog process only, before the taskbar restore.
fn terminate_dock(pid: u32) {
    // SAFETY: no preconditions; if the process is already gone or inaccessible it returns Err.
    match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
        Ok(h) => {
            // SAFETY: handle just opened with PROCESS_TERMINATE.
            match unsafe { TerminateProcess(h, 1) } {
                Ok(()) => log!("watchdog: dock pid={pid} terminated (hung tray host, before the restore)"),
                Err(e) => log!("watchdog: TerminateProcess({pid}) failed: {e}"),
            }
            // SAFETY: handle opened above, closed only once.
            let _ = unsafe { CloseHandle(h) };
        }
        Err(e) => log!("watchdog: OpenProcess(TERMINATE, {pid}) failed: {e}"),
    }
}

/// true when `missed` consecutive wait timeouts without a heartbeat call for a hang restore: the
/// steady-state limit once a real heartbeat was seen, the startup limit before it.
fn restore_due(missed: u32, seen_beat: bool) -> bool {
    missed >= if seen_beat { MAX_MISSED_HEARTBEATS } else { MAX_MISSED_BEFORE_FIRST }
}

/// Post-exit decision of `run_watchdog`: restore unless the dock exited cleanly AND left no state file.
/// `exit_code` is `None` when `GetExitCodeProcess` failed. A clean exit with Explorer absent keeps the file.
fn exit_needs_restore(exit_code: Option<u32>, state_file_exists: bool) -> bool {
    match exit_code {
        Some(0) => state_file_exists,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Silence before a hang restore: one wait timeout times the number of timeouts in a row.
    fn silence_ms(seen_beat: bool) -> u32 {
        WAIT_MS * if seen_beat { MAX_MISSED_HEARTBEATS } else { MAX_MISSED_BEFORE_FIRST }
    }

    /// A heartbeat period fits one wait when the wait is at least 10 % longer than the period.
    fn beat_fits(period_ms: u32, wait_ms: u32) -> bool {
        u64::from(period_ms) * 11 <= u64::from(wait_ms) * 10
    }

    #[test]
    fn hang_silence_is_unchanged() {
        assert!(!restore_due(1, true));
        assert!(restore_due(2, true), "2 timeouts in a row after a beat are a hang");
        assert!(!restore_due(9, false));
        assert!(restore_due(10, false), "10 timeouts in a row before the first beat are a hang");
        assert_eq!(silence_ms(true), 6600, "6.6 s without a beat, as with 6 waits of 1.1 s");
        assert_eq!(silence_ms(false), 33_000, "33 s before the first beat, as with 30 waits of 1.1 s");
    }

    #[test]
    fn a_3_s_beat_lands_inside_one_wait() {
        assert!(beat_fits(3000, WAIT_MS), "the idle keepalive of 3 s");
        assert!(beat_fits(1000, WAIT_MS), "the 1 s beat of today");
        assert!(!beat_fits(3100, WAIT_MS));
    }

    /// The stall a hang restore tolerates when it begins `beat_age_ms` after the last beat.
    fn stall_tolerance_ms(beat_age_ms: u32) -> u32 {
        silence_ms(true) - beat_age_ms
    }

    #[test]
    fn a_beat_at_most_once_a_second() {
        assert!(beat_due(1000, 0));
        assert!(!beat_due(999, 0));
        assert!(!beat_due(5, 10), "a clock read before the last beat");
        assert!(beat_due(u64::MAX, 0));
    }

    #[test]
    fn the_keepalive_fits_the_wait() {
        assert!(beat_fits(HEARTBEAT_MS, WAIT_MS), "an idle dock's 3 s keepalive lands inside one 3.3 s wait");
    }

    #[test]
    fn the_stall_tolerance_is_unchanged() {
        let gap = u32::try_from(BEAT_MIN_GAP_MS).unwrap();
        assert_eq!(stall_tolerance_ms(gap), 5600, "a stall begins at most 1 s after a beat: 5.6 s tolerated, as today");
    }

    #[test]
    fn breakaway_denial_is_not_an_error() {
        use windows::Win32::Foundation::E_FAIL;
        let denied = windows::core::Error::from(E_ACCESSDENIED);
        let failed = windows::core::Error::from(E_FAIL);
        assert_eq!(attempt_note("breakaway", &denied, "x.exe"), None);
        assert_eq!(
            attempt_note("breakaway", &failed, "x.exe"),
            Some(format!("spawn: CreateProcessW of x.exe (breakaway) failed: {failed}"))
        );
        assert_eq!(
            attempt_note("without breakaway", &denied, "x.exe"),
            Some(format!("spawn: CreateProcessW of x.exe (without breakaway) failed: {denied}"))
        );
    }

    /// r151 N1: the stop event (manual-reset) reads as set once signalled and stays set; a handle not signalled (a
    /// running watchdog process) reads as unsignalled, and a failed wait never does.
    #[test]
    fn a_set_manual_reset_event_reads_as_set() {
        // SAFETY: an unnamed manual-reset event of this test, closed below.
        let h = unsafe { CreateEventW(None, true, false, None) }.expect("CreateEventW");
        assert!(!is_set(h));
        assert!(unsignalled(h));
        // SAFETY: the event created above.
        unsafe { SetEvent(h) }.expect("SetEvent");
        assert!(is_set(h));
        assert!(is_set(h), "a manual-reset event stays set");
        assert!(!unsignalled(h));
        assert!(!unsignalled(HANDLE::default()), "a failed wait never reads as a live watchdog");
        assert!(!is_set(HANDLE::default()));
        // SAFETY: the event created above, closed once.
        let _ = unsafe { CloseHandle(h) };
    }

    #[test]
    fn watchdog_restores_whenever_the_state_file_is_left() {
        assert!(exit_needs_restore(Some(0), true), "clean exit with Explorer absent: state file kept");
        assert!(!exit_needs_restore(Some(0), false));
        assert!(exit_needs_restore(Some(1), false));
        assert!(exit_needs_restore(Some(0xC000_0005), true));
        assert!(exit_needs_restore(None, false), "exit code unreadable: restore anyway");
    }
}
