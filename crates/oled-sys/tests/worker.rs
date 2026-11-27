//! WK3: Bound and Detached workers, deadlines and the breaker (F-P4). The worker is this test exe itself, run with
//! OLED_WORKER_TEST=<mode> and `--exact worker_entry --ignored`: `worker_entry` (ignored) is its main. Every test
//! has its own role, so its own pipe name; a Detached host that nobody calls for 30 s ends itself.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use oled_sys::pipe::{self, PipeError, PipeName};
use oled_sys::worker::{self, host, Breaker, Deadlines, Worker, WorkerError, WorkerMode, WorkerSpec};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL, WAIT_OBJECT_0};
use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, SecurityImpersonation, SetTokenInformation, TokenIntegrityLevel, TokenPrimary,
    PSID, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, TerminateProcess,
    WaitForSingleObject, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
};

const APP: &str = "oled-test";
const CAP: usize = 64 * 1024;
const WAIT: Duration = Duration::from_secs(10);
/// `CREATE_NO_WINDOW` (winbase.h): every process a test starts carries it (lint `every_spawned_process_has_no_window`).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The environment belongs to the process: one spawn at a time sets the child's mode, role and marker.
static SPAWN_ENV: Mutex<()> = Mutex::new(());

/// A role no other test uses.
fn role() -> &'static str {
    Box::leak(format!("t-{}", &pipe::nonce()[..20]).into_boxed_str())
}

fn spec(mode: WorkerMode, role: &'static str, start: Duration) -> WorkerSpec {
    WorkerSpec {
        app: APP,
        role,
        exe: std::env::current_exe().unwrap(),
        args: ["--exact", "worker_entry", "--ignored", "--test-threads=1"].map(String::from).to_vec(),
        mode,
        deadlines: Deadlines { start, request: WAIT, heartbeat: None },
        breaker: Breaker::DEFAULT,
        frame_max: CAP,
    }
}

/// `worker::spawn` with the child's mode (and a marker file, for the modes that write one) in its environment.
fn spawn_as(mode: &str, spec: &WorkerSpec, marker: Option<&std::path::Path>) -> Result<Worker, WorkerError> {
    spawn_timed(mode, spec, marker).0
}

/// `spawn_as`, with the time `worker::spawn` itself took (not the wait for another test's spawn).
fn spawn_timed(
    mode: &str,
    spec: &WorkerSpec,
    marker: Option<&std::path::Path>,
) -> (Result<Worker, WorkerError>, Duration) {
    let _env = SPAWN_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::set_var("OLED_WORKER_TEST", mode);
    std::env::set_var("OLED_WORKER_ROLE", spec.role);
    std::env::set_var("OLED_WORKER_MARKER", marker.map(|m| m.as_os_str().to_owned()).unwrap_or_default());
    let t = Instant::now();
    let spawned = worker::spawn(spec);
    (spawned, t.elapsed())
}

/// True when process `pid` has ended (or is gone) within `ms`.
fn ends_within(pid: u32, ms: u32) -> bool {
    // SAFETY: plain open of a process by id; the handle is used once and not closed (the test process ends soon).
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        // SAFETY: a valid process handle; waiting has no other effect.
        Ok(h) => (unsafe { WaitForSingleObject(h, ms) }) == WAIT_OBJECT_0,
        Err(_) => true,
    }
}

/// Waits for the end of a worker's process: its exit code.
fn exit_of(w: &mut Worker, within: Duration) -> Option<u32> {
    let t = Instant::now();
    loop {
        if let Some(code) = w.try_wait() {
            return Some(code);
        }
        if t.elapsed() > within {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A folder of this test's with a copy of the test exe, removed at drop.
struct Copy {
    dir: PathBuf,
    exe: PathBuf,
}
impl Copy {
    fn new(tag: &str) -> Copy {
        let dir = std::env::temp_dir().join(format!("oled-sys-{tag}-{}", pipe::nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("copy.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        Copy { dir, exe }
    }
}
impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn bound_round_trip() {
    let s = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let mut w = spawn_as("bound-echo", &s, None).unwrap();
    for frame in [b"one".to_vec(), Vec::new(), vec![7u8; 40_000]] {
        w.send(&frame).unwrap();
        assert_eq!(w.recv(WAIT).unwrap(), frame);
    }
    assert!(matches!(w.send(&vec![0u8; CAP + 1]), Err(WorkerError::Pipe(PipeError::TooLarge(_)))));
    let pid = w.pid();
    w.kill();
    assert!(w.try_wait().is_some(), "killed: the process has ended");
    assert!(ends_within(pid, 0));
}

#[test]
fn bound_dies_with_its_job() {
    let s = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let mut w = spawn_as("bound-echo", &s, None).unwrap();
    w.send(b"alive").unwrap();
    assert_eq!(w.recv(WAIT).unwrap(), b"alive");
    let pid = w.pid();
    drop(w);
    assert!(ends_within(pid, 1000), "the child ends with its Worker");
}

#[test]
fn bound_end_of_the_child_is_exited() {
    let s = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let mut w = spawn_as("bound-echo", &s, None).unwrap();
    w.send(b"bye").unwrap();
    assert!(matches!(w.recv(WAIT), Err(WorkerError::Exited(Some(0)))));
}

/// A child that closes its output and runs on is `Exited(None)`, and still ends with its Worker.
#[test]
fn bound_with_closed_output_ends_with_its_worker() {
    let s = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let mut w = spawn_as("bound-close-out", &s, None).unwrap();
    let got = w.recv(WAIT);
    assert!(matches!(got, Err(WorkerError::Exited(None))), "{got:?}");
    let pid = w.pid();
    drop(w);
    assert!(ends_within(pid, 1000), "the child ends with its Worker");
}

/// A Bound send is bounded by `deadlines.request`: a child that never reads its input is killed at the deadline.
#[test]
fn bound_send_has_a_deadline() {
    let request = Duration::from_millis(500);
    let base = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let s = WorkerSpec { deadlines: Deadlines { request, ..base.deadlines }, frame_max: 1 << 20, ..base };
    let mut w = spawn_as("bound-no-read", &s, None).unwrap();
    let t = Instant::now();
    let sent = w.send(&vec![0u8; 1 << 20]);
    let took = t.elapsed();
    assert!(matches!(sent, Err(WorkerError::Deadline)), "{sent:?}");
    assert!(took < request + Duration::from_millis(100), "{took:?}");
    assert!(w.try_wait().is_some(), "the child was killed at the deadline");
}

#[test]
fn detached_outlives_its_spawner() {
    let s = spec(WorkerMode::Detached, role(), Duration::from_secs(20));
    let w = spawn_as("detached-echo", &s, None).unwrap();
    let pid = w.pid();
    drop(w);
    assert!(!ends_within(pid, 100), "the host lives on without its Worker");
    let t = Instant::now();
    let mut again = worker::reconnect(&WorkerSpec {
        deadlines: Deadlines { start: Duration::from_secs(1), ..s.deadlines },
        ..s.clone()
    })
    .unwrap();
    assert!(t.elapsed() < Duration::from_millis(1100));
    assert_eq!(again.pid(), pid);
    again.send(b"ping").unwrap();
    assert_eq!(again.recv(WAIT).unwrap(), b"ping");
    again.send(b"bye").unwrap();
    assert_eq!(exit_of(&mut again, WAIT), Some(0), "Bye ends the host");
}

#[test]
fn detached_refuses_a_foreign_client() {
    let r = role();
    let s = spec(WorkerMode::Detached, r, Duration::from_secs(20));
    let w = spawn_as("detached-echo", &s, None).unwrap();
    drop(w);
    let copy = Copy::new("foreign");
    let status = std::process::Command::new(&copy.exe)
        .args(["--exact", "worker_entry", "--ignored", "--test-threads=1"])
        .env("OLED_WORKER_TEST", "foreign-client")
        .env("OLED_WORKER_ROLE", r)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "the host closed the foreign client (1: it answered, 2: it never closed)");
    let mut again = worker::reconnect(&s).unwrap();
    again.send(b"still here").unwrap();
    assert_eq!(again.recv(WAIT).unwrap(), b"still here", "the host kept listening");
    again.send(b"bye").unwrap();
    assert_eq!(exit_of(&mut again, WAIT), Some(0));
}

#[test]
fn reconnect_needs_the_same_image() {
    let s = spec(WorkerMode::Detached, role(), Duration::from_secs(20));
    let w = spawn_as("detached-echo", &s, None).unwrap();
    drop(w);
    let copy = Copy::new("reconnect");
    let wrong = WorkerSpec { exe: copy.exe.clone(), ..s.clone() };
    assert!(matches!(worker::reconnect(&wrong), Err(WorkerError::Untrusted(_))), "the host runs another image");
    let bound = WorkerSpec { mode: WorkerMode::Bound, ..s.clone() };
    assert!(matches!(worker::reconnect(&bound), Err(WorkerError::Spawn(_))), "only a detached worker");
    let mut again = worker::reconnect(&s).unwrap();
    again.send(b"bye").unwrap();
    assert_eq!(exit_of(&mut again, WAIT), Some(0));
}

#[test]
fn start_deadline() {
    let copy = Copy::new("deadline");
    let marker = copy.dir.join("survived");
    let start = Duration::from_millis(500);
    let s = spec(WorkerMode::Detached, role(), start);
    let (r, took) = spawn_timed("never-connect", &s, Some(&marker));
    assert!(matches!(r, Err(WorkerError::Deadline)), "{:?}", r.err());
    assert!(took < start + Duration::from_millis(100), "{took:?}");
    // The worker writes the marker after 3 s if it still lives: it was killed at the deadline.
    std::thread::sleep(Duration::from_millis(3500));
    assert!(!marker.exists(), "the worker outlived its start deadline");
}

#[test]
fn spawn_failures_open_the_breaker() {
    let copy = Copy::new("breaker");
    let s = WorkerSpec { exe: copy.dir.join("missing.exe"), ..spec(WorkerMode::Bound, role(), WAIT) };
    for _ in 0..3 {
        assert!(matches!(worker::spawn(&s), Err(WorkerError::Spawn(_))));
    }
    assert!(matches!(worker::spawn(&s), Err(WorkerError::BreakerOpen)), "three failures within 60 s");
    let other = WorkerSpec { role: role(), ..s.clone() };
    assert!(matches!(worker::spawn(&other), Err(WorkerError::Spawn(_))), "per role");
}

/// The test exe started as a worker at Low integrity (this process's token, duplicated with the Low label), as any
/// process below the client could start it. Killed at drop.
struct LowWorker(OwnedHandle);
impl LowWorker {
    fn start(mode: &str, role: &str) -> LowWorker {
        let _env = SPAWN_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("OLED_WORKER_TEST", mode);
        std::env::set_var("OLED_WORKER_ROLE", role);
        std::env::set_var("OLED_WORKER_MARKER", "");
        let exe = std::env::current_exe().unwrap();
        let args = ["--exact", "worker_entry", "--ignored", "--test-threads=1"];
        let mut line = worker::bound::command_line(&exe, &args).unwrap();
        let app: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let (mut own, mut low) = (HANDLE::default(), HANDLE::default());
        let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY;
        // SAFETY: the pseudo-handle of this process; `own` is writable and closed below.
        unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut own) }.unwrap();
        // SAFETY: `own` is a valid token handle; `low` is writable and closed below.
        unsafe { DuplicateTokenEx(own, access, None, SecurityImpersonation, TokenPrimary, &mut low) }.unwrap();
        let mut sid = PSID::default();
        // SAFETY: a literal SID string; `sid` receives a LocalAlloc'd SID freed below.
        unsafe { ConvertStringSidToSidW(w!("S-1-16-4096"), &mut sid) }.unwrap();
        // SE_GROUP_INTEGRITY (winnt.h): the attribute of a mandatory label.
        let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: 0x20 } };
        // SAFETY: `label` and the SID it points to live for the call; the length covers both.
        let len = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(sid) };
        // SAFETY: as above; lowering the level of an own token needs no privilege.
        unsafe { SetTokenInformation(low, TokenIntegrityLevel, std::ptr::from_ref(&label).cast(), len) }.unwrap();
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: `app` and `line` are NUL-terminated and live for the call (`line` mutable); `si` and `pi` are
        // valid; no handle is inherited; the environment is this process's (with the mode and role set above).
        unsafe {
            CreateProcessAsUserW(
                Some(low),
                PCWSTR(app.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(CREATE_NO_WINDOW),
                None,
                PCWSTR::null(),
                &si,
                &mut pi,
            )
        }
        .unwrap();
        // SAFETY: each handle is closed once and not used afterwards; the SID was allocated by
        // ConvertStringSidToSidW; the process handle is owned by the result.
        unsafe {
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(low);
            let _ = CloseHandle(own);
            LocalFree(Some(HLOCAL(sid.0)));
            LowWorker(OwnedHandle::from_raw_handle(pi.hProcess.0))
        }
    }

    /// Its exit code once it has ended within `ms`.
    fn exit_code(&self, ms: u32) -> Option<u32> {
        let h = HANDLE(self.0.as_raw_handle());
        // SAFETY: a valid process handle while `self` lives; waiting has no other effect.
        if unsafe { WaitForSingleObject(h, ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: as above; `code` is writable.
        unsafe { GetExitCodeProcess(h, &mut code) }.ok()?;
        Some(code)
    }
}
impl Drop for LowWorker {
    fn drop(&mut self) {
        // SAFETY: a valid process handle while `self` lives; an ended process refuses, harmlessly.
        let _ = unsafe { TerminateProcess(HANDLE(self.0.as_raw_handle()), 1) };
    }
}

/// A process below the client that runs the same image and holds the pipe name (anyone at that level can start
/// the image, and the name is predictable) is never taken for the worker.
#[test]
fn reconnect_refuses_a_lower_integrity_host() {
    let r = role();
    let s = spec(WorkerMode::Detached, r, Duration::from_secs(5));
    let low = LowWorker::start("detached-echo", r);
    match worker::reconnect(&s) {
        Err(WorkerError::Untrusted(why)) => assert!(why.contains("integrity"), "{why}"),
        Err(WorkerError::Deadline) => match low.exit_code(1000) {
            Some(code) => println!("skipped: the Low worker could not serve the pipe (exit {code})"),
            None => panic!("the Low worker runs but never served the pipe"),
        },
        Ok(mut w) => {
            let _ = w.send(b"bye");
            panic!("a host below the client was accepted (pid {})", w.pid());
        }
        Err(e) => panic!("{e:?}"),
    }
}

/// The echo loop of the test hosts: each frame back, until "bye" (0) or the client's end (1).
fn echo(h: &mut host::Host) -> i32 {
    loop {
        match h.recv(CAP) {
            Ok(Some(f)) if f == b"bye" => return 0,
            Ok(Some(f)) => {
                if h.send(&f).is_err() {
                    return 1;
                }
            }
            Ok(None) | Err(_) => return 1,
        }
    }
}

/// The worker side: does nothing unless this process was started as a test worker.
#[test]
#[ignore = "the main of the worker processes the other tests start"]
fn worker_entry() {
    let Ok(mode) = std::env::var("OLED_WORKER_TEST") else { return };
    let role: &'static str = Box::leak(std::env::var("OLED_WORKER_ROLE").unwrap_or_default().into_boxed_str());
    let code = match mode.as_str() {
        "bound-echo" => echo(&mut host::bound()),
        "detached-echo" => match host::detached(APP, role, &std::env::current_exe().unwrap()) {
            Ok(mut h) => loop {
                if h.wait_client(Duration::from_secs(30)).is_err() {
                    break 4;
                }
                if echo(&mut h) == 0 {
                    break 0;
                }
            },
            Err(WorkerError::Pipe(PipeError::Squatted)) => 3,
            Err(_) => 5,
        },
        "bound-close-out" => {
            let _host = host::bound();
            // SAFETY: this process's stdout handle, closed once; nothing writes to it afterwards.
            let _ = unsafe { CloseHandle(HANDLE(std::io::stdout().as_raw_handle())) };
            std::thread::sleep(Duration::from_secs(20));
            0
        }
        "bound-no-read" => {
            let _host = host::bound();
            std::thread::sleep(Duration::from_secs(20));
            0
        }
        "never-connect" => {
            std::thread::sleep(Duration::from_secs(3));
            if let Some(marker) = std::env::var_os("OLED_WORKER_MARKER").filter(|m| !m.is_empty()) {
                let _ = std::fs::write(marker, b"alive");
            }
            0
        }
        "foreign-client" => {
            let session = pipe::peer_of_pid(std::process::id()).unwrap().session;
            let name = PipeName::new(APP, role, Some(&session.to_string())).unwrap();
            match pipe::connect(&name, WAIT) {
                Ok(mut conn) => {
                    let _ = conn.send(b"hello");
                    match conn.recv(CAP, Some(Duration::from_secs(5))) {
                        Err(PipeError::Closed) => 0,
                        Ok(_) => 1,
                        Err(_) => 2,
                    }
                }
                Err(_) => 6,
            }
        }
        _ => 7,
    };
    std::process::exit(code);
}

/// The handle type the worker hands out is a waitable process handle.
#[test]
fn the_process_handle_is_waitable() {
    let s = spec(WorkerMode::Bound, role(), Duration::from_secs(20));
    let mut w = spawn_as("bound-echo", &s, None).unwrap();
    let h: HANDLE = w.process();
    // SAFETY: the handle is valid while `w` lives; waiting has no other effect.
    assert_ne!(unsafe { WaitForSingleObject(h, 0) }, WAIT_OBJECT_0, "running");
    w.kill();
    // SAFETY: as above.
    assert_eq!(unsafe { WaitForSingleObject(h, 1000) }, WAIT_OBJECT_0, "ended");
}
