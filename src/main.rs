//! Dock for Windows 11 (Rust + windows-rs).
//!
//! Startup, CLI, threads, message loop, cleanup, panic hook.
//!
//! THREADS
//!  1. UI (main): layered window, message loop, timers, render, apps, taskbar (WinEvent),
//!     ui. Has COM STA initialized (needed by `apps::window_identity`).
//!  2. `hook`: WH_KEYBOARD_LL, just a message loop (keep it minimal: it must never block).
//!  3. `log`: writes to (stdout only with `--console` and) `%ProgramData%\mini-oled-dock\dock.log`
//!     (`logger`; data folder security in `datadir`).
//!  4. `shell`: COM STA for names/icons/audio/launch (`shell_thread`).
//!  5. `stop-watch`: waits for the stop event of `--restore` (`watchdog`).
//!  6. Thread created by Windows for Ctrl+C: only `PostMessageW(WM_CLOSE)`.
//!  7. `tray-host` + `tray-fwd`: hidden Shell_TrayWnd window and forwarding to Explorer
//!     (`tray_host`), only if the native taskbar is hidden and `config.show_tray`.
//!  8. `notif`: COM MTA, WinRT notification list + "Do not disturb" state (`notif`).
//!  9. `power`: Sleep/Restart/Shut down (`power`).
//!
//!  Popups (`popup`) live on the UI thread.
//!  Plus the watchdog PROCESS (`dock.exe --watchdog <pid> <event>`).
//!
//! STARTUP ORDER (`run`): config -> watchdog::spawn (before touching the taskbar) -> taskbar::init
//! -> ui::init (geometry) -> window -> ui::attach (+ apps WinEvent hook) -> popup::init -> shell thread -> notif ->
//! tray host (+ taskbar::announce_tray) -> taskbar WinEvent hook -> stop-watch ->
//! Ctrl+C/hotkey/timers -> keyboard hook -> message loop. Cleanup runs in reverse order, with the tray host
//! stopped and the taskbar restored first (host destroyed BEFORE the TaskbarCreated broadcast).

//! SUBSYSTEM: `windows` (no console). The CLI modes that print (--help, --restore,
//! --install, --uninstall, --preview, errors) attach to the parent process console with
//! `attach_console`; `--console` does the same for normal runs too (log on stdout and Ctrl+C).
#![windows_subsystem = "windows"]

mod apps;
mod arp;
mod audio;
mod autostart;
mod battery;
mod calendar;
mod clock;
mod config;
mod datadir;
mod glyphs;
mod hook;
mod i18n;
mod icons;
mod logger;
mod mem;
mod metrics;
mod msg;
mod net;
mod notif;
mod picker;
mod popup;
mod popup_audio;
mod popup_battery;
mod popup_calendar;
mod popup_network;
mod popup_notif;
mod popup_power;
mod popup_tray;
mod power;
mod preview;
mod render;
mod settings_pages;
mod settings_view;
mod settings_window;
mod sha256;
mod shell_thread;
mod start_import;
mod start_search;
mod surface;
mod taskbar;
mod tray_host;
mod ui;
mod update;
mod update_release;
mod watchdog;
mod winhttp;

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{
    AttachConsole, GetConsoleMode, GetStdHandle, SetConsoleCtrlHandler, SetConsoleMode, SetStdHandle,
    ATTACH_PARENT_PROCESS, CONSOLE_MODE as ConMode, CTRL_BREAK_EVENT, CTRL_C_EVENT, ENABLE_EXTENDED_FLAGS,
    ENABLE_QUICK_EDIT_MODE, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW, InternalGetWindowText,
    KillTimer, LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, TranslateMessage,
    UnregisterClassW, IDC_ARROW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MSG, MSGFLT_ALLOW, WM_CLOSE, WM_DESTROY, WM_ENDSESSION,
    WM_HOTKEY, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use msg::{
    TIMER_ANIM, TIMER_APPS_DEBOUNCE, TIMER_CLOCK, TIMER_HOUSEKEEPING, TIMER_HOVER_PICKER, TIMER_POLL, TIMER_RAM,
    TIMER_RECONCILE, TIMER_SECOND, TIMER_TRAY_REANNOUNCE, TIMER_UPDATE_CHECK, WM_DOCK_CONFIG_APPLY, WM_DOCK_DIAG,
    WM_DOCK_NOTIF, WM_DOCK_OPEN_SETTINGS, WM_DOCK_POPUP_CLOSED, WM_DOCK_QUIT_REQUEST, WM_DOCK_START_MENU,
    WM_DOCK_TASKBAR_EVENT, WM_DOCK_TOGGLE, WM_DOCK_TRAY_CHANGED, WM_DOCK_UPDATE, WM_DOCK_WINKEY,
};

// ---------------------------------------------------------------- constants

const CLASS_NAME: PCWSTR = w!("DockWindow");
const HOTKEY_ID: i32 = 1;

// ------------------------------------------------------------------- state

/// HWND of the dock window (0 = not created yet / already destroyed). Also read
/// by the Ctrl+C handler, the hook/shell threads and callbacks through `hwnd()`.
static HWND_RAW: AtomicIsize = AtomicIsize::new(0);
/// Original input console mode (0 = not saved), restored on exit.
static CONSOLE_MODE: AtomicU32 = AtomicU32::new(0);
/// true if the process has an attached console (`attach_console` succeeded).
static CONSOLE_ATTACHED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static HOOK_THREAD: RefCell<Option<hook::HookThread>> = const { RefCell::new(None) };
    static SHELL_THREAD: RefCell<Option<shell_thread::ShellHandle>> = const { RefCell::new(None) };
    static STOP_WATCHER: RefCell<Option<JoinHandle<()>>> = const { RefCell::new(None) };
}

/// HWND of the dock window (null if it does not exist). Usable from any thread.
pub fn hwnd() -> HWND {
    HWND(HWND_RAW.load(Relaxed) as *mut c_void)
}

// --------------------------------------------------------------------- CLI

/// Command-line arguments. `dock.exe [--top|--bottom] [--no-autohide] [--keep-taskbar]
/// [--diag] [--restore] [--watchdog <pid> <event>]` plus the legacy flags.
#[derive(Debug, Default)]
pub struct Args {
    /// `Some(true)` = `--top`, `Some(false)` = `--bottom`, `None` = use the config.
    pub top: Option<bool>,
    /// `--no-autohide`: bar always visible.
    pub no_autohide: bool,
    /// `--keep-taskbar`: do not touch the native taskbar.
    pub keep_taskbar: bool,
    /// `--diag`: Win key event diagnostics.
    pub diag: bool,
    /// `--restore`: restore the native taskbar and exit.
    pub restore: bool,
    /// `--watchdog <pid> <event>`: watchdog mode.
    pub watchdog: Option<(u32, String)>,
    /// `--foreground` (legacy flag): always force foreground on show.
    pub foreground: bool,
    /// `--no-foreground` (legacy flag): never auto-foreground over fullscreen apps.
    pub no_foreground: bool,
    /// `--no-toggle` (legacy flag): Win alone is swallowed but does not toggle.
    pub no_toggle: bool,
    /// `--preview <dir>`: write offscreen PNG previews into the folder and exit.
    pub preview: Option<String>,
    /// `--list-apps`: print the open app windows with their resolved names and exit.
    pub list_apps: bool,
    /// `--install`: copy the exe to `%ProgramFiles%\mini-oled-dock` and register the logon autostart.
    pub install: bool,
    /// `--uninstall`: remove the logon autostart.
    pub uninstall: bool,
    /// `--autostart`: started by the logon task (waits for Explorer before starting).
    pub autostart: bool,
    /// `--console`: attach the parent console (log on stdout, Ctrl+C) in normal runs.
    pub console: bool,
    /// `--version`: print "mini-oled-dock X.Y.Z" and exit 0.
    pub version: bool,
    /// `--quiet` (with `--install` / `--uninstall`): no message box, no console output; the exit code is the result.
    pub quiet: bool,
    /// `--no-relaunch` (with `--install`): do not start the dock again after installing.
    pub no_relaunch: bool,
    /// `--remove` (with `--uninstall`): also delete the installed files.
    pub remove: bool,
    /// `--purge-data` (with `--uninstall --remove`): also delete the data folder.
    pub purge_data: bool,
    /// `--update-check`: check for a new release, print the result and exit.
    pub update_check: bool,
    /// `--update-install <tag>`: install the staged release `<tag>`.
    pub update_install: Option<String>,
    /// `--settings`: ask the running dock to open its Settings window and exit.
    pub settings: bool,
}

static ARGS: OnceLock<Args> = OnceLock::new();

/// Process arguments (defaults if `main` has not set them yet). Any thread.
pub fn args() -> &'static Args {
    ARGS.get_or_init(Args::default)
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    while let Some(s) = it.next() {
        match s.as_str() {
            "--top" => a.top = Some(true),
            "--bottom" => a.top = Some(false),
            "--no-autohide" => a.no_autohide = true,
            "--keep-taskbar" => a.keep_taskbar = true,
            "--diag" => a.diag = true,
            "--restore" => a.restore = true,
            "--watchdog" => {
                let pid = it.next().and_then(|p| p.parse::<u32>().ok()).ok_or("--watchdog requires <pid> <event>")?;
                let ev = it.next().ok_or("--watchdog requires <pid> <event>")?;
                a.watchdog = Some((pid, ev));
            }
            "--foreground" => a.foreground = true,
            "--no-foreground" => a.no_foreground = true,
            "--no-toggle" => a.no_toggle = true,
            "--preview" => a.preview = Some(it.next().ok_or("--preview requires <folder>")?),
            "--list-apps" => a.list_apps = true,
            "--install" => a.install = true,
            "--uninstall" => a.uninstall = true,
            "--autostart" => a.autostart = true,
            "--console" => a.console = true,
            "--version" => a.version = true,
            "--quiet" => a.quiet = true,
            "--no-relaunch" => a.no_relaunch = true,
            "--remove" => a.remove = true,
            "--purge-data" => a.purge_data = true,
            "--update-check" => a.update_check = true,
            "--settings" => a.settings = true,
            "--update-install" => a.update_install = Some(it.next().ok_or("--update-install requires <tag>")?),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.install && a.uninstall {
        return Err("--install and --uninstall cannot be combined".into());
    }
    if a.quiet && !(a.install || a.uninstall) {
        return Err("--quiet requires --install or --uninstall".into());
    }
    if a.no_relaunch && !a.install {
        return Err("--no-relaunch requires --install".into());
    }
    if a.remove && !a.uninstall {
        return Err("--remove requires --uninstall".into());
    }
    if a.purge_data && !a.remove {
        return Err("--purge-data requires --uninstall --remove".into());
    }
    Ok(a)
}

/// Exit code for a command-line parse error: 0 for `--help` (empty message), else "bad arguments".
fn parse_error_exit(msg: &str) -> i32 {
    if msg.is_empty() {
        autostart::EXIT_OK
    } else {
        autostart::EXIT_BAD_ARGS
    }
}

/// Where the result of `--install` / `--uninstall` goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CliReport {
    /// Printed on the attached console.
    Console,
    /// No console: shown in a message box (the behaviour without `--quiet`).
    MessageBox,
    /// `--quiet`: log file only, the exit code carries the result.
    LogOnly,
}

fn cli_report(quiet: bool, has_console: bool) -> CliReport {
    if quiet {
        CliReport::LogOnly
    } else if has_console {
        CliReport::Console
    } else {
        CliReport::MessageBox
    }
}

// ------------------------------------------------------------------ RAM

/// Idle-trim scheduler state (UI thread only), driven by the 2 s housekeeping timer.
#[derive(Default)]
struct TrimState {
    /// Since when the bar has been hidden with no popup open.
    hidden_since: Option<Instant>,
    /// A trim already happened during the current hidden period.
    trimmed: bool,
    /// Last time the bar was hidden-to-visible, or a popup was open (visible-idle timer base).
    last_activity: Option<Instant>,
    /// Last trim of any kind (global rate limit).
    last_trim: Option<Instant>,
}

const TRIM_HIDDEN_DELAY: Duration = Duration::from_secs(4);
const TRIM_VISIBLE_IDLE: Duration = Duration::from_secs(600);
const TRIM_MIN_GAP: Duration = Duration::from_secs(60);

thread_local! {
    static TRIM: RefCell<TrimState> = RefCell::new(TrimState::default());
}

/// Decides whether to trim memory now; never during show/hide animations (4 s hidden delay).
fn trim_tick() {
    let now = Instant::now();
    let visible = ui::is_visible();
    // An open Settings window counts like an open popup: no trim under the user's hands.
    let popup = popup::is_open() || settings_window::is_open();
    let go = TRIM.with(|s| {
        let mut s = s.borrow_mut();
        let rate_ok = s.last_trim.is_none_or(|t| now.duration_since(t) >= TRIM_MIN_GAP);
        let mut go = None;
        if !visible && !popup {
            let since = *s.hidden_since.get_or_insert(now);
            if !s.trimmed && rate_ok && now.duration_since(since) >= TRIM_HIDDEN_DELAY {
                go = Some("hidden");
            }
            s.last_activity = Some(now);
        } else {
            s.hidden_since = None;
            s.trimmed = false;
            if popup {
                s.last_activity = Some(now);
            } else {
                let base = *s.last_activity.get_or_insert(now);
                if rate_ok && now.duration_since(base) >= TRIM_VISIBLE_IDLE {
                    go = Some("visible-idle");
                    s.last_activity = Some(now);
                }
            }
        }
        if go.is_some() {
            s.trimmed = true;
            s.last_trim = Some(now);
        }
        go
    });
    if let Some(reason) = go {
        mem::trim(reason);
    }
}

/// `busy` of `update::on_idle`: an automatic install would interrupt an open popup, the open Settings
/// window or a fullscreen app.
fn auto_install_busy(popup: bool, settings: bool, fullscreen: bool) -> bool {
    popup || settings || fullscreen
}

// ------------------------------------------------------------ window proc

/// "Core" messages handled here (exit, keyboard hook, service timers, taskbar, session);
/// everything else goes to `ui::handle_message`, and if the UI does not handle it either, to `DefWindowProcW`.
unsafe extern "system" fn wnd_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_TIMER => match wp.0 {
            TIMER_HOUSEKEEPING => {
                // Windows may silently remove the hook after a timeout; the hook thread reinstalls it.
                HOOK_THREAD.with(|t| {
                    if let Some(t) = t.borrow().as_ref() {
                        hook::request_reinstall(t);
                    }
                });
                if config::poll_changed() {
                    tray_host::invalidate_trust_cache();
                    ui::on_config_changed();
                    update::sync_timer(h);
                }
                trim_tick();
                if update::auto_install_pending() {
                    // No automatic install while a popup or the Settings window is open, or over a
                    // fullscreen app.
                    update::on_idle(auto_install_busy(
                        popup::is_open(),
                        settings_window::is_open(),
                        ui::foreground_is_fullscreen(),
                    ));
                }
                LRESULT(0)
            }
            TIMER_UPDATE_CHECK => {
                update::on_timer(h);
                LRESULT(0)
            }
            TIMER_RAM => {
                mem::log_memory();
                LRESULT(0)
            }
            TIMER_SECOND => {
                watchdog::heartbeat();
                taskbar::on_safety_timer();
                LRESULT(0)
            }
            TIMER_TRAY_REANNOUNCE => {
                // One-shot: it kills itself before running.
                // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
                let _ = unsafe { KillTimer(Some(h), TIMER_TRAY_REANNOUNCE) };
                taskbar::on_reannounce_timer();
                LRESULT(0)
            }
            _ => dispatch_to_ui(h, m, wp, lp),
        },
        WM_DOCK_TOGGLE => {
            ui::win_tap();
            LRESULT(0)
        }
        WM_DOCK_START_MENU => {
            ui::win_double_tap();
            LRESULT(0)
        }
        WM_DOCK_WINKEY => {
            match wp.0 {
                hook::EV_PRESSED => log!("Win pressed (swallowed)"),
                hook::EV_ALONE => log!("Win released alone -> toggle dock"),
                hook::EV_COMBO => log!("Win + other key -> injected the pair [Win down, key down]"),
                hook::EV_SENDFAIL => log!("SendInput failed: Win+key combination NOT delivered"),
                hook::EV_DOUBLE => log!("Win tapped twice -> Start menu"),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DOCK_DIAG => {
            if !hook::DIAG.load(Relaxed) {
                return LRESULT(0);
            }
            let w = wp.0;
            let fg = HWND(lp.0 as *mut c_void);
            let mut buf = [0u16; 64];
            // SAFETY: `buf` is writable; InternalGetWindowText does not send messages to the window.
            let n = unsafe { InternalGetWindowText(fg, &mut buf) }.max(0) as usize;
            let title = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
            log!(
                "  [diag] WIN {}{} inj={} lowil={} swallowed={} ourtag={} ext={} rwin={} scan=0x{:X} latency={}ms fg=\"{}\"",
                if w & 1 != 0 { "DOWN" } else { "" },
                if w & 2 != 0 { "UP" } else { "" },
                (w >> 2) & 1,
                (w >> 3) & 1,
                (w >> 4) & 1,
                (w >> 5) & 1,
                (w >> 6) & 1,
                (w >> 7) & 1,
                (w >> 8) & 0xFFFF,
                (w >> 24) & 0xFFFF,
                title
            );
            LRESULT(0)
        }
        WM_DOCK_TRAY_CHANGED => {
            ui::on_tray_changed(wp.0);
            popup::on_tray_changed();
            settings_window::on_tray_changed();
            LRESULT(0)
        }
        WM_DOCK_NOTIF => {
            ui::on_notif_changed();
            popup::on_notif_changed();
            LRESULT(0)
        }
        WM_DOCK_POPUP_CLOSED => {
            ui::on_popup_closed();
            LRESULT(0)
        }
        WM_DOCK_CONFIG_APPLY => {
            ui::on_config_changed();
            update::sync_timer(h);
            LRESULT(0)
        }
        WM_DOCK_OPEN_SETTINGS => {
            settings_window::open();
            LRESULT(0)
        }
        WM_DOCK_UPDATE => {
            update::on_changed(h);
            settings_window::on_update_changed();
            LRESULT(0)
        }
        WM_DOCK_TASKBAR_EVENT => {
            taskbar::on_winevent(wp.0 as u32, HWND(lp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_DOCK_QUIT_REQUEST => {
            taskbar::begin_exit();
            log!("quit request (stop event: --restore, --install or --uninstall): exiting");
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wp.0 as i32 == HOTKEY_ID {
                taskbar::begin_exit();
                log!("Ctrl+Alt+Q: exit");
                // SAFETY: no preconditions.
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            taskbar::begin_exit();
            log!("close request (Ctrl+C / menu): exiting");
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_QUERYENDSESSION => {
            // Deliberately no `begin_exit`: another app can still cancel the shutdown, and a surviving dock
            // would be left with `RESTORING` set and its paused tray host never restarted.
            log!("WM_QUERYENDSESSION: allowing the session end, exit deferred to WM_ENDSESSION");
            LRESULT(1)
        }
        WM_ENDSESSION => {
            // End of session: the taskbar state would survive a reboot, so restore it right away.
            if wp.0 != 0 {
                taskbar::begin_exit();
                taskbar::restore();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            taskbar::restore();
            LRESULT(0)
        }
        _ if m != 0 && m == taskbar::taskbar_created_msg() => {
            taskbar::on_taskbar_created();
            LRESULT(0)
        }
        _ => dispatch_to_ui(h, m, wp, lp),
    }
}

fn dispatch_to_ui(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match ui::handle_message(h, m, wp, lp) {
        Some(r) => r,
        // SAFETY: forwards the parameters received from Windows unchanged.
        None => unsafe { DefWindowProcW(h, m, wp, lp) },
    }
}

/// Runs on the thread created by Windows for Ctrl+C: it can only post a message.
/// Exit goes through WM_CLOSE -> cleanup -> `taskbar::restore()` (and the watchdog covers the rest).
unsafe extern "system" fn ctrl_handler(ctrl: u32) -> BOOL {
    if ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT {
        // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
        let _ = unsafe { PostMessageW(Some(hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        BOOL(1)
    } else {
        BOOL(0)
    }
}

// ------------------------------------------------------- window and loop

fn register_class(inst: HINSTANCE) -> bool {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: inst,
        // SAFETY: loads a system cursor (null hInstance), no pointers to manage.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialized; the class name is a static literal.
    let atom = unsafe { RegisterClassExW(&wc) };
    atom != 0
}

fn create_window(inst: HINSTANCE, g: &ui::Geom) -> Option<HWND> {
    // SAFETY: class registered above; the PCWSTRs are static literals.
    let r = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("Dock"),
            WS_POPUP,
            g.x,
            g.y,
            g.w,
            g.h,
            None,
            None,
            Some(inst),
            None,
        )
    };
    match r {
        Ok(h) => {
            // `dock --settings` posts from a medium-integrity terminal to this elevated window; UIPI would
            // drop the message. It only opens the dock's own Settings window, so allowing it is harmless.
            // SAFETY: `h` is the window just created by this thread; no optional struct.
            if let Err(e) = unsafe { ChangeWindowMessageFilterEx(h, WM_DOCK_OPEN_SETTINGS, MSGFLT_ALLOW, None) } {
                log!("ChangeWindowMessageFilterEx(WM_DOCK_OPEN_SETTINGS) failed: {e}");
            }
            Some(h)
        }
        Err(e) => {
            log!("CreateWindowExW failed: {e}");
            None
        }
    }
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is valid and writable; filter None = all windows of the thread.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            break; // WM_QUIT
        }
        if r.0 < 0 {
            log!("GetMessageW error");
            break;
        }
        // SAFETY: `msg` was just filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn set_timer(h: HWND, id: usize, ms: u32) {
    // SAFETY: `h` is our window; without a callback the WM_TIMERs reach wnd_proc.
    if unsafe { SetTimer(Some(h), id, ms, None) } == 0 {
        log!("SetTimer {id} failed");
    }
}

// --------------------------------------------------------- console / process

/// Attaches the parent process console (if any) so that stdout/stderr are visible again in a
/// terminal despite the `windows` subsystem. Failures are ignored (started from Explorer/task:
/// no parent with a console; elevated dock launched from a non-elevated terminal: access denied).
/// If the standard handles are not valid it points them at `CONOUT$`. Returns true if attached.
fn attach_console() -> bool {
    // SAFETY: no preconditions; fails harmlessly if the parent has no console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
        return false;
    }
    CONSOLE_ATTACHED.store(true, Relaxed);
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: no preconditions.
        let cur = unsafe { GetStdHandle(which) };
        if matches!(cur, Ok(h) if !h.is_invalid() && !h.0.is_null()) {
            continue;
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            use std::os::windows::io::IntoRawHandle;
            let raw = f.into_raw_handle();
            // SAFETY: freshly opened console handle, handed over to the process (intentionally never closed).
            let _ = unsafe { SetStdHandle(which, windows::Win32::Foundation::HANDLE(raw)) };
        }
    }
    true
}

/// Waits (up to 30 s) for the Explorer shell to be ready: used with `--autostart`, when the
/// dock starts very early during logon. Then a short pause to let Explorer finish its tray init.
fn wait_for_shell() {
    let t = std::time::Instant::now();
    loop {
        // SAFETY: static literal class; FindWindowW has no other preconditions.
        let found = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.is_ok();
        if found {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            log!("autostart: shell ready after {} ms", t.elapsed().as_millis());
            return;
        }
        if t.elapsed() > std::time::Duration::from_secs(30) {
            log!("autostart: Shell_TrayWnd did not appear within 30 s, continuing anyway");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Disables QuickEdit on the input console: a mouse selection would pause
/// writes to stdout. Only the `log` thread would block now (the hook is no longer
/// involved); still useful to avoid piling up lines in the channel during a selection.
fn disable_quick_edit() {
    if !CONSOLE_ATTACHED.load(Relaxed) {
        return; // no console (normal case): nothing to do
    }
    // SAFETY: no preconditions; the handle is the process standard one.
    let Ok(hin) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else { return };
    let mut mode = ConMode(0);
    // SAFETY: `mode` is valid and writable; if stdin is not a console it fails harmlessly.
    if unsafe { GetConsoleMode(hin, &mut mode) }.is_err() {
        return; // stdin is not a console
    }
    if mode.0 & ENABLE_QUICK_EDIT_MODE.0 == 0 {
        return;
    }
    CONSOLE_MODE.store(mode.0, Relaxed);
    let new = ConMode((mode.0 & !ENABLE_QUICK_EDIT_MODE.0) | ENABLE_EXTENDED_FLAGS.0);
    // SAFETY: console handle obtained above.
    match unsafe { SetConsoleMode(hin, new) } {
        Ok(()) => log!("console QuickEdit disabled (restored on exit)"),
        Err(e) => {
            CONSOLE_MODE.store(0, Relaxed);
            log!("SetConsoleMode failed: {e}");
        }
    }
}

fn restore_console_mode() {
    let saved = CONSOLE_MODE.swap(0, Relaxed);
    if saved == 0 {
        return;
    }
    // SAFETY: no preconditions.
    if let Ok(hin) = unsafe { GetStdHandle(STD_INPUT_HANDLE) } {
        // SAFETY: console handle; restores the mode saved at startup.
        let _ = unsafe { SetConsoleMode(hin, ConMode(saved)) };
    }
}

/// Windows 11 may apply EcoQoS/power throttling to a background process,
/// slowing down the wake-up of its threads: for the LL hook this is fatal (timeout -> the key
/// passes through). Disables throttling and raises the process priority class.
fn disable_power_throttling() {
    use windows::Win32::System::Threading::{
        ProcessPowerThrottling, SetPriorityClass, SetProcessInformation, ABOVE_NORMAL_PRIORITY_CLASS,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0, // 0 = throttling disabled for the bits in ControlMask
    };
    // SAFETY: current process pseudo-handle; struct initialized and size correct.
    let r = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    match r {
        Ok(()) => log!("power throttling disabled"),
        Err(e) => log!("SetProcessInformation(PowerThrottling) failed: {e}"),
    }
    // SAFETY: current process pseudo-handle.
    match unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) } {
        Ok(()) => log!("priority class ABOVE_NORMAL"),
        Err(e) => log!("SetPriorityClass failed: {e}"),
    }
}

/// Panic hook: with `panic = "abort"` the process terminates right after, so the reason is written
/// synchronously to the log and the taskbar is restored before the abort. (The watchdog covers the rest.)
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logger::write_sync(&format!("PANIC: {info}"));
        taskbar::restore();
        prev(info);
    }));
}

// ------------------------------------------------------------ run / cleanup

fn cleanup(h: HWND, inst: HINSTANCE) {
    log!("cleanup...");
    for id in [
        TIMER_POLL,
        TIMER_HOUSEKEEPING,
        TIMER_RAM,
        TIMER_SECOND,
        TIMER_RECONCILE,
        TIMER_APPS_DEBOUNCE,
        TIMER_ANIM,
        TIMER_CLOCK,
        TIMER_TRAY_REANNOUNCE,
        TIMER_HOVER_PICKER,
        TIMER_UPDATE_CHECK,
    ] {
        // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
        let _ = unsafe { KillTimer(Some(h), id) };
    }
    // The tray host BEFORE the taskbar: its Shell_TrayWnd must disappear before the
    // TaskbarCreated broadcast of `restore`, so apps re-register their icons with Explorer.
    tray_host::stop();
    // The native taskbar right after: it is the thing that must not stay hidden.
    taskbar::shutdown();
    picker::shutdown();
    // A drag still held in the Settings window is saved before the window goes.
    settings_window::shutdown();
    popup::shutdown();
    // SAFETY: as above.
    let _ = unsafe { UnregisterHotKey(Some(h), HOTKEY_ID) };
    if let Some(t) = HOOK_THREAD.with(|t| t.borrow_mut().take()) {
        hook::stop(t);
    }
    apps::remove_winevent_hook();
    notif::stop();
    if let Some(s) = SHELL_THREAD.with(|s| s.borrow_mut().take()) {
        shell_thread::stop(s);
    }
    if CONSOLE_ATTACHED.load(Relaxed) {
        // SAFETY: removes the handler registered in setup_and_loop.
        let _ = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), false) };
    }
    ui::shutdown();
    restore_console_mode();
    if !h.0.is_null() {
        // SAFETY: window created by this thread (WM_DESTROY also restores the taskbar: idempotent).
        if let Err(e) = unsafe { DestroyWindow(h) } {
            log!("DestroyWindow failed: {e}");
        }
        HWND_RAW.store(0, Relaxed);
    }
    // SAFETY: class registered by this module; fails if never registered.
    let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(inst)) };
    watchdog::shutdown(STOP_WATCHER.with(|s| s.borrow_mut().take()));
    log!("clean exit");
}

fn run(args: &Args) -> Result<(), String> {
    // The manifest already sets PerMonitorV2; the call is a safety net and may fail
    // with "access denied" if the context is already fixed: not an error.
    // SAFETY: must be called before creating windows; the constant is valid.
    if let Err(e) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        log!("SetProcessDpiAwarenessContext: {e} (probably already set by the manifest)");
    }
    // COM STA on the UI thread too (needed by SHGetPropertyStoreForWindow in `apps`).
    // SAFETY: paired with CoUninitialize at the end of `run`.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if com.is_err() {
        log!("CoInitializeEx (UI thread) failed: {com:?}");
    }
    let result = run_inner(args);
    if com.is_ok() {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

fn run_inner(args: &Args) -> Result<(), String> {
    disable_quick_edit();
    hook::NO_TOGGLE.store(args.no_toggle, Relaxed);
    hook::DIAG.store(args.diag, Relaxed);
    if args.no_toggle {
        log!("EXPERIMENT --no-toggle: Win alone is swallowed but the dock does NOT show");
    }

    config::init();

    // 1. Watchdog BEFORE touching the taskbar. If it does not start, nothing gets hidden (no safety net).
    let watchdog_ok = match watchdog::spawn() {
        Ok(()) => true,
        Err(e) => {
            log!("watchdog not started: {e}");
            false
        }
    };
    // 2. Native taskbar (wait for Shell_TrayWnd up to 30 s, self-heal, save state, hide).
    if config::effective_hide_native_taskbar() && watchdog_ok {
        match taskbar::init() {
            Ok(true) => log!("native taskbar hidden"),
            Ok(false) => log!("native taskbar left as is"),
            Err(e) => log!("taskbar::init failed (continuing without hiding it): {e}"),
        }
    } else {
        log!("native taskbar NOT touched (--keep-taskbar, config or watchdog missing)");
    }

    // 3. Geometry and window.
    let g = ui::init()?;
    // SAFETY: no preconditions.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let inst = HINSTANCE(module.0);
    if !register_class(inst) {
        return Err("RegisterClassExW failed".into());
    }
    let Some(h) = create_window(inst, &g) else {
        // SAFETY: class registered above by this module.
        let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(inst)) };
        return Err("window creation failed".into());
    };
    HWND_RAW.store(h.0 as isize, Relaxed);

    let result = setup_and_loop(h);
    cleanup(h, inst);
    result
}

fn setup_and_loop(h: HWND) -> Result<(), String> {
    ui::attach(h)?;
    // OLED popups: without a registered class the UI falls back to the direct (non-popup) actions.
    if let Err(e) = popup::init() {
        log!("popups unavailable: {e}");
    }

    // Shell thread (COM STA): name/icons/audio/launch.
    match shell_thread::start(h) {
        Ok(s) => {
            SHELL_THREAD.with(|c| *c.borrow_mut() = Some(s));
            if config::get().show_volume {
                shell_thread::request(shell_thread::ShellRequest::VolumeRefresh);
            }
        }
        Err(e) => log!("shell thread not started (no names/icons/audio): {e}"),
    }
    // WinRT notifications + "Do not disturb" (`notif` thread, non-blocking).
    if config::get().show_notifications {
        notif::start(h);
    }
    // Tray host: only with the native taskbar hidden. Order: Explorer already hidden and at
    // the bottom of the Z-order -> host topmost and ready -> TaskbarCreated announcement (apps re-register with us).
    if taskbar::is_hidden() && config::get().show_tray {
        match tray_host::start(h) {
            Ok(()) => taskbar::announce_tray(),
            Err(e) => log!("tray host not started (icons stay in Explorer's tray): {e}"),
        }
    }

    // WinEventHook (UI thread): taskbar control (the apps one is installed by `ui::attach`).
    if let Err(e) = taskbar::install_hook() {
        log!("taskbar WinEventHook not installed: {e}");
    }
    STOP_WATCHER.with(|c| *c.borrow_mut() = watchdog::start_stop_watcher(h));

    // Ctrl+C only with an attached console (`--console`): without one, exit is Ctrl+Alt+Q or the menu.
    if CONSOLE_ATTACHED.load(Relaxed) {
        // SAFETY: `extern "system"` handler valid for the whole lifetime of the process.
        if let Err(e) = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) } {
            log!("SetConsoleCtrlHandler failed (Ctrl+C not handled): {e}");
        }
    }
    // SAFETY: `h` is our window; 0x51 = 'Q'.
    if let Err(e) = unsafe { RegisterHotKey(Some(h), HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x51) } {
        log!("RegisterHotKey Ctrl+Alt+Q failed: {e}");
    } else {
        log!("hotkey Ctrl+Alt+Q registered");
    }

    // Service timers (the UI ones are armed by `ui::attach`).
    set_timer(h, TIMER_HOUSEKEEPING, 2000);
    set_timer(h, TIMER_RAM, 30_000);
    set_timer(h, TIMER_SECOND, 1000);
    update::init(h);
    mem::log_memory();

    // The window already exists (HWND_RAW set): the callback can post messages to it.
    let t = hook::start()?;
    HOOK_THREAD.with(|c| *c.borrow_mut() = Some(t));
    log!("running. Win alone = toggle, Ctrl+Alt+Q or Ctrl+C = exit");
    message_loop();
    Ok(())
}

// ------------------------------------------------------------------ main

/// First line of `--version`.
fn version_line() -> String {
    format!("mini-oled-dock {}", env!("CARGO_PKG_VERSION"))
}

/// `--settings`: posts `WM_DOCK_OPEN_SETTINGS` to the running dock. 0 = posted, 1 = no dock or post failed.
fn open_settings_cli() -> u8 {
    use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: static literal class; FindWindowW has no other preconditions.
    let Ok(h) = (unsafe { FindWindowW(CLASS_NAME, PCWSTR::null()) }) else {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "dock: no running dock found");
        return 1;
    };
    let mut pid = 0u32;
    // SAFETY: `h` was just found; `pid` is a valid out pointer. Both calls fail harmlessly if the window is gone.
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
    match unsafe { PostMessageW(Some(h), WM_DOCK_OPEN_SETTINGS, WPARAM(0), LPARAM(0)) } {
        Ok(()) => 0,
        Err(e) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "dock: could not reach the dock (access denied?): {e}");
            1
        }
    }
}

fn usage() {
    use std::io::Write;
    let mut e = std::io::stderr();
    let _ = writeln!(
        e,
        "usage: dock [--top | --bottom] [--no-autohide] [--keep-taskbar] [--diag] [--restore] \
         [--install [--quiet] [--no-relaunch]] [--uninstall [--remove [--purge-data]] [--quiet]] \
         [--version] [--settings] [--update-check] [--update-install <tag>] [--autostart] [--console] \
         [--watchdog <pid> <event>] [--foreground | --no-foreground] [--no-toggle] [--preview <folder>] [--list-apps]"
    );
}

/// Sets up the data folder (`datadir::init`: elevated = create/verify/repair) and then starts the
/// logger; the folder diagnostics are logged as the first lines. `--preview` and `--list-apps` do
/// not call this, so they never create or touch the data folder.
fn start_logging(tag: &'static str) -> (Option<JoinHandle<()>>, Vec<String>) {
    let notes = datadir::init();
    let handle = logger::start(tag);
    for n in &notes {
        log!("{n}");
    }
    (handle, notes)
}

fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            attach_console();
            if !msg.is_empty() {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "{msg}");
            }
            usage();
            return std::process::ExitCode::from(parse_error_exit(&msg) as u8);
        }
    };
    let args = ARGS.get_or_init(|| args);

    // --version: no data folder, logger or mutex.
    if args.version {
        attach_console();
        println!("{}", version_line());
        return std::process::ExitCode::SUCCESS;
    }

    // --preview mode: offscreen rendering to PNG, before watchdog/taskbar/hook/single instance.
    if let Some(dir) = &args.preview {
        attach_console();
        return std::process::ExitCode::from(preview::run(dir).clamp(0, 255) as u8);
    }
    // --list-apps: diagnostic dump of the name pipeline; no taskbar, no hooks, no single instance.
    if args.list_apps {
        attach_console();
        return std::process::ExitCode::from(shell_thread::list_apps().clamp(0, 255) as u8);
    }
    // Watchdog mode: no window, no hook; log with tag "wd".
    if let Some((pid, event)) = &args.watchdog {
        let (log_thread, _) = start_logging("wd");
        let code = watchdog::run_watchdog(*pid, event);
        logger::stop(log_thread);
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }
    // --install / --uninstall mode: messages via `logger::say`, log to file only.
    if args.install || args.uninstall {
        // Without a console (UAC from a non-elevated terminal, Explorer) the result would be lost: message box.
        let report = cli_report(args.quiet, !args.quiet && attach_console());
        if report != CliReport::Console {
            logger::buffer_say();
        }
        let (log_thread, notes) = start_logging("");
        // The installer is where a refused (e.g. squatted) data folder must be visible to the user.
        for n in notes.iter().filter(|n| !n.starts_with("data: created")) {
            logger::say(n);
        }
        let mut code = if args.install {
            autostart::run_install_cli(args.no_relaunch)
        } else {
            autostart::run_uninstall_cli(args.remove)
        };
        if report == CliReport::LogOnly {
            // Keep the messages in the log, then keep buffering whatever is said later (nobody shows it).
            for line in logger::take_say_buffer().lines() {
                log!("{line}");
            }
            logger::buffer_say();
        }
        logger::stop(log_thread);
        // `--purge-data`: only after a clean `--uninstall --remove`, and only now that `dock.log` is closed.
        // The result is still said before the message box or console output (with `--quiet` only the exit
        // code reports it).
        if code == autostart::EXIT_OK && args.purge_data {
            match datadir::purge() {
                Ok(()) => logger::say("Data folder removed."),
                Err(e) => {
                    logger::say(&format!("Removing the data folder failed: {e}"));
                    code = autostart::EXIT_FAILED;
                }
            }
        }
        if report == CliReport::MessageBox {
            let text = logger::take_say_buffer();
            if !text.is_empty() {
                let wtext: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let icon = if code == 0 { MB_ICONINFORMATION } else { MB_ICONERROR };
                // SAFETY: `wtext` and `w!` are NUL-terminated UTF-16 strings, valid for the call.
                unsafe { MessageBoxW(None, PCWSTR(wtext.as_ptr()), w!("Dock"), MB_OK | icon) };
            }
        }
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }
    // --restore mode: restores the taskbar and exits.
    if args.restore {
        if attach_console() {
            logger::enable_stdout();
        }
        let (log_thread, _) = start_logging("");
        let code = taskbar::run_restore_cli();
        logger::stop(log_thread);
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }

    // --settings mode: asks the running dock to open the Settings window.
    if args.settings {
        attach_console();
        return std::process::ExitCode::from(open_settings_cli());
    }

    // --update-check mode: asks GitHub once, prints the result and exits.
    if args.update_check {
        if attach_console() {
            logger::enable_stdout();
        }
        let (log_thread, _) = start_logging("");
        let code = update::run_check_cli();
        logger::stop(log_thread);
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }

    // --update-install <tag> mode: hands an already staged, verified release to --install --quiet.
    if let Some(tag) = &args.update_install {
        if attach_console() {
            logger::enable_stdout();
        }
        let (log_thread, _) = start_logging("");
        let code = update::run_install_cli(tag);
        logger::stop(log_thread);
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }

    if args.console && attach_console() {
        logger::enable_stdout();
    }
    // Single instance: a second instance would mistake the taskbar hidden by the first for a dead
    // run (self-heal) and would share the state file. Exits before touching anything (data folder
    // included: `datadir::init` runs only once the mutex is ours), without the final
    // `taskbar::restore()` (which would restore the first instance's taskbar).
    if !watchdog::acquire_single_instance() {
        datadir::debug_out("another dock instance is already running: exiting");
        return std::process::ExitCode::FAILURE;
    }
    let (log_thread, _) = start_logging("");
    if args.autostart {
        autostart::warn_if_not_installed();
        wait_for_shell();
    }
    install_panic_hook();
    disable_power_throttling();
    let res = run(args);
    // Safety net: even if `run` exited early with an error, the taskbar comes back (idempotent).
    taskbar::restore();
    restore_console_mode();
    let code = match res {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log!("ERROR: {e}");
            std::process::ExitCode::FAILURE
        }
    };
    logger::stop(log_thread);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Args, String> {
        parse_args_from(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_auto_install_while_settings_is_open() {
        assert!(auto_install_busy(false, true, false), "Settings window open");
        assert!(auto_install_busy(true, false, false), "popup open");
        assert!(auto_install_busy(false, false, true), "fullscreen app");
        assert!(!auto_install_busy(false, false, false));
    }

    #[test]
    fn settings_flag_parses() {
        let a = p(&["--settings"]).unwrap();
        assert!(a.settings && !a.install);
        assert!(!p(&[]).unwrap().settings);
        assert!(p(&["--setings"]).is_err());
    }

    #[test]
    fn cli_new_flags() {
        let a = p(&["--install", "--quiet", "--no-relaunch"]).unwrap();
        assert!(a.install && a.quiet && a.no_relaunch);
        let u = p(&["--uninstall", "--remove", "--purge-data", "--quiet"]).unwrap();
        assert!(u.uninstall && u.remove && u.purge_data && u.quiet);
        assert_eq!(p(&["--update-install", "v1.4.1"]).unwrap().update_install.as_deref(), Some("v1.4.1"));
        assert!(p(&["--update-check"]).unwrap().update_check);
        assert!(p(&["--version"]).unwrap().version);
    }

    #[test]
    fn cli_rejects_bad_combinations() {
        let bad: [&[&str]; 8] = [
            &["--quiet"],
            &["--no-relaunch"],
            &["--remove"],
            &["--uninstall", "--purge-data"],
            &["--install", "--remove"],
            &["--install", "--uninstall"],
            &["--uninstall", "--no-relaunch"],
            &["--update-install"],
        ];
        for b in bad {
            assert!(matches!(p(b), Err(e) if !e.is_empty()), "{b:?}");
        }
    }

    #[test]
    fn cli_report_quiet_never_shows_a_box() {
        assert_eq!(cli_report(true, false), CliReport::LogOnly);
        assert_eq!(cli_report(true, true), CliReport::LogOnly);
        assert_eq!(cli_report(false, true), CliReport::Console);
        assert_eq!(cli_report(false, false), CliReport::MessageBox);
    }

    #[test]
    fn cli_exit_codes() {
        use crate::autostart::{EXIT_BAD_ARGS, EXIT_FAILED, EXIT_OK, EXIT_STOP_FAILED};
        assert_eq!((EXIT_OK, EXIT_FAILED, EXIT_BAD_ARGS, EXIT_STOP_FAILED), (0, 1, 2, 3));
        assert_eq!(parse_error_exit(""), EXIT_OK);
        assert_eq!(parse_error_exit(&p(&["--quiet"]).unwrap_err()), EXIT_BAD_ARGS);
        assert_eq!(parse_error_exit(&p(&["--bogus"]).unwrap_err()), EXIT_BAD_ARGS);
    }

    #[test]
    fn version_line_matches_cargo() {
        assert_eq!(version_line(), format!("mini-oled-dock {}", env!("CARGO_PKG_VERSION")));
    }
}
