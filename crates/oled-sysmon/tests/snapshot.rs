//! The snapshot walk, the live reader and the capture sanitiser (SP-T1's layout, x64).
use std::io::Write;

use oled_sysmon::snapshot::{self, ENTRY_SIZE, INITIAL_BUFFER, MAX_BUFFER, THREAD_SIZE};
use oled_sysmon::{NameId, ProcKey, ProcRow, ProcTable, SnapshotReader, SysmonError};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};

/// Builds a SystemProcessInformation buffer at a made-up base address: each entry is followed by its thread array and its
/// image name inside its own span.
#[derive(Clone, Debug, Default)]
struct SnapEntry {
    pid: u32,
    parent: u32,
    name: Option<String>,
    threads: u32,
    create: i64,
    user: i64,
    kernel: i64,
    ws_private: i64,
    ws: u64,
    ws_peak: u64,
    private: u64,
    io: [i64; 3],
    handles: u32,
    session: u32,
    prio: i32,
    thread_create: i64,
    /// Spare bytes after the name inside the entry's span (the old name slot is bigger than the name).
    slack: usize,
}

impl SnapEntry {
    fn new(pid: u32, name: &str, threads: u32) -> SnapEntry {
        SnapEntry {
            pid,
            parent: 4,
            name: (!name.is_empty()).then(|| name.to_string()),
            threads,
            create: 1_000 + i64::from(pid),
            user: 10,
            kernel: 20,
            ws_private: 111,
            ws: 222,
            ws_peak: 333,
            private: 444,
            io: [1, 2, 3],
            handles: 55,
            session: 1,
            prio: 8,
            thread_create: 0,
            slack: 0,
        }
    }
}

const BASE: usize = 0x0000_7000_0000_0000;
const INVENTED_BASE: usize = snapshot::INVENTED_BASE;

fn put(b: &mut [u8], o: usize, v: &[u8]) {
    b[o..o + v.len()].copy_from_slice(v);
}

fn build(entries: &[SnapEntry]) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let off = buf.len();
        let name: Vec<u16> = e.name.as_deref().unwrap_or("").encode_utf16().collect();
        let tail = ENTRY_SIZE + THREAD_SIZE * e.threads as usize;
        let size = (tail + name.len() * 2 + 2 + e.slack).div_ceil(8) * 8;
        buf.resize(off + size, 0);
        let last = i + 1 == entries.len();
        put(&mut buf, off, &(if last { 0u32 } else { size as u32 }).to_le_bytes());
        put(&mut buf, off + 4, &e.threads.to_le_bytes());
        put(&mut buf, off + 8, &e.ws_private.to_le_bytes());
        put(&mut buf, off + 32, &e.create.to_le_bytes());
        put(&mut buf, off + 40, &e.user.to_le_bytes());
        put(&mut buf, off + 48, &e.kernel.to_le_bytes());
        for t in 0..e.threads as usize {
            let th = off + ENTRY_SIZE + THREAD_SIZE * t;
            put(&mut buf, th + 8, &6i64.to_le_bytes());
            put(&mut buf, th + 16, &e.thread_create.to_le_bytes());
            put(&mut buf, th + 24, &777u32.to_le_bytes());
            put(&mut buf, th + 32, &0x7FF6_0000_1000u64.to_le_bytes());
        }
        if !name.is_empty() {
            put(&mut buf, off + 56, &((name.len() * 2) as u16).to_le_bytes());
            put(&mut buf, off + 58, &((name.len() * 2 + 2) as u16).to_le_bytes());
            put(&mut buf, off + 64, &((BASE + off + tail) as u64).to_le_bytes());
            for (k, u) in name.iter().enumerate() {
                put(&mut buf, off + tail + k * 2, &u.to_le_bytes());
            }
        }
        put(&mut buf, off + 72, &e.prio.to_le_bytes());
        put(&mut buf, off + 80, &u64::from(e.pid).to_le_bytes());
        put(&mut buf, off + 88, &u64::from(e.parent).to_le_bytes());
        put(&mut buf, off + 96, &e.handles.to_le_bytes());
        put(&mut buf, off + 100, &e.session.to_le_bytes());
        put(&mut buf, off + 24, &9_999u64.to_le_bytes());
        put(&mut buf, off + 104, &0xFFFF_8000_1234_0000u64.to_le_bytes());
        put(&mut buf, off + 136, &e.ws_peak.to_le_bytes());
        put(&mut buf, off + 144, &e.ws.to_le_bytes());
        put(&mut buf, off + 200, &e.private.to_le_bytes());
        put(&mut buf, off + 232, &e.io[0].to_le_bytes());
        put(&mut buf, off + 240, &e.io[1].to_le_bytes());
        put(&mut buf, off + 248, &e.io[2].to_le_bytes());
    }
    buf
}

fn layout_what(r: Result<snapshot::ReadStats, SysmonError>) -> &'static str {
    match r {
        Err(SysmonError::Layout { what, .. }) => what,
        other => panic!("expected a layout error, got {other:?}"),
    }
}

fn three() -> Vec<SnapEntry> {
    vec![SnapEntry::new(4, "System", 3), SnapEntry::new(100, "alpha.exe", 2), SnapEntry::new(200, "beta tool", 1)]
}

fn fifty() -> Vec<u8> {
    let entries: Vec<SnapEntry> =
        (0..50).map(|i| SnapEntry::new(4 + i * 4, &format!("proc{i}.exe"), 1 + i % 5)).collect();
    build(&entries)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const EDGES: [u64; 10] =
    [0, 1, 7, 8, 0x100, 0xFFFF, 0x7FFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF, 0x7FFF_7000_0000_0000];

/// One mutated input per index: byte flips, a cut, or an edge value written into a header or name field.
fn mutate(rng: &mut Rng, base_buf: &[u8], i: usize) -> Vec<u8> {
    let mut b = base_buf.to_vec();
    match i % 4 {
        0 => {
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(b.len());
                b[at] ^= 1 << rng.below(8);
            }
        }
        1 => {
            let cut = ((i / 4) * 97) % b.len();
            b.truncate(cut);
        }
        _ => {
            let entry = rng.below(50);
            let mut off = 0usize;
            for _ in 0..entry {
                let next = u32::from_le_bytes(b[off..off + 4].try_into().unwrap()) as usize;
                if next == 0 || off + next + 8 > b.len() {
                    break;
                }
                off += next;
            }
            let v = EDGES[rng.below(EDGES.len())];
            match rng.below(4) {
                0 => put(&mut b, off, &(v as u32).to_le_bytes()),
                1 => put(&mut b, off + 4, &(v as u32).to_le_bytes()),
                2 => put(&mut b, off + 56, &(v as u16).to_le_bytes()),
                _ => put(&mut b, off + 64, &v.to_le_bytes()),
            }
        }
    }
    b
}

fn fuzz(n: usize) {
    fuzz_buf(&fifty(), BASE, n);
}

fn fuzz_buf(seed_buf: &[u8], base: usize, n: usize) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut table = ProcTable::new();
    for i in 0..n {
        let b = mutate(&mut rng, seed_buf, i);
        let mut copy = b.clone();
        let _ = snapshot::sanitize(&mut copy, base);
        if snapshot::parse(&b, base, &mut table).is_ok() {
            for r in &table.rows {
                assert!(r.image == NameId::NONE || (1..=260).contains(&table.names.get(r.image).len()));
            }
            assert!(table.rows.windows(2).all(|w| w[0].key.pid < w[1].key.pid));
        } else {
            assert!(table.rows.is_empty());
        }
    }
}

#[test]
fn parses_a_built_buffer() {
    let buf = build(&three());
    let mut t = ProcTable::new();
    let stats = snapshot::parse(&buf, BASE, &mut t).unwrap();
    assert_eq!((stats.entries, stats.threads, stats.bytes as usize), (3, 6, buf.len()));
    assert_eq!(t.len(), 3);
    let r = t.find_pid(100).unwrap();
    assert_eq!(r.key, ProcKey { pid: 100, created: 1_100 });
    assert_eq!((r.parent_pid, r.session, r.threads, r.handles, r.base_prio), (4, 1, 2, 55, 8));
    assert_eq!((r.cpu_100ns, r.ws, r.ws_peak, r.ws_private, r.private_bytes), (30, 222, 333, 111, 444));
    assert_eq!((r.io_read, r.io_write, r.io_other), (1, 2, 3));
    assert_eq!(t.names.to_string_lossy(r.image), "alpha.exe");
    assert_eq!(t.names.to_string_lossy(t.find_pid(200).unwrap().image), "beta tool");
    assert_eq!(t.names.to_string_lossy(t.find_pid(4).unwrap().image), "System");
    assert_eq!(t.find_pid(100).unwrap().parent, Some(ProcKey { pid: 4, created: 1_004 }));
}

#[test]
fn rejects_every_broken_bound() {
    let good = build(&three());
    let mut t = ProcTable::new();
    let mut check = |buf: &[u8], what: &str| {
        let got = layout_what(snapshot::parse(buf, BASE, &mut t));
        assert_eq!(got, what);
        assert!(t.is_empty());
    };
    // The first entry has 3 threads: its tail is 0x100 + 3 x 0x50.
    let mut b = good.clone();
    put(&mut b, 0, &(ENTRY_SIZE as u32).to_le_bytes());
    check(&b, "next entry offset too small");
    let mut b = good.clone();
    put(&mut b, 0, &(good.len() as u32 + 8).to_le_bytes());
    check(&b, "next entry offset");
    let mut b = good.clone();
    put(&mut b, 56, &7u16.to_le_bytes());
    check(&b, "image name length");
    let mut b = good.clone();
    put(&mut b, 56, &600u16.to_le_bytes());
    check(&b, "image name length");
    let mut b = good.clone();
    put(&mut b, 64, &((BASE - 8) as u64).to_le_bytes());
    check(&b, "image name pointer");
    let mut b = good.clone();
    put(&mut b, 64, &((BASE + good.len()) as u64).to_le_bytes());
    check(&b, "image name pointer");
    let mut b = good.clone();
    put(&mut b, 64, &((BASE + 301) as u64).to_le_bytes());
    check(&b, "image name pointer");
    let mut b = good.clone();
    put(&mut b, 4, &70_000u32.to_le_bytes());
    check(&b, "thread count");
    let mut b = good.clone();
    put(&mut b, 80, &(u64::from(u32::MAX) + 1).to_le_bytes());
    check(&b, "process id");
    check(&good[..ENTRY_SIZE - 1], "truncated entry");
    check(&[], "truncated entry");
    let mut dup = three();
    dup[2].pid = 100;
    check(&build(&dup), "duplicate process id");
    // An entry offset that is not a multiple of 8 (still past the tail and inside the buffer).
    let mut b = vec![0u8; ENTRY_SIZE + 4 + ENTRY_SIZE];
    put(&mut b, 0, &(ENTRY_SIZE as u32 + 4).to_le_bytes());
    check(&b, "unaligned entry");
    // One entry more than the cap.
    let many = 65_537usize;
    let mut big = vec![0u8; many * ENTRY_SIZE];
    for i in 0..many - 1 {
        put(&mut big, i * ENTRY_SIZE, &(ENTRY_SIZE as u32).to_le_bytes());
    }
    check(&big, "too many entries");
    // The last entry claims more threads than the buffer holds.
    let mut last = good.clone();
    let off = good.len() - (ENTRY_SIZE + THREAD_SIZE + 24);
    assert_eq!(&last[off + 80..off + 84], &200u32.to_le_bytes());
    put(&mut last, off + 4, &40u32.to_le_bytes());
    check(&last, "truncated thread array");
}

#[test]
fn empty_name_is_none() {
    let buf = build(&[SnapEntry::new(0, "", 4), SnapEntry::new(8, "x.exe", 1)]);
    let mut t = ProcTable::new();
    snapshot::parse(&buf, BASE, &mut t).unwrap();
    assert_eq!(t.find_pid(0).unwrap().image, NameId::NONE);
    assert_ne!(t.find_pid(8).unwrap().image, NameId::NONE);
}

#[test]
fn negative_times_read_zero() {
    let mut e = SnapEntry::new(8, "n.exe", 1);
    e.create = -5;
    e.user = -1;
    e.kernel = -7;
    e.ws_private = -3;
    e.io = [-1, -2, -3];
    let mut t = ProcTable::new();
    snapshot::parse(&build(&[e]), BASE, &mut t).unwrap();
    let r = t.find_pid(8).unwrap();
    assert_eq!((r.key.created, r.cpu_100ns, r.ws_private), (0, 0, 0));
    assert_eq!((r.io_read, r.io_write, r.io_other), (0, 0, 0));
}

#[test]
fn fuzz_smoke_never_panics() {
    fuzz(200_000);
}

#[test]
#[ignore]
fn fuzz_snapshot_walk_1m() {
    fuzz(1_000_000);
}

fn own_created() -> u64 {
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: four valid local FILETIMEs; the pseudo handle of the own process needs no rights.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.unwrap();
    (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime)
}

fn own_pid() -> u32 {
    // SAFETY: no arguments, no side effects.
    unsafe { GetCurrentProcessId() }
}

fn log_build() {
    let mut v =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: v is a valid local OSVERSIONINFOW with its size set.
    let _ = unsafe { RtlGetVersion(&mut v) };
    // stderr is not captured by libtest, so the build reaches the CI log even when the test passes.
    let _ = writeln!(std::io::stderr(), "windows build {}", v.dwBuildNumber);
}

#[test]
fn live_read_finds_this_process() {
    log_build();
    let mut reader = SnapshotReader::new();
    let mut t = ProcTable::new();
    reader.read(&mut t).unwrap();
    let r = t.find_pid(own_pid()).expect("own pid in the snapshot");
    assert!(r.threads >= 1 && r.handles > 0 && r.ws > 0);
    assert_eq!(r.key.created, own_created());
    let (reported, long) = snapshot::own_image_names();
    let name = t.names.get(r.image);
    assert!(snapshot::name_matches(name, reported.as_deref().unwrap_or(""), long.as_deref()));
}

#[test]
fn self_test_passes_here() {
    let mut reader = SnapshotReader::new();
    reader.self_test().unwrap();
    assert_eq!(reader.self_test_note(), None);
}

#[test]
fn self_test_rules() {
    use snapshot::self_test_verdict as v;
    assert_eq!(v(false, true, true, true), Err(SysmonError::SelfTest("own process: created")));
    assert_eq!(v(true, false, true, true), Err(SysmonError::SelfTest("own process: session")));
    assert_eq!(v(true, true, false, true), Err(SysmonError::SelfTest("own process: threads")));
    assert_eq!(v(false, false, false, false), Err(SysmonError::SelfTest("own process: created")));
    assert_eq!(v(true, true, true, false), Ok(Some("own process: image name")));
    assert_eq!(v(true, true, true, true), Ok(None));
}

#[test]
fn name_match_accepts_long_or_reported() {
    let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    assert!(snapshot::name_matches(&u("TASKS~1.EXE"), "TASKS~1.EXE", Some("tasks-engine.exe")));
    assert!(snapshot::name_matches(&u("Tasks-Engine.exe"), "TASKS~1.EXE", Some("tasks-engine.exe")));
    assert!(snapshot::name_matches(&u("tool.exe"), "TOOL.EXE", None));
    assert!(!snapshot::name_matches(&u("other.exe"), "TASKS~1.EXE", Some("tasks-engine.exe")));
    assert!(!snapshot::name_matches(&u("other.exe"), "TASKS~1.EXE", None));
}

#[test]
fn growth_is_bounded() {
    let kib64 = 64 * 1024;
    assert_eq!(snapshot::next_capacity(INITIAL_BUFFER, 300_000).unwrap(), 393_216);
    assert_eq!(snapshot::next_capacity(INITIAL_BUFFER, 1).unwrap(), 327_680);
    assert_eq!(snapshot::next_capacity(kib64, 10).unwrap() % kib64, 0);
    assert!(snapshot::next_capacity(kib64, 10).unwrap() > kib64);
    assert!(matches!(snapshot::next_capacity(MAX_BUFFER, 0), Err(SysmonError::TooLarge { .. })));
    assert!(matches!(snapshot::next_capacity(1024, u32::MAX), Err(SysmonError::TooLarge { need: u32::MAX })));
    assert!(snapshot::next_capacity(MAX_BUFFER / 2, 0).unwrap() <= MAX_BUFFER);
}

#[test]
fn a_read_reuses_its_buffer() {
    let mut reader = SnapshotReader::new();
    let mut t = ProcTable::new();
    reader.read(&mut t).unwrap();
    let first = reader.stats();
    reader.read(&mut t).unwrap();
    let second = reader.stats();
    assert_eq!(second.retries, 0);
    assert_eq!(second.capacity, first.capacity);
    assert!(second.entries > 0 && second.bytes > 0);
    let (raw, base) = reader.raw();
    assert_eq!(raw.len(), second.bytes as usize);
    assert_ne!(base, 0);
}

fn names(buf: &[u8], base: usize) -> Vec<String> {
    let mut t = ProcTable::new();
    snapshot::parse(buf, base, &mut t).unwrap();
    t.rows.iter().map(|r| t.names.to_string_lossy(r.image)).collect()
}

#[test]
fn sanitize_keeps_layout_and_system_names() {
    let mut entries =
        vec![SnapEntry::new(4, "System", 1), SnapEntry::new(8, "browser.exe", 2), SnapEntry::new(12, "MyTool", 1)];
    entries.push(SnapEntry::new(16, "SVCHOST.EXE", 1));
    let buf = build(&entries);
    let mut clean = buf.clone();
    assert_eq!(snapshot::sanitize(&mut clean, BASE).unwrap(), 2);
    assert_eq!(clean.len(), buf.len());
    assert_eq!(names(&clean, INVENTED_BASE), ["System", "x", "x", "SVCHOST.EXE"]);
    let (mut a, mut b) = (ProcTable::new(), ProcTable::new());
    snapshot::parse(&buf, BASE, &mut a).unwrap();
    snapshot::parse(&clean, INVENTED_BASE, &mut b).unwrap();
    for (x, y) in a.rows.iter().zip(&b.rows) {
        let blank = |r: &ProcRow| ProcRow {
            image: NameId::NONE,
            key: ProcKey { pid: r.key.pid, created: 0 },
            parent: None,
            ..*r
        };
        assert_eq!(blank(x), blank(y));
    }
}

#[test]
fn sanitize_handles_non_ascii_names() {
    // 5 units, last 4 UTF-8 bytes split a character; and a non-ASCII name ending in ".EXE".
    let mut buf = build(&[
        SnapEntry::new(4, "\u{30B9}\u{30B9}\u{30B9}\u{30B9}\u{30B9}", 1),
        SnapEntry::new(8, "\u{E9}\u{E9}\u{E9}.EXE", 1),
        SnapEntry::new(12, "ab\u{30B9}cd", 1),
    ]);
    assert_eq!(snapshot::sanitize(&mut buf, BASE).unwrap(), 3);
    let mut t = ProcTable::new();
    snapshot::parse(&buf, INVENTED_BASE, &mut t).unwrap();
    for pid in [4, 8, 12] {
        assert_eq!(t.names.to_string_lossy(t.find_pid(pid).unwrap().image), "x");
    }
}

#[test]
fn sanitize_is_idempotent() {
    let mut buf = build(&three());
    snapshot::sanitize(&mut buf, BASE).unwrap();
    let once = buf.clone();
    snapshot::sanitize(&mut buf, INVENTED_BASE).unwrap();
    assert_eq!(buf, once);
    assert!(snapshot::sanitize(&mut buf[..40], BASE).is_err());
}

/// 2026-11-20 17:46 UTC and neighbours, as FILETIMEs: a real wall clock of the kind a capture holds.
const REAL_T0: i64 = 134_084_000_000_000_000;
const DAY: i64 = 864_000_000_000;

fn timed(pid: u32, name: &str, create: i64) -> SnapEntry {
    SnapEntry { create, thread_create: create + 1_000, ..SnapEntry::new(pid, name, 2) }
}

fn name_lengths(buf: &[u8]) -> Vec<u16> {
    let mut v = Vec::new();
    let mut off = 0usize;
    loop {
        v.push(u16::from_le_bytes(buf[off + 56..off + 58].try_into().unwrap()));
        let next = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if next == 0 {
            return v;
        }
        off += next;
    }
}

#[test]
fn sanitize_gives_every_masked_name_one_length() {
    let entries = vec![
        SnapEntry::new(4, "System", 1),
        SnapEntry::new(8, "a-long-private-tool-name.exe", 2),
        SnapEntry::new(12, "tiny", 1),
        SnapEntry::new(16, "svchost.exe", 1),
        SnapEntry::new(20, "mid size.exe", 3),
        SnapEntry::new(24, "", 1),
    ];
    let buf = build(&entries);
    let mut clean = buf.clone();
    assert_eq!(snapshot::sanitize(&mut clean, BASE).unwrap(), 3);
    assert_eq!(clean.len(), buf.len());
    let lens = name_lengths(&clean);
    let old = name_lengths(&buf);
    for (i, pid) in [4u32, 8, 12, 16, 20, 24].into_iter().enumerate() {
        let masked = [8, 12, 20].contains(&pid);
        if masked {
            assert_eq!(lens[i] as usize, snapshot::MASK_UNITS * 2, "pid {pid}");
        } else {
            assert_eq!(lens[i], old[i], "pid {pid} untouched");
        }
    }
    assert_eq!(names(&clean, INVENTED_BASE), ["System", "x", "x", "svchost.exe", "x", ""]);
    // The old name bytes beyond the kept characters are zero: no tail of the real name survives.
    let units: Vec<u16> = clean.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    for gone in ["private", "tiny", "size"] {
        assert!(!text.contains(gone), "{gone} survived");
    }
    let (mut a, mut b) = (ProcTable::new(), ProcTable::new());
    let sa = snapshot::parse(&buf, BASE, &mut a).unwrap();
    let sb = snapshot::parse(&clean, INVENTED_BASE, &mut b).unwrap();
    assert_eq!((sa.entries, sa.threads), (sb.entries, sb.threads));
}

#[test]
fn sanitize_shifts_create_times_by_one_constant() {
    let start = snapshot::INVENTED_START as i64;
    let entries = vec![
        timed(0, "", 0),
        timed(4, "System", REAL_T0),
        timed(8, "private.exe", REAL_T0 + 3 * DAY + 77),
        timed(12, "svchost.exe", REAL_T0 + 5),
    ];
    let mut buf = build(&entries);
    snapshot::sanitize(&mut buf, BASE).unwrap();
    let mut t = ProcTable::new();
    snapshot::parse(&buf, INVENTED_BASE, &mut t).unwrap();
    let created = |pid: u32| t.find_pid(pid).unwrap().key.created as i64;
    assert_eq!(created(0), 0, "an unset time stays unset");
    assert_eq!(created(4), start, "the earliest process starts at the invented instant");
    assert_eq!(created(12) - created(4), 5);
    assert_eq!(created(8) - created(4), 3 * DAY + 77);
    // Thread create times move by the same constant (here 1000 after their process).
    let thread0 = |pid: u32| {
        let mut off = 0usize;
        loop {
            if u64::from_le_bytes(buf[off + 80..off + 88].try_into().unwrap()) == u64::from(pid) {
                return i64::from_le_bytes(buf[off + ENTRY_SIZE + 16..off + ENTRY_SIZE + 24].try_into().unwrap());
            }
            off += u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        }
    };
    assert_eq!(thread0(4), start + 1_000);
    assert_eq!(thread0(8), start + 3 * DAY + 77 + 1_000);
    for pid in [4, 8, 12] {
        assert!((created(pid) - REAL_T0).abs() > 365 * DAY);
        assert!((thread0(pid) - REAL_T0).abs() > 365 * DAY);
    }
}

#[test]
fn sanitize_clears_machine_fields_and_keeps_numbers() {
    let mut buf = build(&[timed(4, "System", REAL_T0), timed(8, "private.exe", REAL_T0 + 9)]);
    let before = buf.clone();
    snapshot::sanitize(&mut buf, BASE).unwrap();
    let mut off = 0usize;
    loop {
        let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        assert_eq!(u64_at(&buf, off + 104), 0, "unique process key");
        assert_eq!(u64_at(&buf, off + 24), 9_999, "cycle time stays");
        assert_eq!(u64_at(&buf, off + 40), u64_at(&before, off + 40), "user time stays");
        for t in 0..2 {
            let th = off + ENTRY_SIZE + THREAD_SIZE * t;
            assert_eq!(u32::from_le_bytes(buf[th + 24..th + 28].try_into().unwrap()), 0, "wait tick count");
            assert_eq!(u64_at(&buf, th + 32), 0, "thread start address");
            assert_eq!(u64_at(&buf, th), 0, "thread kernel time stays");
            assert_eq!(u64_at(&buf, th + 8), 6, "thread user time stays");
        }
        let next = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if next == 0 {
            break;
        }
        off += next;
    }
}

/// One entry's pieces, read by walking NextEntryOffset (not through parse), so spacing and spare fields are visible.
struct Raw {
    off: usize,
    fixed: Vec<u8>,
    threads: Vec<u8>,
    name: Vec<u8>,
    max: usize,
    next: usize,
    ptr: usize,
    /// Distance from the end of the thread array to the next entry (or to the end of the buffer for the last).
    slot: usize,
}

fn raw_entries(buf: &[u8], base: usize) -> Vec<Raw> {
    let u16_at = |o: usize| u16::from_le_bytes(buf[o..o + 2].try_into().unwrap()) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap()) as usize;
    let mut v = Vec::new();
    let mut off = 0usize;
    loop {
        let threads = u32_at(off + 4);
        let tail = ENTRY_SIZE + THREAD_SIZE * threads;
        let next = u32_at(off);
        let (len, max) = (u16_at(off + 56), u16_at(off + 58));
        let ptr = u64::from_le_bytes(buf[off + 64..off + 72].try_into().unwrap()) as usize;
        let name = if len > 0 { buf[ptr - base..ptr - base + len].to_vec() } else { Vec::new() };
        let end = if next == 0 { buf.len() } else { off + next };
        v.push(Raw {
            off,
            fixed: buf[off..off + ENTRY_SIZE].to_vec(),
            threads: buf[off + ENTRY_SIZE..off + tail].to_vec(),
            name,
            max,
            next,
            ptr,
            slot: end - off - tail,
        });
        if next == 0 {
            return v;
        }
        off += next;
    }
}

fn units(b: &[u8]) -> String {
    String::from_utf16_lossy(&b.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect::<Vec<_>>())
}

fn is_system(name: &str) -> bool {
    snapshot::SYSTEM_IMAGES.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// Asserts a and b are equal outside the given byte ranges.
fn same_outside(a: &[u8], b: &[u8], changed: &[std::ops::Range<usize>], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    for i in 0..a.len() {
        if !changed.iter().any(|r| r.contains(&i)) {
            assert_eq!(a[i], b[i], "{what}: byte {i}");
        }
    }
}

fn odd_slots() -> Vec<SnapEntry> {
    let slack = |n: usize, e: SnapEntry| SnapEntry { slack: n, ..e };
    vec![
        timed(0, "", 0),
        slack(0, timed(4, "System", REAL_T0)),
        slack(40, timed(8, "a-long-private-tool-name.exe", REAL_T0 + 3 * DAY)),
        slack(6, timed(12, "tiny", REAL_T0 + 50)),
        slack(100, timed(16, "svchost.exe", REAL_T0 + 7)),
        slack(0, timed(20, "mid size.exe", REAL_T0 + DAY)),
        slack(24, timed(24, "Memory Compression", REAL_T0 + 9)),
    ]
}

#[test]
fn sanitize_rebuilds_the_buffer_compactly() {
    let buf = build(&odd_slots());
    let mut clean = buf.clone();
    let (done, len) = snapshot::sanitize_len(&mut clean, BASE).unwrap();
    assert_eq!(done, 3);
    assert_eq!(clean.len(), buf.len(), "sanitize keeps the slice length");
    assert!(len < buf.len());
    assert!(clean[len..].iter().all(|&b| b == 0), "everything after the last entry is zero");
    let raw = raw_entries(&clean[..len], INVENTED_BASE);
    assert_eq!(raw.len(), 7);
    for (i, r) in raw.iter().enumerate() {
        let name = units(&r.name);
        let tail = r.threads.len() + ENTRY_SIZE;
        assert_eq!(r.off % 8, 0);
        assert_eq!(r.next == 0, i == raw.len() - 1);
        if r.name.is_empty() {
            assert_eq!((r.max, r.ptr), (0, 0));
            assert_eq!(r.slot, 0);
            continue;
        }
        assert_eq!(r.ptr, INVENTED_BASE + r.off + tail, "the name follows its thread array");
        if is_system(&name) {
            assert_eq!(r.max, r.name.len() + 2, "an allowlisted name keeps Length and MaximumLength");
            assert_eq!(r.slot, r.max.div_ceil(8) * 8);
        } else {
            assert_eq!((name.as_str(), r.name.len(), r.max, r.slot), ("x", 2, 4, 8));
        }
    }
    // The buffer ends at the last entry and nothing non-zero lies outside the entries' own bytes.
    let last = raw.last().unwrap();
    assert_eq!(len, last.off + ENTRY_SIZE + last.threads.len() + last.slot);
    let mut used = vec![false; len];
    for r in &raw {
        let tail = ENTRY_SIZE + r.threads.len();
        used[r.off..r.off + tail + r.max].fill(true);
    }
    assert!(clean[..len].iter().zip(&used).all(|(&b, &u)| u || b == 0), "no stray byte outside the entries");
}

#[test]
fn sanitize_changes_only_the_named_fields() {
    let buf = build(&odd_slots());
    let mut clean = buf.clone();
    let (_, len) = snapshot::sanitize_len(&mut clean, BASE).unwrap();
    let (old, new) = (raw_entries(&buf, BASE), raw_entries(&clean[..len], INVENTED_BASE));
    assert_eq!(old.len(), new.len());
    for (o, n) in old.iter().zip(&new) {
        // NextEntryOffset, create time, name Length/MaximumLength/pointer, unique process key.
        same_outside(&o.fixed, &n.fixed, &[0..4, 32..40, 56..72, 104..112], "entry");
        assert_eq!(o.threads.len(), n.threads.len());
        for t in 0..o.threads.len() / THREAD_SIZE {
            let r = t * THREAD_SIZE;
            // create time, wait ticks, start address.
            same_outside(
                &o.threads[r..r + THREAD_SIZE],
                &n.threads[r..r + THREAD_SIZE],
                &[16..24, 24..28, 32..40],
                "thread",
            );
        }
        if o.name.is_empty() || is_system(&units(&o.name)) {
            assert_eq!(o.name, n.name);
        }
    }
    let (mut a, mut b) = (ProcTable::new(), ProcTable::new());
    let sa = snapshot::parse(&buf, BASE, &mut a).unwrap();
    let sb = snapshot::parse(&clean, INVENTED_BASE, &mut b).unwrap();
    assert_eq!((sa.entries, sa.threads), (sb.entries, sb.threads));
    assert_eq!(a.rows.len(), b.rows.len());
    for (x, y) in a.rows.iter().zip(&b.rows) {
        let blank = |r: &ProcRow| ProcRow {
            image: NameId::NONE,
            key: ProcKey { pid: r.key.pid, created: 0 },
            parent: None,
            ..*r
        };
        assert_eq!(blank(x), blank(y));
        assert_eq!((x.parent_pid, x.cpu_100ns), (y.parent_pid, y.cpu_100ns));
    }
}

fn thread_times(buf: &[u8], base: usize) -> Vec<i64> {
    raw_entries(buf, base)
        .iter()
        .flat_map(|r| r.threads.chunks(THREAD_SIZE).map(|t| i64::from_le_bytes(t[16..24].try_into().unwrap())))
        .collect()
}

fn proc_times(buf: &[u8], base: usize) -> Vec<i64> {
    raw_entries(buf, base).iter().map(|r| i64::from_le_bytes(r.fixed[32..40].try_into().unwrap())).collect()
}

#[test]
fn time_shift_without_a_real_time_shifts_nothing_and_passes() {
    // Every time is below the floor: no wall clock to remove, none is invented.
    let mut buf = build(&[timed(4, "System", 500), timed(8, "p.exe", 900)]);
    snapshot::sanitize(&mut buf, BASE).unwrap();
    assert_eq!(proc_times(&buf, INVENTED_BASE), [500, 900]);
    assert_eq!(thread_times(&buf, INVENTED_BASE), [1_500, 1_500, 1_900, 1_900]);
}

#[test]
fn time_shift_uses_one_floor_for_processes_and_threads() {
    // A thread time is the earliest real time (its process time is below the floor): it defines the shift.
    let mut e = timed(8, "p.exe", 700);
    e.thread_create = REAL_T0;
    let mut buf = build(&[timed(4, "System", REAL_T0 + 50), e]);
    snapshot::sanitize(&mut buf, BASE).unwrap();
    let start = snapshot::INVENTED_START as i64;
    assert_eq!(proc_times(&buf, INVENTED_BASE), [start + 50, 700], "a time below the floor stays");
    assert_eq!(thread_times(&buf, INVENTED_BASE)[2], start);
}

#[test]
fn time_shift_fails_closed_outside_one_year() {
    let mut buf = build(&[timed(4, "System", REAL_T0), timed(8, "p.exe", REAL_T0 + 366 * DAY)]);
    let before = buf.clone();
    assert!(matches!(snapshot::sanitize(&mut buf, BASE), Err(SysmonError::Layout { what: "create time window", .. })));
    assert_eq!(buf, before, "a refused buffer is left as it was");
    // A time just inside the window passes.
    let mut ok = build(&[timed(4, "System", REAL_T0), timed(8, "p.exe", REAL_T0 + 364 * DAY)]);
    snapshot::sanitize(&mut ok, BASE).unwrap();
    // The extreme value cannot overflow into the window.
    let mut e = SnapEntry::new(8, "n.exe", 1);
    e.create = i64::MAX;
    e.thread_create = i64::MAX;
    let mut big = build(&[timed(4, "System", REAL_T0), e]);
    assert!(snapshot::sanitize(&mut big, BASE).is_err());
}

#[test]
fn time_shift_is_stable_on_a_sanitised_buffer() {
    let mut buf = build(&odd_slots());
    snapshot::sanitize(&mut buf, BASE).unwrap();
    let once = buf.clone();
    snapshot::sanitize(&mut buf, INVENTED_BASE).unwrap();
    assert_eq!(buf, once);
}

#[test]
fn system_image_list_has_forty_names() {
    assert_eq!(snapshot::SYSTEM_IMAGES.len(), 40);
    let mut sorted: Vec<String> = snapshot::SYSTEM_IMAGES.iter().map(|s| s.to_ascii_lowercase()).collect();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 40);
}

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A recorded capture: the 32-byte header (magic, build, entries, base, len) and the buffer.
struct Capture {
    build: u32,
    entries: u32,
    base: usize,
    buf: Vec<u8>,
}

fn load_capture(path: &std::path::Path) -> Capture {
    let raw = std::fs::read(path).unwrap();
    assert!(raw.len() >= 32 && &raw[0..8] == b"OSMSNAP1", "bad header");
    let u32_at = |o: usize| u32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    let base = u64::from_le_bytes(raw[16..24].try_into().unwrap());
    let len = u32_at(24) as usize;
    assert_eq!(raw.len(), 32 + len);
    Capture { build: u32_at(8), entries: u32_at(12), base: base as usize, buf: raw[32..].to_vec() }
}

fn fixture_26300() -> Capture {
    load_capture(&std::path::Path::new(FIXTURE_DIR).join("snap-26300.osnap"))
}

#[test]
fn fixture_26300_parses() {
    let c = fixture_26300();
    assert_eq!(c.build, 26300);
    let mut t = ProcTable::new();
    let stats = snapshot::parse(&c.buf, c.base, &mut t).unwrap();
    assert_eq!(t.rows.len(), c.entries as usize);
    assert_eq!(stats.entries, c.entries);
    let mut pids: Vec<u32> = t.rows.iter().map(|r| r.key.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(pids.len(), t.rows.len(), "pids are unique");
    for r in &t.rows {
        let name = t.names.to_string_lossy(r.image);
        let exempt =
            r.key.pid == 0 || name.eq_ignore_ascii_case("Secure System") || name.eq_ignore_ascii_case("Registry");
        assert!(exempt || r.threads >= 1, "pid {} ({name}) has no thread", r.key.pid);
    }
    assert!(t.rows.iter().any(|r| t.names.to_string_lossy(r.image).eq_ignore_ascii_case("svchost.exe")));
}

fn is_masked(name: &str) -> bool {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    !stem.is_empty() && stem.bytes().all(|b| b == b'x')
}

#[test]
fn fixtures_hold_no_private_names() {
    let mut seen = 0;
    for e in std::fs::read_dir(FIXTURE_DIR).unwrap() {
        let path = e.unwrap().path();
        if path.extension().and_then(|x| x.to_str()) != Some("osnap") {
            continue;
        }
        seen += 1;
        let c = load_capture(&path);
        let mut t = ProcTable::new();
        snapshot::parse(&c.buf, c.base, &mut t).unwrap();
        for r in &t.rows {
            if r.image == NameId::NONE {
                continue;
            }
            let name = t.names.to_string_lossy(r.image);
            let listed = snapshot::SYSTEM_IMAGES.iter().any(|s| s.eq_ignore_ascii_case(&name));
            assert!(listed || is_masked(&name), "{}: name outside the allowlist at pid {}", path.display(), r.key.pid);
        }
    }
    assert!(seen >= 1);
}

#[test]
fn fixture_26300_is_anonymous() {
    let c = fixture_26300();
    let mut t = ProcTable::new();
    let stats = snapshot::parse(&c.buf, c.base, &mut t).unwrap();
    assert_eq!((stats.entries, stats.threads), (432, 6993));
    // Every masked name has the one fixed length: no name length is left to guess from.
    let mut lens = std::collections::BTreeSet::new();
    let mut masked = 0;
    for r in &t.rows {
        let name = t.names.to_string_lossy(r.image);
        if r.image != NameId::NONE && !snapshot::SYSTEM_IMAGES.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            masked += 1;
            lens.insert(t.names.get(r.image).len());
        }
    }
    assert_eq!(masked, 283);
    assert_eq!(lens.into_iter().collect::<Vec<_>>(), [snapshot::MASK_UNITS]);
    // No wall clock: the earliest process starts at the invented instant and the file spans days, not years.
    let times: Vec<i64> = proc_times(&c.buf, c.base)
        .into_iter()
        .chain(thread_times(&c.buf, c.base))
        .filter(|&v| v >= snapshot::REAL_TIME_MIN)
        .collect();
    assert_eq!(times.iter().copied().min(), Some(snapshot::INVENTED_START as i64));
    assert!(times.iter().all(|&v| v < snapshot::INVENTED_START as i64 + 30 * DAY));
    assert!(times.iter().all(|&v| v <= snapshot::INVENTED_START as i64 + snapshot::TIME_WINDOW));
    assert_eq!(c.base, INVENTED_BASE, "the header holds the invented base");
    // No spacing or spare field depends on an old name: Length, MaximumLength and the slot are one value each.
    let raw = raw_entries(&c.buf, c.base);
    assert_eq!(raw.len(), 432);
    let (mut length, mut max, mut slot) = (Vec::new(), Vec::new(), Vec::new());
    for r in &raw {
        if r.name.is_empty() || is_system(&units(&r.name)) {
            continue;
        }
        length.push(r.name.len());
        max.push(r.max);
        slot.push(r.slot);
    }
    for (what, v, want) in [("Length", &mut length, 2), ("MaximumLength", &mut max, 4), ("slot", &mut slot, 8)] {
        v.sort_unstable();
        v.dedup();
        assert_eq!(*v, [want], "{what}");
    }
    // The file ends with the last entry; no non-zero byte lies outside the entries' own bytes.
    let mut used = vec![false; c.buf.len()];
    for r in &raw {
        let tail = ENTRY_SIZE + r.threads.len();
        if !r.name.is_empty() {
            assert_eq!(r.ptr, c.base + r.off + tail);
        }
        used[r.off..r.off + tail + r.max].fill(true);
    }
    assert!(c.buf.iter().zip(&used).all(|(&b, &u)| u || b == 0));
    let last = raw.last().unwrap();
    assert_eq!(c.buf.len(), last.off + ENTRY_SIZE + last.threads.len() + last.slot);
    assert_eq!(raw.iter().filter(|r| r.next == 0).count(), 1);
}

#[test]
fn fixture_fuzz_smoke() {
    let c = fixture_26300();
    fuzz_buf(&c.buf, c.base, 50_000);
}
