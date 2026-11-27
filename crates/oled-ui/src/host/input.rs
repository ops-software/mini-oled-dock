//! Pointer, keyboard, focus, activation, close, cursor and timer messages into the dispatcher's Input (K1a.8, KA2).
//! The translations are pure functions; on_input feeds the dispatcher through the window's state, in order.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::def_proc;
use super::window::{self, Outcome, WinHost};
use crate::dispatch::Input;
use crate::event::{Key, Mods, MouseButton, TimerId, WheelDelta};
use crate::geom::PointP;
use crate::node::Cursor;

/// WM_MOUSELEAVE (winuser.h; the windows crate has it under UI::Controls only).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The host's Win32 timer ids: one one-shot timer for the dispatcher's internal deadlines (tooltip delay, toast hold,
/// submenu delay), one frame timer while a tween lives, and one one-shot timer per armed TimerId.
pub(crate) const TIMER_DEADLINE: usize = 0x4EFE;
pub(crate) const TIMER_FRAME: usize = 0x4EFF;
pub(crate) const TIMER_APP_BASE: usize = 0x4F00;

/// What a WM_TIMER of the host is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostTimer {
    Deadline,
    Frame,
    App(TimerId),
}

/// The Win32 id of a host timer (one to one: app ids, kit ids included, sit above the two fixed ones).
pub(crate) fn timer_id(t: HostTimer) -> usize {
    match t {
        HostTimer::Deadline => TIMER_DEADLINE,
        HostTimer::Frame => TIMER_FRAME,
        HostTimer::App(id) => TIMER_APP_BASE + id.0 as usize,
    }
}

/// The host timer of a WM_TIMER id; None for an id the host never arms.
pub(crate) fn timer_of(id: usize) -> Option<HostTimer> {
    match id {
        TIMER_DEADLINE => Some(HostTimer::Deadline),
        TIMER_FRAME => Some(HostTimer::Frame),
        n if n >= TIMER_APP_BASE => u32::try_from(n - TIMER_APP_BASE).ok().map(|v| HostTimer::App(TimerId(v))),
        _ => None,
    }
}

/// The signed client point of a mouse message's lParam (GET_X_LPARAM and GET_Y_LPARAM: a monitor left of or above the
/// primary one has negative coordinates).
pub(crate) fn lparam_point(lp: isize) -> PointP {
    PointP { x: (lp & 0xFFFF) as u16 as i16 as i32, y: ((lp >> 16) & 0xFFFF) as u16 as i16 as i32 }
}

/// A wheel message's delta: whole notches (multiples of 120) are lines, notches x `lines` (SPI_GETWHEELSCROLLLINES);
/// anything else is a touchpad: pixels, delta / 120 x `row_px` (the table row), its fractions kept for the receiver.
pub(crate) fn wheel_delta(delta: i16, lines: u32, row_px: i32) -> WheelDelta {
    let notches = f32::from(delta) / 120.0;
    if delta % 120 == 0 {
        WheelDelta::Lines(notches * lines as f32)
    } else {
        WheelDelta::Pixels(notches * row_px as f32)
    }
}

/// SPI_GETWHEELSCROLLLINES as lines per notch: WHEEL_PAGESCROLL (a screen per notch, no page here) counts as the
/// system's default of 3.
pub(crate) fn lines_per_notch(spi: u32) -> u32 {
    if spi == u32::MAX {
        3
    } else {
        spi.min(100)
    }
}

/// One UTF-16 unit of WM_CHAR: a high surrogate waits in `held` for its low half (as the dock's popups combine them).
pub(crate) fn utf16_char(held: &mut u16, unit: u32) -> Option<char> {
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

/// The modifiers held, from a key-state reader (GetKeyState: negative while the key is down).
pub(crate) fn mods_from(state: impl Fn(i32) -> i16) -> Mods {
    let down = |vk: u16| state(i32::from(vk)) < 0;
    let mut m = Mods::NONE;
    if down(VK_CONTROL_) {
        m = m | Mods::CTRL;
    }
    if down(VK_SHIFT_) {
        m = m | Mods::SHIFT;
    }
    if down(VK_MENU_) {
        m = m | Mods::ALT;
    }
    if down(VK_LWIN_) || down(VK_RWIN_) {
        m = m | Mods::WIN;
    }
    m
}

const VK_SHIFT_: u16 = 0x10;
const VK_CONTROL_: u16 = 0x11;
const VK_MENU_: u16 = 0x12;
const VK_LWIN_: u16 = 0x5B;
const VK_RWIN_: u16 = 0x5C;
const VK_SPACE_: usize = 0x20;

/// SPI_GETWHEELSCROLLLINES and SPI_GETWHEELSCROLLCHARS as lines per notch (3 when Windows does not say).
pub(crate) fn read_wheel() -> (u32, u32) {
    let read = |action: SYSTEM_PARAMETERS_INFO_ACTION| {
        let mut v: u32 = 3;
        // SAFETY: `v` is a writable u32, the out parameter both actions fill; nothing is written to the system.
        let _ = unsafe {
            SystemParametersInfoW(action, 0, Some((&mut v as *mut u32).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
        };
        lines_per_notch(v)
    };
    (read(SPI_GETWHEELSCROLLLINES), read(SPI_GETWHEELSCROLLCHARS))
}

/// The modifiers held now.
fn mods() -> Mods {
    // SAFETY: GetKeyState only reads this thread's key state.
    mods_from(|vk| unsafe { GetKeyState(vk) })
}

/// A button message: the button and, for a press, its click count (2 for a DBLCLK).
pub(crate) fn button(msg: u32, wp: usize) -> Option<(MouseButton, Option<u8>)> {
    let x = if (wp >> 16) & 0xFFFF == 2 { MouseButton::X2 } else { MouseButton::X1 };
    Some(match msg {
        WM_LBUTTONDOWN => (MouseButton::Left, Some(1)),
        WM_LBUTTONDBLCLK => (MouseButton::Left, Some(2)),
        WM_LBUTTONUP => (MouseButton::Left, None),
        WM_RBUTTONDOWN => (MouseButton::Right, Some(1)),
        WM_RBUTTONDBLCLK => (MouseButton::Right, Some(2)),
        WM_RBUTTONUP => (MouseButton::Right, None),
        WM_MBUTTONDOWN => (MouseButton::Middle, Some(1)),
        WM_MBUTTONDBLCLK => (MouseButton::Middle, Some(2)),
        WM_MBUTTONUP => (MouseButton::Middle, None),
        WM_XBUTTONDOWN => (x, Some(1)),
        WM_XBUTTONDBLCLK => (x, Some(2)),
        WM_XBUTTONUP => (x, None),
        _ => return None,
    })
}

/// A bit per button, for the capture held while any is down.
fn button_bit(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::X1 => 8,
        MouseButton::X2 => 16,
    }
}

/// The system cursor of a node's cursor.
fn cursor_resource(c: Cursor) -> windows::core::PCWSTR {
    match c {
        Cursor::Arrow => IDC_ARROW,
        Cursor::IBeam => IDC_IBEAM,
        Cursor::SizeWE => IDC_SIZEWE,
        Cursor::SizeNS => IDC_SIZENS,
        Cursor::Hand => IDC_HAND,
    }
}

/// Runs `f` on the state in order, or queues the message: Some(result), or the LRESULT to return now.
fn feed<R>(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, f: impl FnOnce(&mut WinHost) -> R) -> Result<R, LRESULT> {
    match window::handle(hwnd, msg, wp, lp, f) {
        Outcome::Done(r) => Ok(r),
        Outcome::Queued => Err(LRESULT(0)),
        Outcome::NoState => Err(def_proc(hwnd, msg, wp, lp)),
    }
}

/// Every input message of the window procedure.
pub(crate) fn on_input(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let done = |r: Result<(), LRESULT>| r.err().unwrap_or(LRESULT(0));
    match msg {
        WM_MOUSEMOVE => {
            let p = lparam_point(lp.0);
            done(feed(hwnd, msg, wp, lp, |h| {
                track_leave(h);
                h.input(Input::Move(p, mods()));
            }))
        }
        WM_MOUSELEAVE => done(feed(hwnd, msg, wp, lp, |h| {
            h.tracking_leave = false;
            h.input(Input::Leave);
        })),
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK
        | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK | WM_XBUTTONDOWN | WM_XBUTTONUP | WM_XBUTTONDBLCLK => {
            let Some((b, clicks)) = button(msg, wp.0) else { return def_proc(hwnd, msg, wp, lp) };
            let p = lparam_point(lp.0);
            let r = done(feed(hwnd, msg, wp, lp, |h| press(h, p, b, clicks)));
            // The X buttons' messages want TRUE.
            if matches!(b, MouseButton::X1 | MouseButton::X2) {
                LRESULT(1)
            } else {
                r
            }
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
            let mut pt = POINT { x: lparam_point(lp.0).x, y: lparam_point(lp.0).y };
            // SAFETY: our window; `pt` is writable. ScreenToClient sends no message.
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            let horizontal = msg == WM_MOUSEHWHEEL;
            done(feed(hwnd, msg, wp, lp, |h| {
                let row = h.scale.px(h.tokens().size.row_table);
                let lines = if horizontal { h.lp.wheel_chars.get() } else { h.lp.wheel_lines.get() };
                let d = wheel_delta(delta, lines, row);
                h.input(Input::Wheel(PointP { x: pt.x, y: pt.y }, d, horizontal, mods()));
            }))
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let repeat = down && (lp.0 >> 30) & 1 != 0;
            let key = Key(wp.0 as u16);
            let r = done(feed(hwnd, msg, wp, lp, |h| {
                h.input(Input::Key(key, down, repeat, mods()));
            }));
            // Alt+Space opens the system menu (DefWindowProcW); every other key stays the kit's, so Alt or F10 alone
            // never enter the menu-bar mode a window without a menu bar does not have.
            if (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) && wp.0 == VK_SPACE_ {
                def_proc(hwnd, msg, wp, lp)
            } else {
                r
            }
        }
        WM_SYSCHAR => {
            if wp.0 == VK_SPACE_ {
                def_proc(hwnd, msg, wp, lp)
            } else {
                // An Alt+letter the system would answer with a beep: the kit had it as WM_SYSKEYDOWN.
                LRESULT(0)
            }
        }
        WM_CHAR => done(feed(hwnd, msg, wp, lp, |h| {
            if let Some(c) = utf16_char(&mut h.high_surrogate, wp.0 as u32).filter(|c| !c.is_control()) {
                h.input(Input::Char(c));
            }
        })),
        // The window's focus follows its activation (WM_ACTIVATE); the kit's focus is a node.
        WM_SETFOCUS | WM_KILLFOCUS => LRESULT(0),
        WM_ACTIVATE => {
            let on = wp.0 & 0xFFFF != 0;
            let _ = window::handle(hwnd, msg, wp, lp, |h| {
                h.input(Input::Active(on));
            });
            // DefWindowProcW gives the keyboard focus to an activated window.
            def_proc(hwnd, msg, wp, lp)
        }
        WM_CLOSE => done(feed(hwnd, msg, wp, lp, |h| {
            h.input(Input::Close);
        })),
        WM_SETCURSOR => {
            const HTCLIENT: isize = 1;
            if lp.0 & 0xFFFF != HTCLIENT {
                return def_proc(hwnd, msg, wp, lp);
            }
            let Some(c) = window::with_host(hwnd, |h| h.cursor()) else { return def_proc(hwnd, msg, wp, lp) };
            // SAFETY: a shared system cursor, never freed; SetCursor sends no message.
            unsafe {
                let cur = LoadCursorW(None, cursor_resource(c)).ok();
                SetCursor(cur);
            }
            LRESULT(1)
        }
        WM_CAPTURECHANGED => {
            let to = lp.0;
            done(feed(hwnd, msg, wp, lp, |h| {
                if to != h.hwnd.0 as isize {
                    capture_lost(h);
                }
            }))
        }
        WM_TIMER => match timer_of(wp.0) {
            Some(t) => done(feed(hwnd, msg, wp, lp, |h| on_timer(h, t))),
            None => def_proc(hwnd, msg, wp, lp),
        },
        _ => def_proc(hwnd, msg, wp, lp),
    }
}

/// TrackMouseEvent(TME_LEAVE) once per entry into the client.
fn track_leave(h: &mut WinHost) {
    if h.tracking_leave {
        return;
    }
    let mut t = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: h.hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `t` is a complete TRACKMOUSEEVENT for our window; the call sends no message.
    h.tracking_leave = unsafe { TrackMouseEvent(&mut t) }.is_ok();
}

/// A button press or release: the capture is held while any button is down.
fn press(h: &mut WinHost, p: PointP, b: MouseButton, clicks: Option<u8>) {
    let bit = button_bit(b);
    match clicks {
        Some(n) => {
            if h.buttons == 0 {
                h.defer(window::Sys::Capture);
            }
            h.buttons |= bit;
            h.input(Input::Down(p, b, n, mods()));
        }
        None => {
            h.buttons &= !bit;
            h.input(Input::Up(p, b, mods()));
            if h.buttons == 0 {
                h.defer(window::Sys::Release);
            }
        }
    }
}

/// Another window took the capture while a button was down: a drag ends cancelled (the dispatcher's own Esc rule), a
/// press ends without a click.
fn capture_lost(h: &mut WinHost) {
    if h.buttons == 0 {
        return;
    }
    h.buttons = 0;
    if h.disp.state.press.is_some_and(|p| p.dragging) {
        h.input(Input::Key(Key::ESC, true, false, Mods::NONE));
    }
    let pressed = h.disp.state.pressed.take();
    h.disp.state.press = None;
    h.disp.state.captured = None;
    if let Some(r) = pressed.and_then(|id| h.disp.frame.get(id)).map(|n| n.rect) {
        h.add_damage(h.halo(r));
    }
}

/// WM_TIMER of a host timer.
fn on_timer(h: &mut WinHost, t: HostTimer) {
    match t {
        HostTimer::Frame => super::pacing::on_frame_timer(h),
        HostTimer::Deadline => {
            kill(h, TIMER_DEADLINE);
            h.deadline = None;
            let now = h.lp.now_ms();
            h.disp.on_due(now);
            h.apply_pending();
            h.after_dispatch();
        }
        HostTimer::App(id) => {
            if let Some(i) = h.app_timers.iter().position(|t| *t == id) {
                h.app_timers.swap_remove(i);
                kill(h, timer_id(t));
            }
            h.input(Input::Timer(id));
        }
    }
}

/// Arms a Win32 timer of the window (SetTimer sends no message).
pub(crate) fn arm(h: &WinHost, id: usize, ms: u32, newly: bool) {
    // SAFETY: our window; no timer procedure: WM_TIMER comes to the window procedure.
    unsafe { SetTimer(Some(h.hwnd), id, ms.max(USER_TIMER_MINIMUM), None) };
    h.lp.count(|s| {
        s.timers_armed += 1;
        s.timers_live += u32::from(newly);
    });
}

/// Kills a Win32 timer of the window.
pub(crate) fn kill(h: &WinHost, id: usize) {
    // SAFETY: our window; killing a timer that does not exist fails harmlessly.
    if unsafe { KillTimer(Some(h.hwnd), id) }.is_ok() {
        h.lp.count(|s| s.timers_live = s.timers_live.saturating_sub(1));
    }
}

/// Effect::SetTimer: a one-shot timer (killed when it fires).
pub(crate) fn set_timer(h: &mut WinHost, id: TimerId, after_ms: u32) {
    let newly = !h.app_timers.contains(&id);
    if newly {
        h.app_timers.push(id);
    }
    arm(h, timer_id(HostTimer::App(id)), after_ms, newly);
}

/// Effect::CancelTimer.
pub(crate) fn cancel_timer(h: &mut WinHost, id: TimerId) {
    if let Some(i) = h.app_timers.iter().position(|t| *t == id) {
        h.app_timers.swap_remove(i);
        kill(h, timer_id(HostTimer::App(id)));
    }
}

/// The deadline timer follows the dispatcher's earliest internal deadline (armed, moved or killed).
pub(crate) fn sync_deadline(h: &mut WinHost) {
    match h.disp.next_due() {
        Some(due) if h.deadline != Some(due) => {
            let after = due.saturating_sub(h.lp.now_ms()).min(u64::from(u32::MAX)) as u32;
            arm(h, TIMER_DEADLINE, after, h.deadline.is_none());
            h.deadline = Some(due);
        }
        None if h.deadline.is_some() => {
            kill(h, TIMER_DEADLINE);
            h.deadline = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::WidgetId;

    #[test]
    fn wheel_120_is_three_lines() {
        assert_eq!(wheel_delta(120, 3, 22), WheelDelta::Lines(3.0));
        assert_eq!(wheel_delta(-240, 3, 22), WheelDelta::Lines(-6.0));
        assert_eq!(wheel_delta(120, lines_per_notch(u32::MAX), 22), WheelDelta::Lines(3.0), "page scroll: 3 lines");
        assert_eq!(wheel_delta(0, 3, 22), WheelDelta::Lines(0.0));
    }

    #[test]
    fn touchpad_delta_is_pixels() {
        assert_eq!(wheel_delta(30, 3, 22), WheelDelta::Pixels(5.5), "a quarter notch of a 22 px row");
        assert_eq!(wheel_delta(-60, 3, 33), WheelDelta::Pixels(-16.5));
        assert!(matches!(wheel_delta(119, 3, 22), WheelDelta::Pixels(_)), "only whole notches are lines");
    }

    #[test]
    fn lparam_point_is_signed() {
        let lp = |x: i16, y: i16| ((y as u16 as isize) << 16) | (x as u16 as isize);
        assert_eq!(lparam_point(lp(-1200, 40)), PointP { x: -1200, y: 40 }, "a monitor left of the primary one");
        assert_eq!(lparam_point(lp(30, -5)), PointP { x: 30, y: -5 });
        assert_eq!(lparam_point(lp(32767, 0)), PointP { x: 32767, y: 0 });
    }

    #[test]
    fn surrogate_pair_combines() {
        let mut held = 0;
        assert_eq!(utf16_char(&mut held, 0xD83D), None, "the high half waits");
        assert_eq!(utf16_char(&mut held, 0xDE00), Some('\u{1F600}'));
        assert_eq!(held, 0);
        assert_eq!(utf16_char(&mut held, 'a' as u32), Some('a'));
        assert_eq!(utf16_char(&mut held, 0xDE00), None, "a lone low half is dropped");
        assert_eq!(utf16_char(&mut held, 0xD83D), None);
        assert_eq!(utf16_char(&mut held, 'b' as u32), Some('b'), "a high half followed by a plain unit is dropped");
    }

    #[test]
    fn mods_from_state() {
        let held = |keys: &'static [u16]| move |vk: i32| if keys.contains(&(vk as u16)) { -128i16 } else { 1 };
        assert_eq!(mods_from(held(&[])), Mods::NONE, "a toggled key (low bit) is not held");
        assert_eq!(mods_from(held(&[0x11, 0x10])), Mods::CTRL | Mods::SHIFT);
        assert_eq!(mods_from(held(&[0x12])), Mods::ALT);
        assert_eq!(mods_from(held(&[0x5C])), Mods::WIN, "either Windows key");
        assert_eq!(mods_from(held(&[0x5B, 0x11])), Mods::CTRL | Mods::WIN);
    }

    #[test]
    fn timer_ids_do_not_collide() {
        let apps = [
            TimerId(0),
            TimerId(1),
            TimerId(0x4EFE),
            TimerId(0x7FFF_FFFF),
            TimerId::kit(WidgetId(0), 0),
            TimerId::kit(WidgetId(5), 2),
            TimerId::kit(WidgetId(0x7F_FFFF), 255),
            TimerId(0x4F00),
        ];
        assert_eq!(TimerId::kit(WidgetId(0x7F_FFFF), 255), TimerId(u32::MAX), "the largest id is in the list");
        let mut all = vec![HostTimer::Deadline, HostTimer::Frame];
        all.extend(apps.iter().map(|t| HostTimer::App(*t)));
        let ids: Vec<usize> = all.iter().map(|t| timer_id(*t)).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "no two timers share a Win32 id");
        for (t, id) in all.iter().zip(&ids) {
            assert_eq!(timer_of(*id), Some(*t), "{t:?} maps back");
        }
        assert_eq!(timer_of(1), None, "an id the host never arms is not the host's");
        assert_eq!((timer_id(HostTimer::Deadline), timer_id(HostTimer::Frame)), (0x4EFE, 0x4EFF));
    }

    #[test]
    fn buttons_and_double_clicks() {
        assert_eq!(button(WM_LBUTTONDBLCLK, 0), Some((MouseButton::Left, Some(2))));
        assert_eq!(button(WM_RBUTTONUP, 0), Some((MouseButton::Right, None)));
        assert_eq!(button(WM_XBUTTONDOWN, 2 << 16), Some((MouseButton::X2, Some(1))));
        assert_eq!(button(WM_XBUTTONUP, 1 << 16), Some((MouseButton::X1, None)));
        assert_eq!(button(WM_MOUSEMOVE, 0), None);
    }
}
