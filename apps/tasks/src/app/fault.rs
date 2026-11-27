//! The fault switch of the live chord matrix (F-T2, T6b.1): fault-inject builds only, never shipped. One line of
//! %LOCALAPPDATA%\mini-oled-dock\tasks\fault.txt, read once at start, makes this Tasks fail in one named way, so the
//! dock's Ctrl+Shift+Esc path and its fallback can be measured against it (LIVE-T6b):
//!
//! | Line | What Tasks does |
//! |---|---|
//! | hang-before-show <ms> | sleeps before the event loop, then exits without a window |
//! | crash-at-start | std::process::abort() |
//! | hang-after-show <ms> | the UI thread sleeps once, after the first paint: an instance that stopped answering |
//! | slow-show <ms> | sleeps before the event loop, then starts as usual |
//! | no-foreground | never asks for the front: the dock's own steps have to raise the window |
//!
//! ms is 1..=600000. A missing file, a line not in the table or more words on it change nothing; only the first 64
//! bytes are read. UTF-8 (with or without its BOM) and UTF-16LE with its BOM are read alike.
//!
//! Std and oled-sys only: tests/fault_t6b.rs compiles this file as a module of its own, so the parser is tested in every
//! build while the module exists in tasks only with the feature.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::UI::Shell::FOLDERID_LocalAppData;

/// The script, in the local folder of Tasks.
pub const FILE_NAME: &str = "fault.txt";
/// Bytes read from it; its first line has to fit in them.
pub const MAX_BYTES: usize = 64;
/// The longest sleep a line may ask (10 minutes), so a forgotten script cannot hang a Tasks for good.
pub const MAX_MS: u32 = 600_000;

/// One way to fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    HangBeforeShow { ms: u32 },
    CrashAtStart,
    HangAfterShow { ms: u32 },
    SlowShow { ms: u32 },
    NoForeground,
}

impl Fault {
    /// The first word of its line (also its name in the log).
    pub fn word(self) -> &'static str {
        match self {
            Fault::HangBeforeShow { .. } => "hang-before-show",
            Fault::CrashAtStart => "crash-at-start",
            Fault::HangAfterShow { .. } => "hang-after-show",
            Fault::SlowShow { .. } => "slow-show",
            Fault::NoForeground => "no-foreground",
        }
    }
}

/// The fault of this process, set once (at_start, or install in a test); None: no fault.
static FAULT: OnceLock<Option<Fault>> = OnceLock::new();
/// The first paint was seen: after_first_paint acts once.
static PAINTED: AtomicBool = AtomicBool::new(false);

/// The fault a script names. Pure and total: the first line of the first MAX_BYTES bytes, one word and, for the three
/// that sleep, one number of milliseconds in 1..=MAX_MS.
pub fn parse(bytes: &[u8]) -> Option<Fault> {
    let line = first_line(bytes.get(..MAX_BYTES).unwrap_or(bytes))?;
    let mut words = line.split_whitespace();
    let (word, arg, more) = (words.next()?, words.next(), words.next());
    if more.is_some() {
        return None;
    }
    match (word, arg) {
        ("crash-at-start", None) => Some(Fault::CrashAtStart),
        ("no-foreground", None) => Some(Fault::NoForeground),
        ("hang-before-show", Some(a)) => millis(a).map(|ms| Fault::HangBeforeShow { ms }),
        ("hang-after-show", Some(a)) => millis(a).map(|ms| Fault::HangAfterShow { ms }),
        ("slow-show", Some(a)) => millis(a).map(|ms| Fault::SlowShow { ms }),
        _ => None,
    }
}

/// The first line, without its line end: UTF-16LE when the bytes start with its BOM, else UTF-8 (its BOM dropped).
fn first_line(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| match c {
                [lo, hi] => u16::from_le_bytes([*lo, *hi]),
                _ => 0,
            })
            .take_while(|&u| u != u16::from(b'\n'))
            .collect();
        return String::from_utf16(&units).ok().map(|s| s.trim_end_matches('\r').to_string());
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let line = bytes.split(|&b| b == b'\n').next().unwrap_or_default();
    std::str::from_utf8(line).ok().map(|s| s.trim_end_matches('\r').to_string())
}

/// Decimal digits only, 1..=MAX_MS.
fn millis(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|ms| (1..=MAX_MS).contains(ms))
}

/// %LOCALAPPDATA%\mini-oled-dock\tasks\fault.txt (nothing is created).
pub fn script_path() -> Option<PathBuf> {
    oled_sys::datadir::known_folder(&FOLDERID_LocalAppData)
        .map(|d| d.join("mini-oled-dock").join("tasks").join(FILE_NAME))
}

/// The fault of the script at `path`: None when it cannot be read.
pub fn read(path: &Path) -> Option<Fault> {
    let mut buf = Vec::with_capacity(MAX_BYTES);
    std::fs::File::open(path).ok()?.take(MAX_BYTES as u64).read_to_end(&mut buf).ok()?;
    parse(&buf)
}

/// Sets the fault of this process; only the first call counts (true when this call set it).
pub fn install(f: Option<Fault>) -> bool {
    FAULT.set(f).is_ok()
}

/// The fault of this process (None before at_start and when the script names none).
pub fn current() -> Option<Fault> {
    FAULT.get().copied().flatten()
}

/// Step (4) of app::main: reads the script and acts on the faults of the start (no log yet: it starts at step 5).
pub fn at_start() {
    install(script_path().and_then(|p| read(&p)));
    match current() {
        Some(Fault::CrashAtStart) => std::process::abort(),
        Some(Fault::HangBeforeShow { ms }) => {
            std::thread::sleep(Duration::from_millis(u64::from(ms)));
            // No window at all: a late one would take the front in a later run of the matrix.
            std::process::exit(1)
        }
        Some(Fault::SlowShow { ms }) => std::thread::sleep(Duration::from_millis(u64::from(ms))),
        Some(Fault::HangAfterShow { .. } | Fault::NoForeground) | None => {}
    }
}

/// What the first paint does under `f`: the sleep of hang-after-show, else none. Pure.
pub fn sleep_after_paint(f: Option<Fault>) -> Option<Duration> {
    match f {
        Some(Fault::HangAfterShow { ms }) => Some(Duration::from_millis(u64::from(ms))),
        _ => None,
    }
}

/// The first paint is done (ui/show.rs on_first_paint, the start of the first call after it): names the fault in the
/// log once and, under hang-after-show, holds the UI thread for its time.
pub fn after_first_paint() {
    let Some(f) = current() else { return };
    if PAINTED.swap(true, Ordering::AcqRel) {
        return;
    }
    oled_sys::log!("tasks: fault {}", f.word());
    if let Some(d) = sleep_after_paint(Some(f)) {
        std::thread::sleep(d);
    }
}

/// false under no-foreground: Tasks does not ask the kit for the front (ui/show.rs). Pure over `f`.
pub fn front_allowed_under(f: Option<Fault>) -> bool {
    f != Some(Fault::NoForeground)
}

/// front_allowed_under the fault of this process.
pub fn front_allowed() -> bool {
    front_allowed_under(current())
}
