//! Workers: a child process with frames over stdio (Bound) or a pipe (Detached) (F-P4).
//!
//! **Bound**: `bound::spawn_in_job` (no window, suspended until it is in the kill-on-close job, inheriting only its
//! pipes); frames go in on the child's stdin, written by a `worker-tx` thread (so a child that stops reading holds that
//! thread, not the caller), and come out on its stdout, read by a `worker-rx` thread into a channel of `QUEUE` frames (a
//! child that floods blocks on its own output). The child's stdout starts with `HELLO`, after at most
//! `NOISE_MAX` bytes of whatever a runtime prints first; `spawn` waits for it within `deadlines.start`, so a child that
//! is no worker host is told apart. A Bound worker ends with its `Worker` (drop kills it) and with the process that
//! spawned it (the job).
//!
//! **Detached**: a breakaway process with no stdio that serves the pipe `mini-oled-dock.<app>.<role>.<session>`; it
//! outlives its spawner, and `reconnect` reaches it again (`detached.rs`).
//!
//! **Breaker**: per (app, role) in this process, a failed start and an unexpected end of a worker each count as one
//! failure; at the spec's limit within its window `spawn` answers `BreakerOpen` until the oldest counted failure is a
//! window old.
//!
//! `deadlines.request` bounds every receive (a longer timeout is cut to it) and every Bound send (a frame the child has
//! not taken by then is cut, so the child is killed and `send` answers `Deadline`); `deadlines.heartbeat` belongs to
//! the protocol on top (this layer sends none).

pub mod bound;
pub mod breaker;
pub mod detached;

pub use breaker::BreakerState;

use std::io::{PipeWriter, Read, Write};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, GetProcessId, TerminateProcess, WaitForSingleObject};

use crate::pipe::{Conn, FrameReader, PipeError};

/// The first bytes of a Bound worker's stdout (a NUL first: no text a runtime prints matches it).
pub(crate) const HELLO: [u8; 8] = *b"\0oled-wk";
/// The most bytes a Bound worker may write before `HELLO` (a test runner's banner, say).
pub(crate) const NOISE_MAX: usize = 4096;
/// Frames a Bound worker's reader holds before the child blocks on its output.
const QUEUE: usize = 4;
/// How long a worker whose stream ended gets to exit, for its exit code.
const EXIT_GRACE: Duration = Duration::from_millis(200);
/// How long `kill` waits for the end of a Detached worker.
const KILL_WAIT_MS: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerMode {
    Bound,
    Detached,
}
#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    pub start: std::time::Duration,
    pub request: std::time::Duration,
    pub heartbeat: Option<std::time::Duration>,
}
#[derive(Clone, Copy, Debug)]
pub struct Breaker {
    pub max_restarts: u8,
    pub window: std::time::Duration,
}
impl Breaker {
    pub const DEFAULT: Breaker = Breaker { max_restarts: 3, window: std::time::Duration::from_secs(60) };
}
#[derive(Clone, Debug)]
pub struct WorkerSpec {
    pub app: &'static str,
    pub role: &'static str,
    pub exe: std::path::PathBuf,
    pub args: Vec<String>,
    pub mode: WorkerMode,
    pub deadlines: Deadlines,
    pub breaker: Breaker,
    pub frame_max: usize,
}
#[derive(Debug)]
pub enum WorkerError {
    Untrusted(String),
    Spawn(std::io::Error),
    Pipe(super::pipe::PipeError),
    Deadline,
    BreakerOpen,
    Exited(Option<u32>),
}

impl From<PipeError> for WorkerError {
    fn from(e: PipeError) -> Self {
        WorkerError::Pipe(e)
    }
}

/// One event of a Bound worker's output.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Hello,
    Frame(Vec<u8>),
}

/// The decoder of a Bound worker's stdout: at most `NOISE_MAX` bytes of anything, `HELLO`, then u32-length frames of
/// at most `cap` bytes. Total: any bytes, cut anywhere, give events, an error or "not yet"; it holds at most the noise
/// window and one frame of at most `cap` bytes.
pub(crate) struct BoundStream {
    cap: usize,
    hello: bool,
    noise: Vec<u8>,
    frames: FrameReader,
}

impl BoundStream {
    pub(crate) fn new(cap: usize) -> BoundStream {
        BoundStream { cap, hello: false, noise: Vec::new(), frames: FrameReader::default() }
    }

    /// Takes the next bytes the child wrote; the events they complete go to `out`. Err: no `HELLO` within the noise
    /// window, or a frame over the cap; the stream is then not read again.
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<Event>) -> Result<(), PipeError> {
        let mut rest = bytes;
        let after_hello;
        if !self.hello {
            let window = NOISE_MAX + HELLO.len();
            let take = rest.len().min(window.saturating_sub(self.noise.len()));
            self.noise.extend_from_slice(rest.get(..take).unwrap_or_default());
            rest = rest.get(take..).unwrap_or_default();
            let Some(at) = self.noise.windows(HELLO.len()).position(|w| w == HELLO) else {
                return if self.noise.len() >= window {
                    Err(PipeError::Peer("no worker hello in the child's output".into()))
                } else {
                    Ok(())
                };
            };
            self.hello = true;
            out.push(Event::Hello);
            let mut tail = self.noise.split_off(at + HELLO.len());
            self.noise = Vec::new();
            tail.extend_from_slice(rest);
            after_hello = tail;
            rest = &after_hello;
        }
        while !rest.is_empty() {
            let space = self.frames.space();
            let n = space.len().min(rest.len());
            if n == 0 {
                break;
            }
            for (d, s) in space.iter_mut().zip(rest) {
                *d = *s;
            }
            rest = rest.get(n..).unwrap_or_default();
            if let Some(frame) = self.frames.advance(n, self.cap)? {
                out.push(Event::Frame(frame));
            }
        }
        Ok(())
    }

    /// The bytes held now: the noise window and the frame being read.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.noise.capacity() + self.frames.held()
    }
}

/// A frame as both ends of a Bound worker write it on stdio: the u32 little-endian length, then the bytes.
fn write_frame(w: &mut impl Write, frame: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(frame.len()).map_err(|_| std::io::Error::other("frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(frame)?;
    w.flush()
}

pub struct Worker {
    inner: Inner,
    app: &'static str,
    role: &'static str,
    breaker: Breaker,
    frame_max: usize,
    request: Duration,
    /// Set once an unexpected end was counted, or by `kill` (the end it causes is no failure): nothing is counted twice.
    counted: bool,
    /// Set by `kill`: the drop of a Bound worker has nothing left to end.
    killed: bool,
}

enum Inner {
    Bound { child: bound::JobChild, pid: u32, stdin: Option<Writer>, rx: Receiver<Result<Event, PipeError>> },
    Detached { process: OwnedHandle, pid: u32, conn: Conn },
}

impl Worker {
    pub fn pid(&self) -> u32 {
        match &self.inner {
            Inner::Bound { pid, .. } | Inner::Detached { pid, .. } => *pid,
        }
    }
    pub fn process(&self) -> windows::Win32::Foundation::HANDLE {
        match &self.inner {
            Inner::Bound { child, .. } => HANDLE(child.as_raw_handle()),
            Inner::Detached { process, .. } => HANDLE(process.as_raw_handle()),
        }
    } // waitable
    pub fn send(&mut self, frame: &[u8]) -> Result<(), WorkerError> {
        if frame.len() > self.frame_max {
            return Err(WorkerError::Pipe(PipeError::TooLarge(frame.len())));
        }
        let sent = match &mut self.inner {
            Inner::Bound { stdin: Some(w), .. } => w.write(frame, self.request).map(|written| {
                written.map_err(|e| {
                    let gone = e.kind() == std::io::ErrorKind::BrokenPipe;
                    WorkerError::Pipe(if gone { PipeError::Closed } else { PipeError::Io(e) })
                })
            }),
            Inner::Bound { stdin: None, .. } => Some(Err(WorkerError::Pipe(PipeError::Closed))),
            Inner::Detached { conn, .. } => Some(conn.send(frame).map_err(WorkerError::Pipe)),
        };
        let Some(sent) = sent else {
            // The child has not taken the whole frame within `deadlines.request`: its input is cut mid-frame, so the
            // worker ends here (the spawner's choice: not counted against the breaker).
            self.kill();
            return Err(WorkerError::Deadline);
        };
        sent.map_err(|e| self.ended(e))
    }
    pub fn recv(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, WorkerError> {
        let wait = timeout.min(self.request);
        let got = match &mut self.inner {
            Inner::Bound { rx, .. } => match rx.recv_timeout(wait) {
                Ok(Ok(Event::Frame(frame))) => Ok(frame),
                Ok(Ok(Event::Hello)) => Err(WorkerError::Pipe(PipeError::Peer("a second hello".into()))),
                Ok(Err(e)) => Err(WorkerError::Pipe(e)),
                Err(RecvTimeoutError::Timeout) => Err(WorkerError::Deadline),
                Err(RecvTimeoutError::Disconnected) => Err(WorkerError::Pipe(PipeError::Closed)),
            },
            Inner::Detached { conn, .. } => match conn.recv(self.frame_max, Some(wait)) {
                Ok(frame) => Ok(frame),
                Err(PipeError::Timeout) => Err(WorkerError::Deadline),
                Err(e) => Err(WorkerError::Pipe(e)),
            },
        };
        got.map_err(|e| self.ended(e))
    }
    pub fn kill(&mut self) {
        self.counted = true;
        self.killed = true;
        match &mut self.inner {
            Inner::Bound { child, stdin, .. } => {
                *stdin = None;
                let _ = child.kill();
                let _ = child.wait();
            }
            Inner::Detached { process, .. } => {
                let h = HANDLE(process.as_raw_handle());
                // SAFETY: the process handle is valid while `process` lives; an ended process refuses, harmlessly.
                let _ = unsafe { TerminateProcess(h, 1) };
                // SAFETY: as above; waiting has no other effect.
                let _ = unsafe { WaitForSingleObject(h, KILL_WAIT_MS) };
            }
        }
    }
    pub fn try_wait(&mut self) -> Option<u32> {
        exit_code(self.process(), 0)
    }

    /// A send or receive failed. A worker that has ended (or a Bound one whose output closed) gives `Exited` with its
    /// code when it has one within a short grace, counted once against the breaker; other errors pass unchanged.
    fn ended(&mut self, e: WorkerError) -> WorkerError {
        let closed = matches!(e, WorkerError::Pipe(PipeError::Closed));
        let grace = if closed { EXIT_GRACE.as_millis() as u32 } else { 0 };
        let code = exit_code(self.process(), grace);
        let bound = matches!(self.inner, Inner::Bound { .. });
        if code.is_none() && !(closed && bound) {
            return e;
        }
        if !self.counted {
            self.counted = true;
            breaker_failed(self.app, self.role, self.breaker);
        }
        WorkerError::Exited(code)
    }
}

/// The `worker-tx` thread of a Bound worker: it writes each frame `send` hands it to the child's stdin and hands the
/// outcome back. Dropping it ends the thread once its write in progress ends (at once when idle; a killed child's
/// input fails), and the thread then closes the child's stdin.
struct Writer {
    frames: SyncSender<Vec<u8>>,
    done: Receiver<std::io::Result<()>>,
}

impl Writer {
    fn start(mut stdin: PipeWriter) -> Result<Writer, WorkerError> {
        // One frame in flight: `send` waits for each outcome, or kills the child, before the next.
        let (frames, todo) = mpsc::sync_channel::<Vec<u8>>(1);
        let (report, done) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("worker-tx".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                bound::thread_started();
                for frame in todo {
                    if report.send(write_frame(&mut stdin, &frame)).is_err() {
                        return;
                    }
                }
            })
            .map_err(WorkerError::Spawn)?;
        Ok(Writer { frames, done })
    }

    /// Writes `frame` on the child's stdin: the outcome, or None when the write has not ended within `deadline`.
    fn write(&self, frame: &[u8], deadline: Duration) -> Option<std::io::Result<()>> {
        let gone = || std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        if self.frames.send(frame.to_vec()).is_err() {
            return Some(Err(gone()));
        }
        match self.done.recv_timeout(deadline) {
            Ok(written) => Some(written),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(gone())),
        }
    }
}

impl Drop for Worker {
    /// A Bound worker ends with its Worker, also one counted as ended whose process still runs (a child that closed
    /// its output); a Detached one lives on (only its handles close).
    fn drop(&mut self) {
        if matches!(self.inner, Inner::Bound { .. }) && !self.killed {
            self.kill();
        }
    }
}

/// The exit code of `process` once it has ended within `ms`; None while it runs.
fn exit_code(process: HANDLE, ms: u32) -> Option<u32> {
    // SAFETY: `process` is a valid process handle of the caller's; waiting has no other effect.
    if unsafe { WaitForSingleObject(process, ms) } != WAIT_OBJECT_0 {
        return None;
    }
    let mut code = 0u32;
    // SAFETY: as above; `code` is writable.
    unsafe { GetExitCodeProcess(process, &mut code) }.ok()?;
    Some(code)
}

pub fn spawn(spec: &WorkerSpec) -> Result<Worker, WorkerError> {
    if !breaker_allows(spec) {
        return Err(WorkerError::BreakerOpen);
    }
    let started = match spec.mode {
        WorkerMode::Bound => spawn_bound(spec),
        WorkerMode::Detached => detached::spawn(spec),
    };
    if started.is_err() {
        breaker_failed(spec.app, spec.role, spec.breaker);
    }
    started
}
//   Bound: bound::spawn_in_job (suspended, kill-on-close job, inherits only its pipes), frames on stdin/stdout.
//   Detached: CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW, no stdio; then connect to mini-oled-dock.<app>.<role>.<session>
//   within deadlines.start; the server must be that pid, image == spec.exe, same session.
pub fn reconnect(spec: &WorkerSpec) -> Result<Worker, WorkerError> {
    if spec.mode != WorkerMode::Detached {
        let why = "only a detached worker can be reached again";
        return Err(WorkerError::Spawn(std::io::Error::new(std::io::ErrorKind::InvalidInput, why)));
    }
    detached::reconnect(spec)
} // Detached only: an already running host of that role

/// A Worker around its parts, with the spec's limits.
fn worker(spec: &WorkerSpec, inner: Inner) -> Worker {
    Worker {
        inner,
        app: spec.app,
        role: spec.role,
        breaker: spec.breaker,
        frame_max: spec.frame_max.min(crate::pipe::FRAME_MAX),
        request: spec.deadlines.request,
        counted: false,
        killed: false,
    }
}

/// Bound: the child in the job with stdin and stdout piped, its output read by a `worker-rx` thread; Ok once its
/// hello arrived within `deadlines.start`, else the child is killed.
fn spawn_bound(spec: &WorkerSpec) -> Result<Worker, WorkerError> {
    let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    let mut child = bound::spawn_in_job(&spec.exe, &args, true).map_err(WorkerError::Spawn)?;
    // SAFETY: the process handle is valid while `child` lives.
    let pid = unsafe { GetProcessId(HANDLE(child.as_raw_handle())) };
    let (input, stdout) = (child.stdin.take(), child.stdout.take());
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let cap = spec.frame_max.min(crate::pipe::FRAME_MAX);
    let mut w = worker(spec, Inner::Bound { child, pid, stdin: None, rx });
    let reader = match stdout {
        Some(out) => std::thread::Builder::new()
            .name("worker-rx".into())
            .stack_size(64 * 1024)
            .spawn(move || read_bound(out, cap, tx))
            .map_err(WorkerError::Spawn),
        None => Err(WorkerError::Pipe(PipeError::Closed)),
    };
    if let Err(e) = reader {
        w.kill();
        return Err(e);
    }
    let writer = match input.map(Writer::start).transpose() {
        Ok(writer) => writer,
        Err(e) => {
            w.kill();
            return Err(e);
        }
    };
    if let Inner::Bound { stdin, .. } = &mut w.inner {
        *stdin = writer;
    }
    let Inner::Bound { rx, .. } = &w.inner else { return Err(WorkerError::Deadline) };
    let hello = rx.recv_timeout(spec.deadlines.start);
    let refused = match hello {
        Ok(Ok(Event::Hello)) => return Ok(w),
        Err(RecvTimeoutError::Timeout) => WorkerError::Deadline,
        Ok(Ok(Event::Frame(_))) => WorkerError::Pipe(PipeError::Peer("a frame before the hello".into())),
        Ok(Err(PipeError::Closed)) | Err(RecvTimeoutError::Disconnected) => {
            WorkerError::Exited(exit_code(w.process(), EXIT_GRACE.as_millis() as u32))
        }
        Ok(Err(e)) => WorkerError::Pipe(e),
    };
    w.kill();
    Err(refused)
}

/// The `worker-rx` thread of a Bound worker: decodes the child's stdout into `tx` until the output ends, the stream
/// is refused, or the Worker is gone.
fn read_bound(mut out: std::io::PipeReader, cap: usize, tx: SyncSender<Result<Event, PipeError>>) {
    bound::thread_started();
    let mut stream = BoundStream::new(cap);
    let mut buf = vec![0u8; 64 * 1024];
    let mut events = Vec::new();
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) => 0,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
            Err(e) => {
                let _ = tx.send(Err(PipeError::Io(e)));
                return;
            }
        };
        if n == 0 {
            let _ = tx.send(Err(PipeError::Closed));
            return;
        }
        let fed = stream.feed(buf.get(..n).unwrap_or_default(), &mut events);
        for event in events.drain(..) {
            if tx.send(Ok(event)).is_err() {
                return;
            }
        }
        if let Err(e) = fed {
            let _ = tx.send(Err(e));
            return;
        }
    }
}

/// The breakers of this process, by (app, role).
static BREAKERS: Mutex<Vec<(&str, &str, BreakerState)>> = Mutex::new(Vec::new());

fn breaker_allows(spec: &WorkerSpec) -> bool {
    let now = Instant::now();
    let all = BREAKERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    all.iter().find(|(a, r, _)| *a == spec.app && *r == spec.role).is_none_or(|(_, _, b)| b.allow(now))
}

fn breaker_failed(app: &'static str, role: &'static str, breaker: Breaker) {
    let now = Instant::now();
    let mut all = BREAKERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match all.iter_mut().find(|(a, r, _)| *a == app && *r == role) {
        Some((_, _, b)) => b.failed(now),
        None => {
            let mut b = BreakerState::new(breaker);
            b.failed(now);
            all.push((app, role, b));
        }
    }
}

pub mod host {
    // the worker process's side
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::time::Instant;

    use super::{WorkerError, HELLO};
    use crate::pipe::{Conn, FrameReader, PipeError};

    pub struct Host {
        inner: Inner,
    }

    enum Inner {
        /// Frames on stdin and stdout; the spawner is the only client.
        Bound { frames: FrameReader },
        /// The pipe of (app, role) in this session, one client at a time, each of `client_image`.
        Detached { conn: Conn, client_image: PathBuf, connected: bool },
    }

    /// A Bound host: says `HELLO` on stdout, then reads frames from stdin and writes them to stdout.
    pub fn bound() -> Host {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&HELLO).and_then(|()| out.flush());
        Host { inner: Inner::Bound { frames: FrameReader::default() } }
    }
    /// A Detached host: serves `mini-oled-dock.<app>.<role>.<session>` (DACL: SYSTEM and this process's user).
    /// `Pipe(Squatted)`: another process holds the name; the host's main then exits with code 3.
    pub fn detached(app: &str, role: &str, client_image: &std::path::Path) -> Result<Host, super::WorkerError> {
        let conn = super::detached::serve(app, role)?;
        Ok(Host { inner: Inner::Detached { conn, client_image: client_image.to_path_buf(), connected: false } })
    }
    impl Host {
        pub fn recv(&mut self, cap: usize) -> Result<Option<Vec<u8>>, super::WorkerError> {
            match &mut self.inner {
                Inner::Bound { frames } => {
                    let mut input = std::io::stdin().lock();
                    loop {
                        let n = match input.read(frames.space()) {
                            Ok(0) => return Ok(None),
                            Ok(n) => n,
                            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(None),
                            Err(e) => return Err(WorkerError::Pipe(PipeError::Io(e))),
                        };
                        if let Some(frame) = frames.advance(n, cap)? {
                            return Ok(Some(frame));
                        }
                    }
                }
                Inner::Detached { conn, connected, .. } => {
                    if !*connected {
                        return Ok(None);
                    }
                    let got = conn.recv(cap, None);
                    if got.is_err() {
                        conn.disconnect();
                        *connected = false;
                    }
                    match got {
                        Ok(frame) => Ok(Some(frame)),
                        Err(PipeError::Closed) => Ok(None),
                        Err(e) => Err(WorkerError::Pipe(e)),
                    }
                }
            }
        } // None: client gone (Detached: wait for the next)
        pub fn send(&mut self, frame: &[u8]) -> Result<(), super::WorkerError> {
            match &mut self.inner {
                Inner::Bound { .. } => super::write_frame(&mut std::io::stdout().lock(), frame)
                    .map_err(|e| WorkerError::Pipe(PipeError::Io(e))),
                Inner::Detached { conn, connected: true, .. } => conn.send(frame).map_err(WorkerError::Pipe),
                Inner::Detached { .. } => Err(WorkerError::Pipe(PipeError::Closed)),
            }
        }
        /// Detached: waits for the next client and admits it only if it runs `client_image` in this session at no
        /// lower integrity than this host (any other is dropped and the wait goes on); `Deadline` when none came in
        /// time. Bound: Ok at once.
        pub fn wait_client(&mut self, timeout: std::time::Duration) -> Result<(), super::WorkerError> {
            let Inner::Detached { conn, client_image, connected } = &mut self.inner else { return Ok(()) };
            if *connected {
                return Ok(());
            }
            let deadline = Instant::now().checked_add(timeout);
            let rule = super::detached::client_rule(client_image, crate::pipe::current_integrity()?);
            loop {
                match conn.wait_client(deadline) {
                    Ok(()) => {}
                    Err(PipeError::Timeout) => return Err(WorkerError::Deadline),
                    // A client that came and went before it was seen.
                    Err(PipeError::Closed) => {
                        conn.disconnect();
                        continue;
                    }
                    Err(e) => return Err(WorkerError::Pipe(e)),
                }
                if conn.client().and_then(|peer| Conn::check(&peer, &rule)).is_ok() {
                    *connected = true;
                    return Ok(());
                }
                conn.disconnect();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::tests::{random_cap, random_frames, reference, Rng};

    /// Noise of up to a bit more than the window (sometimes holding a broken hello), maybe the hello, then frames.
    fn random_output(rng: &mut Rng, cap: usize) -> (Vec<u8>, Option<usize>) {
        let noise_len = match rng.below(4) {
            0 => 0,
            1 => rng.below(64),
            2 => NOISE_MAX - rng.below(16),
            _ => rng.below(NOISE_MAX + 64),
        };
        let mut s: Vec<u8> = (0..noise_len).map(|_| b"\nrunning 1 test\0oled-w"[rng.below(22)]).collect();
        if rng.below(8) == 0 {
            return (s, None);
        }
        let hello_at = s.len();
        s.extend_from_slice(&HELLO);
        s.extend(random_frames(rng, cap));
        (s, Some(hello_at))
    }

    /// The plain rule: the first hello, if it starts within the noise window, then the frames that follow it.
    fn expected(s: &[u8], cap: usize) -> (bool, Vec<Vec<u8>>, bool) {
        let window = NOISE_MAX + HELLO.len();
        let first = s.windows(HELLO.len()).position(|w| w == HELLO).filter(|&at| at + HELLO.len() <= window);
        let Some(at) = first else { return (false, Vec::new(), s.len() >= window) };
        let (frames, too_large) = reference(&s[at + HELLO.len()..], cap);
        (true, frames, too_large)
    }

    fn fuzz_bound(cases: u32, seed: u64) {
        let mut rng = Rng(seed);
        for _ in 0..cases {
            let cap = random_cap(&mut rng);
            let (s, _) = random_output(&mut rng, cap);
            let mut stream = BoundStream::new(cap);
            let (mut hello, mut frames, mut refused) = (false, Vec::new(), false);
            let mut at = 0;
            while at < s.len() && !refused {
                let n = (1 + rng.below(6000)).min(s.len() - at);
                let mut events = Vec::new();
                refused = stream.feed(&s[at..at + n], &mut events).is_err();
                at += n;
                for e in events {
                    match e {
                        Event::Hello => {
                            assert!(!hello, "one hello");
                            hello = true;
                        }
                        Event::Frame(f) => frames.push(f),
                    }
                }
                assert!(stream.held() <= 2 * (NOISE_MAX + HELLO.len()) + cap, "held {}", stream.held());
            }
            assert_eq!((hello, frames, refused), expected(&s, cap));
        }
    }

    #[test]
    fn fuzz_worker_frames_10k() {
        fuzz_bound(10_000, 3);
    }

    #[test]
    #[ignore = "nightly: 1,000,000 inputs"]
    fn fuzz_worker_frames_1m() {
        fuzz_bound(1_000_000, 4);
    }

    #[test]
    fn a_banner_before_the_hello_is_skipped() {
        let mut stream = BoundStream::new(16);
        let mut out = Vec::new();
        stream.feed(b"\nrunning 1 test\n", &mut out).unwrap();
        stream.feed(&HELLO[..3], &mut out).unwrap();
        assert!(out.is_empty());
        let mut rest = HELLO[3..].to_vec();
        rest.extend_from_slice(&[2, 0, 0, 0, b'o', b'k', 1]);
        stream.feed(&rest, &mut out).unwrap();
        assert_eq!(out, [Event::Hello, Event::Frame(b"ok".to_vec())]);
        let mut late = BoundStream::new(16);
        let err = late.feed(&vec![b'x'; NOISE_MAX + HELLO.len()], &mut Vec::new());
        assert!(matches!(err, Err(PipeError::Peer(_))), "no hello within the window");
    }
}
