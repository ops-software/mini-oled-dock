//! Live updates (P3-files-app F3.9, F3.10): the watch of the shown folder, the diffs it brings, and our own changes
//! applied at once. Plain tests over the fakes through the kit's Harness; the folders the diffs read are TempRoot's.

mod common;

use common::temp::TempRoot;
use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::listing::{Gen, WidePath};
use files_engine::live;
use files_engine::msg::{Fwd, Msg};
use files_engine::tab;
use files_engine::watch::{ChangeAction, RawChange, WatchEvent, WatchId};
use files_engine::window::{FilesScreen, Shared, WinKind, WinRec};
use oled_trust::frame::files::{Change, ChangeKind, Location, OpId};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::ListModel;
use oled_ui::{
    CaptionHit, CommandTable, Event, EventCx, LayoutCx, PaintCx, PointP, Reply, Screen, SizeL, SizeP, Tokens, UiLang,
};
use std::rc::Rc;

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

enum Op {
    Go(Location),
    OpenTab(Location, bool),
    Event(Event),
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
                    Op::Go(to) => files_engine::window::navigate(&mut self.0, cx, to, None),
                    Op::OpenTab(at, activate) => {
                        let t = tab::new_tab(self.0.shared(), self.0.win(), at);
                        self.0.tabs_mut().open(t, activate);
                        live::watch_active(&mut self.0, cx);
                    }
                    Op::Event(ev) => {
                        self.0.event(cx, &ev);
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
    fn caption_hit(&self, p: PointP) -> CaptionHit {
        self.0.caption_hit(p)
    }
}

struct Win {
    h: Harness<Wrap>,
    win: u32,
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }
    fn labels(&self) -> Vec<String> {
        let m = self.fs().tabs().active().table.model();
        (0..ListModel::len(m)).map(|r| m.label(r).to_string()).collect()
    }
    fn selected_labels(&self) -> Vec<String> {
        let t = &self.fs().tabs().active().table;
        let m = t.model();
        (0..ListModel::len(m))
            .filter(|r| t.selection().contains(ListModel::key(m, *r)))
            .map(|r| m.label(r).to_string())
            .collect()
    }
    fn has_label(&self, text: &str) -> bool {
        self.h.a11y_dump().lines().any(|l| l.trim_start() == format!("Label {text:?}"))
    }
    fn here(&self) -> String {
        match self.fs().tabs().active().nav.location() {
            Location::Fs(p) => p.to_string_lossy(),
            other => format!("{other:?}"),
        }
    }
}

struct Env {
    f: Fakes,
    shared: Rc<Shared>,
    w: Win,
}

fn open_window(f: &Fakes, shared: &Rc<Shared>, win: u32, dir: &WidePath, entries: &[(&str, bool, u64, i64)]) -> Win {
    f.enumr.queue(dir, fakes::listing(entries));
    let tab = shared.new_tab();
    shared.windows.borrow_mut().push(WinRec { win, kind: WinKind::Main, wref: None, hwnd: None, location: None });
    let screen = FilesScreen::new(Rc::clone(shared), win, Location::Fs(dir.clone()), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, win);
    h.render();
    Win { h, win }
}

fn env(root: &TempRoot, entries: &[(&str, bool, u64, i64)]) -> Env {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let dir = wp(root.path().to_str().unwrap());
    let w = open_window(&f, &shared, 1, &dir, entries);
    Env { f, shared, w }
}

impl Env {
    fn pump(&mut self) {
        for _ in 0..3 {
            self.f.pump(&mut self.w.h, self.w.win);
            self.w.h.render();
        }
    }
    fn send(&mut self, m: Msg) {
        self.w.h.send(Box::new(m));
        self.pump();
    }
    fn op(&mut self, op: Op) {
        self.w.h.send(Box::new(op));
        self.pump();
    }
    fn watch(&self) -> (WatchId, Gen) {
        let (id, _, gen) = self.f.watch.watches().last().cloned().expect("a watch is registered");
        (id, gen)
    }
    fn changes(&mut self, items: &[(ChangeAction, &str)]) {
        let (id, gen) = self.watch();
        let items = items.iter().map(|(a, n)| RawChange { action: *a, name: n.as_bytes().into() }).collect();
        self.f.watch.emit(WatchEvent::Changes { id, gen, items });
        self.pump();
    }
    fn own(&mut self, kind: ChangeKind, from: Option<&std::path::Path>, to: Option<&std::path::Path>) {
        let p = |p: &std::path::Path| wp(p.to_str().unwrap());
        let c = Change { kind, from: from.map(p), to: to.map(p) };
        self.send(Msg::Fwd(Fwd::Own { op: OpId(7), changes: vec![c] }));
    }
}

fn two_files() -> Vec<(&'static str, bool, u64, i64)> {
    vec![("a.txt", false, 1, day(1)), ("b.txt", false, 1, day(2))]
}

#[test]
fn one_watch_per_window_active_tab() {
    let root = TempRoot::new("live-one");
    root.dir("sub");
    let mut e = env(&root, &two_files());
    let w = e.f.watch.watches();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].1, wp(root.path().to_str().unwrap()));
    assert_eq!(e.shared.watch_owner.borrow().len(), 1);
    let sub = wp(root.path().join("sub").to_str().unwrap());
    e.f.enumr.queue(&sub, fakes::listing(&[("x.txt", false, 1, day(1))]));
    e.op(Op::Go(Location::Fs(sub.clone())));
    let w = e.f.watch.watches();
    assert_eq!(w.len(), 2, "the new folder is watched");
    assert_eq!(w[1].1, sub);
    assert_eq!(e.f.watch.unwatched(), [w[0].0], "the old one is released first");
    assert_eq!(e.shared.watch_owner.borrow().len(), 1);
    assert_eq!(e.shared.watch_owner.borrow().get(&w[1].0), Some(&1));
}

#[test]
fn events_reach_the_watching_window() {
    let (r1, r2) = (TempRoot::new("live-w1"), TempRoot::new("live-w2"));
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let (d1, d2) = (wp(r1.path().to_str().unwrap()), wp(r2.path().to_str().unwrap()));
    let mut w1 = open_window(&f, &shared, 1, &d1, &two_files());
    f.attach_window(2);
    let mut w2 = open_window(&f, &shared, 2, &d2, &two_files());
    assert_eq!(f.watch.watches().len(), 2);
    let (id2, _, gen2) = f.watch.watches()[1].clone();
    assert_eq!(shared.watch_owner.borrow().get(&id2), Some(&2));
    r2.file("c.txt", b"hello");
    f.watch.emit(WatchEvent::Changes {
        id: id2,
        gen: gen2,
        items: vec![RawChange { action: ChangeAction::Added, name: b"c.txt".as_slice().into() }],
    });
    for _ in 0..3 {
        f.pump(&mut w1.h, 1);
        f.pump(&mut w2.h, 2);
    }
    w2.h.render();
    assert_eq!(w2.labels(), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(w1.labels(), ["a.txt", "b.txt"], "the other window does not apply it");
}

#[test]
fn assoc_changed_reaches_class_icons() {
    let root = TempRoot::new("live-assoc");
    let mut e = env(&root, &two_files());
    let before = e.w.fs().tabs().active().table.model().generation();
    e.f.watch.emit(WatchEvent::AssocChanged);
    e.pump();
    assert_eq!(e.f.icons.assoc_changed_calls(), 1);
    assert!(e.shared.types.is_empty(), "the type names are cleared");
    assert!(e.w.fs().tabs().active().table.model().generation() > before, "the window is bumped");
}

#[test]
fn switch_tab_moves_the_watch() {
    let (r1, r2) = (TempRoot::new("live-t1"), TempRoot::new("live-t2"));
    let mut e = env(&r1, &two_files());
    let d2 = wp(r2.path().to_str().unwrap());
    e.f.enumr.queue(&d2, fakes::listing(&two_files()));
    e.op(Op::OpenTab(Location::Fs(d2.clone()), true));
    let w = e.f.watch.watches();
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(w[1].1, d2);
    assert_eq!(e.f.watch.unwatched(), [w[0].0]);
    assert_eq!(e.shared.watch_owner.borrow().len(), 1);
}

#[test]
fn background_tabs_hold_no_watch() {
    let (r1, r2) = (TempRoot::new("live-b1"), TempRoot::new("live-b2"));
    let mut e = env(&r1, &two_files());
    let d2 = wp(r2.path().to_str().unwrap());
    e.f.enumr.queue(&d2, fakes::listing(&two_files()));
    e.op(Op::OpenTab(Location::Fs(d2), false));
    assert_eq!(e.f.watch.watches().len(), 1, "only the active tab is watched");
    assert!(e.f.watch.unwatched().is_empty());
    assert!(e.w.fs().tabs().iter().filter(|t| t.watch.is_some()).count() == 1);
}

#[test]
fn changes_resolved_off_thread_and_applied_with_anchor() {
    let root = TempRoot::new("live-anchor");
    let names: Vec<String> = (0..200).map(|i| format!("m-{i:03}.txt")).collect();
    let entries: Vec<(&str, bool, u64, i64)> = names.iter().map(|n| (n.as_str(), false, 1, day(1))).collect();
    let mut e = env(&root, &entries);
    e.w.h.key("End");
    let y = |e: &Env| {
        let t = &e.w.fs().tabs().active().table;
        let k = t.focused().expect("a row is focused");
        let row = t.model().row_of(k).expect("it has a row");
        (t.model().label(row).to_string(), t.row_rect(row).map(|r| r.t))
    };
    let before = y(&e);
    assert_eq!(before.0, "m-199.txt");
    root.file("a-new.txt", b"x");
    let (id, gen) = e.watch();
    e.f.watch.emit(WatchEvent::Changes {
        id,
        gen,
        items: vec![RawChange { action: ChangeAction::Added, name: b"a-new.txt".as_slice().into() }],
    });
    e.f.deliver();
    e.w.h.render();
    assert_eq!(e.f.bg.queued(), 0, "nothing ran on the UI thread: the job ran in deliver()");
    e.pump();
    assert_eq!(e.w.labels().first().map(String::as_str), Some("a-new.txt"));
    assert_eq!(y(&e), before, "the focused row keeps its place");
}

#[test]
fn changes_job_goes_to_the_bg_lane() {
    let root = TempRoot::new("live-lane");
    let mut e = env(&root, &two_files());
    let (id, gen) = e.watch();
    let runs = e.f.bg.runs().len();
    e.f.watch.emit(WatchEvent::Changes {
        id,
        gen,
        items: vec![RawChange { action: ChangeAction::Removed, name: b"b.txt".as_slice().into() }],
    });
    e.f.watch.emit(WatchEvent::Changes { id, gen, items: vec![] });
    e.f.deliver();
    e.w.h.render();
    // deliver() also ran the job; the pump fed the answer to the window.
    e.pump();
    assert!(e.f.bg.runs().len() > runs, "the diff was resolved by a Bg job");
    assert_eq!(e.w.labels(), ["a.txt"]);
}

#[test]
fn rescan_merges_keeping_ids() {
    let root = TempRoot::new("live-rescan");
    let mut e = env(&root, &two_files());
    let id_of_b = |e: &Env| e.w.fs().tabs().active().table.model().listing().find(b"b.txt");
    let before = id_of_b(&e).expect("b is listed");
    let (id, gen) = e.watch();
    let dir = wp(root.path().to_str().unwrap());
    e.f.enumr.queue(&dir, fakes::listing(&[("b.txt", false, 1, day(2)), ("z.txt", false, 5, day(1))]));
    e.f.watch.emit(WatchEvent::Rescan { id, gen });
    e.pump();
    assert_eq!(e.w.labels(), ["b.txt", "z.txt"]);
    assert_eq!(id_of_b(&e), Some(before), "survivors keep their ids");
}

#[test]
fn limited_shows_note_and_refreshes_on_activate() {
    let root = TempRoot::new("live-limited");
    let mut e = env(&root, &two_files());
    let (id, gen) = e.watch();
    e.f.watch.emit(WatchEvent::Limited { id, gen });
    e.pump();
    assert!(e.w.has_label("Live updates limited · F5 refreshes"), "{}", e.w.h.a11y_dump());
    let dir = wp(root.path().to_str().unwrap());
    let jobs = e.f.enumr.jobs().len();
    e.f.enumr.queue(&dir, fakes::listing(&[("a.txt", false, 1, day(1)), ("c.txt", false, 1, day(1))]));
    e.op(Op::Event(Event::WindowActive(true)));
    assert_eq!(e.f.enumr.jobs().len(), jobs + 1, "activation re-enumerates the folder");
    assert_eq!(e.w.labels(), ["a.txt", "c.txt"]);
    // And again on the next activation.
    e.f.enumr.queue(&dir, fakes::listing(&[("a.txt", false, 1, day(1))]));
    e.op(Op::Event(Event::WindowActive(true)));
    assert_eq!(e.w.labels(), ["a.txt"]);
}

#[test]
fn limited_clears_when_the_tab_moves_to_a_watched_folder() {
    let root = TempRoot::new("live-limited-clear");
    root.dir("sub");
    let mut e = env(&root, &two_files());
    let sub = wp(root.path().join("sub").to_str().unwrap());
    e.f.watch.set_fail(Some(files_engine::watch::WatchError::Unsupported));
    e.f.enumr.queue(&sub, fakes::listing(&[("x.txt", false, 1, day(1))]));
    e.op(Op::Go(Location::Fs(sub.clone())));
    assert!(e.w.has_label("Live updates limited · F5 refreshes"), "{}", e.w.h.a11y_dump());
    e.f.watch.set_fail(None);
    let dir = wp(root.path().to_str().unwrap());
    e.f.enumr.queue(&dir, fakes::listing(&two_files()));
    e.op(Op::Go(Location::Fs(dir)));
    let jobs = e.f.enumr.jobs().len();
    e.op(Op::Event(Event::WindowActive(true)));
    assert_eq!(e.f.enumr.jobs().len(), jobs, "a watched folder is not refreshed on activation");
}

#[test]
fn gone_ignores_a_stat_reply_with_a_colliding_req() {
    let root = TempRoot::new("live-gone-req");
    root.dir("sub");
    let sub = wp(root.path().join("sub").to_str().unwrap());
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let w = open_window(&f, &shared, 1, &sub, &[("x.txt", false, 1, day(1))]);
    let mut e = Env { f, shared, w };
    root.remove_dir("sub");
    let (id, gen) = e.watch();
    // The walk is pending (its job has not run); a small-counter reply must not be taken for it.
    e.f.watch.emit(WatchEvent::Gone { id, gen });
    for (win, fwd) in live::drain(&e.shared) {
        assert_eq!(win, 1);
        e.w.h.send(Box::new(Msg::Fwd(fwd)));
    }
    let other = wp(root.path().to_str().unwrap());
    let done = files_engine::msg::JobDone::Stat { req: 1, path: other, found: Some(true) };
    e.w.h.send(Box::new(Msg::Job { win: 1, done }));
    assert_eq!(e.w.here(), sub.to_string_lossy(), "the foreign reply changed nothing");
    assert!(!e.w.has_label("sub was moved or deleted."));
}

#[test]
fn gone_goes_to_nearest_parent_with_notice() {
    let root = TempRoot::new("live-gone");
    root.dir("sub/deep");
    let sub = root.path().join("sub");
    let deep = wp(sub.join("deep").to_str().unwrap());
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let w = open_window(&f, &shared, 1, &deep, &[("x.txt", false, 1, day(1))]);
    let mut e = Env { f, shared, w };
    // sub\deep and then sub vanish: the nearest folder that still exists is the root.
    root.remove_dir("sub/deep");
    root.remove_dir("sub");
    let (id, gen) = e.watch();
    let up = wp(root.path().to_str().unwrap());
    e.f.enumr.queue(&up, fakes::listing(&two_files()));
    e.f.watch.emit(WatchEvent::Gone { id, gen });
    e.pump();
    assert_eq!(e.w.here(), root.path().to_str().unwrap());
    assert_eq!(e.w.labels(), ["a.txt", "b.txt"]);
    assert!(e.w.has_label("deep was moved or deleted."), "{}", e.w.h.a11y_dump());
}

#[test]
fn rename_applies_once_despite_watcher_duplicate() {
    let root = TempRoot::new("live-rename");
    root.file("a.txt", b"x");
    root.file("b.txt", b"x");
    let mut e = env(&root, &two_files());
    root.rename("a.txt", "c.txt");
    e.own(ChangeKind::Renamed, Some(&root.path().join("a.txt")), Some(&root.path().join("c.txt")));
    assert_eq!(e.w.labels(), ["b.txt", "c.txt"]);
    let runs = e.f.bg.runs().len();
    // The watcher reports the same rename a moment later: nothing is looked at again.
    e.changes(&[(ChangeAction::RenamedFrom, "a.txt"), (ChangeAction::RenamedTo, "c.txt")]);
    assert_eq!(e.f.bg.runs().len(), runs, "the duplicates were dropped");
    assert_eq!(e.w.labels(), ["b.txt", "c.txt"]);
    // After 2 s the same name is looked at again.
    e.f.clock.advance(2_500);
    e.changes(&[(ChangeAction::RenamedTo, "c.txt")]);
    assert!(e.f.bg.runs().len() > runs);
}

#[test]
fn recycled_rows_leave_at_once() {
    let root = TempRoot::new("live-recycled");
    let mut e = env(&root, &two_files());
    let runs = e.f.bg.runs().len();
    e.own(ChangeKind::Recycled, Some(&root.path().join("a.txt")), None);
    assert_eq!(e.w.labels(), ["b.txt"]);
    assert_eq!(e.f.bg.runs().len(), runs, "a removal needs no read");
}

#[test]
fn focus_moves_to_a_neighbour_when_its_row_is_removed() {
    let root = TempRoot::new("live-focus");
    let mut e = env(&root, &two_files());
    let focused = |e: &Env| {
        let t = &e.w.fs().tabs().active().table;
        let k = t.focused()?;
        t.model().row_of(k).map(|row| t.model().label(row).to_string())
    };
    e.w.h.key("Home");
    assert_eq!(focused(&e).as_deref(), Some("a.txt"));
    e.own(ChangeKind::Recycled, Some(&root.path().join("a.txt")), None);
    assert_eq!(focused(&e).as_deref(), Some("b.txt"), "the row after it takes the focus");
    let mut e = env(&root, &[("a.txt", false, 1, day(1)), ("b.txt", false, 1, day(2))]);
    e.w.h.key("End");
    assert_eq!(focused(&e).as_deref(), Some("b.txt"));
    e.own(ChangeKind::Recycled, Some(&root.path().join("b.txt")), None);
    assert_eq!(focused(&e).as_deref(), Some("a.txt"), "the last row goes: the one before it takes the focus");
}

#[test]
fn created_becomes_selection() {
    let root = TempRoot::new("live-created");
    let mut e = env(&root, &two_files());
    root.dir("new folder");
    e.own(ChangeKind::Created, None, Some(&root.path().join("new folder")));
    assert!(e.w.labels().contains(&"new folder".to_string()), "{:?}", e.w.labels());
    assert_eq!(e.w.selected_labels(), ["new folder"]);
}

#[test]
fn other_process_changes_never_steal_selection() {
    let root = TempRoot::new("live-steal");
    let mut e = env(&root, &two_files());
    e.w.h.key("Down").key("Down");
    let before = e.w.selected_labels();
    assert!(!before.is_empty(), "something is selected to be stolen");
    root.file("c.txt", b"x");
    e.changes(&[(ChangeAction::Added, "c.txt")]);
    assert_eq!(e.w.labels(), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(e.w.selected_labels(), before, "a change from outside leaves the selection alone");
}
