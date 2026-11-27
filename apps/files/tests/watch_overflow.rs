//! Overflow: with the read buffer shrunk (the debug-only FILES_TEST_WATCH_BUF override, 64 bytes: a record of a long name alone does not fit) creates overflow it,
//! a Rescan arrives, and rescan_merge makes the store equal a fresh enumeration. Its own test binary: the override is
//! process-wide.

mod common;

use std::collections::BTreeMap;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::listing::{
    rescan_merge, CancelToken, EntryStore, Enumerator, FindEnumerator, Gen, GroupBy, SortDir, SortKey, WidePath,
};
use files_engine::watch::{resolve, WatchEvent, Watcher};

fn wide(p: &Path) -> WidePath {
    WidePath::new(&p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
}

/// A fresh enumeration of `dir` into a store (sorted, so order() lists every entry).
fn enumerate(dir: &WidePath, gen: Gen) -> EntryStore {
    let mut store = EntryStore::new(dir.clone(), gen);
    FindEnumerator::default().run(dir, gen, &CancelToken::new(), &mut |b| {
        store.append(b);
        store.merge_pending();
    });
    store.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
    store
}

type Snapshot = BTreeMap<Vec<u8>, (u32, u64, i64)>;

/// Name, attributes, size and write time of every entry.
fn snapshot(s: &EntryStore) -> Snapshot {
    s.order()
        .0
        .iter()
        .map(|&id| {
            let e = s.entry(id).expect("an ordered id is live");
            (s.name(id).expect("a live name").to_vec(), (e.attrs, e.size, e.mtime))
        })
        .collect()
}

/// Applies what the watcher has produced, the way the UI would: resolve a delivery, rescan on overflow.
fn apply_events(store: &mut EntryStore, dir: &WidePath, gen: Gen, events: Vec<WatchEvent>, rescans: &mut u32) {
    for e in events {
        match e {
            WatchEvent::Changes { items, .. } => {
                let d = resolve(dir, gen, &items);
                store.apply(&d, None);
            }
            WatchEvent::Rescan { .. } => {
                *rescans += 1;
                rescan_merge(store, enumerate(dir, gen));
            }
            WatchEvent::Gone { .. } | WatchEvent::Limited { .. } => panic!("the folder is still there: {e:?}"),
            WatchEvent::Drives(_) | WatchEvent::AssocChanged => {}
        }
    }
}

/// Drains until the store equals a fresh enumeration, up to `limit`; true when it does.
fn settle(
    w: &Watcher,
    wake: &Receiver<()>,
    store: &mut EntryStore,
    dir: &WidePath,
    gen: Gen,
    rescans: &mut u32,
    limit: Duration,
) -> bool {
    let end = Instant::now() + limit;
    loop {
        let _ = wake.recv_timeout(Duration::from_millis(150));
        apply_events(store, dir, gen, w.drain(), rescans);
        if snapshot(store) == snapshot(&enumerate(dir, gen)) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
    }
}

fn rig() -> (Watcher, Receiver<()>) {
    let (tx, rx) = channel::<()>();
    let tx = Mutex::new(tx);
    let w = Watcher::new(Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    }));
    (w, rx)
}

#[test]
fn storm_overflow() {
    // Single test in this binary, set before any watcher exists.
    std::env::set_var("FILES_TEST_WATCH_BUF", "64");
    let root = TempRoot::new("watch_storm_overflow");
    let dir = wide(root.path());
    let gen = Gen(1);
    let mut store = enumerate(&dir, gen);
    let (w, wake) = rig();
    let id = w.watch(&dir, gen).unwrap();
    let mut rescans = 0u32;
    let mut made = 0usize;
    // 300 quick creates per round; a round that happens not to overflow (the thread kept up) is followed by another.
    for round in 0..8 {
        for i in 0..300 {
            root.file(&format!("a burst file with a name too long for the buffer {round}-{i:03}.txt"), b"x");
            made += 1;
        }
        apply_events(&mut store, &dir, gen, w.drain(), &mut rescans);
        if rescans > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
        apply_events(&mut store, &dir, gen, w.drain(), &mut rescans);
        if rescans > 0 {
            break;
        }
    }
    assert!(
        settle(&w, &wake, &mut store, &dir, gen, &mut rescans, Duration::from_secs(30)),
        "the store never equalled a fresh enumeration"
    );
    assert!(rescans >= 1 && w.stats().overflows >= 1, "no overflow after {made} creates in a 64-byte buffer");
    assert_eq!(store.len() as usize, made);
    w.unwatch(id);
}
