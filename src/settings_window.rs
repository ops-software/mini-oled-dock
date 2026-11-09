//! Settings window (`DockSettings`): a normal, resizable top-level window of the dock process with
//! the pages of `settings_pages`, laid out and painted by `settings_view`.
//!
//! * Opened by `WM_DOCK_OPEN_SETTINGS` (`open`): centred on the bar's display at the saved logical
//!   size (`settings_window_size`, clamped to the work area), never topmost, one instance (opening it
//!   again restores and activates the open one). No owner: it can sit behind the bar and the popups.
//! * Towards the bar it behaves like an app window (`ui::is_own_window` is false for it): a Win tap
//!   while it is in front shows the bar and gives the focus back to it on hide, a popup is not pushed
//!   in front of it, and it is not a bar app (`apps::excluded_class`).
//! * DWM dark frame in the config background colour (re-applied when the colour changes).
//! * Every change applies at once: a click is saved with `config::modify`; a slider or picker drag
//!   only sets the value in memory (`config::set`), rebuilds the bar at most every 30 ms
//!   (`WM_DOCK_CONFIG_APPLY`) and writes the file on release, on close or when the dock exits
//!   (`Pending`).
//! * Keyboard: Esc closes; Tab / Shift+Tab move the focus between the navigation and the page; Up and
//!   Down change page (navigation) or scroll one row (page). The wheel scrolls the page, over a slider
//!   too.
//! * Nothing exists while it is closed: the state is dropped, the surface freed, the class
//!   unregistered and its timers killed.
//!
//! THREADING: UI thread only. State is thread-local and taken with `try_borrow_mut`; a re-entry is
//! logged and ignored. No borrow is held across `SetWindowPos`, `ShowWindow`, `DestroyWindow`,
//! `SetCapture`/`ReleaseCapture` or `ui::force_foreground`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Arc;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, ScreenToClient, HMONITOR, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_SHIFT,
    VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, IsIconic, KillTimer, LoadCursorW,
    PostMessageW, RegisterClassExW, SetCursor, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, UnregisterClassW,
    CS_HREDRAW, CS_VREDRAW, HTCLIENT, IDC_ARROW, IDC_HAND, MINMAXINFO, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOW, WHEEL_DELTA, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_MAXIMIZEBOX,
    WS_OVERLAPPEDWINDOW,
};

use crate::config::{self, Background, Config, SETTINGS_WINDOW_MIN};
use crate::i18n::tr;
use crate::log;
use crate::msg::{TIMER_SETTINGS_APPLY, TIMER_SETTINGS_BAR, TIMER_SETTINGS_TICK, WM_DOCK_CONFIG_APPLY};
use crate::popup_tray::TrayRow;
use crate::render::Metrics;
use crate::settings_pages::{
    self, apply_edit, background_of, preset_index, rgb_to_hsv, same_rows, Action, Control, Ctx, Edit,
    Effect, Memory, Page, Row,
};
use crate::settings_view::{self, decode, hue_at, sv_at, BarPixels, Focus, Frame, Geometry, Hit, PART_HUE, PART_SV};
use crate::shell_thread::{self, ShellRequest};
use crate::surface::Surface;
use crate::ui::Screen;
use crate::update::UpdateStatus;

/// Window class; also the class `apps` never lists as a bar app.
pub(crate) const CLASS: &str = "DockSettings";
const CLASS_W: PCWSTR = w!("DockSettings");
/// Bar rebuilds while dragging: at most one per this many ms.
const APPLY_THROTTLE_MS: u64 = 30;
const TICK_MS: u32 = 1000;
/// `WM_MOUSELEAVE` (defined here, as in `picker`, to avoid depending on the feature that exports it).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The open window's handle (0 = closed), readable from any thread (`is_window`).
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);

fn style() -> WINDOW_STYLE {
    WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 & !WS_MAXIMIZEBOX.0)
}

fn window_title() -> &'static str {
    tr("Dock settings", "Impostazioni dock")
}

// ------------------------------------------------------------------ pure parts

/// Outer rect of the window: client of `logical` px at `dpi`, at most the work area minus the frame
/// (`frame` = `AdjustWindowRectExForDpi` of an empty rect) and never below the minimum, centred on
/// the work area; on an axis where it is bigger than the work area it starts at the work area's edge,
/// so the title bar stays reachable.
fn initial_rect(work: RECT, dpi: u32, logical: (i32, i32), frame: RECT) -> RECT {
    let m = Metrics { dpi };
    let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
    let (ww, wh) = (work.right - work.left, work.bottom - work.top);
    let cw = m.px(logical.0).min(ww - fw).max(m.px(SETTINGS_WINDOW_MIN.0));
    let ch = m.px(logical.1).min(wh - fh).max(m.px(SETTINGS_WINDOW_MIN.1));
    let (w, h) = (cw + fw, ch + fh);
    let left = if w > ww { work.left } else { work.left + (ww - w) / 2 };
    let top = if h > wh { work.top } else { work.top + (wh - h) / 2 };
    RECT { left, top, right: left + w, bottom: top + h }
}

/// Client size in logical px (96 DPI), rounded and raised to at least the minimum.
fn logical_size(client_w: i32, client_h: i32, dpi: u32) -> [u16; 2] {
    let l =
        |v: i32, min: i32| ((v as f32 * 96.0 / dpi.max(1) as f32).round() as i32).clamp(min, u16::MAX as i32) as u16;
    [l(client_w, SETTINGS_WINDOW_MIN.0), l(client_h, SETTINGS_WINDOW_MIN.1)]
}

/// Smallest outer size of the window at `dpi`.
fn min_track(dpi: u32, frame: RECT) -> (i32, i32) {
    let m = Metrics { dpi };
    (m.px(SETTINGS_WINDOW_MIN.0) + frame.right - frame.left, m.px(SETTINGS_WINDOW_MIN.1) + frame.bottom - frame.top)
}

/// `bg` as a COLORREF (0x00BBGGRR).
fn colorref(bg: Background) -> u32 {
    let (r, g, b) = bg.rgb();
    r | (g << 8) | (b << 16)
}

#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    None,
    Close,
    Redraw,
    Goto(Page),
    Scroll(i32),
}

/// Key `vk` with the focus on `focus` and `page` shown: the new focus and what to do. With two focus
/// areas Tab and Shift+Tab both move to the other one.
fn key_action(vk: u16, shift: bool, focus: Focus, page: Page) -> (Focus, KeyAction) {
    let _ = shift;
    let i = Page::ALL.iter().position(|p| *p == page).unwrap_or(0);
    let a = if vk == VK_ESCAPE.0 {
        KeyAction::Close
    } else if vk == VK_TAB.0 {
        let other = if focus == Focus::Nav { Focus::Page } else { Focus::Nav };
        return (other, KeyAction::Redraw);
    } else if vk == VK_UP.0 || vk == VK_DOWN.0 {
        let down = vk == VK_DOWN.0;
        match focus {
            Focus::Page => KeyAction::Scroll(if down { 1 } else { -1 }),
            Focus::Nav => {
                let j = if down { i.checked_add(1) } else { i.checked_sub(1) };
                j.and_then(|j| Page::ALL.get(j)).map_or(KeyAction::None, |p| KeyAction::Goto(*p))
            }
        }
    } else {
        KeyAction::None
    };
    (focus, a)
}

/// A value set in memory and not yet written, and a bar rebuild the throttle skipped.
#[derive(Default)]
struct Pending {
    unsaved: bool,
    apply_pending: bool,
    last_apply: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum Finish {
    Nothing,
    Save,
    Apply,
    SaveAndApply,
}

impl Pending {
    /// A change at `now`: `save` = already written by `config::modify` (a click), otherwise set in
    /// memory by a drag move. true = post `WM_DOCK_CONFIG_APPLY` now; false = skipped by the
    /// throttle (the caller arms `TIMER_SETTINGS_APPLY`).
    fn changed(&mut self, save: bool, now: u64) -> bool {
        if save {
            // `config::modify` writes the whole snapshot: an unsaved drag value is saved with it.
            self.unsaved = false;
            self.apply_pending = false;
            self.last_apply = now;
            return true;
        }
        self.unsaved = true;
        if now.saturating_sub(self.last_apply) >= APPLY_THROTTLE_MS {
            self.last_apply = now;
            self.apply_pending = false;
            true
        } else {
            self.apply_pending = true;
            false
        }
    }

    /// End of a drag (release, close, exit): what is still owed to the file and the bar.
    fn finish(&mut self) -> Finish {
        match (std::mem::take(&mut self.unsaved), std::mem::take(&mut self.apply_pending)) {
            (true, true) => Finish::SaveAndApply,
            (true, false) => Finish::Save,
            (false, true) => Finish::Apply,
            (false, false) => Finish::Nothing,
        }
    }

    /// ms until the skipped rebuild is due (plus a little slack).
    fn flush_in(&self, now: u64) -> u32 {
        (APPLY_THROTTLE_MS.saturating_sub(now.saturating_sub(self.last_apply)) + 5) as u32
    }
}

/// ms until a stale mini bar may be rebuilt (it was last built at `built`), or None = now. Each
/// rebuild paints a whole bar offscreen, so a drag or a live resize rebuilds it at most every
/// `APPLY_THROTTLE_MS`; meanwhile the old bar stays, unless it no longer `fits` the card.
fn bar_wait(built: u64, now: u64, fits: bool) -> Option<u32> {
    let since = now.saturating_sub(built);
    (fits && since < APPLY_THROTTLE_MS).then(|| (APPLY_THROTTLE_MS - since + 5) as u32)
}

// ------------------------------------------------------------------ state

/// The mini bar of the Appearance card and what it was drawn for.
struct MiniBar {
    cfg: Arc<Config>,
    dpi: u32,
    max_w: i32,
    px: BarPixels,
}

struct Win {
    hwnd: HWND,
    surf: Surface,
    dpi: u32,
    client: (i32, i32),
    page: Page,
    focus: Focus,
    scroll: i32,
    /// Wheel delta not yet turned into a notch.
    wheel: i32,
    rows: Vec<Row>,
    geo: Geometry,
    hot: Option<u32>,
    pressed: Option<u32>,
    /// The pressed part is a slider or the picker: moves drag it.
    dragging: bool,
    tracking: bool,
    hsv: (f32, f32, f32),
    picker_open: bool,
    chooser_open: bool,
    tray: Vec<TrayRow>,
    screens: Vec<Screen>,
    update: UpdateStatus,
    import: crate::start_import::Status,
    memory: Memory,
    data_dir: Option<String>,
    log_path: Option<String>,
    pending: Pending,
    bar: Option<MiniBar>,
    /// `tick_ms` of the last mini bar rebuild (see `bar_wait`).
    bar_built: u64,
}

#[derive(Default)]
struct State {
    win: Option<Win>,
    registered: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut g) => Some(f(&mut g)),
        Err(_) => {
            log!("settings: state re-entry (ignored)");
            None
        }
    })
}

fn with_win<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    with(|st| st.win.as_mut().map(f)).flatten()
}

/// true while the Settings window is open.
pub fn is_open() -> bool {
    OPEN_HWND.load(Relaxed) != 0
}

/// true if `h` is the Settings window (any thread).
pub fn is_window(h: HWND) -> bool {
    let v = OPEN_HWND.load(Relaxed);
    v != 0 && h.0 as isize == v
}

fn module() -> Option<HINSTANCE> {
    // SAFETY: no preconditions.
    match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(m) => Some(HINSTANCE(m.0)),
        Err(e) => {
            log!("settings: GetModuleHandleW failed: {e}");
            None
        }
    }
}

fn post_apply() {
    // SAFETY: posting to the dock window; if it no longer exists the call just fails.
    if let Err(e) = unsafe { PostMessageW(Some(crate::hwnd()), WM_DOCK_CONFIG_APPLY, WPARAM(0), LPARAM(0)) } {
        log!("settings: PostMessageW(WM_DOCK_CONFIG_APPLY) failed: {e}");
    }
}

/// Writes the in-memory config (a drag value) to the file; the bar already shows it.
fn save_file() {
    if let Err(e) = config::modify(|_| {}) {
        log!("settings: saving failed ({e}), the value stays for this session");
    }
}

fn invalidate(h: HWND) {
    // SAFETY: our window; whole client area, no erase.
    let _ = unsafe { InvalidateRect(Some(h), None, false) };
}

fn client_size(h: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    // SAFETY: `r` is writable; `h` is our window.
    if unsafe { GetClientRect(h, &mut r) }.is_err() {
        return (0, 0);
    }
    (r.right - r.left, r.bottom - r.top)
}

fn monitor_dpi(h: HMONITOR) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: `x`/`y` are writable u32s for the duration of the call.
    match unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x >= 96 => x,
        _ => 96,
    }
}

fn window_dpi(h: HWND) -> u32 {
    // SAFETY: `h` is our window (0 if it is not a window).
    match unsafe { GetDpiForWindow(h) } {
        0 => 96,
        d => d,
    }
}

/// `AdjustWindowRectExForDpi` of an empty rect: the frame insets at `dpi`.
fn frame_insets(dpi: u32) -> RECT {
    let mut r = RECT::default();
    // SAFETY: `r` is a writable RECT.
    if unsafe { AdjustWindowRectExForDpi(&mut r, style(), false, WINDOW_EX_STYLE(0), dpi) }.is_err() {
        let m = Metrics { dpi };
        return RECT { left: -m.px(8), top: -m.px(31), right: m.px(8), bottom: m.px(8) };
    }
    r
}

/// Dark caption and border in the background colour, white caption text.
fn apply_frame(h: HWND, bg: Background) {
    let set = |attr, v: u32| {
        // SAFETY: `v` lives for the call; the size matches a DWORD attribute.
        let r = unsafe { DwmSetWindowAttribute(h, attr, (&v as *const u32).cast(), std::mem::size_of::<u32>() as u32) };
        if let Err(e) = r {
            log!("settings: DwmSetWindowAttribute({}) failed: {e}", attr.0);
        }
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
    set(DWMWA_CAPTION_COLOR, colorref(bg));
    set(DWMWA_BORDER_COLOR, colorref(bg));
    set(DWMWA_TEXT_COLOR, 0x00FF_FFFF);
}

fn set_title(h: HWND) {
    // SAFETY: our window; the string lives for the call.
    if let Err(e) = unsafe { SetWindowTextW(h, &HSTRING::from(window_title())) } {
        log!("settings: SetWindowTextW failed: {e}");
    }
}

fn kill(h: HWND, id: usize) {
    // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
    let _ = unsafe { KillTimer(Some(h), id) };
}

/// What a click on a hit id does.
enum Act {
    Goto(Page),
    Effect(Effect),
}

impl Win {
    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            screens: &self.screens,
            picker_open: self.picker_open,
            chooser_open: self.chooser_open,
            tray: &self.tray,
            import: self.import,
            update: &self.update,
            memory: self.memory,
            data_dir: self.data_dir.as_deref(),
            log_path: self.log_path.as_deref(),
        }
    }

    fn rows_now(&self) -> Vec<Row> {
        settings_pages::rows(self.page, &config::get(), &self.ctx())
    }

    fn frame<'a>(&'a self, bar: Option<&'a BarPixels>) -> Frame<'a> {
        Frame {
            page: self.page,
            rows: &self.rows,
            tray: &self.tray,
            hot: self.hot,
            pressed: self.pressed,
            focus: self.focus,
            hsv: self.hsv,
            bar,
            badge: self.update.badge(),
            background: config::get().background,
        }
    }

    fn relayout(&mut self) {
        let geo = settings_view::layout(&self.surf, self.client, &self.frame(None), self.scroll);
        self.scroll = geo.scroll;
        self.geo = geo;
        self.sync_bar();
    }

    /// The mini bar: only on the Appearance page, redrawn when the config, the DPI or the card width
    /// changed, at most every `APPLY_THROTTLE_MS` (`TIMER_SETTINGS_BAR` redraws the skipped one).
    fn sync_bar(&mut self) {
        let card = self.geo.preview;
        if self.page != Page::Appearance || card.right <= card.left {
            self.bar = None;
            return;
        }
        let m = Metrics { dpi: self.dpi };
        let max_w = card.right - card.left - m.px(32);
        let cfg = config::get();
        let fresh =
            self.bar.as_ref().is_some_and(|b| Arc::ptr_eq(&b.cfg, &cfg) && b.dpi == self.dpi && b.max_w == max_w);
        if fresh {
            return;
        }
        let now = crate::popup::tick_ms();
        let old = self.bar.as_ref().filter(|b| b.dpi == self.dpi);
        if let Some(ms) = old.and_then(|b| bar_wait(self.bar_built, now, b.px.w <= max_w)) {
            // SAFETY: our window, on its thread.
            if unsafe { SetTimer(Some(self.hwnd), TIMER_SETTINGS_BAR, ms, None) } != 0 {
                return;
            }
            log!("settings: SetTimer(bar) failed");
        }
        self.bar = settings_view::mini_bar(m, &cfg, max_w).map(|px| MiniBar { cfg, dpi: self.dpi, max_w, px });
        self.bar_built = now;
    }

    fn paint(&mut self) {
        // Field by field: the surface is written while the frame borrows the rest.
        let f = Frame {
            page: self.page,
            rows: &self.rows,
            tray: &self.tray,
            hot: self.hot,
            pressed: self.pressed,
            focus: self.focus,
            hsv: self.hsv,
            bar: self.bar.as_ref().map(|b| &b.px),
            badge: self.update.badge(),
            background: config::get().background,
        };
        settings_view::paint(&mut self.surf, &self.geo, &f);
        invalidate(self.hwnd);
    }

    /// Rows, layout and pixels from the current config and state.
    fn refresh(&mut self) {
        self.rows = self.rows_now();
        self.relayout();
        self.paint();
    }

    /// Re-reads the live state (import, update, memory); repaints only when something shown changed.
    fn refresh_live(&mut self) {
        self.import = crate::start_import::status();
        let badge = self.update.badge();
        self.update = crate::update::status();
        self.memory = Memory { now: crate::mem::private_bytes(), trimmed: crate::mem::last_hidden_trim() };
        let rows = self.rows_now();
        if !same_rows(&rows, &self.rows) || badge != self.update.badge() {
            self.rows = rows;
            self.relayout();
            self.paint();
        }
    }

    /// true while the colour picker is being dragged (its hsv must not be re-read from the colour).
    fn pressed_picker(&self) -> bool {
        let Some(Hit::Row(i, _)) = self.pressed.and_then(decode) else { return false };
        self.dragging && matches!(self.rows.get(i).map(|r| &r.control), Some(Control::Picker))
    }

    /// true if a press on `id` starts a drag: a slider track or a part of the picker.
    fn drags(&self, id: u32) -> bool {
        let Some(Hit::Row(i, Some(part))) = decode(id) else { return false };
        match self.rows.get(i).map(|r| &r.control) {
            Some(Control::Slider { .. }) => part == 0,
            Some(Control::Picker) => part == PART_SV || part == PART_HUE,
            _ => false,
        }
    }

    fn clickable(&self, id: u32) -> bool {
        match decode(id) {
            Some(Hit::Nav(_)) => true,
            Some(Hit::Row(i, part)) => match self.rows.get(i).map(|r| &r.control) {
                Some(Control::Toggle { enabled, .. }) => *enabled,
                Some(Control::Choice { selected, .. }) => part != *selected,
                Some(_) => true,
                None => false,
            },
            None => false,
        }
    }

    fn act(&self, id: u32) -> Option<Act> {
        match decode(id)? {
            Hit::Nav(p) => Some(Act::Goto(p)),
            Hit::Row(i, part) => Some(Act::Effect(settings_pages::click(self.rows.get(i)?, part, &self.ctx()))),
        }
    }

    /// Runs what a finished drag still owes to the file and the bar.
    fn finish(&mut self) {
        match self.pending.finish() {
            Finish::Nothing => {}
            Finish::Save => save_file(),
            Finish::Apply => {
                kill(self.hwnd, TIMER_SETTINGS_APPLY);
                post_apply();
            }
            Finish::SaveAndApply => {
                kill(self.hwnd, TIMER_SETTINGS_APPLY);
                save_file();
                post_apply();
            }
        }
    }

    /// A click on a setting: written at once and applied.
    fn save(&mut self, e: Edit) {
        if let Err(err) = config::modify(|c| apply_edit(c, e, &self.screens)) {
            // Not writable (or the write failed): the change still applies for this session.
            log!("settings: saving failed ({err}), applied in memory only");
            let mut c = (*config::get()).clone();
            apply_edit(&mut c, e, &self.screens);
            config::set(c);
        }
        self.pending.changed(true, crate::popup::tick_ms());
        kill(self.hwnd, TIMER_SETTINGS_APPLY);
        post_apply();
        if let Edit::Background(bg) = e {
            self.hsv = rgb_to_hsv(bg.rgb());
        }
        self.refresh();
    }

    /// A drag move: set in memory, the bar rebuilt at most every `APPLY_THROTTLE_MS`.
    fn drag_edit(&mut self, e: Edit) {
        let mut c = (*config::get()).clone();
        apply_edit(&mut c, e, &self.screens);
        config::set(c);
        let now = crate::popup::tick_ms();
        if self.pending.changed(false, now) {
            post_apply();
        } else {
            // Holding still after the last move: the skipped rebuild is flushed by the timer.
            // SAFETY: our window, on its thread.
            if unsafe { SetTimer(Some(self.hwnd), TIMER_SETTINGS_APPLY, self.pending.flush_in(now), None) } == 0 {
                log!("settings: SetTimer(apply) failed");
            }
        }
    }

    /// Drags the pressed part to `(x, y)`.
    fn drag_to(&mut self, id: u32, x: i32, y: i32) {
        let Some(Hit::Row(i, Some(part))) = decode(id) else { return };
        let Some(control) = self.rows.get(i).map(|r| r.control.clone()) else { return };
        match control {
            Control::Slider { slider, value, min, max, .. } if part == 0 => {
                let Some(rc) = self.geo.hits.rect(id) else { return };
                let v = crate::popup::slider_value_at(&rc, self.geo.knob_r, min, max, x);
                if v != value {
                    self.drag_edit(Edit::Slider(slider, v));
                }
            }
            Control::Picker if part == PART_SV || part == PART_HUE => {
                if part == PART_SV {
                    let (s, v) = sv_at(self.geo.sv, x, y);
                    self.hsv.1 = s;
                    self.hsv.2 = v;
                } else {
                    self.hsv.0 = hue_at(self.geo.hue, x);
                }
                let bg = background_of(self.hsv);
                if bg != config::get().background {
                    self.drag_edit(Edit::Background(bg));
                }
            }
            _ => return,
        }
        self.refresh();
    }

    /// Shows `page`: top of the page, chooser closed, picker open when the colour is not a preset.
    fn goto(&mut self, page: Page) {
        self.finish();
        let bg = config::get().background;
        self.page = page;
        self.scroll = 0;
        self.chooser_open = false;
        self.picker_open = page == Page::Appearance && preset_index(bg).is_none();
        self.hsv = rgb_to_hsv(bg.rgb());
        self.refresh();
    }

    /// Scrolls by `dy` physical px and re-reads the hover under the pointer.
    fn scroll_by(&mut self, dy: i32) {
        self.scroll += dy;
        self.relayout();
        let mut pt = POINT::default();
        // SAFETY: `pt` is writable; our window.
        if unsafe { GetCursorPos(&mut pt) }.is_ok() && unsafe { ScreenToClient(self.hwnd, &mut pt) }.as_bool() {
            self.hot = self.geo.hits.at(pt.x, pt.y);
        }
        self.paint();
    }
}

// ------------------------------------------------------------------ public API (UI thread)

/// Opens the window, or restores and activates the open one.
pub fn open() {
    let Some(existing) = with(|st| st.win.as_ref().map(|w| w.hwnd)) else { return };
    if let Some(h) = existing {
        // SAFETY: our window, on its thread.
        if unsafe { IsIconic(h) }.as_bool() {
            let _ = unsafe { ShowWindow(h, SW_RESTORE) };
        }
        crate::ui::force_foreground(h);
        return;
    }
    if let Err(e) = create() {
        log!("settings: cannot open the window: {e}");
    }
}

fn create() -> Result<(), String> {
    let inst = module().ok_or("no module handle")?;
    if !with(|st| st.registered).unwrap_or(true) {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: inst,
            // SAFETY: loads a system cursor (null hInstance), no pointers to manage.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            lpszClassName: CLASS_W,
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialized; the class name is a static literal.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            return Err("RegisterClassExW(DockSettings) failed".into());
        }
        with(|st| st.registered = true);
    }
    // A failed open leaves nothing behind: the class goes with the window that never was.
    open_window(inst).inspect_err(|_| {
        if with(|st| std::mem::replace(&mut st.registered, false)).unwrap_or(false) {
            unregister_class(Some(inst));
        }
    })
}

/// Unregisters the `DockSettings` class (no window of it may survive).
fn unregister_class(inst: Option<HINSTANCE>) {
    // SAFETY: class registered by `create` with this module; no window of it survives.
    let _ = unsafe { UnregisterClassW(CLASS_W, inst) };
}

fn open_window(inst: HINSTANCE) -> Result<(), String> {
    let hmon = crate::ui::dock_hmonitor();
    let dpi = monitor_dpi(hmon);
    let work = crate::ui::monitor_info(hmon).map(|mi| mi.rcWork).ok_or("GetMonitorInfoW failed")?;
    let cfg = config::get();
    let r = initial_rect(work, dpi, cfg.settings_size(), frame_insets(dpi));
    // SAFETY: class registered above; no owner (a normal top-level window).
    let h = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_W,
            &HSTRING::from(window_title()),
            style(),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(inst),
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW failed: {e}"))?;
    // The window has the DPI of the display it was created on.
    let dpi = window_dpi(h);
    let client = client_size(h);
    let surf = Surface::new(Metrics { dpi }).and_then(|mut s| s.resize(client.0.max(1), client.1.max(1)).map(|()| s));
    let surf = match surf {
        Ok(s) => s,
        Err(e) => {
            // SAFETY: created above on this thread.
            let _ = unsafe { DestroyWindow(h) };
            return Err(format!("surface: {e}"));
        }
    };
    // A finished import's result belongs to the window that started it.
    crate::start_import::reset();
    let data_dir = crate::datadir::config_path().and_then(|p| p.parent().map(|d| d.display().to_string()));
    let mut win = Win {
        hwnd: h,
        surf,
        dpi,
        client,
        page: Page::Appearance,
        focus: Focus::Nav,
        scroll: 0,
        wheel: 0,
        rows: Vec::new(),
        geo: Geometry::default(),
        hot: None,
        pressed: None,
        dragging: false,
        tracking: false,
        hsv: rgb_to_hsv(cfg.background.rgb()),
        picker_open: preset_index(cfg.background).is_none(),
        chooser_open: false,
        tray: Vec::new(),
        screens: crate::ui::screens(),
        update: crate::update::status(),
        import: crate::start_import::status(),
        memory: Memory { now: crate::mem::private_bytes(), trimmed: crate::mem::last_hidden_trim() },
        data_dir,
        log_path: crate::datadir::log_path().map(|p| p.display().to_string()),
        pending: Pending::default(),
        bar: None,
        bar_built: 0,
    };
    win.refresh();
    with(|st| st.win = Some(win));
    OPEN_HWND.store(h.0 as isize, Relaxed);
    apply_frame(h, cfg.background);
    // SAFETY: our window, on its thread.
    let _ = unsafe { ShowWindow(h, SW_SHOW) };
    crate::ui::force_foreground(h);
    // SAFETY: as above.
    if unsafe { SetTimer(Some(h), TIMER_SETTINGS_TICK, TICK_MS, None) } == 0 {
        log!("settings: SetTimer(tick) failed");
    }
    log!("settings: window open ({} x {} px at {dpi} dpi)", client.0, client.1);
    Ok(())
}

/// Saves what a drag still holds, destroys the window, unregisters the class and drops the state.
/// Idempotent.
pub fn close(reason: &str) {
    let Some((win, registered)) = with(|st| {
        if let Some(w) = st.win.as_mut() {
            w.finish();
        }
        (st.win.take(), std::mem::replace(&mut st.registered, false))
    }) else {
        return;
    };
    if let Some(w) = win {
        OPEN_HWND.store(0, Relaxed);
        let h = w.hwnd;
        kill(h, TIMER_SETTINGS_TICK);
        kill(h, TIMER_SETTINGS_APPLY);
        kill(h, TIMER_SETTINGS_BAR);
        drop(w);
        // SAFETY: window created by this module on the UI thread.
        if let Err(e) = unsafe { DestroyWindow(h) } {
            log!("settings: DestroyWindow failed: {e}");
        }
        log!("settings: window closed ({reason})");
    }
    if registered {
        unregister_class(module());
    }
}

/// `cleanup`: closes the window (a held drag is saved first).
pub fn shutdown() {
    close("shutdown");
}

/// The config changed (`ui::on_config_changed`): rows, colours, title (language) and pixels.
pub fn on_config_changed() {
    let r = with_win(|w| {
        let bg = config::get().background;
        if !w.pressed_picker() {
            w.hsv = rgb_to_hsv(bg.rgb());
        }
        w.refresh();
        (w.hwnd, bg)
    });
    if let Some((h, bg)) = r {
        apply_frame(h, bg);
        set_title(h);
    }
}

/// The tray changed: the open chooser re-reads its list.
pub fn on_tray_changed() {
    with_win(|w| {
        if w.chooser_open {
            w.tray = crate::popup_tray::customize_rows();
            w.refresh();
        }
    });
}

/// The update status changed (`WM_DOCK_UPDATE`).
pub fn on_update_changed() {
    with_win(Win::refresh_live);
}

// ------------------------------------------------------------------ effects

fn run(act: Act) {
    match act {
        Act::Goto(p) => {
            with_win(|w| w.goto(p));
        }
        Act::Effect(e) => run_effect(e),
    }
}

fn run_effect(e: Effect) {
    match e {
        Effect::None => {}
        Effect::Save(edit) => {
            with_win(|w| w.save(edit));
        }
        Effect::TogglePicker => {
            with_win(|w| {
                w.picker_open = !w.picker_open;
                w.refresh();
            });
        }
        Effect::ToggleChooser => {
            with_win(|w| {
                w.chooser_open = !w.chooser_open;
                if w.chooser_open {
                    w.tray = crate::popup_tray::customize_rows();
                }
                w.refresh();
            });
        }
        Effect::TrayVisible { key, hidden } => {
            crate::tray_host::set_user_hidden(&key, hidden);
            with_win(|w| {
                w.tray = crate::popup_tray::customize_rows();
                w.refresh();
            });
        }
        Effect::Run(a) => run_action(a),
    }
}

fn run_action(a: Action) {
    match a {
        Action::ResetSizes => {
            with_win(|w| w.save(Edit::ResetSizes));
        }
        Action::ImportStart => {
            if !crate::start_import::start() {
                log!("settings: Start import not started (already running)");
            }
            with_win(Win::refresh_live);
        }
        Action::CheckUpdates => {
            let _ = crate::update::check_now();
        }
        Action::InstallUpdate => {
            let _ = crate::update::install_now();
        }
        Action::ReleaseNotes => crate::update::open_release_page(),
        Action::OpenDataFolder => {
            if let Some(dir) = with_win(|w| w.data_dir.clone()).flatten() {
                shell_thread::request(ShellRequest::Launch {
                    target: "explorer.exe".into(),
                    args: Some(format!("\"{dir}\"")),
                });
            }
        }
        Action::OpenLog => {
            if let Some(p) = with_win(|w| w.log_path.clone()).flatten() {
                crate::ui::launch_target_str(&p);
            }
        }
        Action::ExitDock => {
            close("exit");
            crate::power::run(crate::power::PowerAction::ExitDock);
        }
    }
}

// ------------------------------------------------------------------ messages

fn lo_i16(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as u16 as i16 as i32
}

fn hi_i16(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32
}

fn on_down(h: HWND, x: i32, y: i32) {
    let pressed = with_win(|w| {
        let id = w.geo.hits.at(x, y)?;
        w.pressed = Some(id);
        w.hot = Some(id);
        w.dragging = w.drags(id);
        if w.dragging {
            // A press on a slider or the picker sets the value at once.
            w.drag_to(id, x, y);
        } else {
            w.paint();
        }
        Some(id)
    })
    .flatten();
    if pressed.is_some() {
        // SAFETY: our window, on its thread.
        let _ = unsafe { SetCapture(h) };
    }
}

fn on_move(h: HWND, x: i32, y: i32) {
    let track = with_win(|w| {
        if w.dragging {
            if let Some(id) = w.pressed {
                w.drag_to(id, x, y);
            }
        } else {
            let hot = w.geo.hits.at(x, y);
            if hot != w.hot {
                w.hot = hot;
                w.paint();
            }
        }
        !std::mem::replace(&mut w.tracking, true)
    })
    .unwrap_or(false);
    if track {
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: h,
            dwHoverTime: 0,
        };
        // SAFETY: `t` is initialized with the correct cbSize; `h` is our window.
        if let Err(e) = unsafe { TrackMouseEvent(&mut t) } {
            log!("settings: TrackMouseEvent failed: {e}");
            with_win(|w| w.tracking = false);
        }
    }
}

fn on_up(x: i32, y: i32) {
    let Some((pressed, at)) = with_win(|w| (w.pressed, w.geo.hits.at(x, y))) else { return };
    // Outside the borrow: WM_CAPTURECHANGED arrives synchronously and ends the press (`end_press`).
    // SAFETY: no preconditions.
    let _ = unsafe { ReleaseCapture() };
    let act = match pressed {
        Some(id) if at == Some(id) => with_win(|w| w.act(id)).flatten(),
        _ => None,
    };
    end_press();
    if let Some(a) = act {
        run(a);
    }
    with_win(Win::finish);
}

/// The press ended (button released, capture lost): what the drag holds is saved and applied.
fn end_press() {
    with_win(|w| {
        let was = w.pressed.take().is_some();
        w.dragging = false;
        w.finish();
        if was {
            w.paint();
        }
    });
}

unsafe extern "system" fn wnd_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: our window; BeginPaint/EndPaint are paired.
            let hdc = unsafe { BeginPaint(h, &mut ps) };
            STATE.with(|s| {
                if let Ok(g) = s.try_borrow() {
                    if let Some(w) = g.win.as_ref() {
                        w.surf.copy_to(hdc, 0, 0);
                    }
                }
            });
            // SAFETY: paired with BeginPaint above.
            let _ = unsafe { EndPaint(h, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if wp.0 as u32 != SIZE_MINIMIZED {
                let (cw, ch) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
                with_win(|w| {
                    w.client = (cw, ch);
                    if let Err(e) = w.surf.resize(cw.max(1), ch.max(1)) {
                        log!("settings: surface resize failed: {e}");
                    }
                    w.relayout();
                    w.paint();
                });
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            // SAFETY: for WM_DPICHANGED lParam points to the suggested window RECT.
            let r = unsafe { *(lp.0 as *const RECT) };
            with_win(|w| {
                if dpi != w.dpi {
                    match Surface::new(Metrics { dpi }) {
                        Ok(s) => {
                            w.surf = s;
                            w.dpi = dpi;
                            w.bar = None;
                        }
                        Err(e) => log!("settings: surface for {dpi} dpi failed: {e}"),
                    }
                }
            });
            // SAFETY: our window; no state borrow is held (WM_SIZE arrives synchronously).
            let _ = unsafe {
                SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE)
            };
            let c = client_size(h);
            with_win(|w| {
                w.client = c;
                if let Err(e) = w.surf.resize(c.0.max(1), c.1.max(1)) {
                    log!("settings: surface resize failed: {e}");
                }
                w.relayout();
                w.paint();
            });
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let dpi = window_dpi(h);
            let (mw, mh) = min_track(dpi, frame_insets(dpi));
            // SAFETY: for WM_GETMINMAXINFO lParam points to a writable MINMAXINFO.
            unsafe { (*(lp.0 as *mut MINMAXINFO)).ptMinTrackSize = POINT { x: mw, y: mh } };
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            let (cw, ch) = client_size(h);
            let size = logical_size(cw, ch, window_dpi(h));
            if cw > 0 && size != config::get().settings_window_size {
                if let Err(e) = config::modify(|c| c.settings_window_size = size) {
                    log!("settings: saving the window size failed: {e}");
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            with_win(|w| {
                w.screens = crate::ui::screens();
                w.refresh();
            });
            // SAFETY: default handling.
            unsafe { DefWindowProcW(h, m, wp, lp) }
        }
        WM_MOUSEMOVE => {
            on_move(h, lo_i16(lp), hi_i16(lp));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_win(|w| {
                w.tracking = false;
                if !w.dragging && w.hot.take().is_some() {
                    w.paint();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            on_down(h, lo_i16(lp), hi_i16(lp));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_up(lo_i16(lp), hi_i16(lp));
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            end_press();
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            // SAFETY: `pt` is writable; our window.
            let ok = unsafe { GetCursorPos(&mut pt) }.is_ok() && unsafe { ScreenToClient(h, &mut pt) }.as_bool();
            let hand = ok && with_win(|w| w.geo.hits.at(pt.x, pt.y).is_some_and(|id| w.clickable(id))).unwrap_or(false);
            // SAFETY: system cursors (null hInstance).
            if let Ok(c) = unsafe { LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }) } {
                // SAFETY: valid cursor handle.
                unsafe { SetCursor(Some(c)) };
            }
            LRESULT(1)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            with_win(|w| {
                w.wheel += delta;
                let notches = w.wheel / WHEEL_DELTA as i32;
                w.wheel -= notches * WHEEL_DELTA as i32;
                if notches != 0 {
                    let step = Metrics { dpi: w.dpi }.px(settings_view::SCROLL_STEP);
                    w.scroll_by(-notches * step);
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // SAFETY: no preconditions; the high bit means "pressed".
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            let a = with_win(|w| {
                let (focus, a) = key_action(wp.0 as u16, shift, w.focus, w.page);
                w.focus = focus;
                a
            });
            match a {
                Some(KeyAction::Close) => close("Esc"),
                Some(KeyAction::Redraw) => {
                    with_win(Win::paint);
                }
                Some(KeyAction::Goto(p)) => run(Act::Goto(p)),
                Some(KeyAction::Scroll(n)) => {
                    with_win(|w| w.scroll_by(n * Metrics { dpi: w.dpi }.px(settings_view::ROW_H)));
                }
                Some(KeyAction::None) | None => {
                    // SAFETY: default handling.
                    return unsafe { DefWindowProcW(h, m, wp, lp) };
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_SETTINGS_TICK => {
                    with_win(Win::refresh_live);
                }
                TIMER_SETTINGS_APPLY => {
                    kill(h, TIMER_SETTINGS_APPLY);
                    with_win(|w| {
                        if std::mem::take(&mut w.pending.apply_pending) {
                            w.pending.last_apply = crate::popup::tick_ms();
                            // The file is still written by `finish`.
                            post_apply();
                        }
                    });
                }
                TIMER_SETTINGS_BAR => {
                    kill(h, TIMER_SETTINGS_BAR);
                    with_win(|w| {
                        w.sync_bar();
                        w.paint();
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close("title bar");
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(h, m, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_TAB, VK_UP};
    const FRAME: RECT = RECT { left: -8, top: -31, right: 8, bottom: 8 };

    #[test]
    fn initial_rect_centres_and_clamps() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        let r = initial_rect(work, 96, (860, 600), FRAME);
        assert_eq!((r.right - r.left, r.bottom - r.top), (876, 639));
        assert_eq!((r.left, r.top), ((1920 - 876) / 2, (1040 - 639) / 2));
        let r = initial_rect(work, 144, (860, 600), FRAME);
        assert_eq!((r.right - r.left, r.bottom - r.top), (1290 + 16, 900 + 39));
        // Saved on a 4K display, opened on a small second display.
        let small = RECT { left: 1920, top: 0, right: 3286, bottom: 728 };
        let r = initial_rect(small, 96, (3000, 2000), FRAME);
        assert!(r.left >= small.left && r.top >= small.top && r.right <= small.right && r.bottom <= small.bottom);
        // Work area below the minimum: minimum size, title bar at the top-left of the work area.
        let tiny = RECT { left: 0, top: 0, right: 600, bottom: 400 };
        let r = initial_rect(tiny, 96, (860, 600), FRAME);
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (0, 0, 640 + 16, 460 + 39));
    }

    #[test]
    fn logical_size_survives_dpi_changes() {
        assert_eq!(logical_size(1290, 900, 144), [860, 600]);
        assert_eq!(logical_size(1075, 750, 120), [860, 600]);
        assert_eq!(logical_size(1291, 901, 144), [861, 601]);
        assert_eq!(logical_size(100, 100, 96), [640, 460]);
        assert_eq!(min_track(144, FRAME), (960 + 16, 690 + 39));
    }

    #[test]
    fn caption_colours_are_colorrefs() {
        assert_eq!(colorref(Background(0x112233)), 0x0033_2211);
        assert_eq!(colorref(Background::BLACK), 0);
    }

    #[test]
    fn keyboard_moves_between_nav_and_page() {
        use Focus::{Nav, Page as P};
        let k = |vk: u16, shift, f, p| key_action(vk, shift, f, p);
        assert_eq!(k(VK_ESCAPE.0, false, Nav, Page::Bar), (Nav, KeyAction::Close));
        assert_eq!(k(VK_ESCAPE.0, false, P, Page::Bar), (P, KeyAction::Close));
        assert_eq!(k(VK_TAB.0, false, Nav, Page::Bar), (P, KeyAction::Redraw));
        assert_eq!(k(VK_TAB.0, true, P, Page::Bar), (Nav, KeyAction::Redraw));
        assert_eq!(k(VK_DOWN.0, false, Nav, Page::Appearance), (Nav, KeyAction::Goto(Page::Bar)));
        assert_eq!(k(VK_UP.0, false, Nav, Page::Appearance), (Nav, KeyAction::None));
        assert_eq!(k(VK_DOWN.0, false, Nav, Page::About), (Nav, KeyAction::None));
        assert_eq!(k(VK_DOWN.0, false, P, Page::Bar), (P, KeyAction::Scroll(1)));
        assert_eq!(k(VK_UP.0, false, P, Page::Bar), (P, KeyAction::Scroll(-1)));
        assert_eq!(k(0x41, false, Nav, Page::Bar), (Nav, KeyAction::None));
    }

    #[test]
    fn pending_saves_on_finish() {
        let mut p = Pending::default();
        assert!(p.changed(false, 100)); // first drag move: rebuild now
        assert_eq!(p.finish(), Finish::Save); // released: the bar already shows it, only the file is written
        assert_eq!(p.finish(), Finish::Nothing);
        assert!(p.changed(false, 200));
        assert!(!p.changed(false, 210)); // inside the 30 ms throttle: skipped
        assert!(p.unsaved && p.apply_pending);
        assert_eq!(p.finish(), Finish::SaveAndApply); // release, Esc, X or exit: file written AND bar rebuilt
        assert_eq!(p.finish(), Finish::Nothing);
        assert!(p.changed(false, 300));
        assert!(!p.changed(false, 310)); // skipped
        assert!(p.changed(true, 320)); // a click inside the throttle window: saved and applied at once
        assert!(!p.unsaved && !p.apply_pending);
        assert_eq!(p.finish(), Finish::Nothing);
        p.apply_pending = true; // only a skipped rebuild left: finish applies it
        assert_eq!(p.finish(), Finish::Apply);
        assert_eq!(p.flush_in(330), 25); // 30 ms after the last rebuild (320), plus 5
    }

    #[test]
    fn mini_bar_rebuilds_are_throttled() {
        assert_eq!(bar_wait(0, 1000, true), None); // never built: now
        assert_eq!(bar_wait(1000, 1040, true), None); // 30 ms passed: now
        assert_eq!(bar_wait(1000, 1010, true), Some(25)); // inside the throttle: the old bar stays
        assert_eq!(bar_wait(1000, 1010, false), None); // the old bar no longer fits the card: now
    }
}
