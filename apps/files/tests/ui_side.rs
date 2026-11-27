//! The sidebar, the drives, the pins and Home (F4.8-F4.10): plain tests over the fakes through the kit's Harness, plus the
//! shell host's own answer for the pins. No window, no desktop.
#![allow(dead_code)]

mod common;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use files_engine::drives::{self, Budget, DriveApi, DriveInfo, DriveKind};
use files_engine::fakes::Fakes;
use files_engine::format::Fmt;
use files_engine::home;
use files_engine::listing::WidePath;
use files_engine::msg::{Fwd, JobDone, Msg};
use files_engine::options::FilesOptions;
use files_engine::services::ReqOwner;
use files_engine::shell_client::ShellReply;
use files_engine::sidebar::{self, Inputs, RowKind};
use files_engine::window::{FilesScreen, Shared};
use files_engine::{open, pins};
use oled_trust::frame::files::{Location, ReqId, ShellEvt, ShellReq};
use oled_ui::harness::Harness;
use oled_ui::model::FgRole;
use oled_ui::{PointL, SizeL, Tokens, UiLang, WidgetId};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a made-up path parses")
}

fn drive(
    letter: u8,
    label: Option<&str>,
    kind: DriveKind,
    free: Option<u64>,
    total: Option<u64>,
    ready: bool,
) -> DriveInfo {
    DriveInfo { letter, label: label.map(String::from), kind, free, total, ready }
}

const GB: u64 = 1_000_000_000;

fn pc_drives() -> Vec<DriveInfo> {
    vec![
        drive(b'C', Some("System"), DriveKind::Fixed, Some(412 * GB), Some(1000 * GB), true),
        drive(b'D', Some("Data"), DriveKind::Fixed, Some(50 * GB), Some(1000 * GB), true),
    ]
}

/// build() over made-up state.
fn built(pins: &[&str], drives: &[DriveInfo], net: &[&str]) -> sidebar::Built {
    let fmt = Fmt::fixed_en();
    let (pins, net): (Vec<WidePath>, Vec<WidePath>) =
        (pins.iter().map(|p| wp(p)).collect(), net.iter().map(|p| wp(p)).collect());
    let (missing, expanded, children) = (HashSet::new(), HashSet::new(), HashMap::new());
    sidebar::build(&Inputs {
        lang: UiLang::EN,
        fmt: &fmt,
        like_explorer: false,
        pins: &pins,
        missing: &missing,
        drives,
        network: &net,
        expanded: &expanded,
        children: &children,
        busy: None,
    })
}

fn labels(b: &sidebar::Built) -> Vec<String> {
    b.items.iter().map(|i| i.label.to_string()).collect()
}

fn window_with(f: &Fakes, opts: FilesOptions, size: SizeL, at: Location) -> Harness<FilesScreen> {
    f.options.set(opts);
    let shared = f.shared(UiLang::EN);
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    let tab = shared.new_tab();
    let s = FilesScreen::new(shared, 1, at, None, tab);
    let mut h = Harness::new(s, size, 96, Tokens::oled());
    h.render();
    h
}

const WIDE: SizeL = SizeL { w: 900, h: 600 };

fn opts_with_pins(pins: &[&str]) -> FilesOptions {
    FilesOptions { pins: pins.iter().map(|p| p.to_string()).collect(), ..FilesOptions::default() }
}

fn shared_of(h: &Harness<FilesScreen>) -> Rc<Shared> {
    Rc::clone(h.screen().shared())
}

fn set_drives(h: &mut Harness<FilesScreen>, list: &[DriveInfo]) {
    let shared = shared_of(h);
    for d in list {
        shared.drives.borrow_mut().set(d.clone());
    }
    h.send(Box::new(Msg::Fwd(Fwd::Bump)));
    h.render();
}

fn row_at(h: &Harness<FilesScreen>, pred: impl Fn(&sidebar::Row) -> bool) -> usize {
    sidebar::of(h.screen()).rows().iter().position(pred).expect("the row is there")
}

/// The centre of a row's expander (dx 6) or label (dx 60), at 96 dpi where logical px are physical px.
fn row_pt(h: &Harness<FilesScreen>, idx: usize, dx: i32, depth: i32) -> PointL {
    let sb = sidebar::of(h.screen());
    let (pane, first) = (sb.pane(), sb.first_row());
    PointL { x: pane.l + 8 + dx + 16 * depth, y: pane.t + 28 * (idx - first) as i32 + 14 }
}

fn location(h: &Harness<FilesScreen>) -> Location {
    h.screen().tabs().active().nav.location().clone()
}

// ---- F4.9 -----------------------------------------------------------------------------------------------------

#[test]
fn sections_and_rows() {
    let mut d = pc_drives();
    d.push(drive(b'Z', None, DriveKind::Network, None, None, true));
    let b = built(&[r"C:\made-up\work", r"D:\made-up\media"], &d, &[r"\\made-up-server\share"]);
    assert_eq!(
        labels(&b),
        [
            "Pinned",
            "work",
            "media",
            "This PC",
            "System (C:)",
            "Data (D:)",
            "Network",
            "Z:",
            r"\\made-up-server\share",
            "Network \u{2197}",
            "Recycle Bin \u{2197}"
        ]
    );
    let kinds: Vec<RowKind> = b.rows.iter().map(|r| r.kind).collect();
    assert_eq!(kinds[0], RowKind::Section);
    assert_eq!(kinds[1], RowKind::Pin);
    assert_eq!(kinds[4], RowKind::Drive(b'C'));
    assert_eq!(kinds[8], RowKind::Net);
    assert_eq!(kinds[9], RowKind::NetHand);
    assert_eq!(kinds[10], RowKind::Recycle);
    let keys: HashSet<u64> = b.rows.iter().map(|r| r.key).collect();
    assert_eq!(keys.len(), b.rows.len(), "every row has its own key");
    assert!(b.items[0].section && !b.items[1].section);
    let sys = &b.items[4];
    assert_eq!(sys.trailing.as_ref().map(|t| t.0.to_string()), Some(Fmt::fixed_en().size(412 * GB, false)));
}

#[test]
fn pinned_section_is_hidden_without_pins() {
    let b = built(&[], &pc_drives(), &[]);
    assert_eq!(labels(&b)[0], "This PC");
}

#[test]
fn low_space_is_attention() {
    let b = built(&[], &pc_drives(), &[]);
    let role = |i: usize| b.items[i].trailing.as_ref().map(|t| t.1);
    assert!(matches!(role(1), Some(FgRole::Tertiary)), "412 of 1000 GB free");
    assert!(matches!(role(2), Some(FgRole::Attention)), "50 of 1000 GB free: under 10 %");
    let low = [drive(b'E', Some("Tiny"), DriveKind::Fixed, Some(5 * GB), Some(100 * GB), true)];
    let b = built(&[], &low, &[]);
    assert!(matches!(b.items[1].trailing.as_ref().map(|t| t.1), Some(FgRole::Attention)), "5 % free");
    let edge = [drive(b'E', None, DriveKind::Fixed, Some(10 * GB), Some(100 * GB), true)];
    assert!(
        matches!(built(&[], &edge, &[]).items[1].trailing.as_ref().map(|t| t.1), Some(FgRole::Tertiary)),
        "10 % is not under"
    );
}

#[test]
fn network_drive_has_no_size() {
    let net = [drive(b'Z', Some("Share"), DriveKind::Network, Some(5 * GB), Some(100 * GB), true)];
    let b = built(&[], &net, &[]);
    let z = b.items.iter().find(|i| i.label == "Share (Z:)").expect("the mapped drive");
    assert!(z.trailing.is_none(), "no size for a network drive");
    let off = [drive(b'E', Some("Empty"), DriveKind::Optical, None, None, false)];
    let b = built(&[], &off, &[]);
    assert!(b.items[1].disabled && b.items[1].trailing.is_none(), "not ready: disabled, no size");
}

struct Api {
    slow_space: u8,
}

impl DriveApi for Api {
    fn present(&self) -> u32 {
        (1 << (b'C' - b'A')) | (1 << (b'Z' - b'A'))
    }
    fn kind(&self, l: u8) -> DriveKind {
        if l == b'Z' {
            DriveKind::Network
        } else {
            DriveKind::Fixed
        }
    }
    fn label(&self, l: u8) -> Option<String> {
        if l == b'Z' {
            std::thread::sleep(Duration::from_millis(600));
        }
        Some("Disk".into())
    }
    fn space(&self, l: u8) -> Option<(u64, u64)> {
        if l == self.slow_space {
            std::thread::sleep(Duration::from_millis(600));
        }
        Some((10, 100))
    }
}

#[test]
fn drive_timeout_shows_letter() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let budget = Budget { local_ms: 150, net_label_ms: 100 };
    drives::scan_with(&shared, Arc::new(Api { slow_space: b'C' }), budget);
    let at_once: Vec<(u8, bool)> = shared.drives.borrow().all().iter().map(|d| (d.letter, d.free.is_some())).collect();
    assert_eq!(at_once, [(b'C', false), (b'Z', false)], "the letters are there at once, without a size");
    f.deliver();
    let got: Vec<DriveInfo> = f
        .posted
        .take()
        .into_iter()
        .filter_map(|m| match m {
            Msg::Job { done: JobDone::DriveFacts(d), .. } => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(got.len(), 2);
    let c = got.iter().find(|d| d.letter == b'C').expect("C");
    assert_eq!((c.label.clone(), c.free, c.ready), (None, None, true), "no answer in 150 ms: the letter alone");
    let z = got.iter().find(|d| d.letter == b'Z').expect("Z");
    assert_eq!((z.label.clone(), z.free), (None, None), "a network label gives up after its budget");
    let lanes = f.bg.runs();
    assert_eq!(lanes.iter().filter(|(_, l)| *l == files_engine::bg::Lane::Network).count(), 1);
}

#[test]
fn fold_below_760() {
    let f = Fakes::new();
    let h = window_with(&f, FilesOptions::default(), SizeL { w: 700, h: 500 }, Location::Fs(wp(r"C:\made-up\work")));
    assert!(!sidebar::of(h.screen()).is_shown(), "folded below 760 px");
    assert_eq!(h.screen().rects().side.w(), 0);
    let f = Fakes::new();
    let h = window_with(&f, FilesOptions::default(), WIDE, Location::Fs(wp(r"C:\made-up\work")));
    assert!(sidebar::of(h.screen()).is_shown());
    assert_eq!(h.screen().rects().side.w(), 201, "the default width and the line");
    assert!(h.screen().rects().body.l > 200, "the body sits beside it");
}

#[test]
fn ctrl_b_toggles() {
    let f = Fakes::new();
    let mut h = window_with(&f, FilesOptions::default(), WIDE, Location::Fs(wp(r"C:\made-up\work")));
    h.key("Ctrl+B");
    h.render();
    assert!(!sidebar::of(h.screen()).is_shown());
    assert!(!f.options.modifications().last().expect("saved through the service").sidebar_visible);
    h.key("Ctrl+B");
    h.render();
    assert!(sidebar::of(h.screen()).is_shown());
    assert_eq!(f.options.modifications().len(), 2);
}

#[test]
fn width_is_saved_after_the_drag() {
    let f = Fakes::new();
    let mut h = window_with(&f, FilesOptions::default(), WIDE, Location::Fs(wp(r"C:\made-up\work")));
    h.drag(PointL { x: 200, y: 300 }, PointL { x: 300, y: 300 });
    h.render();
    let saved: Vec<u16> = f.options.modifications().iter().map(|o| o.sidebar_width).collect();
    assert_eq!(saved.len(), 1, "one write for the whole drag: {saved:?}");
    assert_eq!(saved[0], 300);
    h.double_click(PointL { x: 300, y: 300 });
    h.render();
    assert_eq!(f.options.modifications().last().map(|o| o.sidebar_width), Some(200), "a double click resets");
}

#[test]
fn current_row_follows_navigation() {
    let f = Fakes::new();
    let mut h = window_with(&f, opts_with_pins(&[r"C:\made-up\work"]), WIDE, Location::Fs(wp(r"C:\made-up\work\sub")));
    set_drives(&mut h, &pc_drives());
    let rows = sidebar::of(h.screen()).rows().to_vec();
    let pin = rows.iter().find(|r| r.kind == RowKind::Pin).expect("pin").key;
    let c = rows.iter().find(|r| r.kind == RowKind::Drive(b'C')).expect("C").key;
    assert_eq!(sidebar::current_key(&rows, &location(&h)), Some(pin), "the deepest row holding the folder");
    assert_eq!(sidebar::current_key(&rows, &Location::Fs(wp(r"C:\elsewhere"))), Some(c));
    assert_eq!(sidebar::current_key(&rows, &Location::Fs(wp(r"E:\x"))), None);
    assert_eq!(sidebar::current_key(&rows, &Location::Home), None);
    // A click on a row goes there.
    let i = row_at(&h, |r| r.kind == RowKind::Drive(b'D'));
    h.click(row_pt(&h, i, 60, 0));
    assert_eq!(location(&h), Location::Fs(wp(r"D:\")));
}

#[test]
fn every_row_named() {
    let f = Fakes::new();
    let mut h = window_with(&f, opts_with_pins(&[r"C:\made-up\work"]), WIDE, Location::Fs(wp(r"C:\made-up\work")));
    set_drives(&mut h, &pc_drives());
    h.assert_named();
    let dump = h.a11y_dump();
    for want in ["System (C:)", "Data (D:)", "work", "Recycle Bin"] {
        assert!(dump.contains(want), "{want} is in the tree of names");
    }
}

#[test]
fn tree_expand_lists_dirs_and_collapse_cancels() {
    let root = common::temp::TempRoot::new("side-tree");
    for d in ["b-dir", "a2", "a10", "a-dir"] {
        root.dir(d);
    }
    root.file("note.txt", b"x");
    let path = root.path().to_string_lossy().into_owned();
    let f = Fakes::new();
    let mut h = window_with(&f, opts_with_pins(&[&path]), WIDE, Location::Fs(wp(&path)));
    f.pump(&mut h, 1);
    h.render();
    let i = row_at(&h, |r| r.kind == RowKind::Pin);
    // Expand: one job, then the folders as rows one level deeper, in natural order, files left out.
    h.click(row_pt(&h, i, 6, 0));
    assert_eq!(f.bg.queued(), 1, "one enumeration");
    f.pump(&mut h, 1);
    h.render();
    let names: Vec<String> = sidebar::of(h.screen()).rows()[i + 1..i + 5]
        .iter()
        .map(|r| {
            r.path.as_ref().map(|p| p.file_name().map(String::from_utf16_lossy).unwrap_or_default()).unwrap_or_default()
        })
        .collect();
    assert!(names.iter().position(|n| n == "a2") < names.iter().position(|n| n == "a10"), "{names:?}");
    assert_eq!(names.len(), 4);
    assert!(sidebar::of(h.screen()).rows()[i + 1..i + 5].iter().all(|r| r.kind == RowKind::Dir && r.depth == 1));
    assert!(!sidebar::of(h.screen())
        .rows()
        .iter()
        .any(|r| r.path.as_ref().is_some_and(|p| p.to_string_lossy().ends_with("note.txt"))));
    // Collapse drops them.
    h.click(row_pt(&h, i, 6, 0));
    h.render();
    assert!(sidebar::of(h.screen()).rows().iter().all(|r| r.kind != RowKind::Dir));
    // Expand and collapse before the job runs: its answer never comes.
    h.click(row_pt(&h, i, 6, 0));
    assert_eq!(f.bg.queued(), 1);
    h.click(row_pt(&h, i, 6, 0));
    f.pump(&mut h, 1);
    h.render();
    assert!(sidebar::of(h.screen()).rows().iter().all(|r| r.kind != RowKind::Dir), "a cancelled job brings nothing");
}

#[test]
fn ctrl_shift_e_expands_to_current() {
    let root = common::temp::TempRoot::new("side-walk");
    root.dir(r"one\two\three");
    root.dir("zzz");
    let target = root.path().join("one").join("two");
    let path = target.to_string_lossy().into_owned();
    let letter = path.as_bytes()[0].to_ascii_uppercase();
    let f = Fakes::new();
    let opts = FilesOptions { hidden_items: true, ..FilesOptions::default() };
    let mut h = window_with(&f, opts, WIDE, Location::Fs(wp(&path)));
    set_drives(&mut h, &[drive(letter, None, DriveKind::Fixed, Some(GB), Some(10 * GB), true)]);
    h.key("Ctrl+Shift+E");
    for _ in 0..40 {
        f.pump(&mut h, 1);
        h.render();
    }
    let rows = sidebar::of(h.screen()).rows().to_vec();
    let wanted = path.to_lowercase();
    let current = sidebar::current_key(&rows, &location(&h)).expect("a current row");
    let cur = rows.iter().find(|r| r.key == current).expect("row");
    assert_eq!(
        cur.path.as_ref().map(|p| p.to_string_lossy().to_lowercase()),
        Some(wanted),
        "the row of the folder itself is in the tree"
    );
    assert_eq!(cur.kind, RowKind::Dir);
}

// ---- F4.8 -----------------------------------------------------------------------------------------------------

fn pins_reply(shared: &Shared, f: &Fakes, req: ReqId, lists: &[&[&str]]) {
    for l in lists {
        f.shell.reply(ShellReply::Evt(ShellEvt::Pins { req, pins: l.iter().map(|p| wp(p)).collect() }));
    }
    f.shell.reply(ShellReply::Evt(ShellEvt::ReqDone { req }));
    f.deliver();
    let _ = open::drain_shell(shared);
}

fn last_pins_req(f: &Fakes) -> ReqId {
    f.shell
        .requests()
        .iter()
        .rev()
        .find_map(|r| match r {
            ShellReq::QuickAccessPins { req } => Some(*req),
            _ => None,
        })
        .expect("a QuickAccessPins request went out")
}

#[test]
fn first_run_imports_once() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    assert!(pins::import_if_due_with(&shared, true), "the first run asks");
    assert!(!pins::import_if_due_with(&shared, true), "one request at a time");
    let req = last_pins_req(&f);
    assert_eq!(shared.req_owner.borrow().get(&req), Some(&ReqOwner::Pins));
    pins_reply(&shared, &f, req, &[&[r"C:\made-up\a", r"C:\made-up\b"], &[r"C:\made-up\A", r"C:\made-up\c"]]);
    let saved = f.options.modifications().last().cloned().expect("saved through the service");
    assert_eq!(saved.pins, [r"C:\made-up\a", r"C:\made-up\b", r"C:\made-up\c"], "order kept, duplicates dropped");
    assert!(saved.pins_imported);
    assert_eq!(pins::paths(&shared).len(), 3);
    assert!(!pins::import_if_due_with(&shared, true), "never again once imported");
    assert_eq!(f.shell.requests().iter().filter(|r| matches!(r, ShellReq::QuickAccessPins { .. })).count(), 1);
}

#[test]
fn import_is_cut_at_64() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    assert!(pins::import_if_due_with(&shared, true));
    let req = last_pins_req(&f);
    let many: Vec<String> = (0..80).map(|i| format!(r"C:\made-up\p{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    pins_reply(&shared, &f, req, &[&refs]);
    assert_eq!(pins::paths(&shared).len(), pins::MAX_PINS);
}

#[test]
fn no_import_while_not_ok() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    if pins::PINS_IMPORT_OK {
        // The FS-2 record passed: this test is about the constant as it is while false.
        return;
    }
    pins::import_if_due(&shared);
    assert!(f.shell.requests().is_empty(), "no request while the FS-2 record has not passed");
    assert!(!pins::reimport(&shared));
    assert!(!pins::reimport_enabled(), "Re-import is disabled with its note");
    assert!(f.shell.requests().is_empty());
    assert!(!shared.opts.borrow().pins_imported);
}

#[test]
fn failed_import_changes_nothing() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    assert!(pins::import_if_due_with(&shared, true));
    let req = last_pins_req(&f);
    f.shell.reply(ShellReply::Evt(ShellEvt::Failed { req, hr: -1 }));
    f.deliver();
    let _ = open::drain_shell(&shared);
    assert!(!shared.opts.borrow().pins_imported, "asked again next time");
    assert!(pins::import_if_due_with(&shared, true), "and nothing is stuck");
    let req = last_pins_req(&f);
    f.shell.reply(ShellReply::Evt(ShellEvt::Failed { req, hr: -1 }));
    f.deliver();
    let _ = open::drain_shell(&shared);
}

#[test]
fn pin_and_unpin_edit_our_list() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let a = wp(r"C:\made-up\a");
    assert!(pins::add(&shared, &a));
    assert!(!pins::add(&shared, &wp(r"c:\MADE-UP\a")), "already there, whatever the case");
    assert!(pins::contains(&shared, &a));
    assert!(pins::add(&shared, &wp(r"C:\made-up\b")));
    assert_eq!(
        f.options.modifications().last().map(|o| o.pins.clone()),
        Some(vec![r"C:\made-up\a".to_string(), r"C:\made-up\b".to_string()])
    );
    assert!(pins::remove(&shared, &a));
    assert!(!pins::remove(&shared, &a), "not there any more");
    assert_eq!(pins::paths(&shared), [wp(r"C:\made-up\b")]);
    assert!(f.shell.requests().is_empty(), "nothing is asked of, or written to, Explorer");
    for i in 0..pins::MAX_PINS {
        pins::add(&shared, &wp(&format!(r"C:\made-up\q{i}")));
    }
    assert_eq!(pins::paths(&shared).len(), pins::MAX_PINS);
    assert!(!pins::add(&shared, &wp(r"C:\made-up\over")), "the list is full");
}

#[test]
fn reimport_replaces() {
    let f = Fakes::new();
    f.options.set(opts_with_pins(&[r"C:\made-up\old"]));
    let shared = f.shared(UiLang::EN);
    assert!(pins::reimport_with(&shared, true));
    let req = last_pins_req(&f);
    pins_reply(&shared, &f, req, &[&[r"C:\made-up\new1", r"C:\made-up\new2"]]);
    assert_eq!(pins::paths(&shared), [wp(r"C:\made-up\new1"), wp(r"C:\made-up\new2")]);
    assert!(shared.opts.borrow().pins_imported);
}

#[cfg(debug_assertions)]
#[test]
fn host_serves_pins() {
    use files_engine::testclient::shell::Direct;
    let root = common::temp::TempRoot::new("host-pins");
    let h = common::hooks(&root);
    let mut env: Vec<(String, String)> = h.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    env.push(("FILES_TEST_PINS".into(), r"C:\made-up\a|D:\made-up\b".into()));
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let (mut d, _) = Direct::ready(&common::files_exe(), &pairs, Duration::from_secs(10)).expect("Hello and Ready");
    d.send(&ShellReq::QuickAccessPins { req: ReqId(5) }).expect("sent");
    let got = d
        .until(Duration::from_secs(20), |e| matches!(e, ShellEvt::ReqDone { .. } | ShellEvt::Failed { .. }))
        .expect("the host answers");
    let pins: Vec<WidePath> = got
        .iter()
        .flat_map(|e| if let ShellEvt::Pins { req: ReqId(5), pins } = e { pins.clone() } else { Vec::new() })
        .collect();
    assert_eq!(pins, [wp(r"C:\made-up\a"), wp(r"D:\made-up\b")]);
    assert_eq!(got.last(), Some(&ShellEvt::ReqDone { req: ReqId(5) }));
}

// ---- F4.10 ----------------------------------------------------------------------------------------------------

fn home_window(f: &Fakes, pins: &[&str]) -> Harness<FilesScreen> {
    let mut h = window_with(f, opts_with_pins(pins), WIDE, Location::Home);
    set_drives(&mut h, &pc_drives());
    h
}

#[test]
fn home_sections_and_nodes() {
    let f = Fakes::new();
    let h = home_window(&f, &[r"C:\made-up\work", r"D:\made-up\media"]);
    let page = home::of(h.screen());
    assert_eq!(page.sections(), [0, 1, 3], "pinned, drives, the hand-offs (no network places)");
    let labels: Vec<&str> = page.items().iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["work", "media", "System (C:)", "Data (D:)", "Recycle Bin \u{2197}", "Phones and other devices \u{2197}"]
    );
    h.assert_named();
    let dump = h.a11y_dump();
    assert!(dump.contains("System (C:)") && dump.contains("work"));
}

#[test]
fn empty_pins_section_hidden() {
    let f = Fakes::new();
    let h = home_window(&f, &[]);
    assert!(!home::of(h.screen()).sections().contains(&0));
    let f = Fakes::new();
    let h = home_window(&f, &[r"C:\made-up\work"]);
    assert!(home::of(h.screen()).sections().contains(&0));
}

#[test]
fn capacity_bar_attention_above_90() {
    assert!(!home::bar_attention(90) && home::bar_attention(91));
    let f = Fakes::new();
    let mut h = home_window(&f, &[]);
    set_drives(&mut h, &[drive(b'E', Some("Full"), DriveKind::Fixed, Some(5 * GB), Some(100 * GB), true)]);
    let used: Vec<Option<u64>> = home::of(h.screen()).items().iter().map(|i| i.used_pct).collect();
    assert!(used.contains(&Some(95)) && used.contains(&Some(58)), "{used:?}");
    let _ = h.render();
}

#[test]
fn arrows_rove_and_enter_opens() {
    let f = Fakes::new();
    let mut h = home_window(&f, &[r"C:\made-up\work", r"D:\made-up\media"]);
    let on_home = |h: &Harness<FilesScreen>| h.focused().is_some_and(|n| n.owner() == WidgetId(70));
    for _ in 0..30 {
        if on_home(&h) {
            break;
        }
        h.key("Tab");
    }
    assert!(on_home(&h), "Tab reaches the page");
    let first = h.focused().expect("focus");
    h.key("Tab");
    let next_section = h.focused().expect("focus");
    assert_eq!(next_section.slot(), first.slot() + 2, "one Tab stop per section: Tab lands on the first Drives row");
    h.key("Shift+Tab");
    assert_eq!(h.focused().expect("focus").slot(), first.slot(), "Shift+Tab goes back");
    h.key("Right");
    let second = h.focused().expect("focus");
    assert_eq!(second.slot(), first.slot() + 1, "the arrow roves to the next tile");
    h.key("Enter");
    assert_eq!(location(&h), Location::Fs(wp(r"D:\made-up\media")));
}

#[test]
fn home_steps_stay_inside_their_section() {
    let f = Fakes::new();
    let h = home_window(&f, &[r"C:\made-up\work", r"D:\made-up\media"]);
    let items = home::of(h.screen()).items();
    let (a, b) = (0usize, 1usize);
    assert_eq!(home::step(items, a, oled_ui::Key::RIGHT, 4), Some(b));
    assert_eq!(home::step(items, b, oled_ui::Key::RIGHT, 4), None, "the last tile of the section");
    assert_eq!(home::step(items, 2, oled_ui::Key::UP, 4), None, "the first row of the drives");
    assert_eq!(home::step(items, 2, oled_ui::Key::DOWN, 4), Some(3));
}
