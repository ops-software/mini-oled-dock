//! Undo and redo (P3-files-app F3.3): one stack per process, pushed by the helper's undo records, refused with the
//! helper's reasons, redo through the same flows (a restore's redo classifies again). Plain tests over the fakes
//! through the kit's Harness; requests pass the gate inside a TempRoot and reach only FakeOps.

mod common;

use std::rc::Rc;
use std::sync::Arc;

use common::temp::TempRoot;
use files_engine::bar;
use files_engine::fakes::{self, Fakes, OpsCall, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::listing::WidePath;
use files_engine::ops::classify::{NoBin, Recyclability};
use files_engine::ops_client::{OpsEvent, UndoRecord};
use files_engine::ops_ui::{self, MutReq};
use files_engine::undo::{UndoEntry, UndoStack, UNDO_CAP};
use files_engine::window::{FilesScreen, Shared, WinKind, WinRec};
use oled_trust::frame::files::{
    ItemRef, Location, OpEvt, OpId, OpKind, OpResult, Refusal, ShellReq, UndoEntry as Entry, UndoKind,
};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::{KeySet, Selection};
use oled_ui::{CommandTable, Event, EventCx, LayoutCx, PaintCx, Reply, Screen, SizeL, SizeP, Tokens, UiLang};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

enum Op {
    Req(MutReq),
    Select(Vec<&'static str>),
}

struct Wrap(FilesScreen);

impl Screen for Wrap {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.0.layout(cx, avail)
    }
    fn paint(&self, cx: &mut PaintCx) {
        self.0.paint(cx)
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.0.event(cx, ev)
    }
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        match msg.downcast::<Op>() {
            Ok(op) => {
                match *op {
                    Op::Req(r) => {
                        ops_ui::request(&mut self.0, cx, r);
                    }
                    Op::Select(names) => {
                        let t = self.0.tabs_mut().active_mut();
                        let mut keys = KeySet::new();
                        for n in names {
                            if let Some(id) = t.table.model().listing().find(n.as_bytes()) {
                                keys.insert(files_engine::model::key_of(id));
                            }
                        }
                        t.table.set_selection(cx, Selection::Keys(keys));
                    }
                }
                Reply::Relayout
            }
            Err(msg) => self.0.data(cx, msg),
        }
    }
    fn commands(&self) -> &CommandTable {
        self.0.commands()
    }
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

struct Win {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<Wrap>,
    dir: WidePath,
    _root: TempRoot,
}

fn listing() -> files_engine::listing::Batch {
    fakes::listing(&[
        ("plans", true, 0, day(2)),
        ("a-notes.txt", false, 1_200, day(1)),
        ("b-report-final-v2.pdf", false, 3_400_000, day(5)),
    ])
}

fn screen_for(shared: &Rc<Shared>, f: &Fakes, win: u32, dir: &WidePath) -> Harness<Wrap> {
    f.enumr.queue(dir, listing());
    shared.windows.borrow_mut().push(WinRec {
        win,
        kind: WinKind::Main,
        wref: None,
        hwnd: None,
        location: Some(Location::Fs(dir.clone())),
    });
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(shared), win, Location::Fs(dir.clone()), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, win);
    h.render();
    h
}

fn in_root(tag: &str) -> Win {
    let root = TempRoot::new(tag);
    let dir = wp(&root.path().to_string_lossy());
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    assert_eq!(shared.new_win(), 1);
    let h = screen_for(&shared, &f, 1, &dir);
    Win { f, shared, h, dir, _root: root }
}

impl Win {
    fn path(&self, name: &str) -> WidePath {
        self.dir.join(&name.encode_utf16().collect::<Vec<u16>>()).unwrap()
    }
    fn req(&mut self, r: MutReq) {
        self.h.send(Box::new(Op::Req(r)));
        self.h.render();
    }
    fn key(&mut self, k: &str) {
        self.h.key(k);
        self.h.render();
    }
    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }
    fn emit(&self, e: OpsEvent) {
        self.f.ops.emit(e);
    }
    fn calls(&self) -> Vec<OpsCall> {
        self.f.ops.calls()
    }
    fn has_label(&self, text: &str) -> bool {
        self.h.a11y_dump().lines().any(|l| l.trim_start() == format!("Label {text:?}"))
    }
    fn undo_len(&self) -> usize {
        self.shared.undo.borrow().len()
    }
    fn redo_len(&self) -> usize {
        self.shared.undo.borrow().redo_len()
    }
    /// The helper renames a to b as op: its record, then its Finished.
    fn renamed(&mut self, op: u32, from: &str, to: &str) {
        let rec = UndoRecord {
            of: OpId(op),
            kind: UndoKind::RenameBack,
            replaced: 0,
            entries: vec![entry(self.path(from), self.path(to))],
        };
        self.emit(OpsEvent::Undo(rec));
        self.emit(done(op, Some(UndoKind::RenameBack)));
        self.pump();
    }
}

fn entry(from: WidePath, to: WidePath) -> Entry {
    Entry { from, to, size: 1, mtime: 0, dir: false, bin: None }
}

fn done(op: u32, undo: Option<UndoKind>) -> OpsEvent {
    OpsEvent::Evt(OpEvt::Finished { op: OpId(op), result: OpResult::Done, undo, replaced: 0, items: 1, failed: 0 })
}

fn rename_a_to_b(w: &mut Win) {
    let item = w.path("a-notes.txt");
    w.req(MutReq::Rename { item, to: "b-notes.txt".into() });
    assert_eq!(w.calls().last(), Some(&OpsCall::Rename(w.path("a-notes.txt"), "b-notes.txt".into())));
}

#[test]
fn push_only_with_undo_record() {
    let mut w = in_root("undo_push");
    rename_a_to_b(&mut w);
    // Finished without a record: nothing to undo.
    w.emit(done(1, None));
    w.pump();
    assert_eq!(w.undo_len(), 0);
    // With its record: one entry, labelled by its kind.
    rename_a_to_b(&mut w);
    w.renamed(2, "a-notes.txt", "b-notes.txt");
    assert_eq!(w.undo_len(), 1);
    assert_eq!(w.shared.undo.borrow().label(UiLang::EN).as_deref(), Some("Undo Rename"));
    assert_eq!(
        w.shared.undo.borrow().label(UiLang { italian: true, pseudo: false }).as_deref(),
        Some("Annulla Rinomina")
    );
    // A record for an operation this process did not ask for is not pushed.
    w.emit(OpsEvent::Undo(UndoRecord { of: OpId(99), kind: UndoKind::RenameBack, replaced: 0, entries: Vec::new() }));
    w.pump();
    assert_eq!(w.undo_len(), 1);
    // A new folder and a recycle label as theirs.
    w.req(MutReq::NewFolder { parent: w.dir.clone(), name: "x".into() });
    let made = w.path("x");
    w.emit(OpsEvent::Undo(UndoRecord {
        of: OpId(3),
        kind: UndoKind::RecycleNewFolder,
        replaced: 0,
        entries: vec![entry(made.clone(), made)],
    }));
    w.pump();
    assert_eq!(w.shared.undo.borrow().label(UiLang::EN).as_deref(), Some("Undo New folder"));
    w.req(MutReq::Recycle(vec![w.path("b-report-final-v2.pdf")]));
    w.emit(OpsEvent::Undo(UndoRecord {
        of: OpId(4),
        kind: UndoKind::OpenRecycleBin,
        replaced: 0,
        entries: Vec::new(),
    }));
    w.pump();
    assert_eq!(w.shared.undo.borrow().label(UiLang::EN).as_deref(), Some("Undo Delete"));
}

#[test]
fn cap_50() {
    let mut s = UndoStack::new();
    assert_eq!(s.label(UiLang::EN), None);
    for i in 0..(UNDO_CAP as u32 + 5) {
        let record = UndoRecord { of: OpId(i), kind: UndoKind::RenameBack, replaced: 0, entries: Vec::new() };
        s.push(UndoEntry { op: OpId(i), kind: OpKind::Rename, record });
    }
    assert_eq!(s.len(), UNDO_CAP);
    assert_eq!(s.top_op(), Some(OpId(UNDO_CAP as u32 + 4)));
    let mut ops = Vec::new();
    while let Some(e) = s.pop() {
        ops.push(e.op.0);
    }
    assert_eq!(ops.last(), Some(&5), "the five oldest went");
}

#[test]
fn undo_rename_sends_undo() {
    let mut w = in_root("undo_rename");
    rename_a_to_b(&mut w);
    w.renamed(1, "a-notes.txt", "b-notes.txt");
    w.key("Ctrl+Z");
    assert_eq!(w.calls().last(), Some(&OpsCall::Undo(OpId(1))));
    assert_eq!(w.undo_len(), 0, "the entry is taken");
    let m = w.shared.ops.borrow();
    let r = m.row(OpId(2)).expect("the undo is an operation");
    assert_eq!(r.kind, OpKind::Undo);
    assert_eq!(r.undo_of.as_ref().map(|u| u.of), Some(OpId(1)));
}

#[test]
fn refusal_texts() {
    let mut w = in_root("undo_refusals");
    w.key("Ctrl+Z");
    assert!(w.has_label("Nothing to undo"), "{}", w.h.a11y_dump());
    let cases: [(Refusal, &str, u32, &str); 3] = [
        (Refusal::Changed, "b-notes.txt", 0, "Can't undo: b-notes.txt changed since"),
        (Refusal::Replaced, "", 3, "Can't undo: 3 items replaced older files"),
        (Refusal::NotEmpty, "plans", 0, "Can't undo: plans isn't empty any more"),
    ];
    for (i, (why, detail, replaced, text)) in cases.into_iter().enumerate() {
        let op = 1 + 2 * i as u32;
        rename_a_to_b(&mut w);
        let rec = UndoRecord {
            of: OpId(op),
            kind: UndoKind::RenameBack,
            replaced,
            entries: vec![entry(w.path("a-notes.txt"), w.path("b-notes.txt"))],
        };
        w.emit(OpsEvent::Undo(rec));
        w.emit(done(op, Some(UndoKind::RenameBack)));
        w.pump();
        w.key("Ctrl+Z");
        assert_eq!(w.calls().last(), Some(&OpsCall::Undo(OpId(op))));
        w.emit(OpsEvent::Evt(OpEvt::Refused { op: OpId(op + 1), why, detail: detail.into() }));
        w.pump();
        assert!(w.has_label(text), "{why:?}: {}", w.h.a11y_dump());
        assert!(w.shared.ops.borrow().row(OpId(op + 1)).is_none(), "a refused undo leaves the model");
    }
}

#[test]
fn open_recycle_bin_kind_hands_off() {
    let mut w = in_root("undo_open_bin");
    w.req(MutReq::Recycle(vec![w.path("a-notes.txt")]));
    w.emit(OpsEvent::Undo(UndoRecord {
        of: OpId(1),
        kind: UndoKind::OpenRecycleBin,
        replaced: 0,
        entries: Vec::new(),
    }));
    w.emit(done(1, Some(UndoKind::OpenRecycleBin)));
    w.pump();
    assert_eq!(w.undo_len(), 1);
    let before = w.calls().len();
    w.key("Ctrl+Z");
    assert_eq!(w.calls().len(), before, "nothing goes to the helper");
    assert_eq!(w.undo_len(), 0, "the entry is consumed");
    // The Recycle Bin opens in File Explorer through the shell host (F4.13).
    let runs: Vec<String> =
        w.f.shell
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                ShellReq::RunTyped { text, .. } => Some(text.to_string()),
                _ => None,
            })
            .collect();
    assert_eq!(runs, [files_engine::handoff::RECYCLE_BIN]);
}

#[test]
fn redo_after_undo() {
    let mut w = in_root("undo_redo");
    rename_a_to_b(&mut w);
    w.renamed(1, "a-notes.txt", "b-notes.txt");
    w.key("Ctrl+Z");
    assert_eq!(w.redo_len(), 0, "redo waits for the undo's success");
    w.emit(done(2, None));
    w.pump();
    assert_eq!(w.redo_len(), 1);
    assert_eq!(w.shared.undo.borrow().redo_label(UiLang::EN).as_deref(), Some("Redo"));
    w.key("Ctrl+Y");
    assert_eq!(w.calls().last(), Some(&OpsCall::Rename(w.path("a-notes.txt"), "b-notes.txt".into())));
    assert_eq!(w.redo_len(), 0);
    // The redo's own record makes it undoable again; Ctrl+Shift+Z is redo too.
    w.renamed(3, "a-notes.txt", "b-notes.txt");
    w.key("Ctrl+Z");
    w.emit(done(4, None));
    w.pump();
    w.key("Ctrl+Shift+Z");
    assert_eq!(w.calls().last(), Some(&OpsCall::Rename(w.path("a-notes.txt"), "b-notes.txt".into())));
    // A new folder's redo makes the folder again.
    let made = w.path("x");
    let rec = UndoRecord {
        of: OpId(9),
        kind: UndoKind::RecycleNewFolder,
        replaced: 0,
        entries: vec![entry(made.clone(), made)],
    };
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(9), kind: OpKind::NewFolder, record: rec });
    w.key("Ctrl+Y");
    assert_eq!(w.calls().last(), Some(&OpsCall::NewFolder(w.dir.clone(), "x".into())));
    // Nothing to redo: nothing.
    let n = w.calls().len();
    w.key("Ctrl+Y");
    assert_eq!(w.calls().len(), n);
}

#[test]
fn redo_of_restore_classifies() {
    let mut w = in_root("undo_redo_restore");
    let (a, b) = (w.path("a-notes.txt"), w.path("b-report-final-v2.pdf"));
    let bin = wp(r"C:\$Recycle.Bin\S-1-5-21-0\$R0001.txt");
    let rec = UndoRecord {
        of: OpId(7),
        kind: UndoKind::RestoreRecycled,
        replaced: 0,
        entries: vec![
            Entry { bin: Some(bin.clone()), ..entry(a.clone(), bin.clone()) },
            Entry { bin: Some(bin.clone()), ..entry(b.clone(), bin) },
        ],
    };
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(7), kind: OpKind::Recycle, record: rec.clone() });
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(6), kind: OpKind::Recycle, record: rec });
    // The bin was turned off for one of them since the restore.
    let facts: ops_ui::FactsFn = Arc::new(|items: &[(WidePath, u64)]| {
        items
            .iter()
            .map(|(p, _)| {
                let off = p.to_string_lossy().ends_with(".pdf");
                (p.clone(), if off { Recyclability::No(NoBin::Disabled) } else { Recyclability::Yes })
            })
            .collect()
    });
    w.shared.ops.borrow_mut().set_facts(facts);
    w.key("Ctrl+Y");
    w.pump();
    assert!(w.calls().is_empty(), "no Recycle before the bar's Enter: {:?}", w.calls());
    assert!(bar::bar_of(&w.h.screen().0).is_open());
    w.key("Enter");
    assert_eq!(w.calls(), [OpsCall::Recycle(vec![ItemRef::Path(a)]), OpsCall::DeletePermanent(vec![ItemRef::Path(b)])]);
    assert_eq!(w.redo_len(), 1, "the redo's requests keep the rest of the redo stack");
    // All recyclable: one Recycle, still no permanent delete.
    w.shared.ops.borrow_mut().set_facts(Arc::new(|items: &[(WidePath, u64)]| {
        items.iter().map(|(p, _)| (p.clone(), Recyclability::Yes)).collect()
    }));
    w.key("Ctrl+Y");
    w.pump();
    assert!(matches!(w.calls().last(), Some(OpsCall::Recycle(v)) if v.len() == 2), "{:?}", w.calls());
    assert!(!w.calls().iter().skip(2).any(|c| matches!(c, OpsCall::DeletePermanent(_))));
}

#[test]
fn refused_redo_leaves_no_mark() {
    // A redo whose bar requests the gate refuses (items outside the test roots): the next, unrelated operation is a
    // new one again and clears the redo stack.
    let mut w = in_root("undo_redo_refused");
    let (a, bin) = (wp(r"C:\made-up\elsewhere\a-notes.txt"), wp(r"C:\$Recycle.Bin\S-1-5-21-0\$R0002.txt"));
    let rec = UndoRecord {
        of: OpId(8),
        kind: UndoKind::RestoreRecycled,
        replaced: 0,
        entries: vec![Entry { bin: Some(bin.clone()), ..entry(a, bin) }],
    };
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(9), kind: OpKind::Recycle, record: rec.clone() });
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(8), kind: OpKind::Recycle, record: rec });
    w.shared.ops.borrow_mut().set_facts(Arc::new(|items: &[(WidePath, u64)]| {
        items.iter().map(|(p, _)| (p.clone(), Recyclability::No(NoBin::Disabled))).collect()
    }));
    w.key("Ctrl+Y");
    w.pump();
    assert!(bar::bar_of(&w.h.screen().0).is_open());
    w.key("Enter");
    assert!(w.calls().is_empty(), "the gate refused: {:?}", w.calls());
    assert_eq!(w.redo_len(), 1);
    w.h.send(Box::new(Op::Select(vec!["b-report-final-v2.pdf"])));
    w.key("Shift+Del");
    assert!(matches!(w.calls().last(), Some(OpsCall::DeletePermanent(_))));
    assert_eq!(w.redo_len(), 0, "a new operation clears the redo stack");
}

#[test]
fn new_op_clears_redo() {
    let mut w = in_root("undo_clears_redo");
    let rec = UndoRecord {
        of: OpId(5),
        kind: UndoKind::RenameBack,
        replaced: 0,
        entries: vec![entry(w.path("a-notes.txt"), w.path("b-notes.txt"))],
    };
    w.shared.undo.borrow_mut().push_redo(UndoEntry { op: OpId(5), kind: OpKind::Rename, record: rec.clone() });
    // An undo is not a new operation.
    w.shared.undo.borrow_mut().push(UndoEntry { op: OpId(5), kind: OpKind::Rename, record: rec });
    w.key("Ctrl+Z");
    assert!(matches!(w.calls().last(), Some(OpsCall::Undo(_))));
    assert_eq!(w.redo_len(), 1);
    // A delete is.
    w.h.send(Box::new(Op::Select(vec!["b-report-final-v2.pdf"])));
    w.key("Shift+Del");
    assert!(matches!(w.calls().last(), Some(OpsCall::DeletePermanent(_))));
    assert_eq!(w.redo_len(), 0);
}

#[test]
fn shared_across_windows() {
    let mut w = in_root("undo_two_windows");
    assert_eq!(w.shared.new_win(), 2);
    let c2 = w.f.attach_window(2);
    let mut h2 = screen_for(&w.shared, &w.f, 2, &w.dir);
    rename_a_to_b(&mut w);
    w.renamed(1, "a-notes.txt", "b-notes.txt");
    // Both windows heard the operations; the other window undoes it.
    assert_eq!(w.undo_len(), 1);
    h2.key("Ctrl+Z");
    assert_eq!(w.calls().last(), Some(&OpsCall::Undo(OpId(1))));
    assert_eq!(w.undo_len(), 0);
    let r = w.shared.ops.borrow().row(OpId(2)).map(|r| r.win);
    assert_eq!(r, Some(2), "the undo belongs to the window that asked");
    drop(c2);
}
