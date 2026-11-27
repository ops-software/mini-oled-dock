//! The window's memory at 100,000 entries (the row of the budget table): a real entry store inside the table model
//! inside the kit's virtual table with 50,000 selected keys, counted with the file's own allocator. The harness's
//! other threads are parked while the one test runs, so the live-byte counter sees only the test's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

use files_engine::fakes;
use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{EntryStore, Gen, WidePath};
use files_engine::model::{FilesTable, ViewOpts};
use files_engine::tab::TABLE_ID;
use files_engine::types::TypeNames;
use files_engine::view::columns::default_columns;
use oled_ui::model::{KeySet, ListModel, Selection, SelectionMode, TableStyle};
use oled_ui::widgets::VirtualTable;
use oled_ui::UiLang;

static LIVE: AtomicIsize = AtomicIsize::new(0);

struct Counting;

// SAFETY: every call is forwarded to System unchanged; the counter is a plain atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Relaxed);
        System.dealloc(p, l);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_add(n as isize - l.size() as isize, Relaxed);
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

const N: u32 = 100_000;

#[test]
fn ui_heap_100k() {
    // The measure starts before the made-up batch: the store takes it over, and what it keeps is what a window holds.
    let base = LIVE.load(Relaxed);
    let batch = fakes::listing_n(N, 7);
    let mut store = EntryStore::new(WidePath::parse(r"C:\made-up\heap").unwrap(), Gen(0));
    store.append(batch);
    store.merge_pending();
    let after_store = LIVE.load(Relaxed) - base;
    let store_parts = store.heap_parts();
    let now = LocalTime { year: 2026, month: 11, day: 21, hour: 14, minute: 40 };
    let model = FilesTable::new(
        Box::new(store),
        Rc::new(Fmt::fixed_en()),
        Rc::new(TypeNames::empty()),
        ViewOpts::default(),
        now,
    );
    let table =
        VirtualTable::new(TABLE_ID, model, default_columns(UiLang::EN), SelectionMode::Multi, TableStyle::FILES);
    let after_table = LIVE.load(Relaxed) - base;
    assert_eq!(ListModel::len(table.model()), N as usize, "every entry is listed");
    let mut keys = KeySet::new();
    for row in (0..N as usize).step_by(2) {
        keys.insert(ListModel::key(table.model(), row));
    }
    assert_eq!(keys.len(), 50_000);
    let key_bytes = keys.heap_bytes();
    let selection = Selection::Keys(keys);
    let total = LIVE.load(Relaxed) - base;
    println!("heap_parts: {store_parts:?}");
    println!(
        "ui_heap: store {after_store} bytes, table {} bytes, selection {key_bytes} bytes",
        after_table - after_store
    );
    println!("bench: files ui_heap_100k_bytes {total} bytes");
    drop(selection);
    assert!(total <= 10_485_760, "{total} bytes over the row of 10,485,760");
}
