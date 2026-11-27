//! The window's benches (the bench job runs them in release, serially): the first frame, a full 1080p repaint of a
//! 100,000-row list, the cell formatters and one key move. Everything is in memory over the fakes: no window and no
//! file is touched. The numbers are the rows of the budget table.

use std::rc::Rc;
use std::time::Instant;

use files_engine::fakes::{self, Fakes};
use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{EntryStore, WidePath};
use files_engine::model::{ColKind, FilesTable, ViewOpts};
use files_engine::types::TypeNames;
use files_engine::window::FilesScreen;
use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::model::{CellOut, ColId, TableModel};
use oled_ui::{SizeL, Tokens, UiLang};

const DIR: &str = r"C:\made-up\bench";

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0: the shared CI runner is slower.
fn slack() -> f64 {
    let v = std::env::var("OLED_BENCH_SLACK").ok();
    v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn p99(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[(v.len() * 99 / 100).min(v.len() - 1)]
}

fn ms_of(f: &mut dyn FnMut()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

fn us_of(f: &mut dyn FnMut()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1_000_000.0
}

/// A window over `n` made-up entries in a Harness of `size` at `dpi`, its first batches delivered and drawn.
fn window(fakes: &Fakes, n: u32, size: SizeL, dpi: u32) -> Harness<FilesScreen> {
    let path = WidePath::parse(DIR).unwrap();
    fakes.enumr.queue(&path, fakes::listing_n(n, 7));
    let shared = fakes.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(shared, 1, Location::Fs(path), None, tab);
    let mut h = Harness::new(screen, size, dpi, Tokens::oled());
    h.render();
    fakes.pump(&mut h, 1);
    h.render();
    h
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_ui_first_frame() {
    let ms = median(
        (0..20)
            .map(|_| {
                let fakes = Fakes::new();
                ms_of(&mut || {
                    let mut h = window(&fakes, 256, SizeL { w: 1100, h: 720 }, 144);
                    h.render();
                })
            })
            .collect(),
    );
    println!("bench: files ui_first_frame {ms:.2} ms");
    assert!(ms <= 30.0 * slack(), "median {ms} ms over 30 ms");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_frame_1080p() {
    let fakes = Fakes::new();
    let mut h = window(&fakes, 100_000, SizeL { w: 1920, h: 1080 }, 96);
    // To the middle of the list by pages (the focus drags the list along).
    h.key("Home");
    loop {
        h.render();
        let at = h.screen().tabs().active().table.visible_rows().start;
        if at + 80 >= 50_000 {
            break;
        }
        for _ in 0..((50_000 - at) / 60).max(1) {
            h.key("PageDown");
        }
    }
    let ms = median((0..20).map(|_| ms_of(&mut || drop(h.render()))).collect());
    println!("bench: files frame_1080p {ms:.2} ms");
    assert!(ms <= 8.0 * slack(), "median {ms} ms over 8 ms");
}

fn table_of_40() -> FilesTable {
    let mut store = EntryStore::new(WidePath::parse(DIR).unwrap(), files_engine::listing::Gen(0));
    store.append(fakes::listing_n(40, 11));
    store.merge_pending();
    let now = LocalTime { year: 2026, month: 11, day: 21, hour: 14, minute: 40 };
    FilesTable::new(Box::new(store), Rc::new(Fmt::fixed_en()), Rc::new(TypeNames::empty()), ViewOpts::default(), now)
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_cells_40x4() {
    let mut t = table_of_40();
    let cols = [ColKind::Name, ColKind::Modified, ColKind::Type, ColKind::Size].map(|k| ColId(k as u16));
    let mut out = CellOut::default();
    let mut run = |t: &mut FilesTable| {
        for row in 0..40 {
            for &c in &cols {
                t.cell(row, c, &mut out);
            }
        }
    };
    for _ in 0..20 {
        run(&mut t);
    }
    let us = p99((0..200).map(|_| us_of(&mut || run(&mut t))).collect());
    println!("bench: files cells_40x4 {us:.1} us");
    assert!(us <= 500.0 * slack(), "p99 {us} us over 500 us");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_key_move() {
    let fakes = Fakes::new();
    let mut h = window(&fakes, 100_000, SizeL { w: 1100, h: 720 }, 96);
    h.key("Home");
    let us = p99((0..500)
        .map(|_| {
            us_of(&mut || {
                h.key("Down");
            })
        })
        .collect());
    println!("bench: files key_move {us:.1} us");
    assert!(us <= 1_000.0 * slack(), "p99 {us} us over 1,000 us");
}
