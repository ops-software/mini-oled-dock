//! "shell" COM STA thread: everything that is COM, Shell or spawns processes.
//!
//! Does: program name resolution, icon extraction/caching, audio (`AudioController`),
//! app launch (de-elevated through the desktop shell, see `launch`). The UI thread NEVER does these things.
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
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
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
    CoAllowSetForegroundWindow, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, IDispatch,
    IPersistFile, IServiceProvider, CLSCTX_ALL, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
    STGM_READ,
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
use crate::launcher::gesture::{is_marker, Gesture};
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
    /// Start menu (Win double tap) and the launcher: enumerates `shell:AppsFolder` ->
    /// `ShellEvent::StartApps`; then, as `icons` says, queues the Start menu's icons (see `IconPlan`).
    ListStartApps { icon_px: u32, icons: IconPlan },
    /// One batch of Start menu icons -> `ShellEvent::StartIcons`. Queued by `ListStartApps` only;
    /// low priority like `ResolveApp`.
    StartIcons { icon_px: u32, ids: Vec<String> },
    /// The Start menu closed: forget the icons sent and drop the queued `StartIcons` batches.
    StartRelease,
    /// Sent by the thread of `start_search::load_settings` when the settings catalog is ready
    /// -> `ShellEvent::StartSettings` (this thread is the one that can wake the UI).
    StartSettingsReady,
    /// Click on a notification whose app has no open window: launches the app that sent it, found in
    /// the AppsFolder catalog by AppUserModelID or display name; Win+N if it cannot be found.
    OpenNotifApp { aumid: String, name: String },
    // ---- the launcher's requests (built by launcher_sys.rs only). Each launch that fails pushes
    // `ShellEvent::LaunchFailed`; the shared requests above never do. Logged as kind and length only.
    /// Open a file or folder of a result row: the path as it is, in one desktop-shell call (no
    /// `updated_exe_path`, no `is_dir`, no working directory).
    OpenPath { path: String },
    /// Run as administrator a typed line or a file row's quoted path (Ctrl+Shift+Enter): `line` split
    /// at `file` (a byte range of it), run by the desktop shell with `runas` (Windows' UAC prompt; the
    /// dock's token is never used). Carries the hook-proven `Gesture`; built only by launcher_sys.rs
    /// (lint 1). The only launcher request with `runas`.
    RunAsTyped { line: String, file: Range<usize>, gesture: Gesture },
    /// Run a typed line (Enter) as it was typed: `line` split at `file`, unexpanded, no working
    /// directory. With `console` (a console program, or Shift+Enter) it runs as `cmd.exe /k <line>`,
    /// `cmd.exe` from `system_dir`.
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
    /// Icons of the launcher's visible rows that lack one -> `ShellEvent::LauncherIcons`. Not a
    /// low-priority request: it runs ahead of the Start menu's batches, and a newer one replaces the
    /// pending one. It never reads or changes what the Start menu was sent.
    StartIconsFor { icon_px: u32, ids: Vec<String> },
    /// A query that found nothing: Windows' typo rewrite of it (`start_search::converted`, a 1.4 MB
    /// read kept off the UI thread) -> `ShellEvent::LauncherConverted`. A newer one replaces the
    /// pending one.
    LauncherConvert { gen: u64, query: String },
    /// The launcher's first open of a session: the shell user's SID and history files ->
    /// `ShellEvent::LauncherState`, read on this thread from that user's `cache\<SID>` only.
    #[allow(dead_code)] // sent by the launcher (L1a-15)
    LauncherStateLoad,
    /// Writes the launcher's history files (`Some` = this content, `None` = left as it is); `delete`
    /// removes both instead ("learn" off, Forget history).
    #[allow(dead_code)] // sent by the launcher (L1a-15)
    LauncherStateSave { frecency: Option<Vec<u8>>, run: Option<Vec<u8>>, delete: bool },
}

/// The icons a `ListStartApps` queues.
#[derive(Clone, Debug)]
pub enum IconPlan {
    /// The Start menu: the icons not sent yet at `icon_px` (the ids in `first`, the menu pins, go
    /// first) as `StartIcons` batches, which run behind every other request.
    All { first: Vec<String> },
    /// The launcher's catalogue: none, and what the Start menu was sent is left as it is (the
    /// launcher asks for its rows' icons with `StartIconsFor`).
    #[allow(dead_code)] // sent by the launcher (L1a-16)
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
    /// The AppsFolder catalog (reply to `ListStartApps`), in enumeration order.
    StartApps(Vec<StartApp>),
    /// Icons of Start menu apps at `icon_px` (`None` = the app has none: the menu keeps a placeholder).
    StartIcons { icon_px: u32, icons: Vec<(String, Option<IconBitmap>)> },
    /// The settings catalog of the Start menu search is ready (`start_search::settings`).
    StartSettings,
    /// A launcher request did not reach the desktop shell, or the shell refused it. Never pushed for
    /// the shared requests (`Launch`, `LaunchAppAdmin`, `RevealApp`).
    #[allow(dead_code)] // read by the launcher (L1a-17)
    LaunchFailed { kind: LaunchKind },
    /// Icons of launcher rows at `icon_px` (reply to `StartIconsFor`; `None` = the app has none).
    #[allow(dead_code)] // read by the launcher (L1a-15)
    LauncherIcons { icon_px: u32, icons: Vec<(String, Option<IconBitmap>)> },
    /// What Windows Search reads the query of generation `gen` as (reply to `LauncherConvert`);
    /// `None` = no rewrite.
    LauncherConverted { gen: u64, meant: Option<String> },
    /// The shell user's SID and the launcher's history files (reply to `LauncherStateLoad`); `None`
    /// for what could not be found or read.
    #[allow(dead_code)] // read by the launcher (L1a-15)
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

fn push_event(ui: isize, ev: ShellEvent) {
    match EVENTS.lock() {
        Ok(mut g) => g.push(ev),
        Err(p) => p.into_inner().push(ev),
    }
    let hwnd = HWND(ui as *mut core::ffi::c_void);
    // SAFETY: PostMessageW is thread-safe; it only fails if the window no longer exists.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_DOCK_SHELL_READY, WPARAM(0), LPARAM(0)) };
}

// ------------------------------------------------------------------ constants

/// Period of the thread timer used as a safety net: if a `WM_SHELL_WAKE` were
/// swallowed by a nested COM message loop, the mpsc queue is still drained within 1 s.
const SAFETY_TIMER_MS: u32 = 1000;
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
    };

    // SAFETY: thread timer (null hwnd, system-chosen id), no callback: it arrives as
    // WM_TIMER in the thread queue. Cancelled at the end.
    let timer = unsafe { SetTimer(None, 0, SAFETY_TIMER_MS, None) };

    loop {
        // SAFETY: `msg` is valid and writable; filter None = all thread messages.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT or error
        }
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
    }
    if timer != 0 {
        // SAFETY: `timer` is the id returned by SetTimer on the same thread.
        let _ = unsafe { KillTimer(None, timer) };
    }
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

    /// Drains the mpsc queue and works through the requests: volume/launch first, then the (slow)
    /// resolutions and Start menu icon batches, re-checking the queue after each request.
    fn pump(&mut self, rx: &Receiver<ShellRequest>) {
        loop {
            while let Ok(r) = rx.try_recv() {
                enqueue(&mut self.pending, r);
            }
            let Some(req) = next_request(&mut self.pending) else { break };
            self.handle_request(req);
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
                let _ = launch_app_admin(&id);
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
            ShellRequest::StartIconsFor { icon_px, ids } => launcher_icons(self.ui, icon_px, ids),
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
            ShellRequest::ListStartApps { icon_px, icons } => self.list_start_apps(icon_px, &icons),
            ShellRequest::StartSettingsReady => push_event(self.ui, ShellEvent::StartSettings),
            ShellRequest::StartRelease => {
                start_release(&mut self.start_px, &mut self.start_sent, &mut self.pending);
                crate::start_search::release_app_index();
            }
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

    /// `ListStartApps`: publishes the catalog, then queues the icons the UI does not have yet.
    fn list_start_apps(&mut self, icon_px: u32, icons: &IconPlan) {
        let t = Instant::now();
        let apps = enum_apps_folder();
        log!("shell: Start menu: {} apps listed in {} ms", apps.len(), t.elapsed().as_millis());
        queue_start_icons(&mut self.start_px, &mut self.start_sent, &mut self.pending, &apps, icon_px, icons);
        // The search data the menu attaches to the catalog (read here, never on the UI thread).
        crate::start_search::load_app_index();
        push_event(self.ui, ShellEvent::StartApps(apps));
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

fn collect_lnk(dir: &Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < INDEX_MAX_DEPTH {
                collect_lnk(&p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            out.push(p);
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
fn build_start_index() -> HashMap<String, String> {
    let mut files = Vec::new();
    for id in [FOLDERID_CommonPrograms, FOLDERID_Programs] {
        if let Some(dir) = known_folder(&id) {
            collect_lnk(&dir, 0, &mut files);
        }
    }
    // SAFETY: COM STA initialized on this thread; ShellLink CLSID with the requested interface.
    let Ok(link) = (unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }) else {
        return HashMap::new();
    };
    let Ok(file) = link.cast::<IPersistFile>() else { return HashMap::new() };
    let mut map: HashMap<String, String> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for lnk in &files {
        let Some(target) = lnk_target(&link, &file, lnk) else { continue };
        let target = if target.contains('%') { expand_env(&target) } else { target };
        if !target.to_lowercase().ends_with(".exe") {
            continue;
        }
        let Some(stem) = lnk.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let lower = stem.to_lowercase();
        if lower.contains("uninstall") || lower.contains("readme") {
            continue;
        }
        // The name the shell shows (Windows' own shortcuts are localized through `desktop.ini`:
        // "Remote Desktop Connection.lnk" is "Connessione Desktop remoto" in Italian, as in the
        // Start menu), else the file name.
        let name = strip_paren_suffix(&lnk_display_name(lnk).unwrap_or(stem));
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
    map.shrink_to_fit();
    log!("shell: Start Menu index: {} shortcuts scanned, {} exe targets", files.len(), map.len());
    map
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
    if !is_elevated() {
        log!("shell: Launch '{target}' args={args:?}");
        return shell_execute(target, args, dir.as_deref());
    }
    match desktop_shell_execute(target, args, dir.as_deref(), Verb::Open, SW_SHOWNORMAL.0) {
        Ok(()) => {
            log!("shell: Launch '{target}' args={args:?} via the desktop shell (de-elevated)");
            Ok(())
        }
        Err(e) => {
            log!("shell: de-elevated launch of '{target}' failed ({e}): REFUSED, never launching elevated");
            Err(format!("de-elevated launch failed: {e}"))
        }
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
fn launch_app_admin(id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("empty app id".into());
    }
    let target = format!("shell:AppsFolder\\{id}");
    match desktop_shell_execute(&target, None, None, Verb::RunAs, SW_SHOWNORMAL.0) {
        Ok(()) => {
            log!("shell: LaunchAppAdmin '{target}' via the desktop shell (runas, UAC prompt)");
            Ok(())
        }
        Err(e) => {
            log!("shell: LaunchAppAdmin '{target}' through the desktop shell failed ({e}): REFUSED");
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
    let Some(path) = app_target_path(id) else {
        log!("shell: RevealApp '{id}': no file to show");
        return Ok(());
    };
    let Some(explorer) = system_dir().and_then(|s| s.parent().map(|w| w.join("explorer.exe"))) else {
        return Err("no Windows folder".into());
    };
    let r = launch(&explorer.to_string_lossy(), Some(&format!("/select,\"{path}\"")));
    if let Err(e) = &r {
        log!("shell: RevealApp '{path}' not executed: {e}");
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

/// The real `Exec`: the desktop shell (Explorer, medium integrity) when the dock is elevated or the
/// verb is `runas` (Explorer asks UAC), as `launch` and `launch_admin` do; the dock's token is never
/// used. Plain `ShellExecuteExW` only for an unelevated dock opening something.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn shell_exec(file: &str, args: Option<&str>, dir: Option<&str>, verb: Verb) -> Result<(), String> {
    if verb == Verb::Open && !is_elevated() {
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

/// What a launcher launch pushes when it failed.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn failure_event(kind: LaunchKind, r: &Result<(), String>) -> Option<ShellEvent> {
    r.is_err().then_some(ShellEvent::LaunchFailed { kind })
}

/// Runs one launcher launch; a failure is logged (kind and error only) and pushed as `LaunchFailed`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_launch(ui: isize, req: ShellRequest) {
    let Some(kind) = launch_kind(&req) else { return };
    let mut exec = shell_exec;
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
        // Catalogue ids and settings targets: these functions log as they do for Start.
        ShellRequest::LauncherLaunch { target, args } => launch(&target, args.as_deref()),
        ShellRequest::LauncherAppAdmin { id } => launch_app_admin(&id),
        ShellRequest::LauncherRevealApp { id } => reveal_app(&id),
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

/// `StartIconsFor`: the icons of the launcher's rows, made at `icon_px` (clamped as the Start menu's).
/// Start's `start_px` and `start_sent` are not touched.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_icons(ui: isize, icon_px: u32, ids: Vec<String>) {
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
fn open_path_with(path: &str, exec: Exec) -> Result<(), String> {
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err("refused: an empty path or a control character".into());
    }
    exec(path, None, None, Verb::Open)
}

/// The file and the arguments of a run line, split where the launcher's syntax-only classification
/// put the file (`file`, a byte range of `line`): the arguments are the trimmed text after the file
/// (after its closing quote when it was quoted). Only the opening quote may stand before the file, and
/// the file must end at a word break or at its closing quote; anything else (a range out of bounds or
/// off a character boundary included) is refused, never a panic. With `console` the file is
/// `<sys>\cmd.exe` and the arguments `/k ` and the whole trimmed line. Nothing is looked up or expanded.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn command_parts(
    line: &str,
    file: Range<usize>,
    console: bool,
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
    if console {
        if !sys.is_absolute() {
            return Err("no system directory");
        }
        return Ok((sys.join("cmd.exe").to_string_lossy().into_owned(), Some(format!("/k {}", line.trim()))));
    }
    let args = rest.trim();
    Ok((part.to_string(), (!args.is_empty()).then(|| args.to_string())))
}

/// `RunAsTyped`: the line split by `command_parts` and run by the desktop shell with `runas`, whether or
/// not the dock is elevated (as `launch_admin`), so Windows' UAC prompt names the program and the dock's
/// token is never used. Refused here again, whatever the launcher checked: any control or bidi
/// character in the line (what is shown is what runs), a range that does not split it, a network file
/// (`\\server` or `//server`). `_proof` is the hook-proven gesture the request carries: nothing
/// reaches this without one.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn run_as_typed_with(line: &str, file: Range<usize>, _proof: &Gesture, exec: Exec) -> Result<(), String> {
    if line.chars().any(is_marker) {
        return Err("refused: a control or bidi character".into());
    }
    let (file, args) = command_parts(line, file, false, Path::new("")).map_err(|e| format!("refused: {e}"))?;
    let start = file.trim_start();
    if start.starts_with("\\\\") || start.starts_with("//") {
        return Err("refused: a network file".into());
    }
    exec(&file, args.as_deref(), None, Verb::RunAs)
}

/// `LaunchCommand`: the line split by `command_parts` (with `console`: `<sys>\cmd.exe /k <line>`), one
/// shell call with no working directory. Nothing is expanded or looked up here: the line goes to the
/// shell as typed.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launch_command_with(line: &str, file: Range<usize>, console: bool, sys: &Path, exec: Exec) -> Result<(), String> {
    let (file, args) = command_parts(line, file, console, sys).map_err(|e| format!("refused: {e}"))?;
    exec(&file, args.as_deref(), None, Verb::Open)
}

/// `RevealPath`: refuses an empty path or one with a double quote (it would end the `/select`
/// argument) or a control character (as `app_target_path`), then exactly one shell call:
/// `<windows>\explorer.exe /select,"<path>"`. Not through `launch`, whose `is_dir` would touch the path.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn reveal_path_with(path: &str, windows: &Path, exec: Exec) -> Result<(), String> {
    if path.trim().is_empty() || path.contains('"') || path.chars().any(char::is_control) {
        return Err("refused: an empty path, a double quote or a control character".into());
    }
    if !windows.is_absolute() {
        return Err("no Windows folder".into());
    }
    let explorer = windows.join("explorer.exe");
    exec(&explorer.to_string_lossy(), Some(&format!("/select,\"{path}\"")), None, Verb::Open)
}

/// The longest web address `OpenUrl` passes on.
const MAX_URL_BYTES: usize = 8 * 1024;

/// `OpenUrl`'s check on the shell thread, whatever the launcher checked: an http or https address
/// rebuilt from its parts, so only what was checked reaches the shell. The scheme in lowercase; a host
/// of letters, digits, hyphens and dots (no empty label) with an optional numeric port; no user name
/// or password before the host (refused: "https://bank.it@evil.example" reads as one host and is
/// another); then the path, query and fragment with every character that is not URL-safe ASCII
/// percent-encoded as UTF-8, so a quote or a bracket can never end the address inside a browser's
/// command line. Whitespace and control characters are refused.
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
    if authority.contains('@') {
        return Err("a user name before the host");
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let label_ok = |l: &str| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || c == '-');
    if !host.split('.').all(label_ok) {
        return Err("not a host name");
    }
    if port.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u16>().is_err()) {
        return Err("not a port");
    }
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
fn open_url_with(url: &str, exec: Exec) -> Result<(), String> {
    let url = rebuild_url(url).map_err(|e| format!("refused: {e}"))?;
    exec(&url, None, None, Verb::Open)
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
/// only the latest keystroke's rows matter.
fn enqueue(pending: &mut VecDeque<ShellRequest>, r: ShellRequest) {
    match r {
        ShellRequest::StartIconsFor { .. } => pending.retain(|p| !matches!(p, ShellRequest::StartIconsFor { .. })),
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

/// The Start menu's part of `ListStartApps`. `IconPlan::All`: the icons not sent yet at `icon_px` are
/// queued as `StartIcons` batches, the pins (`first`) first; a new size starts over. `IconPlan::None`
/// (the launcher's catalogue) touches nothing.
fn queue_start_icons(
    start_px: &mut u32,
    sent: &mut HashSet<String>,
    pending: &mut VecDeque<ShellRequest>,
    apps: &[StartApp],
    icon_px: u32,
    plan: &IconPlan,
) {
    let IconPlan::All { first } = plan else { return };
    let icon_px = icon_px.clamp(8, 256);
    if icon_px != *start_px {
        *start_px = icon_px;
        sent.clear();
        pending.retain(|r| !matches!(r, ShellRequest::StartIcons { .. }));
    }
    let mut missing: Vec<String> = apps.iter().filter(|a| !sent.contains(&a.id)).map(|a| a.id.clone()).collect();
    // Pins first (stable sort: the catalog order is kept inside each group).
    missing.sort_by_key(|id| !first.iter().any(|f| f.eq_ignore_ascii_case(id)));
    sent.extend(missing.iter().cloned());
    for chunk in missing.chunks(START_ICON_BATCH) {
        pending.push_back(ShellRequest::StartIcons { icon_px, ids: chunk.to_vec() });
    }
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
    fn launch_failed_pushed_on_error() {
        let mut calls = Vec::new();
        let r = open_path_with("C:\\a.txt", &mut recorder(&mut calls, false));
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            failure_event(LaunchKind::Path, &r),
            Some(ShellEvent::LaunchFailed { kind: LaunchKind::Path })
        ));
        assert!(failure_event(LaunchKind::Path, &Ok(())).is_none());
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
    fn runas_refuses_a_bad_range() {
        let g = Gesture::for_test(1);
        let mut calls = Vec::new();
        let ranges =
            [("cmd", 0..99), ("\u{e9}", 1..2), ("cmd", Range { start: 3, end: 1 }), ("cmd", 0..0), ("cmd", 1..3)];
        for (line, file) in ranges {
            let shown = format!("{file:?}");
            let e = run_as_typed_with(line, file, &g, &mut recorder(&mut calls, true)).expect_err(&shown);
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
        assert_eq!(
            calls,
            [
                ("C:\\Windows\\System32\\cmd.exe".to_string(), Some("/k ping 1.1.1.1".to_string()), None, Verb::Open),
                ("notepad".to_string(), Some("a.txt".to_string()), None, Verb::Open),
            ]
        );
        // No system directory: no console, and no guess from the environment.
        calls.clear();
        assert!(launch_command_with(&line, file, true, Path::new(""), &mut recorder(&mut calls, true)).is_err());
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
    fn only_launcher_requests_report_failures() {
        let err: Result<(), String> = Err("0x80004005".into());
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
    fn icon_plan_none_leaves_start_sent() {
        let app = |id: &str| StartApp { id: id.into(), name: id.into() };
        let apps = [app("a"), app("b"), app("c")];
        let mut px = 48u32;
        let mut sent = HashSet::from(["a".to_string()]);
        let mut pending: VecDeque<ShellRequest> = VecDeque::new();
        pending.push_back(ShellRequest::StartIcons { icon_px: 48, ids: vec!["z".into()] });
        queue_start_icons(&mut px, &mut sent, &mut pending, &apps, 32, &IconPlan::None);
        assert_eq!((px, sent.len(), pending.len()), (48, 1, 1), "the launcher's catalogue changes nothing of Start's");
        queue_start_icons(&mut px, &mut sent, &mut pending, &apps, 48, &IconPlan::All { first: vec!["C".into()] });
        assert_eq!((px, sent.len(), pending.len()), (48, 3, 2));
        assert!(matches!(&pending[1], ShellRequest::StartIcons { icon_px: 48, ids } if *ids == ["c", "b"]));
        // A new size starts Start's icons over.
        queue_start_icons(&mut px, &mut sent, &mut pending, &apps, 64, &IconPlan::All { first: Vec::new() });
        assert_eq!((px, sent.len(), pending.len()), (64, 3, 1));
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
}
