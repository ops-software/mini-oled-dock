//! The test client's shell lines ("shell_*", "probe-handle"): drive a ShellClient or probe an inherited handle from a
//! script (debug builds only).

use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use oled_trust::frame::files::{ShellEvt, ShellReq, SHELL};
use oled_trust::frame::{self, Assembler};
use serde_json::Value;
use windows::Win32::Foundation::{GetHandleInformation, HANDLE};
use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
use windows::Win32::System::Threading::TerminateProcess;

use crate::exit;
use crate::shell::crumbs;
use crate::shell_client::{spawn_host, HostProcess};

/// Runs one script line; 0 on success, else the exit code to end with.
pub fn run_line(line: &Value) -> i32 {
    match line.get("op").and_then(Value::as_str).unwrap_or("") {
        "probe-handle" => probe_handle(line),
        "shell_spawn" => shell_spawn(),
        "shell_menu_busy" => shell_menu_busy(line),
        _ => exit::USAGE,
    }
}

/// The hosts this process started, kept for its life (a test kills the process and watches them end).
static HOSTS: std::sync::Mutex<Vec<Direct>> = std::sync::Mutex::new(Vec::new());

/// {"op":"shell_spawn"}: starts a shell host with this process's environment, waits for Ready and prints
/// "host <pid>" on stdout.
fn shell_spawn() -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        return exit::USAGE;
    };
    match Direct::ready(&exe, &[], Duration::from_secs(10)) {
        Ok((d, pid)) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "host {pid}").and_then(|()| out.flush());
            HOSTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(d);
            exit::OK
        }
        Err(_) => exit::HELLO_FAILED,
    }
}

/// The clients this process made, kept for its life.
static CLIENTS: std::sync::Mutex<Vec<crate::shell_client::ShellClient>> = std::sync::Mutex::new(Vec::new());

/// {"op":"shell_menu_busy","file":"<path>"}: a ShellClient (this process's environment reaches the host) asks for the
/// file's menu and waits (at most 30 s) for the host's Busy on: the picked verb is being invoked. Prints
/// "busy <host pid>" on stdout and keeps the client for the life of the process.
fn shell_menu_busy(v: &Value) -> i32 {
    use crate::shell_client::{ShellClient, ShellConfig, ShellReply};
    use oled_trust::frame::files::{ItemRef, WidePath};
    let (Some(file), Ok(exe)) = (v.get("file").and_then(Value::as_str), std::env::current_exe()) else {
        return exit::USAGE;
    };
    let Ok(path) = WidePath::parse(file) else {
        return exit::USAGE;
    };
    let c = ShellClient::new(ShellConfig { exe, test: None }, std::sync::Arc::new(|| {}));
    let req = c.next_req();
    c.request(ShellReq::Menu { req, items: vec![ItemRef::Path(path)], at: (0, 0), extended: false, owner: 0 });
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        for r in c.drain() {
            match r {
                ShellReply::Evt(ShellEvt::Busy { on: true }) => {
                    let pid = c.host_pid().unwrap_or(0);
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "busy {pid}").and_then(|()| out.flush());
                    CLIENTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(c);
                    return exit::OK;
                }
                ShellReply::Fallback { .. } | ShellReply::Evt(ShellEvt::MenuDone { .. } | ShellEvt::Failed { .. }) => {
                    return exit::HELLO_FAILED;
                }
                ShellReply::Evt(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    exit::HELLO_FAILED
}

/// {"op":"probe-handle","out":<listed handle>,"check":<unlisted handle>}: writes "listed=ok unlisted=<what>" through
/// `out` (the pipe end this process inherited), where what is "pipe" when `check` names a pipe here, "other" for
/// another kind of handle and "invalid" when it names nothing.
fn probe_handle(v: &Value) -> i32 {
    let (Some(out), Some(check)) = (v.get("out").and_then(Value::as_u64), v.get("check").and_then(Value::as_u64))
    else {
        return exit::USAGE;
    };
    let check = HANDLE(check as usize as *mut c_void);
    let mut flags = 0u32;
    // SAFETY: a query on a handle value that may name nothing: it fails with ERROR_INVALID_HANDLE then.
    let what = if unsafe { GetHandleInformation(check, &mut flags) }.is_err() {
        "invalid"
    // SAFETY: a valid handle of this process (checked above); the query has no other effect.
    } else if unsafe { GetFileType(check) } == FILE_TYPE_PIPE {
        "pipe"
    } else {
        "other"
    };
    // SAFETY: `out` is the pipe end this process inherited for its report; owned (and closed) here.
    let mut f = unsafe { std::fs::File::from_raw_handle(out as usize as *mut c_void) };
    match writeln!(f, "listed=ok unlisted={what}") {
        Ok(()) => exit::OK,
        Err(_) => exit::USAGE,
    }
}

/// What the host sent, as a Direct reader sees it.
#[derive(Debug, PartialEq, Eq)]
pub enum Got {
    Hello,
    Evt(ShellEvt),
    /// The host closed its end (it exited).
    End,
}

/// A shell host spoken to frame by frame, without a ShellClient (tests and the test client's lines): its events arrive
/// on a channel read by a thread of its own. Dropping it ends the host through the kill protocol.
pub struct Direct {
    pub host: HostProcess,
    rx: mpsc::Receiver<Got>,
}

impl Direct {
    /// Starts a host with `env` on the child; nothing read yet.
    pub fn spawn(exe: &Path, env: &[(&str, &str)]) -> std::io::Result<Direct> {
        let host = spawn_host(exe, env)?;
        let reader = host.from_host.try_clone()?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("direct-reader".into()).spawn(move || read_events(reader, &tx))?;
        Ok(Direct { host, rx })
    }

    /// Starts a host and waits (at most `within`) for its Hello and Ready; the pid it reported.
    pub fn ready(exe: &Path, env: &[(&str, &str)], within: Duration) -> std::io::Result<(Direct, u32)> {
        let d = Direct::spawn(exe, env)?;
        let fail = |what: &str| std::io::Error::other(format!("no {what} from the host"));
        if d.next(within) != Some(Got::Hello) {
            return Err(fail("Hello"));
        }
        match d.next(within) {
            Some(Got::Evt(ShellEvt::Ready { pid })) => Ok((d, pid)),
            _ => Err(fail("Ready")),
        }
    }

    /// Sends one request.
    pub fn send(&mut self, r: &ShellReq) -> std::io::Result<()> {
        let mut out = Vec::new();
        frame::encode(r, &mut out).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        self.host.to_host.write_all(&out)
    }

    /// Sends raw bytes (a forged Hello).
    pub fn send_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.host.to_host.write_all(bytes)
    }

    /// The next thing the host sent, within `within`.
    pub fn next(&self, within: Duration) -> Option<Got> {
        self.rx.recv_timeout(within).ok()
    }

    /// Every event up to and including the first that `done` accepts; Err with what came when the time ran out or the
    /// host ended first.
    pub fn until(&self, within: Duration, done: impl Fn(&ShellEvt) -> bool) -> Result<Vec<ShellEvt>, Vec<ShellEvt>> {
        let end = Instant::now() + within;
        let mut got = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Got::Evt(e)) => {
                    let last = done(&e);
                    got.push(e);
                    if last {
                        return Ok(got);
                    }
                }
                Ok(Got::Hello) => {}
                Ok(Got::End) | Err(_) => return Err(got),
            }
        }
    }

    /// The host's crumbs as the client maps them.
    pub fn section(&self) -> crumbs::Section {
        self.host.crumbs.section()
    }
}

impl Drop for Direct {
    fn drop(&mut self) {
        // A test's host is ended at once: through the kill protocol when it is not busy, else cut (a test ends a busy
        // host only after its own assertions).
        let s = self.section();
        if !s.try_kill() {
            s.force_killing();
        }
        // SAFETY: the child's process handle is open while `self` lives.
        let _ = unsafe { TerminateProcess(self.host.child.handle(), 1) };
        let _ = self.host.child.wait_ms(5_000);
    }
}

/// The reader thread of a Direct: the Hello, then events, then End.
fn read_events(mut r: std::io::PipeReader, tx: &mpsc::Sender<Got>) {
    let mut asm = Assembler::new(SHELL.max_payload);
    let mut buf = vec![0u8; 64 * 1024];
    let mut hello = false;
    loop {
        loop {
            match asm.next_frame() {
                Ok(Some(f)) => {
                    let got = if hello {
                        match frame::decode::<ShellEvt>(&f) {
                            Ok((e, _)) => Got::Evt(e),
                            Err(_) => Got::End,
                        }
                    } else {
                        hello = true;
                        match frame::decode_hello(&SHELL, &f) {
                            Ok(_) => Got::Hello,
                            Err(_) => Got::End,
                        }
                    };
                    let end = got == Got::End;
                    if tx.send(got).is_err() || end {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let _ = tx.send(Got::End);
                    return;
                }
            }
        }
        match r.read(&mut buf) {
            Ok(0) | Err(_) => {
                let _ = tx.send(Got::End);
                return;
            }
            Ok(n) => asm.push(buf.get(..n).unwrap_or_default()),
        }
    }
}
