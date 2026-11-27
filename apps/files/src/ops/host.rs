//! The ops host's request dispatch (OP5b): the requests of one attached UI over a FrameIo, the queue of operations
//! (at most MAX_RUNNING run, more wait FIFO), the events of the running ones, and what is kept for a UI that is away:
//! ops-done.json and the journals of interrupted operations (Pending when their host is dead). One writer queue per
//! connection, fed under the host's lock, so frames never interleave and a reply always precedes the events it
//! causes.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use oled_trust::frame::files::{
    OpEvt, OpId, OpKind, OpReq, OpResult, OpStatus, Refusal, UndoEntry, UndoKind, WidePath, OPS, UNDO_MAX_ENTRIES,
};
use oled_trust::frame::{decode, decode_hello, encode, encode_hello};
use serde::{Deserialize, Serialize};
use windows::core::BOOL;
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
};

use super::checks::{check_item, check_name, item_path, same_parent};
use super::consts::{MAX_RUNNING, PAUSE_OK, STAGE_IN_MEMORY, STAGE_MAX};
use super::journal::{self, JKind, Journal, PathRepr, StagedItems};
use super::runner::{run, RunCtx, RunStats};
pub use super::runner::{JournalCtx, OpJob, Prepared, Work};
use super::undo::{self, UndoFs};
use crate::host::named::FrameIo;
use crate::ops_client::UndoRecord;
use crate::own_files::OwnDir;

/// Runs operations for the host; the real one is OP3's runner on threads, tests inject a fake.
pub trait Runner: Send {
    fn start(&mut self, op: OpId, work: Work, events: Sender<OpEvt>);
    fn cancel(&mut self, op: OpId);
    /// False when pausing is not supported (PAUSE_OK false): the host refuses the request.
    fn pause(&mut self, op: OpId, on: bool) -> bool;
    /// Brings the operation's system dialog forward, when it has one.
    fn bring_forward(&mut self, _op: OpId) {}
}

/// The host's settings.
pub struct HostConfig {
    pub data: OwnDir,
    pub pid: u32,
    /// This process's creation time (FILETIME).
    pub created: u64,
    /// FILES_TEST_ROOT (debug builds).
    pub roots: Option<Vec<WidePath>>,
    /// Staged items one operation may hold (STAGE_MAX; tests lower it).
    pub stage_max: u64,
    pub fs: Box<dyn UndoFs + Send>,
}

/// One entry of ops-done.json: an operation that finished while no UI was attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoneEntry {
    pub op: u32,
    pub kind: JKind,
    pub items: u32,
    pub failed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<PathRepr>,
    /// FILETIME of the end.
    pub finished: u64,
}

impl HostConfig {
    /// The production settings over the data folder `data`.
    pub fn new(data: OwnDir, pid: u32, created: u64, roots: Option<Vec<WidePath>>) -> HostConfig {
        HostConfig { data, pid, created, roots, stage_max: STAGE_MAX, fs: Box::new(undo::OsFs) }
    }
}

pub const OPS_DONE: &str = "ops-done.json";

/// HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND): a resumed item whose source and destination are both gone.
const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

struct Running {
    kind: OpKind,
    /// Top-level items.
    items: u64,
    done: u64,
    total: u64,
    needs_input: bool,
    dest: Option<WidePath>,
}

struct State {
    cfg: HostConfig,
    runner: Box<dyn Runner>,
    /// The writer queue of the connected UI, once it sent Attach.
    ui: Option<Sender<Vec<u8>>>,
    staged: HashMap<OpId, StagedItems>,
    undo_staged: HashMap<OpId, Vec<UndoEntry>>,
    running: HashMap<OpId, Running>,
    queue: VecDeque<(OpId, Work)>,
    owner: u64,
    /// The files a resume makes obsolete (the interrupted operation's journal and spill), removed once the resume's
    /// own journal is on disk (retire), or when it finishes. While listed, they are not offered or resumed again.
    obsolete: HashMap<OpId, Vec<String>>,
}

/// Called when attachment or business changes (the lifetime machine of OP5c).
pub type OnChange = Box<dyn Fn(bool, bool) + Send + Sync>;

pub struct Host {
    state: Mutex<State>,
    events: Sender<OpEvt>,
    attached: Arc<AtomicBool>,
    on_change: Mutex<Option<OnChange>>,
    /// The data folder and this host's pid, for the reads done outside the lock (Attach's journals).
    data: OwnDir,
    pid: u32,
}

/// How a connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeEnd {
    /// Bye, or the UI closed the pipe.
    Detached,
    /// The peer speaks another protocol or version (its Hello).
    Version,
    /// A frame that does not decode: the connection is closed.
    BadFrame,
}

impl Host {
    /// A host with its runner; a pump thread hands the runner's events to the host while the host lives. The
    /// attachment flag is shared with the runner (an UndoPart due while no UI is attached drops the record).
    pub fn new(cfg: HostConfig, runner: Box<dyn Runner>, attached: Arc<AtomicBool>) -> Arc<Host> {
        let (tx, rx) = channel::<OpEvt>();
        let (data, pid) = (cfg.data.clone(), cfg.pid);
        let state = State {
            cfg,
            runner,
            ui: None,
            staged: HashMap::new(),
            undo_staged: HashMap::new(),
            running: HashMap::new(),
            queue: VecDeque::new(),
            owner: 0,
            obsolete: HashMap::new(),
        };
        let host =
            Arc::new(Host { state: Mutex::new(state), events: tx, attached, on_change: Mutex::new(None), data, pid });
        let weak: Weak<Host> = Arc::downgrade(&host);
        let _ = std::thread::Builder::new().name("ops-events".into()).spawn(move || {
            for e in rx {
                match weak.upgrade() {
                    Some(h) => h.on_event(e),
                    None => break,
                }
            }
        });
        host
    }

    pub fn set_on_change(&self, f: OnChange) {
        *self.on_change.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Operations running or queued.
    pub fn busy(&self) -> bool {
        let s = self.lock();
        !s.running.is_empty() || !s.queue.is_empty()
    }

    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }

    /// The running operations (kind, top-level items), for the session-end reason.
    pub fn running(&self) -> Vec<(OpKind, u64)> {
        self.lock().running.values().map(|r| (r.kind, r.items)).collect()
    }

    fn changed(&self) {
        let (attached, busy) = (self.is_attached(), self.busy());
        if let Some(f) = self.on_change.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            f(attached, busy);
        }
    }

    /// Serves one connection: Hello both ways, then requests until Bye or the end of the pipe.
    pub fn serve<F: FrameIo + Clone + 'static>(self: &Arc<Self>, io: F) -> ServeEnd {
        let (mut r, mut w) = (io.clone(), io);
        let mut hello = Vec::new();
        encode_hello(&OPS, crate::BUILD_ID, &mut hello);
        if w.send_frame(&hello).is_err() {
            return ServeEnd::Detached;
        }
        match r.recv_frame() {
            Ok(Some(f)) => match decode_hello(&OPS, &f) {
                Ok((h, _)) => oled_sys::log!("files ops: a client attached (build {})", build_text(&h.build)),
                Err(e) => {
                    oled_sys::log!("files ops: a client of another protocol: {e:?}");
                    return ServeEnd::Version;
                }
            },
            _ => return ServeEnd::Detached,
        }
        let (tx, rx) = channel::<Vec<u8>>();
        let writer = std::thread::Builder::new().name("ops-writer".into()).spawn(move || {
            for f in rx {
                if w.send_frame(&f).is_err() {
                    break;
                }
            }
        });
        let end = loop {
            match r.recv_frame() {
                Ok(Some(f)) => match decode::<OpReq>(&f) {
                    Ok((OpReq::Bye, _)) => break ServeEnd::Detached,
                    Ok((req, _)) => self.handle(req, &tx),
                    Err(e) => {
                        oled_sys::log!("files ops: a frame that does not decode: {e:?}");
                        break ServeEnd::BadFrame;
                    }
                },
                Ok(None) | Err(_) => break ServeEnd::Detached,
            }
        };
        self.detach();
        drop(tx);
        if let Ok(t) = writer {
            let _ = t.join();
        }
        end
    }

    fn detach(&self) {
        let mut s = self.lock();
        s.ui = None;
        self.attached.store(false, Ordering::Release);
        drop(s);
        self.changed();
    }

    /// One request; its replies go to `out` before anything it starts can send an event.
    pub fn handle(&self, req: OpReq, out: &Sender<Vec<u8>>) {
        // Attach reads the journals of interrupted operations, which can be hundreds of MB: line by line and before
        // the lock, so the events of running operations never wait on it.
        let interrupted = if matches!(req, OpReq::Attach { .. }) { pending(&self.data, self.pid) } else { Vec::new() };
        let mut s = self.lock();
        let send = |e: OpEvt| {
            let mut buf = Vec::new();
            if encode(&e, &mut buf).is_ok() {
                let _ = out.send(buf);
            }
        };
        match req {
            OpReq::Attach { ui_pid, owner } => {
                oled_sys::log!("files ops: attach from pid {ui_pid}");
                s.owner = owner;
                s.ui = Some(out.clone());
                self.attached.store(true, Ordering::Release);
                let running = s
                    .running
                    .iter()
                    .map(|(op, r)| OpStatus {
                        op: *op,
                        kind: r.kind,
                        done: r.done,
                        total: r.total,
                        needs_input: r.needs_input,
                    })
                    .collect();
                send(OpEvt::Snapshot { running });
                for (rel, e) in interrupted {
                    // Not one a resume of this host covers (queued or running), nor one removed since it was read.
                    if !covered(&s, &rel) && s.cfg.data.path().join(&rel).is_file() {
                        send(e);
                    }
                }
                for d in read_done(&s.cfg.data) {
                    send(OpEvt::DoneWhileAway {
                        of: OpId(d.op),
                        kind: d.kind.into(),
                        items: d.items,
                        failed: d.failed,
                        dest: d.dest.as_ref().and_then(PathRepr::to_path),
                    });
                }
            }
            OpReq::Stage { op, items } => {
                if s.running.contains_key(&op) || s.queue.iter().any(|(o, _)| *o == op) {
                    return send(refused(op, Refusal::OpInUse, ""));
                }
                let (data, pid, max) = (s.cfg.data.clone(), s.cfg.pid, s.cfg.stage_max);
                let st = s.staged.entry(op).or_insert_with(|| StagedItems::new(Some(&data), op, pid, STAGE_IN_MEMORY));
                if st.len() + items.len() as u64 > max {
                    drop_staged(&mut s, op);
                    return send(refused(op, Refusal::TooMany, ""));
                }
                for it in &items {
                    let pushed = match item_path(it) {
                        Ok(p) => st.push(p.clone()).map_err(|_| Refusal::TooMany),
                        Err(r) => Err(r),
                    };
                    if let Err(r) = pushed {
                        drop_staged(&mut s, op);
                        return send(refused(op, r, ""));
                    }
                }
            }
            OpReq::Copy { op, dest, conflicts } => {
                self.queue_staged(&mut s, op, OpKind::Copy, Some(dest), conflicts, &send)
            }
            OpReq::Move { op, dest, conflicts } => {
                self.queue_staged(&mut s, op, OpKind::Move, Some(dest), conflicts, &send)
            }
            OpReq::Recycle { op } => self.queue_staged(
                &mut s,
                op,
                OpKind::Recycle,
                None,
                oled_trust::frame::files::Conflicts::AskSystem,
                &send,
            ),
            OpReq::DeletePermanent { op } => self.queue_staged(
                &mut s,
                op,
                OpKind::DeletePermanent,
                None,
                oled_trust::frame::files::Conflicts::AskSystem,
                &send,
            ),
            OpReq::Rename { op, item, to } => {
                let roots = s.cfg.roots.clone();
                if let Err(r) = check_item(&item, OpKind::Rename, roots.as_deref()).and_then(|()| check_name(&to)) {
                    return send(refused(op, r, &name_text(&item)));
                }
                let job = OpJob {
                    kind: OpKind::Rename,
                    dest: None,
                    name: Some(to),
                    conflicts: oled_trust::frame::files::Conflicts::AskSystem,
                    staged: StagedItems::from_vec(vec![item]),
                };
                self.enqueue(&mut s, op, Work::Staged(job), &send);
            }
            OpReq::NewFolder { op, parent, name } => {
                let roots = s.cfg.roots.clone();
                if let Err(r) =
                    check_item(&parent, OpKind::NewFolder, roots.as_deref()).and_then(|()| check_name(&name))
                {
                    return send(refused(op, r, &name));
                }
                let job = OpJob {
                    kind: OpKind::NewFolder,
                    dest: Some(parent),
                    name: Some(name),
                    conflicts: oled_trust::frame::files::Conflicts::AskSystem,
                    staged: StagedItems::from_vec(Vec::new()),
                };
                self.enqueue(&mut s, op, Work::Staged(job), &send);
            }
            OpReq::UndoStage { op, entries } => {
                let held = s.undo_staged.entry(op).or_default();
                if held.len() + entries.len() > UNDO_MAX_ENTRIES as usize {
                    s.undo_staged.remove(&op);
                    return send(refused(op, Refusal::TooMany, ""));
                }
                held.extend(entries);
            }
            OpReq::Undo { op, of, kind, replaced } => {
                let entries = s.undo_staged.remove(&op).unwrap_or_default();
                let rec = UndoRecord { of, kind, replaced, entries };
                match undo::check(&rec, s.cfg.fs.as_ref()) {
                    Err((why, detail)) => send(refused(op, why, &detail)),
                    Ok(UndoKind::OpenRecycleBin) => {
                        send(OpEvt::Accepted { op });
                        send(OpEvt::Finished {
                            op,
                            result: OpResult::Done,
                            undo: Some(UndoKind::OpenRecycleBin),
                            replaced: 0,
                            items: 0,
                            failed: 0,
                        });
                    }
                    Ok(k) => {
                        let plan = undo::plan(op, &UndoRecord { kind: k, ..rec });
                        if let Err((why, detail)) = check_plan(&plan, s.cfg.roots.as_deref()) {
                            return send(refused(op, why, &detail));
                        }
                        self.enqueue(&mut s, op, Work::Planned(OpKind::Undo, plan), &send);
                    }
                }
            }
            OpReq::Cancel { op } => {
                if s.running.contains_key(&op) {
                    s.runner.cancel(op);
                } else if let Some(i) = s.queue.iter().position(|(o, _)| *o == op) {
                    s.queue.remove(i);
                    send(OpEvt::Finished {
                        op,
                        result: OpResult::Cancelled,
                        undo: None,
                        replaced: 0,
                        items: 0,
                        failed: 0,
                    });
                } else {
                    drop_staged(&mut s, op);
                }
            }
            OpReq::Pause { op, on } => {
                if !(PAUSE_OK && s.runner.pause(op, on)) {
                    send(refused(op, Refusal::PauseUnsupported, ""));
                }
            }
            OpReq::BringForward { op } => s.runner.bring_forward(op),
            OpReq::ResumeRest { op, of } => {
                // One resume per interrupted operation: a second one while the first is queued or running is refused.
                let names = journal_names(&s.cfg.data, s.cfg.pid);
                if names.iter().any(|(o, _, name)| *o == of && covered(&s, &format!("ops\\{name}"))) {
                    return send(refused(op, Refusal::OpInUse, ""));
                }
                match resume_plan(&s.cfg, op, of) {
                    Ok((plan, missing, obsolete)) => {
                        if let Err((why, detail)) = check_plan(&plan, s.cfg.roots.as_deref()) {
                            return send(refused(op, why, &detail));
                        }
                        if s.running.contains_key(&op) || s.queue.iter().any(|(o, _)| *o == op) {
                            return send(refused(op, Refusal::OpInUse, ""));
                        }
                        send(OpEvt::Accepted { op });
                        for item in missing {
                            send(OpEvt::ItemFailed { op, item, hr: NOT_FOUND });
                        }
                        s.obsolete.insert(op, obsolete);
                        s.queue.push_back((op, Work::Planned(OpKind::Resume, plan)));
                        self.start_next(&mut s);
                    }
                    Err(why) => send(refused(op, why, "")),
                }
            }
            OpReq::AckDone { of } => ack_done(&s.cfg, of),
            OpReq::Bye => {}
        }
        drop(s);
        self.changed();
    }

    /// Copy, Move, Recycle, DeletePermanent of the staged items: every check before Accepted, then the queue.
    fn queue_staged(
        &self,
        s: &mut State,
        op: OpId,
        kind: OpKind,
        dest: Option<WidePath>,
        conflicts: oled_trust::frame::files::Conflicts,
        send: &dyn Fn(OpEvt),
    ) {
        let Some(mut staged) = s.staged.remove(&op).filter(|st| !st.is_empty()) else {
            drop_staged(s, op);
            return send(refused(op, Refusal::NoItems, ""));
        };
        let roots = s.cfg.roots.clone();
        if let Some(d) = &dest {
            if let Err(r) = check_item(d, OpKind::Copy, roots.as_deref()) {
                staged.remove_spill();
                return send(refused(op, r, &name_text(d)));
            }
        }
        let mut first: Option<WidePath> = None;
        let mut bad: Option<(Refusal, String)> = None;
        match staged.chunks(4_096) {
            Ok(chunks) => {
                'all: for chunk in chunks {
                    let Ok(chunk) = chunk else {
                        bad = Some((Refusal::BadPath, String::new()));
                        break;
                    };
                    for item in &chunk {
                        if let Err(r) = check_item(item, kind, roots.as_deref()) {
                            bad = Some((r, name_text(item)));
                            break 'all;
                        }
                        if matches!(kind, OpKind::Recycle | OpKind::DeletePermanent) {
                            match &first {
                                None => first = Some(item.clone()),
                                Some(f) => {
                                    if same_parent(&[f.clone(), item.clone()]).is_err() {
                                        bad = Some((Refusal::MixedParents, name_text(item)));
                                        break 'all;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => bad = Some((Refusal::BadPath, String::new())),
        }
        if let Some((r, detail)) = bad {
            staged.remove_spill();
            return send(refused(op, r, &detail));
        }
        let job = OpJob { kind, dest, name: None, conflicts, staged };
        self.enqueue(s, op, Work::Staged(job), send);
    }

    fn enqueue(&self, s: &mut State, op: OpId, work: Work, send: &dyn Fn(OpEvt)) {
        if s.running.contains_key(&op) || s.queue.iter().any(|(o, _)| *o == op) {
            return send(refused(op, Refusal::OpInUse, ""));
        }
        send(OpEvt::Accepted { op });
        s.queue.push_back((op, work));
        self.start_next(s);
    }

    fn start_next(&self, s: &mut State) {
        while s.running.len() < MAX_RUNNING {
            let Some((op, work)) = s.queue.pop_front() else { break };
            let dest = match &work {
                Work::Staged(j) => j.dest.clone(),
                Work::Planned(_, p) => p.first().and_then(|c| c.dest.clone()),
            };
            let r = Running { kind: work.kind(), items: work.items(), done: 0, total: 0, needs_input: false, dest };
            s.running.insert(op, r);
            s.runner.start(op, work, self.events.clone());
        }
    }

    /// One event of a running operation: forwarded to an attached UI; Finished ends it.
    fn on_event(&self, e: OpEvt) {
        let mut s = self.lock();
        match &e {
            OpEvt::Progress { op, done, total, .. } => {
                if let Some(r) = s.running.get_mut(op) {
                    r.done = *done;
                    r.total = *total;
                }
            }
            OpEvt::NeedsInput { op, on } => {
                if let Some(r) = s.running.get_mut(op) {
                    r.needs_input = *on;
                }
            }
            _ => {}
        }
        if let Some(op) = run_event_op(&e) {
            retire(&mut s, op);
        }
        if let OpEvt::Finished { op, items, failed, .. } = &e {
            let (op, items, failed) = (*op, *items, *failed);
            let finished = s.running.remove(&op);
            for rel in s.obsolete.remove(&op).unwrap_or_default() {
                let _ = s.cfg.data.remove(&rel);
            }
            let rel = journal::journal_rel(op, s.cfg.pid);
            match s.ui.clone() {
                Some(ui) => {
                    forward(&ui, &e);
                    let _ = s.cfg.data.remove(&rel);
                }
                None => {
                    let (kind, dest) = finished.map_or((OpKind::Copy, None), |r| (r.kind, r.dest));
                    let entry = DoneEntry {
                        op: op.0,
                        kind: kind.into(),
                        items,
                        failed,
                        dest: dest.as_ref().map(PathRepr::of),
                        finished: journal::now_filetime(),
                    };
                    append_done(&s.cfg.data, entry);
                }
            }
            self.start_next(&mut s);
            drop(s);
            self.changed();
            return;
        }
        if let Some(ui) = &s.ui {
            forward(ui, &e);
        }
    }
}

fn forward(ui: &Sender<Vec<u8>>, e: &OpEvt) {
    let mut buf = Vec::new();
    if encode(e, &mut buf).is_ok() {
        let _ = ui.send(buf);
    }
}

fn build_text(b: &[u8; 16]) -> String {
    String::from_utf8_lossy(b).trim_end_matches('\0').to_string()
}

fn refused(op: OpId, why: Refusal, detail: &str) -> OpEvt {
    let mut end = detail.len().min(520);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    OpEvt::Refused { op, why, detail: detail.get(..end).unwrap_or_default().into() }
}

fn name_text(p: &WidePath) -> String {
    p.file_name().map(String::from_utf16_lossy).unwrap_or_else(|| p.to_string_lossy())
}

fn drop_staged(s: &mut State, op: OpId) {
    if let Some(mut st) = s.staged.remove(&op) {
        st.remove_spill();
    }
}

/// Whether a resume of this host, queued or running, covers the file `rel` (an interrupted operation's journal).
fn covered(s: &State, rel: &str) -> bool {
    s.obsolete.values().any(|files| files.iter().any(|f| f == rel))
}

/// The operation of an event run() sends after its journal step: anything but Started, Finished and the watcher's
/// NeedsInput.
fn run_event_op(e: &OpEvt) -> Option<OpId> {
    match e {
        OpEvt::Progress { op, .. }
        | OpEvt::ItemsDone { op, .. }
        | OpEvt::ItemFailed { op, .. }
        | OpEvt::UndoPart { op, .. }
        | OpEvt::Refused { op, .. } => Some(*op),
        _ => None,
    }
}

/// The interrupted operation's journal and spill go as soon as the resume `op` has its own journal on disk: run()
/// writes and flushes that journal's header and a "queued" record for every item it will run before it sends any
/// event after Started, so the new journal names everything still to do, and a host that dies during the resume
/// leaves one Pending, not two.
fn retire(s: &mut State, op: OpId) {
    if s.obsolete.contains_key(&op) && s.cfg.data.path().join(journal::journal_rel(op, s.cfg.pid)).is_file() {
        for rel in s.obsolete.remove(&op).unwrap_or_default() {
            let _ = s.cfg.data.remove(&rel);
        }
    }
}

/// Every item and destination of a plan passes the checks before it is queued.
fn check_plan(plan: &[Prepared], roots: Option<&[WidePath]>) -> Result<(), (Refusal, String)> {
    for c in plan {
        for item in &c.items {
            check_item(item, c.kind, roots).map_err(|r| (r, name_text(item)))?;
        }
        if let Some(d) = &c.dest {
            check_item(d, OpKind::Copy, roots).map_err(|r| (r, name_text(d)))?;
        }
    }
    Ok(())
}

/// A resume: its chunks, the items that cannot be found and the files it makes obsolete.
type ResumeWork = (Vec<Prepared>, Vec<WidePath>, Vec<String>);

/// The resume plan of the interrupted operation `of` (a journal without an end whose host is dead), the items
/// that cannot be found, and the files (journal, spill) the resume makes obsolete.
fn resume_plan(cfg: &HostConfig, op: OpId, of: OpId) -> Result<ResumeWork, Refusal> {
    // Only the journals of `of` are read, picked by their names.
    let mut found: Option<(u32, Journal)> = None;
    for (_, pid, name) in journal_names(&cfg.data, cfg.pid).into_iter().filter(|(o, _, _)| *o == of) {
        let Ok(bytes) = cfg.data.read(&format!("ops\\{name}")) else { continue };
        let Ok(j) = journal::parse(&bytes) else { continue };
        if j.end.is_some() || owner_alive(j.header.host_pid, j.header.host_created) {
            continue;
        }
        if found.as_ref().is_none_or(|(_, f)| j.header.started >= f.header.started) {
            found = Some((pid, j));
        }
    }
    let (pid, j) = found.ok_or(Refusal::UnknownOp)?;
    let stage = journal::stage_rel(of, pid);
    let spill = read_spill(cfg, &j, &stage)?;
    let (plan, missing) = super::resume::plan(op, &j, &spill, cfg.fs.as_ref())?;
    Ok((plan, missing, vec![journal::journal_rel(of, pid), stage]))
}

/// The stage spill of an interrupted operation: at most the items its journal says went past memory (never more than
/// the host's staging cap), so a file planted beside the journal cannot queue paths the operation never staged. A
/// spill of more entries or more bytes is damaged: the resume is refused. No spill file is no spill.
fn read_spill(cfg: &HostConfig, j: &Journal, stage: &str) -> Result<Vec<WidePath>, Refusal> {
    let expected = j.header.items.min(cfg.stage_max).saturating_sub(STAGE_IN_MEMORY as u64);
    let mut out = Vec::new();
    let mut r = match journal::SpillReader::open_expecting(&cfg.data, stage, expected) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Err(Refusal::BadPath),
        Err(_) => return Ok(out),
    };
    while let Ok(Some(p)) = r.next_path() {
        if out.len() as u64 >= expected {
            return Err(Refusal::BadPath);
        }
        out.push(p);
    }
    Ok(out)
}

/// Whether the process `pid` created at `created` (FILETIME) still runs. One that has exited is dead even while a
/// handle (a parent's, or Windows Error Reporting's after a crash) keeps its process object and its id.
pub fn owner_alive(pid: u32, created: u64) -> bool {
    // SAFETY: a plain open by id; the handle is closed below.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { return false };
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: a live process handle; four writable FILETIMEs.
    let ok = unsafe { GetProcessTimes(h, c, e, k, u) }.is_ok();
    let mut code = 0u32;
    // SAFETY: the same handle; `code` is writable. A host never exits with STILL_ACTIVE's value (its exit codes are
    // small), so that value means it runs.
    let running = unsafe { GetExitCodeProcess(h, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32;
    // SAFETY: the handle opened above, closed once.
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
    let at = (u64::from(t[0].dwHighDateTime) << 32) | u64::from(t[0].dwLowDateTime);
    ok && running && at == created
}

/// The journals in ops\ (op, host pid, file name) whose host is not this one, by their names: nothing is read.
fn journal_names(data: &OwnDir, own_pid: u32) -> Vec<(OpId, u32, String)> {
    let mut out = Vec::new();
    for name in data.list("ops").unwrap_or_default() {
        let name = name.to_string_lossy().into_owned();
        if let Some((op, pid)) = journal::parse_name(&name, "jsonl").filter(|(_, pid)| *pid != own_pid) {
            out.push((op, pid, name));
        }
    }
    out
}

/// Pending for every journal without an end whose host is dead, with its path in the data folder. Each journal is
/// read line by line (journal::summary: one line and one bit per item at a time), one of a live host only up to its
/// header.
fn pending(data: &OwnDir, own_pid: u32) -> Vec<(String, OpEvt)> {
    let mut out = Vec::new();
    for (op, _, name) in journal_names(data, own_pid) {
        let rel = format!("ops\\{name}");
        let Ok(mut r) = journal::reader(data, &rel) else { continue };
        let dead = |h: &journal::Header| !owner_alive(h.host_pid, h.host_created);
        let Some(j) = journal::summary(&mut r, dead).filter(|j| !j.ended) else { continue };
        let e = OpEvt::Pending {
            of: op,
            kind: j.header.kind.into(),
            done: j.done,
            total: u32::try_from(j.header.items).unwrap_or(u32::MAX),
            dest: j.header.dest.as_ref().and_then(PathRepr::to_path),
        };
        out.push((rel, e));
    }
    out
}

pub fn read_done(data: &OwnDir) -> Vec<DoneEntry> {
    data.read(OPS_DONE).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn append_done(data: &OwnDir, e: DoneEntry) {
    let mut all = read_done(data);
    all.push(e);
    if let Ok(bytes) = serde_json::to_vec(&all) {
        let _ = data.write_atomic(OPS_DONE, &bytes);
    }
}

/// AckDone: the entry leaves ops-done.json and its journal (and stage spill) are removed, whichever host wrote them.
fn ack_done(cfg: &HostConfig, of: OpId) {
    let mut all = read_done(&cfg.data);
    let before = all.len();
    all.retain(|d| d.op != of.0);
    if all.len() != before {
        if let Ok(bytes) = serde_json::to_vec(&all) {
            let _ = cfg.data.write_atomic(OPS_DONE, &bytes);
        }
    }
    for name in cfg.data.list("ops").unwrap_or_default() {
        let name = name.to_string_lossy().into_owned();
        let ours = ["jsonl", "stage"].iter().any(|ext| journal::parse_name(&name, ext).is_some_and(|(op, _)| op == of));
        if ours {
            let _ = cfg.data.remove(&format!("ops\\{name}"));
        }
    }
}

// ---- the real runner: OP3's run on one thread per operation, and the NeedsInput watcher --------------------------

struct OpHandle {
    cancel: Arc<AtomicBool>,
    thread: Arc<AtomicU32>,
    /// The visible window the operation waits on (a system dialog, or the shell's operation window), as a value. The
    /// thread is new and owns no window when the operation starts, so any visible top-level window of it is a dialog.
    dialog: Option<isize>,
}

/// What every run of the real runner shares.
pub struct RunBase {
    pub roots: Option<Vec<WidePath>>,
    pub test_no_ui: bool,
    pub dialogs_under_silent: bool,
    pub host_window: Option<isize>,
    pub journal: Option<JournalCtx>,
    pub attached: Arc<AtomicBool>,
    pub slow_post_ms: u32,
}

pub struct RealRunner {
    base: Arc<RunBase>,
    ops: Arc<Mutex<HashMap<OpId, OpHandle>>>,
    watching: Arc<AtomicBool>,
}

impl RealRunner {
    pub fn new(base: RunBase) -> RealRunner {
        RealRunner { base: Arc::new(base), ops: Arc::default(), watching: Arc::new(AtomicBool::new(false)) }
    }

    /// The NeedsInput watcher: every 500 ms while operations run, the visible top-level windows of this process, each
    /// counted for its operation's thread (dialog_of); it ends with the last operation, so an idle host has no timer.
    fn watch(&self, events: Sender<OpEvt>) {
        if self.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        let (ops, watching) = (self.ops.clone(), self.watching.clone());
        let spawned = std::thread::Builder::new().name("ops-watch".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let mut map = ops.lock().unwrap_or_else(|e| e.into_inner());
            if map.is_empty() {
                watching.store(false, Ordering::Release);
                return;
            }
            let windows = process_windows();
            let threads: Vec<u32> = map.values().map(|h| h.thread.load(Ordering::Acquire)).collect();
            for (op, h) in map.iter_mut() {
                let tid = h.thread.load(Ordering::Acquire);
                if tid == 0 {
                    continue;
                }
                let new = dialog_of(tid, &windows, &threads);
                match (new, h.dialog) {
                    (Some(w), None) => {
                        h.dialog = Some(w);
                        let _ = events.send(OpEvt::NeedsInput { op: *op, on: true });
                    }
                    (None, Some(_)) => {
                        h.dialog = None;
                        let _ = events.send(OpEvt::NeedsInput { op: *op, on: false });
                    }
                    _ => {}
                }
            }
        });
        if spawned.is_err() {
            self.watching.store(false, Ordering::Release);
        }
    }
}

/// The window that makes the operation on thread `tid` wait for the user: a visible window of that thread, else one of
/// a thread that runs no operation. The shell shows its own operation window on a thread of its own, and some Windows
/// builds ask about a collision there even under FOF_SILENT (FS-3 a on the disposable runner); such a window counts
/// for every running operation. `windows` are (window, thread) pairs, `op_threads` the running operations' threads.
/// Pure.
pub fn dialog_of(tid: u32, windows: &[(isize, u32)], op_threads: &[u32]) -> Option<isize> {
    let own = windows.iter().find(|(_, t)| *t == tid);
    own.or_else(|| windows.iter().find(|(_, t)| !op_threads.contains(t))).map(|(w, _)| *w)
}

/// The visible top-level windows of this process and their threads. The host shows no window of its own (its hidden
/// window is never shown), so each is a system dialog or the shell's operation window.
fn process_windows() -> Vec<(isize, u32)> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the address of the pair below, alive for the enumeration.
        let out = unsafe { &mut *(lparam.0 as *mut (u32, Vec<(isize, u32)>)) };
        let mut pid = 0u32;
        // SAFETY: a window handle the enumeration passes; `pid` is writable.
        let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // SAFETY: as above.
        if pid == out.0 && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            out.1.push((hwnd.0 as isize, tid));
        }
        BOOL(1)
    }
    let mut out: (u32, Vec<(isize, u32)>) = (std::process::id(), Vec::new());
    // SAFETY: the callback only pushes into `out`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(std::ptr::from_mut(&mut out) as isize)) };
    out.1
}

impl Runner for RealRunner {
    fn start(&mut self, op: OpId, work: Work, events: Sender<OpEvt>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = Arc::new(AtomicU32::new(0));
        let h = OpHandle { cancel: cancel.clone(), thread: thread.clone(), dialog: None };
        self.ops.lock().unwrap_or_else(|e| e.into_inner()).insert(op, h);
        let (base, ops) = (self.base.clone(), self.ops.clone());
        let ev = events.clone();
        let spawned = std::thread::Builder::new().name(format!("ops-run-{}", op.0)).spawn(move || {
            // SAFETY: no preconditions.
            thread.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
            let ctx = RunCtx {
                roots: base.roots.clone(),
                test_no_ui: base.test_no_ui,
                dialogs_under_silent: base.dialogs_under_silent,
                ui_owner: None,
                host_window: base.host_window,
                journal: base.journal.clone(),
                cancel,
                attached: base.attached.clone(),
                events: ev,
                stats: Arc::new(RunStats::default()),
                hook_post: None,
                slow_post_ms: base.slow_post_ms,
                bin_off: crate::ops::classify::bin_turned_off,
            };
            run(op, work, &ctx);
            ops.lock().unwrap_or_else(|e| e.into_inner()).remove(&op);
        });
        match spawned {
            Ok(_) => self.watch(events),
            Err(_) => {
                self.ops.lock().unwrap_or_else(|e| e.into_inner()).remove(&op);
                let _ = events.send(OpEvt::Finished {
                    op,
                    result: OpResult::Failed,
                    undo: None,
                    replaced: 0,
                    items: 0,
                    failed: 0,
                });
            }
        }
    }

    fn cancel(&mut self, op: OpId) {
        if let Some(h) = self.ops.lock().unwrap_or_else(|e| e.into_inner()).get(&op) {
            h.cancel.store(true, Ordering::Release);
        }
    }

    fn pause(&mut self, _op: OpId, _on: bool) -> bool {
        PAUSE_OK
    }

    fn bring_forward(&mut self, op: OpId) {
        let dialog = self.ops.lock().unwrap_or_else(|e| e.into_inner()).get(&op).and_then(|h| h.dialog);
        if let Some(w) = dialog {
            // SAFETY: a window of this process's operation thread; the UI allowed this process to take the
            // foreground (AllowSetForegroundWindow) before asking.
            let _ = unsafe { SetForegroundWindow(HWND(w as *mut std::ffi::c_void)) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_of_its_thread_or_of_none() {
        // Two operations on threads 10 and 20; the shell's operation window on thread 30.
        let threads = [10, 20];
        assert_eq!(dialog_of(10, &[(1, 10), (2, 30)], &threads), Some(1), "its own thread's window first");
        assert_eq!(dialog_of(20, &[(1, 10)], &threads), None, "another operation's dialog is not its own");
        assert_eq!(dialog_of(20, &[(1, 10), (2, 30)], &threads), Some(2), "a window of no operation counts for each");
        assert_eq!(dialog_of(10, &[], &threads), None);
    }
}
