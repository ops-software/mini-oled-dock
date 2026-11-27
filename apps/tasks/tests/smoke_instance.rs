//! CI-only smoke tests of the single instance and the forward (T6.1): they start real processes and windows, so they
//! return at once unless the CI variable is set (house rule 1; the live sessions run them by hand on the tester's own
//! machine). They share one mutex name, so they run one at a time.

use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use oled_trust::frame::tasks::{MUTEX_NAME, WINDOW_CLASS};
use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

const EXE: &str = env!("CARGO_BIN_EXE_tasks");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static LOCK: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ci() -> bool {
    std::env::var_os("CI").is_some()
}

fn start(args: &[&str]) -> Child {
    Command::new(EXE)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("tasks starts")
}

/// The MiniOledTasks windows of `pid` (all of them when `pid` is None).
fn windows_of(pid: Option<u32>) -> Vec<HWND> {
    struct Probe {
        pid: Option<u32>,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the address of the Probe on the caller's stack, alive for the whole EnumWindows call.
        let p = unsafe { &mut *(lparam.0 as *mut Probe) };
        let mut class = [0u16; 64];
        // SAFETY: the buffer is writable for its whole length.
        let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        let mut owner = 0u32;
        // SAFETY: owner is writable; a window that is gone gives 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if String::from_utf16_lossy(&class[..n]) == WINDOW_CLASS && p.pid.is_none_or(|x| x == owner) {
            p.found.push(hwnd);
        }
        BOOL(1)
    }
    let mut p = Probe { pid, found: Vec::new() };
    // SAFETY: the callback only reads its arguments and writes the probe, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut p as *mut Probe as isize)) };
    p.found
}

fn wait_window(pid: u32, within: Duration) -> Option<HWND> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(w) = windows_of(Some(pid)).first() {
            return Some(*w);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn wait_exit(c: &mut Child, within: Duration) -> Option<ExitStatus> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(s) = c.try_wait().expect("wait") {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Closes the window the way a user does and waits for the exit; kills what stays.
fn close(mut c: Child, hwnd: Option<HWND>) {
    if let Some(h) = hwnd {
        // SAFETY: posts WM_CLOSE to a window of the child this test started.
        let _ = unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    if wait_exit(&mut c, Duration::from_secs(5)).is_none() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn log_text() -> String {
    oled_sys::datadir::known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
        .map(|d| d.join("mini-oled-dock").join("tasks").join("tasks.log"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

#[test]
#[ignore = "starts real windows: CI only"]
fn second_start_forwards_and_exits_0() {
    if !ci() {
        return;
    }
    let _g = serial();
    let owner = start(&[]);
    let hwnd = wait_window(owner.id(), Duration::from_secs(10)).expect("the first window shows");
    let before = log_text().len();
    let begun = Instant::now();
    let mut second = start(&["--select", "4"]);
    let status = wait_exit(&mut second, Duration::from_millis(1000)).expect("the second start exits within 1 s");
    assert_eq!(status.code(), Some(0), "forwarded");
    assert!(begun.elapsed() < Duration::from_secs(1), "within the budget");
    std::thread::sleep(Duration::from_millis(300));
    let log = log_text();
    let appended = log.get(before..).unwrap_or_default();
    assert!(appended.contains("tasks: show applied"), "the first one applied the Show");
    assert_eq!(windows_of(None).len(), 1, "still one window");
    close(owner, Some(hwnd));
}

#[test]
#[ignore = "starts real windows: CI only"]
fn a_squatter_with_no_window() {
    if !ci() {
        return;
    }
    let _g = serial();
    // SAFETY: a NUL-terminated name that lives for the call; the handle is closed at the end.
    let squat = unsafe { CreateMutexW(None, false, &HSTRING::from(MUTEX_NAME)) }.expect("the test takes the name");
    // A chord start gives up.
    let begun = Instant::now();
    let mut chord = start(&["--from-chord"]);
    let status = wait_exit(&mut chord, Duration::from_millis(1500)).expect("the chord start exits");
    assert_eq!(status.code(), Some(3), "no usable instance");
    assert!(begun.elapsed() < Duration::from_millis(1500));
    assert!(windows_of(Some(chord.id())).is_empty(), "no window of its pid");
    // Any other start opens an unguarded window.
    let other = start(&["--select", "4"]);
    let hwnd = wait_window(other.id(), Duration::from_secs(2)).expect("an unguarded window shows within 2 s");
    close(other, Some(hwnd));
    // SAFETY: the handle CreateMutexW returned above.
    let _ = unsafe { CloseHandle(squat) };
}

/// A fault-inject build whose fault.txt says hang-after-show 5000 (the test writes it first): the second start exits 3
/// within 1.5 s.
#[cfg(feature = "fault-inject")]
#[test]
#[ignore = "starts real windows: CI only"]
fn a_hung_owner_gives_exit_3() {
    if !ci() {
        return;
    }
    let _g = serial();
    let dir = oled_sys::datadir::known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
        .map(|d| d.join("mini-oled-dock").join("tasks"))
        .expect("the local folder");
    let fault = dir.join("fault.txt");
    std::fs::create_dir_all(&dir).expect("the folder");
    std::fs::write(&fault, "hang-after-show 5000\n").expect("the fault script");
    let owner = start(&[]);
    let hwnd = wait_window(owner.id(), Duration::from_secs(10));
    let mut second = start(&["--select", "4"]);
    let status = wait_exit(&mut second, Duration::from_millis(1500));
    let _ = std::fs::remove_file(&fault);
    assert_eq!(status.and_then(|s| s.code()), Some(3), "a hung owner does not answer");
    close(owner, hwnd);
}
