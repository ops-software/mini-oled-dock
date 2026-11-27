//! The UI tick bench: 300 processes at 640 x 560 and 150 %, 1,000 ticks of the data path (merge, list, order, publish).
//! The budget is the data path's. The Harness renders a whole window (the host paints the damaged rows only), so the
//! render is printed for information and not held to the budget. Release only: ignored; run with
//! cargo test --release -p tasks --test ui_bench -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;

use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::InjectFrame;

const PROCESSES: u32 = 300;
const ITERATIONS: u32 = 1_000;
const DPI: u32 = 144;
/// The budget at slack 1, microseconds (P3-tasks-app section 4).
const LIMIT_US: f64 = 1_400.0;

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

/// 300 processes whose CPU and I/O wander, so every tick changes a share of the rows.
fn scene() -> Scene {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t1_list_calm.json");
    let mut s = Scene::load(&path).expect("the calm scene");
    let proto = s.procs[5].clone();
    s.procs = (0..PROCESSES)
        .map(|i| {
            let mut p = proto.clone();
            p.pid = 1000 + i * 4;
            p.created += u64::from(i);
            let base = 0.02 + f32::from((i % 17) as u8) * 0.05;
            p.cpu = (0..=ITERATIONS + 2).map(|t| base * (1.0 + 0.6 * ((t + i) as f32 * 0.7).sin())).collect();
            p.io = (0..=ITERATIONS + 2).map(|t| 40_000.0 * (1.0 + ((t * 3 + i) as f32 * 0.3).cos())).collect();
            p
        })
        .collect();
    s.size = [640, 560];
    s.ticks = ITERATIONS + 2;
    s
}

#[test]
#[ignore]
fn tasks_ui_tick() {
    let s = scene();
    let frames = s.frames();
    let mut start = s.clone();
    start.ticks = 0;
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let mut h = scene::open(&start, PreviewLang::En, DPI, launcher, exec).expect("the scene opens");
    let mut frames = frames.into_iter();
    // Two frames first: the rates are primed and the glyphs, fonts and digit atlases are warm.
    for f in frames.by_ref().take(3) {
        h.send(Box::new(InjectFrame(f)));
    }
    let _ = h.render();
    let (mut data_us, mut render_us) = (Vec::new(), Vec::new());
    for f in frames.take(ITERATIONS as usize) {
        let t = Instant::now();
        h.send(Box::new(InjectFrame(f)));
        let mid = Instant::now();
        let img = h.render();
        let end = Instant::now();
        data_us.push((mid - t).as_secs_f64() * 1e6);
        render_us.push((end - mid).as_secs_f64() * 1e6);
        assert!(img.w > 0);
    }
    data_us.sort_by(f64::total_cmp);
    render_us.sort_by(f64::total_cmp);
    let pct = |v: &[f64], p: usize| v[(v.len() * p / 100).min(v.len() - 1)];
    println!(
        "bench: tasks_ui_render_full_window p50 {:.0} us p95 {:.0} us (information)",
        pct(&render_us, 50),
        pct(&render_us, 95)
    );
    let (p50, p95) = (pct(&data_us, 50), pct(&data_us, 95));
    println!("bench: tasks_ui_tick p50 {p50:.0} us p95 {p95:.0} us");
    let limit = LIMIT_US * slack();
    assert!(p95 <= limit, "tasks_ui_tick p95 {p95:.0} us over the budget {limit:.0} us");
}
