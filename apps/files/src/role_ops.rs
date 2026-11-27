//! files.exe --role=ops: the ops host (OP5c), the only process that changes user files, and only through
//! IFileOperation. asInvoker and never elevated (a UAC-elevated token exits 5), one per session and build: it serves
//! the named pipe mini-oled-dock.files.ops.<session>-<proto>-<build8> (a taken name exits 4). The main thread owns a
//! hidden window (class MiniOledFilesOps, never shown) for session end and dialog ownership and waits on its
//! messages, the idle timer and (test mode) the test parent; the pipe is served on one thread, each operation runs
//! on its own. An idle host leaves 30 s after its queue empties with no UI attached (5 s in test mode).
//!
//! Debug builds read the test hooks from their environment: FILES_TEST_PIPE_TAG, FILES_TEST_BUILD_TAG,
//! FILES_TEST_ROOT, FILES_TEST_NO_UI, FILES_TEST_PARENT, FILES_TEST_DIALOGS_UNDER_SILENT, FILES_TEST_SLOW_POST_MS and
//! FILES_DATA_DIR. Release builds ignore them all.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use oled_trust::frame::files::OPS;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TokenElevationType, TOKEN_ELEVATION_TYPE, TOKEN_QUERY};
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CancelWaitableTimer, CreateWaitableTimerW, GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    SetWaitableTimer, INFINITE, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, RegisterClassExW,
    TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WINDOW_EX_STYLE, WM_ENDSESSION,
    WM_QUERYENDSESSION, WM_QUIT, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::exit;
use crate::host::named::{ops_pipe_name, own_peer, NamedError, NamedServer};
use crate::ops::checks::test_roots_from;
use crate::ops::consts::DIALOGS_UNDER_SILENT;
use crate::ops::flags::{dialogs_under_silent_from, test_no_ui_from};
use crate::ops::host::{Host, HostConfig, JournalCtx, RealRunner, RunBase};
use crate::ops::session::{end_session, query_end_session, Lifetime, IDLE_EXIT, IDLE_EXIT_TEST};
use crate::own_files::OwnDir;

/// The host's window class.
pub const WINDOW_CLASS: &str = "MiniOledFilesOps";

/// The host, for the window procedure.
static HOST: OnceLock<Arc<Host>> = OnceLock::new();

/// A test hook of this process's environment: debug builds only.
fn test_var(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    } else {
        None
    }
}

/// TOKEN_ELEVATION_TYPE values (winnt.h).
const ELEVATION_FULL: i32 = 2;

/// A UAC-elevated token (TokenElevationTypeFull) is refused: an elevated UI must start its helpers de-elevated; a
/// runner without UAC (TokenElevationTypeDefault) runs. Pure.
pub fn refuse_elevation(elevation_type: i32) -> bool {
    elevation_type == ELEVATION_FULL
}

fn elevation_type() -> i32 {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process; `token` is writable and closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return 0;
    }
    let mut t = TOKEN_ELEVATION_TYPE::default();
    let mut len = 0u32;
    let size = std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32;
    // SAFETY: `t` is a writable TOKEN_ELEVATION_TYPE of `size` bytes.
    let ok = unsafe {
        GetTokenInformation(token, TokenElevationType, Some(std::ptr::from_mut(&mut t).cast()), size, &mut len)
    };
    // SAFETY: the token opened above, closed once.
    let _ = unsafe { CloseHandle(token) };
    if ok.is_ok() {
        t.0
    } else {
        0
    }
}

/// The protocol a command line asks for: --proto=<n>. Pure.
pub fn proto_arg(args: &[String]) -> Option<u16> {
    args.iter().find_map(|a| a.strip_prefix("--proto=")).and_then(|v| v.parse().ok())
}

/// `args` are the arguments after --role=ops. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match proto_arg(args) {
        Some(p) if p == OPS.version => {}
        Some(_) => return exit::PROTO_MISMATCH,
        None => return exit::USAGE,
    }
    if refuse_elevation(elevation_type()) {
        return exit::ELEVATED;
    }
    // SAFETY: plain process-wide setting: no critical-error or fault dialog boxes.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
    let log = oled_sys::logger::start("ops", &crate::LOG_OPS);
    let code = host_main();
    oled_sys::log!("files ops: exit {code}");
    oled_sys::logger::stop(log);
    code
}

fn own_created() -> u64 {
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: the pseudo-handle of this process; four writable FILETIMEs.
    let _ = unsafe { GetProcessTimes(GetCurrentProcess(), c, e, k, u) };
    (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime)
}

/// The tag parts of the pipe name the hooks may set: a test tag of 1-9 [a-z0-9], a build tag of 8 hex digits.
fn valid_tag(s: &str, len: std::ops::RangeInclusive<usize>, hex: bool) -> bool {
    len.contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || if hex { (b'a'..=b'f').contains(&b) } else { b.is_ascii_lowercase() })
}

/// The ops pipe name for this session and build, with the test hooks of `pipe_tag` and `build_tag`.
pub fn pipe_name(pipe_tag: Option<&str>, build_tag: Option<&str>) -> Result<String, NamedError> {
    let session = own_peer().map(|p| p.session).ok_or(NamedError::Name)?;
    let build = match build_tag {
        Some(b) if valid_tag(b, 8..=8, true) => b.to_string(),
        Some(_) => return Err(NamedError::Name),
        None => crate::build_tag(),
    };
    if pipe_tag.is_some_and(|t| !valid_tag(t, 1..=9, false)) {
        return Err(NamedError::Name);
    }
    ops_pipe_name(session, OPS.version, &build, pipe_tag)
}

fn host_main() -> i32 {
    let roots = test_roots_from(cfg!(debug_assertions), test_var("FILES_TEST_ROOT").as_deref());
    let test_mode = roots.is_some();
    let Some(data) = OwnDir::default_dir().and_then(|d| OwnDir::open(d.path()).ok()) else {
        oled_sys::log!("files ops: no data folder");
        return exit::USAGE;
    };
    let Ok(image) = std::env::current_exe() else { return exit::USAGE };
    let name = match pipe_name(test_var("FILES_TEST_PIPE_TAG").as_deref(), test_var("FILES_TEST_BUILD_TAG").as_deref())
    {
        Ok(n) => n,
        Err(_) => return exit::USAGE,
    };
    let server = match NamedServer::create(&name, &image, test_mode) {
        Ok(s) => s,
        Err(NamedError::Squatted) => {
            oled_sys::log!("files ops: the pipe name is taken");
            return exit::PIPE_TAKEN;
        }
        Err(e) => {
            oled_sys::log!("files ops: cannot create the pipe: {e}");
            return exit::PIPE_TAKEN;
        }
    };
    let Some(hwnd) = hidden_window() else { return exit::USAGE };
    let pid = std::process::id();
    let created = own_created();
    let attached = Arc::new(AtomicBool::new(false));
    let base = RunBase {
        roots: roots.clone(),
        test_no_ui: test_no_ui_from(cfg!(debug_assertions), test_var("FILES_TEST_NO_UI").as_deref()),
        dialogs_under_silent: dialogs_under_silent_from(
            cfg!(debug_assertions),
            DIALOGS_UNDER_SILENT,
            test_var("FILES_TEST_DIALOGS_UNDER_SILENT").as_deref(),
        ),
        host_window: Some(hwnd.0 as isize),
        journal: Some(JournalCtx { dir: data.clone(), pid, created }),
        attached: attached.clone(),
        slow_post_ms: test_var("FILES_TEST_SLOW_POST_MS").and_then(|v| v.parse().ok()).unwrap_or(0).min(10_000),
    };
    let host = Host::new(HostConfig::new(data, pid, created, roots), Box::new(RealRunner::new(base)), attached);
    let _ = HOST.set(host.clone());
    // SAFETY: an unnamed auto-reset waitable timer; the handle lives for the process.
    let Ok(timer) = (unsafe { CreateWaitableTimerW(None, false, PCWSTR::null()) }) else { return exit::USAGE };
    let idle = if test_mode { IDLE_EXIT_TEST } else { IDLE_EXIT };
    let life = Arc::new(Mutex::new(Lifetime::new(idle, Instant::now())));
    let timer_v = timer.0 as isize;
    let arm = {
        let life = life.clone();
        move |attached: bool, busy: bool| {
            let mut l = life.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            l.set(attached, busy, now);
            set_timer(HANDLE(timer_v as *mut std::ffi::c_void), l.deadline().map(|d| d.saturating_duration_since(now)));
        }
    };
    arm(false, false);
    host.set_on_change(Box::new(arm));
    let h = host.clone();
    let _ = std::thread::Builder::new().name("ops-pipe".into()).spawn(move || loop {
        match server.accept() {
            Ok(conn) => {
                let end = h.serve(conn);
                oled_sys::log!("files ops: connection ended: {end:?}");
                server.disconnect();
            }
            Err(e) => {
                oled_sys::log!("files ops: accept failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    });
    let parent = test_var("FILES_TEST_PARENT").and_then(|p| p.parse::<u32>().ok()).and_then(|pid| {
        // SAFETY: a plain open by id for waiting on; the handle lives for the process.
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok()
    });
    main_loop(timer, parent, &life, &host);
    crate::ops::journal::flush_all();
    exit::OK
}

/// Sets the idle timer to fire after `after`, or cancels it.
fn set_timer(timer: HANDLE, after: Option<std::time::Duration>) {
    match after {
        Some(d) => {
            // Relative due time in 100 ns units (negative); at least one unit.
            let due: i64 = -i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX).max(1);
            // SAFETY: a live waitable timer; `due` lives for the call; no completion routine.
            let _ = unsafe { SetWaitableTimer(timer, &due, 0, None, None, false) };
        }
        None => {
            // SAFETY: a live waitable timer.
            let _ = unsafe { CancelWaitableTimer(timer) };
        }
    }
}

/// Waits on the window's messages, the idle timer and the test parent; returns when the host should leave.
fn main_loop(timer: HANDLE, parent: Option<HANDLE>, life: &Mutex<Lifetime>, host: &Arc<Host>) {
    let mut handles = vec![timer];
    handles.extend(parent);
    loop {
        // SAFETY: `handles` are live wait handles of this process.
        let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        let n = handles.len();
        if i < n {
            let mut l = life.lock().unwrap_or_else(|e| e.into_inner());
            if i == 1 {
                // The test parent has exited: no wait on it any more.
                l.parent_gone();
                l.set(host.is_attached(), host.busy(), Instant::now());
                handles.truncate(1);
            }
            if l.should_exit(Instant::now()) && !host.busy() {
                return;
            }
            continue;
        }
        if i > n {
            // WAIT_FAILED cannot come from live handles; never spin on it.
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }
        let mut msg = MSG::default();
        // SAFETY: `msg` is writable; messages of this thread only.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return;
            }
            // SAFETY: a message this thread just received.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            let running = HOST.get().map(|h| h.running()).unwrap_or_default();
            LRESULT(isize::from(query_end_session(hwnd, &running, crate::strings::lang())))
        }
        WM_ENDSESSION => {
            end_session(wparam.0 != 0);
            LRESULT(0)
        }
        // SAFETY: the default handling of a message of our own window.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The host's hidden top-level window (never shown): it receives the session-end messages and owns the dialogs of
/// its operations.
fn hidden_window() -> Option<HWND> {
    // SAFETY: the module handle of this executable.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = HSTRING::from(WINDOW_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // SAFETY: `wc` and the class name it points to live for the call.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return None;
    }
    // SAFETY: the class registered above; a popup tool window that is never shown, no parent, no menu.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
            &class,
            w!("files ops"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_decision_table() {
        // TokenElevationTypeDefault (1): no UAC, a hosted runner; Full (2): elevated; Limited (3): a standard user.
        for (t, refused) in [(1, false), (2, true), (3, false), (0, false)] {
            assert_eq!(refuse_elevation(t), refused, "{t}");
        }
    }

    #[test]
    fn proto_and_tags() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(proto_arg(&a(&["--proto=1"])), Some(1));
        assert_eq!(proto_arg(&a(&["--proto=2"])), Some(2));
        assert_eq!(proto_arg(&a(&["--proto=x"])), None);
        assert_eq!(proto_arg(&a(&[])), None);
        assert!(valid_tag("abc123xyz", 1..=9, false));
        assert!(!valid_tag("abc123xyz0", 1..=9, false));
        assert!(!valid_tag("ABC", 1..=9, false));
        assert!(valid_tag("aaaaaaaa", 8..=8, true));
        assert!(!valid_tag("aaaaaaag", 8..=8, true));
        let n = pipe_name(Some("t1"), Some("aaaaaaaa")).unwrap();
        assert!(n.ends_with("-1-aaaaaaaa-t1"), "{n}");
        assert!(pipe_name(Some("T1"), None).is_err());
        assert!(pipe_name(None, None).unwrap().ends_with(&format!("-1-{}", crate::build_tag())));
    }
}
