//! The shell-notification fallback and the drive watch. Its own test binary: FILES_TEST_WATCH_SHELL is process-wide.
//! Nothing here shows a window or sends input; the watcher's own window is message-only.

mod common;

use std::os::windows::ffi::OsStrExt;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::listing::{Gen, WidePath};
use files_engine::watch::{ChangeAction, WatchEvent, Watcher};
use windows::Win32::UI::WindowsAndMessaging::GetShellWindow;

fn wide(p: &std::path::Path) -> WidePath {
    WidePath::new(&p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
}

#[test]
fn fallback_forced_on_temp() {
    // SAFETY: no arguments.
    if unsafe { GetShellWindow() }.0.is_null() {
        println!("note: no shell window on this desktop, the shell fallback test is skipped");
        return;
    }
    std::env::set_var("FILES_TEST_WATCH_SHELL", "1");
    let root = TempRoot::new("watch_fallback_forced");
    let (tx, rx) = channel::<()>();
    let tx = Mutex::new(tx);
    let w = Watcher::new(Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    }));
    let id = w.watch(&wide(root.path()), Gen(5)).expect("the fallback takes the folder");
    assert_eq!(w.stats().watches, 1);
    let mut seen: Vec<WatchEvent> = w.drain();
    assert!(
        seen.iter().any(|e| matches!(e, WatchEvent::Limited { id: i, gen } if *i == id && *gen == Gen(5))),
        "the watch is reported as limited: {seen:?}"
    );
    // The shell sets up its own watch of a newly registered folder asynchronously: a change made in the first moments
    // is not reported (measured here: it was missed with no pause). The test waits it out; a folder that really takes
    // this path (a share) is rarely changed in the instant it is opened.
    std::thread::sleep(Duration::from_millis(1500));
    root.file("made.txt", b"x");
    let start = Instant::now();
    let found = loop {
        let _ = rx.recv_timeout(Duration::from_millis(100));
        seen.extend(w.drain());
        let hit = seen.iter().any(|e| match e {
            WatchEvent::Changes { id: i, items, .. } => {
                *i == id && items.iter().any(|c| c.action == ChangeAction::Added && &*c.name == b"made.txt")
            }
            _ => false,
        });
        if hit {
            break true;
        }
        if start.elapsed() > Duration::from_secs(10) {
            break false;
        }
    };
    println!("fallback: create seen after {} ms", start.elapsed().as_millis());
    assert!(found, "no Added for the create through the shell fallback: {seen:?}");
    w.unwatch(id);
    assert_eq!(w.stats().watches, 0);
    w.unwatch(id);
}

#[test]
fn drives_watch_starts_and_stops() {
    let w = Watcher::new(Arc::new(|| {}));
    w.drives(true);
    w.drives(true);
    w.poke_devices(); // the drives did not change a moment ago
    assert!(!w.drain().iter().any(|e| matches!(e, WatchEvent::Drives(_))));
    w.drives(false);
    w.poke_devices(); // off: nothing to compare with, nothing queued
    assert!(w.drain().is_empty());
}
