//! Request dispatch (OP5b) over the in-memory FrameIo with a fake runner and a TempRoot data folder (plain).

mod common;
mod ops_util;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::host::named::{mem_pair, FrameIo, MemIo};
use files_engine::ops::host::{read_done, DoneEntry, Host, HostConfig, Runner, Work, OPS_DONE};
use files_engine::ops::journal::{
    journal_rel, EndKind, Header, JConflicts, JKind, JournalWriter, PathRepr, Pre, Record, St, VERSION,
};
use files_engine::own_files::OwnDir;
use oled_trust::frame::files::{Conflicts, ItemRef, OpEvt, OpId, OpKind, OpReq, OpResult, Refusal, UndoKind, OPS};
use oled_trust::frame::{decode, decode_hello, encode, encode_hello};
use ops_util::wp;

/// A dead host's pid (no process has it).
const DEAD_PID: u32 = 0xFFFF_FFF0;

#[derive(Default)]
struct FakeState {
    started: Vec<OpId>,
    kinds: HashMap<OpId, OpKind>,
    senders: HashMap<OpId, Sender<OpEvt>>,
    cancelled: Vec<OpId>,
}

/// A runner that runs nothing: it records what it was asked and finishes on command.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Runner for Fake {
    fn start(&mut self, op: OpId, work: Work, events: Sender<OpEvt>) {
        let mut s = self.0.lock().unwrap();
        s.started.push(op);
        s.kinds.insert(op, work.kind());
        s.senders.insert(op, events);
    }
    fn cancel(&mut self, op: OpId) {
        self.0.lock().unwrap().cancelled.push(op);
    }
    fn pause(&mut self, _op: OpId, _on: bool) -> bool {
        false
    }
}

impl Fake {
    fn started(&self) -> Vec<OpId> {
        self.0.lock().unwrap().started.clone()
    }
    fn event(&self, op: OpId, e: OpEvt) {
        let tx = self.0.lock().unwrap().senders.get(&op).cloned().expect("a started op");
        tx.send(e).unwrap();
    }
    fn finish(&self, op: OpId, items: u32) {
        let tx = self.0.lock().unwrap().senders.get(&op).cloned().expect("a started op");
        let undo = Some(UndoKind::RecycleCopies);
        tx.send(OpEvt::Finished { op, result: OpResult::Done, undo, replaced: 0, items, failed: 0 }).unwrap();
    }
}

/// The UI's end of an in-memory connection, its events read by a thread.
struct Ui {
    io: MemIo,
    rx: Receiver<OpEvt>,
}

impl Ui {
    fn send(&mut self, req: OpReq) {
        let mut buf = Vec::new();
        encode(&req, &mut buf).unwrap();
        self.io.send_frame(&buf).unwrap();
    }
    fn next(&self) -> OpEvt {
        self.rx.recv_timeout(Duration::from_secs(10)).expect("an event within 10 s")
    }
    fn none(&self) {
        assert!(self.rx.recv_timeout(Duration::from_millis(100)).is_err(), "no further event");
    }
    fn attach(&mut self) -> Vec<OpEvt> {
        self.send(OpReq::Attach { ui_pid: std::process::id(), owner: 0 });
        let mut out = vec![self.next()];
        assert!(matches!(out[0], OpEvt::Snapshot { .. }), "{:?}", out[0]);
        while let Ok(e) = self.rx.recv_timeout(Duration::from_millis(100)) {
            out.push(e);
        }
        out
    }
}

struct Rig {
    root: TempRoot,
    data: OwnDir,
    host: Arc<Host>,
    fake: Fake,
}

fn rig(tag: &str, tweak: impl FnOnce(&mut HostConfig)) -> Rig {
    let root = TempRoot::new(tag);
    let data = OwnDir::open(&root.dir("data")).unwrap();
    let mut cfg = HostConfig::new(data.clone(), std::process::id(), 1, Some(vec![wp(root.path())]));
    tweak(&mut cfg);
    let fake = Fake::default();
    let host = Host::new(cfg, Box::new(fake.clone()), Arc::new(AtomicBool::new(false)));
    Rig { root, data, host, fake }
}

impl Rig {
    /// A UI connected (Hello both ways), not yet attached.
    fn connect(&self) -> Ui {
        let (host_end, mut ui_end) = mem_pair();
        let h = self.host.clone();
        std::thread::spawn(move || h.serve(host_end));
        let hello = ui_end.recv_frame().unwrap().unwrap();
        decode_hello(&OPS, &hello).unwrap();
        let mut mine = Vec::new();
        encode_hello(&OPS, files_engine::BUILD_ID, &mut mine);
        ui_end.send_frame(&mine).unwrap();
        let (tx, rx) = channel();
        let mut reader = ui_end.clone();
        std::thread::spawn(move || {
            while let Ok(Some(f)) = reader.recv_frame() {
                if tx.send(decode::<OpEvt>(&f).unwrap().0).is_err() {
                    break;
                }
            }
        });
        Ui { io: ui_end, rx }
    }

    fn item(&self, rel: &str) -> ItemRef {
        ItemRef::Path(wp(&self.root.file(rel, b"x")))
    }

    /// Stage one file and Copy it: Accepted.
    fn copy(&self, ui: &mut Ui, op: u32) {
        ui.send(OpReq::Stage { op: OpId(op), items: vec![self.item(&format!("src/{op}.txt"))] });
        ui.send(OpReq::Copy { op: OpId(op), dest: wp(&self.root.dir("dst")), conflicts: Conflicts::AskSystem });
        assert_eq!(ui.next(), OpEvt::Accepted { op: OpId(op) });
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn header(op: u32, pid: u32, kind: JKind, items: u64) -> Header {
    Header {
        v: VERSION,
        op,
        kind,
        host_pid: pid,
        host_created: 1,
        dest: Some(PathRepr::Text("C:\\dst".into())),
        conflicts: JConflicts::Ask,
        items,
        started: 1,
    }
}

fn rec(i: u32, st: St) -> Record {
    Record { i, src: PathRepr::Text(format!("C:\\src\\{i}")), dst: None, pre: Pre::Absent, st, size: 0, mtime: 0 }
}

#[test]
fn attach_gives_snapshot_pending_and_done() {
    let r = rig("host_attach", |_| {});
    let now = Instant::now();
    let mut j = JournalWriter::create(&r.data, OpId(7), DEAD_PID, &header(7, DEAD_PID, JKind::Copy, 5), now).unwrap();
    j.record(&rec(0, St::Queued), now).unwrap();
    j.record(&rec(0, St::Done), now).unwrap();
    j.record(&rec(1, St::Started), now).unwrap();
    drop(j);
    let mut ended =
        JournalWriter::create(&r.data, OpId(8), DEAD_PID, &header(8, DEAD_PID, JKind::Move, 1), now).unwrap();
    ended.end(EndKind::Cancelled, now).unwrap();
    drop(ended);
    let done = vec![DoneEntry { op: 9, kind: JKind::Move, items: 3, failed: 1, dest: None, finished: 1 }];
    r.data.write_atomic(OPS_DONE, &serde_json::to_vec(&done).unwrap()).unwrap();
    let mut ui = r.connect();
    let evs = ui.attach();
    assert_eq!(evs[0], OpEvt::Snapshot { running: vec![] });
    let dest = Some(oled_trust::frame::files::WidePath::parse("C:\\dst").unwrap());
    assert!(evs.contains(&OpEvt::Pending { of: OpId(7), kind: OpKind::Copy, done: 1, total: 5, dest }), "{evs:?}");
    assert!(!evs.iter().any(|e| matches!(e, OpEvt::Pending { of: OpId(8), .. })), "an ended journal is not pending");
    let away = OpEvt::DoneWhileAway { of: OpId(9), kind: OpKind::Move, items: 3, failed: 1, dest: None };
    assert!(evs.contains(&away), "{evs:?}");
    assert!(r.host.is_attached());
}

#[test]
fn one_resume_per_interrupted_op() {
    let r = rig("host_resume_once", |_| {});
    let src = wp(&r.root.file("src/a.txt", b"a"));
    let h = Header { dest: Some(PathRepr::of(&wp(&r.root.dir("dst")))), ..header(7, DEAD_PID, JKind::Copy, 1) };
    let now = Instant::now();
    let mut j = JournalWriter::create(&r.data, OpId(7), DEAD_PID, &h, now).unwrap();
    j.record(&Record { src: PathRepr::of(&src), ..rec(0, St::Queued) }, now).unwrap();
    drop(j);
    let old = journal_rel(OpId(7), DEAD_PID);
    let pending7 = |evs: &[OpEvt]| evs.iter().any(|e| matches!(e, OpEvt::Pending { of: OpId(7), .. }));
    let mut ui = r.connect();
    assert!(pending7(&ui.attach()));
    ui.send(OpReq::ResumeRest { op: OpId(20), of: OpId(7) });
    assert_eq!(ui.next(), OpEvt::Accepted { op: OpId(20) });
    ui.send(OpReq::ResumeRest { op: OpId(21), of: OpId(7) });
    assert!(
        matches!(ui.next(), OpEvt::Refused { op: OpId(21), why: Refusal::OpInUse, .. }),
        "a second resume of the same operation"
    );
    assert_eq!(r.fake.started(), [OpId(20)], "one resume runs");
    assert!(!pending7(&ui.attach()), "not offered again while it is resumed");
    // Without its own journal on disk the resume keeps the interrupted one.
    let progress = OpEvt::Progress { op: OpId(20), done: 0, total: 1, bps: 0, current: "".into() };
    r.fake.event(OpId(20), progress.clone());
    assert_eq!(ui.next(), progress);
    assert!(r.data.read(&old).is_ok(), "kept while the resume has no journal");
    // With it, the interrupted journal goes at the resume's next event, and nothing is left to resume twice.
    let own = std::process::id();
    drop(JournalWriter::create(&r.data, OpId(20), own, &header(20, own, JKind::Copy, 1), Instant::now()).unwrap());
    r.fake.event(OpId(20), progress.clone());
    assert_eq!(ui.next(), progress);
    assert!(r.data.read(&old).is_err(), "the resume's own journal names everything still to do");
    ui.send(OpReq::ResumeRest { op: OpId(22), of: OpId(7) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(22), why: Refusal::UnknownOp, .. }));
    r.fake.finish(OpId(20), 1);
    assert!(matches!(ui.next(), OpEvt::Finished { op: OpId(20), .. }));
    assert_eq!(r.fake.started(), [OpId(20)]);
}

/// Writes the stage spill of the dead host's operation `op`: `paths` in the writer's layout.
fn write_spill(r: &Rig, op: u32, paths: &[&str]) {
    use std::io::Write;
    let mut f = r.data.append(&files_engine::ops::journal::stage_rel(OpId(op), DEAD_PID)).unwrap();
    for p in paths {
        let units: Vec<u16> = p.encode_utf16().collect();
        f.write_all(&(units.len() as u16).to_le_bytes()).unwrap();
        for u in units {
            f.write_all(&u.to_le_bytes()).unwrap();
        }
    }
}

#[test]
fn resume_refuses_a_spill_longer_than_its_journal() {
    let r = rig("host_spill_long", |_| {});
    let spilled = wp(&r.root.file("src/spilled.txt", b"a"));
    let first = wp(&r.root.file("src/first.txt", b"a"));
    let dest = PathRepr::of(&wp(&r.root.dir("dst")));
    let stage_in_memory = files_engine::ops::consts::STAGE_IN_MEMORY as u64;
    // The journal says one item went to the spill; an honest spill holds one.
    let mk = |op: u32, items: u64, paths: &[&str]| {
        let h = Header { dest: Some(dest.clone()), ..header(op, DEAD_PID, JKind::Copy, items) };
        let now = Instant::now();
        let mut j = JournalWriter::create(&r.data, OpId(op), DEAD_PID, &h, now).unwrap();
        j.record(&Record { src: PathRepr::of(&first), ..rec(0, St::Queued) }, now).unwrap();
        drop(j);
        write_spill(&r, op, paths);
    };
    let mut ui = r.connect();
    ui.attach();
    let spilled_path = spilled.to_string_lossy();
    mk(7, stage_in_memory + 1, &[&spilled_path]);
    ui.send(OpReq::ResumeRest { op: OpId(20), of: OpId(7) });
    assert_eq!(ui.next(), OpEvt::Accepted { op: OpId(20) }, "a spill of the journal's size resumes");
    // Entries the journal never staged: the resume is refused, nothing is queued from them.
    mk(8, stage_in_memory + 1, &[&spilled_path, &spilled_path, &spilled_path]);
    ui.send(OpReq::ResumeRest { op: OpId(21), of: OpId(8) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(21), why: Refusal::BadPath, .. }), "more entries");
    // A spill beside a journal that staged nothing outside memory.
    mk(9, 1, &[&spilled_path]);
    ui.send(OpReq::ResumeRest { op: OpId(22), of: OpId(9) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(22), why: Refusal::BadPath, .. }), "no spill expected");
    assert_eq!(r.fake.started(), [OpId(20)]);
}

#[test]
fn stage_cap_refuses_too_many() {
    let r = rig("host_stagecap", |c| c.stage_max = 5_000);
    let mut ui = r.connect();
    ui.attach();
    let item = r.item("src/a.txt");
    for _ in 0..2 {
        ui.send(OpReq::Stage { op: OpId(1), items: vec![item.clone(); 2_048] });
    }
    ui.none();
    ui.send(OpReq::Stage { op: OpId(1), items: vec![item.clone(); 2_048] });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(1), why: Refusal::TooMany, .. }));
    ui.send(OpReq::Copy { op: OpId(1), dest: wp(&r.root.dir("dst")), conflicts: Conflicts::AskSystem });
    assert!(
        matches!(ui.next(), OpEvt::Refused { op: OpId(1), why: Refusal::NoItems, .. }),
        "the staged list was dropped"
    );
    assert!(r.fake.started().is_empty());
    assert_eq!(files_engine::ops::consts::STAGE_MAX, 1_000_000);
    assert!(r.data.list("ops").map(|l| l.is_empty()).unwrap_or(true), "the spill is gone");
}

#[test]
fn checks_refuse_before_queue() {
    let r = rig("host_checks", |_| {});
    let mut ui = r.connect();
    ui.attach();
    let outside = ItemRef::Path(oled_trust::frame::files::WidePath::parse("C:\\mod-files-absent\\x").unwrap());
    ui.send(OpReq::Stage { op: OpId(1), items: vec![outside] });
    ui.send(OpReq::DeletePermanent { op: OpId(1) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(1), why: Refusal::OutsideTestRoot, .. }));
    let root = ItemRef::Path(oled_trust::frame::files::WidePath::parse("C:\\").unwrap());
    ui.send(OpReq::Stage { op: OpId(2), items: vec![root] });
    ui.send(OpReq::Recycle { op: OpId(2) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(2), why: Refusal::Root, .. }));
    ui.send(OpReq::Stage { op: OpId(3), items: vec![r.item("src/a.txt")] });
    let out = oled_trust::frame::files::WidePath::parse("C:\\mod-files-absent").unwrap();
    ui.send(OpReq::Copy { op: OpId(3), dest: out, conflicts: Conflicts::AskSystem });
    assert!(
        matches!(ui.next(), OpEvt::Refused { op: OpId(3), why: Refusal::OutsideTestRoot, .. }),
        "the destination too"
    );
    ui.send(OpReq::Stage { op: OpId(4), items: vec![r.item("a/x.txt"), r.item("b/y.txt")] });
    ui.send(OpReq::DeletePermanent { op: OpId(4) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(4), why: Refusal::MixedParents, .. }));
    ui.send(OpReq::Rename { op: OpId(5), item: wp(&r.root.file("n.txt", b"n")), to: "a:b".into() });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(5), why: Refusal::BadName, .. }));
    ui.send(OpReq::Stage { op: OpId(6), items: vec![ItemRef::Pidl(Box::new([1, 2, 3]))] });
    assert!(
        matches!(ui.next(), OpEvt::Refused { op: OpId(6), why: Refusal::BadPath, .. }),
        "a PIDL is never operated on"
    );
    ui.send(OpReq::Recycle { op: OpId(7) });
    assert!(matches!(ui.next(), OpEvt::Refused { op: OpId(7), why: Refusal::NoItems, .. }));
    ui.none();
    assert!(r.fake.started().is_empty(), "nothing reached the runner");
}

#[test]
fn fifo_at_most_four() {
    let r = rig("host_fifo", |_| {});
    let mut ui = r.connect();
    ui.attach();
    for op in 1..=6 {
        r.copy(&mut ui, op);
    }
    assert_eq!(r.fake.started(), [OpId(1), OpId(2), OpId(3), OpId(4)], "four run, two wait");
    r.fake.finish(OpId(2), 1);
    assert!(matches!(ui.next(), OpEvt::Finished { op: OpId(2), .. }));
    wait_for("op 5 to start", || r.fake.started().len() == 5);
    r.fake.finish(OpId(1), 1);
    wait_for("op 6 to start", || r.fake.started().len() == 6);
    assert_eq!(r.fake.started(), [1, 2, 3, 4, 5, 6].map(OpId), "FIFO");
    ui.send(OpReq::Attach { ui_pid: 1, owner: 0 });
    loop {
        if let OpEvt::Snapshot { running } = ui.next() {
            let mut ops: Vec<u32> = running.iter().map(|s| s.op.0).collect();
            ops.sort();
            assert_eq!(ops, [3, 4, 5, 6]);
            break;
        }
    }
}

#[test]
fn ack_done_removes() {
    let r = rig("host_ack", |_| {});
    let done = vec![
        DoneEntry { op: 9, kind: JKind::Copy, items: 1, failed: 0, dest: None, finished: 1 },
        DoneEntry { op: 10, kind: JKind::Copy, items: 2, failed: 0, dest: None, finished: 1 },
    ];
    r.data.write_atomic(OPS_DONE, &serde_json::to_vec(&done).unwrap()).unwrap();
    let mut j = JournalWriter::create(&r.data, OpId(9), DEAD_PID, &header(9, DEAD_PID, JKind::Copy, 1), Instant::now())
        .unwrap();
    j.end(EndKind::Done, Instant::now()).unwrap();
    drop(j);
    let mut ui = r.connect();
    ui.attach();
    ui.send(OpReq::AckDone { of: OpId(9) });
    ui.attach();
    assert_eq!(read_done(&r.data).iter().map(|d| d.op).collect::<Vec<_>>(), [10]);
    assert!(
        !r.data.list("ops").unwrap().iter().any(|n| n.to_string_lossy().starts_with("9-")),
        "its journal is removed"
    );
}

#[test]
fn bye_keeps_ops_running() {
    let r = rig("host_bye", |_| {});
    let mut ui = r.connect();
    ui.attach();
    r.copy(&mut ui, 1);
    ui.send(OpReq::Bye);
    wait_for("the detach", || !r.host.is_attached());
    assert!(r.fake.0.lock().unwrap().cancelled.is_empty(), "nothing was cancelled");
    assert!(r.host.busy(), "the operation still runs");
    r.fake.finish(OpId(1), 1);
    wait_for("ops-done.json", || !read_done(&r.data).is_empty());
    assert!(!r.host.busy());
}

#[test]
fn finished_without_ui_goes_to_ops_done() {
    let r = rig("host_away", |_| {});
    let own = std::process::id();
    let mut ui = r.connect();
    ui.attach();
    for op in [1, 2] {
        let j =
            JournalWriter::create(&r.data, OpId(op), own, &header(op, own, JKind::Copy, 1), Instant::now()).unwrap();
        drop(j);
        r.copy(&mut ui, op);
    }
    r.fake.finish(OpId(1), 1);
    assert!(matches!(ui.next(), OpEvt::Finished { op: OpId(1), .. }), "delivered to the attached UI");
    wait_for("journal 1 removed", || {
        !r.data.list("ops").unwrap().iter().any(|n| *n == *journal_rel(OpId(1), own).trim_start_matches("ops\\"))
    });
    ui.send(OpReq::Bye);
    wait_for("the detach", || !r.host.is_attached());
    r.fake.finish(OpId(2), 1);
    wait_for("ops-done.json", || !read_done(&r.data).is_empty());
    let d = read_done(&r.data);
    assert_eq!((d[0].op, d[0].kind, d[0].items), (2, JKind::Copy, 1));
    assert!(r.data.read(&journal_rel(OpId(2), own)).is_ok(), "the journal is kept until AckDone");
    let mut ui = r.connect();
    let evs = ui.attach();
    assert!(evs.iter().any(|e| matches!(e, OpEvt::DoneWhileAway { of: OpId(2), items: 1, .. })), "{evs:?}");
}
