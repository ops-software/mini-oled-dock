//! "shell" COM STA thread: everything that is COM, Shell or spawns processes.
//!
//! Does: program name resolution, icon extraction/caching, audio (`AudioController`),
//! app launch (de-elevated through the desktop shell, see `launch`). The UI thread NEVER does these things.
//! The Start menu's and the launcher's icons are made by short-lived `--start-icons` children (one job per
//! client, read by an "icons" thread that pushes the events), the AppsFolder catalogue by an `--start-apps`
//! child: the shell components they load leave with them.
//!
//! PROTOCOL (neither side blocks the other):
//!  * UI -> shell: `request(ShellRequest)`. Puts the request in an mpsc channel and posts the
//!    thread message `WM_SHELL_WAKE` to the shell thread (which has a message loop, needed for
//!    COM STA and for audio callbacks). Never blocks.
//!  * shell -> UI: the shell thread puts results in a queue (`Mutex<Vec<ShellEvent>>`) and posts
//!    `WM_DOCK_SHELL_READY` to the UI window; the UI calls `drain_events()`.
//!    Volume does NOT go through the queue: `WM_DOCK_VOLUME` is posted directly (see `audio::post_volume`).
//!  * Shutdown: `stop(handle)` posts `WM_QUIT` to the thread and joins it (max 2 s).
//!
//! Name and icon resolution order: see the resolution functions below.
//! The shell thread reads the config (`config::get()`) for `names` and for custom icons.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use windows::core::{Interface, BSTR, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, PROPERTYKEY, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Graphics::Gdi::HGDIOBJ;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{
    CoAllowSetForegroundWindow, CoCreateInstance, CoFreeUnusedLibraries, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    IBindCtx, IDispatch, IPersistFile, IServiceProvider, CLSCTX_ALL, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, OpenProcessToken};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_BSTR, VT_I4};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_LWIN,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_CommonPrograms, FOLDERID_Programs, IEnumShellItems, IShellDispatch4, IShellItem,
    IShellItem2, IShellItemImageFactory, IShellLinkW, SHCreateItemFromParsingName, SHDefExtractIconW, SHGetFileInfoW,
    SHGetImageList, SHGetKnownFolderPath, Shell, ShellExecuteExW, ShellLink, KF_FLAG_DEFAULT, SEE_MASK_FLAG_NO_UI,
    SHELLEXECUTEINFOW, SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_JUMBO, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    SIIGBF_ICONONLY,
};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser, ShellWindows,
    CSIDL_DESKTOP, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, DestroyIcon, DispatchMessageW, GetClassLongPtrW, GetMessageW, GetWindowThreadProcessId,
    KillTimer, PeekMessageW, PostMessageW, PostThreadMessageW, SendMessageTimeoutW, SetTimer, TranslateMessage,
    ASFW_ANY, GCLP_HICON, GCLP_HICONSM, HICON, ICON_BIG, ICON_SMALL2, MSG, PM_NOREMOVE, SMTO_ABORTIFHUNG, SW_HIDE, SW_SHOWNORMAL,
    WM_GETICON, WM_QUIT, WM_TIMER,
};

use crate::audio::{self, AudioController};
use crate::icons::{self, IconBitmap, MASTER_PX};
use crate::launcher::arena::{Arena, Caps, Kind as ArenaKind};
use crate::launcher::catalog;
use crate::launcher::gesture::{is_marker, is_network_file, Gesture};
use crate::launcher::web::{check_authority, MAX_URL_BYTES};
use crate::log;
use crate::msg::{WM_DOCK_SHELL_READY, WM_SHELL_AUDIO_DEVICE_CHANGED, WM_SHELL_SPATIAL_CHANGED, WM_SHELL_WAKE};

// ------------------------------------------------------------------ public types

/// Request to resolve name+icon for an app entry.
#[derive(Clone, Debug)]
pub struct ResolveApp {
    /// Grouping key (and cache key).
    pub key: String,
    /// Exe path (original case). For a pin without windows: the `launch` if it is a path,
    /// otherwise an empty string.
    pub exe_path: String,
    /// AUMID, if known (window or package; for UWP pins: from `launch` `shell:AppsFolder\<AUMID>`).
    pub aumid: Option<String>,
    /// A window of the entry (as isize) for the `WM_GETICON` fallback; 0 if there is none.
    pub hwnd: isize,
    /// Final icon side in physical pixels (24 logical * dpi/96).
    pub icon_px: u32,
    /// `icon` field of the pin in the config (plain name inside `datadir::icons_dir()`), if present.
    pub custom_icon: Option<String>,
}

/// One app of the `shell:AppsFolder` catalog (classic programs and Store apps alike), for the Start
/// menu opened by the Win double tap (`popup_startmenu`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartApp {
    /// Parsing name inside AppsFolder (AUMID, `{KNOWNFOLDER}\path.exe`, ...): launched as
    /// `shell:AppsFolder\<id>`.
    pub id: String,
    /// Display name, as the native Start menu shows it.
    pub name: String,
}

/// Tools the Start menu can start elevated (via UAC, see `ShellRequest::LaunchAdmin`). A closed set:
/// the target is a constant resolved here (an absolute System32 path or an AUMID), never a bare exe
/// name and never a string from the config, pins or registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminTool {
    Terminal,
}

impl AdminTool {
    /// What Explorer is asked to run: the Store app Windows Terminal by its AUMID. `_system_dir` is
    /// for tools addressed by absolute System32 path (none at the moment).
    fn target(self, _system_dir: &std::path::Path) -> String {
        match self {
            AdminTool::Terminal => r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App".to_string(),
        }
    }
}

/// Verb for `IShellDispatch2::ShellExecute`. Closed on purpose: `RunAs` is built only by `launch_admin`,
/// `launch_app_admin` and `run_as_typed_with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Open,
    RunAs,
}

impl Verb {
    fn as_str(self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::RunAs => "runas",
        }
    }
}

/// UI -> shell thread requests.
#[derive(Clone, Debug)]
pub enum ShellRequest {
    /// Resolve name (then icon): produces `ShellEvent::AppName` and `ShellEvent::AppIcon`.
    ResolveApp(ResolveApp),
    /// Launch `target` (exe path, `shell:AppsFolder\...`, `ms-settings:...`) with optional `args`.
    /// When the dock is elevated it ALWAYS runs de-elevated through the desktop shell, or not at all.
    Launch { target: String, args: Option<String> },
    /// Start menu "(admin)" entries: asks the desktop shell (Explorer, medium integrity) to run a FIXED
    /// tool with the `runas` verb, so Windows shows its UAC prompt. The dock's own token is never used;
    /// without the desktop shell the launch is refused. Only this request, `LaunchAppAdmin` and
    /// `RunAsTyped` carry `runas`.
    LaunchAdmin(AdminTool),
    /// Start menu "Run as administrator" on an app of the catalog: the desktop shell runs
    /// `shell:AppsFolder\<id>` with the `runas` verb (UAC prompt, the dock's token never used), as
    /// the native Start menu does. `id` is an AppsFolder id the menu got from `StartApps`.
    LaunchAppAdmin { id: String },
    /// Start menu "Open file location": selects the target of the app `id` in a File Explorer
    /// window (de-elevated). Nothing for a Store app, which has no file to show.
    RevealApp { id: String },
    /// Change the volume by `delta_percent` points (wheel: +-`audio::WHEEL_STEP_PERCENT`).
    VolumeAdjust { delta_percent: i32 },
    /// Toggle mute.
    VolumeToggleMute,
    /// Re-read and republish the volume (`WM_DOCK_VOLUME`), e.g. after startup.
    VolumeRefresh,
    /// set the master volume to `percent` (0..=100) -> `WM_DOCK_VOLUME`. Popup slider.
    VolumeSet { percent: u8 },
    /// re-read the output devices -> `ShellEvent::AudioOutputs`.
    ListOutputs,
    /// make `id` the default output device (IPolicyConfig), then republish
    /// `ShellEvent::AudioOutputs` and the volume. On failure: log + `Launch("ms-settings:sound")`.
    SetDefaultOutput { id: String },
    /// Audio popup: read the spatial sound formats of the default output -> `ShellEvent::Spatial`.
    ListSpatial,
    /// Audio popup: make `subtype` (empty = Off) the default spatial format. Runs on a short-lived
    /// thread; when it ends `ShellEvent::Spatial` is pushed again (if the popup is still open).
    SetSpatialFormat { subtype: String },
    /// Audio popup: enumerate the per-app sessions of the default output -> `ShellEvent::AudioSessions`.
    ListSessions,
    /// Audio popup: set the level (0..=100) of all the sessions of one mixer row. No reply (the
    /// popup asks `ListSessions` when the drag ends).
    SetSessionVolume { pid: u32, system: bool, level: u8 },
    /// Audio popup: toggle the mute of all the sessions of one mixer row -> `ShellEvent::AudioSessions`.
    ToggleSessionMute { pid: u32, system: bool },
    /// "Show desktop" (`IShellDispatch4::ToggleDesktop`, fallback Win+D with `INJECT_TAG`).
    ToggleDesktop,
    /// "Start" button: synthetic Win press and release with `INJECT_TAG` (opens / closes the Start menu).
    OpenStart,
    /// Network module: query the active connection -> `ShellEvent::Network`. The first request
    /// creates the `net::Querier` (libraries / COM object loaded on demand, see `net`).
    NetworkQuery,
    /// Network module switched off: drop the `net::Querier` (frees everything it loaded).
    NetworkRelease,
    /// Volume module disabled: shuts the `AudioController` down and drops it.
    AudioRelease,
    /// Start menu (Win double tap) and the launcher: lists `shell:AppsFolder` (`list_apps_arena`: in an
    /// `--start-apps` child when `child`, in-process otherwise) -> `ShellEvent::StartApps`; then, as
    /// `icons` says, has the Start menu's icons made (see `IconPlan`). `child` is set by the launcher
    /// and, only while a launcher chord is set, by Start: with no chord Start lists in-process (M1).
    ListStartApps { icon_px: u32, icons: IconPlan, child: bool },
    /// One batch of Start menu icons made on this thread -> `ShellEvent::StartIcons`. Queued by
    /// `ListStartApps` only when its icons child cannot start or gave no header; low priority like
    /// `ResolveApp`.
    StartIcons { icon_px: u32, ids: Vec<String> },
    /// The Start menu closed: its icons child is killed and reaped, the icons sent are forgotten and
    /// the queued `StartIcons` batches dropped.
    StartRelease,
    /// Every trim (`mem::trim`): this STA's `CoFreeUnusedLibraries` pass. The proxy/stub DLLs a desktop-shell launch
    /// loads here are unloaded by a pass at least 10 minutes after the one that found them unused, so a trim after shell
    /// COM work and the second pass 11 minutes on (`mem::second_pass_tick`) unload them (M1 F4).
    ComFree,
    /// Sent by the thread of `start_search::load_settings` when the settings catalog is ready
    /// -> `ShellEvent::StartSettings` (this thread is the one that can wake the UI).
    StartSettingsReady,
    /// Click on a notification whose app has no open window: launches the app that sent it, found in
    /// the AppsFolder catalog by AppUserModelID or display name; Win+N if it cannot be found.
    OpenNotifApp { aumid: String, name: String },
    // ---- the launcher's requests (built by launcher_sys.rs only). Each launch whose shell call fails
    // pushes `ShellEvent::LaunchFailed` (one refused by the checks here first is only logged); the shared
    // requests above never do. Logged as kind and length only.
    /// Open a file or folder of a result row: the path as it is, in one desktop-shell call (no
    /// `updated_exe_path`, no `is_dir`, no working directory).
    OpenPath { path: String },
    /// Run as administrator a typed line or a file row's quoted path (Ctrl+Shift+Enter): `line` split
    /// at `file` (a byte range of it), run by the desktop shell with `runas` (Windows' UAC prompt; the
    /// dock's token is never used). Carries the hook-proven `Gesture`; built only by launcher_sys.rs
    /// (lint 1). The only launcher request with `runas`.
    RunAsTyped { line: String, file: Range<usize>, gesture: Gesture },
    /// Run a typed line (Enter) as it was typed: `line` split at `file`, unexpanded, no working
    /// directory. With `console` (a console program, or Shift+Enter) it runs as
    /// `cmd.exe /s /k "<line>"` (with /s cmd strips exactly that outer pair of quotes), `cmd.exe` from
    /// `system_dir`.
    LaunchCommand { line: String, file: Range<usize>, console: bool },
    /// Show a result's file or folder selected in File Explorer (`explorer.exe /select,"<path>"` from
    /// the Windows folder), in one shell call: the path is never looked at on disk.
    RevealPath { path: String },
    /// Open a web address (a typed URL or a web search): checked again here, http or https only, and
    /// rebuilt from its parts before it reaches the shell (`rebuild_url`).
    OpenUrl { url: String },
    /// `Launch` for the launcher (a catalogue app as `shell:AppsFolder\<id>`, a setting's target, the
    /// registry row): the same function, so the same updated-exe and folder logic, which exists for
    /// these targets only; nothing typed reaches it. Unlike `Launch`, a failure pushes `LaunchFailed`.
    LauncherLaunch { target: String, args: Option<String> },
    /// `LaunchAppAdmin` for the launcher: a failure pushes `LaunchFailed`.
    LauncherAppAdmin { id: String },
    /// `RevealApp` for the launcher: a failure pushes `LaunchFailed` (an app with no file to show is
    /// no failure).
    LauncherRevealApp { id: String },
    /// Icons of the launcher's visible rows that lack one -> `ShellEvent::LauncherIcons`, made by an
    /// `--start-icons` child (the launcher's job, which a newer request replaces). Not a low-priority
    /// request: it runs ahead of the Start menu's batches, and a newer one replaces the pending one. It
    /// never reads or changes what the Start menu was sent.
    StartIconsFor { icon_px: u32, ids: Vec<String> },
    /// The launcher closed after asking for icons: its icons child is killed and reaped, and a pending
    /// `StartIconsFor` is dropped (the next open asks again for the rows it shows).
    LauncherClosed,
    /// A query that found nothing: Windows' typo rewrite of it (`start_search::converted`, a 1.4 MB
    /// read kept off the UI thread) -> `ShellEvent::LauncherConverted`. A newer one replaces the
    /// pending one.
    LauncherConvert { gen: u64, query: String },
    /// The launcher's first open of a session: the shell user's SID and history files ->
    /// `ShellEvent::LauncherState`, read on this thread from that user's `cache\<SID>` only.
    LauncherStateLoad,
    /// Writes the launcher's history files (`Some` = this content, `None` = left as it is); `delete`
    /// removes both instead ("learn" off, Forget history).
    LauncherStateSave { frecency: Option<Vec<u8>>, run: Option<Vec<u8>>, delete: bool },
}

/// The icons a `ListStartApps` queues.
#[derive(Clone, Debug)]
pub enum IconPlan {
    /// The Start menu: the icons not sent yet at `icon_px` (the ids in `first`, the menu pins, go
    /// first), made by an `--start-icons` child (L3 T20); when it cannot start or gave no header, as
    /// `StartIcons` batches made here, which run behind every other request.
    All { first: Vec<String> },
    /// The launcher's catalogue: none, and what the Start menu was sent is left as it is (the
    /// launcher asks for its rows' icons with `StartIconsFor`).
    None,
}

/// Shell thread -> UI results (via queue + `WM_DOCK_SHELL_READY`).
#[derive(Clone, Debug)]
pub enum ShellEvent {
    /// Program name resolved for the `key` entry (config overrides already applied).
    AppName { key: String, name: String },
    /// Icon for the `key` entry, already scaled to `icon_px` x `icon_px`. `bitmap == None` = not found:
    /// the UI keeps the placeholder.
    AppIcon { key: String, icon_px: u32, bitmap: Option<IconBitmap> },
    /// active output devices (reply to `ListOutputs`/`SetDefaultOutput`, and
    /// spontaneously after `WM_SHELL_AUDIO_DEVICE_CHANGED`). Empty = none / error.
    AudioOutputs(Vec<audio::OutputDevice>),
    /// Spatial sound state of the default output (`None` = not supported). Reply to `ListSpatial`,
    /// and pushed after a format change or a device change while the audio popup is open.
    Spatial(Option<audio::SpatialState>),
    /// Per-app mixer rows of the default output (reply to `ListSessions` / `ToggleSessionMute`, and
    /// after a device change while the audio popup is open).
    AudioSessions(Vec<audio::AppSession>),
    /// Status of the active network connection (reply to `NetworkQuery`).
    Network(crate::net::NetInfo),
    /// The AppsFolder catalogue (reply to `ListStartApps`) as one checked arena, sorted by name: listed by an
    /// `--start-apps` child (in-process when the child fails). Start makes its rows from it, the launcher keeps it.
    StartApps(Arc<Arena>),
    /// Icons of Start menu apps at `icon_px` (`None` = the app has none: the menu keeps a placeholder).
    StartIcons { icon_px: u32, icons: Vec<(String, Option<IconBitmap>)> },
    /// The settings catalog of the Start menu search is ready (`start_search::settings`).
    StartSettings,
    /// A launcher request's shell call failed (Explorer not answering, or the shell refused it). Not
    /// pushed for a request the shell thread's own checks refused before any shell call (only logged),
    /// and never for the shared requests (`Launch`, `LaunchAppAdmin`, `RevealApp`).
    LaunchFailed { kind: LaunchKind },
    /// Icons of launcher rows at `icon_px` (reply to `StartIconsFor`; `None` = the app has none).
    LauncherIcons { icon_px: u32, icons: Vec<(String, Option<IconBitmap>)> },
    /// What Windows Search reads the query of generation `gen` as (reply to `LauncherConvert`);
    /// `None` = no rewrite.
    LauncherConverted { gen: u64, meant: Option<String> },
    /// The launcher's files pump (`launcher_files`, plan L2-F3), tagged with the number of the open it belongs to: rows
    /// of a generation, the busy mark, or why files do not answer. Pushed from the pump thread, never from this thread.
    LauncherFiles(u32, crate::launcher::files::FilesEvent),
    /// The shell user's SID and the launcher's history files (reply to `LauncherStateLoad`); `None`
    /// for what could not be found or read.
    #[allow(dead_code)] // `sid` is not read yet
    LauncherState { sid: Option<String>, frecency: Option<Vec<u8>>, run: Option<Vec<u8>> },
}

/// What a failed launcher launch was (`ShellEvent::LaunchFailed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    App,
    Path,
    Command,
    Admin,
    Url,
    Reveal,
}

/// Shell thread handle; passed to `stop`.
pub struct ShellHandle {
    join: Option<JoinHandle<()>>,
    /// Receives `()` when the thread exits: allows a join with timeout in `stop`.
    done: Receiver<()>,
    thread_id: u32,
}

// ------------------------------------------------------------------ global state

/// Request sender (set by `start`).
static REQ_TX: OnceLock<Sender<ShellRequest>> = OnceLock::new();
/// Shell thread id (0 = not started).
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// Result queue towards the UI.
static EVENTS: Mutex<Vec<ShellEvent>> = Mutex::new(Vec::new());

// ------------------------------------------------------------------ public API

/// Starts the shell thread (COM STA + message loop) and waits (max 2 s) for its message queue to
/// exist, so `request` cannot lose messages. `ui_hwnd` = window to post results to.
/// Called from the main thread after the window is created.
pub fn start(ui_hwnd: HWND) -> Result<ShellHandle, String> {
    let (tx, rx) = mpsc::channel::<ShellRequest>();
    let (ready_tx, ready_rx) = mpsc::channel::<u32>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let ui = ui_hwnd.0 as isize;
    let join = std::thread::Builder::new()
        .name("shell".into())
        .spawn(move || {
            thread_main(ui, rx, ready_tx);
            let _ = done_tx.send(());
        })
        .map_err(|e| format!("spawn shell thread: {e}"))?;
    let tid =
        ready_rx.recv_timeout(Duration::from_secs(2)).map_err(|_| "shell thread not ready within 2 s".to_string())?;
    REQ_TX.set(tx).map_err(|_| "shell_thread::start called twice".to_string())?;
    THREAD_ID.store(tid, Relaxed);
    Ok(ShellHandle { join: Some(join), done: done_rx, thread_id: tid })
}

/// Queues a request; does not block. If the thread is not running the request is dropped (and logged).
/// Callable from any thread.
pub fn request(req: ShellRequest) {
    let (Some(tx), tid) = (REQ_TX.get(), THREAD_ID.load(Relaxed)) else {
        log!("shell_thread::request: thread not started, request dropped");
        return;
    };
    if tx.send(req).is_err() || tid == 0 {
        log!("shell_thread::request: thread terminated, request dropped");
        return;
    }
    // SAFETY: PostThreadMessageW is thread-safe; if the thread no longer exists it just fails.
    let _ = unsafe { PostThreadMessageW(tid, WM_SHELL_WAKE, WPARAM(0), LPARAM(0)) };
}

/// Drains the result queue (non-blocking). To be called on the UI thread on receipt of
/// `WM_DOCK_SHELL_READY`.
pub fn drain_events() -> Vec<ShellEvent> {
    match EVENTS.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(p) => std::mem::take(&mut *p.into_inner()),
    }
}

/// Shell thread id (0 if not started). Used by the audio callbacks to post
/// `WM_SHELL_AUDIO_DEVICE_CHANGED`.
pub fn thread_id() -> u32 {
    THREAD_ID.load(Relaxed)
}

/// Stops the thread (WM_QUIT + join with a 2 s timeout). Idempotent.
pub fn stop(mut h: ShellHandle) {
    THREAD_ID.store(0, Relaxed);
    // SAFETY: thread-safe; if the thread has already terminated it just fails.
    let _ = unsafe { PostThreadMessageW(h.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
    if let Some(j) = h.join.take() {
        // `JoinHandle::join` has no timeout: it is done only after the exit signal. If the thread is
        // blocked in a COM/shell call we proceed without joining (process exit will close it).
        match h.done.recv_timeout(Duration::from_secs(2)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = j.join();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => log!("shell thread: no reply within 2 s, proceeding without join"),
        }
    }
}

pub(crate) fn push_event(ui: isize, ev: ShellEvent) {
    match EVENTS.lock() {
        Ok(mut g) => g.push(ev),
        Err(p) => p.into_inner().push(ev),
    }
    let hwnd = HWND(ui as *mut core::ffi::c_void);
    // SAFETY: PostMessageW is thread-safe; it only fails if the window no longer exists.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_DOCK_SHELL_READY, WPARAM(0), LPARAM(0)) };
}

// ------------------------------------------------------------------ constants

/// Period of the thread timer that runs only while an icons child exists: `reap_icon_jobs` (at the start of
/// every `pump`) is the only place that reaps an ended child and kills one past its deadline, and nothing
/// posts a message when a normal child ends. With no job there is no timer and no wake (a `WM_SHELL_WAKE`
/// swallowed by a nested COM loop leaves nothing behind: `pump` drains the channel after every message).
const SAFETY_TIMER_MS: u32 = 1000;

/// True while the thread needs its timer: a Start menu or a launcher icons child is running.
fn timer_wanted(start_job: bool, launcher_job: bool) -> bool {
    start_job || launcher_job
}
/// Side (px) requested from high-resolution icon sources.
const SRC_ICON_PX: i32 = 256;
/// `WM_GETICON` timeout towards other apps' windows.
const GETICON_TIMEOUT_MS: u32 = 50;
/// `PKEY_FileDescription` ({0CEF7D53-FA64-11D1-A203-0000F81FEDEE}, 3). Defined here to avoid
/// enabling the `Win32_Storage_EnhancedStorage` feature just for a constant.
const PKEY_FILE_DESCRIPTION: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x0cef7d53_fa64_11d1_a203_0000f81fedee), pid: 3 };

/// `PKEY_Software_ProductName` (same format id as FileDescription, 7).
const PKEY_PRODUCT_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x0cef7d53_fa64_11d1_a203_0000f81fedee), pid: 7 };
/// `PKEY_Company` ({D5CDD502-2E9C-101B-9397-08002B2CF9AE}, 15).
const PKEY_COMPANY: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xd5cdd502_2e9c_101b_9397_08002b2cf9ae), pid: 15 };

// ------------------------------------------------------------------ thread

/// State owned by the shell thread.
struct Shell {
    ui: isize,
    audio: Option<AudioController>,
    /// "Natural" name (without config override) per group key.
    names: HashMap<String, String>,
    /// Requests received but not yet executed (non-`ResolveApp` ones take precedence).
    pending: VecDeque<ShellRequest>,
    /// Network module state: `None` until the first `NetworkQuery` and after `NetworkRelease`.
    net: Option<crate::net::Querier>,
    /// Start menu icons: the size they are made at and the ids already sent or queued at that size
    /// (a new size starts over).
    start_px: u32,
    start_sent: HashSet<String>,
    /// The `--start-icons` child at work for the Start menu and the launcher's, each while it runs (L3 T20).
    start_job: Option<IconJob>,
    launcher_job: Option<IconJob>,
    /// An icons child of this process gave no header: every later batch is made here, as before T20.
    icons_child_off: bool,
}

fn thread_main(ui: isize, rx: Receiver<ShellRequest>, ready: Sender<u32>) {
    // SAFETY: initializes COM STA for this thread; paired with CoUninitialize at the end.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        log!("shell thread: CoInitializeEx failed: {hr:?}");
    }
    // Force creation of the message queue BEFORE announcing the thread id.
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid and writable.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: no preconditions.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = ready.send(tid);
    log!("shell thread started (COM STA)");

    let mut shell = Shell {
        ui,
        audio: None,
        names: HashMap::new(),
        pending: VecDeque::new(),
        net: None,
        start_px: 0,
        start_sent: HashSet::new(),
        start_job: None,
        launcher_job: None,
        icons_child_off: false,
    };

    // The thread timer (null hwnd, system-chosen id, no callback: it arrives as WM_TIMER in the thread queue)
    // exists only while an icons child does; 0 = none. Cancelled at the end.
    let mut timer: usize = 0;
    #[cfg(feature = "wake-probe")]
    let probe_hook = crate::probe::install_send_hook(crate::probe::Th::Shell);

    loop {
        // SAFETY: `msg` is valid and writable; filter None = all thread messages.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT or error
        }
        probe!(crate::probe::thread_msg(crate::probe::Th::Shell, &msg));
        match msg.message {
            WM_SHELL_WAKE => {}
            WM_TIMER if msg.hwnd.0.is_null() => {}
            WM_SHELL_AUDIO_DEVICE_CHANGED => shell.on_audio_device_changed(),
            WM_SHELL_SPATIAL_CHANGED => {
                if audio::popup_wants_spatial() {
                    shell.push_spatial();
                }
            }
            _ => {
                // SAFETY: `msg` was just filled by GetMessageW; needed by COM STA for its internal messages.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        shell.pump(&rx);
        let wanted = timer_wanted(shell.start_job.is_some(), shell.launcher_job.is_some());
        if wanted && timer == 0 {
            // SAFETY: thread timer on this thread, see above.
            timer = unsafe { SetTimer(None, 0, SAFETY_TIMER_MS, None) };
        } else if !wanted && timer != 0 {
            // SAFETY: `timer` is the id returned by SetTimer on this thread.
            let _ = unsafe { KillTimer(None, timer) };
            timer = 0;
        }
    }
    #[cfg(feature = "wake-probe")]
    crate::probe::remove_send_hook(probe_hook);
    if timer != 0 {
        // SAFETY: `timer` is the id returned by SetTimer on this thread.
        let _ = unsafe { KillTimer(None, timer) };
    }
    // No icons child outlives the thread that started it.
    let _ = shell.end_job(IconClient::Start);
    let _ = shell.end_job(IconClient::Launcher);
    // COM objects (audio callbacks, network list manager) must be released before CoUninitialize.
    shell.net = None;
    if let Some(a) = shell.audio.take() {
        a.shutdown();
    }
    if hr.is_ok() {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    log!("shell thread terminated");
}

impl Shell {
    /// Creates the `AudioController` if it does not exist yet (retries on every volume request).
    /// Does nothing while the volume module is off: no COM object, no callbacks.
    fn ensure_audio(&mut self) {
        if self.audio.is_some() || !crate::config::get().show_volume {
            return;
        }
        let hwnd = HWND(self.ui as *mut c_void);
        match AudioController::new(hwnd) {
            Ok(a) => self.audio = Some(a),
            Err(e) => log!("shell thread: AudioController::new failed: {e}"),
        }
    }

    /// Publishes the volume state to the UI (or "no device" if the controller is missing).
    fn publish_volume(&self, st: Option<audio::VolumeState>) {
        audio::post_volume(self.ui, st.unwrap_or_default());
    }

    fn on_audio_device_changed(&mut self) {
        self.ensure_audio();
        let st = self.audio.as_mut().map(|a| a.reopen_endpoint());
        self.publish_volume(st);
        // The device list, the spatial formats and the sessions belong to the default device:
        // re-read each one only while the audio popup shows it (open popup, section expanded and
        // visible: `audio::set_popup_wants`). A collapsed section enumerates nothing.
        if audio::popup_wants_outputs() {
            let list = self.audio.as_ref().map(|a| a.list_outputs()).unwrap_or_default();
            push_event(self.ui, ShellEvent::AudioOutputs(list));
        }
        if audio::popup_wants_spatial() {
            self.push_spatial();
        }
        if audio::popup_wants_sessions() {
            self.push_sessions();
        }
    }

    /// Reads the spatial sound state now (nothing kept) and pushes it to the UI.
    fn push_spatial(&mut self) {
        self.ensure_audio();
        let st = self.audio.as_ref().and_then(|a| a.spatial());
        push_event(self.ui, ShellEvent::Spatial(st));
    }

    /// Enumerates the per-app sessions now (no COM object kept), resolves the row names and pushes
    /// them to the UI.
    fn push_sessions(&mut self) {
        self.ensure_audio();
        let raw = self.audio.as_ref().map(|a| a.list_sessions()).unwrap_or_default();
        let mut rows = audio::group_sessions(&raw);
        for r in rows.iter_mut() {
            if r.system {
                r.name = crate::i18n::tr("System sounds", "Suoni di sistema").to_string();
            } else if r.name.is_empty() {
                r.name = session_app_name(r.pid);
            }
        }
        push_event(self.ui, ShellEvent::AudioSessions(rows));
    }

    /// Reaps the icons jobs that ended or hung (`reap_icon_jobs`), then drains the mpsc queue and works
    /// through the requests: volume/launch first, then the (slow) resolutions and Start menu icon batches,
    /// re-checking the queue after each request.
    fn pump(&mut self, rx: &Receiver<ShellRequest>) {
        self.reap_icon_jobs();
        loop {
            while let Ok(r) = rx.try_recv() {
                enqueue(&mut self.pending, r);
            }
            let Some(req) = next_request(&mut self.pending) else { break };
            let com = loads_com(&req);
            self.handle_request(req);
            if com {
                COM_WORK.store(true, SeqCst);
            }
        }
    }

    fn handle_request(&mut self, req: ShellRequest) {
        match req {
            ShellRequest::ResolveApp(r) => self.resolve_app(r),
            ShellRequest::Launch { target, args } => {
                if let Err(e) = launch(&target, args.as_deref()) {
                    log!("shell: Launch '{target}' not executed: {e}");
                }
            }
            ShellRequest::LaunchAdmin(tool) => launch_admin(tool),
            ShellRequest::LaunchAppAdmin { id } => {
                let _ = launch_app_admin(&id, LaunchBy::Shared);
            }
            ShellRequest::RevealApp { id } => {
                let _ = reveal_app(&id);
            }
            // launcher arms: begin
            req @ (ShellRequest::OpenPath { .. }
            | ShellRequest::RunAsTyped { .. }
            | ShellRequest::LaunchCommand { .. }
            | ShellRequest::RevealPath { .. }
            | ShellRequest::OpenUrl { .. }
            | ShellRequest::LauncherLaunch { .. }
            | ShellRequest::LauncherAppAdmin { .. }
            | ShellRequest::LauncherRevealApp { .. }) => launcher_launch(self.ui, req),
            ShellRequest::StartIconsFor { icon_px, ids } => {
                let px = icon_px.clamp(*ICON_PX_RANGE.start(), *ICON_PX_RANGE.end());
                if let Err(back) = self.start_icon_job(IconClient::Launcher, px, ids) {
                    launcher_icons(self.ui, px, back);
                }
            }
            ShellRequest::LauncherClosed => {
                let _ = self.end_job(IconClient::Launcher);
            }
            ShellRequest::LauncherConvert { gen, query } => launcher_convert(self.ui, gen, &query),
            ShellRequest::LauncherStateLoad => launcher_state_load(self.ui),
            ShellRequest::LauncherStateSave { frecency, run, delete } => launcher_state_save(frecency, run, delete),
            // launcher arms: end
            ShellRequest::VolumeAdjust { delta_percent } => {
                self.ensure_audio();
                let st = self.audio.as_mut().map(|a| a.adjust(delta_percent));
                self.publish_volume(st);
            }
            ShellRequest::VolumeToggleMute => {
                self.ensure_audio();
                let st = self.audio.as_mut().map(|a| a.toggle_mute());
                self.publish_volume(st);
            }
            ShellRequest::VolumeRefresh => {
                self.ensure_audio();
                let st = self.audio.as_mut().map(|a| a.refresh());
                self.publish_volume(st);
            }
            ShellRequest::VolumeSet { percent } => {
                self.ensure_audio();
                let st = self.audio.as_mut().map(|a| a.set_level(percent));
                self.publish_volume(st);
            }
            ShellRequest::ListOutputs => {
                self.ensure_audio();
                let list = self.audio.as_ref().map(|a| a.list_outputs()).unwrap_or_default();
                push_event(self.ui, ShellEvent::AudioOutputs(list));
            }
            ShellRequest::SetDefaultOutput { id } => {
                self.ensure_audio();
                if let Some(Err(e)) = self.audio.as_mut().map(|a| a.set_default_output(&id)) {
                    log!("shell: SetDefaultOutput failed: {e}, opening ms-settings:sound");
                    if let Err(e) = launch("ms-settings:sound", None) {
                        log!("shell: Launch 'ms-settings:sound' not executed: {e}");
                    }
                }
                let list = self.audio.as_ref().map(|a| a.list_outputs()).unwrap_or_default();
                push_event(self.ui, ShellEvent::AudioOutputs(list));
            }
            ShellRequest::ListSpatial => self.push_spatial(),
            ShellRequest::SetSpatialFormat { subtype } => {
                self.ensure_audio();
                match self.audio.as_ref().and_then(|a| a.default_winrt_id()) {
                    Some(id) => audio::spawn_set_spatial(id, subtype),
                    None => log!("shell: SetSpatialFormat: no default output device"),
                }
            }
            ShellRequest::ListSessions => self.push_sessions(),
            ShellRequest::SetSessionVolume { pid, system, level } => {
                self.ensure_audio();
                if let Some(a) = self.audio.as_ref() {
                    a.set_session_volume(pid, system, level);
                }
            }
            ShellRequest::ToggleSessionMute { pid, system } => {
                self.ensure_audio();
                if let Some(a) = self.audio.as_ref() {
                    a.toggle_session_mute(pid, system);
                }
                self.push_sessions();
            }
            ShellRequest::ToggleDesktop => toggle_desktop(),
            ShellRequest::OpenStart => send_win_alone(),
            ShellRequest::OpenNotifApp { aumid, name } => open_notif_app(&aumid, &name),
            ShellRequest::NetworkQuery => {
                let info = self.net.get_or_insert_with(crate::net::Querier::new).query();
                push_event(self.ui, ShellEvent::Network(info));
            }
            ShellRequest::NetworkRelease => self.net = None,
            ShellRequest::AudioRelease => {
                if let Some(a) = self.audio.take() {
                    a.shutdown();
                }
            }
            ShellRequest::ListStartApps { icon_px, icons, child } => self.list_start_apps(icon_px, &icons, child),
            ShellRequest::StartSettingsReady => push_event(self.ui, ShellEvent::StartSettings),
            ShellRequest::StartRelease => {
                let _ = self.end_job(IconClient::Start);
                start_release(&mut self.start_px, &mut self.start_sent, &mut self.pending);
                crate::start_search::release_app_index();
                // The icon pipeline and the AppsFolder catalog are done: this STA's COM servers with no object left are
                // unloaded at once (M1 F4). Nothing kept points into them: icons and strings were copied.
                // SAFETY: plain call on a COM-initialised thread.
                unsafe { CoFreeUnusedLibraries() };
            }
            // SAFETY: plain call on this COM-initialised thread; only DLLs whose DllCanUnloadNow says so are unloaded.
            ShellRequest::ComFree => unsafe { CoFreeUnusedLibraries() },
            ShellRequest::StartIcons { icon_px, ids } => {
                // A batch queued for a previous size: its ids were forgotten with that size.
                if icon_px != self.start_px {
                    return;
                }
                let icons = ids
                    .into_iter()
                    .map(|id| {
                        let b = start_icon(&id, icon_px);
                        (id, b)
                    })
                    .collect();
                push_event(self.ui, ShellEvent::StartIcons { icon_px, icons });
            }
        }
    }

    // -------------------------------------------------------------- Start menu

    /// `ListStartApps`: lists the catalogue (`list_apps_arena`), publishes it, then has the icons the Start menu does
    /// not have yet made (`start_menu_icons`). One listing serves Start and the launcher.
    fn list_start_apps(&mut self, icon_px: u32, icons: &IconPlan, child: bool) {
        let apps = Arc::new(list_apps_arena(child));
        let v = apps.view();
        let ids = (0..v.len()).map(|i| v.id(i));
        let missing =
            missing_start_icons(&mut self.start_px, &mut self.start_sent, &mut self.pending, ids, icon_px, icons);
        push_event(self.ui, ShellEvent::StartApps(apps));
        if let Some(missing) = missing {
            self.start_menu_icons(missing);
        }
    }

    // -------------------------------------------------------------- name and icon

    fn resolve_app(&mut self, mut r: ResolveApp) {
        // A pin whose exe is gone because the app updated itself into a new versioned folder:
        // name, icon and the stored launch target follow it.
        if let Some(now) = crate::apps::updated_exe_path(&r.exe_path) {
            log!("shell: '{}' moved to '{now}'", r.exe_path);
            if let Err(e) = crate::config::sync_pinned_launch(&r.key, &now) {
                log!("shell: cannot update the launch target of '{}': {e}", r.key);
            }
            r.exe_path = now;
        }
        let (name, overridden) = self.resolve_name(&r);
        // Self-heal: a pin's stored name is only a fallback; keep it in sync with the live resolver
        // (an explicit `names` override is the user's choice, never written into the pin).
        if !overridden {
            if let Err(e) = crate::config::sync_pinned_name(&r.key, &name) {
                log!("shell: cannot update the stored name of '{}': {e}", r.key);
            }
        }
        push_event(self.ui, ShellEvent::AppName { key: r.key.clone(), name });
        let bitmap = resolve_icon(&r);
        if bitmap.is_none() {
            log!("shell: no icon for '{}'", r.key);
        }
        push_event(self.ui, ShellEvent::AppIcon { key: r.key, icon_px: r.icon_px, bitmap });
    }

    /// Config override, then in-memory cache, then resolution (AppsFolder, FileDescription, stem).
    /// The flag is true when the name comes from a `names` override.
    fn resolve_name(&mut self, r: &ResolveApp) -> (String, bool) {
        if let Some(o) = override_name(&crate::config::get().names, r) {
            return (o, true);
        }
        if let Some(n) = self.names.get(&r.key) {
            return (n.clone(), false);
        }
        let (n, step) = natural_name(r);
        log!("shell: name for '{}' = '{n}' ({step})", r.key);
        self.names.insert(r.key.clone(), n.clone());
        (n, false)
    }
}

// ------------------------------------------------------------------ program name

fn exe_file_name(exe_path: &str) -> Option<String> {
    Path::new(exe_path).file_name().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// true if `exe_path` points to an executable usable for name/icon (not empty and not the
/// UWP window container, which would give the wrong identity).
fn is_usable_exe(exe_path: &str) -> bool {
    match exe_file_name(exe_path) {
        Some(n) => !n.eq_ignore_ascii_case("applicationframehost.exe"),
        None => false,
    }
}

/// Override from `config.names`: key = AUMID, exe file name (lowercase) or group key,
/// case-insensitive comparison.
fn override_name(names: &std::collections::BTreeMap<String, String>, r: &ResolveApp) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let find = |k: &str| -> Option<String> {
        if k.is_empty() {
            return None;
        }
        names
            .iter()
            .find(|(nk, v)| nk.eq_ignore_ascii_case(k) && !v.trim().is_empty())
            .map(|(_, v)| v.trim().to_string())
    };
    r.aumid
        .as_deref()
        .and_then(find)
        .or_else(|| exe_file_name(&r.exe_path).and_then(|n| find(&n)))
        .or_else(|| find(&r.key))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App" -> "WindowsTerminal".
fn name_from_aumid(aumid: &str) -> String {
    let base = aumid.split(['!', '_']).next().unwrap_or(aumid);
    capitalize(base.rsplit('.').next().unwrap_or(base))
}

/// Program name without override, in this order: (b) AppsFolder catalog (packaged / explicit AUMID),
/// (c) Start Menu shortcut name for the exe, (d) smart choice between FileDescription and ProductName,
/// (e) prettified exe stem, then (last resort) AUMID piece or key. Also says which step produced it.
fn natural_name(r: &ResolveApp) -> (String, &'static str) {
    let aumid = r.aumid.as_deref().map(str::trim).filter(|a| !a.is_empty());
    if let Some(a) = aumid {
        if let Some(n) = appsfolder_name(a) {
            return (n, "aumid");
        }
    }
    if is_usable_exe(&r.exe_path) {
        if let Some(n) = start_menu_name(&r.exe_path) {
            return (n, "shortcut");
        }
        let stem = Path::new(&r.exe_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let info = FileInfo::read(&r.exe_path);
        // Windows' own programs have localized descriptions ("Esplora risorse") that share no word with the stem.
        if in_windows_dir(&r.exe_path) {
            if let Some(d) = info.description.as_deref().map(strip_generic).filter(|d| !d.is_empty()) {
                return (d, "description");
            }
        }
        if let Some(n) =
            pick_description(info.description.as_deref(), info.product.as_deref(), info.company.as_deref(), &stem)
        {
            return (n, "description");
        }
        let pretty = pretty_stem(&stem);
        if !pretty.is_empty() {
            return (pretty, "stem");
        }
    }
    if let Some(a) = aumid {
        let n = name_from_aumid(a);
        if !n.is_empty() {
            return (n, "aumid-id");
        }
    }
    if let Some(stem) = Path::new(&r.exe_path).file_stem().map(|s| s.to_string_lossy().into_owned()) {
        if !stem.is_empty() {
            return (capitalize(&stem), "stem");
        }
    }
    (r.key.clone(), "key")
}

/// Name of a mixer row without a usable session display name: the exe FileDescription (generic
/// tails stripped), else the prettified exe stem, else "PID n". Nothing is cached.
fn session_app_name(pid: u32) -> String {
    let Some(path) = crate::apps::exe_path_of_pid(pid) else { return format!("PID {pid}") };
    if let Some(d) = FileInfo::read(&path).description.as_deref().map(strip_generic).filter(|d| !d.is_empty()) {
        return d;
    }
    let stem = Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let pretty = pretty_stem(&stem);
    if pretty.is_empty() {
        format!("PID {pid}")
    } else {
        pretty
    }
}

/// True for executables under `%SystemRoot%`.
fn in_windows_dir(exe_path: &str) -> bool {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    norm_path(exe_path).starts_with(&format!("{}\\", norm_path(&root).trim_end_matches('\\')))
}

// ------------------------------------------------------------------ name heuristics (pure)

/// Words of `s`, case preserved: splits on non-alphanumerics and camelCase boundaries
/// (`RazerAppEngine` -> Razer App Engine, `HTTPServer` -> HTTP Server).
fn split_words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(cur.drain(..).collect());
            }
            continue;
        }
        if let Some(&prev) = cur.last() {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary =
                (prev.is_lowercase() && c.is_uppercase()) || (prev.is_uppercase() && c.is_uppercase() && next_lower);
            if boundary {
                out.push(cur.drain(..).collect());
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur.into_iter().collect());
    }
    out
}

fn tokens(s: &str) -> Vec<String> {
    split_words(s).into_iter().map(|w| w.to_lowercase()).collect()
}

/// Corporate suffixes ignored when matching the company name.
const COMPANY_NOISE: [&str; 10] =
    ["inc", "corporation", "corp", "ltd", "llc", "co", "gmbh", "software", "the", "limited"];

/// True if some token of `cand` equals (or, for tokens of 4+ chars, is a prefix of / prefixed by) a token of `refs`.
fn shares_token(cand: &[String], refs: &[String]) -> bool {
    cand.iter().any(|c| {
        refs.iter().any(|r| {
            c == r || (c.len() >= 4 && r.len() >= 4 && (r.starts_with(c.as_str()) || c.starts_with(r.as_str())))
        })
    })
}

/// Generic trailing words removed from a name (only while something remains).
const GENERIC_TAIL: [&str; 10] =
    ["client", "bootstrapper", "webhelper", "helper", "host", "launcher", "application", "app", "service", "desktop"];

fn strip_generic(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    while words.len() > 1 && words.last().is_some_and(|w| GENERIC_TAIL.contains(&w.to_lowercase().as_str())) {
        words.pop();
    }
    words.join(" ")
}

/// `PowerToys.QuickAccess`-style identifier: contains a dot between alphanumerics and no spaces.
fn is_dotted_identifier(s: &str) -> bool {
    !s.contains(char::is_whitespace) && s.contains('.')
}

/// True if `a` is a whole-word prefix of `b` (case-insensitive): "Steam" of "Steam Client".
fn word_prefix(a: &str, b: &str) -> bool {
    let (ta, tb) = (tokens(a), tokens(b));
    !ta.is_empty() && ta.len() < tb.len() && tb[..ta.len()] == ta[..]
}

/// Smart choice between FileDescription and ProductName: a candidate sharing no word with the other
/// sources (exe stem, the other candidate, company) is rejected (taglines like "Freedom to Stream"),
/// as are dotted identifiers. Generic tails are stripped; the shorter of two nested names wins.
fn pick_description(desc: Option<&str>, product: Option<&str>, company: Option<&str>, stem: &str) -> Option<String> {
    let company_tokens: Vec<String> =
        company.map(tokens).unwrap_or_default().into_iter().filter(|t| !COMPANY_NOISE.contains(&t.as_str())).collect();
    let valid = |c: Option<&str>, other: Option<&str>, with_company: bool| -> Option<String> {
        let c = c.map(str::trim).filter(|c| !c.is_empty() && !is_dotted_identifier(c))?;
        let mut refs = tokens(stem);
        refs.extend(other.map(tokens).unwrap_or_default());
        if with_company {
            refs.extend(company_tokens.iter().cloned());
        }
        if !shares_token(&tokens(c), &refs) {
            return None;
        }
        let s = strip_generic(c);
        Some(s).filter(|s| !s.is_empty())
    };
    let d = valid(desc, product, true);
    let p = valid(product, desc, false);
    match (d, p) {
        (Some(d), Some(p)) => Some(if word_prefix(&p, &d) { p } else { d }),
        (Some(d), None) => Some(d),
        (None, p) => p,
    }
}

/// Words dropped when prettifying an exe stem.
const STEM_NOISE: [&str; 9] = ["shell", "ng", "x64", "x86", "win64", "win32", "helper", "app", "exe"];

/// `stremio-shell-ng` -> "Stremio"; `RazerAppEngine` -> "Razer App Engine"; `zen` -> "Zen".
fn pretty_stem(stem: &str) -> String {
    let words = split_words(stem);
    let kept: Vec<&String> = words.iter().filter(|w| !STEM_NOISE.contains(&w.to_lowercase().as_str())).collect();
    let chosen: Vec<&String> = if kept.is_empty() { words.iter().collect() } else { kept };
    chosen
        .iter()
        .map(|w| if w.chars().any(char::is_uppercase) { (*w).clone() } else { capitalize(w) })
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------ Start Menu shortcut index

/// Refresh a missed lookup only if the index is older than this.
const INDEX_MAX_AGE: Duration = Duration::from_secs(600);
/// Maximum folder depth scanned under each Start Menu root.
const INDEX_MAX_DEPTH: u32 = 6;
/// Most `.lnk` files indexed under each Start Menu root.
const INDEX_MAX_FILES: usize = 4096;

/// A `.lnk` of the Start Menu index. `per_user`: under the user's own Programs folder, which any
/// program of the user can write, so its bytes are parsed in Rust (`jumplist::read_lnk`) and never
/// reach the shell link loader; the all-users folder is writable by administrators only.
///
/// RESIDUAL (accepted for 1.5.1): `enum_apps_folder` (and the `--start-apps` child, `APPS_FLAG`, as
/// elevated as the dock), `appsfolder_name`, `app_target_path` and the AppsFolder icon factories
/// (`start_icon`, `icon_from_appsfolder`) still have the shell resolve per-user shortcuts in an
/// elevated process; a subfolder of the all-users Start menu that an installer left writable is
/// treated as admin-writable.
struct StartLnk {
    path: std::path::PathBuf,
    per_user: bool,
}

/// Per-user shortcuts refused by a COM wrapper (`com_guard`) of the index or the Start import; the
/// routing (`index_links`, `start_import::target_for`) never sends one there, so it stays 0.
pub(crate) static PER_USER_VIA_COM: AtomicUsize = AtomicUsize::new(0);

/// The fail-closed guard at the top of every COM wrapper that loads a Start menu shortcut: true
/// (refused, counted in `PER_USER_VIA_COM`) for a per-user one.
pub(crate) fn com_guard(per_user: bool) -> bool {
    if per_user {
        PER_USER_VIA_COM.fetch_add(1, Relaxed);
    }
    per_user
}

/// Counts of one index build, for its log line.
#[derive(Debug, Default, PartialEq, Eq)]
struct IndexStats {
    /// Per-user links whose target the Rust parser read, or could not read.
    rust_parsed: usize,
    rust_unresolved: usize,
    /// All-users links given to the shell link loader.
    com_common: usize,
}

#[derive(Default)]
struct StartIndex {
    /// lowercase exe path -> shortcut name (ambiguous targets are not stored).
    map: HashMap<String, String>,
    built: Option<Instant>,
}

thread_local! {
    static START_INDEX: RefCell<StartIndex> = RefCell::new(StartIndex::default());
}

fn norm_path(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

/// Name of the Start Menu shortcut whose target is `exe_path`, if any. Builds the index lazily, once,
/// and rebuilds it on a miss when it is older than ten minutes.
fn start_menu_name(exe_path: &str) -> Option<String> {
    let key = norm_path(exe_path);
    let stale = START_INDEX.with(|i| i.borrow().built.is_none_or(|t| t.elapsed() >= INDEX_MAX_AGE));
    let hit = |key: &str| START_INDEX.with(|i| i.borrow().map.get(key).cloned());
    if !stale {
        return hit(&key);
    }
    if let Some(n) = hit(&key) {
        return Some(n);
    }
    let map = build_start_index();
    START_INDEX.with(|i| *i.borrow_mut() = StartIndex { map, built: Some(Instant::now()) });
    hit(&key)
}

/// `Name (Preview)` / `Name (x64)` -> `Name`.
fn strip_paren_suffix(name: &str) -> String {
    let t = name.trim();
    if t.ends_with(')') {
        if let Some(i) = t.rfind(" (") {
            let head = t[..i].trim_end();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    t.to_string()
}

fn known_folder(id: &GUID) -> Option<std::path::PathBuf> {
    // SAFETY: COM initialized; the returned PWSTR is freed by `take_pwstr`.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    take_pwstr(p).map(std::path::PathBuf::from)
}

/// The `.lnk` files under `dir`, at most `INDEX_MAX_DEPTH` folders deep and `*left` files in all.
fn collect_lnk(dir: &Path, depth: u32, per_user: bool, left: &mut usize, out: &mut Vec<StartLnk>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *left == 0 {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < INDEX_MAX_DEPTH {
                collect_lnk(&p, depth + 1, per_user, left, out);
            }
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            *left -= 1;
            out.push(StartLnk { path: p, per_user });
        }
    }
}

/// Resolves the target of a .lnk without UI and without searching (`IPersistFile::Load` + `GetPath`).
fn lnk_target(link: &IShellLinkW, file: &IPersistFile, lnk: &Path) -> Option<String> {
    let wide = HSTRING::from(lnk.as_os_str());
    // SAFETY: COM STA initialized; `wide` is NUL-terminated and lives for the call.
    unsafe { file.Load(PCWSTR(wide.as_ptr()), STGM_READ) }.ok()?;
    let mut buf = [0u16; 520];
    let mut fd = WIN32_FIND_DATAW::default();
    // SAFETY: `buf` and `fd` are writable and sized as declared; flags 0 = long path.
    unsafe { link.GetPath(&mut buf, &mut fd, 0) }.ok()?;
    // Shortcuts with arguments ("explorer.exe <folder>", "java -jar ...") name something other than the exe.
    let mut args = [0u16; 8];
    // SAFETY: `args` is writable with the declared length; a longer string just fails (treated as "has arguments").
    if unsafe { link.GetArguments(&mut args) }.is_err() || args[0] != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    Some(s).filter(|s| !s.is_empty())
}

/// Display name of a `.lnk` as the shell shows it: localized, without the extension.
fn lnk_display_name(lnk: &Path) -> Option<String> {
    let wide = HSTRING::from(lnk.as_os_str());
    // SAFETY: COM STA initialized on this thread; `wide` lives for the call.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&wide, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; the returned PWSTR is freed by `take_pwstr`.
    let name = take_pwstr(unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.ok()?)?;
    let name = name.trim();
    let name = if name.len() > 4 && name.to_lowercase().ends_with(".lnk") { &name[..name.len() - 4] } else { name };
    Some(name.to_string()).filter(|n| !n.is_empty())
}

/// Scans the common and per-user Start Menu `.lnk` files; keeps only `exe path -> name` pairs.
/// The all-users links go to the shell (target and localized name), the per-user ones to the Rust
/// parser (target; the name is the file's), see `StartLnk`.
fn build_start_index() -> HashMap<String, String> {
    let mut files = Vec::new();
    for (id, per_user) in [(FOLDERID_CommonPrograms, false), (FOLDERID_Programs, true)] {
        if let Some(dir) = known_folder(&id) {
            let mut left = INDEX_MAX_FILES;
            collect_lnk(&dir, 0, per_user, &mut left, &mut files);
        }
    }
    // SAFETY: COM STA initialized on this thread; ShellLink CLSID with the requested interface.
    let Ok(link) = (unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }) else {
        return HashMap::new();
    };
    let Ok(file) = link.cast::<IPersistFile>() else { return HashMap::new() };
    let mut rust_target = |l: &StartLnk| {
        // Rejected with arguments, as `lnk_target` does.
        let target = expand_env(&crate::jumplist::read_lnk(&l.path).filter(|f| !f.has_args)?.target);
        crate::jumplist::is_local_file_path(&target).then_some(target)
    };
    let mut com_target = |l: &StartLnk| {
        let refused = com_guard(l.per_user);
        debug_assert!(!refused, "a per-user shortcut reached the shell link loader");
        if refused {
            None
        } else {
            lnk_target(&link, &file, &l.path)
        }
    };
    let mut com_display = |l: &StartLnk| {
        let refused = com_guard(l.per_user);
        debug_assert!(!refused, "a per-user shortcut reached the shell's display name");
        if refused {
            None
        } else {
            lnk_display_name(&l.path)
        }
    };
    let (mut map, stats) = index_links(&files, &mut rust_target, &mut com_target, &mut com_display);
    map.shrink_to_fit();
    log!(
        "shell: Start Menu index: {} shortcuts scanned, {} exe targets, per-user parsed in Rust: {}, unresolved: {}, per-user via COM: {}",
        files.len(),
        map.len(),
        stats.rust_parsed,
        stats.rust_unresolved,
        PER_USER_VIA_COM.load(Relaxed)
    );
    map
}

/// The loop of `build_start_index` (pure): `exe path -> name` pairs of `files`, ambiguous targets
/// dropped. A per-user link is read by `rust_target` only and named after its file; an all-users
/// one by `com_target` and `com_display`.
fn index_links(
    files: &[StartLnk],
    rust_target: &mut dyn FnMut(&StartLnk) -> Option<String>,
    com_target: &mut dyn FnMut(&StartLnk) -> Option<String>,
    com_display: &mut dyn FnMut(&StartLnk) -> Option<String>,
) -> (HashMap<String, String>, IndexStats) {
    let mut stats = IndexStats::default();
    let mut map: HashMap<String, String> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for lnk in files {
        let target = if lnk.per_user {
            let t = rust_target(lnk);
            if t.is_some() {
                stats.rust_parsed += 1;
            } else {
                stats.rust_unresolved += 1;
            }
            t
        } else {
            stats.com_common += 1;
            com_target(lnk)
        };
        let Some(target) = target else { continue };
        let target = if target.contains('%') { expand_env(&target) } else { target };
        if !target.to_lowercase().ends_with(".exe") {
            continue;
        }
        let Some(stem) = lnk.path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let lower = stem.to_lowercase();
        if lower.contains("uninstall") || lower.contains("readme") {
            continue;
        }
        // The name the shell shows (Windows' own shortcuts are localized through `desktop.ini`:
        // "Remote Desktop Connection.lnk" is "Connessione Desktop remoto" in Italian, as in the
        // Start menu), else the file name; per-user links always use the file name.
        let shown = if lnk.per_user { None } else { com_display(lnk) };
        let name = strip_paren_suffix(&shown.unwrap_or(stem));
        if name.is_empty() {
            continue;
        }
        let key = norm_path(&target);
        match map.get(&key) {
            Some(old) if !old.eq_ignore_ascii_case(&name) => ambiguous.push(key),
            Some(_) => {}
            None => {
                map.insert(key, name);
            }
        }
    }
    for k in ambiguous {
        map.remove(&k);
    }
    (map, stats)
}

/// `%NAME%` variables of the process environment expanded; unknown ones are kept as written.
pub(crate) fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut parts = s.split('%');
    out.push_str(parts.next().unwrap_or(""));
    while let Some(var) = parts.next() {
        match (parts.next(), std::env::var(var)) {
            (Some(rest), Ok(v)) => {
                out.push_str(&v);
                out.push_str(rest);
            }
            // Unknown variable or unbalanced '%': keep the text as is.
            (Some(rest), Err(_)) => {
                out.push('%');
                out.push_str(var);
                out.push('%');
                out.push_str(rest);
            }
            (None, _) => {
                out.push('%');
                out.push_str(var);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ --list-apps

/// `dock --list-apps`: prints the current top-level app windows with the exact name pipeline of the
/// dock (exe, root exe, AUMID, chosen name, which step produced it). Touches nothing else.
pub fn list_apps() -> i32 {
    use crate::logger::say;
    // SAFETY: initializes COM STA for this (main) thread; paired with CoUninitialize below.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let names = crate::config::get().names.clone();
    let wins = crate::apps::list_windows();
    say(&format!("{} app window(s)", wins.len()));
    for w in wins {
        let r = ResolveApp {
            key: w.ident.key.clone(),
            exe_path: w.ident.exe_path.clone(),
            aumid: w.ident.aumid.clone(),
            hwnd: 0,
            icon_px: 24,
            custom_icon: None,
        };
        let (name, step) = match override_name(&names, &r) {
            Some(o) => (o, "config"),
            None => natural_name(&r),
        };
        say(&format!("- window : {}", w.title));
        say(&format!("  name   : {name}   [{step}]"));
        say(&format!("  exe    : {}", w.own_exe));
        say(&format!("  root   : {}", w.ident.exe_path));
        say(&format!("  aumid  : {}", w.ident.aumid.as_deref().unwrap_or("-")));
        say(&format!("  key    : {}", w.ident.key));
    }
    if hr.is_ok() {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    0
}

/// Frees a `PWSTR` allocated by COM (`CoTaskMemAlloc`) and extracts its string.
fn take_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a NUL-terminated wide string returned by a shell API; it is read
    // once and then freed with CoTaskMemFree, as required by the GetDisplayName/GetString contract.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: same pointer, allocated by COM, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Name from the `shell:AppsFolder\<AUMID>` catalog (`SIGDN_NORMALDISPLAY`).
fn appsfolder_name(aumid: &str) -> Option<String> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: COM STA initialized on the thread; `path` lives for the call.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; the returned PWSTR is freed by `take_pwstr`.
    let name = take_pwstr(unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.ok()?)?;
    // If the catalog does not know the AUMID some paths return the AUMID itself: discarded.
    if name.eq_ignore_ascii_case(aumid) || name.contains('!') {
        return None;
    }
    Some(name)
}

/// Start menu icons per `StartIcons` batch (keeps every other request responsive while ~300 load).
const START_ICON_BATCH: usize = 8;
/// Safety cap on the AppsFolder entries kept.
const START_MAX_APPS: usize = 2000;

/// true for the AppsFolder entries the Start menu lists: a name, not an uninstaller, not a web link.
fn keep_start_app(id: &str, name: &str) -> bool {
    let n = name.to_lowercase();
    let i = id.to_lowercase();
    !name.trim().is_empty()
        && !n.contains("uninstall")
        && !n.contains("disinstall")
        && !i.starts_with("http:")
        && !i.starts_with("https:")
}

/// Every app of `shell:AppsFolder` (desktop shortcuts of the Start Menu folders AND packaged Store
/// apps, the same catalog as the native Start menu): `IShellItem::BindToHandler(BHID_EnumItems)`.
/// Duplicate ids are dropped. Empty on any failure (logged). Needs COM on the calling thread (the
/// shell thread, or the short-lived import job of `start_import`).
pub(crate) fn enum_apps_folder() -> Vec<StartApp> {
    let path = HSTRING::from("shell:AppsFolder");
    // SAFETY: COM STA initialized on the shell thread; `path` lives for the call.
    let folder: IShellItem = match unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) } {
        Ok(f) => f,
        Err(e) => {
            log!("shell: Start menu: shell:AppsFolder not available: {e}");
            return Vec::new();
        }
    };
    // SAFETY: `folder` is valid; BHID_EnumItems with IEnumShellItems is the documented pairing.
    let en: IEnumShellItems = match unsafe { folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems) } {
        Ok(e) => e,
        Err(e) => {
            log!("shell: Start menu: AppsFolder enumeration failed: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        let mut items: [Option<IShellItem>; 1] = [None];
        let mut fetched = 0u32;
        // SAFETY: `items` and `fetched` are writable and outlive the call; one element requested.
        let hr = unsafe { en.Next(&mut items, Some(&mut fetched)) };
        if hr.is_err() || fetched == 0 {
            break;
        }
        let Some(item) = items[0].take() else { break };
        // SAFETY: `item` is valid; the returned PWSTRs are freed by `take_pwstr`.
        let id = unsafe { item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING) }.ok().and_then(take_pwstr);
        // SAFETY: as above.
        let name = unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }.ok().and_then(take_pwstr);
        let (Some(id), Some(name)) = (id, name) else { continue };
        if !keep_start_app(&id, &name) || !seen.insert(id.to_lowercase()) {
            continue;
        }
        out.push(StartApp { id, name });
        if out.len() >= START_MAX_APPS {
            break;
        }
    }
    out
}

// -------------------------------------------------------------- the apps child (L3 T17)

/// The flag of the child mode that lists the AppsFolder catalogue (`run_apps_child`); not combinable.
pub(crate) const APPS_FLAG: &str = "--start-apps";
/// How long the shell thread waits for the apps child: it takes about 0.3 s, and this thread also runs every launch.
const APPS_CHILD_TIMEOUT: Duration = Duration::from_secs(5);
/// The most the dock reads back from the apps child: the apps arena's own cap.
const APPS_CHILD_MAX_BYTES: u64 = Caps::APPS.bytes as u64;
/// Exit code of `--start-apps` when nothing was listed (AppsFolder unavailable or empty): the dock then lists in-process.
const APPS_EXIT_EMPTY: i32 = 3;

/// The catalogue of one `ListStartApps`, from an `--start-apps` child (`update::run_capture`: no window, the dock's
/// job, at most the apps arena's cap read, 5 s), taken only as `apps_from_child` checks it. A child that cannot start,
/// fails, or prints anything the check refuses is logged (once per process) and the catalogue is listed here as before,
/// through the same encoder; after a child that ran out of time every later listing of this process is made here, so a
/// hung child never holds this thread 5 s twice. The AppsFolder enumeration and Windows' app index (the shell modules and
/// the parse of two index files) then load only in the child, which takes them with it when it exits. With `child`
/// false (Start with no launcher chord) the catalogue is listed here as in M1, with no child, thread or job.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn list_apps_arena(child: bool) -> Arena {
    static CHILD_OFF: AtomicBool = AtomicBool::new(false);
    static WARNED: AtomicBool = AtomicBool::new(false);
    if child && !CHILD_OFF.load(Relaxed) {
        let t = Instant::now();
        let out = crate::update::child_exe()
            .and_then(|exe| crate::update::run_capture(&exe, &[APPS_FLAG], APPS_CHILD_TIMEOUT, APPS_CHILD_MAX_BYTES));
        match apps_from_child(out) {
            Ok(a) => {
                log!("shell: Start menu: {} apps listed in {} ms (child)", a.len(), t.elapsed().as_millis());
                return a;
            }
            Err(why) => {
                let hung = t.elapsed() >= APPS_CHILD_TIMEOUT;
                if hung {
                    CHILD_OFF.store(true, Relaxed);
                }
                if !WARNED.swap(true, Relaxed) || hung {
                    let rest = if hung { " from now on" } else { "" };
                    log!("shell: Start menu: the apps process failed ({why}), listing in-process{rest}");
                }
            }
        }
    }
    COM_WORK.store(true, SeqCst);
    let t = Instant::now();
    let list = enum_apps_folder();
    crate::start_search::load_app_index();
    let index = crate::start_search::take_app_index();
    let arena = catalog::build_apps(&list, index.as_ref()).unwrap_or_else(|e| {
        log!("shell: Start menu: the catalogue of {} apps was not kept: {e}", list.len());
        Arena::default()
    });
    log!("shell: Start menu: {} apps listed in {} ms", arena.len(), t.elapsed().as_millis());
    arena
}

/// What the dock keeps of an `--start-apps` child's `run_capture` result: the arena, when the child exited 0 and its
/// whole output passes `arena::check` (header, sizes, every reference and order, the checksum) under the apps caps;
/// otherwise the reason. The bytes are untrusted: nothing is read from them before the check.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn apps_from_child(out: Result<(i32, Vec<u8>), String>) -> Result<Arena, String> {
    let (code, bytes) = out?;
    if code != 0 {
        return Err(format!("exit code {code}"));
    }
    let n = bytes.len();
    Arena::new(bytes, ArenaKind::Apps, None, Caps::APPS).map_err(|e| format!("{n} bytes refused: {e}"))
}

/// `--start-apps`, in the child `list_apps_arena` starts: the AppsFolder catalogue and Windows' app index, folded and
/// indexed by the in-process path's entry builder and encoder, written to stdout as one apps arena. It touches no data
/// folder, logger, mutex or console, reads no input and writes nothing but stdout. Exit 0 = an arena written; 1 = COM,
/// the encoder or stdout failed; 3 = nothing listed.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn run_apps_child() -> i32 {
    use std::io::Write;
    // SAFETY: the first COM call of this thread (the child's main thread); paired with CoUninitialize below.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return 1;
    }
    let list = enum_apps_folder();
    let code = if list.is_empty() {
        APPS_EXIT_EMPTY
    } else {
        crate::start_search::load_app_index();
        let index = crate::start_search::take_app_index();
        match catalog::encode_app_list(&list, index.as_ref()) {
            Ok(bytes) => {
                let mut stdout = std::io::stdout().lock();
                match stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
                    Ok(()) => 0,
                    Err(_) => 1,
                }
            }
            Err(_) => 1,
        }
    };
    // SAFETY: paired with the successful CoInitializeEx above; the listing released every COM object it made.
    unsafe { CoUninitialize() };
    code
}

// ------------------------------------------------------------------ icons from a child (L3 T20, M1 F5)

/// The flag of the child mode that makes icons (`run_icons_child`), followed by the pixel size; not combinable.
pub(crate) const ICONS_FLAG: &str = "--start-icons";
/// The pixel sizes `--start-icons` takes: the clamp every icon request of the dock gets.
pub(crate) const ICON_PX_RANGE: std::ops::RangeInclusive<u32> = 8..=256;
/// The first line of an icons child's output.
const ICONS_HEADER: &[u8] = b"mini-oled-dock start-icons 1\n";
/// What one icons child is given at most: ids, bytes per id (its line, the newline aside), bytes of input.
const ICON_IDS_MAX: usize = 2_000;
const ICON_ID_MAX_BYTES: usize = 1_024;
const ICON_INPUT_MAX: usize = 2 * 1024 * 1024;
// A full batch, newlines included, always fits in the child's input: the dock's writer never fills a pipe the child
// has stopped reading.
const _: () = assert!(ICON_IDS_MAX * (ICON_ID_MAX_BYTES + 1) <= ICON_INPUT_MAX);
/// The bytes of a frame's head: index, width and height, each a u32 LE.
const ICON_FRAME_HEAD: usize = 12;
/// A job older than this plus `ICON_JOB_PER_ID` per id is a hung child (about 7 ms per icon measured).
const ICON_JOB_BASE: Duration = Duration::from_secs(2);
const ICON_JOB_PER_ID: Duration = Duration::from_millis(50);
/// The child's write buffer (several frames at the dock's sizes).
const ICON_IO_BUF: usize = 64 * 1024;
/// The read buffer of a job's reader thread in the dock: the child flushes each frame, so a read rarely returns more than
/// one (3.6 KB at the launcher's 30 px, 9.2 KB at 48 px); a frame larger than the buffer is read through it (`read_full`).
/// 16 KiB instead of 64 KiB: 48 KiB less while each job runs (L3 MEM-3).
const ICON_READ_BUF: usize = 16 * 1024;
/// The stack of a job's reader thread.
const ICON_READER_STACK: usize = 128 * 1024;
/// How long the shell thread waits for a killed child to be gone: one held in the kernel past it is left to the dock's
/// job, so a launch never waits behind it.
const ICON_REAP_WAIT_MS: u32 = 1_000;

/// Whose icons a job makes; each client has at most one job at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IconClient {
    Start,
    Launcher,
}

/// What a job's reader thread tells the shell thread: how many of the job's ids were answered (every id before that
/// index went to the UI), that it ended (both pipes closed: nothing more comes), that the shell thread stopped the
/// child on purpose (nothing more of the job goes to the UI, and its cut output is no failure to log), and that the
/// child gave no header while nobody stopped it (`icons_in_process`; stored before `ended`): it made no icon, so the
/// shell thread makes the job's ids itself.
#[derive(Default)]
struct IconProgress {
    delivered: AtomicUsize,
    ended: AtomicBool,
    stopped: AtomicBool,
    no_header: AtomicBool,
}

/// How a job's output ended, as its reader saw it.
enum IconEnd {
    /// Every frame, to a clean end.
    Done,
    /// The ids could not be written, or the output ended or differed before its header: the child made no icon (it
    /// writes the header before its first extraction).
    NoHeader(String),
    /// A frame refused, or a read error, after the header: the child was at work on the icons.
    Broken(String),
}

/// true when a job's ids are made in-process (house rule 7 of plan L3: never worse than before the child): only when
/// its child gave no header and the shell thread did not stop it. A child stopped (killed late, so possibly hung, or
/// replaced) or broken after its header was extracting an icon, which in-process could hang or crash the shell thread:
/// what it left is not made here.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn icons_in_process(end: &IconEnd, stopped: bool) -> bool {
    matches!(end, IconEnd::NoHeader(_)) && !stopped
}

/// One `--start-icons` child at work for a client: the process (only the shell thread waits on it or kills it; the
/// reader thread holds its pipes), the size and ids it was given, its progress and when it started.
struct IconJob {
    child: crate::update::JobChild,
    px: u32,
    ids: Arc<[String]>,
    progress: Arc<IconProgress>,
    started: Instant,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl IconJob {
    /// How long the job may run before it counts as hung.
    fn deadline(&self) -> Duration {
        let n = u32::try_from(self.ids.len()).unwrap_or(u32::MAX);
        ICON_JOB_BASE.saturating_add(ICON_JOB_PER_ID.saturating_mul(n))
    }

    /// Kills and reaps the child (harmless once it has exited; the wait is bounded by `ICON_REAP_WAIT_MS`): its ids and
    /// how many of them were answered. Its reader sees the output close and ends; nothing it still decodes goes to the
    /// UI.
    fn stop(mut self) -> (Arc<[String]>, usize) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::System::Threading::WaitForSingleObject;
        self.progress.stopped.store(true, SeqCst);
        let _ = self.child.kill();
        // SAFETY: the process handle is valid while `self.child` lives; waiting on it has no other effect.
        let _ = unsafe { WaitForSingleObject(HANDLE(self.child.as_raw_handle()), ICON_REAP_WAIT_MS) };
        let _ = self.child.try_wait();
        (self.ids, self.progress.delivered.load(SeqCst))
    }
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl Shell {
    fn job(&mut self, c: IconClient) -> &mut Option<IconJob> {
        match c {
            IconClient::Start => &mut self.start_job,
            IconClient::Launcher => &mut self.launcher_job,
        }
    }

    /// Ends the client's job, if any (killed and reaped): its size, ids and answered count.
    fn end_job(&mut self, c: IconClient) -> Option<(u32, Arc<[String]>, usize)> {
        let job = self.job(c).take()?;
        let px = job.px;
        let (ids, delivered) = job.stop();
        Some((px, ids, delivered))
    }

    /// At each pass of the queue: a job whose reader ended is reaped once its child exited, or at once when the child
    /// gave no header; a job past its deadline is killed and reaped (a hung child; logged once per process). The ids of
    /// a job whose child gave no header are made here (`icons_here`), and so is every later batch of this process
    /// (`icons_child_off`). What any other job left unanswered for the Start menu leaves `start_sent`, so its next
    /// listing asks for it again.
    fn reap_icon_jobs(&mut self) {
        for c in [IconClient::Start, IconClient::Launcher] {
            let Some(job) = self.job(c).as_mut() else { continue };
            let exited = !matches!(job.child.try_wait(), Ok(None));
            let late = job.started.elapsed() > job.deadline();
            let ended = job.progress.ended.load(SeqCst);
            // Stored before `ended`, so read after it.
            let no_header = ended && job.progress.no_header.load(SeqCst);
            if !(late || ended && (exited || no_header)) {
                continue;
            }
            if late && !exited && !no_header {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Relaxed) {
                    log!(
                        "start icons: the icons process was still at work after {} ms and was stopped",
                        job.deadline().as_millis()
                    );
                }
            }
            if let Some((px, ids, delivered)) = self.end_job(c) {
                if no_header {
                    self.icons_child_off = true;
                    self.icons_here(c, px, ids.get(delivered..).unwrap_or(&[]).to_vec());
                } else if c == IconClient::Start {
                    unsend_icons(&mut self.start_sent, &ids, delivered);
                }
            }
        }
    }

    /// Ids a client's child did not make, made here as before T20: the Start menu's queued as `StartIcons` batches
    /// (they stay in `start_sent`), the launcher's as one `StartIconsFor`, which `launcher_icons` makes while
    /// `icons_child_off` (the launcher's close or a newer request drops it, as any pending one).
    fn icons_here(&mut self, c: IconClient, px: u32, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        match c {
            IconClient::Start => {
                for chunk in ids.chunks(START_ICON_BATCH) {
                    self.pending.push_back(ShellRequest::StartIcons { icon_px: px, ids: chunk.to_vec() });
                }
            }
            IconClient::Launcher => enqueue(&mut self.pending, ShellRequest::StartIconsFor { icon_px: px, ids }),
        }
    }

    /// Starts the client's icons child for `ids` at `px` in place of its running job, whose ids not answered yet (at
    /// the same size) go first (`next_icon_batch`): killing a batch would otherwise lose its rest for the whole open,
    /// since neither the launcher nor Start asks for an id twice. An id a child cannot take gets no icon; past
    /// `ICON_IDS_MAX` the others are dropped (for Start they leave `start_sent`). Err: the child could not start
    /// (logged once per process) or an icons child of this process gave no header (`icons_child_off`), with the ids to
    /// make here, as before.
    fn start_icon_job(&mut self, c: IconClient, px: u32, ids: Vec<String>) -> Result<(), Vec<String>> {
        let ids = match self.end_job(c) {
            Some((old_px, old, delivered)) if old_px == px => next_icon_batch(&old, delivered, ids),
            _ => ids,
        };
        let (batch, over) = icon_batch(ids);
        if c == IconClient::Start {
            for id in &over {
                self.start_sent.remove(id);
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        if self.icons_child_off {
            return Err(batch);
        }
        match spawn_icon_job(self.ui, c, px, batch) {
            Ok(job) => {
                *self.job(c) = Some(job);
                Ok(())
            }
            Err((why, batch)) => {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Relaxed) {
                    log!("start icons: the icons process could not start ({why}), icons made in-process");
                }
                Err(batch)
            }
        }
    }

    /// The Start menu's icons not sent yet (`missing`, at `start_px`): made by its icons child, or queued as
    /// `StartIcons` batches made here, as before, when the child cannot start or an icons child gave no header. A job
    /// at another size is ended first (its ids were forgotten with that size).
    fn start_menu_icons(&mut self, missing: Vec<String>) {
        let px = self.start_px;
        if self.start_job.as_ref().is_some_and(|j| j.px != px) {
            let _ = self.end_job(IconClient::Start);
        }
        if missing.is_empty() {
            return;
        }
        if let Err(back) = self.start_icon_job(IconClient::Start, px, missing) {
            self.icons_here(IconClient::Start, px, back);
        }
    }
}

/// Starts an `--start-icons <px>` child for `ids` (`update::spawn_in_job`: the dock's own exe and token, no window,
/// the dock's job; stdin and stdout piped, stderr null) and its reader thread ("icons", 128 KB of stack), which gets
/// both pipes. Err: why, with the ids back.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn spawn_icon_job(ui: isize, client: IconClient, px: u32, ids: Vec<String>) -> Result<IconJob, (String, Vec<String>)> {
    let exe = match crate::update::child_exe() {
        Ok(exe) => exe,
        Err(why) => return Err((why, ids)),
    };
    let mut child = match crate::update::spawn_in_job(&exe, &[ICONS_FLAG, &px.to_string()], true) {
        Ok(child) => child,
        Err(e) => return Err((e.to_string(), ids)),
    };
    let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(("no pipes".into(), ids));
    };
    let ids: Arc<[String]> = ids.into();
    let progress = Arc::new(IconProgress::default());
    let started = Instant::now();
    let reader = IconReader { ui, client, px, ids: Arc::clone(&ids), progress: Arc::clone(&progress), started };
    let spawned = std::thread::Builder::new().name("icons".into()).stack_size(ICON_READER_STACK).spawn(move || {
        probe!(crate::probe::bump(crate::probe::C::Spawned));
        reader.run(input, output)
    });
    if let Err(e) = spawned {
        let _ = child.kill();
        let _ = child.wait();
        return Err((format!("no reader thread: {e}"), ids.to_vec()));
    }
    Ok(IconJob { child, px, ids, progress, started })
}

/// What a job's reader thread needs besides the pipes.
struct IconReader {
    ui: isize,
    client: IconClient,
    px: u32,
    ids: Arc<[String]>,
    progress: Arc<IconProgress>,
    started: Instant,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl IconReader {
    /// The reader thread of one job: it writes the ids (the shell thread never writes to a pipe, so a child slow to
    /// read its input never holds a launch), closes the child's input, then decodes the frames and pushes them to the
    /// UI as they arrive: the Start menu's in batches of `START_ICON_BATCH`, the launcher's as soon as nothing more
    /// waits in the pipe. It ends at the end of the output, at a write error, a missing header or the first frame
    /// refused (each kind logged once per process; dropping the pipes makes a child still at work exit at its next
    /// write), or when the shell thread kills the child; then it logs what it delivered. A child that gave no header,
    /// unless stopped, marks the job `no_header` and wakes the shell thread, which makes its ids in-process.
    fn run(self, input: std::io::PipeWriter, output: std::io::PipeReader) {
        static HEADER_WARNED: AtomicBool = AtomicBool::new(false);
        static FRAME_WARNED: AtomicBool = AtomicBool::new(false);
        let (first, end) = match self.feed(input) {
            Ok(()) => self.read(output),
            Err(e) => {
                drop(output);
                (None, IconEnd::NoHeader(format!("the ids could not be written: {e}")))
            }
        };
        let stopped = self.progress.stopped.load(SeqCst);
        let here = icons_in_process(&end, stopped);
        self.progress.no_header.store(here, SeqCst);
        self.progress.ended.store(true, SeqCst);
        let tid = thread_id();
        if here && tid != 0 {
            // SAFETY: PostThreadMessageW is thread-safe; if the thread no longer exists it just fails.
            let _ = unsafe { PostThreadMessageW(tid, WM_SHELL_WAKE, WPARAM(0), LPARAM(0)) };
        }
        match end {
            IconEnd::NoHeader(why) if here && !HEADER_WARNED.swap(true, Relaxed) => {
                log!("start icons: the icons process failed ({why}), icons made in-process from now on");
            }
            IconEnd::Broken(why) if !stopped && !FRAME_WARNED.swap(true, Relaxed) => {
                log!("start icons: the icons process failed ({why}); its other icons are not made");
            }
            _ => {}
        }
        let n = self.ids.len();
        let delivered = self.progress.delivered.load(SeqCst);
        let first =
            first.map_or_else(|| "none delivered".to_string(), |t| format!("first batch after {} ms", t.as_millis()));
        let whose = if self.client == IconClient::Launcher { " for the launcher" } else { "" };
        log!("start icons: {delivered} of {n} in {} ms, {first} (child){whose}", self.started.elapsed().as_millis());
    }

    /// Writes the ids, one per line, and closes the child's input.
    fn feed(&self, mut input: std::io::PipeWriter) -> std::io::Result<()> {
        use std::io::Write;
        let mut text = Vec::with_capacity(self.ids.iter().map(|id| id.len() + 1).sum());
        for id in self.ids.iter() {
            text.extend_from_slice(id.as_bytes());
            text.push(b'\n');
        }
        input.write_all(&text)
    }

    /// Decodes the output and delivers the frames: when the first batch went out, and how the output ended.
    fn read(&self, output: std::io::PipeReader) -> (Option<Duration>, IconEnd) {
        let n = self.ids.len();
        let mut r = std::io::BufReader::with_capacity(ICON_READ_BUF, output);
        if let Err(why) = read_icon_header(&mut r) {
            return (None, IconEnd::NoHeader(why));
        }
        let (mut batch, mut prev, mut first) = (Vec::new(), None, None);
        let outcome = loop {
            match read_icon_frame(&mut r, n, self.px, prev) {
                Ok(Some((i, icon))) => {
                    prev = Some(i);
                    let Some(id) = self.ids.get(i) else { break IconEnd::Broken(format!("frame {i} of {n}")) };
                    batch.push((id.clone(), icon));
                    let due = match self.client {
                        IconClient::Start => batch.len() >= START_ICON_BATCH,
                        IconClient::Launcher => r.buffer().is_empty(),
                    };
                    if due {
                        self.deliver(&mut batch, i, &mut first);
                    }
                }
                Ok(None) => break IconEnd::Done,
                Err(why) => break IconEnd::Broken(why),
            }
        };
        if let (Some(i), false) = (prev, batch.is_empty()) {
            self.deliver(&mut batch, i, &mut first);
        }
        (first, outcome)
    }

    /// Pushes `batch` (its last frame of index `last`) to the UI, unless the shell thread stopped the job.
    fn deliver(&self, batch: &mut Vec<(String, Option<IconBitmap>)>, last: usize, first: &mut Option<Duration>) {
        let icons = std::mem::take(batch);
        if self.progress.stopped.load(SeqCst) {
            return;
        }
        let icon_px = self.px;
        push_event(
            self.ui,
            match self.client {
                IconClient::Start => ShellEvent::StartIcons { icon_px, icons },
                IconClient::Launcher => ShellEvent::LauncherIcons { icon_px, icons },
            },
        );
        self.progress.delivered.store(last + 1, SeqCst);
        first.get_or_insert_with(|| self.started.elapsed());
    }
}

/// `--start-icons <px>`, in the child a job starts: all of stdin is read first (`read_icon_ids`; the dock's writer and
/// this reader never wait on each other), then `write_icon_frames` with the pixels of `start_icon`, so identical to the
/// ones the dock made in-process. It touches no data folder, logger, mutex or console and writes nothing but stdout.
/// Exit 0 = every frame written; 1 = COM failed, or a write failed (the dock went away or stopped the job): it exits at
/// once.
pub fn run_icons_child(px: u32) -> i32 {
    let ids = read_icon_ids(&mut std::io::stdin().lock());
    // SAFETY: the first COM call of this thread (the child's main thread); paired with CoUninitialize below.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return 1;
    }
    let mut out = std::io::BufWriter::with_capacity(ICON_IO_BUF, std::io::stdout().lock());
    let code = write_icon_frames(&ids, px, &mut out, start_icon);
    if code == 0 {
        // SAFETY: paired with the successful CoInitializeEx above; every icon released its COM objects.
        unsafe { CoUninitialize() };
    }
    code
}

/// What an icons child given `ids` at `px` writes to `out`: the header, then one frame per id in input order, each
/// written and flushed at once. A frame carries `make`'s bitmap only when it is exactly `px` x `px`; otherwise, and
/// for a line the child could not read (`None`, never given to `make`), it is the 0 x 0 frame. 0 = every frame
/// written; 1 at the first write or flush that fails, after which nothing more is made or written.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn write_icon_frames(
    ids: &[Option<String>],
    px: u32,
    out: &mut impl std::io::Write,
    mut make: impl FnMut(&str, u32) -> Option<IconBitmap>,
) -> i32 {
    if out.write_all(ICONS_HEADER).and_then(|()| out.flush()).is_err() {
        return 1;
    }
    for (i, id) in ids.iter().enumerate() {
        let icon = id.as_deref().and_then(|id| make(id, px)).filter(|b| b.width == px && b.height == px);
        let frame = encode_icon_frame(u32::try_from(i).unwrap_or(u32::MAX), icon.as_ref());
        if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
            return 1;
        }
    }
    0
}

/// One frame of an icons child: u32 LE index, width and height, then width x height u32 LE premultiplied pixels,
/// top-down (`IconBitmap`'s layout). `None`, or a bitmap whose pixels do not match its size, is the 0 x 0 frame:
/// no icon.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn encode_icon_frame(index: u32, icon: Option<&IconBitmap>) -> Vec<u8> {
    let icon =
        icon.filter(|b| !b.pixels.is_empty() && b.pixels.len() as u64 == u64::from(b.width) * u64::from(b.height));
    let (w, h, pixels) = icon.map_or((0, 0, &[][..]), |b| (b.width, b.height, b.pixels.as_slice()));
    let mut out = Vec::with_capacity(ICON_FRAME_HEAD + pixels.len() * 4);
    for x in [index, w, h] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for p in pixels {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
}

/// Fills `buf` from `r` until it is full or the input ends: the bytes read.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0usize;
    while let Some(rest) = buf.get_mut(got..).filter(|b| !b.is_empty()) {
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => got = got.saturating_add(k),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got.min(buf.len()))
}

/// Reads the header of an icons child's output: `mini-oled-dock start-icons 1` and a newline, nothing else.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn read_icon_header(r: &mut impl std::io::Read) -> Result<(), String> {
    let mut head = [0u8; ICONS_HEADER.len()];
    match read_full(r, &mut head) {
        Ok(0) => Err("no output".into()),
        Ok(k) if k == head.len() && head == *ICONS_HEADER => Ok(()),
        Ok(_) => Err("not an icons output".into()),
        Err(e) => Err(format!("reading the header: {e}")),
    }
}

/// The next frame of an icons child that was given `n` ids at `px` (one of `ICON_PX_RANGE`), after the frame of
/// index `prev`; `Ok(None)` at a clean end (the output ends between two frames). Refused, before anything is allocated
/// from it: an index of `n` or more, or not above `prev` (so never more than `n` frames); a width other than the
/// height, or neither 0 nor exactly `px` (`start_icon` always makes `px` x `px`), so no frame is larger than
/// `px` x `px` x 4 bytes; a frame cut short.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn read_icon_frame(
    r: &mut impl std::io::Read,
    n: usize,
    px: u32,
    prev: Option<usize>,
) -> Result<Option<(usize, Option<IconBitmap>)>, String> {
    if !ICON_PX_RANGE.contains(&px) {
        return Err(format!("{px} px is no icon size"));
    }
    let mut head = [0u8; ICON_FRAME_HEAD];
    match read_full(r, &mut head).map_err(|e| format!("reading a frame: {e}"))? {
        0 => return Ok(None),
        ICON_FRAME_HEAD => {}
        k => return Err(format!("a frame cut after {k} bytes")),
    }
    let [i0, i1, i2, i3, w0, w1, w2, w3, h0, h1, h2, h3] = head;
    let index = usize::try_from(u32::from_le_bytes([i0, i1, i2, i3])).unwrap_or(usize::MAX);
    let (w, h) = (u32::from_le_bytes([w0, w1, w2, w3]), u32::from_le_bytes([h0, h1, h2, h3]));
    if index >= n {
        return Err(format!("frame {index} of {n}"));
    }
    if let Some(p) = prev.filter(|&p| index <= p) {
        return Err(format!("frame {index} after frame {p}"));
    }
    if w != h {
        return Err(format!("a {w} x {h} frame"));
    }
    if w == 0 {
        return Ok(Some((index, None)));
    }
    if w != px {
        return Err(format!("a {w} px frame at {px} px"));
    }
    let side = px as usize;
    let mut bytes = vec![0u8; side * side * 4];
    let got = read_full(r, &mut bytes).map_err(|e| format!("reading a frame: {e}"))?;
    if got != bytes.len() {
        return Err(format!("a frame cut after {} bytes", ICON_FRAME_HEAD + got));
    }
    let pixels = bytes.chunks_exact(4).map(|c| <[u8; 4]>::try_from(c).map_or(0, u32::from_le_bytes)).collect();
    Ok(Some((index, Some(IconBitmap { width: px, height: px, pixels }))))
}

/// The ids an icons child reads from its input, one per line as the dock wrote them: reading stops at 2 MiB (the line
/// the cap cuts is dropped) and at most `ICON_IDS_MAX` lines are taken; a line over `ICON_ID_MAX_BYTES` or not UTF-8
/// is an id with no icon (`None`), so every answer keeps the index of its line. A read error ends the input there.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn read_icon_ids(r: &mut impl std::io::Read) -> Vec<Option<String>> {
    use std::io::Read;
    let mut input = Vec::new();
    let _ = r.take(ICON_INPUT_MAX as u64 + 1).read_to_end(&mut input);
    if input.len() > ICON_INPUT_MAX {
        let keep = input.get(..ICON_INPUT_MAX).and_then(|b| b.iter().rposition(|&c| c == b'\n')).map_or(0, |i| i + 1);
        input.truncate(keep);
    }
    let mut lines: Vec<&[u8]> = input.split(|&b| b == b'\n').collect();
    // After the last newline: nothing, or a last line without its newline.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .take(ICON_IDS_MAX)
        .map(|l| if l.len() <= ICON_ID_MAX_BYTES { std::str::from_utf8(l).ok().map(str::to_string) } else { None })
        .collect()
}

/// The ids one child is given, in order: an id with a control character or over the child's line cap (1,024 bytes) is
/// left out, so it gets no icon, as when an extraction fails, and never breaks a batch; past `ICON_IDS_MAX` the others
/// come back.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn icon_batch(ids: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut batch: Vec<String> =
        ids.into_iter().filter(|id| id.len() <= ICON_ID_MAX_BYTES && !id.chars().any(char::is_control)).collect();
    let over = if batch.len() > ICON_IDS_MAX { batch.split_off(ICON_IDS_MAX) } else { Vec::new() };
    (batch, over)
}

/// The batch that replaces a running job of the same client: the ids the old job had not answered (`old[delivered..]`)
/// that `ids` does not ask for again, first (they were asked earlier), then `ids`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn next_icon_batch(old: &[String], delivered: usize, ids: Vec<String>) -> Vec<String> {
    let rest = old.get(delivered..).unwrap_or(&[]);
    if rest.is_empty() {
        return ids;
    }
    let mut batch: Vec<String> = {
        let asked: HashSet<&str> = ids.iter().map(String::as_str).collect();
        rest.iter().filter(|id| !asked.contains(id.as_str())).cloned().collect()
    };
    batch.extend(ids);
    batch
}

/// The Start menu's ids a job ended without answering (`old[delivered..]`), with no batch to carry them: they leave
/// `start_sent`, so the next listing of the same open asks for them again.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn unsend_icons(sent: &mut HashSet<String>, old: &[String], delivered: usize) {
    for id in old.get(delivered..).unwrap_or(&[]) {
        sent.remove(id);
    }
}

/// Index in `apps` of the app that sent a notification: same id as the AUMID, then same display
/// name, then (Win32 toast ids such as "FirefoxToast-308046B0AF4A39CB") the id equal to the part
/// after the last '-'. Pure, for the tests.
pub(crate) fn find_notif_app(apps: &[StartApp], aumid: &str, name: &str) -> Option<usize> {
    let aumid = aumid.trim();
    let name = name.trim();
    if !aumid.is_empty() {
        if let Some(i) = apps.iter().position(|a| a.id.eq_ignore_ascii_case(aumid)) {
            return Some(i);
        }
    }
    if !name.is_empty() {
        if let Some(i) = apps.iter().position(|a| a.name.eq_ignore_ascii_case(name)) {
            return Some(i);
        }
    }
    let tail = aumid.rsplit('-').next().unwrap_or("");
    if tail.len() >= 8 && tail.len() < aumid.len() {
        return apps.iter().position(|a| a.id.eq_ignore_ascii_case(tail));
    }
    None
}

/// `ShellRequest::OpenNotifApp`.
fn open_notif_app(aumid: &str, name: &str) {
    let apps = enum_apps_folder();
    match find_notif_app(&apps, aumid, name) {
        Some(i) => {
            let target = format!("shell:AppsFolder\\{}", apps[i].id);
            if let Err(e) = launch(&target, None) {
                log!("notification: launching '{}' failed: {e}, opening the notification center", apps[i].name);
                crate::ui::send_win_n();
            }
        }
        None => {
            log!("notification: app '{name}' ({aumid}) not in the catalog, opening the notification center");
            crate::ui::send_win_n();
        }
    }
}

/// Icon of the AppsFolder app `id`, `px` x `px`: `IShellItemImageFactory::GetImage(px, ICONONLY)`
/// (asked directly at the final size: much faster than the 256 px source used for the bar).
fn start_icon(id: &str, px: u32) -> Option<IconBitmap> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{id}"));
    // SAFETY: COM STA initialized; `path` lives for the call.
    let item: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    let side = px as i32;
    // SAFETY: `item` is valid; the returned HBITMAP is ours and is deleted below.
    let hbm = unsafe { item.GetImage(SIZE { cx: side, cy: side }, SIIGBF_ICONONLY) }.ok()?;
    let b = icons::hbitmap_to_bitmap(hbm).and_then(nonempty);
    // SAFETY: bitmap owned by us, not selected into any DC.
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
    nonempty(icons::resize_area(&b?, px))
}

/// Version-resource strings of an executable, read through `IShellItem2::GetString`. Also used by
/// `tray_host` for the tray icons' app names. Needs COM initialized on the calling thread.
#[derive(Default)]
pub(crate) struct FileInfo {
    pub(crate) description: Option<String>,
    product: Option<String>,
    company: Option<String>,
}

impl FileInfo {
    pub(crate) fn read(exe_path: &str) -> FileInfo {
        let path = HSTRING::from(exe_path);
        // SAFETY: COM STA initialized; `path` lives for the call.
        let Ok(item) = (unsafe { SHCreateItemFromParsingName::<_, _, IShellItem2>(&path, None::<&IBindCtx>) }) else {
            return FileInfo::default();
        };
        let get = |key: &PROPERTYKEY| {
            // SAFETY: `item` is valid, key is valid; the PWSTR is freed by `take_pwstr`.
            unsafe { item.GetString(key) }.ok().and_then(take_pwstr)
        };
        FileInfo {
            description: get(&PKEY_FILE_DESCRIPTION),
            product: get(&PKEY_PRODUCT_NAME),
            company: get(&PKEY_COMPANY),
        }
    }
}

// ------------------------------------------------------------------ icon

fn nonempty(b: IconBitmap) -> Option<IconBitmap> {
    if b.is_empty() || !b.has_visible_pixels() {
        None
    } else {
        Some(b)
    }
}

/// Order: custom PNG, disk cache, extraction (AppsFolder, jumbo, SHDefExtractIconW,
/// WM_GETICON/class). Returns the icon already scaled to `r.icon_px`.
fn resolve_icon(r: &ResolveApp) -> Option<IconBitmap> {
    let px = if r.icon_px == 0 { 24 } else { r.icon_px.clamp(8, 256) };

    // 1. User's custom PNG (absolute priority, never cached).
    if let Some(b) = icons::load_custom_icon(&r.key, r.custom_icon.as_deref()).and_then(nonempty) {
        return nonempty(icons::resize_area(&b, px));
    }

    // 2. Disk cache (64 px master), invalidated by the exe's mtime.
    let exe_ok = is_usable_exe(&r.exe_path);
    let exe_mtime = if exe_ok { icons::file_mtime(Path::new(&r.exe_path)) } else { None };
    // Only for sizes the master covers: beyond it the original source is used (below), never a stretched copy.
    if px <= MASTER_PX {
        if let Some(m) = icons::cache_load(&r.key, exe_mtime) {
            if let Some(b) = nonempty(icons::resize_area(&m, px)) {
                return Some(b);
            }
        }
    }

    // 3-5. Extraction.
    let (src, cacheable) = extract_icon(r, exe_ok)?;
    let master = nonempty(icons::resize_area(&src, MASTER_PX))?;
    if cacheable {
        icons::cache_store(&r.key, &master);
    }
    // For final sizes beyond the master, start from the source to avoid blurring.
    nonempty(icons::resize_area(if px > MASTER_PX { &src } else { &master }, px))
}

/// Tries the sources in order; the second value says whether the result should be cached
/// (the window icon, low resolution and per-window, should not).
fn extract_icon(r: &ResolveApp, exe_ok: bool) -> Option<(IconBitmap, bool)> {
    if let Some(a) = r.aumid.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        if let Some(b) = icon_from_appsfolder(a) {
            return Some((b, true));
        }
    }
    if exe_ok {
        if let Some(b) = icon_from_def_extract(&r.exe_path) {
            return Some((b, true));
        }
        // Fallback only: the jumbo system image list caches ~256 KB per index for the life of the process.
        if let Some(b) = icon_from_jumbo_list(&r.exe_path) {
            return Some((b, true));
        }
    }
    if r.hwnd != 0 {
        if let Some(b) = icon_from_window(r.hwnd) {
            return Some((b, false));
        }
    }
    None
}

/// Converts an owned HICON (to be destroyed) into a bitmap.
fn owned_hicon_to_bitmap(icon: HICON) -> Option<IconBitmap> {
    let b = icons::hicon_to_bitmap(icon).and_then(nonempty);
    // SAFETY: `icon` was created for us (GetIcon/SHDefExtractIconW) and is no longer used.
    let _ = unsafe { DestroyIcon(icon) };
    b
}

/// (2) `IShellItemImageFactory::GetImage(256, SIIGBF_ICONONLY)` on `shell:AppsFolder\<AUMID>`.
fn icon_from_appsfolder(aumid: &str) -> Option<IconBitmap> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: COM STA initialized; `path` lives for the call.
    let item: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; the returned HBITMAP is ours and is deleted below.
    let hbm = unsafe { item.GetImage(SIZE { cx: SRC_ICON_PX, cy: SRC_ICON_PX }, SIIGBF_ICONONLY) }.ok()?;
    let b = icons::hbitmap_to_bitmap(hbm).and_then(nonempty);
    // SAFETY: bitmap owned by us, not selected into any DC.
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
    b
}

/// (3) `SHGetFileInfoW(SHGFI_SYSICONINDEX)` + `SHGetImageList(SHIL_JUMBO)` + `GetIcon(ILD_TRANSPARENT)`,
/// discarding `iIcon == 0` (generic document icon).
fn icon_from_jumbo_list(exe_path: &str) -> Option<IconBitmap> {
    let path = HSTRING::from(exe_path);
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: `path` NUL-terminated for the duration of the call; `sfi` writable with its size.
    let ok = unsafe {
        SHGetFileInfoW(
            &path,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if ok == 0 || sfi.iIcon == 0 {
        return None;
    }
    // SAFETY: COM STA initialized; the requested interface matches the IID.
    let list: IImageList = unsafe { SHGetImageList(SHIL_JUMBO as i32) }.ok()?;
    // SAFETY: `list` is valid; index obtained from SHGetFileInfoW on the same system list.
    let icon = unsafe { list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) }.ok()?;
    owned_hicon_to_bitmap(icon)
}

/// (4) `SHDefExtractIconW(exe, 0, 0, 256)`: extracts the resource closest to 256 px.
fn icon_from_def_extract(exe_path: &str) -> Option<IconBitmap> {
    let path = HSTRING::from(exe_path);
    let mut large = HICON::default();
    // SAFETY: `path` valid for the call; `large` is writable; small icon not requested.
    let hr = unsafe { SHDefExtractIconW(&path, 0, 0, Some(&mut large), None, SRC_ICON_PX as u32) };
    if hr.is_err() || large.is_invalid() {
        return None;
    }
    owned_hicon_to_bitmap(large)
}

/// (5) `WM_GETICON` (with timeout, ICON_BIG then ICON_SMALL2), then the class icon.
/// The HICONs remain owned by the window/class: they must NOT be destroyed.
fn icon_from_window(hwnd: isize) -> Option<IconBitmap> {
    let hwnd = HWND(hwnd as *mut c_void);
    for kind in [ICON_BIG, ICON_SMALL2] {
        let mut res = 0usize;
        // SAFETY: SendMessageTimeoutW with SMTO_ABORTIFHUNG and a short timeout: it cannot block for
        // long; `res` is writable. The shell thread is not the thread of any dock window.
        let r = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                GETICON_TIMEOUT_MS,
                Some(&mut res),
            )
        };
        if r.0 != 0 && res != 0 {
            if let Some(b) = icons::hicon_to_bitmap(HICON(res as *mut c_void)).and_then(nonempty) {
                return Some(b);
            }
        }
    }
    for idx in [GCLP_HICON, GCLP_HICONSM] {
        // SAFETY: reads a class attribute; if the window is gone it returns 0.
        let v = unsafe { GetClassLongPtrW(hwnd, idx) };
        if v != 0 {
            if let Some(b) = icons::hicon_to_bitmap(HICON(v as *mut c_void)).and_then(nonempty) {
                return Some(b);
            }
        }
    }
    None
}

// ------------------------------------------------------------------ launch

/// true if the current process is elevated (token with `TokenIsElevated`); false when unknown. Computed once.
pub(crate) fn is_elevated() -> bool {
    elevation() == Some(true)
}

/// Whether the current process is elevated; `None` when its token cannot be read. Computed once.
pub(crate) fn elevation() -> Option<bool> {
    static ELEVATED: OnceLock<Option<bool>> = OnceLock::new();
    *ELEVATED.get_or_init(|| {
        let mut token = HANDLE::default();
        // SAFETY: GetCurrentProcess returns the pseudo-handle; `token` writable, closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
            return None;
        }
        let mut te = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        // SAFETY: `te` is a writable TOKEN_ELEVATION of declared size; `token` is valid.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut te as *mut TOKEN_ELEVATION as *mut c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
        }
        .is_ok();
        // SAFETY: handle opened above and no longer used.
        let _ = unsafe { CloseHandle(token) };
        ok.then_some(te.TokenIsElevated != 0)
    })
}

/// Set after a request whose handling may have loaded in-process COM servers on this thread outside the Start menu's
/// own cycle (a launch through the desktop shell, a name or an icon looked up in AppsFolder): the next trim's MTA pass
/// marks what they left, and `mem` makes sure of the second pass 11 minutes on that unloads it (M1 F4; live-b L-5).
static COM_WORK: AtomicBool = AtomicBool::new(false);

/// Takes the flag of `COM_WORK` (the UI thread's trim).
pub(crate) fn take_com_work() -> bool {
    COM_WORK.swap(false, SeqCst)
}

/// The requests whose handling may load in-process COM servers here: every launch and reveal (the desktop shell's
/// proxies), a bar entry's name and icon (AppsFolder, the icon pipeline), Show desktop, a notification's app, Start's
/// in-process icons and an in-process listing. Volume, network and the launcher's state files are not.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn loads_com(req: &ShellRequest) -> bool {
    match req {
        // The children's requests count only when they fall back in-process (`COM_WORK` is set there).
        ShellRequest::ListStartApps { child: false, .. }
        | ShellRequest::ResolveApp(_)
        | ShellRequest::Launch { .. }
        | ShellRequest::LaunchAdmin(_)
        | ShellRequest::LaunchAppAdmin { .. }
        | ShellRequest::RevealApp { .. }
        | ShellRequest::ToggleDesktop
        | ShellRequest::OpenNotifApp { .. }
        | ShellRequest::StartIcons { .. }
        | ShellRequest::OpenPath { .. }
        | ShellRequest::RunAsTyped { .. }
        | ShellRequest::LaunchCommand { .. }
        | ShellRequest::RevealPath { .. }
        | ShellRequest::OpenUrl { .. }
        | ShellRequest::LauncherLaunch { .. }
        | ShellRequest::LauncherAppAdmin { .. }
        | ShellRequest::LauncherRevealApp { .. } => true,
        ShellRequest::ListStartApps { child: true, .. }
        | ShellRequest::StartIconsFor { .. }
        | ShellRequest::VolumeAdjust { .. }
        | ShellRequest::VolumeToggleMute
        | ShellRequest::VolumeRefresh
        | ShellRequest::VolumeSet { .. }
        | ShellRequest::ListOutputs
        | ShellRequest::SetDefaultOutput { .. }
        | ShellRequest::ListSpatial
        | ShellRequest::SetSpatialFormat { .. }
        | ShellRequest::ListSessions
        | ShellRequest::SetSessionVolume { .. }
        | ShellRequest::ToggleSessionMute { .. }
        | ShellRequest::OpenStart
        | ShellRequest::NetworkQuery
        | ShellRequest::NetworkRelease
        | ShellRequest::AudioRelease
        | ShellRequest::StartRelease
        | ShellRequest::ComFree
        | ShellRequest::StartSettingsReady
        | ShellRequest::LauncherClosed
        | ShellRequest::LauncherConvert { .. }
        | ShellRequest::LauncherStateLoad
        | ShellRequest::LauncherStateSave { .. } => false,
    }
}

/// Launches `target` (exe path, `shell:AppsFolder\...`, `ms-settings:...`) with optional `args`.
/// From the shell thread, does not block the UI.
///
/// SECURITY: targets come from the config, which any medium-integrity process can write. When
/// the dock is elevated, EVERY launch goes through the desktop shell (`desktop_shell_execute`),
/// so it runs unelevated as the interactive user. If that path fails the launch is REFUSED:
/// there is no elevated fallback. Even `ms-settings:` URIs are routed this way, because an
/// elevated ShellExecute resolves protocol handlers through the merged HKCR view, which includes
/// the medium-writable `HKCU\Software\Classes` (the classic fodhelper-style UAC bypass).
fn launch(target: &str, args: Option<&str>) -> Result<(), String> {
    launch_with(target, args, LaunchBy::Shared)
}

/// Who asks `launch_with` for a launch: the shared requests (Start, the bar, notifications, the power popup), whose
/// rule is 1.4's, or the launcher, whose requests fail closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LaunchBy {
    Shared,
    Launcher,
}

/// Whether a launch may run `ShellExecuteExW` in the dock's own process: a shared request whenever the dock is not
/// known to be elevated (1.4's rule); a launcher request only when it is known not to be (`plain_open`: a token that
/// cannot be read counts as elevated).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn in_process(by: LaunchBy, elevation: Option<bool>) -> bool {
    match by {
        LaunchBy::Shared => elevation != Some(true),
        LaunchBy::Launcher => plain_open(Verb::Open, elevation),
    }
}

/// `launch` with the caller's rule for the in-process path (`in_process`); otherwise the desktop shell, or refused.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launch_with(target: &str, args: Option<&str>, by: LaunchBy) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("empty target".into());
    }
    // An exe that is gone because the app updated itself into a new versioned folder.
    let moved = crate::apps::updated_exe_path(target);
    let target = moved.as_deref().unwrap_or(target);
    let args = args.map(str::trim).filter(|a| !a.is_empty());
    let dir = Path::new(target)
        .parent()
        .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
        .map(|p| p.to_string_lossy().into_owned());
    if in_process(by, elevation()) {
        log!("{}", launch_line(by, target, args, ""));
        return shell_execute(target, args, dir.as_deref());
    }
    match desktop_shell_execute(target, args, dir.as_deref(), Verb::Open, SW_SHOWNORMAL.0) {
        Ok(()) => {
            log!("{}", launch_line(by, target, args, " via the desktop shell (de-elevated)"));
            Ok(())
        }
        Err(e) => {
            log!("{}", launch_refused_line(by, target, &e.to_string()));
            Err(format!("de-elevated launch failed: {e}"))
        }
    }
}

/// Log text of a launch. A launcher launch names only its kind and the length of its text: `dock.log` inherits the
/// data root's read ACE for BUILTIN\Users, so what the user picked must not be written there (SPEC anti-goal G).
fn launch_line(by: LaunchBy, target: &str, args: Option<&str>, tail: &str) -> String {
    match by {
        LaunchBy::Shared => format!("shell: Launch '{target}' args={args:?}{tail}"),
        LaunchBy::Launcher => format!("shell: Launch ({} chars){tail}", target.chars().count()),
    }
}

/// Log text of a refused de-elevated launch (see `launch_line`).
fn launch_refused_line(by: LaunchBy, target: &str, e: &str) -> String {
    match by {
        LaunchBy::Shared => {
            format!("shell: de-elevated launch of '{target}' failed ({e}): REFUSED, never launching elevated")
        }
        LaunchBy::Launcher => format!("shell: de-elevated launch failed ({e}): REFUSED, never launching elevated"),
    }
}

/// Log text of an elevated app launch (see `launch_line`); `outcome` is the error, if any.
fn app_admin_line(by: LaunchBy, target: &str, outcome: Result<(), &str>) -> String {
    match (by, outcome) {
        (LaunchBy::Shared, Ok(())) => {
            format!("shell: LaunchAppAdmin '{target}' via the desktop shell (runas, UAC prompt)")
        }
        (LaunchBy::Shared, Err(e)) => {
            format!("shell: LaunchAppAdmin '{target}' through the desktop shell failed ({e}): REFUSED")
        }
        (LaunchBy::Launcher, Ok(())) => {
            format!(
                "shell: LaunchAppAdmin ({} chars) via the desktop shell (runas, UAC prompt)",
                target.chars().count()
            )
        }
        (LaunchBy::Launcher, Err(e)) => {
            format!("shell: LaunchAppAdmin through the desktop shell failed ({e}): REFUSED")
        }
    }
}

/// Log text of a reveal that found no file (`path` is `None`, `id` is the app) or did not run (see `launch_line`).
fn reveal_line(by: LaunchBy, id: &str, path: Option<&str>, e: &str) -> String {
    match (by, path) {
        (LaunchBy::Shared, None) => format!("shell: RevealApp '{id}': no file to show"),
        (LaunchBy::Shared, Some(p)) => format!("shell: RevealApp '{p}' not executed: {e}"),
        (LaunchBy::Launcher, None) => format!("shell: RevealApp ({} chars): no file to show", id.chars().count()),
        (LaunchBy::Launcher, Some(_)) => format!("shell: RevealApp not executed: {e}"),
    }
}

/// Starts `tool` elevated the way the native "Terminal (Admin)" does: Explorer (medium integrity) runs
/// it with the `runas` verb and Windows shows the UAC prompt. Same path whether or not the dock is
/// elevated; if the desktop shell is unavailable the launch is REFUSED (no direct ShellExecute or
/// CreateProcess, so the dock's token is never inherited). A cancelled prompt is handled by Explorer.
fn launch_admin(tool: AdminTool) {
    let Some(sys) = system_dir() else {
        log!("shell: LaunchAdmin: no system directory: REFUSED");
        return;
    };
    let target = tool.target(&sys);
    match desktop_shell_execute(&target, None, None, Verb::RunAs, SW_SHOWNORMAL.0) {
        Ok(()) => log!("shell: LaunchAdmin '{target}' via the desktop shell (runas, UAC prompt)"),
        Err(e) => log!("shell: LaunchAdmin '{target}' through the desktop shell failed ({e}): REFUSED"),
    }
}

/// `ShellRequest::LaunchAppAdmin`: like `launch_admin`, for an app of the AppsFolder catalog.
fn launch_app_admin(id: &str, by: LaunchBy) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("empty app id".into());
    }
    let target = format!("shell:AppsFolder\\{id}");
    match desktop_shell_execute(&target, None, None, Verb::RunAs, SW_SHOWNORMAL.0) {
        Ok(()) => {
            log!("{}", app_admin_line(by, &target, Ok(())));
            Ok(())
        }
        Err(e) => {
            log!("{}", app_admin_line(by, &target, Err(&e.to_string())));
            Err(format!("desktop shell: {e}"))
        }
    }
}

/// `PKEY_Link_TargetParsingPath` ({B9B4B3FC-2B51-4A42-B5D8-324146AFCF25}, 2).
const PKEY_LINK_TARGET: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25), pid: 2 };

/// File an AppsFolder app starts, as the catalog knows it (`None` for a Store app or a target that
/// is not an existing absolute path).
fn app_target_path(id: &str) -> Option<String> {
    let name = HSTRING::from(format!("shell:AppsFolder\\{}", id.trim()));
    // SAFETY: COM STA initialized on the shell thread; `name` lives for the call.
    let item: IShellItem2 = unsafe { SHCreateItemFromParsingName(&name, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; the returned PWSTR is freed by `take_pwstr`.
    let path = take_pwstr(unsafe { item.GetString(&PKEY_LINK_TARGET) }.ok()?)?;
    let p = Path::new(&path);
    (p.is_absolute() && !path.contains('"') && p.exists()).then_some(path)
}

/// `ShellRequest::RevealApp`: File Explorer (from the Windows folder) with the app's file selected.
/// An app with no file to show (a Store app) is not a failure.
fn reveal_app(id: &str) -> Result<(), String> {
    reveal_app_with(id, LaunchBy::Shared)
}

/// `reveal_app` with the caller's rule for `launch_with`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn reveal_app_with(id: &str, by: LaunchBy) -> Result<(), String> {
    let Some(path) = app_target_path(id) else {
        log!("{}", reveal_line(by, id, None, ""));
        return Ok(());
    };
    let Some(explorer) = system_dir().and_then(|s| s.parent().map(|w| w.join("explorer.exe"))) else {
        return Err("no Windows folder".into());
    };
    let r = launch_with(&explorer.to_string_lossy(), Some(&format!("/select,\"{path}\"")), by);
    if let Err(e) = &r {
        log!("{}", reveal_line(by, id, Some(&path), e));
    }
    r
}

// launcher requests: begin
// What the launcher asks for. Every launch leaves through the desktop shell (`shell_exec`), `runas`
// only from `RunAsTyped`. Logs carry the request's kind and the length of its text, never the text.

/// One desktop-shell call as the launcher's requests make it: (file, arguments, working directory,
/// verb). The real one is `shell_exec`; the tests pass a recorder, so they see exactly what would
/// reach the shell and start nothing.
type Exec<'a> = &'a mut dyn FnMut(&str, Option<&str>, Option<&str>, Verb) -> Result<(), String>;

/// Why a launcher launch did not happen.
#[derive(Debug, PartialEq, Eq)]
enum Fail {
    /// Refused by the checks here before any shell call: logged only, no `LaunchFailed` (Explorer was
    /// never asked, so "Explorer isn't answering" would be false).
    Refused(&'static str),
    /// The shell call was made and failed: logged and pushed as `LaunchFailed`.
    Shell(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Refused(why) => write!(f, "refused: {why}"),
            Fail::Shell(e) => f.write_str(e),
        }
    }
}

/// Whether a launcher request may use plain `ShellExecuteExW` in the dock's own process: only to open
/// something, and only when the dock is known not to be elevated. A token that cannot be read
/// (`elevation() == None`) counts as elevated: what the user typed never reaches an admin token.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn plain_open(verb: Verb, elevation: Option<bool>) -> bool {
    verb == Verb::Open && elevation == Some(false)
}

/// Whether one launcher launch of `kind` may run in the dock's own process: `plain_open`, except for a path or a
/// command line (`OpenPath`, `LaunchCommand`), which leave through the desktop shell whether or not the dock is
/// elevated. The in-process call carries SEE_MASK_FLAG_NO_UI, which may hide the prompt Windows raises before it runs
/// a downloaded file, and that prompt is the second gate for typed paths and alias targets (5.4.6). A line with a %VAR% reaches its program through
/// cmd's `start` (ShellExecuteEx, in the user's context): live check 6 must show the prompt for that case too.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn in_process_exec(kind: LaunchKind, verb: Verb, elevation: Option<bool>) -> bool {
    !matches!(kind, LaunchKind::Path | LaunchKind::Command) && plain_open(verb, elevation)
}

/// The real `Exec` of a launcher launch of `kind`: the desktop shell (Explorer, medium integrity) unless
/// `in_process_exec` (an open by a dock known not to be elevated, never of a path or a command line); `runas` always
/// goes there (Explorer asks UAC), as in `launch_admin`. If the desktop shell is unavailable the request is refused;
/// the dock's token is never used.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn shell_exec(kind: LaunchKind, file: &str, args: Option<&str>, dir: Option<&str>, verb: Verb) -> Result<(), String> {
    if in_process_exec(kind, verb, elevation()) {
        return shell_execute(file, args, dir);
    }
    desktop_shell_execute(file, args, dir, verb, SW_SHOWNORMAL.0).map_err(|e| format!("desktop shell: {e}"))
}

/// The launcher launch a request is; `None` for the shared requests (Start, the bar, notifications,
/// the power popup, the updater), whose failures never become a launcher notice.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launch_kind(req: &ShellRequest) -> Option<LaunchKind> {
    match req {
        ShellRequest::OpenPath { .. } => Some(LaunchKind::Path),
        ShellRequest::RunAsTyped { .. } => Some(LaunchKind::Admin),
        ShellRequest::LaunchCommand { .. } => Some(LaunchKind::Command),
        ShellRequest::RevealPath { .. } => Some(LaunchKind::Reveal),
        ShellRequest::OpenUrl { .. } => Some(LaunchKind::Url),
        ShellRequest::LauncherLaunch { .. } => Some(LaunchKind::App),
        ShellRequest::LauncherAppAdmin { .. } => Some(LaunchKind::Admin),
        ShellRequest::LauncherRevealApp { .. } => Some(LaunchKind::Reveal),
        _ => None,
    }
}

/// What a launcher launch pushes when it failed: `LaunchFailed` for a failed shell call only.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn failure_event(kind: LaunchKind, r: &Result<(), Fail>) -> Option<ShellEvent> {
    matches!(r, Err(Fail::Shell(_))).then_some(ShellEvent::LaunchFailed { kind })
}

/// Runs one launcher launch; a failure is logged (kind and error only), and pushed as `LaunchFailed`
/// when the shell call failed (`failure_event`).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_launch(ui: isize, req: ShellRequest) {
    let Some(kind) = launch_kind(&req) else { return };
    let mut exec =
        |file: &str, args: Option<&str>, dir: Option<&str>, verb: Verb| shell_exec(kind, file, args, dir, verb);
    let r = match req {
        ShellRequest::OpenPath { path } => {
            let r = open_path_with(&path, &mut exec);
            if r.is_ok() {
                log!("shell: OpenPath ({} chars) via the desktop shell", path.chars().count());
            }
            r
        }
        ShellRequest::RunAsTyped { line, file, gesture } => {
            let r = run_as_typed_with(&line, file, &gesture, &mut exec);
            if r.is_ok() {
                log!("shell: RunAsTyped ({} chars) via the desktop shell (runas, UAC prompt)", line.chars().count());
            }
            r
        }
        ShellRequest::LaunchCommand { line, file, console } => {
            let sys = system_dir().unwrap_or_default();
            let r = launch_command_with(&line, file, console, &sys, &mut exec);
            if r.is_ok() {
                log!("shell: LaunchCommand ({} chars) via the desktop shell", line.chars().count());
            }
            r
        }
        ShellRequest::RevealPath { path } => {
            let windows = system_dir().and_then(|s| s.parent().map(Path::to_path_buf)).unwrap_or_default();
            let r = reveal_path_with(&path, &windows, &mut exec);
            if r.is_ok() {
                log!("shell: RevealPath ({} chars) via the desktop shell", path.chars().count());
            }
            r
        }
        ShellRequest::OpenUrl { url } => {
            let r = open_url_with(&url, &mut exec);
            if r.is_ok() {
                log!("shell: OpenUrl ({} chars) via the desktop shell", url.chars().count());
            }
            r
        }
        // Catalogue ids and settings targets: these functions log as they do for Start, and use the dock's own
        // ShellExecute only when it is known not to be elevated (`in_process`).
        ShellRequest::LauncherLaunch { target, args } => {
            launch_with(&target, args.as_deref(), LaunchBy::Launcher).map_err(Fail::Shell)
        }
        ShellRequest::LauncherAppAdmin { id } => launch_app_admin(&id, LaunchBy::Launcher).map_err(Fail::Shell),
        ShellRequest::LauncherRevealApp { id } => reveal_app_with(&id, LaunchBy::Launcher).map_err(Fail::Shell),
        _ => return,
    };
    if let Err(err) = &r {
        let e = format!("{kind:?}: {err}");
        log!("shell: launcher launch not executed ({e})");
    }
    if let Some(ev) = failure_event(kind, &r) {
        push_event(ui, ev);
    }
}

/// `StartIconsFor` made here, when its icons child cannot start or gave no header: the icons of the launcher's rows,
/// made at `icon_px` (clamped as the Start menu's). Start's `start_px` and `start_sent` are not touched.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_icons(ui: isize, icon_px: u32, ids: Vec<String>) {
    COM_WORK.store(true, SeqCst);
    let icon_px = icon_px.clamp(8, 256);
    let icons = ids
        .into_iter()
        .map(|id| {
            let b = start_icon(&id, icon_px);
            (id, b)
        })
        .collect();
    push_event(ui, ShellEvent::LauncherIcons { icon_px, icons });
}

/// `LauncherConvert`: Windows' typo rewrite of a query that found nothing, read here so the UI thread
/// never reads the 1.4 MB file.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_convert(ui: isize, gen: u64, query: &str) {
    log!("shell: LauncherConvert ({} chars)", query.chars().count());
    let meant = crate::start_search::converted(query);
    push_event(ui, ShellEvent::LauncherConverted { gen, meant });
}

/// The launcher's history files, in the shell user's folder (`datadir::user_cache_dir`).
const STATE_FILES: [&str; 2] = ["launcher-frecency.txt", "launcher-run.txt"];
/// The largest history file read or written.
const STATE_MAX_BYTES: u64 = 65_536;

/// The SID of the shell's user: the token user of the process that owns the shell window (Explorer),
/// so under Administrator protection the history is the person's at the keyboard, not the account's
/// the elevated dock runs as. Only this SID's folder is ever opened.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn shell_user_sid() -> Result<String, String> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{TokenUser, TOKEN_USER};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::GetShellWindow;

    // SAFETY: no preconditions.
    let shell = unsafe { GetShellWindow() };
    if shell.0.is_null() {
        return Err("no shell window".into());
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is writable for the call; a stale window only yields 0.
    unsafe { GetWindowThreadProcessId(shell, Some(&mut pid)) };
    if pid == 0 {
        return Err("no shell process".into());
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

/// The history files in `dir`: (frecency, run); `None` for a file that is missing, over 64 KiB or not a
/// single-link regular file (opened without following reparse points).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn load_state_from(dir: &Path) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let read = |name: &str| match crate::datadir::read_file(&dir.join(name), STATE_MAX_BYTES, true) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log!("shell: a launcher history file is not used: {e}");
            None
        }
    };
    let [frecency, run] = STATE_FILES;
    (read(frecency), read(run))
}

/// Writes (`Some`) or leaves (`None`) each history file in `dir`, atomically and with the folder's
/// DACL; `delete` removes both instead.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn save_state_to(dir: &Path, frecency: Option<Vec<u8>>, run: Option<Vec<u8>>, delete: bool) -> Result<(), String> {
    if delete {
        for name in STATE_FILES {
            match std::fs::remove_file(dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("removing a history file: {e}")),
            }
        }
        return Ok(());
    }
    let [f_name, r_name] = STATE_FILES;
    for (name, bytes) in [(f_name, frecency), (r_name, run)] {
        let Some(bytes) = bytes else { continue };
        if bytes.len() as u64 > STATE_MAX_BYTES {
            return Err("a history file over 64 KiB".into());
        }
        crate::datadir::write_atomic(&dir.join(name), &bytes, crate::datadir::FileAcl::Inherit)?;
    }
    Ok(())
}

/// `LauncherStateLoad`: the shell user's SID and history files -> `ShellEvent::LauncherState`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_state_load(ui: isize) {
    let sid = match shell_user_sid() {
        Ok(sid) => Some(sid),
        Err(e) => {
            log!("shell: LauncherStateLoad: no shell user ({e})");
            None
        }
    };
    let (frecency, run) = match sid.as_deref().map(crate::datadir::user_cache_dir) {
        Some(Ok(dir)) => load_state_from(&dir),
        Some(Err(e)) => {
            log!("shell: LauncherStateLoad: no history folder ({e})");
            (None, None)
        }
        None => (None, None),
    };
    push_event(ui, ShellEvent::LauncherState { sid, frecency, run });
}

/// `LauncherStateSave`: into the shell user's folder, nowhere else.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_state_save(frecency: Option<Vec<u8>>, run: Option<Vec<u8>>, delete: bool) {
    let r = shell_user_sid()
        .and_then(|sid| crate::datadir::user_cache_dir(&sid))
        .and_then(|dir| save_state_to(&dir, frecency, run, delete));
    if let Err(e) = r {
        log!("shell: LauncherStateSave not done ({e})");
    }
}

/// `OpenPath`: refuses an empty path or one with a control character, then exactly one shell call
/// with the path as it is: no `updated_exe_path`, no `is_dir`, no working directory, so nothing
/// the launcher found or the user typed touches the file system from the elevated process.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn open_path_with(path: &str, exec: Exec) -> Result<(), Fail> {
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err(Fail::Refused("an empty path or a control character"));
    }
    exec(path, None, None, Verb::Open).map_err(Fail::Shell)
}

/// The file and the arguments of a run line, split where the launcher's syntax-only classification
/// put the file (`file`, a byte range of `line`): the arguments are the trimmed text after the file
/// (after its closing quote when it was quoted). Only the opening quote may stand before the file, and
/// the file must end at a word break or at its closing quote; anything else (a range out of bounds or
/// off a character boundary included) is refused, never a panic. With `console` the file is
/// `<sys>\cmd.exe` and the arguments `/s /k "<the whole trimmed line>"`: with /s cmd removes exactly that
/// outer pair of quotes and runs the line unchanged (without /s, a line that starts with a quote and holds
/// more than two loses its first and last quote). Nothing is looked up or expanded.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn command_parts(
    line: &str,
    file: Range<usize>,
    console: bool,
    sys: &Path,
) -> Result<(String, Option<String>), &'static str> {
    command_parts_with(line, file, console.then_some("/k"), sys)
}

/// `command_parts` with the switch of the cmd.exe wrapper: `/k` (a console kept open), `/c` (cmd expands the %VAR% of
/// the line, runs it and closes), or none (the line is split as typed).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn command_parts_with(
    line: &str,
    file: Range<usize>,
    wrapper: Option<&str>,
    sys: &Path,
) -> Result<(String, Option<String>), &'static str> {
    let part = line.get(file.clone()).ok_or("the file range does not split the line")?;
    if part.trim().is_empty() {
        return Err("an empty file");
    }
    let quoted = match line.get(..file.start).map(str::trim_start) {
        Some("") => false,
        Some("\"") => true,
        _ => return Err("text before the file"),
    };
    let rest = line.get(file.end..).ok_or("the file range does not split the line")?;
    let rest = match (quoted, rest.strip_prefix('"')) {
        (true, Some(after)) => after,
        (true, None) if rest.is_empty() => rest,
        (false, _) if rest.is_empty() || rest.starts_with(char::is_whitespace) => rest,
        _ => return Err("the file does not end where its range ends"),
    };
    if let Some(switch) = wrapper {
        if !sys.is_absolute() {
            return Err("no system directory");
        }
        // A file that is the whole line goes quoted when cmd would split it (a drive path with spaces) or its %VAR%
        // may expand to a space; a bare word (dir, set) stays as typed, as cmd runs no quoted built-in. /c hands the
        // line to cmd's `start`, which resolves App Paths; no attachment prompt is promised for such a line, as at a command
        // prompt (owner's decision, SPEC 5.4.9).
        let t = line.trim();
        let split = !t.contains('"') && t.contains(|c: char| c.is_whitespace() || "&|<>^(),;=%".contains(c));
        let body = if !quoted && rest.is_empty() && split { format!("\"{t}\"") } else { t.to_string() };
        let start = if switch == "/c" { "start \"\" " } else { "" };
        let args = format!("/s {switch} \"{start}{body}\"");
        return Ok((sys.join("cmd.exe").to_string_lossy().into_owned(), Some(args)));
    }
    let args = rest.trim();
    Ok((part.to_string(), (!args.is_empty()).then(|| args.to_string())))
}

/// `RunAsTyped`: the line split by `command_parts` and run by the desktop shell with `runas`, whether or
/// not the dock is elevated (as `launch_admin`), so Windows' UAC prompt names the program and the dock's
/// token is never used. Refused here again, whatever the launcher checked: any control or bidi
/// character in the line (what is shown is what runs), a range that does not split it, a network file
/// (`gesture::is_network_file`: two leading separators in any mix of `\` and `/`, `\??\` or a URL; a mapped
/// network drive letter cannot be told from a local one by syntax). `_proof` is the hook-proven gesture the request
/// carries: nothing reaches this without one.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn run_as_typed_with(line: &str, file: Range<usize>, _proof: &Gesture, exec: Exec) -> Result<(), Fail> {
    if line.chars().any(is_marker) {
        return Err(Fail::Refused("a control or bidi character"));
    }
    let (file, args) = command_parts(line, file, false, Path::new("")).map_err(Fail::Refused)?;
    if is_network_file(&file) {
        return Err(Fail::Refused("a network file"));
    }
    exec(&file, args.as_deref(), None, Verb::RunAs).map_err(Fail::Shell)
}

/// `LaunchCommand`: the line split by `command_parts` (with `console`: `<sys>\cmd.exe /s /k "<line>"`; a line with a
/// %VAR%, which the desktop shell does not expand, `<sys>\cmd.exe /s /c "start "" <line>"`: cmd expands it in the user's
/// context, `start` hands it to ShellExecuteEx (App Paths; no attachment prompt is promised, SPEC 5.4.9) and cmd closes
/// at once; an address is never wrapped), one shell call with no working directory. Nothing is
/// expanded or looked up here: the line goes to the shell as typed.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launch_command_with(line: &str, file: Range<usize>, console: bool, sys: &Path, exec: Exec) -> Result<(), Fail> {
    let wrapper = if console { Some("/k") } else { crate::launcher::runline::expands_in_cmd(line).then_some("/c") };
    // cmd reads the whole line (its & | > and %): what is shown is what runs, so no control or bidi character in it.
    if wrapper.is_some() && line.chars().any(is_marker) {
        return Err(Fail::Refused("a control or bidi character"));
    }
    let (file, args) = command_parts_with(line, file, wrapper, sys).map_err(Fail::Refused)?;
    exec(&file, args.as_deref(), None, Verb::Open).map_err(Fail::Shell)
}

/// `RevealPath`: refuses an empty path or one with a double quote (it would end the `/select`
/// argument) or a control character (as `app_target_path`), then exactly one shell call:
/// `<windows>\explorer.exe /select,"<path>"`. Not through `launch`, whose `is_dir` would touch the path.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn reveal_path_with(path: &str, windows: &Path, exec: Exec) -> Result<(), Fail> {
    if path.trim().is_empty() || path.contains('"') || path.chars().any(char::is_control) {
        return Err(Fail::Refused("an empty path, a double quote or a control character"));
    }
    if !windows.is_absolute() {
        return Err(Fail::Refused("no Windows folder"));
    }
    let explorer = windows.join("explorer.exe");
    exec(&explorer.to_string_lossy(), Some(&format!("/select,\"{path}\"")), None, Verb::Open).map_err(Fail::Shell)
}

/// `OpenUrl`'s check on the shell thread, whatever the launcher checked: an http or https address
/// rebuilt from its parts, so only what was checked reaches the shell. The scheme in lowercase; the
/// host part as `web::check_authority` accepts it (the launcher offers a typed URL only after the same
/// check): letters, digits, hyphens and dots (no empty label), an optional numeric port, no user name
/// or password before the host ("https://bank.it@evil.example" reads as one host and is another);
/// then the path, query and fragment with every character that is not URL-safe ASCII percent-encoded
/// as UTF-8, so a quote or a bracket can never end the address inside a browser's command line.
/// Whitespace and control characters are refused.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn rebuild_url(url: &str) -> Result<String, &'static str> {
    const KEEP: &str = "-._~:/?#[]@!$&'()*+,;=%";
    if url.len() > MAX_URL_BYTES {
        return Err("too long");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("whitespace or a control character");
    }
    let (scheme, rest) = url.split_once("://").ok_or("no http or https scheme")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("no http or https scheme");
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (Some(authority), Some(tail)) = (rest.get(..end), rest.get(end..)) else {
        return Err("no host");
    };
    check_authority(authority)?;
    let mut out = String::with_capacity(url.len() + 16);
    out.push_str(&scheme);
    out.push_str("://");
    out.push_str(authority);
    for c in tail.chars() {
        if c.is_ascii_alphanumeric() || KEEP.contains(c) {
            out.push(c);
            continue;
        }
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).bytes() {
            out.push('%');
            for nibble in [b >> 4, b & 15] {
                out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0').to_ascii_uppercase());
            }
        }
    }
    Ok(out)
}

/// `OpenUrl`: the address rebuilt by `rebuild_url`, in one shell call.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn open_url_with(url: &str, exec: Exec) -> Result<(), Fail> {
    let url = rebuild_url(url).map_err(Fail::Refused)?;
    exec(&url, None, None, Verb::Open).map_err(Fail::Shell)
}

// launcher requests: end

/// Owned `VARIANT` that is released with `VariantClear` on drop.
struct OwnedVariant(VARIANT);

impl OwnedVariant {
    /// `VT_EMPTY` (an omitted optional argument).
    fn empty() -> Self {
        OwnedVariant(VARIANT::default())
    }

    fn i4(n: i32) -> Self {
        let mut v = VARIANT::default();
        // SAFETY: `v` is a zeroed VARIANT (VT_EMPTY); writing the type tag and the matching union
        // member produces a valid VT_I4 VARIANT.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = n;
        }
        OwnedVariant(v)
    }

    fn bstr(s: &str) -> Self {
        let mut v = VARIANT::default();
        // SAFETY: as in `i4`; the BSTR is owned by the VARIANT and freed by VariantClear in `drop`.
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_BSTR;
            inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(s));
        }
        OwnedVariant(v)
    }

    fn opt_bstr(s: Option<&str>) -> Self {
        s.map_or_else(Self::empty, Self::bstr)
    }
}

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid VARIANT built by the constructors above and owned by us.
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

/// true if Explorer's desktop is registered with `ShellWindows`, i.e. `desktop_shell_execute` can
/// reach it. A freshly (re)started Explorer registers it some seconds after its taskbar appears.
/// Needs COM initialized on the calling thread.
pub(crate) fn desktop_registered() -> bool {
    // SAFETY: COM initialized by the caller; the interface is released when it goes out of scope and
    // the VARIANTs outlive the call.
    unsafe {
        let Ok(windows) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_LOCAL_SERVER) else {
            return false;
        };
        let loc = OwnedVariant::i4(CSIDL_DESKTOP as i32);
        let root = OwnedVariant::empty();
        let mut hwnd = 0i32;
        windows.FindWindowSW(&loc.0, &root.0, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH).is_ok()
    }
}

/// Runs `file` through the desktop shell (Explorer, at the user's medium integrity), so the new
/// process is NOT elevated even when the dock is:
/// `IShellWindows::FindWindowSW(CSIDL_DESKTOP)` -> `IServiceProvider::QueryService(SID_STopLevelBrowser)`
/// -> `IShellBrowser::QueryActiveShellView` -> `IShellView::GetItemObject(SVGIO_BACKGROUND)`
/// -> `IShellFolderViewDual::Application` -> `IShellDispatch2::ShellExecute(..., verb, show)`
/// (`SW_SHOWNORMAL` except the hidden PowerShell export, see `desktop_open_hidden`).
/// `verb` is `Open` for every launch except `launch_admin` / `launch_app_admin` / `run_as_typed_with`
/// (`RunAs`: Explorer asks UAC, not the dock).
/// Before executing, grants foreground rights to Explorer so the launched app opens in the foreground.
/// Must be called on a COM STA thread (the shell thread).
fn desktop_shell_execute(
    file: &str,
    args: Option<&str>,
    dir: Option<&str>,
    verb: Verb,
    show: i32,
) -> windows::core::Result<()> {
    let (desktop, hwnd) = find_desktop()?;
    // SAFETY: COM STA initialized on the shell thread; every interface below is used only on this
    // thread and released when it goes out of scope. The VARIANTs passed by pointer outlive the calls.
    unsafe {
        let sp: IServiceProvider = desktop.cast()?;
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
        let view = browser.QueryActiveShellView()?;
        let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
        let folder_view: IShellFolderViewDual = background.cast()?;
        let shell: IShellDispatch2 = folder_view.Application()?.cast()?;
        // Foreground rights: the dock received the click, Explorer did not. Without this the
        // de-elevated app (started by Explorer) opens behind the current foreground window.
        // Grant them to Explorer (desktop window owner), which passes them on to the process it
        // starts; ASFW_ANY as a fallback. Both expire at the next user input.
        let mut explorer_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut explorer_pid));
        if explorer_pid == 0 || AllowSetForegroundWindow(explorer_pid).is_err() {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        let _ = CoAllowSetForegroundWindow(&shell, None);
        shell.ShellExecute(
            &BSTR::from(file),
            &OwnedVariant::opt_bstr(args).0,
            &OwnedVariant::opt_bstr(dir).0,
            &OwnedVariant::bstr(verb.as_str()).0,
            &OwnedVariant::i4(show).0,
        )
    }
}

/// Explorer's desktop (`IShellWindows::FindWindowSW(CSIDL_DESKTOP)`) and its window.
/// Must be called on a COM STA thread.
fn find_desktop() -> windows::core::Result<(IDispatch, HWND)> {
    // SAFETY: COM STA initialized by the caller; the interfaces are released when they go out of
    // scope and the VARIANTs outlive the call.
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
        let loc = OwnedVariant::i4(CSIDL_DESKTOP as i32);
        let root = OwnedVariant::empty();
        let mut hwnd = 0i32;
        let desktop: IDispatch = windows.FindWindowSW(&loc.0, &root.0, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        Ok((desktop, HWND(hwnd as isize as *mut c_void)))
    }
}

/// Pid of the Explorer process that owns the desktop window. Must be called on a COM STA thread.
pub(crate) fn explorer_pid() -> windows::core::Result<u32> {
    let (_, hwnd) = find_desktop()?;
    let mut pid = 0u32;
    // SAFETY: `pid` is writable for the call; a stale `hwnd` only yields 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return Err(windows::Win32::Foundation::E_FAIL.into());
    }
    Ok(pid)
}

/// `file` opened through the desktop shell, de-elevated (see `desktop_shell_execute`), with
/// `SW_HIDE`: no console window for the PowerShell export.
/// Must be called on a COM STA thread.
pub(crate) fn desktop_open_hidden(file: &str, args: Option<&str>, dir: Option<&str>) -> windows::core::Result<()> {
    desktop_shell_execute(file, args, dir, Verb::Open, SW_HIDE.0)
}

/// `System32` from `GetSystemDirectoryW` (never from an environment variable).
pub(crate) fn system_dir() -> Option<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is writable and its length is passed by the wrapper.
    let n = unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

// ------------------------------------------------------------------ show desktop

/// "Show desktop": `IShellDispatch4::ToggleDesktop`; if it fails, synthetic Win+D.
fn toggle_desktop() {
    // SAFETY: COM STA initialized on the shell thread; Shell CLSID with the requested interface.
    let sd = unsafe { CoCreateInstance::<_, IShellDispatch4>(&Shell, None, CLSCTX_ALL) };
    // SAFETY: `sd` is a valid COM object, used on the thread that created it.
    let r = sd.and_then(|sd| unsafe { sd.ToggleDesktop() });
    if let Err(e) = r {
        log!("shell: ToggleDesktop COM failed: {e}, using Win+D");
        // Plain SendInput (no UI-thread state): safe to call from the shell thread.
        crate::ui::send_win_key(VIRTUAL_KEY(0x44));
    }
}

/// Win pressed and released alone with `dwExtraInfo = INJECT_TAG`: the keyboard hook lets it through and
/// Windows toggles the Start menu (the physical Win key is swallowed by the hook, it only toggles the dock).
fn send_win_alone() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0,
                dwFlags: flags | KEYEVENTF_EXTENDEDKEY,
                time: 0,
                dwExtraInfo: crate::hook::INJECT_TAG,
            },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    // SAFETY: `inputs` is a valid array of INPUT with the declared size.
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        log!("shell: SendInput Win: sent {n}/{}", inputs.len());
    }
}

/// Plain `ShellExecuteExW` (default verb). Used ONLY when the dock is not elevated.
fn shell_execute(file: &str, params: Option<&str>, dir: Option<&str>) -> Result<(), String> {
    let file_w = HSTRING::from(file);
    let params_w = params.map(HSTRING::from);
    let dir_w = dir.map(HSTRING::from);
    let opt = |h: &Option<HSTRING>| h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: opt(&params_w),
        lpDirectory: opt(&dir_w),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `sei` initialized with the correct cbSize; the strings (NUL-terminated HSTRINGs) live
    // until the end of the function, which returns after ShellExecuteExW.
    unsafe { ShellExecuteExW(&mut sei) }.map_err(|e| format!("ShellExecuteExW: {e}"))
}

/// Puts a request in the queue. A newer launcher icon request or typo rewrite replaces the pending one:
/// only the latest keystroke's rows matter; the launcher's close drops its pending icon request.
fn enqueue(pending: &mut VecDeque<ShellRequest>, r: ShellRequest) {
    match r {
        ShellRequest::StartIconsFor { .. } | ShellRequest::LauncherClosed => {
            pending.retain(|p| !matches!(p, ShellRequest::StartIconsFor { .. }))
        }
        ShellRequest::LauncherConvert { .. } => pending.retain(|p| !matches!(p, ShellRequest::LauncherConvert { .. })),
        _ => {}
    }
    pending.push_back(r);
}

/// The next request to run: the oldest one that is not a slow name/icon resolution or a Start menu icon
/// batch, otherwise the oldest of those.
fn next_request(pending: &mut VecDeque<ShellRequest>) -> Option<ShellRequest> {
    let idx = pending
        .iter()
        .position(|r| !matches!(r, ShellRequest::ResolveApp(_) | ShellRequest::StartIcons { .. }))
        .or(if pending.is_empty() { None } else { Some(0) });
    idx.and_then(|i| pending.remove(i))
}

/// The Start menu's part of `ListStartApps`, over the catalogue's `ids` (the arena's order, by name).
/// `IconPlan::All`: the icons not sent yet at `icon_px`, the pins (`first`) first, marked as sent and
/// returned (`Shell::start_menu_icons` has them made); a new size starts over and drops the batches
/// queued at the old one. `IconPlan::None` (the launcher's catalogue) touches nothing: `None`.
fn missing_start_icons<'a>(
    start_px: &mut u32,
    sent: &mut HashSet<String>,
    pending: &mut VecDeque<ShellRequest>,
    ids: impl IntoIterator<Item = &'a str>,
    icon_px: u32,
    plan: &IconPlan,
) -> Option<Vec<String>> {
    let IconPlan::All { first } = plan else { return None };
    let icon_px = icon_px.clamp(8, 256);
    if icon_px != *start_px {
        *start_px = icon_px;
        sent.clear();
        pending.retain(|r| !matches!(r, ShellRequest::StartIcons { .. }));
    }
    // Pins first; the catalogue order is kept inside each group (a stable partition: the key is a bool, so no sort).
    let is_pin = |id: &str| first.iter().any(|f| f.eq_ignore_ascii_case(id));
    let (mut missing, rest): (Vec<String>, Vec<String>) =
        ids.into_iter().filter(|id| !sent.contains(*id)).map(str::to_string).partition(|id| is_pin(id));
    missing.extend(rest);
    sent.extend(missing.iter().cloned());
    Some(missing)
}

/// `StartRelease`: a reopened Start menu starts from nothing, so the next `ListStartApps` resends every icon.
fn start_release(start_px: &mut u32, sent: &mut HashSet<String>, pending: &mut VecDeque<ShellRequest>) {
    *start_px = 0;
    sent.clear();
    pending.retain(|r| !matches!(r, ShellRequest::StartIcons { .. }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_launch_logs_never_name_the_pick() {
        let (t, a, id) = ("C:\\Secret\\pickme.exe", "--arg-secret", "Vendor.Secret!App");
        let lines = [
            launch_line(LaunchBy::Launcher, t, Some(a), ""),
            launch_line(LaunchBy::Launcher, t, Some(a), " via the desktop shell (de-elevated)"),
            launch_refused_line(LaunchBy::Launcher, t, "0x80004005"),
            app_admin_line(LaunchBy::Launcher, id, Ok(())),
            app_admin_line(LaunchBy::Launcher, id, Err("0x80004005")),
            reveal_line(LaunchBy::Launcher, id, None, ""),
            reveal_line(LaunchBy::Launcher, id, Some(t), "0x80004005"),
        ];
        for l in &lines {
            for n in ["Secret", "pickme", "arg-secret", "Vendor"] {
                assert!(!l.contains(n), "{l}");
            }
        }
        assert!(lines[0].contains("(20 chars)"), "{}", lines[0]);
        // The shared requests keep 1.4's text.
        assert!(launch_line(LaunchBy::Shared, t, None, "").contains("pickme"));
        assert!(app_admin_line(LaunchBy::Shared, id, Ok(())).contains("Vendor"));
    }

    #[test]
    fn start_release_makes_a_reopen_resend_everything() {
        let mut px = 48u32;
        let mut sent: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        pending.push_back(ShellRequest::StartIcons { icon_px: 48, ids: vec!["c".into()] });
        pending.push_back(ShellRequest::VolumeRefresh);
        start_release(&mut px, &mut sent, &mut pending);
        assert_eq!((px, sent.len(), pending.len()), (0, 0, 1));
        assert!(matches!(pending[0], ShellRequest::VolumeRefresh));
    }

    #[test]
    fn admin_tools_launch_by_absolute_path_or_aumid() {
        let sys = std::path::Path::new(r"C:\Windows\System32");
        assert_eq!(AdminTool::Terminal.target(sys), r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App");
        let s = AdminTool::Terminal.target(sys);
        assert!(s.contains('\\'), "never a bare executable name: {s}");
        assert!(!s.eq_ignore_ascii_case("wt.exe") && !s.eq_ignore_ascii_case("powershell.exe"));
    }

    #[test]
    fn words_and_tokens() {
        assert_eq!(split_words("RazerAppEngine"), ["Razer", "App", "Engine"]);
        assert_eq!(split_words("stremio-shell-ng"), ["stremio", "shell", "ng"]);
        assert_eq!(split_words("HTTPServer"), ["HTTP", "Server"]);
    }

    #[test]
    fn description_choice() {
        let pick = |d, p, c, s| pick_description(d, p, c, s);
        // Tagline rejected, nothing else usable.
        assert_eq!(
            pick(Some("Freedom to Stream"), Some("stremio-shell-ng"), None, "stremio-shell-ng"),
            Some("stremio-shell-ng".into())
        );
        // Dotted identifier loses against the product name.
        assert_eq!(
            pick(Some("PowerToys.QuickAccess"), Some("PowerToys"), None, "PowerToys.QuickAccess"),
            Some("PowerToys".into())
        );
        // Generic tails stripped, shorter nested name wins.
        assert_eq!(pick(Some("Steam Client WebHelper"), Some("Steam"), None, "steamwebhelper"), Some("Steam".into()));
        assert_eq!(
            pick(Some("Steam Client Bootstrapper"), Some("Steam"), Some("Valve"), "steam"),
            Some("Steam".into())
        );
        assert_eq!(pick(Some("Discord"), Some("Discord"), Some("Discord Inc."), "Discord"), Some("Discord".into()));
        assert_eq!(pick(Some("Freedom to Stream"), None, None, "stremio-shell-ng"), None);
    }

    #[test]
    fn start_menu_filter() {
        assert!(keep_start_app("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calculator"));
        assert!(keep_start_app("{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe", "Git Bash"));
        assert!(!keep_start_app("{7C5A40EF}\\App\\unins000.exe", "Uninstall App"));
        assert!(!keep_start_app("x", "Disinstalla Foo"));
        assert!(!keep_start_app("https://example.com/", "Example website"));
        assert!(!keep_start_app("id", "  "));
    }

    #[test]
    fn notification_app_lookup() {
        let app = |id: &str, name: &str| StartApp { id: id.into(), name: name.into() };
        let apps = [
            app("308046B0AF4A39CB", "Firefox"),
            app("Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows", "Outlook"),
            app("{6D809377}\\Telegram\\Telegram.exe", "Telegram"),
        ];
        let outlook = "microsoft.outlookforwindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows";
        assert_eq!(find_notif_app(&apps, outlook, "Outlook (new)"), Some(1));
        assert_eq!(find_notif_app(&apps, "FirefoxToast-308046B0AF4A39CB", ""), Some(0));
        assert_eq!(find_notif_app(&apps, "Telegram.TelegramDesktop", "telegram"), Some(2));
        assert_eq!(find_notif_app(&apps, "Some-App", "Unknown"), None);
        assert_eq!(find_notif_app(&apps, "", ""), None);
    }

    /// The index routes every per-user link to the Rust parser and every all-users one to the shell.
    #[test]
    fn per_user_links_never_reach_the_shell_loader() {
        let l = |p: &str, per_user: bool| StartLnk { path: p.into(), per_user };
        let files = [
            l(r"C:\All\Tool.lnk", false),
            l(r"C:\Me\Mine.lnk", true),
            l(r"C:\All\Other (x64).lnk", false),
            l(r"C:\Me\Broken.lnk", true),
            l(r"C:\All\Uninstall Tool.lnk", false),
        ];
        let mut rust_calls = Vec::new();
        let mut rust = |s: &StartLnk| {
            rust_calls.push(s.path.clone());
            s.path.ends_with("Mine.lnk").then(|| r"C:\Me\mine.exe".to_string())
        };
        let mut com_target = |s: &StartLnk| {
            assert!(!s.per_user, "{:?} reached the shell link loader", s.path);
            Some(format!(r"C:\All\{}.exe", s.path.file_stem()?.to_string_lossy()))
        };
        let mut com_display = |s: &StartLnk| {
            assert!(!s.per_user, "{:?} reached the shell's display name", s.path);
            s.path.ends_with("Tool.lnk").then(|| "Strumento".to_string())
        };
        let (map, stats) = index_links(&files, &mut rust, &mut com_target, &mut com_display);
        assert_eq!(rust_calls, [std::path::PathBuf::from(r"C:\Me\Mine.lnk"), r"C:\Me\Broken.lnk".into()]);
        assert_eq!(stats, IndexStats { rust_parsed: 1, rust_unresolved: 1, com_common: 3 });
        assert_eq!(map.get(r"c:\me\mine.exe").map(String::as_str), Some("Mine"), "per-user: the file name");
        assert_eq!(map.get(r"c:\all\tool.exe").map(String::as_str), Some("Strumento"), "the shell's name");
        assert_eq!(map.get(r"c:\all\other (x64).exe").map(String::as_str), Some("Other"));
        assert_eq!(map.len(), 3, "the uninstaller is left out: {map:?}");
    }

    #[test]
    fn com_wrapper_refuses_per_user() {
        let before = PER_USER_VIA_COM.load(Relaxed);
        assert!(com_guard(true), "a per-user shortcut is refused");
        assert!(PER_USER_VIA_COM.load(Relaxed) > before, "and counted");
        assert!(!com_guard(false), "an all-users shortcut passes");
    }

    /// Compares the Rust parser with the shell link loader on a corpus of shortcuts, sub-folder by
    /// sub-folder (live check L6, unelevated: `DOCK_LNK_CORPUS=<dir> cargo test -- --ignored
    /// --nocapture lnk_matches_ishelllink`). A mismatch is printed as `<lnk> | rust=<a> | com=<b>`.
    #[test]
    #[ignore]
    fn lnk_matches_ishelllink() {
        let corpus = std::path::PathBuf::from(std::env::var_os("DOCK_LNK_CORPUS").expect("DOCK_LNK_CORPUS"));
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        let shown = |s: &str| match s.get(..profile.len()) {
            Some(head) if !profile.is_empty() && head.eq_ignore_ascii_case(&profile) => {
                format!("%USERPROFILE%{}", &s[profile.len()..])
            }
            _ => s.to_string(),
        };
        // SAFETY: COM STA for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // SAFETY: COM STA initialized above; ShellLink CLSID with the requested interface.
        let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.expect("ShellLink");
        let file = link.cast::<IPersistFile>().expect("IPersistFile");
        let mut dirs: Vec<_> = std::fs::read_dir(&corpus)
            .expect("corpus folder")
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        dirs.sort();
        let (mut mismatches, mut total) = (0, 0);
        for dir in &dirs {
            let mut files = Vec::new();
            let mut left = usize::MAX;
            collect_lnk(dir, 0, false, &mut left, &mut files);
            if files.is_empty() {
                continue;
            }
            total += files.len();
            let (mut matched, mut parser_none, mut mismatch) = (0, 0, 0);
            for l in &files {
                let rust = crate::jumplist::read_lnk(&l.path)
                    .filter(|f| !f.has_args)
                    .map(|f| expand_env(&f.target))
                    .filter(|t| crate::jumplist::is_local_file_path(t));
                let com = lnk_target(&link, &file, &l.path).map(|t| expand_env(&t));
                match (&rust, &com) {
                    (Some(a), Some(b)) if a.to_lowercase() == b.to_lowercase() => matched += 1,
                    (None, None) => matched += 1,
                    (None, Some(_)) => parser_none += 1,
                    _ => {
                        mismatch += 1;
                        let (a, b) = (rust.as_deref().unwrap_or("-"), com.as_deref().unwrap_or("-"));
                        println!("{} | rust={} | com={}", shown(&l.path.to_string_lossy()), shown(a), shown(b));
                    }
                }
            }
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let n = files.len();
            println!("{name}: matched {matched}, parser-none {parser_none}, mismatch {mismatch}, total {n}");
            mismatches += mismatch;
        }
        drop((file, link));
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above; the COM objects are dropped.
            unsafe { CoUninitialize() };
        }
        assert!(total > 0, "no shortcut in the sub-folders of {}", corpus.display());
        assert_eq!(mismatches, 0);
    }

    /// Reads the REAL Start Menu shortcuts of this machine: the bar name of a Windows program is the
    /// one the Start menu shows (localized), not the `.lnk` file name.
    #[test]
    #[ignore]
    fn start_index_uses_the_shell_display_name() {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let map = build_start_index();
        let apps = enum_apps_folder();
        let sys = system_dir().expect("system dir");
        let name = map.get(&norm_path(&sys.join("mstsc.exe").to_string_lossy())).expect("mstsc shortcut");
        println!("mstsc.exe -> '{name}'");
        assert!(apps.iter().any(|a| &a.name == name), "same name as the Start menu catalog");
        assert!(!name.to_lowercase().ends_with(".lnk"));
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }

    /// Reads the REAL AppsFolder of this machine (run by hand: `cargo test -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn apps_folder_lists_real_apps() {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let t = Instant::now();
        let apps = enum_apps_folder();
        let list_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let probe: Vec<&StartApp> = apps.iter().take(40).collect();
        let with_icon = probe.iter().filter(|a| start_icon(&a.id, 32).is_some_and(|b| b.width == 32)).count();
        println!(
            "AppsFolder: {} apps in {list_ms} ms; icons {with_icon}/{} in {} ms",
            apps.len(),
            probe.len(),
            t.elapsed().as_millis()
        );
        let store = apps.iter().filter(|a| a.id.contains('!')).count();
        println!("packaged (Store) apps: {store}, desktop: {}", apps.len() - store);
        for a in apps.iter().take(8) {
            println!("  {} -> {}", a.name, a.id);
        }
        assert!(apps.len() > 10, "too few apps: {}", apps.len());
        assert!(store > 0, "no Store app found");
        assert!(with_icon * 10 >= probe.len() * 8, "too many apps without an icon");
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }

    /// L3 T17: the apps arena of this machine's catalogue, as the `--start-apps` child makes it (run by hand:
    /// `cargo test -- --ignored --nocapture real_apps_arena_size_printed`). Prints sizes and times only, no name.
    #[test]
    #[ignore = "reads this machine's AppsFolder and app index"]
    fn real_apps_arena_size_printed() {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let t = Instant::now();
        let list = enum_apps_folder();
        let listed = t.elapsed();
        let t = Instant::now();
        crate::start_search::load_app_index();
        let index = crate::start_search::take_app_index();
        let read = t.elapsed();
        let t = Instant::now();
        let bytes = catalog::encode_app_list(&list, index.as_ref()).expect("the catalogue encodes");
        let encoded = t.elapsed();
        let mut checks: Vec<Duration> = (0..51)
            .map(|_| {
                let t = Instant::now();
                assert!(apps_from_child(Ok((0, bytes.clone()))).is_ok());
                t.elapsed()
            })
            .collect();
        checks.sort();
        let a = apps_from_child(Ok((0, bytes.clone()))).expect("the child's bytes check");
        let v = a.view();
        let start_keys: usize = (0..v.len()).map(|i| v.start_keys(i).len()).sum();
        println!(
            "{} apps: listed in {listed:?}, index read in {read:?}, encoded in {encoded:?}; arena {} B ({start_keys} B of \
             Start's synonyms), checked in {:?} (median of 51, with the copy)",
            v.len(),
            a.bytes(),
            checks[25]
        );
        assert!(v.len() <= list.len() && v.len() > 10, "{} of {}", v.len(), list.len());
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }

    #[test]
    fn stems_and_suffixes() {
        assert_eq!(pretty_stem("stremio-shell-ng"), "Stremio");
        assert_eq!(pretty_stem("zen"), "Zen");
        assert_eq!(strip_paren_suffix("PowerToys (Preview)"), "PowerToys");
        assert_eq!(strip_paren_suffix("Steam"), "Steam");
        assert_eq!(expand_env("%NOPE_X%\\a"), "%NOPE_X%\\a");
    }

    #[test]
    fn system_dir_comes_from_the_api() {
        let d = system_dir().expect("GetSystemDirectoryW");
        assert!(d.is_absolute() && d.join("kernel32.dll").is_file());
    }

    // ---- launcher requests

    type Call = (String, Option<String>, Option<String>, Verb);

    /// A fake desktop shell: records every call, succeeds or fails as told, starts nothing.
    fn recorder(
        calls: &mut Vec<Call>,
        ok: bool,
    ) -> impl FnMut(&str, Option<&str>, Option<&str>, Verb) -> Result<(), String> + '_ {
        move |f, a, d, v| {
            calls.push((f.to_string(), a.map(str::to_string), d.map(str::to_string), v));
            if ok {
                Ok(())
            } else {
                Err("0x80004005".to_string())
            }
        }
    }

    #[test]
    fn open_path_is_one_shell_call() {
        let mut calls = Vec::new();
        assert!(open_path_with("C:\\x y\\a.txt", &mut recorder(&mut calls, true)).is_ok());
        assert_eq!(calls, [("C:\\x y\\a.txt".to_string(), None, None, Verb::Open)]);
    }

    #[test]
    fn open_path_refuses_controls() {
        let mut calls = Vec::new();
        for bad in ["", "   ", "C:\\a\nb.txt", "C:\\a\u{0}.txt", "C:\\a\u{85}.txt", "\u{7F}"] {
            assert!(open_path_with(bad, &mut recorder(&mut calls, true)).is_err(), "{bad:?}");
        }
        assert!(calls.is_empty(), "a refused path reaches no shell call: {calls:?}");
    }

    #[test]
    fn com_work_requests_plan_the_second_pass() {
        let resolve = ResolveApp {
            key: "k".into(),
            exe_path: String::new(),
            aumid: Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()),
            hwnd: 0,
            icon_px: 30,
            custom_icon: None,
        };
        // A new bar entry's name and icon, every launch through the desktop shell, and an in-process listing.
        for r in [
            ShellRequest::ResolveApp(resolve),
            ShellRequest::Launch { target: "shell:AppsFolder\\x!App".into(), args: None },
            ShellRequest::LaunchAdmin(AdminTool::Terminal),
            ShellRequest::LaunchAppAdmin { id: "x!App".into() },
            ShellRequest::RevealApp { id: "x!App".into() },
            ShellRequest::LauncherLaunch { target: "shell:AppsFolder\\x!App".into(), args: None },
            ShellRequest::OpenUrl { url: "https://example.org".into() },
            ShellRequest::ToggleDesktop,
            ShellRequest::ListStartApps { icon_px: 30, icons: IconPlan::None, child: false },
            ShellRequest::OpenPath { path: "C:\\x.txt".into() },
            ShellRequest::RunAsTyped { line: "x".into(), file: 0..1, gesture: Gesture::for_test(1) },
            ShellRequest::LaunchCommand { line: "x".into(), file: 0..1, console: false },
            ShellRequest::RevealPath { path: "C:\\x.txt".into() },
            ShellRequest::LauncherAppAdmin { id: "x!App".into() },
            ShellRequest::LauncherRevealApp { id: "x!App".into() },
            ShellRequest::OpenNotifApp { aumid: "x!App".into(), name: "x".into() },
            ShellRequest::StartIcons { icon_px: 30, ids: Vec::new() },
        ] {
            assert!(loads_com(&r), "{r:?}");
        }
        // The children's listing, the volume and the launcher's state files load no COM server here.
        for r in [
            ShellRequest::ListStartApps { icon_px: 30, icons: IconPlan::None, child: true },
            ShellRequest::VolumeToggleMute,
            ShellRequest::LauncherStateLoad,
            ShellRequest::LauncherClosed,
            ShellRequest::StartRelease,
            // Counted where they fall back in-process (`COM_WORK` is set there), not by the request.
            ShellRequest::StartIconsFor { icon_px: 30, ids: Vec::new() },
            // The trim's own pass plans nothing, or every trim would plan the next one.
            ShellRequest::ComFree,
        ] {
            assert!(!loads_com(&r), "{r:?}");
        }
    }

    #[test]
    fn launch_failed_pushed_on_error() {
        let mut calls = Vec::new();
        let r = open_path_with("C:\\a.txt", &mut recorder(&mut calls, false));
        assert_eq!(calls.len(), 1);
        assert_eq!(r, Err(Fail::Shell("0x80004005".into())));
        assert!(matches!(
            failure_event(LaunchKind::Path, &r),
            Some(ShellEvent::LaunchFailed { kind: LaunchKind::Path })
        ));
        assert!(failure_event(LaunchKind::Path, &Ok(())).is_none());
        // Refused here before any shell call: only logged, never "Explorer isn't answering".
        calls.clear();
        let (sys, windows, g) = (Path::new("C:\\Windows\\System32"), Path::new("C:\\Windows"), Gesture::for_test(1));
        let refused = |r: Result<(), Fail>| {
            assert!(matches!(r, Err(Fail::Refused(_))), "{r:?}");
            assert!(failure_event(LaunchKind::Path, &r).is_none(), "{r:?}");
        };
        refused(open_path_with("", &mut recorder(&mut calls, false)));
        refused(run_as_typed_with("\\\\server\\x.exe", 0..14, &g, &mut recorder(&mut calls, false)));
        refused(run_as_typed_with("cmd", 0..99, &g, &mut recorder(&mut calls, false)));
        refused(launch_command_with("cmd", 0..99, false, sys, &mut recorder(&mut calls, false)));
        refused(launch_command_with("ping", 0..4, true, Path::new(""), &mut recorder(&mut calls, false)));
        refused(reveal_path_with("C:\\a\".txt", windows, &mut recorder(&mut calls, false)));
        refused(reveal_path_with("C:\\a.txt", Path::new(""), &mut recorder(&mut calls, false)));
        refused(open_url_with("https://user@a.it/", &mut recorder(&mut calls, false)));
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(launch_kind(&ShellRequest::OpenPath { path: "C:\\a.txt".into() }), Some(LaunchKind::Path));
    }

    /// (line, file range) as the launcher's run mode splits it.
    fn run_line(s: &str) -> (String, Range<usize>) {
        let r = crate::launcher::runline::classify(s).expect("a run line");
        (r.line.to_string(), r.file)
    }

    #[test]
    fn runas_refuses_unc_and_markers() {
        let g = Gesture::for_test(1);
        let mut calls = Vec::new();
        let bad = [
            "\\\\server\\share\\setup.exe",
            "\"\\\\server\\x.exe\" -y",
            "//server/share/x.exe",
            "file://server/share/setup.exe",
            "File:\\\\server\\x.exe",
            "https://example.com/setup.exe",
            "\\/server\\share\\x.exe",
            "/\\server\\x.exe",
            "\"\\/server\\x.exe\" -y",
            "\\??\\UNC\\server\\share\\x.exe",
            "notepad a\u{202E}b",
            "notepad\tx",
            "cmd /c \u{200F}x",
            "x\u{061C}y",
            "notepad a\u{85}b",
        ];
        for s in bad {
            let (line, file) = run_line(s);
            let r = run_as_typed_with(&line, file, &g, &mut recorder(&mut calls, true));
            assert!(r.is_err(), "{s:?}");
        }
        assert!(calls.is_empty(), "{calls:?}");
        let (line, file) = run_line("notepad C:\\temp\\a.txt");
        assert!(run_as_typed_with(&line, file, &g, &mut recorder(&mut calls, true)).is_ok());
        assert_eq!(calls.len(), 1);
        let (f, a, d, v) = &calls[0];
        assert_eq!(
            (f.as_str(), a.as_deref(), d.as_deref(), v.as_str()),
            ("notepad", Some("C:\\temp\\a.txt"), None, "runas")
        );
    }

    #[test]
    fn plain_shell_execute_only_when_known_unelevated() {
        assert!(plain_open(Verb::Open, Some(false)));
        assert!(!plain_open(Verb::Open, Some(true)));
        assert!(!plain_open(Verb::Open, None), "a token that cannot be read goes through the desktop shell");
        let mut calls = Vec::new();
        let (line, file) = run_line("notepad");
        assert!(run_as_typed_with(&line, file, &Gesture::for_test(1), &mut recorder(&mut calls, true)).is_ok());
        assert!(!plain_open(calls[0].3, Some(false)), "runas always goes through the desktop shell");
    }

    #[test]
    fn launcher_paths_and_command_lines_always_leave_through_the_desktop_shell() {
        // Windows' prompt before it runs a downloaded file is the second gate for typed paths and alias targets
        // (5.4.6); the in-process call's SEE_MASK_FLAG_NO_UI may hide it, so these never run in the dock's process.
        for kind in [LaunchKind::Path, LaunchKind::Command] {
            for elevation in [Some(false), Some(true), None] {
                assert!(!in_process_exec(kind, Verb::Open, elevation), "{kind:?} {elevation:?}");
            }
        }
        // The launcher's other opens keep `plain_open`'s rule (runas: `plain_shell_execute_only_when_known_unelevated`).
        for kind in [LaunchKind::Reveal, LaunchKind::Url] {
            assert!(in_process_exec(kind, Verb::Open, Some(false)), "{kind:?}");
            assert!(!in_process_exec(kind, Verb::Open, Some(true)) && !in_process_exec(kind, Verb::Open, None));
        }
    }

    #[test]
    fn launcher_launches_run_in_process_only_when_known_unelevated() {
        // The launcher's catalogue apps, settings targets and reveals fail closed, as its other requests (`plain_open`).
        assert!(in_process(LaunchBy::Launcher, Some(false)));
        assert!(!in_process(LaunchBy::Launcher, Some(true)));
        assert!(!in_process(LaunchBy::Launcher, None), "a token that cannot be read goes through the desktop shell");
        // The shared requests (Start, the bar, notifications) keep 1.4's rule.
        assert!(in_process(LaunchBy::Shared, Some(false)));
        assert!(!in_process(LaunchBy::Shared, Some(true)));
        assert!(in_process(LaunchBy::Shared, None));
    }

    #[test]
    fn runas_refuses_a_bad_range() {
        let g = Gesture::for_test(1);
        let mut calls = Vec::new();
        let ranges =
            [("cmd", 0..99), ("\u{e9}", 1..2), ("cmd", Range { start: 3, end: 1 }), ("cmd", 0..0), ("cmd", 1..3)];
        for (line, file) in ranges {
            let shown = format!("{file:?}");
            let e = run_as_typed_with(line, file, &g, &mut recorder(&mut calls, true)).expect_err(&shown).to_string();
            // What the shell thread logs for a refusal: the kind and the reason, never the line.
            assert!(!e.contains(line), "{e}");
        }
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn launcher_requests_split_lines() {
        let sys = Path::new("C:\\Windows\\System32");
        let quoted = "\"C:\\Program Files\\x.exe\" -y";
        assert_eq!(run_line(quoted).1, 1..23);
        assert_eq!(
            command_parts(quoted, 1..23, false, sys),
            Ok(("C:\\Program Files\\x.exe".into(), Some("-y".into())))
        );
        assert_eq!(command_parts("notepad", 0..7, false, sys), Ok(("notepad".into(), None)));
        assert_eq!(command_parts("notepad   a.txt ", 0..7, false, sys), Ok(("notepad".into(), Some("a.txt".into()))));
        let (line, file) = run_line("\"C:\\x y.exe\"");
        assert_eq!(command_parts(&line, file, false, sys), Ok(("C:\\x y.exe".into(), None)));
        // A range that does not end where the file ends, or leaves text before it, is no split.
        assert!(command_parts(quoted, 1..5, false, sys).is_err());
        assert!(command_parts("notepad a.txt", 0..4, false, sys).is_err());
        assert!(command_parts("x notepad", 2..9, false, sys).is_err());
    }

    #[test]
    fn launch_command_console_wraps_cmd() {
        let sys = Path::new("C:\\Windows\\System32");
        let mut calls = Vec::new();
        let (line, file) = run_line("ping 1.1.1.1");
        assert!(launch_command_with(&line, file.clone(), true, sys, &mut recorder(&mut calls, true)).is_ok());
        let (line2, file2) = run_line("notepad a.txt");
        assert!(launch_command_with(&line2, file2, false, sys, &mut recorder(&mut calls, true)).is_ok());
        // A quoted program with quoted arguments: with /s cmd strips exactly the outer pair, never a quote of the line.
        let (line3, file3) = run_line("\"C:\\Program Files\\Git\\bin\\bash.exe\" --login \"x y\"");
        assert!(launch_command_with(&line3, file3, true, sys, &mut recorder(&mut calls, true)).is_ok());
        let cmd = || "C:\\Windows\\System32\\cmd.exe".to_string();
        assert_eq!(
            calls,
            [
                (cmd(), Some("/s /k \"ping 1.1.1.1\"".to_string()), None, Verb::Open),
                ("notepad".to_string(), Some("a.txt".to_string()), None, Verb::Open),
                (
                    cmd(),
                    Some("/s /k \"\"C:\\Program Files\\Git\\bin\\bash.exe\" --login \"x y\"\"".to_string()),
                    None,
                    Verb::Open
                ),
            ]
        );
        // No system directory: no console, and no guess from the environment.
        calls.clear();
        assert!(launch_command_with(&line, file, true, Path::new(""), &mut recorder(&mut calls, true)).is_err());
        assert!(calls.is_empty());
    }

    /// Live check V-13: the desktop shell does not expand %VAR%, so such a line goes through `cmd.exe /s /c` (cmd expands
    /// it, closes after a windowed program started, and keeps no console open); the typed line stays whole, quotes
    /// included; a line without a variable, and Ctrl+Shift+Enter's elevated run, are not wrapped.
    #[test]
    fn launch_command_with_a_marker_is_refused_under_k_as_under_c() {
        let sys = Path::new("C:\\Windows\\System32");
        for (line, console) in
            [("ping 1.1.1.1\u{202E}& calc", true), ("ping 1.1.1.1\u{7}x", true), ("notepad %A%\u{2066}", false)]
        {
            let (line, file) = run_line(line);
            let mut calls = Vec::new();
            let r = launch_command_with(&line, file, console, sys, &mut recorder(&mut calls, true));
            assert_eq!(r, Err(Fail::Refused("a control or bidi character")), "{line:?}");
            assert!(calls.is_empty());
        }
    }

    #[test]
    fn launch_command_with_a_variable_goes_through_cmd_c() {
        let sys = Path::new("C:\\Windows\\System32");
        let cmd = || "C:\\Windows\\System32\\cmd.exe".to_string();
        let mut calls = Vec::new();
        let (line, file) = run_line("notepad %USERPROFILE%\\x.txt");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
        let (line, file) = run_line("\"C:\\Program Files\\x.exe\" \"%TEMP%\\a b\"");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
        // A console program with a variable keeps its console (/k), as before.
        let (line, file) = run_line("ping %COMPUTERNAME%");
        assert!(launch_command_with(&line, file, true, sys, &mut recorder(&mut calls, true)).is_ok());
        // A lone percent sign is no variable.
        let (line, file) = run_line("notepad 100%");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
        assert_eq!(
            calls,
            [
                (cmd(), Some("/s /c \"start \"\" notepad %USERPROFILE%\\x.txt\"".to_string()), None, Verb::Open),
                (
                    cmd(),
                    Some("/s /c \"start \"\" \"C:\\Program Files\\x.exe\" \"%TEMP%\\a b\"\"".to_string()),
                    None,
                    Verb::Open
                ),
                (cmd(), Some("/s /k \"ping %COMPUTERNAME%\"".to_string()), None, Verb::Open),
                ("notepad".to_string(), Some("100%".to_string()), None, Verb::Open),
            ]
        );
        // A bidi or control character in a wrapped line: refused.
        let (line, file) = run_line("notepad %X%\u{202e}a");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_err());
        assert_eq!(calls.len(), 4);
        // An address stays with Explorer (a % in it is percent-encoding; cmd would split it at an &).
        calls.clear();
        for url in [
            "https://example.com/s?q=a%20b%20c&x=1",
            "shell:%20x%20",
            "ms-settings:%20a%20",
            "\"https://e.com/a%20b%20c\"",
        ] {
            let (line, file) = run_line(url);
            assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
            assert_ne!(calls.last().map(|c| c.0.as_str()), Some("C:\\Windows\\System32\\cmd.exe"), "{url}");
        }
        assert_eq!(calls.len(), 4);
        // A file that is the whole line goes quoted into the start (cmd splits at a space), also under /k.
        calls.clear();
        let (line, file) = run_line("C:\\My Files\\%USERNAME%.txt");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
        let (line, file) = run_line("C:\\My Files\\%USERNAME%.exe");
        assert!(launch_command_with(&line, file, true, sys, &mut recorder(&mut calls, true)).is_ok());
        assert_eq!(calls[0].1.as_deref(), Some("/s /c \"start \"\" \"C:\\My Files\\%USERNAME%.txt\"\""));
        assert_eq!(calls[1].1.as_deref(), Some("/s /k \"\"C:\\My Files\\%USERNAME%.exe\"\""));
        // Only a whole-line file cmd would split, or whose variable may expand to a space, goes quoted: cmd runs no
        // quoted built-in ('"dir"' is not recognized), so Shift+Enter on a bare word runs it as before (spec 5.4.9).
        calls.clear();
        for word in ["dir", "set", "ipconfig", "C:\\My Files\\x.exe", "%ProgramFiles%\\x.exe"] {
            let (line, file) = run_line(word);
            assert!(launch_command_with(&line, file, true, sys, &mut recorder(&mut calls, true)).is_ok(), "{word}");
        }
        let (line, file) = run_line("%ProgramFiles%\\x.exe");
        assert!(launch_command_with(&line, file, false, sys, &mut recorder(&mut calls, true)).is_ok());
        let args: Vec<_> = calls.iter().map(|c| c.1.as_deref().unwrap_or("")).collect();
        assert_eq!(
            args,
            [
                "/s /k \"dir\"",
                "/s /k \"set\"",
                "/s /k \"ipconfig\"",
                "/s /k \"\"C:\\My Files\\x.exe\"\"",
                "/s /k \"\"%ProgramFiles%\\x.exe\"\"",
                "/s /c \"start \"\" \"%ProgramFiles%\\x.exe\"\"",
            ]
        );
        // No system directory: nothing runs, and no guess from the environment.
        calls.clear();
        let (line, file) = run_line("notepad %X%");
        assert!(launch_command_with(&line, file, false, Path::new(""), &mut recorder(&mut calls, true)).is_err());
        assert!(calls.is_empty());
    }

    #[test]
    fn reveal_path_refuses_quotes() {
        let windows = Path::new("C:\\Windows");
        let mut calls = Vec::new();
        for bad in ["", "C:\\a\".txt", "C:\\a.txt\" /e,C:\\", "C:\\a\nb", "C:\\a\u{9F}b"] {
            assert!(reveal_path_with(bad, windows, &mut recorder(&mut calls, true)).is_err(), "{bad:?}");
        }
        assert!(reveal_path_with("C:\\a.txt", Path::new(""), &mut recorder(&mut calls, true)).is_err());
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn reveal_path_is_one_shell_call() {
        let mut calls = Vec::new();
        assert!(reveal_path_with("C:\\x y\\a.txt", Path::new("C:\\Windows"), &mut recorder(&mut calls, true)).is_ok());
        assert_eq!(
            calls,
            [(
                "C:\\Windows\\explorer.exe".to_string(),
                Some("/select,\"C:\\x y\\a.txt\"".to_string()),
                None,
                Verb::Open
            )]
        );
    }

    #[test]
    fn open_url_rebuild() {
        let ok = |u: &str| rebuild_url(u).unwrap_or_else(|e| panic!("{u}: {e}"));
        assert_eq!(ok("https://a.it/x?q=1"), "https://a.it/x?q=1");
        assert_eq!(
            ok("https://www.google.com/search?q=caf%C3%A9%20x"),
            "https://www.google.com/search?q=caf%C3%A9%20x"
        );
        assert_eq!(ok("HTTPS://a.it"), "https://a.it");
        assert_eq!(ok("http://a.it:8080/p#top"), "http://a.it:8080/p#top");
        // What could end the URL inside a handler's command line is percent-encoded, never passed raw.
        assert_eq!(ok("https://a.it/\"x\"<y>|"), "https://a.it/%22x%22%3Cy%3E%7C");
        assert_eq!(ok("https://a.it/caff\u{e8}"), "https://a.it/caff%C3%A8");
        assert_eq!(ok("https://a.it/a\\b"), "https://a.it/a%5Cb");
        assert_eq!(ok("https://a.it/\u{202E}"), "https://a.it/%E2%80%AE", "a bidi mark is shown, not obeyed");
        // A user name or password before the host is refused, never passed on (and never kept).
        for u in ["https://user:pw@a.it/x", "https://a.it@evil.example/", "http://@a.it", "https://a.it\\@evil.example"]
        {
            assert!(rebuild_url(u).is_err(), "{u}");
        }
        // Other schemes, no scheme, bad hosts and ports, whitespace and controls.
        for u in [
            "ftp://a.it/x",
            "file:///C:/x",
            "javascript:alert(1)",
            "ms-settings:display",
            "shell:AppsFolder",
            "a.it",
            "https://",
            "https:///x",
            "https://a..it",
            "https://.a.it",
            "https://a.it:",
            "https://a.it:99999",
            "https://a.it:8a",
            "https://a b.it",
            "https://a.it/x y",
            "https://a.it/\n",
        ] {
            assert!(rebuild_url(u).is_err(), "{u:?}");
        }
        let mut calls = Vec::new();
        assert!(open_url_with("HTTPS://a.it/\"", &mut recorder(&mut calls, true)).is_ok());
        assert!(open_url_with("ftp://a.it", &mut recorder(&mut calls, true)).is_err());
        assert_eq!(calls, [("https://a.it/%22".to_string(), None, None, Verb::Open)]);
    }

    #[test]
    fn typed_urls_pass_open_url() {
        // Every address the launcher offers as a typed URL is one OpenUrl passes on: no row ends in a refusal.
        let prefixes = ["https://", "http://", "HTTPS://", "www.", "WWW."];
        let hosts = [
            "a.it",
            "user@host",
            "u:p@a.it",
            "[::1]",
            "my_host",
            "a.it.",
            "a..it",
            "",
            ".a.it",
            "a.it:",
            "a.it:8080",
            "a.it:65535",
            "a.it:65536",
            "a.it:8a",
            "citt\u{e0}.it",
            "a-b.it",
            "a.it\\x",
            "a.it%40b",
        ];
        let tails = ["", "/", "/x?q=1#f", "?q", "#f", "/\"<>|", "/@x", "/a:b"];
        let mut offered = 0;
        for p in prefixes {
            for h in hosts {
                for t in tails {
                    let typed = format!("{p}{h}{t}");
                    if let Some(url) = crate::launcher::web::parse_typed_url(&typed) {
                        offered += 1;
                        assert!(rebuild_url(&url).is_ok(), "{typed:?} -> {url:?}: {:?}", rebuild_url(&url));
                    }
                }
            }
        }
        assert!(offered > 50, "{offered}");
    }

    #[test]
    fn only_launcher_requests_report_failures() {
        let err: Result<(), Fail> = Err(Fail::Shell("0x80004005".into()));
        let reported = |r: &ShellRequest| match launch_kind(r).and_then(|k| failure_event(k, &err)) {
            Some(ShellEvent::LaunchFailed { kind }) => Some(kind),
            _ => None,
        };
        let id = || "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".to_string();
        let shared = [
            ShellRequest::Launch { target: format!("shell:AppsFolder\\{}", id()), args: None },
            ShellRequest::LaunchAppAdmin { id: id() },
            ShellRequest::RevealApp { id: id() },
            ShellRequest::LaunchAdmin(AdminTool::Terminal),
            ShellRequest::OpenNotifApp { aumid: id(), name: "Calculator".into() },
            ShellRequest::ToggleDesktop,
        ];
        for r in &shared {
            assert_eq!(reported(r), None, "{r:?}");
        }
        let g = Gesture::for_test(1);
        let twins = [
            (
                ShellRequest::LauncherLaunch { target: format!("shell:AppsFolder\\{}", id()), args: None },
                LaunchKind::App,
            ),
            (ShellRequest::LauncherAppAdmin { id: id() }, LaunchKind::Admin),
            (ShellRequest::LauncherRevealApp { id: id() }, LaunchKind::Reveal),
            (ShellRequest::OpenPath { path: "C:\\a.txt".into() }, LaunchKind::Path),
            (ShellRequest::RevealPath { path: "C:\\a.txt".into() }, LaunchKind::Reveal),
            (ShellRequest::LaunchCommand { line: "ping x".into(), file: 0..4, console: true }, LaunchKind::Command),
            (ShellRequest::RunAsTyped { line: "cmd".into(), file: 0..3, gesture: g }, LaunchKind::Admin),
            (ShellRequest::OpenUrl { url: "https://a.it".into() }, LaunchKind::Url),
        ];
        for (r, kind) in &twins {
            assert_eq!(reported(r), Some(*kind), "{r:?}");
            assert!(launch_kind(r).and_then(|k| failure_event(k, &Ok(()))).is_none(), "{r:?}");
        }
    }

    #[test]
    fn start_icons_for_replaces_a_pending_one() {
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        let ids = |s: &str| vec![s.to_string()];
        enqueue(&mut pending, ShellRequest::StartIcons { icon_px: 48, ids: ids("c") });
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("a") });
        enqueue(&mut pending, ShellRequest::VolumeRefresh);
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("b") });
        assert_eq!(pending.len(), 3);
        // Start's release forgets its own batches only.
        let (mut px, mut sent) = (48u32, HashSet::from(["c".to_string()]));
        start_release(&mut px, &mut sent, &mut pending);
        assert_eq!(pending.len(), 2);
        enqueue(&mut pending, ShellRequest::StartIcons { icon_px: 48, ids: ids("d") });
        // Not a low-priority request: it runs ahead of the Start menu's batches, and only the newest is left.
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::VolumeRefresh)));
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::StartIconsFor { ids, .. }) if ids == ["b"]));
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::StartIcons { .. })));
        assert!(next_request(&mut pending).is_none());
    }

    #[test]
    fn one_pump_drains_every_request() {
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        let ids = |s: &str| vec![s.to_string()];
        enqueue(&mut pending, ShellRequest::VolumeAdjust { delta_percent: 2 });
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("a") });
        enqueue(&mut pending, ShellRequest::Launch { target: "x.exe".into(), args: None });
        enqueue(&mut pending, ShellRequest::LauncherConvert { gen: 1, query: "q".into() });
        enqueue(&mut pending, ShellRequest::VolumeRefresh);
        enqueue(&mut pending, ShellRequest::StartIcons { icon_px: 48, ids: ids("s") });
        let mut taken = 0;
        let mut seen_icons = false;
        while let Some(r) = next_request(&mut pending) {
            taken += 1;
            let low = matches!(r, ShellRequest::StartIcons { .. });
            assert!(!seen_icons || low, "a normal request came after a Start batch");
            seen_icons |= low;
        }
        assert_eq!(taken, 6, "every request comes out exactly once");
        // Newer ones replace older ones; the launcher's close drops a pending icon request.
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("a") });
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("b") });
        enqueue(&mut pending, ShellRequest::LauncherConvert { gen: 1, query: "q".into() });
        enqueue(&mut pending, ShellRequest::LauncherConvert { gen: 2, query: "qq".into() });
        assert_eq!(pending.len(), 2);
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::StartIconsFor { ids, .. }) if ids == ["b"]));
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::LauncherConvert { gen: 2, .. })));
        assert!(next_request(&mut pending).is_none());
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: ids("c") });
        enqueue(&mut pending, ShellRequest::LauncherClosed);
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::LauncherClosed)));
        assert!(next_request(&mut pending).is_none());
    }

    #[test]
    fn the_safety_timer_follows_the_icon_jobs() {
        assert!(!timer_wanted(false, false));
        assert!(timer_wanted(true, false));
        assert!(timer_wanted(false, true));
        assert!(timer_wanted(true, true));
    }

    #[test]
    fn icon_plan_none_leaves_start_sent() {
        // The ids as the arena holds them (sorted by name).
        let apps = ["a", "b", "c"];
        let mut px = 48u32;
        let mut sent = HashSet::from(["a".to_string()]);
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        pending.push_back(ShellRequest::StartIcons { icon_px: 48, ids: vec!["z".into()] });
        let none = missing_start_icons(&mut px, &mut sent, &mut pending, apps, 32, &IconPlan::None);
        assert_eq!(
            (none, px, sent.len(), pending.len()),
            (None, 48, 1, 1),
            "the launcher's catalogue changes nothing of Start's"
        );
        let pins = IconPlan::All { first: vec!["C".into()] };
        let missing = missing_start_icons(&mut px, &mut sent, &mut pending, apps, 48, &pins);
        assert_eq!((missing, px, sent.len(), pending.len()), (Some(strings(&["c", "b"])), 48, 3, 1));
        // Nothing new: nothing to make.
        let missing = missing_start_icons(&mut px, &mut sent, &mut pending, apps, 48, &pins);
        assert_eq!(missing, Some(Vec::new()));
        // A new size starts Start's icons over, and drops the batches made here at the old one.
        let missing =
            missing_start_icons(&mut px, &mut sent, &mut pending, apps, 64, &IconPlan::All { first: Vec::new() });
        assert_eq!((missing, px, sent.len(), pending.len()), (Some(strings(&["a", "b", "c"])), 64, 3, 0));
    }

    /// L3 T17: what the dock takes from an `--start-apps` child is checked whole before it is used, and any failure is a
    /// reason (the caller then lists in-process).
    #[test]
    fn apps_child_output_checked() {
        let list = [
            StartApp { id: "Vendor.Tool".into(), name: "Tool".into() },
            StartApp { id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), name: "Calculator".into() },
        ];
        let valid = catalog::encode_app_list(&list, None).unwrap();
        // A valid encoding with exit 0: the arena, sorted by name, without the slack of the read buffer.
        let mut read = Vec::with_capacity(valid.len() * 2);
        read.extend_from_slice(&valid);
        let a = apps_from_child(Ok((0, read))).unwrap();
        assert_eq!((a.len(), a.view().name(0), a.view().id(1)), (2, "Calculator", "Vendor.Tool"));
        assert_eq!(a.bytes(), valid.len(), "only the bytes of the arena stay");
        // A child that failed, printed nothing, printed something else, or printed a damaged or longer arena.
        assert!(apps_from_child(Ok((1, valid.clone()))).unwrap_err().contains("exit code 1"));
        assert!(apps_from_child(Ok((-1, valid.clone()))).is_err());
        assert!(apps_from_child(Ok((0, Vec::new()))).is_err());
        assert!(apps_from_child(Ok((0, b"mini-oled-dock apps-catalog 1\nhello".to_vec()))).is_err());
        assert!(apps_from_child(Ok((0, (0..4096u32).map(|i| (i * 7) as u8).collect()))).is_err());
        for at in [0, 40, valid.len() / 2, valid.len() - 1] {
            let mut b = valid.clone();
            b[at] ^= 0x10;
            assert!(apps_from_child(Ok((0, b))).is_err(), "a flip at {at}");
        }
        let mut longer = valid.clone();
        longer.extend_from_slice(&[0; 8]);
        assert!(apps_from_child(Ok((0, longer))).is_err());
        assert!(apps_from_child(Ok((0, valid[..valid.len() - 8].to_vec()))).is_err());
        // A settings arena is no apps arena.
        let settings = crate::launcher::arena::test_settings(&[("Display", &[], 1)]);
        assert!(apps_from_child(Ok((0, settings.raw().to_vec()))).is_err());
        // `run_capture`'s own reason (no start, timeout, too much output) is passed on.
        assert_eq!(apps_from_child(Err("timeout".into())).unwrap_err(), "timeout");
    }

    #[test]
    fn launcher_convert_replaces_a_pending_one() {
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        enqueue(&mut pending, ShellRequest::LauncherConvert { gen: 1, query: "rkd".into() });
        enqueue(&mut pending, ShellRequest::VolumeRefresh);
        enqueue(&mut pending, ShellRequest::LauncherConvert { gen: 2, query: "rkdp".into() });
        enqueue(&mut pending, ShellRequest::StartIconsFor { icon_px: 24, ids: Vec::new() });
        assert_eq!(pending.len(), 3);
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::VolumeRefresh)));
        assert!(matches!(next_request(&mut pending), Some(ShellRequest::LauncherConvert { gen: 2, .. })));
    }

    #[test]
    fn launcher_state_files_round_trip() {
        let dir = std::env::temp_dir().join(format!("dock_launcher_state_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let b = |s: &[u8]| Some(s.to_vec());
        assert_eq!(load_state_from(&dir), (None, None), "no files yet");
        assert!(save_state_to(&dir, b(b"f1"), b(b"r1"), false).is_ok());
        assert!(dir.join("launcher-frecency.txt").is_file() && dir.join("launcher-run.txt").is_file());
        assert_eq!(load_state_from(&dir), (b(b"f1"), b(b"r1")));
        // None leaves a file as it is.
        assert!(save_state_to(&dir, None, b(b"r2"), false).is_ok());
        assert_eq!(load_state_from(&dir), (b(b"f1"), b(b"r2")));
        // Over 64 KiB: never written, never read.
        assert!(save_state_to(&dir, Some(vec![b'x'; 65_537]), None, false).is_err());
        assert_eq!(load_state_from(&dir).0, b(b"f1"));
        assert!(std::fs::write(dir.join("launcher-run.txt"), vec![b'x'; 65_537]).is_ok());
        assert_eq!(load_state_from(&dir).1, None);
        // delete removes both, whatever else the request carries.
        assert!(save_state_to(&dir, b(b"f3"), None, true).is_ok());
        assert_eq!(load_state_from(&dir), (None, None));
        assert!(save_state_to(&dir, None, None, true).is_ok(), "nothing to delete is no failure");
        let _ = std::fs::remove_dir(&dir);
    }

    // ---- L3 T20: icons from a child

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A frame's bytes as an icons child writes them, with `pixels` zero pixels.
    fn raw_frame(index: u32, w: u32, h: u32, pixels: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [index, w, h] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.resize(v.len() + pixels * 4, 0);
        v
    }

    #[test]
    fn icon_frames_through_the_reader_buffer() {
        // L3 MEM-3: the dock's reader buffer (16 KiB) is smaller than a 72 px frame (20,748 B): frames of every size the
        // launcher and Start ask for still come through it whole, in order.
        for px in [24u32, 30, 48, 72] {
            let icon = IconBitmap {
                width: px,
                height: px,
                pixels: (0..px * px).map(|i| i.wrapping_mul(0x0103_0507)).collect(),
            };
            let out =
                [ICONS_HEADER.to_vec(), encode_icon_frame(0, Some(&icon)), encode_icon_frame(1, Some(&icon))].concat();
            let mut r = std::io::BufReader::with_capacity(ICON_READ_BUF, &out[..]);
            read_icon_header(&mut r).unwrap();
            assert_eq!(read_icon_frame(&mut r, 2, px, None).unwrap(), Some((0, Some(icon.clone()))), "{px} px");
            assert_eq!(read_icon_frame(&mut r, 2, px, Some(0)).unwrap(), Some((1, Some(icon))), "{px} px");
            assert_eq!(read_icon_frame(&mut r, 2, px, Some(1)).unwrap(), None);
        }
    }

    #[test]
    fn icon_frames_round_trip() {
        let a = IconBitmap { width: 8, height: 8, pixels: (0..64u32).map(|i| i.wrapping_mul(0x0103_0507)).collect() };
        let c = IconBitmap { width: 8, height: 8, pixels: vec![0xFF12_3456; 64] };
        let out = [
            ICONS_HEADER.to_vec(),
            encode_icon_frame(0, Some(&a)),
            encode_icon_frame(1, None),
            encode_icon_frame(2, Some(&c)),
        ]
        .concat();
        assert_eq!(out.len(), ICONS_HEADER.len() + 3 * ICON_FRAME_HEAD + 2 * 8 * 8 * 4);
        let mut r = &out[..];
        read_icon_header(&mut r).unwrap();
        assert_eq!(read_icon_frame(&mut r, 3, 8, None).unwrap(), Some((0, Some(a))));
        assert_eq!(read_icon_frame(&mut r, 3, 8, Some(0)).unwrap(), Some((1, None)));
        assert_eq!(read_icon_frame(&mut r, 3, 8, Some(1)).unwrap(), Some((2, Some(c))));
        assert_eq!(read_icon_frame(&mut r, 3, 8, Some(2)).unwrap(), None, "a clean end");
        // The head is the index, the width and the height, little-endian; no icon is a 0 x 0 frame.
        assert_eq!(encode_icon_frame(0x0102_0304, None), [4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        // A bitmap whose pixels do not match its size goes as no icon.
        let odd = IconBitmap { width: 8, height: 8, pixels: vec![1; 3] };
        assert_eq!(encode_icon_frame(5, Some(&odd)), encode_icon_frame(5, None));
        assert_eq!(encode_icon_frame(5, Some(&IconBitmap::default())), encode_icon_frame(5, None));
    }

    #[test]
    fn icon_frames_rejected() {
        let bad_headers: [&[u8]; 6] = [
            b"",
            b"mini-oled-dock start-icons 1",
            b"mini-oled-dock start-icons 2\n",
            b"MINI-OLED-DOCK START-ICONS 1\n",
            b"mini-oled-dock apps-catalog 1\n",
            b"\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
        ];
        for h in bad_headers {
            assert!(read_icon_header(&mut &h[..]).is_err(), "{h:?}");
        }
        let read = |b: &[u8], n: usize, px: u32, prev: Option<usize>| read_icon_frame(&mut &b[..], n, px, prev);
        // An index of n or more.
        assert!(read(&raw_frame(3, 0, 0, 0), 3, 30, None).is_err());
        assert!(read(&raw_frame(u32::MAX, 0, 0, 0), 3, 30, None).is_err());
        assert!(read(&raw_frame(0, 0, 0, 0), 0, 30, None).is_err(), "no id at all");
        // An index not above the previous one.
        assert!(read(&raw_frame(1, 0, 0, 0), 3, 30, Some(1)).is_err());
        assert!(read(&raw_frame(0, 0, 0, 0), 3, 30, Some(1)).is_err());
        assert_eq!(read(&raw_frame(2, 0, 0, 0), 3, 30, Some(0)).unwrap(), Some((2, None)), "an id skipped is no error");
        // A width other than the height.
        assert!(read(&raw_frame(0, 30, 29, 30 * 29), 3, 30, None).is_err());
        assert!(read(&raw_frame(0, 0, 30, 0), 3, 30, None).is_err());
        assert!(read(&raw_frame(0, 30, 0, 0), 3, 30, None).is_err());
        // A width neither 0 nor exactly px: a 256 px frame when px is 30, a smaller one, a huge one with no pixels.
        assert!(read(&raw_frame(0, 256, 256, 256 * 256), 3, 30, None).is_err());
        assert!(read(&raw_frame(0, 29, 29, 29 * 29), 3, 30, None).is_err());
        assert!(read(&raw_frame(0, 0x8000_0000, 0x8000_0000, 0), 3, 30, None).is_err());
        // A size the dock never asks for is refused whatever the frame says.
        assert!(read(&raw_frame(0, 300, 300, 300 * 300), 3, 300, None).is_err());
        assert!(read(&raw_frame(0, 0, 0, 0), 3, 0, None).is_err());
        // A frame cut short, in its head or in its pixels.
        let whole = raw_frame(0, 30, 30, 900);
        assert!(read(&whole, 3, 30, None).unwrap().is_some());
        for cut in [1, 4, 11, 12, 13, whole.len() - 1] {
            assert!(read(&whole[..cut], 3, 30, None).is_err(), "cut at {cut}");
        }
        // More than n frames: the one after the n-th is refused.
        let three = [raw_frame(0, 0, 0, 0), raw_frame(1, 0, 0, 0), raw_frame(2, 0, 0, 0)].concat();
        let mut r = &three[..];
        let mut prev = None;
        for want in [0, 1] {
            let got = read_icon_frame(&mut r, 2, 30, prev).unwrap().map(|(i, _)| i);
            assert_eq!(got, Some(want));
            prev = got;
        }
        assert!(read_icon_frame(&mut r, 2, 30, prev).is_err());
    }

    #[test]
    fn icon_frame_reader_never_panics() {
        let mut rng = crate::launcher::testkit::XorShift::new(0x1C0F_2A3D);
        let valid = [
            ICONS_HEADER.to_vec(),
            encode_icon_frame(0, Some(&IconBitmap::new(8, 8))),
            encode_icon_frame(2, None),
            encode_icon_frame(3, Some(&IconBitmap { width: 8, height: 8, pixels: vec![0xFF00_00FF; 64] })),
        ]
        .concat();
        let mut frames = 0usize;
        for round in 0..10_000u32 {
            let mut buf: Vec<u8> = if round % 2 == 0 {
                // A valid output, damaged: some bytes changed, then cut anywhere.
                let mut b = valid.clone();
                for _ in 0..1 + rng.next_u64() % 4 {
                    let at = (rng.next_u64() as usize) % b.len();
                    b[at] = rng.next_u64() as u8;
                }
                b.truncate((rng.next_u64() as usize) % (b.len() + 1));
                b
            } else {
                let len = (rng.next_u64() % 700) as usize;
                (0..len).map(|_| rng.next_u64() as u8).collect()
            };
            if round % 4 == 1 {
                // Random bytes behind a good header, so the frames are read.
                buf.splice(0..0, ICONS_HEADER.iter().copied());
            }
            let n = (rng.next_u64() % 6) as usize;
            let px = *rng.pick(&[8u32, 9, 30, 256]);
            let mut r = &buf[..];
            if read_icon_header(&mut r).is_err() {
                continue;
            }
            let mut prev: Option<usize> = None;
            let mut count = 0;
            while let Ok(Some((i, icon))) = read_icon_frame(&mut r, n, px, prev) {
                assert!(i < n && prev.is_none_or(|p| i > p), "{i} after {prev:?} of {n}");
                if let Some(b) = icon {
                    assert_eq!((b.width, b.height, b.pixels.len()), (px, px, (px * px) as usize));
                }
                prev = Some(i);
                count += 1;
                assert!(count <= n);
            }
            frames += count;
        }
        assert!(frames > 500, "the damaged outputs reach the frames: {frames}");
    }

    #[test]
    fn icon_ids_skip_bad_ids() {
        let ok_long = "b".repeat(1_024);
        let ids = vec![
            "Vendor.App".to_string(),
            "bad\u{7}id".into(),
            "a".repeat(1_025),
            ok_long.clone(),
            "line\nbreak".into(),
            "cr\r".into(),
            "tab\tid".into(),
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
        ];
        let (batch, rest) = icon_batch(ids);
        assert_eq!(batch, ["Vendor.App", ok_long.as_str(), "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"]);
        assert!(rest.is_empty());
        // At most 2,000 ids go to one child; the others come back, in order.
        let (batch, rest) = icon_batch((0..2_005).map(|i| format!("app{i}")).collect());
        assert_eq!((batch.len(), rest.len(), rest[0].as_str()), (2_000, 5, "app2000"));
        // The child reads them as they were written, one per line; a line it cannot take is an id without an icon,
        // so every answer keeps the index of its line.
        let mut input = b"a\n\xFF\xFE\nb\n".to_vec();
        input.extend_from_slice(&[b'x'; 1_025]);
        input.extend_from_slice(b"\nc");
        let read = |b: &[u8]| read_icon_ids(&mut &b[..]);
        let want = [Some("a".to_string()), None, Some("b".into()), None, Some("c".into())];
        assert_eq!(read(&input), want);
        assert!(read(b"").is_empty());
        assert_eq!(read(b"x\n"), [Some("x".to_string())]);
        assert_eq!(read(&b"y\n".repeat(2_001)).len(), 2_000, "2,000 lines at most");
        // Reading stops at 2 MiB (an endless input ends there), and the line the cap cuts is no id.
        assert!(read_icon_ids(&mut std::io::repeat(b'y')).is_empty());
        let mut big = b"first\n".to_vec();
        big.resize(3 * 1024 * 1024, b'q');
        assert_eq!(read(&big), [Some("first".to_string())]);
    }

    /// A sink that takes `cap` bytes, then fails; it keeps what it took, the length at each flush and the calls made
    /// after its first failure.
    struct CutWriter {
        cap: usize,
        got: Vec<u8>,
        flushed: Vec<usize>,
        failed: bool,
        after: usize,
    }

    impl CutWriter {
        fn new(cap: usize) -> Self {
            CutWriter { cap, got: Vec::new(), flushed: Vec::new(), failed: false, after: 0 }
        }
    }

    impl std::io::Write for CutWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.failed {
                self.after += 1;
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let k = buf.len().min(self.cap - self.got.len());
            if k == 0 && !buf.is_empty() {
                self.failed = true;
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.got.extend_from_slice(&buf[..k]);
            Ok(k)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.failed {
                self.after += 1;
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.flushed.push(self.got.len());
            Ok(())
        }
    }

    /// The icon a fake extraction makes at `px`: "a" a `px` x `px` bitmap, "c" one of the wrong size, others none.
    fn fake_icon(id: &str, px: u32) -> Option<IconBitmap> {
        let side = match id {
            "a" => px,
            "c" => px + 1,
            _ => return None,
        };
        Some(IconBitmap { width: side, height: side, pixels: (0..side * side).map(|i| 0xFF00_0000 | i).collect() })
    }

    #[test]
    fn icons_child_writes_what_the_dock_reads() {
        let px = 9u32;
        let ids = [Some("a".to_string()), None, Some("b".into()), Some("c".into())];
        let mut made = Vec::new();
        let mut w = CutWriter::new(usize::MAX);
        let code = write_icon_frames(&ids, px, &mut w, |id, size| {
            made.push(id.to_string());
            fake_icon(id, size)
        });
        assert_eq!(code, 0);
        assert_eq!(made, ["a", "b", "c"], "no extraction for a line the child could not read");
        // The header, then one frame per id in input order: a wrong size goes as no icon.
        let frames = [
            encode_icon_frame(0, fake_icon("a", px).as_ref()),
            encode_icon_frame(1, None),
            encode_icon_frame(2, None),
            encode_icon_frame(3, None),
        ];
        let whole = [&[ICONS_HEADER.to_vec()][..], &frames[..]].concat().concat();
        assert_eq!(w.got, whole);
        // Flushed after the header and after every frame.
        let mut ends = vec![ICONS_HEADER.len()];
        for f in &frames {
            ends.push(ends[ends.len() - 1] + f.len());
        }
        assert_eq!(w.flushed, ends);
        // The dock's reader takes it back to the same frames.
        let mut r = &w.got[..];
        read_icon_header(&mut r).unwrap();
        let mut back = Vec::new();
        let mut prev = None;
        while let Some((i, icon)) = read_icon_frame(&mut r, ids.len(), px, prev).unwrap() {
            prev = Some(i);
            back.push((i, icon));
        }
        assert_eq!(back, [(0, fake_icon("a", px)), (1, None), (2, None), (3, None)]);
        // A write that fails at byte k: exit 1 at once, nothing made or written after it.
        let starts = &ends[..ends.len() - 1];
        let k_frame0 = ends[0] + ICON_FRAME_HEAD + 5;
        for k in [0, 1, ICONS_HEADER.len() - 1, ends[0], k_frame0, ends[1], ends[2] + 3, whole.len() - 1] {
            let mut w = CutWriter::new(k);
            let mut calls = 0;
            let code = write_icon_frames(&ids, px, &mut w, |id, size| {
                calls += 1;
                fake_icon(id, size)
            });
            assert_eq!(code, 1, "cut at {k}");
            assert_eq!(w.got, whole[..k], "cut at {k}");
            assert_eq!(w.after, 0, "cut at {k}: a write after the error");
            let begun = starts.iter().zip(&ids).filter(|(s, id)| **s <= k && id.is_some()).count();
            assert_eq!(calls, begun, "cut at {k}: an extraction after the error");
        }
    }

    /// A job's reader feeds the ids to a real child, decodes its output from the pipe and delivers Start's icons in
    /// batches of 8; then the job counts every id answered, and ended.
    #[test]
    fn icon_reader_delivers_a_childs_frames() {
        let px = 9u32;
        let ids: Vec<String> = (0..10).map(|i| format!("vendor.app{i}")).collect();
        let mut out = ICONS_HEADER.to_vec();
        for i in 0..10u32 {
            let icon = (i % 3 != 1).then(|| IconBitmap { width: px, height: px, pixels: vec![0xFF00_0000 | i; 81] });
            out.extend(encode_icon_frame(i, icon.as_ref()));
        }
        let path = std::env::temp_dir().join(format!("dock_icon_frames_{}.bin", std::process::id()));
        std::fs::write(&path, &out).unwrap();
        // A child that prints the frames and never reads its input, as a child stuck before its first read would.
        let cmd = system_dir().unwrap().join("cmd.exe");
        let mut child = crate::update::spawn_in_job(&cmd, &["/c", "type", path.to_str().unwrap()], true).unwrap();
        let progress = Arc::new(IconProgress::default());
        let (input, output) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let reader = IconReader {
            ui: 0,
            client: IconClient::Start,
            px,
            ids: ids.into(),
            progress: Arc::clone(&progress),
            started: Instant::now(),
        };
        reader.run(input, output);
        let _ = child.wait();
        let _ = std::fs::remove_file(&path);
        assert!(progress.ended.load(SeqCst));
        assert_eq!(progress.delivered.load(SeqCst), 10);
        let batches: Vec<Vec<(String, bool)>> = drain_events()
            .into_iter()
            .filter_map(|e| match e {
                ShellEvent::StartIcons { icon_px: 9, icons } => {
                    Some(icons.into_iter().map(|(id, b)| (id, b.is_some_and(|b| b.pixels.len() == 81))).collect())
                }
                _ => None,
            })
            .collect();
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [8, 2]);
        assert_eq!(batches[0][0], ("vendor.app0".to_string(), true));
        assert_eq!(batches[0][1], ("vendor.app1".to_string(), false), "a 0 x 0 frame is no icon");
        assert_eq!(batches[1][1], ("vendor.app9".to_string(), true));
    }

    #[test]
    fn replaced_job_carries_its_undelivered_ids() {
        let old = strings(&["a", "b", "c", "d"]);
        // Two answered: c and d were asked first, so they go first; d once, since the new batch asks for it too.
        assert_eq!(next_icon_batch(&old, 2, strings(&["e", "d"])), strings(&["c", "e", "d"]));
        assert_eq!(next_icon_batch(&old, 0, strings(&["e"])), strings(&["a", "b", "c", "d", "e"]));
        assert_eq!(next_icon_batch(&old, 4, strings(&["e"])), strings(&["e"]));
        assert_eq!(next_icon_batch(&old, 9, strings(&["e"])), strings(&["e"]), "a count past the end carries nothing");
        assert_eq!(next_icon_batch(&old, 1, Vec::new()), strings(&["b", "c", "d"]));
        // Start: ids a job ended without answering, with no batch to carry them, leave start_sent, so the next listing
        // of the same open asks for them again; the answered ones and the others stay.
        let mut sent: HashSet<String> = strings(&["a", "b", "c", "d", "z"]).into_iter().collect();
        unsend_icons(&mut sent, &old, 2);
        let mut left: Vec<&str> = sent.iter().map(String::as_str).collect();
        left.sort_unstable();
        assert_eq!(left, ["a", "b", "z"]);
        unsend_icons(&mut sent, &old, 9);
        assert_eq!(sent.len(), 3);
    }

    /// A Shell with no audio, network or job, as `thread_main` makes it.
    fn test_shell() -> Shell {
        Shell {
            ui: 0,
            audio: None,
            names: HashMap::new(),
            pending: VecDeque::new(),
            net: None,
            start_px: 0,
            start_sent: HashSet::new(),
            start_job: None,
            launcher_job: None,
            icons_child_off: false,
        }
    }

    /// A job whose "child" waits on its input until it is killed (`cmd /c pause` on a pipe), or exits at once.
    fn fake_job(ids: &[&str], waits: bool) -> (IconJob, HANDLE) {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::DUPLICATE_SAME_ACCESS;
        let args: &[&str] = if waits { &["/c", "pause"] } else { &["/c", "exit 0"] };
        let child = crate::update::spawn_in_job(&system_dir().unwrap().join("cmd.exe"), args, true).unwrap();
        // A handle of the test's own, to see the process end after the job's handle is gone.
        let mut seen = HANDLE::default();
        // SAFETY: the child's handle is valid while `child` lives; `seen` is written once and closed by `ended`.
        unsafe {
            let me = GetCurrentProcess();
            windows::Win32::Foundation::DuplicateHandle(
                me,
                HANDLE(child.as_raw_handle()),
                me,
                &mut seen,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .unwrap();
        }
        let job =
            IconJob { child, px: 24, ids: strings(ids).into(), progress: Arc::default(), started: Instant::now() };
        (job, seen)
    }

    /// The process of `h` has ended (waiting at most `ms`); closes `h`.
    fn ended(h: HANDLE, ms: u32) -> bool {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: `h` is a process handle of ours, closed once here.
        unsafe {
            let done = WaitForSingleObject(h, ms) == WAIT_OBJECT_0;
            let _ = CloseHandle(h);
            done
        }
    }

    #[test]
    fn launcher_close_ends_its_icon_job() {
        let mut sh = test_shell();
        let (start, start_seen) = fake_job(&["a"], true);
        let (launcher, launcher_seen) = fake_job(&["b", "c"], true);
        sh.start_job = Some(start);
        sh.launcher_job = Some(launcher);
        enqueue(&mut sh.pending, ShellRequest::StartIconsFor { icon_px: 24, ids: strings(&["d"]) });
        enqueue(&mut sh.pending, ShellRequest::VolumeRefresh);
        enqueue(&mut sh.pending, ShellRequest::LauncherClosed);
        assert_eq!(sh.pending.len(), 2, "the close drops the launcher's pending icons");
        sh.handle_request(ShellRequest::LauncherClosed);
        assert!(sh.launcher_job.is_none());
        assert!(ended(launcher_seen, 0), "killed and reaped before the request returned");
        // Start's job is untouched.
        let still = sh.start_job.as_mut().map(|j| matches!(j.child.try_wait(), Ok(None)));
        assert_eq!(still, Some(true));
        // Start's own release ends it.
        sh.handle_request(ShellRequest::StartRelease);
        assert!(sh.start_job.is_none() && ended(start_seen, 0));
    }

    /// A child that gives no header (it exited at once, its COM start failed, an antivirus stopped it, its input could
    /// not be written) made no icon: the reader marks its job for the in-process path. A child the shell thread stopped
    /// (a late kill, a replaced job) or one whose frame is refused after the header is not marked.
    #[test]
    fn icon_reader_marks_a_child_without_header() {
        let cmd = system_dir().unwrap().join("cmd.exe");
        let refused = std::env::temp_dir().join(format!("dock_icon_refused_{}.bin", std::process::id()));
        std::fs::write(&refused, [ICONS_HEADER, &raw_frame(5, 0, 0, 0)].concat()).unwrap();
        let marked = |args: &[&str], stopped: bool| {
            let mut child = crate::update::spawn_in_job(&cmd, args, true).unwrap();
            let progress = Arc::new(IconProgress::default());
            progress.stopped.store(stopped, SeqCst);
            let (input, output) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
            let reader = IconReader {
                ui: 0,
                client: IconClient::Launcher,
                px: 10,
                ids: strings(&["a", "b"]).into(),
                progress: Arc::clone(&progress),
                started: Instant::now(),
            };
            reader.run(input, output);
            let _ = child.wait();
            assert!(progress.ended.load(SeqCst) && progress.delivered.load(SeqCst) == 0, "{args:?}");
            progress.no_header.load(SeqCst)
        };
        assert!(marked(&["/c", "exit 1"], false), "no output at all");
        assert!(marked(&["/c", "echo", "not icons"], false), "an output that is not an icons header");
        assert!(!marked(&["/c", "exit 1"], true), "stopped by the shell thread");
        assert!(!marked(&["/c", "type", refused.to_str().unwrap()], false), "a frame refused after the header");
        let _ = std::fs::remove_file(&refused);
    }

    #[test]
    fn icon_jobs_without_header_are_made_in_process() {
        // The decision: only a child that gave no header and that the shell thread did not stop. A refused frame or a
        // kill comes from a child at work on an icon, which in-process could crash or hang the shell thread.
        let no_header = IconEnd::NoHeader("no output".into());
        assert!(icons_in_process(&no_header, false));
        assert!(!icons_in_process(&no_header, true), "a late kill or a replaced job");
        assert!(!icons_in_process(&IconEnd::Broken("frame 5 of 2".into()), false), "a refused frame");
        assert!(!icons_in_process(&IconEnd::Done, false));
        // Reaped: Start's ids are queued as in-process batches and stay sent; the launcher's go as one request.
        let mut sh = test_shell();
        sh.start_px = 24;
        let ten: Vec<String> = (0..10).map(|i| format!("app{i}")).collect();
        let (start, start_seen) = fake_job(&ten.iter().map(String::as_str).collect::<Vec<_>>(), false);
        let (launcher, launcher_seen) = fake_job(&["x", "y"], false);
        for p in [&start.progress, &launcher.progress] {
            p.no_header.store(true, SeqCst);
            p.ended.store(true, SeqCst);
        }
        sh.start_sent = ten.iter().cloned().collect();
        sh.start_job = Some(start);
        sh.launcher_job = Some(launcher);
        sh.reap_icon_jobs();
        assert!(sh.start_job.is_none() && sh.launcher_job.is_none());
        assert!(ended(start_seen, 10_000) && ended(launcher_seen, 10_000));
        assert!(sh.icons_child_off);
        assert_eq!(sh.start_sent.len(), 10);
        let queued: Vec<(bool, u32, Vec<String>)> = sh
            .pending
            .drain(..)
            .map(|r| match r {
                ShellRequest::StartIcons { icon_px, ids } => (true, icon_px, ids),
                ShellRequest::StartIconsFor { icon_px, ids } => (false, icon_px, ids),
                other => panic!("{other:?}"),
            })
            .collect();
        let want = [(true, 24, ten[..8].to_vec()), (true, 24, ten[8..].to_vec()), (false, 24, strings(&["x", "y"]))];
        assert_eq!(queued, want);
        // From now on no child starts: the ids come back to be made here.
        assert_eq!(sh.start_icon_job(IconClient::Launcher, 24, strings(&["k"])), Err(strings(&["k"])));
        sh.start_menu_icons(strings(&["m"]));
        assert!(sh.start_job.is_none() && sh.launcher_job.is_none());
        assert!(matches!(sh.pending.pop_front(), Some(ShellRequest::StartIcons { icon_px: 24, ids }) if ids == ["m"]));
    }

    #[test]
    fn finished_and_hung_icon_jobs_are_reaped() {
        let mut sh = test_shell();
        // A child that exited with one id of three answered: reaped once its reader ended, and the two others leave
        // start_sent.
        let (job, seen) = fake_job(&["a", "b", "c"], false);
        let progress = Arc::clone(&job.progress);
        progress.delivered.store(1, Relaxed);
        sh.start_sent = strings(&["a", "b", "c", "z"]).into_iter().collect();
        sh.start_job = Some(job);
        assert!(ended(seen, 10_000));
        if let Some(j) = sh.start_job.as_mut() {
            j.started = Instant::now();
        }
        sh.reap_icon_jobs();
        assert!(sh.start_job.is_some(), "its reader may still be pushing the last icons");
        progress.ended.store(true, Relaxed);
        sh.reap_icon_jobs();
        assert!(sh.start_job.is_none());
        let mut left: Vec<&str> = sh.start_sent.iter().map(String::as_str).collect();
        left.sort_unstable();
        assert_eq!(left, ["a", "z"]);
        // A child still running within its deadline stays; past it (2 s and 50 ms per id) it is killed and reaped.
        let (job, seen) = fake_job(&["x"], true);
        sh.launcher_job = Some(job);
        sh.reap_icon_jobs();
        assert!(sh.launcher_job.is_some());
        if let Some(j) = sh.launcher_job.as_mut() {
            j.started = Instant::now().checked_sub(Duration::from_millis(2_100)).unwrap();
        }
        sh.reap_icon_jobs();
        assert!(sh.launcher_job.is_none() && ended(seen, 0));
        assert!(sh.start_sent.len() == 2, "the launcher's job touches nothing of Start's");
        assert!(sh.pending.is_empty() && !sh.icons_child_off, "a job with a header, or killed: nothing made here");
    }
}
