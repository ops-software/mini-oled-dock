//! The entry store at 100,000 entries (ST6): its memory against the row of the budget table, and the sort, filter,
//! apply and merge benches of the bench job. Everything here is in memory: no file is touched.

use std::time::Instant;

use files_engine::listing::{
    natural_cmp, Batch, Diff, EntryStore, FilterQuery, Gen, GroupBy, RawEntry, SortDir, SortKey, WidePath,
};

const N: usize = 100_000;
const FOLDER: u32 = 0x10;

/// A deterministic, unsorted name of about 24 bytes for index i (a bijection on 0..100,000).
fn name_of(i: usize) -> String {
    format!("data {:06} id {:06}.txt", (i * 7_919) % N, i)
}

fn batch(rows: std::ops::Range<usize>, done: bool) -> Batch {
    let mut b = Batch { gen: Gen(1), names: Vec::new(), entries: Vec::new(), done, error: None, elapsed_us: 0 };
    for i in rows {
        let name = name_of(i);
        let off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off: off,
            name_len: name.len() as u16,
            attrs: if i % 10 == 0 { FOLDER } else { 0x20 },
            reparse_tag: 0,
            size: (i as u64 * 31) % 10_007,
            mtime: (i as i64 * 17) % 5_003,
            ctime: 0,
        });
    }
    b
}

/// 100,000 entries appended in the enumerator's batches of 2,048, each merged, sorted by name.
fn big_store() -> EntryStore {
    let mut s = EntryStore::new(WidePath::parse("C:\\bench").unwrap(), Gen(1));
    for start in (0..N).step_by(2_048) {
        let end = (start + 2_048).min(N);
        s.append(batch(start..end, end == N));
        s.merge_pending();
    }
    s.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
    s
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0: the shared CI runner is slower.
fn slack() -> f64 {
    let v = std::env::var("OLED_BENCH_SLACK").ok();
    v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
}

fn ms_of(f: &mut dyn FnMut()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
fn store_100k_fits() {
    let s = big_store();
    assert_eq!(s.len() as usize, N);
    assert_eq!(s.order().0.len(), N, "every entry is visible");
    let p = s.heap_parts();
    println!("heap_parts: {p:?}");
    println!("bench: files store_100k_bytes {} bytes", s.heap_bytes());
    assert_eq!(s.heap_bytes(), p.entries + p.names + p.order + p.inverse + p.index + p.other);
    assert!(s.heap_bytes() <= 8_388_608, "{} bytes over the row of 8,388,608", s.heap_bytes());
    // The exact-length parts hold what the arithmetic of the budget says.
    assert_eq!((p.order, p.inverse, p.index), (400_000, 400_000, 800_000));
}

#[test]
fn fixture_names_order_naturally() {
    let stems = ["a b", "a.b", "Ab", "ab", "B x", "b y", "c", "C"];
    let exts = ["txt", "RS", "md", "", "bin"];
    let mut names: Vec<String> = (0..40usize)
        .map(|i| {
            let (ext, dot) = (exts[i % 5], if exts[i % 5].is_empty() { "" } else { "." });
            format!("{} {:02}{dot}{ext}", stems[i % 8], i)
        })
        .collect();
    let mut by_folded_ordinal = names.clone();
    by_folded_ordinal.sort_by_key(|n| n.to_lowercase());
    names.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
    assert_eq!(names, by_folded_ordinal, "Explorer's order and the folded ordinal order agree on the store's fixture");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_sort_100k() {
    let mut s = big_store();
    let ms = median(
        (0..5)
            .map(|_| {
                ms_of(&mut || {
                    let _ = s.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
                })
            })
            .collect(),
    );
    println!("bench: files sort_100k {ms:.1} ms");
    assert!(ms <= 50.0 * slack(), "median {ms} ms over 50 ms");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_filter_100k() {
    let s = big_store();
    let q = FilterQuery { text: "7".into() };
    let ms = median((0..5).map(|_| ms_of(&mut || assert!(!s.filter(&q, Gen(1)).0.is_empty()))).collect());
    println!("bench: files filter_100k {ms:.1} ms");
    // Recorded only: the gate of the filter belongs to the window's frame budget (budget row files.filter_100k).
    assert!(ms <= 16.0 * slack(), "median {ms} ms over the recorded 16 ms");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_apply_100_at_100k() {
    let mut s = big_store();
    let mut times = Vec::new();
    for round in 0..5usize {
        // 100 changes: 40 removals, 30 changes of a size and a time, 30 new names.
        let mut d = Diff::new(Gen(1));
        let raw =
            |mtime: i64| RawEntry { name_off: 0, name_len: 0, attrs: 0x20, reparse_tag: 0, size: 9, mtime, ctime: 0 };
        for k in 0..40 {
            d.remove(name_of(round * 1_000 + k * 7 + 1).as_bytes());
        }
        for k in 0..30 {
            d.upsert(name_of(round * 1_000 + k * 7 + 3).as_bytes(), raw(9_000 + k as i64));
        }
        for k in 0..30 {
            d.upsert(format!("new {round} {k}").as_bytes(), raw(k as i64));
        }
        times.push(ms_of(&mut || {
            let _ = s.apply(&d, None);
        }));
    }
    let ms = median(times);
    println!("bench: files apply_100_at_100k {ms:.3} ms");
    assert!(ms <= slack(), "median {ms} ms over 1 ms");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_merge_batch_at_100k() {
    let mut s = big_store();
    let mut times = Vec::new();
    for round in 0..5usize {
        let rows: Vec<(String, u32)> =
            (0..2_048).map(|i| (format!("merge {round} {:05} x{}", (i * 7_919) % 2_048, i), 0x20)).collect();
        let mut b =
            Batch { gen: Gen(1), names: Vec::new(), entries: Vec::new(), done: false, error: None, elapsed_us: 0 };
        for (n, a) in &rows {
            let off = b.names.len() as u32;
            b.names.extend_from_slice(n.as_bytes());
            b.entries.push(RawEntry {
                name_off: off,
                name_len: n.len() as u16,
                attrs: *a,
                reparse_tag: 0,
                size: 1,
                mtime: 0,
                ctime: 0,
            });
        }
        s.append(b);
        times.push(ms_of(&mut || {
            let _ = s.merge_pending();
        }));
    }
    let ms = median(times);
    println!("bench: files merge_batch_at_100k {ms:.3} ms");
    assert!(ms <= 2.0 * slack(), "median {ms} ms over 2 ms");
}
