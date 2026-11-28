//! UI state: items, hover/animations, hit-testing, clicks, context menu, auto-hide.
//!
//! THREADING: UI thread ONLY. State is thread-local (`UI`) or atomic (visibility flags,
//! also read by the Ctrl+C handler only through `main::hwnd`). No blocking work: COM,
//! icons, audio and process launches go to the shell thread via `shell_thread::request`; results
//! come back as `WM_DOCK_SHELL_READY` / `WM_DOCK_VOLUME`.
//!
//! RESPONSIBILITIES
//!  * Builds the item list: pinned apps first (`config.pinned` order), then unpinned
//!    `apps::groups()` by `first_seen`. For every new key it sends
//!    `ShellRequest::ResolveApp` (name + icon at `BarSizes::icon_px`); on a DPI or icon size change it redoes the icon requests.
//!  * Every `redraw()`: builds a `render::Scene`, calls `render::layout`, recomputes the geometry
//!    (bar horizontally centered, width = `layout.width`), `render::resize` if the size
//!    changes, `render::paint`, `render::present`.
//!  * Hover: a plate (`render::Plate`) that animates in over 90 ms and out over 140 ms (`PLATE_IN_MS`, `PLATE_OUT_MS`;
//!    `TIMER_ANIM` timer, 15 ms, out-cubic easing, `KillTimer` when idle) towards the item under the cursor; pressed = alpha 36.
//!  * Mouse: `WM_MOUSEMOVE`/`WM_MOUSELEAVE` (TrackMouseEvent), `WM_SETCURSOR` (hand over items),
//!    `WM_LBUTTONDOWN/UP`, `WM_MBUTTONDOWN/UP`, `WM_RBUTTONUP`, `WM_MOUSEWHEEL`: every gesture resolves
//!    through `actions::resolve` and runs in `run_action` (Shift+click, Shift+middle closes a group, right click on empty
//!    space opens the dock menu, the wheel cycles an app's windows or changes the volume, +-2% a notch).
//!  * Drag & drop: left button down on an item + movement past `DRAG_THRESHOLD` starts a reorder inside
//!    the item's zone (apps / tray icons / system modules). The dragged item follows the cursor, the others slide
//!    (`TIMER_ANIM`); the drop persists `pinned` / `app_order` / `tray_order` / `modules_order`.
//!  * Click: apps (left, middle and right click), bell (Win+N via `SendInput` with
//!    `hook::INJECT_TAG` in `dwExtraInfo`), volume (toggle mute / `ms-settings:apps-volume`), clock
//!    (`ms-settings:dateandtime`), network (popup / right click `ms-settings:network`), metrics
//!    (Task Manager), battery (popup `popup_battery`; without popups `ms-settings:batterysaver`), Start button (fixed item at the left end, not draggable: left click = the dock's Start menu
//!    `popup_startmenu`, right click = quick-links popup, `popup_start`).
//!  * System monitor (`TIMER_SYSMON`, `sysmon_sync`): network and metrics modules, polled only while
//!    the bar is visible; a disabled module has no timer, no state and nothing loaded.
//!  * Battery (`TIMER_BATTERY`, `battery_refresh`, `battery_sync`): `GetSystemPowerStatus` every 30 s only while the
//!    bar is visible and a battery is present, plus `WM_POWERBROADCAST`; a hidden bar takes no sample.
//!  * Themed context menu (`popup::open_app_menu`, command in `WM_DOCK_APP_MENU_CMD`): Pin/Unpin
//!    (`config::pin/unpin`), New window, Close all windows (`apps::close_group`).
//!    While the popup is open, auto-hide is suspended (`popup::is_open`).
//!  * Window picker (`picker`): the pointer resting `PICKER_HOVER_MS` on an app with 2+ windows
//!    (`TIMER_HOVER_PICKER`, one-shot, armed only then) opens live thumbnails of its windows; it
//!    also suspends auto-hide and closes on a click on the bar, another item, or the bar hiding.
//!  * DPI: `WM_DPICHANGED`/`WM_DISPLAYCHANGE` -> recompute metrics, fonts, icons, geometry.
//!  * Display: the bar sits on `config.monitor` (`dock_hmonitor`), the primary monitor when it is
//!    empty or that display is not connected; `WM_DISPLAYCHANGE` moves it when displays come and go.
//!    With `config.monitor == "pointer"` the Win key brings the bar to the display under the pointer
//!    (`follow_pointer`), and a double tap opens the dock's Start menu there (`open_start_menu`, `popup_startmenu`).
//!  * Keyboard (SPEC-dock 5.2.2): the owned chords follow the config, none while a remote-desktop or VM client or a
//!    window above the dock (the UAC prompt) is in front (`chords_sync`, `chords_follow_foreground`,
//!    `hook::set_dock_chords`); the hook
//!    posts `WM_DOCK_CHORD` (Win+n acts on the n-th app, Win+Alt+n, Win+T, Win+Shift+T and Win+B give the bar the
//!    foreground and a focused item) and `WM_DOCK_PEEK` (digit chips while Win is held). On the focused bar
//!    `WM_KEYDOWN`/`WM_CHAR` (and their SYS forms without Alt) move a ring over the items (`bar_keys`), act through
//!    the mouse map, open the picker or a popup with the keyboard in it, reorder with Ctrl+arrows, type ahead, and Esc
//!    hides; a mouse motion, a hide or another window in front ends it.
//!  * Auto-hide (screen edge, Win key, the hide delay (config hide_delay_ms; a keyboard show stays at least 2.5 s)); `config::effective_autohide()`
//!    false = always visible. The edge poll runs every 50 ms while the bar is shown, while a dwell or an
//!    icon request is pending, and for 1 s after the pointer last moved; otherwise raw input waits for the
//!    next motion (see "edge watch": `WM_INPUT`, `RIDEV_INPUTSINK`).
//!
//! REENTRANCY: `SetWindowPos`/`UpdateLayeredWindow` can re-enter (WM_DPICHANGED), so
//! no `borrow` of `UI` is held across calls that pump messages (`SetWindowPos`, `present`).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering::Relaxed};

use windows::core::BOOL;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetFocus, GetKeyState, ReleaseCapture, SendInput, SetCapture, SetFocus, TrackMouseEvent, INPUT,
    INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, TME_LEAVE,
    TRACKMOUSEEVENT, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_F15, VK_LBUTTON, VK_LWIN, VK_RBUTTON, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    RIDEV_REMOVE, RID_INPUT,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, DeregisterShellHookWindow, GetClassNameW, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
    KillTimer, LoadCursorW, RegisterShellHookWindow, RegisterWindowMessageW, SendMessageTimeoutW, SetCursor,
    SetForegroundWindow, SetWindowPos, ShowWindow, SwitchToThisWindow, WindowFromPoint, EVENT_SYSTEM_FOREGROUND,
    GUITHREADINFO, GWL_STYLE, HTCLIENT, HWND_TOP, HWND_TOPMOST, IDC_HAND, MA_NOACTIVATE, PBT_APMPOWERSTATUSCHANGE,
    SC_CLOSE, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WM_CHAR, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL,
    WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETCURSOR, WM_SETTINGCHANGE, WM_SYSCHAR, WM_SYSCOMMAND, WM_SYSKEYDOWN,
    WM_TIMER, WS_CAPTION,
};

use crate::appbar;
use crate::apps::{self, ClickOutcome};
use crate::audio::VolumeState;
use crate::config::{self, PinnedApp};
use crate::icons::IconBitmap;
use crate::log;
use crate::msg::{
    ID_MENU_CLOSE_ALL, ID_MENU_NEW_WINDOW, ID_MENU_PIN, ID_MENU_UNPIN, TIMER_ANIM, TIMER_APPS_DEBOUNCE, TIMER_BATTERY,
    TIMER_CLOCK, TIMER_HOVER_PICKER, TIMER_POLL, TIMER_RECONCILE, TIMER_SYSMON, WM_DOCK_APPS_DIRTY, WM_DOCK_APP_MENU_CMD,
    WM_DOCK_SHELL_READY, WM_DOCK_VOLUME,
};
use crate::popup::{self, Anchor, AppMenuInfo, PopupKind};
use crate::render::{
    AppView, BarSizes, Indicator, ItemId, ItemRect, Layout, Lift, Metrics, Plate, Scene, SystemView, TrayIcon,
};
use crate::shell_thread::{self, ResolveApp, ShellEvent, ShellRequest};
use crate::tray_host::{self, TrayClick, TrayGeometry, TrayView};
use crate::{hook, hwnd, render};

// Child modules via #[path]: this way main.rs does not need to declare them.
#[path = "actions.rs"]
pub(crate) mod actions;
use actions::{resolve, Action, Gesture, SettingsLink, Target, TrayButton};
#[path = "bar_keys.rs"]
pub(crate) mod bar_keys; // DK-PAINT fills it
#[path = "incidents.rs"]
pub(crate) mod incidents; // DS-SAFE fills it
#[path = "suite_links.rs"]
pub(crate) mod suite_links; // DS-ENTRY fills it
#[path = "takeover.rs"]
pub(crate) mod takeover; // DS-GATE fills it
#[path = "takeover_os.rs"]
pub(crate) mod takeover_os; // DS-DELIVER fills it

/// Push reveal is built and S-PUSH passed; settings_pages shows the Reveal row.
pub(crate) const PUSH_AVAILABLE: bool = true;

// ---------------------------------------------------------------- constants

/// Time the dock stays visible after startup (ms).
const SHOW_MS_STARTUP: u64 = 3000;
/// How long a show by the keyboard (Win tap, Win+T, Win+B, Win+Alt+n) stays without the pointer over the bar (ms);
/// never shorter than 1.5.2's.
const KEY_SHOW_MS: u64 = 2500;

/// Time the bar stays after the pointer leaves it (config `hide_delay_ms`).
fn hide_delay() -> u64 {
    u64::from(config::get().hide_delay_ms)
}

/// Time a show stays when the pointer did not start it on the bar (keyboard or screen edge; the edge row lies
/// outside the floating bar): the hide delay, but never under `KEY_SHOW_MS`, or a delay of 0 would hide the
/// bar at the next poll.
fn key_show_ms(hide_delay_ms: u32) -> u64 {
    u64::from(hide_delay_ms).max(KEY_SHOW_MS)
}

/// true once the pointer has dwelt on the edge (since `since`, 0 = not on it) for the show delay.
fn dwell_done(since: u64, now: u64, delay: u64) -> bool {
    since != 0 && now - since >= delay
}

/// Instant (ms) at which the bar hides when the countdown restarts at `now`.
fn hide_at(now: u64, delay_ms: u64) -> u64 {
    now + delay_ms
}
/// Provisional bar width (logical px) before the first layout.
const DEFAULT_BAR_WIDTH: i32 = 400;
/// Duration of the hover plate's entry animation (ms).
const PLATE_IN_MS: u64 = oled_ui::tokens::DUR_FAST as u64;
/// Duration of the plate's exit fade (ms).
const PLATE_OUT_MS: u64 = oled_ui::tokens::DUR_NORMAL as u64;
/// Animation timer period (ms).
const ANIM_TICK_MS: u32 = 15;
/// Plate alpha when hovered / pressed.
const HOVER_ALPHA: u8 = oled_ui::tokens::FILL_HOVER as u8;
const PRESS_ALPHA: u8 = oled_ui::tokens::FILL_PRESS as u8;
/// Debounce after `WM_DOCK_APPS_DIRTY` (ms).
const DEBOUNCE_MS: u32 = 100;
/// Pointer resting on an app item with 2+ windows for this long opens the window picker (ms).
const PICKER_HOVER_MS: u32 = oled_ui::tokens::HOVER_DELAY;
/// Periodic reconciliation of the window list (ms).
const RECONCILE_MS: u32 = 5000;
/// Timer for the bar width animation (local id, distinct from those in `msg`).
const TIMER_WIDTH: usize = 40;
/// After a popup closes, a foreground change to the fullscreen app within this
/// time (ms) is the automatic focus return and does not hide the dock.
const POPUP_CLOSE_GRACE_MS: u64 = 400;
/// Right after the dock/popup take the foreground over a fullscreen app, a foreground change back
/// to that app within this time (ms) is the app bouncing it back (or a stale event), not the user.
const FS_BOUNCE_GRACE_MS: u64 = 800;
/// How many times the foreground is re-taken after such a bounce, per show of the dock.
const FS_MAX_RETAKES: u32 = 2;
/// Duration of the width animation (ms, out-cubic).
const WIDTH_ANIM_MS: u64 = oled_ui::tokens::DUR_SLOW as u64;
/// Timer of the attention blink (local id).
const TIMER_ATTN: usize = 41;
/// Timer of the show/hide slide+fade (local id).
const TIMER_VIS: usize = 42;
/// Blink half period (ms) and total blinking time before the indicator stays steady orange.
const ATTN_HALF_MS: u64 = 500;
const ATTN_BLINK_MS: u64 = 6000;
/// Show / hide animation durations (ms) and slide distance (logical px).
const SHOW_ANIM_MS: u64 = oled_ui::tokens::DUR_NORMAL as u64;
const HIDE_ANIM_MS: u64 = 100;
const SLIDE_PX: i32 = oled_ui::tokens::SLIDE_PX_BAR;
/// Shell hook notifications (wParam of the registered "SHELLHOOK" message).
const HSHELL_WINDOWACTIVATED: usize = 4;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;
/// `WM_MOUSELEAVE` (defined here so as not to depend on the feature that exports it).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// `WM_CAPTURECHANGED`: the window lost the mouse capture.
const WM_CAPTURECHANGED: u32 = 0x0215;
/// Horizontal movement (logical px) after which a press becomes a drag.
const DRAG_THRESHOLD: i32 = oled_ui::tokens::DRAG_THRESHOLD;
/// Duration (ms) of the slide of the other items / of the drop settle (out-cubic).
const SLIDE_MS: u64 = 120;
/// Duration (ms) of the indicator width change when the foreground moves (out-cubic).
const ACT_ANIM_MS: u64 = 120;
/// Period of `TIMER_SYSMON` (ms): one CPU/RAM sample per tick.
const SYSMON_MS: u32 = 2000;
/// The network is queried every this many sysmon ticks (~6 s), the first tick included.
pub const NET_POLL_TICKS: u32 = 3;
/// N key (Win+N opens the notification center).
const VK_N: VIRTUAL_KEY = VIRTUAL_KEY(0x4E);

// ------------------------------------------------------------------- state

static VISIBLE: AtomicBool = AtomicBool::new(false);
static HIDE_AT: AtomicU64 = AtomicU64::new(0);
static EDGE_SINCE: AtomicU64 = AtomicU64::new(0);
static EDGE_ARMED: AtomicBool = AtomicBool::new(true);
/// true if the dock was shown over a fullscreen / borderless app.
static FS_SHOWN: AtomicBool = AtomicBool::new(false);
/// HWND (as isize, 0 = none) that had the foreground before the dock took it (Win key, fullscreen
/// app), to give it the foreground back when the dock hides. With `FS_SHOWN` it is the fullscreen app.
static PREV_FG: AtomicIsize = AtomicIsize::new(0);
/// Tick (ms) of the last `force_foreground` call (0 = never).
static FG_TAKEN_AT: AtomicU64 = AtomicU64::new(0);
/// Foreground re-takes after a fullscreen bounce since the dock was shown.
static FS_RETAKES: AtomicU32 = AtomicU32::new(0);
/// Previous state of the left/right mouse buttons (falling-edge detection).
static CLICK_DOWN: AtomicBool = AtomicBool::new(false);
/// Id of the registered "SHELLHOOK" window message (0 = not registered).
static SHELLHOOK_MSG: AtomicU32 = AtomicU32::new(0);
/// Tick (ms) of the last new attention request: origin of the blink phase.
static ATTN_START: AtomicU64 = AtomicU64::new(0);

/// Geometry of the dock and of its display (physical pixels, per-monitor DPI).
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Rectangle of the bar's display (`config.monitor`, primary by default).
    pub mon: RECT,
}

/// An app item (pinned and/or with windows) in the UI state.
#[derive(Clone, Debug, Default)]
struct Entry {
    /// Item key (the pin's key if pinned, otherwise the group's).
    key: String,
    /// Window group key to use with `apps::*` (may differ from `key` in case).
    group_key: String,
    /// Displayed name (config.pinned.name or the shell thread's reply); empty until it arrives.
    name: String,
    /// true if the name must not be overwritten by the shell thread (currently never: a pin's stored name
    /// is only a fallback).
    fixed_name: bool,
    /// Icon already at the current physical size; `None` = placeholder.
    icon: Option<IconBitmap>,
    /// Side (physical px) for which `ShellRequest::ResolveApp` was sent (0 = never).
    req_px: u32,
    pinned: bool,
    /// Launch target (pin, or derived from exe/AUMID).
    launch: Option<String>,
    exe_path: String,
    aumid: Option<String>,
    /// Custom icon of the pin (plain name inside `datadir::icons_dir()`).
    custom_icon: Option<String>,
    /// Group of current windows (empty = no windows).
    windows: Vec<isize>,
    active: bool,
    /// Indicator "foreground" level animation: `act_from` -> `act_to` (0 or 1) from tick `act_start`
    /// (0 = settled).
    act_from: f32,
    act_to: f32,
    act_start: u64,
}

/// Indicator level 0..=1 of the entry at instant `now` (out-cubic).
fn act_value(e: &Entry, now: u64) -> f32 {
    if e.act_start == 0 {
        return e.act_to;
    }
    let t = (now.saturating_sub(e.act_start) as f32 / ACT_ANIM_MS as f32).clamp(0.0, 1.0);
    e.act_from + (e.act_to - e.act_from) * (1.0 - (1.0 - t).powi(3))
}

/// Hover plate animation.
#[derive(Clone, Copy, Debug, Default)]
struct PlateAnim {
    from: RECT,
    to: RECT,
    /// Start tick (ms).
    start: u64,
    /// Start/end alpha (0..=255).
    from_alpha: u8,
    to_alpha: u8,
    /// Duration (ms) of this animation: shorter when entering than when leaving.
    dur: u64,
    running: bool,
}

/// Width animation of the pill (the bar stays centered).
#[derive(Clone, Copy, Debug, Default)]
struct WidthAnim {
    from: i32,
    to: i32,
    /// Start tick (ms).
    start: u64,
    running: bool,
}

/// Interpolated width (out-cubic) at instant `now`.
fn width_at(a: &WidthAnim, now: u64) -> i32 {
    let t = (now.saturating_sub(a.start) as f32 / WIDTH_ANIM_MS as f32).clamp(0.0, 1.0);
    lerp_i32(a.from, a.to, 1.0 - (1.0 - t).powi(3))
}

/// Shifts the layout by `off` px and widens it to `new_w`: needed when the window is wider than the
/// final layout (shrinking pill), so the items stay centered.
fn shift_layout(l: &mut Layout, off: i32, new_w: i32) {
    let mv = |r: &mut RECT| {
        r.left += off;
        r.right += off;
    };
    for it in &mut l.items {
        mv(&mut it.rect);
    }
    mv(&mut l.tray_area);
    if l.separator_x >= 0 {
        l.separator_x += off;
    }
    if l.separator2_x >= 0 {
        l.separator2_x += off;
    }
    l.width = new_w;
}

/// Show/hide animation: visibility level 0..=1 (alpha and slide) moving `from` -> `to`.
#[derive(Clone, Copy, Debug)]
struct VisAnim {
    from: f32,
    to: f32,
    start: u64,
    dur: u64,
    running: bool,
}

/// Current visibility level: ease-out-cubic when showing, ease-in when hiding.
fn vis_level(a: &VisAnim, now: u64) -> f32 {
    if !a.running {
        return a.to;
    }
    let t = (now.saturating_sub(a.start) as f32 / a.dur.max(1) as f32).clamp(0.0, 1.0);
    let e = if a.to >= a.from { 1.0 - (1.0 - t).powi(3) } else { t * t };
    a.from + (a.to - a.from) * e
}

#[derive(Default)]
struct UiState {
    /// Width of the displayed pill (physical px; 0 = never drawn).
    shown_w: i32,
    wanim: WidthAnim,
    /// Bar metrics: the monitor DPI.
    metrics: Metrics,
    /// Content sizes of the bar (`render::BarSizes::from_config`).
    sizes: BarSizes,
    /// Effective DPI of the bar's monitor (the popups use it).
    monitor_dpi: u32,
    entries: Vec<Entry>,
    layout: Layout,
    /// Last battery sample (`battery::BatteryStatus::NONE` until the module is sampled).
    battery: crate::battery::BatteryStatus,
    volume: VolumeState,
    hover: Option<ItemId>,
    pressed: Option<ItemId>,
    plate: PlateAnim,
    tracking_mouse: bool,
    /// Current tray icons (`tray_host::snapshot()`).
    tray: Vec<TrayView>,
    /// Cached `tray_host::any_user_hidden()` (recomputed in `refresh_tray_state`, not per frame).
    tray_overflow: bool,
    /// Pending notifications (bell badge).
    notif_count: usize,
    /// "Do Not Disturb" state (None = unreadable).
    notif_dnd: Option<bool>,
    /// Last left click on a tray icon: (id, tick ms, screen point), for double-click detection.
    last_tray_click: Option<(u64, u64, POINT)>,
    /// Last geometry/rects sent to the tray host (flat key of integers).
    last_tray_geo: Vec<i32>,
    /// Client x of the last left-button press, and whether the pressed item can be dragged.
    press_x: i32,
    press_drag: bool,
    /// Reorder in progress (or settling after the drop).
    drag: Option<DragState>,
    /// Network glyph state (last `ShellEvent::Network`); default while the module is off.
    net: crate::net::NetStatus,
    /// CPU / RAM usage shown by the metrics module.
    usage: crate::metrics::Usage,
    /// CPU sampler: exists only while the metrics module is on (`sysmon_sync`).
    sampler: Option<crate::metrics::Sampler>,
}

thread_local! {
    static GEOM: Cell<Option<Geom>> = const { Cell::new(None) };
    /// HMONITOR (as isize) the bar was last placed on, to log a display change once.
    static LAST_HMON: Cell<isize> = const { Cell::new(0) };
    /// "pointer" mode (`config::MONITOR_POINTER`): HMONITOR (as isize) the Win key last brought the
    /// bar to; 0 = not chosen yet (the display under the pointer is used).
    static FOLLOW_HMON: Cell<isize> = const { Cell::new(0) };
    static POLL_TICKS: Cell<u32> = const { Cell::new(0) };
    static VIS: Cell<VisAnim> = const { Cell::new(VisAnim { from: 0.0, to: 0.0, start: 0, dur: 1, running: false }) };
    /// Windows that asked for attention (FlashWindowEx) and were not activated yet.
    static ATTN: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
    /// Ids of the overflow tray icons in the last drawn layout (kept outside `UI` so it can be
    /// read from the popup without risking a double borrow).
    static TRAY_HIDDEN: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// true if some items have no ResolveApp request because the shell thread was not ready yet.
    static NEED_REQUESTS: Cell<bool> = const { Cell::new(false) };
    /// Edge watch (see "edge watch"): true while `TIMER_POLL` is armed (no timer is armed when the UI thread starts).
    static POLL_ON: Cell<bool> = const { Cell::new(false) };
    /// Edge watch: true while the raw input registration is in place.
    static RAW_ON: Cell<bool> = const { Cell::new(false) };
    /// Edge watch: false after a failed registration, until the next show (the poll then stays on).
    static GATE_OK: Cell<bool> = const { Cell::new(MOTION_GATE) };
    /// Edge watch: the pointer position read by the last poll.
    static LAST_POS: Cell<Option<POINT>> = const { Cell::new(None) };
    /// Edge watch: `now_ms()` when the pointer was last seen to move.
    static STILL_SINCE: Cell<u64> = const { Cell::new(0) };
    /// Push reveal: true while the bar is hidden, the mode is Push and the last poll saw the pointer on the edge row.
    static PUSH_EDGE: Cell<bool> = const { Cell::new(false) };
    /// Push reveal: the sum of the moves towards the edge (see `push_step`).
    static PUSH_SUM: Cell<PushSum> = const { Cell::new(PushSum { sum: 0, since: 0 }) };
    /// Push reveal: the threshold of the config, read by the poll (the raw input arm does not read the config).
    static PUSH_THRESHOLD: Cell<u32> = const { Cell::new(0) };
    /// Push reveal: true when the bar is at the top, cached by the poll next to the threshold.
    static PUSH_TOP: Cell<bool> = const { Cell::new(false) };
    /// Push reveal: `now_ms()` of the last raw input message that ran the poll body (`raw_poll_due`).
    static RAW_POLL_AT: Cell<u64> = const { Cell::new(0) };
    /// `now_ms()` of the last `refresh_apps` (`apps_fresh`).
    static LAST_APPS_REFRESH: Cell<u64> = const { Cell::new(0) };
    /// true if the apps debounce timer is armed.
    static DEBOUNCE_ARMED: Cell<bool> = const { Cell::new(false) };
    /// `config.names` at the last `rebuild_entries` (to redo the requests if it changes).
    static LAST_NAMES: RefCell<Option<std::collections::BTreeMap<String, String>>> = const { RefCell::new(None) };
    /// Group key of the App item under the last middle press (None elsewhere), for Shift+middle's CloseGroup.
    static MID_PRESS: RefCell<Option<String>> = const { RefCell::new(None) };
    /// `now_ms()` of the last wheel cycle step (`cycle_due`).
    static LAST_CYCLE: Cell<u64> = const { Cell::new(0) };
    /// Wheel remainder (WHEEL_DELTA units) not yet translated into volume steps.
    static WHEEL_ACC: Cell<i32> = const { Cell::new(0) };
    /// true while `TIMER_SYSMON` is armed; ticks since it was armed.
    static SYSMON_ARMED: Cell<bool> = const { Cell::new(false) };
    /// Metrics flag the running `TIMER_SYSMON` period was computed for.
    static SYSMON_PERIOD_METRICS: Cell<bool> = const { Cell::new(false) };
    static SYSMON_TICKS: Cell<u32> = const { Cell::new(0) };
    /// true once a `NetworkQuery` was sent: the shell thread holds a `net::Querier` to release.
    static NET_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// true while the shell thread is expected to hold the audio controller (volume module on).
    static AUDIO_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// The chord rows the config owns (`chords_sync`); the hook gets none of them while a remote client or a window
    /// above the dock is in front.
    static CHORDS: Cell<hook::DockChords> = const { Cell::new(hook::DockChords::NONE) };
    /// What was in front at the last look (`chords_follow_foreground`).
    static FRONT: Cell<Front> = const { Cell::new(Front::Local) };
    /// The bar's keyboard layer (see "keyboard: the focused bar").
    static KB: RefCell<Kb> = const { RefCell::new(Kb::EMPTY) };
    /// Peek (`on_peek`).
    static PEEK: Cell<Peek> = const { Cell::new(Peek { chips: false, shown: false }) };
}

fn now_ms() -> u64 {
    // SAFETY: GetTickCount64 has no preconditions.
    unsafe { GetTickCount64() }
}

// ------------------------------------------------------------------ public API

/// Prepares the UI state BEFORE the window exists: DPI of the bar's display, provisional geometry
/// (centered bar, provisional width). Returns the `Geom` to create the window with.
/// Called by `main::run` after `config::init()`.
pub fn init() -> Result<Geom, String> {
    let dpi = dock_dpi();
    let sizes = BarSizes::from_config(&config::get());
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.metrics = Metrics { dpi };
        st.sizes = sizes;
        st.monitor_dpi = dpi;
    });
    let g = compute_geom(config_top(), Metrics { dpi }.px(DEFAULT_BAR_WIDTH), dpi, sizes)?;
    GEOM.with(|c| c.set(Some(g)));
    log!(
        "initial geometry: x={} y={} w={} h={} dpi={dpi} (monitor {}x{})",
        g.x,
        g.y,
        g.w,
        g.h,
        g.mon.right - g.mon.left,
        g.mon.bottom - g.mon.top
    );
    Ok(g)
}

/// After the window is created: `render::init`, initial items from the config, the UI's own timers
/// (`TIMER_POLL` 50 ms, `TIMER_RECONCILE` 5 s, `TIMER_CLOCK` aligned to the minute),
/// first `redraw`, and initial visibility of `SHOW_MS_STARTUP` ms (always visible if
/// `!config::effective_autohide()`), a plain show that takes the foreground only with `--foreground`
/// (`show_dock_as`'s `startup`). Also installs `apps::install_winevent_hook`.
pub fn attach(h: HWND) -> Result<(), String> {
    // A strip left reserved by a dock that died (marker file): freed before anything else.
    appbar::release_all("startup");
    let (metrics, sizes) = UI.with(|u| {
        let st = u.borrow();
        (st.metrics, st.sizes)
    });
    render::init(metrics, sizes)?;
    if let Err(e) = apps::install_winevent_hook() {
        log!("ui::attach: apps WinEventHook not installed: {e}");
    }
    tray_host::set_icon_px((sizes.tray_px(metrics)).max(metrics.px(16)) as u32);
    refresh_tray_state();
    apps::refresh(h);
    STILL_SINCE.set(now_ms());
    rebuild_entries();
    poll_sync();
    reconcile_sync(h);
    arm_clock(h);
    // SAFETY: registering a window message by name has no preconditions.
    let shell_msg = unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) };
    if shell_msg == 0 {
        log!("ui::attach: RegisterWindowMessage(SHELLHOOK) failed");
    } else {
        SHELLHOOK_MSG.store(shell_msg, Relaxed);
        // SAFETY: `h` is our live top-level window.
        if !unsafe { RegisterShellHookWindow(h) }.as_bool() {
            log!("ui::attach: RegisterShellHookWindow failed");
        }
    }
    chords_sync();
    suite_links::refresh();
    takeover::refresh_gate();
    let ms = if config::effective_autohide() { SHOW_MS_STARTUP } else { u64::MAX / 4 };
    show_dock_as("startup", ms, false, true);
    Ok(())
}

/// Id of the registered "SHELLHOOK" window message, 0 while none is registered (`main::wnd_proc`: it does not beat).
pub(crate) fn shell_hook_msg() -> u32 {
    SHELLHOOK_MSG.load(Relaxed)
}

/// All window messages NOT handled directly by `main`. Returns `None` if the
/// message is of no interest (then `main` calls `DefWindowProcW`). Messages the UI must handle:
///  * `WM_TIMER` for `TIMER_POLL`, `TIMER_RECONCILE`, `TIMER_APPS_DEBOUNCE`, `TIMER_ANIM`, `TIMER_CLOCK`;
///  * `WM_DOCK_APPS_DIRTY` (starts the debounce), `WM_DOCK_SHELL_READY` (`shell_thread::drain_events`),
///    `WM_DOCK_VOLUME` (`audio::decode_volume_msg`);
///  * mouse (see the module doc), `WM_MOUSEACTIVATE` (-> `MA_NOACTIVATE`), `WM_DPICHANGED`,
///    `WM_DISPLAYCHANGE`, `WM_SETTINGCHANGE` (with "intl" -> `clock::invalidate_formats`).
pub fn handle_message(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg != 0 && msg == SHELLHOOK_MSG.load(Relaxed) {
        on_shell_hook(h, wp.0, lp.0);
        return Some(LRESULT(0));
    }
    match msg {
        WM_TIMER => on_timer(h, wp.0),
        WM_POWERBROADCAST => {
            if let Some(why) = cut_off_of(msg, wp.0) {
                crate::hook::cut_off(why);
            }
            if wp.0 == PBT_APMPOWERSTATUSCHANGE as usize {
                battery_tick();
            }
            if wp.0 == windows::Win32::UI::WindowsAndMessaging::PBT_APMSUSPEND as usize {
                popup::popup_launcher::on_suspend();
            }
            if wp.0 == windows::Win32::UI::WindowsAndMessaging::PBT_APMRESUMEAUTOMATIC as usize {
                popup::popup_launcher::on_resume();
                crate::hook::note_activity();
                crate::tray_host::raise_after("resume");
                // Displays wake up and may come back with another work area.
                appbar_sync("resume", true);
            }
            // TRUE grants the event.
            Some(LRESULT(1))
        }
        WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
        // AppBar (`appbar`): Explorer's notifications, and the activation / position messages it wants forwarded.
        m if m == crate::msg::WM_DOCK_APPBAR => {
            if appbar::on_notify(wp.0, lp.0, fullscreen_where()) {
                appbar_sync("AppBar notification", true);
            }
            Some(LRESULT(0))
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATE => {
            appbar::on_activate(h);
            None
        }
        windows::Win32::UI::WindowsAndMessaging::WM_WINDOWPOSCHANGED => {
            appbar::on_window_pos_changed(h, lp);
            None
        }
        // The bar can hold the foreground (Win key): Alt+F4 must not reach WM_CLOSE, which exits the dock.
        WM_SYSCOMMAND if wp.0 & 0xFFF0 == SC_CLOSE as usize => {
            log!("Alt+F4 / SC_CLOSE on the bar ignored");
            Some(LRESULT(0))
        }
        WM_DOCK_APP_MENU_CMD => {
            on_app_menu_cmd(wp.0 as u32);
            Some(LRESULT(0))
        }
        // The launcher's chord (posted by the hook only while a chord is set): wParam = sequence, lParam = kb.time.
        crate::msg::WM_DOCK_LAUNCHER => {
            popup::popup_launcher::on_chord(wp.0 as u32, lp.0 as u32);
            Some(LRESULT(0))
        }
        // The chord passed under YIELD (posted only while a chord is set): lParam = kb.time.
        crate::msg::WM_DOCK_LAUNCHER_YIELDED => {
            popup::popup_launcher::on_yielded(lp.0 as u32);
            Some(LRESULT(0))
        }
        // The chord passed under FULLSCREEN or failed open (posted only while a chord is set): wParam = the newest
        // sequence, which the hook takes as acknowledged; the pass flags are computed again.
        crate::msg::WM_DOCK_LAUNCHER_PASSED => {
            popup::popup_launcher::on_passed(wp.0 as u32);
            Some(LRESULT(0))
        }
        // The close the launcher's first activation asked for, run outside it (posted by the launcher only): wParam =
        // the open's sequence.
        crate::msg::WM_DOCK_LAUNCHER_CLOSE => {
            popup::popup_launcher::on_deferred_close(wp.0 as u32);
            Some(LRESULT(0))
        }
        // PowerToys (posted only while the chord is Alt+Space): wParam 0 a watched chord owner exited, 1-4 the notice's
        // answer.
        crate::msg::WM_DOCK_LAUNCHER_PT => {
            popup::popup_launcher::on_pt_message(wp.0);
            Some(LRESULT(0))
        }
        // An alias saved or removed in the launcher (posted by the launcher only): config.json is written here.
        crate::msg::WM_DOCK_LAUNCHER_ALIAS => {
            popup::popup_launcher::on_alias_message();
            Some(LRESULT(0))
        }
        // An owned dock chord (posted by the hook only while chord rows are set): wParam = hook::encode_chord, lParam =
        // kb.time.
        crate::msg::WM_DOCK_CHORD => {
            on_chord(wp.0, lp.0 as u32);
            Some(LRESULT(0))
        }
        // Keys on the bar, which is the foreground window after Win, Win+T, Win+B or Win+Alt+n. WM_SYSKEYDOWN without the
        // Alt bit is what an active window without the keyboard focus receives: a plain key. With Alt held it goes to
        // DefWindowProc, so Alt+F4 keeps its SC_CLOSE path above.
        WM_KEYDOWN | WM_SYSKEYDOWN => match as_key(msg, lp.0) {
            // lParam bit 30: the key was already down (an auto-repeat).
            Some(false) => on_bar_key(wp.0 as u16, (lp.0 >> 30) & 1 != 0),
            _ => None,
        },
        // Typed characters (type-ahead), by the same rule: WM_SYSCHAR without the Alt bit is a plain character.
        WM_CHAR | WM_SYSCHAR if as_char(msg, lp.0) => on_bar_char(wp.0),
        // Peek (posted by the hook only while Peek is owned): wParam 1 began, 0 ended; lParam = kb.time.
        crate::msg::WM_DOCK_PEEK => {
            on_peek(wp.0 != 0);
            Some(LRESULT(0))
        }
        // Registered once the launcher's chord or a dock chord row is set (`launcher_sys::watch_sessions`): lock, unlock,
        // connects and disconnects.
        windows::Win32::UI::WindowsAndMessaging::WM_WTSSESSION_CHANGE => {
            if let Some(why) = cut_off_of(msg, wp.0) {
                crate::hook::cut_off(why);
            }
            popup::popup_launcher::on_session_change(wp.0 as u32);
            Some(LRESULT(0))
        }
        WM_DOCK_APPS_DIRTY => {
            if wp.0 as u32 == EVENT_SYSTEM_FOREGROUND {
                on_shell_hook(h, HSHELL_WINDOWACTIVATED, lp.0);
                on_foreground_changed(HWND(lp.0 as *mut std::ffi::c_void));
                takeover::on_foreground(HWND(lp.0 as *mut std::ffi::c_void));
                chords_follow_foreground();
                peek_fullscreen_sync(CHORDS.get());
                kb_on_foreground();
                crate::hook::note_activity();
                crate::tray_host::raise();
            }
            request_apps_refresh(h);
            Some(LRESULT(0))
        }
        WM_DOCK_SHELL_READY => {
            let mut changed = false;
            for ev in shell_thread::drain_events() {
                changed |= apply_shell_event(ev);
            }
            if changed {
                // New name/icon can change the width: the layout is recomputed even when the bar is hidden.
                redraw();
            }
            Some(LRESULT(0))
        }
        WM_DOCK_VOLUME => {
            let st = crate::audio::decode_volume_msg(wp.0, lp.0);
            UI.with(|u| u.borrow_mut().volume = st);
            crate::popup::on_volume(st);
            redraw_if_visible();
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE => {
            on_mouse_move(h, wp, lp);
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            on_mouse_leave();
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED => {
            on_capture_lost();
            Some(LRESULT(0))
        }
        WM_SETCURSOR => on_set_cursor(lp),
        WM_LBUTTONDOWN => {
            on_lbutton_down(h, lp);
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            on_lbutton_up(wp, lp);
            Some(LRESULT(0))
        }
        WM_MBUTTONDOWN => {
            on_mbutton_down(wp, lp);
            Some(LRESULT(0))
        }
        WM_MBUTTONUP => {
            on_mbutton_up(wp, lp);
            Some(LRESULT(0))
        }
        WM_RBUTTONUP => {
            on_rbutton_up(lp);
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL => {
            on_mouse_wheel(wp, lp);
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            picker_cancel("display changed");
            crate::tray_host::raise_after("DPI change");
            apply_dpi(if dpi >= 96 { dpi } else { dock_dpi() });
            // A new scale for the bar's display comes without WM_DISPLAYCHANGE: the strip takes the new bar height.
            appbar_sync("DPI change", false);
            Some(LRESULT(0))
        }
        WM_DISPLAYCHANGE => {
            // Also a display plugged in or out: the bar moves to (or back to) its chosen display.
            picker_cancel("display changed");
            crate::tray_host::raise_after("display change");
            apply_dpi(dock_dpi());
            appbar_sync("display change", true);
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            on_setting_change(lp);
            None // DefWindowProc may do its own thing
        }
        windows::Win32::UI::WindowsAndMessaging::WM_INPUT => {
            // Raw input waits for the pointer's first motion (see "edge watch"). The few messages still queued after
            // the registration was removed find RAW_ON false. None: the caller runs DefWindowProc (RIM_INPUT cleanup).
            if RAW_ON.get() {
                let now = now_ms();
                // While pushing: only the report's vertical delta and the accumulator per message; the rest of the
                // arm at most once per poll period.
                if PUSH_EDGE.get() {
                    if let Some(dy) = raw_mouse_dy(lp) {
                        let (sum, hit) =
                            push_step(PUSH_SUM.get(), towards_edge(dy, PUSH_TOP.get()), now, PUSH_THRESHOLD.get());
                        PUSH_SUM.set(sum);
                        if hit {
                            push_show();
                        }
                    }
                }
                if !PUSH_EDGE.get() {
                    STILL_SINCE.set(now);
                    poll_sync();
                    on_poll();
                    crate::hook::note_activity();
                } else if raw_poll_due(RAW_POLL_AT.get(), now) {
                    // While pushing the poll timer (PollRaw) already runs the body; only without it (Motion) does
                    // the arm run it, so the body never runs more than once per poll period.
                    RAW_POLL_AT.set(now);
                    STILL_SINCE.set(now);
                    crate::hook::note_activity();
                    if !POLL_ON.get() {
                        poll_sync();
                        on_poll();
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn on_timer(h: HWND, id: usize) -> Option<LRESULT> {
    match id {
        TIMER_POLL => {
            if NEED_REQUESTS.with(|n| n.get()) && shell_thread::thread_id() != 0 {
                request_missing();
            }
            on_poll();
        }
        TIMER_RECONCILE => {
            refresh_apps();
            request_missing();
        }
        TIMER_APPS_DEBOUNCE => {
            kill_timer(h, TIMER_APPS_DEBOUNCE);
            DEBOUNCE_ARMED.with(|d| d.set(false));
            refresh_apps();
        }
        TIMER_ANIM => on_anim_tick(h),
        TIMER_WIDTH => on_width_tick(h),
        TIMER_ATTN => on_attn_tick(h),
        TIMER_VIS => on_vis_tick(h),
        TIMER_SYSMON => sysmon_tick(),
        TIMER_BATTERY => battery_tick(),
        TIMER_HOVER_PICKER => on_hover_picker_timer(h),
        TIMER_CLOCK => {
            if VISIBLE.load(Relaxed) && config::get().show_clock {
                // Re-armed one-shot: SetTimer on the same id replaces the previous interval.
                set_timer(h, TIMER_CLOCK, crate::clock::tick_ms());
                redraw();
            } else {
                // Hidden bar or clock module: no per-tick redraws; `arm_clock` re-arms the timer.
                kill_timer(h, TIMER_CLOCK);
            }
        }
        _ => {
            // The launcher's one-shots (`launcher_sys::arm_once`): on this window, never destroyed while the dock runs.
            if !popup::on_launcher_timer(h, id) {
                return None;
            }
        }
    }
    Some(LRESULT(0))
}

/// Win key alone (`WM_DOCK_TOGGLE`): shows or hides the dock. In "pointer" mode the bar first moves
/// to the display under the pointer; if it was visible on another display it stays visible there.
pub fn toggle() {
    let moved = follow_pointer();
    if VISIBLE.load(Relaxed) && !moved {
        hide_dock("Win alone (toggle)");
    } else {
        show_dock_win(if moved { "Win alone (pointer display)" } else { "Win alone (toggle)" });
    }
}

/// Lone Win tap (`WM_DOCK_TOGGLE`). With `config.win_opens_start` it opens / closes the Start menu
/// together with the bar (`start_with_bar`), otherwise it shows / hides the bar (`toggle`).
pub fn win_tap() {
    if config::get().win_opens_start {
        start_with_bar();
    } else {
        toggle();
    }
}

/// Second quick Win tap (`WM_DOCK_START_MENU`). With `config.win_opens_start` every tap is the same
/// toggle (`start_with_bar`), otherwise it opens the Start menu (`open_start_menu`).
pub fn win_double_tap() {
    if config::get().win_opens_start {
        start_with_bar();
    } else {
        open_start_menu();
    }
}

/// `win_opens_start`: the Start menu and the bar appear and disappear as ONE animation. A hidden bar
/// is shown and the Start menu is linked to its slide+fade (`popup::link_to_bar`: same level, same
/// frame, presented from `on_vis_tick`); with the Start menu open, the bar hides and the menu is linked
/// to the hide animation, then closes on its last frame. A tap during that hide reverses both. With
/// auto-hide off the bar stays and only the Start menu closes.
fn start_with_bar() {
    if popup::current() == Some(PopupKind::StartMenu) {
        if !VISIBLE.load(Relaxed) && popup::is_linked() {
            show_dock_win("Win (Start + bar, reversed)");
        } else if VISIBLE.load(Relaxed) && config::effective_autohide() {
            popup::link_to_bar();
            hide_dock("Win (Start + bar)");
        } else {
            popup::close("Win (Start)");
        }
        return;
    }
    toggle_start_menu("Win (Start + bar)");
}

/// Toggles the dock's Start menu on the bar, which in "pointer" mode is first brought to the display
/// under the pointer. A hidden bar is shown and an opening menu is linked to its slide+fade.
/// Only the Win key calls it: a hidden bar is shown with the foreground (`show_dock_win`).
fn toggle_start_menu(reason: &str) {
    follow_pointer();
    let was_visible = VISIBLE.load(Relaxed);
    if !was_visible {
        show_dock_win(reason);
    }
    match anchor_for(ItemId::Start).or_else(bar_start_anchor) {
        Some(a) => popup::toggle(PopupKind::StartMenu, a),
        None => {
            log!("toggle_start_menu: the bar has no geometry yet");
            return;
        }
    }
    if !was_visible && popup::current() == Some(PopupKind::StartMenu) && popup::link_to_bar() {
        let (dy, alpha) = vis_offset(config_top());
        popup::follow_bar(dy, alpha, false);
    }
}

/// Win key tapped twice (`WM_DOCK_START_MENU`): opens (or closes) the dock's own Start menu
/// (`popup_startmenu`: all apps, search, pins) on the bar, which in "pointer" mode is first brought
/// to the display under the pointer. The first tap already toggled the bar, so it is shown again if
/// that tap hid it.
pub fn open_start_menu() {
    toggle_start_menu("Win double tap (Start menu)");
}

/// Anchor at the left end of the bar, for the Start popup when the Start button is hidden.
fn bar_start_anchor() -> Option<Anchor> {
    let g = GEOM.with(|g| g.get())?;
    let dpi = UI.with(|u| {
        let st = u.borrow();
        if st.monitor_dpi >= 96 {
            st.monitor_dpi
        } else {
            st.metrics.dpi
        }
    });
    Some(Anchor {
        item: RECT { left: g.x, top: g.y, right: g.x + g.h, bottom: g.y + g.h },
        bar: RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h },
        top: config_top(),
        dpi,
    })
}

/// true when the bar follows the pointer (`config.monitor == "pointer"`).
fn follows_pointer() -> bool {
    config::get().monitor.eq_ignore_ascii_case(config::MONITOR_POINTER)
}

/// Display under the mouse pointer (the nearest one if the pointer position cannot be read).
fn pointer_hmonitor() -> HMONITOR {
    let p = cursor_pos().unwrap_or_default();
    // SAFETY: MonitorFromPoint with MONITOR_DEFAULTTONEAREST has no preconditions and always returns a monitor.
    unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) }
}

/// "pointer" mode: brings the bar to the display under the pointer (metrics, fonts, icons and
/// geometry follow that display's DPI). Returns true if the bar changed display; a bar visible on the
/// old display is hidden at once there, so the caller's `show_dock` slides it in on the new one.
fn follow_pointer() -> bool {
    if !follows_pointer() {
        return false;
    }
    let h = pointer_hmonitor();
    let prev = FOLLOW_HMON.with(|c| c.replace(h.0 as isize));
    if prev == h.0 as isize || GEOM.with(|g| g.get()).is_none() {
        return false;
    }
    // A popup or the picker anchored to the old position would be left behind.
    popup::close_unless_launcher("bar moved to the pointer display");
    picker_cancel("bar moved to the pointer display");
    if VISIBLE.load(Relaxed) {
        hide("bar moved to the pointer display", true);
    }
    apply_dpi(dock_dpi());
    true
}

/// What the 2 s housekeeping runs when `config::poll_changed` reloaded a hand edit (main.rs), and the launcher's alias
/// change after it loaded one first (`popup_launcher::on_alias_message`): the tray's trust cache, every follower of the
/// config, the update timer.
pub fn config_reloaded() {
    crate::tray_host::invalidate_trust_cache();
    on_config_changed();
    crate::update::sync_timer(hwnd());
}

/// The config changed on disk (`config::poll_changed` returned true): rebuilds the items
/// (pins/names), rereads position/auto-hide and redraws.
pub fn on_config_changed() {
    log!("ui::on_config_changed: rebuilding items and geometry");
    suite_links::refresh();
    takeover::refresh_gate();
    // Sizes (or what makes them count: display mode, tray) changed: same path as a DPI change
    // (fonts, icons at the new size, geometry).
    let (mon, cur) = UI.with(|u| {
        let st = u.borrow();
        (st.monitor_dpi, st.sizes)
    });
    // A different display (`monitor`) may also have a different DPI.
    let dpi = dock_dpi();
    if mon >= 96 && (dpi != mon || BarSizes::from_config(&config::get()) != cur) {
        apply_dpi(dpi);
    }
    // Volume / notification modules switched: create or release their threads and COM objects.
    modules_sync();
    // `show_tray` switched: start or stop the tray host (stopped = no threads, no window).
    tray_sync();
    rebuild_entries();
    // `tray_hidden` may have been edited by hand: realign the bar icons.
    refresh_tray_state();
    popup::on_tray_changed();
    // The position (top/bottom) may have changed: the geometry is always recomputed.
    redraw();
    // `reserve_space` switched, or the bar moved to another edge or display: the strip follows, no restart.
    appbar_sync("config changed", false);
    // Modules switched on/off: start/stop their polling and free their state.
    sysmon_sync();
    battery_refresh(VISIBLE.load(Relaxed));
    battery_sync();
    let battery_present = UI.with(|u| u.borrow().battery.present);
    if battery_popup_should_close(&render::visible_modules(&config::get(), battery_present), popup::current()) {
        popup::close("battery module off");
    }
    if VISIBLE.load(Relaxed) {
        // `show_seconds` / `show_clock` may have changed: re-align the clock timer.
        arm_clock(hwnd());
        if let Some(g) = GEOM.with(|g| g.get()) {
            show_topmost(hwnd(), &g);
        }
    }
    // Window previews switched off: no hover timer, picker hidden.
    if !config::get().window_previews {
        picker_cancel("window previews off");
        // Off = no timer and no window: destroy it (idempotent; `open` recreates it).
        crate::picker::shutdown();
    }
    // The bar may have moved or resized under an open popup (Settings): keep it attached to its item.
    if let Some(a) = popup::current().and_then(item_for_popup).and_then(anchor_for) {
        popup::reanchor(a);
    }
    // The launcher's chord and options.
    popup::popup_launcher::on_config();
    // The dock's own chords (keyboard section, `hide_native_taskbar`).
    chords_sync();
    // Last: the Settings window shows the values the bar now uses.
    crate::settings_window::on_config_changed();
}

/// Starts or stops the notification thread and the audio controller to match `show_notifications` /
/// `show_volume` (a disabled module holds no thread, COM object or callback).
fn modules_sync() {
    let cfg = config::get();
    if cfg.show_notifications && !crate::notif::is_started() {
        crate::notif::start(hwnd(), is_visible());
    } else if !cfg.show_notifications && crate::notif::is_started() {
        let hook = popup::suspend_mouse_hook();
        crate::notif::stop();
        popup::resume_mouse_hook(hook);
    }
    let was = AUDIO_ACTIVE.with(|a| a.replace(cfg.show_volume));
    if cfg.show_volume && !was {
        shell_thread::request(ShellRequest::VolumeRefresh);
    } else if !cfg.show_volume {
        // Idempotent on the shell side (also covers a module that was already off at startup).
        shell_thread::request(ShellRequest::AudioRelease);
    }
}

/// Starts the tray host when `show_tray` is switched on (native taskbar hidden), stops it when switched
/// off; in both cases `TaskbarCreated` is announced so apps re-register their icons with whoever now owns
/// the tray (our host, or Explorer). Blocks the UI thread for at most ~2 s, only on a real switch.
/// Never starts during the Explorer-restart pause (`taskbar::host_paused`): the resume path calls it.
pub(crate) fn tray_sync() {
    let action = tray_sync_action(
        config::get().show_tray,
        tray_host::is_started(),
        crate::taskbar::is_hidden(),
        crate::taskbar::host_paused(),
        crate::taskbar::exiting(),
    );
    match action {
        TraySync::Start => {
            // The wait below must not run under the popup's low-level mouse hook (it would stall the mouse).
            let hook = popup::suspend_mouse_hook();
            match tray_host::start(hwnd()) {
                Ok(()) => crate::taskbar::announce_tray(),
                Err(e) => log!("tray host not started: {e}"),
            }
            popup::resume_mouse_hook(hook);
        }
        TraySync::Stop => {
            let hook = popup::suspend_mouse_hook();
            tray_host::stop();
            crate::taskbar::announce_tray();
            popup::resume_mouse_hook(hook);
            log!("tray: host stopped (show_tray off)");
        }
        TraySync::Nothing => {}
    }
}

/// What `tray_sync` must do with the tray host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraySync {
    Start,
    Stop,
    Nothing,
}

/// Start only when wanted, not running, native taskbar hidden by us, Explorer not restarting
/// (a host started then would own `Shell_TrayWnd` and Explorer would never register its desktop)
/// and the dock not exiting (`taskbar::exiting`). Stopping is always allowed.
fn tray_sync_action(show_tray: bool, started: bool, hidden: bool, paused: bool, exiting: bool) -> TraySync {
    if show_tray && !started && hidden && !paused && !exiting {
        TraySync::Start
    } else if !show_tray && started {
        TraySync::Stop
    } else {
        TraySync::Nothing
    }
}

/// Rereads `tray_host::snapshot()` into the UI state.
fn refresh_tray_state() {
    let mut snap = tray_host::snapshot();
    let cfg = config::get();
    if !cfg.tray_order.is_empty() {
        // Stable sort: icons missing from `tray_order` keep their registration order, at the end.
        crate::tinysort::sort_by_key(&mut snap, |t| {
            cfg.tray_order.iter().position(|k| k.eq_ignore_ascii_case(&t.key)).unwrap_or(usize::MAX)
        });
    }
    let overflow = tray_host::any_user_hidden();
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.tray = snap;
        st.tray_overflow = overflow;
    });
}

/// `WM_DOCK_TRAY_CHANGED` (wParam = 1 bitmap, 2 layout): rereads `tray_host::snapshot()` and redraws.
/// Called by `main`. With the bar hidden, a layout change still recomputes the geometry,
/// so apps (`Shell_NotifyIconGetRect`) find the rects of the "shown" position.
pub fn on_tray_changed(kind: usize) {
    refresh_tray_state();
    if VISIBLE.load(Relaxed) || kind == 2 {
        redraw();
    }
}

/// `WM_DOCK_NOTIF`: updates the bell's badge/DND state from `notif::snapshot()`. Called by `main`.
pub fn on_notif_changed() {
    let dnd = crate::notif::snapshot().dnd;
    let count = crate::notif::count();
    let changed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let changed = st.notif_count != count || st.notif_dnd != dnd;
        st.notif_count = count;
        st.notif_dnd = dnd;
        changed
    });
    if changed {
        redraw_if_visible();
    }
}

/// `WM_DOCK_POPUP_CLOSED`: the popup closed; the auto-hide countdown restarts and the persistent
/// plate disappears (the plate goes back to following the mouse). Called by `main`.
pub fn on_popup_closed() {
    HIDE_AT.store(hide_at(now_ms(), hide_delay()), Relaxed);
    kb_after_popup();
    redraw_if_visible();
}

/// Bar item that opens popup `kind` (persistent plate); the app menu has none.
fn item_for_popup(kind: PopupKind) -> Option<ItemId> {
    match kind {
        PopupKind::Notifications => Some(ItemId::Notifications),
        PopupKind::Audio => Some(ItemId::Volume),
        PopupKind::Calendar => Some(ItemId::Clock),
        PopupKind::Power => Some(ItemId::Power),
        PopupKind::TrayOverflow => Some(ItemId::TrayOverflow),
        PopupKind::Network => Some(ItemId::Network),
        PopupKind::Battery => Some(ItemId::Battery),
        PopupKind::Start | PopupKind::StartMenu => Some(ItemId::Start),
        PopupKind::MetricsMenu => Some(ItemId::Metrics),
        PopupKind::AppMenu | PopupKind::DockMenu | PopupKind::Launcher => None,
    }
}

/// `Anchor` for opening a popup from item `id` (screen coordinates).
fn anchor_for(id: ItemId) -> Option<Anchor> {
    let g = GEOM.with(|g| g.get())?;
    let r = UI.with(|u| u.borrow().layout.rect_of(id))?;
    Some(Anchor {
        item: RECT { left: g.x + r.left, top: g.y + r.top, right: g.x + r.right, bottom: g.y + r.bottom },
        ..bar_anchor()?
    })
}

/// An anchor for the whole bar (its `item` is the bar's rectangle): the base of every popup anchor.
fn bar_anchor() -> Option<Anchor> {
    let g = GEOM.with(|g| g.get())?;
    let dpi = UI.with(|u| {
        let st = u.borrow();
        if st.monitor_dpi >= 96 {
            st.monitor_dpi
        } else {
            st.metrics.dpi
        }
    });
    let bar = RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h };
    Some(Anchor { item: bar, bar, top: config_top(), dpi })
}

/// Opens/closes popup `kind` from item `id`.
fn toggle_popup(kind: PopupKind, id: ItemId) {
    match anchor_for(id) {
        Some(a) => popup::toggle(kind, a),
        None => log!("toggle_popup({kind:?}): item {id:?} not in the layout"),
    }
}

/// Ids of the tray icons that ended up in overflow (behind the chevron) in the last drawn layout, in
/// bar order. Used by `popup_tray`. UI thread only.
pub fn tray_hidden_ids() -> Vec<u64> {
    TRAY_HIDDEN.with(|h| h.borrow().clone())
}

/// After a redraw: sends the tray host the bar, tray area and (screen) icon rects if they changed.
/// Overflow icons receive the chevron's rect.
fn sync_tray_geometry(layout: &Layout, g: &Geom, top: bool) {
    let off = |r: RECT| RECT { left: g.x + r.left, top: g.y + r.top, right: g.x + r.right, bottom: g.y + r.bottom };
    let bar = RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h };
    let area = off(layout.tray_area);
    let edge = if top { 1 } else { 3 };
    TRAY_HIDDEN.with(|h| h.borrow_mut().clone_from(&layout.tray_hidden));
    let chevron = if layout.has_chevron { layout.rect_of(ItemId::TrayOverflow).map(off) } else { None };
    let mut rects: Vec<(u64, RECT)> = Vec::new();
    for it in &layout.items {
        if let ItemId::Tray(id) = it.id {
            rects.push((id, off(it.rect)));
        }
    }
    if let Some(c) = chevron {
        for id in &layout.tray_hidden {
            rects.push((*id, c));
        }
    }
    let mut key: Vec<i32> =
        vec![bar.left, bar.top, bar.right, bar.bottom, area.left, area.top, area.right, area.bottom, edge as i32];
    for (id, r) in &rects {
        key.extend_from_slice(&[(*id & 0x7FFF_FFFF) as i32, (*id >> 31) as i32, r.left, r.top, r.right, r.bottom]);
    }
    let changed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        if st.last_tray_geo == key {
            false
        } else {
            st.last_tray_geo = key;
            true
        }
    });
    if changed {
        tray_host::set_geometry(TrayGeometry { bar, tray_area: area, edge });
        tray_host::set_icon_rects(&rects);
    }
}

/// true if the bar is visible.
pub fn is_visible() -> bool {
    VISIBLE.load(Relaxed)
}

/// Redraws the bar only if it is visible (the geometry is recomputed anyway on the next show).
fn redraw_if_visible() {
    if VISIBLE.load(Relaxed) {
        redraw();
    }
}

/// Redraws the bar (pipeline: scene -> layout -> geometry -> resize -> paint -> present).
pub fn redraw() {
    if GEOM.with(|g| g.get()).is_none() {
        return;
    }
    let clock = crate::clock::format_now();
    let cfg = config::get();
    let top = config_top();
    let (dpi, sizes) = UI.with(|u| {
        let st = u.borrow();
        (st.metrics.dpi, st.sizes)
    });
    let mi = match dock_monitor() {
        Ok(mi) => mi,
        Err(e) => {
            log!("redraw: {e}");
            return;
        }
    };
    let m = Metrics { dpi };
    let wa = mi.rcWork;
    let max_width = (wa.right - wa.left - 2 * m.px(render::EDGE_MARGIN)).max(0);
    let date = if cfg.show_date && !clock.date.is_empty() { Some(clock.date.as_str()) } else { None };
    let battery = UI.with(|u| u.borrow().battery);
    let modules = render::visible_modules(&cfg, battery.present);

    let now = now_ms();
    let attn: Vec<isize> = ATTN.with(|a| a.borrow().clone());
    let attn_on = attn_phase_on(now);
    let kb_ring = kb_ring();
    // Scene, layout, resize and paint under a single short borrow (render does not re-enter the UI).
    let mut drag_lost = false;
    let layout = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let views: Vec<AppView<'_>> = st
            .entries
            .iter()
            .map(|e| AppView {
                name: &e.name,
                icon: e.icon.as_ref(),
                indicator: if e.windows.is_empty() {
                    Indicator::None
                } else if !attn.is_empty() && e.windows.iter().any(|w| attn.contains(w)) {
                    // Blink: orange bar in the "on" phase, nothing in the "off" phase.
                    if attn_on {
                        Indicator::Attention
                    } else {
                        Indicator::None
                    }
                } else if e.active {
                    Indicator::Active
                } else {
                    Indicator::Open
                },
                dimmed: e.windows.is_empty(),
                active_t: act_value(e, now),
            })
            .collect();
        let tray_icons: Vec<TrayIcon<'_>> =
            st.tray.iter().filter_map(|t| t.bitmap.as_deref().map(|b| TrayIcon { id: t.id, icon: b })).collect();
        let mut scene = Scene {
            metrics: m,
            sizes,
            apps: &views,
            tray: &tray_icons,
            overflow_enabled: popup::is_available(),
            force_chevron: st.tray_overflow, // chevron only if some icon is hidden
            bar_top: top,
            system: SystemView {
                volume: st.volume,
                show_volume_percent: cfg.show_volume_percent,
                time: &clock.time,
                date,
                bell_badge: st.notif_count > 0,
                bell_dnd: st.notif_dnd == Some(true),
                net: st.net,
                usage: st.usage,
                battery: render::BatteryView {
                    status: battery,
                    percent: cfg.battery_percent,
                    inside: cfg.battery_percent_inside,
                },
            },
            plate: None,
            modules: &modules,
            start_button: cfg.show_start_button,
            lift: None,
            hover: st.hover.or_else(|| popup::current().and_then(item_for_popup)),
            max_width,
            focus: kb_ring,
            chips: PEEK.get().chips,
        };
        let mut layout = render::layout(&scene);
        // Width animation: the content snaps to the final layout right away, the pill
        // grows/shrinks centered. With the bar hidden (or on the first draw) there is no animation.
        let target_w = layout.width;
        let dw = if !VISIBLE.load(Relaxed) || st.shown_w <= 0 {
            st.wanim.running = false;
            target_w
        } else {
            let cur = if st.wanim.running { width_at(&st.wanim, now) } else { st.shown_w };
            if !(st.wanim.running && st.wanim.to == target_w) {
                if cur != target_w {
                    st.wanim = WidthAnim { from: cur, to: target_w, start: now, running: true };
                    set_timer(hwnd(), TIMER_WIDTH, ANIM_TICK_MS);
                } else {
                    st.wanim.running = false;
                }
            }
            if st.wanim.running {
                if now.saturating_sub(st.wanim.start) >= WIDTH_ANIM_MS {
                    st.wanim.running = false;
                    target_w
                } else {
                    width_at(&st.wanim, now)
                }
            } else {
                target_w
            }
        };
        st.shown_w = dw;
        // The window is as wide as the larger of pill and layout; the layout stays centered.
        let win_w = dw.max(target_w);
        if win_w != target_w {
            shift_layout(&mut layout, (win_w - target_w) / 2, win_w);
        }
        render::set_pill_width(if dw < win_w { dw } else { 0 });
        // Drag in progress: the zone being reordered gets its rectangles moved; if the items under the
        // drag changed (app closed, DPI change...) the drag is dropped.
        if let Some(d) = st.drag.as_mut() {
            if apply_drag(&mut layout, d, &st.entries, &st.tray, now) {
                scene.lift = Some(Lift { id: d.id, alpha: PRESS_ALPHA });
                scene.hover = Some(d.id);
            } else {
                drag_lost = true;
            }
        }
        if drag_lost {
            st.drag = None;
        }
        // Hover on an item that disappeared from the layout (or moved): the plate follows it.
        if let Some(id) = st.hover {
            if layout.rect_of(id).is_none() {
                st.hover = None;
                st.pressed = None;
            }
        }
        let target = plate_target(st.hover, st.pressed, &layout, m);
        retarget_plate(&mut st.plate, target, now);
        let (rect, alpha) = plate_now(&st.plate, now);
        if alpha > 0 {
            scene.plate = Some(Plate { rect, alpha });
        }
        if let Err(e) = render::resize(layout.width, layout.height) {
            log!("render::resize failed: {e}");
            return None;
        }
        render::paint(&scene, &layout);
        Some(layout)
    });
    if drag_lost {
        log!("drag: the items changed under the drag, cancelled");
        release_capture();
    }
    let Some(layout) = layout else { return };
    // Geometry: bar centered on the work area, width = layout.width.
    let mut g = match geom_from(&mi, top, layout.width, dpi, sizes) {
        Ok(g) => g,
        Err(e) => {
            log!("redraw: {e}");
            return;
        }
    };
    if g.w != layout.width {
        // Layout wider than the work area (degradation exhausted): the bar stays centered.
        g.x += (g.w - layout.width) / 2;
        g.w = layout.width;
    }
    GEOM.with(|c| c.set(Some(g)));
    // During the width animation the tray host is not updated on every frame: the final rect
    // arrives with the last frame (`running` = false).
    // Nor during a drag: the moved rectangles are not final (the drop redraws and syncs).
    if !UI.with(|u| u.borrow().wanim.running || u.borrow().drag.is_some()) {
        sync_tray_geometry(&layout, &g, top);
    }
    UI.with(|u| u.borrow_mut().layout = layout);
    // UpdateLayeredWindow sets position AND size: it also covers the width change.
    let (dy, alpha) = vis_offset(top);
    render::present_alpha(hwnd(), g.x, g.y + dy, alpha);
}

// ------------------------------------------------------------ show/hide animation

/// Vertical offset (px, towards the screen edge the bar sits on) and constant alpha for the
/// current animation level. Idle and visible = `(0, 255)`.
fn vis_offset(top: bool) -> (i32, u8) {
    let lvl = VIS.with(|v| vis_level(&v.get(), now_ms()));
    if lvl >= 1.0 {
        return (0, 255);
    }
    let dpi = UI.with(|u| u.borrow().metrics.dpi);
    let slide = Metrics { dpi }.px(SLIDE_PX) as f32;
    let dy = ((1.0 - lvl) * slide).round() as i32;
    (if top { -dy } else { dy }, (lvl * 255.0).round() as u8)
}

/// Starts (or reverses, from the current level) the slide+fade towards `to` (1 = shown, 0 = hidden).
fn start_vis_anim(to: f32, dur: u64) {
    let now = now_ms();
    let cur = VIS.with(|v| vis_level(&v.get(), now));
    VIS.with(|v| v.set(VisAnim { from: cur, to, start: now, dur, running: true }));
    set_timer(hwnd(), TIMER_VIS, ANIM_TICK_MS);
}

/// Stops the animation and jumps to the final level `to` (used for the instant hide).
fn stop_vis_anim(to: f32) {
    VIS.with(|v| v.set(VisAnim { from: to, to, start: 0, dur: 1, running: false }));
    kill_timer(hwnd(), TIMER_VIS);
}

/// Animation tick: re-presents the already painted DIB with the new offset/alpha (no repaint).
fn on_vis_tick(h: HWND) {
    let now = now_ms();
    let a = VIS.with(|v| v.get());
    let done = !a.running || now.saturating_sub(a.start) >= a.dur;
    if done {
        stop_vis_anim(a.to);
        if a.to <= 0.0 {
            // A popup linked to the hide (Win with the Start menu open) goes away on the same frame.
            if popup::is_linked() {
                popup::close_unless_launcher("bar hidden (with Start)");
            }
            // Deferred by `hide` while the linked popup held the foreground; before the bar hides, which
            // could hand the foreground to a third window.
            restore_prev_foreground();
            hide_window(h);
            return;
        }
    }
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    let (dy, alpha) = vis_offset(config_top());
    render::present_alpha(h, g.x, g.y + dy, alpha);
    // Same level, same tick: a linked popup moves with the bar (released once the show settles).
    popup::follow_bar(dy, alpha, done);
}

// ------------------------------------------------------------ attention (FlashWindowEx)

/// true in the visible phase of the blink; steady (true) once the blinking time is over.
fn attn_phase_on(now: u64) -> bool {
    let el = now.saturating_sub(ATTN_START.load(Relaxed));
    el >= ATTN_BLINK_MS || (el / ATTN_HALF_MS).is_multiple_of(2)
}

/// Shell hook message: `what` = HSHELL_* code, `win` = window handle.
fn on_shell_hook(h: HWND, what: usize, win: isize) {
    match what {
        HSHELL_FLASH => {
            // SAFETY: no preconditions.
            if unsafe { GetForegroundWindow() }.0 as isize == win {
                return; // the active window needs no attention
            }
            let added = ATTN.with(|a| {
                let mut a = a.borrow_mut();
                if a.contains(&win) {
                    false
                } else {
                    a.push(win);
                    true
                }
            });
            if added {
                ATTN_START.store(now_ms(), Relaxed);
                // The blink runs only while the bar is shown; `attn_sync` arms it at the next show if it is not over.
                if VISIBLE.load(Relaxed) {
                    set_timer(h, TIMER_ATTN, ATTN_HALF_MS as u32);
                }
                redraw_if_visible();
            }
        }
        HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED => {
            let (removed, empty) = ATTN.with(|a| {
                let mut a = a.borrow_mut();
                let n = a.len();
                a.retain(|w| *w != win);
                (a.len() != n, a.is_empty())
            });
            if removed {
                if empty {
                    kill_timer(h, TIMER_ATTN);
                }
                redraw_if_visible();
            }
        }
        _ => {}
    }
}

/// Whether the blink timer should run: only while the bar is shown, a window asks for attention and the blink time
/// (`ATTN_BLINK_MS` after the flash) is not over; the steady mark after that needs no timer.
fn attn_timer_wanted(visible: bool, any: bool, elapsed_ms: u64) -> bool {
    visible && any && elapsed_ms < ATTN_BLINK_MS
}

/// Drops the destroyed windows of `ATTN`; true when none is left.
fn attn_drop_dead() -> bool {
    ATTN.with(|a| {
        let mut a = a.borrow_mut();
        // SAFETY: IsWindow accepts any handle value.
        a.retain(|w| unsafe { IsWindow(Some(HWND(*w as *mut std::ffi::c_void))) }.as_bool());
        a.is_empty()
    })
}

/// Arms or kills `TIMER_ATTN` by the bar's visibility (`on_visibility`): at a show the destroyed windows are dropped
/// and the timer runs when the blink is not over; at a hide it is killed.
fn attn_sync() {
    let visible = VISIBLE.load(Relaxed);
    let empty = visible && attn_drop_dead();
    let elapsed = now_ms().saturating_sub(ATTN_START.load(Relaxed));
    if attn_timer_wanted(visible, !empty, elapsed) {
        set_timer(hwnd(), TIMER_ATTN, ATTN_HALF_MS as u32);
    } else {
        kill_timer(hwnd(), TIMER_ATTN);
    }
}

/// Blink tick: drops destroyed windows, redraws, and stops once steady (or nothing flashes).
fn on_attn_tick(h: HWND) {
    let empty = attn_drop_dead();
    redraw_if_visible();
    if empty || now_ms().saturating_sub(ATTN_START.load(Relaxed)) >= ATTN_BLINK_MS {
        kill_timer(h, TIMER_ATTN);
    }
}

/// Destroys the UI resources (`render::destroy`; `main::cleanup` removes the apps WinEventHook).
pub fn shutdown() {
    appbar::release_all("shutdown");
    kill_timer(hwnd(), TIMER_ANIM);
    kill_timer(hwnd(), TIMER_VIS);
    kill_timer(hwnd(), TIMER_ATTN);
    if RAW_ON.get() {
        raw_motion(false);
    }
    if SHELLHOOK_MSG.load(Relaxed) != 0 {
        // SAFETY: `hwnd()` is our window, registered in `attach`.
        let _ = unsafe { DeregisterShellHookWindow(hwnd()) };
    }
    render::destroy();
}

// ------------------------------------------------------------------ items

fn same_key(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Launch target from exe/AUMID: packages (AUMID with '!') via AppsFolder, the others via exe.
fn compute_launch(exe: &str, aumid: Option<&str>) -> Option<String> {
    if let Some(a) = aumid {
        if a.contains('!') {
            return Some(format!("shell:AppsFolder\\{a}"));
        }
    }
    if !exe.is_empty() {
        return Some(exe.to_string());
    }
    aumid.map(|a| format!("shell:AppsFolder\\{a}"))
}

/// Rebuilds `entries`: pins first (config order), then unpinned groups by `first_seen`.
/// Keeps name/icon/requests of already known items and sends requests for the new ones.
fn rebuild_entries() {
    let cfg = config::get();
    let groups = apps::groups();
    let moved = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let old = std::mem::take(&mut st.entries);
        let mut used = vec![false; groups.len()];
        let mut out: Vec<Entry> = Vec::with_capacity(cfg.pinned.len() + groups.len());
        let mut moved: Vec<(String, String)> = Vec::new();
        for p in &cfg.pinned {
            if out.iter().any(|e| same_key(&e.key, &p.key)) {
                continue;
            }
            let gi = groups.iter().enumerate().position(|(i, g)| !used[i] && same_key(&g.key, &p.key));
            let mut e = Entry {
                key: p.key.clone(),
                group_key: p.key.clone(),
                // The stored name is only a placeholder/fallback: the live resolver (and the `names`
                // overrides) replace it as soon as the shell thread answers.
                name: p.name.clone(),
                fixed_name: false,
                pinned: true,
                custom_icon: p.icon.clone(),
                ..Default::default()
            };
            let from_launch = apps::aumid_from_launch(&p.launch);
            if let Some(i) = gi {
                used[i] = true;
                let g = &groups[i];
                e.group_key = g.key.clone();
                e.exe_path = g.exe_path.clone();
                e.aumid = g.aumid.clone().or(from_launch);
                e.windows = g.windows.clone();
                e.active = g.active;
            } else {
                if from_launch.is_none() && !p.launch.to_ascii_lowercase().starts_with("shell:") {
                    e.exe_path = p.launch.clone();
                }
                e.aumid = from_launch;
            }
            e.launch = if p.launch.is_empty() {
                compute_launch(&e.exe_path, e.aumid.as_deref())
            } else {
                Some(p.launch.clone())
            };
            // The app runs from another versioned folder than the one the pin stores (it updated
            // itself): launch from where it is now, and store that (below, outside the borrow).
            // Only once the stored file is gone: while an old version still runs next to the
            // new one, the pin does not go back and forth.
            if gi.is_some()
                && apps::same_exe_other_version(&p.launch, &e.exe_path)
                && !std::path::Path::new(&p.launch).exists()
            {
                e.launch = Some(e.exe_path.clone());
                moved.push((p.key.clone(), e.exe_path.clone()));
            }
            out.push(e);
        }
        // Unpinned apps: `app_order` first (its order), then the rest in first-seen order (stable sort).
        let mut rest: Vec<usize> = (0..groups.len()).filter(|i| !used[*i]).collect();
        if !cfg.app_order.is_empty() {
            crate::tinysort::sort_by_key(&mut rest, |i| {
                cfg.app_order.iter().position(|k| same_key(k, &groups[*i].key)).unwrap_or(usize::MAX)
            });
        }
        for i in rest {
            let g = &groups[i];
            out.push(Entry {
                key: g.key.clone(),
                group_key: g.key.clone(),
                launch: compute_launch(&g.exe_path, g.aumid.as_deref()),
                exe_path: g.exe_path.clone(),
                aumid: g.aumid.clone(),
                windows: g.windows.clone(),
                active: g.active,
                ..Default::default()
            });
        }
        // Config reloaded live: if `names`, a pin's custom icon or fixed name change,
        // the item must be resolved again (meanwhile the old icon and name stay as placeholders).
        let names_changed = LAST_NAMES.with(|n| {
            let mut n = n.borrow_mut();
            let changed = n.as_ref().is_some_and(|prev| *prev != cfg.names);
            *n = Some(cfg.names.clone());
            changed
        });
        let now = now_ms();
        let animate = VISIBLE.load(Relaxed);
        let mut act_started = false;
        for e in &mut out {
            let target = if e.active && !e.windows.is_empty() { 1.0 } else { 0.0 };
            e.act_from = target;
            e.act_to = target;
            if let Some(o) = old.iter().find(|o| same_key(&o.key, &e.key)) {
                // Indicator level: keep the running animation, start one if the foreground changed.
                e.act_from = o.act_from;
                e.act_to = o.act_to;
                e.act_start = o.act_start;
                if o.act_to != target {
                    if animate {
                        e.act_from = act_value(o, now);
                        e.act_to = target;
                        e.act_start = now;
                        act_started = true;
                    } else {
                        e.act_from = target;
                        e.act_to = target;
                        e.act_start = 0;
                    }
                }
                // A fixed name removed from the config must not be kept: the shell thread's name replaces it.
                if !e.fixed_name && !o.fixed_name {
                    e.name = o.name.clone();
                }
                e.icon = o.icon.clone();
                let same_inputs = o.custom_icon == e.custom_icon && o.fixed_name == e.fixed_name;
                e.req_px = if same_inputs && !names_changed { o.req_px } else { 0 };
            }
        }
        // A pressed app follows its entry to the new index (or the press is dropped if it is gone), so
        // a release or a drag never hits the app that slid into the old index. A moved item also loses
        // its drag (the grab offset would be stale): the release stays a click.
        if let Some(ItemId::App(i)) = st.pressed {
            let ni = old.get(i).and_then(|o| out.iter().position(|e| same_key(&e.key, &o.key)));
            st.pressed = ni.map(ItemId::App);
            if ni != Some(i) {
                st.press_drag = false;
            }
        }
        st.entries = out;
        if act_started {
            set_timer(hwnd(), TIMER_ANIM, ANIM_TICK_MS);
        }
        moved
    });
    kb_follow_entries();
    for (key, exe) in moved {
        match config::sync_pinned_launch(&key, &exe) {
            Ok(()) => log!("pin '{key}': the app moved to '{exe}'"),
            Err(e) => log!("pin '{key}': cannot store the new launch target: {e}"),
        }
    }
    // An open window picker whose app gained or lost windows closes (no live re-layout).
    if let Some(k) = crate::picker::open_key() {
        let wins = UI.with(|u| u.borrow().entries.iter().find(|e| e.group_key == k).map(|e| e.windows.clone()));
        crate::picker::sync_windows(&k, wins.as_deref());
    }
    request_missing();
}

/// Sends `ResolveApp` for the items that have not yet received one at the current icon size.
/// If the shell thread has not started yet, retries from the 50 ms poll.
fn request_missing() {
    let ready = shell_thread::thread_id() != 0;
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let px = st.sizes.icon_px(st.metrics) as u32;
        let mut pending = false;
        for e in st.entries.iter_mut() {
            if e.req_px == px {
                continue;
            }
            if !ready {
                pending = true;
                continue;
            }
            e.req_px = px;
            shell_thread::request(ShellRequest::ResolveApp(ResolveApp {
                key: e.key.clone(),
                exe_path: e.exe_path.clone(),
                aumid: e.aumid.clone(),
                hwnd: e.windows.first().copied().unwrap_or(0),
                icon_px: px,
                custom_icon: e.custom_icon.clone(),
            }));
        }
        NEED_REQUESTS.with(|n| n.set(pending));
    });
    poll_sync();
}

/// Applies a result from the shell thread; true if something changed.
fn apply_shell_event(ev: ShellEvent) -> bool {
    // Network and Start menu data go to the popups OUTSIDE the `UI` borrow (they may relayout).
    let ev = match ev {
        ShellEvent::Network(info) => return apply_network(info),
        ShellEvent::StartApps(list) => {
            popup::on_start_apps(list);
            return false;
        }
        ShellEvent::StartIcons { icon_px, icons } => {
            popup::on_start_icons(icon_px, icons);
            return false;
        }
        ShellEvent::StartSettings => {
            popup::on_start_settings();
            return false;
        }
        // Spatial sound and per-app mixer data: only the audio popup uses them.
        ShellEvent::Spatial(state) => {
            popup::on_audio_spatial(state);
            return false;
        }
        ShellEvent::AudioSessions(list) => {
            popup::on_audio_sessions(list);
            return false;
        }
        // The launcher's replies.
        ShellEvent::LauncherConverted { gen, meant } => {
            popup::popup_launcher::on_converted(gen, meant);
            return false;
        }
        ShellEvent::LauncherFiles(open, ev) => {
            popup::popup_launcher::on_files(open, ev);
            return false;
        }
        ShellEvent::LauncherIcons { icon_px, icons } => {
            popup::popup_launcher::on_icons(icon_px, icons);
            return false;
        }
        ShellEvent::LauncherState { frecency, run, .. } => {
            popup::popup_launcher::on_state(frecency, run);
            return false;
        }
        ShellEvent::LaunchFailed { kind } => {
            popup::popup_launcher::on_launch_failed(kind);
            return false;
        }
        other => other,
    };
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let px = st.sizes.icon_px(st.metrics) as u32;
        match ev {
            ShellEvent::AppName { key, name } => {
                let Some(e) = st.entries.iter_mut().find(|e| same_key(&e.key, &key)) else { return false };
                if e.fixed_name || e.name == name {
                    return false;
                }
                e.name = name;
                true
            }
            ShellEvent::AppIcon { key, icon_px, bitmap } => {
                if icon_px != px {
                    return false; // result from a previous DPI
                }
                let Some(e) = st.entries.iter_mut().find(|e| same_key(&e.key, &key)) else { return false };
                match bitmap {
                    Some(b) => {
                        e.icon = Some(b);
                        true
                    }
                    None => false,
                }
            }
            // the audio devices are only needed by the audio popup.
            ShellEvent::AudioOutputs(list) => {
                crate::popup::on_audio_outputs(list);
                false
            }
            // Handled before the borrow.
            ShellEvent::Network(_)
            | ShellEvent::StartApps(_)
            | ShellEvent::StartIcons { .. }
            | ShellEvent::StartSettings
            | ShellEvent::Spatial(_)
            | ShellEvent::AudioSessions(_)
            | ShellEvent::LaunchFailed { .. }
            | ShellEvent::LauncherIcons { .. }
            | ShellEvent::LauncherConverted { .. }
            | ShellEvent::LauncherFiles(..)
            | ShellEvent::LauncherState { .. } => false,
        }
    })
}

/// New network status: stored for the bar glyph and forwarded to the network popup (outside the
/// `UI` borrow: the popup may relayout). true if the glyph changed. A late reply after the module
/// was switched off is ignored.
fn apply_network(info: crate::net::NetInfo) -> bool {
    if !NET_ACTIVE.with(|a| a.get()) {
        return false;
    }
    let status = info.status();
    let changed = UI.with(|u| std::mem::replace(&mut u.borrow_mut().net, status) != status);
    popup::on_network(info);
    changed
}

/// Whether a hidden bar's window list is stale enough to re-read now (a launcher open): the 5 s reconcile of a shown
/// bar, applied on demand while the timer is off.
fn refresh_due(visible: bool, last_ms: u64, now: u64) -> bool {
    !visible && now.saturating_sub(last_ms) >= u64::from(RECONCILE_MS)
}

/// Whether `TIMER_RECONCILE` runs: only while the bar is shown.
fn reconcile_wanted(visible: bool) -> bool {
    visible
}

/// Arms or kills `TIMER_RECONCILE` by the bar's visibility.
fn reconcile_sync(h: HWND) {
    if reconcile_wanted(VISIBLE.load(Relaxed)) {
        set_timer(h, TIMER_RECONCILE, RECONCILE_MS);
    } else {
        kill_timer(h, TIMER_RECONCILE);
    }
}

/// One window-list refresh through the debounce (`TIMER_APPS_DEBOUNCE`): the body of the `WM_DOCK_APPS_DIRTY` arm.
fn request_apps_refresh(h: HWND) {
    if !DEBOUNCE_ARMED.with(|d| d.replace(true)) {
        set_timer(h, TIMER_APPS_DEBOUNCE, DEBOUNCE_MS);
    }
}

/// The launcher's window rows read the bar's cache while the reconcile timer may be off: re-enumerate now when the
/// cache is older than the reconcile period and the bar is hidden (UI thread; one `EnumWindows`).
pub(crate) fn apps_fresh() {
    if refresh_due(VISIBLE.load(Relaxed), LAST_APPS_REFRESH.get(), now_ms()) {
        refresh_apps();
    }
}

/// Re-enumerates the windows; if they changed, rebuilds the items and redraws.
fn refresh_apps() {
    LAST_APPS_REFRESH.set(now_ms());
    if apps::refresh(hwnd()) {
        rebuild_entries();
        // Even with the bar hidden: the next appearance must already have the right width.
        redraw();
    }
}

/// New monitor DPI (or monitor change, or sizes change): metrics, fonts, icon requests, geometry.
fn apply_dpi(mon: u32) {
    let dpi = mon;
    let m = Metrics { dpi };
    let sizes = BarSizes::from_config(&config::get());
    let changed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.monitor_dpi = mon;
        if st.metrics == m && st.sizes == sizes {
            return false;
        }
        if sizes.icon_px(m) != st.sizes.icon_px(st.metrics) {
            for e in &mut st.entries {
                e.req_px = 0; // icons must be redone at the new size (the old one stays meanwhile)
            }
        }
        st.metrics = m;
        st.sizes = sizes;
        true
    });
    if changed {
        log!("DPI -> {dpi}, {sizes:?}");
        tray_host::set_icon_px((sizes.tray_px(m)).max(m.px(16)) as u32);
        if let Err(e) = render::set_metrics(m, sizes) {
            log!("render::set_metrics failed: {e}");
        }
        request_missing();
    }
    redraw();
    if VISIBLE.load(Relaxed) {
        if let Some(g) = GEOM.with(|g| g.get()) {
            show_topmost(hwnd(), &g);
        }
    }
}

/// `WM_SETTINGCHANGE` with "intl": the date/time formats changed.
fn on_setting_change(lp: LPARAM) {
    if lp.0 == 0 {
        return;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lParam is a pointer to a NUL-terminated wide string,
    // valid for the duration of the message (or a value ignored by `to_string` if it is not text).
    let s = unsafe { PCWSTR(lp.0 as *const u16).to_string() }.unwrap_or_default();
    if s == "intl" {
        crate::clock::invalidate_formats();
        redraw_if_visible();
    }
}

// ------------------------------------------------------------ hover plate

fn lerp_i32(a: i32, b: i32, t: f32) -> i32 {
    (a as f32 + (b - a) as f32 * t).round() as i32
}

fn rect_eq(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

/// Current rectangle and alpha of the plate at instant `now` (out-cubic).
fn plate_now(p: &PlateAnim, now: u64) -> (RECT, u8) {
    if !p.running {
        return (p.to, p.to_alpha);
    }
    let t = (now.saturating_sub(p.start) as f32 / p.dur.max(1) as f32).clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - t).powi(3);
    let rect = RECT {
        left: lerp_i32(p.from.left, p.to.left, e),
        top: lerp_i32(p.from.top, p.to.top, e),
        right: lerp_i32(p.from.right, p.to.right, e),
        bottom: lerp_i32(p.from.bottom, p.to.bottom, e),
    };
    (rect, lerp_i32(p.from_alpha as i32, p.to_alpha as i32, e).clamp(0, 255) as u8)
}

/// Where the plate should be: on the hovered item, with pressed alpha if it is also pressed; with no
/// hover, on the item of the open popup (persistent plate).
fn plate_target(hover: Option<ItemId>, pressed: Option<ItemId>, layout: &Layout, m: Metrics) -> Option<(RECT, u8)> {
    let id = hover.or_else(|| popup::current().and_then(item_for_popup))?;
    let r = render::plate_rect(layout.rect_of(id)?, m);
    Some((r, if pressed == Some(id) { PRESS_ALPHA } else { HOVER_ALPHA }))
}

/// New animation target: starts from the current position/alpha. `None` = fade
/// in place. Turns on `TIMER_ANIM` if animation is needed.
fn retarget_plate(p: &mut PlateAnim, target: Option<(RECT, u8)>, now: u64) {
    let (cur, cur_a) = plate_now(p, now);
    let (to, to_a) = target.unwrap_or((cur, 0));
    if p.to_alpha == to_a && (target.is_none() || rect_eq(&p.to, &to)) {
        return;
    }
    let (from, from_a) = if cur_a == 0 && to_a > 0 { (to, 0) } else { (cur, cur_a) };
    let dur = if to_a >= from_a { PLATE_IN_MS } else { PLATE_OUT_MS };
    *p = PlateAnim { from, to, start: now, from_alpha: from_a, to_alpha: to_a, dur, running: true };
    set_timer(hwnd(), TIMER_ANIM, ANIM_TICK_MS);
}

/// true while something driven by `TIMER_ANIM` is still moving: the hover plate, an indicator
/// width change, the items sliding in a drag or the drop settle.
fn anims_busy(st: &UiState, now: u64) -> bool {
    st.plate.running
        || st.entries.iter().any(|e| e.act_start != 0 && now.saturating_sub(e.act_start) < ACT_ANIM_MS)
        || st.drag.as_ref().is_some_and(|d| d.settle.is_some() || now < d.busy_until)
}

/// Animation timer tick: redraws; when nothing moves any more it stops the timer.
fn on_anim_tick(h: HWND) {
    let now = now_ms();
    UI.with(|u| {
        let p = &mut u.borrow_mut().plate;
        if p.running && now.saturating_sub(p.start) >= p.dur {
            p.running = false;
        }
    });
    redraw_if_visible();
    finish_drag(now_ms(), false);
    // The redraw may have re-armed an animation (`retarget_plate`): then the timer stays.
    if !UI.with(|u| anims_busy(&u.borrow(), now_ms())) {
        kill_timer(h, TIMER_ANIM);
    }
}

/// Width timer tick: redraws; at the end of the run it stops the timer.
fn on_width_tick(h: HWND) {
    redraw_if_visible();
    if !UI.with(|u| u.borrow().wanim.running) {
        kill_timer(h, TIMER_WIDTH);
    }
}

/// Resets hover/press (e.g. when the dock hides) and stops the animation.
fn reset_hover() {
    abort_drag();
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.hover = None;
        st.pressed = None;
        st.tracking_mouse = false;
        st.plate = PlateAnim::default();
    });
    kill_timer(hwnd(), TIMER_ANIM);
}

// ------------------------------------------------------------------ mouse

fn lo_i16(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as u16 as i16 as i32
}

fn hi_i16(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32
}

fn hit_at(x: i32, y: i32) -> Option<ItemId> {
    UI.with(|u| u.borrow().layout.hit_test(x, y))
}

/// Sets the hovered item and updates the plate target.
fn set_hover(hit: Option<ItemId>) {
    let changed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        if st.hover == hit {
            return false;
        }
        st.hover = hit;
        let t = plate_target(st.hover, st.pressed, &st.layout, st.metrics);
        retarget_plate(&mut st.plate, t, now_ms());
        true
    });
    if changed {
        picker_hover(hit);
    }
    // Entering a tray icon: a single hover notification to the app.
    if let (true, Some(ItemId::Tray(id))) = (changed, hit) {
        if let Some(p) = cursor_pos() {
            tray_host::hover(id, p);
        }
    }
}

// ------------------------------------------------------------------ window picker

/// App item `hit` with 2+ windows -> (index, group key, name); `None` otherwise.
fn multi_window_app(hit: Option<ItemId>) -> Option<(usize, String, String)> {
    let Some(ItemId::App(i)) = hit else { return None };
    UI.with(|u| {
        let st = u.borrow();
        st.entries.get(i).filter(|e| e.windows.len() >= 2).map(|e| (i, e.group_key.clone(), e.name.clone()))
    })
}

/// The hover changed to `hit`. On an app with 2+ windows (previews on) the picker's open delay is
/// (re)armed; back on the item of the open picker it stays open; on another item the picker closes
/// at once; off the bar it closes by itself unless the pointer reaches it (`picker::arm_leave`).
/// Anything else kills the delay: no timer runs unless the pointer rests on such an item.
fn picker_hover(hit: Option<ItemId>) {
    let target = if config::get().window_previews { multi_window_app(hit) } else { None };
    let open = crate::picker::open_key();
    if let (Some((_, k, _)), Some(o)) = (&target, &open) {
        if k == o {
            kill_timer(hwnd(), TIMER_HOVER_PICKER);
            crate::picker::cancel_leave();
            return;
        }
    }
    if open.is_some() {
        if hit.is_none() {
            crate::picker::arm_leave();
        } else {
            crate::picker::close("hover moved to another item");
        }
    }
    if target.is_some() && !popup::is_open() {
        set_timer(hwnd(), TIMER_HOVER_PICKER, PICKER_HOVER_MS);
    } else {
        kill_timer(hwnd(), TIMER_HOVER_PICKER);
    }
}

/// `TIMER_HOVER_PICKER` (one-shot): the pointer rested on an app with 2+ windows: open its picker.
fn on_hover_picker_timer(h: HWND) {
    kill_timer(h, TIMER_HOVER_PICKER);
    let (hover, busy) = UI.with(|u| {
        let st = u.borrow();
        (st.hover, st.pressed.is_some() || st.drag.is_some())
    });
    if busy || !VISIBLE.load(Relaxed) || popup::is_open() || !config::get().window_previews {
        return;
    }
    let Some((i, key, name)) = multi_window_app(hover) else { return };
    open_picker(i, &key, name);
}

/// Opens the window picker of App item `i` (group `key`, shown as `name`) at its item; false when it is not in the
/// layout.
fn open_picker(i: usize, key: &str, name: String) -> bool {
    let Some(a) = anchor_for(ItemId::App(i)) else { return false };
    let name = if name.is_empty() { crate::i18n::t(crate::i18n::k::COMMON_WINDOW).to_string() } else { name };
    crate::picker::open(a, key, &name, &apps::windows_in_order(key));
    true
}

/// Kills the picker's open delay and closes the picker (click on the bar, dock hidden, previews off).
fn picker_cancel(reason: &str) {
    kill_timer(hwnd(), TIMER_HOVER_PICKER);
    crate::picker::close(reason);
}

fn on_mouse_move(h: HWND, wp: WPARAM, lp: LPARAM) {
    kb_mouse_moved();
    let need_track = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        !std::mem::replace(&mut st.tracking_mouse, true)
    });
    if need_track {
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: h,
            dwHoverTime: 0,
        };
        // SAFETY: `t` is initialized with the correct cbSize; `h` is our window.
        if let Err(e) = unsafe { TrackMouseEvent(&mut t) } {
            log!("TrackMouseEvent failed: {e}");
            UI.with(|u| u.borrow_mut().tracking_mouse = false);
        }
    }
    let (x, y) = (lo_i16(lp), hi_i16(lp));
    // (drag in progress, a pressed draggable item moved past the threshold with the button still down)
    let (dragging, pending) = UI.with(|u| {
        let st = &*u.borrow();
        let moved = (x - st.press_x).abs() > st.metrics.px(DRAG_THRESHOLD);
        (st.drag.is_some(), st.pressed.filter(|_| st.press_drag && moved && wp.0 & 1 != 0))
    });
    if dragging {
        drag_move(x);
        return;
    }
    if let Some(id) = pending {
        if begin_drag(id) {
            drag_move(x);
            return;
        }
        UI.with(|u| u.borrow_mut().press_drag = false); // nothing to reorder: stays a normal click
    }
    set_hover(hit_at(x, y));
}

fn on_mouse_leave() {
    // The middle button takes no capture: a press whose release may land elsewhere is dropped.
    MID_PRESS.with(|m| *m.borrow_mut() = None);
    let pressed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.tracking_mouse = false;
        st.pressed.is_some() || st.drag.is_some()
    });
    if !pressed {
        set_hover(None);
    }
}

/// `WM_SETCURSOR`: hand over clickable items, otherwise default (None).
fn on_set_cursor(lp: LPARAM) -> Option<LRESULT> {
    if (lp.0 & 0xFFFF) as u32 != HTCLIENT {
        return None;
    }
    let g = GEOM.with(|g| g.get())?;
    let p = cursor_pos()?;
    hit_at(p.x - g.x, p.y - g.y)?;
    // SAFETY: loads a shared system cursor (null hInstance); no pointers to manage.
    let cur = unsafe { LoadCursorW(None, IDC_HAND) }.ok()?;
    // SAFETY: `cur` is a valid shared cursor.
    unsafe { SetCursor(Some(cur)) };
    Some(LRESULT(1))
}

fn on_lbutton_down(h: HWND, lp: LPARAM) {
    // A drop still settling is committed right away.
    finish_drag(now_ms(), true);
    let hit = hit_at(lo_i16(lp), hi_i16(lp));
    picker_cancel("click on the bar");
    // An open popup closes right away on a click on the bar outside its own item (the bar is
    // NOACTIVATE: the popup does not receive WA_INACTIVE). On its own item `toggle` handles it on release.
    if popup::is_open() {
        let own = popup::current().and_then(item_for_popup);
        if own.is_none() || hit != own {
            popup::close("click on the bar");
        }
    }
    UI.with(|u| {
        let st = &mut *u.borrow_mut();
        st.pressed = hit;
        st.press_x = lo_i16(lp);
        st.press_drag = hit.is_some_and(|id| zone_of(id).is_some());
        let t = plate_target(st.hover, st.pressed, &st.layout, st.metrics);
        retarget_plate(&mut st.plate, t, now_ms());
    });
    if hit.is_some() {
        // SAFETY: `h` is our window; the capture is needed to receive the release even outside the bar.
        unsafe { SetCapture(h) };
    }
}

fn on_lbutton_up(wp: WPARAM, lp: LPARAM) {
    if drag_active() {
        end_drag(false);
        return;
    }
    // SAFETY: no preconditions; fails harmlessly if we do not have the capture.
    let _ = unsafe { ReleaseCapture() };
    if !VISIBLE.load(Relaxed) {
        // The dock hid during the click (capture active): the release must not open anything.
        UI.with(|u| u.borrow_mut().pressed = None);
        return;
    }
    let hit = hit_at(lo_i16(lp), hi_i16(lp));
    let pressed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let pressed = st.pressed.take();
        st.hover = hit;
        let t = plate_target(st.hover, st.pressed, &st.layout, st.metrics);
        retarget_plate(&mut st.plate, t, now_ms());
        pressed
    });
    if let (Some(p), Some(h)) = (pressed, hit) {
        if p == h {
            let g = if wp.0 & MK_SHIFT != 0 { Gesture::ShiftClick } else { Gesture::Click };
            run_action(resolve(target_of(Some(h)), g), Some(h));
        }
    }
}

/// MK_SHIFT of the mouse messages' wParam.
const MK_SHIFT: usize = 0x0004;
/// MK_CONTROL of the mouse messages' wParam.
const MK_CONTROL: usize = 0x0008;

/// What a middle press stores for "close all windows of the app": the group key, only when Shift is down at the press
/// (a release with Shift down after a plain middle click closes nothing).
fn press_close_key(shift_down: bool, key: Option<String>) -> Option<String> {
    key.filter(|_| shift_down)
}

/// The group key of the App item under the pointer at a middle press with Shift down (None elsewhere): the release
/// closes a group only if Shift is still down and the item under it then still has this key.
fn on_mbutton_down(wp: WPARAM, lp: LPARAM) {
    let key = match hit_at(lo_i16(lp), hi_i16(lp)) {
        Some(ItemId::App(i)) => entry_at(i).map(|e| e.group_key),
        _ => None,
    };
    let key = press_close_key(wp.0 & MK_SHIFT != 0, key);
    MID_PRESS.with(|m| *m.borrow_mut() = key);
}

/// The target of a gesture released at client point `(x, y)` over `hit`: `None` when the point is outside the bar.
fn gesture_target(hit: Option<ItemId>, x: i32, y: i32) -> Option<Target> {
    if hit.is_none() {
        let g = GEOM.with(|g| g.get())?;
        let bar = RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h };
        if !empty_hit(bar, POINT { x: g.x + x, y: g.y + y }, hit) {
            return None;
        }
    }
    Some(target_of(hit))
}

/// Empty space: a point inside the bar's rectangle (screen px) with no item under it.
fn empty_hit(bar: RECT, p: POINT, hit: Option<ItemId>) -> bool {
    hit.is_none() && point_in(&bar, p)
}

/// The group key to close at a middle release: the key of the item at index `i` of the release-time keys, only when it
/// equals the key stored at the press. A press that stored nothing, an index out of range or another key give None.
fn close_key<'a>(press: Option<&str>, release: Option<(usize, &[&'a str])>) -> Option<&'a str> {
    let press = press?;
    let (i, keys) = release?;
    keys.get(i).copied().filter(|k| *k == press)
}

/// CloseGroup at a middle release over `hit`: WM_CLOSE to the group of the App item under the pointer, resolved now, and
/// only if it is the group the press stored (a re-layout, an app that closed or a stale index acts on nothing).
fn close_group_at_release(hit: Option<ItemId>) {
    let Some(ItemId::App(i)) = hit else { return };
    let keys: Vec<String> = UI.with(|u| u.borrow().entries.iter().map(|e| e.group_key.clone()).collect());
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let press = MID_PRESS.with(|m| m.borrow().clone());
    if let Some(k) = close_key(press.as_deref(), Some((i, &refs))) {
        apps::close_group(k);
    }
}

fn on_mbutton_up(wp: WPARAM, lp: LPARAM) {
    picker_cancel("middle click on the bar");
    let (x, y) = (lo_i16(lp), hi_i16(lp));
    let hit = hit_at(x, y);
    let g = if wp.0 & MK_SHIFT != 0 { Gesture::ShiftMiddle } else { Gesture::Middle };
    if let Some(t) = gesture_target(hit, x, y) {
        run_action(resolve(t, g), hit);
    }
    MID_PRESS.with(|m| *m.borrow_mut() = None);
}

fn on_rbutton_up(lp: LPARAM) {
    picker_cancel("right click on the bar");
    let (x, y) = (lo_i16(lp), hi_i16(lp));
    let hit = hit_at(x, y);
    if let Some(t) = gesture_target(hit, x, y) {
        run_action(resolve(t, Gesture::Right), hit);
    }
}

/// Forwards a click to the app owning tray icon `id` (first closes any open popup), at `at` (screen px; None: the
/// cursor, every mouse caller).
fn tray_click(id: u64, kind: TrayClick, at: Option<POINT>) {
    popup::close("tray click");
    let p = at.or_else(cursor_pos).unwrap_or_default();
    tray_host::click(id, kind, p);
}

/// Left click on a tray icon with manual double-click detection (same icon, within
/// `GetDoubleClickTime` and `SM_CXDOUBLECLK/SM_CYDOUBLECLK`), at `at` (screen px; None: the cursor).
fn tray_left_click(id: u64, at: Option<POINT>) {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK};
    let p = at.or_else(cursor_pos).unwrap_or_default();
    let now = now_ms();
    // SAFETY: functions without preconditions.
    let (dt, dx, dy) =
        unsafe { (GetDoubleClickTime(), GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK)) };
    let double = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let is_double = matches!(
            st.last_tray_click,
            Some((lid, lt, lp)) if lid == id
                && now.saturating_sub(lt) <= dt as u64
                && (p.x - lp.x).abs() <= dx / 2
                && (p.y - lp.y).abs() <= dy / 2
        );
        // After a double click the next one starts from scratch (like Windows).
        st.last_tray_click = if is_double { None } else { Some((id, now, p)) };
        is_double
    });
    popup::close("tray click");
    tray_host::click(id, if double { TrayClick::Double } else { TrayClick::Left }, p);
}

/// Minimum time between two wheel cycle steps (a fast wheel must not sweep through many windows).
const CYCLE_STEP_MS: u64 = 120;

/// true when a cycle step may run: `CYCLE_STEP_MS` have passed since `last`.
fn cycle_due(last: u64, now: u64) -> bool {
    now.saturating_sub(last) >= CYCLE_STEP_MS
}

/// `notches` wheel notches of volume, each `d` percent.
fn volume_request(d: i8, notches: i32) {
    shell_thread::request(ShellRequest::VolumeAdjust { delta_percent: notches * i32::from(d) });
}

fn on_mouse_wheel(wp: WPARAM, lp: LPARAM) {
    // Ctrl + wheel is reserved for magnification.
    if wp.0 & MK_CONTROL != 0 {
        return;
    }
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    // The wheel carries SCREEN coordinates (not client).
    let (x, y) = (lo_i16(lp) - g.x, hi_i16(lp) - g.y);
    let hit = hit_at(x, y);
    let Some(target) = gesture_target(hit, x, y) else { return };
    // An open popup takes the wheel: only the Volume item still acts.
    if popup::is_open() && target != Target::Volume {
        return;
    }
    if resolve(target, Gesture::WheelUp) == Action::None && resolve(target, Gesture::WheelDown) == Action::None {
        return;
    }
    let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    // High-resolution wheels/touchpads send fractions of a notch: they accumulate up to 120
    // (WHEEL_DELTA). A change of direction resets the remainder.
    let notches = WHEEL_ACC.with(|a| {
        let mut acc = a.get();
        if acc.signum() * delta.signum() < 0 {
            acc = 0;
        }
        acc += delta;
        let n = acc / 120;
        a.set(acc - n * 120);
        n
    });
    if notches == 0 {
        return;
    }
    let up = notches > 0;
    match resolve(target, if up { Gesture::WheelUp } else { Gesture::WheelDown }) {
        Action::Volume(d) => volume_request(d, notches.abs()),
        a => run_action(a, hit),
    }
}

// ------------------------------------------------------------ drag & drop reordering

/// The three independent reorder zones: items never cross from one to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Apps,
    Tray,
    Modules,
}

/// Zone of a draggable item (`None` = not draggable, e.g. the overflow chevron).
fn zone_of(id: ItemId) -> Option<Zone> {
    match id {
        ItemId::App(_) => Some(Zone::Apps),
        ItemId::Tray(_) => Some(Zone::Tray),
        // The Start button is fixed at the left end of the bar.
        ItemId::TrayOverflow | ItemId::Start => None,
        ItemId::Notifications
        | ItemId::Volume
        | ItemId::Clock
        | ItemId::ShowDesktop
        | ItemId::Power
        | ItemId::Network
        | ItemId::Battery
        | ItemId::Metrics => Some(Zone::Modules),
    }
}

/// Horizontal slide (out-cubic) of an item, in px relative to the start of the zone.
#[derive(Clone, Copy, Debug, Default)]
struct SlideAnim {
    from: i32,
    to: i32,
    start: u64,
}

fn slide_at(a: &SlideAnim, now: u64) -> i32 {
    let t = (now.saturating_sub(a.start) as f32 / SLIDE_MS as f32).clamp(0.0, 1.0);
    lerp_i32(a.from, a.to, 1.0 - (1.0 - t).powi(3))
}

/// Drop animation of the dragged item; `commit` = the order changed and is persisted at its end.
#[derive(Clone, Copy, Debug)]
struct Settle {
    anim: SlideAnim,
    commit: bool,
}

/// State of a reorder. The items of the zone being reordered are `layout.items[first..first + n]`
/// (base order); all positions are relative to `x0`, the left edge of the zone.
struct DragState {
    id: ItemId,
    zone: Zone,
    /// App zone: the pinned and the unpinned entries are reordered separately.
    pinned: bool,
    first: usize,
    from: usize,
    slot: usize,
    ids: Vec<ItemId>,
    /// Config key of every item (app key, tray key, module name), to validate and to persist.
    keys: Vec<String>,
    widths: Vec<i32>,
    anims: Vec<SlideAnim>,
    x0: i32,
    total: i32,
    /// Cursor x minus the left edge of the dragged item.
    grab: i32,
    /// Left edge of the dragged item while it follows the cursor.
    cur_rel: i32,
    /// Where the dragged item would land (left edge of `slot`).
    slot_rel: i32,
    settle: Option<Settle>,
    /// Tick until which the sliding items still move.
    busy_until: u64,
}

fn drag_active() -> bool {
    UI.with(|u| u.borrow().drag.is_some())
}

fn release_capture() {
    // SAFETY: no preconditions; fails harmlessly if we do not have the capture. Never call it while
    // `UI` is borrowed: it synchronously sends WM_CAPTURECHANGED.
    let _ = unsafe { ReleaseCapture() };
}

/// Index (in base order) of the item that sits at position `p` when the dragged item `from` is in `slot`.
fn slot_item(p: usize, from: usize, slot: usize) -> usize {
    if p == slot {
        return from;
    }
    let o = if p < slot { p } else { p - 1 };
    if o < from {
        o
    } else {
        o + 1
    }
}

/// Moves the drop slot: the other items slide to their new places, `slot_rel` is where the dragged one lands.
fn retarget_slots(d: &mut DragState, slot: usize, now: u64) {
    d.slot = slot;
    let mut x = 0;
    for p in 0..d.widths.len() {
        let k = slot_item(p, d.from, slot);
        if k == d.from {
            d.slot_rel = x;
        } else {
            let cur = slide_at(&d.anims[k], now);
            d.anims[k] = SlideAnim { from: cur, to: x, start: now };
        }
        x += d.widths[k];
    }
    d.busy_until = now + SLIDE_MS;
}

/// Left edge (relative to the zone) of the dragged item at `now`.
fn dragged_rel(d: &DragState, now: u64) -> i32 {
    match &d.settle {
        Some(s) => slide_at(&s.anim, now),
        None => d.cur_rel,
    }
}

fn key_matches(id: ItemId, key: &str, entries: &[Entry], tray: &[TrayView]) -> bool {
    match id {
        ItemId::App(i) => entries.get(i).is_some_and(|e| e.key == key),
        ItemId::Tray(t) => tray.iter().any(|v| v.id == t && v.key == key),
        other => render::module_name(other) == Some(key),
    }
}

fn key_of(st: &UiState, id: ItemId) -> Option<String> {
    match id {
        ItemId::App(i) => st.entries.get(i).map(|e| e.key.clone()),
        ItemId::Tray(t) => st.tray.iter().find(|v| v.id == t).map(|v| v.key.clone()),
        other => render::module_name(other).map(str::to_string),
    }
}

/// Moves the rectangles of the zone in a freshly computed (base) layout to the drag positions.
/// false (layout untouched) if the items are no longer the ones the drag started with.
fn apply_drag(layout: &mut Layout, d: &mut DragState, entries: &[Entry], tray: &[TrayView], now: u64) -> bool {
    let n = d.ids.len();
    // The zone is found by its first item: changes in earlier zones (an app opening while a tray icon
    // is dragged) only shift its index.
    let Some(first) = layout.items.iter().position(|i| i.id == d.ids[0]) else { return false };
    d.first = first;
    let Some(slice) = layout.items.get_mut(first..first + n) else { return false };
    for (k, it) in slice.iter().enumerate() {
        let same = it.id == d.ids[k] && it.rect.right - it.rect.left == d.widths[k];
        if !same || !key_matches(it.id, &d.keys[k], entries, tray) {
            return false;
        }
    }
    let x0 = slice[0].rect.left;
    d.x0 = x0;
    for (k, it) in slice.iter_mut().enumerate() {
        let rel = if k == d.from { dragged_rel(d, now) } else { slide_at(&d.anims[k], now) };
        it.rect.left = x0 + rel;
        it.rect.right = x0 + rel + d.widths[k];
    }
    true
}

/// A run of items of one reorder zone: what a drag (or a keyboard move) reorders.
struct ZoneRun {
    zone: Zone,
    /// App zone: the run is the pinned apps, or the running ones.
    pinned: bool,
    /// Index of the run's first item in the layout.
    first: usize,
    /// The item's place in the run.
    from: usize,
    ids: Vec<ItemId>,
    /// Config key of every item (app key, tray key, module name), to validate and to persist.
    keys: Vec<String>,
    widths: Vec<i32>,
}

/// The run of `items[idx]`'s zone around it (the app zone splits the pinned apps from the running ones, by `pinned_of`;
/// `key_of` gives each item's config key). None for an item that cannot move, a run of one, or an item without a key.
fn zone_run(
    items: &[ItemRect],
    idx: usize,
    pinned_of: impl Fn(ItemId) -> Option<bool>,
    key_of: impl Fn(ItemId) -> Option<String>,
) -> Option<ZoneRun> {
    let id = items.get(idx)?.id;
    let zone = zone_of(id)?;
    let pinned = pinned_of(id)?;
    let same = |it: &ItemRect| zone_of(it.id) == Some(zone) && pinned_of(it.id) == Some(pinned);
    let mut first = idx;
    while first > 0 && same(&items[first - 1]) {
        first -= 1;
    }
    let mut end = idx + 1;
    while end < items.len() && same(&items[end]) {
        end += 1;
    }
    let n = end - first;
    if n < 2 {
        return None;
    }
    let (mut ids, mut keys, mut widths) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    for it in &items[first..end] {
        ids.push(it.id);
        keys.push(key_of(it.id)?);
        widths.push(it.rect.right - it.rect.left);
    }
    Some(ZoneRun { zone, pinned, first, from: idx - first, ids, keys, widths })
}

/// Whether the entry of App item `id` is pinned (None: no such entry); every other item counts as not pinned.
fn pinned_of(st: &UiState, id: ItemId) -> Option<bool> {
    match id {
        ItemId::App(i) => st.entries.get(i).map(|e| e.pinned),
        _ => Some(false),
    }
}

/// Starts a reorder of `id` (the pressed item). false if its zone has nothing to reorder.
fn begin_drag(id: ItemId) -> bool {
    let ok = UI
        .with(|u| {
            let st = &mut *u.borrow_mut();
            let items = &st.layout.items;
            let idx = items.iter().position(|i| i.id == id)?;
            let run = zone_run(items, idx, |id| pinned_of(st, id), |id| key_of(st, id))?;
            let x0 = items[run.first].rect.left;
            let mut anims = Vec::with_capacity(run.widths.len());
            let mut rel = 0;
            for w in &run.widths {
                anims.push(SlideAnim { from: rel, to: rel, start: 0 });
                rel += w;
            }
            let left = items[idx].rect.left;
            let drag = DragState {
                id,
                zone: run.zone,
                pinned: run.pinned,
                first: run.first,
                from: run.from,
                slot: run.from,
                ids: run.ids,
                keys: run.keys,
                total: rel,
                slot_rel: left - x0,
                anims,
                widths: run.widths,
                x0,
                grab: st.press_x - left,
                cur_rel: left - x0,
                settle: None,
                busy_until: 0,
            };
            st.drag = Some(drag);
            // The hover plate gives way to the lifted item's own plate.
            st.hover = None;
            st.pressed = None;
            st.plate = PlateAnim::default();
            Some(())
        })
        .is_some();
    if ok {
        popup::close("drag");
    }
    ok
}

/// The cursor moved (client x): the dragged item follows it, clamped to its zone; the drop slot is
/// the number of other items whose centre is left of the dragged item's centre.
fn drag_move(x: i32) {
    let now = now_ms();
    let slid = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let Some(d) = st.drag.as_mut() else { return false };
        if d.settle.is_some() {
            return false;
        }
        d.cur_rel = (x - d.grab - d.x0).clamp(0, (d.total - d.widths[d.from]).max(0));
        let centre = d.cur_rel + d.widths[d.from] / 2;
        let (mut slot, mut pos) = (0, 0);
        for (k, w) in d.widths.iter().enumerate() {
            if k != d.from && pos + w / 2 < centre {
                slot += 1;
            }
            pos += w;
        }
        if slot == d.slot {
            return false;
        }
        retarget_slots(d, slot, now);
        true
    });
    if slid {
        set_timer(hwnd(), TIMER_ANIM, ANIM_TICK_MS);
    }
    redraw_if_visible();
}

/// Drop (`cancel == false`) or cancel (Esc, capture lost): the dragged item glides to its slot and the order is
/// persisted when the glide ends (`finish_drag`). A cancel animates everything back.
fn end_drag(cancel: bool) {
    let now = now_ms();
    let armed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let Some(d) = st.drag.as_mut() else { return false };
        if d.settle.is_some() {
            return false;
        }
        if cancel && d.slot != d.from {
            let from = d.from;
            retarget_slots(d, from, now);
        }
        let commit = !cancel && d.slot != d.from;
        d.settle = Some(Settle { anim: SlideAnim { from: d.cur_rel, to: d.slot_rel, start: now }, commit });
        d.busy_until = d.busy_until.max(now + SLIDE_MS);
        true
    });
    // After `settle` is set: the WM_CAPTURECHANGED sent by the release is ignored.
    release_capture();
    if armed {
        set_timer(hwnd(), TIMER_ANIM, ANIM_TICK_MS);
        redraw_if_visible();
    }
}

/// WM_CAPTURECHANGED: another window took the mouse capture mid-drag -> cancel.
fn on_capture_lost() {
    end_drag(true);
}

/// Drops a drag immediately (dock hidden...). A drag still lifted is discarded; a drop already
/// settling with a new order is persisted, as if its glide had ended.
fn abort_drag() {
    let Some(d) = UI.with(|u| u.borrow_mut().drag.take()) else { return };
    release_capture();
    if d.settle.is_some_and(|s| s.commit) {
        commit_drag(&d);
    }
}

/// Ends the drag once its settle animation is over (or at once if `force`): persists the new order.
fn finish_drag(now: u64, force: bool) {
    let done = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let due = st.drag.as_ref().and_then(|d| d.settle).is_some_and(|s| force || now >= s.anim.start + SLIDE_MS);
        if due {
            st.drag.take()
        } else {
            None
        }
    });
    let Some(d) = done else { return };
    if d.settle.is_some_and(|s| s.commit) {
        commit_drag(&d);
    }
    if let (Some(g), Some(p)) = (GEOM.with(|g| g.get()), cursor_pos()) {
        set_hover(hit_at(p.x - g.x, p.y - g.y));
    }
    redraw_if_visible();
}

/// `new` (the reordered keys) first, then the keys of `old` that are not in it, at most `cap`.
fn merge_order(new: &[String], old: &[String], cap: usize) -> Vec<String> {
    let mut out: Vec<String> = new.to_vec();
    out.extend(old.iter().filter(|k| !new.iter().any(|n| same_key(n, k))).cloned());
    out.truncate(cap);
    out
}

/// Rewrites the `pinned` entries named in `keys` so they appear in that order, in the same slots.
fn reorder_pinned(c: &mut config::Config, keys: &[String]) {
    let slots: Vec<usize> =
        c.pinned.iter().enumerate().filter(|(_, p)| keys.iter().any(|k| same_key(k, &p.key))).map(|(i, _)| i).collect();
    let moved: Vec<PinnedApp> =
        keys.iter().filter_map(|k| c.pinned.iter().find(|p| same_key(&p.key, k)).cloned()).collect();
    if slots.len() == moved.len() {
        for (slot, p) in slots.into_iter().zip(moved) {
            c.pinned[slot] = p;
        }
    }
}

/// Full module order after a drag of the VISIBLE modules: the hidden ones keep their slots, the
/// visible slots take `visible` in its new order.
fn reorder_visible_modules(old: &[String], visible: &[String]) -> Vec<String> {
    let mut next = visible.iter();
    render::modules_from_names(old)
        .into_iter()
        .filter_map(render::module_name)
        .map(|n| {
            if visible.iter().any(|v| v == n) {
                next.next().cloned().unwrap_or_else(|| n.to_string())
            } else {
                n.to_string()
            }
        })
        .collect()
}

/// `keys` with the key at `from` moved to `slot` (removed, then inserted); unchanged when either is out of range.
fn reordered(keys: &[String], from: usize, slot: usize) -> Vec<String> {
    let mut k = keys.to_vec();
    if from < k.len() && slot < k.len() {
        let moved = k.remove(from);
        k.insert(slot, moved);
    }
    k
}

/// Persists `keys`, the new order of a run of `zone` (the pinned or the running apps by `pinned`): the pins in their
/// slots, `app_order`, `tray_order` or the visible modules' slots (atomic config rewrite).
fn commit_order(zone: Zone, pinned: bool, keys: &[String]) -> Result<(), String> {
    match zone {
        Zone::Apps if pinned => config::modify(|c| reorder_pinned(c, keys)),
        Zone::Apps => config::modify(|c| c.app_order = merge_order(keys, &c.app_order, config::APP_ORDER_CAP)),
        Zone::Tray => config::modify(|c| c.tray_order = merge_order(keys, &c.tray_order, config::TRAY_ORDER_CAP)),
        Zone::Modules => config::modify(|c| c.modules_order = reorder_visible_modules(&c.modules_order, keys)),
    }
}

/// Persists the new order of the dragged zone (atomic config rewrite) and rebuilds the items.
fn commit_drag(d: &DragState) {
    match commit_order(d.zone, d.pinned, &reordered(&d.keys, d.from, d.slot)) {
        Ok(()) => on_config_changed(),
        Err(e) => log!("drag: saving the new order failed: {e}"),
    }
}

// ------------------------------------------------------------------ click

fn entry_at(i: usize) -> Option<Entry> {
    UI.with(|u| u.borrow().entries.get(i).cloned())
}

fn launch_target(e: &Entry) -> Option<String> {
    match &e.launch {
        Some(l) if !l.is_empty() => Some(l.clone()),
        _ => compute_launch(&e.exe_path, e.aumid.as_deref()),
    }
}

/// Draws the bar's icon of the group `key` (a `size` px square at `(x, y)`) on a popup surface, for the launcher's rows of
/// running programs that are not in the catalogue. false when the bar has no icon for it yet, or its state is borrowed.
pub(crate) fn paint_bar_icon(s: &mut crate::surface::Surface, key: &str, x: i32, y: i32, size: i32) -> bool {
    UI.with(|u| {
        let Ok(st) = u.try_borrow() else { return false };
        let icon = st
            .entries
            .iter()
            .find(|e| same_key(&e.key, key) || same_key(&e.group_key, key))
            .and_then(|e| e.icon.as_ref());
        match icon {
            Some(b) => {
                s.blit(b, x, y, size, 255);
                true
            }
            None => false,
        }
    })
}

/// The name the bar shows for the program of group `group_key` (compared ignoring ASCII case), None when the bar has no such
/// entry yet or its state is borrowed.
pub(crate) fn bar_name(group_key: &str) -> Option<String> {
    UI.with(|u| {
        let st = u.try_borrow().ok()?;
        let e = st.entries.iter().find(|e| e.group_key.eq_ignore_ascii_case(group_key))?;
        (!e.name.is_empty()).then(|| e.name.clone())
    })
}

pub(crate) fn launch_target_str(target: &str) {
    shell_thread::request(ShellRequest::Launch { target: target.to_string(), args: None });
}

/// Smart click on the App item `i`: the group's window logic, or a launch when it has no window.
fn smart_click(i: usize) {
    let Some(e) = entry_at(i) else { return };
    match apps::click_group(&e.group_key) {
        ClickOutcome::NoWindows => match launch_target(&e) {
            Some(t) => shell_thread::request(ShellRequest::Launch { target: t, args: None }),
            None => log!("click: no launch target for '{}'", e.key),
        },
        // A minimized window must not get the foreground back when the bar hides.
        ClickOutcome::Minimized(h) => {
            let _ = PREV_FG.compare_exchange(h, 0, Relaxed, Relaxed);
        }
        ClickOutcome::Activated(_) => {}
    }
}

/// A new instance of the App item `i` (a launch of its target).
fn new_instance(i: usize) {
    if let Some(e) = entry_at(i) {
        match launch_target(&e) {
            Some(t) => shell_thread::request(ShellRequest::Launch { target: t, args: None }),
            None => log!("middle click: no launch target for '{}'", e.key),
        }
    }
}

/// Opens or closes popup `kind` from item `id`. Without a popup (class not registered) the kinds that have one fall back
/// to the direct action.
fn popup_action(kind: PopupKind, id: ItemId) {
    let up = popup::is_available();
    match kind {
        PopupKind::Notifications if !up => send_win_n(),
        PopupKind::Audio if !up => shell_thread::request(ShellRequest::VolumeToggleMute),
        PopupKind::Calendar if !up => launch_target_str("ms-settings:dateandtime"),
        PopupKind::Power if !up => log!("Power: popup not available"),
        // Left click opens the dock's own Start menu (`popup_startmenu`), like the Win double tap; without a popup it
        // falls back to the Windows Start menu.
        PopupKind::StartMenu if !up => shell_thread::request(ShellRequest::OpenStart),
        PopupKind::Network if !up => launch_target_str("ms-settings:network"),
        PopupKind::Battery if !up => {
            popup::close("battery settings");
            launch_target_str("ms-settings:batterysaver");
        }
        _ => toggle_popup(kind, id),
    }
}

/// The part of the bar a gesture on `hit` lands on (`None`: the space between the items).
fn target_of(hit: Option<ItemId>) -> Target {
    match hit {
        None => Target::Empty,
        Some(ItemId::App(_)) => Target::App,
        Some(ItemId::Tray(_)) => Target::Tray,
        Some(ItemId::TrayOverflow) => Target::TrayOverflow,
        Some(ItemId::Notifications) => Target::Notifications,
        Some(ItemId::Volume) => Target::Volume,
        Some(ItemId::Clock) => Target::Clock,
        Some(ItemId::Start) => Target::Start,
        Some(ItemId::ShowDesktop) => Target::ShowDesktop,
        Some(ItemId::Power) => Target::Power,
        Some(ItemId::Network) => Target::Network,
        Some(ItemId::Metrics) => Target::Metrics,
        Some(ItemId::Battery) => Target::Battery,
    }
}

/// Runs `a` for the gesture that landed on `hit`. Every entry it needs is read through `entry_at` here, at the moment it
/// runs: no index is kept across messages.
fn run_action(a: Action, hit: Option<ItemId>) {
    run_action_at(a, hit, None);
}

/// `run_action` with the point a tray icon is clicked at (screen px; None: the cursor): the keyboard clicks a tray icon
/// at its centre.
fn run_action_at(a: Action, hit: Option<ItemId>, at: Option<POINT>) {
    match a {
        Action::None => {}
        Action::Smart => {
            if let Some(ItemId::App(i)) = hit {
                smart_click(i);
            }
        }
        Action::NewInstance => {
            if let Some(ItemId::App(i)) = hit {
                new_instance(i);
            }
        }
        Action::Menu => {
            if let Some(ItemId::App(i)) = hit {
                if let Some(e) = entry_at(i) {
                    show_app_menu(&e, ItemId::App(i));
                }
            }
        }
        Action::Popup(kind) => {
            if let Some(id) = hit {
                popup_action(kind, id);
            }
        }
        Action::ShowDesktop => {
            popup::close("show desktop");
            shell_thread::request(ShellRequest::ToggleDesktop);
        }
        Action::Mute => shell_thread::request(ShellRequest::VolumeToggleMute),
        Action::OpenTasks => {
            popup::close("task manager");
            launch_target_str("taskmgr.exe");
        }
        Action::OpenSettings(link) => launch_target_str(match link {
            SettingsLink::AppsVolume => "ms-settings:apps-volume",
            SettingsLink::Network => "ms-settings:network",
            SettingsLink::BatterySaver => "ms-settings:batterysaver",
        }),
        Action::Tray(button) => {
            if let Some(ItemId::Tray(id)) = hit {
                match button {
                    TrayButton::Left => tray_left_click(id, at),
                    TrayButton::Middle => tray_click(id, TrayClick::Middle, at),
                    TrayButton::Right => tray_click(id, TrayClick::Right, at),
                }
            }
        }
        Action::CloseGroup => close_group_at_release(hit),
        Action::DockMenu => {
            if let (Some(p), Some(a)) = (cursor_pos(), bar_anchor()) {
                // A 1 px wide rectangle at the pointer, as tall as the bar: the popup centres on the pointer.
                popup::open_dock_menu(Anchor { item: RECT { left: p.x, right: p.x + 1, ..a.bar }, ..a });
            }
        }
        Action::Cycle { skip_min, forward } => {
            if let Some(ItemId::App(i)) = hit {
                let now = now_ms();
                if cycle_due(LAST_CYCLE.get(), now) {
                    LAST_CYCLE.set(now);
                    if let Some(e) = entry_at(i) {
                        let _ = apps::cycle_group(&e.group_key, forward, skip_min);
                    }
                }
            }
        }
        Action::Volume(d) => volume_request(d, 1),
    }
}

/// Win+N (notification center) with `dwExtraInfo = INJECT_TAG`: the keyboard hook lets it through.
/// `pub` because the notifications popup also uses it ("Open notification center").
pub fn send_win_n() {
    send_win_key(VK_N);
}

/// Click on a notification card (`popup_notif`). Windows offers no way to activate another app's
/// toast, so the app that sent it is brought up instead: its most recent window if it has one (the
/// group with the same AUMID, or a bar item / exe with the same name), otherwise the shell thread
/// launches it from the AppsFolder catalog (`ShellRequest::OpenNotifApp`, Win+N as the last resort).
pub fn open_notification_app(aumid: &str, app: &str) {
    let groups = apps::groups();
    let alive = |g: &&apps::AppGroup| !g.windows.is_empty();
    let by_aumid = (!aumid.is_empty())
        .then(|| {
            groups.iter().filter(alive).find(|g| g.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(aumid)))
        })
        .flatten();
    let by_name = || {
        if app.is_empty() {
            return None;
        }
        let key = UI.with(|u| {
            u.borrow()
                .entries
                .iter()
                .find(|e| !e.windows.is_empty() && e.name.eq_ignore_ascii_case(app))
                .map(|e| e.group_key.clone())
        });
        key.and_then(|k| groups.iter().filter(alive).find(|g| g.key == k)).or_else(|| {
            groups.iter().filter(alive).find(|g| {
                Path::new(&g.exe_path).file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(app))
            })
        })
    };
    if let Some(g) = by_aumid.or_else(by_name) {
        log!("notification: activating '{}' ({} windows)", g.key, g.windows.len());
        apps::activate_window(g.windows[0]);
        return;
    }
    shell_thread::request(ShellRequest::OpenNotifApp { aumid: aumid.to_string(), name: app.to_string() });
}

/// Win + `vk` pressed and released with `dwExtraInfo = INJECT_TAG` (Win+N, Win+S, Win+R ...).
pub fn send_win_key(vk: VIRTUAL_KEY) {
    let inputs = win_key_inputs(vk);
    // SAFETY: slice of 4 initialized INPUTs; cbSize = size_of::<INPUT>() as required.
    let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if n != inputs.len() as u32 {
        log!("SendInput(Win+N): {n}/{} events accepted", inputs.len());
    }
}

/// The four events `send_win_key` sends: Win down (extended), `vk` down, `vk` up, Win up, every one tagged.
pub(crate) fn win_key_inputs(vk: VIRTUAL_KEY) -> [INPUT; 4] {
    let ev = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: hook::INJECT_TAG },
        },
    };
    [
        ev(VK_LWIN, KEYEVENTF_EXTENDEDKEY),
        ev(vk, KEYBD_EVENT_FLAGS(0)),
        ev(vk, KEYEVENTF_KEYUP),
        ev(VK_LWIN, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ]
}

// ------------------------------------------------------------ keyboard: owned chords (dock-keys K1-K3)

/// The owned chord rows for config `c` (SPEC-dock 5.2.2): none while the native taskbar is shown; Win+n, Win+Shift+n
/// and Win+Alt+n from `win_numbers`, Win+T and Win+Shift+T from `win_t`, Win+B from `win_b`, Peek from `peek` with the
/// digits (its chips are the digits).
fn dock_chords(c: &config::Config) -> hook::DockChords {
    use hook::DockChords as C;
    if !c.hide_native_taskbar {
        return C::NONE;
    }
    let k = c.keyboard;
    let mut t = C::NONE;
    if k.win_numbers {
        t = t.with(C::DIGITS).with(C::SHIFT_DIGITS).with(C::ALT_DIGITS);
    }
    if k.win_t {
        t = t.with(C::WIN_T).with(C::WIN_SHIFT_T);
    }
    if k.win_b {
        t = t.with(C::WIN_B);
    }
    if k.peek && k.win_numbers {
        t = t.with(C::PEEK);
    }
    t
}

/// Hands the hook the chord rows of the current config (none with `--keep-taskbar`, which keeps the native taskbar),
/// and the fullscreen flag Peek needs.
fn chords_sync() {
    let chords = if crate::args().keep_taskbar { hook::DockChords::NONE } else { dock_chords(&config::get()) };
    CHORDS.set(chords);
    if chords != hook::DockChords::NONE {
        // Lock and unlock are cut-offs the hook must hear of (`cut_off_of`).
        crate::launcher_sys::watch_sessions();
    }
    chords_follow_foreground();
    peek_fullscreen_sync(chords);
}

/// The session and power events after which keys may have gone by the hook unseen (`hook::cut_off`): a lock, an
/// unlock, a console or remote connect or disconnect (WM_WTSSESSION_CHANGE), a suspend or a resume (WM_POWERBROADCAST).
fn cut_off_of(msg: u32, code: usize) -> Option<hook::CutOff> {
    use windows::Win32::UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, WM_WTSSESSION_CHANGE, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT,
        WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };
    let session = [
        WTS_SESSION_LOCK,
        WTS_SESSION_UNLOCK,
        WTS_CONSOLE_CONNECT,
        WTS_CONSOLE_DISCONNECT,
        WTS_REMOTE_CONNECT,
        WTS_REMOTE_DISCONNECT,
    ];
    match msg {
        WM_WTSSESSION_CHANGE if session.iter().any(|&c| c as usize == code) => Some(hook::CutOff::Session),
        WM_POWERBROADCAST if code == PBT_APMSUSPEND as usize || code == PBT_APMRESUMEAUTOMATIC as usize => {
            Some(hook::CutOff::Power)
        }
        _ => None,
    }
}

/// What is in front, for the dock's chords (`chords_follow_foreground`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Front {
    /// Any other window, or none: the config's rows.
    Local,
    /// A remote-desktop or VM client: its own hook can take keys before the dock's, and its session gets the Win
    /// chords.
    Remote,
    /// A window above the dock: the UAC prompt shown without the secure desktop, which runs above the elevated dock, so
    /// the dock's hook gets none of the keys typed while it is in front, a Win release among them; or a window of any
    /// process the dock may not open for limited information, taken for one such.
    Above,
}

/// The window in front from its process's image (`launcher_sys::with_foreground_image`): a remote-desktop or VM client
/// by the launcher's own list (`powertoys::is_remote_client`, whose REMOTE flag this does not change); the UAC prompt
/// (`powertoys::is_elevation_prompt`) or a process the dock may not look at is above the dock; anything else is local.
fn front_of(image: crate::launcher_sys::ProcessImage<'_>) -> Front {
    use crate::launcher::powertoys::{is_elevation_prompt, is_remote_client};
    use crate::launcher_sys::ProcessImage;
    match image {
        ProcessImage::Path(path) if is_remote_client(path) => Front::Remote,
        ProcessImage::Path(path) if is_elevation_prompt(path) => Front::Above,
        ProcessImage::Refused => Front::Above,
        ProcessImage::Path(_) | ProcessImage::Unknown => Front::Local,
    }
}

/// The rows the hook owns for the window in front: none while a remote-desktop or VM client is there, so its session
/// gets Win+n, Win+T, Win+B and the rest as in 1.5.2 (the hook answers as with no row), nor while a window above the
/// dock is, whose keys the hook never sees; otherwise the config's `rows`.
fn effective_chords(rows: hook::DockChords, front: Front) -> hook::DockChords {
    match front {
        Front::Local => rows,
        Front::Remote | Front::Above => hook::DockChords::NONE,
    }
}

/// The cut-off of a change of the window in front from `was` to `now`: a window above the dock coming to the front or
/// leaving it (the keys typed while it is there never reach the dock's hook, a Win release among them), or a remote
/// client leaving it (its own hook could take keys before the dock's). A remote client coming to the front is none.
fn front_cut_off(was: Front, now: Front) -> Option<hook::CutOff> {
    if was == now {
        None
    } else if was == Front::Above || now == Front::Above {
        Some(hook::CutOff::WindowAbove)
    } else if was == Front::Remote {
        Some(hook::CutOff::RemoteClient)
    } else {
        None
    }
}

/// At each foreground change and config change: the rows the hook owns now (`effective_chords`) for the window in front
/// (`front_of`), which is looked at only while the config owns a row. A change of the window in front can be a cut-off
/// for the hook (`front_cut_off`).
fn chords_follow_foreground() {
    let rows = CHORDS.get();
    let front = if rows == hook::DockChords::NONE {
        Front::Local
    } else {
        crate::launcher_sys::with_foreground_image(front_of)
    };
    let was = FRONT.with(|f| f.replace(front));
    if was != front {
        match front {
            Front::Remote => log!("keyboard: a remote or VM client is in front, its session gets the Win chords"),
            Front::Above => log!("keyboard: a window above the dock is in front, the dock owns no Win chord"),
            Front::Local => log!("keyboard: a local window is in front, the dock owns its Win chords again"),
        }
    }
    hook::set_dock_chords(effective_chords(rows, front).with(takeover::chord_bits_now()));
    if let Some(why) = front_cut_off(was, front) {
        hook::cut_off(why);
    }
}

/// While Peek is owned, on each foreground change: whether a fullscreen window is in front (the hook then never arms
/// Peek over it).
fn peek_fullscreen_sync(chords: hook::DockChords) {
    if chords.has(hook::DockChords::PEEK) {
        hook::set_dock_fullscreen(foreground_is_fullscreen());
    }
}

/// A WM_DOCK_CHORD posted at `sent` (its kb.time) is stale at `now` (GetTickCount): more than CHORD_STALE_MS old,
/// both clocks wrapping.
fn chord_stale(sent: u32, now: u32) -> bool {
    now.wrapping_sub(sent) > hook::CHORD_STALE_MS
}

/// The `n`-th app item (0-based) among `ids`, the layout's items in bar order (pins, then running apps); None beyond
/// the count.
fn nth_app(ids: &[ItemId], n: u8) -> Option<ItemId> {
    ids.iter().filter(|i| matches!(i, ItemId::App(_))).nth(usize::from(n)).copied()
}

/// Where Win+T, Win+Shift+T and Win+B put the keyboard focus among `ids`: the first app, the last app, the first tray
/// icon (no tray: the first module); without apps the first or last item.
fn chord_focus(ids: &[ItemId], a: hook::ChordAction) -> Option<usize> {
    use bar_keys::{move_focus, zone_of as kzone, KZone, Key};
    match a {
        hook::ChordAction::FocusFirst => move_focus(ids, None, Key::Home),
        hook::ChordAction::FocusLast => move_focus(ids, None, Key::End),
        hook::ChordAction::FocusTray => ids
            .iter()
            .position(|&i| kzone(i) == KZone::Tray)
            .or_else(|| ids.iter().position(|&i| kzone(i) == KZone::Modules))
            .or_else(|| move_focus(ids, None, Key::Home)),
        _ => None,
    }
}

/// The ids of the last layout, in bar order.
fn layout_ids() -> Vec<ItemId> {
    UI.with(|u| u.borrow().layout.items.iter().map(|i| i.id).collect())
}

/// `WM_DOCK_CHORD`: an owned chord the hook swallowed (SPEC-dock 5.2.2). Acknowledged first, so the hook keeps owning
/// the chords; a stale post and a Sync act on nothing. Win+n acts like a click on the n-th app and never takes the bar's
/// foreground; Win+Alt+n, Win+T, Win+Shift+T and Win+B give the bar the keyboard (`focus_bar`).
fn on_chord(wp: usize, sent: u32) {
    use hook::ChordAction as C;
    hook::ack_chord((wp >> 16) as u16);
    let Some((a, _)) = hook::decode_chord(wp) else { return };
    if a == C::Sync {
        return;
    }
    // SAFETY: no preconditions.
    let now = unsafe { GetTickCount() };
    if chord_stale(sent, now) {
        log!("chord {a:?} dropped: posted {} ms ago", now.wrapping_sub(sent));
        return;
    }
    log!("chord {a:?}");
    let ids = layout_ids();
    match a {
        C::App(n) | C::NewInstance(n) | C::AppMenu(n) => {
            let Some(id) = nth_app(&ids, n) else {
                log!("chord {a:?}: no app in that slot");
                return;
            };
            match a {
                C::App(_) => run_action(Action::Smart, Some(id)),
                C::NewInstance(_) => run_action(Action::NewInstance, Some(id)),
                _ => {
                    focus_bar("Win+Alt+n");
                    if VISIBLE.load(Relaxed) {
                        kb_event(KbEvent::Chord);
                        kb_set_focus(Some(id));
                    }
                    popup::set_keyboard_entry(true);
                    run_action(Action::Menu, Some(id));
                    popup::set_keyboard_entry(false);
                }
            }
        }
        C::FocusFirst | C::FocusLast | C::FocusTray => {
            focus_bar(match a {
                C::FocusFirst => "Win+T",
                C::FocusLast => "Win+Shift+T",
                _ => "Win+B",
            });
            // A bar that could not show (no geometry yet) holds no keyboard state.
            if VISIBLE.load(Relaxed) {
                kb_event(KbEvent::Chord);
                kb_set_focus(chord_focus(&ids, a).and_then(|k| ids.get(k).copied()));
                redraw();
            }
        }
        C::Sync => {}
    }
}

/// Win+T, Win+Shift+T, Win+B, Win+Alt+n: the bar takes the foreground and the keyboard. Hidden: the Win key's show (it
/// takes the foreground, remembers the window in front in PREV_FG and stays the key-show time). Shown: the window in
/// front, when it is not ours, is remembered for Esc, the bar takes the foreground and stays at least the key-show time.
fn focus_bar(reason: &str) {
    if !VISIBLE.load(Relaxed) {
        show_dock_win(reason);
        return;
    }
    let fg = foreign_foreground();
    if !fg.0.is_null() {
        PREV_FG.store(fg.0 as isize, Relaxed);
    }
    log!("FOCUS ({reason})");
    force_foreground(hwnd());
    HIDE_AT.fetch_max(hide_at(now_ms(), key_show_ms(config::get().hide_delay_ms)), Relaxed);
    ensure_key_focus();
}

/// Peek's show lasts at most this long (ms): a lost Win release cannot keep the bar up.
const PEEK_HOLD_MS: u64 = 10_000;

/// Peek: the digit chips are drawn (Win held), and the bar was hidden when it began (its release hides it again).
#[derive(Clone, Copy, Debug)]
struct Peek {
    chips: bool,
    shown: bool,
}

/// The Peek release hides the bar only when Peek showed it, the pointer is not over it and the keyboard is not on it.
fn peek_release(shown_by_peek: bool, pointer_over: bool, kb_active: bool) -> bool {
    shown_by_peek && !pointer_over && !kb_active
}

/// `WM_DOCK_PEEK`: Win held alone PEEK_MS (`on`), or its release. On: the chips on the first ten apps, and a hidden bar
/// shown without the foreground for at most PEEK_HOLD_MS (never over a fullscreen window). Off: the chips go, and a bar
/// Peek showed hides unless the pointer or the keyboard is on it.
fn on_peek(on: bool) {
    if on {
        let hidden = !VISIBLE.load(Relaxed);
        if hidden && foreground_is_fullscreen() {
            return;
        }
        PEEK.set(Peek { chips: true, shown: PEEK.get().shown || hidden });
        if hidden {
            show_dock("Win held (peek)", PEEK_HOLD_MS);
        } else {
            redraw();
        }
        return;
    }
    let p = PEEK.replace(Peek { chips: false, shown: false });
    // The chips leave before the slide-out, which presents the last frame.
    redraw_if_visible();
    if VISIBLE.load(Relaxed) && peek_release(p.shown, pointer_over_bar(), kb_active()) {
        hide_dock("peek released");
    }
}

/// true when the pointer is over the bar.
fn pointer_over_bar() -> bool {
    let (Some(g), Some(p)) = (GEOM.with(|g| g.get()), cursor_pos()) else { return false };
    point_in(&RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h }, p)
}

/// The keyboard layer is in use: an item has the focus and the bar or one of our popups is in front (D 5.2.3).
fn kb_active_of(focused: bool, fg_ours: bool) -> bool {
    focused && fg_ours
}

/// The hide countdown while the keyboard is on the bar: the key-show time (never the bare hide delay, which may be 0).
fn kb_hide_at(now: u64, hide_delay_ms: u32) -> u64 {
    hide_at(now, key_show_ms(hide_delay_ms))
}

/// The keyboard state outlives the ring: a focus or armed letters with the bar or a popup in front. The idle end and the
/// idle clock go by this, so a pointer crossing the bar (ring off) cannot leave an armed layer that never ends.
fn kb_held_of(focus: bool, deliberate: bool, fg_ours: bool) -> bool {
    kb_armed(focus, deliberate) && fg_ours
}

/// The keyboard state is armed: an item has the focus or the letters were armed on purpose (the ring does not matter).
fn kb_armed(focus: bool, deliberate: bool) -> bool {
    focus || deliberate
}

/// The keyboard decision of a poll, from the raw state: the idle end and the idle clock go by it.
fn kb_poll_of(engaged: bool, autohide: bool, focus: bool, deliberate: bool, fg_ours: bool) -> KbPoll {
    kb_poll(engaged, autohide, kb_held_of(focus, deliberate, fg_ours))
}

/// `kb_poll_of` for the current state and foreground.
fn kb_poll_now(engaged: bool, autohide: bool) -> KbPoll {
    let (focus, deliberate) = KB.with(|k| {
        let k = k.borrow();
        (k.focus.is_some(), k.deliberate)
    });
    let fg_ours = kb_armed(focus, deliberate) && {
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        !fg.0.is_null() && is_own_window(fg)
    };
    kb_poll_of(engaged, autohide, focus, deliberate, fg_ours)
}

/// `kb_active_of` for the current focus (with its ring: a mouse motion hands the bar back to the pointer's rules) and
/// foreground.
fn kb_active() -> bool {
    let focused = KB.with(|k| {
        let k = k.borrow();
        k.focus.is_some() && k.ring
    });
    if !focused {
        return false;
    }
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    kb_active_of(focused, !fg.0.is_null() && is_own_window(fg))
}

/// true when the bar is the foreground window without the keyboard focus (keys would come as WM_SYSKEYDOWN).
fn needs_focus(fg_is_bar: bool, focus_is_bar: bool) -> bool {
    fg_is_bar && !focus_is_bar
}

/// The bar in front without the keyboard focus takes it, as popup.rs `mark_active` does for a popup.
fn ensure_key_focus() {
    let h = hwnd();
    // SAFETY: no preconditions; both read this thread's or the system's state.
    let (fg, focus) = unsafe { (GetForegroundWindow(), GetFocus()) };
    if needs_focus(fg == h, focus == h) {
        // SAFETY: `h` is our window and this is its thread. The result, the previous focus, is ignored.
        let _ = unsafe { SetFocus(Some(h)) };
    }
}

// ------------------------------------------------------------ keyboard: the focused bar (dock-keys K2-K6)

/// The bar's keyboard layer: the item with the keyboard focus and whether its ring shows. Empty while the bar is hidden.
struct Kb {
    focus: Option<ItemId>,
    /// The key of the focused app: its index changes when the entries are rebuilt.
    app_key: Option<String>,
    /// The ring is drawn: a key put the focus there.
    ring: bool,
    /// Where the pointer was when the ring came on (screen px): only a real motion from there hides it.
    ring_pt: Option<POINT>,
    /// Type-ahead: the word typed so far (allocated only while typing) and when its last character came (ms).
    typed: String,
    typed_at: u64,
    /// The keyboard was put on the bar on purpose (Win+T, Win+Shift+T, Win+B, Win+Alt+n or a navigation key): only
    /// then do letters start type-ahead. A lone Win tap gives the bar the keys without this.
    deliberate: bool,
    /// When the last key reached the bar (ms), for `kb_idle`.
    last_key: u64,
}

impl Kb {
    const EMPTY: Kb = Kb {
        focus: None,
        app_key: None,
        ring: false,
        ring_pt: None,
        typed: String::new(),
        typed_at: 0,
        deliberate: false,
        last_key: 0,
    };
}

/// Without a key for this long (ms) and with the pointer off the bar, the keyboard layer ends and the bar hides again.
const KB_IDLE_MS: u64 = 4000;

/// Letters are type-ahead only once the keyboard was put on the bar on purpose: after a lone Win tap, typing goes
/// nowhere and the bar's countdown runs as in 1.5.2.
fn letters_act(deliberate: bool) -> bool {
    deliberate
}

/// What changes whether the keyboard on the bar is deliberate (`Kb::deliberate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KbEvent {
    /// A lone Win tap shows the bar (`show_dock_win`).
    WinTap,
    /// Win+T, Win+Shift+T, Win+B or Win+Alt+n (`on_chord`).
    Chord,
    /// Prev, Next, Home, End or a zone key on the bar (`kb_run_key`).
    Nav,
    /// The keyboard layer ends: Esc, hide, idle (`kb_reset`).
    Reset,
}

/// `Kb::deliberate` after `ev`: only a chord or a navigation key arms the letters; a Win tap and the end of the
/// layer leave them inert.
fn kb_after(ev: KbEvent) -> bool {
    matches!(ev, KbEvent::Chord | KbEvent::Nav)
}

/// true when the keyboard layer has been left alone long enough to end: the last key came more than KB_IDLE_MS ago.
fn kb_idle(now: u64, last_key: u64) -> bool {
    now.saturating_sub(last_key) > KB_IDLE_MS
}

/// `ev` happened to the keyboard on the bar, now. Everything but a Win tap is a key the user pressed.
fn kb_event(ev: KbEvent) {
    let now = now_ms();
    KB.with(|k| {
        let k = &mut *k.borrow_mut();
        k.deliberate = kb_after(ev);
        if ev != KbEvent::WinTap {
            k.last_key = now;
        }
    });
}

/// What a poll does about the keyboard layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KbPoll {
    /// The pointer is over the bar or something of ours is open: the idle clock restarts.
    Engaged,
    /// The keyboard is on the bar and left alone: it ends when the idle time is up; `restore` gives the foreground
    /// back to the window that had it (no auto-hide: nothing hides the bar to do it).
    CheckIdle {
        restore: bool,
    },
    Nothing,
}

/// The idle decision of a poll, whatever the hide rules say (`autohide` only chooses how the end hands the foreground back).
fn kb_poll(engaged: bool, autohide: bool, active: bool) -> KbPoll {
    if engaged {
        KbPoll::Engaged
    } else if active {
        KbPoll::CheckIdle { restore: !autohide }
    } else {
        KbPoll::Nothing
    }
}

/// The idle clock after a poll: time with the pointer over the bar, a popup, the picker, a drag or a foreign menu (`engaged`)
/// is not time left alone, so the clock restarts when that ends.
fn kb_clock(now: u64, last_key: u64, engaged: bool) -> u64 {
    if engaged {
        now
    } else {
        last_key
    }
}

/// Poll while the bar is engaged (see `kb_clock`): a keyboard focus keeps its idle clock at `now`.
fn kb_hold(now: u64) {
    KB.with(|k| {
        let k = &mut *k.borrow_mut();
        if kb_armed(k.focus.is_some(), k.deliberate) {
            k.last_key = kb_clock(now, k.last_key, true);
        }
    });
}

/// A key reached the bar, now.
fn kb_touch() {
    let now = now_ms();
    KB.with(|k| k.borrow_mut().last_key = now);
}

/// Poll: ends the keyboard layer left alone (see `kb_idle`). true when it ended.
fn kb_idle_end(now: u64) -> bool {
    let idle = KB.with(|k| kb_idle(now, k.borrow().last_key));
    if idle {
        kb_reset();
        redraw_if_visible();
    }
    idle
}

/// The keyboard layer ends: no focus, no ring, no typed word (its buffer freed: a hidden bar holds nothing).
fn kb_reset() {
    KB.with(|k| {
        let k = &mut *k.borrow_mut();
        *k = Kb::EMPTY;
        k.deliberate = kb_after(KbEvent::Reset);
    });
}

/// Whether a pointer at `now` (screen px; None: unreadable) moved from `since`, where it was when the ring came on (None:
/// unknown, any motion counts). A WM_MOUSEMOVE Windows sends when the bar slides in under a resting pointer is no motion.
fn pointer_moved(since: Option<POINT>, now: Option<POINT>) -> bool {
    match (since, now) {
        (Some(a), Some(b)) => a != b,
        (None, _) => true,
        (Some(_), None) => false,
    }
}

/// WM_MOUSEMOVE on the bar: the first real motion hides the keyboard ring (the focus stays for the next key).
fn kb_mouse_moved() {
    let ended = KB.with(|k| {
        let k = &mut *k.borrow_mut();
        if k.ring && pointer_moved(k.ring_pt, cursor_pos()) {
            k.ring = false;
            return true;
        }
        false
    });
    if ended {
        redraw_if_visible();
    }
}

/// The keyboard focus survives a foreground change only to the bar or to one of our popups (a null foreground is a
/// transient: kept).
fn kb_keeps(fg_is_bar: bool, fg_is_own: bool) -> bool {
    fg_is_bar || fg_is_own
}

/// EVENT_SYSTEM_FOREGROUND: a window that is neither the bar nor ours ends the keyboard layer. The foreground is read
/// now: the event is posted, and a stale one must not take the focus Win+T just gave.
fn kb_on_foreground() {
    if KB.with(|k| k.borrow().focus.is_none()) {
        return;
    }
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if !kb_keeps(fg == hwnd(), fg.0.is_null() || is_own_window(fg)) {
        kb_reset();
        redraw_if_visible();
    }
}

/// A popup closed while the keyboard was on the bar: closed by Esc with the foreground left to nobody or to us, the bar
/// takes the foreground back (Esc in a popup returns to the bar, D 5.2.2); a popup left any other way never takes it.
fn back_to_bar(kb_active: bool, by_esc: bool, fg_null_or_ours: bool) -> bool {
    kb_active && by_esc && fg_null_or_ours
}

/// `WM_DOCK_POPUP_CLOSED`: `back_to_bar` for the closed popup (its Esc flag is consumed either way).
fn kb_after_popup() {
    let by_esc = popup::take_closed_by_esc();
    let in_use = KB.with(|k| {
        let k = k.borrow();
        k.focus.is_some() && k.ring
    });
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if back_to_bar(in_use && VISIBLE.load(Relaxed), by_esc, fg.0.is_null() || is_own_window(fg)) {
        force_foreground(hwnd());
        ensure_key_focus();
    }
}

/// true while a typed word is live: a character came less than TYPE_AHEAD_MS ago.
fn typing_live(typed: &str, typed_at: u64, now: u64) -> bool {
    !typed.is_empty() && now.saturating_sub(typed_at) <= bar_keys::TYPE_AHEAD_MS
}

/// The item drawn with the keyboard ring.
fn kb_ring() -> Option<ItemId> {
    KB.with(|k| {
        let k = k.borrow();
        k.focus.filter(|_| k.ring)
    })
}

/// The keyboard focus moves to `id` (None: no focus), with the ring on; an app is followed by its key.
fn kb_set_focus(id: Option<ItemId>) {
    let key = match id {
        Some(ItemId::App(i)) => UI.with(|u| u.borrow().entries.get(i).map(|e| e.key.clone())),
        _ => None,
    };
    let pt = cursor_pos();
    KB.with(|k| {
        let k = &mut *k.borrow_mut();
        k.focus = id;
        k.app_key = key;
        k.ring = id.is_some();
        k.ring_pt = pt;
        k.deliberate |= id.is_some();
        k.last_key = now_ms();
    });
}

/// The item with the keyboard focus.
fn kb_focus() -> Option<ItemId> {
    KB.with(|k| k.borrow().focus)
}

/// The index of `key` among `keys` (the entries' keys, compared as `same_key` does).
fn refind(keys: &[&str], key: &str) -> Option<usize> {
    keys.iter().position(|k| same_key(k, key))
}

/// After `rebuild_entries`: the focused app is found again by its key; an app that is gone takes the focus with it.
fn kb_follow_entries() {
    KB.with(|k| {
        let k = &mut *k.borrow_mut();
        let Some(key) = k.app_key.as_deref() else { return };
        let i = UI.with(|u| {
            let st = u.borrow();
            let keys: Vec<&str> = st.entries.iter().map(|e| e.key.as_str()).collect();
            refind(&keys, key)
        });
        k.focus = i.map(ItemId::App);
        if i.is_none() {
            k.app_key = None;
            k.ring = false;
        }
    });
}

/// A key message for the bar: Some(false) a key down, Some(true) a key down with Alt held (lParam bit 29, the context
/// code), None any other message. WM_SYSKEYDOWN without the bit is what Windows sends an active window that has no
/// keyboard focus, so it counts as a plain key.
fn as_key(msg: u32, lp: isize) -> Option<bool> {
    match msg {
        WM_KEYDOWN => Some(false),
        WM_SYSKEYDOWN => Some((lp >> 29) & 1 != 0),
        _ => None,
    }
}

/// true while `vk` is held, as this thread's input saw it.
fn key_held(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: no preconditions; the high bit means "down".
    let state = unsafe { GetKeyState(i32::from(vk.0)) };
    state < 0
}

/// A held key (`repeat`) moves a focus again: the bar's arrows, Home, End and Tab (`key`), or the picker's arrows
/// (`picker_arrow`). Every other key acts once, so a held Enter, Delete, Esc, menu key or Ctrl+arrow makes no burst of
/// launches, closes or config writes.
fn repeat_acts(key: Option<bar_keys::Key>, picker_arrow: bool) -> bool {
    use bar_keys::Key;
    picker_arrow || matches!(key, Some(Key::Prev | Key::Next | Key::Home | Key::End | Key::NextZone | Key::PrevZone))
}

/// A key on the bar (SPEC-dock 5.2.2), `repeat` when it is an auto-repeat: through `bar_keys::map_key`; None lets
/// DefWindowProc have it. A hidden bar has no keyboard state.
fn on_bar_key(vk: u16, repeat: bool) -> Option<LRESULT> {
    #[cfg(feature = "owner-probe")]
    let _work = crate::launcher_probe::ui_work(crate::launcher_probe::UiWork::BarKey);
    if !VISIBLE.load(Relaxed) {
        return None;
    }
    kb_touch();
    // The picker opened from the keyboard takes the keys first (arrows, Enter, Delete, Esc).
    let picker_arrow = (0x25..=0x28).contains(&vk);
    if (!repeat || repeat_acts(None, picker_arrow)) && crate::picker::key(vk) {
        return Some(LRESULT(0));
    }
    let key = bar_keys::map_key(vk, key_held(VK_SHIFT), key_held(VK_CONTROL), false, config_top())?;
    if repeat && !repeat_acts(Some(key), false) {
        return Some(LRESULT(0));
    }
    // A space inside a typed word is a character (WM_CHAR adds it), not Activate.
    let typing = KB.with(|k| {
        let k = k.borrow();
        typing_live(&k.typed, k.typed_at, now_ms())
    });
    if !(vk == VK_SPACE.0 && typing) {
        // A popup this key opens ignores its auto-repeat (Up held on an item must not walk the popup's ring).
        popup::set_opening_key(vk);
        kb_run_key(key);
        popup::set_opening_key(0);
    }
    Some(LRESULT(0))
}

/// A character message for the bar: WM_CHAR, or WM_SYSCHAR without the Alt bit (lParam bit 29; `as_key`'s rule).
fn as_char(msg: u32, lp: isize) -> bool {
    match msg {
        WM_CHAR => true,
        WM_SYSCHAR => (lp >> 29) & 1 == 0,
        _ => false,
    }
}

/// A typed character on the bar: a printable one is type-ahead (`Key::Letter`); control characters (Enter, Tab, Esc,
/// Backspace) belong to their keys, and a space starts no word (it was Activate).
fn on_bar_char(code: usize) -> Option<LRESULT> {
    if !VISIBLE.load(Relaxed) {
        return None;
    }
    let c = char::from_u32(u32::try_from(code).ok()?).filter(|c| !c.is_control())?;
    kb_touch();
    // After a lone Win tap the characters go nowhere: no type-ahead, and the countdown is not held.
    if letters_act(KB.with(|k| k.borrow().deliberate)) {
        kb_run_key(bar_keys::Key::Letter(c));
    }
    Some(LRESULT(0))
}

/// What a key does on the focused bar.
fn kb_run_key(key: bar_keys::Key) {
    use bar_keys::Key;
    match key {
        Key::Prev | Key::Next | Key::Home | Key::End | Key::NextZone | Key::PrevZone => {
            kb_event(KbEvent::Nav);
            kb_move(key);
        }
        Key::Activate | Key::Middle | Key::Context => {
            if let Some(g) = keyboard_gesture(key) {
                kb_act(g);
            }
        }
        Key::Interior => kb_interior(),
        Key::MoveLeft => kb_reorder(false),
        Key::MoveRight => kb_reorder(true),
        Key::Escape => kb_escape(),
        Key::Letter(c) => kb_type(c),
    }
}

/// What Esc on the focused bar does: with auto-hide the bar hides (and gives the foreground back, `hide`); without it
/// the foreground goes back to the window that had it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscAction {
    Hide,
    RestoreForeground,
}

fn esc_action(autohide: bool) -> EscAction {
    if autohide {
        EscAction::Hide
    } else {
        EscAction::RestoreForeground
    }
}

/// Esc on the bar (P0 item 8): a mouse drag in progress is cancelled; otherwise the keyboard layer ends and the bar
/// hides, or gives the foreground back when it never hides.
fn kb_escape() {
    if drag_active() {
        end_drag(true);
        return;
    }
    kb_reset();
    match esc_action(config::effective_autohide()) {
        EscAction::Hide => hide_dock("Esc"),
        EscAction::RestoreForeground => {
            restore_prev_foreground();
            redraw_if_visible();
        }
    }
}

/// Type-ahead: `c` joins the typed word (a pause over TYPE_AHEAD_MS starts a new one) and the focus moves to the next
/// app whose name starts with it (`bar_keys::type_ahead`).
fn kb_type(c: char) {
    let now = now_ms();
    let Some((typed, focus)) = KB.with(|k| {
        let k = &mut *k.borrow_mut();
        let live = typing_live(&k.typed, k.typed_at, now);
        if !live {
            k.typed.clear();
            if c == ' ' {
                return None;
            }
        }
        k.typed.push(c);
        k.typed_at = now;
        Some((k.typed.clone(), k.focus))
    }) else {
        return;
    };
    let target = UI.with(|u| {
        let st = u.borrow();
        let items = &st.layout.items;
        let names: Vec<(usize, &str)> = items
            .iter()
            .enumerate()
            .filter_map(|(k, it)| match it.id {
                ItemId::App(i) => st.entries.get(i).map(|e| (k, e.name.as_str())),
                _ => None,
            })
            .collect();
        let cur = focus.and_then(|f| items.iter().position(|i| i.id == f));
        bar_keys::type_ahead(&names, cur, &typed).and_then(|k| items.get(k)).map(|i| i.id)
    });
    if let Some(id) = target {
        kb_set_focus(Some(id));
        redraw();
    }
}

/// The mouse gesture a key stands for on the focused item: Enter and Space click, Shift+Enter is the middle click, the
/// menu key and Shift+F10 the right click.
fn keyboard_gesture(k: bar_keys::Key) -> Option<Gesture> {
    use bar_keys::Key;
    match k {
        Key::Activate => Some(Gesture::Click),
        Key::Middle => Some(Gesture::Middle),
        Key::Context => Some(Gesture::Right),
        _ => None,
    }
}

/// The centre of `r` (client px) on the screen, the client origin being `origin`.
fn item_centre(r: RECT, origin: POINT) -> POINT {
    POINT { x: origin.x + (r.left + r.right) / 2, y: origin.y + (r.top + r.bottom) / 2 }
}

/// Gesture `g` on the focused item, through the mouse map (`actions::resolve`): a popup it opens starts with the
/// keyboard on its first target, and a tray icon is clicked at its centre instead of the cursor.
fn kb_act(g: Gesture) {
    let Some(id) = kb_focus() else { return };
    let at = match (GEOM.with(|g| g.get()), UI.with(|u| u.borrow().layout.rect_of(id))) {
        (Some(geo), Some(r)) => Some(item_centre(r, POINT { x: geo.x, y: geo.y })),
        _ => None,
    };
    popup::set_keyboard_entry(true);
    run_action_at(resolve(target_of(Some(id)), g), Some(id), at);
    // Consumed by the popup the action opened; an action that opened none leaves no flag for the next open.
    popup::set_keyboard_entry(false);
}

/// What the interior arrow (Up on a bottom bar, Down on a top bar) opens on the focused item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interior {
    Picker,
    AppMenu,
    Popup,
    Nothing,
}

/// The interior arrow on an item of kind `kind` (an app with `windows` windows): an app with two or more windows opens
/// the window picker, any other app its menu; an item whose click opens a popup opens it; the rest nothing.
fn interior(kind: Target, windows: usize) -> Interior {
    match kind {
        Target::App if windows >= 2 => Interior::Picker,
        Target::App => Interior::AppMenu,
        t if matches!(resolve(t, Gesture::Click), Action::Popup(_)) => Interior::Popup,
        _ => Interior::Nothing,
    }
}

/// The interior arrow on the focused item: the picker with its keyboard selection on the first card (the bar keeps the
/// foreground and hands it the keys, `on_bar_key`), the app menu, or the item's popup, each with the keyboard in it.
fn kb_interior() {
    let Some(id) = kb_focus() else { return };
    let windows = match id {
        ItemId::App(i) if config::get().window_previews => entry_at(i).map_or(0, |e| e.windows.len()),
        _ => 0,
    };
    match interior(target_of(Some(id)), windows) {
        Interior::Picker => {
            let Some((i, key, name)) = multi_window_app(Some(id)) else { return };
            kill_timer(hwnd(), TIMER_HOVER_PICKER);
            if open_picker(i, &key, name) {
                crate::picker::set_keyboard(true);
            }
        }
        Interior::AppMenu => kb_act(Gesture::Right),
        Interior::Popup => kb_act(Gesture::Click),
        Interior::Nothing => {}
    }
}

/// The slot Ctrl+Left (`right` false) or Ctrl+Right moves the item at `from` of a run of `len` items to: the next
/// place that way, never past either end (no wrap).
fn reorder_slot(from: usize, len: usize, right: bool) -> Option<usize> {
    if from >= len {
        return None;
    }
    if right {
        (from + 1 < len).then_some(from + 1)
    } else {
        from.checked_sub(1)
    }
}

/// Ctrl+Left and Ctrl+Right move the focused item one place inside its zone run (the drag's run, `zone_run`) and
/// persist the order as a drop does; the focus follows the item (an app by its key, `kb_follow_entries`).
fn kb_reorder(right: bool) {
    let Some(id) = kb_focus() else { return };
    if drag_active() {
        return;
    }
    let run = UI.with(|u| {
        let st = u.borrow();
        let idx = st.layout.items.iter().position(|i| i.id == id)?;
        zone_run(&st.layout.items, idx, |id| pinned_of(&st, id), |id| key_of(&st, id))
    });
    let Some(run) = run else { return };
    let Some(slot) = reorder_slot(run.from, run.keys.len(), right) else { return };
    match commit_order(run.zone, run.pinned, &reordered(&run.keys, run.from, slot)) {
        Ok(()) => on_config_changed(),
        Err(e) => log!("keyboard move: saving the new order failed: {e}"),
    }
}

/// Arrows, Home, End and Tab move the keyboard focus over the bar's items (`bar_keys::move_focus`), ring on.
fn kb_move(key: bar_keys::Key) {
    let ids = layout_ids();
    let cur = kb_focus().and_then(|f| ids.iter().position(|&i| i == f));
    if let Some(k) = bar_keys::move_focus(&ids, cur, key) {
        kb_set_focus(ids.get(k).copied());
        redraw();
    }
}

// ------------------------------------------------------------ context menu

/// Opens the themed context menu of app item `i` (anchored to the item, above the bar or below if
/// the bar is at the top). The choice comes back as `WM_DOCK_APP_MENU_CMD` (see `on_app_menu_cmd`).
fn show_app_menu(e: &Entry, id: ItemId) {
    let Some(anchor) = anchor_for(id) else {
        log!("app menu: item {id:?} not in the layout");
        return;
    };
    let name = if e.name.is_empty() { e.key.clone() } else { e.name.clone() };
    // The id the app's jump list is stored under: the entry's, or the one its window publishes
    // (a helper window grouped under its root exe, like Steam's, has dropped it).
    let app_id = e.aumid.clone().or_else(|| e.windows.first().and_then(|h| apps::window_app_id(*h)));
    suite_links::refresh();
    popup::open_app_menu(
        anchor,
        AppMenuInfo {
            key: e.key.clone(),
            name,
            pinned: e.pinned,
            windows: e.windows.len(),
            can_launch: launch_target(e).is_some(),
            jump: crate::jumplist::load(app_id.as_deref(), &e.exe_path),
            show_in_tasks: suite_links::usable(suite_links::App::Tasks),
        },
    );
}

/// `WM_DOCK_APP_MENU_CMD`: the popup closed the menu with choice `cmd` (`ID_MENU_*`).
fn on_app_menu_cmd(cmd: u32) {
    let Some(key) = popup::take_app_menu_key() else {
        log!("app menu: command {cmd} without key");
        return;
    };
    let e = UI.with(|u| u.borrow().entries.iter().find(|e| same_key(&e.key, &key)).cloned());
    let Some(e) = e else {
        log!("app menu: item '{key}' no longer present");
        return;
    };
    run_menu_command(cmd, e);
}

fn run_menu_command(cmd: u32, e: Entry) {
    match cmd {
        ID_MENU_PIN => {
            let Some(launch) = launch_target(&e) else {
                log!("Pin: no launch target for '{}'", e.key);
                return;
            };
            // Name not arrived yet: pin without a fixed name, the shell thread resolves it.
            let name = e.name.clone();
            match config::pin(PinnedApp { key: e.key.clone(), name, launch, icon: None }) {
                Ok(()) => on_config_changed(),
                Err(err) => log!("Pin failed: {err}"),
            }
        }
        ID_MENU_UNPIN => match config::unpin(&e.key) {
            Ok(()) => on_config_changed(),
            Err(err) => log!("Unpin failed: {err}"),
        },
        ID_MENU_CLOSE_ALL => apps::close_group(&e.group_key),
        ID_MENU_NEW_WINDOW => match launch_target(&e) {
            Some(t) => shell_thread::request(ShellRequest::Launch { target: t, args: None }),
            None => log!("New window: no launch target for '{}'", e.key),
        },
        _ => {}
    }
}

// ------------------------------------------------------------------ geometry

fn config_top() -> bool {
    config::effective_position() == config::Position::Top
}

/// A display the bar can sit on (Settings -> Screen, `config.monitor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// GDI device name, e.g. "\\.\DISPLAY2" (the value stored in `config.monitor`).
    pub device: String,
    /// Short label: the display number Windows puts in the device name ("2"), or its position.
    pub label: &'static str,
    pub primary: bool,
}

/// Displays offered by the Settings window (more are ignored: the segments would not fit).
const MAX_SCREENS: usize = 6;
const SCREEN_LABELS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Every connected display with its `MONITORINFOEXW`, in enumeration order.
fn enum_monitors() -> Vec<(HMONITOR, MONITORINFOEXW)> {
    unsafe extern "system" fn add(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` is the `&mut Vec` passed below, alive for the whole (synchronous) enumeration.
        let out = unsafe { &mut *(lp.0 as *mut Vec<(HMONITOR, MONITORINFOEXW)>) };
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `mi` is a writable MONITORINFOEXW whose cbSize says so.
        if unsafe { GetMonitorInfoW(h, &mut mi.monitorInfo) }.as_bool() {
            out.push((h, mi));
        }
        BOOL(1)
    }
    let mut out: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();
    // SAFETY: the callback only touches `out` through `lp`, and only during this call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(add), LPARAM(&mut out as *mut _ as isize)) };
    out
}

fn device_name(mi: &MONITORINFOEXW) -> String {
    let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
    String::from_utf16_lossy(&mi.szDevice[..n])
}

/// Label of the display named `device` ("\\.\DISPLAY2" -> "2"); `idx` (0-based) if it has no number.
fn screen_label(device: &str, idx: usize) -> &'static str {
    let digits: String =
        device.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let n = digits.parse::<usize>().ok().filter(|n| (1..=SCREEN_LABELS.len()).contains(n)).unwrap_or(idx + 1);
    SCREEN_LABELS[(n - 1).min(SCREEN_LABELS.len() - 1)]
}

/// Connected displays: the primary first, then the others by device name. At most `MAX_SCREENS`.
pub fn screens() -> Vec<Screen> {
    let mut v: Vec<(bool, String)> = enum_monitors()
        .iter()
        .map(|(_, mi)| (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0, device_name(mi)))
        .collect();
    crate::tinysort::sort_by(&mut v, |a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    v.truncate(MAX_SCREENS);
    v.into_iter()
        .enumerate()
        .map(|(i, (primary, device))| Screen { label: screen_label(&device, i), device, primary })
        .collect()
}

/// The display the bar belongs on: `config.monitor` if it is connected, otherwise the primary one.
/// `pub(crate)`: the Settings window opens centred on it.
pub(crate) fn dock_hmonitor() -> HMONITOR {
    let want = config::get().monitor.clone();
    if want.eq_ignore_ascii_case(config::MONITOR_POINTER) {
        return followed_hmonitor();
    }
    let found = if want.is_empty() {
        None
    } else {
        enum_monitors().into_iter().find(|(_, mi)| device_name(mi).eq_ignore_ascii_case(&want)).map(|(h, _)| h)
    };
    // SAFETY: MonitorFromPoint with MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    let h = found.unwrap_or_else(|| unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) });
    // Logged only when the display actually changes (chosen, unplugged, plugged back).
    let prev = LAST_HMON.with(|c| c.replace(h.0 as isize));
    if prev != h.0 as isize {
        if found.is_none() && !want.is_empty() {
            log!("monitor: \"{want}\" not connected, the bar uses the primary monitor");
        } else {
            log!("monitor: the bar uses {}", if want.is_empty() { "the primary monitor" } else { want.as_str() });
        }
    }
    h
}

/// "pointer" mode: the display the Win key last brought the bar to, as long as it is connected;
/// otherwise (first use, display unplugged) the display under the pointer, which becomes the new one.
fn followed_hmonitor() -> HMONITOR {
    let cur = FOLLOW_HMON.with(|c| c.get());
    let mons = enum_monitors();
    let h = match mons.iter().find(|(m, _)| cur != 0 && m.0 as isize == cur) {
        Some((m, _)) => *m,
        None => {
            let m = pointer_hmonitor();
            FOLLOW_HMON.with(|c| c.set(m.0 as isize));
            m
        }
    };
    // Logged only when the display actually changes.
    let prev = LAST_HMON.with(|c| c.replace(h.0 as isize));
    if prev != h.0 as isize {
        let name = mons.iter().find(|(m, _)| *m == h).map(|(_, mi)| device_name(mi)).unwrap_or_default();
        log!("monitor: the bar follows the pointer, now on {name}");
    }
    h
}

/// Effective DPI of the bar's display (96 if the call fails).
fn dock_dpi() -> u32 {
    let mon = dock_hmonitor();
    let (mut dx, mut dy) = (96u32, 96u32);
    // SAFETY: `dx`/`dy` are valid, writable u32s for the duration of the call.
    match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
        Ok(()) if dx >= 96 => dx,
        _ => 96,
    }
}

fn dock_monitor() -> Result<MONITORINFO, String> {
    monitor_info(dock_hmonitor()).ok_or_else(|| "GetMonitorInfoW failed".into())
}

/// Monitor and work area rects of display `h` (`None` if the call fails).
pub(crate) fn monitor_info(h: HMONITOR) -> Option<MONITORINFO> {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` has cbSize initialized and is writable.
    unsafe { GetMonitorInfoW(h, &mut mi) }.as_bool().then_some(mi)
}

/// Geometry: bar of `bar_w` physical px, horizontally centered on the work area of the bar's
/// display, `render::bar_height` tall, `EDGE_MARGIN` logical from the edge (bottom or top).
fn compute_geom(top: bool, bar_w: i32, dpi: u32, sizes: BarSizes) -> Result<Geom, String> {
    geom_from(&dock_monitor()?, top, bar_w, dpi, sizes)
}

fn geom_from(mi: &MONITORINFO, top: bool, bar_w: i32, dpi: u32, sizes: BarSizes) -> Result<Geom, String> {
    let m = Metrics { dpi };
    // Without the strip the dock itself reserves (`appbar`), or it would climb by it at each reservation.
    let wa = appbar::unreserved_work(mi.rcWork, mi.rcMonitor);
    let h = render::bar_height(m, sizes);
    let margin = m.px(render::EDGE_MARGIN);
    let w = bar_w.min(wa.right - wa.left - 2 * margin);
    if w < 100 {
        return Err(format!("work area too narrow ({w} px)"));
    }
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = if top { wa.top + margin } else { wa.bottom - margin - h };
    Ok(Geom { x, y, w, h, mon: mi.rcMonitor })
}

// ------------------------------------------------------- small unsafe wrappers

fn cursor_pos() -> Option<POINT> {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid, writable POINT for the duration of the call.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(p)
}

fn set_timer(h: HWND, id: usize, ms: u32) {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    // SAFETY: `h` is our window; without a callback the WM_TIMERs arrive at the wnd_proc.
    if unsafe { SetTimer(Some(h), id, ms, None) } == 0 {
        log!("SetTimer {id} failed");
    }
}

/// Shows the window at the top of the topmost band, at the dock's position.
fn show_topmost(h: HWND, g: &Geom) {
    // SAFETY: `h` is our window (alive until cleanup); no pointers passed.
    if let Err(e) = unsafe { SetWindowPos(h, Some(HWND_TOPMOST), g.x, g.y, g.w, g.h, SWP_SHOWWINDOW | SWP_NOACTIVATE) }
    {
        log!("SetWindowPos(HWND_TOPMOST, show) failed: {e}");
    }
}

/// Re-asserts only the topmost Z-order, without moving or resizing.
fn reassert_topmost(h: HWND) {
    // SAFETY: as above.
    if let Err(e) = unsafe { SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE) }
    {
        log!("SetWindowPos(HWND_TOPMOST) failed: {e}");
    }
}

fn kill_timer(h: HWND, id: usize) {
    // SAFETY: our id/window; fails harmlessly if the timer does not exist.
    let _ = unsafe { KillTimer(Some(h), id) };
}

fn hide_window(h: HWND) {
    // SAFETY: `h` is our window; ShowWindow uses no pointers.
    let _ = unsafe { ShowWindow(h, SW_HIDE) };
}

// ------------------------------------------------------- show / hide

/// Shows the dock above everything. Topmost is RE-ASSERTED on every show with
/// `SetWindowPos(HWND_TOPMOST, SWP_SHOWWINDOW | SWP_NOACTIVATE)`, which places the window at the top
/// of the topmost band even if the shell had lowered it. If a fullscreen / borderless window is in
/// the foreground (or with `--foreground`) it also tries to bring itself to the foreground
/// (`force_foreground`) and remembers the app to give the foreground back when it hides.
pub fn show_dock(reason: &str, hide_after_ms: u64) {
    show_dock_as(reason, hide_after_ms, false, false);
}

/// `show_dock` for the Win key: like the Windows taskbar and Start it always takes the foreground
/// (`show_takes_foreground`), over any window, fullscreen or not.
fn show_dock_win(reason: &str) {
    kb_event(KbEvent::WinTap);
    show_dock_as(reason, key_show_ms(config::get().hide_delay_ms), true, false);
    // The keys of "Win, Right, Enter" reach the bar as WM_KEYDOWN (dock-keys K2).
    ensure_key_focus();
}

/// true if a show takes the foreground: by the Win key or over a recognised fullscreen app, unless
/// `--no-foreground`; always with `--foreground`. The screen edge, the startup show (whatever window is in
/// front) and other shows do not.
fn show_takes_foreground(
    by_win: bool,
    startup: bool,
    fullscreen: bool,
    foreground_flag: bool,
    no_foreground: bool,
) -> bool {
    foreground_flag || (!startup && (by_win || fullscreen) && !no_foreground)
}

/// Window (as isize, 0 = none) to remember on a fresh show: the fullscreen app, else the previous
/// foreground window (not ours) if the show takes the foreground, else the one still remembered
/// (a linked Start hide reversed before its deferred restore).
fn prev_to_remember(fullscreen: isize, other_fg: isize, takes: bool, kept: isize) -> isize {
    if fullscreen != 0 {
        fullscreen
    } else if takes && other_fg != 0 {
        other_fg
    } else {
        kept
    }
}

/// `startup`: the show `attach` makes when the dock starts. Nobody asked for it, so it covers no fullscreen
/// app and does not take the foreground (`show_takes_foreground`), unless `--foreground`.
fn show_dock_as(reason: &str, hide_after_ms: u64, by_win: bool, startup: bool) {
    if GEOM.with(|g| g.get()).is_none() {
        return;
    }
    let was_visible = VISIBLE.load(Relaxed);
    if !was_visible {
        // Before the redraw, so the first present is already at alpha 0 / slid out.
        start_vis_anim(1.0, SHOW_ANIM_MS);
        // A dead owner's icon goes before the first frame, so the slide-in does not shrink mid-animation.
        if tray_host::sweep_dead_now() {
            refresh_tray_state();
        }
    }
    if !was_visible {
        // One sample just before the first frame, so the battery module is already in it.
        battery_refresh(true);
    }
    // List from the cache: no EnumWindows in the hook path.
    redraw();
    // The geometry is reread AFTER the redraw: the width may have changed.
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    // The edge must not re-trigger right after a hide: it re-arms when the mouse leaves it.
    EDGE_SINCE.store(0, Relaxed);
    EDGE_ARMED.store(false, Relaxed);
    // The foreground must be read BEFORE showing/activating the dock (afterwards it would be the dock itself).
    // The startup show reads no fullscreen app to cover: FS_SHOWN stays false (no retake, no instant hide).
    let fs = if was_visible || startup { None } else { fullscreen_window(&g.mon) };
    let other_fg = if was_visible { HWND::default() } else { foreign_foreground() };
    let (dy, _) = vis_offset(config_top());
    show_topmost(hwnd(), &Geom { y: g.y + dy, ..g });
    HIDE_AT.store(now_ms() + hide_after_ms, Relaxed);
    VISIBLE.store(true, Relaxed);
    if !was_visible {
        // The clock and sysmon timers are off while hidden: redraw happened above, re-arm them now.
        arm_clock(hwnd());
        sysmon_sync();
        battery_sync();
        on_visibility(true);
    }
    if hide_after_ms >= u64::MAX / 4 {
        log!("SHOW ({reason}), autohide off");
    } else {
        log!("SHOW ({reason}), hiding in {hide_after_ms} ms");
    }
    let args = crate::args();
    let fullscreen = FS_SHOWN.load(Relaxed) || fs.is_some();
    let takes = show_takes_foreground(by_win, startup, fullscreen, args.foreground, args.no_foreground);
    if !was_visible {
        FS_SHOWN.store(fs.is_some(), Relaxed);
        let fs_raw = fs.map(|h| h.0 as isize).unwrap_or(0);
        PREV_FG.store(prev_to_remember(fs_raw, other_fg.0 as isize, takes, PREV_FG.load(Relaxed)), Relaxed);
        FS_RETAKES.store(0, Relaxed);
        CLICK_DOWN.store(mouse_buttons_down(), Relaxed);
        if let Some(h) = fs {
            log!("  fullscreen foreground window: class \"{}\"", window_class(h));
        } else if takes && !other_fg.0.is_null() {
            log!("  previous foreground window: class \"{}\"", window_class(other_fg));
        }
    }
    if !was_visible {
        // After `FS_SHOWN` is known: over a fullscreen app nothing is reserved.
        appbar_sync("shown", false);
    }
    if takes {
        // An open popup (linked Start menu, Win+N) keeps the focus: it closes when it loses it.
        force_foreground(popup::current_hwnd().unwrap_or_else(hwnd));
    }
}

/// Foreground window if it is not ours (null otherwise).
fn foreign_foreground() -> HWND {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || is_own_window(fg) {
        HWND::default()
    } else {
        fg
    }
}

/// Hides the dock. If the dock took the foreground (Win key, fullscreen app) and it is still ours
/// or nobody's (timeout, Win key), it gives it back to the window that had it before.
pub fn hide_dock(reason: &str) {
    hide(reason, false);
}

/// `hide_dock` without the slide+fade out (`now`), or with it unless over a fullscreen app.
fn hide(reason: &str, now: bool) {
    // Hiding over a fullscreen app is instant; otherwise slide+fade out.
    let was_visible = VISIBLE.load(Relaxed);
    let fs = FS_SHOWN.swap(false, Relaxed);
    let instant = now || fs;
    // A popup linked to the bar (`start_with_bar`) fades out with it and closes in `on_vis_tick`,
    // which then gives the foreground back: taken now, the popup would close on the focus loss.
    let linked = !instant && crate::popup::is_linked();
    let prev = if linked { 0 } else { PREV_FG.swap(0, Relaxed) };
    if !linked {
        crate::popup::close_unless_launcher("dock hidden");
    }
    picker_cancel("dock hidden");
    if instant {
        stop_vis_anim(0.0);
        hide_window(hwnd());
    }
    VISIBLE.store(false, Relaxed);
    if was_visible {
        on_visibility(false);
    }
    // The strip goes back with the bar (the windows are given their space again at the start of the fade).
    appbar_sync("hidden", false);
    reset_hover();
    kill_timer(hwnd(), TIMER_CLOCK);
    sysmon_sync();
    battery_sync();
    let was_animating = UI.with(|u| std::mem::replace(&mut u.borrow_mut().wanim.running, false));
    kill_timer(hwnd(), TIMER_WIDTH);
    if was_animating {
        // With the bar hidden the redraw skips the animation: the tray host receives the final geometry.
        redraw();
    }
    if !instant {
        start_vis_anim(0.0, HIDE_ANIM_MS);
    }
    log!("HIDE ({reason})");
    if prev != 0 {
        restore_foreground(HWND(prev as *mut std::ffi::c_void));
    }
}

/// The one place that follows the bar's real visibility changes (once per transition: a hide of a hidden bar
/// calls nothing). Called after `VISIBLE` is stored, before any foreground call.
fn on_visibility(visible: bool) {
    if !visible {
        MID_PRESS.with(|m| *m.borrow_mut() = None);
        // A bar shown again later was not shown by this Peek: its release must not hide it.
        PEEK.set(Peek { shown: false, ..PEEK.get() });
        // A hidden bar has no keyboard state.
        kb_reset();
    }
    crate::notif::set_bar_visible(visible);
    crate::tray_host::set_bar_visible(visible);
    crate::hook::note_activity();
    if visible {
        GATE_OK.set(gate_after(GATE_OK.get(), GateEvent::Shown));
    }
    poll_sync();
    // The service timers follow the bar: the reconcile and the attention blink run only while it is shown; one
    // refresh 100 ms after a show catches what the missed window events left.
    reconcile_sync(hwnd());
    attn_sync();
    if visible {
        request_apps_refresh(hwnd());
    }
    crate::on_bar_visibility(visible);
}

/// The 3 s safety tick (`main`, `TIMER_SECOND`): an `ABM_REMOVE` that did not reach Explorer is sent again
/// (`appbar::retry_removal`), and a strip Explorer no longer holds (the watchdog removed it while the dock hung, or
/// Explorer restarted) is set again at once (`appbar::take_lost`). Cheap while nothing is reserved or pending.
pub(crate) fn appbar_tick() {
    appbar::retry_removal();
    if appbar::take_lost().is_some() {
        appbar_sync("registration lost", true);
    }
}

/// Sets the strip again after the watchdog's give-up, whose taskbar restore freed it while the dock runs on.
pub(crate) fn appbar_resync(reason: &str) {
    appbar_sync(reason, true);
}

/// Reserves or releases the AppBar strip (`appbar`) to match `config.reserve_space` and auto-hide, the bar's
/// visibility, a fullscreen app and the bar's edge and display. `force` repeats the position (display change, resume,
/// `ABN_POSCHANGED`).
fn appbar_sync(reason: &str, force: bool) {
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    let dpi = UI.with(|u| u.borrow().metrics.dpi);
    let mon = dock_monitor().map(|mi| mi.rcMonitor).unwrap_or(g.mon);
    appbar::sync(
        hwnd(),
        reason,
        &appbar::Input {
            option: appbar::wanted(config::get().reserve_space, config::effective_autohide()),
            visible: VISIBLE.load(Relaxed),
            fullscreen: FS_SHOWN.load(Relaxed),
            exiting: appbar::dock_exiting(crate::taskbar::exiting(), crate::watchdog::gave_up()),
            force,
            mon: [mon.left, mon.top, mon.right, mon.bottom],
            top: config_top(),
            bar_h: g.h,
            margin: Metrics { dpi }.px(render::EDGE_MARGIN),
        },
    );
}

/// (Re)arms the one-shot clock timer, aligned to the next minute/second: only while the bar is
/// visible and the clock module is shown (otherwise it is killed).
fn arm_clock(h: HWND) {
    if VISIBLE.load(Relaxed) && config::get().show_clock {
        set_timer(h, TIMER_CLOCK, crate::clock::tick_ms());
    } else {
        kill_timer(h, TIMER_CLOCK);
    }
}

// ------------------------------------------------------- battery

/// `TIMER_BATTERY` period.
const BATTERY_POLL_MS: u32 = 30_000;

/// The battery status to store: `Off` clears it (sample not taken); a hidden bar takes no sample and
/// keeps the last value; otherwise a fresh sample (`None` = unreadable or unknown right now: the last
/// value is kept, so a brief "unknown" from the firmware neither drops the module nor stops the polling).
fn next_battery(
    show: config::BatteryShow,
    visible: bool,
    current: crate::battery::BatteryStatus,
    sample: impl FnOnce() -> Option<crate::battery::BatteryStatus>,
) -> crate::battery::BatteryStatus {
    use crate::battery::BatteryStatus;
    if show == config::BatteryShow::Off {
        BatteryStatus::NONE
    } else if !visible {
        current
    } else {
        sample().unwrap_or(current)
    }
}

/// The only caller of `battery::query`. `visible` is a parameter because `show_dock` samples before
/// it sets `VISIBLE`. Redraws the bar when the stored value changed.
fn battery_refresh(visible: bool) {
    let show = config::get().show_battery;
    let changed = UI.with(|u| {
        let st = &mut *u.borrow_mut();
        let next = next_battery(show, visible, st.battery, crate::battery::query);
        std::mem::replace(&mut st.battery, next) != next
    });
    if changed {
        popup::on_battery(UI.with(|u| u.borrow().battery));
        redraw_if_visible();
    }
}

/// Arms `TIMER_BATTERY` while the bar is visible, a battery is present and the module is not off;
/// kills it otherwise. Takes no sample.
fn battery_sync() {
    let present = UI.with(|u| u.borrow().battery.present);
    if crate::battery::polling_wanted(config::get().show_battery, present, VISIBLE.load(Relaxed)) {
        set_timer(hwnd(), TIMER_BATTERY, BATTERY_POLL_MS);
    } else {
        kill_timer(hwnd(), TIMER_BATTERY);
    }
}

/// True when the open popup is the battery one and the module is no longer in the bar.
fn battery_popup_should_close(visible: &[ItemId], current: Option<PopupKind>) -> bool {
    current == Some(PopupKind::Battery) && !visible.contains(&ItemId::Battery)
}

/// Timer tick and power broadcast: sample (if visible), then re-evaluate the timer (a battery can
/// appear or vanish between two samples).
fn battery_tick() {
    battery_refresh(VISIBLE.load(Relaxed));
    battery_sync();
}

// ------------------------------------------------------- system monitor (network, metrics)

/// `TIMER_SYSMON` period: 2 s with metrics on; with only the network on every tick is a network poll,
/// so the timer runs at the poll period instead of waking up for nothing.
fn sysmon_period(metrics: bool) -> u32 {
    if metrics {
        SYSMON_MS
    } else {
        SYSMON_MS * NET_POLL_TICKS
    }
}

/// Matches `TIMER_SYSMON` and the module state to the config and to the bar's visibility. Called on
/// show, hide and config change. ZERO COST WHEN OFF: a disabled module drops its state at once
/// (CPU sampler; the shell thread's `net::Querier` via `NetworkRelease`, which unloads its libraries
/// and COM object), and with both modules off or the bar hidden the timer does not exist.
fn sysmon_sync() {
    let cfg = config::get();
    let (net_on, met_on) = (cfg.show_network, cfg.show_metrics);
    if !met_on {
        UI.with(|u| {
            let st = &mut *u.borrow_mut();
            st.sampler = None;
            st.usage = crate::metrics::Usage::default();
        });
    }
    if !net_on && NET_ACTIVE.with(|a| a.replace(false)) {
        shell_thread::request(ShellRequest::NetworkRelease);
        UI.with(|u| u.borrow_mut().net = crate::net::NetStatus::default());
        popup::on_network(crate::net::NetInfo::default());
    }
    let want = VISIBLE.load(Relaxed) && (net_on || met_on);
    let armed = SYSMON_ARMED.with(|a| a.get());
    if want && !armed {
        SYSMON_ARMED.with(|a| a.set(true));
        SYSMON_TICKS.with(|t| t.set(0));
        set_timer(hwnd(), TIMER_SYSMON, sysmon_period(met_on));
        SYSMON_PERIOD_METRICS.with(|p| p.set(met_on));
        // First sample / query right away: the bar just appeared.
        sysmon_tick();
    } else if want && armed && SYSMON_PERIOD_METRICS.with(|p| p.replace(met_on)) != met_on {
        // Metrics switched: 2 s for the sampler, the network period alone otherwise.
        SYSMON_TICKS.with(|t| t.set(0));
        set_timer(hwnd(), TIMER_SYSMON, sysmon_period(met_on));
    } else if !want && armed {
        SYSMON_ARMED.with(|a| a.set(false));
        kill_timer(hwnd(), TIMER_SYSMON);
    }
}

/// `TIMER_SYSMON`: CPU/RAM sample (metrics on), network query every `NET_POLL_TICKS` ticks (network
/// on; the reply arrives as `ShellEvent::Network`). Redraws only if the metrics changed.
fn sysmon_tick() {
    let cfg = config::get();
    let n = SYSMON_TICKS.with(|t| {
        let n = t.get();
        t.set(n.wrapping_add(1));
        n
    });
    let mut changed = false;
    if cfg.show_metrics {
        changed = UI.with(|u| {
            let st = &mut *u.borrow_mut();
            let usage = match st.sampler.as_mut() {
                Some(s) => s.sample(),
                None => {
                    // Primed now: the CPU figure starts with the next tick.
                    let s = crate::metrics::Sampler::new();
                    let u = s.last();
                    st.sampler = Some(s);
                    u
                }
            };
            std::mem::replace(&mut st.usage, usage) != usage
        });
    }
    if cfg.show_network && (!cfg.show_metrics || n.is_multiple_of(NET_POLL_TICKS)) {
        NET_ACTIVE.with(|a| a.set(true));
        shell_thread::request(ShellRequest::NetworkQuery);
    }
    if changed {
        redraw_if_visible();
    }
}

/// `restore_foreground` for the remembered window (`PREV_FG`), which is then forgotten.
fn restore_prev_foreground() {
    let prev = PREV_FG.swap(0, Relaxed);
    if prev != 0 {
        restore_foreground(HWND(prev as *mut std::ffi::c_void));
    }
}

/// Foreground window as the app buttons see it: while the bar (or a popup) holds the foreground it
/// took by the Win key, the window that had it before (`PREV_FG`), so a click on that app still
/// minimizes or cycles it. UI thread.
pub fn effective_foreground() -> HWND {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    let prev = PREV_FG.load(Relaxed);
    // SAFETY: IsWindow accepts any handle value.
    let prev_alive = prev != 0 && unsafe { IsWindow(Some(HWND(prev as *mut std::ffi::c_void))) }.as_bool();
    let own = !fg.0.is_null() && is_own_window(fg);
    HWND(effective_fg(fg.0 as isize, own, prev, prev_alive) as *mut std::ffi::c_void)
}

/// `effective_foreground` decision: `prev` if the foreground `fg` is ours and `prev` is alive, else `fg`.
fn effective_fg(fg: isize, fg_own: bool, prev: isize, prev_alive: bool) -> isize {
    if fg_own && prev != 0 && prev_alive {
        prev
    } else {
        fg
    }
}

/// Gives the foreground back to `prev` only if it now belongs to nobody else (null or ours).
fn restore_foreground(prev: HWND) {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg == prev || (!fg.0.is_null() && !is_own_window(fg)) {
        return;
    }
    // SAFETY: `prev` is a previously saved HWND; IsWindow accepts any value.
    if !unsafe { IsWindow(Some(prev)) }.as_bool() {
        return;
    }
    // SAFETY: SetForegroundWindow is allowed for us: until now we are the foreground.
    let ok = unsafe { SetForegroundWindow(prev) };
    log!("  foreground returned to the previous window -> {}", ok.as_bool());
}

/// true if `h` is one of the dock's own surfaces (bar, popups, picker, tray host). The Settings
/// window lives in this process too but counts as a normal app window: the bar remembers it as the
/// previous foreground (`PREV_FG`) and gives the focus back to it, never takes the focus from it on
/// hide, a popup is not pushed in front of it, and over a fullscreen app activating it is like
/// going to any other window.
pub fn is_own_window(h: HWND) -> bool {
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; `h` can be any HWND (fails with pid 0).
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    // SAFETY: no preconditions.
    let in_process = pid != 0 && pid == unsafe { GetCurrentProcessId() };
    dock_surface(in_process, in_process && crate::settings_window::is_window(h))
}

/// `is_own_window` decision: a window of this process that is not the Settings window.
fn dock_surface(in_process: bool, settings: bool) -> bool {
    in_process && !settings
}

/// true if the left or right mouse button is pressed right now.
fn mouse_buttons_down() -> bool {
    // SAFETY: no preconditions; the high bit means "pressed".
    unsafe {
        (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 | GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16) & 0x8000 != 0
    }
}

/// true if a mouse button was pressed since the last `GetAsyncKeyState` read.
/// The low bit is cleared by the read: call it after `mouse_buttons_down` and on every poll.
fn mouse_buttons_tapped() -> bool {
    // SAFETY: no preconditions; the low bit means "pressed since the last call".
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 | GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16) & 0x1 != 0 }
}

/// Window class name of `h` (what dock.log may say about a foreign window; never its title).
pub(crate) fn window_class(h: HWND) -> String {
    let mut cls = [0u16; 64];
    // SAFETY: `cls` is a writable buffer.
    let n = unsafe { GetClassNameW(h, &mut cls) }.max(0) as usize;
    String::from_utf16_lossy(&cls[..n.min(cls.len())])
}

/// Foreground window covering the whole monitor `mon` (exclusive fullscreen or borderless),
/// if any. Excludes desktop (Progman/WorkerW), shell, minimized/invisible windows, our own and
/// windows with a title bar (normal maximized ones).
fn fullscreen_window(mon: &RECT) -> Option<HWND> {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || is_own_window(fg) {
        return None;
    }
    // SAFETY: `fg` is an HWND obtained from Windows; the queries accept arbitrary HWNDs.
    if !unsafe { IsWindowVisible(fg) }.as_bool() || unsafe { IsIconic(fg) }.as_bool() {
        return None;
    }
    let class = window_class(fg);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return None;
    }
    // SAFETY: style read; valid HWND or the call returns 0.
    let style = unsafe { GetWindowLongPtrW(fg, GWL_STYLE) } as u32;
    if style & WS_CAPTION.0 == WS_CAPTION.0 {
        return None; // normal window (even maximized): not fullscreen
    }
    // SAFETY: IsZoomed accepts any HWND. Maximized windows without a title bar
    // (custom frame) are not fullscreen/borderless: those are not "zoomed".
    if unsafe { IsZoomed(fg) }.as_bool() {
        return None;
    }
    let mut r = RECT::default();
    // SAFETY: `r` is a writable RECT of the declared size.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            fg,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut std::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    // SAFETY: `r` is valid and writable.
    if dwm.is_err() && unsafe { GetWindowRect(fg, &mut r) }.is_err() {
        return None;
    }
    // The window's monitor must be the dock's, and the window must cover it entirely.
    // SAFETY: MonitorFromWindow with NEAREST always returns a monitor.
    let hm = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
    if !monitor_info(hm).is_some_and(|mi| rect_eq(&mi.rcMonitor, mon)) {
        return None;
    }
    let full = r.left <= mon.left && r.top <= mon.top && r.right >= mon.right && r.bottom >= mon.bottom;
    full.then_some(fg)
}

/// A fullscreen or borderless app has the foreground on its monitor (the auto-installer must not
/// interrupt it).
pub fn foreground_is_fullscreen() -> bool {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return false;
    }
    // SAFETY: MonitorFromWindow with NEAREST always returns a monitor.
    let hm = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
    monitor_info(hm).is_some_and(|mi| fullscreen_window(&mi.rcMonitor).is_some())
}

/// Where the fullscreen window in front is, for `ABN_FULLSCREENAPP` (which names no display): on the
/// bar's display, on another one, or not recognised.
fn fullscreen_where() -> appbar::FsWhere {
    let Ok(mi) = dock_monitor() else { return appbar::FsWhere::Unknown };
    if fullscreen_window(&mi.rcMonitor).is_some() {
        appbar::FsWhere::DockDisplay
    } else if foreground_is_fullscreen() {
        appbar::FsWhere::OtherDisplay
    } else {
        appbar::FsWhere::Unknown
    }
}

/// `EVENT_SYSTEM_FOREGROUND` (forwarded by `WM_DOCK_APPS_DIRTY`): if the dock is over a fullscreen
/// app and the foreground moves to another window that is not ours (the user went back to the game),
/// the dock hides immediately. A fullscreen app taking the foreground while the dock is
/// visible hides it too.
fn on_foreground_changed(fg: HWND) {
    popup::popup_launcher::on_foreground(fg);
    if !VISIBLE.load(Relaxed) || fg.0.is_null() || is_own_window(fg) || tray_host::foreign_menu_active() {
        return;
    }
    // The event is posted: by now the foreground may already be ours again (stale event).
    // SAFETY: no preconditions.
    let cur = unsafe { GetForegroundWindow() };
    if !cur.0.is_null() && is_own_window(cur) {
        log!("foreground event ignored: the foreground is ours again");
        return;
    }
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    if FS_SHOWN.load(Relaxed) {
        // Closing one of our popups gives the foreground back to the game: it is not a user click.
        // A real click on the game is detected by `on_poll` anyway.
        if fg.0 as isize == PREV_FG.load(Relaxed) && popup::closed_within(POPUP_CLOSE_GRACE_MS) {
            log!("foreground returned to the game by closing a popup: the dock stays");
            return;
        }
        // The game bounced the foreground back right after we took it: take it again (a few times).
        if fs_bounce_grace(fg) && FS_RETAKES.load(Relaxed) < FS_MAX_RETAKES {
            let n = FS_RETAKES.fetch_add(1, Relaxed) + 1;
            log!("fullscreen app bounced the foreground back right after the dock took it: re-taking it ({n})");
            force_foreground(popup::current_hwnd().unwrap_or_else(hwnd));
            return;
        }
        hide_dock("foreground moved to another window (fullscreen)");
    } else if fullscreen_window(&g.mon).is_some() {
        hide_dock("a fullscreen app took the foreground");
    }
}

/// true if `fg` is the fullscreen app under the dock and the dock/popup took the foreground less
/// than `FS_BOUNCE_GRACE_MS` ago: the change is the app bouncing it back, not a user action.
/// `pub` because `popup` must not close on that bounce either.
pub fn fs_bounce_grace(fg: HWND) -> bool {
    FS_SHOWN.load(Relaxed)
        && fg.0 as isize == PREV_FG.load(Relaxed)
        && within_grace(now_ms(), FG_TAKEN_AT.load(Relaxed), FS_BOUNCE_GRACE_MS)
}

/// true if `taken_at` (0 = never) is less than `grace` ms before `now`.
fn within_grace(now: u64, taken_at: u64, grace: u64) -> bool {
    taken_at != 0 && now.saturating_sub(taken_at) < grace
}

/// "Foreground" fallback (the foreground fallback of the original design).
/// Variant 1: AttachThreadInput to the foreground thread + SetForegroundWindow, only when that thread answers, and so does
/// every other thread whose window its input queue names (`variant1_attempt`); a thread that does not answer is never
/// joined and variant 1 is skipped.
/// Variant 2 (only if 1 failed or was skipped): a synthetic F15 key (releases the foreground
/// lock) and SetForegroundWindow again.
/// Variant 3 (only if 2 failed): `SwitchToThisWindow`, which bypasses the lock in cases where the
/// first two are refused (e.g. an ELEVATED app in front: UIPI blocks both the attach and the F15).
/// Variants 2 and 3 never wait for the foreground thread: without a joined queue its deactivation is posted to it.
/// A null foreground (transient, e.g. right after a window was hidden) is NOT a reason to give up:
/// with no foreground window SetForegroundWindow is allowed.
/// The outcome of each is in the log. Returns true if `h` is the foreground window at the end.
/// `pub` because `popup` also uses it to activate the popup window, and the launcher to give the foreground back.
pub fn force_foreground(h: HWND) -> bool {
    FG_TAKEN_AT.store(now_ms(), Relaxed);
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg == h {
        return true;
    }
    if variant1_attempt(h, fg).is_some_and(|ok| ok && is_foreground(h)) {
        return true;
    }
    // A window hidden meanwhile (a popup closed during its own activation) is not pushed to the front: variants 2 and 3
    // would take the foreground from the window the close gave it to and leave it on an invisible window.
    // SAFETY: IsWindowVisible accepts any handle.
    if !unsafe { IsWindowVisible(h) }.as_bool() {
        return false;
    }
    let sent = send_f15();
    // SAFETY: `h` is a window handle; SetForegroundWindow accepts any.
    let ok2 = unsafe { SetForegroundWindow(h) };
    log!("  foreground variant 2 (synthetic F15, SendInput={sent}): SetForegroundWindow -> {}", ok2.as_bool());
    if ok2.as_bool() && is_foreground(h) {
        return true;
    }
    // SAFETY: `h` is a window handle; SwitchToThisWindow has no other preconditions.
    unsafe { SwitchToThisWindow(h, true) };
    let ok3 = is_foreground(h);
    log!("  foreground variant 3 (SwitchToThisWindow): foreground -> {ok3}");
    ok3
}

/// How long `answers` waits for another thread's window (ms): a pumping thread answers in microseconds; one that stopped
/// pumping costs this once per open (`SILENT_MS`).
const PING_MS: u32 = 50;
/// A foreground thread that did not answer is taken as silent without a new ping for this long (ms): the open's
/// foreground retries (6 x 80 ms) and a Start opened by the Win key right after the bar's show wait once.
const SILENT_MS: u64 = 1000;

thread_local! {
    /// The last foreground thread that did not answer and when (thread id, tick in ms).
    static SILENT: Cell<(u32, u64)> = const { Cell::new((0, 0)) };
}

/// How variant 1 of `force_foreground` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant1 {
    /// No other thread in front (no foreground window, or one of the dock's thread): variant 1 without a join.
    Alone,
    /// The foreground thread answered, and so did the other threads its input queue names: the queue is joined for
    /// variant 1, as in 1.4.3.
    Joined,
    /// The foreground thread does not answer: no join and no variant 1, whose activation would wait for it.
    Skipped,
    /// The foreground thread answers, another thread whose window its input queue names does not (`queue_answers`):
    /// no join and no variant 1, as for `Skipped`.
    QueueSilent,
}

/// The choice of variant 1 for the foreground thread `fg_tid` (0: no foreground window) seen from the dock's thread
/// `my_tid`. `front` (the ping of the foreground window), then `queue` (the other threads of its input queue), are
/// asked only when there is another thread to join.
fn variant1(fg_tid: u32, my_tid: u32, front: impl FnOnce() -> bool, queue: impl FnOnce() -> bool) -> Variant1 {
    if fg_tid == 0 || fg_tid == my_tid {
        Variant1::Alone
    } else if !front() {
        Variant1::Skipped
    } else if !queue() {
        Variant1::QueueSilent
    } else {
        Variant1::Joined
    }
}

/// Whether `tid`, the thread of a window the foreground input queue names, is a third thread that must answer before the
/// join: neither the foreground thread `fg_tid` (pinged already) nor the dock's `my_tid`; 0 is no window or one gone.
fn third_thread(tid: u32, fg_tid: u32, my_tid: u32) -> bool {
    tid != 0 && tid != fg_tid && tid != my_tid
}

/// Whether the windows the input queue of the foreground thread `fg_tid` names (active, focus, capture, menu owner,
/// move-size) answer when they belong to a third thread. Windows shares one input queue between a window and its child
/// windows of another thread or process (a preview pane, an out-of-process designer, an in-place OLE server): joined to
/// it, variant 1's activation sends WM_NCACTIVATE, WM_KILLFOCUS and the like to them and waits for their thread, which
/// the foreground window's ping says nothing about. GetGUIThreadInfo sends nothing; its failure is taken as silence.
/// UI thread.
fn queue_answers(fg_tid: u32, my_tid: u32) -> bool {
    let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    // SAFETY: `gti` is writable for the call and its cbSize is set, as the call requires.
    if unsafe { GetGUIThreadInfo(fg_tid, &mut gti) }.is_err() {
        return false;
    }
    let wins = [gti.hwndActive, gti.hwndFocus, gti.hwndCapture, gti.hwndMenuOwner, gti.hwndMoveSize];
    wins.into_iter().all(|w| {
        // SAFETY: any handle; a null or stale one yields 0.
        let tid = unsafe { GetWindowThreadProcessId(w, None) };
        !third_thread(tid, fg_tid, my_tid) || front_answers(w, tid)
    })
}

/// Whether thread `tid` is still taken as silent at `now`, `last` being the last thread that did not answer and when.
fn still_silent(last: (u32, u64), tid: u32, now: u64) -> bool {
    tid != 0 && last.0 == tid && now.saturating_sub(last.1) < SILENT_MS
}

/// Whether the thread of window `h` answers now: a WM_NULL through SendMessageTimeoutW, which returns at once for a
/// window Windows already calls hung (SMTO_ABORTIFHUNG, 5 s without pumping) and after `PING_MS` of wall-clock time at
/// most otherwise. SMTO_BLOCK makes that bound true: without it this thread dispatches the messages other threads send
/// its windows while it waits, and each one starts the timeout again (live check V-14: 7.45 s with another process
/// sending the dock a message every 10 ms, the heartbeat starved). It cannot deadlock: the wait is one wait for the
/// reply with that timeout. A thread that sends the dock a message meanwhile waits those 50 ms at most, as for any
/// 50 ms handler; a window whose answer needs the dock's thread is taken as silent, the safe side. A window of the
/// dock's own thread answers; a refused or failed send (a window gone, a higher integrity) does not. UI thread.
pub(crate) fn answers(h: HWND) -> bool {
    // SAFETY: any handle; a stale one yields 0.
    if unsafe { GetWindowThreadProcessId(h, None) } == unsafe { GetCurrentThreadId() } {
        return true;
    }
    let mut res = 0usize;
    let flags = SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT;
    // SAFETY: `res` is writable for the call; any handle is accepted (a stale one fails at once); WM_NULL does nothing.
    unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), flags, PING_MS, Some(&mut res)) }.0 != 0
}

/// `answers` for a window `fg` of thread `tid` that variant 1 would wait for (the foreground window, another thread's
/// window in its input queue), which a thread that did not answer in the last `SILENT_MS` skips. UI thread.
fn front_answers(fg: HWND, tid: u32) -> bool {
    let now = now_ms();
    if SILENT.with(|s| still_silent(s.get(), tid, now)) {
        return false;
    }
    let ok = answers(fg);
    if !ok {
        SILENT.with(|s| s.set((tid, now)));
    }
    ok
}

/// Variant 1 of `force_foreground`, the dock's only AttachThreadInput (launcher lint 17): the dock's input queue is
/// joined to the foreground thread's only when that thread and the others its queue names answer (`variant1`). Joined,
/// the raise and the activation below wait for those threads: one that stopped pumping held the dock's UI thread for as
/// long as it hung (live check V-14, 28.5 s; the watchdog then gave the taskbar back). None when variant 1 was skipped.
fn variant1_attempt(h: HWND, fg: HWND) -> Option<bool> {
    // SAFETY: `fg` is an HWND obtained from Windows (or null: then the call returns 0); the out pid is optional.
    let fg_tid = if fg.0.is_null() { 0 } else { unsafe { GetWindowThreadProcessId(fg, None) } };
    // SAFETY: no preconditions.
    let my_tid = unsafe { GetCurrentThreadId() };
    let v = variant1(fg_tid, my_tid, || front_answers(fg, fg_tid), || queue_answers(fg_tid, my_tid));
    if v == Variant1::Skipped {
        log!("  foreground variant 1 skipped: the foreground window does not answer, its input queue is not joined");
        return None;
    }
    if v == Variant1::QueueSilent {
        log!("  foreground variant 1 skipped: another thread's window in the foreground input queue does not answer");
        return None;
    }
    if v == Variant1::Joined {
        // SAFETY: valid thread ids; the attach is always undone below.
        let _ = unsafe { AttachThreadInput(my_tid, fg_tid, true) };
    }
    raise(h);
    // SAFETY: `h` is a window handle; SetForegroundWindow accepts any.
    let ok = unsafe { SetForegroundWindow(h) }.as_bool();
    if v == Variant1::Joined {
        // SAFETY: undoes the attach done above.
        let _ = unsafe { AttachThreadInput(my_tid, fg_tid, false) };
    }
    log!("  foreground variant 1 (AttachThreadInput, fg null={}): SetForegroundWindow -> {ok}", fg.0.is_null());
    Some(ok)
}

/// `BringWindowToTop` for a window of the dock's thread. Another thread's window (the launcher gives the foreground back
/// to the window it came from, the bar activates an app) gets the same request posted to its own thread
/// (SWP_ASYNCWINDOWPOS): done in place, it waits for that thread, without bound when the window hangs.
pub(crate) fn raise(h: HWND) {
    // SAFETY: any handle; a stale one yields 0 and the calls below fail harmlessly.
    unsafe {
        if GetWindowThreadProcessId(h, None) == GetCurrentThreadId() {
            let _ = BringWindowToTop(h);
        } else {
            let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS);
        }
    }
}

/// true if `h` is the foreground window right now.
fn is_foreground(h: HWND) -> bool {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    fg == h
}

/// Presses and releases F15 (a key no app uses): unlocks SetForegroundWindow. Tagged INJECT_TAG like every key the dock
/// injects (launcher lint 13): an owner-probe build that accepts injected keys would otherwise buffer it as typed.
/// Returns the number of events accepted by SendInput.
fn send_f15() -> u32 {
    let ev = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_F15, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: hook::INJECT_TAG },
        },
    };
    let inputs = [ev(KEYBD_EVENT_FLAGS(0)), ev(KEYEVENTF_KEYUP)];
    // SAFETY: slice of 2 initialized INPUTs; cbSize = size_of::<INPUT>() as required.
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }
}

// ------------------------------------------------------------ edge watch

/// How long the pointer must be still before the poll hands over to raw input.
const STILL_MS: u64 = 1000;
/// The one switch: false gives the always-on 50 ms poll of 1.5.0.
const MOTION_GATE: bool = true;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeWatch {
    /// The 50 ms `TIMER_POLL` runs.
    Poll,
    /// No timer: raw input wakes the UI thread at the pointer's next motion.
    Motion,
    /// The poll runs and raw input stays registered (Push reveal, the pointer moving on the edge row).
    PollRaw,
}

#[derive(Clone, Copy)]
struct WatchInputs {
    visible: bool,
    gate_ok: bool,
    dwell: bool,
    requests: bool,
    drag: bool,
    still_ms: u64,
    /// Push reveal: the pointer rests on the edge row of the hidden bar.
    push_edge: bool,
}

/// Push reveal: the sum of the relative moves towards the screen edge inside a window of `PUSH_WINDOW_MS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PushSum {
    sum: i32,
    since: u64,
}

/// A push counts only inside this window (ms); an older window restarts.
const PUSH_WINDOW_MS: u64 = 1000;

/// Adds one relative vertical move (already signed towards the edge) to the push sum. A window older than
/// `PUSH_WINDOW_MS` restarts, a move away from the edge resets the sum, moves towards it add. True when the sum
/// reaches `threshold` (the sum then resets).
fn push_step(p: PushSum, dy_towards_edge: i32, now: u64, threshold: u32) -> (PushSum, bool) {
    let mut p = if now.saturating_sub(p.since) > PUSH_WINDOW_MS { PushSum { sum: 0, since: now } } else { p };
    if dy_towards_edge < 0 {
        p.sum = 0;
    } else {
        p.sum = p.sum.saturating_add(dy_towards_edge);
    }
    if i64::from(p.sum) >= i64::from(threshold) {
        return (PushSum::default(), true);
    }
    (p, false)
}

/// The vertical move of a raw report as a move towards the edge: up for the top bar, down for the bottom one.
fn towards_edge(dy: i32, top: bool) -> i32 {
    if top {
        dy.saturating_neg()
    } else {
        dy
    }
}

/// The vertical move of a raw input report that may count as a push: only a mouse report with relative motion.
/// Absolute devices (pens, touch screens, remote sessions) never push.
fn push_delta(kind: u32, flags: u16, dy: i32) -> Option<i32> {
    const RIM_TYPEMOUSE: u32 = 0;
    const MOUSE_MOVE_ABSOLUTE: u16 = 1;
    (kind == RIM_TYPEMOUSE && flags & MOUSE_MOVE_ABSOLUTE == 0).then_some(dy)
}

/// True when the raw input arm may run the poll body again (the poll timer's period) while a push is going on.
fn raw_poll_due(last: u64, now: u64) -> bool {
    now.saturating_sub(last) >= 50
}

/// Push reveal is the mode in force: built, and chosen in the config.
fn push_mode() -> bool {
    PUSH_AVAILABLE && config::get().reveal == config::Reveal::Push
}

#[derive(Clone, Copy)]
enum GateEvent {
    RegisterFailed,
    Shown,
}

fn edge_watch(w: WatchInputs) -> EdgeWatch {
    if w.push_edge && !w.visible && w.gate_ok && !w.requests && !w.drag {
        return if w.still_ms < STILL_MS { EdgeWatch::PollRaw } else { EdgeWatch::Motion };
    }
    if w.visible || !w.gate_ok || w.dwell || w.requests || w.drag || w.still_ms < STILL_MS {
        EdgeWatch::Poll
    } else {
        EdgeWatch::Motion
    }
}

fn still_since(prev: Option<POINT>, cur: Option<POINT>, since: u64, now: u64) -> u64 {
    match (prev, cur) {
        (_, None) => since,
        (Some(a), Some(b)) if a.x == b.x && a.y == b.y => since,
        _ => now,
    }
}

fn gate_after(_gate_ok: bool, ev: GateEvent) -> bool {
    match ev {
        GateEvent::RegisterFailed => false,
        GateEvent::Shown => MOTION_GATE,
    }
}

/// Registers (`on`) or removes the raw input of the pointing devices (mouse, pen, touch screen, touch pad) with
/// RIDEV_INPUTSINK: no foreground needed, other programs' input untouched. Known limit: a pointer moved only by
/// SetCursorPos makes no input event and does not open the gate; injected input (SendInput, remote desktop, KVM
/// tools) does. Pen, touch screen and touch pad usages cannot be tried here: pure tests only.
/// Invariant: only one window per process may register a raw input device class and nothing else in the dock
/// registers raw input; a future user of raw input must share this registration.
fn raw_motion(on: bool) -> bool {
    let target = if on { hwnd() } else { HWND::default() };
    let flags = if on { RIDEV_INPUTSINK } else { RIDEV_REMOVE };
    let dev = |page: u16, usage: u16| RAWINPUTDEVICE {
        usUsagePage: page,
        usUsage: usage,
        dwFlags: flags,
        hwndTarget: target,
    };
    let all = [dev(0x01, 0x02), dev(0x0D, 0x02), dev(0x0D, 0x04), dev(0x0D, 0x05)];
    let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
    // SAFETY: the slices hold valid RAWINPUTDEVICEs; hwndTarget is our window (or null with RIDEV_REMOVE).
    let mut res = unsafe { RegisterRawInputDevices(&all, size) };
    if res.is_err() {
        // SAFETY: as above, the mouse alone.
        res = unsafe { RegisterRawInputDevices(&all[..1], size) };
    }
    match res {
        Ok(()) => {
            RAW_ON.set(on);
            true
        }
        Err(e) => {
            if on {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, Relaxed) {
                    log!("ui: raw input unavailable ({e}), the edge poll stays on until the next show");
                }
            } else {
                // The registration is gone or never was: do not wait for a message that cannot come.
                RAW_ON.set(false);
            }
            false
        }
    }
}

/// Puts the edge watch in the state `edge_watch` asks for; touches the timer and the registration only on a change.
fn poll_sync() {
    let h = hwnd();
    if h.0.is_null() {
        return;
    }
    let want = edge_watch(WatchInputs {
        visible: VISIBLE.load(Relaxed),
        gate_ok: GATE_OK.get(),
        dwell: EDGE_SINCE.load(Relaxed) != 0,
        requests: NEED_REQUESTS.with(|n| n.get()),
        drag: drag_active(),
        still_ms: now_ms().saturating_sub(STILL_SINCE.get()),
        push_edge: PUSH_EDGE.get(),
    });
    match want {
        EdgeWatch::PollRaw => {
            if !POLL_ON.get() {
                set_timer(h, TIMER_POLL, 50);
                POLL_ON.set(true);
            }
            if !RAW_ON.get() && !raw_motion(true) {
                GATE_OK.set(gate_after(GATE_OK.get(), GateEvent::RegisterFailed));
            }
        }
        EdgeWatch::Poll => {
            if RAW_ON.get() {
                raw_motion(false);
            }
            if !POLL_ON.get() {
                set_timer(h, TIMER_POLL, 50);
                POLL_ON.set(true);
            }
        }
        EdgeWatch::Motion => {
            if POLL_ON.get() {
                kill_timer(h, TIMER_POLL);
                POLL_ON.set(false);
            }
            if !RAW_ON.get() && !raw_motion(true) {
                GATE_OK.set(gate_after(GATE_OK.get(), GateEvent::RegisterFailed));
                set_timer(h, TIMER_POLL, 50);
                POLL_ON.set(true);
            }
        }
    }
}

// ------------------------------------------------------------ poll timer

/// With several monitors: the edge `y == mon.top` (or `mon.bottom - 1`) is a real screen border
/// only if there is no other monitor right beyond it (otherwise the cursor crosses it freely).
fn edge_is_screen_border(mon: &RECT, x: i32) -> bool {
    let beyond = if config_top() { POINT { x, y: mon.top - 1 } } else { POINT { x, y: mon.bottom } };
    // SAFETY: MonitorFromPoint with MONITOR_DEFAULTTONULL has no preconditions; returns NULL if outside.
    let other = unsafe { MonitorFromPoint(beyond, MONITOR_DEFAULTTONULL) };
    other.0.is_null()
}

/// The vertical delta of the raw input report `lp` when it is a relative mouse move (`push_delta`).
fn raw_mouse_dy(lp: LPARAM) -> Option<i32> {
    let mut raw = RAWINPUT::default();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    // SAFETY: `raw` is a RAWINPUT on the stack and `size` is its size, as GetRawInputData requires; `lp` is the
    // handle of the report this WM_INPUT carries; the header size is that of RAWINPUTHEADER.
    let n = unsafe {
        GetRawInputData(
            HRAWINPUT(lp.0 as *mut std::ffi::c_void),
            RID_INPUT,
            Some(&mut raw as *mut RAWINPUT as *mut std::ffi::c_void),
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if n == u32::MAX || n == 0 {
        return None;
    }
    const RIM_TYPEMOUSE: u32 = 0;
    if raw.header.dwType != RIM_TYPEMOUSE {
        return None;
    }
    // SAFETY: dwType is RIM_TYPEMOUSE, so data.mouse is the member GetRawInputData filled.
    let m = unsafe { raw.data.mouse };
    push_delta(raw.header.dwType, m.usFlags.0, m.lLastY)
}

/// Push reveal: shows the bar from the edge, as the dwell does (not over a fullscreen window), and starts over.
fn push_show() {
    PUSH_EDGE.set(false);
    PUSH_SUM.set(PushSum::default());
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    let mon = if follows_pointer() { monitor_info(pointer_hmonitor()).map_or(g.mon, |mi| mi.rcMonitor) } else { g.mon };
    if fullscreen_window(&mon).is_some() {
        return;
    }
    if !rect_eq(&mon, &g.mon) {
        follow_pointer();
    }
    show_dock("screen edge (push)", key_show_ms(config::get().hide_delay_ms));
}

fn point_in(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// 50 ms timer while the bar is shown, while a dwell or an icon request is pending and for 1 s after the pointer last
/// moved (otherwise raw input waits for the next motion, see "edge watch"): auto-hide, screen edge, topmost
/// re-assertion.
/// With `!config::effective_autohide()` the bar never hides by itself; nor does it
/// while the context menu (popup) is open.
fn on_poll() {
    on_poll_body();
    if !VISIBLE.load(Relaxed) {
        poll_sync();
    }
}

fn on_poll_body() {
    // Esc cancels a drag (the bar has the keyboard focus only after the Win key, so the key state is polled).
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit means "pressed".
    if drag_active() && unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000 != 0 {
        end_drag(true);
    }
    let Some(g) = GEOM.with(|g| g.get()) else { return };
    let cur = cursor_pos();
    let now = now_ms();
    STILL_SINCE.set(still_since(LAST_POS.get(), cur, STILL_SINCE.get(), now));
    if cur.is_some() {
        LAST_POS.set(cur);
    }
    // A failed read (locked session, secure desktop, disconnected RDP) counts as stillness.
    let Some(p) = cur else {
        PUSH_EDGE.set(false);
        return;
    };
    let autohide = crate::config::effective_autohide();
    if VISIBLE.load(Relaxed) {
        let bar = RECT { left: g.x, top: g.y, right: g.x + g.w, bottom: g.y + g.h };
        // Dock open over a fullscreen app: a click outside the bar and our popups
        // (windows of our process) returns to the app and hides the dock immediately.
        let down = mouse_buttons_down();
        let was_down = CLICK_DOWN.swap(down, Relaxed);
        // A click shorter than the poll period only leaves the "pressed since the last call" bit.
        let tapped = mouse_buttons_tapped();
        if FS_SHOWN.load(Relaxed)
            && ((down && !was_down) || tapped)
            && !point_in(&bar, p)
            && !tray_host::foreign_menu_active()
            && !drag_active()
        {
            // SAFETY: WindowFromPoint has no preconditions; returns NULL if there is no window.
            let under = unsafe { WindowFromPoint(p) };
            if under.0.is_null() || !is_own_window(under) {
                popup::close("click outside the dock (fullscreen)");
                hide_dock("click outside the dock over a fullscreen app");
                return;
            }
        }
        let engaged = point_in(&bar, p)
            || crate::popup::is_open()
            || crate::picker::is_open()
            || tray_host::foreign_menu_active()
            || drag_active();
        // The keyboard layer's idle end does not depend on the hide decision: with auto-hide off it still ends.
        let mut kb_stays = false;
        match kb_poll_now(engaged, autohide) {
            // The time spent in a popup, the picker or under the pointer is not time left alone.
            KbPoll::Engaged => kb_hold(now),
            KbPoll::CheckIdle { restore } => {
                if !kb_idle_end(now) {
                    // Only a visible ring holds the bar; with the ring off the pointer's rules hide it.
                    kb_stays = kb_active();
                } else if restore {
                    // Nothing hides the bar to hand the foreground back, as Esc does.
                    restore_prev_foreground();
                }
            }
            KbPoll::Nothing => {}
        }
        if engaged || !autohide {
            HIDE_AT.store(hide_at(now, hide_delay()), Relaxed);
        } else if kb_stays {
            // The keyboard is on the bar (D 5.2.3): it stays, and keeps the key-show time once the keys stop.
            HIDE_AT.store(kb_hide_at(now, config::get().hide_delay_ms), Relaxed);
        } else if now >= HIDE_AT.load(Relaxed) {
            hide_dock("timeout");
        }
        // Re-asserts topmost about every 500 ms while visible.
        let n = POLL_TICKS.with(|t| {
            t.set(t.get().wrapping_add(1));
            t.get()
        });
        if n.is_multiple_of(10) && VISIBLE.load(Relaxed) && !tray_host::foreign_menu_active() {
            reassert_topmost(hwnd());
        }
    }
    // Screen edge: y==top (bar at the top) or y==bottom-1 (bar at the bottom), on the bar's display.
    // "pointer" mode: on the display under the pointer, which the bar then moves to (even while still
    // visible on another display).
    let follow = follows_pointer();
    let mon = if follow { monitor_info(pointer_hmonitor()).map_or(g.mon, |mi| mi.rcMonitor) } else { g.mon };
    let other_display = !rect_eq(&mon, &g.mon);
    let edge_y = if config_top() { mon.top } else { mon.bottom - 1 };
    let at_edge = p.y == edge_y && p.x >= mon.left && p.x < mon.right && edge_is_screen_border(&mon, p.x);
    // Push reveal keeps only the edge flag up to date here; the reveal itself comes from the raw input arm.
    let push = push_mode();
    let push_edge = push && at_edge && !VISIBLE.load(Relaxed);
    if push_edge != PUSH_EDGE.get() {
        PUSH_EDGE.set(push_edge);
        PUSH_SUM.set(PushSum::default());
    }
    PUSH_THRESHOLD.set(config::get().push_threshold);
    PUSH_TOP.set(config_top());
    if !at_edge {
        EDGE_SINCE.store(0, Relaxed);
        EDGE_ARMED.store(true, Relaxed); // re-arms only after leaving the edge
    } else if !push && (!VISIBLE.load(Relaxed) || other_display) && EDGE_ARMED.load(Relaxed) {
        let since = EDGE_SINCE.load(Relaxed);
        if since == 0 {
            EDGE_SINCE.store(now, Relaxed);
        } else if dwell_done(since, now, u64::from(config::get().show_delay_ms)) {
            EDGE_SINCE.store(0, Relaxed);
            EDGE_ARMED.store(false, Relaxed);
            // On fullscreen / borderless apps the edge does NOT open the dock: only the Win key.
            if fullscreen_window(&mon).is_none() {
                if other_display {
                    follow_pointer();
                }
                show_dock("screen edge", key_show_ms(config::get().hide_delay_ms));
            }
        }
    }
}

#[cfg(test)]
#[path = "theme_contract.rs"]
mod theme_contract; // DS-THEME fills it

#[cfg(test)]
mod tests {
    /// The text of ui.rs before its test module, comments dropped.
    fn code_before_tests() -> String {
        let src = include_str!("ui.rs");
        let end = src.find("\nmod tests {").unwrap_or(src.len());
        src.get(..end)
            .unwrap_or("")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The takeover bits are part of every publication of the chord table: the hook gets one table, not two.
    #[test]
    fn takeover_bits_reach_every_chord_publication() {
        let code = code_before_tests();
        let calls: Vec<_> = code.match_indices("set_dock_chords(").collect();
        assert!(!calls.is_empty(), "the publication exists");
        for (at, _) in calls {
            let line = code.get(at..).and_then(|t| t.lines().next()).unwrap_or("");
            assert!(line.contains("chord_bits_now()"), "{line}");
        }
    }

    /// The takeover's foreground hook sits in the WM_DOCK_APPS_DIRTY arm, beside the guard, never inside it.
    #[test]
    fn foreground_hook_is_beside_the_guard() {
        let code = code_before_tests();
        assert_eq!(code.matches("takeover::on_foreground(").count(), 1);
        let arm = code.find("WM_DOCK_APPS_DIRTY => {").expect("the arm exists");
        let hook = code.find("takeover::on_foreground(").expect("the call exists");
        let next_arm = code.get(arm..).and_then(|t| t.find("WM_DOCK_SHELL_READY =>")).expect("the next arm exists");
        assert!(arm < hook && hook < arm + next_arm, "the call is in the arm");
        let f = code.find("fn on_foreground_changed(").expect("the guard exists");
        let body = code.get(f..).and_then(|t| t.find("\n}\n").map(|e| &t[..e])).expect("the guard ends");
        assert!(!body.contains("takeover::"), "nothing of the takeover inside the guard");
    }

    /// Every change of the bar's size or display sets the reserved strip again (`appbar_sync`): a display change, and
    /// a DPI change of the bar's display, which comes without WM_DISPLAYCHANGE (a new scale in Windows' settings).
    #[test]
    fn the_strip_follows_every_geometry_change() {
        let code = code_before_tests();
        for arm in ["WM_DPICHANGED => {", "WM_DISPLAYCHANGE => {"] {
            let at = code.find(arm).expect(arm);
            let end = code.get(at..).and_then(|t| t.find("Some(LRESULT(0))")).expect("the arm ends") + at;
            assert!(code[at..end].contains("appbar_sync("), "{arm}: {}", &code[at..end]);
        }
    }

    /// The strip's option counts only while auto-hide is off (`appbar::wanted`).
    #[test]
    fn the_strip_option_counts_only_without_auto_hide() {
        let code = code_before_tests();
        assert!(code.contains("option: appbar::wanted(config::get().reserve_space, config::effective_autohide()),"));
    }

    /// The strip's exit is the dock's own, not the taskbar restore of the watchdog's give-up, after which the dock runs on.
    #[test]
    fn the_strip_reads_the_real_exit() {
        let code = code_before_tests();
        assert!(code.contains("exiting: appbar::dock_exiting(crate::taskbar::exiting(), crate::watchdog::gave_up()),"));
    }

    /// Explorer hears of the bar's position changes with the change itself (lParam's WINDOWPOS), so a z-order change
    /// alone stays the dock's.
    #[test]
    fn explorer_hears_of_the_moves_it_can_use() {
        assert!(code_before_tests().contains("appbar::on_window_pos_changed(h, lp);"));
    }

    /// The app menu's rows depend on the apps' state: it is re-read right before the menu opens.
    #[test]
    fn app_menu_refreshes_first() {
        let code = code_before_tests();
        let open = code.find("popup::open_app_menu(").expect("the opener exists");
        let before = code.get(..open).unwrap_or("").trim_end();
        assert!(before.ends_with("suite_links::refresh();"), "the statement before the call");
        assert!(code.get(open..).unwrap_or("").contains("show_in_tasks: suite_links::usable(suite_links::App::Tasks)"));
    }

    #[test]
    fn ui_aliases_equal_their_old_literals() {
        assert_eq!(PLATE_IN_MS, 90);
        assert_eq!(PLATE_OUT_MS, 140);
        assert_eq!(HOVER_ALPHA, 20);
        assert_eq!(PRESS_ALPHA, 36);
        assert_eq!(PICKER_HOVER_MS, 400);
        assert_eq!(WIDTH_ANIM_MS, 160);
        assert_eq!(SHOW_ANIM_MS, 140);
        assert_eq!(SLIDE_PX, 10);
        assert_eq!(DRAG_THRESHOLD, 6);
    }

    use super::*;

    #[test]
    fn dock_chords_from_config() {
        use hook::DockChords as C;
        let all = C::DIGITS
            .with(C::SHIFT_DIGITS)
            .with(C::ALT_DIGITS)
            .with(C::WIN_T)
            .with(C::WIN_SHIFT_T)
            .with(C::WIN_B)
            .with(C::PEEK);
        let def = config::Config::default();
        assert_eq!(dock_chords(&def), all, "the default owns all seven rows");
        assert_eq!(all, C(0x7F));
        let shown = config::Config { hide_native_taskbar: false, ..config::Config::default() };
        assert_eq!(dock_chords(&shown), C::NONE, "the native taskbar shown: nothing owned");
        let mut c = config::Config::default();
        c.keyboard.win_numbers = false;
        let t = dock_chords(&c);
        for f in [C::DIGITS, C::SHIFT_DIGITS, C::ALT_DIGITS, C::PEEK] {
            assert!(!t.has(f), "{f:?} without win_numbers");
        }
        assert!(t.has(C::WIN_T) && t.has(C::WIN_SHIFT_T) && t.has(C::WIN_B));
        let mut c = config::Config::default();
        c.keyboard.win_t = false;
        let t = dock_chords(&c);
        assert!(!t.has(C::WIN_T) && !t.has(C::WIN_SHIFT_T));
        assert!(t.has(C::DIGITS) && t.has(C::WIN_B) && t.has(C::PEEK));
        let mut c = config::Config::default();
        c.keyboard.win_b = false;
        assert!(!dock_chords(&c).has(C::WIN_B));
        let mut c = config::Config::default();
        c.keyboard.peek = false;
        assert_eq!(dock_chords(&c), C(all.0 & !C::PEEK.0));
    }

    /// Lock, unlock, connects, disconnects, suspend and resume are cut-offs for the hook; the other session and power
    /// events (a battery change, a remote control change) are not.
    #[test]
    fn session_and_power_cut_offs() {
        use windows::Win32::UI::WindowsAndMessaging::{
            PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, WM_WTSSESSION_CHANGE, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT,
            WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGON, WTS_SESSION_REMOTE_CONTROL,
            WTS_SESSION_UNLOCK,
        };
        let session = Some(hook::CutOff::Session);
        for code in [
            WTS_SESSION_LOCK,
            WTS_SESSION_UNLOCK,
            WTS_CONSOLE_CONNECT,
            WTS_CONSOLE_DISCONNECT,
            WTS_REMOTE_CONNECT,
            WTS_REMOTE_DISCONNECT,
        ] {
            assert_eq!(cut_off_of(WM_WTSSESSION_CHANGE, code as usize), session, "{code}");
        }
        for code in [WTS_SESSION_LOGON, WTS_SESSION_REMOTE_CONTROL] {
            assert_eq!(cut_off_of(WM_WTSSESSION_CHANGE, code as usize), None, "{code}");
        }
        let power = Some(hook::CutOff::Power);
        assert_eq!(cut_off_of(WM_POWERBROADCAST, PBT_APMSUSPEND as usize), power);
        assert_eq!(cut_off_of(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize), power);
        assert_eq!(cut_off_of(WM_POWERBROADCAST, PBT_APMPOWERSTATUSCHANGE as usize), None);
        assert_eq!(cut_off_of(WM_TIMER, WTS_SESSION_LOCK as usize), None, "only the two messages");
    }

    /// A remote-desktop or VM client in front owns every chord: the hook gets no row (it answers as 1.5.2 did, so the
    /// session gets Win+n, Win+T, Win+B and Peek never arms); so does a window above the dock, whose keys the hook
    /// never sees; any other window gets the config's rows unchanged.
    #[test]
    fn a_remote_client_in_front_gets_the_chords() {
        use hook::DockChords as C;
        let all = dock_chords(&config::Config::default());
        let some = [all, C::DIGITS, C::WIN_T.with(C::WIN_SHIFT_T), C::WIN_B, C::PEEK.with(C::DIGITS), C::NONE];
        for rows in some {
            assert_eq!(effective_chords(rows, Front::Remote), C::NONE, "{rows:?}: remote client in front");
            assert_eq!(effective_chords(rows, Front::Above), C::NONE, "{rows:?}: the UAC prompt in front");
            assert_eq!(effective_chords(rows, Front::Local), rows, "{rows:?}: a local window");
        }
    }

    /// The window in front by its process's image: the UAC prompt (consent.exe) and a process the dock may not open are
    /// above the dock; the remote clients stay remote (the launcher's list, unchanged); anything else, or nothing, is
    /// local.
    #[test]
    fn the_uac_prompt_or_a_refused_process_is_above_the_dock() {
        use crate::launcher_sys::ProcessImage::{Path, Refused, Unknown};
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let prompt = w("C:\\Windows\\System32\\consent.exe");
        assert_eq!(front_of(Path(&prompt)), Front::Above, "the UAC prompt");
        assert_eq!(front_of(Refused), Front::Above, "a process the dock may not look at");
        for remote in ["C:\\Windows\\System32\\mstsc.exe", "C:\\Windows\\System32\\vmconnect.exe"] {
            assert_eq!(front_of(Path(&w(remote))), Front::Remote, "{remote}");
        }
        for local in ["C:\\Windows\\System32\\notepad.exe", "C:\\Windows\\explorer.exe", ""] {
            assert_eq!(front_of(Path(&w(local))), Front::Local, "{local}");
        }
        assert_eq!(front_of(Unknown), Front::Local, "no window, the dock's own, a process gone");
    }

    /// A window above the dock coming to the front and leaving it are cut-offs: Win+Shift+3 starts an app that needs
    /// elevation, the UAC prompt comes up without the secure desktop while Win and Shift are held, their releases and
    /// whatever is typed in the prompt never reach the hook, and "2" typed in the app afterwards must not be Win+2. A
    /// remote client leaving the front is one too; a remote client coming to the front, or no change, is none.
    #[test]
    fn a_window_above_the_dock_coming_or_leaving_is_a_cut_off() {
        use hook::CutOff;
        use Front::{Above, Local, Remote};
        for (was, now) in [(Local, Above), (Above, Local), (Above, Remote), (Remote, Above)] {
            assert_eq!(front_cut_off(was, now), Some(CutOff::WindowAbove), "{was:?} -> {now:?}");
        }
        assert_eq!(front_cut_off(Remote, Local), Some(CutOff::RemoteClient));
        for (was, now) in [(Local, Local), (Local, Remote), (Above, Above), (Remote, Remote)] {
            assert_eq!(front_cut_off(was, now), None, "{was:?} -> {now:?}");
        }
    }

    #[test]
    fn key_focus_needed() {
        assert!(needs_focus(true, false));
        assert!(!needs_focus(true, true));
        assert!(!needs_focus(false, false));
        assert!(!needs_focus(false, true));
    }

    #[test]
    fn chord_stale_window() {
        assert!(!chord_stale(5000, 5999));
        assert!(!chord_stale(5000, 6000), "exactly CHORD_STALE_MS is still fresh");
        assert!(chord_stale(5000, 6001));
        assert!(!chord_stale(5000, 5000));
        // Both clocks wrap at u32::MAX.
        assert!(!chord_stale(u32::MAX - 100, 898), "999 ms across the wrap");
        assert!(chord_stale(u32::MAX - 100, 900), "1001 ms across the wrap");
    }

    /// A made-up bar: Start, three apps, a tray icon, the chevron, two modules.
    fn kb_bar() -> Vec<ItemId> {
        vec![
            ItemId::Start,
            ItemId::App(0),
            ItemId::App(1),
            ItemId::App(2),
            ItemId::Tray(9),
            ItemId::TrayOverflow,
            ItemId::Volume,
            ItemId::Clock,
        ]
    }

    #[test]
    fn nth_app_counts_apps_only() {
        let b = kb_bar();
        assert_eq!(nth_app(&b, 0), Some(ItemId::App(0)));
        assert_eq!(nth_app(&b, 1), Some(ItemId::App(1)));
        assert_eq!(nth_app(&b, 2), Some(ItemId::App(2)));
        assert_eq!(nth_app(&b, 3), None, "beyond the count nothing happens");
        assert_eq!(nth_app(&b, 9), None);
        assert_eq!(nth_app(&[ItemId::Start, ItemId::Clock], 0), None);
    }

    #[test]
    fn chord_focus_targets() {
        use hook::ChordAction as C;
        let b = kb_bar();
        assert_eq!(chord_focus(&b, C::FocusFirst), Some(1), "the first app");
        assert_eq!(chord_focus(&b, C::FocusLast), Some(3), "the last app");
        assert_eq!(chord_focus(&b, C::FocusTray), Some(4), "the first tray icon");
        let no_tray = [ItemId::Start, ItemId::App(0), ItemId::Volume, ItemId::Clock];
        assert_eq!(chord_focus(&no_tray, C::FocusTray), Some(2), "no tray: the first module");
        let chevron = [ItemId::App(0), ItemId::TrayOverflow, ItemId::Clock];
        assert_eq!(chord_focus(&chevron, C::FocusTray), Some(1), "the chevron is in the tray zone");
        let no_apps = [ItemId::Start, ItemId::Clock];
        assert_eq!(chord_focus(&no_apps, C::FocusFirst), Some(0));
        assert_eq!(chord_focus(&no_apps, C::FocusLast), Some(1));
        assert_eq!(chord_focus(&[], C::FocusFirst), None);
        assert_eq!(chord_focus(&b, C::App(0)), None);
    }

    #[test]
    fn peek_release_hides_only_what_peek_showed() {
        assert!(peek_release(true, false, false));
        assert!(!peek_release(false, false, false), "the bar was already shown: it stays");
        assert!(!peek_release(true, true, false), "the pointer is over the bar");
        assert!(!peek_release(true, false, true), "the keyboard is on the bar");
        assert!(!peek_release(false, true, true));
    }

    #[test]
    fn kb_active_needs_a_focused_item() {
        assert!(kb_active_of(true, true));
        assert!(!kb_active_of(false, true), "the bar in front without a focused item (a lone Win tap)");
        assert!(!kb_active_of(true, false), "another window in front");
        assert!(!kb_active_of(false, false));
    }

    /// A made-up layout of 30 px items: Start, pinned A B, running C D E, tray icons x y, the Volume and Clock modules.
    fn zone_layout() -> Vec<ItemRect> {
        let ids = [
            ItemId::Start,
            ItemId::App(0),
            ItemId::App(1),
            ItemId::App(2),
            ItemId::App(3),
            ItemId::App(4),
            ItemId::Tray(11),
            ItemId::Tray(12),
            ItemId::Volume,
            ItemId::Clock,
        ];
        ids.iter()
            .enumerate()
            .map(|(k, &id)| ItemRect {
                id,
                rect: RECT { left: 30 * k as i32, top: 0, right: 30 * k as i32 + 30, bottom: 40 },
            })
            .collect()
    }

    fn zone_pinned(id: ItemId) -> Option<bool> {
        match id {
            ItemId::App(i) => [true, true, false, false, false].get(i).copied(),
            _ => Some(false),
        }
    }

    fn zone_key(id: ItemId) -> Option<String> {
        match id {
            ItemId::App(i) => ["A", "B", "C", "D", "E"].get(i).map(|s| s.to_string()),
            ItemId::Tray(11) => Some("x".into()),
            ItemId::Tray(12) => Some("y".into()),
            other => render::module_name(other).map(str::to_string),
        }
    }

    #[test]
    fn zone_run_splits_pinned_and_running() {
        let items = zone_layout();
        let run = |idx| zone_run(&items, idx, zone_pinned, zone_key);
        let c = run(3).expect("C has a run");
        assert_eq!((c.zone, c.pinned, c.first, c.from), (Zone::Apps, false, 3, 0));
        assert_eq!(c.keys, ["C", "D", "E"]);
        assert_eq!(c.ids, [ItemId::App(2), ItemId::App(3), ItemId::App(4)]);
        assert_eq!(c.widths, [30, 30, 30]);
        assert_eq!(run(5).expect("E").from, 2);
        let a = run(1).expect("A has a run");
        assert_eq!((a.zone, a.pinned, a.first, a.from), (Zone::Apps, true, 1, 0));
        assert_eq!(a.keys, ["A", "B"]);
        assert_eq!(run(2).expect("B").from, 1);
        let x = run(6).expect("x has a run");
        assert_eq!((x.zone, x.first, x.from), (Zone::Tray, 6, 0));
        assert_eq!(x.keys, ["x", "y"]);
        let v = run(9).expect("the modules have a run");
        assert_eq!((v.zone, v.first, v.from, v.ids.len()), (Zone::Modules, 8, 1, 2));
        assert!(run(0).is_none(), "Start does not move");
        assert!(run(99).is_none(), "an index out of range");
        // A lone pinned app between Start and the running apps.
        let lone: Vec<ItemRect> = items.iter().filter(|i| i.id != ItemId::App(1)).copied().collect();
        assert!(zone_run(&lone, 1, zone_pinned, zone_key).is_none(), "a run of one");
        // An item without a key stops the run.
        assert!(zone_run(&items, 3, zone_pinned, |id| if id == ItemId::App(4) { None } else { zone_key(id) }).is_none());
    }

    #[test]
    fn reordered_matches_remove_insert() {
        let keys: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        for from in 0..4 {
            for slot in 0..4 {
                let mut k = keys.to_vec();
                let m = k.remove(from);
                k.insert(slot, m);
                assert_eq!(reordered(&keys, from, slot), k, "{from} -> {slot}");
            }
        }
        assert_eq!(reordered(&keys, 4, 0), keys, "from out of range: unchanged");
        assert_eq!(reordered(&keys, 0, 4), keys, "slot out of range: unchanged");
    }

    #[test]
    fn syskey_without_alt_is_a_key() {
        const ALT_BIT: isize = 1 << 29;
        assert_eq!(as_key(WM_KEYDOWN, 0), Some(false));
        assert_eq!(as_key(WM_SYSKEYDOWN, 0), Some(false), "an active window without the focus");
        assert_eq!(as_key(WM_SYSKEYDOWN, 0x0001_0001), Some(false), "repeat count and scan code do not matter");
        assert_eq!(as_key(WM_SYSKEYDOWN, ALT_BIT), Some(true), "Alt held: DefWindowProc keeps Alt+F4");
        assert_eq!(as_key(WM_SYSKEYDOWN, ALT_BIT | 0x0038_0001), Some(true));
        assert_eq!(as_key(WM_TIMER, 0), None);
        assert_eq!(as_key(WM_MOUSEMOVE, ALT_BIT), None);
    }

    #[test]
    fn focus_follows_rebuild() {
        let keys = ["contoso.exe", "Example.Chat", "fabrikam.exe"];
        assert_eq!(refind(&keys, "example.chat"), Some(1), "the key, ignoring ASCII case");
        assert_eq!(refind(&keys, "fabrikam.exe"), Some(2));
        assert_eq!(refind(&keys[..2], "fabrikam.exe"), None, "the app is gone");
        assert_eq!(refind(&[], "contoso.exe"), None);
    }

    #[test]
    fn keyboard_gesture_of_each_key() {
        use bar_keys::Key;
        assert_eq!(keyboard_gesture(Key::Activate), Some(Gesture::Click));
        assert_eq!(keyboard_gesture(Key::Middle), Some(Gesture::Middle));
        assert_eq!(keyboard_gesture(Key::Context), Some(Gesture::Right));
        for k in [Key::Next, Key::Interior, Key::Escape, Key::MoveLeft, Key::Letter('a')] {
            assert_eq!(keyboard_gesture(k), None, "{k:?}");
        }
    }

    #[test]
    fn tray_point_is_the_item_centre() {
        let r = RECT { left: 100, top: 4, right: 126, bottom: 44 };
        assert_eq!(item_centre(r, POINT { x: 500, y: 1000 }), POINT { x: 613, y: 1024 });
        assert_eq!(item_centre(r, POINT::default()), POINT { x: 113, y: 24 });
        let odd = RECT { left: 0, top: 0, right: 27, bottom: 41 };
        let left = POINT { x: -1920, y: 0 };
        assert_eq!(item_centre(odd, left), POINT { x: -1907, y: 20 }, "a display left of the primary");
    }

    #[test]
    fn interior_target() {
        assert_eq!(interior(Target::App, 2), Interior::Picker);
        assert_eq!(interior(Target::App, 5), Interior::Picker);
        assert_eq!(interior(Target::App, 1), Interior::AppMenu);
        assert_eq!(interior(Target::App, 0), Interior::AppMenu, "a pin without windows");
        for t in [
            Target::Start,
            Target::Volume,
            Target::Network,
            Target::Battery,
            Target::Notifications,
            Target::Clock,
            Target::Power,
            Target::TrayOverflow,
        ] {
            assert_eq!(interior(t, 0), Interior::Popup, "{t:?}");
        }
        for t in [Target::Metrics, Target::ShowDesktop, Target::Tray, Target::Empty] {
            assert_eq!(interior(t, 3), Interior::Nothing, "{t:?}");
        }
    }

    #[test]
    fn keyboard_reorder_moves_one_slot() {
        assert_eq!(reorder_slot(0, 3, false), None, "the first item does not wrap left");
        assert_eq!(reorder_slot(0, 3, true), Some(1));
        assert_eq!(reorder_slot(1, 3, false), Some(0));
        assert_eq!(reorder_slot(1, 3, true), Some(2));
        assert_eq!(reorder_slot(2, 3, false), Some(1));
        assert_eq!(reorder_slot(2, 3, true), None, "the last item does not wrap right");
        assert_eq!(reorder_slot(3, 3, false), None, "out of the run");
        assert_eq!(reorder_slot(0, 0, true), None);
        // The order the move persists: one place, as a drop there would.
        let keys: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(reordered(&keys, 0, reorder_slot(0, 3, true).unwrap_or(0)), ["b", "a", "c"]);
        assert_eq!(reordered(&keys, 2, reorder_slot(2, 3, false).unwrap_or(2)), ["a", "c", "b"]);
    }

    #[test]
    fn esc_plan() {
        assert_eq!(esc_action(true), EscAction::Hide);
        assert_eq!(esc_action(false), EscAction::RestoreForeground);
    }

    #[test]
    fn syschar_without_alt_types() {
        const ALT_BIT: isize = 1 << 29;
        assert!(as_char(WM_CHAR, 0));
        assert!(as_char(WM_SYSCHAR, 0x0001_0001), "an active window without the focus");
        assert!(!as_char(WM_SYSCHAR, ALT_BIT | 1), "Alt+letter stays a menu key");
        assert!(!as_char(WM_KEYDOWN, 0));
        assert!(!as_char(WM_SYSKEYDOWN, 0));
    }

    #[test]
    fn letters_wait_for_a_deliberate_keyboard() {
        use KbEvent::*;
        // Letters typed after `events`, from a hidden bar.
        let letters_after =
            |events: &[KbEvent]| letters_act(events.iter().fold(Kb::EMPTY.deliberate, |_, e| kb_after(*e)));
        assert!(!letters_after(&[]), "a hidden bar starts undecided");
        assert!(!letters_after(&[WinTap]), "after a lone Win tap typing is not type-ahead");
        assert!(letters_after(&[Chord]), "after Win+T, Win+Shift+T, Win+B or Win+Alt+n");
        assert!(letters_after(&[Nav]), "after Prev, Next, Home, End or a zone key");
        assert!(letters_after(&[WinTap, Chord]), "a chord after the tap arms them");
        assert!(!letters_after(&[Chord, Reset]), "Esc, hide and idle disarm them");
        assert!(!letters_after(&[Nav, WinTap]), "a new Win tap starts undecided again");
        assert_eq!(Kb::EMPTY.deliberate, kb_after(Reset), "the reset state is the disarmed one");
    }

    #[test]
    fn keyboard_left_alone_ends() {
        assert!(!kb_idle(1000 + KB_IDLE_MS, 1000), "within the idle time");
        assert!(kb_idle(1001 + KB_IDLE_MS, 1000), "no key for a few seconds");
        const { assert!(KB_IDLE_MS > bar_keys::TYPE_AHEAD_MS, "a typed word is never cut short") };
    }

    #[test]
    fn a_popup_visit_is_not_idle_time() {
        // Win+T, End, Enter: the power popup is read for a long while, then Esc returns to the bar.
        let mut last_key = 1000;
        let mut now = 1000;
        while now < 1000 + 5 * KB_IDLE_MS {
            now += 50;
            last_key = kb_clock(now, last_key, true); // the popup is open at every poll
        }
        now += 50; // the first poll after the popup closed
        assert!(!kb_idle(now, last_key), "the keyboard layer survives the return from a long visit");
        let mut left = last_key;
        for _ in 0..(KB_IDLE_MS / 50 + 2) {
            now += 50;
            left = kb_clock(now, left, false);
        }
        assert!(kb_idle(now, left), "left alone afterwards it still ends");
    }

    #[test]
    fn the_keyboard_ends_with_auto_hide_off_too() {
        // Win+T, then left alone: the idle check runs with or without auto-hide.
        for autohide in [true, false] {
            assert_eq!(kb_poll(false, autohide, true), KbPoll::CheckIdle { restore: !autohide });
            assert_eq!(kb_poll(true, autohide, true), KbPoll::Engaged, "the pointer or a popup holds the clock");
            assert_eq!(kb_poll(false, autohide, false), KbPoll::Nothing, "no keyboard layer, nothing to end");
        }
        assert_eq!(esc_action(false), EscAction::RestoreForeground, "the foreground goes back as Esc does it");
    }

    #[test]
    fn the_idle_end_does_not_need_the_ring() {
        // A pointer crossing the bar hides the ring; focus and the armed letters stay, so the idle end must still run.
        assert!(kb_held_of(true, false, true), "focus kept, ring off");
        assert!(kb_held_of(false, true, true), "armed letters without a focus");
        assert!(!kb_held_of(true, true, false), "another window in front");
        assert!(!kb_held_of(false, false, true), "a lone Win tap");
        // The decision the poll runs, from the raw state: ring off, focus kept, auto-hide off, not engaged.
        assert_eq!(kb_poll_of(false, false, true, false, true), KbPoll::CheckIdle { restore: true });
        assert_eq!(kb_poll_of(false, true, true, false, true), KbPoll::CheckIdle { restore: false });
        assert_eq!(kb_poll_of(false, false, false, true, true), KbPoll::CheckIdle { restore: true }, "armed letters");
        assert_eq!(kb_poll_of(true, false, true, false, true), KbPoll::Engaged);
        assert_eq!(kb_poll_of(false, false, true, true, false), KbPoll::Nothing, "another window in front");
        assert_eq!(kb_poll_of(false, false, false, false, true), KbPoll::Nothing, "a lone Win tap");
        // The idle clock follows the same armed test.
        assert!(kb_armed(true, false) && kb_armed(false, true) && !kb_armed(false, false));
    }

    #[test]
    fn close_all_needs_shift_at_the_press() {
        let key = || Some("app".to_string());
        assert_eq!(press_close_key(true, key()), key());
        assert_eq!(press_close_key(false, key()), None, "a plain middle press closes nothing at its release");
        assert_eq!(press_close_key(true, None), None);
        // The release then closes only the stored group.
        assert_eq!(close_key(None, Some((0, &["app"]))), None);
    }

    #[test]
    fn typed_word_lives_one_pause() {
        assert!(typing_live("c", 1000, 1000 + bar_keys::TYPE_AHEAD_MS));
        assert!(!typing_live("c", 1000, 1001 + bar_keys::TYPE_AHEAD_MS), "a pause starts a new word");
        assert!(!typing_live("", 1000, 1000), "no word");
    }

    #[test]
    fn focus_survives_only_our_windows() {
        assert!(kb_keeps(true, true), "the bar");
        assert!(kb_keeps(false, true), "one of our popups");
        assert!(!kb_keeps(false, false), "any other window, the Settings window included");
    }

    #[test]
    fn esc_in_a_popup_returns_to_the_bar() {
        assert!(back_to_bar(true, true, true));
        assert!(!back_to_bar(true, false, true), "closed by a click elsewhere or by its action");
        assert!(!back_to_bar(true, true, false), "another window has the foreground: never taken back");
        assert!(!back_to_bar(false, true, true), "the keyboard was not on the bar");
    }

    #[test]
    fn ring_ends_only_when_the_pointer_moves() {
        let p = |x, y| Some(POINT { x, y });
        assert!(!pointer_moved(p(10, 20), p(10, 20)), "a WM_MOUSEMOVE without motion (the bar slid in)");
        assert!(pointer_moved(p(10, 20), p(11, 20)));
        assert!(pointer_moved(None, p(0, 0)), "no position at the ring: the first move ends it");
        assert!(!pointer_moved(p(10, 20), None), "an unreadable pointer is no motion");
    }

    #[test]
    fn keyboard_keeps_the_key_show_time() {
        assert_eq!(kb_hide_at(1000, 0), 3500, "a hide delay of 0 never hides a bar the keyboard just left");
        assert_eq!(kb_hide_at(1000, 1000), 3500);
        assert_eq!(kb_hide_at(1000, 5000), 6000);
    }

    #[test]
    fn held_keys_repeat_only_focus_moves() {
        use bar_keys::Key;
        for k in [Key::Prev, Key::Next, Key::Home, Key::End, Key::NextZone, Key::PrevZone] {
            assert!(repeat_acts(Some(k), false), "{k:?}");
        }
        for k in [
            Key::Activate,
            Key::Middle,
            Key::Context,
            Key::Interior,
            Key::MoveLeft,
            Key::MoveRight,
            Key::Escape,
            Key::Letter('a'),
        ] {
            assert!(!repeat_acts(Some(k), false), "{k:?} acts once");
        }
        assert!(repeat_acts(None, true), "the picker's arrows");
        assert!(!repeat_acts(None, false), "the picker's Enter, Delete and Esc");
    }

    #[test]
    fn dwell_reveals_after_the_show_delay() {
        assert!(!dwell_done(0, 1000, 0), "not on the edge");
        assert!(dwell_done(1000, 1000, 0), "delay 0 shows at once");
        assert!(!dwell_done(1000, 1149, 150));
        assert!(dwell_done(1000, 1150, 150));
    }

    #[test]
    fn hide_at_follows_the_delay() {
        assert_eq!(hide_at(5000, 0), 5000);
        assert_eq!(hide_at(5000, 1000), 6000);
    }

    #[test]
    fn win_show_ignores_a_zero_hide_delay() {
        assert_eq!(key_show_ms(0), 2500);
        assert_eq!(key_show_ms(1000), 2500);
        assert_eq!(key_show_ms(5000), 5000);
    }

    #[test]
    fn edge_show_outlasts_a_zero_hide_delay() {
        assert_eq!(key_show_ms(0), 2500);
        assert_eq!(key_show_ms(1000), 2500);
        assert_eq!(key_show_ms(5000), 5000);
    }

    #[test]
    fn empty_space_is_inside_the_bar_only() {
        let bar = RECT { left: 100, top: 1000, right: 900, bottom: 1060 };
        let p = |x, y| POINT { x, y };
        assert!(empty_hit(bar, p(500, 1030), None));
        assert!(empty_hit(bar, p(100, 1000), None));
        assert!(!empty_hit(bar, p(900, 1030), None), "right edge is outside");
        assert!(!empty_hit(bar, p(500, 1060), None), "bottom edge is outside");
        assert!(!empty_hit(bar, p(99, 1030), None));
        assert!(!empty_hit(bar, p(500, 999), None));
        assert!(!empty_hit(bar, p(500, 1030), Some(ItemId::Volume)), "an item is not empty space");
        assert!(!empty_hit(bar, p(500, 1030), Some(ItemId::App(0))));
    }

    #[test]
    fn shift_middle_off_an_app_does_nothing() {
        assert_eq!(resolve(Target::Empty, Gesture::ShiftMiddle), Action::None);
        for t in [Target::Volume, Target::Tray, Target::Start, Target::Metrics, Target::Battery] {
            let a = resolve(t, Gesture::ShiftMiddle);
            assert_ne!(a, Action::CloseGroup, "{t:?}");
            assert_eq!(a, resolve(t, Gesture::Middle), "{t:?}");
        }
        assert_eq!(resolve(Target::App, Gesture::ShiftMiddle), Action::CloseGroup);
        // Outside the bar there is no target at all; inside it with no item the target is Empty.
        GEOM.with(|g| g.set(Some(Geom { x: 100, y: 1000, w: 800, h: 60, mon: RECT::default() })));
        assert_eq!(gesture_target(None, 10, 10), Some(Target::Empty));
        assert_eq!(gesture_target(None, -1, 10), None);
        assert_eq!(gesture_target(None, 800, 10), None);
        assert_eq!(gesture_target(None, 10, 60), None);
        assert_eq!(gesture_target(Some(ItemId::Volume), 10, 10), Some(Target::Volume));
    }

    #[test]
    fn close_group_resolves_at_the_release() {
        let keys = ["a", "b", "c"];
        assert_eq!(close_key(Some("b"), Some((1, &keys))), Some("b"));
        assert_eq!(close_key(Some("a"), Some((1, &keys))), None, "another group under the pointer");
        assert_eq!(close_key(Some("c"), Some((3, &keys))), None, "index out of range");
        assert_eq!(close_key(None, Some((0, &keys))), None, "no press");
        assert_eq!(close_key(Some("a"), None), None, "no item at the release");
        assert_eq!(close_key(Some("a"), Some((0, &[]))), None, "the list emptied");
    }

    #[test]
    fn cycle_rate_limit() {
        assert!(cycle_due(0, CYCLE_STEP_MS));
        assert!(!cycle_due(1000, 1000 + CYCLE_STEP_MS - 1));
        assert!(cycle_due(1000, 1000 + CYCLE_STEP_MS));
        assert!(!cycle_due(1000, 900), "a clock that went back never fires twice");
    }

    #[test]
    fn target_of_maps_every_item() {
        let cases = [
            (ItemId::App(0), Target::App),
            (ItemId::App(7), Target::App),
            (ItemId::Tray(3), Target::Tray),
            (ItemId::TrayOverflow, Target::TrayOverflow),
            (ItemId::Notifications, Target::Notifications),
            (ItemId::Volume, Target::Volume),
            (ItemId::Clock, Target::Clock),
            (ItemId::Start, Target::Start),
            (ItemId::ShowDesktop, Target::ShowDesktop),
            (ItemId::Power, Target::Power),
            (ItemId::Network, Target::Network),
            (ItemId::Metrics, Target::Metrics),
            (ItemId::Battery, Target::Battery),
        ];
        for (id, t) in cases {
            assert_eq!(target_of(Some(id)), t, "{id:?}");
        }
        assert_eq!(target_of(None), Target::Empty);
    }

    #[test]
    fn push_reveals_at_the_threshold() {
        let mut p = PushSum::default();
        for i in 0..3 {
            let (n, hit) = push_step(p, 5, 10_000 + i * 10, 20);
            assert!(!hit, "sum {} is under 20", n.sum);
            p = n;
        }
        let (n, hit) = push_step(p, 5, 10_030, 20);
        assert!(hit);
        assert_eq!(n, PushSum::default(), "the sum resets after a reveal");
    }

    #[test]
    fn push_window_is_one_second() {
        let (p, _) = push_step(PushSum::default(), 10, 5_000, 30);
        assert_eq!(p.sum, 10);
        // Still inside the window at exactly 1000 ms.
        let (q, hit) = push_step(p, 20, 6_000, 30);
        assert!(hit, "10 + 20 reaches 30 inside the window");
        assert_eq!(q.sum, 0);
        // One ms later the window restarts: 20 alone is not enough.
        let (r, hit) = push_step(p, 20, 6_001, 30);
        assert!(!hit);
        assert_eq!((r.sum, r.since), (20, 6_001));
    }

    #[test]
    fn moving_away_resets() {
        let (p, _) = push_step(PushSum::default(), 15, 5_000, 30);
        let (p, hit) = push_step(p, -1, 5_010, 30);
        assert!(!hit);
        assert_eq!(p.sum, 0);
        let (_, hit) = push_step(p, 15, 5_020, 30);
        assert!(!hit, "the earlier 15 no longer counts");
        // A move along the edge row (dy 0) neither adds nor resets.
        let (p, _) = push_step(PushSum::default(), 10, 5_000, 30);
        let (p, _) = push_step(p, 0, 5_010, 30);
        assert_eq!(p.sum, 10);
    }

    #[test]
    fn top_bar_pushes_up() {
        assert_eq!(towards_edge(-5, true), 5);
        assert_eq!(towards_edge(5, true), -5);
        assert_eq!(towards_edge(5, false), 5);
        assert_eq!(towards_edge(-5, false), -5);
        assert_eq!(towards_edge(i32::MIN, true), i32::MAX);
    }

    fn wi(visible: bool, still_ms: u64) -> WatchInputs {
        WatchInputs { visible, gate_ok: true, dwell: false, requests: false, drag: false, still_ms, push_edge: false }
    }

    #[test]
    fn edge_watch_with_push() {
        let pe = |visible, still_ms| WatchInputs { push_edge: true, ..wi(visible, still_ms) };
        assert_eq!(edge_watch(pe(false, 0)), EdgeWatch::PollRaw);
        assert_eq!(edge_watch(pe(false, 999)), EdgeWatch::PollRaw);
        assert_eq!(edge_watch(pe(false, 1000)), EdgeWatch::Motion);
        assert_eq!(edge_watch(pe(false, 60_000)), EdgeWatch::Motion);
        // A shown bar, a failed gate, a drag or pending requests keep the plain poll.
        assert_eq!(edge_watch(pe(true, 5000)), EdgeWatch::Poll);
        assert_eq!(edge_watch(WatchInputs { gate_ok: false, ..pe(false, 5000) }), EdgeWatch::Poll);
        assert_eq!(edge_watch(WatchInputs { drag: true, ..pe(false, 5000) }), EdgeWatch::Poll);
        assert_eq!(edge_watch(WatchInputs { requests: true, ..pe(false, 5000) }), EdgeWatch::Poll);
        // Without push_edge: today's table.
        assert_eq!(edge_watch(wi(false, 5000)), EdgeWatch::Motion);
        assert_eq!(edge_watch(wi(false, 999)), EdgeWatch::Poll);
    }

    #[test]
    fn only_relative_mouse_reports_push() {
        assert_eq!(push_delta(0, 0, -7), Some(-7), "a relative mouse report");
        assert_eq!(push_delta(0, 1, 5), None, "MOUSE_MOVE_ABSOLUTE");
        assert_eq!(push_delta(0, 3, 5), None, "absolute over the virtual desktop");
        assert_eq!(push_delta(0, 4, 5), Some(5), "other flag bits do not matter");
        assert_eq!(push_delta(1, 0, 5), None, "a keyboard report");
        assert_eq!(push_delta(2, 0, 5), None, "a HID report");
    }

    #[test]
    fn raw_poll_is_throttled() {
        assert!(!raw_poll_due(1000, 1049));
        assert!(raw_poll_due(1000, 1050));
        assert!(raw_poll_due(0, 12_345), "the first message runs the poll");
        assert!(!raw_poll_due(2000, 1000), "a clock that went back does not run it");
    }

    #[test]
    fn the_edge_watch_starts_with_nothing_armed() {
        assert!(!POLL_ON.get());
        assert!(!RAW_ON.get());
    }

    #[test]
    fn a_hidden_open_refreshes_a_stale_list() {
        assert!(refresh_due(false, 0, 5000));
        assert!(!refresh_due(false, 1000, 5999));
        assert!(!refresh_due(true, 0, 60_000));
        assert!(!refresh_due(false, 10, 5));
    }

    #[test]
    fn the_blink_runs_only_while_shown() {
        assert!(attn_timer_wanted(true, true, 0));
        assert!(attn_timer_wanted(true, true, ATTN_BLINK_MS - 1));
        assert!(!attn_timer_wanted(true, true, ATTN_BLINK_MS));
        assert!(!attn_timer_wanted(false, true, 0));
        assert!(!attn_timer_wanted(true, false, 0));
    }

    #[test]
    fn reconcile_only_while_shown() {
        assert!(reconcile_wanted(true));
        assert!(!reconcile_wanted(false));
    }

    #[test]
    fn a_shown_bar_polls() {
        for ms in [0, 1000, 60_000] {
            assert_eq!(edge_watch(wi(true, ms)), EdgeWatch::Poll);
        }
    }

    #[test]
    fn a_still_hidden_pointer_waits_for_motion() {
        assert_eq!(edge_watch(wi(false, 1000)), EdgeWatch::Motion);
        assert_eq!(edge_watch(wi(false, 999)), EdgeWatch::Poll);
    }

    #[test]
    fn a_dwell_in_progress_keeps_polling() {
        assert_eq!(edge_watch(WatchInputs { dwell: true, ..wi(false, 5000) }), EdgeWatch::Poll);
    }

    #[test]
    fn pending_icon_requests_keep_polling() {
        assert_eq!(edge_watch(WatchInputs { requests: true, ..wi(false, 5000) }), EdgeWatch::Poll);
    }

    #[test]
    fn without_raw_input_it_polls_as_today() {
        assert_eq!(edge_watch(WatchInputs { gate_ok: false, ..wi(false, 60_000) }), EdgeWatch::Poll);
    }

    #[test]
    fn a_drag_keeps_polling() {
        assert_eq!(edge_watch(WatchInputs { drag: true, ..wi(false, 5000) }), EdgeWatch::Poll);
    }

    #[test]
    fn stillness_restarts_on_any_move() {
        let a = POINT { x: 5, y: 6 };
        let b = POINT { x: 5, y: 7 };
        assert_eq!(still_since(None, Some(a), 10, 50), 50);
        assert_eq!(still_since(Some(a), Some(b), 10, 50), 50);
        assert_eq!(still_since(Some(a), Some(a), 10, 50), 10);
    }

    #[test]
    fn an_unreadable_cursor_counts_as_still() {
        let a = POINT { x: 5, y: 6 };
        assert_eq!(still_since(Some(a), None, 10, 50), 10);
        assert_eq!(still_since(None, None, 10, 50), 10);
    }

    #[test]
    fn a_failed_registration_is_retried_at_the_next_show() {
        let ok = gate_after(true, GateEvent::RegisterFailed);
        assert!(!ok);
        assert_eq!(gate_after(ok, GateEvent::Shown), MOTION_GATE);
    }

    #[test]
    fn tray_sync_waits_for_explorer() {
        use TraySync::*;
        //                          show   started hidden paused exiting
        assert_eq!(tray_sync_action(true,  false,  true,  false, false), Start);
        assert_eq!(tray_sync_action(true,  false,  true,  true,  false), Nothing,
            "Explorer restarting: no Shell_TrayWnd of ours");
        assert_eq!(tray_sync_action(true,  false,  false, false, false), Nothing, "native taskbar not hidden by us");
        assert_eq!(tray_sync_action(false, true,   true,  false, false), Stop);
        assert_eq!(tray_sync_action(false, true,   true,  true,  false), Stop, "switching off is always allowed");
        assert_eq!(tray_sync_action(false, false,  true,  false, false), Nothing);
        assert_eq!(tray_sync_action(true,  true,   true,  false, false), Nothing);
        assert_eq!(tray_sync_action(true,  false,  true,  false, true),  Nothing, "never started while exiting");
        assert_eq!(tray_sync_action(false, true,   true,  false, true),  Stop, "stopping while exiting is allowed");
    }

    #[test]
    fn win_show_takes_the_foreground() {
        assert!(show_takes_foreground(true, false, false, false, false), "Win over a normal window or a missed game");
        assert!(show_takes_foreground(true, false, true, false, false), "Win over a recognised fullscreen app");
        assert!(show_takes_foreground(false, false, true, false, false), "fullscreen show: as before");
        assert!(!show_takes_foreground(false, false, false, false, false), "screen edge: no focus");
        assert!(!show_takes_foreground(true, false, false, false, true), "--no-foreground wins over Win");
        assert!(!show_takes_foreground(true, false, true, false, true), "--no-foreground wins over fullscreen");
        assert!(show_takes_foreground(false, false, false, true, false), "--foreground always");
        assert!(show_takes_foreground(false, false, false, true, true), "--foreground wins over --no-foreground");
    }

    #[test]
    fn startup_show_takes_the_foreground_only_with_the_flag() {
        // A start (sign-in, the update's restart, a watchdog or task restart) is not a user request.
        assert!(!show_takes_foreground(false, true, true, false, false), "startup over a fullscreen app: no focus");
        assert!(!show_takes_foreground(false, true, false, false, false), "startup over a normal window: no focus");
        assert!(!show_takes_foreground(false, true, true, false, true), "startup with --no-foreground: no focus");
        assert!(show_takes_foreground(false, true, true, true, false), "--foreground: the startup show takes it");
        assert!(show_takes_foreground(false, true, false, true, false), "--foreground over a normal window");
        assert!(show_takes_foreground(false, true, true, true, true), "--foreground wins over --no-foreground");
        // The startup show reads no fullscreen app (0) and does not take the foreground: PREV_FG is kept.
        assert_eq!(prev_to_remember(0, 9, false, 0), 0, "startup: nothing to give back");
        assert_eq!(prev_to_remember(0, 9, false, 5), 5, "startup: PREV_FG not changed");
    }

    #[test]
    fn remembered_window_on_show() {
        assert_eq!(prev_to_remember(7, 9, true, 0), 7, "the fullscreen app first");
        assert_eq!(prev_to_remember(7, 9, false, 0), 7, "fullscreen app kept even with --no-foreground");
        assert_eq!(prev_to_remember(0, 9, true, 0), 9, "Win over a normal window");
        assert_eq!(prev_to_remember(0, 9, false, 0), 0, "edge show: nothing to give back");
        assert_eq!(prev_to_remember(0, 0, true, 5), 5, "foreground ours (linked Start reversed): kept");
        assert_eq!(prev_to_remember(0, 0, false, 0), 0);
    }

    #[test]
    fn effective_foreground_choice() {
        assert_eq!(effective_fg(3, true, 9, true), 9, "bar focused by Win: the previous window");
        assert_eq!(effective_fg(3, false, 9, true), 3, "foreign foreground: itself");
        assert_eq!(effective_fg(3, true, 9, false), 3, "previous window closed");
        assert_eq!(effective_fg(3, true, 0, false), 3, "nothing remembered");
        assert_eq!(effective_fg(0, false, 9, true), 0, "null foreground: not ours");
    }

    #[test]
    fn settings_window_is_not_a_dock_surface() {
        assert!(dock_surface(true, false), "bar, popups, picker, tray host");
        assert!(!dock_surface(true, true), "the Settings window is treated like an app window");
        assert!(!dock_surface(false, false), "another process");
    }

    #[test]
    fn slot_items_follow_the_drop_slot() {
        let order = |from, slot| (0..4).map(|p| slot_item(p, from, slot)).collect::<Vec<_>>();
        assert_eq!(order(1, 3), [0, 2, 3, 1]);
        assert_eq!(order(1, 0), [1, 0, 2, 3]);
        assert_eq!(order(2, 2), [0, 1, 2, 3]);
        assert_eq!(order(0, 3), [1, 2, 3, 0]);
    }

    #[test]
    fn battery_popup_closes_when_the_module_leaves_the_bar() {
        use crate::popup::PopupKind::{Audio, Battery};
        assert!(battery_popup_should_close(&[ItemId::Volume], Some(Battery))); // module switched off or battery gone
        assert!(!battery_popup_should_close(&[ItemId::Volume, ItemId::Battery], Some(Battery)));
        assert!(!battery_popup_should_close(&[ItemId::Volume], Some(Audio))); // another popup is not ours to close
        assert!(!battery_popup_should_close(&[], None));
    }

    #[test]
    fn next_battery_follows_show_and_visibility() {
        use crate::battery::BatteryStatus;
        use crate::config::BatteryShow::*;
        let b = BatteryStatus::from_raw(0, 1, 64, 600);
        let boom = || -> Option<BatteryStatus> { panic!("a sample was taken") };
        assert_eq!(next_battery(Off, true, b, || Some(b)), BatteryStatus::NONE); // switched off: nothing stale is drawn
        assert_eq!(next_battery(Off, false, b, boom), BatteryStatus::NONE); // ... and nothing is sampled
        assert_eq!(next_battery(Auto, false, b, boom), b); // bar hidden: no sample, last value kept
        assert_eq!(next_battery(Auto, true, BatteryStatus::NONE, || Some(b)), b);
        assert_eq!(next_battery(On, true, b, || None), b); // failed or unknown sample: last value kept
        assert!(next_battery(Auto, true, b, || None).present); // ... so the module and its timer stay
        assert_eq!(next_battery(Auto, true, BatteryStatus::NONE, || None), BatteryStatus::NONE); // desktop stays empty
        assert_eq!(next_battery(Auto, true, b, || Some(BatteryStatus::NONE)), BatteryStatus::NONE); // battery removed
    }

    #[test]
    fn hidden_modules_keep_their_slots() {
        let old: Vec<String> = ["notifications", "volume", "clock", "show_desktop", "power"].map(String::from).to_vec();
        // Old 5-module config: network / metrics come back next to their default neighbours.
        let full = render::modules_from_names(&old);
        assert_eq!(full.len(), 8);
        assert_eq!(full[0], ItemId::Metrics);
        assert_eq!(full[2], ItemId::Network);
        // A "start" written by an earlier version is ignored: the Start button is not a module any more.
        let with_start: Vec<String> = std::iter::once("start".to_string()).chain(old.iter().cloned()).collect();
        assert_eq!(render::modules_from_names(&with_start), full);
        // Drag among the visible ones (metrics hidden): power moved first.
        let vis: Vec<String> =
            ["power", "notifications", "network", "volume", "clock", "show_desktop"].map(String::from).to_vec();
        let out = reorder_visible_modules(&old, &vis);
        assert_eq!(out, ["metrics", "power", "notifications", "network", "battery", "volume", "clock", "show_desktop"]);
    }

    #[test]
    fn merge_order_keeps_unseen_keys_and_caps() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(merge_order(&s(&["b", "a"]), &s(&["a", "z", "b"]), 64), s(&["b", "a", "z"]));
        assert_eq!(merge_order(&s(&["b", "a"]), &s(&["z"]), 2), s(&["b", "a"]));
    }

    #[test]
    fn reorder_pinned_keeps_other_slots() {
        let p = |k: &str| PinnedApp { key: k.into(), ..Default::default() };
        let mut c = config::Config { pinned: vec![p("a"), p("x"), p("b"), p("c")], ..Default::default() };
        reorder_pinned(&mut c, &["c".to_string(), "a".to_string(), "b".to_string()]);
        let keys: Vec<&str> = c.pinned.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["c", "x", "a", "b"]);
    }

    #[test]
    fn bounce_grace_window_edges() {
        assert!(!within_grace(1000, 0, 800), "never taken");
        assert!(within_grace(1000, 1000, 800));
        assert!(within_grace(1799, 1000, 800));
        assert!(!within_grace(1800, 1000, 800));
        assert!(within_grace(900, 1000, 800), "clock before the take saturates to 0");
    }

    #[test]
    fn variant1_joins_only_a_front_thread_that_answers() {
        // A front window that answers, and so does its input queue: the queue is joined, as in 1.4.3.
        assert_eq!(variant1(7, 3, || true, || true), Variant1::Joined);
        // A front window that does not answer (hung, or busy past the ping): never joined (live check V-14), and its
        // queue is not looked at.
        assert_eq!(variant1(7, 3, || false, || unreachable!("no queue look after a silent ping")), Variant1::Skipped);
        // A front window that answers whose queue holds another thread's window that does not (a hung child window of
        // another process, which shares the queue): never joined either.
        assert_eq!(variant1(7, 3, || true, || false), Variant1::QueueSilent);
        // No foreground window, or the dock's own thread in front: nothing to join, and no ping.
        let none = || -> bool { unreachable!("no ping without another thread") };
        assert_eq!(variant1(0, 3, none, none), Variant1::Alone);
        assert_eq!(variant1(3, 3, none, none), Variant1::Alone);
    }

    #[test]
    fn a_third_thread_in_the_front_queue_must_answer() {
        // The focus window of a child of another thread in the foreground queue: pinged before the join.
        assert!(third_thread(9, 7, 3));
        // The foreground thread itself (pinged already), the dock's thread, no window or one gone: not asked.
        assert!(!third_thread(7, 7, 3));
        assert!(!third_thread(3, 7, 3));
        assert!(!third_thread(0, 7, 3));
    }

    #[test]
    fn a_silent_front_thread_is_not_asked_again_for_a_second() {
        let last = (7, 10_000);
        assert!(still_silent(last, 7, 10_000));
        assert!(still_silent(last, 7, 10_000 + SILENT_MS - 1), "the open's retries pay no second ping");
        assert!(!still_silent(last, 7, 10_000 + SILENT_MS), "asked again after SILENT_MS");
        assert!(!still_silent(last, 8, 10_100), "another thread is asked");
        assert!(!still_silent((0, 0), 0, 50), "none yet");
    }

    /// Live check V-14, run B1: while `answers` waited for a thread that had stopped pumping, the UI thread dispatched
    /// the messages another process sent the dock's windows, and each one started the 50 ms again (7.45 s with a sender
    /// every 10 ms; the heartbeat starved and the watchdog gave the taskbar back). Here a second thread's message-only
    /// window never pumps, a third thread sends a message-only window of this thread a message every 5 ms, and the ping
    /// must still end, false, well under 1 s (without SMTO_BLOCK it lasted until the silent thread ended, 4 s).
    /// Headless: message-only windows are never shown and never take the focus.
    #[test]
    fn the_ping_ends_on_time_while_the_dock_is_sent_messages() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_NOREMOVE, SMTO_NORMAL, WINDOW_EX_STYLE,
            WINDOW_STYLE,
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
        // The silent thread: its window exists and it never reads its queue until the test is done (4 s at most).
        let (tx, rx) = std::sync::mpsc::channel();
        let (done, wait_done) = std::sync::mpsc::channel::<()>();
        let silent = std::thread::spawn(move || {
            let h = message_only();
            tx.send(h.0 as isize).expect("the test thread waits");
            let _ = wait_done.recv_timeout(Duration::from_secs(4));
            // SAFETY: the window of this thread.
            let _ = unsafe { DestroyWindow(h) };
        });
        let target = HWND(rx.recv().expect("the silent thread starts") as *mut std::ffi::c_void);
        // A window of this thread, which the sender keeps busy as another process kept DockWindow busy in V-14.
        let mine = message_only();
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU32::new(0));
        let sender = {
            let (stop, sent, to) = (stop.clone(), sent.clone(), mine.0 as isize);
            std::thread::spawn(move || {
                while !stop.load(Relaxed) {
                    let mut res = 0usize;
                    let to = HWND(to as *mut std::ffi::c_void);
                    // SAFETY: WM_NULL to a window of the test thread; `res` is writable; the timeout bounds the wait.
                    let _ = unsafe {
                        SendMessageTimeoutW(to, WM_NULL, WPARAM(0), LPARAM(0), SMTO_NORMAL, 200, Some(&mut res))
                    };
                    sent.fetch_add(1, Relaxed);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        // Serve the sender until it runs: PeekMessageW dispatches the messages sent to this thread.
        let mut msg = MSG::default();
        while sent.load(Relaxed) < 3 {
            // SAFETY: `msg` is writable; PM_NOREMOVE leaves posted messages in the queue.
            let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
            std::thread::sleep(Duration::from_millis(1));
        }
        let t = Instant::now();
        let ok = answers(target);
        let waited = t.elapsed();
        stop.store(true, Relaxed);
        let _ = done.send(());
        sender.join().expect("the sender ends");
        silent.join().expect("the silent thread ends");
        // SAFETY: the window of this thread.
        let _ = unsafe { DestroyWindow(mine) };
        assert!(!ok, "a thread that never reads its queue does not answer");
        assert!(waited < Duration::from_secs(1), "the {PING_MS} ms ping took {} ms", waited.as_millis());
    }

    /// What the WM_NULL ping of `answers` costs (live check V-14): a pumping thread's message-only window, then the same
    /// thread asleep past PING_MS, then the memo of `front_answers`. Headless: a message-only window is never shown and
    /// never takes the focus. In-process (another thread), where the dock pings another process. No timing assertion:
    /// `cargo test --release ping_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ping_cost() {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, PostThreadMessageW, HWND_MESSAGE, MSG,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_QUIT,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let pump = std::thread::spawn(move || {
            // SAFETY: a message-only STATIC window of this thread, destroyed before the thread ends.
            let h = unsafe {
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
            .expect("a message-only window");
            // SAFETY: no preconditions.
            tx.send((h.0 as isize, unsafe { GetCurrentThreadId() })).expect("the test thread waits");
            let mut msg = MSG::default();
            // SAFETY: a plain message loop on this thread; WM_APP asks it to stop pumping for wParam ms.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                if msg.message == WM_APP {
                    std::thread::sleep(Duration::from_millis(msg.wParam.0 as u64));
                } else {
                    let _ = unsafe { DispatchMessageW(&msg) };
                }
            }
            // SAFETY: the window of this thread.
            let _ = unsafe { DestroyWindow(h) };
        });
        let (raw, tid) = rx.recv().expect("the pump thread starts");
        let h = HWND(raw as *mut std::ffi::c_void);
        let mut us: Vec<u128> = (0..2000)
            .map(|_| {
                let t = Instant::now();
                assert!(answers(h));
                t.elapsed().as_micros()
            })
            .collect();
        us.sort_unstable();
        let at = |p: usize| us[(us.len() - 1) * p / 100];
        println!("ping, pumping thread: p50 {} us, p95 {} us, p99 {} us, max {} us", at(50), at(95), at(99), at(100));
        // SAFETY: a thread message to the pump thread.
        unsafe { PostThreadMessageW(tid, WM_APP, WPARAM(400), LPARAM(0)) }.expect("the pump thread runs");
        std::thread::sleep(Duration::from_millis(20));
        let t = Instant::now();
        let first = front_answers(h, tid);
        let waited = t.elapsed();
        let t = Instant::now();
        let again = front_answers(h, tid);
        println!(
            "thread asleep: first ping {first} after {} ms (PING_MS {PING_MS}), retry {again} after {} us (memo)",
            waited.as_millis(),
            t.elapsed().as_micros()
        );
        // The look at the queue a front window that answers adds before the join: GetGUIThreadInfo and no ping (the
        // message-only window's queue names no window; a third thread's would cost one ping as above).
        // SAFETY: no preconditions.
        let me = unsafe { GetCurrentThreadId() };
        let mut us: Vec<u128> = (0..2000)
            .map(|_| {
                let t = Instant::now();
                assert!(queue_answers(tid, me));
                t.elapsed().as_micros()
            })
            .collect();
        us.sort_unstable();
        let at = |p: usize| us[(us.len() - 1) * p / 100];
        println!("queue look: p50 {} us, p95 {} us, p99 {} us, max {} us", at(50), at(95), at(99), at(100));
        // SAFETY: a thread message to the pump thread.
        unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) }.expect("the pump thread runs");
        pump.join().expect("the pump thread ends");
    }
}
