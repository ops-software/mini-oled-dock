//! The CI-only window smoke tests (K1a.7, K2b.8): #[ignore], never run on a developer's machine.

use std::os::windows::process::CommandExt;
use std::process::Command;

/// CREATE_NO_WINDOW: the child gets no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// gallery.exe with `args`, without a console; its exit status and stdout.
fn gallery(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_gallery"))
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("gallery.exe starts");
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
#[ignore = "CI only: opens a window (P2-kit 0.4 rule 1)"]
fn smoke_measure_empty_runs() {
    let (ok, text) = gallery(&["--measure", "empty", "--secs", "1"]);
    assert!(ok, "exit status: {text}");
    let line = text.lines().last().expect("one line");
    let v: serde_json::Value = serde_json::from_str(line).expect("a JSON line");
    for k in ["private_bytes", "first_present_ms", "dpi", "ole"] {
        assert!(v.get(k).is_some(), "{k} in {line}");
    }
    assert!(v["private_bytes"].as_u64().is_some_and(|b| b > 0), "{line}");
    assert!(v["first_present_ms"].as_f64().is_some(), "the window was painted: {line}");
}

/// S-A11Y (K2b.8): with no UIA client attached, a static page idles as without the bridge: the AccessKit adapter stays
/// lazy until a client sends WM_GETOBJECT. CI runs it on the a11y build (`--features oled-ui/a11y`, K.CI2).
#[test]
#[ignore = "CI only: opens a window (P2-kit 0.4 rule 1)"]
fn smoke_idle_a11y_no_wakeups() {
    let (ok, text) = gallery(&["--page", "buttons", "--measure", "idle", "--secs", "10"]);
    assert!(ok, "exit status: {text}");
    let line = text.lines().last().expect("one line");
    let v: serde_json::Value = serde_json::from_str(line).expect("a JSON line");
    for k in ["wakeups", "timers_armed", "timers_live"] {
        assert_eq!(v[k].as_u64(), Some(0), "{k} in {line}");
    }
}
