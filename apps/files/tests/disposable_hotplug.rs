//! F4.11: a drive that comes and goes, on the throwaway runner only. The real Watcher (drives on) tells the UI that
//! something changed, drives::scan reads the letters, and a headless sidebar model (sidebar::build over the drive
//! cache) must show the new letter within a second of the attach and drop it within a second of the detach.
//! Ignored by default and guarded by common::disposable(): the script it drives changes the machine's disks.
#![cfg(windows)]

mod common;

use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
use std::sync::Arc;
use std::time::{Duration, Instant};

use files_engine::drives::{self, DriveApi, SystemApi};
use files_engine::fakes::Fakes;
use files_engine::format::Fmt;
use files_engine::sidebar::{self, Inputs};
use files_engine::watch::{WatchEvent, Watcher};
use files_engine::window::Shared;
use oled_ui::UiLang;

const SCRIPT: &str = "tools/ci/files-disposable.ps1";
const WITHIN: Duration = Duration::from_secs(1);
const OVERALL: Duration = Duration::from_secs(60);

/// The script's output for one mode (its stdout), run from the repository root.
fn run(mode: &str) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let out = Command::new("pwsh")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-File", SCRIPT, "-Hotplug", mode])
        .current_dir(root)
        .output()
        .expect("pwsh runs");
    assert!(out.status.success(), "{mode}: {}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Detaches on the way out, whatever happened.
struct Detach(bool);

impl Drop for Detach {
    fn drop(&mut self) {
        if self.0 {
            let _ = Command::new("pwsh")
                .creation_flags(CREATE_NO_WINDOW)
                .args(["-NoProfile", "-File", SCRIPT, "-Hotplug", "detach"])
                .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
                .output();
        }
    }
}

/// Whether the sidebar model (the rows build() makes from the drive cache) has a row for the letter.
fn shows(shared: &Shared, letter: char) -> bool {
    let fmt = Fmt::fixed_en();
    let drives = shared.drives.borrow();
    let missing = std::collections::HashSet::new();
    let (expanded, children) = (std::collections::HashSet::new(), std::collections::HashMap::new());
    let built = sidebar::build(&Inputs {
        lang: UiLang::EN,
        fmt: &fmt,
        like_explorer: false,
        pins: &[],
        missing: &missing,
        drives: drives.all(),
        network: &[],
        expanded: &expanded,
        children: &children,
        busy: None,
    });
    let want = format!("{letter}:");
    built.items.iter().any(|i| i.label.ends_with(&want) || i.label.ends_with(&format!("({want})")))
}

/// Runs the script on a side thread and polls at once: t0 is when the system's drive mask first gains (or loses) a
/// letter, t1 when the sidebar model follows. Returns (letter, t1 - t0, the script's output); the whole run is
/// bounded by OVERALL.
fn follow(w: &Watcher, shared: &Shared, mode: &'static str, letter: Option<char>) -> (char, Duration, String) {
    let before = SystemApi.present() & 0x03FF_FFFF;
    let script = std::thread::spawn(move || run(mode));
    let start = Instant::now();
    let (mut letter, mut t0) = (letter, None::<Instant>);
    loop {
        assert!(start.elapsed() < OVERALL, "{mode}: nothing happened within {OVERALL:?}");
        if w.drain().iter().any(|e| matches!(e, WatchEvent::Drives(_))) {
            drives::scan(shared);
        }
        let now = SystemApi.present() & 0x03FF_FFFF;
        let changed = if mode == "attach" { now & !before } else { before & !now };
        if t0.is_none() && changed != 0 {
            t0 = Some(Instant::now());
            if letter.is_none() {
                letter = Some(char::from(b'A' + changed.trailing_zeros() as u8));
            }
        }
        if let (Some(t0), Some(l)) = (t0, letter) {
            if shows(shared, l) == (mode == "attach") {
                let took = t0.elapsed();
                let said = script.join().expect("the script thread ends");
                return (l, took, said);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_drive_arrival_within_1s() {
    common::disposable();
    let f = Fakes::new();
    drives::set_api(Some(Arc::new(SystemApi)));
    let shared = f.shared(UiLang::EN);
    drives::scan(&shared);
    let w = Watcher::new(Arc::new(|| {}));
    w.drives(true);
    let mut guard = Detach(true);
    let (letter, took, said) = follow(&w, &shared, "attach", None);
    assert!(took <= WITHIN, "{letter}: the sidebar model took {took:?} after the letter appeared (attach)");
    assert!(said.contains("letter"), "the script prints the letter: {said}");
    let (_, gone, _) = follow(&w, &shared, "detach", Some(letter));
    guard.0 = false;
    assert!(gone <= WITHIN, "{letter}: the sidebar model took {gone:?} after the letter vanished (detach)");
}
