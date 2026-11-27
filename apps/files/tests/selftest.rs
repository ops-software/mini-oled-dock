//! --selftest (P3-files-app F1a.18): the children are files.exe with their own FILES_TEST_TAG, FILES_DATA_DIR and
//! FILES_CONFIG_DIR under a TempRoot; none opens a visible window. The tests take one lock.

mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use common::{files_exe, spawn_no_window, test_tag, ChildGuard};
use files_engine::names;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, EnumWindows, GetWindowThreadProcessId, HWND_MESSAGE};

static LOCK: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn user_sid() -> String {
    oled_sys::pipe::current_user_sid().expect("the user's SID")
}

/// A selftest child: files.exe --selftest --signal <nonce>, tagged, with its folders under `root`.
fn selftest(root: &TempRoot, tag: &str, nonce: &str, hook: Option<&str>) -> ChildGuard {
    let (data, config) = (s(&root.at("data")), s(&root.at("config")));
    let parent = std::process::id().to_string();
    let mut env: Vec<(&str, &str)> = vec![
        (names::TEST_TAG_VAR, tag),
        ("FILES_DATA_DIR", &data),
        ("FILES_CONFIG_DIR", &config),
        ("FILES_TEST_PARENT", &parent),
    ];
    if let Some(h) = hook {
        env.push((files_engine::selftest::HOOK_VAR, h));
    }
    spawn_no_window(&files_exe(), &["--selftest", "--signal", nonce], &env)
}

fn lines_of(g: &mut ChildGuard) -> std::sync::mpsc::Receiver<String> {
    let out = g.take_stdout().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The windows of `pid`: top-level ones, and the message-only ones (children of HWND_MESSAGE).
fn windows_of(pid: u32) -> usize {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the (pid, count) pair below, alive for the enumeration.
        let found = unsafe { &mut *(lp.0 as *mut (u32, usize)) };
        let mut pid = 0u32;
        // SAFETY: `pid` is writable.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == found.0 {
            found.1 += 1;
        }
        BOOL(1)
    }
    let mut found = (pid, 0usize);
    let lp = LPARAM(std::ptr::from_mut(&mut found) as isize);
    // SAFETY: the callback reads only `found`, which outlives the calls.
    unsafe {
        let _ = EnumWindows(Some(each), lp);
        let _ = EnumChildWindows(Some(HWND_MESSAGE), Some(each), lp);
    }
    found.1
}

#[test]
fn selftest_passes_while_a_normal_instance_runs() {
    let _l = serial();
    let tag = test_tag();
    let root = TempRoot::new("selftest_beside");
    let (data, config) = (s(&root.at("data")), s(&root.at("config")));
    let parent = std::process::id().to_string();
    let out = s(&root.at("out.jsonl"));
    // A test-ipc child of the same tag holds the mutex and the IPC window.
    let env: [(&str, &str); 5] = [
        (names::TEST_TAG_VAR, &tag),
        ("FILES_TEST_OUT", &out),
        ("FILES_TEST_PARENT", &parent),
        ("FILES_DATA_DIR", &data),
        ("FILES_CONFIG_DIR", &config),
    ];
    let mut holder = spawn_no_window(&files_exe(), &["--role=test-ipc"], &env);
    let rx = lines_of(&mut holder);
    let ready = rx.recv_timeout(Duration::from_secs(60)).expect("the holder is ready");
    assert!(ready.starts_with("ready "), "{ready}");

    let (waiter, nonce) = oled_sys::selftest::Waiter::create("files", "selftest", &user_sid()).expect("the waiter");
    let mut child = selftest(&root, &tag, &nonce, None);
    assert!(waiter.wait(Duration::from_secs(10)), "no signal within 10 s");
    let status = child.wait_timeout(Duration::from_secs(10)).expect("exits");
    assert_eq!(status.code(), Some(0));
    // The holder still runs and took no command: nothing was forwarded.
    assert!(holder.wait_timeout(Duration::from_millis(100)).is_none(), "the instance still runs");
    let taken = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(taken.trim().is_empty(), "a command was forwarded: {taken}");
    assert!(rx.try_recv().is_err(), "the holder printed something else");
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the pause hook is read by a debug files.exe only")]
fn selftest_creates_no_window() {
    let _l = serial();
    let root = TempRoot::new("selftest_windows");
    let (_waiter, nonce) = oled_sys::selftest::Waiter::create("files", "selftest", &user_sid()).expect("the waiter");
    let mut child = selftest(&root, &test_tag(), &nonce, Some("pause"));
    let pid = child.pid();
    let rx = lines_of(&mut child);
    // Paused after its checks: sample the windows until it says so (and once after).
    let end = Instant::now() + Duration::from_secs(60);
    let mut paused = false;
    while !paused {
        assert_eq!(windows_of(pid), 0, "the selftest opened a window");
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(l) => paused = l == "paused",
            Err(_) => assert!(Instant::now() < end, "never paused"),
        }
    }
    assert_eq!(windows_of(pid), 0, "the selftest has a window after its checks");
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the drop-it hook is read by a debug files.exe only")]
fn selftest_fails_on_a_missing_string() {
    let _l = serial();
    let root = TempRoot::new("selftest_missing");
    let (waiter, nonce) = oled_sys::selftest::Waiter::create("files", "selftest", &user_sid()).expect("the waiter");
    let mut child = selftest(&root, &test_tag(), &nonce, Some("drop-it"));
    let status = child.wait_timeout(Duration::from_secs(60)).expect("exits");
    assert_eq!(status.code(), Some(1));
    assert!(!waiter.wait(Duration::from_millis(100)), "a failed selftest signalled");
}

#[test]
fn selftest_writes_nothing() {
    let _l = serial();
    let root = TempRoot::new("selftest_writes");
    // An old-format files.json in the config folder, an empty data folder.
    root.dir("data");
    root.file(
        "config/files.json",
        b"// mini-oled-dock: files options\n{ \"show_extensions\": false, \"old_key\": 1 }\n",
    );
    let before = (root.hash_tree("config"), root.hash_tree("data"));
    let (waiter, nonce) = oled_sys::selftest::Waiter::create("files", "selftest", &user_sid()).expect("the waiter");
    let mut child = selftest(&root, &test_tag(), &nonce, None);
    assert_eq!(child.wait_timeout(Duration::from_secs(60)).expect("exits").code(), Some(0));
    assert!(waiter.wait(Duration::from_millis(100)), "signalled");
    let after = (root.hash_tree("config"), root.hash_tree("data"));
    assert_eq!(after.0, before.0, "the config folder changed");
    // The log file is the only thing the data folder may gain.
    let extra: Vec<_> = after.1.keys().filter(|k| !before.1.contains_key(*k)).collect();
    assert!(extra.iter().all(|k| k.ends_with("files.log")), "new files: {extra:?}");
    for (k, v) in &before.1 {
        assert_eq!(after.1.get(k), Some(v), "{k} changed");
    }
}
