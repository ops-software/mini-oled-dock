//! T3c's scenes: the rows' verbs, the inline action, busy rows, ended rows with Reopen, the outcome notes and the
//! context menu, over the recording executor (nothing runs), and the goldens (the CI runner's base set, or the local one
//! for looking).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_sysmon::ProcKey;
use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::model::Selection;
use oled_ui::model::{CellOut, FgRole, ListModel, RowAction, RowKey, TableModel};
use oled_ui::MenuItem;
use tasks::app::access::Refusal;
use tasks::app::args::PreviewLang;
use tasks::app::exec::{ActVerb, ActionDone, Batch, Outcome, RecordingExecutor};
use tasks::app::launch::RecordingLauncher;
use tasks::model::group::AppKey;
use tasks::model::view::Section;
use tasks::ui::actions::{self, RowVerb};
use tasks::ui::commands::{CLOSE_OR_END, END_NOW, END_TREE, OPEN_WINDOWS_TM, REOPEN};
use tasks::ui::rows::{group_key, RowEntry, RowKind, COL_NAME};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectCommand, InjectFrame, TasksScreen};

const SCENE: &str = "t3c_actions";

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

struct Rig {
    h: Harness<TasksScreen>,
    exec: Arc<RecordingExecutor>,
}

fn rig_of(scene: &Scene, lang: PreviewLang, dpi: u32) -> Rig {
    let exec = Arc::new(RecordingExecutor::default());
    let launcher = Arc::new(RecordingLauncher::default());
    let h = scene::open(scene, lang, dpi, launcher, exec.clone()).unwrap_or_else(|e| panic!("{e}"));
    Rig { h, exec }
}

fn rig() -> Rig {
    rig_of(&load(SCENE), PreviewLang::En, 96)
}

fn batches(r: &Rig) -> Vec<Batch> {
    r.exec.batches.lock().unwrap().clone()
}

fn rows(h: &Harness<TasksScreen>) -> &[RowEntry] {
    &h.screen().table().model().rows
}

fn cell(h: &Harness<TasksScreen>, row: usize) -> CellOut {
    let mut out = CellOut::default();
    h.screen().table().model().cell(row, COL_NAME, &mut out);
    out
}

fn note(h: &Harness<TasksScreen>, row: usize) -> Option<(String, FgRole)> {
    cell(h, row).note.map(|(t, fg)| (t.into_owned(), fg))
}

fn key_of(pid: u32) -> ProcKey {
    ProcKey { pid, created: 133_500_000_000_000_000 + u64::from(pid) * 1000 }
}

/// The row of the group whose oldest member is `root_pid` (a group of one process of Part of Windows is that process's
/// own row).
fn group_row(h: &Harness<TasksScreen>, root_pid: u32) -> usize {
    let grouping = h.screen().parts().grouped.grouping();
    let g = grouping.groups.iter().find(|g| g.members.first().is_some_and(|m| m.pid == root_pid));
    let key = match g {
        Some(g) if matches!(g.key, AppKey::Single(_)) => tasks::ui::rows::proc_key(g.members[0]),
        Some(g) => group_key(g.key),
        None => panic!("no group of pid {root_pid}"),
    };
    h.screen().table().model().row_of(key).unwrap_or_else(|| panic!("no row of pid {root_pid}"))
}

fn row_of_key(h: &Harness<TasksScreen>, key: RowKey) -> Option<usize> {
    h.screen().table().model().row_of(key)
}

fn centre(h: &Harness<TasksScreen>, row: usize) -> PointL {
    let r = h.screen().table().row_rect(row).expect("row on screen");
    PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
}

fn select(r: &mut Rig, row: usize) -> RowKey {
    let p = centre(&r.h, row);
    r.h.click(p);
    rows(&r.h)[row].key
}

fn select_pid(r: &mut Rig, root_pid: u32) -> RowKey {
    let row = group_row(&r.h, root_pid);
    select(r, row)
}

fn section_row(h: &Harness<TasksScreen>, s: Section) -> usize {
    rows(h).iter().position(|r| r.kind == RowKind::Section(s)).expect("the section row")
}

fn done(r: &mut Rig, row: RowKey, outcome: Outcome, refused: u16) {
    r.h.send(Box::new(ActionDone { row, outcome, refused }));
}

/// The last frame of the scene without the processes of `image`.
fn frame_without(image: &str) -> tasks::engine::frame::Frame {
    let mut s = load(SCENE);
    let gone: Vec<u32> = s.procs.iter().filter(|p| p.image == image).map(|p| p.pid).collect();
    s.procs.retain(|p| p.image != image);
    s.windows.retain(|w| !gone.contains(&w.pid));
    s.frames().pop().expect("a frame")
}

// ---- T3.8 the row verbs and their states

#[test]
fn verbs_by_row() {
    let mut r = rig();
    let verb = |r: &Rig, row: usize| {
        let sc = r.h.screen();
        actions::row_verb(&sc.parts().actions, sc.view(), &rows(&r.h)[row])
    };
    assert_eq!(verb(&r, group_row(&r.h, 2000)), Some(RowVerb::Close), "a responding app with windows");
    assert_eq!(verb(&r, group_row(&r.h, 5000)), Some(RowVerb::End), "a hung app");
    assert_eq!(verb(&r, group_row(&r.h, 7000)), Some(RowVerb::End), "a Background group");
    assert_eq!(verb(&r, group_row(&r.h, 7500)), Some(RowVerb::End), "a Background process");
    assert_eq!(verb(&r, group_row(&r.h, 900)), Some(RowVerb::EndTask), "Part of Windows");
    assert_eq!(verb(&r, group_row(&r.h, 600)), Some(RowVerb::EndTask));
    for s in [Section::Apps, Section::Background, Section::Windows] {
        assert_eq!(verb(&r, section_row(&r.h, s)), None);
    }
    let hw = rows(&r.h).iter().position(|x| x.kind == RowKind::Hardware).expect("the hardware row");
    assert_eq!(verb(&r, hw), None);
    // The members of an open group.
    let g = group_row(&r.h, 2000);
    select(&mut r, g);
    r.h.key("Right");
    let g = group_row(&r.h, 2000);
    assert_eq!(rows(&r.h)[g + 1].depth, 1);
    assert_eq!(verb(&r, g + 1), Some(RowVerb::End), "a member");
    // Tree and List: every process row ends, except a Part of Windows process: End task, as in Grouped mode.
    let grouping = r.h.screen().parts().grouped.grouping();
    let windows: Vec<ProcKey> =
        grouping.groups.iter().filter(|g| g.section == Section::Windows).flat_map(|g| g.members.clone()).collect();
    assert!(windows.contains(&key_of(900)) && !windows.contains(&key_of(1000)));
    for mode in ["Ctrl+T", "Ctrl+L"] {
        r.h.key(mode);
        for (i, x) in rows(&r.h).iter().enumerate() {
            let RowKind::Proc(p) = x.kind else { continue };
            let want = if windows.contains(&p) { RowVerb::EndTask } else { RowVerb::End };
            assert_eq!(verb(&r, i), Some(want), "{mode}: pid {}", p.pid);
        }
    }
}

#[test]
fn inline_action_only_on_apps_and_background() {
    let mut r = rig();
    let action = |h: &Harness<TasksScreen>, row: usize| cell(h, row).action;
    let close = Some(RowAction { label: "Close".into(), command: CLOSE_OR_END });
    let end = Some(RowAction { label: "End".into(), command: CLOSE_OR_END });
    assert_eq!(action(&r.h, group_row(&r.h, 2000)), close);
    assert_eq!(action(&r.h, group_row(&r.h, 3000)), close);
    assert_eq!(action(&r.h, group_row(&r.h, 5000)), end, "hung: End");
    assert_eq!(action(&r.h, group_row(&r.h, 7000)), end);
    assert_eq!(action(&r.h, group_row(&r.h, 7500)), end);
    // Part of Windows (the menu only), the shell (refused), sections and the hardware row.
    for pid in [900, 600, 800, 1000] {
        assert_eq!(action(&r.h, group_row(&r.h, pid)), None, "pid {pid}");
    }
    for (i, x) in rows(&r.h).iter().enumerate() {
        if matches!(x.kind, RowKind::Section(_) | RowKind::Hardware) {
            assert_eq!(action(&r.h, i), None);
        }
    }
    // Members of an open group have none; List has none.
    let g = group_row(&r.h, 2000);
    select(&mut r, g);
    r.h.key("Right");
    let g = group_row(&r.h, 2000);
    assert_eq!(action(&r.h, g + 1), None);
    r.h.key("Ctrl+L");
    assert!((0..rows(&r.h).len()).all(|i| action(&r.h, i).is_none()));
    r.h.assert_named();
}

#[test]
fn del_submits_one_batch_through_the_executor() {
    let mut r = rig();
    let key = select_pid(&mut r, 2000);
    r.h.key("Del");
    let b = batches(&r);
    assert_eq!(b.len(), 1, "one batch, through the executor");
    assert_eq!(b[0].verb, ActVerb::Close);
    assert_eq!(b[0].targets.len(), 1);
    assert_eq!(b[0].targets[0].row, key);
    assert_eq!(b[0].targets[0].windows, [0x1000 + 2000], "WM_CLOSE to the group's window");
    assert_eq!(b[0].targets[0].members.len(), 4);
    // Del on Part of Windows is refused with the notice line: nothing is submitted.
    select_pid(&mut r, 900);
    r.h.key("Del");
    r.h.key("Shift+Del");
    assert_eq!(batches(&r).len(), 1);
    assert!(r.h.screen().notices_shown(), "Use the menu to end Windows processes");
    // A second Del on a row in flight submits nothing more.
    select_pid(&mut r, 2000);
    r.h.key("Del");
    assert_eq!(batches(&r).len(), 1);
}

#[test]
fn del_on_a_windows_process_in_list_is_refused() {
    let mut r = rig();
    r.h.key("Ctrl+L");
    let row = rows(&r.h).iter().position(|x| x.kind == RowKind::Proc(key_of(900))).expect("dwm.exe");
    select(&mut r, row);
    r.h.key("Del");
    r.h.key("Shift+Del");
    assert!(batches(&r).is_empty(), "nothing submitted");
    assert!(r.h.screen().notices_shown(), "Use the menu to end Windows processes");
    // The menu is the way: End task, with its warning and no Del hint.
    let row = rows(&r.h).iter().position(|x| x.kind == RowKind::Proc(key_of(900))).expect("dwm.exe");
    let at = centre(&r.h, row);
    r.h.right_click(at);
    let items = r.h.menus().last().expect("a menu").items.clone();
    let end_task = Some((s("End task"), None, true, Some(s("Windows may need it; other things can close with it"))));
    assert_eq!(item(&items, CLOSE_OR_END), end_task);
    assert_eq!(item(&items, END_NOW), None);
}

#[test]
fn end_reaches_the_tree_and_a_process_row_only_itself() {
    let mut r = rig();
    select_pid(&mut r, 2000);
    r.h.key("Shift+Del");
    let b = batches(&r);
    assert_eq!(b[0].verb, ActVerb::EndNow);
    assert!(!b[0].edges.is_empty(), "a group takes its descendants");
    let guard = &b[0].guard;
    assert_eq!(guard.shell, Some(key_of(1000)), "the scene's shell is never reached");
    // A member alone.
    let mut r = rig();
    let g = group_row(&r.h, 3000);
    select(&mut r, g);
    r.h.key("Right");
    let g = group_row(&r.h, 3000);
    select(&mut r, g + 1);
    r.h.key("Shift+Del");
    let b = batches(&r);
    assert_eq!(b[0].targets[0].members.len(), 1);
    assert!(b[0].edges.is_empty(), "a process row ends only itself");
}

#[test]
fn busy_after_300_ms() {
    let mut r = rig();
    select_pid(&mut r, 7500);
    r.h.key("Del");
    assert_eq!(batches(&r)[0].verb, ActVerb::End);
    let row = group_row(&r.h, 7500);
    assert_eq!(note(&r.h, row), None, "a quick act shows nothing");
    r.h.advance(250);
    assert_eq!(note(&r.h, group_row(&r.h, 7500)), None);
    r.h.advance(60);
    let row = group_row(&r.h, 7500);
    assert_eq!(note(&r.h, row), Some(("Ending...".into(), FgRole::Secondary)));
    assert!(rows(&r.h)[row].out.busy);
    assert_eq!(cell(&r.h, row).action, None, "no second act while one is out");
    assert!(r.h.screen().parts().actions.is_busy(), "the order holds");
}

#[test]
fn ended_reopen_for_10_s() {
    let mut r = rig();
    let key = select_pid(&mut r, 2000);
    r.h.key("Shift+Del");
    done(&mut r, key, Outcome::Done, 0);
    r.h.send(Box::new(InjectFrame(frame_without("orbit.exe"))));
    let row = row_of_key(&r.h, key).expect("the ended app keeps its row");
    assert!(rows(&r.h)[row].out.ghost);
    assert_eq!(note(&r.h, row), Some(("Ended \u{b7} Reopen".into(), FgRole::Disabled)));
    assert_eq!(cell(&r.h, row).action, Some(RowAction { label: "Reopen".into(), command: REOPEN }));
    assert_eq!(r.h.screen().table().model().name(row), "Orbit (4)");
    assert_eq!(rows(&r.h)[row].section, Some(Section::Apps), "in its section");
    // Reopen: the launch string of the app through the executor; the ghost goes.
    r.h.send(Box::new(InjectCommand(REOPEN)));
    let b = batches(&r);
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].verb, ActVerb::Reopen);
    assert_eq!(b[1].targets[0].launch.as_deref(), Some("C:\\Program Files\\Orbit\\orbit.exe"));
    assert_eq!(row_of_key(&r.h, key), None);
    // Unused, it goes after 10 s.
    let mut r = rig();
    let key = select_pid(&mut r, 2000);
    r.h.key("Shift+Del");
    done(&mut r, key, Outcome::Done, 0);
    r.h.send(Box::new(InjectFrame(frame_without("orbit.exe"))));
    r.h.advance(9_900);
    assert!(row_of_key(&r.h, key).is_some());
    r.h.advance(200);
    assert_eq!(row_of_key(&r.h, key), None, "after 10 s");
    assert!(r.h.is_idle(), "no timer is left");
}

#[test]
fn needs_admin_note_6_s() {
    let mut r = rig();
    let key = select_pid(&mut r, 7500);
    r.h.key("Del");
    done(&mut r, key, Outcome::NeedsAdmin, 0);
    let row = group_row(&r.h, 7500);
    assert_eq!(note(&r.h, row), Some(("Couldn't end: needs administrator".into(), FgRole::Danger)));
    assert!(!rows(&r.h)[row].out.busy);
    r.h.advance(5_900);
    assert!(note(&r.h, group_row(&r.h, 7500)).is_some());
    r.h.advance(200);
    assert_eq!(note(&r.h, group_row(&r.h, 7500)), None);
    assert!(r.h.is_idle());
}

#[test]
fn refused_note_names_the_reason() {
    let mut r = rig();
    let key = select_pid(&mut r, 1000);
    r.h.key("Del");
    assert_eq!(batches(&r).len(), 1, "the executor refuses, not the key");
    done(&mut r, key, Outcome::Refused(Refusal::Shell), 1);
    let row = group_row(&r.h, 1000);
    assert_eq!(note(&r.h, row), Some(("Not ended: The taskbar and desktop would close".into(), FgRole::Attention)));
    // Ended with some kept, and a failure with its code.
    let mut r = rig();
    let key = select_pid(&mut r, 7000);
    r.h.key("Del");
    done(&mut r, key, Outcome::Done, 2);
    let n = note(&r.h, group_row(&r.h, 7000)).expect("a note").0;
    assert_eq!(n, "Ended \u{b7} 2 kept running: Windows needs them");
    let mut r = rig();
    let key = select_pid(&mut r, 7500);
    r.h.key("Del");
    done(&mut r, key, Outcome::Failed(31), 0);
    assert_eq!(note(&r.h, group_row(&r.h, 7500)), Some(("Couldn't end (31)".into(), FgRole::Danger)));
}

#[test]
fn close_then_still_open_offers_end_now() {
    let mut r = rig();
    let key = select_pid(&mut r, 2000);
    r.h.key("Del");
    done(&mut r, key, Outcome::Posted, 0);
    r.h.advance(300);
    assert_eq!(note(&r.h, group_row(&r.h, 2000)), Some(("Closing...".into(), FgRole::Secondary)));
    r.h.advance(4_800);
    let row = group_row(&r.h, 2000);
    assert_eq!(note(&r.h, row), Some(("Still open: it may be asking you something".into(), FgRole::Attention)));
    assert_eq!(cell(&r.h, row).action, Some(RowAction { label: "End now".into(), command: END_NOW }));
    assert_eq!(batches(&r).len(), 1, "Tasks never ends anything by itself");
    // The app closes after all: the row goes with it.
    r.h.send(Box::new(InjectFrame(frame_without("orbit.exe"))));
    assert_eq!(row_of_key(&r.h, key), None);
    // The order's hold, which the row in flight kept, ends on its own clock.
    r.h.advance(1_000);
    assert!(r.h.is_idle());
}

#[test]
fn close_that_posted_nothing_shows_no_ended() {
    let mut r = rig();
    let key = select_pid(&mut r, 2000);
    r.h.key("Del");
    assert_eq!(batches(&r)[0].verb, ActVerb::Close);
    // No window of the group qualified when the batch ran: nothing was posted and the app still runs.
    done(&mut r, key, Outcome::Gone, 0);
    let row = row_of_key(&r.h, key).expect("its row");
    assert!(!rows(&r.h)[row].out.ghost, "a running app is not shown as ended");
    assert_eq!(note(&r.h, row), None);
    assert_eq!(cell(&r.h, row).action, Some(RowAction { label: "Close".into(), command: CLOSE_OR_END }));
    assert!(!r.h.screen().parts().actions.is_busy());
}

#[test]
fn inline_action_acts_on_its_row() {
    let mut r = rig();
    let row = group_row(&r.h, 7500);
    let y = centre(&r.h, row).y;
    // The action sits at the right end of the Name column, clickable once fully shown: find it from the right.
    let found = (200..640).rev().step_by(4).find(|&x| {
        let at = PointL { x, y };
        r.h.hover(at);
        r.h.advance(150);
        r.h.click(at);
        !batches(&r).is_empty()
    });
    assert!(found.is_some(), "the inline action answers a click");
    let b = batches(&r);
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].verb, b[0].targets[0].row), (ActVerb::End, rows(&r.h)[group_row(&r.h, 7500)].key));
}

// ---- the goldens

/// The cell name of a golden: "<lang>-<dpi>".
fn cell_name(lang: PreviewLang, dpi: u32) -> String {
    let l = match lang {
        PreviewLang::En => "en",
        PreviewLang::It => "it",
        PreviewLang::Pseudo => "pseudo",
    };
    format!("{l}-{dpi}")
}

/// OLED in English and Italian at four DPIs, and the pseudo-locale at 144 (the kit's matrix).
fn matrix() -> Vec<(PreviewLang, u32)> {
    let mut v = Vec::new();
    for lang in [PreviewLang::En, PreviewLang::It] {
        for dpi in [96, 120, 144, 192] {
            v.push((lang, dpi));
        }
    }
    v.push((PreviewLang::Pseudo, 144));
    v
}

/// The centre of a row in logical px at `dpi` (the Harness takes logical points).
fn centre_at(h: &Harness<TasksScreen>, row: usize, dpi: u32) -> PointL {
    let r = h.screen().table().row_rect(row).expect("row on screen");
    let d = dpi as i32;
    PointL { x: (r.l + r.r) / 2 * 96 / d, y: (r.t + r.b) / 2 * 96 / d }
}

fn click_pid(r: &mut Rig, pid: u32, dpi: u32) -> RowKey {
    let row = group_row(&r.h, pid);
    let at = centre_at(&r.h, row, dpi);
    r.h.click(at);
    rows(&r.h)[row].key
}

/// The scene driven into one of T3c's states.
fn state(page: &str, lang: PreviewLang, dpi: u32) -> Rig {
    let mut r = rig_of(&load(SCENE), lang, dpi);
    match page {
        "t3c_hover_action" => {
            let at = centre_at(&r.h, group_row(&r.h, 2000), dpi);
            r.h.hover(at);
            r.h.advance(200);
        }
        "t3c_busy" => {
            click_pid(&mut r, 7500, dpi);
            r.h.key("Del");
            r.h.advance(300);
        }
        "t3c_ended_reopen" => {
            let key = click_pid(&mut r, 2000, dpi);
            r.h.key("Shift+Del");
            done(&mut r, key, Outcome::Done, 0);
            r.h.send(Box::new(InjectFrame(frame_without("orbit.exe"))));
        }
        "t3c_needs_admin" => {
            let key = click_pid(&mut r, 7500, dpi);
            r.h.key("Del");
            done(&mut r, key, Outcome::NeedsAdmin, 0);
        }
        "t3c_menu_app" => {
            let at = centre_at(&r.h, group_row(&r.h, 2000), dpi);
            r.h.right_click(at);
        }
        "t3c_menu_critical" => {
            let at = centre_at(&r.h, group_row(&r.h, 600), dpi);
            r.h.right_click(at);
        }
        other => panic!("no state {other}"),
    }
    r
}

const PAGES: [&str; 6] =
    ["t3c_hover_action", "t3c_menu_app", "t3c_menu_critical", "t3c_busy", "t3c_ended_reopen", "t3c_needs_admin"];

#[test]
fn golden_t3c_states_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for page in PAGES {
        for (lang, dpi) in matrix() {
            let mut r = state(page, lang, dpi);
            r.h.assert_named();
            let img = if page.starts_with("t3c_menu") {
                r.h.render();
                r.h.menu_image(0).expect("the menu")
            } else {
                r.h.render()
            };
            golden::check(&root, &diff, &format!("{page}/{}", cell_name(lang, dpi)), &img);
            if lang == PreviewLang::En && dpi == 96 {
                golden::check_text(&root, &diff, &format!("{page}/a11y.txt"), &r.h.a11y_dump());
            }
        }
    }
}

// ---- T3.9 the context menu

fn menu_of(r: &mut Rig, pid: u32) -> Vec<MenuItem> {
    let at = centre(&r.h, group_row(&r.h, pid));
    let before = r.h.menus().len();
    r.h.right_click(at);
    assert_eq!(r.h.menus().len(), before + 1, "a menu opened");
    r.h.menus().last().expect("a menu").items.clone()
}

/// (label, chord, enabled, note) of the item of `id`.
type Item = (String, Option<String>, bool, Option<String>);

fn item(items: &[MenuItem], id: oled_ui::CommandId) -> Option<Item> {
    items.iter().find_map(|i| match i {
        MenuItem::Custom { id: x, label, chord, enabled, note, .. } if *x == id => Some((
            label.to_string(),
            chord.as_ref().map(|c| c.to_string()),
            *enabled,
            note.as_ref().map(|n| n.to_string()),
        )),
        _ => None,
    })
}

fn header(items: &[MenuItem]) -> String {
    match items.first() {
        Some(MenuItem::Header(t)) => t.to_string(),
        other => panic!("no header: {other:?}"),
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

#[test]
fn menu_for_app_group() {
    let mut r = rig();
    let items = menu_of(&mut r, 2000);
    assert_eq!(header(&items), "Orbit \u{b7} 4 processes");
    assert_eq!(
        item(&items, CLOSE_OR_END),
        Some((s("Close Orbit"), Some(s("Del")), true, Some(s("Asks it to close. It may ask to save."))))
    );
    assert_eq!(item(&items, END_NOW), Some((s("End now"), Some(s("Shift+Del")), true, None)));
    assert_eq!(item(&items, END_TREE), None, "End process tree is for process rows in Tree and List");
    let tm = item(&items, OPEN_WINDOWS_TM).expect("Open Windows Task Manager");
    assert!(tm.2 && tm.3.is_some(), "enabled, with its description");
    assert!(matches!(items[items.len() - 2], MenuItem::Separator));
    // The right click selected the row; a pick acts on the rows the menu showed.
    let key = rows(&r.h)[group_row(&r.h, 2000)].key;
    assert_eq!(r.h.screen().table().selection().single(), Some(key));
    assert_eq!(r.h.screen().parts().actions.menu_target(), Some(&[key][..]));
    r.h.send(Box::new(InjectCommand(CLOSE_OR_END)));
    let b = batches(&r);
    assert_eq!((b.len(), b[0].verb, b[0].targets[0].row), (1, ActVerb::Close, key));
    // The keyboard opens it too.
    r.h.key("Esc");
    let before = r.h.menus().len();
    r.h.key("Shift+F10");
    assert_eq!(r.h.menus().len(), before + 1);
}

#[test]
fn menu_for_part_of_windows() {
    let mut r = rig();
    let items = menu_of(&mut r, 900);
    assert_eq!(header(&items), "dwm.exe \u{b7} 900");
    assert_eq!(
        item(&items, CLOSE_OR_END),
        Some((s("End task"), None, true, Some(s("Windows may need it; other things can close with it")))),
        "End task with its description, and no Del hint"
    );
    assert_eq!(item(&items, END_NOW), None);
    // From the menu it acts; the same keys refuse.
    r.h.send(Box::new(InjectCommand(CLOSE_OR_END)));
    let b = batches(&r);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].verb, ActVerb::End);
    assert_eq!(b[0].targets[0].members, [key_of(900)]);
    assert_eq!(r.h.screen().parts().actions.menu_target(), None, "the pick ended the menu");
    let mut r = rig();
    let at = centre(&r.h, group_row(&r.h, 900));
    r.h.click(at);
    r.h.key("Del");
    assert!(batches(&r).is_empty());
}

#[test]
fn menu_for_multi() {
    let mut r = rig();
    // The first three rows of Apps.
    let first = section_row(&r.h, Section::Apps) + 1;
    let at = centre(&r.h, first);
    r.h.click(at);
    r.h.key("Shift+Down");
    r.h.key("Shift+Down");
    let picked: Vec<RowKey> = rows(&r.h)[first..first + 3].iter().map(|x| x.key).collect();
    assert!(!matches!(r.h.screen().table().selection(), Selection::None | Selection::Single(_)), "a multi-selection");
    let at = centre(&r.h, first + 1);
    r.h.right_click(at);
    let items = r.h.menus().last().expect("a menu").items.clone();
    assert_eq!(header(&items), "3 apps");
    assert_eq!(item(&items, END_NOW).map(|i| (i.0, i.1, i.2)), Some((s("End 3 apps now"), Some(s("Shift+Del")), true)));
    let all_close = picked.iter().all(|k| {
        let row = &rows(&r.h)[row_of_key(&r.h, *k).unwrap()];
        actions::row_verb(&r.h.screen().parts().actions, r.h.screen().view(), row) == Some(RowVerb::Close)
    });
    assert_eq!(item(&items, CLOSE_OR_END).map(|i| i.0), all_close.then(|| s("Close 3 apps")));
    // One batch over the three rows; a refused one is skipped by the executor and named in its note.
    r.h.send(Box::new(InjectCommand(END_NOW)));
    let b = batches(&r);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].targets.iter().map(|t| t.row).collect::<Vec<_>>(), picked);
}

#[test]
fn menu_in_tree_offers_end_process_tree() {
    let mut r = rig();
    r.h.key("Ctrl+T");
    let row = rows(&r.h).iter().position(|x| x.kind == RowKind::Proc(key_of(2000))).expect("orbit's root");
    let at = centre(&r.h, row);
    r.h.right_click(at);
    let items = r.h.menus().last().expect("a menu").items.clone();
    assert_eq!(header(&items), "orbit.exe \u{b7} 2000");
    assert_eq!(item(&items, CLOSE_OR_END).map(|i| i.0), Some(s("End orbit.exe")));
    assert_eq!(item(&items, END_TREE).map(|i| (i.0, i.2)), Some((s("End process tree"), true)));
    r.h.send(Box::new(InjectCommand(END_TREE)));
    let b = batches(&r);
    assert_eq!((b[0].verb, b[0].targets[0].members.clone()), (ActVerb::EndTree, vec![key_of(2000)]));
    assert!(!b[0].edges.is_empty(), "the tree is the descendants");
}

#[test]
fn rows_without_a_verb_open_no_menu() {
    let mut r = rig();
    for row in [section_row(&r.h, Section::Apps), rows(&r.h).iter().position(|x| x.kind == RowKind::Hardware).unwrap()]
    {
        let at = centre(&r.h, row);
        r.h.right_click(at);
    }
    assert!(r.h.menus().is_empty());
    assert_eq!(r.h.screen().parts().actions.menu_target(), None);
}
