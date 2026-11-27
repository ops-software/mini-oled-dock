//! One Tasks per session (F-T2, open point 19): the mutex, forwarding a Show to a running window, the unguarded owner.
//!
//! What another process can send is one WM_COPYDATA frame that only selects, sorts or pages (ui/show.rs). The forward
//! goes only to a MiniOledTasks window whose process runs this very image file and is not this process.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use oled_trust::frame::tasks::{copydata_tag, InstanceCmd, Show, MUTEX_NAME, WINDOW_CLASS};
use windows::core::HSTRING;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
    SMTO_NORMAL, WM_COPYDATA,
};

/// How long a start looks for the owner's window before it decides nobody shows one.
const FIND_WINDOW_MS: u64 = 1_000;
/// The pause between two looks.
const FIND_STEP_MS: u64 = 50;
/// SendMessageTimeoutW's bound: a hung owner answers at once (SMTO_ABORTIFHUNG), a slow one in time.
const SEND_TIMEOUT_MS: u32 = 300;
/// Windows of our class one look reads (a flood of squatting windows stays finite).
const MAX_CANDIDATES: usize = 64;

/// The mutex handle while a Guard lives (a handle is one pointer-sized value; 0 = none). There is one per process.
static HELD: AtomicIsize = AtomicIsize::new(0);

/// Holds the single-instance mutex while it lives; dropping it releases the name (the restart after an update).
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let h = HELD.swap(0, Ordering::AcqRel);
        if h != 0 {
            // SAFETY: the handle was returned by CreateMutexW and is closed once: the swap took it out of the static.
            let _ = unsafe { CloseHandle(HANDLE(h as *mut c_void)) };
        }
    }
}

pub enum Acquire {
    /// We own the name.
    Owner(Guard),
    /// The name is held and no window answered, and this is not a --from-chord start: a window of our own anyway.
    Unguarded,
    /// A running Tasks took the Show: exit 0.
    Forwarded,
    /// A running Tasks did not answer, or a --from-chord start found no window: exit 3.
    NoAnswer,
}

/// A MiniOledTasks window a look found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub hwnd: isize,
    pub pid: u32,
    /// Its process runs the same image file as this one (oled_sys::ident::same_image).
    pub same_image: bool,
}

/// The window a Show goes to: the first of the same image that is not ours. Pure.
pub fn forward_target(candidates: &[Candidate], own_pid: u32) -> Option<Candidate> {
    candidates.iter().copied().find(|c| c.same_image && c.pid != own_pid)
}

/// What a start does when the name is held and no window showed in time: a chord start gives up (the dock falls back
/// to Windows' Task Manager), any other start opens its own, unguarded window. Pure.
pub fn on_no_window(from_chord: bool) -> Acquire {
    if from_chord {
        Acquire::NoAnswer
    } else {
        Acquire::Unguarded
    }
}

/// T6.1: the mutex, else a forward, else the unguarded window.
pub fn acquire(show: &Show) -> Acquire {
    // SAFETY: clears this thread's last error, so the one read below is CreateMutexW's.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // SAFETY: a NUL-terminated name that lives for the call; no security attributes.
    let made = unsafe { CreateMutexW(None, false, &HSTRING::from(MUTEX_NAME)) };
    // SAFETY: reads this thread's last error.
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    match made {
        Ok(h) if !exists => {
            HELD.store(h.0 as isize, Ordering::Release);
            return Acquire::Owner(Guard);
        }
        // SAFETY: a handle CreateMutexW just returned; this process does not keep it.
        Ok(h) => {
            let _ = unsafe { CloseHandle(h) };
        }
        // A name held with an access we cannot get reads as held.
        Err(_) => {}
    }
    forward(show)
}

/// The name is held: hand the Show to the owner's window, looking for it for up to a second.
fn forward(show: &Show) -> Acquire {
    let mut frame = Vec::new();
    if oled_trust::frame::encode(&InstanceCmd::Show(show.clone()), &mut frame).is_err() {
        oled_sys::log!("tasks: the Show does not encode");
        return Acquire::NoAnswer;
    }
    let Ok(exe) = std::env::current_exe() else {
        return on_no_window(show.from_chord);
    };
    let own = std::process::id();
    let start = Instant::now();
    loop {
        if let Some(t) = forward_target(&candidates(&exe), own) {
            return send(HWND(t.hwnd as *mut c_void), t.pid, &frame);
        }
        if start.elapsed() >= Duration::from_millis(FIND_WINDOW_MS) {
            break;
        }
        std::thread::sleep(Duration::from_millis(FIND_STEP_MS));
    }
    if show.from_chord {
        oled_sys::log!("tasks: mutex held, no window");
    }
    on_no_window(show.from_chord)
}

/// The windows of our class, with their process and whether it runs this image.
fn candidates(exe: &std::path::Path) -> Vec<Candidate> {
    let class = HSTRING::from(WINDOW_CLASS);
    let mut out = Vec::new();
    let mut prev: Option<HWND> = None;
    for _ in 0..MAX_CANDIDATES {
        // SAFETY: `class` is NUL-terminated and lives for the call; `prev` is a window found before or none.
        let Ok(hwnd) = (unsafe { FindWindowExW(None, prev, &class, windows::core::PCWSTR::null()) }) else { break };
        prev = Some(hwnd);
        let mut pid = 0u32;
        // SAFETY: `pid` is writable; a window that is gone gives 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            continue;
        }
        let same_image = oled_sys::pipe::peer_of_pid(pid).is_ok_and(|p| oled_sys::ident::same_image(&p.image, exe));
        out.push(Candidate { hwnd: hwnd.0 as isize, pid, same_image });
    }
    out
}

/// AllowSetForegroundWindow, then the frame through WM_COPYDATA: TRUE is Forwarded, a timeout or FALSE is NoAnswer.
fn send(hwnd: HWND, pid: u32, frame: &[u8]) -> Acquire {
    // SAFETY: no pointers; lets that process bring its window to the front for this Show.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
    let cds = COPYDATASTRUCT {
        dwData: copydata_tag(),
        cbData: frame.len() as u32,
        lpData: frame.as_ptr().cast_mut().cast::<c_void>(),
    };
    let mut result = 0usize;
    // SAFETY: `cds` and the frame it points to live for the call (the system copies them to the receiver); `result`
    // is writable.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(std::ptr::from_ref(&cds) as isize),
            SMTO_ABORTIFHUNG | SMTO_NORMAL,
            SEND_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    if sent.0 != 0 && result == 1 {
        Acquire::Forwarded
    } else {
        oled_sys::log!("tasks: the running instance did not take the Show");
        Acquire::NoAnswer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(pid: u32, same_image: bool) -> Candidate {
        Candidate { hwnd: pid as isize * 10, pid, same_image }
    }

    #[test]
    fn forward_target_picks_same_image_not_self() {
        let own = 100;
        assert_eq!(forward_target(&[], own), None);
        assert_eq!(forward_target(&[c(7, false)], own), None, "another program's window of the class");
        assert_eq!(forward_target(&[c(own, true)], own), None, "never ourselves");
        assert_eq!(forward_target(&[c(7, false), c(own, true), c(9, true), c(11, true)], own), Some(c(9, true)));
    }

    #[test]
    fn no_window_decision() {
        assert!(matches!(on_no_window(true), Acquire::NoAnswer), "a chord start gives up");
        assert!(matches!(on_no_window(false), Acquire::Unguarded), "any other start opens its own window");
    }

    #[test]
    fn dropping_a_guard_that_holds_nothing_is_harmless() {
        drop(Guard);
        assert_eq!(HELD.load(Ordering::Acquire), 0);
    }
}
