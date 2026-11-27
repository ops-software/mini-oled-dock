//! The search bench: one filter pass over 1,000 rows with a 3-term query (SPEC-tasks 5.2.8). Release only: ignored; run
//! with cargo test --release -p tasks --test search_bench -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;

use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::ui::scene::{self, Scene};
use tasks::ui::search::{self, Matcher};

const PROCESSES: u32 = 1_000;
const ITERATIONS: u32 = 400;
/// The budget at slack 1, microseconds (P3-tasks-app section 4).
const LIMIT_US: f64 = 2_000.0;

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0 (the engine's helper, a local copy).
fn slack_of(v: Option<&str>) -> f64 {
    v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
}

fn slack() -> f64 {
    slack_of(std::env::var("OLED_BENCH_SLACK").ok().as_deref())
}

#[test]
fn slack_parses_inside_its_range_only() {
    assert_eq!(slack_of(None), 1.0);
    assert_eq!(slack_of(Some("2")), 2.0);
    assert_eq!(slack_of(Some("9")), 1.0);
    assert_eq!(slack_of(Some("x")), 1.0);
}

fn scene() -> Scene {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t1_list_calm.json");
    let mut s = Scene::load(&path).expect("the calm scene");
    let proto = s.procs[5].clone();
    s.procs = (0..PROCESSES)
        .map(|i| {
            let mut p = proto.clone();
            p.pid = 1000 + i * 4;
            p.created += u64::from(i);
            p.image = format!("fixture{i}.exe");
            p.path = Some(format!("C:\\Fixture\\app{i}\\fixture{i}.exe"));
            p
        })
        .collect();
    s.size = [640, 560];
    s.ticks = 1;
    s
}

#[test]
#[ignore]
fn search_filter_pass() {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(&scene(), PreviewLang::En, 96, launcher, exec).expect("the scene");
    let whole = h.screen().table().model().rows.clone();
    assert_eq!(whole.len(), PROCESSES as usize);
    let (v, names) = (h.screen().view(), &h.screen().parts().names);
    // Three terms: a name part, the extension and a PID; together they keep one row of a thousand.
    let mut m = Matcher::default();
    m.set_query("fixture7 exe 1028");
    search::prepare(v, names, &mut m);
    let mut times = Vec::with_capacity(ITERATIONS as usize);
    let mut kept = 0;
    for _ in 0..ITERATIONS {
        let mut rows = whole.clone();
        let t = Instant::now();
        search::filter(v, &mut m, &mut rows);
        times.push(t.elapsed().as_nanos() as f64 / 1_000.0);
        kept = rows.len();
    }
    assert!(kept > 0 && kept < whole.len(), "the query keeps some rows: {kept}");
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = times[times.len() / 2];
    let p95 = times[times.len() * 95 / 100];
    println!("bench: tasks_search_key p50 {p50:.0} us p95 {p95:.0} us ({kept} of {} rows kept)", whole.len());
    let limit = LIMIT_US * slack();
    assert!(p95 <= limit, "one filter pass p95 {p95:.0} us over {limit:.0} us");
}
