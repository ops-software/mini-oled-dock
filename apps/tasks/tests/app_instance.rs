//! TA-T6a (T6.2, T6.3) on the screen: a Show from the command line or a forwarded WM_COPYDATA selects, sorts and fronts,
//! and a foreign frame can do nothing else. Through the kit Harness: no window, no process, no desktop.

use std::path::Path;
use std::sync::Arc;

use oled_trust::frame::tasks::{copydata_tag, InstanceCmd, Select, Show, SortBy};
use oled_trust::frame::{encode, HEADER_LEN};
use oled_ui::harness::{Harness, WindowRequest};
use oled_ui::model::{ListModel, RowKey, Selection};
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::model::view::{ColumnId, Section};
use tasks::ui::rows::{RowKind, COL_NAME};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectFrame, TasksScreen};

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

struct Rig {
    h: Harness<TasksScreen>,
    exec: Arc<RecordingExecutor>,
    launcher: Arc<RecordingLauncher>,
}

fn rig(s: &Scene) -> Rig {
    let exec = Arc::new(RecordingExecutor::default());
    let launcher = Arc::new(RecordingLauncher::default());
    let h = scene::open(s, PreviewLang::En, 96, launcher.clone(), exec.clone()).unwrap_or_else(|e| panic!("{e}"));
    Rig { h, exec, launcher }
}

fn frame_of(show: &Show) -> Vec<u8> {
    let mut v = Vec::new();
    encode(&InstanceCmd::Show(show.clone()), &mut v).unwrap_or_else(|e| panic!("{e:?}"));
    v
}

fn send(r: &mut Rig, show: &Show) -> bool {
    r.h.copy_data(copydata_tag(), &frame_of(show))
}

fn show(select: Select, sort: SortBy) -> Show {
    Show { select, sort, ..Show::default() }
}

fn selected(r: &Rig) -> Option<RowKey> {
    match r.h.screen().table().selection() {
        Selection::Single(k) => Some(*k),
        _ => None,
    }
}

fn key_of_pid(r: &Rig, pid: u32) -> RowKey {
    let row = r.h.screen().view().procs().rows.iter().find(|x| x.key.pid == pid).unwrap_or_else(|| panic!("pid {pid}"));
    tasks::ui::rows::proc_key(row.key)
}

fn kind_of(r: &Rig, key: RowKey) -> RowKind {
    let m = r.h.screen().table().model();
    m.rows[m.row_of(key).unwrap_or_else(|| panic!("the selected row is shown"))].kind
}

#[test]
fn show_selects_pid_and_expands() {
    let mut r = rig(&load("t3a_part_of_windows_collapsed_hot"));
    let state = r.h.screen().deps().state.clone();
    assert_eq!(state.collapsed(Section::Windows), Some(true), "Part of Windows starts closed in this scene");
    assert!(send(&mut r, &show(Select::Pid(700), SortBy::Auto)), "decoded and applied");
    assert_eq!(state.collapsed(Section::Windows), Some(false), "its section was opened");
    let key = selected(&r).unwrap_or_else(|| panic!("a row is selected"));
    assert_eq!(r.h.screen().table().focused(), Some(key), "and focused");
    assert_eq!(key, key_of_pid(&r, 700), "the row is the process 700");
    assert_eq!(r.h.window_requests(), [WindowRequest::BringToFront], "a forwarded Show comes to the front");
}

#[test]
fn show_selects_identity() {
    let mut r = rig(&load("t3a_grouped"));
    assert!(send(&mut r, &show(Select::Identity("orbit-id".into()), SortBy::Auto)));
    let key = selected(&r).unwrap_or_else(|| panic!("the Orbit group is selected"));
    let RowKind::Group(ak) = kind_of(&r, key) else { panic!("a group row") };
    let tasks::model::group::AppKey::App(id) = ak else { panic!("an app group") };
    assert_eq!(r.h.screen().view().interned(id.0), "orbit-id");

    // An exe key: the program with no window identity.
    let exe = oled_shellmodel::identity::key_from_exe_path(r"C:\Program Files\Syncer\syncer.exe");
    assert!(send(&mut r, &show(Select::Identity(exe.into()), SortBy::Auto)));
    let key = selected(&r).unwrap_or_else(|| panic!("the Syncer group is selected"));
    assert!(matches!(kind_of(&r, key), RowKind::Group(tasks::model::group::AppKey::Exe(_))));
}

#[test]
fn show_selects_identity_in_list_mode() {
    let mut r = rig(&load("t3a_list"));
    let id = "orbit-id";
    assert!(send(&mut r, &show(Select::Identity(id.into()), SortBy::Auto)));
    let key = selected(&r).unwrap_or_else(|| panic!("a row of the Orbit app is selected in List mode"));
    assert!(matches!(kind_of(&r, key), RowKind::Proc(_)), "a process row");
}

#[test]
fn start_select_applies_after_first_paint() {
    use tasks::app::launch::Launcher;
    use tasks::app::Deps;
    let s = load("t1_list_calm");
    let pid = s.procs.last().unwrap_or_else(|| panic!("a process")).pid;
    let l: Arc<dyn Launcher> = Arc::new(RecordingLauncher::default());
    let mut deps = Deps::inert(oled_ui::UiLang::EN, l, Arc::new(RecordingExecutor::default()));
    deps.show = Show { select: Select::Pid(pid), ..Show::default() };
    deps.state.set_view(tasks::model::view::ViewMode::List);
    let mut h = Harness::new(TasksScreen::new(deps), oled_ui::SizeL { w: 640, h: 560 }, 96, oled_ui::Tokens::oled());
    h.render();
    let mut first = s.clone();
    first.ticks = 0;
    for f in first.frames() {
        h.send(Box::new(InjectFrame(f)));
    }
    h.render();
    h.send(Box::new(tasks::app::presence::PresenceMsg { minimised: false, cloaked: false }));
    let want =
        h.screen().view().procs().rows.iter().find(|x| x.key.pid == pid).map(|x| tasks::ui::rows::proc_key(x.key));
    let got = match h.screen().table().selection() {
        Selection::Single(k) => Some(*k),
        _ => None,
    };
    assert!(want.is_some() && got == want, "the start's select was applied once its row came");
}

#[test]
fn a_forwarded_sort_replaces_the_automatic_one() {
    let mut r = rig(&load("t1_list_calm"));
    assert!(send(&mut r, &show(Select::None, SortBy::Memory)));
    let s = r.h.screen().view().sort();
    assert_eq!((s.column, s.descending, s.auto), (ColumnId::Memory, true, false));
    assert!(send(&mut r, &show(Select::None, SortBy::Cpu)));
    assert_eq!(r.h.screen().view().sort().column, ColumnId::Cpu);
}

#[test]
fn pending_select_applies_on_a_later_frame_or_expires() {
    let full = load("t1_list_calm");
    let mut early = full.clone();
    let late = early.procs.pop().unwrap_or_else(|| panic!("a process"));
    let mut r = rig(&early);
    assert!(send(&mut r, &show(Select::Pid(late.pid), SortBy::Auto)), "accepted though the row is not there");
    assert_eq!(selected(&r), None);
    r.h.advance(1000);
    let mut at_first = full.clone();
    at_first.ticks = 0;
    for f in at_first.frames() {
        r.h.send(Box::new(InjectFrame(f)));
    }
    assert_eq!(selected(&r), Some(key_of_pid(&r, late.pid)), "the row came 1 s later and was selected");

    // Another one never comes: the wait ends after 3 s and a later arrival is not selected.
    let mut r = rig(&early);
    assert!(send(&mut r, &show(Select::Pid(late.pid), SortBy::Auto)));
    r.h.advance(3100);
    let mut calm = early.clone();
    calm.ticks = 0;
    for f in calm.frames() {
        r.h.send(Box::new(InjectFrame(f)));
    }
    let mut at_last = full;
    at_last.ticks = 0;
    for f in at_last.frames() {
        r.h.send(Box::new(InjectFrame(f)));
    }
    assert_eq!(selected(&r), None, "expired");
}

#[test]
fn foreign_frame_refused() {
    let mut r = rig(&load("t3a_grouped"));
    let good = frame_of(&show(Select::Pid(2000), SortBy::Memory));
    let before = r.h.screen().view().sort();
    assert!(!r.h.copy_data(copydata_tag() ^ 1, &good), "another tag");
    assert!(!r.h.copy_data(copydata_tag(), &good[..good.len() - 1]), "truncated");
    assert!(!r.h.copy_data(copydata_tag(), &[good.as_slice(), &[0]].concat()), "trailing bytes");
    let mut bad_page = good.clone();
    bad_page[HEADER_LEN] = 9;
    assert!(!r.h.copy_data(copydata_tag(), &bad_page), "a page byte 9");
    assert!(!r.h.copy_data(copydata_tag(), &[]), "empty");
    assert_eq!(selected(&r), None, "no state change");
    assert_eq!(r.h.screen().view().sort(), before);
    assert!(r.h.window_requests().is_empty(), "and no front");
}

#[test]
fn show_never_acts() {
    let mut r = rig(&load("t3a_grouped"));
    for s in [
        show(Select::Pid(2000), SortBy::Cpu),
        show(Select::Identity("orbit-id".into()), SortBy::Memory),
        Show { from_chord: true, ..Show::default() },
    ] {
        assert!(send(&mut r, &s));
    }
    r.h.advance(5000);
    assert!(r.exec.batches.lock().map(|b| b.is_empty()).unwrap_or(false), "no Batch was ever submitted");
    assert!(r.launcher.reqs.lock().map(|b| b.is_empty()).unwrap_or(false), "nothing was launched");
    let _ = COL_NAME;
}

#[test]
fn chord_open_requests_front() {
    use tasks::app::launch::Launcher;
    use tasks::app::Deps;
    let exec = Arc::new(RecordingExecutor::default());
    let l: Arc<dyn Launcher> = Arc::new(RecordingLauncher::default());
    let mut deps = Deps::inert(oled_ui::UiLang::EN, l, exec);
    deps.show = Show { from_chord: true, ..Show::default() };
    let mut h = Harness::new(TasksScreen::new(deps), oled_ui::SizeL { w: 640, h: 560 }, 96, oled_ui::Tokens::oled());
    h.render();
    h.advance(10);
    h.send(Box::new(tasks::app::presence::PresenceMsg { minimised: false, cloaked: false }));
    assert_eq!(h.window_requests(), [WindowRequest::BringToFront]);
    h.advance(1000);
    assert_eq!(h.window_requests(), [WindowRequest::BringToFront], "asked once");

    // A start without the chord asks for nothing.
    let l: Arc<dyn Launcher> = Arc::new(RecordingLauncher::default());
    let deps = Deps::inert(oled_ui::UiLang::EN, l, Arc::new(RecordingExecutor::default()));
    let mut h = Harness::new(TasksScreen::new(deps), oled_ui::SizeL { w: 640, h: 560 }, 96, oled_ui::Tokens::oled());
    h.render();
    h.send(Box::new(tasks::app::presence::PresenceMsg { minimised: false, cloaked: false }));
    assert!(h.window_requests().is_empty());
}
