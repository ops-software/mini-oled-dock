//! Asynchronous log with rotation.
//!
//! Callers (`log!`) format the line and send it to an unbounded channel: they never block.
//! A dedicated `log` thread writes it to stdout (ONLY if `enable_stdout`: the exe uses the windows
//! subsystem, so it has no console) and (append, flush on every line) to
//! `%ProgramData%\mini-oled-dock\dock.log` (admin-only folder, see `datadir`; opened without
//! following reparse points). When the file exceeds `MAX_LOG_BYTES` (2 MB) it is
//! renamed to `dock.log.1` (replacing the previous one) and a new file is opened.
//! If there is no usable data folder (or the file cannot be opened), lines go to the debugger output
//! (`OutputDebugStringW`, visible with DebugView) besides stdout. The watchdog process uses the same
//! file with the `wd` tag in its lines, the update guard with the `ug` tag; both write synchronously.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread::JoinHandle;

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// What a host exe tells the logger about itself.
pub struct LogSink {
    /// The log file; `None` = no usable data folder.
    pub path: fn() -> Option<std::path::PathBuf>,
    pub open_append: fn(&std::path::Path) -> Option<std::fs::File>,
    /// Fallback when no file can be opened.
    pub debug_out: fn(&str),
    /// The host exe's version, for "log: <who> <version>, pid <n>".
    pub version: &'static str,
    /// Called once per line by the log thread (the dock's wake probe).
    pub on_line: fn(),
}

/// The host's sink, set first thing in `start`.
static SINK: OnceLock<&'static LogSink> = OnceLock::new();

/// Log file rotation threshold.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Channel to the `log` thread; `None` = shut down. Initialized once by `start`.
static LOG_TX: OnceLock<Sender<Option<String>>> = OnceLock::new();
/// true = the `log` thread also writes to stdout (only with an attached console).
static STDOUT_ON: AtomicBool = AtomicBool::new(false);
/// Process role tag ("" for the dock, "wd" for the watchdog, "ug" for the update guard), prepended to lines.
static TAG: OnceLock<&'static str> = OnceLock::new();
/// true = synchronous mode (watchdog, update guard): no thread, each line is appended straight to the file.
static SYNC: AtomicBool = AtomicBool::new(false);

/// Enables the log copy on stdout (call only after attaching a console).
pub fn enable_stdout() {
    STDOUT_ON.store(true, Relaxed);
}

/// Buffer for `say` messages when there is no console (CLI modes launched from UAC/Explorer).
static SAY_BUF: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// From now on `say` accumulates messages instead of writing them to stdout (no console attached).
pub fn buffer_say() {
    if let Ok(mut b) = SAY_BUF.lock() {
        *b = Some(String::new());
    }
}

/// Returns and clears the messages accumulated by `say` after `buffer_say`.
pub fn take_say_buffer() -> String {
    SAY_BUF.lock().ok().and_then(|mut b| b.take()).unwrap_or_default()
}

/// Prints a user message to stdout (CLI mode); errors ignored (no console).
pub fn say(msg: &str) {
    if let Ok(mut b) = SAY_BUF.lock() {
        if let Some(buf) = b.as_mut() {
            // No console: the message is accumulated and shown in a window at the end of the command.
            buf.push_str(msg);
            buf.push('\n');
            return;
        }
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// Local wall-clock stamp "[YYYY-MM-DD hh:mm:ss.mmm]" (integer formatting only).
fn stamp(t: &SYSTEMTIME) -> String {
    format!(
        "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}]",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// "<stamp> msg", or "<stamp> [tag] msg" when the process has a role tag.
fn format_line(stamp: &str, tag: &str, msg: &str) -> String {
    if tag.is_empty() {
        format!("{stamp} {msg}")
    } else {
        format!("{stamp} [{tag}] {msg}")
    }
}

/// The stamp of the current local time (the caller's thread, before any channel).
fn now_stamp() -> String {
    // SAFETY: GetLocalTime has no preconditions.
    stamp(&unsafe { GetLocalTime() })
}

/// Path of the log file, as the sink says; `None` if the data folder is not usable. Never initializes
/// anything (safe from the panic hook).
fn log_path(sink: &LogSink) -> Option<PathBuf> {
    (sink.path)()
}

/// Formats and sends the line: `Sender::send` on an unbounded channel never blocks the caller.
/// Before `start` (or after `stop`) the line is discarded. Use the `log!` macro.
pub fn log_line(args: std::fmt::Arguments<'_>) {
    let sync = SYNC.load(Relaxed);
    if !sync && LOG_TX.get().is_none() {
        return;
    }
    let tag = TAG.get().copied().unwrap_or("");
    let line = format_line(&now_stamp(), tag, &args.to_string());
    if sync {
        // Watchdog: rare lines, so open-append-close is cheaper than a dedicated thread.
        if let Some(sink) = SINK.get() {
            append(sink, &line);
        }
    } else if let Some(tx) = LOG_TX.get() {
        // If the log thread already ended the error is ignored.
        let _ = tx.send(Some(line));
    }
}

/// `log!("...", args)`: like `format!`, but writes to the async log.
#[macro_export]
macro_rules! log {
    ($($a:tt)*) => { $crate::logger::log_line(format_args!($($a)*)) };
}

/// Writes a message to the log file (append) IMMEDIATELY and synchronously, bypassing the channel,
/// stamped like every other line. Only for the panic hook (with `panic = "abort"` the `log`
/// thread would not have time to drain).
pub fn write_sync(sink: &LogSink, msg: &str) {
    append(sink, &format_line(&now_stamp(), TAG.get().copied().unwrap_or(""), msg));
}

/// Appends an already formatted line to the log file (open, append, close).
fn append(sink: &LogSink, line: &str) {
    match log_path(sink).and_then(|p| (sink.open_append)(&p)) {
        Some(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        None => (sink.debug_out)(line),
    }
}

fn open_log_file(sink: &LogSink) -> Option<(std::fs::File, u64)> {
    let path = log_path(sink)?;
    let mut f = (sink.open_append)(&path)?;
    let mut len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len >= MAX_LOG_BYTES {
        drop(f);
        rotate(&path);
        f = (sink.open_append)(&path)?;
        len = f.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Some((f, len))
}

/// Renames `dock.log` to `dock.log.1` (the old `.1` is replaced). Delete and rename act on the names
/// themselves (never on a link target), inside the admin-only data folder.
fn rotate(path: &std::path::Path) {
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    let old = PathBuf::from(old);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(path, &old);
}

/// Starts the `log` thread. `tag` = "" for the dock, "wd" for the watchdog, "ug" for the update guard. Must be called
/// once, before any `log!`. Returns the handle to pass to `stop` (`None` in the synchronous `wd` and `ug` modes).
pub fn start(tag: &'static str, sink: &'static LogSink) -> Option<JoinHandle<()>> {
    let _ = SINK.set(sink);
    let _ = TAG.set(tag);
    if logs_synchronously(tag) {
        // The watchdog and the update guard log a handful of lines: no thread, no channel.
        SYNC.store(true, Relaxed);
        start_marker(tag, sink);
        return None;
    }
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handle = std::thread::Builder::new()
        .name("log".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut file = open_log_file(sink);
            while let Ok(Some(line)) = rx.recv() {
                (sink.on_line)();
                // If stdout is closed or slow we do not want a panic: errors ignored.
                if STDOUT_ON.load(Relaxed) {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
                let mut rotate_now = false;
                if let Some((f, len)) = file.as_mut() {
                    if writeln!(f, "{line}").is_ok() {
                        let _ = f.flush();
                        *len += line.len() as u64 + 2;
                        rotate_now = *len >= MAX_LOG_BYTES;
                    }
                } else {
                    (sink.debug_out)(&line);
                }
                if rotate_now {
                    drop(file.take()); // closes the file before renaming it
                    file = open_log_file(sink);
                }
            }
        })
        .ok()?;
    LOG_TX.set(tx).ok()?;
    start_marker(tag, sink);
    Some(handle)
}

/// true for the roles that log synchronously, each line on disk when `log!` returns (no thread, no channel): the
/// watchdog ("wd") and the update guard ("ug"), a process that may be ended at any moment.
fn logs_synchronously(tag: &str) -> bool {
    matches!(tag, "wd" | "ug")
}

/// First line of a run: uptime is a line's time minus this line's time.
fn start_marker(tag: &str, sink: &LogSink) {
    log_line(format_args!("{}", marker_text(tag, sink.version, std::process::id())));
}

/// "log: <who> <version>, pid <n>"; the dock's role has an empty tag.
fn marker_text(tag: &str, version: &str, pid: u32) -> String {
    let who = if tag.is_empty() { "dock" } else { tag };
    format!("log: {who} {version}, pid {pid}")
}

/// Flushes and shuts down the `log` thread (the last thing before exiting).
pub fn stop(handle: Option<JoinHandle<()>>) {
    if let (Some(tx), Some(h)) = (LOG_TX.get(), handle) {
        let _ = tx.send(None);
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16, ms: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: y,
            wMonth: mo,
            wDayOfWeek: 0,
            wDay: d,
            wHour: h,
            wMinute: mi,
            wSecond: s,
            wMilliseconds: ms,
        }
    }

    #[test]
    fn stamp_is_local_date_and_time() {
        assert_eq!(stamp(&st(2026, 11, 22, 9, 5, 7, 3)), "[2026-11-22 09:05:07.003]");
        assert_eq!(stamp(&st(2026, 1, 2, 23, 59, 59, 999)), "[2026-01-02 23:59:59.999]");
    }

    static DBG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    fn dbg_push(s: &str) {
        DBG.lock().unwrap().push(s.to_string());
    }
    fn temp_log() -> PathBuf {
        std::env::temp_dir().join(format!("oled-sys-log-{}", std::process::id()))
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let mut o = self.0.as_os_str().to_owned();
            o.push(".1");
            let _ = std::fs::remove_file(PathBuf::from(o));
        }
    }
    fn open_any(p: &std::path::Path) -> Option<std::fs::File> {
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    }
    static NO_FILE: LogSink =
        LogSink { path: || None, open_append: |_| None, debug_out: dbg_push, version: "0.0.0", on_line: || {} };
    static TEMP_FILE: LogSink = LogSink {
        path: || Some(temp_log()),
        open_append: open_any,
        debug_out: |_| panic!("the file was usable"),
        version: "0.0.0",
        on_line: || {},
    };

    #[test]
    fn marker_names_the_host_version() {
        assert_eq!(marker_text("", "1.6.0", 7), "log: dock 1.6.0, pid 7");
        assert_eq!(marker_text("wd", "1.6.0", 7), "log: wd 1.6.0, pid 7");
    }

    #[test]
    fn write_sync_uses_the_sink_and_falls_back_to_debug_out() {
        write_sync(&NO_FILE, "boom");
        {
            let d = DBG.lock().unwrap();
            assert_eq!(d.len(), 1);
            assert!(d[0].ends_with("boom"));
        }
        let _g = Cleanup(temp_log());
        write_sync(&TEMP_FILE, "boom");
        let text = std::fs::read_to_string(temp_log()).unwrap();
        assert!(text.trim_end().ends_with("boom"));
        assert_eq!(DBG.lock().unwrap().len(), 1, "nothing more in debug_out");
    }

    #[test]
    fn rotate_moves_the_log_to_dot_1() {
        let p = std::env::temp_dir().join(format!("oled-sys-rot-{}", std::process::id()));
        let _g = Cleanup(p.clone());
        std::fs::write(&p, "x").unwrap();
        rotate(&p);
        let mut o = p.as_os_str().to_owned();
        o.push(".1");
        assert!(!p.exists());
        assert!(PathBuf::from(o).exists());
    }

    #[test]
    fn line_keeps_the_tag() {
        assert_eq!(format_line("[s]", "", "msg"), "[s] msg");
        assert_eq!(format_line("[s]", "wd", "msg"), "[s] [wd] msg");
    }

    #[test]
    fn the_watchdog_and_the_update_guard_log_synchronously() {
        assert!(logs_synchronously("wd"));
        assert!(logs_synchronously("ug"), "a guard ended mid-update leaves its last line on disk");
        assert!(!logs_synchronously(""), "the dock logs through its log thread");
    }
}
