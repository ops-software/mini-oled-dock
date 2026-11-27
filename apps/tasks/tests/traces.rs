//! Verdict traces: the text format, the replay and the synthetic scenarios.
use tasks::engine::trace::{
    flaps, parse_trace, replay, token_key, write_trace, Tick, Trace, TraceError, TraceGroup, TraceHeader,
};
use tasks::model::group::{AppKey, IdentityId};
use tasks::model::verdict::{SysState, Verdict};
use tasks::model::view::Section;

fn header(cpus: u32) -> TraceHeader {
    TraceHeader {
        version: 1,
        tick_ms: 1000,
        logical_cpus: cpus,
        total_phys: 17_179_869_184,
        build: 26300,
        note: "unit".into(),
    }
}

fn sys(cpus: u32) -> SysState {
    SysState {
        cpu_pct: 12.5,
        mem_load_pct: 41,
        commit_pct: 50.25,
        dpc_irq_pct: 0.5,
        logical_cpus: cpus,
        paused: false,
        read_error: None,
    }
}

fn group(token: &str, section: Section, cpu: f32) -> TraceGroup {
    TraceGroup { token: token.into(), section, cpu, mem: 123_456_789, io_bps: 2048.5, hung: false, has_windows: true }
}

fn sample() -> Trace {
    let mut paused = sys(8);
    paused.paused = true;
    paused.read_error = Some(-1073741820);
    Trace {
        header: header(8),
        ticks: vec![
            Tick {
                t_ms: 0,
                sys: sys(8),
                groups: vec![group("a01", Section::Apps, 3.125), group("b12", Section::Background, 0.0)],
            },
            Tick { t_ms: 1000, sys: paused, groups: vec![group("w9999", Section::Windows, 100.0)] },
            Tick { t_ms: 2000, sys: sys(8), groups: vec![] },
        ],
    }
}

#[test]
fn round_trip() {
    let t = sample();
    let mut text = String::new();
    write_trace(&t, &mut text);
    assert!(text.starts_with("tasks-trace 1 tick_ms=1000 cpus=8 phys=17179869184 build=26300 note=unit\n"));
    assert!(text.contains("\nt 0 cpu=12.5 mem=41 commit=50.25 dpc=0.5 paused=0 err=-\n"));
    assert!(text.contains("\ng a01 A 3.125 123456789 2048.5 0 1\n"));
    assert_eq!(parse_trace(&text), Ok(t.clone()));
    let mut again = String::new();
    write_trace(&parse_trace(&text).unwrap(), &mut again);
    assert_eq!(again, text);
}

#[test]
fn rejects_bad_lines() {
    let head = "tasks-trace 1 tick_ms=1000 cpus=4 phys=100 build=1 note=x\n";
    let tick = "t 0 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=-\n";
    let ok = |body: &str| parse_trace(&format!("{head}{body}"));
    assert!(ok("").is_ok());
    assert!(ok(&format!("{tick}g a01 A 1 2 3 0 1\n")).is_ok());

    for bad in [
        "",
        "tasks-trace 2 tick_ms=1000 cpus=4 phys=100 build=1 note=x\n",
        "tasks-trace 1 tick_ms=x cpus=4 phys=100 build=1 note=x\n",
        "tasks-trace 1 tick_ms=1000 cpus=0 phys=100 build=1 note=x\n",
        "tasks-trace 1 tick_ms=1000 cpus=4 phys=100 build=1 note=X\n",
        "tasks-trace 1 tick_ms=1000 cpus=4 phys=100 build=1 note=x extra\n",
        "tasks-trace 1 cpus=4 tick_ms=1000 phys=100 build=1 note=x\n",
    ] {
        assert_eq!(parse_trace(bad), Err(TraceError::Header), "{bad:?}");
    }

    let field = |body: &str, line: usize, name: &'static str| {
        assert_eq!(ok(body), Err(TraceError::Field(line, name)), "{body:?}");
    };
    field("t x cpu=1 mem=2 commit=3 dpc=0 paused=0 err=-\n", 2, "t");
    field("t 0 cpu=-1 mem=2 commit=3 dpc=0 paused=0 err=-\n", 2, "cpu");
    field("t 0 cpu=nan mem=2 commit=3 dpc=0 paused=0 err=-\n", 2, "cpu");
    field("t 0 cpu=1e3 mem=2 commit=3 dpc=0 paused=0 err=-\n", 2, "cpu");
    field("t 0 cpu=1.0001 mem=2 commit=3 dpc=0 paused=0 err=-\n", 2, "cpu");
    field("t 0 cpu=1 mem=101 commit=3 dpc=0 paused=0 err=-\n", 2, "mem");
    field("t 0 cpu=1 mem=2 commit=x dpc=0 paused=0 err=-\n", 2, "commit");
    field("t 0 cpu=1 mem=2 commit=3 dpc= paused=0 err=-\n", 2, "dpc");
    field("t 0 cpu=1 mem=2 commit=3 dpc=0 paused=2 err=-\n", 2, "paused");
    field("t 0 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=zz\n", 2, "err");
    field("t 0 cpu=1 mem=2 commit=3 dpc=0 paused=0\n", 2, "err");
    assert!(ok(&format!("{tick}t 0 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=-\n")).is_ok());
    assert_eq!(ok(&format!("t 5 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=-\n{tick}")), Err(TraceError::Field(3, "t")));
    field(&format!("{tick}g a01 X 1 2 3 0 1\n"), 3, "section");
    field(&format!("{tick}g a01 A x 2 3 0 1\n"), 3, "cpu");
    field(&format!("{tick}g a01 A 1 -2 3 0 1\n"), 3, "mem");
    field(&format!("{tick}g a01 A 1 2 x 0 1\n"), 3, "io");
    field(&format!("{tick}g a01 A 1 2 3 2 1\n"), 3, "hung");
    field(&format!("{tick}g a01 A 1 2 3 0\n"), 3, "windows");
    field(&format!("{tick}g a01 A 1 2 3 0 1\ng a01 B 1 2 3 0 1\n"), 4, "token");

    let line = |body: &str, line: usize| assert_eq!(ok(body), Err(TraceError::Line(line)), "{body:?}");
    line("g a01 A 1 2 3 0 1\n", 2);
    line(&format!("{tick}x\n"), 3);
    line(&format!("{tick}\n"), 3);
    line(&format!("{tick}g a01 A 1 2 3 0 1 9\n"), 3);
    line("t 0 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=- extra\n", 2);
}

#[test]
fn tokens_are_never_names() {
    let head =
        "tasks-trace 1 tick_ms=1000 cpus=4 phys=100 build=1 note=x\nt 0 cpu=1 mem=2 commit=3 dpc=0 paused=0 err=-\n";
    for ok in ["a01", "z9999", "q123"] {
        assert!(parse_trace(&format!("{head}g {ok} A 1 2 3 0 1\n")).is_ok(), "{ok}");
    }
    for bad in ["a.exe", "a\\b", "A01", "a1", "a12345", "01a", "chrome", "a0b", "ab12", "a-1", "é01", ""] {
        assert_eq!(parse_trace(&format!("{head}g {bad} A 1 2 3 0 1\n")), Err(TraceError::Field(3, "token")), "{bad:?}");
    }
    for bad in ["C:\\x", "Name", "a b", "x.y", "a/b"] {
        let text = format!("tasks-trace 1 tick_ms=1000 cpus=4 phys=100 build=1 note={bad}\n");
        assert_eq!(parse_trace(&text), Err(TraceError::Header), "{bad:?}");
    }
}

#[test]
fn replay_names_tokens_by_first_appearance() {
    let t = sample();
    assert_eq!(token_key(&t, "a01"), Some(AppKey::App(IdentityId(0))));
    assert_eq!(token_key(&t, "b12"), Some(AppKey::App(IdentityId(1))));
    assert_eq!(token_key(&t, "w9999"), Some(AppKey::App(IdentityId(2))));
    assert_eq!(token_key(&t, "zzz"), None);
    let r = replay(&t);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].1, Verdict::Looking);
    assert_eq!(r[1].1, Verdict::ReadError { status: -1073741820 });
    assert!(replay(&Trace { header: header(4), ticks: vec![] }).is_empty());
}

#[test]
fn flaps_counts_only_returns_within_the_window() {
    let a = AppKey::App(IdentityId(0));
    let b = AppKey::App(IdentityId(1));
    let hog = |top| Verdict::CpuHog { top, pct: 50 };
    let seq = |v: &[(u64, Verdict)]| v.iter().map(|(t, v)| (*t, *v, 0u8)).collect::<Vec<_>>();
    // The named app changes and changes back within the window: one flap; slower: none.
    let quick = seq(&[(0, hog(a)), (1000, hog(b)), (2000, hog(a))]);
    assert_eq!(flaps(&quick, 10_000), 1);
    assert_eq!(flaps(&quick, 500), 0);
    // One change that stays is no flap.
    assert_eq!(flaps(&seq(&[(0, hog(a)), (1000, hog(b)), (2000, hog(b))]), 10_000), 0);
    // Issue to issue and back: a flap. Calm to an issue and back: not.
    let mem = Verdict::MemoryFull { top: a, bytes: 1 };
    let calm = Verdict::Calm { busiest: Some(a), pct: 1 };
    assert_eq!(flaps(&seq(&[(0, hog(a)), (1000, mem), (2000, hog(a))]), 10_000), 1);
    assert_eq!(flaps(&seq(&[(0, calm), (1000, hog(a)), (2000, calm)]), 10_000), 0);
    // Calm's busiest app changing and returning is one.
    let calm_b = Verdict::Calm { busiest: Some(b), pct: 1 };
    assert_eq!(flaps(&seq(&[(0, calm), (1000, calm_b), (2000, calm)]), 10_000), 1);
}

// ---- TR2: synthetic scenarios -------------------------------------------------------------------------------

const MIB: f32 = 1_048_576.0;
const GIB: u64 = 1 << 30;
const TICK_MS: u64 = 1000;

/// A small seeded generator (xorshift64*): the same seed always gives the same trace.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// 0.0..1.0
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn noise_token(i: usize) -> String {
    format!("n{:02}", i + 1)
}

/// Thirty groups of background noise on eight CPUs: each has its own base load (a seed decides which is busiest) and
/// a few percent of jitter, so the machine sits near 25 % and nothing reaches a rule.
fn base_trace(seed: u64, ticks: u64) -> Trace {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut order: Vec<usize> = (0..30).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    let mut header = header(8);
    header.note = "synthetic".into();
    let mut out = Trace { header, ticks: Vec::new() };
    for n in 0..ticks {
        let mut groups = Vec::new();
        let mut total = 0.0f32;
        for (i, rank) in order.iter().enumerate() {
            let base = 0.1 + *rank as f32 * 0.05;
            let cpu = base * (0.95 + 0.1 * rng.unit());
            total += cpu;
            groups.push(TraceGroup {
                token: noise_token(i),
                section: if i < 5 { Section::Apps } else { Section::Background },
                cpu: (cpu * 1000.0).round() / 1000.0,
                mem: (20 + *rank as u64 * 8) << 20,
                io_bps: 1000.0 * rng.unit().round(),
                hung: false,
                has_windows: i < 5,
            });
        }
        let cpu_pct = ((total + 2.0) * 1000.0).round() / 1000.0;
        let sys = SysState {
            cpu_pct,
            mem_load_pct: 45,
            commit_pct: 52.0,
            dpc_irq_pct: 0.5,
            logical_cpus: 8,
            paused: false,
            read_error: None,
        };
        out.ticks.push(Tick { t_ms: n * TICK_MS, sys, groups });
    }
    out
}

fn add(t: &mut Trace, g: TraceGroup) {
    for tick in &mut t.ticks {
        tick.groups.push(g.clone());
    }
}

/// Run f on every tick whose time is in from_s..to_s seconds.
fn during(t: &mut Trace, from_s: u64, to_s: u64, mut f: impl FnMut(&mut Tick)) {
    for tick in &mut t.ticks {
        if (from_s * 1000..to_s * 1000).contains(&tick.t_ms) {
            f(tick);
        }
    }
}

fn special(token: &str, mem: u64) -> TraceGroup {
    TraceGroup {
        token: token.into(),
        section: Section::Apps,
        cpu: 0.2,
        mem,
        io_bps: 0.0,
        hung: false,
        has_windows: true,
    }
}

fn group_mut<'a>(tick: &'a mut Tick, token: &str) -> &'a mut TraceGroup {
    tick.groups.iter_mut().find(|g| g.token == token).expect("scenario group")
}

fn at(v: &[(u64, Verdict, u8)], secs: u64) -> (Verdict, u8) {
    let r = v.iter().find(|r| r.0 == secs * 1000).expect("tick in trace");
    (r.1, r.2)
}

/// The first tick (in seconds) from from_s on whose verdict satisfies pred.
fn first(v: &[(u64, Verdict, u8)], from_s: u64, pred: impl Fn(&Verdict) -> bool) -> Option<u64> {
    v.iter().find(|r| r.0 >= from_s * 1000 && pred(&r.1)).map(|r| r.0 / 1000)
}

fn app(t: &Trace, token: &str) -> AppKey {
    token_key(t, token).expect("token in trace")
}

fn is_calm(v: &Verdict) -> bool {
    matches!(v, Verdict::Calm { .. })
}

/// The scenario survives the text format unchanged.
fn assert_text_round_trip(t: &Trace) {
    let mut text = String::new();
    write_trace(t, &mut text);
    let back = parse_trace(&text).expect("scenario parses");
    assert_eq!(replay(&back), replay(t));
}

#[test]
fn idle_stays_calm() {
    let t = base_trace(1, 600);
    let v = replay(&t);
    assert_eq!(v.len(), 600);
    assert_eq!(at(&v, 0).0, Verdict::Looking);
    assert!(v[1..].iter().all(|r| is_calm(&r.1) && r.2 == 0));
    assert_eq!(flaps(&v, 10_000), 0);
    assert_text_round_trip(&t);
}

#[test]
fn hog_is_named_after_its_dwell_and_leaves_after_five_seconds() {
    let mut t = base_trace(2, 120);
    add(&mut t, special("h01", GIB / 4));
    during(&mut t, 20, 60, |tick| {
        tick.sys.cpu_pct = 88.0;
        group_mut(tick, "h01").cpu = 62.0;
    });
    let v = replay(&t);
    let hog = app(&t, "h01");
    assert!(v[..23].iter().all(|r| is_calm(&r.1) || r.1 == Verdict::Looking), "no verdict before the dwell");
    assert_eq!(at(&v, 23), (Verdict::CpuHog { top: hog, pct: 62 }, 0));
    assert_eq!(first(&v, 20, |v| !is_calm(v)), Some(23));
    // The top app and its percentage are named while the hog runs; while it leaves, the name stays and the number falls.
    assert!(v.iter().filter(|r| (23_000..60_000).contains(&r.0)).all(|r| r.1 == Verdict::CpuHog { top: hog, pct: 62 }));
    assert!(v
        .iter()
        .filter(|r| (60_000..65_000).contains(&r.0))
        .all(|r| matches!(r.1, Verdict::CpuHog { top, .. } if top == hog)));
    // 62 % of eight CPUs is also a core-bound group (rule 6, 10 s dwell): it counts as "1 more" from 30 s to 65 s.
    assert!(v.iter().filter(|r| (23_000..30_000).contains(&r.0)).all(|r| r.2 == 0));
    assert!(v.iter().filter(|r| (30_000..65_000).contains(&r.0)).all(|r| r.2 == 1));
    assert_eq!(first(&v, 60, is_calm), Some(65));
    assert_eq!(flaps(&v, 10_000), 0);
    assert_text_round_trip(&t);
}

#[test]
fn memory_names_the_largest_group() {
    let mut t = base_trace(3, 120);
    add(&mut t, special("m01", 4 * GIB));
    during(&mut t, 30, 50, |tick| tick.sys.mem_load_pct = 92);
    let v = replay(&t);
    let top = app(&t, "m01");
    assert!(v[..33].iter().all(|r| !matches!(r.1, Verdict::MemoryFull { .. })));
    assert_eq!(at(&v, 33), (Verdict::MemoryFull { top, bytes: 4 * GIB }, 0));
    assert_eq!(first(&v, 30, |v| matches!(v, Verdict::MemoryFull { .. })), Some(33));
    assert_eq!(first(&v, 50, is_calm), Some(55));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn hung_app_is_named_on_the_second_call_and_leaves_after_five_seconds() {
    let mut t = base_trace(4, 120);
    add(&mut t, special("a01", GIB / 8));
    during(&mut t, 10, 40, |tick| group_mut(tick, "a01").hung = true);
    let v = replay(&t);
    let hung = app(&t, "a01");
    assert!(v[..11].iter().all(|r| !matches!(r.1, Verdict::NotResponding { .. })));
    assert_eq!(at(&v, 11).0, Verdict::NotResponding { app: hung, secs: 1 });
    assert_eq!(at(&v, 20).0, Verdict::NotResponding { app: hung, secs: 10 });
    assert_eq!(first(&v, 40, is_calm), Some(45));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn drivers_after_five_seconds_of_dpc() {
    let mut t = base_trace(5, 120);
    during(&mut t, 30, 38, |tick| tick.sys.dpc_irq_pct = 12.0);
    let v = replay(&t);
    assert!(v[..35].iter().all(|r| !matches!(r.1, Verdict::Drivers { .. })));
    assert_eq!(at(&v, 35), (Verdict::Drivers { pct: 12 }, 0));
    assert_eq!(first(&v, 38, is_calm), Some(43));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn one_core_bound_group_after_ten_seconds() {
    let mut t = base_trace(6, 120);
    add(&mut t, special("c01", GIB / 8));
    during(&mut t, 20, 35, |tick| {
        // 99 % of one core on eight CPUs.
        group_mut(tick, "c01").cpu = 99.0 / 8.0;
        tick.sys.cpu_pct += 99.0 / 8.0;
    });
    let v = replay(&t);
    let app = app(&t, "c01");
    assert!(v[..30].iter().all(|r| !matches!(r.1, Verdict::CoreBound { .. })));
    assert_eq!(at(&v, 30), (Verdict::CoreBound { app }, 0));
    assert_eq!(first(&v, 35, is_calm), Some(40));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn heavy_io_after_five_seconds() {
    let mut t = base_trace(7, 120);
    add(&mut t, special("i01", GIB / 8));
    during(&mut t, 30, 38, |tick| group_mut(tick, "i01").io_bps = 85.0 * MIB);
    let v = replay(&t);
    let top = app(&t, "i01");
    assert!(v[..35].iter().all(|r| !matches!(r.1, Verdict::IoHeavy { .. })));
    assert_eq!(at(&v, 35), (Verdict::IoHeavy { top, bps: 85 * 1_048_576 }, 0));
    assert_eq!(first(&v, 38, is_calm), Some(43));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn two_issues_show_the_first_and_count_the_other() {
    let mut t = base_trace(8, 120);
    add(&mut t, special("h01", GIB / 4));
    add(&mut t, special("m01", 4 * GIB));
    during(&mut t, 20, 60, |tick| {
        tick.sys.cpu_pct = 88.0;
        group_mut(tick, "h01").cpu = 62.0;
    });
    during(&mut t, 30, 50, |tick| tick.sys.mem_load_pct = 92);
    let v = replay(&t);
    let (hog, big) = (app(&t, "h01"), app(&t, "m01"));
    assert_eq!(at(&v, 23), (Verdict::CpuHog { top: hog, pct: 62 }, 0));
    assert_eq!(at(&v, 32), (Verdict::CpuHog { top: hog, pct: 62 }, 1));
    // Memory outranks the CPU: it takes the line; the hog and its core-bound twin (see the hog scenario) are "2 more".
    assert_eq!(at(&v, 33), (Verdict::MemoryFull { top: big, bytes: 4 * GIB }, 2));
    assert!(v.iter().filter(|r| (33_000..55_000).contains(&r.0)).all(|r| r.2 == 2));
    assert_eq!(at(&v, 55), (Verdict::CpuHog { top: hog, pct: 62 }, 1));
    assert_eq!(first(&v, 60, is_calm), Some(65));
    assert_eq!(flaps(&v, 10_000), 0);
}

#[test]
fn noise_seeds_are_flap_free() {
    for seed in 1..=20u64 {
        let t = base_trace(seed, 600);
        let v = replay(&t);
        assert_eq!(flaps(&v, 10_000), 0, "seed {seed}");
        assert!(v[1..].iter().all(|r| is_calm(&r.1) && r.2 == 0), "seed {seed}: an issue on a quiet machine");
        // The busiest app on the Calm line changes at most once per 3 s.
        let mut last: Option<AppKey> = None;
        let mut changed_at: Option<u64> = None;
        for (ms, verdict, _) in &v {
            let Verdict::Calm { busiest, .. } = verdict else { continue };
            if last.is_some() && *busiest != last {
                if let Some(prev) = changed_at {
                    assert!(ms - prev >= 3000, "seed {seed}: busiest changed twice within 3 s at {ms}");
                }
                changed_at = Some(*ms);
            }
            last = *busiest;
        }
    }
}
