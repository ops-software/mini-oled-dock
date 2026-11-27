//! Minimised, cloaked and display off (open point 9): two process-scoped WinEvent hooks and the power notification.

use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard};

use oled_ui::host::Proxy;
use windows::core::GUID;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Power::{
    RegisterPowerSettingNotification, UnregisterPowerSettingNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    HPOWERNOTIFY, POWERBROADCAST_SETTING,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, DEVICE_NOTIFY_CALLBACK, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
};

/// AppMsg: the window was minimised or restored, cloaked or uncloaked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresenceMsg {
    pub minimised: bool,
    pub cloaked: bool,
}

/// AppMsg: the display is on (true) or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayPower(pub bool);

/// GUID_CONSOLE_DISPLAY_STATE: its data is 0 (off), 1 (on) or 2 (dimmed).
const CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
/// PBT_POWERSETTINGCHANGE, the type of the power callback's call.
const POWER_SETTING_CHANGE: u32 = 0x8013;

/// The three inputs of "the window is on screen", pure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visibility {
    pub minimised: bool,
    pub cloaked: bool,
    pub display_off: bool,
}

impl Visibility {
    /// Visible exactly when none of the three holds.
    pub fn visible(&self) -> bool {
        !self.minimised && !self.cloaked && !self.display_off
    }

    /// A WinEvent code of the two ranges; false for any other code (nothing changed).
    pub fn on_event(&mut self, event: u32) -> bool {
        match event {
            EVENT_SYSTEM_MINIMIZESTART => self.minimised = true,
            EVENT_SYSTEM_MINIMIZEEND => self.minimised = false,
            EVENT_OBJECT_CLOAKED => self.cloaked = true,
            EVENT_OBJECT_UNCLOAKED => self.cloaked = false,
            _ => return false,
        }
        true
    }

    pub fn msg(&self) -> PresenceMsg {
        PresenceMsg { minimised: self.minimised, cloaked: self.cloaked }
    }
}

/// The pure filter of the WinEvent callback: our own window, as a window (not a child object).
pub fn is_ours(event_hwnd: isize, ours: isize, id_object: i32) -> bool {
    event_hwnd == ours && id_object == OBJID_WINDOW.0
}

/// The display state of a GUID_CONSOLE_DISPLAY_STATE payload: on unless it is 0. None when the payload is too short.
pub fn display_on(data: &[u8]) -> Option<bool> {
    let b: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(u32::from_le_bytes(b) != 0)
}

struct Ctx {
    proxy: Proxy,
    hwnd: isize,
    state: Visibility,
}

/// The window's proxy and state for the WinEvent callback, which runs on the UI thread and has no context pointer.
static CTX: Mutex<Option<Ctx>> = Mutex::new(None);

fn ctx() -> MutexGuard<'static, Option<Ctx>> {
    CTX.lock().unwrap_or_else(|e| e.into_inner())
}

/// The installed hooks and notification; removed on drop.
pub struct Presence {
    hooks: [HWINEVENTHOOK; 2],
    power: Option<HPOWERNOTIFY>,
    /// The registration's parameters (kept alive with it).
    _params: Option<Box<DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>>,
}

/// Installs the hooks and the notification on the UI thread and reads the initial state once.
pub fn install(proxy: Proxy, hwnd: HWND) -> Presence {
    let state = Visibility { minimised: is_minimised(hwnd), cloaked: is_cloaked(hwnd), display_off: false };
    if state.minimised || state.cloaked {
        let _ = proxy.send(Box::new(state.msg()));
    }
    *ctx() = Some(Ctx { proxy: proxy.clone(), hwnd: hwnd.0 as isize, state });
    let pid = std::process::id();
    let hooks = [
        set_hook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND, pid),
        set_hook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED, pid),
    ];
    // Leaked on purpose: the power callback may run on a system thread at any time, and the documentation does not say
    // that no call is in flight once the unregistration returns. One small Proxy per process.
    let power_ctx: *const Proxy = Box::leak(Box::new(proxy));
    let mut params = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(power_callback),
        Context: power_ctx.cast_mut().cast(),
    });
    let recipient = HANDLE((&mut *params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast());
    // SAFETY: the recipient points to the parameters, which live in the Presence until after the unregistration; the
    // GUID is a constant.
    let power = unsafe { RegisterPowerSettingNotification(recipient, &CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_CALLBACK) };
    let power = match power {
        Ok(h) => Some(h),
        Err(e) => {
            oled_sys::log!("tasks: display power not followed ({:#x})", e.code().0);
            None
        }
    };
    Presence { hooks, power, _params: Some(params) }
}

fn set_hook(min: u32, max: u32, pid: u32) -> HWINEVENTHOOK {
    // SAFETY: the callback is an extern "system" fn that lives for the process; out of context, so it runs on this
    // thread's message loop; scoped to our own process.
    let h = unsafe { SetWinEventHook(min, max, None, Some(win_event), pid, 0, WINEVENT_OUTOFCONTEXT) };
    if h.0.is_null() {
        oled_sys::log!("tasks: a window event hook was not installed");
    }
    h
}

fn is_minimised(hwnd: HWND) -> bool {
    // SAFETY: a window handle; IsIconic only reads.
    unsafe { IsIconic(hwnd) }.as_bool()
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut v = 0u32;
    // SAFETY: the out pointer is a local u32 of the size passed.
    let r = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut v as *mut u32).cast(), 4) };
    r.is_ok() && v != 0
}

/// The WinEvent callback, on the UI thread.
unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let mut g = ctx();
    let Some(c) = g.as_mut() else { return };
    if !is_ours(hwnd.0 as isize, c.hwnd, id_object) {
        return;
    }
    if c.state.on_event(event) {
        let _ = c.proxy.send(Box::new(c.state.msg()));
    }
}

/// The power notification's callback, on a system thread.
unsafe extern "system" fn power_callback(context: *const c_void, kind: u32, setting: *const c_void) -> u32 {
    if kind != POWER_SETTING_CHANGE || context.is_null() || setting.is_null() {
        return 0;
    }
    // SAFETY: the system passes a POWERBROADCAST_SETTING for this type; its DataLength bytes follow the Data field.
    let data = unsafe {
        let s = setting.cast::<POWERBROADCAST_SETTING>();
        if (*s).PowerSetting != CONSOLE_DISPLAY_STATE {
            return 0;
        }
        let len = ((*s).DataLength as usize).min(16);
        std::slice::from_raw_parts(std::ptr::addr_of!((*s).Data).cast::<u8>(), len)
    };
    if let Some(on) = display_on(data) {
        // SAFETY: the context is the leaked Box<Proxy> of install, never freed, and only read through a shared reference.
        let proxy = unsafe { &*context.cast::<Proxy>() };
        let _ = proxy.send(Box::new(DisplayPower(on)));
    }
    0
}

impl Drop for Presence {
    fn drop(&mut self) {
        for h in self.hooks {
            if !h.0.is_null() {
                // SAFETY: a hook this Presence installed, removed once.
                let _ = unsafe { UnhookWinEvent(h) };
            }
        }
        if let Some(p) = self.power.take() {
            // SAFETY: a registration this Presence made, removed once.
            let _ = unsafe { UnregisterPowerSettingNotification(p) };
        }
        *ctx() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permute(a: &mut [usize; 6], k: usize, out: &mut Vec<[usize; 6]>) {
        if k == a.len() {
            out.push(*a);
            return;
        }
        for i in k..a.len() {
            a.swap(k, i);
            permute(a, k + 1, out);
            a.swap(k, i);
        }
    }

    #[test]
    fn presence_state_machine() {
        let mut v = Visibility::default();
        assert!(v.visible());
        for code in [EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED]
        {
            assert!(Visibility::default().on_event(code), "a known code reports a change");
        }
        let mut only_min = Visibility::default();
        only_min.on_event(EVENT_SYSTEM_MINIMIZESTART);
        assert!(only_min.minimised && !only_min.visible());
        // Every ordering of the six steps against a separate model of the flags.
        let mut order = [0usize, 1, 2, 3, 4, 5];
        let mut perms = Vec::new();
        permute(&mut order, 0, &mut perms);
        assert_eq!(perms.len(), 720);
        for perm in perms {
            let mut v = Visibility::default();
            let (mut min, mut cloak, mut off) = (false, false, false);
            for step in perm {
                match step {
                    0 => {
                        v.on_event(EVENT_SYSTEM_MINIMIZESTART);
                        min = true;
                    }
                    1 => {
                        v.on_event(EVENT_SYSTEM_MINIMIZEEND);
                        min = false;
                    }
                    2 => {
                        v.on_event(EVENT_OBJECT_CLOAKED);
                        cloak = true;
                    }
                    3 => {
                        v.on_event(EVENT_OBJECT_UNCLOAKED);
                        cloak = false;
                    }
                    4 => {
                        v.display_off = true;
                        off = true;
                    }
                    _ => {
                        v.display_off = false;
                        off = false;
                    }
                }
                assert_eq!(v, Visibility { minimised: min, cloaked: cloak, display_off: off });
                assert_eq!(v.visible(), !(min || cloak || off));
            }
        }
        v.on_event(EVENT_SYSTEM_MINIMIZESTART);
        v.on_event(EVENT_OBJECT_CLOAKED);
        v.display_off = true;
        assert!(!v.visible());
        v.on_event(EVENT_SYSTEM_MINIMIZEEND);
        v.display_off = false;
        assert!(!v.visible(), "still cloaked");
        v.on_event(EVENT_OBJECT_UNCLOAKED);
        assert!(v.visible());
        assert!(!v.on_event(0x8000), "another event changes nothing");
        assert!(Visibility { minimised: true, cloaked: false, display_off: true }.msg().minimised);
    }

    #[test]
    fn callbacks_filter_other_windows() {
        assert!(is_ours(77, 77, OBJID_WINDOW.0));
        assert!(!is_ours(78, 77, OBJID_WINDOW.0), "another window");
        assert!(!is_ours(77, 77, 1), "a child object of ours");
        assert_eq!(display_on(&0u32.to_le_bytes()), Some(false));
        assert_eq!(display_on(&1u32.to_le_bytes()), Some(true));
        assert_eq!(display_on(&2u32.to_le_bytes()), Some(true), "dimmed is on");
        assert_eq!(display_on(&[1, 0]), None);
    }
}
