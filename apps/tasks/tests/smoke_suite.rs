//! CI-only smoke tests of --selftest (TS1): they start real processes, so they return at once unless the CI variable is
//! set (house rule 1; the live sessions run them by hand on the tester's own machine).

use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};

const EXE: &str = env!("CARGO_BIN_EXE_tasks");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn ci() -> bool {
    std::env::var_os("CI").is_some()
}

/// How many top-level windows the process owns right now.
fn windows_of(pid: u32) -> u32 {
    struct Probe {
        pid: u32,
        n: u32,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the address of the Probe on the caller's stack, alive for the whole EnumWindows call.
        let p = unsafe { &mut *(lparam.0 as *mut Probe) };
        let mut owner = 0u32;
        // SAFETY: owner is writable; a window that is gone gives 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if owner == p.pid {
            p.n += 1;
        }
        BOOL(1)
    }
    let mut p = Probe { pid, n: 0 };
    // SAFETY: the callback only reads its arguments and writes the probe, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut p as *mut Probe as isize)) };
    p.n
}

fn user_sid() -> String {
    oled_sys::pipe::current_user_sid().expect("the user's SID")
}

/// Runs `tasks --selftest --signal <nonce>`; true when the event was signalled in time. The child's windows are
/// counted while it runs.
fn selftest(windows_seen: &mut u32) -> (bool, Option<i32>) {
    let (waiter, nonce) = oled_sys::selftest::Waiter::create("tasks", "selftest", &user_sid()).expect("the waiter");
    let mut child: Child = Command::new(EXE)
        .args(["--selftest", "--signal", &nonce])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("tasks starts");
    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        *windows_seen += windows_of(child.id());
        if let Some(s) = child.try_wait().expect("wait") {
            break Some(s);
        }
        if Instant::now() > end {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (waiter.wait(Duration::from_millis(100)), status.and_then(|s| s.code()))
}

#[test]
fn selftest_creates_no_window() {
    if !ci() {
        return;
    }
    let mut seen = 0;
    let (signalled, code) = selftest(&mut seen);
    assert!(signalled, "the self-test signals the waiter");
    assert_eq!(code, Some(0));
    assert_eq!(seen, 0, "a self-test never creates a window");
}

#[test]
fn selftest_passes_while_a_normal_instance_runs() {
    if !ci() {
        return;
    }
    // A real instance first; the self-test must not forward to it, wait for it or take its mutex.
    let mut owner = Command::new(EXE)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("the instance starts");
    std::thread::sleep(Duration::from_millis(1500));
    let mut seen = 0;
    let (signalled, code) = selftest(&mut seen);
    let alive = owner.try_wait().expect("wait").is_none();
    let _ = owner.kill();
    let _ = owner.wait();
    assert!(alive, "the instance kept running");
    assert!(signalled, "the self-test signalled within 10 s");
    assert_eq!(code, Some(0));
    // Nothing was forwarded: the instance's log has no copy_data line.
    let log = oled_sys::datadir::known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
        .map(|d| d.join("mini-oled-dock").join("tasks").join("tasks.log"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    assert!(!log.contains("copy_data"), "the self-test forwarded nothing");
}
