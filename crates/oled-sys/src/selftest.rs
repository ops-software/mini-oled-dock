//! Named events for --selftest and --undo-user, and the "updated" notice (F-P7).
//!
//! A waiter (the elevated dock, or whoever asks for a selftest) creates a fresh, manual-reset event named
//! `Local\mini-oled-dock.<app>.<purpose>.<nonce>` whose DACL lets the given user only signal it and wait on it; the
//! component opens it by name and signals it when its checks pass. A name that already exists is `Squatted`: the
//! waiter never trusts an event it did not create. `notify_updated` posts the registered "updated" message to an
//! app's windows, only to those whose process runs the given image.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WAIT_OBJECT_0,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, PostMessageW, RegisterWindowMessageW, HWND_MESSAGE,
};

use crate::datadir::{is_sid_string, SecDesc, ROOT_NAME};
use crate::pipe::{is_name_part, PipeError};

/// The most windows `notify_updated` looks at (an app has one or two; the bound keeps a hostile flood finite).
const MAX_WINDOWS: usize = 64;

pub fn event_name(app: &str, purpose: &str, nonce: &str) -> String {
    format!(r"Local\{ROOT_NAME}.{app}.{purpose}.{nonce}")
} // Local\mini-oled-dock.<app>.<purpose>.<nonce>; purpose "selftest" or "undo-user"
pub fn signal(app: &str, purpose: &str, nonce: &str) -> std::io::Result<()> {
    if ![app, purpose, nonce].iter().all(|p| is_name_part(p)) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a valid event name"));
    }
    let name = HSTRING::from(event_name(app, purpose, nonce));
    // SAFETY: `name` is NUL-terminated; the handle returned is owned below.
    let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &name) }?;
    // SAFETY: a fresh handle, owned by nothing else.
    let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
    // SAFETY: the handle is valid while `event` lives and was opened with EVENT_MODIFY_STATE.
    unsafe { SetEvent(HANDLE(event.as_raw_handle())) }?;
    Ok(())
} // OpenEventW(EVENT_MODIFY_STATE) + SetEvent
pub struct Waiter {
    event: OwnedHandle,
}
impl Waiter {
    pub fn create(
        app: &str,
        purpose: &str,
        user_sid: &str,
    ) -> Result<(Waiter, String /* nonce */), crate::pipe::PipeError> {
        let nonce = crate::pipe::nonce();
        let waiter = Waiter::create_named(app, purpose, &nonce, user_sid)?;
        Ok((waiter, nonce))
    }
    //   CreateEventW, manual reset, DACL SYSTEM + Administrators full, user_sid EVENT_MODIFY_STATE | SYNCHRONIZE;
    //   ERROR_ALREADY_EXISTS = Squatted (ROADMAP R6)
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1).min(u32::MAX - 1);
        // SAFETY: the event handle is valid while `self` lives; waiting has no other effect.
        unsafe { WaitForSingleObject(HANDLE(self.event.as_raw_handle()), ms) == WAIT_OBJECT_0 }
    }

    /// `create` with a given nonce: a name that exists already (or that this process may not open) is Squatted.
    pub(crate) fn create_named(app: &str, purpose: &str, nonce: &str, user_sid: &str) -> Result<Waiter, PipeError> {
        let sddl = event_sddl(user_sid).filter(|_| [app, purpose, nonce].iter().all(|p| is_name_part(p)));
        let sddl = sddl.ok_or(PipeError::Name)?;
        let sd = SecDesc::from_sddl(&sddl).map_err(|e| PipeError::Io(std::io::Error::other(e)))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_ptr().0,
            bInheritHandle: false.into(),
        };
        let name = HSTRING::from(event_name(app, purpose, nonce));
        // SAFETY: clearing this thread's last error, so the one read below is the call's.
        unsafe { SetLastError(WIN32_ERROR(0)) };
        // SAFETY: `name` is NUL-terminated; `sa` and the descriptor it points to live for the call.
        let created = unsafe { CreateEventW(Some(&sa), true, false, &name) };
        // SAFETY: reads this thread's last error, set by the call above.
        let last = unsafe { GetLastError() };
        match created {
            Ok(event) => {
                // SAFETY: a fresh handle, owned by nothing else (closed at once when the event was not ours).
                let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
                if last == ERROR_ALREADY_EXISTS {
                    return Err(PipeError::Squatted);
                }
                Ok(Waiter { event })
            }
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(PipeError::Squatted),
            Err(e) => Err(PipeError::Io(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))),
        }
    }

    /// The event's handle (for the DACL test).
    #[cfg(test)]
    pub(crate) fn handle(&self) -> HANDLE {
        HANDLE(self.event.as_raw_handle())
    }
}

/// The waiter event's DACL: SYSTEM and Administrators full, `user` may only signal and wait; None if `user` is no SID.
pub(crate) fn event_sddl(user: &str) -> Option<String> {
    let user_mask = EVENT_MODIFY_STATE.0 | SYNCHRONIZATION_SYNCHRONIZE.0;
    is_sid_string(user).then(|| format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x{user_mask:x};;;{user})"))
}

pub const UPDATED_MESSAGE: &str = "mini-oled-dock.Updated"; // RegisterWindowMessageW; wParam = (major << 16) | (minor << 8) | patch
pub fn notify_updated(app_class: &str, message_only: bool, image: &std::path::Path, v: (u32, u32, u32)) -> u32 {
    let message = HSTRING::from(UPDATED_MESSAGE);
    // SAFETY: `message` is NUL-terminated.
    let msg = unsafe { RegisterWindowMessageW(&message) };
    if msg == 0 {
        return 0;
    }
    let class = HSTRING::from(app_class);
    let parent = message_only.then_some(HWND_MESSAGE);
    let version = WPARAM(((v.0 as usize) << 16) | ((v.1 as usize) << 8) | v.2 as usize);
    let mut posted = 0;
    let mut prev: Option<HWND> = None;
    for _ in 0..MAX_WINDOWS {
        // SAFETY: `class` is NUL-terminated; `parent` and `prev` are a window (or the message-only parent) or none.
        let Ok(hwnd) = (unsafe { FindWindowExW(parent, prev, &class, PCWSTR::null()) }) else { break };
        prev = Some(hwnd);
        if runs_image(hwnd, image) {
            // SAFETY: posting a registered message to a window; nothing is passed by pointer.
            if unsafe { PostMessageW(Some(hwnd), msg, version, LPARAM(0)) }.is_ok() {
                posted += 1;
            }
        }
    }
    posted
} // windows posted to

/// True when the process that owns `hwnd` runs `image` (the same file: `ident::same_image`).
fn runs_image(hwnd: HWND, image: &Path) -> bool {
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; a window that is gone gives 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid != 0 && crate::pipe::image_of_pid(pid).is_some_and(|own| crate::ident::same_image(&own, image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, PeekMessageW, UnregisterClassW, MSG, PM_REMOVE,
        WINDOW_EX_STYLE, WINDOW_STYLE, WS_POPUP,
    };

    /// WNDCLASSW (winuser.h), declared here: the windows crate puts it behind its GDI feature, which this crate does
    /// not enable for one test.
    #[repr(C)]
    struct WndClass {
        style: u32,
        wnd_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut std::ffi::c_void,
        icon: *mut std::ffi::c_void,
        cursor: *mut std::ffi::c_void,
        background: *mut std::ffi::c_void,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(wc: *const WndClass) -> u16;
    }

    #[test]
    fn waiter_and_signal() {
        let sid = crate::pipe::current_user_sid().unwrap();
        let (waiter, nonce) = Waiter::create("oled-test", "selftest", &sid).unwrap();
        assert!(!waiter.wait(Duration::ZERO), "not signaled yet");
        let t = Instant::now();
        signal("oled-test", "selftest", &nonce).unwrap();
        assert!(waiter.wait(Duration::from_millis(100)));
        assert!(t.elapsed() < Duration::from_millis(100));
        assert!(matches!(Waiter::create_named("oled-test", "selftest", &nonce, &sid), Err(PipeError::Squatted)));
        assert!(signal("oled-test", "selftest", &crate::pipe::nonce()).is_err(), "no such event");
        assert!(signal("oled-test", r"self\test", &nonce).is_err(), "not a name");
        assert!(matches!(Waiter::create("oled-test", "selftest", "x)(A;;GA;;;WD"), Err(PipeError::Name)));
        let dacl = crate::datadir::read_security(waiter.handle()).unwrap().dacl.unwrap();
        let aces: Vec<(String, u32)> = dacl.iter().map(|a| (a.sid.clone().unwrap_or_default(), a.mask)).collect();
        let full = 0x001F_0003; // EVENT_ALL_ACCESS, what GENERIC_ALL maps to for an event
        let user = EVENT_MODIFY_STATE.0 | SYNCHRONIZATION_SYNCHRONIZE.0;
        assert_eq!(aces, [("S-1-5-18".to_string(), full), ("S-1-5-32-544".to_string(), full), (sid, user)]);
        assert!(dacl.iter().all(|a| !a.deny));
        assert_eq!(event_name("tasks", "selftest", "ab"), r"Local\mini-oled-dock.tasks.selftest.ab");
    }

    unsafe extern "system" fn window_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: the arguments Windows passed to this window procedure.
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    /// The windows are this test's own and never shown: a message-only one and a hidden top-level one, both of a
    /// class with a random name, so no other window can match.
    #[test]
    fn notify_updated_finds_message_only_windows() {
        let class_name = format!("OledTest{}", crate::pipe::nonce());
        let class = HSTRING::from(class_name.as_str());
        let null = std::ptr::null_mut();
        let wc = WndClass {
            style: 0,
            wnd_proc: Some(window_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance: null,
            icon: null,
            cursor: null,
            background: null,
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
        };
        // SAFETY: `wc` is a WNDCLASSW and the class name it points to lives for the call.
        assert_ne!(unsafe { RegisterClassW(&wc) }, 0);
        let create = |parent: Option<HWND>, style: WINDOW_STYLE| {
            // SAFETY: a registered class; no parameters by pointer.
            unsafe {
                CreateWindowExW(WINDOW_EX_STYLE(0), &class, PCWSTR::null(), style, 0, 0, 0, 0, parent, None, None, None)
            }
            .unwrap()
        };
        let message_only = create(Some(HWND_MESSAGE), WINDOW_STYLE(0));
        let top = create(None, WS_POPUP);
        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("oled-sys-notify-{}", crate::pipe::nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("copy.exe");
        std::fs::copy(&exe, &copy).unwrap();
        // SAFETY: a NUL-terminated name.
        let msg = unsafe { RegisterWindowMessageW(&HSTRING::from(UPDATED_MESSAGE)) };
        let take = |hwnd: HWND| {
            let mut m = MSG::default();
            // SAFETY: `m` is writable; the window is this thread's.
            unsafe { PeekMessageW(&mut m, Some(hwnd), msg, msg, PM_REMOVE) }.as_bool().then_some(m.wParam.0)
        };
        let to_message_only = notify_updated(&class_name, true, &exe, (1, 8, 3));
        let (got_message_only, got_top) = (take(message_only), take(top));
        let to_another_image = notify_updated(&class_name, false, &copy, (1, 8, 3));
        let after_another_image = take(top);
        let to_own_top = notify_updated(&class_name, false, &exe, (2, 0, 1));
        let got_own_top = take(top);
        // SAFETY: this thread's windows and class, destroyed once.
        unsafe {
            let _ = DestroyWindow(message_only);
            let _ = DestroyWindow(top);
            let _ = UnregisterClassW(&class, None);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(to_message_only, 1, "only the message-only window");
        assert_eq!(got_message_only, Some((1 << 16) | (8 << 8) | 3));
        assert_eq!(got_top, None);
        assert_eq!((to_another_image, after_another_image), (0, None), "a window of another image is skipped");
        assert_eq!((to_own_top, got_own_top), (1, Some((2 << 16) | 1)));
    }
}
