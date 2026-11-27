//! The medium agent (P0.7b): `dock.exe --agent <nonce>`. The elevated dock starts it through Explorer, so it runs as
//! the shell's user at medium integrity, and has it do the shell work that would otherwise run elevated: the Start
//! catalogue (`--start-apps`'s apps arena), Start's icons (`--start-icons`'s frames), the shortcuts of the user's own
//! Start menu folder, and the version strings and icon of a program a user can write.
//!
//! PIPES. The dock creates two named pipes for one agent, `mini-oled-dock.dock.agent.<nonce>` (requests, dock to agent)
//! and `mini-oled-dock.dock.agent-out.<nonce>` (replies, agent to dock): a pipe `Conn` is used by one thread at a time,
//! and each side reads and writes on two threads. The agent connects to both within 5 s and checks that their server is
//! one process, its own image (file id), in its session, at medium or high integrity; then the dock's `Hello` (proto 1,
//! the server's pid) and its own. Records are `agent_proto`'s; requests are decoded with `REQ_MAX`.
//!
//! THREADS. "agent-rx" owns the request pipe: it notes a `Cancel` at once (the worker stops a stream before its next
//! chunk), ends the process on `Bye`, and queues the other requests to the worker; on a read error, a closed pipe or a
//! malformed frame it TERMINATES the process (TerminateProcess, not ExitProcess: a worker stuck in a third-party shell
//! extension may hold the loader lock), so the agent dies with its pipe whatever its worker does. Best effort beside it,
//! "agent-watch" waits on the dock's process (when it may open it: SYNCHRONIZE can be refused across integrity levels)
//! and terminates the agent the same way. The worker (the main thread, COM STA) serves one request at a time. No
//! window, no logger, no data folder: like the dock's other children the agent writes nothing but its pipe.

use std::collections::HashMap;
use std::io::Write;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use oled_sys::pipe::{self, Conn, Integrity, Peer, PeerRule, PipeError, PipeName, Server};
use windows::core::Interface;
use windows::Win32::Foundation::{FILETIME, HANDLE, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject,
    INFINITE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows::Win32::UI::Shell::{FOLDERID_Programs, IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, SW_HIDE, WM_NULL,
};

use crate::agent_proto::{
    decode_reply, decode_request, encode_reply, encode_request, ExeInfoRec, LinkRec, Reply, Request, AGENT_PROTO,
    CHUNK_MAX, REC_MAX, REQ_MAX,
};
use crate::icons::IconBitmap;
use crate::shell_thread::Verb;

/// The app part of the agent's pipe names, and the purposes of its two pipes.
pub(crate) const PIPE_APP: &str = "dock";
pub(crate) const REQUESTS: &str = "agent";
pub(crate) const REPLIES: &str = "agent-out";
/// How long the agent waits for the dock's pipes, and then for its `Hello`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The stack of the agent's two small threads (frames are decoded into the heap).
const THREAD_STACK: usize = 64 * 1024;
/// No request cancelled: the dock's request ids start at 1.
const NO_ID: u32 = 0;
/// The `End` status of a request served, failed, or cancelled by the dock. `ListApps` ends with the exit code
/// `--start-apps` would have (0, 1, or 3 for nothing listed), as `apps_from_child` reads it.
pub(crate) const STATUS_OK: u16 = 0;
pub(crate) const STATUS_FAILED: u16 = 1;
pub(crate) const STATUS_CANCELLED: u16 = 2;
/// The longest version string kept, in UTF-16 units (`ExeInfoRec`'s cap).
const FACT_UNITS: usize = 260;

/// The name of one of the agent's pipes.
pub(crate) fn pipe_name(purpose: &str, nonce: &str) -> Result<PipeName, PipeError> {
    PipeName::new(PIPE_APP, purpose, Some(nonce))
}

/// What the agent learns of a program for `ExeInfo`: its version strings and its icon at the size asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExeFacts {
    pub(crate) description: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) company: Option<String>,
    pub(crate) icon: Option<IconBitmap>,
}

/// The work behind each request: the shell at medium (`ShellWork`), or a test's fake.
pub(crate) trait Work {
    /// The apps arena exactly as `--start-apps` prints it, with that child's exit code.
    fn apps(&mut self) -> (i32, Vec<u8>);
    /// The frames `--start-icons <px>` prints for `ids`, written to `out`: 0 when every frame was written.
    fn icons(&mut self, ids: &[Option<String>], px: u32, out: &mut dyn Write) -> i32;
    /// The shortcuts of the user's Start menu folder: (path under it, target, the shell's display name or "").
    fn links(&mut self) -> Vec<(String, String, String)>;
    /// The version strings of the program at `path` and its icon at `px` (one of `EXE_ICON_PX`).
    fn exe_info(&mut self, path: &str, px: u16) -> ExeFacts;
}

/// `--agent <nonce>`. Exit 2 when the dock's pipes cannot be reached or trusted, or its `Hello` is wrong; otherwise
/// the process ends on `Bye` (0), with its pipe (0) or on a malformed request (2), terminated by its reading thread.
pub fn run(nonce: &str) -> i32 {
    // Before COM: no window of its own yet whose message the read could take.
    end_start_feedback();
    let Ok(exe) = std::env::current_exe() else { return 2 };
    // SAFETY: the first COM call of this (main) thread, the agent's worker; the process ends without uninitializing, as
    // every other child's failure path does.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return 2;
    }
    run_with(nonce, &server_rule(exe), &mut ShellWork, terminate)
}

/// Ends the "working in background" pointer Windows shows while a GUI program the shell started (dock.exe; Explorer
/// cannot pass the STARTF_FORCEOFFFEEDBACK the dock's own children get) has not read its first message: up to 7 s at
/// every agent start, the agent having no message loop. The thread reads one message it posts to itself, then finds
/// its queue empty. True when the message was read.
fn end_start_feedback() -> bool {
    let mut msg = MSG::default();
    // SAFETY: no window (None); PM_NOREMOVE only creates this thread's message queue and writes `msg`.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: a thread message to this very thread, whose queue exists now; WM_NULL does nothing.
    if unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_NULL, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }
    // SAFETY: the queue holds the WM_NULL just posted, so GetMessageW returns at once; `msg` is writable.
    let read = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0;
    // SAFETY: as the first call; nothing is removed or waited for.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    read
}

/// What the agent's server must be: the dock, so this very image (`exe`, by file id), in this session, at medium or
/// high integrity (the dock is elevated; a medium dock, under a policy that gives no elevation, is still the user's).
pub(crate) fn server_rule(exe: PathBuf) -> PeerRule {
    PeerRule {
        image: Some(exe),
        same_session: true,
        max_integrity: Some(Integrity::High),
        min_integrity: Some(Integrity::Medium),
        pid: None,
    }
}

/// Ends this process at once with `code`, every thread with it, whatever a thread is doing.
fn terminate(code: i32) {
    // SAFETY: the pseudo-handle of this process; TerminateProcess on it does not return.
    let _ = unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
}

/// The agent over the pipes of `nonce`, with `rule` for their server, `work` behind the requests and `end` for the
/// reading thread's verdict (`terminate`; tests that run an agent on a thread pass a function that returns, and the
/// worker then ends when the reading thread has).
pub(crate) fn run_with(nonce: &str, rule: &PeerRule, work: &mut dyn Work, end: fn(i32)) -> i32 {
    let Ok((mut requests, mut replies, server)) = connect(nonce, rule) else { return 2 };
    let first = requests.recv(REQ_MAX, Some(CONNECT_TIMEOUT)).ok().and_then(|f| decode_request(&f).ok());
    let Some(hello) = handshake(first, server) else { return 2 };
    if !send(&mut replies, &hello) {
        return 2;
    }
    if server != std::process::id() {
        watch_server(server, end);
    }
    let cancelled = Arc::new(AtomicU32::new(NO_ID));
    let (tx, rx) = mpsc::channel();
    let seen = Arc::clone(&cancelled);
    let reading = std::thread::Builder::new().name("agent-rx".into()).stack_size(THREAD_STACK).spawn(move || loop {
        if let Rx::Exit(code) = rx_step(requests.recv(REQ_MAX, None), &seen, &tx) {
            return end(code);
        }
    });
    if reading.is_err() {
        return 2;
    }
    serve(&rx, &mut |r: &Reply| send(&mut replies, r), work, &cancelled);
    // The reply pipe failed (the dock went away) or the reading thread ended: nothing more to serve.
    0
}

/// Connects to the two pipes of `nonce`, then checks their server against `rule`: one process (pid and start time)
/// for both. Both are connected first, so a server refused sees both close at once instead of waiting out its accept
/// of the second. The server's pid.
fn connect(nonce: &str, rule: &PeerRule) -> Result<(Conn, Conn, u32), PipeError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let requests = pipe::connect(&pipe_name(REQUESTS, nonce)?, CONNECT_TIMEOUT)?;
    let replies = pipe::connect(&pipe_name(REPLIES, nonce)?, deadline.saturating_duration_since(Instant::now()))?;
    let server = requests.server()?;
    Conn::check(&server, rule)?;
    let other = replies.server()?;
    if (other.pid, other.created) != (server.pid, server.created) {
        return Err(PipeError::Peer("the two pipes have different servers".into()));
    }
    Ok((requests, replies, server.pid))
}

/// The agent's `Hello` when the dock's first request is its `Hello`: protocol 1 and the pid of the pipes' server.
pub(crate) fn handshake(first: Option<Request>, server: u32) -> Option<Reply> {
    (first == Some(Request::Hello { proto: AGENT_PROTO, pid: server }))
        .then(|| Reply::Hello { proto: AGENT_PROTO, pid: std::process::id() })
}

/// Best effort: a thread that waits for the dock's process to end, then `end`s the agent. Nothing when the dock's
/// process cannot be opened for waiting (another integrity level, Administrator protection): the pipe's end does it.
fn watch_server(pid: u32, end: fn(i32)) {
    // SAFETY: plain open of a process by id, for waiting only; the handle is owned below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return };
    // SAFETY: a fresh handle, owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(h.0) };
    let _ = std::thread::Builder::new().name("agent-watch".into()).stack_size(THREAD_STACK).spawn(move || {
        // SAFETY: the handle is valid while `process` lives (this closure); waiting has no other effect.
        let _ = unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), INFINITE) };
        end(0);
    });
}

/// Sends one reply: false when the pipe failed (the dock is gone). A record over a cap is not sent, and is no failure.
fn send(conn: &mut Conn, r: &Reply) -> bool {
    match encode_reply(r) {
        Ok(frame) => conn.send(&frame).is_ok(),
        Err(_) => true,
    }
}

/// What the reading thread does after one frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rx {
    Go,
    /// End the process with this code: 0 for `Bye` or a pipe that failed or closed, 2 for a malformed request.
    Exit(i32),
}

/// The reading thread's step over one frame of the request pipe (or its failure): a `Cancel` marks its id for the
/// worker at once, `Bye` ends, a second `Hello` or a frame that does not decode ends with 2, and every other request
/// goes to the worker's queue.
pub(crate) fn rx_step(frame: Result<Vec<u8>, PipeError>, cancelled: &AtomicU32, work: &Sender<Request>) -> Rx {
    let Ok(frame) = frame else { return Rx::Exit(0) };
    match decode_request(&frame) {
        Ok(Request::Cancel { id }) => {
            cancelled.store(id, SeqCst);
            Rx::Go
        }
        Ok(Request::Bye) => Rx::Exit(0),
        Ok(Request::Hello { .. }) | Err(_) => Rx::Exit(2),
        Ok(r) => {
            if work.send(r).is_ok() {
                Rx::Go
            } else {
                Rx::Exit(0)
            }
        }
    }
}

/// The worker: serves the queued requests one at a time until the queue closes (the reading thread ended) or a reply
/// cannot be sent.
pub(crate) fn serve(
    requests: &Receiver<Request>,
    send: &mut dyn FnMut(&Reply) -> bool,
    work: &mut dyn Work,
    cancelled: &AtomicU32,
) {
    while let Ok(r) = requests.recv() {
        if !serve_one(r, send, work, cancelled) {
            return;
        }
    }
}

/// One request: its records, then its `End`. False when a reply could not be sent.
fn serve_one(r: Request, send: &mut dyn FnMut(&Reply) -> bool, work: &mut dyn Work, cancelled: &AtomicU32) -> bool {
    let id = match &r {
        Request::ListApps { id }
        | Request::Icons { id, .. }
        | Request::StartLinks { id }
        | Request::ExeInfo { id, .. }
        | Request::UserJournal { id } => *id,
        // Handled by the reading thread; never queued.
        Request::Hello { .. } | Request::Cancel { .. } | Request::Bye => return true,
    };
    let status = if cancelled.load(SeqCst) == id {
        STATUS_CANCELLED
    } else {
        match r {
            Request::ListApps { .. } => {
                let (code, bytes) = work.apps();
                let mut out = ChunkWriter::new(id, send, cancelled);
                let streamed = out.write_all(&bytes).and_then(|()| out.finish());
                match out.verdict(streamed.is_ok()) {
                    Streamed::Whole => u16::try_from(code).unwrap_or(STATUS_FAILED),
                    Streamed::Cancelled => STATUS_CANCELLED,
                    Streamed::Failed => STATUS_FAILED,
                    Streamed::PipeFailed => return false,
                }
            }
            Request::Icons { px, ids, .. } => {
                let ids: Vec<Option<String>> = ids.into_iter().map(Some).collect();
                let mut out = ChunkWriter::new(id, send, cancelled);
                let code = work.icons(&ids, u32::from(px), &mut out);
                let whole = code == 0 && out.finish().is_ok();
                match out.verdict(whole) {
                    Streamed::Whole => STATUS_OK,
                    Streamed::Cancelled => STATUS_CANCELLED,
                    Streamed::Failed => STATUS_FAILED,
                    Streamed::PipeFailed => return false,
                }
            }
            Request::StartLinks { .. } => {
                let mut status = STATUS_OK;
                for (rel, target, name) in work.links() {
                    if cancelled.load(SeqCst) == id {
                        status = STATUS_CANCELLED;
                        break;
                    }
                    if !send(&Reply::Link(LinkRec { id, rel, target, name })) {
                        return false;
                    }
                }
                status
            }
            Request::ExeInfo { path, px, .. } => {
                let facts = work.exe_info(&path, px);
                if !send(&Reply::ExeInfo(exe_record(id, facts, px))) {
                    return false;
                }
                STATUS_OK
            }
            // The user journal's reader lands with J1c.
            Request::UserJournal { .. } => STATUS_FAILED,
            Request::Hello { .. } | Request::Cancel { .. } | Request::Bye => return true,
        }
    };
    send(&Reply::End { id, status })
}

/// The `ExeInfo` record of `facts`: a string over the cap is left out, and so is an icon that is not `px` x `px`.
fn exe_record(id: u32, facts: ExeFacts, px: u16) -> ExeInfoRec {
    let fit = |s: Option<String>| s.filter(|s| s.encode_utf16().count() <= FACT_UNITS);
    let side = u32::from(px);
    let icon = facts
        .icon
        .filter(|b| b.width == side && b.height == side && b.pixels.len() as u64 == u64::from(side) * u64::from(side))
        .map(|b| (px, b.pixels.iter().flat_map(|p| p.to_le_bytes()).collect()));
    ExeInfoRec {
        id,
        description: fit(facts.description),
        product: fit(facts.product),
        company: fit(facts.company),
        icon,
    }
}

/// How a stream ended.
enum Streamed {
    /// Every byte went, the last chunk marked last.
    Whole,
    /// The dock cancelled it: no more chunks were sent.
    Cancelled,
    /// The work failed (not the pipe).
    Failed,
    /// A chunk could not be sent: the dock is gone.
    PipeFailed,
}

/// The `Write` a stream's bytes go through: `Chunk`s of at most `CHUNK_MAX` bytes, one at each flush (the icons
/// writer flushes every frame) or when full. Before each chunk it checks the cancelled id, so a `Cancel` stops the
/// stream after its current chunk.
struct ChunkWriter<'a> {
    id: u32,
    buf: Vec<u8>,
    send: &'a mut dyn FnMut(&Reply) -> bool,
    cancelled: &'a AtomicU32,
    stopped: bool,
    broken: bool,
}

impl<'a> ChunkWriter<'a> {
    fn new(id: u32, send: &'a mut dyn FnMut(&Reply) -> bool, cancelled: &'a AtomicU32) -> ChunkWriter<'a> {
        ChunkWriter { id, buf: Vec::new(), send, cancelled, stopped: false, broken: false }
    }

    fn chunk(&mut self, last: bool) -> std::io::Result<()> {
        if self.broken || self.stopped {
            return Err(std::io::Error::other("the stream ended"));
        }
        if self.cancelled.load(SeqCst) == self.id {
            self.stopped = true;
            return Err(std::io::Error::other("cancelled"));
        }
        let bytes = std::mem::take(&mut self.buf);
        if !(self.send)(&Reply::Chunk { id: self.id, last, bytes }) {
            self.broken = true;
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        Ok(())
    }

    /// The last chunk: what is left, marked last, sent even when empty so the dock knows the stream is whole.
    fn finish(&mut self) -> std::io::Result<()> {
        self.chunk(true)
    }

    fn verdict(&self, whole: bool) -> Streamed {
        match (whole, self.broken, self.stopped) {
            (_, true, _) => Streamed::PipeFailed,
            (true, false, _) => Streamed::Whole,
            (false, false, true) => Streamed::Cancelled,
            (false, false, false) => Streamed::Failed,
        }
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.broken || self.stopped {
            return Err(std::io::Error::other("the stream ended"));
        }
        let n = data.len().min(CHUNK_MAX.saturating_sub(self.buf.len()));
        self.buf.extend_from_slice(data.get(..n).unwrap_or_default());
        if self.buf.len() >= CHUNK_MAX {
            self.chunk(false)?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            self.chunk(false)
        }
    }
}

/// The real work, at medium on the agent's COM thread, through the dock's own helpers.
struct ShellWork;

impl Work for ShellWork {
    fn apps(&mut self) -> (i32, Vec<u8>) {
        crate::shell_thread::apps_child_bytes()
    }

    fn icons(&mut self, ids: &[Option<String>], px: u32, mut out: &mut dyn Write) -> i32 {
        crate::shell_thread::write_start_icons(ids, px, &mut out)
    }

    fn links(&mut self) -> Vec<(String, String, String)> {
        user_links()
    }

    fn exe_info(&mut self, path: &str, px: u16) -> ExeFacts {
        use crate::shell_thread::{icon_from_def_extract, icon_from_jumbo_list, nonempty, FileInfo};
        let info = FileInfo::read(path);
        // The exe sources of the dock's icon pipeline, in its order (`extract_icon`).
        let icon = icon_from_def_extract(path)
            .or_else(|| icon_from_jumbo_list(path))
            .and_then(|b| nonempty(crate::icons::resize_area(&b, u32::from(px))));
        ExeFacts { description: info.description, product: info.product, company: info.company, icon }
    }
}

/// The shortcuts of the user's own Start menu folder (`FOLDERID_Programs`: the agent runs as the shell's user), read by
/// the shell at medium, as the index takes them (`collect_lnk`'s caps; `lnk_target` refuses one with arguments): each
/// one's path under that folder, its target, and its display name ("" when the shell gives none).
fn user_links() -> Vec<(String, String, String)> {
    use crate::shell_thread::{collect_lnk, known_folder, lnk_display_name, lnk_target, INDEX_MAX_FILES};
    let Some(root) = known_folder(&FOLDERID_Programs) else { return Vec::new() };
    let mut files = Vec::new();
    let mut left = INDEX_MAX_FILES;
    collect_lnk(&root, 0, true, &mut left, &mut files);
    // SAFETY: COM STA initialized on this thread (`run`); the ShellLink CLSID with the interface it implements.
    let Ok(link) = (unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }) else {
        return Vec::new();
    };
    let Ok(file) = link.cast::<IPersistFile>() else { return Vec::new() };
    files
        .iter()
        .filter_map(|l| {
            let rel = l.path.strip_prefix(&root).ok()?.to_str()?.to_string();
            let target = lnk_target(&link, &file, &l.path)?;
            Some((rel, target, lnk_display_name(&l.path).unwrap_or_default()))
        })
        .collect()
}

// ================================================================== the dock's end (AG3)
//
// `Link` lives on the shell thread (COM, and `desktop_shell_execute` to start the agent at medium). It starts an agent
// when a request needs one (`Life`): two pipes with a fresh nonce and a protected DACL naming SYSTEM and the shell's
// user (Explorer's token user: the real user under Administrator protection); `dock.exe --agent <nonce>` started
// through Explorer, hidden; accepted within 5 s and checked: the dock's own image (file id), its session, exactly
// medium integrity (above is not the user's agent, below is a squatter); its process opened for killing and waiting
// and checked by start time; then the Hellos. "agent-rx" (one thread while an agent lives) decodes every reply
// (`REC_MAX`) and routes it by request id: an icons stream's chunk bytes into the pipe its job's reader reads as it reads
// a child's stdout, every other record to the request the shell thread waits on with its deadline. Kill closes the
// request pipe (the agent's reading thread ends its process at once), then TerminateProcess on the kept handle: on a
// missed deadline, when the link breaks, after an unanswered Bye and when the shell thread ends (a crashed dock closes
// the pipe with its process). So no hung agent keeps dock.exe mapped for more than about a second after the dock.

/// How long the dock waits for its agent to connect and answer its Hello.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// An agent idle this long after its last answer, with nothing in flight, is sent `Bye`.
pub(crate) const IDLE: Duration = Duration::from_secs(30);
/// Failures within `FAILURE_WINDOW` that break the link until the dock's next start.
const BREAK_AFTER: usize = 3;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);
/// How long the dock waits for an agent to end after `Bye` or TerminateProcess.
const END_WAIT_MS: u32 = 1000;
/// A request whose caller stopped waiting (`ExeInfo`, `StartLinks`) and is still unanswered this long: the agent hangs.
const OWED_LIMIT: Duration = Duration::from_secs(5);
/// Replies queued for the request the shell thread waits on; the reading thread waits beyond that.
const QUEUE: usize = 8;

/// Where the link stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentState {
    /// No agent: the next request starts one.
    Off,
    /// One is being started (until `ACCEPT_TIMEOUT`).
    Starting { since: Instant },
    /// One serves; `last_use` is its last answer.
    Up { last_use: Instant },
    /// Three failures within ten minutes: the dock's own helpers serve until the dock starts again.
    Broken,
}

/// What the link does after a step of its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    Start,
    /// Ask the agent to end, then kill whatever is left after a second.
    Bye,
    /// Close the pipe, then TerminateProcess.
    Kill,
}

/// The life of the link, pure: every step is given its instant.
#[derive(Debug)]
pub(crate) struct Life {
    state: AgentState,
    failures: Vec<Instant>,
}

impl Life {
    pub(crate) fn new() -> Life {
        Life { state: AgentState::Off, failures: Vec::new() }
    }

    pub(crate) fn state(&self) -> AgentState {
        self.state
    }

    /// A request wants the agent: `Start`, once, when there is none; otherwise `Nothing` (see `state`).
    pub(crate) fn ensure(&mut self, now: Instant) -> Action {
        if self.state == AgentState::Off {
            self.state = AgentState::Starting { since: now };
            Action::Start
        } else {
            Action::Nothing
        }
    }

    /// The agent started answered its Hello.
    pub(crate) fn up(&mut self, now: Instant) {
        if matches!(self.state, AgentState::Starting { .. }) {
            self.state = AgentState::Up { last_use: now };
        }
    }

    /// The agent answered.
    pub(crate) fn used(&mut self, now: Instant) {
        if let AgentState::Up { last_use } = &mut self.state {
            *last_use = now;
        }
    }

    /// The agent did not start, broke its pipe or sent what the dock refuses: counted, and killed (then Off, or Broken
    /// at the third failure within ten minutes).
    pub(crate) fn failed(&mut self, now: Instant) -> Action {
        self.failures.retain(|t| now.saturating_duration_since(*t) < FAILURE_WINDOW);
        self.failures.push(now);
        self.state = if self.failures.len() >= BREAK_AFTER { AgentState::Broken } else { AgentState::Off };
        Action::Kill
    }

    /// A request past its deadline: the agent hangs (a shell extension that never returns), so it is a failure too.
    pub(crate) fn missed(&mut self, now: Instant) -> Action {
        self.failed(now)
    }

    /// The clock: a start past `ACCEPT_TIMEOUT` failed; an agent idle for `IDLE` with nothing in flight (`busy`) is
    /// sent `Bye`.
    pub(crate) fn tick(&mut self, now: Instant, busy: bool) -> Action {
        match self.state {
            AgentState::Starting { since } if now.saturating_duration_since(since) >= ACCEPT_TIMEOUT => {
                self.failed(now)
            }
            AgentState::Up { last_use } if !busy && now.saturating_duration_since(last_use) >= IDLE => {
                self.state = AgentState::Off;
                Action::Bye
            }
            _ => Action::Nothing,
        }
    }

    /// The dock ends: whatever runs is killed.
    pub(crate) fn exit(&mut self) -> Action {
        if self.state != AgentState::Broken {
            self.state = AgentState::Off;
        }
        Action::Kill
    }

    /// Whether the shell thread needs its timer for the link: while an agent is starting or up (its idle end).
    pub(crate) fn timer_wanted(&self) -> bool {
        matches!(self.state, AgentState::Starting { .. } | AgentState::Up { .. })
    }
}

/// How the link starts an agent: through Explorer (`Explorer`), or a test's own way.
pub(crate) trait Launcher {
    /// Why no agent can ever start in this dock start (None: one may), asked before each start.
    fn never(&mut self) -> Option<&'static str> {
        None
    }
    /// What the agent must be, as the client of the pipes.
    fn rule(&mut self) -> Result<PeerRule, String>;
    /// The SIDs the pipes admit besides SYSTEM.
    fn sids(&mut self) -> Result<Vec<String>, String>;
    /// Starts `--agent <nonce>`.
    fn launch(&mut self, nonce: &str) -> Result<(), String>;
}

/// The dock's launcher: `dock.exe` (the exe its children run, `update::child_exe`) started by the desktop shell, so as
/// Explorer's user at medium, hidden; the pipes admit Explorer's token user.
struct Explorer;

impl Launcher for Explorer {
    fn never(&mut self) -> Option<&'static str> {
        let pid = crate::shell_thread::explorer_pid().ok()?;
        shell_level_refusal(pipe::peer_of_pid(pid).ok()?.integrity)
    }

    fn rule(&mut self) -> Result<PeerRule, String> {
        Ok(PeerRule {
            image: Some(crate::update::child_exe()?),
            same_session: true,
            max_integrity: Some(Integrity::Medium),
            min_integrity: Some(Integrity::Medium),
            pid: None,
        })
    }

    fn sids(&mut self) -> Result<Vec<String>, String> {
        let pid = crate::shell_thread::explorer_pid().map_err(|e| format!("no desktop shell ({:#x})", e.code().0))?;
        Ok(vec![pipe::user_sid_of_pid(pid).map_err(|e| format!("the shell's user: {e}"))?])
    }

    fn launch(&mut self, nonce: &str) -> Result<(), String> {
        let exe = crate::update::child_exe()?;
        let args = format!("--agent {nonce}");
        crate::shell_thread::desktop_shell_execute(&exe.to_string_lossy(), Some(&args), None, Verb::Open, SW_HIDE.0)
            .map_err(|e| format!("the shell did not start it ({:#x})", e.code().0))
    }
}

/// Why no agent can start under a desktop shell at `level`: the agent runs at the shell's level and only a medium one
/// serves (its role refuses elevation, the link accepts nothing else), so a shell at high (UAC off) never gives one.
fn shell_level_refusal(level: Integrity) -> Option<&'static str> {
    (level != Integrity::Medium).then_some("the desktop shell is not at medium integrity")
}

/// A launcher that never starts anything (a link that is never used).
struct NoLauncher;

impl Launcher for NoLauncher {
    fn rule(&mut self) -> Result<PeerRule, String> {
        Err("no agent".into())
    }
    fn sids(&mut self) -> Result<Vec<String>, String> {
        Err("no agent".into())
    }
    fn launch(&mut self, _nonce: &str) -> Result<(), String> {
        Err("no agent".into())
    }
}

/// Where the bytes of an icons stream go: the pipe its job's reader reads (`PipeWriter`), or a test's buffer.
pub(crate) trait StreamSink: Send + Sync {
    /// False when the reader is gone.
    fn put(&self, bytes: &[u8]) -> bool;
}

impl StreamSink for std::io::PipeWriter {
    fn put(&self, bytes: &[u8]) -> bool {
        let mut w: &std::io::PipeWriter = self;
        w.write_all(bytes).is_ok()
    }
}

/// Where the replies of one request go.
#[derive(Clone)]
pub(crate) enum Route {
    /// An icons stream: chunk bytes to the sink; the last chunk or its End closes it.
    Stream(Arc<dyn StreamSink>),
    /// A request the shell thread waits on: every record, then its End.
    Collect(SyncSender<Reply>),
    /// A request whose caller stopped waiting, since then: its records are dropped, its End clears it.
    Owed(Instant),
}

/// What the link's reading thread and the shell thread share.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) routes: Mutex<HashMap<u32, Route>>,
    /// The reading thread ended: the agent is gone or sent what the dock refuses.
    ended: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The reading thread's routing of one decoded reply, by its request id; a reply of no live request (cancelled, late,
/// unknown) is dropped. A stream's bytes are written under the routes' lock, so once `Link::end_stream` has removed a
/// stream none of its bytes reach its reader (the reader always drains its pipe, so the lock is held only briefly);
/// a waiter's records are queued outside it (its queue may be full). False for a reply that has no place after the
/// handshake (a Hello): the link ends.
pub(crate) fn route(shared: &Shared, reply: Reply) -> bool {
    let id = match &reply {
        Reply::Hello { .. } => return false,
        Reply::Chunk { id, .. } | Reply::End { id, .. } => *id,
        Reply::Link(l) => l.id,
        Reply::ExeInfo(e) => e.id,
        Reply::Journal(j) => j.id,
    };
    let mut routes = lock(&shared.routes);
    let Some(to) = routes.get(&id).cloned() else { return true };
    let done = match (to, reply) {
        (Route::Stream(sink), Reply::Chunk { bytes, last, .. }) => !sink.put(&bytes) || last,
        // An End, or a record that has no place in a stream: the stream is over.
        (Route::Stream(_), _) => true,
        (Route::Owed(_), r) => matches!(r, Reply::End { .. }),
        (Route::Collect(tx), r) => {
            drop(routes);
            let end = matches!(r, Reply::End { .. });
            if tx.send(r).is_err() || end {
                lock(&shared.routes).remove(&id);
            }
            return true;
        }
    };
    if done {
        routes.remove(&id);
    }
    true
}

/// The reading thread of one agent: every reply decoded (`REC_MAX`) and routed until the pipe ends or a reply is
/// refused; then every route is dropped, so each waiter and each stream sees the end.
fn read_replies(mut replies: Conn, shared: &Shared) {
    while let Ok(frame) = replies.recv(REC_MAX, None) {
        let Ok(reply) = decode_reply(&frame) else { break };
        if !route(shared, reply) {
            break;
        }
    }
    shared.ended.store(true, SeqCst);
    lock(&shared.routes).clear();
}

/// One live agent.
struct Session {
    requests: Conn,
    /// PROCESS_TERMINATE | SYNCHRONIZE, when it could be opened.
    process: Option<OwnedHandle>,
    shared: Arc<Shared>,
}

/// What a request through the agent came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Done(u16),
    /// No agent: broken, or it could not start.
    Unavailable,
    /// No answer within the deadline.
    Late,
    /// The pipe failed or the answer was refused: the agent was let go.
    Failed,
}

impl Outcome {
    fn why(&self) -> String {
        match self {
            Outcome::Done(status) => format!("status {status}"),
            Outcome::Unavailable => "no agent".into(),
            Outcome::Late => "no answer in time".into(),
            Outcome::Failed => "the agent failed".into(),
        }
    }
}

/// What a missed deadline does: kill the agent (a listing or a stream: the agent hangs), or leave the answer owed (a
/// caller that only waits a little; the agent is killed once it is `OWED_LIMIT` late).
#[derive(Clone, Copy)]
enum Late {
    Kill,
    Owe,
}

/// The dock's link to its agent, on the shell thread.
pub(crate) struct Link {
    life: Life,
    session: Option<Session>,
    launcher: Box<dyn Launcher>,
    next_id: u32,
    /// The first start's outcome is logged once per dock start, and so is a break.
    said: bool,
    said_broken: bool,
}

impl Link {
    pub(crate) fn new(launcher: Box<dyn Launcher>) -> Link {
        Link { life: Life::new(), session: None, launcher, next_id: 0, said: false, said_broken: false }
    }

    /// The shell thread's link. An unelevated dock has no use for an agent (its own helpers already run as the user).
    pub(crate) fn shell() -> Link {
        if crate::shell_thread::elevation() == Some(false) {
            crate::log!("agent: not used, the dock is not elevated");
            return Link::unavailable();
        }
        Link::new(Box::new(Explorer))
    }

    /// A link that never starts an agent.
    pub(crate) fn unavailable() -> Link {
        let mut link = Link::new(Box::new(NoLauncher));
        link.life.state = AgentState::Broken;
        link.said = true;
        link.said_broken = true;
        link
    }

    /// Whether a request may try the agent: it is up, or can start (not broken).
    pub(crate) fn usable(&self) -> bool {
        self.life.state() != AgentState::Broken
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> AgentState {
        self.life.state()
    }

    /// Whether the shell thread needs its timer for the link (`Life::timer_wanted`).
    pub(crate) fn timer_wanted(&self) -> bool {
        self.life.timer_wanted()
    }

    /// The shell thread's clock, at every pass of its queue: an agent gone or hung is let go, an idle one sent `Bye`.
    pub(crate) fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    pub(crate) fn tick_at(&mut self, now: Instant) {
        self.check(now);
        let busy = self.session.as_ref().is_some_and(|s| !lock(&s.shared.routes).is_empty());
        let action = self.life.tick(now, busy);
        self.act(action);
    }

    /// The dock ends: the agent is killed.
    pub(crate) fn exit(&mut self) {
        let action = self.life.exit();
        self.act(action);
    }

    /// The Start catalogue through the agent: the apps arena's bytes (at most `cap`) and the exit code `--start-apps`
    /// would have given, as `apps_from_child` takes them. Past `deadline` the agent is killed.
    pub(crate) fn apps(&mut self, deadline: Duration, cap: usize) -> Result<(i32, Vec<u8>), String> {
        let mut bytes = Vec::new();
        let mut whole = false;
        let outcome = self.call(|id| Request::ListApps { id }, deadline, Late::Kill, &mut |r| match r {
            Reply::Chunk { bytes: b, last, .. } if !whole && bytes.len().saturating_add(b.len()) <= cap => {
                bytes.extend_from_slice(&b);
                whole = last;
                true
            }
            _ => false,
        });
        match outcome {
            Outcome::Done(status) if whole => Ok((i32::from(status), bytes)),
            Outcome::Done(_) => Err("a listing without its last chunk".into()),
            other => Err(other.why()),
        }
    }

    /// Starts an icons stream: `Icons` for `ids` at `px`, whose chunk bytes go to `sink` as they arrive (the end of the
    /// stream drops it). The request id, or None when the agent cannot serve.
    pub(crate) fn icons(&mut self, px: u32, ids: &[String], sink: Arc<dyn StreamSink>) -> Option<u32> {
        let px = u16::try_from(px).ok()?;
        if !self.ready() {
            return None;
        }
        let id = self.take_id();
        let frame = encode_request(&Request::Icons { id, px, ids: ids.to_vec() }).ok()?;
        let session = self.session.as_mut()?;
        lock(&session.shared.routes).insert(id, Route::Stream(sink));
        if session.requests.send(&frame).is_err() {
            self.fail("its pipe failed");
            return None;
        }
        Some(id)
    }

    /// Ends the stream `id` (its job was stopped or replaced): its later chunks are dropped and, when it was still
    /// running, the agent is told to stop it.
    pub(crate) fn end_stream(&mut self, id: u32) {
        let Some(session) = self.session.as_mut() else { return };
        let running = lock(&session.shared.routes).remove(&id).is_some();
        if running && session.requests.send(&cancel_frame(id)).is_err() {
            self.fail("its pipe failed");
        }
    }

    /// A stream ended in time: the agent answered.
    pub(crate) fn stream_done(&mut self) {
        self.life.used(Instant::now());
    }

    /// A stream ran past its deadline: the agent hangs.
    pub(crate) fn stream_missed(&mut self) {
        self.miss("an icons request ran past its deadline");
    }

    /// A stream refused after its header (a frame the decoder refuses, or one cut short) that nobody stopped: the agent
    /// sent what the dock refuses, a failure as in `call`, and it is let go. Nothing when its session has already ended
    /// (that end was counted).
    pub(crate) fn stream_refused(&mut self) {
        if self.session.is_some() {
            self.fail("an icons stream the dock refuses");
        }
    }

    /// The shortcuts of the user's Start menu folder as the shell resolves them at medium, when the agent answers within
    /// `deadline` (otherwise the answer is owed and the caller goes on without it).
    pub(crate) fn start_links(&mut self, deadline: Duration, max: usize) -> Option<Vec<LinkRec>> {
        let mut links = Vec::new();
        let outcome = self.call(|id| Request::StartLinks { id }, deadline, Late::Owe, &mut |r| match r {
            Reply::Link(l) if links.len() < max => {
                links.push(l);
                true
            }
            _ => false,
        });
        (outcome == Outcome::Done(STATUS_OK)).then_some(links)
    }

    /// The version strings and icon (`px`, one of `EXE_ICON_PX`) of the program at `path`, read by the agent, when it
    /// answers within `deadline`; a late answer is dropped.
    pub(crate) fn exe_info(&mut self, path: &str, px: u16, deadline: Duration) -> Option<ExeInfoRec> {
        let mut info = None;
        let request = |id| Request::ExeInfo { id, path: path.to_string(), px };
        let outcome = self.call(request, deadline, Late::Owe, &mut |r| match r {
            Reply::ExeInfo(e) if info.is_none() => {
                info = Some(e);
                true
            }
            _ => false,
        });
        info.filter(|_| outcome == Outcome::Done(STATUS_OK))
    }

    fn take_id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    /// Sends the request `make(id)` and gives its records to `take` until its End (its status) or `deadline`. `take`
    /// refuses a record (false) that has no place in the answer or passes its cap: the agent is then let go.
    fn call(
        &mut self,
        make: impl FnOnce(u32) -> Request,
        deadline: Duration,
        late: Late,
        take: &mut dyn FnMut(Reply) -> bool,
    ) -> Outcome {
        if !self.ready() {
            return Outcome::Unavailable;
        }
        let id = self.take_id();
        let Ok(frame) = encode_request(&make(id)) else { return Outcome::Failed };
        let Some(session) = self.session.as_mut() else { return Outcome::Unavailable };
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        lock(&session.shared.routes).insert(id, Route::Collect(tx));
        let sent = Instant::now();
        if session.requests.send(&frame).is_err() {
            self.fail("its pipe failed");
            return Outcome::Failed;
        }
        let (outcome, why) = loop {
            match rx.recv_timeout((sent + deadline).saturating_duration_since(Instant::now())) {
                Ok(Reply::End { status, .. }) => break (Outcome::Done(status), ""),
                Ok(r) => {
                    if !take(r) {
                        break (Outcome::Failed, "an answer the dock refuses");
                    }
                }
                Err(RecvTimeoutError::Timeout) => break (Outcome::Late, "no answer within the deadline"),
                Err(RecvTimeoutError::Disconnected) => break (Outcome::Failed, "its pipe ended"),
            }
        };
        if let (Outcome::Late, Late::Owe) = (&outcome, late) {
            self.owe(id, sent);
            return outcome;
        }
        // The request is over for this thread: its route goes now (the reading thread may still hold its queue).
        if let Some(session) = &self.session {
            lock(&session.shared.routes).remove(&id);
        }
        match outcome {
            Outcome::Done(_) => self.life.used(Instant::now()),
            Outcome::Late => self.miss(why),
            Outcome::Unavailable | Outcome::Failed => self.fail(why),
        }
        outcome
    }

    /// The answer to `id` is owed: its route becomes `Owed` in one step (unless its End came in the meantime, which
    /// removed it), so its records are dropped and its End clears it; the agent skips it if it has not begun.
    fn owe(&mut self, id: u32, since: Instant) {
        let Some(session) = self.session.as_mut() else { return };
        if let Some(route) = lock(&session.shared.routes).get_mut(&id) {
            *route = Route::Owed(since);
        }
        if session.requests.send(&cancel_frame(id)).is_err() {
            self.fail("its pipe failed");
        }
    }

    /// An agent to send to: the one up, or one started now. False when broken or the start failed.
    fn ready(&mut self) -> bool {
        let now = Instant::now();
        self.check(now);
        match self.life.state() {
            AgentState::Up { .. } => return self.session.is_some(),
            AgentState::Broken => return false,
            AgentState::Off | AgentState::Starting { .. } => {}
        }
        if self.life.ensure(now) != Action::Start {
            return false;
        }
        if let Some(why) = self.launcher.never() {
            // Nothing started, nothing to count or kill: broken until the dock starts again, in one line.
            self.life.state = AgentState::Broken;
            self.said = true;
            self.said_broken = true;
            crate::log!("agent: unavailable ({why}), using the dock's own helpers until the dock restarts");
            return false;
        }
        match self.start() {
            Ok(session) => {
                self.session = Some(session);
                self.life.up(Instant::now());
                self.say("up (medium)");
                true
            }
            Err(why) => {
                self.say(&format!("unavailable ({why}), using the dock's own helpers"));
                self.fail(&why);
                false
            }
        }
    }

    /// The live agent's pipe ended (it is gone, or sent what the dock refuses), or an owed answer is `OWED_LIMIT` late
    /// (it hangs): let go.
    fn check(&mut self, now: Instant) {
        let Some(session) = &self.session else { return };
        if session.shared.ended.load(SeqCst) {
            self.fail("its pipe ended");
            return;
        }
        let hung = lock(&session.shared.routes)
            .values()
            .any(|r| matches!(r, Route::Owed(since) if now.saturating_duration_since(*since) >= OWED_LIMIT));
        if hung {
            self.fail("a request hung");
        }
    }

    /// A missed deadline (`Life::missed`): the agent is killed.
    fn miss(&mut self, why: &str) {
        let action = self.life.missed(Instant::now());
        self.after_failure(action, why);
    }

    /// One failure (`Life::failed`): the agent is killed; at the break, one log line.
    fn fail(&mut self, why: &str) {
        let action = self.life.failed(Instant::now());
        self.after_failure(action, why);
    }

    fn after_failure(&mut self, action: Action, why: &str) {
        self.act(action);
        if self.life.state() == AgentState::Broken && !self.said_broken {
            self.said_broken = true;
            crate::log!("agent: unavailable after {BREAK_AFTER} failures ({why}), using the dock's own helpers until the dock restarts");
        }
    }

    /// The first start's outcome, once per dock start.
    fn say(&mut self, what: &str) {
        if !self.said {
            self.said = true;
            crate::log!("agent: {what}");
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Nothing | Action::Start => {}
            Action::Bye => self.bye(),
            Action::Kill => self.kill(),
        }
    }

    /// `Bye`, a second for the agent to end on its own, then `kill` for whatever is left.
    fn bye(&mut self) {
        if let Some(session) = self.session.as_mut() {
            if let Ok(bye) = encode_request(&Request::Bye) {
                let _ = session.requests.send(&bye);
            }
            if let Some(p) = &session.process {
                // SAFETY: the handle is valid while `session` lives; waiting has no other effect.
                let _ = unsafe { WaitForSingleObject(HANDLE(p.as_raw_handle()), END_WAIT_MS) };
            }
        }
        self.kill();
    }

    /// Closes the request pipe (the agent's reading thread ends its process at once), then TerminateProcess on the kept
    /// handle and a wait for its end; every request in flight sees the end.
    fn kill(&mut self) {
        let Some(Session { requests, process, shared }) = self.session.take() else { return };
        drop(requests);
        if let Some(p) = process {
            end_process(&p);
        }
        lock(&shared.routes).clear();
    }

    /// Starts an agent: the pipes, the launch, the accepts and checks, the Hellos, the reading thread.
    fn start(&mut self) -> Result<Session, String> {
        let rule = self.launcher.rule()?;
        let sids = self.launcher.sids()?;
        let nonce = pipe::nonce();
        let server = |purpose: &str| {
            pipe_name(purpose, &nonce).and_then(|n| Server::create(&n, &sids)).map_err(|e| format!("its pipe: {e}"))
        };
        let (requests, replies) = (server(REQUESTS)?, server(REPLIES)?);
        let t = Instant::now();
        self.launcher.launch(&nonce)?;
        let left = || ACCEPT_TIMEOUT.saturating_sub(t.elapsed());
        let mut requests = requests.accept(left()).map_err(|e| format!("it did not connect: {e}"))?;
        let client = requests.client().map_err(|e| e.to_string())?;
        Conn::check(&client, &rule).map_err(|e| e.to_string())?;
        let process = open_agent(&client);
        let handshake = (|| {
            let mut replies = replies.accept(left()).map_err(|e| format!("it did not connect: {e}"))?;
            let other = replies.client().map_err(|e| e.to_string())?;
            if (other.pid, other.created) != (client.pid, client.created) {
                return Err("its two pipes have different clients".to_string());
            }
            let hello = encode_request(&Request::Hello { proto: AGENT_PROTO, pid: std::process::id() })
                .map_err(|e| format!("{e:?}"))?;
            requests.send(&hello).map_err(|e| e.to_string())?;
            let answer = replies.recv(REC_MAX, Some(left())).map_err(|e| format!("no Hello: {e}"))?;
            if decode_reply(&answer) != Ok(Reply::Hello { proto: AGENT_PROTO, pid: client.pid }) {
                return Err("a wrong Hello".to_string());
            }
            Ok(replies)
        })();
        let shared = Arc::new(Shared::default());
        let theirs = Arc::clone(&shared);
        let reading = handshake.and_then(|replies| {
            std::thread::Builder::new()
                .name("agent-rx".into())
                .stack_size(THREAD_STACK)
                .spawn(move || {
                    probe!(crate::probe::bump(crate::probe::C::Spawned));
                    read_replies(replies, &theirs);
                })
                .map_err(|e| format!("no reading thread: {e}"))
        });
        if let Err(why) = reading {
            if let Some(p) = &process {
                end_process(p);
            }
            return Err(why);
        }
        Ok(Session { requests, process, shared })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A `Cancel` frame.
fn cancel_frame(id: u32) -> Vec<u8> {
    encode_request(&Request::Cancel { id }).unwrap_or_default()
}

/// The agent's process, opened to kill it and wait for its end (and to read its start time), when it is still the
/// process the checks saw (a pid can be reused). None when it cannot be opened (Administrator protection may refuse
/// it: the agent's pipe still ends it; logged once) and for an agent on a thread of this very process (tests).
fn open_agent(peer: &Peer) -> Option<OwnedHandle> {
    if peer.pid == std::process::id() {
        return None;
    }
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    // SAFETY: plain open of a process by id; the handle is owned below.
    let Ok(h) = (unsafe { OpenProcess(access, false, peer.pid) }) else {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Relaxed) {
            crate::log!("agent: its process cannot be opened; its pipe ends it");
        }
        return None;
    };
    // SAFETY: a fresh handle, owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(h.0) };
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the handle is valid while `process` lives; the four FILETIMEs are writable.
    unsafe { GetProcessTimes(HANDLE(process.as_raw_handle()), created, exited, kernel, user) }.ok()?;
    let created = (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    (created == peer.created).then_some(process)
}

/// TerminateProcess on the agent, then up to a second for its end.
fn end_process(p: &OwnedHandle) {
    let h = HANDLE(p.as_raw_handle());
    // SAFETY: the handle is valid while `p` lives; an ended process refuses, harmlessly.
    let _ = unsafe { TerminateProcess(h, 1) };
    // SAFETY: as above; waiting has no other effect.
    let _ = unsafe { WaitForSingleObject(h, END_WAIT_MS) };
}

/// Where the Start catalogue and icons come from, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Agent,
    Child,
    Here,
}

/// The source to try after `failed` (None: the first): the agent when it may serve, then today's child when it is
/// allowed, then the dock itself (pure).
pub(crate) fn next_source(failed: Option<Source>, agent: bool, child: bool) -> Source {
    match failed {
        None if agent => Source::Agent,
        None | Some(Source::Agent) if child => Source::Child,
        _ => Source::Here,
    }
}

/// How many listings and icon batches each source served since the dock started.
static SERVED: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// Counts one listing or icon batch served by `s`.
pub(crate) fn served(s: Source) {
    if let Some(n) = SERVED.get(s as usize) {
        n.fetch_add(1, Relaxed);
    }
}

/// The counts for the log, beside the Start index line.
pub(crate) fn served_line() -> String {
    let [agent, child, here] = &SERVED;
    format!(
        "Start catalogue and icons served by the agent {}, by the dock's helpers {}, in-process {}",
        agent.load(Relaxed),
        child.load(Relaxed),
        here.load(Relaxed)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) mod tests {
    use super::*;
    use crate::agent_proto::{decode_reply, encode_request, REC_MAX};
    use oled_sys::pipe::Server;
    use std::io::BufRead;
    use std::sync::atomic::AtomicUsize;

    /// An icon the fake work makes: "a" a `px` x `px` bitmap, the others none.
    fn fake_icon(id: &str, px: u32) -> Option<IconBitmap> {
        (id == "a").then(|| IconBitmap {
            width: px,
            height: px,
            pixels: (0..px * px).map(|i| 0xFF00_0000 | i).collect(),
        })
    }

    /// The bytes `--start-icons` prints for `ids` at `px` with the fake icons.
    fn child_icon_bytes(ids: &[Option<String>], px: u32) -> Vec<u8> {
        let mut out = Vec::new();
        assert_eq!(crate::shell_thread::write_icon_frames(ids, px, &mut out, fake_icon), 0);
        out
    }

    /// Fake work: fixed answers; with `hang`, a request that never ends.
    #[derive(Clone, Default)]
    pub(crate) struct Fake {
        pub(crate) apps: (i32, Vec<u8>),
        pub(crate) links: Vec<(String, String, String)>,
        pub(crate) exe: ExeFacts,
        pub(crate) hang: bool,
        /// `exe_info` calls, and how long each one takes.
        pub(crate) calls: Arc<AtomicUsize>,
        pub(crate) slow: Duration,
    }

    impl Work for Fake {
        fn apps(&mut self) -> (i32, Vec<u8>) {
            if self.hang {
                loop {
                    std::thread::park();
                }
            }
            self.apps.clone()
        }
        fn icons(&mut self, ids: &[Option<String>], px: u32, mut out: &mut dyn Write) -> i32 {
            crate::shell_thread::write_icon_frames(ids, px, &mut out, fake_icon)
        }
        fn links(&mut self) -> Vec<(String, String, String)> {
            self.links.clone()
        }
        fn exe_info(&mut self, _path: &str, _px: u16) -> ExeFacts {
            self.calls.fetch_add(1, SeqCst);
            std::thread::sleep(self.slow);
            self.exe.clone()
        }
    }

    fn frame(r: &Request) -> Result<Vec<u8>, PipeError> {
        Ok(encode_request(r).unwrap())
    }

    /// The agent over in-memory pipes: the reading thread's steps over `frames`, then the worker over what they queued.
    /// The exit code the reading thread reached (None: the frames ran out first) and every reply.
    fn serve_frames(frames: Vec<Result<Vec<u8>, PipeError>>, work: &mut dyn Work) -> (Option<i32>, Vec<Reply>) {
        let cancelled = AtomicU32::new(NO_ID);
        let (tx, rx) = mpsc::channel();
        let mut exit = None;
        for f in frames {
            if let Rx::Exit(code) = rx_step(f, &cancelled, &tx) {
                exit = Some(code);
                break;
            }
        }
        drop(tx);
        let mut replies = Vec::new();
        serve(
            &rx,
            &mut |r: &Reply| {
                // Every reply crosses the codec as on the pipe.
                replies.push(decode_reply(&encode_reply(r).unwrap()).unwrap());
                true
            },
            work,
            &cancelled,
        );
        (exit, replies)
    }

    /// The bytes of the chunks of `id`, and whether the last one said so.
    fn stream_of(replies: &[Reply], id: u32) -> (Vec<u8>, bool) {
        let mut bytes = Vec::new();
        let mut last = false;
        for r in replies {
            if let Reply::Chunk { id: i, last: l, bytes: b } = r {
                if *i == id {
                    assert!(!last, "a chunk after the last one");
                    assert!(b.len() <= CHUNK_MAX);
                    bytes.extend_from_slice(b);
                    last = *l;
                }
            }
        }
        (bytes, last)
    }

    #[test]
    fn agent_serves_over_fake_io() {
        // Hello: the dock's (protocol 1, the server's pid) is answered with the agent's; anything else is not.
        let me = std::process::id();
        assert_eq!(
            handshake(Some(Request::Hello { proto: AGENT_PROTO, pid: 41 }), 41),
            Some(Reply::Hello { proto: AGENT_PROTO, pid: me })
        );
        assert_eq!(handshake(Some(Request::Hello { proto: 2, pid: 41 }), 41), None);
        assert_eq!(handshake(Some(Request::Hello { proto: AGENT_PROTO, pid: 42 }), 41), None);
        assert_eq!(handshake(Some(Request::ListApps { id: 1 }), 41), None);
        assert_eq!(handshake(None, 41), None);

        let arena: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
        let link = ("Tools\\Editor.lnk".to_string(), r"C:\Apps\editor.exe".to_string(), "Editor".to_string());
        let icon = IconBitmap { width: 16, height: 16, pixels: vec![0x8040_2010; 256] };
        let mut work = Fake {
            apps: (0, arena.clone()),
            links: vec![link.clone()],
            exe: ExeFacts {
                description: Some("Editor".into()),
                product: Some("x".repeat(300)),
                company: None,
                icon: Some(icon),
            },
            hang: false,
            ..Fake::default()
        };
        let ids = vec!["a".to_string(), "b".to_string()];
        let frames = vec![
            frame(&Request::ListApps { id: 1 }),
            frame(&Request::Icons { id: 2, px: 9, ids: ids.clone() }),
            frame(&Request::StartLinks { id: 3 }),
            frame(&Request::ExeInfo { id: 4, path: r"C:\Apps\editor.exe".into(), px: 16 }),
            frame(&Request::UserJournal { id: 5 }),
            frame(&Request::Bye),
            frame(&Request::ListApps { id: 6 }),
        ];
        let (exit, replies) = serve_frames(frames, &mut work);
        assert_eq!(exit, Some(0), "Bye ends the agent");
        // ListApps: the arena in chunks of at most CHUNK_MAX, the last one marked, then End with the child's code.
        assert_eq!(stream_of(&replies, 1), (arena, true));
        let chunks = replies.iter().filter(|r| matches!(r, Reply::Chunk { id: 1, .. })).count();
        assert_eq!(chunks, 3);
        // Icons: exactly what the icons child prints for the same ids.
        let some: Vec<Option<String>> = ids.into_iter().map(Some).collect();
        assert_eq!(stream_of(&replies, 2), (child_icon_bytes(&some, 9), true));
        // StartLinks: one record per link.
        let (rel, target, name) = link;
        assert!(replies.contains(&Reply::Link(LinkRec { id: 3, rel, target, name })));
        // ExeInfo: the strings within their cap, the icon as BGRA bytes.
        let Some(Reply::ExeInfo(e)) = replies.iter().find(|r| matches!(r, Reply::ExeInfo(_))) else { panic!() };
        assert_eq!((e.id, e.description.as_deref(), e.product.as_deref()), (4, Some("Editor"), None));
        assert_eq!(
            e.icon.as_ref().map(|(px, b)| (*px, b.len(), b[..4].to_vec())),
            Some((16, 1024, vec![0x10, 0x20, 0x40, 0x80]))
        );
        // Each request ends with its status, in order; the request after Bye is never served.
        let ends: Vec<(u32, u16)> = replies
            .iter()
            .filter_map(|r| if let Reply::End { id, status } = r { Some((*id, *status)) } else { None })
            .collect();
        assert_eq!(ends, [(1, 0), (2, STATUS_OK), (3, STATUS_OK), (4, STATUS_OK), (5, STATUS_FAILED)]);

        // An empty listing ends with the child's "nothing listed" code after an empty last chunk.
        let mut empty = Fake { apps: (3, Vec::new()), ..Fake::default() };
        let (exit, replies) = serve_frames(vec![frame(&Request::ListApps { id: 7 })], &mut empty);
        assert_eq!(exit, None);
        assert_eq!(replies, [Reply::Chunk { id: 7, last: true, bytes: vec![] }, Reply::End { id: 7, status: 3 }]);

        // A malformed request, a second Hello, a closed or failed pipe: the reading thread ends the agent.
        for (f, code) in [
            (Ok(vec![0xFF]), 2),
            (Ok(vec![2, 1]), 2),
            (frame(&Request::Hello { proto: AGENT_PROTO, pid: 1 }), 2),
            (Err(PipeError::Closed), 0),
            (Err(PipeError::Timeout), 0),
        ] {
            let (exit, replies) = serve_frames(vec![f, frame(&Request::ListApps { id: 8 })], &mut Fake::default());
            assert_eq!((exit, replies.len()), (Some(code), 0));
        }
    }

    #[test]
    fn icons_child_bytes_unchanged() {
        // Frames far larger than a chunk (200 px: 160,012 bytes each) cross the chunk writer byte for byte.
        let ids: Vec<Option<String>> = [Some("a"), None, Some("b"), Some("a")].map(|s| s.map(String::from)).to_vec();
        for px in [8u32, 48, 200] {
            let mut chunks = Vec::new();
            let cancelled = AtomicU32::new(NO_ID);
            let mut send = |r: &Reply| {
                chunks.push(r.clone());
                true
            };
            let mut out = ChunkWriter::new(9, &mut send, &cancelled);
            assert_eq!(Fake::default().icons(&ids, px, &mut out), 0);
            out.finish().unwrap();
            assert_eq!(stream_of(&chunks, 9), (child_icon_bytes(&ids, px), true), "{px} px");
        }
    }

    #[test]
    fn cancel_reaches_a_busy_agent() {
        // The worker streams a long listing; the dock's Cancel arrives while the first chunk goes: the reading thread
        // marks it at once (nothing queued behind the busy worker), and the stream stops after that chunk.
        let cancelled = AtomicU32::new(NO_ID);
        let (tx, rx) = mpsc::channel();
        assert_eq!(rx_step(frame(&Request::ListApps { id: 5 }), &cancelled, &tx), Rx::Go);
        drop(tx);
        let (queue, _unused) = mpsc::channel();
        let mut sent = Vec::new();
        let mut work = Fake { apps: (0, vec![1; 10 * CHUNK_MAX]), ..Fake::default() };
        serve(
            &rx,
            &mut |r: &Reply| {
                if sent.is_empty() {
                    assert_eq!(rx_step(frame(&Request::Cancel { id: 5 }), &cancelled, &queue), Rx::Go);
                    assert_eq!(cancelled.load(SeqCst), 5, "seen at once");
                }
                sent.push(r.clone());
                true
            },
            &mut work,
            &cancelled,
        );
        assert_eq!(sent.len(), 2, "{:?}", sent.iter().map(|r| matches!(r, Reply::Chunk { .. })).collect::<Vec<_>>());
        assert!(matches!(sent[0], Reply::Chunk { id: 5, last: false, .. }));
        assert_eq!(sent[1], Reply::End { id: 5, status: STATUS_CANCELLED });
        // An icons stream stops the same way, after the frame being sent; a request cancelled before it starts is
        // only ended.
        let cancelled = AtomicU32::new(NO_ID);
        let mut sent = Vec::new();
        let ids: Vec<String> = (0..20).map(|_| "a".to_string()).collect();
        let (tx, rx) = mpsc::channel();
        for r in [Request::Icons { id: 6, px: 8, ids }, Request::StartLinks { id: 6 }] {
            tx.send(r).unwrap();
        }
        drop(tx);
        serve(
            &rx,
            &mut |r: &Reply| {
                if matches!(r, Reply::Chunk { .. }) && sent.len() == 3 {
                    cancelled.store(6, SeqCst);
                }
                sent.push(r.clone());
                true
            },
            &mut Fake::default(),
            &cancelled,
        );
        let chunks = sent.iter().filter(|r| matches!(r, Reply::Chunk { .. })).count();
        assert_eq!(chunks, 4, "the header and three frames, then nothing");
        let ends: Vec<&Reply> = sent.iter().filter(|r| matches!(r, Reply::End { .. })).collect();
        assert_eq!(ends, [&Reply::End { id: 6, status: STATUS_CANCELLED }; 2]);
    }

    #[test]
    fn the_start_feedback_ends_without_waiting() {
        // On a fresh thread, as on the agent's main thread at its start: one message is read at once (the system ends
        // the shell's busy pointer at the first GetMessage), and nothing waits.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let first = end_start_feedback();
            let _ = tx.send((first, end_start_feedback()));
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok((true, true)));
    }

    #[test]
    fn agent_role_refuses_elevation() {
        use crate::roles::{gate, Gate, Role};
        assert!(matches!(gate(Role::Agent, Some(true)), Gate::Refuse(_)));
        assert!(matches!(gate(Role::Agent, None), Gate::Refuse(_)));
        assert_eq!(gate(Role::Agent, Some(false)), Gate::Run);
    }

    // ---------------------------------------------------------------- helper processes

    /// Turns this test binary into a helper process of the agent tests ("<role> <nonce>"); set by them only.
    const HELPER_VAR: &str = "DOCK_AGENT_TEST_HELPER";

    /// A hard link of this test binary next to it, for the helper processes: the same file (the agent's and the dock's
    /// image checks compare file ids) under another path, so a test that looks for processes running this binary by its
    /// path (`autostart::exe_in_use_by_other`) never sees them. Removed when dropped, once its processes have ended.
    pub(crate) struct HelperExe(PathBuf);

    impl HelperExe {
        pub(crate) fn new() -> HelperExe {
            static N: AtomicU32 = AtomicU32::new(0);
            let exe = std::env::current_exe().unwrap();
            let n = N.fetch_add(1, SeqCst);
            let link = exe.with_file_name(format!("agent-helper-{}-{n}.exe", std::process::id()));
            let _ = std::fs::remove_file(&link);
            std::fs::hard_link(&exe, &link).unwrap();
            HelperExe(link)
        }

        /// This test binary started again as one helper (`agent_helper_process`), its stdout piped.
        pub(crate) fn spawn(&self, role: &str, nonce: &str) -> std::process::Child {
            use std::os::windows::process::CommandExt;
            use windows::Win32::System::Threading::CREATE_NO_WINDOW;
            std::process::Command::new(&self.0)
                .creation_flags(CREATE_NO_WINDOW.0)
                .args(["agent::tests::agent_helper_process", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
                .env(HELPER_VAR, format!("{role} {nonce}"))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        }
    }

    impl Drop for HelperExe {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The lines a helper prints, as they come.
    fn lines_of(child: &mut std::process::Child) -> Receiver<String> {
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        rx
    }

    /// Waits up to 10 s for a line `want` from a helper.
    fn wait_line(lines: &Receiver<String>, want: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = lines.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            match line {
                // libtest's "test <name> ... " has no newline before the first line a test prints.
                Ok(l) if l.trim_end().ends_with(want) => return,
                Ok(_) => {}
                Err(e) => panic!("no {want:?} from the helper: {e}"),
            }
        }
    }

    /// How long `child` takes to end, up to `limit` (None: still running then).
    pub(crate) fn ends_within(child: &mut std::process::Child, limit: Duration) -> Option<Duration> {
        let t = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                return Some(t.elapsed());
            }
            if t.elapsed() > limit {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A fake dock: the two pipes of `nonce`, an agent accepted on both, the Hellos, one `ListApps`, then nothing
    /// until it is killed. Prints "listening" and "busy"; with `close`, 300 ms later it closes both pipes, prints
    /// "closed" and lives on.
    fn fake_dock(nonce: &str, close: bool) {
        let sids = [pipe::current_user_sid().unwrap()];
        let requests = Server::create(&pipe_name(REQUESTS, nonce).unwrap(), &sids).unwrap();
        let replies = Server::create(&pipe_name(REPLIES, nonce).unwrap(), &sids).unwrap();
        println!("listening");
        let mut requests = requests.accept(Duration::from_secs(10)).unwrap();
        let mut replies = replies.accept(Duration::from_secs(10)).unwrap();
        let me = std::process::id();
        requests.send(&encode_request(&Request::Hello { proto: AGENT_PROTO, pid: me }).unwrap()).unwrap();
        let hello = decode_reply(&replies.recv(REC_MAX, Some(Duration::from_secs(10))).unwrap()).unwrap();
        assert!(matches!(hello, Reply::Hello { proto: AGENT_PROTO, .. }), "{hello:?}");
        requests.send(&encode_request(&Request::ListApps { id: 1 }).unwrap()).unwrap();
        println!("busy");
        if close {
            std::thread::sleep(Duration::from_millis(300));
            drop((requests, replies));
            println!("closed");
        }
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    #[test]
    #[ignore = "a helper process of the agent tests, which start it; does nothing on its own"]
    fn agent_helper_process() {
        let Ok(what) = std::env::var(HELPER_VAR) else { return };
        let (role, nonce) = what.split_once(' ').unwrap();
        let exe = std::env::current_exe().unwrap();
        match role {
            "dock" => fake_dock(nonce, false),
            "dock-close" => fake_dock(nonce, true),
            "agent-hang" => {
                let code = run_with(nonce, &server_rule(exe), &mut Fake { hang: true, ..Fake::default() }, terminate);
                std::process::exit(code);
            }
            // Its reading thread returns at the pipe's end instead of ending the process, and its worker hangs: only
            // another process's TerminateProcess ends it.
            "agent-deaf" => {
                let code = run_with(nonce, &server_rule(exe), &mut Fake { hang: true, ..Fake::default() }, |_| {});
                std::process::exit(code);
            }
            other => panic!("no helper {other}"),
        }
    }

    #[test]
    fn the_agent_exits_within_1s_of_its_server_ending() {
        let nonce = pipe::nonce();
        let exe = HelperExe::new();
        let mut dock = exe.spawn("dock", &nonce);
        let dock_says = lines_of(&mut dock);
        wait_line(&dock_says, "listening");
        let mut agent = exe.spawn("agent-hang", &nonce);
        wait_line(&dock_says, "busy");
        // The agent takes the request and hangs in it.
        std::thread::sleep(Duration::from_millis(300));
        let alive = agent.try_wait().unwrap();
        if alive.is_some() {
            let _ = dock.kill();
        }
        assert_eq!(alive, None, "the agent waits in its request");
        dock.kill().unwrap();
        dock.wait().unwrap();
        let gone = ends_within(&mut agent, Duration::from_secs(1));
        if gone.is_none() {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        assert!(gone.is_some(), "the agent outlived its dock by more than 1 s");
    }

    #[test]
    fn the_agent_exits_within_1s_of_its_pipe_closing() {
        // The fake dock closes both pipes and lives on, so only the pipe's end can end the hung agent: the one way left
        // when the agent may not open the dock's process (Administrator protection).
        let nonce = pipe::nonce();
        let exe = HelperExe::new();
        let mut dock = exe.spawn("dock-close", &nonce);
        let dock_says = lines_of(&mut dock);
        wait_line(&dock_says, "listening");
        let mut agent = exe.spawn("agent-hang", &nonce);
        wait_line(&dock_says, "closed");
        let gone = ends_within(&mut agent, Duration::from_secs(1));
        let dock_alive = dock.try_wait().unwrap().is_none();
        let _ = dock.kill();
        let _ = dock.wait();
        if gone.is_none() {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        assert!(gone.is_some(), "the agent outlived its pipe by more than 1 s");
        assert!(dock_alive, "the dock still ran, so its end did not end the agent");
    }

    // ---------------------------------------------------------------- the dock's end (AG3)

    /// The rule an agent of this test meets (a helper process or a thread): this image, session and integrity.
    pub(crate) fn own_rule() -> PeerRule {
        let me = pipe::peer_of_pid(std::process::id()).unwrap();
        PeerRule {
            image: Some(std::env::current_exe().unwrap()),
            same_session: true,
            max_integrity: Some(me.integrity),
            min_integrity: Some(me.integrity),
            pid: None,
        }
    }

    /// A launcher that runs the agent on a thread of this process, over the real pipes, with fake work. Nothing is ever
    /// terminated: the link closes the pipes, the thread's agent ends with them.
    pub(crate) struct ThreadLauncher(pub(crate) Fake);

    impl Launcher for ThreadLauncher {
        fn rule(&mut self) -> Result<PeerRule, String> {
            Ok(own_rule())
        }
        fn sids(&mut self) -> Result<Vec<String>, String> {
            Ok(vec![pipe::current_user_sid().unwrap()])
        }
        fn launch(&mut self, nonce: &str) -> Result<(), String> {
            let (nonce, mut work) = (nonce.to_string(), self.0.clone());
            let rule = server_rule(std::env::current_exe().unwrap());
            std::thread::spawn(move || run_with(&nonce, &rule, &mut work, |_| {}));
            Ok(())
        }
    }

    /// A launcher that starts a helper process (`role`) as the agent, and keeps it for the test.
    struct ProcLauncher {
        exe: HelperExe,
        role: &'static str,
        agent: Arc<Mutex<Option<std::process::Child>>>,
    }

    impl Launcher for ProcLauncher {
        fn rule(&mut self) -> Result<PeerRule, String> {
            Ok(own_rule())
        }
        fn sids(&mut self) -> Result<Vec<String>, String> {
            Ok(vec![pipe::current_user_sid().unwrap()])
        }
        fn launch(&mut self, nonce: &str) -> Result<(), String> {
            *lock(&self.agent) = Some(self.exe.spawn(self.role, nonce));
            Ok(())
        }
    }

    /// A launcher whose agent, a thread of this process over the real pipes, refuses their server.
    struct RefusingLauncher;

    impl Launcher for RefusingLauncher {
        fn rule(&mut self) -> Result<PeerRule, String> {
            Ok(own_rule())
        }
        fn sids(&mut self) -> Result<Vec<String>, String> {
            Ok(vec![pipe::current_user_sid().unwrap()])
        }
        fn launch(&mut self, nonce: &str) -> Result<(), String> {
            let nonce = nonce.to_string();
            // No process has pid 0: the agent refuses whatever serves the pipes.
            let rule = PeerRule { pid: Some(0), ..server_rule(std::env::current_exe().unwrap()) };
            std::thread::spawn(move || run_with(&nonce, &rule, &mut Fake::default(), |_| {}));
            Ok(())
        }
    }

    /// A launcher under a desktop shell at high integrity: it must never be asked to launch.
    struct HighShellLauncher;

    impl Launcher for HighShellLauncher {
        fn never(&mut self) -> Option<&'static str> {
            shell_level_refusal(Integrity::High)
        }
        fn rule(&mut self) -> Result<PeerRule, String> {
            Ok(own_rule())
        }
        fn sids(&mut self) -> Result<Vec<String>, String> {
            Ok(vec![pipe::current_user_sid().unwrap()])
        }
        fn launch(&mut self, _nonce: &str) -> Result<(), String> {
            panic!("an agent launched under a shell that cannot give one");
        }
    }

    /// A stream's bytes in memory.
    #[derive(Default)]
    pub(crate) struct Memory(pub(crate) Mutex<Vec<u8>>);

    impl StreamSink for Memory {
        fn put(&self, bytes: &[u8]) -> bool {
            lock(&self.0).extend_from_slice(bytes);
            true
        }
    }

    #[test]
    fn link_lifecycle() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut life = Life::new();
        assert!(!life.timer_wanted(), "no agent, no timer");
        // ensure() starts once.
        assert_eq!(life.ensure(t0), Action::Start);
        assert_eq!(life.state(), AgentState::Starting { since: t0 });
        assert_eq!(life.ensure(at(1)), Action::Nothing);
        assert!(life.timer_wanted());
        // Not accepted within 5 s: a failure, killed, Off.
        assert_eq!(life.tick(at(4), false), Action::Nothing);
        assert_eq!(life.tick(at(5), false), Action::Kill);
        assert_eq!(life.state(), AgentState::Off);
        // Up; idle 30 s after its last answer (and nothing in flight): Bye, then Off.
        assert_eq!(life.ensure(at(10)), Action::Start);
        life.up(at(11));
        life.used(at(20));
        assert_eq!(life.state(), AgentState::Up { last_use: at(20) });
        assert_eq!(life.tick(at(49), false), Action::Nothing);
        assert_eq!(life.tick(at(50), true), Action::Nothing, "a request in flight is no idleness");
        assert_eq!(life.tick(at(50), false), Action::Bye);
        assert_eq!(life.state(), AgentState::Off);
        assert!(!life.timer_wanted());
        // Two more failures within ten minutes: broken until the dock starts again.
        assert_eq!(life.ensure(at(60)), Action::Start);
        assert_eq!(life.failed(at(61)), Action::Kill);
        assert_eq!(life.state(), AgentState::Off);
        assert_eq!(life.ensure(at(70)), Action::Start);
        life.up(at(71));
        assert_eq!(life.failed(at(80)), Action::Kill);
        assert_eq!(life.state(), AgentState::Broken);
        assert_eq!(life.ensure(at(5000)), Action::Nothing);
        assert_eq!(life.tick(at(5000), false), Action::Nothing);
        assert_eq!(life.state(), AgentState::Broken);
        assert!(!life.timer_wanted());
        // Failures further apart than ten minutes never break it.
        let mut life = Life::new();
        for k in 0..8 {
            assert_eq!(life.ensure(at(k * 400)), Action::Start);
            assert_eq!(life.failed(at(k * 400 + 1)), Action::Kill);
            assert_eq!(life.state(), AgentState::Off, "failure {k}");
        }
    }

    #[test]
    fn a_missed_deadline_kills_the_agent() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut life = Life::new();
        life.ensure(t0);
        life.up(t0);
        assert_eq!(life.missed(at(5)), Action::Kill);
        assert_eq!(life.state(), AgentState::Off);
        // The failure that breaks the link kills too.
        life.ensure(at(6));
        assert_eq!(life.failed(at(7)), Action::Kill);
        life.ensure(at(8));
        life.up(at(8));
        assert_eq!(life.missed(at(9)), Action::Kill);
        assert_eq!(life.state(), AgentState::Broken);
        // The dock's exit kills whatever runs.
        let mut life = Life::new();
        life.ensure(t0);
        life.up(t0);
        assert_eq!(life.exit(), Action::Kill);
        assert_eq!(life.state(), AgentState::Off);
        assert_eq!(Life::new().exit(), Action::Kill);
    }

    #[test]
    fn a_hung_fake_agent_is_killed_within_its_deadline() {
        // The agent process takes ListApps and never answers (a shell extension that hangs): at the request's deadline
        // (APPS_CHILD_TIMEOUT, 5 s) the link closes the pipe and terminates it through the handle it opened at the
        // client check. The agent is deaf to its pipe's end, so only the link's TerminateProcess (exit code 1) ends it.
        let agent = Arc::new(Mutex::new(None));
        let launcher = ProcLauncher { exe: HelperExe::new(), role: "agent-deaf", agent: Arc::clone(&agent) };
        let mut link = Link::new(Box::new(launcher));
        let deadline = Duration::from_secs(5);
        let t = Instant::now();
        let got = link.apps(deadline, 1 << 20);
        let waited = t.elapsed();
        let mut child = lock(&agent).take().expect("an agent was started");
        // Only "never killed" is caught here: how soon a loaded machine schedules the kill is not the claim (the
        // deadline decision itself is `a_missed_deadline_kills_the_agent`, on an injected clock).
        let gone = ends_within(&mut child, Duration::from_secs(60));
        if gone.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(gone.is_some(), "the hung agent was never killed");
        assert_eq!(child.wait().unwrap().code(), Some(1), "ended by the link's TerminateProcess");
        assert_eq!(got, Err("no answer in time".into()));
        assert!(waited >= deadline, "{waited:?}");
        // One failure: Off, and this request goes on to the dock's own child.
        assert_eq!(link.state(), AgentState::Off);
        assert!(link.usable());
        assert_eq!(next_source(Some(Source::Agent), link.usable(), true), Source::Child);
    }

    #[test]
    fn a_start_that_cannot_succeed_fails_at_once() {
        // The agent refuses its server after connecting both pipes: the dock's start ends as they close, not after the
        // accept timeout (5 s) of a second pipe nobody connects.
        let mut link = Link::new(Box::new(RefusingLauncher));
        let t = Instant::now();
        assert!(link.apps(Duration::from_secs(5), 16).is_err());
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert_eq!(link.state(), AgentState::Off, "one failure, counted");
        // A desktop shell not at medium (high: UAC off) never gives one: broken at once, and nothing is launched.
        assert_eq!(shell_level_refusal(Integrity::Medium), None);
        for level in [Integrity::Untrusted, Integrity::Low, Integrity::High, Integrity::System] {
            assert!(shell_level_refusal(level).is_some(), "{level:?}");
        }
        let mut link = Link::new(Box::new(HighShellLauncher));
        assert_eq!(link.apps(Duration::from_secs(5), 16), Err("no agent".into()));
        assert_eq!(link.state(), AgentState::Broken);
        assert!(!link.usable() && !link.timer_wanted());
    }

    #[test]
    fn cancelled_icon_chunks_are_dropped() {
        let chunk = |id: u32, last: bool, b: &[u8]| Reply::Chunk { id, last, bytes: b.to_vec() };
        let shared = Shared::default();
        let (four, five) = (Arc::new(Memory::default()), Arc::new(Memory::default()));
        lock(&shared.routes).insert(4, Route::Stream(four.clone()));
        lock(&shared.routes).insert(5, Route::Stream(five.clone()));
        assert!(route(&shared, chunk(4, false, b"one")));
        // The job is stopped: its stream's route goes (`end_stream`); what the agent still sends for it goes nowhere.
        lock(&shared.routes).remove(&4);
        for r in [chunk(4, false, b"two"), chunk(4, true, b"three"), Reply::End { id: 4, status: 0 }] {
            assert!(route(&shared, r));
        }
        assert_eq!(*lock(&four.0), b"one");
        // Another stream is untouched; its last chunk closes it.
        assert!(route(&shared, chunk(5, true, b"x")));
        assert_eq!(*lock(&five.0), b"x");
        assert!(lock(&shared.routes).is_empty());
        // A Hello after the handshake ends the link.
        assert!(!route(&shared, Reply::Hello { proto: AGENT_PROTO, pid: 1 }));

        // Through a link and an agent: once end_stream has returned, no byte of the stream reaches its sink.
        let mut link = Link::new(Box::new(ThreadLauncher(Fake::default())));
        let sink = Arc::new(Memory::default());
        let ids: Vec<String> = (0..2000).map(|_| "a".to_string()).collect();
        let id = link.icons(64, &ids, sink.clone()).expect("the agent serves");
        let t = Instant::now();
        while lock(&sink.0).is_empty() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        link.end_stream(id);
        let at_cancel = lock(&sink.0).len();
        assert!(at_cancel > 0 && at_cancel < 2000 * (12 + 64 * 64 * 4), "{at_cancel}");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(lock(&sink.0).len(), at_cancel);
        // The agent stopped the stream and still serves.
        assert_eq!(link.apps(Duration::from_secs(5), 16), Ok((0, Vec::new())));
    }

    /// The three counts of `served_line`.
    fn served_counts() -> [u32; 3] {
        let line = served_line();
        let n: Vec<u32> = line.split(|c: char| !c.is_ascii_digit()).filter_map(|w| w.parse().ok()).collect();
        [n[0], n[1], n[2]]
    }

    #[test]
    fn fallback_order() {
        use Source::{Agent, Child, Here};
        // The agent when it may serve, then today's child when allowed, then in-process.
        assert_eq!(next_source(None, true, true), Agent);
        assert_eq!(next_source(Some(Agent), true, true), Child);
        assert_eq!(next_source(Some(Child), true, true), Here);
        assert_eq!(next_source(Some(Here), true, true), Here);
        assert_eq!(next_source(None, true, false), Agent);
        assert_eq!(next_source(Some(Agent), true, false), Here);
        assert_eq!(next_source(None, false, true), Child);
        assert_eq!(next_source(None, false, false), Here);
        // Each counted (other tests count too: only the rise is checked).
        let before = served_counts();
        served(Agent);
        served(Child);
        served(Child);
        served(Here);
        let after = served_counts();
        assert!(after[0] > before[0] && after[1] >= before[1] + 2 && after[2] > before[2], "{before:?} {after:?}");
        assert!(served_line().starts_with("Start catalogue and icons served by the agent "));
        // A link that is never to start one serves nothing and starts nothing.
        let mut off = Link::unavailable();
        assert!(!off.usable());
        assert_eq!(off.apps(Duration::from_secs(5), 16), Err("no agent".into()));
        assert_eq!(off.icons(16, &[], Arc::new(Memory::default())), None);
    }
}
