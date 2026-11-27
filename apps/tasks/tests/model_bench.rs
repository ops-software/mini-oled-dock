//! Model tick bench: the whole pure-model work of one tasks tick at 300 processes and 40 apps. Release only
//! (ignored: run with cargo test --release -p tasks --test model_bench -- --ignored --nocapture).
use oled_sysmon::{ProcKey, ProcRow, ProcTable};
use std::time::Instant;
use tasks::model::classify::{ImagePath, InfoBook, PathMap};
use tasks::model::group::{group, group_stats, AppKey, AppWindow, GroupInput, Grouping, IdentityId, Interner};
use tasks::model::order::{reorder, OrderState, SortSpec};
use tasks::model::rates::RateBook;
use tasks::model::verdict::{judge, GroupStat, SysState, VerdictState};
use tasks::model::view::{ColumnId, Section};

const PROCS: usize = 300;
const APPS: usize = 40;
const ITERATIONS: usize = 2_000;
const LIMIT_US: f64 = 400.0;
const ROOT: &str = "C:\\Windows";

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0.
fn slack_of(v: Option<&str>) -> f64 {
    v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
}

fn slack() -> f64 {
    slack_of(std::env::var("OLED_BENCH_SLACK").ok().as_deref())
}

fn key(i: usize) -> ProcKey {
    ProcKey { pid: 100 + 4 * i as u32, created: 1_000 + i as u64 }
}

/// One tick: the first APPS processes own a window each and the rest are their children; `tick` moves the counters.
fn table(tick: u64) -> ProcTable {
    let mut t = ProcTable::new();
    for i in 0..PROCS {
        let name: Vec<u16> = format!("process{:03}.exe", i % 120).encode_utf16().collect();
        let image = t.names.intern(&name);
        let parent_pid = if i < APPS { 0 } else { key(i % APPS).pid };
        let jitter = (i as u64 * 7 + tick * 13) % 17;
        t.push(ProcRow {
            key: key(i),
            parent_pid,
            session: if i % 25 == 24 { 0 } else { 1 },
            image,
            cpu_100ns: 10_000_000 * (i as u64 + 1) + tick * (1_000_000 + 40_000 * jitter),
            ws: (20 + i as u64) << 20,
            ws_peak: (30 + i as u64) << 20,
            ws_private: (10 + i as u64) << 20,
            private_bytes: (12 + i as u64) << 20,
            io_read: 4_096 * (i as u64 + tick * jitter),
            io_write: 2_048 * (i as u64 + tick * jitter),
            io_other: 512 * (i as u64 + tick),
            threads: 4 + (i % 9) as u32,
            handles: 100 + i as u32,
            base_prio: 8,
            ..ProcRow::default()
        });
    }
    t.finish();
    t
}

fn paths() -> PathMap {
    let mut entries: Vec<(ProcKey, ImagePath)> = (0..PROCS)
        .map(|i| {
            let p = match i % 10 {
                0 => ImagePath::NoImage,
                1 => ImagePath::Unknown,
                2 | 3 => ImagePath::Path(format!("c:\\windows\\system32\\process{:03}.exe", i % 120).into()),
                _ => ImagePath::Path(format!("c:\\program files\\app{:02}\\process{:03}.exe", i % 30, i % 120).into()),
            };
            (key(i), p)
        })
        .collect();
    entries.sort_by_key(|e| e.0);
    PathMap { entries }
}

/// Row ids and sort keys of the groups of one section.
fn section_rows(stats: &[GroupStat], section: Section, rows: &mut Vec<u32>, keys: &mut Vec<f32>) {
    rows.clear();
    keys.clear();
    for (i, g) in stats.iter().enumerate().filter(|(_, g)| g.section == section) {
        rows.push(i as u32);
        keys.push(g.cpu);
    }
}

#[test]
#[ignore]
fn tasks_model_tick() {
    let (prev, now) = (table(0), table(1));
    let paths = paths();
    let windows: Vec<AppWindow> =
        (0..APPS).map(|i| AppWindow { owner: key(i), identity: IdentityId(i as u32), hung: false }).collect();
    let hosts = [key(APPS + 1)];
    let sys = SysState { cpu_pct: 12.0, mem_load_pct: 55, commit_pct: 40.0, logical_cpus: 8, ..SysState::default() };
    let mut names = Interner::new();
    let mut book = InfoBook::new();
    let mut rates = RateBook::new();
    let mut grouping = Grouping::default();
    let mut stats: Vec<GroupStat> = Vec::new();
    let mut orders = [OrderState::default(), OrderState::default(), OrderState::default()];
    let mut verdict = VerdictState::new(0);
    let spec = SortSpec::for_column(ColumnId::Cpu, true);
    let (mut rows, mut keys) = (Vec::new(), Vec::new());
    let mut samples: Vec<f64> = Vec::with_capacity(ITERATIONS);
    let mut apps_seen = 0;
    for it in 0..ITERATIONS {
        let now_ms = 1_000 + 1_000 * it as u64;
        let t = Instant::now();
        let info = book.update(&now, &paths, ROOT, &mut names);
        rates.update(&prev, &now, 1_000, sys.logical_cpus);
        group(&GroupInput { procs: &now, info, windows: &windows, session: 1, hosts: &hosts }, &mut grouping);
        group_stats(&grouping, &now, &rates, &mut stats);
        for (state, section) in orders.iter_mut().zip([Section::Apps, Section::Background, Section::Windows]) {
            section_rows(&stats, section, &mut rows, &mut keys);
            reorder(&rows, &keys, spec, false, state, now_ms);
        }
        let v = judge(&sys, &stats, &mut verdict, now_ms);
        samples.push(t.elapsed().as_secs_f64() * 1e6);
        std::hint::black_box(v);
        apps_seen = grouping.groups.iter().filter(|g| matches!(g.key, AppKey::App(_))).count();
    }
    assert_eq!(apps_seen, APPS, "every window owner is one app group");
    samples.sort_by(|a, b| a.total_cmp(b));
    let p50 = samples[ITERATIONS / 2];
    let p95 = samples[ITERATIONS * 95 / 100];
    let limit = LIMIT_US * slack();
    println!("bench: tasks_model_tick p50 {p50:.0} us p95 {p95:.0} us limit {limit:.0} us");
    if !cfg!(debug_assertions) {
        assert!(p95 <= limit, "model tick p95 {p95:.0} us over the {limit:.0} us limit");
    }
}

#[test]
fn slack_parses_in_range() {
    assert_eq!(slack_of(None), 1.0);
    assert_eq!(slack_of(Some("2")), 2.0);
    assert_eq!(slack_of(Some("9")), 1.0);
    assert_eq!(slack_of(Some("x")), 1.0);
}

#[test]
fn fixture_has_the_planned_shape() {
    let t = table(1);
    assert_eq!(t.len(), PROCS);
    assert_eq!(paths().entries.len(), PROCS);
    assert!(t.rows.iter().all(|r| r.key.pid != 0));
}
