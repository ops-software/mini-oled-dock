//! Mini OLED popup (notifications, audio, calendar, power, tray overflow, network, battery, start menu): a layered window
//! SEPARATE from the bar, only one open at a time.
//!
//! WINDOW: class `DockPopup`, `WS_POPUP`, `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`
//! (WITHOUT `WS_EX_NOACTIVATE`: it needs focus for Esc and to know when the user clicks outside). Lives on the
//! UI thread, drawn with `surface::Surface` (no WM_PAINT). Created on first open and
//! reused (hidden on close). With `LAUNCHER_UNOWNED` (the S-OWNER fallback) the launcher gets a second window of the
//! same class with no owner, created on its first open and never destroyed before `shutdown`.
//!
//! CLOSING: `WM_ACTIVATE` with `WA_INACTIVE` (click on another window/desktop, Alt+Tab), Esc,
//! `close()` from the bar (the bar is NOACTIVATE: clicking it does not deactivate the popup, so the UI
//! closes it explicitly), action performed, dock hidden. On every close
//! `WM_DOCK_POPUP_CLOSED` is posted to the dock window.
//!
//! OUTSIDE CLICKS: `WM_ACTIVATE` alone is not reliable (the popup is opened from a click on the NOACTIVATE
//! bar and `SetForegroundWindow` may be refused or the foreground may never leave the previous app), so
//! while a popup is open a `WH_MOUSE_LL` hook watches button-down events and posts `WM_POPUP_OUTSIDE`
//! when the click is outside the popup (and outside the anchor item, which `toggle` handles on release).
//! The hook exists ONLY while a popup is open (installed in `open`, removed in `close_inner`).
//!
//! TOGGLE: `toggle(kind)` on the same kind already open closes it; if the popup of that kind was closed
//! by `WA_INACTIVE` less than `TOGGLE_DEBOUNCE_MS` ago it is NOT reopened (it was the same click).
//!
//! THREADING: UI thread only. State is thread-local; no borrow across calls that
//! pump messages (`SetForegroundWindow`, `SetWindowPos`, `ShowWindow`, `UpdateLayeredWindow`).
//! State is ALWAYS taken with `try_borrow_mut` (`with`): an unexpected re-entry is a logged no-op,
//! never a panic (panic = abort).

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, ScreenToClient, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetForegroundWindow, IsWindow, KillTimer,
    LoadCursorW, PostMessageW, RegisterClassExW, SetCursor, SetTimer, SetWindowsHookExW, ShowWindow,
    UnhookWindowsHookEx, UnregisterClassW, HHOOK, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MBUTTONDOWN, WM_RBUTTONDOWN,
    WM_XBUTTONDOWN, HTCLIENT,
    IDC_ARROW, IDC_HAND, SW_HIDE, SW_SHOWNA, WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

// The launcher's additions (L1a-13), a group of their own.
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowTextW, SC_CLOSE, SC_KEYMENU, WM_CLOSE, WM_SYSCHAR, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

// Child module via #[path]: this way main.rs does not need to declare it.
#[path = "popup_app.rs"]
pub(crate) mod popup_app;
#[path = "popup_launcher.rs"]
pub(crate) mod popup_launcher;
#[path = "popup_scroll.rs"]
pub(crate) mod popup_scroll;
#[path = "popup_start.rs"]
pub(crate) mod popup_start;
#[path = "popup_startmenu.rs"]
pub(crate) mod popup_startmenu;

use crate::audio::{OutputDevice, VolumeState};
use crate::launcher::field::KeyEvent;
use crate::launcher::model::Mods;
use crate::log;
use crate::msg::{
    TIMER_POPUP_ANIM, TIMER_POPUP_COM_FREE, TIMER_POPUP_FOCUS, TIMER_POPUP_START_RELEASE, TIMER_POPUP_TICK,
    WM_DOCK_POPUP_CLOSED, WM_POPUP_OUTSIDE,
};
use crate::render::Metrics;
use crate::surface::Surface;

/// A close due to inactivity followed within this time by `toggle` of the same kind = the same click.
pub const TOGGLE_DEBOUNCE_MS: u64 = 200;
/// Distance between the bar and the popup (logical px).
pub const POPUP_GAP: i32 = 6;
/// Minimum margin from the work area edges (logical px).
pub const POPUP_SCREEN_MARGIN: i32 = 8;
/// Duration of the opening fade (ms).
pub const FADE_MS: u64 = 100;

/// `WM_MOUSELEAVE` (defined here to avoid depending on the feature that exports it).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Fade timer step (ms).
const ANIM_STEP_MS: u32 = 15;

const CLASS_NAME: PCWSTR = w!("DockPopup");

/// Popup kind. `as usize` goes into wParam of `WM_DOCK_POPUP_CLOSED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupKind {
    Notifications = 1,
    Audio = 2,
    Calendar = 3,
    Power = 4,
    TrayOverflow = 5,
    AppMenu = 6,
    Network = 7,
    // 8 was Settings (removed in 1.4.0; the Settings window is `settings_window`).
    /// Quick-links menu of the Start button (right click).
    Start = 9,
    /// Full Start menu (all apps, search, pins) opened by the Win double tap (`popup_startmenu`).
    StartMenu = 10,
    /// Battery details of the BATTERY module (`popup_battery`).
    Battery = 11,
    /// The launcher (`popup_launcher`, SPEC-launcher), opened by its chord.
    Launcher = 12,
}

/// Where to open the popup. SCREEN coordinates in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    /// Rectangle of the clicked bar item (the popup is horizontally centered on it).
    pub item: RECT,
    /// Bar rectangle (the popup sits above it, or below if `top`, at `POPUP_GAP`).
    pub bar: RECT,
    /// true if the bar is at the top (popup below the bar).
    pub top: bool,
    /// DPI of the bar's monitor.
    pub dpi: u32,
}

/// Where a centred popup opens (the launcher, SPEC 5.2.1): the work area (screen px) and DPI of its monitor, and the
/// top edge as a percentage of the work height. The panel grows downward only.
#[derive(Clone, Copy, Debug)]
pub struct Centred {
    pub work: RECT,
    pub dpi: u32,
    /// The top edge sits at `work.top` plus this percentage of the work height.
    pub top_percent: u8,
}

/// Data of the app item for the themed context menu (`open_app_menu`).
#[derive(Clone, Debug, Default)]
pub struct AppMenuInfo {
    /// Grouping key of the item (returned by `take_app_menu_key`).
    pub key: String,
    /// Name shown at the top of the menu.
    pub name: String,
    pub pinned: bool,
    /// Number of open windows (0 = "Close" disabled).
    pub windows: usize,
    /// true if the item has a launch target ("New window" enabled).
    pub can_launch: bool,
    /// The app's own jump list (`jumplist::load`), shown above the dock's rows; empty if none.
    pub jump: Vec<crate::jumplist::Group>,
}

/// Opens the themed context menu for an app item (same style as the popups). The choice reaches the
/// dock window as `WM_DOCK_APP_MENU_CMD` (wParam = `ID_MENU_*`), AFTER the popup closes.
/// If the menu is already open for the same item it closes it (toggle).
pub fn open_app_menu(anchor: Anchor, info: AppMenuInfo) {
    if !is_available() {
        return;
    }
    let now = now_ms();
    let same_key = APP_OPEN_KEY.with(|k| k.try_borrow().map(|g| *g == info.key).unwrap_or(false));
    if current() == Some(PopupKind::AppMenu) && same_key {
        close_inner("toggle");
        return;
    }
    // Closed by focus loss a moment ago on the same item = same click: do not reopen.
    let debounced = with(|st| match st.last_inactive {
        Some((PopupKind::AppMenu, t)) if now.saturating_sub(t) < TOGGLE_DEBOUNCE_MS => {
            st.last_inactive = None;
            same_key
        }
        _ => false,
    })
    .unwrap_or(false);
    if debounced {
        return;
    }
    APP_OPEN_KEY.with(|k| {
        if let Ok(mut g) = k.try_borrow_mut() {
            *g = info.key.clone();
        }
    });
    APP_INFO.with(|i| {
        if let Ok(mut g) = i.try_borrow_mut() {
            *g = Some(info);
        }
    });
    open(PopupKind::AppMenu, anchor, None);
}

/// Key of the app chosen last in the context menu (consumes it). UI thread.
pub fn take_app_menu_key() -> Option<String> {
    APP_CHOSEN_KEY.with(|k| k.try_borrow_mut().ok().and_then(|mut g| g.take()))
}

/// Data for the app menu content (consumed by `make_content`).
pub(crate) fn take_app_info() -> Option<AppMenuInfo> {
    APP_INFO.with(|i| i.try_borrow_mut().ok().and_then(|mut g| g.take()))
}

/// Stores the chosen key (the content sets it on click, before closing).
pub(crate) fn set_app_menu_key(key: String) {
    APP_CHOSEN_KEY.with(|k| {
        if let Ok(mut g) = k.try_borrow_mut() {
            *g = Some(key);
        }
    });
}

thread_local! {
    /// Key of the item for which the app menu is (or was) open.
    static APP_OPEN_KEY: RefCell<String> = const { RefCell::new(String::new()) };
    /// Info waiting to be handed to the content.
    static APP_INFO: RefCell<Option<AppMenuInfo>> = const { RefCell::new(None) };
    /// Key chosen by the user, waiting for `take_app_menu_key`.
    static APP_CHOSEN_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

// ------------------------------------------------------- internal content contract

/// A content's reply to an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// Nothing to do.
    None,
    /// Redraw at the same size.
    Redraw,
    /// Recompute size and position, then redraw (e.g. notification list changed).
    Relayout,
    /// Close the popup (action performed).
    Close,
}

impl Reply {
    /// The "stronger" reply of the two.
    fn merge(self, other: Reply) -> Reply {
        fn rank(r: Reply) -> u8 {
            match r {
                Reply::None => 0,
                Reply::Redraw => 1,
                Reply::Relayout => 2,
                Reply::Close => 3,
            }
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Button other than the left one (tray overflow).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Right,
    Middle,
}

/// Data arriving from outside while the popup is open (forwarded to the current content).
#[derive(Clone, Debug)]
pub(crate) enum DataEvent {
    Volume(VolumeState),
    AudioOutputs(Vec<OutputDevice>),
    /// Spatial sound formats of the default output (`None` = not supported: section hidden).
    Spatial(Option<crate::audio::SpatialState>),
    /// Per-app audio sessions of the default output (volume mixer rows).
    AudioSessions(Vec<crate::audio::AppSession>),
    NotifChanged,
    TrayChanged,
    Network(crate::net::NetInfo),
    Battery(crate::battery::BatteryStatus),
    /// The Start menu catalog or some of its icons arrived (`popup_startmenu` store updated).
    StartMenuChanged,
}

/// A popup content. Client coordinates of the popup, physical pixels. `hot`/`pressed` are the ids of the
/// controls being hovered/pressed (returned by `hit`). Internal to "popups": may be changed.
pub(crate) trait Content {
    fn kind(&self) -> PopupKind;
    /// Computes (and caches) the layout given the surface's metrics/fonts; returns the
    /// desired size (physical px).
    fn size(&mut self, s: &Surface) -> (i32, i32);
    /// Largest size (physical px) the popup may take: the work area minus the margins. Contents with a
    /// long list lay themselves out inside it; the others are only clipped by `fit_to_work`.
    fn limit(&mut self, _max: (i32, i32)) {}
    /// true: the content scrolls inside the room above or below the bar (`limit_for`) instead of being clipped
    /// by the work area.
    fn scroll_cap(&self) -> bool {
        false
    }
    /// Draws everything (panel included).
    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>);
    /// Control under the point.
    fn hit(&self, x: i32, y: i32) -> Option<u32>;
    /// true if control `id` shows the hand cursor.
    fn clickable(&self, _id: u32) -> bool {
        true
    }
    /// Left click completed on control `id`.
    fn click(&mut self, id: u32) -> Reply;
    /// Left button released after a press on `id` (always, even outside the control).
    fn release(&mut self, _id: u32) -> Reply {
        Reply::None
    }
    /// Right/middle click completed on control `id`.
    fn button(&mut self, _id: u32, _which: Button) -> Reply {
        Reply::None
    }
    /// Wheel (whole notches, positive = up) over the point `(x, y)`.
    fn wheel(&mut self, _notches: i32, _x: i32, _y: i32) -> Reply {
        Reply::None
    }
    /// Drag with the button held on `id` (volume slider).
    fn drag(&mut self, _id: u32, _x: i32, _y: i32) -> Reply {
        Reply::None
    }
    /// Key pressed while the popup has the focus (`WM_KEYDOWN`, virtual-key code). `None` = not
    /// handled: Esc then closes the popup.
    fn key(&mut self, _vk: u16) -> Option<Reply> {
        None
    }
    /// Character typed while the popup has the focus (`WM_CHAR`; also `WM_SYSCHAR` while `wants_sys_keys`).
    fn char(&mut self, _c: char) -> Reply {
        Reply::None
    }
    /// Key pressed while the popup has the focus, for a content that `wants_sys_keys`: `WM_KEYDOWN` and
    /// `WM_SYSKEYDOWN` with the modifiers held. `None` = not handled: Esc then closes the popup, an unhandled system
    /// key is swallowed.
    fn key_ex(&mut self, _ev: KeyEvent) -> Option<Reply> {
        None
    }
    /// true: the window procedure sends the system keys (Alt held) to `key_ex` and `char` and swallows the Alt release
    /// and the keyboard menu (the launcher only; `route`).
    fn wants_sys_keys(&self) -> bool {
        false
    }
    /// Alt+F4 (`SC_CLOSE`) or `WM_CLOSE` while `wants_sys_keys`: the content says how it closes.
    fn close_request(&mut self) -> Reply {
        Reply::Close
    }
    /// `true`: the popup became the foreground window (once per open). `false`: it lost it and is about to close.
    fn focus(&mut self, _active: bool) -> Reply {
        Reply::None
    }
    /// The pointer moved to `(x, y)` (client px); called before the hit test.
    fn pointer(&mut self, _x: i32, _y: i32) -> Reply {
        Reply::None
    }
    /// The pointer left the popup window.
    fn leave(&mut self) -> Reply {
        Reply::None
    }
    /// true: the popup closes when the foreground retries end without the keyboard (the launcher: what the user types
    /// would reach the window behind it).
    fn focus_lost_closes(&self) -> bool {
        false
    }
    /// 1 s tick (`TIMER_POPUP_TICK`); also fired early by `popup::tick_soon`.
    fn tick(&mut self) -> Reply {
        Reply::None
    }
    /// External data updated.
    fn data(&mut self, _ev: &DataEvent) -> Reply {
        Reply::None
    }
    /// The popup is closing (e.g. `notif::set_popup_open(false)`).
    fn closing(&mut self) {}
    /// The popup is already hidden: run the actions to perform AFTER closing here.
    fn after_close(&mut self) {}
    /// The launcher's content (`with_launcher`); None for every other kind.
    fn launcher(&mut self) -> Option<&mut popup_launcher::Launcher> {
        None
    }
}

/// Clickable zones of a content (client coordinates, physical px). The last one added wins.
#[derive(Default)]
pub(crate) struct HitMap(Vec<(u32, RECT)>);

impl HitMap {
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn add(&mut self, id: u32, rc: RECT) {
        self.0.push((id, rc));
    }

    pub(crate) fn at(&self, x: i32, y: i32) -> Option<u32> {
        self.0.iter().rev().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(id, _)| *id)
    }

    pub(crate) fn rect(&self, id: u32) -> Option<RECT> {
        self.0.iter().rev().find(|(i, _)| *i == id).map(|(_, r)| *r)
    }
}

// ------------------------------------------------------------------ state

/// Open popup.
struct Open {
    kind: PopupKind,
    /// The window it is shown in (`ensure_window_for`).
    hwnd: HWND,
    content: Box<dyn Content>,
    /// `None` only while the surface is temporarily out for `present`.
    surf: Option<Surface>,
    anchor: Anchor,
    /// Top-left corner in screen coordinates.
    pos: (i32, i32),
    hot: Option<u32>,
    pressed: Option<u32>,
    tracking: bool,
    fade_start: u64,
    alpha: u8,
    /// true while the bar's show/hide animation drives this popup (`link_to_bar`): `alpha` and `dy`
    /// come from `follow_bar`, the popup's own fade is off.
    linked: bool,
    /// Vertical offset (px) of the linked slide, added to `pos` when presenting; 0 otherwise.
    dy: i32,
    /// Foreground window at open time (tick safety net).
    fg_at_open: HWND,
    /// true once the popup has been seen as the foreground window.
    was_active: bool,
    wheel_acc: i32,
    /// Foreground retries still allowed (`TIMER_POPUP_FOCUS`) while the popup is not foreground; 0 once none is armed.
    focus_tries: u8,
    /// Set for a centred popup (the launcher): placed on that work area instead of next to `anchor`.
    centred: Option<Centred>,
}

/// Delay between two foreground retries after open.
const FOCUS_RETRY_MS: u32 = 80;
/// Foreground retries after the attempt made in `open` (about 0.5 s in total).
const FOCUS_RETRIES: u8 = 6;

struct State {
    /// The shared popup window.
    hwnd: HWND,
    /// The launcher's own window (`LAUNCHER_UNOWNED`); null until its first open, and always without the fallback.
    launcher_hwnd: HWND,
    available: bool,
    registered: bool,
    open: Option<Open>,
    /// Last close due to `WA_INACTIVE`: (kind, time in ms) for the `toggle` debounce.
    last_inactive: Option<(PopupKind, u64)>,
    /// Last `VolumeState` received (even while the popup is closed).
    last_volume: VolumeState,
    /// Last network status received (initial data of the network popup; empty while the module is off).
    last_net: crate::net::NetInfo,
    /// Last battery sample received (initial data of the battery popup).
    last_battery: crate::battery::BatteryStatus,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        hwnd: HWND::default(),
        launcher_hwnd: HWND::default(),
        available: false,
        registered: false,
        open: None,
        last_inactive: None,
        last_volume: VolumeState::default(),
        last_net: crate::net::NetInfo::default(),
        last_battery: crate::battery::BatteryStatus::NONE,
    });
}

/// Runs `f` on the state. `None` if the state is already borrowed (re-entry): never a panic.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut g) => Some(f(&mut g)),
        Err(_) => {
            log!("popup: state already in use (re-entry), operation skipped");
            None
        }
    })
}

fn now_ms() -> u64 {
    // SAFETY: no preconditions.
    unsafe { GetTickCount64() }
}

// ------------------------------------------------------------------ public API

/// Registers the `DockPopup` class. Called by `main` after `ui::attach`. Error = no popups
/// (the UI falls back to the direct (non-popup) actions).
pub fn init() -> Result<(), String> {
    // SAFETY: no preconditions.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(module.0),
        // SAFETY: loads a system cursor (null hInstance), no pointers to manage.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialized; the class name is a static literal.
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        return Err("RegisterClassExW(DockPopup) failed".into());
    }
    with(|st| {
        st.registered = true;
        st.available = true;
    });
    log!("popup::init: DockPopup class registered");
    popup_launcher::init();
    Ok(())
}

/// true if `init` succeeded (class registered). false = the UI uses the direct (non-popup) actions
/// (bell = Win+N, volume = mute, clock = ms-settings:dateandtime, power = native menu
/// "Exit dock").
pub fn is_available() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.available).unwrap_or(false))
}

/// Opens popup `kind` anchored to `anchor` (first closing any other popup), or closes it
/// if the same kind is already open (see TOGGLE in the module doc).
pub fn toggle(kind: PopupKind, anchor: Anchor) {
    #[derive(PartialEq)]
    enum Plan {
        Skip,
        Close,
        Open,
    }
    let now = now_ms();
    let plan = with(|st| {
        if !st.available {
            return Plan::Skip;
        }
        let open = st.open.as_ref().map(|o| o.kind);
        if open == Some(kind) {
            return Plan::Close;
        }
        if let Some((k, t)) = st.last_inactive {
            if k == kind && now.saturating_sub(t) < TOGGLE_DEBOUNCE_MS {
                st.last_inactive = None;
                return Plan::Skip;
            }
        }
        Plan::Open
    })
    .unwrap_or(Plan::Skip);
    match plan {
        Plan::Skip => {}
        Plan::Close => close_inner("toggle"),
        Plan::Open => {
            open(kind, anchor, None);
        }
    }
}

/// Closes the open popup (no-op if none). `reason` is for the log only.
pub fn close(reason: &str) {
    close_inner(reason);
}

/// Whether a close the host applies (the bar hides or moves) reaches the open popup: the launcher is exempt (SPEC 5.2.1).
fn host_close_applies(open: Option<PopupKind>) -> bool {
    open != Some(PopupKind::Launcher)
}

/// `close` for the host's own closes (the bar hides or moves): a no-op while the launcher is open.
pub fn close_unless_launcher(reason: &str) {
    if host_close_applies(current()) {
        close_inner(reason);
    }
}

/// Opens popup `kind` centred on `c.work` (the launcher), first closing any other popup: the open path of `toggle`
/// with the placement of `Centred`. false when nothing is shown (no popups, no surface, or the first render failed).
pub fn open_centred(kind: PopupKind, c: Centred) -> bool {
    if !is_available() {
        return false;
    }
    open(kind, Anchor { item: c.work, bar: c.work, top: false, dpi: c.dpi }, Some(c))
}

/// Runs `f` on the open launcher's content and applies its reply (the launcher's controller reaches its open content
/// through it, never while a content method runs). false when the launcher is not the open popup.
pub(crate) fn with_launcher(f: impl FnOnce(&mut popup_launcher::Launcher) -> Reply) -> bool {
    let reply = with(|st| st.open.as_mut().and_then(|o| o.content.launcher()).map(f)).flatten();
    match reply {
        Some(r) => {
            apply(r);
            true
        }
        None => false,
    }
}

/// Links the open popup to the bar's show/hide animation (Win with `win_opens_start`): its own fade
/// stops and `follow_bar` sets its alpha and slide on the bar's frames. false if no popup is open.
pub fn link_to_bar() -> bool {
    let hwnd = with(|st| {
        let o = st.open.as_mut()?;
        o.linked = true;
        Some(o.hwnd)
    })
    .flatten();
    let Some(hwnd) = hwnd else { return false };
    // SAFETY: timer of our window; fails harmlessly if it is not armed.
    let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_ANIM) };
    true
}

/// true if the open popup follows the bar's animation (`link_to_bar`).
pub fn is_linked() -> bool {
    STATE.with(|s| s.try_borrow().ok().and_then(|g| g.open.as_ref().map(|o| o.linked)).unwrap_or(false))
}

/// One frame of the bar's show/hide animation (`ui::on_vis_tick`): a linked popup is presented with
/// the bar's slide `dy` and `alpha`, without repainting. `settled` with full alpha ends the link (the
/// show is over). No-op when nothing is linked.
pub fn follow_bar(dy: i32, alpha: u8, settled: bool) {
    let prepared = with(|st| {
        let o = st.open.as_mut()?;
        let hwnd = o.hwnd;
        if !o.linked {
            return None;
        }
        o.dy = dy;
        o.alpha = alpha;
        if settled && alpha == 255 {
            o.linked = false;
            o.dy = 0;
        }
        let surf = o.surf.take()?;
        Some((hwnd, surf, o.pos))
    })
    .flatten();
    let Some((hwnd, surf, pos)) = prepared else { return };
    surf.present(hwnd, pos.0, pos.1 + dy, alpha);
    with(|st| {
        if let Some(o) = st.open.as_mut() {
            o.surf = Some(surf);
        }
    });
}

/// true if a popup is open (the UI suspends auto-hide).
pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.open.is_some()).unwrap_or(false))
}

/// Window of the open popup (None if no popup is open).
// clippy: kept as is, no runtime change in 1.4.3 (ROADMAP 3.6); rewrite is a follow-up of the stream that owns this file
#[allow(clippy::unnecessary_lazy_evaluations)]
pub fn current_hwnd() -> Option<HWND> {
    let h = POPUP_HWND.load(std::sync::atomic::Ordering::Relaxed);
    (h != 0 && is_open()).then(|| HWND(h as *mut _))
}

/// Kind of the open popup (the UI keeps the plate on the matching item).
pub fn current() -> Option<PopupKind> {
    STATE.with(|s| s.try_borrow().ok().and_then(|g| g.open.as_ref().map(|o| o.kind)))
}

/// The bar moved or resized (config change) while a popup is open: re-anchors it to `anchor` (the
/// same item's new rectangle). Called by `ui`. While the left button is held (a Settings slider or
/// the colour picker being dragged) the popup stays put, so the control does not slide away under
/// the pointer: the new anchor is used by the next relayout (the release saves and re-applies).
pub fn reanchor(anchor: Anchor) {
    let moved = with(|st| match st.open.as_mut() {
        Some(o) => {
            o.anchor = anchor;
            o.pressed.is_none()
        }
        None => false,
    });
    if moved == Some(true) {
        store_rect(&ITEM_RECT, &anchor.item);
        render(true);
    }
}

/// `WM_DOCK_VOLUME` reached the UI: updates the audio popup if open. Called by `ui`.
pub fn on_volume(st: VolumeState) {
    with(|s| s.last_volume = st);
    dispatch(DataEvent::Volume(st));
}

/// `ShellEvent::AudioOutputs` reached the UI. Called by `ui`.
pub fn on_audio_outputs(list: Vec<OutputDevice>) {
    if is_open() {
        dispatch(DataEvent::AudioOutputs(list));
    }
}

/// `ShellEvent::Spatial` reached the UI. Called by `ui`.
pub fn on_audio_spatial(state: Option<crate::audio::SpatialState>) {
    if current() == Some(PopupKind::Audio) {
        dispatch(DataEvent::Spatial(state));
    }
}

/// `ShellEvent::AudioSessions` reached the UI. Called by `ui`.
pub fn on_audio_sessions(list: Vec<crate::audio::AppSession>) {
    if current() == Some(PopupKind::Audio) {
        dispatch(DataEvent::AudioSessions(list));
    }
}

/// `ShellEvent::Network` reached the UI: remembered for the next network popup and forwarded to
/// the open one. Called by `ui`.
pub fn on_network(info: crate::net::NetInfo) {
    let open = with(|s| {
        s.last_net.clone_from(&info);
        s.open.as_ref().is_some_and(|o| o.kind == PopupKind::Network)
    });
    if open == Some(true) {
        dispatch(DataEvent::Network(info));
    }
}

/// A new battery sample reached the UI: remembered for the next battery popup and forwarded to the
/// open one. Called by `ui`.
pub fn on_battery(status: crate::battery::BatteryStatus) {
    let open = with(|s| {
        s.last_battery = status;
        s.open.as_ref().is_some_and(|o| o.kind == PopupKind::Battery)
    });
    if open == Some(true) {
        dispatch(DataEvent::Battery(status));
    }
}

/// `WM_DOCK_NOTIF`: re-reads `notif::snapshot()` if the notifications popup is open. Called by `main`.
pub fn on_notif_changed() {
    dispatch(DataEvent::NotifChanged);
}

/// `WM_DOCK_TRAY_CHANGED`: updates the overflow popup if open. Called by `main`.
pub fn on_tray_changed() {
    dispatch(DataEvent::TrayChanged);
}

/// `ShellEvent::StartApps` reached the UI: one listing for both (L3 T17). The launcher keeps the arena (with a chord
/// set), the Start menu makes its rows of it (when open), and the catalog is refreshed in an open Start menu.
pub fn on_start_apps(apps: std::sync::Arc<crate::launcher::arena::Arena>) {
    popup_startmenu::store_apps(&apps);
    popup_launcher::on_catalog(apps);
    if current() == Some(PopupKind::StartMenu) {
        dispatch(DataEvent::StartMenuChanged);
    }
}

/// `ShellEvent::StartIcons` reached the UI: stores the icons and redraws the Start menu if open.
pub fn on_start_icons(icon_px: u32, icons: Vec<(String, Option<crate::icons::IconBitmap>)>) {
    if popup_startmenu::store_icons(icon_px, icons) && current() == Some(PopupKind::StartMenu) {
        dispatch(DataEvent::StartMenuChanged);
    }
}

/// `ShellEvent::StartSettings` reached the UI: the settings catalog is ready, so an open Start menu
/// searches again (what was typed may match a setting).
pub fn on_start_settings() {
    popup_launcher::on_settings_ready();
    if current() == Some(PopupKind::StartMenu) {
        dispatch(DataEvent::StartMenuChanged);
    }
}

/// Destroys the popup window and the surface, unregisters the class. In `cleanup`, before
/// `ui::shutdown`. Idempotent.
pub fn shutdown() {
    close_inner("shutdown");
    popup_launcher::shutdown();
    for h in with(|st| [st.hwnd, st.launcher_hwnd]).unwrap_or_default().into_iter().filter(|h| !h.0.is_null()) {
        // SAFETY: our window.
        let _ = unsafe { KillTimer(Some(h), TIMER_POPUP_START_RELEASE) };
        let _ = unsafe { KillTimer(Some(h), TIMER_POPUP_COM_FREE) };
    }
    if let Some(h) = crate::launcher_sys::timer_window() {
        for id in LAUNCHER_TIMERS {
            // SAFETY: the dock window (same thread), still alive here; an id that is not armed fails harmlessly.
            let _ = unsafe { KillTimer(Some(h), id) };
        }
    }
    let Some((windows, registered)) = with(|st| {
        let r = ([st.hwnd, st.launcher_hwnd], st.registered);
        st.hwnd = HWND::default();
        st.launcher_hwnd = HWND::default();
        st.registered = false;
        st.available = false;
        r
    }) else {
        return;
    };
    for hwnd in windows.into_iter().filter(|h| !h.0.is_null()) {
        // SAFETY: window created by this module on the UI thread.
        if let Err(e) = unsafe { DestroyWindow(hwnd) } {
            log!("popup: DestroyWindow failed: {e}");
        }
    }
    if registered {
        // SAFETY: class registered by `init` with this same module; no window survives.
        let inst = unsafe { GetModuleHandleW(PCWSTR::null()) }.map(|m| HINSTANCE(m.0)).ok();
        let _ = unsafe { UnregisterClassW(CLASS_NAME, inst) };
    }
}

// ------------------------------------------------------------------ outside-click hook

/// Installed `WH_MOUSE_LL` handle (0 = none).
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
/// Popup window rectangle (screen px) and anchor item rectangle, read by the hook callback:
/// `[left, top, right, bottom]` each.
static POPUP_RECT: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];
static ITEM_RECT: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];

fn store_rect(dst: &[AtomicI32; 4], r: &RECT) {
    dst[0].store(r.left, Ordering::Relaxed);
    dst[1].store(r.top, Ordering::Relaxed);
    dst[2].store(r.right, Ordering::Relaxed);
    dst[3].store(r.bottom, Ordering::Relaxed);
}

fn in_rect(r: &[AtomicI32; 4], p: POINT) -> bool {
    p.x >= r[0].load(Ordering::Relaxed)
        && p.x < r[2].load(Ordering::Relaxed)
        && p.y >= r[1].load(Ordering::Relaxed)
        && p.y < r[3].load(Ordering::Relaxed)
}

/// Installs the outside-click hook (idempotent). The callback runs on this (UI) thread during message
/// pumping, so it only reads atomics and posts a message.
fn install_mouse_hook() {
    if MOUSE_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    // SAFETY: no preconditions.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok();
    // SAFETY: the callback is a valid `extern "system"` fn for the lifetime of the process.
    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), module.map(|m| HINSTANCE(m.0)), 0) } {
        Ok(h) => MOUSE_HOOK.store(h.0 as isize, Ordering::Relaxed),
        Err(e) => log!("popup: SetWindowsHookExW(WH_MOUSE_LL) failed: {e}"),
    }
}

/// Removes the outside-click hook for the duration of a long blocking call on the UI thread (a stalled
/// low-level hook freezes the system mouse and is dropped by Windows after `LowLevelHooksTimeout`).
/// Returns true if it was installed; pass that to `resume_mouse_hook` afterwards. On any other thread it does nothing
/// and returns false: the UI thread installed the hook and serves it, and `resume_mouse_hook` installs it on the
/// calling thread.
pub fn suspend_mouse_hook() -> bool {
    if !crate::on_ui_thread() {
        return false;
    }
    let was = MOUSE_HOOK.load(Ordering::Relaxed) != 0;
    remove_mouse_hook();
    was
}

/// Reinstalls the hook removed by `suspend_mouse_hook`, if a popup is still open (never for the launcher).
pub fn resume_mouse_hook(was: bool) {
    if was && is_open() && current() != Some(PopupKind::Launcher) {
        install_mouse_hook();
    }
}

fn remove_mouse_hook() {
    let raw = MOUSE_HOOK.swap(0, Ordering::Relaxed);
    if raw != 0 {
        // SAFETY: handle returned by SetWindowsHookExW, removed only once (swap).
        if let Err(e) = unsafe { UnhookWindowsHookEx(HHOOK(raw as *mut _)) } {
            log!("popup: UnhookWindowsHookEx failed: {e}");
        }
    }
}

unsafe extern "system" fn ll_mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let m = wp.0 as u32;
        probe!(crate::probe::mouse(m));
        if m == WM_LBUTTONDOWN || m == WM_RBUTTONDOWN || m == WM_MBUTTONDOWN || m == WM_XBUTTONDOWN {
            // SAFETY: for HC_ACTION in a WH_MOUSE_LL hook, lparam points to an MSLLHOOKSTRUCT valid
            // for the duration of the callback.
            let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
            if !in_rect(&POPUP_RECT, pt) && !in_rect(&ITEM_RECT, pt) {
                // try_borrow: if the state is busy the hwnd read is skipped (the tick safety net remains).
                let hwnd = STATE
                    .with(|s| s.try_borrow().map(|g| g.open.as_ref().map_or(g.hwnd, |o| o.hwnd)).unwrap_or_default());
                if !hwnd.0.is_null() {
                    probe!(crate::probe::post_by(crate::probe::Th::Ui, crate::probe::BY_MOUSE_HOOK, WM_POPUP_OUTSIDE));
                    // SAFETY: posting to our own window; failure is harmless.
                    let _ = unsafe { PostMessageW(Some(hwnd), WM_POPUP_OUTSIDE, WPARAM(0), LPARAM(0)) };
                }
            }
        }
    }
    // SAFETY: unchanged forwarding to the hook chain.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

// ------------------------------------------------------------------ open / close

/// Builds the content for `kind`.
fn make_content(
    kind: PopupKind,
    volume: VolumeState,
    net: crate::net::NetInfo,
    battery: crate::battery::BatteryStatus,
) -> Box<dyn Content> {
    match kind {
        PopupKind::Notifications => crate::popup_notif::new(),
        PopupKind::Audio => crate::popup_audio::new(volume),
        PopupKind::Calendar => crate::popup_calendar::new(),
        PopupKind::Power => crate::popup_power::new(),
        PopupKind::TrayOverflow => crate::popup_tray::new(),
        PopupKind::AppMenu => popup_app::new(take_app_info().unwrap_or_default()),
        PopupKind::Network => crate::popup_network::new(net),
        PopupKind::Battery => crate::popup_battery::new(battery, crate::battery::power_mode()),
        PopupKind::Start => popup_start::new(),
        PopupKind::StartMenu => popup_startmenu::new(),
        PopupKind::Launcher => popup_launcher::new(),
    }
}

/// Name of the popup window while `kind` is shown: screen readers get the launcher's static name (SPEC 5.6), every
/// other kind keeps the shared window's name.
fn window_name_for(kind: PopupKind) -> &'static str {
    match kind {
        PopupKind::Launcher => crate::launcher::strings::window_name(),
        _ => "Dock popup",
    }
}

/// S-OWNER (L0 owner session): false = the launcher shows in the shared popup window, owned by the dock window (the
/// default until S-OWNER is recorded); true = the launcher gets its own unowned window, the fallback for an owned window
/// that does not show or take the keyboard while its owner, the bar, is hidden.
const LAUNCHER_UNOWNED: bool = false;

/// The window a popup kind is shown in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// The shared popup window (`State::hwnd`), owned by the dock window.
    Shared,
    /// The launcher's own window (`State::launcher_hwnd`), with no owner.
    Launcher,
}

/// `unowned_launcher` is `LAUNCHER_UNOWNED`.
fn window_for_kind(kind: PopupKind, unowned_launcher: bool) -> Slot {
    if kind == PopupKind::Launcher && unowned_launcher {
        Slot::Launcher
    } else {
        Slot::Shared
    }
}

/// true while `kind` is shown in the shared window under the launcher's name (renamed at open, named back at close).
fn renames_shared_window(kind: PopupKind) -> bool {
    kind == PopupKind::Launcher && window_for_kind(kind, LAUNCHER_UNOWNED) == Slot::Shared
}

/// The window of popup `kind`, created on first request and reused.
fn ensure_window_for(kind: PopupKind) -> Option<HWND> {
    let slot = window_for_kind(kind, LAUNCHER_UNOWNED);
    let existing = with(|st| match slot {
        Slot::Shared => st.hwnd,
        Slot::Launcher => st.launcher_hwnd,
    })?;
    // SAFETY: IsWindow accepts any handle, even null.
    if !existing.0.is_null() && unsafe { IsWindow(Some(existing)) }.as_bool() {
        return Some(existing);
    }
    // It existed and was destroyed (Alt+F4 on a kind that does not take the system keys reaches DefWindowProc): the
    // timers armed on it are gone. Logged only with the launcher on, so the log without a chord stays 1.4.3's.
    if !existing.0.is_null() && popup_launcher::is_on() {
        log!("popup: {slot:?} window was destroyed, recreated (its timers are gone)");
    }
    // SAFETY: no preconditions.
    let module = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(m) => m,
        Err(e) => {
            log!("popup: GetModuleHandleW failed: {e}");
            return None;
        }
    };
    let owner = crate::hwnd();
    let owner = if owner.0.is_null() || slot == Slot::Launcher { None } else { Some(owner) };
    // The launcher's own window keeps its name for good.
    let launcher_name = (slot == Slot::Launcher).then(|| HSTRING::from(window_name_for(PopupKind::Launcher)));
    let name = launcher_name.as_ref().map_or(w!("Dock popup"), |n| PCWSTR(n.as_ptr()));
    // SAFETY: class registered by `init`; the names are a static literal or `launcher_name`, alive for the call; the
    // owner is the dock window (same thread) or none.
    let r = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            name,
            WS_POPUP,
            0,
            0,
            0,
            0,
            owner,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
    };
    match (r, slot) {
        (Ok(h), Slot::Shared) => {
            with(|st| st.hwnd = h);
            POPUP_HWND.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
            Some(h)
        }
        (Ok(h), Slot::Launcher) => {
            with(|st| st.launcher_hwnd = h);
            Some(h)
        }
        (Err(e), Slot::Shared) => {
            log!("popup: CreateWindowExW failed: {e}");
            with(|st| st.available = false);
            None
        }
        // Only the launcher fails to open; the other popups keep their window.
        (Err(e), Slot::Launcher) => {
            log!("popup: CreateWindowExW (launcher) failed: {e}");
            None
        }
    }
}

/// Work area of the monitor containing `bar`.
pub(crate) fn work_area(bar: &RECT) -> RECT {
    // SAFETY: `bar` is valid; MONITOR_DEFAULTTONEAREST always returns a monitor.
    let mon = unsafe { MonitorFromRect(bar, MONITOR_DEFAULTTONEAREST) };
    crate::ui::monitor_info(mon).map_or(*bar, |mi| mi.rcWork)
}

/// `w` x `h` capped to the work area minus `margin` on each side (never below 1 px).
pub(crate) fn fit_to_work(w: i32, h: i32, work: &RECT, margin: i32) -> (i32, i32) {
    (w.min(work.right - work.left - 2 * margin).max(1), h.min(work.bottom - work.top - 2 * margin).max(1))
}

/// Gap (logical px) a scrolling popup keeps from the screen edge on the side away from the bar.
const POPUP_MAX_MARGIN: i32 = 16;

/// Largest size of a scrolling popup: the work width minus two margins, and the height between the bar (plus
/// `POPUP_GAP`) and the far margin. `compute_pos` clamps to the same margins, so a popup of this size never
/// reaches over the bar.
pub(crate) fn max_popup_size(work: &RECT, bar: &RECT, top: bool, dpi: u32) -> (i32, i32) {
    let m = Metrics { dpi };
    let (margin, gap) = (m.px(POPUP_MAX_MARGIN), m.px(POPUP_GAP));
    let h = if top { work.bottom - margin - (bar.bottom + gap) } else { (bar.top - gap) - (work.top + margin) };
    ((work.right - work.left - 2 * margin).max(1), h.max(1))
}

/// The limit `render` hands to a content anchored to a bar: a scrolling content gets `max_popup_size`, every
/// other one the old work-area bound.
pub(crate) fn limit_for(scroll_cap: bool, work: &RECT, bar: &RECT, top: bool, dpi: u32) -> (i32, i32) {
    if scroll_cap {
        max_popup_size(work, bar, top, dpi)
    } else {
        fit_to_work(i32::MAX, i32::MAX, work, Metrics { dpi }.px(POPUP_SCREEN_MARGIN))
    }
}

/// Top-left corner position of a `w` x `h` popup (also used by the window picker).
pub(crate) fn compute_pos(a: &Anchor, w: i32, h: i32) -> (i32, i32) {
    let m = Metrics { dpi: a.dpi };
    let work = work_area(&a.bar);
    let margin = m.px(POPUP_SCREEN_MARGIN);
    let gap = m.px(POPUP_GAP);
    let cx = (a.item.left + a.item.right) / 2;
    let (lo, hi) = (work.left + margin, work.right - margin - w);
    let x = if hi < lo { lo } else { (cx - w / 2).clamp(lo, hi) };
    let y = if a.top { a.bar.bottom + gap } else { a.bar.top - gap - h };
    let (ylo, yhi) = (work.top + margin, work.bottom - margin - h);
    let y = if yhi < ylo { ylo } else { y.clamp(ylo, yhi) };
    (x, y)
}

/// The top edge of a centred popup: `top_percent` of the work height below the work-area top.
fn centred_top(c: &Centred) -> i32 {
    c.work.top + (c.work.bottom - c.work.top) * i32::from(c.top_percent.min(100)) / 100
}

/// Largest size of a centred popup: the work width minus `margin` on each side, and from its top edge down to
/// `margin` above the work-area bottom (it never moves up to make room).
fn centred_max(c: &Centred, margin: i32) -> (i32, i32) {
    ((c.work.right - c.work.left - 2 * margin).max(1), (c.work.bottom - margin - centred_top(c)).max(1))
}

/// Position and size `(x, y, w, h)` of a centred popup that wants `w` x `h`: capped by `centred_max`,
/// horizontally centred on the work area, its top edge at `centred_top`.
fn centred_place(c: &Centred, margin: i32, w: i32, h: i32) -> (i32, i32, i32, i32) {
    let (mw, mh) = centred_max(c, margin);
    let (w, h) = (w.min(mw).max(1), h.min(mh).max(1));
    (c.work.left + (c.work.right - c.work.left - w) / 2, centred_top(c), w, h)
}

/// Alpha a popup opens at: the launcher shows at once (its field takes the typing), every other kind fades in.
fn initial_alpha(kind: PopupKind) -> u8 {
    if kind == PopupKind::Launcher {
        255
    } else {
        0
    }
}

/// Opens `kind` (see `toggle` and `open_centred`); false when nothing is shown.
fn open(kind: PopupKind, anchor: Anchor, centred: Option<Centred>) -> bool {
    // Any other popup closes without a fade; so does the window picker.
    close_inner("popup switch");
    crate::picker::close("popup opened");
    let Some(hwnd) = ensure_window_for(kind) else { return false };
    if kind == PopupKind::StartMenu {
        // SAFETY: our window; fails harmlessly if the timer is not armed.
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_START_RELEASE) };
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_COM_FREE) };
    }
    let metrics = Metrics { dpi: anchor.dpi.max(96) };
    let surf = match Surface::new(metrics) {
        Ok(s) => s,
        Err(e) => {
            log!("popup: Surface::new failed: {e}");
            return false;
        }
    };
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    let Some((volume, net, battery)) = with(|st| (st.last_volume, st.last_net.clone(), st.last_battery)) else {
        return false;
    };
    let content = make_content(kind, volume, net, battery);
    debug_assert_eq!(content.kind(), kind);
    let installed = with(|st| {
        st.last_inactive = None;
        st.open = Some(Open {
            kind,
            hwnd,
            content,
            surf: Some(surf),
            anchor,
            pos: (0, 0),
            hot: None,
            pressed: None,
            tracking: false,
            fade_start: now_ms(),
            alpha: initial_alpha(kind),
            linked: false,
            dy: 0,
            fg_at_open: fg,
            was_active: false,
            wheel_acc: 0,
            focus_tries: FOCUS_RETRIES,
            centred,
        });
    });
    if installed.is_none() {
        return false;
    }
    // `current_hwnd` and `tick_soon` reach the open popup's window.
    POPUP_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::render_start(kind == PopupKind::Launcher);
    if !render(true) {
        close_inner("initial render failed");
        return false;
    }
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::presented(kind == PopupKind::Launcher);
    log!("popup: opening {kind:?}");
    // No borrow during ShowWindow / force_foreground: they can deliver messages to the popup.
    // SHOWNA: activated only by `force_foreground`. Active before its AttachThreadInput, the popup would get
    // WA_INACTIVE from the merged input queues and close at once (first click lost when another app was in front).
    if renames_shared_window(kind) {
        // SAFETY: `hwnd` is our popup window (same thread); no state borrow is held.
        if let Err(e) = unsafe { SetWindowTextW(hwnd, &HSTRING::from(window_name_for(kind))) } {
            log!("popup: SetWindowTextW failed: {e}");
        }
    }
    // SAFETY: `hwnd` is our popup window.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNA) };
    store_rect(&ITEM_RECT, &anchor.item);
    // The launcher installs no mouse hook (SPEC 5.2.10): a busy UI thread would stall the system mouse. It closes on
    // focus loss alone.
    if kind != PopupKind::Launcher {
        install_mouse_hook();
    }
    let outcome = retry_outcome(FOCUS_RETRIES, crate::ui::force_foreground(hwnd), false);
    if outcome == Retry::Focused {
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::won_foreground(hwnd);
        // A no-op when WM_ACTIVATE already saw the popup in front (`was_active`).
        became_foreground();
    }
    // A close during the activation (the content's `focus` runs from WM_ACTIVATE inside SetForegroundWindow, or from the
    // won attempt's `became_foreground` just above) leaves no popup to arm timers for: the tick would re-arm itself on a
    // closed popup.
    if with(|st| st.open.as_ref().map(|o| o.hwnd)).flatten() != Some(hwnd) {
        return true;
    }
    // The foreground can be refused (lock, elevated app in front, a transient null foreground):
    // visible but without the keyboard, the Start menu search box would not receive typing.
    if outcome == Retry::Again {
        arm_retry(hwnd);
    }
    // SAFETY: `hwnd` is our window; the callbacks are in its wnd_proc.
    unsafe {
        if initial_alpha(kind) < 255 && SetTimer(Some(hwnd), TIMER_POPUP_ANIM, ANIM_STEP_MS, None) == 0 {
            log!("popup: SetTimer(anim) failed");
        }
        if SetTimer(Some(hwnd), TIMER_POPUP_TICK, 1000, None) == 0 {
            log!("popup: SetTimer(tick) failed");
        }
    }
    with(|st| {
        if let Some(o) = st.open.as_mut() {
            o.fade_start = now_ms();
        }
    });
    true
}

/// Closes the open popup. Idempotent.
/// Time (GetTickCount64) of the last popup close, for `closed_within`.
static LAST_CLOSE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// true if a popup closed within the last `ms` milliseconds.
pub fn closed_within(ms: u64) -> bool {
    // SAFETY: GetTickCount64 has no preconditions.
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let t = LAST_CLOSE_MS.load(std::sync::atomic::Ordering::Relaxed);
    t != 0 && now.saturating_sub(t) <= ms
}

fn close_inner(reason: &str) {
    let Some(taken) = with(|st| st.open.take()) else { return };
    let Some(mut o) = taken else { return };
    let hwnd = o.hwnd;
    log!("popup: closing {:?} ({reason})", o.kind);
    HIGH.with(|h| h.set(0));
    // SAFETY: GetTickCount64 has no preconditions.
    let tick = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    LAST_CLOSE_MS.store(tick, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: `hwnd` is our window (or null: then the calls just fail).
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_POPUP_ANIM);
        let _ = KillTimer(Some(hwnd), TIMER_POPUP_TICK);
        let _ = KillTimer(Some(hwnd), TIMER_POPUP_FOCUS);
    }
    remove_mouse_hook();
    if o.pressed.is_some() {
        // SAFETY: releases any mouse capture taken by this popup.
        let _ = unsafe { ReleaseCapture() };
    }
    o.content.closing();
    if !hwnd.0.is_null() {
        // SAFETY: `hwnd` is our window; the state is not borrowed.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        if renames_shared_window(o.kind) {
            // The shared window gets its own name back once hidden.
            // SAFETY: `hwnd` is our window; the state is not borrowed.
            let _ = unsafe { SetWindowTextW(hwnd, w!("Dock popup")) };
        }
    }
    o.content.after_close();
    let kind = o.kind;
    drop(o);
    // SAFETY: PostMessageW to the dock window; if it no longer exists it just fails.
    if let Err(e) = unsafe { PostMessageW(Some(crate::hwnd()), WM_DOCK_POPUP_CLOSED, WPARAM(kind as usize), LPARAM(0)) }
    {
        log!("popup: PostMessageW(WM_DOCK_POPUP_CLOSED) failed: {e}");
    }
}

/// Redraws the open popup; with `relayout` it first recomputes size and position. false if there
/// is nothing left to show or rendering fails.
fn render(relayout: bool) -> bool {
    // The surface leaves the state for the whole of `present`: no borrow during UpdateLayeredWindow.
    let prepared = with(|st| {
        let o = st.open.as_mut()?;
        let hwnd = o.hwnd;
        let mut surf = o.surf.take()?;
        // A previous failed resize leaves the surface without a DIB (0 x 0): retry the layout.
        if relayout || surf.size() == (0, 0) {
            let (w, h, centred_at) = match o.centred {
                Some(c) => {
                    let margin = Metrics { dpi: c.dpi }.px(POPUP_SCREEN_MARGIN);
                    o.content.limit(centred_max(&c, margin));
                    let (w, h) = o.content.size(&surf);
                    let (x, y, w, h) = centred_place(&c, margin, w, h);
                    (w, h, Some((x, y)))
                }
                None => {
                    let work = work_area(&o.anchor.bar);
                    o.content.limit(limit_for(
                        o.content.scroll_cap(),
                        &work,
                        &o.anchor.bar,
                        o.anchor.top,
                        o.anchor.dpi,
                    ));
                    let (w, h) = o.content.size(&surf);
                    let margin = Metrics { dpi: o.anchor.dpi }.px(POPUP_SCREEN_MARGIN);
                    let (w, h) = fit_to_work(w, h, &work_area(&o.anchor.bar), margin);
                    (w, h, None)
                }
            };
            if let Err(e) = surf.resize(w.max(1), h.max(1)) {
                log!("popup: resize failed: {e}");
                // The surface goes back into the state: the next render retries instead of staying
                // frozen forever.
                o.surf = Some(surf);
                return None;
            }
            o.pos = centred_at.unwrap_or_else(|| compute_pos(&o.anchor, w.max(1), h.max(1)));
        }
        o.content.paint(&mut surf, o.hot, o.pressed);
        Some((hwnd, surf, o.pos, o.alpha, o.dy))
    })
    .flatten();
    let Some((hwnd, surf, pos, alpha, dy)) = prepared else { return false };
    let (sw, sh) = surf.size();
    store_rect(&POPUP_RECT, &RECT { left: pos.0, top: pos.1, right: pos.0 + sw, bottom: pos.1 + sh });
    let ok = surf.present(hwnd, pos.0, pos.1 + dy, alpha);
    with(|st| {
        if let Some(o) = st.open.as_mut() {
            o.surf = Some(surf);
        }
    });
    ok
}

fn apply(reply: Reply) {
    match reply {
        Reply::None => {}
        Reply::Redraw => {
            render(false);
        }
        Reply::Relayout => {
            render(true);
        }
        Reply::Close => close_inner("action"),
    }
}

/// Forwards external data to the open content.
fn dispatch(ev: DataEvent) {
    let reply = with(|st| st.open.as_mut().map(|o| o.content.data(&ev))).flatten();
    if let Some(r) = reply {
        apply(r);
    }
}

// ------------------------------------------------------------------ window

fn lo_i16(v: isize) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}

fn hi_i16(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// What the window procedure does with a keyboard or close message while the open content `wants_sys_keys`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// A key down for the content's `key_ex`.
    ToContent,
    /// A system character for the content's `char`.
    Char,
    /// Swallowed: answered 0, never DefWindowProc (no menu mode, no beep).
    Consume,
    /// The content's `close_request`; answered 0, so the shared window is never destroyed.
    CloseRequest,
    /// The usual arm, as for every other kind.
    Default,
}

/// How `msg` is handled: the one list of the routed messages (lint 12). `wants_sys` reads the open content's
/// `wants_sys_keys`, asked only for a listed message (every other one costs no state borrow); false gives `Default`.
fn route(msg: u32, wp: usize, wants_sys: impl FnOnce() -> bool) -> Route {
    let r = match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => Route::ToContent,
        WM_SYSCHAR => Route::Char,
        WM_SYSKEYUP if wp == usize::from(VK_MENU.0) => Route::Consume,
        // The low four bits of an SC_ code are the system's own.
        WM_SYSCOMMAND => match (wp & 0xFFF0) as u32 {
            SC_KEYMENU => Route::Consume,
            SC_CLOSE => Route::CloseRequest,
            _ => Route::Default,
        },
        WM_CLOSE => Route::CloseRequest,
        _ => Route::Default,
    };
    if r != Route::Default && wants_sys() {
        r
    } else {
        Route::Default
    }
}

/// The modifiers of a key event.
fn mods_of(ctrl: bool, shift: bool, alt: bool, win: bool) -> Mods {
    let mut m = Mods::NONE;
    for (down, bit) in [(ctrl, Mods::CTRL), (shift, Mods::SHIFT), (alt, Mods::ALT), (win, Mods::WIN)] {
        if down {
            m = m.with(bit);
        }
    }
    m
}

/// The modifiers held when the key message being handled was posted.
fn held_mods() -> Mods {
    // SAFETY: GetKeyState has no preconditions.
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(vk.0)) } < 0;
    mods_of(down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU), down(VK_LWIN) || down(VK_RWIN))
}

/// The `KeyEvent` of a `WM_KEYDOWN` or `WM_SYSKEYDOWN`: lParam bit 30 is the auto-repeat.
fn key_event(msg: u32, wp: usize, lp: isize, mods: Mods) -> KeyEvent {
    KeyEvent { vk: wp as u16, mods, repeat: lp & (1 << 30) != 0, sys: msg == WM_SYSKEYDOWN }
}

thread_local! {
    /// The high surrogate of a character outside the BMP, held until its low half arrives (0 = none).
    static HIGH: Cell<u16> = const { Cell::new(0) };
}

/// One UTF-16 unit of a `WM_CHAR` or `WM_SYSCHAR`: a high surrogate is held in `held` until the low one completes
/// the character; a lone surrogate is dropped.
fn utf16_char(held: &mut u16, unit: u32) -> Option<char> {
    let high = std::mem::take(held);
    match unit {
        0xD800..=0xDBFF => {
            *held = unit as u16;
            None
        }
        0xDC00..=0xDFFF if high != 0 => char::decode_utf16([high, unit as u16]).next()?.ok(),
        _ => char::from_u32(unit),
    }
}

/// A `WM_CHAR` (or routed `WM_SYSCHAR`) for the open content.
fn on_char_unit(unit: u32) {
    #[cfg(feature = "owner-probe")]
    let work = crate::launcher_probe::work_start();
    let c = HIGH.with(|h| {
        let mut held = h.get();
        let c = utf16_char(&mut held, unit);
        h.set(held);
        c
    });
    let Some(c) = c else { return };
    let reply = with(|st| st.open.as_mut().map(|o| o.content.char(c))).flatten();
    if let Some(r) = reply {
        apply(r);
    }
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::work_end(work);
}

/// true if the open content takes the system keys. A busy state (re-entry) reads as false: the usual arms.
fn open_wants_sys_keys() -> bool {
    STATE
        .with(|s| s.try_borrow().ok().and_then(|g| g.open.as_ref().map(|o| o.content.wants_sys_keys())))
        .unwrap_or(false)
}

/// The keyboard and close messages while the open content `wants_sys_keys` (`route`); None = the usual arm.
fn sys_key_msg(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match route(msg, wp.0, open_wants_sys_keys) {
        Route::Default => None,
        Route::ToContent => {
            #[cfg(feature = "owner-probe")]
            let work = crate::launcher_probe::work_start();
            let r = on_key_ex(hwnd, msg, wp, lp);
            #[cfg(feature = "owner-probe")]
            crate::launcher_probe::work_end(work);
            Some(r)
        }
        Route::Char => {
            on_char_unit(wp.0 as u32);
            Some(LRESULT(0))
        }
        Route::Consume => Some(LRESULT(0)),
        Route::CloseRequest => {
            let reply = with(|st| st.open.as_mut().map(|o| o.content.close_request())).flatten();
            if let Some(r) = reply {
                apply(r);
            }
            Some(LRESULT(0))
        }
    }
}

/// A key down for `key_ex`, with the modifiers read once.
fn on_key_ex(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ev = key_event(msg, wp.0, lp.0, held_mods());
    let handled = with(|st| st.open.as_mut().and_then(|o| o.content.key_ex(ev))).flatten();
    if let Some(reply) = handled {
        apply(reply);
        return LRESULT(0);
    }
    if msg == WM_SYSKEYDOWN {
        // Never DefWindowProc: no menu mode, no beep, no SC_CLOSE of its own.
        return LRESULT(0);
    }
    if wp.0 == VK_ESCAPE.0 as usize {
        close_inner("Esc");
        return LRESULT(0);
    }
    // SAFETY: forwarding to the default handler with the same arguments.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = sys_key_msg(hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                // lParam: the window being activated (may be null or of another thread).
                on_inactive(HWND(lp.0 as *mut _));
            } else {
                mark_active(hwnd);
            }
            LRESULT(0)
        }
        WM_POPUP_OUTSIDE => {
            close_inner("click outside");
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let handled = with(|st| st.open.as_mut().and_then(|o| o.content.key(wp.0 as u16))).flatten();
            if let Some(reply) = handled {
                apply(reply);
                return LRESULT(0);
            }
            if wp.0 == VK_ESCAPE.0 as usize {
                close_inner("Esc");
                return LRESULT(0);
            }
            // SAFETY: forwarding to the default handler with the same arguments.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_CHAR => {
            // Characters outside the BMP arrive as two surrogates: combined by `utf16_char`.
            on_char_unit(wp.0 as u32);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, lo_i16(lp.0), hi_i16(lp.0));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            on_mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            on_lbutton_down(hwnd, lo_i16(lp.0), hi_i16(lp.0));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_lbutton_up(lo_i16(lp.0), hi_i16(lp.0));
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            on_other_button(Button::Right, lo_i16(lp.0), hi_i16(lp.0));
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            on_other_button(Button::Middle, lo_i16(lp.0), hi_i16(lp.0));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            on_wheel(hwnd, wp, lp);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if (lp.0 & 0xFFFF) as u32 == HTCLIENT && on_set_cursor(hwnd) {
                return LRESULT(1);
            }
            // SAFETY: forwarding to the default handler with the same arguments.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_TIMER => {
            match (timer_owner(wp.0), wp.0) {
                (TimerOwner::Launcher, id) => popup_launcher::on_timer(hwnd, id),
                (TimerOwner::Host, TIMER_POPUP_ANIM) => on_anim(hwnd),
                (TimerOwner::Host, TIMER_POPUP_TICK) => on_tick(hwnd),
                (TimerOwner::Host, TIMER_POPUP_FOCUS) => on_focus_retry(hwnd),
                (TimerOwner::Host, TIMER_POPUP_START_RELEASE) => on_start_release(hwnd),
                (TimerOwner::Host, TIMER_POPUP_COM_FREE) => on_com_free(hwnd),
                (TimerOwner::Host, _) => {}
            }
            LRESULT(0)
        }
        // The popup uses the bar's DPI, not its own.
        WM_DPICHANGED => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        // SAFETY: forwarding to the default handler with the same arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn mark_active(hwnd: HWND) {
    // WM_ACTIVATE is handled here WITHOUT DefWindowProc, which is what normally gives the keyboard
    // focus to an activated window: without this the popup is active but has NO focus, Windows then
    // delivers keys as WM_SYSKEYDOWN/WM_SYSCHAR and the Start menu search box receives nothing.
    // SAFETY: `hwnd` is our window and this runs on its thread (wnd_proc).
    // The result is ignored: it returns the PREVIOUS focus, which is legitimately null here.
    let _ = unsafe { SetFocus(Some(hwnd)) };
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    // Variant 1 of `ui::force_foreground` on the synthetic-F15 path: active in the dock's thread, another window in front.
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::activated(fg == hwnd);
    if fg == hwnd {
        became_foreground();
    }
}

/// The open popup is seen as the foreground window: the first time per open the content hears `focus(true)`.
fn became_foreground() {
    let reply = with(|st| {
        let o = st.open.as_mut()?;
        if o.was_active {
            return None;
        }
        o.was_active = true;
        Some(o.content.focus(true))
    })
    .flatten();
    if let Some(r) = reply {
        apply(r);
    }
}

fn on_inactive(other: HWND) {
    let kind = with(|st| st.open.as_ref().map(|o| o.kind)).flatten();
    if let Some(k) = kind {
        // A fullscreen app bouncing the foreground back right after we took it: the UI re-takes it.
        if crate::ui::fs_bounce_grace(other) {
            log!("popup: focus lost to the fullscreen app's bounce: staying open");
            return;
        }
        with(|st| {
            st.last_inactive = Some((k, now_ms()));
            if let Some(o) = st.open.as_mut() {
                // The popup closes right below: the reply has nothing left to change.
                let _ = o.content.focus(false);
            }
        });
        close_inner("focus lost");
    }
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let r = with(|st| {
        let o = st.open.as_mut()?;
        let mut reply = o.content.pointer(x, y);
        let hit = o.content.hit(x, y);
        if let Some(id) = o.pressed {
            reply = reply.merge(o.content.drag(id, x, y));
            let nh = if hit == Some(id) { hit } else { None };
            if nh != o.hot {
                o.hot = nh;
                reply = reply.merge(Reply::Redraw);
            }
        } else if hit != o.hot {
            o.hot = hit;
            reply = reply.merge(Reply::Redraw);
        }
        let need = !o.tracking;
        o.tracking = true;
        Some((reply, need))
    })
    .flatten();
    let Some((reply, need_track)) = r else { return };
    if need_track {
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `t` is initialized with the correct cbSize; `hwnd` is our window.
        if let Err(e) = unsafe { TrackMouseEvent(&mut t) } {
            log!("popup: TrackMouseEvent failed: {e}");
            with(|st| {
                if let Some(o) = st.open.as_mut() {
                    o.tracking = false;
                }
            });
        }
    }
    apply(reply);
}

fn on_mouse_leave() {
    let changed = with(|st| {
        let o = st.open.as_mut()?;
        o.tracking = false;
        let left = o.content.leave();
        if o.pressed.is_none() && o.hot.is_some() {
            o.hot = None;
            return Some(true);
        }
        Some(matches!(left, Reply::Redraw | Reply::Relayout))
    })
    .flatten();
    if changed == Some(true) {
        apply(Reply::Redraw);
    }
}

fn on_lbutton_down(hwnd: HWND, x: i32, y: i32) {
    let r = with(|st| {
        let o = st.open.as_mut()?;
        let hit = o.content.hit(x, y)?;
        o.pressed = Some(hit);
        o.hot = Some(hit);
        let reply = o.content.drag(hit, x, y).merge(Reply::Redraw);
        Some(reply)
    })
    .flatten();
    let Some(reply) = r else { return };
    // SAFETY: `hwnd` is our window; capture is released when the button is released.
    let _ = unsafe { SetCapture(hwnd) };
    apply(reply);
}

fn on_lbutton_up(x: i32, y: i32) {
    let r = with(|st| {
        let o = st.open.as_mut()?;
        let id = o.pressed.take()?;
        let hit = o.content.hit(x, y);
        o.hot = hit;
        let mut reply = o.content.release(id).merge(Reply::Redraw);
        if hit == Some(id) {
            reply = reply.merge(o.content.click(id));
        }
        Some(reply)
    })
    .flatten();
    // SAFETY: releases the capture taken in `on_lbutton_down` (harmless if there is none).
    let _ = unsafe { ReleaseCapture() };
    if let Some(reply) = r {
        apply(reply);
    }
}

fn on_other_button(which: Button, x: i32, y: i32) {
    let r = with(|st| {
        let o = st.open.as_mut()?;
        let id = o.content.hit(x, y)?;
        Some(o.content.button(id, which))
    })
    .flatten();
    if let Some(reply) = r {
        apply(reply);
    }
}

fn on_wheel(hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    // The wheel carries SCREEN coordinates (not client).
    let mut pt = POINT { x: lo_i16(lp.0), y: hi_i16(lp.0) };
    // SAFETY: `hwnd` is our window, `pt` is writable.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    let r = with(|st| {
        let o = st.open.as_mut()?;
        // High-resolution wheels: accumulate up to 120 (WHEEL_DELTA). A change of direction resets.
        if o.wheel_acc.signum() * delta.signum() < 0 {
            o.wheel_acc = 0;
        }
        o.wheel_acc += delta;
        let n = o.wheel_acc / 120;
        o.wheel_acc -= n * 120;
        if n == 0 {
            return None;
        }
        Some(o.content.wheel(n, pt.x, pt.y))
    })
    .flatten();
    if let Some(reply) = r {
        apply(reply);
    }
}

/// true if the cursor was set.
fn on_set_cursor(hwnd: HWND) -> bool {
    let mut pt = POINT::default();
    // SAFETY: `pt` is writable.
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return false;
    }
    // SAFETY: `hwnd` is our window, `pt` is writable.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    let hand = with(|st| {
        let o = st.open.as_ref()?;
        let id = o.content.hit(pt.x, pt.y)?;
        Some(o.content.clickable(id))
    })
    .flatten()
    .unwrap_or(false);
    // SAFETY: loads a system cursor (null hInstance) and sets it.
    unsafe {
        let c = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).unwrap_or_default();
        SetCursor(Some(c));
    }
    true
}

fn on_anim(hwnd: HWND) {
    let done = with(|st| {
        let o = st.open.as_mut()?;
        if o.linked {
            // The bar's animation drives the alpha (`follow_bar`): the own fade stops here.
            return Some(true);
        }
        let el = now_ms().saturating_sub(o.fade_start);
        let a = ((el * 255) / FADE_MS).min(255) as u8;
        o.alpha = a;
        Some(a == 255)
    })
    .flatten();
    let Some(done) = done else {
        // SAFETY: timer of our window; no popup open = nothing to animate.
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_ANIM) };
        return;
    };
    render(false);
    if done {
        // SAFETY: timer of our window.
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_ANIM) };
    }
}

/// The popup window (0 = none), readable while the state is borrowed.
static POPUP_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The launcher's one-shot timers (msg.rs 34-39 without 37, unused since L3 T19, and 38, unused since plan L2 D2), armed
/// on the dock window (`launcher_sys::timer_window`), which hands them over through `on_launcher_timer`. None is armed on
/// a popup window: the shared one can be destroyed.
const LAUNCHER_TIMERS: [usize; 4] = [
    crate::msg::TIMER_LAUNCHER_SHRINK,
    crate::msg::TIMER_LAUNCHER_WARM,
    crate::msg::TIMER_LAUNCHER_SAVE,
    crate::msg::TIMER_LAUNCHER_PT,
];

/// Who handles a `WM_TIMER` of the popup window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerOwner {
    /// This module (fade, tick, focus retries, Start release); an unknown id is ignored here.
    Host,
    /// `popup_launcher::on_timer`.
    Launcher,
}

fn timer_owner(id: usize) -> TimerOwner {
    if LAUNCHER_TIMERS.contains(&id) {
        TimerOwner::Launcher
    } else {
        TimerOwner::Host
    }
}

/// A `WM_TIMER` of the dock window that `ui::on_timer` does not know: true when it was one of the launcher's one-shots,
/// now handled.
pub(crate) fn on_launcher_timer(h: HWND, id: usize) -> bool {
    let ours = timer_owner(id) == TimerOwner::Launcher;
    if ours {
        popup_launcher::on_timer(h, id);
    }
    ours
}

/// The launcher's popup window, created if needed (`launcher_sys::chord_is_free` registers its probe on it).
pub(crate) fn popup_hwnd() -> Option<HWND> {
    ensure_window_for(PopupKind::Launcher)
}

/// Fires the content's `tick` once after about `ms` (re-arms the 1 s tick timer, which `close_inner`
/// kills, so nothing outlives the popup). Only meaningful while a popup is open.
pub(crate) fn tick_soon(ms: u32) {
    let h = POPUP_HWND.load(std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        // SAFETY: the window belongs to this thread; a stale handle just makes the call fail.
        unsafe {
            SetTimer(Some(HWND(h as *mut _)), TIMER_POPUP_TICK, ms.max(10), None);
        }
    }
}

/// Delay after a Start menu close before its catalog and icons are released (`TIMER_POPUP_START_RELEASE`).
const START_RELEASE_MS: u32 = 60_000;

/// Delay after the Start data release before the trim is re-armed for COM's second `CoFreeUnusedLibraries` call: more
/// than COM's 10 minute default delay for free-threaded DLLs (`TIMER_POPUP_COM_FREE`).
const COM_FREE_MS: u32 = 660_000;

/// Whether the release timer may free the Start data: not while a Start menu is open.
fn start_release_due(open: Option<PopupKind>) -> bool {
    open != Some(PopupKind::StartMenu)
}

/// Arms (or restarts) the one-shot that releases the Start menu data; called when it closes.
pub(crate) fn arm_start_release() {
    let Some(hwnd) = with(|st| st.hwnd) else { return };
    if hwnd.0.is_null() {
        return;
    }
    // SAFETY: `hwnd` is our window; the callback is in its wnd_proc.
    if unsafe { SetTimer(Some(hwnd), TIMER_POPUP_START_RELEASE, START_RELEASE_MS, None) } == 0 {
        log!("popup: SetTimer(start release) failed");
    }
}

/// `TIMER_POPUP_START_RELEASE`: frees the Start menu data unless a Start menu is open again.
fn on_start_release(hwnd: HWND) {
    // SAFETY: timer of our window.
    let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_START_RELEASE) };
    if start_release_due(current()) {
        popup_startmenu::release();
        // The shell thread's COM libraries are freed with the release; the catalog and icons freed here are trimmed in
        // the next hidden period (M1 F3), and the free-threaded ones are unloaded by a second pass 11 minutes on (F4).
        crate::mem::rearm();
        // SAFETY: our window; the callback is in its wnd_proc.
        if unsafe { SetTimer(Some(hwnd), TIMER_POPUP_COM_FREE, COM_FREE_MS, None) } == 0 {
            log!("popup: SetTimer(com free) failed");
        }
        // With the launcher on its release also trims at once while the bar is hidden.
        if popup_launcher::is_on() {
            crate::mem::released_data();
        }
    }
}

/// `TIMER_POPUP_COM_FREE`: one-shot; the idle trim runs again, and its MTA pass unloads what the first one marked.
fn on_com_free(hwnd: HWND) {
    // SAFETY: timer of our window.
    let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_COM_FREE) };
    crate::mem::rearm();
}

/// What follows a foreground attempt of `on_focus_retry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retry {
    /// Try again after `FOCUS_RETRY_MS`.
    Again,
    /// No more tries; the popup stays (the tick safety net remains).
    Stop,
    /// No more tries and the content closes without the keyboard (`focus_lost_closes`).
    Close,
    /// The attempt left the popup in front: the content hears `focus(true)` now. `force_foreground` can win with no
    /// WM_ACTIVATE that sees the popup in front (variant 1 activates it in the dock's thread while another window keeps
    /// the foreground, variant 2 then wins without a new activation), and nothing else would tell the content before
    /// the 1 s tick.
    Focused,
}

/// The outcome of a foreground attempt: `tries_left` after it, `focused` its result. A won attempt is `Focused`
/// whatever is left: the caller tells the content at once (B1: the type-ahead window is 500 ms, the tick 1 s).
fn retry_outcome(tries_left: u8, focused: bool, focus_lost_closes: bool) -> Retry {
    if focused {
        Retry::Focused
    } else if tries_left > 0 {
        Retry::Again
    } else if focus_lost_closes {
        Retry::Close
    } else {
        Retry::Stop
    }
}

/// `TIMER_POPUP_FOCUS`: the popup is open but did not get the foreground in `open`. Retries while
/// the foreground is still the window that was in front at open time (or null, or ours): if the
/// user has meanwhile moved to another window, the popup is not pushed in front of it, and a content
/// that `focus_lost_closes` closes then, or when the last try fails (`retry_gate`).
fn on_focus_retry(hwnd: HWND) {
    // SAFETY: timer of our window.
    let _ = unsafe { KillTimer(Some(hwnd), TIMER_POPUP_FOCUS) };
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg == hwnd {
        became_foreground();
        return;
    }
    let go = with(|st| {
        let o = st.open.as_mut()?;
        let kept =
            fg.0.is_null() || fg == o.fg_at_open || crate::ui::is_own_window(fg) || crate::ui::fs_bounce_grace(fg);
        let closes = o.content.focus_lost_closes();
        let gate = retry_gate(o.focus_tries, kept, closes);
        if gate.is_none() {
            o.focus_tries -= 1;
        }
        Some((gate, o.focus_tries, closes))
    })
    .flatten();
    let Some((gate, left, closes)) = go else { return };
    let outcome = gate.unwrap_or_else(|| {
        let ok = crate::ui::force_foreground(hwnd);
        log!("popup: foreground retry ({left} left) -> {ok}");
        retry_outcome(left, ok, closes)
    });
    match outcome {
        Retry::Again => arm_retry(hwnd),
        Retry::Focused => became_foreground(),
        Retry::Stop => {}
        Retry::Close => close_inner("no foreground"),
    }
}

/// Before a foreground retry: None makes the attempt, Some ends the retries without one. `kept`: the foreground is
/// still the window in front at open time, null, ours or a fullscreen bounce. A content that `focus_lost_closes`
/// closes at once when the user went elsewhere: back in the window of the open before the tick, they would find it
/// shown, topmost and without the keyboard, its type-ahead kept for a later click to replay.
fn retry_gate(tries_left: u8, kept: bool, closes: bool) -> Option<Retry> {
    if tries_left > 0 && kept {
        None
    } else {
        Some(retry_outcome(0, false, closes))
    }
}

/// Arms the next foreground retry. A timer Windows refuses ends the retries (`focus_tries` 0: the tick's safety net
/// knows none will come).
fn arm_retry(hwnd: HWND) {
    // SAFETY: `hwnd` is our window; the callback is in its wnd_proc.
    if unsafe { SetTimer(Some(hwnd), TIMER_POPUP_FOCUS, FOCUS_RETRY_MS, None) } == 0 {
        log!("popup: SetTimer(focus) failed");
        with(|st| {
            if let Some(o) = st.open.as_mut() {
                o.focus_tries = 0;
            }
        });
    }
}

/// The tick's safety net for a popup never seen in front, which no WA_INACTIVE will close: true closes it. `moved`:
/// the foreground is another window than the one in front at open time (and no fullscreen bounce). A content that
/// `focus_lost_closes` also closes once no retry is left to come.
fn unseen_tick_closes(moved: bool, tries_left: u8, closes: bool) -> bool {
    moved || (closes && tries_left == 0)
}

fn on_tick(hwnd: HWND) {
    // Back to the regular 1 s period (a `tick_soon` may have shortened it).
    // SAFETY: `hwnd` is our window.
    unsafe {
        SetTimer(Some(hwnd), TIMER_POPUP_TICK, 1000, None);
    }
    let r = with(|st| {
        let o = st.open.as_mut()?;
        Some((o.content.tick(), o.was_active, o.fg_at_open, o.focus_tries, o.content.focus_lost_closes()))
    })
    .flatten();
    let Some((reply, was_active, fg_at_open, tries, closes)) = r else { return };
    if was_active {
        // Activation was seen: if the foreground is now another window, WA_INACTIVE was missed.
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        if fg != hwnd && !fg.0.is_null() && !crate::ui::fs_bounce_grace(fg) {
            close_inner("foreground changed");
            return;
        }
    } else {
        // Safety net: the foreground steal can fail and the popup would never receive WA_INACTIVE.
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        if fg == hwnd {
            became_foreground();
        } else if unseen_tick_closes(fg != fg_at_open && !crate::ui::fs_bounce_grace(fg), tries, closes) {
            close_inner("foreground changed");
            return;
        }
    }
    apply(reply);
}

// ------------------------------------------------------------------ helpers for the contents

/// Hover/pressed plate of a row (none if `id` is not hovered). Returns true if `id` is
/// hovered: the caller brightens text/glyphs with `lift`.
pub(crate) fn row_plate(s: &mut Surface, rc: RECT, id: u32, hot: Option<u32>, pressed: Option<u32>) -> bool {
    if hot != Some(id) {
        return false;
    }
    let a = if pressed == Some(id) { crate::surface::PRESS_ALPHA } else { crate::surface::HOVER_ALPHA };
    let r = s.metrics().px(crate::surface::ROW_RADIUS);
    s.plate(rc, r, a);
    true
}

/// Alpha of a row's text/glyph: under the mouse it goes up one step (max 255).
pub(crate) fn lift(base: u32, hot: bool) -> u32 {
    if hot {
        (base + 25).min(255)
    } else {
        base
    }
}

/// Switch shared by the popups (32x18, knob 12, margin 3), top-left corner `(x, y)`.
/// On: white track and black knob. Off: dim track with a 1 px outline and gray knob.
pub(crate) fn toggle_switch(s: &mut Surface, x: i32, y: i32, on: bool) {
    let m = s.metrics();
    let (w, h, knob, inset) = (m.px(32), m.px(18), m.px(12), m.px(3));
    let track = crate::surface::rect_wh(x, y, w, h);
    let ky = y + (h - knob) / 2;
    if on {
        s.fill_rr(track, h / 2, crate::surface::WHITE, 235);
        s.fill_rr(crate::surface::rect_wh(x + w - inset - knob, ky, knob, knob), knob / 2, (0, 0, 0), 255);
    } else {
        s.fill_rr(track, h / 2, crate::surface::WHITE, 40);
        s.stroke_rr(track, h / 2, m.px(1).max(1), 70);
        s.fill_rr(crate::surface::rect_wh(x + inset, ky, knob, knob), knob / 2, crate::surface::WHITE, 160);
    }
}

/// Horizontal separator (1 logical px thick) between `x0` and `x1` at ordinate `y`.
pub(crate) fn hline(s: &mut Surface, x0: i32, x1: i32, y: i32) {
    let t = s.metrics().px(1).max(1);
    s.fill_rr(RECT { left: x0, top: y, right: x1, bottom: y + t }, 0, crate::surface::WHITE, crate::surface::SEP_ALPHA);
}

/// Y coordinate to vertically center a line of height `lh` inside `rc`.
pub(crate) fn center_y(rc: &RECT, lh: i32) -> i32 {
    rc.top + ((rc.bottom - rc.top) - lh) / 2
}

/// Track thickness of the slider shared by the popups (logical px).
const SLIDER_TRACK_H: i32 = 4;

/// Value in `lo..=hi` (rounded to the nearest step) for pointer x on slider `rc`. The center of
/// the knob (radius `knob_r`) travels between `rc.left + knob_r` and `rc.right - knob_r`.
/// Shared by the Settings window and the Audio popup.
pub(crate) fn slider_value_at(rc: &RECT, knob_r: i32, lo: i32, hi: i32, x: i32) -> i32 {
    let (l, r) = (rc.left + knob_r, rc.right - knob_r);
    let span = (r - l).max(1);
    lo + ((x - l).clamp(0, span) * (hi - lo) + span / 2) / span
}

/// Slider shared by the popups at value `v` in `lo..=hi`: dim rail across `rc`, bright fill up
/// to the knob, white knob of radius `knob_r` (same travel as `slider_value_at`).
pub(crate) fn paint_slider(s: &mut Surface, rc: &RECT, knob_r: i32, lo: i32, hi: i32, v: i32) {
    use crate::surface::WHITE;
    let (tl, tr) = (rc.left + knob_r, rc.right - knob_r);
    let cy = (rc.top + rc.bottom) / 2;
    let th = s.metrics().px(SLIDER_TRACK_H);
    let rail = RECT { left: rc.left, top: cy - th / 2, right: rc.right, bottom: cy - th / 2 + th };
    s.fill_rr(rail, th / 2, WHITE, 60);
    let kx = tl + (tr - tl) * (v - lo) / (hi - lo).max(1);
    s.fill_rr(RECT { right: kx, ..rail }, th / 2, WHITE, 230);
    s.fill_rr(RECT { left: kx - knob_r, top: cy - knob_r, right: kx + knob_r, bottom: cy + knob_r }, knob_r, WHITE, 255);
}

/// Opens `target` (e.g. `ms-settings:...`) on the shell thread.
pub(crate) fn launch(target: &str) {
    crate::shell_thread::request(crate::shell_thread::ShellRequest::Launch { target: target.to_string(), args: None });
}

/// Monotonic milliseconds (for deadlines and content throttling).
pub(crate) fn tick_ms() -> u64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{TIMER_LAUNCHER_PT, TIMER_LAUNCHER_SAVE, TIMER_LAUNCHER_SHRINK, TIMER_LAUNCHER_WARM};
    use windows::Win32::UI::WindowsAndMessaging::WM_KEYUP;

    #[test]
    fn max_popup_size_leaves_the_bar_and_two_margins() {
        let work = RECT { left: 0, top: 0, right: 1366, bottom: 768 };
        let top_bar = RECT { left: 0, top: 10, right: 1366, bottom: 60 };
        let bottom_bar = RECT { left: 0, top: 708, right: 1366, bottom: 758 };
        for (dpi, want) in [(96, (1334, 686)), (144, (1318, 675)), (192, (1302, 664))] {
            assert_eq!(max_popup_size(&work, &top_bar, true, dpi), want, "top bar at {dpi}");
            assert_eq!(max_popup_size(&work, &bottom_bar, false, dpi), want, "bottom bar at {dpi}");
        }
        let work = RECT { left: 948, top: 2160, right: 2868, bottom: 2880 };
        let bar = RECT { left: 948, top: 2170, right: 2868, bottom: 2220 };
        assert_eq!(max_popup_size(&work, &bar, true, 96), (1888, 638));
    }

    #[test]
    fn limit_for_keeps_the_old_margin_for_other_popups() {
        let work = RECT { left: 0, top: 0, right: 1366, bottom: 768 };
        let bar = RECT { left: 0, top: 10, right: 1366, bottom: 60 };
        assert_eq!(limit_for(false, &work, &bar, true, 96), (1366 - 16, 768 - 16));
        assert_eq!(limit_for(false, &work, &bar, true, 144), (1366 - 24, 768 - 24));
        assert_eq!(limit_for(true, &work, &bar, true, 96), (1334, 686));
    }

    #[test]
    fn popup_is_clamped_to_the_work_area() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        assert_eq!(fit_to_work(1000, 600, &work, 16), (1000, 600), "fits: unchanged");
        assert_eq!(fit_to_work(1280, 1240, &work, 16), (1280, 1008), "1080p at 200 %: 640x620 logical");
        assert_eq!(fit_to_work(5000, 5000, &work, 16), (1888, 1008));
        assert_eq!(fit_to_work(50, 50, &RECT { left: 0, top: 0, right: 40, bottom: 40 }, 16), (8, 8), "40 - 2 * 16");
        assert_eq!(
            fit_to_work(10, 10, &RECT { left: 0, top: 0, right: 20, bottom: 20 }, 16),
            (1, 1),
            "20 - 32 < 0: never zero or negative"
        );
        assert_eq!(fit_to_work(10, 10, &RECT { left: 0, top: 0, right: 10, bottom: 10 }, 16), (1, 1), "never zero");
    }

    #[test]
    fn launcher_kind_makes_a_launcher_content() {
        let c = make_content(
            PopupKind::Launcher,
            VolumeState::default(),
            crate::net::NetInfo::default(),
            crate::battery::BatteryStatus::NONE,
        );
        assert_eq!(c.kind(), PopupKind::Launcher);
    }

    #[test]
    fn launcher_window_name() {
        assert_eq!(window_name_for(PopupKind::Launcher), "Launcher");
        assert_eq!(window_name_for(PopupKind::StartMenu), "Dock popup");
        assert_eq!(window_name_for(PopupKind::Audio), "Dock popup");
    }

    #[test]
    fn centred_position() {
        let c = Centred { work: RECT { left: 0, top: 0, right: 1920, bottom: 1040 }, dpi: 96, top_percent: 24 };
        assert_eq!(centred_max(&c, 8), (1904, 1040 - 8 - 249));
        assert_eq!(centred_place(&c, 8, 640, 422), (640, 249, 640, 422), "1040 * 24 % = 249.6: the top edge at 249");
        // 720 high: the panel grows down to 8 px above the work-area bottom and no further, it never moves up.
        let small = Centred { work: RECT { left: 0, top: 0, right: 1280, bottom: 720 }, ..c };
        assert_eq!(centred_place(&small, 8, 640, 600), (320, 172, 640, 540));
        assert_eq!(centred_place(&small, 8, 2000, 100), (8, 172, 1264, 100), "never wider than the work area");
        // A monitor left of the primary one with a work area that does not start at 0.
        let left = Centred { work: RECT { left: -1280, top: 100, right: 0, bottom: 820 }, ..c };
        assert_eq!(centred_place(&left, 8, 640, 62), (-960, 272, 640, 62));
    }

    #[test]
    fn launcher_opens_at_full_alpha() {
        assert_eq!(initial_alpha(PopupKind::Launcher), 255, "no fade: the field is there at once");
        assert_eq!(initial_alpha(PopupKind::StartMenu), 0);
        assert_eq!(initial_alpha(PopupKind::Audio), 0);
    }

    #[test]
    fn sys_key_routing() {
        let alt = usize::from(VK_MENU.0);
        let cases: [(u32, usize, Route); 11] = [
            (WM_KEYDOWN, 0x41, Route::ToContent),
            (WM_SYSKEYDOWN, 0x41, Route::ToContent),
            (WM_SYSKEYDOWN, alt, Route::ToContent),
            (WM_SYSCHAR, 0x61, Route::Char),
            (WM_SYSKEYUP, alt, Route::Consume),
            (WM_SYSKEYUP, 0x41, Route::Default),
            (WM_SYSCOMMAND, SC_KEYMENU as usize, Route::Consume),
            (WM_SYSCOMMAND, SC_KEYMENU as usize | 0x3, Route::Consume),
            (WM_SYSCOMMAND, 0xF010, Route::Default),
            (WM_CHAR, 0x61, Route::Default),
            (WM_KEYUP, 0x41, Route::Default),
        ];
        for (msg, wp, want) in cases {
            assert_eq!(route(msg, wp, || true), want, "launcher: msg {msg:#x} wp {wp:#x}");
            assert_eq!(route(msg, wp, || false), Route::Default, "other kinds: msg {msg:#x} wp {wp:#x}");
        }
        // The open content is asked only for a routed message: every other one costs no state borrow.
        let asked = Cell::new(0);
        let ask = || {
            asked.set(asked.get() + 1);
            true
        };
        for (msg, wp) in [(WM_MOUSEMOVE, 0), (WM_TIMER, 30), (WM_CHAR, 0x61), (WM_KEYUP, 0x41), (WM_SYSKEYUP, 0x41)] {
            assert_eq!(route(msg, wp, ask), Route::Default, "msg {msg:#x}");
        }
        assert_eq!(asked.get(), 0);
        assert_eq!(route(WM_CLOSE, 0, ask), Route::CloseRequest);
        assert_eq!(asked.get(), 1);
        // The KeyEvent of a key message: lParam bit 30 is the auto-repeat, WM_SYSKEYDOWN is a system key.
        let ev = key_event(WM_SYSKEYDOWN, 0x46, 1 << 30, Mods::ALT);
        assert_eq!(ev, KeyEvent { vk: 0x46, mods: Mods::ALT, repeat: true, sys: true });
        assert_eq!(
            key_event(WM_KEYDOWN, 0x0D, 1, Mods::NONE),
            KeyEvent { vk: 0x0D, mods: Mods::NONE, repeat: false, sys: false }
        );
        assert_eq!(mods_of(true, true, false, false), Mods::CTRL.with(Mods::SHIFT));
        assert_eq!(mods_of(false, false, true, true), Mods::ALT.with(Mods::WIN));
        assert_eq!(mods_of(false, false, false, false), Mods::NONE);
        // Only the launcher takes the system keys.
        assert!(popup_launcher::new().wants_sys_keys());
        assert!(!popup_start::new().wants_sys_keys());
    }

    #[test]
    fn alt_f4_and_wm_close_are_close_requests() {
        assert_eq!(route(WM_SYSCOMMAND, SC_CLOSE as usize, || true), Route::CloseRequest);
        assert_eq!(
            route(WM_SYSCOMMAND, SC_CLOSE as usize | 0x2, || true),
            Route::CloseRequest,
            "low 4 bits are the system's"
        );
        assert_eq!(route(WM_CLOSE, 0, || true), Route::CloseRequest);
        assert_eq!(route(WM_SYSCOMMAND, SC_CLOSE as usize, || false), Route::Default, "other kinds: as today");
        assert_eq!(route(WM_CLOSE, 0, || false), Route::Default);
        // The stub launcher closes like the final Esc (L1a-15 hands it to the session); the default is a close.
        assert_eq!(popup_launcher::new().close_request(), Reply::Close);
        assert_eq!(popup_start::new().close_request(), Reply::Close);
    }

    #[test]
    fn surrogate_pairs_combine() {
        let mut held = 0u16;
        assert_eq!(utf16_char(&mut held, 0x61), Some('a'));
        // U+1F600 arrives as D83D DE00.
        assert_eq!(utf16_char(&mut held, 0xD83D), None);
        assert_eq!(utf16_char(&mut held, 0xDE00), Some('\u{1F600}'));
        assert_eq!(held, 0);
        // A lone low surrogate is dropped.
        assert_eq!(utf16_char(&mut held, 0xDE00), None);
        // A high surrogate followed by anything but a low one is dropped; the next character passes.
        assert_eq!(utf16_char(&mut held, 0xD83D), None);
        assert_eq!(utf16_char(&mut held, 0x62), Some('b'));
        assert_eq!(utf16_char(&mut held, 0xD83D), None);
        assert_eq!(utf16_char(&mut held, 0xD83C), None, "a second high replaces the first");
        assert_eq!(utf16_char(&mut held, 0xDF89), Some('\u{1F389}'));
        assert_eq!(utf16_char(&mut held, 0xE9), Some('\u{E9}'));
    }

    #[test]
    fn focus_retry_outcomes() {
        assert_eq!(retry_outcome(3, false, false), Retry::Again);
        assert_eq!(retry_outcome(3, false, true), Retry::Again);
        assert_eq!(retry_outcome(2, true, true), Retry::Focused, "got the foreground");
        assert_eq!(retry_outcome(0, true, true), Retry::Focused);
        assert_eq!(retry_outcome(0, true, false), Retry::Focused);
        assert_eq!(retry_outcome(0, false, true), Retry::Close, "the launcher never sits there without the keyboard");
        assert_eq!(retry_outcome(0, false, false), Retry::Stop, "another kind stays (the tick safety net remains)");
        assert!(popup_launcher::new().focus_lost_closes());
        assert!(!popup_start::new().focus_lost_closes());
    }

    /// A launcher never seen in front never stays shown without the keyboard: a user who moved to another window
    /// during the retries (and may come back to the window of the open before the tick) gets no attempt and the
    /// launcher closes at once; the tick closes one whose retries are over. Another kind keeps its rules.
    #[test]
    fn an_unfocused_launcher_never_stays() {
        assert_eq!(retry_gate(3, true, true), None, "still the window of the open: the attempt");
        assert_eq!(retry_gate(3, true, false), None);
        assert_eq!(retry_gate(3, false, true), Some(Retry::Close), "the user went elsewhere: the launcher closes now");
        assert_eq!(retry_gate(3, false, false), Some(Retry::Stop), "another kind is not pushed in front, and stays");
        assert_eq!(retry_gate(0, true, true), Some(Retry::Close), "no try left");
        assert_eq!(retry_gate(0, true, false), Some(Retry::Stop));
        assert!(unseen_tick_closes(true, 3, false), "the foreground moved: every kind closes");
        assert!(unseen_tick_closes(true, 0, true));
        assert!(unseen_tick_closes(false, 0, true), "back in the window of the open, no retry to come: it closes");
        assert!(!unseen_tick_closes(false, 2, true), "retries still to come: they decide");
        assert!(!unseen_tick_closes(false, 0, false), "another kind stays");
    }

    #[test]
    fn a_won_foreground_is_heard_at_once() {
        // B1: a foreground attempt that wins is heard now, never at the 1 s tick.
        for left in 0..=FOCUS_RETRIES {
            for closes in [false, true] {
                assert_eq!(retry_outcome(left, true, closes), Retry::Focused, "{left} {closes}");
            }
        }
    }

    #[test]
    fn close_unless_launcher_keeps_the_launcher() {
        assert!(!host_close_applies(Some(PopupKind::Launcher)), "the bar hides or moves: the launcher stays");
        assert!(host_close_applies(Some(PopupKind::StartMenu)));
        assert!(host_close_applies(Some(PopupKind::Audio)));
        assert!(host_close_applies(None));
    }

    #[test]
    fn launcher_timer_ids_forwarded() {
        assert_eq!(
            LAUNCHER_TIMERS,
            [TIMER_LAUNCHER_SHRINK, TIMER_LAUNCHER_WARM, TIMER_LAUNCHER_SAVE, TIMER_LAUNCHER_PT]
        );
        for id in [34, 35, 36, 39] {
            assert_eq!(timer_owner(id), TimerOwner::Launcher, "timer {id}");
        }
        assert_eq!(timer_owner(37), TimerOwner::Host, "37 is unused since L3 T19 (the 10-minute release)");
        assert_eq!(timer_owner(38), TimerOwner::Host, "38 is unused since plan L2 D2 (the files worker's 60 s linger)");
        for id in
            [TIMER_POPUP_ANIM, TIMER_POPUP_TICK, TIMER_POPUP_FOCUS, TIMER_POPUP_START_RELEASE, TIMER_POPUP_COM_FREE]
        {
            assert_eq!(timer_owner(id), TimerOwner::Host, "timer {id}");
        }
        assert_eq!((TIMER_POPUP_ANIM, TIMER_POPUP_START_RELEASE), (30, 33));
        assert_eq!(timer_owner(40), TimerOwner::Host);
        assert_eq!(timer_owner(29), TimerOwner::Host);
    }

    #[test]
    fn window_for_kind_slots() {
        assert_eq!(window_for_kind(PopupKind::Launcher, true), Slot::Launcher, "S-OWNER failed: its own window");
        assert_eq!(window_for_kind(PopupKind::Launcher, false), Slot::Shared, "S-OWNER passed: the shared window");
        for kind in [PopupKind::StartMenu, PopupKind::Audio, PopupKind::AppMenu] {
            assert_eq!(window_for_kind(kind, true), Slot::Shared, "{kind:?}");
            assert_eq!(window_for_kind(kind, false), Slot::Shared, "{kind:?}");
        }
    }

    #[test]
    fn start_data_is_released_only_when_no_start_menu_is_open() {
        assert!(start_release_due(None));
        assert!(start_release_due(Some(PopupKind::Power)));
        assert!(!start_release_due(Some(PopupKind::StartMenu)));
    }
}
