//! Parse bench: 300 entries of 14 threads, 2,000 parses into a reused table. Release only (ignored: run with
//! cargo test --release -p oled-sysmon --test parse_bench -- --ignored --nocapture).
use oled_sysmon::snapshot::{self, ENTRY_SIZE, THREAD_SIZE};
use oled_sysmon::ProcTable;
use std::time::Instant;

const BASE: usize = 0x0000_7000_0000_0000;
const ENTRIES: usize = 300;
const THREADS: usize = 14;
const PARSES: usize = 2_000;
const LIMIT_US: f64 = 600.0;

fn put(b: &mut [u8], o: usize, v: &[u8]) {
    b[o..o + v.len()].copy_from_slice(v);
}

/// ENTRIES entries with THREADS threads each and a 16-unit image name inside each entry's span.
fn build() -> Vec<u8> {
    let tail = ENTRY_SIZE + THREAD_SIZE * THREADS;
    let size = (tail + 32).div_ceil(8) * 8;
    let mut buf = vec![0u8; size * ENTRIES];
    for i in 0..ENTRIES {
        let off = i * size;
        let next = if i + 1 == ENTRIES { 0 } else { size as u32 };
        put(&mut buf, off, &next.to_le_bytes());
        put(&mut buf, off + 4, &(THREADS as u32).to_le_bytes());
        put(&mut buf, off + 32, &(1_000 + i as i64).to_le_bytes());
        put(&mut buf, off + 40, &123i64.to_le_bytes());
        put(&mut buf, off + 48, &456i64.to_le_bytes());
        put(&mut buf, off + 56, &32u16.to_le_bytes());
        put(&mut buf, off + 64, &((BASE + off + tail) as u64).to_le_bytes());
        put(&mut buf, off + 80, &(4 + 4 * i as u64).to_le_bytes());
        put(&mut buf, off + 88, &4u64.to_le_bytes());
        put(&mut buf, off + 144, &(10_000_000u64 + i as u64).to_le_bytes());
        for (k, c) in format!("process{:03}.exe---", i % 120).encode_utf16().take(16).enumerate() {
            put(&mut buf, off + tail + k * 2, &c.to_le_bytes());
        }
    }
    buf
}

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0.
fn slack() -> f64 {
    std::env::var("OLED_BENCH_SLACK")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| (1.0..=4.0).contains(s))
        .unwrap_or(1.0)
}

#[test]
#[ignore]
fn sysmon_parse_300() {
    let buf = build();
    let mut table = ProcTable::new();
    let mut samples: Vec<f64> = Vec::with_capacity(PARSES);
    for _ in 0..PARSES {
        let t = Instant::now();
        let stats = snapshot::parse(&buf, BASE, &mut table).expect("the built buffer parses");
        samples.push(t.elapsed().as_secs_f64() * 1e6);
        assert_eq!(stats.entries as usize, ENTRIES);
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let p50 = samples[PARSES / 2];
    let p95 = samples[PARSES * 95 / 100];
    let limit = LIMIT_US * slack();
    println!("bench: sysmon_parse_300 p50 {p50:.0} us p95 {p95:.0} us limit {limit:.0} us");
    if !cfg!(debug_assertions) {
        assert!(p95 <= limit, "parse p95 {p95:.0} us over the {limit:.0} us limit");
    }
}

#[test]
fn slack_reading() {
    // Without the variable (or with a bad one) the slack is 1.0; the variable itself is never set by this test.
    if std::env::var("OLED_BENCH_SLACK").is_err() {
        assert_eq!(slack(), 1.0);
    }
    assert!(build().len() > ENTRIES * ENTRY_SIZE);
    let mut table = ProcTable::new();
    assert_eq!(snapshot::parse(&build(), BASE, &mut table).unwrap().entries as usize, ENTRIES);
}
