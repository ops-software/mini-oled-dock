//! Records one verdict trace of a live scenario: the sampler host, the model's grouping and the trace format of
//! engine::trace. The file holds tokens only (no name, path or process id).
//!
//! trace_record --scenario idle|busy|alloc|hang|tree --secs N --out <file>
//!
//! The scenario's testee (the testee example, next to this exe) is started by the recorder as a hidden child and
//! stopped at the end. The recorder refuses to overwrite a file.
use oled_sysmon::percore::CoreTimes;
use oled_sysmon::{ProcKey, ProcTable};
use std::collections::HashMap;
use tasks::engine::frame::{sys_state, Frame};
use tasks::engine::trace::{Tick, TraceGroup};
use tasks::model::classify::InfoBook;
use tasks::model::group::{group, group_stats, AppKey, AppWindow, Group, GroupInput, Grouping, IdentityId, Interner};
use tasks::model::rates::RateBook;
use tasks::model::tree::ancestors;
use tasks::model::verdict::GroupStat;

const MAX_SECS: u64 = 3600;
const ALLOC_MIN_AVAIL: u64 = 4 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Idle,
    Busy,
    Alloc,
    Hang,
    Tree,
}

impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Scenario::Idle => "idle",
            Scenario::Busy => "busy",
            Scenario::Alloc => "alloc",
            Scenario::Hang => "hang",
            Scenario::Tree => "tree",
        }
    }

    /// (seconds after the start, testee arguments); None for idle.
    fn testee(self, total_secs: u64) -> Option<(u64, Vec<String>)> {
        let (at, mode): (u64, Vec<&str>) = match self {
            Scenario::Idle => return None,
            Scenario::Busy => (20, vec!["--busy", "1"]),
            Scenario::Alloc => (20, vec!["--alloc", "512"]),
            Scenario::Hang => (10, vec!["--hang-after", "5000"]),
            Scenario::Tree => (10, vec!["--tree", "3"]),
        };
        let secs = match self {
            Scenario::Busy => 60,
            _ => total_secs.saturating_sub(at).max(1),
        };
        let mut args: Vec<String> = mode.into_iter().map(String::from).collect();
        args.extend(["--secs".to_string(), secs.min(300).to_string()]);
        Some((at, args))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Args {
    scenario: Scenario,
    secs: u64,
    out: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArgError {
    Missing(&'static str),
    Bad(String),
}

fn parse_args(args: &[String]) -> Result<Args, ArgError> {
    let (mut scenario, mut secs, mut out) = (None, None, None);
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or_else(|| ArgError::Bad(args[i].clone()))?;
        match args[i].as_str() {
            "--scenario" => {
                scenario = Some(match value.as_str() {
                    "idle" => Scenario::Idle,
                    "busy" => Scenario::Busy,
                    "alloc" => Scenario::Alloc,
                    "hang" => Scenario::Hang,
                    "tree" => Scenario::Tree,
                    _ => return Err(ArgError::Bad(value.clone())),
                })
            }
            "--secs" => {
                secs = Some(
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|s| (1..=MAX_SECS).contains(s))
                        .ok_or_else(|| ArgError::Bad(value.clone()))?,
                )
            }
            "--out" => out = Some(value.clone()),
            other => return Err(ArgError::Bad(other.into())),
        }
        i += 2;
    }
    Ok(Args {
        scenario: scenario.ok_or(ArgError::Missing("--scenario"))?,
        secs: secs.ok_or(ArgError::Missing("--secs"))?,
        out: out.ok_or(ArgError::Missing("--out"))?,
    })
}

/// The recorder and every ancestor up to the tester's shell and terminal: none of them absorbs a windowless child,
/// so the testee is never swallowed by the terminal's app group. Empty when the pid is not in the table.
fn hosts_of(procs: &ProcTable, own_pid: u32, out: &mut Vec<ProcKey>) {
    out.clear();
    if let Some(row) = procs.find_pid(own_pid) {
        ancestors(procs, row.key, out);
    }
}

/// Opaque group tokens: t00 is the group that holds the testee, the others are a01, a02... by first appearance of
/// their AppKey, and a group keeps its token across ticks.
#[derive(Debug, Default)]
struct Tokens {
    seen: HashMap<AppKey, u32>,
    next: u32,
}

impl Tokens {
    fn assign(&mut self, groups: &[Group], testee: Option<ProcKey>, out: &mut Vec<String>) {
        out.clear();
        for g in groups {
            let has_testee = testee.is_some_and(|t| g.members.contains(&t));
            let n = if has_testee {
                self.seen.insert(g.key, 0);
                0
            } else {
                match self.seen.get(&g.key) {
                    Some(&n) if n != 0 => n,
                    _ => {
                        self.next = (self.next + 1).min(9999);
                        self.seen.insert(g.key, self.next);
                        self.next
                    }
                }
            };
            out.push(if n == 0 { "t00".to_string() } else { format!("a{n:02}") });
        }
    }
}

/// The model's side of the recorder: one call per taken frame.
#[derive(Default)]
struct Pipeline {
    info: InfoBook,
    names: Interner,
    rates: RateBook,
    prev: ProcTable,
    prev_cores: Vec<CoreTimes>,
    grouping: Grouping,
    stats: Vec<GroupStat>,
    hosts: Vec<ProcKey>,
    windows: Vec<AppWindow>,
    tokens: Tokens,
    token_list: Vec<String>,
    first_us: Option<u64>,
}

impl Pipeline {
    fn tick(&mut self, f: &Frame, own_pid: u32, testee_pid: Option<u32>, system_root: &str) -> Tick {
        let first = *self.first_us.get_or_insert(f.t_us);
        let info = self.info.update(&f.procs, &f.paths, system_root, &mut self.names).to_vec();
        self.windows.clear();
        for w in &f.windows {
            let Some(i) = f.procs.index_of(w.owner) else { continue };
            // The identity of a window is its owner's interned lower-cased exe path (image name without one).
            let id = info.get(i).map_or(0, |p| p.exe.map_or(p.image.0, |e| e.0));
            self.windows.push(AppWindow { owner: w.owner, identity: IdentityId(id), hung: w.hung });
        }
        hosts_of(&f.procs, own_pid, &mut self.hosts);
        let session = f.procs.find_pid(own_pid).map_or(0, |r| r.session);
        group(
            &GroupInput { procs: &f.procs, info: &info, windows: &self.windows, session, hosts: &self.hosts },
            &mut self.grouping,
        );
        if !self.prev.is_empty() {
            self.rates.update(&self.prev, &f.procs, f.dt_ms, f.sys.logical_cpus);
        }
        group_stats(&self.grouping, &f.procs, &self.rates, &mut self.stats);
        let testee = testee_pid.and_then(|p| f.procs.find_pid(p)).map(|r| r.key);
        self.tokens.assign(&self.grouping.groups, testee, &mut self.token_list);
        let groups = self
            .stats
            .iter()
            .zip(&self.token_list)
            .map(|(s, token)| TraceGroup {
                token: token.clone(),
                section: s.section,
                cpu: s.cpu,
                mem: s.mem,
                io_bps: s.io_bps,
                hung: s.hung,
                has_windows: s.has_windows,
            })
            .collect();
        let sys = sys_state(f, &self.prev_cores, false);
        self.prev_cores.clone_from(&f.sys.cores);
        self.prev.copy_from(&f.procs);
        Tick { t_ms: f.t_us.saturating_sub(first) / 1000, sys, groups }
    }
}

#[cfg(windows)]
mod live {
    use super::*;
    use oled_sysmon::mem::memory;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tasks::engine::cadence::{Control, Refresh};
    use tasks::engine::host::{default_samplers, FrameSink, SamplerHost};
    use tasks::engine::need::{Need, NeedSet};
    use tasks::engine::trace::{write_trace, Trace, TraceHeader};
    use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    use windows::Win32::System::Threading::GetCurrentProcessId;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[repr(C)]
    struct OsVersion {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(v: *mut OsVersion) -> i32;
    }

    fn os_build() -> u32 {
        let mut v = OsVersion {
            size: std::mem::size_of::<OsVersion>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd: [0; 128],
        };
        // SAFETY: v is a correctly sized, writable OSVERSIONINFOW.
        let status = unsafe { RtlGetVersion(&mut v) };
        if status == 0 {
            v.build
        } else {
            0
        }
    }

    fn system_root() -> String {
        let mut buf = [0u16; 260];
        // SAFETY: the buffer is writable and its length is passed.
        let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
        String::from_utf16_lossy(buf.get(..n.min(buf.len())).unwrap_or(&[]))
    }

    struct Wake(Mutex<Sender<()>>);

    impl FrameSink for Wake {
        fn ready(&self) {
            if let Ok(tx) = self.0.lock() {
                let _ = tx.send(());
            }
        }
    }

    fn spawn_testee(args: &[String]) -> std::io::Result<Child> {
        let exe = std::env::current_exe()?.with_file_name("testee.exe");
        Command::new(exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    fn header(build: u32, note: &str, cpus: u32, phys: u64) -> TraceHeader {
        TraceHeader { version: 1, tick_ms: 1000, logical_cpus: cpus.max(1), total_phys: phys, build, note: note.into() }
    }

    fn save(path: &str, trace: &Trace) -> Result<(), String> {
        use std::io::Write;
        let mut text = String::new();
        write_trace(trace, &mut text);
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("{path}: {e}"))?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn record(args: &Args) -> Result<(), String> {
        if std::path::Path::new(&args.out).exists() {
            return Err(format!("{} exists: not overwritten", args.out));
        }
        let build = os_build();
        let mem = memory();
        if args.scenario == Scenario::Alloc && mem.is_none_or(|m| m.avail_phys < ALLOC_MIN_AVAIL) {
            let trace =
                Trace { header: header(build, "alloc-skipped", 1, mem.map_or(0, |m| m.total_phys)), ticks: Vec::new() };
            return save(&args.out, &trace);
        }
        let (tx, rx) = channel();
        let need = NeedSet::EMPTY.with(Need::Procs).with(Need::Windows).with(Need::Hung).with(Need::PerCore);
        let control = Control {
            visible: true,
            foreground: true,
            minimised_or_cloaked: false,
            display_off: false,
            refresh: Refresh::One,
            idle: false,
            need,
        };
        let host = SamplerHost::start(Box::new(Wake(Mutex::new(tx))), default_samplers(), control)
            .map_err(|e| format!("sampler: {e:?}"))?;
        let root = system_root();
        // SAFETY: no arguments.
        let own = unsafe { GetCurrentProcessId() };
        let plan = args.scenario.testee(args.secs);
        let mut child: Option<Child> = None;
        let mut pipeline = Pipeline::default();
        let mut frame = Frame::default();
        let mut ticks = Vec::new();
        let (mut cpus, mut phys) = (1, mem.map_or(0, |m| m.total_phys));
        let start = Instant::now();
        let end = start + Duration::from_secs(args.secs);
        let mut failure = None;
        while Instant::now() < end {
            if let (Some((at, a)), None) = (&plan, &child) {
                if start.elapsed() >= Duration::from_secs(*at) {
                    match spawn_testee(a) {
                        Ok(c) => child = Some(c),
                        Err(e) => {
                            failure = Some(format!("testee: {e}"));
                            break;
                        }
                    }
                }
            }
            if rx.recv_timeout(Duration::from_millis(250)).is_err() || !host.take(&mut frame) {
                continue;
            }
            cpus = cpus.max(frame.sys.logical_cpus);
            phys = frame.sys.mem.map_or(phys, |m| m.total_phys);
            ticks.push(pipeline.tick(&frame, own, child.as_ref().map(|c| c.id()), &root));
        }
        host.stop();
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(e) = failure {
            return Err(e);
        }
        save(&args.out, &Trace { header: header(build, args.scenario.name(), cpus, phys), ticks })
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "trace_record: {e:?}\nusage: trace_record --scenario idle|busy|alloc|hang|tree --secs N --out <file>"
            );
            std::process::exit(2)
        }
    };
    #[cfg(windows)]
    if let Err(e) = live::record(&args) {
        eprintln!("trace_record: {e}");
        std::process::exit(1)
    }
    #[cfg(not(windows))]
    let _ = args;
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sysmon::ProcRow;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn arguments() {
        let ok = parse_args(&a("--scenario busy --secs 120 --out x.trace")).unwrap();
        assert_eq!(ok, Args { scenario: Scenario::Busy, secs: 120, out: "x.trace".into() });
        assert_eq!(parse_args(&a("--secs 5 --out f --scenario idle")).unwrap().scenario, Scenario::Idle);
        for s in ["alloc", "hang", "tree"] {
            assert!(parse_args(&a(&format!("--scenario {s} --secs 1 --out f"))).is_ok());
        }
        assert_eq!(parse_args(&a("--secs 5 --out f")), Err(ArgError::Missing("--scenario")));
        assert_eq!(parse_args(&a("--scenario idle --out f")), Err(ArgError::Missing("--secs")));
        assert_eq!(parse_args(&a("--scenario idle --secs 5")), Err(ArgError::Missing("--out")));
        assert!(matches!(parse_args(&a("--scenario fast --secs 5 --out f")), Err(ArgError::Bad(_))));
        assert!(matches!(parse_args(&a("--scenario idle --secs 0 --out f")), Err(ArgError::Bad(_))));
        assert!(matches!(parse_args(&a("--scenario idle --secs 5 --out")), Err(ArgError::Bad(_))));
        assert!(matches!(parse_args(&a("--bogus 1")), Err(ArgError::Bad(_))));
    }

    #[test]
    fn scenarios_start_their_testee() {
        assert_eq!(Scenario::Idle.testee(180), None);
        assert_eq!(Scenario::Busy.testee(120), Some((20, a("--busy 1 --secs 60"))));
        assert_eq!(Scenario::Alloc.testee(120), Some((20, a("--alloc 512 --secs 100"))));
        assert_eq!(Scenario::Hang.testee(60), Some((10, a("--hang-after 5000 --secs 50"))));
        assert_eq!(Scenario::Tree.testee(60), Some((10, a("--tree 3 --secs 50"))));
    }

    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: pid as u64 * 10 }
    }

    fn group_of(app: u32, members: &[u32]) -> Group {
        Group {
            key: AppKey::App(IdentityId(app)),
            section: tasks::model::view::Section::Apps,
            members: members.iter().map(|p| key(*p)).collect(),
            windows: 0,
            hung_windows: 0,
        }
    }

    #[test]
    fn testee_group_is_t00_by_membership() {
        let mut groups = vec![group_of(1, &[10]), group_of(2, &[20, 21]), group_of(3, &[30]), group_of(4, &[40, 41])];
        let mut t = Tokens::default();
        let mut out = Vec::new();
        t.assign(&groups, Some(key(41)), &mut out);
        assert_eq!(out, ["a01", "a02", "a03", "t00"]);
        // The next tick: the groups move around and a new one appears; every AppKey keeps its token.
        groups.reverse();
        groups.push(group_of(9, &[90]));
        t.assign(&groups, Some(key(41)), &mut out);
        assert_eq!(out, ["t00", "a03", "a02", "a01", "a04"]);
        // Before the testee exists nothing is t00.
        let mut fresh = Tokens::default();
        fresh.assign(&groups[..2], None, &mut out);
        assert_eq!(out, ["a01", "a02"]);
    }

    #[test]
    fn own_chain_becomes_the_hosts() {
        let mut t = ProcTable::new();
        // terminal (10) -> shell (20) -> recorder (30); an unrelated process (40).
        for (pid, ppid) in [(10, 1), (20, 10), (30, 20), (40, 10)] {
            t.push(ProcRow { key: key(pid), parent_pid: ppid, ..ProcRow::default() });
        }
        t.finish();
        let mut hosts = Vec::new();
        hosts_of(&t, 30, &mut hosts);
        assert_eq!(hosts, [key(30), key(20), key(10)]);
        hosts_of(&t, 999, &mut hosts);
        assert!(hosts.is_empty());
    }
}
