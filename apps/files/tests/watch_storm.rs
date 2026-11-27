//! Storms: thousands of creates and deletes while the test applies every delivery to a store; at the end the store
//! equals a fresh enumeration. Folders are TempRoot's, changed only through TempRoot's methods.

mod common;

use std::collections::BTreeMap;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// `creates` files made, every second one followed by the removal of the one before it (half of them go), while the
/// main thread applies the watcher's deliveries.
fn storm(tag: &str, creates: usize) {
    let root = TempRoot::new(tag);
    let dir = wide(root.path());
    let gen = Gen(1);
    let mut store = enumerate(&dir, gen);
    let (w, wake) = rig();
    let id = w.watch(&dir, gen).unwrap();
    let done = AtomicBool::new(false);
    let mut rescans = 0u32;
    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..creates {
                root.file(&format!("storm {i:06}.txt"), b"data");
                if i % 2 == 1 {
                    root.remove(&format!("storm {:06}.txt", i - 1));
                }
            }
            done.store(true, Ordering::Release);
        });
        while !done.load(Ordering::Acquire) {
            let _ = wake.recv_timeout(Duration::from_millis(50));
            apply_events(&mut store, &dir, gen, w.drain(), &mut rescans);
        }
    });
    assert!(
        settle(&w, &wake, &mut store, &dir, gen, &mut rescans, Duration::from_secs(60)),
        "the store never equalled a fresh enumeration (rescans: {rescans})"
    );
    assert_eq!(store.len() as usize, creates / 2);
    println!("storm {creates}: {} overflows, {rescans} rescans", w.stats().overflows);
    w.unwatch(id);
}

#[test]
fn storm_5000() {
    storm("watch_storm_5000", 5_000);
}

/// The spec's 50,000 creates and 25,000 deletes: the CI disposable job (a slow disk or a busy machine is not the
/// point of a unit run).
#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_storm_50000() {
    common::disposable();
    storm("watch_storm_50000", 50_000);
}
