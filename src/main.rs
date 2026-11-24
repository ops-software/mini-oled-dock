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
//! 10. `wake-probe`: only with `DOCK_WAKE_PROBE=1` or the `wakeprobe.on` marker (`probe`, diagnostic build).
//!
//!  Popups (`popup`) live on the UI thread.
//!  Plus the watchdog PROCESS (`dock.exe --watchdog <pid> <event>`).
//!
//! STARTUP ORDER (`run`): config -> watchdog::spawn (before touching the taskbar) -> taskbar::init
//! -> ui::init (geometry) -> window -> ui::attach (+ apps WinEvent hook) -> popup::init -> shell thread -> notif ->
//! tray host (+ taskbar::announce_tray) -> taskbar WinEvent hook -> stop-watch -> watchdog exit wait ->
//! Ctrl+C/hotkey/timers -> keyboard hook -> message loop. Cleanup runs in reverse order, with the tray host
//! stopped and the taskbar restored first (host destroyed BEFORE the TaskbarCreated broadcast).

//! SUBSYSTEM: `windows` (no console). The CLI modes that print (--help, --restore,
//! --install, --uninstall, --preview, errors) attach to the parent process console with
//! `attach_console`; `--console` does the same for normal runs too (log on stdout and Ctrl+C).
#![windows_subsystem = "windows"]

/// Statements of the wake probe (`--features wake-probe`, P0-wsC-v2 Q3): they compile to nothing without it.
macro_rules! probe {
    ($($t:tt)*) => {
        #[cfg(feature = "wake-probe")]
        {
            $($t)*
        }
    };
}

mod apps;
mod arp;
mod audio;
mod autostart;
mod battery;
mod calendar;
mod clock;
mod config;
mod datadir;
mod files_child;
mod glyphs;
mod hook;
mod i18n;
mod icons;
mod jumplist;
mod launcher;
mod launcher_files;
#[cfg(feature = "owner-probe")]
mod launcher_probe;
mod launcher_sys;
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
#[cfg(feature = "wake-probe")]
mod probe;
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
mod tinysort;
mod tray_host;
mod ui;
mod update;
mod update_release;
mod watchdog;
mod winhttp;

use std::cell::{Cell, RefCell};
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
use windows::Win32::System::Diagnostics::Debug::{
    SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, THREAD_ERROR_MODE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW,
    GetMessageW, KillTimer, LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer,
    TranslateMessage, UnregisterClassW, ENDSESSION_CLOSEAPP, IDC_ARROW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MSG,
    MSGFLT_ALLOW, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_HOTKEY, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use msg::{
    TIMER_ANIM, TIMER_APPS_DEBOUNCE, TIMER_CLOCK, TIMER_CONFIG_COALESCE, TIMER_HOUSEKEEPING, TIMER_HOVER_PICKER,
    TIMER_POLL, TIMER_RAM, TIMER_RECONCILE, TIMER_SECOND, TIMER_TRAY_REANNOUNCE, TIMER_TRIM, TIMER_UPDATE_CHECK,
    WM_DOCK_CONFIG_APPLY, WM_DOCK_CONFIG_CHANGED, WM_DOCK_DIAG, WM_DOCK_NOTIF, WM_DOCK_OPEN_SETTINGS,
    WM_DOCK_POPUP_CLOSED, WM_DOCK_QUIT_REQUEST, WM_DOCK_START_MENU, WM_DOCK_TASKBAR_EVENT, WM_DOCK_TOGGLE,
    WM_DOCK_TRAY_CHANGED, WM_DOCK_TRIM_KICK, WM_DOCK_UPDATE, WM_DOCK_WATCHDOG_EXITED, WM_DOCK_WINKEY,
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

/// Whether the calling thread is the dock window's (the UI thread): false on any other thread and before the window
/// exists.
pub fn on_ui_thread() -> bool {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    // SAFETY: no preconditions; a null or stale handle yields 0, which is no thread's id.
    unsafe { GetWindowThreadProcessId(hwnd(), None) == GetCurrentThreadId() }
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
    /// `--update-fetch`: the update check's GET in a child of the dock, its answer on stdout
    /// (`update::run_fetch_child`); combines with nothing.
    pub update_fetch: bool,
    /// `--start-apps`: the AppsFolder catalogue listed in a child of the dock, its arena on stdout
    /// (`shell_thread::run_apps_child`); combines with nothing.
    pub start_apps: bool,
    /// `--start-settings`: the settings catalogue built in a child of the dock, its arena on stdout
    /// (`start_search::run_settings_child`); combines with nothing.
    pub start_settings: bool,
    /// `--start-icons <px>` (8-256): the icons of the AppsFolder ids on stdin made in a child of the dock at `px`, as
    /// frames on stdout (`shell_thread::run_icons_child`); combines with nothing.
    pub start_icons: Option<u32>,
    /// `--launcher-files`: the launcher's file search in a child of the dock (`files_child::run`, plan L2-F2): request
    /// lines on stdin, frames on stdout; combines with nothing but `--stall` and `--root`.
    pub launcher_files: bool,
    /// `--stall <ms>` (owner-probe builds only, with `--launcher-files`): the helper sleeps before each query (V-4).
    pub stall: Option<u32>,
    /// `--root <folder>` (with `--launcher-files`; at most 8, each at most 260 UTF-16 units): a folder of
    /// `launcher.file_folders` the dock accepted; the helper checks each again and trusts none (SPEC 5.4.6).
    pub roots: Vec<String>,
    /// `--update-install <tag>`: install the staged release `<tag>`.
    pub update_install: Option<String>,
    /// `--update-guard <tag>`: the old binary judges the staged release (started only by the updater as
    /// `update\<tag>\dock-guard.exe`); combines with nothing.
    pub update_guard: Option<String>,
    /// `--rollback` (alone or with `--quiet`): install `dock.prev.exe` again.
    pub rollback: bool,
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
    let mut flags = 0usize;
    while let Some(s) = it.next() {
        flags += 1;
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
            "--update-fetch" => a.update_fetch = true,
            "--start-apps" => a.start_apps = true,
            "--start-settings" => a.start_settings = true,
            "--start-icons" => {
                let px =
                    it.next().and_then(|v| v.parse::<u32>().ok()).filter(|px| shell_thread::ICON_PX_RANGE.contains(px));
                a.start_icons = Some(px.ok_or("--start-icons requires a pixel size from 8 to 256")?);
            }
            "--launcher-files" => a.launcher_files = true,
            #[cfg(feature = "owner-probe")]
            "--stall" => a.stall = Some(it.next().and_then(|v| v.parse().ok()).ok_or("--stall requires <ms>")?),
            "--root" if a.roots.len() < launcher::files::MAX_EXTRA_ROOTS => {
                let r = it.next().filter(|r| r.encode_utf16().count() <= launcher::files::ROOT_UNITS);
                a.roots.push(r.ok_or("--root requires a folder of at most 260 characters")?);
            }
            "--settings" => a.settings = true,
            "--update-install" => a.update_install = Some(it.next().ok_or("--update-install requires <tag>")?),
            "--update-guard" => a.update_guard = Some(it.next().ok_or("--update-guard requires <tag>")?),
            "--rollback" => a.rollback = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.update_fetch && flags > 1 {
        return Err("--update-fetch cannot be combined with other arguments".into());
    }
    if a.start_apps && flags > 1 {
        return Err("--start-apps cannot be combined with other arguments".into());
    }
    if a.start_settings && flags > 1 {
        return Err("--start-settings cannot be combined with other arguments".into());
    }
    if a.start_icons.is_some() && flags > 1 {
        return Err("--start-icons cannot be combined with other arguments".into());
    }
    if a.stall.is_some() && !a.launcher_files {
        return Err("--stall requires --launcher-files".into());
    }
    if !a.roots.is_empty() && !a.launcher_files {
        return Err("--root requires --launcher-files".into());
    }
    if a.launcher_files && flags > 1 + usize::from(a.stall.is_some()) + a.roots.len() {
        return Err("--launcher-files cannot be combined with other arguments".into());
    }
    if a.update_guard.is_some() && flags > 1 {
        return Err("--update-guard cannot be combined with other arguments".into());
    }
    if a.rollback && flags > 1 + usize::from(a.quiet) {
        return Err("--rollback combines only with --quiet".into());
    }
    if a.install && a.uninstall {
        return Err("--install and --uninstall cannot be combined".into());
    }
    if a.quiet && !(a.install || a.uninstall || a.rollback) {
        return Err("--quiet requires --install, --uninstall or --rollback".into());
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

/// `--rollback` run from the installed `dock.exe` must leave at once, as the older installer it starts replaces that
/// file (`update::guard::run_rollback_cli`): a message box becomes the log only (that installer shows its own result);
/// a console still gets the lines. Pure.
fn rollback_report(report: CliReport, from_installed: bool) -> CliReport {
    if from_installed && report == CliReport::MessageBox {
        CliReport::LogOnly
    } else {
        report
    }
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

/// The trim decision of one tick: `rearm` (data was freed since the last trim, `mem::take_rearm`) allows one more
/// "hidden" trim in the current hidden period; the 4 s hidden delay, the 60 s gap and "never with a popup or Settings
/// window open" stay.
fn trim_step(s: &mut TrimState, now: Instant, visible: bool, popup: bool, rearm: bool) -> Option<&'static str> {
    if rearm {
        s.trimmed = false;
    }
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
}

/// Milliseconds until the next hidden trim is worth trying, or None when there is nothing to wait for. None once the
/// current hidden period has trimmed (unless `rearm`: data was freed since); 4 s while a popup or the Settings window
/// is open (the trim never runs under the user's hands; a close kicks it sooner); else the time left to 4 s after
/// `hidden_since` or to 60 s after `last_trim`, whichever is later, at least 250 ms.
fn hidden_trim_retry_ms(s: &TrimState, now: Instant, popup: bool, rearm: bool) -> Option<u32> {
    if popup {
        return Some(TRIM_HIDDEN_DELAY.as_millis() as u32);
    }
    if s.trimmed && !rearm {
        return None;
    }
    let since = s.hidden_since.unwrap_or(now) + TRIM_HIDDEN_DELAY;
    let due = s.last_trim.map_or(since, |t| since.max(t + TRIM_MIN_GAP));
    let left = due.saturating_duration_since(now).as_millis();
    Some(left.clamp(250, u128::from(u32::MAX)) as u32)
}

/// Whether `TIMER_HOUSEKEEPING` has work: the visible-idle trim and the bar's own upkeep while shown, an automatic
/// install waiting for an idle moment, the config poll while the folder watch is down.
fn housekeeping_wanted(visible: bool, install_pending: bool, config_watch: bool) -> bool {
    visible || install_pending || !config_watch
}

thread_local! {
    /// Armed state of `TIMER_HOUSEKEEPING` and `TIMER_RAM` (UI thread): SetTimer only on a transition, since arming
    /// again would restart the period.
    static HOUSEKEEPING_ARMED: Cell<bool> = const { Cell::new(false) };
    static RAM_ARMED: Cell<bool> = const { Cell::new(false) };
}

fn timer_transition(h: HWND, id: usize, ms: u32, armed: &'static std::thread::LocalKey<Cell<bool>>, want: bool) {
    if armed.with(|a| a.replace(want)) != want {
        if want {
            set_timer(h, id, ms);
        } else {
            // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
            let _ = unsafe { KillTimer(Some(h), id) };
        }
    }
}

/// Arms or kills `TIMER_HOUSEKEEPING` by `housekeeping_wanted`.
fn housekeeping_sync(h: HWND) {
    let want = housekeeping_wanted(ui::is_visible(), update::auto_install_pending(), config::watching());
    timer_transition(h, TIMER_HOUSEKEEPING, 2000, &HOUSEKEEPING_ARMED, want);
}

/// Arms or kills `TIMER_RAM`: the 30 s RAM line only while the bar is shown.
fn ram_sync(h: HWND) {
    timer_transition(h, TIMER_RAM, 30_000, &RAM_ARMED, ui::is_visible());
}

/// Starts a new hidden period for the trim (a popup or Settings close, a data release, a show).
fn trim_period_reset(now: Instant) {
    TRIM.with(|s| reset_hidden_period(&mut s.borrow_mut(), now));
}

fn reset_hidden_period(s: &mut TrimState, now: Instant) {
    s.hidden_since = None;
    s.trimmed = false;
    s.last_activity = Some(now);
}

/// Delay of `TIMER_TRIM`: the earlier of the hidden trim still to come and the planned second COM pass; none while the
/// bar is shown or when there is neither.
fn trim_timer_ms(visible: bool, retry: Option<u32>, pass: Option<Duration>) -> Option<u32> {
    if visible {
        return None;
    }
    let pass = pass.map(|d| d.as_millis().clamp(250, u128::from(u32::MAX)) as u32);
    retry.into_iter().chain(pass).min()
}

/// (Re)arms the one-shot `TIMER_TRIM` while the bar is hidden: for the hidden trim still to come and for the planned
/// second COM pass, whichever is earlier; killed when there is neither (and while the bar is shown).
fn arm_trim_timer(h: HWND) {
    let now = Instant::now();
    let ms = if ui::is_visible() {
        None
    } else {
        let popup = popup::is_open() || settings_window::is_open();
        let retry = TRIM.with(|s| hidden_trim_retry_ms(&s.borrow(), now, popup, false));
        trim_timer_ms(false, retry, mem::second_pass_in(now))
    };
    match ms {
        Some(ms) => set_timer(h, TIMER_TRIM, ms),
        // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
        None => drop(unsafe { KillTimer(Some(h), TIMER_TRIM) }),
    }
}

/// A new hidden period starts now (a popup or the Settings window closed, data was released): with the bar hidden,
/// the trim runs 4 s from now (at most once per 60 s) and the timer follows. Nothing while the bar is shown.
/// UI thread only.
pub(crate) fn trim_kick(h: HWND) {
    if h.0.is_null() || ui::is_visible() {
        return;
    }
    trim_period_reset(Instant::now());
    trim_tick();
    arm_trim_timer(h);
}

/// The bar was shown or hidden (`ui::on_visibility`): the RAM line and housekeeping follow, the hidden trim starts
/// 4 s after a hide and is cancelled by a show.
pub(crate) fn on_bar_visibility(visible: bool) {
    let h = hwnd();
    if h.0.is_null() {
        return;
    }
    ram_sync(h);
    housekeeping_sync(h);
    if visible {
        trim_period_reset(Instant::now());
        arm_trim_timer(h);
    } else {
        // `trim_tick` records `hidden_since`; the timer then fires when the delay is over.
        trim_tick();
        arm_trim_timer(h);
    }
}

/// Decides whether to trim memory now; never during show/hide animations (4 s hidden delay).
fn trim_tick() {
    mem::second_pass_tick(Instant::now());
    let visible = ui::is_visible();
    // An open Settings window counts like an open popup: no trim under the user's hands.
    let popup = popup::is_open() || settings_window::is_open();
    let go = TRIM.with(|s| trim_step(&mut s.borrow_mut(), Instant::now(), visible, popup, mem::take_rearm()));
    if let Some(reason) = go {
        mem::trim(reason);
        if reason == "hidden" {
            // The hidden bar has no 30 s RAM line: one line per hidden trim instead.
            mem::log_memory();
        }
    }
}

/// `busy` of `update::on_idle`: an automatic install would interrupt an open popup, the open Settings
/// window or a fullscreen app.
fn auto_install_busy(popup: bool, settings: bool, fullscreen: bool) -> bool {
    popup || settings || fullscreen
}

// ------------------------------------------------------------ window proc

/// Set by `message_loop` as its first statement (UI thread); `wnd_proc` beats only from then on (`beats_for`).
static LOOP_RUNNING: AtomicBool = AtomicBool::new(false);

/// Whether a message to the dock window beats (`watchdog::beat_if_due`): none before the message loop runs
/// (`loop_running`), so window creation and setup keep the watchdog's 33 s startup tolerance; then all but the shell
/// hook's (`shell_msg`, 0 while no shell hook is registered).
fn beats_for(m: u32, shell_msg: u32, loop_running: bool) -> bool {
    loop_running && (shell_msg == 0 || m != shell_msg)
}

/// "Core" messages handled here (exit, keyboard hook, service timers, taskbar, session);
/// everything else goes to `ui::handle_message`, and if the UI does not handle it either, to `DefWindowProcW`.
unsafe extern "system" fn wnd_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // The heartbeat at the start of every message to the dock window, posted or sent, at most once a second (wsC Q7): a
    // stall begins inside a message, so at most 1 s after a beat. Not for the shell hook's message: its handler cannot
    // stall, and its HSHELL_REDRAW alone would beat about once a second. Not before the message loop runs: this
    // procedure already runs inside CreateWindowExW, and the first real beat ends the watchdog's startup tolerance.
    let beat = beats_for(m, ui::shell_hook_msg(), LOOP_RUNNING.load(Relaxed)) && watchdog::beat_if_due();
    match m {
        WM_TIMER => match wp.0 {
            TIMER_HOUSEKEEPING => {
                if !config::watching() {
                    apply_config_change(h);
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
                housekeeping_sync(h);
                LRESULT(0)
            }
            TIMER_TRIM => {
                // One-shot: it kills itself before running, then arms itself again if there is more to wait for.
                // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
                let _ = unsafe { KillTimer(Some(h), TIMER_TRIM) };
                trim_tick();
                arm_trim_timer(h);
                LRESULT(0)
            }
            TIMER_CONFIG_COALESCE => {
                // One-shot: it kills itself before running.
                // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
                let _ = unsafe { KillTimer(Some(h), TIMER_CONFIG_COALESCE) };
                apply_config_change(h);
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
                // The idle keepalive: the line above has beaten for this tick unless a beat came less than 1 s before.
                if !beat {
                    watchdog::heartbeat();
                }
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
            let class = crate::ui::window_class(fg);
            log!(
                "  [diag] WIN {}{} inj={} lowil={} swallowed={} ourtag={} ext={} rwin={} scan=0x{:X} latency={}ms fg_class=\"{}\"",
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
                class
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
            trim_kick(h);
            LRESULT(0)
        }
        WM_DOCK_TRIM_KICK => {
            trim_kick(h);
            LRESULT(0)
        }
        WM_DOCK_CONFIG_CHANGED => {
            if wp.0 == 1 {
                if config::stop_watch() {
                    log!("config: change watch broken, polling every 2 s");
                }
                // The watch is gone: the 2 s poll must run even while the bar is hidden.
                housekeeping_sync(h);
            } else {
                // Re-armed by every notification: an editor's several writes become one read.
                set_timer(h, TIMER_CONFIG_COALESCE, 250);
            }
            LRESULT(0)
        }
        WM_DOCK_CONFIG_APPLY => {
            ui::on_config_changed();
            update::sync_timer(h);
            // `update_auto_install` may have just made a staged install pending.
            housekeeping_sync(h);
            LRESULT(0)
        }
        WM_DOCK_OPEN_SETTINGS => {
            if wp.0 == msg::SETTINGS_PAGE_LAUNCHER {
                settings_window::open_at(settings_pages::Page::Launcher);
            } else {
                settings_window::open();
            }
            LRESULT(0)
        }
        WM_DOCK_UPDATE => {
            update::on_changed(h);
            // An automatic install may just have become pending: housekeeping is its only driver.
            housekeeping_sync(h);
            settings_window::on_update_changed();
            LRESULT(0)
        }
        WM_DOCK_TASKBAR_EVENT => {
            taskbar::on_winevent(wp.0 as u32, HWND(lp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_DOCK_WATCHDOG_EXITED => {
            watchdog::on_exited();
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
            if session_step(m, wp.0, lp.0) == Some(SessionStep::Allow { close_app: true }) {
                log!("WM_QUERYENDSESSION (Restart Manager close): allowing it, exit deferred to WM_ENDSESSION");
            } else {
                log!("WM_QUERYENDSESSION: allowing the session end, exit deferred to WM_ENDSESSION");
            }
            LRESULT(1)
        }
        WM_ENDSESSION => {
            match session_step(m, wp.0, lp.0) {
                // End of session: the taskbar state would survive a reboot, so restore it right away. Explorer may save
                // its settings before this restore: the state file stays for the next sign-in, and the watchdog is
                // ended first so it cannot restore again and delete it.
                Some(SessionStep::SessionEnd) => {
                    taskbar::begin_exit();
                    taskbar::restore_at_session_end();
                }
                // The Restart Manager closes the dock and the session goes on: the restore of a normal exit right away
                // (it deletes the state file), then the exit of WM_CLOSE; the watchdog stays until the dock is gone.
                Some(SessionStep::CloseApp) => {
                    taskbar::begin_exit();
                    log!("WM_ENDSESSION (Restart Manager close, the session goes on): restoring the taskbar, exiting");
                    taskbar::restore();
                    // SAFETY: no preconditions.
                    unsafe { PostQuitMessage(0) };
                }
                _ => {}
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

/// What a session message asks of the dock (`wnd_proc`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SessionStep {
    /// `WM_QUERYENDSESSION`: allowed, and nothing changes before `WM_ENDSESSION` (another app can still cancel).
    /// `close_app`: the Restart Manager asks (only the log line differs).
    Allow { close_app: bool },
    /// `WM_ENDSESSION` with wParam FALSE: the end was cancelled; the dock runs on as it was.
    Cancelled,
    /// `WM_ENDSESSION` with wParam TRUE, the session ending (sign-out, shutdown, a critical end): the session-end
    /// restore, which ends the watchdog and keeps the state file for the next sign-in.
    SessionEnd,
    /// `WM_ENDSESSION` with wParam TRUE and `ENDSESSION_CLOSEAPP`: the Restart Manager closes the dock (an installer,
    /// servicing) and the session goes on, so nothing of the session end: the normal exit's restore at once, then the
    /// exit. Also with other bits set, since a restore at a real end is harmless: when in doubt, restore.
    CloseApp,
}

/// Pure: `m`, `wp`, `lp` of a message to the dock window; None for any other message.
fn session_step(m: u32, wp: usize, lp: isize) -> Option<SessionStep> {
    // The ENDSESSION_* flags are the low 32 bits of lParam.
    let close_app = (lp as u32) & ENDSESSION_CLOSEAPP != 0;
    match m {
        WM_QUERYENDSESSION => Some(SessionStep::Allow { close_app }),
        WM_ENDSESSION if wp == 0 => Some(SessionStep::Cancelled),
        WM_ENDSESSION if close_app => Some(SessionStep::CloseApp),
        WM_ENDSESSION => Some(SessionStep::SessionEnd),
        _ => None,
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
    // From here on `wnd_proc` beats (`beats_for`), in the probe's loop too.
    LOOP_RUNNING.store(true, Relaxed);
    // Wake probe on (DOCK_WAKE_PROBE=1 or wakeprobe.on): its instrumented loop; off, the release loop below.
    #[cfg(feature = "wake-probe")]
    if probe::ui_loop() {
        return;
    }
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
        // The dock window's messages beat in `wnd_proc`; the others (popups, Settings, thread messages) here, so no
        // message beats twice.
        if msg.hwnd != hwnd() {
            watchdog::beat_if_due();
        }
        // SAFETY: `msg` was just filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Loads a hand edit of `config.json` (if its mtime differs from the remembered one) and lets the UI rebuild.
fn apply_config_change(h: HWND) {
    if config::poll_changed() {
        ui::config_reloaded();
        housekeeping_sync(h);
    }
}

fn set_timer(h: HWND, id: usize, ms: u32) {
    // SAFETY: `h` is our window; without a callback the WM_TIMERs reach wnd_proc.
    if unsafe { SetTimer(Some(h), id, ms, None) } == 0 {
        log!("SetTimer {id} failed");
    }
}

// --------------------------------------------------------- console / process

/// The error mode of a child mode (`--update-fetch`, `--start-apps`, `--start-settings`, `--start-icons`): a crash in a
/// third-party shell or icon handler, or a failed file open, ends the child silently instead of showing the user a
/// dialog; the dock sees the failed exit and takes its fallback.
fn child_error_mode() -> THREAD_ERROR_MODE {
    SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX
}

/// Whether the arguments select a child mode of the dock; add each new child flag here (the error mode is set for them).
fn is_child_mode(a: &Args) -> bool {
    a.update_fetch || a.start_apps || a.start_settings || a.start_icons.is_some() || a.launcher_files
}

/// Sets `child_error_mode` for the whole process, first thing of a child mode.
fn set_child_error_mode() {
    // SAFETY: no preconditions; the mode of this short-lived child process only.
    unsafe { SetErrorMode(child_error_mode()) };
}

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
        TIMER_CONFIG_COALESCE,
        TIMER_TRIM,
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
    // The config watch's callback posts to the window: it ends first (and waits for a callback in progress).
    config::stop_watch();
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
    // It is skipped when the manifest already made the thread PerMonitorV2 (the normal case: no false error line).
    // SAFETY: GetThreadDpiAwarenessContext and AreDpiAwarenessContextsEqual have no preconditions; the constant is a valid context.
    let already = unsafe {
        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    }
    .as_bool();
    if !already {
        // SAFETY: must be called before creating windows; the constant is valid.
        if let Err(e) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
            log!("SetProcessDpiAwarenessContext: {e} (probably already set by the manifest)");
        }
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
        notif::start(h, ui::is_visible());
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
    // The window exists: the watchdog's exit can be posted to it from here on.
    watchdog::watch_exit();

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
    set_timer(h, TIMER_SECOND, watchdog::HEARTBEAT_MS);
    update::init(h);
    // A hand edit of config.json is applied from the folder's change notification; without it, housekeeping polls.
    config::start_watch(h);
    mem::log_memory();
    // The service timers by state (the bar's visibility changes keep them in step from here); a bar that starts hidden
    // trims 4 s from now.
    housekeeping_sync(h);
    ram_sync(h);
    trim_kick(h);

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
         [--rollback [--quiet]] [--version] [--settings] [--update-check] [--update-install <tag>] [--autostart] \
         [--console] [--watchdog <pid> <event>] [--foreground | --no-foreground] [--no-toggle] [--preview <folder>] \
         [--list-apps]"
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
    if is_child_mode(args) {
        set_child_error_mode();
    }
    // --update-fetch: the update check's GET in a child of the dock (`update::fetch_latest`); stdout only,
    // no data folder, logger, mutex or console.
    if args.update_fetch {
        return std::process::ExitCode::from(update::run_fetch_child().clamp(0, 255) as u8);
    }
    // --start-apps: the AppsFolder catalogue listed in a child of the dock (`shell_thread::list_apps_arena`); stdout
    // only, no data folder, logger, mutex or console.
    if args.start_apps {
        return std::process::ExitCode::from(shell_thread::run_apps_child().clamp(0, 255) as u8);
    }
    // --start-settings: the settings catalogue built in a child of the dock (`start_search::ensure_settings_cache` and
    // Start's load); stdout only, no data folder, logger, mutex or console.
    if args.start_settings {
        return std::process::ExitCode::from(start_search::run_settings_child().clamp(0, 255) as u8);
    }
    // --start-icons <px>: the Start menu's and the launcher's icons made in a child of the dock
    // (`shell_thread::spawn_icon_job`); stdin and stdout only, no data folder, logger, mutex or console.
    if let Some(px) = args.start_icons {
        return std::process::ExitCode::from(shell_thread::run_icons_child(px).clamp(0, 255) as u8);
    }
    // --launcher-files: the launcher's file search in a child of the dock (`files_child::run`, plan L2-F2); stdin and
    // stdout only, no data folder, logger, mutex or console.
    if args.launcher_files {
        return std::process::ExitCode::from(files_child::run(args.stall.unwrap_or(0), &args.roots).clamp(0, 255) as u8);
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
    // --install / --uninstall / --rollback mode: messages via `logger::say`, log to file only.
    if args.install || args.uninstall || args.rollback {
        // Without a console (UAC from a non-elevated terminal, Explorer) the result would be lost: message box.
        let report = cli_report(args.quiet, !args.quiet && attach_console());
        let report = if args.rollback { rollback_report(report, update::guard::runs_installed()) } else { report };
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
        } else if args.uninstall {
            autostart::run_uninstall_cli(args.remove)
        } else {
            update::guard::run_rollback_cli(args.quiet)
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

    // --update-install <tag> mode: hands an already staged, verified release to the update guard.
    if let Some(tag) = &args.update_install {
        if attach_console() {
            logger::enable_stdout();
        }
        let (log_thread, _) = start_logging("");
        let code = update::run_install_cli(tag);
        logger::stop(log_thread);
        return std::process::ExitCode::from(code.clamp(0, 255) as u8);
    }
    // --update-guard <tag>: the old binary judges the staged release (`update::guard`). Its lines carry the `ug` tag
    // and are on disk at once; it never takes `Local\DockInstance`.
    if let Some(tag) = &args.update_guard {
        let (log_thread, _) = start_logging("ug");
        let code = update::guard::run_cli(tag);
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
    #[cfg(feature = "wake-probe")]
    probe::init();
    let res = run(args);
    #[cfg(feature = "wake-probe")]
    probe::stop();
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
    fn probe_macro_compiles_to_nothing_without_the_feature() {
        let n = std::cell::Cell::new(0);
        probe!(n.set(1));
        assert_eq!(n.get(), if cfg!(feature = "wake-probe") { 1 } else { 0 });
    }

    /// 1.5.2 T3c: only a real session end takes the session-end restore; a Restart Manager close (ENDSESSION_CLOSEAPP,
    /// the session goes on) is a normal exit, and a cancelled end changes nothing.
    #[test]
    fn a_restart_manager_close_is_not_a_session_end() {
        use windows::Win32::UI::WindowsAndMessaging::{ENDSESSION_CRITICAL, ENDSESSION_LOGOFF};
        let step = |m, wp, lp: u32| session_step(m, wp, lp as isize);
        let (logoff, critical, close) = (ENDSESSION_LOGOFF, ENDSESSION_CRITICAL, ENDSESSION_CLOSEAPP);
        // A real sign-out.
        assert_eq!(step(WM_QUERYENDSESSION, 0, logoff), Some(SessionStep::Allow { close_app: false }));
        assert_eq!(step(WM_ENDSESSION, 1, logoff), Some(SessionStep::SessionEnd));
        // A real shutdown or restart (lParam 0).
        assert_eq!(step(WM_QUERYENDSESSION, 0, 0), Some(SessionStep::Allow { close_app: false }));
        assert_eq!(step(WM_ENDSESSION, 1, 0), Some(SessionStep::SessionEnd));
        // A critical end.
        assert_eq!(step(WM_QUERYENDSESSION, 0, critical), Some(SessionStep::Allow { close_app: false }));
        assert_eq!(step(WM_ENDSESSION, 1, critical), Some(SessionStep::SessionEnd));
        // The Restart Manager: wParam TRUE closes the dock without a session end; FALSE was cancelled.
        assert_eq!(step(WM_QUERYENDSESSION, 0, close), Some(SessionStep::Allow { close_app: true }));
        assert_eq!(step(WM_ENDSESSION, 1, close), Some(SessionStep::CloseApp));
        assert_eq!(step(WM_ENDSESSION, 0, close), Some(SessionStep::Cancelled));
        assert_eq!(step(WM_ENDSESSION, 1, close | critical), Some(SessionStep::CloseApp), "when in doubt, restore");
        assert_eq!(step(WM_ENDSESSION, 1, close | logoff), Some(SessionStep::CloseApp));
        // A cancelled query: the session goes on.
        assert_eq!(step(WM_ENDSESSION, 0, logoff), Some(SessionStep::Cancelled));
        assert_eq!(step(WM_ENDSESSION, 0, 0), Some(SessionStep::Cancelled));
        assert_eq!(step(WM_ENDSESSION, 0, critical), Some(SessionStep::Cancelled));
        // Any wParam other than 0 is TRUE; other messages are not session messages.
        assert_eq!(step(WM_ENDSESSION, 2, 0), Some(SessionStep::SessionEnd));
        assert_eq!(step(WM_CLOSE, 1, close), None);
    }

    #[test]
    fn shell_hook_messages_do_not_beat() {
        const SHELL: u32 = 0xC123;
        assert!(!beats_for(SHELL, SHELL, true), "the shell hook's message never beats");
        assert!(beats_for(WM_TIMER, SHELL, true));
        assert!(beats_for(WM_TIMER, 0, true));
        assert!(beats_for(0, 0, true), "no shell hook registered: every message beats");
    }

    /// Review of 7ed2043: `wnd_proc` already runs inside `CreateWindowExW`, with the heartbeat event in place. Its beat
    /// waits for the message loop, so the setup keeps the watchdog's 33 s startup tolerance, as in 1.5.0.
    #[test]
    fn no_beat_before_the_message_loop() {
        assert!(!beats_for(WM_TIMER, 0, false), "window creation and setup: no beat");
        assert!(!beats_for(WM_TIMER, 0xC123, false), "nor with a shell hook registered");
        assert!(beats_for(WM_TIMER, 0xC123, true), "the loop runs: the dock window's messages beat");
    }

    #[test]
    fn no_auto_install_while_settings_is_open() {
        assert!(auto_install_busy(false, true, false), "Settings window open");
        assert!(auto_install_busy(true, false, false), "popup open");
        assert!(auto_install_busy(false, false, true), "fullscreen app");
        assert!(!auto_install_busy(false, false, false));
    }

    #[test]
    fn housekeeping_only_where_it_has_work() {
        for visible in [false, true] {
            for install in [false, true] {
                for watch in [false, true] {
                    assert_eq!(
                        housekeeping_wanted(visible, install, watch),
                        visible || install || !watch,
                        "{visible} {install} {watch}"
                    );
                }
            }
        }
        // Hidden, watching, nothing pending: off. A pending automatic install keeps it armed while hidden.
        assert!(!housekeeping_wanted(false, false, true));
        assert!(housekeeping_wanted(false, true, true));
        assert!(housekeeping_wanted(false, false, false));
    }

    #[test]
    fn hidden_trim_once_per_hidden_period() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = TrimState::default();
        // The hide's own tick records hidden_since; the timer is due 4 s on.
        assert_eq!(trim_step(&mut s, at(0), false, false, false), None);
        assert_eq!(hidden_trim_retry_ms(&s, at(0), false, false), Some(4000));
        assert_eq!(trim_step(&mut s, at(4), false, false, false), Some("hidden"));
        assert_eq!(hidden_trim_retry_ms(&s, at(4), false, false), None);
        // Data freed since: one more trim, at the end of the 60 s gap.
        assert_eq!(hidden_trim_retry_ms(&s, at(10), false, true), Some(54_000));
    }

    #[test]
    fn the_rate_gap_retries_once() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = TrimState::default();
        assert_eq!(trim_step(&mut s, at(0), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(4), false, false, false), Some("hidden"));
        // A new hidden period 10 s later: the trim waits for the gap (60 s after the last one), no longer.
        reset_hidden_period(&mut s, at(10));
        assert_eq!(trim_step(&mut s, at(10), false, false, false), None);
        assert_eq!(hidden_trim_retry_ms(&s, at(10), false, false), Some(54_000));
        assert_eq!(trim_step(&mut s, at(64), false, false, false), Some("hidden"));
        assert_eq!(hidden_trim_retry_ms(&s, at(64), false, false), None);
        // Never below 250 ms.
        let s = TrimState { hidden_since: Some(at(0)), last_trim: None, ..TrimState::default() };
        assert_eq!(hidden_trim_retry_ms(&s, at(4), false, false), Some(250));
    }

    #[test]
    fn the_trim_timer_follows_the_earlier_of_retry_and_second_pass() {
        let s = Duration::from_secs;
        assert_eq!(trim_timer_ms(false, None, Some(s(600))), Some(600_000));
        assert_eq!(trim_timer_ms(false, Some(4000), Some(s(1))), Some(1000));
        assert_eq!(trim_timer_ms(false, None, Some(Duration::ZERO)), Some(250));
        assert_eq!(trim_timer_ms(true, Some(4000), Some(s(1))), None);
        assert_eq!(trim_timer_ms(false, None, None), None);
    }

    #[test]
    fn an_open_popup_defers_the_hidden_trim() {
        let t0 = Instant::now();
        let mut s = TrimState::default();
        assert_eq!(hidden_trim_retry_ms(&s, t0, true, false), Some(4000));
        s.trimmed = true;
        assert_eq!(hidden_trim_retry_ms(&s, t0, true, false), Some(4000));
        // No trim while the popup is open, even a long time after the hide.
        let mut s = TrimState::default();
        assert_eq!(trim_step(&mut s, t0, false, true, false), None);
        assert_eq!(trim_step(&mut s, t0 + Duration::from_secs(30), false, true, false), None);
    }

    #[test]
    fn a_close_while_hidden_trims_again() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = TrimState::default();
        assert_eq!(trim_step(&mut s, at(0), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(4), false, false, false), Some("hidden"));
        assert_eq!(hidden_trim_retry_ms(&s, at(5), false, false), None);
        // The kick after a close at 100 s: a new hidden period, the trim 4 s on.
        reset_hidden_period(&mut s, at(100));
        assert_eq!(trim_step(&mut s, at(100), false, false, false), None);
        assert_eq!(s.hidden_since, Some(at(100)));
        assert!(!s.trimmed);
        assert_eq!(hidden_trim_retry_ms(&s, at(100), false, false), Some(4000));
        assert_eq!(trim_step(&mut s, at(104), false, false, false), Some("hidden"));
        // A close 20 s after a trim: the time left to the 60 s gap.
        reset_hidden_period(&mut s, at(124));
        assert_eq!(trim_step(&mut s, at(124), false, false, false), None);
        assert_eq!(hidden_trim_retry_ms(&s, at(124), false, false), Some(40_000));
    }

    #[test]
    fn rearm_trims_again_once() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = TrimState::default();
        // Hidden for 4 s trims once.
        assert_eq!(trim_step(&mut s, at(0), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(2), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(4), false, false, false), Some("hidden"));
        // Still hidden at 60 s with no rearm: nothing.
        assert_eq!(trim_step(&mut s, at(60), false, false, false), None);
        // Rearm at 70 s while hidden: a trim at 70 s.
        assert_eq!(trim_step(&mut s, at(70), false, false, true), Some("hidden"));
        // Rearm at 75 s: nothing until the gap (130 s) is over, then one.
        assert_eq!(trim_step(&mut s, at(75), false, false, true), None);
        assert_eq!(trim_step(&mut s, at(100), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(129), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(130), false, false, false), Some("hidden"));
        assert_eq!(trim_step(&mut s, at(200), false, false, false), None);

        // A rearm while visible does not trim; the bar hidden at t trims at t + 4 s.
        let mut s = TrimState::default();
        assert_eq!(trim_step(&mut s, at(0), true, false, true), None);
        assert_eq!(trim_step(&mut s, at(10), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(13), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(14), false, false, false), Some("hidden"));

        // A popup open plus a rearm: no trim; the trim comes 4 s after the popup closes, the bar still hidden.
        let mut s = TrimState::default();
        assert_eq!(trim_step(&mut s, at(0), false, true, true), None);
        assert_eq!(trim_step(&mut s, at(30), false, true, false), None);
        assert_eq!(trim_step(&mut s, at(31), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(34), false, false, false), None);
        assert_eq!(trim_step(&mut s, at(35), false, false, false), Some("hidden"));
    }

    #[test]
    fn settings_flag_parses() {
        let a = p(&["--settings"]).unwrap();
        assert!(a.settings && !a.install);
        assert!(!p(&[]).unwrap().settings);
        assert!(p(&["--setings"]).is_err());
    }

    #[test]
    fn the_child_modes_are_told_from_the_rest() {
        let all = [
            &["--update-fetch"][..],
            &["--start-apps"],
            &["--start-settings"],
            &["--start-icons", "32"],
            &["--launcher-files"],
        ];
        for a in all {
            assert!(is_child_mode(&p(a).unwrap()), "{a:?}");
        }
        for a in [
            &[][..],
            &["--preview", "x"],
            &["--watchdog", "1", "ev"],
            &["--version"],
            &["--rollback"],
            &["--update-guard", "v1"],
        ] {
            assert!(!is_child_mode(&p(a).unwrap()), "{a:?}");
        }
    }

    #[test]
    fn the_child_modes_set_the_error_mode_before_any_work() {
        assert_eq!(child_error_mode().0, 0x0001 | 0x0002 | 0x8000);
        let src = include_str!("main.rs");
        let call = src.find("        set_child_error_mode();").expect("the call");
        let firsts = [
            "run_fetch_child()",
            "run_apps_child()",
            "run_settings_child()",
            "run_icons_child(px)",
            "run(args.stall.unwrap_or(0), &args.roots)",
        ];
        for first in firsts {
            let at = src.find(&format!("::{first}.clamp")).expect(first);
            assert!(call < at, "{first} runs after the error mode is set");
        }
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
        let f = p(&[update::FETCH_FLAG]).unwrap(); // the flag the dock starts its child with
        assert!(f.update_fetch && !f.update_check);
        assert!(!p(&["--update-check"]).unwrap().update_fetch);
        let s = p(&[shell_thread::APPS_FLAG]).unwrap(); // the flag of the apps child (L3 T17)
        assert!(s.start_apps && !s.settings && !s.autostart && !s.update_fetch);
        assert!(!p(&["--autostart"]).unwrap().start_apps);
        let s = p(&[start_search::SETTINGS_FLAG]).unwrap(); // the flag of the settings child (L3 T18)
        assert!(s.start_settings && !s.start_apps && !s.settings && !s.autostart);
        assert!(!p(&["--settings"]).unwrap().start_settings);
        let i = p(&[shell_thread::ICONS_FLAG, "40"]).unwrap(); // the flag of the icons child (L3 T20)
        assert!(i.start_icons == Some(40) && !i.start_apps && !i.start_settings && !i.settings);
        assert_eq!(p(&["--start-icons", "8"]).unwrap().start_icons, Some(8));
        assert_eq!(p(&["--start-icons", "256"]).unwrap().start_icons, Some(256));
        assert_eq!(p(&["--autostart"]).unwrap().start_icons, None);
        for bad in ["0", "300", "x", "7", "257", "-1", "", " 40", "40px"] {
            assert!(p(&["--start-icons", bad]).is_err(), "{bad:?}");
        }
        assert!(p(&["--start-icons"]).is_err(), "a size is required");
        // The files helper (plan L2-F2): alone; `--stall` only in owner-probe builds and only with it.
        let f = p(&["--launcher-files"]).unwrap();
        assert!(f.launcher_files && f.stall.is_none() && f.start_icons.is_none() && !f.autostart);
        assert!(!p(&["--autostart"]).unwrap().launcher_files);
        for bad in [
            &["--launcher-files", "--autostart"][..],
            &["--autostart", "--launcher-files"],
            &["--launcher-files", "--launcher-files"],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
        let stalled = p(&["--launcher-files", "--stall", "3000"]);
        #[cfg(feature = "owner-probe")]
        {
            assert_eq!(stalled.unwrap().stall, Some(3000));
            assert_eq!(p(&["--stall", "3000", "--launcher-files"]).unwrap().stall, Some(3000));
            assert!(p(&["--stall", "3000"]).is_err(), "only with --launcher-files");
            assert!(p(&["--launcher-files", "--stall", "x"]).is_err());
            assert!(p(&["--launcher-files", "--stall", "1", "--autostart"]).is_err());
        }
        #[cfg(not(feature = "owner-probe"))]
        assert!(stalled.is_err(), "no --stall in a release build");
        // The folders of launcher.file_folders the dock passes: only with the helper, at most 8, each bounded.
        let edge = format!("D:\\{}", "d".repeat(257));
        let f = p(&["--launcher-files", "--root", "D:\\", "--root", "E:\\Old docs", "--root", &edge]).unwrap();
        assert_eq!((f.launcher_files, f.roots), (true, vec!["D:\\".to_string(), "E:\\Old docs".into(), edge]));
        assert!(p(&["--launcher-files"]).unwrap().roots.is_empty());
        let mut eight = vec!["--launcher-files"];
        for _ in 0..8 {
            eight.extend(["--root", "E:\\x"]);
        }
        assert_eq!(p(&eight).unwrap().roots.len(), 8);
        eight.extend(["--root", "E:\\x"]);
        let over = format!("D:\\{}", "d".repeat(258));
        for bad in [
            &eight[..],
            &["--root", "D:\\"],
            &["--launcher-files", "--root"],
            &["--launcher-files", "--root", over.as_str()],
            &["--launcher-files", "--root", "D:\\", "--autostart"],
            &["--autostart", "--root", "D:\\"],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cli_update_guard_and_rollback() {
        assert_eq!(p(&["--update-guard", "v1.5.3"]).unwrap().update_guard.as_deref(), Some("v1.5.3"));
        let r = p(&["--rollback"]).unwrap();
        assert!(r.rollback && !r.quiet && !r.install && !r.uninstall && r.update_guard.is_none());
        let q = p(&["--rollback", "--quiet"]).unwrap();
        assert!(q.rollback && q.quiet);
        for bad in [&["--update-guard"][..], &["--rollback", "--install"], &["--rollback", "--uninstall"]] {
            assert!(matches!(p(bad), Err(e) if !e.is_empty()), "{bad:?}");
        }
        // The command lines the guard and --rollback start other releases with (contract C2).
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let g = parse_args_from(words(&update::guard::guard_args("v1.5.3")).into_iter()).unwrap();
        assert_eq!(g.update_guard.as_deref(), Some("v1.5.3"));
        let n = parse_args_from(words(update::guard::NEW_INSTALL_ARGS).into_iter()).unwrap();
        assert!(n.install && n.quiet && n.no_relaunch && !n.rollback);
        for quiet in [true, false] {
            let i = parse_args_from(words(update::guard::rollback_install_args(quiet)).into_iter()).unwrap();
            assert!(i.install && i.quiet == quiet && !i.no_relaunch, "{quiet}");
        }
    }

    #[test]
    fn a_rollback_from_the_installed_exe_never_waits_on_a_box() {
        assert_eq!(rollback_report(CliReport::MessageBox, true), CliReport::LogOnly);
        assert_eq!(rollback_report(CliReport::Console, true), CliReport::Console);
        assert_eq!(rollback_report(CliReport::MessageBox, false), CliReport::MessageBox);
        assert_eq!(rollback_report(CliReport::LogOnly, false), CliReport::LogOnly);
    }

    #[test]
    fn cli_rejects_bad_combinations() {
        let bad: [&[&str]; 28] = [
            &["--quiet"],
            &["--no-relaunch"],
            &["--remove"],
            &["--uninstall", "--purge-data"],
            &["--install", "--remove"],
            &["--install", "--uninstall"],
            &["--uninstall", "--no-relaunch"],
            &["--update-install"],
            &["--update-fetch", "--console"],
            &["--update-check", "--update-fetch"],
            &["--version", "--update-fetch"],
            &["--update-fetch", "--update-fetch"],
            &["--start-apps", "--autostart"],
            &["--settings", "--start-apps"],
            &["--start-apps", "--update-fetch"],
            &["--start-apps", "--start-apps"],
            &["--start-settings", "--autostart"],
            &["--settings", "--start-settings"],
            &["--start-settings", "--start-apps"],
            &["--start-settings", "--start-settings"],
            &["--start-icons", "40", "--autostart"],
            &["--settings", "--start-icons", "40"],
            &["--start-icons", "40", "--start-apps"],
            &["--start-icons", "40", "--start-icons", "40"],
            &["--update-guard", "v1.5.3", "--install"],
            &["--update-guard", "v1.5.3", "--update-fetch"],
            &["--update-fetch", "--update-guard", "v1.5.3"],
            &["--rollback", "--update-install", "t"],
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
