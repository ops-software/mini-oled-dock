//! The launcher's port to Windows: what the session (`launcher::session`) and the chord flow (`popup_launcher`'s
//! controller) ask for, done with Win32, the popup host and the shell thread. `RealHook` and `RealSys` are the ports the
//! controller runs on in the dock (its tests run on recorders). The mapping from the session's launches to the shell
//! thread's requests (L1a-12i): every launch becomes one of the launcher's own requests, never the shared `Launch`,
//! `LaunchAppAdmin` or `RevealApp`, so only launcher launches push `LaunchFailed`; `ShellRequest::RunAsTyped` is built
//! here and nowhere else (lint 1).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND, INVALID_HANDLE_VALUE, LPARAM, POINT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Globalization::{
    GetLocaleInfoEx, LOCALE_ITIME, LOCALE_SDECIMAL, LOCALE_SSHORTDATE, LOCALE_STHOUSAND,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, MapViewOfFile, UnmapViewOfFile,
    FILE_MAP_READ, GMEM_MOVEABLE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, RegisterWaitForSingleObject, UnregisterWaitEx,
    WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WT_EXECUTEONLYONCE,
};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, RegisterHotKey, SendInput, ToUnicodeEx, UnregisterHotKey, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOD_ALT, MOD_SHIFT, VIRTUAL_KEY, VK_CAPITAL,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetCursorPos, GetDesktopWindow, GetForegroundWindow, GetMessageExtraInfo, GetMessageTime,
    GetShellWindow, GetWindowThreadProcessId, InternalGetWindowText, IsHungAppWindow, IsWindow, IsWindowVisible,
    PostMessageW, SetTimer, SC_KEYMENU, WM_CLOSE, WM_SYSCOMMAND,
};

use crate::hook::{self, KeyRec, PassFlags, RING_CAP};
#[cfg(test)]
use crate::launcher::arena::Arena;
use crate::launcher::arena::{self, ArenaError, Caps, CatalogView, Kind as ArenaKind, Layout};
use crate::launcher::model::{CommandId, Mods, NumberStyle, WindowRow};
use crate::launcher::powertoys;
use crate::launcher::rank;
use crate::launcher::session::LaunchReq;
use crate::log;
use crate::popup::popup_launcher::{typed_of, HookPort, Launcher, MonitorPick, SysPort, Typed};
use crate::popup::{self, Centred, PopupKind};
use crate::shell_thread::ShellRequest;

/// The keyboard hook's side of the chord (hook.rs's statics).
pub(crate) struct RealHook;

impl HookPort for RealHook {
    fn ack(&mut self, seq: u32) {
        hook::ack_launcher(seq);
    }

    fn end(&mut self, seq: u32) {
        hook::end_typeahead(seq);
    }

    fn shown(&mut self, on: bool) {
        hook::set_launcher_shown(on);
    }

    fn pass(&mut self, flags: PassFlags) {
        hook::set_pass_flags(flags);
    }

    fn seqs(&self) -> (u32, u32) {
        (hook::LAUNCHER_SEQ.load(Relaxed), hook::LAST_FAILOPEN_SEQ.load(Relaxed))
    }

    fn alt_down(&self) -> bool {
        hook::alt_tracked_down()
    }

    fn take(&mut self, seq: u32, out: &mut [KeyRec; RING_CAP]) -> usize {
        hook::take_typeahead(seq, out)
    }

    fn reset_mods(&mut self) {
        hook::reset_modifiers();
    }

    fn gesture_time(&mut self) -> Option<u32> {
        hook::take_gesture_time()
    }
}

/// Windows and the popup host, for the controller (UI thread only).
pub(crate) struct RealSys;

/// The monitor `pick` names; the primary one when there is no better answer.
pub(crate) fn target_monitor(pick: MonitorPick) -> HMONITOR {
    let primary = POINT::default();
    // SAFETY: GetCursorPos writes the POINT it is given; the Monitor* calls accept any point or window (a null
    // foreground included) and fall back to the primary monitor.
    unsafe {
        match pick {
            MonitorPick::Pointer => {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
                } else {
                    MonitorFromPoint(primary, MONITOR_DEFAULTTOPRIMARY)
                }
            }
            MonitorPick::Active => MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY),
            MonitorPick::Primary => MonitorFromPoint(primary, MONITOR_DEFAULTTOPRIMARY),
        }
    }
}

fn hw(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

/// Hidden by DWM (another virtual desktop, a suspended app): not a window to give the foreground to.
fn cloaked(h: HWND) -> bool {
    let mut v = 0u32;
    // SAFETY: `v` is a writable u32, the size passed is its size; a stale window only makes the call fail.
    let r = unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&mut v as *mut u32).cast(), std::mem::size_of::<u32>() as u32)
    };
    r.is_ok() && v != 0
}

/// A window the window menu can act on: not null, still there, not the desktop (Progman, or the WorkerW that Win+D
/// leaves in front) and not one of the dock's own windows (the bar after the Win key, the shared popup window when
/// Start was open).
fn acts_on(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    let w = hw(h);
    // SAFETY: the three calls accept any handle and have no other preconditions.
    let alive = unsafe { IsWindow(Some(w)).as_bool() && w != GetDesktopWindow() && w != GetShellWindow() };
    alive && menu_target(&class_name(w), crate::ui::is_own_window(w))
}

/// The part of `acts_on` for a live window, pure: not a desktop class, not one of the dock's own windows.
fn menu_target(class: &str, own: bool) -> bool {
    !own && !matches!(class, "Progman" | "WorkerW")
}

/// The window class of `h` (at most 63 units); empty when it cannot be read.
fn class_name(h: HWND) -> String {
    let mut cls = [0u16; 64];
    // SAFETY: `cls` is a writable buffer; a stale handle makes the call return 0.
    let n = usize::try_from(unsafe { GetClassNameW(h, &mut cls) }).unwrap_or(0);
    String::from_utf16_lossy(cls.get(..n).unwrap_or(&[]))
}

/// `text` to the clipboard as `CF_UNICODETEXT`, owned by the dock window (a clipboard opened without an owner refuses
/// `SetClipboardData` after `EmptyClipboard`).
fn set_clipboard_text(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the clipboard is opened and closed here, on this thread; the global block is written within its size
    // (`wide.len()` units) while locked and handed to the clipboard, which owns it from then on (freed here only when
    // SetClipboardData refused it).
    unsafe {
        if let Err(e) = OpenClipboard(Some(crate::hwnd())) {
            log!("launcher: OpenClipboard failed: {e}");
            return;
        }
        let r = (|| -> windows::core::Result<()> {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let p = GlobalLock(mem).cast::<u16>();
            if p.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(windows::core::Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e);
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        if let Err(e) = r {
            log!("launcher: the copy to the clipboard failed: {e}");
        }
    }
}

/// The clipboard's `CF_UNICODETEXT`, at most 64 KiB of it (up to its first NUL).
fn clipboard_text() -> Option<String> {
    const MAX_UNITS: usize = 32 * 1024;
    // SAFETY: the clipboard is opened and closed here, on this thread; the block the clipboard owns is read only while
    // locked, within GlobalSize, and never freed here.
    unsafe {
        OpenClipboard(Some(crate::hwnd())).ok()?;
        let text = (|| {
            let h = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let mem = HGLOBAL(h.0);
            let units = (GlobalSize(mem) / 2).min(MAX_UNITS);
            let p = GlobalLock(mem).cast::<u16>().cast_const();
            if p.is_null() {
                return None;
            }
            let all = std::slice::from_raw_parts(p, units);
            let len = all.iter().position(|&u| u == 0).unwrap_or(units);
            let s = String::from_utf16_lossy(all.get(..len).unwrap_or(&[]));
            let _ = GlobalUnlock(mem);
            Some(s)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// The window the launcher's one-shots live on (lint 9): the dock window, never destroyed while the dock runs. The
/// shared popup window can be (Alt+F4 on another kind's popup reaches DefWindowProc), and its timers go with it. None
/// before the dock window exists and after it is gone.
pub(crate) fn timer_window() -> Option<HWND> {
    let h = crate::hwnd();
    (!h.0.is_null()).then_some(h)
}

/// The only `SetTimer` of the launcher (lint 7): a one-shot on the dock window (`timer_window`); `ui::on_timer` hands
/// it to `popup_launcher::on_timer`, which kills it before it acts.
pub(crate) fn arm_once(id: usize, ms: u32) {
    let Some(hwnd) = timer_window() else { return };
    // SAFETY: the dock window (same thread); ui::on_timer forwards the launcher's ids to popup_launcher::on_timer.
    let armed = unsafe { SetTimer(Some(hwnd), id, ms, None) } != 0;
    if !armed {
        log!("launcher: SetTimer failed");
    }
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::timer_armed(id, ms, armed);
}

/// The dock window receives `WM_WTSSESSION_CHANGE` for this session (once, when a chord is first set).
pub(crate) fn watch_sessions() {
    // SAFETY: the dock window exists (popup::init runs after ui::attach) and belongs to this thread.
    if let Err(e) = unsafe { WTSRegisterSessionNotification(crate::hwnd(), NOTIFY_FOR_THIS_SESSION) } {
        log!("launcher: WTSRegisterSessionNotification failed: {e}");
    }
}

/// Undoes `watch_sessions` before the dock window is destroyed.
pub(crate) fn unwatch_sessions() {
    // SAFETY: as above; unregistering a window that is not registered fails harmlessly.
    let _ = unsafe { WTSUnRegisterSessionNotification(crate::hwnd()) };
}

/// One mark of the user locale (`LOCALE_SDECIMAL`, `LOCALE_STHOUSAND`): its first character.
fn locale_mark(lctype: u32) -> Option<char> {
    let mut buf = [0u16; 8];
    // SAFETY: `buf` is writable and its length is passed; a null name is LOCALE_NAME_USER_DEFAULT.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), lctype, Some(&mut buf)) };
    let len = usize::try_from(n).ok()?.saturating_sub(1);
    String::from_utf16_lossy(buf.get(..len)?).chars().next()
}

/// A string of the user locale (`LCTYPE`): at most 80 UTF-16 units, None when it cannot be read.
fn locale_string(lctype: u32) -> Option<String> {
    let mut buf = [0u16; 80];
    // SAFETY: `buf` is writable and its length is passed; a null name is LOCALE_NAME_USER_DEFAULT.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), lctype, Some(&mut buf)) };
    let len = usize::try_from(n).ok()?.saturating_sub(1);
    Some(String::from_utf16_lossy(buf.get(..len)?))
}

/// The offset of local time from UTC in minutes, from the return value and the three biases of
/// `GetTimeZoneInformation` (the bias is UTC minus local time): TIME_ZONE_ID_STANDARD (1) adds the standard bias,
/// DAYLIGHT (2) the daylight bias, UNKNOWN (0, no daylight saving in the zone) neither; anything else is 0.
fn offset_of(ret: u32, bias: i32, std: i32, dst: i32) -> i32 {
    match ret {
        1 => bias.saturating_add(std).saturating_neg(),
        2 => bias.saturating_add(dst).saturating_neg(),
        0 => bias.saturating_neg(),
        _ => 0,
    }
}

/// The power action of a command row.
fn power_action(id: CommandId) -> Option<crate::power::PowerAction> {
    use crate::power::PowerAction;
    Some(match id {
        CommandId::Lock => PowerAction::Lock,
        CommandId::Sleep => PowerAction::Sleep,
        CommandId::Restart => PowerAction::Restart,
        CommandId::ShutDown => PowerAction::Shutdown,
        CommandId::SignOut => PowerAction::SignOut,
        CommandId::Registry | CommandId::WindowMenu | CommandId::ClipboardHistory => return None,
    })
}

/// The effective DPI of a monitor (96 when it cannot be read).
pub(crate) fn monitor_dpi(mon: HMONITOR) -> u32 {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: both out pointers are valid for the call; a stale monitor handle only makes it fail.
    match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x >= 96 => x,
        _ => 96,
    }
}

impl SysPort for RealSys {
    fn now(&self) -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetTickCount() }
    }

    fn d3d_fullscreen(&self) -> bool {
        // SAFETY: no preconditions; the state is returned by value.
        unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN)
    }

    fn fullscreen_foreground(&self) -> bool {
        crate::ui::foreground_is_fullscreen()
    }

    fn remote_foreground(&self) -> bool {
        // SAFETY: no preconditions.
        let h = unsafe { GetForegroundWindow() };
        if h.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        // SAFETY: `pid` is writable for the call; a stale window only yields 0.
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        // SAFETY: no preconditions.
        let me = unsafe { GetCurrentProcessId() };
        pid != 0 && pid != me && image_matches(pid, powertoys::is_remote_client)
    }

    fn launcher_open(&self) -> bool {
        popup::current() == Some(PopupKind::Launcher)
    }

    fn open(&mut self, pick: MonitorPick, top_percent: u8) -> bool {
        let mon = target_monitor(pick);
        let Some(mi) = crate::ui::monitor_info(mon) else {
            log!("launcher: no monitor to open on");
            return false;
        };
        popup::open_centred(PopupKind::Launcher, Centred { work: mi.rcWork, dpi: monitor_dpi(mon), top_percent })
    }

    fn close_launcher(&mut self) {
        popup::with_launcher(Launcher::chord_again);
    }

    fn inject(&mut self, keys: &[(u16, bool)]) {
        let inputs: Vec<INPUT> = keys
            .iter()
            .map(|&(vk, up)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: hook::INJECT_TAG,
                    },
                },
            })
            .collect();
        // SAFETY: a slice of initialized INPUTs; cbSize = size_of::<INPUT>() as required. The hook passes tagged events.
        let n = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if n as usize != inputs.len() {
            let count = inputs.len();
            log!("launcher: SendInput accepted {n} of {count} events");
        }
    }

    fn win_key(&mut self, vk: u16) {
        #[cfg(feature = "owner-probe")]
        let probe = crate::launcher_probe::before_win_key(vk);
        crate::ui::send_win_key(VIRTUAL_KEY(vk));
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::win_key_sent(probe);
    }

    fn translate(&mut self, rec: &KeyRec) -> Typed {
        let mut state = [0u8; 256];
        let mut down = |vks: &[VIRTUAL_KEY]| {
            for vk in vks {
                if let Some(s) = state.get_mut(usize::from(vk.0)) {
                    *s = 0x80;
                }
            }
        };
        if rec.mods.has(Mods::SHIFT) {
            down(&[VK_SHIFT, VK_LSHIFT]);
        }
        if rec.mods.has(Mods::CTRL) {
            down(&[VK_CONTROL, VK_LCONTROL]);
        }
        if rec.mods.has(Mods::ALT) {
            down(&[VK_MENU, VK_LMENU]);
        }
        // SAFETY: GetKeyState has no preconditions; the low bit is the toggle state.
        if unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) } & 1 != 0 {
            if let Some(s) = state.get_mut(usize::from(VK_CAPITAL.0)) {
                *s = 0x01;
            }
        }
        let mut buf = [0u16; 8];
        // SAFETY: `state` and `buf` are valid for the call; flag 0x4 leaves the keyboard state (dead keys included)
        // unchanged; GetKeyboardLayout(0) is the calling thread's layout.
        let n = unsafe {
            ToUnicodeEx(u32::from(rec.vk), u32::from(rec.scan), &state, &mut buf, 0x4, Some(GetKeyboardLayout(0)))
        };
        typed_of(n, &buf)
    }

    fn foreground(&self) -> isize {
        // SAFETY: no preconditions.
        unsafe { GetForegroundWindow() }.0 as isize
    }

    fn window_target(&self, h: isize) -> bool {
        acts_on(h)
    }

    fn activate(&mut self, h: isize) {
        crate::apps::activate_window(h);
    }

    fn restore(&mut self, h: isize) {
        let w = hw(h);
        // SAFETY: IsWindowVisible accepts any handle.
        if h != 0 && unsafe { IsWindowVisible(w) }.as_bool() && !cloaked(w) {
            crate::ui::force_foreground(w);
        }
    }

    fn copy(&mut self, text: &str) {
        set_clipboard_text(text);
    }

    fn paste(&mut self) -> Option<String> {
        clipboard_text()
    }

    fn close_window(&mut self, h: isize) {
        // SAFETY: PostMessageW accepts any handle; a window that is gone makes it fail.
        if let Err(e) = unsafe { PostMessageW(Some(hw(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
            log!("launcher: PostMessageW(WM_CLOSE) failed: {e}");
        }
    }

    fn window_menu(&mut self, h: isize) {
        if !acts_on(h) {
            log!("launcher: no window for the window menu");
            return;
        }
        // SAFETY: as above; SC_KEYMENU with ' ' opens the window menu as Alt+Space does.
        let r = unsafe { PostMessageW(Some(hw(h)), WM_SYSCOMMAND, WPARAM(SC_KEYMENU as usize), LPARAM(0x20)) };
        if let Err(e) = r {
            log!("launcher: PostMessageW(SC_KEYMENU) failed: {e}");
        }
    }

    fn shell(&mut self, r: ShellRequest) {
        crate::shell_thread::request(r);
    }

    fn power(&mut self, id: CommandId) {
        if let Some(a) = power_action(id) {
            crate::power::run(a);
        }
    }

    fn open_settings(&mut self) {
        // SAFETY: PostMessageW to the dock window; if it no longer exists it just fails.
        let r = unsafe {
            PostMessageW(
                Some(crate::hwnd()),
                crate::msg::WM_DOCK_OPEN_SETTINGS,
                WPARAM(crate::msg::SETTINGS_PAGE_LAUNCHER),
                LPARAM(0),
            )
        };
        if let Err(e) = r {
            log!("launcher: PostMessageW(WM_DOCK_OPEN_SETTINGS) failed: {e}");
        }
    }

    fn prepare_open(&mut self) {
        popup::popup_launcher::prepare_open();
    }

    fn settings_bytes(&mut self) -> Result<SettingsArena, SettingsMiss> {
        settings_now()
    }

    fn ensure_settings(&mut self, rebuild: bool) {
        crate::start_search::ensure_settings_cache(rebuild);
    }

    fn fill_titles(&mut self, rows: &mut [WindowRow]) {
        fill_titles(rows);
    }

    fn arm(&mut self, id: usize, ms: u32) {
        arm_once(id, ms);
    }

    fn msg_time(&self) -> u32 {
        // SAFETY: no preconditions; the time of the message being handled, on the GetTickCount clock.
        unsafe { GetMessageTime() as u32 }
    }

    fn msg_extra(&self) -> usize {
        // SAFETY: no preconditions.
        unsafe { GetMessageExtraInfo() }.0 as usize
    }

    fn now_ms(&self) -> u64 {
        // SAFETY: no preconditions.
        unsafe { GetTickCount64() }
    }

    fn number_style(&self) -> NumberStyle {
        let decimal = locale_mark(LOCALE_SDECIMAL).unwrap_or('.');
        let fallback = if decimal == ',' { '.' } else { ',' };
        let group = locale_mark(LOCALE_STHOUSAND).filter(|g| *g != decimal).unwrap_or(fallback);
        NumberStyle { decimal, group }
    }

    fn time_style(&self) -> crate::launcher::datetime::TimeStyle {
        let itime = locale_string(LOCALE_ITIME).unwrap_or_default();
        let short = locale_string(LOCALE_SSHORTDATE).unwrap_or_default();
        crate::launcher::datetime::TimeStyle::from_locale(&itime, &short)
    }

    fn utc_offset_min(&self) -> i32 {
        let mut tz = TIME_ZONE_INFORMATION::default();
        // SAFETY: `tz` is a writable TIME_ZONE_INFORMATION for the call.
        let ret = unsafe { GetTimeZoneInformation(&mut tz) };
        if ret == 0xFFFF_FFFF {
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Relaxed) {
                log!("launcher: the time zone could not be read, dates show UTC");
            }
        }
        offset_of(ret, tz.Bias, tz.StandardBias, tz.DaylightBias)
    }

    fn close_quiet(&mut self) {
        popup::close("session end");
    }

    fn defer_close(&mut self, seq: u32) -> bool {
        let (msg, wp) = (crate::msg::WM_DOCK_LAUNCHER_CLOSE, WPARAM(seq as usize));
        // SAFETY: PostMessageW to the dock window; if it no longer exists it just fails.
        let r = unsafe { PostMessageW(Some(crate::hwnd()), msg, wp, LPARAM(0)) };
        if let Err(e) = &r {
            log!("launcher: posting the close of the open failed: {e}");
        }
        r.is_ok()
    }

    fn chord_owners(&mut self) -> bool {
        let pids = chord_owner_pids();
        sync_owner_waits(&pids);
        !pids.is_empty()
    }

    fn owner_window(&self, h: isize) -> bool {
        is_owner_window(h)
    }

    fn pt_answer(&mut self, code: usize) {
        // SAFETY: PostMessageW to the dock window; if it no longer exists it just fails.
        let r = unsafe { PostMessageW(Some(crate::hwnd()), crate::msg::WM_DOCK_LAUNCHER_PT, WPARAM(code), LPARAM(0)) };
        if let Err(e) = r {
            log!("launcher: posting the PowerToys answer failed: {e}");
        }
    }
}

// ---------------------------------------------------------------- PowerToys (L1b-5, SPEC 5.7)

/// A chord owner the dock waits on: its process, opened for SYNCHRONIZE, and the thread-pool wait that posts its exit.
struct OwnerWait {
    pid: u32,
    process: HANDLE,
    wait: HANDLE,
}

thread_local! {
    /// UI thread only; empty while no chord owner runs (and always with a chord other than Alt+Space).
    static OWNER_WAITS: RefCell<Vec<OwnerWait>> = const { RefCell::new(Vec::new()) };
}

/// At most this many chord owners are waited on (Command Palette and PowerToys Run, each perhaps twice in a restart).
const MAX_OWNER_WAITS: usize = 4;

/// The image of process `pid` is a chord owner (`powertoys::is_chord_owner`).
fn image_is_owner(pid: u32) -> bool {
    image_matches(pid, powertoys::is_chord_owner)
}

/// The full image path of process `pid` passes `test`. Read-only: the process is opened with
/// PROCESS_QUERY_LIMITED_INFORMATION and closed at once; its path is read into the stack, nothing is allocated (a path
/// over 512 characters is neither a PowerToys install nor a remote client and is skipped).
fn image_matches(pid: u32, test: fn(&[u16]) -> bool) -> bool {
    // SAFETY: OpenProcess has no preconditions; the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { return false };
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has `len` writable elements; `len` is in (capacity) and out (length written, no NUL).
    let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    // SAFETY: the handle opened above, closed once.
    let _ = unsafe { CloseHandle(process) };
    ok.is_ok() && buf.get(..len as usize).is_some_and(test)
}

/// The process ids of the running chord owners (5.7: by image name; PowerToys.exe alone never counts). One
/// `EnumProcesses` and one image query per process; nothing of PowerToys is read.
fn chord_owner_pids() -> Vec<u32> {
    let mut pids = vec![0u32; 1024];
    let mut bytes = 0u32;
    loop {
        let cap = u32::try_from(pids.len() * std::mem::size_of::<u32>()).unwrap_or(u32::MAX);
        // SAFETY: `pids` has `cap` writable bytes; `bytes` receives the number of bytes written.
        if unsafe { EnumProcesses(pids.as_mut_ptr(), cap, &mut bytes) }.is_err() {
            log!("launcher: EnumProcesses failed");
            return Vec::new();
        }
        if bytes < cap || pids.len() >= 1 << 16 {
            break;
        }
        // Full: the list may be cut, so grow and ask again.
        pids.resize(pids.len() * 2, 0);
    }
    pids.truncate(bytes as usize / std::mem::size_of::<u32>());
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    pids.retain(|&pid| pid > 4 && pid != me && image_is_owner(pid));
    pids.truncate(MAX_OWNER_WAITS);
    pids
}

/// Thread-pool callback of a chord owner's exit: one post to the dock window, nothing else.
unsafe extern "system" fn owner_exited(_context: *mut core::ffi::c_void, _timed_out: bool) {
    let h = crate::hwnd();
    if !h.0.is_null() {
        // SAFETY: PostMessageW to the dock window from any thread; if it no longer exists it just fails.
        let _ = unsafe { PostMessageW(Some(h), crate::msg::WM_DOCK_LAUNCHER_PT, WPARAM(0), LPARAM(0)) };
    }
}

/// Ends one wait: the thread-pool wait first (blocking until a running callback, a single post, has returned), then the
/// process handle it waited on.
fn end_wait(w: OwnerWait) {
    // SAFETY: `w.wait` came from RegisterWaitForSingleObject and is unregistered once; INVALID_HANDLE_VALUE waits for a
    // callback in progress, so the process handle is no longer in use when it is closed.
    if let Err(e) = unsafe { UnregisterWaitEx(w.wait, Some(INVALID_HANDLE_VALUE)) } {
        log!("launcher: UnregisterWaitEx failed: {e}");
    }
    // SAFETY: the process handle opened by `sync_owner_waits`, closed once.
    let _ = unsafe { CloseHandle(w.process) };
}

/// The waits follow `pids`: a wait whose process has ended (its pid may have been reused) or is no longer listed ends;
/// a listed pid without a wait gets one (RegisterWaitForSingleObject: no poll, one post at the exit).
fn sync_owner_waits(pids: &[u32]) {
    OWNER_WAITS.with(|cell| {
        let Ok(mut waits) = cell.try_borrow_mut() else { return };
        let mut i = 0;
        while let Some(w) = waits.get(i) {
            // SAFETY: a process handle this list owns; a zero timeout only reads its state.
            let ended = unsafe { WaitForSingleObject(w.process, 0) } == WAIT_OBJECT_0;
            if ended || !pids.contains(&w.pid) {
                end_wait(waits.swap_remove(i));
            } else {
                i += 1;
            }
        }
        for &pid in pids {
            if waits.len() >= MAX_OWNER_WAITS || waits.iter().any(|w| w.pid == pid) {
                continue;
            }
            // SAFETY: OpenProcess has no preconditions; the handle is owned by the wait below or closed.
            let Ok(process) =
                (unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
            else {
                continue;
            };
            let mut wait = HANDLE::default();
            // SAFETY: `wait` is writable; `process` stays open until the wait is unregistered (`end_wait`); the callback
            // only posts a message.
            let r = unsafe {
                RegisterWaitForSingleObject(&mut wait, process, Some(owner_exited), None, INFINITE, WT_EXECUTEONLYONCE)
            };
            match r {
                Ok(()) => waits.push(OwnerWait { pid, process, wait }),
                Err(e) => {
                    log!("launcher: RegisterWaitForSingleObject failed: {e}");
                    // SAFETY: opened above, closed once.
                    let _ = unsafe { CloseHandle(process) };
                }
            }
        }
    });
}

/// Every wait ends (the chord is off or not Alt+Space, or the dock exits): nothing of it stays.
pub(crate) fn release_owner_waits() {
    OWNER_WAITS.with(|cell| {
        if let Ok(mut waits) = cell.try_borrow_mut() {
            for w in std::mem::take(&mut *waits) {
                end_wait(w);
            }
        }
    });
}

/// The window `h` belongs to a chord owner: a process the dock waits on, or one whose image is an owner's.
fn is_owner_window(h: isize) -> bool {
    if h == 0 {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is writable for the call; a stale window only yields 0.
    unsafe { GetWindowThreadProcessId(hw(h), Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    let waited = OWNER_WAITS.with(|cell| cell.try_borrow().is_ok_and(|w| w.iter().any(|w| w.pid == pid)));
    waited || image_is_owner(pid)
}

/// The id of the probe's hotkey (the launcher's, "LA").
const PROBE_HOTKEY_ID: i32 = 0x4C41;

/// Whether no other app holds the launcher's chord: Alt+Space, or Alt+Shift+Space with `shift`. The chord is registered on
/// the popup window and given back at once; a refusal means another program owns it. Only the offer in Start asks.
pub(crate) fn chord_is_free(shift: bool) -> bool {
    let Some(hwnd) = popup::popup_hwnd() else { return true };
    let mods = if shift { MOD_ALT | MOD_SHIFT } else { MOD_ALT };
    // SAFETY: our own window on this thread; the registration is undone right after.
    match unsafe { RegisterHotKey(Some(hwnd), PROBE_HOTKEY_ID, mods, u32::from(VK_SPACE.0)) } {
        Ok(()) => {
            // SAFETY: the hotkey just registered with this id on this window.
            let _ = unsafe { UnregisterHotKey(Some(hwnd), PROBE_HOTKEY_ID) };
            true
        }
        Err(_) => false,
    }
}

/// Most windows the launcher lists (one row per window).
const MAX_WINDOW_ROWS: usize = 128;

/// The name a window row shows: the bar's name for its program, or the program file's name without its extension.
pub(crate) fn row_name(bar: Option<&str>, exe_path: &str) -> String {
    match bar.filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => {
            std::path::Path::new(exe_path).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned())
        }
    }
}

/// One row per open window (at most 128), most recent first within each program group: the bar's data is read, no window
/// is sent anything. Titles are left out (`fill_titles`).
pub(crate) fn window_rows() -> Vec<WindowRow> {
    let mut rows = Vec::new();
    for g in crate::apps::groups() {
        let name =
            row_name(crate::ui::bar_name(&g.key).as_deref(), if g.exe_path.is_empty() { &g.key } else { &g.exe_path });
        let mut folded = String::with_capacity(name.len());
        rank::fold(&name, &mut folded);
        for &h in &g.windows {
            if rows.len() >= MAX_WINDOW_ROWS {
                return rows;
            }
            // SAFETY: a plain query on a window handle; a stale one answers false.
            let hung = unsafe { IsHungAppWindow(hw(h)) }.as_bool();
            rows.push(WindowRow {
                hwnd: h,
                group_key: g.key.as_str().into(),
                aumid: g.aumid.as_deref().map(Into::into),
                name: name.as_str().into(),
                name_folded: folded.as_str().into(),
                title: None,
                hung,
            });
        }
    }
    rows
}

/// Reads the title of every row with `InternalGetWindowText` (a 256-unit buffer): it sends no message, so a hung app
/// cannot block the UI thread (the plain call asks the window itself and is never used here, lint 6).
pub(crate) fn fill_titles(rows: &mut [WindowRow]) {
    let mut buf = [0u16; 256];
    for r in rows {
        // SAFETY: `buf` is writable for its length; the call reads the title from the window's own record.
        let n = unsafe { InternalGetWindowText(hw(r.hwnd), &mut buf) }.max(0) as usize;
        let title = String::from_utf16_lossy(buf.get(..n.min(buf.len())).unwrap_or(&[]));
        r.title = (!title.trim().is_empty()).then(|| title.into());
    }
}

/// Why the launcher holds no settings at an open (L3 T18, T19).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsMiss {
    /// No key (Windows' build or its settings files cannot be read) or no cache folder: no file is ever written, so the
    /// settings thread is not asked.
    NoKey,
    /// No cache file yet: the settings thread is asked to make it.
    NoFile,
    /// A file that could not be read, or failed the check: the settings thread rebuilds it.
    Refused,
}

impl SettingsMiss {
    /// What to ask the settings thread (`start_search::ensure_settings_cache`'s `rebuild`), if anything.
    pub(crate) fn rebuild(self) -> Option<bool> {
        match self {
            SettingsMiss::NoKey => None,
            SettingsMiss::NoFile => Some(false),
            SettingsMiss::Refused => Some(true),
        }
    }
}

/// A file mapped read-only (L3 T19): the settings cache while the launcher is open, 0 private bytes (its pages are the
/// file's). Opened with read sharing only (`datadir::open_checked_read_only`), so while it is mapped nobody can write,
/// truncate, rename or delete the file and a read of the view can never fault; refused when it is a reparse point, has
/// more than one link, is empty or is larger than the cap. Dropped: the view unmapped, the mapping and the file closed.
pub(crate) struct MappedFile {
    _file: std::fs::File,
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

/// Why a file was not mapped.
#[derive(Debug)]
pub(crate) enum MapError {
    /// No such file.
    Missing,
    /// Not a file to read (a reparse point, a folder, another link, its size, its access): never read.
    Refused(String),
    /// The mapping itself failed: the file may be read instead.
    Map(String),
}

impl MappedFile {
    pub(crate) fn open(path: &std::path::Path, cap: u64) -> Result<MappedFile, MapError> {
        use std::os::windows::io::AsRawHandle;
        let file = crate::datadir::open_checked_read_only(path, true).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => MapError::Missing,
            std::io::ErrorKind::PermissionDenied => MapError::Refused(e.to_string()),
            _ => MapError::Map(e.to_string()),
        })?;
        let size = file.metadata().map_err(|e| MapError::Map(e.to_string()))?.len();
        if size == 0 || size > cap {
            return Err(MapError::Refused(format!("{size} bytes")));
        }
        let len = usize::try_from(size).map_err(|e| MapError::Refused(e.to_string()))?;
        // SAFETY: the handle of `file`, open for reading for the whole call; no security attributes, no name; the
        // mapping covers the file as it is (its size cannot change: nobody else may write it).
        let map =
            unsafe { CreateFileMappingW(HANDLE(file.as_raw_handle()), None, PAGE_READONLY, 0, 0, PCWSTR::null()) }
                .map_err(|e| MapError::Map(e.to_string()))?;
        // SAFETY: the mapping handle made above; the whole file, read-only.
        let view = unsafe { MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            let e = windows::core::Error::from_thread();
            // SAFETY: the mapping handle made above, closed once.
            let _ = unsafe { CloseHandle(map) };
            return Err(MapError::Map(e.to_string()));
        }
        Ok(MappedFile { _file: file, map, view, len })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: `view` maps the whole file, `len` bytes, read-only, until `drop`; the file cannot change while
        // `_file` is open (read sharing only) and the slice lives no longer than `&self`.
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>().cast_const(), self.len) }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        // SAFETY: the view and the mapping handle made in `open`, each released once; the file closes after, with
        // `_file`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.map);
        }
    }
}

/// The bytes of a settings arena held by the launcher.
enum SettingsBytes {
    Mapped(MappedFile),
    /// Read into one block, when the mapping failed.
    Read(Box<[u8]>),
}

impl SettingsBytes {
    fn get(&self) -> &[u8] {
        match self {
            SettingsBytes::Mapped(m) => m.bytes(),
            SettingsBytes::Read(b) => b,
        }
    }
}

/// The launcher's settings while it is open (L3 T19): the cache file's bytes, mapped (0 private bytes) or read, with
/// the layout `arena::check` made of them. Never replaced during an open; dropped at the close.
pub(crate) struct SettingsArena {
    bytes: SettingsBytes,
    layout: Layout,
}

impl SettingsArena {
    fn checked(bytes: SettingsBytes, key: Option<&str>) -> Result<SettingsArena, ArenaError> {
        let layout = arena::check(bytes.get(), ArenaKind::Settings, key, Caps::SETTINGS)?;
        Ok(SettingsArena { bytes, layout })
    }

    pub(crate) fn view(&self) -> CatalogView<'_> {
        CatalogView::new(self.bytes.get(), &self.layout)
    }

    pub(crate) fn len(&self) -> usize {
        self.layout.entries()
    }

    /// The bytes are the file's pages, not the heap's.
    #[cfg(test)]
    pub(crate) fn is_mapped(&self) -> bool {
        matches!(self.bytes, SettingsBytes::Mapped(_))
    }

    /// The bytes of a test arena, checked again (the fake ports' settings).
    #[cfg(test)]
    pub(crate) fn of(a: &Arena) -> Option<SettingsArena> {
        SettingsArena::checked(SettingsBytes::Read(a.raw().into()), None).ok()
    }
}

/// The launcher's settings now (`SysPort::settings_bytes`): the cache file under the key the settings thread computed.
pub(crate) fn settings_now() -> Result<SettingsArena, SettingsMiss> {
    static NO_KEY: AtomicBool = AtomicBool::new(false);
    let Some((key, path)) =
        crate::start_search::expected_settings_key().zip(crate::start_search::settings_cache_path())
    else {
        if !NO_KEY.swap(true, Relaxed) {
            log!("launcher: no settings cache can be keyed or kept here: the launcher shows no settings");
        }
        return Err(SettingsMiss::NoKey);
    };
    // Unelevated the cache cannot be written: a file of an elevated run is still used, but nothing is asked of the thread.
    map_settings(&path, &key).map_err(|m| if crate::datadir::writable() { m } else { SettingsMiss::NoKey })
}

/// The settings cache (`<root>\cache\settings-catalog.bin`, version 2) mapped (`MappedFile`: read sharing only, one link,
/// no reparse point, at most 4 MiB) and checked whole before anything of it is used (`arena::check` under `key`: header, key
/// line, every section and reference, the checksum): the bytes are untrusted. When the mapping itself fails the file is
/// read into one block instead (logged once per process; that open may pass MEM-3).
pub(crate) fn map_settings(path: &std::path::Path, key: &str) -> Result<SettingsArena, SettingsMiss> {
    static READ: AtomicBool = AtomicBool::new(false);
    static REFUSED: AtomicBool = AtomicBool::new(false);
    let refused = |e: &dyn std::fmt::Display| {
        if !REFUSED.swap(true, Relaxed) {
            log!("launcher: the settings cache was refused: {e}");
        }
        SettingsMiss::Refused
    };
    let bytes = match MappedFile::open(path, crate::start_search::CACHE_MAX) {
        Ok(m) => SettingsBytes::Mapped(m),
        Err(MapError::Missing) => return Err(SettingsMiss::NoFile),
        Err(MapError::Refused(e)) => return Err(refused(&e)),
        Err(MapError::Map(e)) => {
            if !READ.swap(true, Relaxed) {
                log!("launcher: the settings cache cannot be mapped ({e}): read instead");
            }
            match crate::datadir::read_file(path, crate::start_search::CACHE_MAX, true) {
                Ok(b) => SettingsBytes::Read(b.into_boxed_slice()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SettingsMiss::NoFile),
                Err(e) => return Err(refused(&e)),
            }
        }
    };
    SettingsArena::checked(bytes, Some(key)).map_err(|e| refused(&e))
}

/// The shell request of one launch. `setting` gives the target and arguments of a held setting
/// (`LaunchReq::Setting` carries only its entry index; the controller holds the settings arena). `None`: no such setting,
/// or no Windows folder for the registry row; nothing is launched.
pub(crate) fn to_request(
    r: LaunchReq,
    setting: &dyn Fn(u32) -> Option<(String, Option<String>)>,
) -> Option<ShellRequest> {
    Some(match r {
        // The target Start builds for a catalogue app (popup_startmenu `after_close`).
        LaunchReq::App(id) => ShellRequest::LauncherLaunch { target: format!("shell:AppsFolder\\{id}"), args: None },
        LaunchReq::AppAdmin(id) => ShellRequest::LauncherAppAdmin { id: id.into() },
        LaunchReq::RevealApp(id) => ShellRequest::LauncherRevealApp { id: id.into() },
        LaunchReq::Setting { index } => {
            let (target, args) = setting(index)?;
            ShellRequest::LauncherLaunch { target, args }
        }
        LaunchReq::OpenPath(path) => ShellRequest::OpenPath { path: path.into() },
        LaunchReq::RevealPath(path) => ShellRequest::RevealPath { path: path.into() },
        LaunchReq::Command(r) => ShellRequest::LaunchCommand { line: r.line.into(), file: r.file, console: false },
        LaunchReq::Console(r) => ShellRequest::LaunchCommand { line: r.line.into(), file: r.file, console: true },
        LaunchReq::RunAsTyped { line, gesture } => {
            ShellRequest::RunAsTyped { line: line.line.into(), file: line.file, gesture }
        }
        LaunchReq::Url(url) => ShellRequest::OpenUrl { url: url.into() },
        LaunchReq::Registry => {
            let windows = crate::shell_thread::system_dir()?.parent()?.join("regedit.exe");
            ShellRequest::LauncherLaunch { target: windows.to_string_lossy().into_owned(), args: None }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::gesture::Gesture;
    use crate::launcher::runline;

    #[test]
    fn offset_of_vectors() {
        // Rome in winter: standard time, bias -60 (UTC minus local).
        assert_eq!(offset_of(1, -60, 0, -60), 60);
        // Rome in summer.
        assert_eq!(offset_of(2, -60, 0, -60), 120);
        // New York in winter.
        assert_eq!(offset_of(1, 300, 0, -60), -300);
        // A zone without daylight saving time, and a call that failed.
        assert_eq!(offset_of(0, -330, 0, 0), 330);
        assert_eq!(offset_of(0xFFFF_FFFF, -60, 0, -60), 0);
        assert_eq!(offset_of(7, i32::MIN, i32::MIN, i32::MIN), 0);
        assert_eq!(offset_of(1, i32::MIN, 0, 0), i32::MAX);
    }

    const CALC: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
    const SETTINGS: &str =
        "shell:AppsFolder\\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

    /// The controller's held settings: one, at index 1.
    fn setting(i: u32) -> Option<(String, Option<String>)> {
        (i == 1).then(|| (SETTINGS.to_string(), Some("page=SettingsPageDisplay".to_string())))
    }

    /// Every variant, so a new one cannot miss its row (this match has no wildcard).
    fn variant(r: &LaunchReq) -> u8 {
        match r {
            LaunchReq::App(_) => 0,
            LaunchReq::AppAdmin(_) => 1,
            LaunchReq::RevealApp(_) => 2,
            LaunchReq::Setting { .. } => 3,
            LaunchReq::OpenPath(_) => 4,
            LaunchReq::RevealPath(_) => 5,
            LaunchReq::Command(_) => 6,
            LaunchReq::Console(_) => 7,
            LaunchReq::RunAsTyped { .. } => 8,
            LaunchReq::Url(_) => 9,
            LaunchReq::Registry => 10,
        }
    }

    #[test]
    fn window_menu_targets() {
        assert!(menu_target("Notepad", false));
        assert!(menu_target("", false), "a class that could not be read is not the desktop");
        assert!(!menu_target("WorkerW", false), "the desktop after Win+D");
        assert!(!menu_target("Progman", false), "the desktop");
        assert!(!menu_target("Notepad", true), "the bar, or the shared popup window when Start was open");
        assert!(!acts_on(0), "no window");
    }

    #[test]
    fn row_name_from_the_bar() {
        assert_eq!(row_name(Some("Blocco note"), "C:\\Windows\\notepad.exe"), "Blocco note");
        assert_eq!(row_name(None, "C:\\Windows\\notepad.exe"), "notepad");
        assert_eq!(row_name(Some(""), "C:\\Windows\\System32\\Mmc.EXE"), "Mmc");
        assert_eq!(row_name(None, ""), "");
    }

    #[test]
    fn to_request_table() {
        let run = runline::classify("ping 1.1.1.1").unwrap();
        let quoted = runline::classify("\"C:\\Program Files\\x.exe\" -y").unwrap();
        let regedit = crate::shell_thread::system_dir().unwrap().parent().unwrap().join("regedit.exe");
        let regedit = regedit.to_string_lossy().into_owned();
        let rows = [
            LaunchReq::App(CALC.into()),
            LaunchReq::AppAdmin(CALC.into()),
            LaunchReq::RevealApp(CALC.into()),
            LaunchReq::Setting { index: 1 },
            LaunchReq::OpenPath("C:\\a b\\c.txt".into()),
            LaunchReq::RevealPath("C:\\a b\\c.txt".into()),
            LaunchReq::Command(run.clone()),
            LaunchReq::Console(run.clone()),
            LaunchReq::RunAsTyped { line: quoted.clone(), gesture: Gesture::for_test(7) },
            LaunchReq::Url("https://a.it/x?q=1".into()),
            LaunchReq::Registry,
        ];
        let mut seen: Vec<u8> = rows.iter().map(variant).collect();
        seen.dedup();
        assert_eq!(seen, (0..=10).collect::<Vec<u8>>(), "one row per variant");
        let out: Vec<ShellRequest> = rows.into_iter().map(|r| to_request(r, &setting).expect("a request")).collect();
        let mut it = out.iter();
        let mut next = || it.next().unwrap();
        let app = format!("shell:AppsFolder\\{CALC}");
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: None } if *target == app));
        assert!(matches!(next(), ShellRequest::LauncherAppAdmin { id } if id == CALC));
        assert!(matches!(next(), ShellRequest::LauncherRevealApp { id } if id == CALC));
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: Some(a) }
            if target == SETTINGS && a == "page=SettingsPageDisplay"));
        assert!(matches!(next(), ShellRequest::OpenPath { path } if path == "C:\\a b\\c.txt"));
        assert!(matches!(next(), ShellRequest::RevealPath { path } if path == "C:\\a b\\c.txt"));
        assert!(matches!(next(), ShellRequest::LaunchCommand { line, file, console: false }
            if line == "ping 1.1.1.1" && *file == (0..4)));
        assert!(matches!(next(), ShellRequest::LaunchCommand { line, console: true, .. } if line == "ping 1.1.1.1"));
        assert!(matches!(next(), ShellRequest::RunAsTyped { line, file, gesture }
            if line == "\"C:\\Program Files\\x.exe\" -y" && *file == (1..23) && *gesture == Gesture::for_test(7)));
        assert!(matches!(next(), ShellRequest::OpenUrl { url } if url == "https://a.it/x?q=1"));
        assert!(matches!(next(), ShellRequest::LauncherLaunch { target, args: None } if *target == regedit));
        // None of them is a request whose failure the launcher could not tell from Start's.
        let shared = |r: &ShellRequest| {
            matches!(
                r,
                ShellRequest::Launch { .. } | ShellRequest::LaunchAppAdmin { .. } | ShellRequest::RevealApp { .. }
            )
        };
        assert!(!out.iter().any(shared), "{out:?}");
        // A setting the controller does not hold launches nothing.
        assert!(to_request(LaunchReq::Setting { index: 7 }, &setting).is_none());
    }

    /// L3 T19: a file is mapped whole and read-only; while it is mapped nobody can write, replace or delete it (a reader
    /// still can); one with another hard link, an empty one, one over the cap and a folder are refused, a missing one is
    /// missing; the settings cache is used only under its own key.
    #[test]
    fn mapped_file_reads_and_refuses_links() {
        let dir = std::env::temp_dir().join(format!("dock_map_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(31) >> 3) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let m = MappedFile::open(&path, 1 << 20).unwrap();
        assert!(m.bytes() == bytes.as_slice());
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err(), "no writer while mapped");
        let tmp = dir.join("a.tmp");
        std::fs::write(&tmp, b"x").unwrap();
        assert!(std::fs::rename(&tmp, &path).is_err(), "no replacement while mapped");
        assert!(std::fs::remove_file(&path).is_err(), "no deletion while mapped");
        assert_eq!(crate::datadir::read_file(&path, 1 << 20, true).unwrap(), bytes, "a reader still reads it");
        drop(m);
        assert!(matches!(MappedFile::open(&path, 1_000), Err(MapError::Refused(_))), "over the cap");
        let empty = dir.join("e.bin");
        std::fs::write(&empty, b"").unwrap();
        assert!(matches!(MappedFile::open(&empty, 1 << 20), Err(MapError::Refused(_))), "empty");
        assert!(matches!(MappedFile::open(&dir.join("none.bin"), 1 << 20), Err(MapError::Missing)));
        assert!(matches!(MappedFile::open(&dir, 1 << 20), Err(MapError::Refused(_) | MapError::Map(_))), "a folder");
        let link = dir.join("b.bin");
        std::fs::hard_link(&path, &link).unwrap();
        assert!(matches!(MappedFile::open(&path, 1 << 20), Err(MapError::Refused(_))), "two links");
        assert!(matches!(MappedFile::open(&link, 1 << 20), Err(MapError::Refused(_))));
        // The settings cache: mapped and checked under its key; another key, a missing file and a damaged one are not used.
        let cache = dir.join("settings-catalog.bin");
        let arena = crate::launcher::arena::test_settings(&[("Schermo", &[], 1), ("Mouse", &[], 2)]);
        std::fs::write(&cache, arena.raw()).unwrap();
        let held = map_settings(&cache, "test").unwrap();
        assert!(held.is_mapped() && held.len() == 2 && held.view().name(1) == "Mouse");
        assert!(matches!(map_settings(&cache, "other|fmt 1"), Err(SettingsMiss::Refused)));
        drop(held);
        let mut damaged = arena.raw().to_vec();
        let mid = damaged.len() / 2;
        damaged[mid] ^= 0x20;
        std::fs::write(&cache, &damaged).unwrap();
        assert!(matches!(map_settings(&cache, "test"), Err(SettingsMiss::Refused)));
        assert!(matches!(map_settings(&dir.join("gone.bin"), "test"), Err(SettingsMiss::NoFile)));
        assert_eq!(
            [SettingsMiss::NoKey, SettingsMiss::NoFile, SettingsMiss::Refused].map(SettingsMiss::rebuild),
            [None, Some(false), Some(true)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
