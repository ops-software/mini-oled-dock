//! The window watcher of rows a, b, e2w, g, i and c, and the safety net of rows e and f. Every 100 ms it lists the
//! visible top-level windows of this process (on any thread: the plan's EnumThreadWindows of the operation thread
//! would miss a dialog the copy engine opens on its own thread), prints "<label> <class> <w>x<h>" for each new one so
//! the tester can take a screenshot, and once `close_after` has passed posts WM_CLOSE to it (cancel). It never
//! touches a window of another process. A window still open 20 s after its first WM_CLOSE ends the probe (exit
//! code 3), so no dialog of a probe outlives it. The console window is never a dialog: conhost gives the window of a
//! console the probe started in its first client's pid (the probe's), so it is skipped by handle and by class.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW,
    SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_CLOSE, WM_NULL,
};

use crate::json::J;
use crate::util::{from_wide, ms};

/// How long a window may stay open after its first WM_CLOSE before the probe ends itself.
pub const STUCK_AFTER: Duration = Duration::from_secs(20);
const PERIOD: Duration = Duration::from_millis(100);
const REPOST_EVERY: Duration = Duration::from_secs(2);

/// What the watcher does.
pub struct Cfg {
    /// The word printed before each new window ("dialog", "window").
    pub label: &'static str,
    /// How long new windows are looked for.
    pub watch_for: Duration,
    /// How long a window stays up before WM_CLOSE (0: at once).
    pub close_after: Duration,
    /// The thread that runs PerformOperations (recorded per window).
    pub op_thread: u32,
    /// Row b: send WM_NULL with SMTO_ABORTIFHUNG and a 500 ms timeout five times to each new window.
    pub ping: bool,
    /// The run's start (every time in the record counts from it).
    pub t0: Instant,
}

/// One window the watcher saw.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub class: String,
    pub rect: (i32, i32, i32, i32),
    pub thread: u32,
    pub op_thread: bool,
    pub seen_ms: u64,
    pub close_posted_ms: Option<u64>,
    pub gone_ms: Option<u64>,
    /// Row b: for each WM_NULL, the time it took when it was answered within 500 ms.
    pub pings: Vec<Option<u64>>,
}

impl Seen {
    pub fn json(&self) -> J {
        let (l, t, r, b) = self.rect;
        J::obj()
            .set("class", self.class.as_str())
            .set("rect", vec![J::from(l), J::from(t), J::from(r - l), J::from(b - t)])
            .set("op_thread", self.op_thread)
            .set("seen_ms", self.seen_ms)
            .set("close_posted_ms", self.close_posted_ms)
            .set("gone_ms", self.gone_ms)
            .set("pings_ms", self.pings.iter().map(|p| J::from(*p)).collect::<Vec<_>>())
    }

    /// WM_NULL answered within the timeout.
    pub fn answered(&self) -> usize {
        self.pings.iter().filter(|p| p.is_some()).count()
    }
}

/// The running watcher; `finish` stops it (closing any window still open) and returns what it saw.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Vec<Seen>>>,
}

impl Watcher {
    pub fn start(cfg: Cfg) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let join = std::thread::spawn(move || run(&cfg, &flag));
        Watcher { stop, join: Some(join) }
    }

    pub fn finish(mut self) -> Vec<Seen> {
        self.stop.store(true, Ordering::SeqCst);
        self.join.take().and_then(|j| j.join().ok()).unwrap_or_default()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Scan {
    pid: u32,
    /// The window of this process's console (0 when it has none).
    console: isize,
    out: Vec<(isize, u32)>,
}

/// The classes of a console's window (classic conhost, and the hidden one of a pseudo console).
pub fn console_class(class: &str) -> bool {
    matches!(class, "ConsoleWindowClass" | "PseudoConsoleWindow")
}

fn class_of(h: HWND) -> String {
    let mut class = [0u16; 128];
    // SAFETY: the buffer is writable for its length; GetClassNameW sends no message.
    let n = unsafe { GetClassNameW(h, &mut class) };
    if n > 0 {
        from_wide(&class)
    } else {
        String::new()
    }
}

unsafe extern "system" fn scan_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    // SAFETY: lp carries the &mut Scan of visible_windows, which outlives the EnumWindows call.
    let scan = unsafe { &mut *(lp.0 as *mut Scan) };
    let mut pid = 0u32;
    // SAFETY: plain queries on a window handle EnumWindows just gave.
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: as above.
    if pid == scan.pid
        && hwnd.0 as isize != scan.console
        && unsafe { IsWindowVisible(hwnd) }.as_bool()
        && !console_class(&class_of(hwnd))
    {
        scan.out.push((hwnd.0 as isize, tid));
    }
    BOOL(1)
}

/// The visible top-level windows of process `pid`, with their threads.
pub fn visible_windows(pid: u32) -> Vec<(isize, u32)> {
    // SAFETY: no arguments; a null handle when the process has no console.
    let console = unsafe { GetConsoleWindow() }.0 as isize;
    let mut scan = Scan { pid, console, out: Vec::new() };
    // SAFETY: the callback only reads window facts and writes into scan, which lives across the call.
    let _ = unsafe { EnumWindows(Some(scan_cb), LPARAM(&mut scan as *mut Scan as isize)) };
    scan.out
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn describe(h: isize, tid: u32, cfg: &Cfg) -> Seen {
    let mut rc = RECT::default();
    // SAFETY: rc is a valid RECT.
    let _ = unsafe { GetWindowRect(hwnd(h), &mut rc) };
    Seen {
        class: class_of(hwnd(h)),
        rect: (rc.left, rc.top, rc.right, rc.bottom),
        thread: tid,
        op_thread: tid == cfg.op_thread,
        seen_ms: ms(cfg.t0),
        ..Seen::default()
    }
}

/// Five WM_NULL with SMTO_ABORTIFHUNG and 500 ms each, 100 ms apart.
fn ping(h: isize) -> Vec<Option<u64>> {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            let mut res = 0usize;
            // SAFETY: WM_NULL carries no pointers; the window belongs to this process.
            let r = unsafe {
                SendMessageTimeoutW(hwnd(h), WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 500, Some(&mut res))
            };
            let out = if r.0 != 0 { Some(ms(t)) } else { None };
            std::thread::sleep(PERIOD);
            out
        })
        .collect()
}

fn post_close(h: isize) {
    // SAFETY: WM_CLOSE to a window of this process (checked by the scan); no pointers.
    let _ = unsafe { PostMessageW(Some(hwnd(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
}

fn alive(h: isize) -> bool {
    // SAFETY: IsWindow accepts any value.
    unsafe { IsWindow(Some(hwnd(h))) }.as_bool()
}

struct Entry {
    hwnd: isize,
    seen: Seen,
    first_close: Option<Instant>,
    last_close: Option<Instant>,
}

fn run(cfg: &Cfg, stop: &AtomicBool) -> Vec<Seen> {
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    let start = Instant::now();
    let mut seen: Vec<Entry> = Vec::new();
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        if !stopping && start.elapsed() < cfg.watch_for {
            for (h, tid) in visible_windows(pid) {
                if seen.iter().any(|e| e.hwnd == h) {
                    continue;
                }
                let mut s = describe(h, tid, cfg);
                println!(
                    "{} {} {}x{} at {} ms",
                    cfg.label,
                    s.class,
                    s.rect.2 - s.rect.0,
                    s.rect.3 - s.rect.1,
                    s.seen_ms
                );
                let _ = std::io::stdout().flush();
                if cfg.ping {
                    s.pings = ping(h);
                }
                seen.push(Entry { hwnd: h, seen: s, first_close: None, last_close: None });
            }
        }
        let now = ms(cfg.t0);
        let mut open = false;
        for e in seen.iter_mut().filter(|e| e.seen.gone_ms.is_none()) {
            if !alive(e.hwnd) {
                e.seen.gone_ms = Some(now);
                continue;
            }
            open = true;
            let due = stopping || now >= e.seen.seen_ms + cfg.close_after.as_millis() as u64;
            match (due, e.first_close, e.last_close) {
                (true, None, _) => {
                    post_close(e.hwnd);
                    e.first_close = Some(Instant::now());
                    e.last_close = e.first_close;
                    e.seen.close_posted_ms = Some(now);
                }
                (true, Some(first), _) if first.elapsed() > STUCK_AFTER => {
                    println!("stuck {}: the probe ends itself", e.seen.class);
                    let _ = std::io::stdout().flush();
                    std::process::exit(3);
                }
                (true, Some(_), Some(last)) if last.elapsed() > REPOST_EVERY => {
                    post_close(e.hwnd);
                    e.last_close = Some(Instant::now());
                }
                _ => {}
            }
        }
        if stopping && !open {
            break;
        }
        if !stopping && !open && start.elapsed() >= cfg.watch_for {
            // nothing open and the watch is over: wait for the stop only
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(PERIOD);
            }
            break;
        }
        std::thread::sleep(PERIOD);
    }
    seen.into_iter().map(|e| e.seen).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_windows_are_never_dialogs() {
        assert!(console_class("ConsoleWindowClass") && console_class("PseudoConsoleWindow"));
        assert!(!console_class("#32770") && !console_class(""));
        // Read-only: lists this process's visible top-level windows (no message is sent to any window).
        // SAFETY: no arguments.
        let (pid, console) = unsafe { (GetCurrentProcessId(), GetConsoleWindow().0 as isize) };
        let mine = visible_windows(pid);
        assert!(console == 0 || mine.iter().all(|&(h, _)| h != console), "the console window is listed");
        assert!(mine.iter().all(|&(h, _)| !console_class(&class_of(hwnd(h)))));
    }
}
