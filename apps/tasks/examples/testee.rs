//! A process that does one predictable thing for the live tester and the integration tests, then exits by itself.
//!
//! testee --busy <threads> [--secs N]      spin loops
//! testee --alloc <MiB> [--secs N]         commits and touches the pages
//! testee --hang-after <ms> [--secs N]     a plain window that pumps for ms, then stops pumping
//! testee --tree <depth> [--secs N]        one child of itself per level, without a window
//!
//! Every mode ends after N seconds (default 60, at most 300) and when its parent is gone.
use std::time::{Duration, Instant};

const DEFAULT_SECS: u64 = 60;
const MAX_SECS: u64 = 300;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Busy(u32),
    Alloc(u64),
    HangAfter(u64),
    Tree(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArgError {
    NoMode,
    TwoModes,
    MissingValue(String),
    BadValue(String),
    Unknown(String),
}

fn number(flag: &str, value: Option<&String>) -> Result<u64, ArgError> {
    let Some(v) = value else { return Err(ArgError::MissingValue(flag.into())) };
    v.parse::<u64>().map_err(|_| ArgError::BadValue(flag.into()))
}

/// The arguments after the program name. The seconds are capped at 300; zero threads, MiB or depth are refused.
fn parse_args(args: &[String]) -> Result<Args, ArgError> {
    let mut mode = None;
    let mut secs = DEFAULT_SECS;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = args.get(i + 1);
        let parsed = match flag {
            "--busy" => Some(Mode::Busy(number(flag, value)?.min(256) as u32)),
            "--alloc" => Some(Mode::Alloc(number(flag, value)?.min(16 * 1024))),
            "--hang-after" => Some(Mode::HangAfter(number(flag, value)?.min(MAX_SECS * 1000))),
            "--tree" => Some(Mode::Tree(number(flag, value)?.min(16) as u32)),
            "--secs" => {
                secs = number(flag, value)?.min(MAX_SECS);
                None
            }
            other => return Err(ArgError::Unknown(other.into())),
        };
        if let Some(m) = parsed {
            if mode.replace(m).is_some() {
                return Err(ArgError::TwoModes);
            }
        }
        i += 2;
    }
    match mode {
        None => Err(ArgError::NoMode),
        Some(Mode::Busy(0)) | Some(Mode::Alloc(0)) => Err(ArgError::BadValue("0".into())),
        Some(mode) => Ok(Args { mode, secs }),
    }
}

/// The arguments of the child one level down, or None at depth 0.
fn child_args(depth: u32, secs_left: u64) -> Option<Vec<String>> {
    let next = depth.checked_sub(1)?;
    Some(vec!["--tree".into(), next.to_string(), "--secs".into(), secs_left.max(1).to_string()])
}

#[cfg(windows)]
mod os {
    use super::*;
    use oled_sysmon::{ProcTable, SnapshotReader};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WAIT_TIMEOUT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW, TranslateMessage,
        CW_USEDEFAULT, MSG, PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    /// The open parent, or None when it cannot be found: then only the deadline ends the process.
    pub struct Parent(Option<HANDLE>);

    impl Parent {
        pub fn find() -> Parent {
            let mut table = ProcTable::new();
            if SnapshotReader::new().read(&mut table).is_err() {
                return Parent(None);
            }
            // SAFETY: no arguments.
            let me = unsafe { GetCurrentProcessId() };
            let Some(row) = table.find_pid(me) else { return Parent(None) };
            // A parent link is only real when the parent was created before the child (pid reuse).
            if row.parent.is_none() {
                return Parent(None);
            }
            // SAFETY: plain call; the handle is closed in Drop.
            let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, row.parent_pid) };
            Parent(h.ok())
        }

        pub fn gone(&self) -> bool {
            match self.0 {
                Some(h) => {
                    // SAFETY: the handle is open until Drop.
                    let r = unsafe { WaitForSingleObject(h, 0) };
                    r != WAIT_TIMEOUT
                }
                None => false,
            }
        }
    }

    impl Drop for Parent {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                // SAFETY: the handle was opened by find and is closed once.
                let _ = unsafe { CloseHandle(h) };
            }
        }
    }

    extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: forwards the arguments the system gave.
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    /// Creates the window and pumps it for pump_ms; the window stays, unpumped, afterwards.
    pub fn window_pump(pump_ms: u64) -> bool {
        // SAFETY: plain calls on this thread with static strings; the class is registered once per process.
        unsafe {
            let Ok(hinst) = GetModuleHandleW(PCWSTR::null()) else { return false };
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: w!("MiniOledTestee"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return false;
            }
            let made = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("MiniOledTestee"),
                w!("Tasks testee"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                320,
                120,
                None,
                None,
                Some(hinst.into()),
                None,
            );
            if made.is_err() {
                return false;
            }
            let end = Instant::now().checked_add(Duration::from_millis(pump_ms)).unwrap_or_else(Instant::now);
            while Instant::now() < end {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        true
    }
}

#[cfg(windows)]
fn run(args: Args) -> i32 {
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.secs);
    let parent = os::Parent::find();
    let stop = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    let mut child = None;
    let mut keep: Vec<u8> = Vec::new();
    match args.mode {
        Mode::Busy(n) => {
            for _ in 0..n {
                let stop = Arc::clone(&stop);
                workers.push(std::thread::spawn(move || {
                    let mut counter = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        for _ in 0..10_000 {
                            counter = std::hint::black_box(counter.wrapping_add(1));
                        }
                    }
                    counter
                }));
            }
        }
        Mode::Alloc(mib) => {
            keep = vec![0u8; (mib as usize) << 20];
            for i in (0..keep.len()).step_by(4096) {
                keep[i] = 1;
            }
            std::hint::black_box(&keep);
        }
        Mode::HangAfter(ms) => {
            // The window never pumps again after this: Windows calls it hung after 5 s.
            if !os::window_pump(ms.min(args.secs * 1000)) {
                return 3;
            }
        }
        Mode::Tree(depth) => {
            let left = args.secs.saturating_sub(start.elapsed().as_secs());
            if let (Some(a), Ok(exe)) = (child_args(depth, left), std::env::current_exe()) {
                child = std::process::Command::new(exe).args(a).creation_flags(CREATE_NO_WINDOW).spawn().ok();
            }
        }
    }
    while Instant::now() < deadline && !parent.gone() {
        std::thread::sleep(Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())));
    }
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        let _ = w.join();
    }
    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }
    drop(keep);
    0
}

#[cfg(not(windows))]
fn run(_: Args) -> i32 {
    2
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("testee: {e:?}");
            std::process::exit(2)
        }
    };
    std::process::exit(run(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn hang_after_is_capped() {
        let want = Ok(Args { mode: Mode::HangAfter(MAX_SECS * 1000), secs: 60 });
        assert_eq!(parse_args(&a("--hang-after 18446744073709551615")), want);
    }

    #[test]
    fn every_mode_parses() {
        assert_eq!(parse_args(&a("--busy 4")), Ok(Args { mode: Mode::Busy(4), secs: 60 }));
        assert_eq!(parse_args(&a("--alloc 512 --secs 20")), Ok(Args { mode: Mode::Alloc(512), secs: 20 }));
        assert_eq!(parse_args(&a("--secs 5 --hang-after 5000")), Ok(Args { mode: Mode::HangAfter(5000), secs: 5 }));
        assert_eq!(parse_args(&a("--tree 3")), Ok(Args { mode: Mode::Tree(3), secs: 60 }));
        assert_eq!(parse_args(&a("--tree 0")), Ok(Args { mode: Mode::Tree(0), secs: 60 }));
    }

    #[test]
    fn seconds_are_capped_at_300() {
        assert_eq!(parse_args(&a("--busy 1 --secs 300")).map(|x| x.secs), Ok(300));
        assert_eq!(parse_args(&a("--busy 1 --secs 301")).map(|x| x.secs), Ok(300));
        assert_eq!(parse_args(&a("--busy 1 --secs 99999999999")).map(|x| x.secs), Ok(300));
    }

    #[test]
    fn bad_arguments_are_errors() {
        assert_eq!(parse_args(&a("")), Err(ArgError::NoMode));
        assert_eq!(parse_args(&a("--secs 5")), Err(ArgError::NoMode));
        assert_eq!(parse_args(&a("--busy")), Err(ArgError::MissingValue("--busy".into())));
        assert_eq!(parse_args(&a("--busy 2 --secs")), Err(ArgError::MissingValue("--secs".into())));
        assert_eq!(parse_args(&a("--alloc x")), Err(ArgError::BadValue("--alloc".into())));
        assert_eq!(parse_args(&a("--busy 0")), Err(ArgError::BadValue("0".into())));
        assert_eq!(parse_args(&a("--busy 1 --tree 1")), Err(ArgError::TwoModes));
        assert_eq!(parse_args(&a("--spin 1")), Err(ArgError::Unknown("--spin".into())));
        assert_eq!(parse_args(&a("--hang-after -1")), Err(ArgError::BadValue("--hang-after".into())));
    }

    #[test]
    fn tree_children_count_down() {
        assert_eq!(child_args(3, 40), Some(a("--tree 2 --secs 40")));
        assert_eq!(child_args(1, 0), Some(a("--tree 0 --secs 1")));
        assert_eq!(child_args(0, 40), None);
    }
}
