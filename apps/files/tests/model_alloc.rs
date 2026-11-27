//! cell() allocates nothing once warm (P3-files-app F1a.2): the text buffer of the reused CellOut keeps its capacity
//! and no cell builds a temporary string. Its own counting allocator, so this file holds one test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{Batch, EntryStore, Gen, RawEntry, WidePath};
use files_engine::model::{FilesTable, IconKey, ViewOpts};
use files_engine::types::TypeNames;
use oled_ui::model::{CellOut, ColId, TableModel};

/// Counting is on only inside the measured loop of the file's one test; the harness's other thread is parked then.
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every call is forwarded to System unchanged; the counters are plain atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if COUNTING.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if COUNTING.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn cell_allocates_nothing_when_warm() {
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    for i in 0..40u32 {
        let name = if i % 5 == 0 {
            format!("folder-{i:02}")
        } else {
            format!("report-final-v{i}.{}", ["pdf", "txt", "zip"][i as usize % 3])
        };
        let attrs = if i % 5 == 0 {
            0x10
        } else {
            0x20 | if i % 7 == 0 { 0x1 } else { 0 } | if i % 3 == 0 { 0x8_0000 } else { 0 }
        };
        let name_off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off,
            name_len: name.len() as u16,
            attrs,
            reparse_tag: 0,
            size: u64::from(i) * 1000,
            mtime: 134_396_928_000_000_000 - i64::from(i) * 600_000_000,
            ctime: 134_396_928_000_000_000,
        });
    }
    let mut store = EntryStore::new(WidePath::parse(r"C:\Data\Reports").unwrap(), Gen(0));
    store.append(b);
    store.merge_pending();
    let now = LocalTime { year: 2026, month: 11, day: 21, hour: 15, minute: 0 };
    let mut t = FilesTable::new(
        Box::new(store),
        Rc::new(Fmt::fixed_en()),
        Rc::new(TypeNames::empty()),
        ViewOpts::default(),
        now,
    );
    t.set_icon(IconKey::Folder, 1, Arc::from(vec![1u8; 4]));
    t.set_icon(IconKey::Class(b"pdf".to_vec().into()), 1, Arc::from(vec![2u8; 4]));
    // Name, Type, Attributes, Status. The date and size formatters still allocate inside (scratch Strings), so they join later.
    let cols = [ColId(0), ColId(2), ColId(5), ColId(6)];
    let mut out = CellOut::default();
    for row in 0..40 {
        for &c in &cols {
            t.cell(row, c, &mut out);
        }
    }
    ALLOCS.store(0, Relaxed);
    COUNTING.store(true, Relaxed);
    for _ in 0..(1000 / (40 * cols.len()) + 1) {
        for row in 0..40 {
            for &c in &cols {
                t.cell(row, c, &mut out);
            }
        }
    }
    COUNTING.store(false, Relaxed);
    let n = ALLOCS.load(Relaxed);
    assert_eq!(n, 0, "a warm cell() allocated {n} times");
}
