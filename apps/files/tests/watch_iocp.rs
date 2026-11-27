//! The overlapped change watcher on real folders (TempRoot only; files change only through TempRoot's methods).

mod common;

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::listing::{Gen, WidePath};
use files_engine::watch::{ChangeAction, WatchEvent, WatchId, Watcher};

fn wide(p: &Path) -> WidePath {
    WidePath::new(&p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
}

/// A watcher whose wake-up callback feeds a channel, and every event it has produced so far.
struct Rig {
    w: Watcher,
    wake: Receiver<()>,
    seen: Mutex<Vec<WatchEvent>>,
}

fn rig() -> Rig {
    let (tx, rx) = channel::<()>();
    let tx = Mutex::new(tx);
    let w = Watcher::new(Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    }));
    Rig { w, wake: rx, seen: Mutex::new(Vec::new()) }
}

impl Rig {
    /// Waits up to `limit` until `done` holds over everything seen so far.
    fn wait(&self, limit: Duration, done: impl Fn(&[WatchEvent]) -> bool) -> bool {
        let end = Instant::now() + limit;
        loop {
            {
                let mut seen = self.seen.lock().unwrap();
                seen.extend(self.w.drain());
                if done(&seen) {
                    return true;
                }
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let _ = self.wake.recv_timeout(left.min(Duration::from_millis(200)));
        }
    }

    fn events(&self) -> Vec<WatchEvent> {
        self.seen.lock().unwrap().clone()
    }
}

fn names_with(events: &[WatchEvent], id: WatchId, action: ChangeAction) -> Vec<String> {
    let mut out = Vec::new();
    for e in events {
        if let WatchEvent::Changes { id: i, items, .. } = e {
            if *i == id {
                out.extend(
                    items.iter().filter(|c| c.action == action).map(|c| String::from_utf8_lossy(&c.name).into_owned()),
                );
            }
        }
    }
    out
}

const SEEN: Duration = Duration::from_secs(5);

#[test]
fn create_seen() {
    let root = TempRoot::new("watch_create_seen");
    let r = rig();
    let id = r.w.watch(&wide(root.path()), Gen(3)).unwrap();
    root.file("a.txt", b"x");
    assert!(r.wait(SEEN, |e| names_with(e, id, ChangeAction::Added).iter().any(|n| n == "a.txt")));
    assert!(r.events().iter().all(|e| !matches!(e, WatchEvent::Changes { gen, .. } if *gen != Gen(3))));
    r.w.unwatch(id);
}

#[test]
fn modify_and_remove_seen() {
    let root = TempRoot::new("watch_modify_remove");
    root.file("m.txt", b"1");
    let r = rig();
    let id = r.w.watch(&wide(root.path()), Gen(1)).unwrap();
    root.write("m.txt", b"longer content");
    assert!(r.wait(SEEN, |e| names_with(e, id, ChangeAction::Modified).iter().any(|n| n == "m.txt")));
    root.remove("m.txt");
    assert!(r.wait(SEEN, |e| names_with(e, id, ChangeAction::Removed).iter().any(|n| n == "m.txt")));
    r.w.unwatch(id);
}

#[test]
fn rename_pairs() {
    let root = TempRoot::new("watch_rename_pairs");
    root.file("old name.txt", b"x");
    let r = rig();
    let id = r.w.watch(&wide(root.path()), Gen(1)).unwrap();
    root.rename("old name.txt", "new name.txt");
    assert!(r.wait(SEEN, |e| !names_with(e, id, ChangeAction::RenamedTo).is_empty()));
    let flat: Vec<(ChangeAction, String)> = r
        .events()
        .iter()
        .filter_map(|e| match e {
            WatchEvent::Changes { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .map(|c| (c.action, String::from_utf8_lossy(&c.name).into_owned()))
        .collect();
    let from = flat.iter().position(|x| *x == (ChangeAction::RenamedFrom, "old name.txt".to_string()));
    let to = flat.iter().position(|x| *x == (ChangeAction::RenamedTo, "new name.txt".to_string()));
    assert!(from.is_some() && to == from.map(|f| f + 1), "RenamedFrom then RenamedTo: {flat:?}");
    r.w.unwatch(id);
}

#[test]
fn delete_watched_folder_gone() {
    let root = TempRoot::new("watch_gone");
    root.dir("inner");
    let r = rig();
    let id = r.w.watch(&wide(&root.path().join("inner")), Gen(9)).unwrap();
    root.remove_dir("inner");
    assert!(
        r.wait(SEEN, |e| e.iter().any(|x| matches!(x, WatchEvent::Gone { id: i, gen } if *i == id && *gen == Gen(9))))
    );
    r.w.unwatch(id);
}

#[test]
fn missing_folder_is_an_open_error() {
    let root = TempRoot::new("watch_missing");
    let r = rig();
    let err = r.w.watch(&wide(&root.path().join("nope")), Gen(1)).unwrap_err();
    assert!(matches!(err, files_engine::watch::WatchError::Open(c) if c != 0), "{err:?}");
}

#[test]
fn coalesces() {
    let root = TempRoot::new("watch_coalesces");
    let r = rig();
    let id = r.w.watch(&wide(root.path()), Gen(1)).unwrap();
    for i in 0..100 {
        root.file(&format!("f{i:03}.txt"), b"");
    }
    assert!(r.wait(SEEN, |e| names_with(e, id, ChangeAction::Added).len() >= 100));
    let deliveries = r.events().iter().filter(|e| matches!(e, WatchEvent::Changes { .. })).count();
    assert!(deliveries <= 3, "100 creates arrived in {deliveries} deliveries");
    r.w.unwatch(id);
}

#[test]
fn idle_has_no_wakes() {
    let root = TempRoot::new("watch_idle");
    let r = rig();
    let id = r.w.watch(&wide(root.path()), Gen(1)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let before = r.w.stats();
    assert_eq!(before.watches, 1);
    std::thread::sleep(Duration::from_secs(2));
    let after = r.w.stats();
    assert_eq!((before.completions, before.timeouts), (after.completions, after.timeouts));
    r.w.unwatch(id);
    assert!(r.w.drain().is_empty());
}

#[test]
fn unwatch_releases() {
    let root = TempRoot::new("watch_unwatch");
    root.dir("held");
    let r = rig();
    let id = r.w.watch(&wide(&root.path().join("held")), Gen(1)).unwrap();
    root.file("held/x.txt", b"");
    r.w.unwatch(id);
    r.w.unwatch(id); // twice is harmless
    assert_eq!(r.w.stats().watches, 0);
    root.remove("held/x.txt");
    root.remove_dir("held"); // would fail while the watch handle were open without FILE_SHARE_DELETE or still held
    assert!(!r.w.drain().iter().any(|e| matches!(e, WatchEvent::Changes { id: i, .. } if *i == id)));
}

#[test]
fn many_watches_and_drop() {
    let root = TempRoot::new("watch_many");
    let r = rig();
    let mut ids = Vec::new();
    for i in 0..5 {
        root.dir(&format!("d{i}"));
        ids.push(r.w.watch(&wide(&root.path().join(format!("d{i}"))), Gen(i)).unwrap());
    }
    root.file("d3/x", b"");
    assert!(r.wait(SEEN, |e| !names_with(e, ids[3], ChangeAction::Added).is_empty()));
    assert_eq!(r.w.stats().watches, 5);
    drop(r); // the watcher goes with watches still armed: the thread stops and frees them
}
