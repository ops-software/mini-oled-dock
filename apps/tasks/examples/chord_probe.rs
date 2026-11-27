//! The probe of the live chord matrix (T6b.2, LIVE-T6b): presses Ctrl+Shift+Esc (injected, untagged) and records which
//! task manager comes up, how fast, and whether it took the foreground.
//!
//! chord_probe --runs N --expect tasks|taskmgr [--case <label>] [--from <n>] [--own <pid>] [--tasks <path to tasks.exe>]
//!             [--gap <ms>] [--squat] [--out <file>]
//!
//! * Before each run the Taskmgr and Tasks processes that earlier runs created must be gone (the orchestrator's
//!   elevated step ends Taskmgr by the pids each line lists; the probe closes Tasks itself), and the foreground window
//!   must be the one the first run started from, which itself had to belong to the tester (this console, or the --own
//!   process) or to the Tasks under test. The probe waits up to 30 s for that, else that run is not injected and its
//!   line says why ("foreground not ours", "taskmgr still open", "tasks still open"). The foreground is checked again
//!   as the last step before the injection.
//! * A run sends Ctrl+Shift+Esc, then looks every 10 ms for 2,500 ms for a window of Tasks (class MiniOledTasks, image
//!   tasks.exe, or the --tasks file when given) or of Windows' Task Manager (image %SystemRoot%\System32\Taskmgr.exe)
//!   that became the foreground window, else one that became visible. A consent.exe created after the probe started
//!   stops the probe at once: no further injection.
//! * One JSON line per run on stdout, also appended to --out: case, run (from --from, default 1), expect, got (tasks,
//!   taskmgr, none, aborted or consent prompt), ms (from the injection), foreground, taskmgr_pids_created, and note
//!   when there is one.
//! * It closes only Tasks windows of processes created after it started, with WM_CLOSE (medium to medium). It never
//!   closes Windows' Task Manager: that runs elevated and UIPI drops our WM_CLOSE.
//! * --squat holds the single-instance mutex of Tasks, with no window, for the whole session.
//!
//! Exit: 0 every run recorded, 1 a failure of the probe itself, 2 bad arguments, 4 stopped by a consent prompt.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oled_sysmon::{ProcKey, ProcTable, SnapshotReader};
use oled_trust::frame::tasks::{MUTEX_NAME, WINDOW_CLASS};
use windows::core::{BOOL, HSTRING, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WIN32_ERROR, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemTimeAsFileTime};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_ESCAPE, VK_LCONTROL, VK_LSHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, SetForegroundWindow, GA_ROOTOWNER, WM_CLOSE,
};

/// What --squat holds: the single-instance object of Tasks itself (X10 dropped the ready event).
const SQUAT_NAME: &str = MUTEX_NAME;
/// A run looks this often, for this long.
const POLL_MS: u64 = 10;
const RUN_MS: u64 = 2_500;
/// Before a run: how long the probe waits for the leftovers of the last one to go and the foreground to be right.
const READY_WAIT_MS: u64 = 30_000;
const READY_POLL_MS: u64 = 100;
/// After the last run: how long the probe waits for the Tasks it closed to exit.
const FINISH_WAIT_MS: u64 = 5_000;
const MAX_RUNS: u32 = 500;
const DEFAULT_GAP_MS: u32 = 1_000;
const MAX_GAP_MS: u32 = 60_000;
const MAX_CASE_CHARS: usize = 32;
/// Top-level windows one look reads at most.
const MAX_WINDOWS: usize = 4_096;

const USAGE: &str = "usage: chord_probe --runs N --expect tasks|taskmgr [--case <label>] [--from <n>] [--own <pid>] \
                     [--tasks <path to tasks.exe>] [--gap <ms>] [--squat] [--out <file>]";

/// The two task managers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    Tasks,
    Taskmgr,
}

impl Want {
    fn as_str(self) -> &'static str {
        match self {
            Want::Tasks => "tasks",
            Want::Taskmgr => "taskmgr",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Args {
    runs: u32,
    expect: Want,
    case: String,
    from: u32,
    own: Option<u32>,
    tasks: Option<PathBuf>,
    gap_ms: u32,
    squat: bool,
    out: Option<PathBuf>,
}

const VALUE_FLAGS: [&str; 8] = ["--runs", "--expect", "--case", "--from", "--own", "--tasks", "--gap", "--out"];

/// The arguments after the program name: each flag at most once, --runs and --expect required.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let (mut runs, mut expect, mut case, mut from, mut own) = (None, None, None, None, None);
    let (mut tasks, mut gap, mut out, mut squat) = (None, None, None, false);
    let mut i = 0;
    while let Some(flag) = args.get(i) {
        let flag = flag.as_str();
        i += 1;
        if flag == "--squat" {
            if squat {
                return Err("--squat given twice".into());
            }
            squat = true;
            continue;
        }
        if !VALUE_FLAGS.contains(&flag) {
            return Err(format!("unknown argument {flag}"));
        }
        let Some(v) = args.get(i) else { return Err(format!("{flag} needs a value")) };
        i += 1;
        match flag {
            "--runs" => once(&mut runs, flag, number(flag, v, 1, MAX_RUNS)?)?,
            "--expect" => once(&mut expect, flag, want(v)?)?,
            "--case" => once(&mut case, flag, label(v)?)?,
            "--from" => once(&mut from, flag, number(flag, v, 1, u32::MAX - MAX_RUNS)?)?,
            "--own" => once(&mut own, flag, number(flag, v, 1, u32::MAX)?)?,
            "--tasks" => once(&mut tasks, flag, PathBuf::from(v))?,
            "--gap" => once(&mut gap, flag, number(flag, v, 0, MAX_GAP_MS)?)?,
            _ => once(&mut out, flag, PathBuf::from(v))?,
        }
    }
    Ok(Args {
        runs: runs.ok_or("--runs is required")?,
        expect: expect.ok_or("--expect is required")?,
        case: case.unwrap_or_default(),
        from: from.unwrap_or(1),
        own,
        tasks,
        gap_ms: gap.unwrap_or(DEFAULT_GAP_MS),
        squat,
        out,
    })
}

fn once<T>(slot: &mut Option<T>, flag: &str, v: T) -> Result<(), String> {
    if slot.replace(v).is_some() {
        return Err(format!("{flag} given twice"));
    }
    Ok(())
}

/// Decimal digits only, in lo..=hi.
fn number(flag: &str, v: &str, lo: u32, hi: u32) -> Result<u32, String> {
    let n = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then(|| v.parse::<u32>().ok()).flatten();
    n.filter(|n| (lo..=hi).contains(n)).ok_or_else(|| format!("{flag} takes a number in {lo}..={hi}, not {v}"))
}

fn want(v: &str) -> Result<Want, String> {
    match v {
        "tasks" => Ok(Want::Tasks),
        "taskmgr" => Ok(Want::Taskmgr),
        _ => Err(format!("--expect is tasks or taskmgr, not {v}")),
    }
}

/// A case label: 1..=32 of A-Z, a-z, 0-9, '-', '_' and '.'.
fn label(v: &str) -> Result<String, String> {
    let ok = (1..=MAX_CASE_CHARS).contains(&v.len())
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    ok.then(|| v.to_string()).ok_or_else(|| format!("--case takes 1 to {MAX_CASE_CHARS} letters, digits, - _ or ."))
}

/// What a run saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Got {
    Tasks,
    Taskmgr,
    Nothing,
    Aborted,
    Consent,
}

impl Got {
    fn as_str(self) -> &'static str {
        match self {
            Got::Tasks => "tasks",
            Got::Taskmgr => "taskmgr",
            Got::Nothing => "none",
            Got::Aborted => "aborted",
            Got::Consent => "consent prompt",
        }
    }
}

impl From<Want> for Got {
    fn from(w: Want) -> Got {
        match w {
            Want::Tasks => Got::Tasks,
            Want::Taskmgr => Got::Taskmgr,
        }
    }
}

/// One line of the record.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    case: String,
    run: u32,
    expect: Want,
    got: Got,
    /// From the injection to the window, when one came.
    ms: Option<u64>,
    foreground: bool,
    /// Taskmgr processes created during the run: the orchestrator's elevated step ends exactly these.
    taskmgr_pids: Vec<u32>,
    note: Option<String>,
}

impl Record {
    fn new(a: &Args, run: u32) -> Record {
        Record {
            case: a.case.clone(),
            run,
            expect: a.expect,
            got: Got::Nothing,
            ms: None,
            foreground: false,
            taskmgr_pids: Vec::new(),
            note: None,
        }
    }
}

/// The JSON line of a record, its fields in a fixed order.
fn json_line(r: &Record) -> String {
    let s = |v: &str| serde_json::Value::from(v).to_string();
    let ms = r.ms.map_or_else(|| "null".to_string(), |m| m.to_string());
    let pids: Vec<String> = r.taskmgr_pids.iter().map(u32::to_string).collect();
    let mut line = format!(
        "{{\"case\":{},\"run\":{},\"expect\":{},\"got\":{},\"ms\":{ms},\"foreground\":{},\"taskmgr_pids_created\":[{}]",
        s(&r.case),
        r.run,
        s(r.expect.as_str()),
        s(r.got.as_str()),
        r.foreground,
        pids.join(",")
    );
    if let Some(n) = &r.note {
        line.push_str(&format!(",\"note\":{}", s(n)));
    }
    line.push('}');
    line
}

/// An image name from the process snapshot equals `name`, ASCII case aside.
fn image_is(utf16: &[u16], name: &str) -> bool {
    utf16.len() == name.len()
        && utf16.iter().zip(name.bytes()).all(|(&u, b)| u8::try_from(u).is_ok_and(|c| c.eq_ignore_ascii_case(&b)))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("chord_probe: {e}\n{USAGE}");
            std::process::exit(2)
        }
    };
    std::process::exit(run(&a))
}

/// The processes a look watches, from one snapshot.
#[derive(Default)]
struct Procs {
    consent: Vec<ProcKey>,
    taskmgr: Vec<ProcKey>,
    tasks: Vec<ProcKey>,
}

/// One process snapshot per look (NtQuerySystemInformation through oled-sysmon: names and creation times at medium).
struct Watch {
    reader: SnapshotReader,
    table: ProcTable,
}

impl Watch {
    fn read(&mut self) -> Result<Procs, String> {
        self.reader.read(&mut self.table).map_err(|e| format!("no process snapshot ({e:?})"))?;
        let mut p = Procs::default();
        for row in &self.table.rows {
            let name = self.table.names.get(row.image);
            if image_is(name, "consent.exe") {
                p.consent.push(row.key);
            } else if image_is(name, "Taskmgr.exe") {
                p.taskmgr.push(row.key);
            } else if image_is(name, "tasks.exe") {
                p.tasks.push(row.key);
            }
        }
        Ok(p)
    }
}

/// Now as a FILETIME, the unit of a process's creation time.
fn now_filetime() -> u64 {
    // SAFETY: no arguments.
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn hwnd_of(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

fn foreground() -> isize {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow() }.0 as isize
}

fn pid_of(h: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` is writable; a window that is gone gives 0.
    unsafe { GetWindowThreadProcessId(hwnd_of(h), Some(&mut pid)) };
    pid
}

fn class_of(h: isize) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: the buffer is writable for its whole length, which the slice carries.
    let n = unsafe { GetClassNameW(hwnd_of(h), &mut buf) };
    buf.get(..usize::try_from(n).unwrap_or(0)).map(String::from_utf16_lossy).unwrap_or_default()
}

fn visible(h: isize) -> bool {
    // SAFETY: plain query of a window handle; a window that is gone reads as not visible.
    unsafe { IsWindowVisible(hwnd_of(h)) }.as_bool()
}

/// Not cloaked by DWM (another virtual desktop, a suspended app).
fn uncloaked(h: isize) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is writable and as large as the size given.
    let read = unsafe { DwmGetWindowAttribute(hwnd_of(h), DWMWA_CLOAKED, std::ptr::from_mut(&mut cloaked).cast(), 4) };
    read.is_err() || cloaked == 0
}

/// Every top-level window, at most MAX_WINDOWS.
fn top_windows() -> Vec<isize> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the address of the Vec on the caller's stack, alive for the whole EnumWindows call.
        let v = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
        v.push(hwnd.0 as isize);
        BOOL::from(v.len() < MAX_WINDOWS)
    }
    let mut v: Vec<isize> = Vec::new();
    // SAFETY: the callback only writes the Vec, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut v) as isize)) };
    v
}

/// The full path of a process's image (QueryFullProcessImageNameW: works for an elevated process too).
fn image_of(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain call; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer is writable for `len` units; the call writes the length it used into `len`.
    let read = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    // SAFETY: the handle OpenProcess returned, closed once.
    let _ = unsafe { CloseHandle(h) };
    read.ok()?;
    buf.get(..len as usize).map(|s| PathBuf::from(String::from_utf16_lossy(s)))
}

/// Who counts as the tester, and which images are the two task managers.
struct Env {
    own_pid: u32,
    /// This console's window and its root owner (a terminal's window).
    console: Vec<isize>,
    own: Option<u32>,
    tasks: Option<PathBuf>,
    taskmgr_image: PathBuf,
}

impl Env {
    fn new(a: &Args) -> Result<Env, String> {
        // SAFETY: no arguments.
        let con = unsafe { GetConsoleWindow() };
        let mut console = Vec::new();
        if !con.is_invalid() {
            console.push(con.0 as isize);
            // SAFETY: plain query of a window handle.
            let root = unsafe { GetAncestor(con, GA_ROOTOWNER) };
            if !root.is_invalid() {
                console.push(root.0 as isize);
            }
        }
        let mut buf = [0u16; 260];
        // SAFETY: the buffer is writable for its whole length, which the slice carries.
        let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
        let system = buf.get(..n).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).ok_or("no system folder")?;
        Ok(Env {
            own_pid: std::process::id(),
            console,
            own: a.own,
            tasks: a.tasks.clone(),
            taskmgr_image: Path::new(&system).join("Taskmgr.exe"),
        })
    }

    fn tasks_image(&self, image: &Path) -> bool {
        match &self.tasks {
            Some(t) => oled_sys::ident::same_image(image, t),
            None => image.file_name().is_some_and(|n| n.eq_ignore_ascii_case("tasks.exe")),
        }
    }

    fn taskmgr_pid(&self, pid: u32) -> bool {
        image_of(pid).is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(self.taskmgr_image.as_os_str()))
    }

    /// Which task manager a window belongs to, if any.
    fn kind(&self, h: isize, p: &Procs) -> Option<Want> {
        let pid = pid_of(h);
        if pid == 0 {
            return None;
        }
        if p.taskmgr.iter().any(|k| k.pid == pid) {
            return self.taskmgr_pid(pid).then_some(Want::Taskmgr);
        }
        (class_of(h) == WINDOW_CLASS && image_of(pid).is_some_and(|i| self.tasks_image(&i))).then_some(Want::Tasks)
    }

    /// The tester's own window: this console (or its terminal) or a window of the --own process.
    fn testers(&self, h: isize) -> bool {
        self.console.contains(&h) || {
            let pid = pid_of(h);
            pid != 0 && (pid == self.own_pid || self.own == Some(pid))
        }
    }

    /// The shown windows of either task manager (the cheap tests first: visible, then the kind, then DWM's cloak).
    fn task_windows(&self, p: &Procs) -> Vec<(isize, Want)> {
        top_windows()
            .into_iter()
            .filter(|&h| visible(h))
            .filter_map(|h| self.kind(h, p).map(|k| (h, k)))
            .filter(|&(h, _)| uncloaked(h))
            .collect()
    }
}

/// What the probe's runs created, and when it started.
struct Session {
    started: u64,
    taskmgr: Vec<ProcKey>,
    tasks: Vec<ProcKey>,
    /// Windows already sent WM_CLOSE.
    closed: Vec<isize>,
}

impl Session {
    /// Notes the Taskmgr and Tasks processes created since `since`.
    fn note(&mut self, env: &Env, p: &Procs, since: u64) {
        for k in &p.taskmgr {
            if k.created >= since && !self.taskmgr.contains(k) && env.taskmgr_pid(k.pid) {
                self.taskmgr.push(*k);
            }
        }
        for k in &p.tasks {
            if k.created >= since && !self.tasks.contains(k) && image_of(k.pid).is_some_and(|i| env.tasks_image(&i)) {
                self.tasks.push(*k);
            }
        }
    }

    fn consent(&self, p: &Procs) -> bool {
        p.consent.iter().any(|k| k.created >= self.started)
    }

    /// WM_CLOSE, once each, to the Tasks windows of the processes the runs created (medium to medium).
    fn close_tasks(&mut self, p: &Procs) {
        let alive: Vec<u32> = self.tasks.iter().filter(|k| p.tasks.contains(k)).map(|k| k.pid).collect();
        if alive.is_empty() {
            return;
        }
        for h in top_windows() {
            if self.closed.contains(&h) || !alive.contains(&pid_of(h)) || class_of(h) != WINDOW_CLASS {
                continue;
            }
            // SAFETY: posts WM_CLOSE to a Tasks window of a process this probe's run caused.
            let _ = unsafe { PostMessageW(Some(hwnd_of(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            self.closed.push(h);
        }
    }

    fn taskmgr_left(&self, p: &Procs) -> bool {
        self.taskmgr.iter().any(|k| p.taskmgr.contains(k))
    }

    fn tasks_left(&self, p: &Procs) -> Vec<u32> {
        self.tasks.iter().filter(|k| p.tasks.contains(k)).map(|k| k.pid).collect()
    }
}

/// What the wait before a run decided; Go carries the foreground window it accepted.
enum Ready {
    Go(isize),
    Abort(&'static str),
    Consent,
}

/// Waits (bounded) until the last run's Taskmgr and Tasks are gone and the foreground is the run's starting point.
fn ready(env: &Env, w: &mut Watch, s: &mut Session, first: &mut Option<isize>) -> Result<Ready, String> {
    let begun = Instant::now();
    let mut asked_front = false;
    loop {
        let p = w.read()?;
        if s.consent(&p) {
            return Ok(Ready::Consent);
        }
        s.close_tasks(&p);
        let (taskmgr_left, tasks_left) = (s.taskmgr_left(&p), !s.tasks_left(&p).is_empty());
        let fg = foreground();
        let fg_ok = match *first {
            Some(f) => fg == f,
            None => fg != 0 && (env.testers(fg) || env.kind(fg, &p) == Some(Want::Tasks)),
        };
        if !taskmgr_left && !tasks_left && fg_ok {
            first.get_or_insert(fg);
            return Ok(Ready::Go(fg));
        }
        if let (Some(f), false, false, false) = (*first, taskmgr_left, tasks_left, asked_front) {
            // Once, best effort: the window the first run started from, back in front (no input is sent for it).
            // SAFETY: plain call on the tester's window or the Tasks under test.
            let _ = unsafe { SetForegroundWindow(hwnd_of(f)) };
            asked_front = true;
        }
        if begun.elapsed() >= Duration::from_millis(READY_WAIT_MS) {
            return Ok(Ready::Abort(if taskmgr_left {
                "taskmgr still open"
            } else if tasks_left {
                "tasks still open"
            } else {
                "foreground not ours"
            }));
        }
        std::thread::sleep(Duration::from_millis(READY_POLL_MS));
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    // SAFETY: no pointers.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk.0), MAPVK_VK_TO_VSC) };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: u16::try_from(scan).unwrap_or(0),
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Ctrl+Shift+Esc, presses and releases in one SendInput call, so no key can be left down. Untagged (dwExtraInfo 0).
fn send_chord() -> bool {
    let keys = [
        key(VK_LCONTROL, false),
        key(VK_LSHIFT, false),
        key(VK_ESCAPE, false),
        key(VK_ESCAPE, true),
        key(VK_LSHIFT, true),
        key(VK_LCONTROL, true),
    ];
    // SAFETY: the array lives for the call; cbsize is the size of one INPUT.
    let sent = unsafe { SendInput(&keys, std::mem::size_of::<INPUT>() as i32) };
    sent as usize == keys.len()
}

fn elapsed_ms(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One run: the chord, then the look for a task manager.
fn one_run(env: &Env, w: &mut Watch, s: &mut Session, r: &mut Record, go: isize) -> Result<(), String> {
    let p0 = w.read()?;
    let shown0: Vec<isize> = env.task_windows(&p0).into_iter().map(|(h, _)| h).collect();
    // The last step before the injection: the window ready() accepted must still be the foreground.
    let fg0 = foreground();
    if fg0 != go {
        r.got = Got::Aborted;
        r.note = Some("foreground not ours".into());
        return Ok(());
    }
    let since = now_filetime();
    if !send_chord() {
        r.note = Some("input not sent".into());
        return Ok(());
    }
    let t0 = Instant::now();
    let (mut front, mut visible) = (None, None);
    loop {
        let ms = elapsed_ms(t0);
        let p = w.read()?;
        s.note(env, &p, since);
        if s.consent(&p) {
            r.got = Got::Consent;
            r.ms = Some(ms);
            break;
        }
        let fg = foreground();
        if fg != 0 && fg != fg0 {
            if let Some(k) = env.kind(fg, &p) {
                front = Some((k, ms));
                break;
            }
        }
        if visible.is_none() {
            visible = env.task_windows(&p).into_iter().find(|(h, _)| !shown0.contains(h)).map(|(_, k)| (k, ms));
        }
        if ms >= RUN_MS {
            break;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
    if r.got != Got::Consent {
        match (front, visible) {
            (Some((k, ms)), _) => (r.got, r.ms, r.foreground) = (k.into(), Some(ms), true),
            (None, Some((k, ms))) => (r.got, r.ms, r.foreground) = (k.into(), Some(ms), false),
            (None, None) => {}
        }
    }
    // A last look, for a Taskmgr that started late.
    if let Ok(p) = w.read() {
        s.note(env, &p, since);
        s.close_tasks(&p);
    }
    r.taskmgr_pids = s.taskmgr.iter().filter(|k| k.created >= since).map(|k| k.pid).collect();
    Ok(())
}

/// Holds the Tasks mutex while it lives.
struct Squat(HANDLE);

impl Drop for Squat {
    fn drop(&mut self) {
        // SAFETY: the handle CreateMutexW returned, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn squat() -> Result<Squat, String> {
    // SAFETY: clears this thread's last error, so the one read below is CreateMutexW's.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // SAFETY: a NUL-terminated name that lives for the call; no security attributes.
    let h = unsafe { CreateMutexW(None, false, &HSTRING::from(SQUAT_NAME)) }
        .map_err(|e| format!("the Tasks mutex cannot be held ({:#x})", e.code().0))?;
    // SAFETY: reads this thread's last error.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        eprintln!("chord_probe: the Tasks mutex existed already (a Tasks is running?)");
    }
    Ok(Squat(h))
}

fn emit(out: &mut Option<File>, line: &str) {
    let mut so = std::io::stdout().lock();
    let _ = writeln!(so, "{line}");
    let _ = so.flush();
    if let Some(f) = out {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

fn run(a: &Args) -> i32 {
    let mut out = match &a.out {
        Some(p) => match OpenOptions::new().create(true).append(true).open(p) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("chord_probe: {} cannot be opened ({e})", p.display());
                return 1;
            }
        },
        None => None,
    };
    let _squat = match a.squat.then(squat).transpose() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("chord_probe: {e}");
            return 1;
        }
    };
    let env = match Env::new(a) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("chord_probe: {e}");
            return 1;
        }
    };
    let mut w = Watch { reader: SnapshotReader::new(), table: ProcTable::new() };
    let mut s = Session { started: now_filetime(), taskmgr: Vec::new(), tasks: Vec::new(), closed: Vec::new() };
    let mut first = None;
    for i in 0..a.runs {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(a.gap_ms)));
        }
        let mut r = Record::new(a, a.from.saturating_add(i));
        let step = ready(&env, &mut w, &mut s, &mut first).and_then(|go| match go {
            Ready::Go(fg) => one_run(&env, &mut w, &mut s, &mut r, fg),
            Ready::Abort(note) => {
                r.got = Got::Aborted;
                r.note = Some(note.into());
                Ok(())
            }
            Ready::Consent => {
                r.got = Got::Consent;
                Ok(())
            }
        });
        if let Err(e) = step {
            eprintln!("chord_probe: {e}");
            return 1;
        }
        emit(&mut out, &json_line(&r));
        if r.got == Got::Consent {
            // Stop at once: no further injection.
            return 4;
        }
    }
    finish(&mut w, &mut s);
    0
}

/// After the last run: the Tasks the runs created are closed and awaited; whatever is left is named on stderr.
fn finish(w: &mut Watch, s: &mut Session) {
    let begun = Instant::now();
    while let Ok(p) = w.read() {
        s.close_tasks(&p);
        let left = s.tasks_left(&p);
        if left.is_empty() {
            return;
        }
        if begun.elapsed() >= Duration::from_millis(FINISH_WAIT_MS) {
            eprintln!("chord_probe: tasks pids still running {left:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(READY_POLL_MS));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn probe_args() {
        let a = parse_args(&args("--runs 50 --expect tasks")).unwrap();
        assert_eq!(
            a,
            Args {
                runs: 50,
                expect: Want::Tasks,
                case: String::new(),
                from: 1,
                own: None,
                tasks: None,
                gap_ms: DEFAULT_GAP_MS,
                squat: false,
                out: None,
            }
        );
        let a = parse_args(&args(
            "--squat --out C:\\rec\\c9.jsonl --expect taskmgr --runs 1 --case 9 --from 17 --own 4242 --tasks C:\\x\\tasks.exe --gap 0",
        ))
        .unwrap();
        assert_eq!(
            (a.runs, a.expect, a.case.as_str(), a.from, a.own, a.gap_ms, a.squat),
            (1, Want::Taskmgr, "9", 17, Some(4242), 0, true)
        );
        assert_eq!(a.out, Some(PathBuf::from("C:\\rec\\c9.jsonl")));
        assert_eq!(a.tasks, Some(PathBuf::from("C:\\x\\tasks.exe")));
        for bad in [
            "",
            "--runs 5",
            "--expect tasks",
            "--runs 0 --expect tasks",
            "--runs 501 --expect tasks",
            "--runs -1 --expect tasks",
            "--runs +5 --expect tasks",
            "--runs five --expect tasks",
            "--runs 5 --expect explorer",
            "--runs 5 --expect tasks --runs 6",
            "--runs 5 --expect tasks --squat --squat",
            "--runs 5 --expect tasks --gap 60001",
            "--runs 5 --expect tasks --own 0",
            "--runs 5 --expect tasks --case a\"b",
            "--runs 5 --expect tasks --case 012345678901234567890123456789012",
            "--runs 5 --expect tasks --from 0",
            "--runs 5 --expect tasks --verbose",
            "--runs 5 --expect tasks --out",
            "--runs 5 tasks",
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn probe_json_line() {
        let a = parse_args(&args("--runs 3 --expect tasks --case 2")).unwrap();
        let mut r = Record::new(&a, 3);
        r.got = Got::Tasks;
        r.ms = Some(212);
        r.foreground = true;
        assert_eq!(
            json_line(&r),
            r#"{"case":"2","run":3,"expect":"tasks","got":"tasks","ms":212,"foreground":true,"taskmgr_pids_created":[]}"#
        );
        r.got = Got::Taskmgr;
        r.foreground = false;
        r.taskmgr_pids = vec![812, 9040];
        r.note = Some("a \"quoted\" note".into());
        let line = json_line(&r);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["got"], "taskmgr");
        assert_eq!(v["taskmgr_pids_created"], serde_json::json!([812, 9040]));
        assert_eq!(v["note"], "a \"quoted\" note");
        r.got = Got::Nothing;
        r.ms = None;
        r.note = None;
        let v: serde_json::Value = serde_json::from_str(&json_line(&r)).unwrap();
        assert!(v["ms"].is_null());
        assert_eq!(v["got"], "none");
        assert!(v.get("note").is_none());
        r.got = Got::Consent;
        assert!(json_line(&r).contains("\"got\":\"consent prompt\""), "the abort rule looks for this text");
        r.got = Got::Aborted;
        r.note = Some("foreground not ours".into());
        assert!(json_line(&r).contains("\"got\":\"aborted\",") && json_line(&r).contains("foreground not ours"));
    }

    #[test]
    fn squat_names_the_tasks_mutex() {
        assert_eq!(SQUAT_NAME, oled_trust::frame::tasks::MUTEX_NAME);
        assert_eq!(SQUAT_NAME, "Local\\mini-oled-dock.tasks");
    }

    #[test]
    fn image_names_match_without_case() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert!(image_is(&w("taskmgr.EXE"), "Taskmgr.exe"));
        assert!(image_is(&w("consent.exe"), "consent.exe"));
        assert!(!image_is(&w("Taskmgr.exe.bak"), "Taskmgr.exe"));
        assert!(!image_is(&w("tasks.ex"), "tasks.exe"));
        assert!(!image_is(&w("tásks.exe"), "tasks.exe"));
        assert!(!image_is(&[], "tasks.exe"));
    }
}
