//! The enumerator on real folders: the first-batch bench (EN1) and the enumerate-and-sort bench (EN4). Both are
//! ignored benches of the CI bench job (release, serial); nothing here is asserted against wall-clock time except the
//! budget rows they name, and those only once the row is active: a pending row is declared, not yet gated
//! (budgets.json's protocol), so its bench prints its line and the measured value.

mod common;

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use common::temp::TempRoot;
use files_engine::listing::{
    CancelToken, EntryStore, Enumerator, FindEnumerator, Gen, GroupBy, SortDir, SortKey, WidePath,
};

/// The value of the budgets.json row `id` and whether it gates (status "active").
fn row(id: &str) -> (f64, bool) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("budgets.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let v: serde_json::Value = serde_json::from_str(&text).expect("budgets.json parses");
    let rows = v["rows"].as_array().expect("budgets.json has rows");
    let r = rows.iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("budgets.json has no row {id}"));
    (r["value"].as_f64().expect("the row has a value"), r["status"] == "active")
}

/// Fails when `value` is over the row `id` and the row is active; a pending row only prints the comparison.
fn gate(id: &str, value: f64, what: &str) {
    let (budget, active) = row(id);
    if active {
        assert!(value <= budget, "{what} {value} over {budget} ({id})");
    } else if value > budget {
        println!("{id} is pending, not gated: {what} {value} over {budget}");
    }
}

fn wide(p: &Path) -> WidePath {
    WidePath::new(&p.as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap()
}

/// A folder of `n` empty files (setup, never timed).
fn folder_of(tag: &str, n: usize) -> TempRoot {
    let root = TempRoot::new(tag);
    for i in 0..n {
        root.file(&format!("file {i:06}.txt"), b"");
    }
    root
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_first_batch() {
    let root = folder_of("bench_first_batch", 10_000);
    let dir = wide(root.path());
    let mut firsts: Vec<u32> = Vec::new();
    for _ in 0..50 {
        let mut first: Option<u32> = None;
        let mut total = 0usize;
        FindEnumerator::default().run(&dir, Gen(1), &CancelToken::new(), &mut |b| {
            first.get_or_insert(b.elapsed_us);
            total += b.entries.len();
        });
        assert_eq!(total, 10_000);
        firsts.push(first.expect("a first batch"));
    }
    firsts.sort_unstable();
    let p99 = firsts[(firsts.len() * 99).div_ceil(100) - 1];
    println!("bench: files first_batch {p99} us");
    gate("files.first_batch_p99", f64::from(p99), "p99 of the first batch in us");
}

/// One run of the headless list: enumerate, append every batch, merge it, then sort by name. Returns the store.
fn list_and_sort(dir: &WidePath) -> EntryStore {
    let mut store = EntryStore::new(dir.clone(), Gen(1));
    FindEnumerator::default().run(dir, Gen(1), &CancelToken::new(), &mut |b| {
        store.append(b);
        store.merge_pending();
    });
    store.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
    store
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_enum_sort_10k() {
    let root = folder_of("bench_enum_sort_10k", 10_000);
    let dir = wide(root.path());
    let mut ms: Vec<f64> = Vec::new();
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let store = list_and_sort(&dir);
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(store.len(), 10_000);
        assert_eq!(store.order().0.len(), 10_000);
        assert!(store.done() && store.error().is_none());
    }
    ms.sort_by(f64::total_cmp);
    let median = ms[ms.len() / 2];
    println!("bench: files enum_sort_10k {median:.1} ms");
    gate("files.enum_sort_10k_ci", median, "median in ms");
}
