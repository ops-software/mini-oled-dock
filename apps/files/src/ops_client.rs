//! The UI's client of the ops host (OP6): spawns files.exe --role=ops on the first request (detached, outside the
//! caller's job when the job allows it), speaks the ops protocol over the named pipe and hands events back through
//! drain(). It connects to a host that already runs before it starts one, and admits only a server that is our image
//! at exactly our integrity, in our session and as our user; a pipe whose server is not ours is refused and not tried
//! again for 60 s. Staged items and undo entries go in frames split by bytes as well as by count (2.2).
//!
//! THREADING: requests are sent under one lock, so their frames never interleave; a reader thread decodes events,
//! assembles UndoPart frames into OpsEvent::Undo and calls `notify`; it blocks on its read (no idle wake-up).

use std::collections::{HashMap, VecDeque};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oled_trust::frame::files::*;
use oled_trust::frame::{decode, decode_hello, encode, encode_hello, split_by_bytes, FrameError};
use windows::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};

use crate::host::named::{connect, own_peer, server_check, FrameIo, NamedError, PipeConn};
pub use crate::host::TestHooks;
use crate::ops::checks::check_name;

pub struct OpsConfig {
    pub exe: std::path::PathBuf,
    pub data_dir: std::path::PathBuf,
    pub owner: u64,
    pub test: Option<TestHooks>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpsError {
    Unavailable(&'static str),
    Refused(Refusal),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoRecord {
    pub of: OpId,
    pub kind: UndoKind,
    pub replaced: u32,
    pub entries: Vec<UndoEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpsEvent {
    Evt(OpEvt),
    Undo(UndoRecord),
    HostLost,
    HostUpdated,
}

/// How long a connection to a running host may wait for the pipe (WaitNamedPipeW).
const CONNECT_WAIT: Duration = Duration::from_millis(2_000);
/// How long a host just started may take to serve its pipe.
const START_WAIT: Duration = Duration::from_secs(15);
/// A pipe whose server is not ours is not tried again for this long.
const REFUSE_FOR: Duration = Duration::from_secs(60);

/// One live connection: its writer, the pipe (to end the reader), the host's pid and a generation number.
struct Conn {
    io: Box<dyn FrameIo>,
    pipe: Option<PipeConn>,
    pid: Option<u32>,
    gen: u64,
}

struct Inner {
    cfg: Option<OpsConfig>,
    notify: Arc<dyn Fn() + Send + Sync>,
    conn: Mutex<Option<Conn>>,
    events: Mutex<VecDeque<OpsEvent>>,
    next_op: AtomicU32,
    gen: AtomicU64,
    refused_until: Mutex<Option<Instant>>,
}

pub struct OpsClient {
    inner: Arc<Inner>,
}

/// UndoPart frames of each operation, collected until its Finished.
#[derive(Default)]
struct UndoParts {
    parts: HashMap<OpId, (Vec<UndoEntry>, bool)>,
}

impl UndoParts {
    /// One event: None when it is swallowed (an UndoPart), else the events to queue in order.
    fn take(&mut self, e: OpEvt) -> Vec<OpsEvent> {
        match e {
            OpEvt::UndoPart { op, entries } => {
                let (held, dropped) = self.parts.entry(op).or_default();
                if !*dropped {
                    if held.len() + entries.len() > UNDO_MAX_ENTRIES as usize {
                        *dropped = true;
                        *held = Vec::new();
                    } else {
                        held.extend(entries);
                    }
                }
                Vec::new()
            }
            OpEvt::Finished { op, undo, replaced, .. } => {
                let collected = self.parts.remove(&op);
                let mut out = Vec::new();
                if let (Some(kind), Some((entries, false))) = (undo, collected) {
                    out.push(OpsEvent::Undo(UndoRecord { of: op, kind, replaced, entries }));
                } else if let Some(kind @ UndoKind::OpenRecycleBin) = undo {
                    out.push(OpsEvent::Undo(UndoRecord { of: op, kind, replaced, entries: Vec::new() }));
                }
                out.push(OpsEvent::Evt(e));
                out
            }
            e => vec![OpsEvent::Evt(e)],
        }
    }
}

fn first_op() -> u32 {
    // A start that differs between UI runs, so operation ids of an earlier UI's journals do not repeat soon.
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_millis());
    ((ms % 0x3FFF_FFFF) as u32).max(1)
}

impl OpsClient {
    /// No process before the first request.
    pub fn new(cfg: OpsConfig, notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        OpsClient { inner: Arc::new(Inner::new(Some(cfg), notify)) }
    }

    /// A client over an already connected FrameIo (tests: the in-memory pair and a fake host). Hello and Attach are
    /// exchanged here.
    #[doc(hidden)]
    pub fn with_io<F: FrameIo + Clone + 'static>(io: F, notify: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        let inner = Arc::new(Inner::new(None, notify));
        let attached = Inner::attach(&inner, io, None, None);
        if let Ok(c) = attached {
            *inner.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
        }
        OpsClient { inner }
    }

    pub fn copy(&self, items: &[ItemRef], dest: &WidePath, c: Conflicts) -> Result<OpId, OpsError> {
        self.staged(items, |op| OpReq::Copy { op, dest: dest.clone(), conflicts: c })
    }
    pub fn move_to(&self, items: &[ItemRef], dest: &WidePath, c: Conflicts) -> Result<OpId, OpsError> {
        self.staged(items, |op| OpReq::Move { op, dest: dest.clone(), conflicts: c })
    }
    pub fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.staged(items, |op| OpReq::Recycle { op })
    }
    pub fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.staged(items, |op| OpReq::DeletePermanent { op })
    }
    pub fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError> {
        check_name(to).map_err(OpsError::Refused)?;
        let op = self.inner.op();
        self.inner.send(vec![OpReq::Rename { op, item: item.clone(), to: to.into() }], true)?;
        Ok(op)
    }
    pub fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError> {
        check_name(name).map_err(OpsError::Refused)?;
        let op = self.inner.op();
        self.inner.send(vec![OpReq::NewFolder { op, parent: parent.clone(), name: name.into() }], true)?;
        Ok(op)
    }
    pub fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError> {
        let op = self.inner.op();
        let mut frames: Vec<OpReq> = split_by_bytes(&rec.entries, 4, &OPS, MAX_LIST)
            .into_iter()
            .map(|part| OpReq::UndoStage { op, entries: part.to_vec() })
            .collect();
        frames.push(OpReq::Undo { op, of: rec.of, kind: rec.kind, replaced: rec.replaced });
        self.inner.send(frames, true)?;
        Ok(op)
    }
    pub fn cancel(&self, op: OpId) {
        let _ = self.inner.send(vec![OpReq::Cancel { op }], false);
    }
    pub fn pause(&self, op: OpId, on: bool) {
        let _ = self.inner.send(vec![OpReq::Pause { op, on }], false);
    }
    pub fn bring_forward(&self, op: OpId) {
        let _ = self.inner.send(vec![OpReq::BringForward { op }], false);
    }
    pub fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError> {
        let op = self.inner.op();
        self.inner.send(vec![OpReq::ResumeRest { op, of }], true)?;
        Ok(op)
    }
    pub fn ack_done(&self, of: OpId) {
        let _ = self.inner.send(vec![OpReq::AckDone { of }], false);
    }
    /// For AllowSetForegroundWindow before bring_forward.
    pub fn host_pid(&self) -> Option<u32> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|c| c.pid)
    }
    /// UndoPart frames come back assembled as OpsEvent::Undo (at most UNDO_MAX_ENTRIES; no Undo event for an op whose
    /// record was dropped).
    pub fn drain(&self) -> Vec<OpsEvent> {
        self.inner.events.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
    /// Bye; running operations continue in the host.
    pub fn shutdown(&self) {
        let mut g = self.inner.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut c) = g.take() {
            let _ = send_req(c.io.as_mut(), &OpReq::Bye);
            if let Some(p) = &c.pipe {
                p.cancel();
            }
        }
    }

    /// Stage frames split by bytes, then the operation's own frame.
    fn staged(&self, items: &[ItemRef], last: impl FnOnce(OpId) -> OpReq) -> Result<OpId, OpsError> {
        if items.is_empty() {
            return Err(OpsError::Refused(Refusal::NoItems));
        }
        let op = self.inner.op();
        let mut frames: Vec<OpReq> = split_by_bytes(items, 4, &OPS, MAX_LIST)
            .into_iter()
            .map(|part| OpReq::Stage { op, items: part.to_vec() })
            .collect();
        frames.push(last(op));
        self.inner.send(frames, true)?;
        Ok(op)
    }
}

impl Drop for OpsClient {
    fn drop(&mut self) {
        // The reader thread holds the pipe: end it (the host keeps running; operations continue).
        if let Some(c) = self.inner.conn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if let Some(p) = &c.pipe {
                p.cancel();
            }
        }
    }
}

fn send_req(io: &mut dyn FrameIo, r: &OpReq) -> Result<(), OpsError> {
    let mut buf = Vec::new();
    encode(r, &mut buf).map_err(|_| OpsError::Unavailable("a request does not fit a frame"))?;
    io.send_frame(&buf).map_err(|_| OpsError::Unavailable("ops host lost"))
}

impl Inner {
    fn new(cfg: Option<OpsConfig>, notify: Arc<dyn Fn() + Send + Sync>) -> Inner {
        Inner {
            cfg,
            notify,
            conn: Mutex::new(None),
            events: Mutex::new(VecDeque::new()),
            next_op: AtomicU32::new(first_op()),
            gen: AtomicU64::new(0),
            refused_until: Mutex::new(None),
        }
    }

    fn op(&self) -> OpId {
        OpId(self.next_op.fetch_add(1, Ordering::Relaxed))
    }

    fn push(&self, evs: Vec<OpsEvent>) {
        if evs.is_empty() {
            return;
        }
        self.events.lock().unwrap_or_else(|e| e.into_inner()).extend(evs);
        (self.notify)();
    }

    /// Sends `frames` in order on the connection, connecting (and, with `spawn`, starting a host) first; a
    /// connection that fails is dropped and one fresh one is tried.
    fn send(self: &Arc<Self>, frames: Vec<OpReq>, spawn: bool) -> Result<(), OpsError> {
        let mut g = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if g.is_none() {
                *g = Some(self.open(spawn)?);
            }
            let Some(c) = g.as_mut() else { return Err(OpsError::Unavailable("no ops host")) };
            let sent = frames.iter().try_for_each(|f| send_req(c.io.as_mut(), f));
            match sent {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 1 => {
                    *g = None;
                    return Err(e);
                }
                Err(_) => *g = None,
            }
        }
        Err(OpsError::Unavailable("ops host lost"))
    }

    /// A connection to our host: an existing one first; with `spawn`, a new host when the pipe is absent.
    fn open(self: &Arc<Self>, spawn: bool) -> Result<Conn, OpsError> {
        let Some(cfg) = &self.cfg else { return Err(OpsError::Unavailable("ops host lost")) };
        if self.refused_until.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t) {
            return Err(OpsError::Unavailable("ops host is not ours"));
        }
        let hooks = crate::host::effective(&cfg.test);
        let name = crate::role_ops::pipe_name(
            hooks.and_then(|h| h.pipe_tag.as_deref()),
            hooks.and_then(|h| h.build_tag.as_deref()),
        )
        .map_err(|_| OpsError::Unavailable("no pipe name"))?;
        let pipe = match connect(&name, CONNECT_WAIT, false) {
            Ok(p) => p,
            Err(NamedError::Absent) if spawn => {
                spawn_host(cfg).map_err(|_| OpsError::Unavailable("cannot start the ops host"))?;
                connect(&name, START_WAIT, true).map_err(|_| OpsError::Unavailable("the ops host did not start"))?
            }
            Err(_) => return Err(OpsError::Unavailable("no ops host")),
        };
        let pid = pipe.server_pid();
        if let Err(e) = self.check_server(pid, &cfg.exe) {
            oled_sys::log!("files: the ops pipe's server is refused: {e}");
            pipe.cancel();
            *self.refused_until.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + REFUSE_FOR);
            return Err(OpsError::Unavailable("ops host is not ours"));
        }
        Inner::attach(self, pipe.clone(), Some(pipe), pid)
    }

    fn check_server(&self, pid: Option<u32>, exe: &std::path::Path) -> Result<(), NamedError> {
        let pid = pid.ok_or_else(|| NamedError::Peer("no server pid".into()))?;
        let peer = oled_sys::pipe::peer_of_pid(pid).map_err(|e| NamedError::Peer(e.to_string()))?;
        let server_user = oled_sys::pipe::user_sid_of_pid(pid).map_err(|e| NamedError::Peer(e.to_string()))?;
        let own_user = oled_sys::pipe::current_user_sid().map_err(|e| NamedError::Peer(e.to_string()))?;
        let own = own_peer().ok_or_else(|| NamedError::Peer("own token unreadable".into()))?;
        server_check(&peer, &server_user, &own_user, own.integrity, exe)
    }

    /// Hello both ways, the reader thread, then Attach.
    fn attach<F: FrameIo + Clone + 'static>(
        this: &Arc<Self>,
        io: F,
        pipe: Option<PipeConn>,
        pid: Option<u32>,
    ) -> Result<Conn, OpsError> {
        let (mut r, mut w) = (io.clone(), io);
        let hello = r.recv_frame().ok().flatten().ok_or(OpsError::Unavailable("no Hello from the ops host"))?;
        match decode_hello(&OPS, &hello) {
            Ok(_) => {}
            Err(FrameError::BadProto { .. }) => {
                this.push(vec![OpsEvent::HostUpdated]);
                return Err(OpsError::Unavailable("the ops host speaks another version"));
            }
            Err(_) => return Err(OpsError::Unavailable("not an ops host")),
        }
        let mut mine = Vec::new();
        encode_hello(&OPS, crate::BUILD_ID, &mut mine);
        w.send_frame(&mine).map_err(|_| OpsError::Unavailable("ops host lost"))?;
        let gen = this.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let weak = Arc::downgrade(this);
        std::thread::Builder::new()
            .name("ops-client-rx".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut undo = UndoParts::default();
                loop {
                    let frame = r.recv_frame();
                    let Some(inner) = weak.upgrade() else { return };
                    match frame {
                        Ok(Some(f)) => match decode::<OpEvt>(&f) {
                            Ok((e, _)) => inner.push(undo.take(e)),
                            Err(_) => {
                                inner.lost(gen);
                                return;
                            }
                        },
                        Ok(None) | Err(_) => {
                            inner.lost(gen);
                            return;
                        }
                    }
                }
            })
            .map_err(|_| OpsError::Unavailable("no reader thread"))?;
        let mut conn = Conn { io: Box::new(w), pipe, pid, gen };
        let owner = this.cfg.as_ref().map_or(0, |c| c.owner);
        send_req(conn.io.as_mut(), &OpReq::Attach { ui_pid: std::process::id(), owner })?;
        Ok(conn)
    }

    /// The connection of generation `gen` ended: HostLost, and the next request connects again.
    fn lost(&self, gen: u64) {
        let mut g = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|c| c.gen == gen) {
            *g = None;
            drop(g);
            self.push(vec![OpsEvent::HostLost]);
        }
    }
}

/// Starts files.exe --role=ops, detached and without a window; outside the caller's job when the job allows it.
fn spawn_host(cfg: &OpsConfig) -> std::io::Result<()> {
    let hooks = crate::host::effective(&cfg.test).cloned();
    let prepare = |c: &mut Command| {
        c.args(["--role=ops", "--proto=1"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(h) = &hooks {
            for (k, v) in &h.env {
                c.env(&**k, &**v);
            }
            if let Some(t) = &h.pipe_tag {
                c.env("FILES_TEST_PIPE_TAG", &**t);
            }
            if let Some(t) = &h.build_tag {
                c.env("FILES_TEST_BUILD_TAG", &**t);
            }
            if !h.env.iter().any(|(k, _)| &**k == "FILES_DATA_DIR") {
                c.env("FILES_DATA_DIR", &cfg.data_dir);
            }
        }
    };
    let mut c = Command::new(&cfg.exe);
    c.creation_flags(CREATE_NO_WINDOW.0 | CREATE_BREAKAWAY_FROM_JOB.0);
    prepare(&mut c);
    match c.spawn() {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(5) => {
            oled_sys::log!("files: ops host runs inside the caller's job");
            let mut c = Command::new(&cfg.exe);
            c.creation_flags(CREATE_NO_WINDOW.0);
            prepare(&mut c);
            c.spawn().map(|_| ())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WidePath {
        WidePath::parse(s).unwrap()
    }

    #[test]
    fn undo_parts_are_assembled_per_op() {
        let e = |i: u32| UndoEntry {
            from: p(&format!("C:\\a\\{i}")),
            to: p(&format!("C:\\b\\{i}")),
            size: 1,
            mtime: 2,
            dir: false,
            bin: None,
        };
        let mut u = UndoParts::default();
        assert!(u.take(OpEvt::UndoPart { op: OpId(1), entries: vec![e(1), e(2)] }).is_empty());
        assert!(u.take(OpEvt::UndoPart { op: OpId(1), entries: vec![e(3)] }).is_empty());
        assert!(u.take(OpEvt::UndoPart { op: OpId(2), entries: vec![e(9)] }).is_empty());
        let fin = OpEvt::Finished {
            op: OpId(1),
            result: OpResult::Done,
            undo: Some(UndoKind::RecycleCopies),
            replaced: 0,
            items: 3,
            failed: 0,
        };
        let out = u.take(fin.clone());
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            OpsEvent::Undo(UndoRecord {
                of: OpId(1),
                kind: UndoKind::RecycleCopies,
                replaced: 0,
                entries: vec![e(1), e(2), e(3)]
            })
        );
        assert_eq!(out[1], OpsEvent::Evt(fin));
        let dropped =
            OpEvt::Finished { op: OpId(2), result: OpResult::Done, undo: None, replaced: 0, items: 1, failed: 0 };
        assert_eq!(u.take(dropped.clone()), vec![OpsEvent::Evt(dropped)], "no Undo for a dropped record");
        assert!(u.parts.is_empty(), "nothing is held after Finished");
        let mut big = UndoParts::default();
        for _ in 0..9 {
            big.take(OpEvt::UndoPart { op: OpId(3), entries: vec![e(0); 2_048] });
        }
        let fin = OpEvt::Finished {
            op: OpId(3),
            result: OpResult::Done,
            undo: Some(UndoKind::RecycleCopies),
            replaced: 0,
            items: 1,
            failed: 0,
        };
        assert_eq!(
            big.take(fin.clone()),
            vec![OpsEvent::Evt(fin)],
            "over UNDO_MAX_ENTRIES the client drops what it collected"
        );
    }
}
