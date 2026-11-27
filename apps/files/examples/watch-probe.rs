//! watch-probe: the change watcher on a real folder, for the live tester.
//!
//!   watch-probe storm --root <dir> [--n <files>]    makes <n> files and removes them again through LiveRoot while it
//!                                                   applies every delivery to a store; prints the longest single
//!                                                   apply and whether the store ends equal to a fresh listing
//!   watch-probe idle  --root <dir> [--secs <s>]     arms one watch and does nothing for <s> seconds; prints the
//!                                                   watcher's counters (an idle watcher has none to count)
//!
//! <dir> must be a mod-files-live-* folder (or inside one) under %TEMP%, as LiveRoot::at demands. The probe changes
//! files only inside it, only through LiveRoot, and shows no window.

#[path = "common/live.rs"]
mod live;

use std::collections::BTreeMap;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use files_engine::listing::{
    rescan_merge, CancelToken, EntryStore, Enumerator, FindEnumerator, Gen, GroupBy, SortDir, SortKey, WidePath,
};
use files_engine::watch::{resolve, WatchEvent, Watcher};
use live::LiveRoot;

const USAGE: &str = "usage: watch-probe storm --root <dir> [--n <files>] | watch-probe idle --root <dir> [--secs <s>]";

fn fail(msg: &str) -> ! {
    eprintln!("watch-probe: {msg}\n{USAGE}");
    std::process::exit(2);
}

fn enumerate(dir: &WidePath, gen: Gen) -> EntryStore {
    let mut store = EntryStore::new(dir.clone(), gen);
    FindEnumerator::default().run(dir, gen, &CancelToken::new(), &mut |b| {
        store.append(b);
        store.merge_pending();
    });
    store.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
    store
}

fn snapshot(s: &EntryStore) -> BTreeMap<Vec<u8>, (u32, u64, i64)> {
    s.order()
        .0
        .iter()
        .filter_map(|&id| {
            let e = s.entry(id)?;
            Some((s.name(id)?.to_vec(), (e.attrs, e.size, e.mtime)))
        })
        .collect()
}

struct Applied {
    longest: Duration,
    deliveries: u32,
    rescans: u32,
}

fn apply(store: &mut EntryStore, dir: &WidePath, gen: Gen, events: Vec<WatchEvent>, got: &mut Applied) {
    for e in events {
        let t = Instant::now();
        match e {
            WatchEvent::Changes { items, .. } => {
                let d = resolve(dir, gen, &items);
                let t = Instant::now(); // only the store's apply is timed: resolve runs off the UI thread
                store.apply(&d, None);
                got.longest = got.longest.max(t.elapsed());
                got.deliveries += 1;
                continue;
            }
            WatchEvent::Rescan { .. } => {
                got.rescans += 1;
                rescan_merge(store, enumerate(dir, gen));
            }
            _ => {}
        }
        got.longest = got.longest.max(t.elapsed());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| fail("a mode is required"));
    let (mut root, mut n, mut secs) = (PathBuf::new(), 50_000u32, 60u64);
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--root" => root = PathBuf::from(value()),
            "--n" => n = value().parse().unwrap_or_else(|_| fail("--n takes a number")),
            "--secs" => secs = value().parse().unwrap_or_else(|_| fail("--secs takes a number")),
            other => fail(&format!("unknown argument {other}")),
        }
    }
    if root.as_os_str().is_empty() {
        fail("--root <dir> is required");
    }
    let live = LiveRoot::at(&root).unwrap_or_else(|e| fail(&e));
    let units: Vec<u16> = live.path().as_os_str().encode_wide().collect();
    let dir = WidePath::new(&units).unwrap_or_else(|e| fail(&format!("bad path: {e:?}")));
    let gen = Gen(1);
    let (tx, rx) = channel::<()>();
    let tx = Mutex::new(tx);
    let watcher = Watcher::new(Arc::new(move || {
        let _ = tx.lock().map(|t| t.send(()));
    }));
    let id = watcher.watch(&dir, gen).unwrap_or_else(|e| fail(&format!("cannot watch: {e:?}")));
    match mode.as_str() {
        "storm" => {
            let mut store = enumerate(&dir, gen);
            let mut got = Applied { longest: Duration::ZERO, deliveries: 0, rescans: 0 };
            let done = AtomicBool::new(false);
            let start = Instant::now();
            std::thread::scope(|s| {
                s.spawn(|| {
                    for i in 0..n {
                        live.file(&format!("probe {i:06}.txt"), b"x");
                    }
                    for i in 0..n {
                        live.remove(&format!("probe {i:06}.txt"));
                    }
                    done.store(true, Ordering::Release);
                });
                while !done.load(Ordering::Acquire) {
                    let _ = rx.recv_timeout(Duration::from_millis(50));
                    apply(&mut store, &dir, gen, watcher.drain(), &mut got);
                }
            });
            let mut equal = false;
            let end = Instant::now() + Duration::from_secs(60);
            while !equal && Instant::now() < end {
                let _ = rx.recv_timeout(Duration::from_millis(200));
                apply(&mut store, &dir, gen, watcher.drain(), &mut got);
                equal = snapshot(&store) == snapshot(&enumerate(&dir, gen));
            }
            let st = watcher.stats();
            println!(
                "watch-probe storm: n={n} deliveries={} rescans={} overflows={} longest_apply_ms={:.2} elapsed_ms={} store_equals_listing={equal}",
                got.deliveries,
                got.rescans,
                st.overflows,
                got.longest.as_secs_f64() * 1000.0,
                start.elapsed().as_millis(),
            );
            if !equal {
                std::process::exit(1);
            }
        }
        "idle" => {
            std::thread::sleep(Duration::from_millis(500));
            let before = watcher.stats();
            std::thread::sleep(Duration::from_secs(secs));
            let after = watcher.stats();
            println!(
                "watch-probe idle: secs={secs} completions={} timeouts={} watches={} events={}",
                after.completions - before.completions,
                after.timeouts - before.timeouts,
                after.watches,
                watcher.drain().len(),
            );
        }
        other => fail(&format!("unknown mode {other}")),
    }
    watcher.unwatch(id);
}
