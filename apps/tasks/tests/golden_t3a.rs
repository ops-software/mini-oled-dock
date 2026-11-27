//! T3a's scenes: the goldens (the CI runner's base set, or the local one for looking) and the pure tests over scene
//! frames (the sections, the groups, the hot rows that escape, Tree and List).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::model::{CellOut, Selection, TableModel};
use oled_ui::theme::Tokens;
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::model::group::AppKey;
use tasks::model::view::{Section, ViewMode};
use tasks::ui::rows::{group_key, proc_key, RowEntry, RowKind, COL_CPU, COL_IO, COL_MEMORY, COL_NAME, COL_PID};
use tasks::ui::scene::{self, Scene, SceneProc};
use tasks::ui::screen::TasksScreen;

const SCENES: [&str; 5] = ["t3a_grouped", "t3a_part_of_windows_collapsed_hot", "t3a_tree", "t3a_list", "t3a_hung"];

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

fn rig(scene: &Scene) -> Harness<TasksScreen> {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    scene::open(scene, PreviewLang::En, 96, launcher, exec).unwrap_or_else(|e| panic!("{e}"))
}

fn open(name: &str) -> Harness<TasksScreen> {
    rig(&load(name))
}

fn cell(h: &Harness<TasksScreen>, row: usize, col: oled_ui::model::ColId) -> CellOut {
    let mut out = CellOut::default();
    h.screen().table().model().cell(row, col, &mut out);
    out
}

fn rows(h: &Harness<TasksScreen>) -> &[RowEntry] {
    &h.screen().table().model().rows
}

fn label(h: &Harness<TasksScreen>, row: usize) -> String {
    h.screen().table().model().name(row).to_string()
}

fn section_row(h: &Harness<TasksScreen>, s: Section) -> usize {
    rows(h).iter().position(|r| r.kind == RowKind::Section(s)).expect("the section row")
}

fn proc_row(h: &Harness<TasksScreen>, pid: u32) -> usize {
    rows(h)
        .iter()
        .position(|r| matches!(r.kind, RowKind::Proc(k) if k.pid == pid))
        .unwrap_or_else(|| panic!("no row of pid {pid}"))
}

fn group_row(h: &Harness<TasksScreen>, root_pid: u32) -> usize {
    let g = h.screen().table().model().rows.iter().position(|r| match r.kind {
        RowKind::Group(AppKey::App(_) | AppKey::Exe(_) | AppKey::Image(_)) => {
            let grouping = h.screen().parts().grouped.grouping();
            grouping
                .groups
                .iter()
                .any(|x| group_key(x.key) == r.key && x.members.first().is_some_and(|m| m.pid == root_pid))
        }
        _ => false,
    });
    g.unwrap_or_else(|| panic!("no group row of pid {root_pid}"))
}

fn centre(h: &Harness<TasksScreen>, row: usize) -> PointL {
    let r = h.screen().table().row_rect(row).expect("row on screen");
    PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
}

// ---- the goldens

#[derive(Clone, Copy)]
struct Cellx {
    lang: PreviewLang,
    dpi: u32,
}

impl Cellx {
    fn name(&self) -> String {
        let lang = match self.lang {
            PreviewLang::En => "en",
            PreviewLang::It => "it",
            PreviewLang::Pseudo => "pseudo",
        };
        format!("{lang}-{}", self.dpi)
    }
}

/// OLED in English and Italian at four DPIs, and the pseudo-locale at 144.
fn matrix() -> Vec<Cellx> {
    let mut v = Vec::new();
    for lang in [PreviewLang::En, PreviewLang::It] {
        for dpi in [96, 120, 144, 192] {
            v.push(Cellx { lang, dpi });
        }
    }
    v.push(Cellx { lang: PreviewLang::Pseudo, dpi: 144 });
    v
}

#[test]
fn golden_every_scene_matches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for name in SCENES {
        let scene = load(name);
        for c in matrix() {
            let launcher = Arc::new(RecordingLauncher::default());
            let exec = Arc::new(RecordingExecutor::default());
            let mut h = scene::open_with(&scene, c.lang, c.dpi, Tokens::oled(), launcher, exec)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            h.assert_named();
            golden::check(&root, &diff, &format!("{name}/{}", c.name()), &h.render());
            if c.lang == PreviewLang::En && c.dpi == 96 {
                golden::check_text(&root, &diff, &format!("{name}/a11y.txt"), &h.a11y_dump());
            }
        }
    }
}

// ---- Grouped

#[test]
fn sections_and_counts() {
    let h = open("t3a_grouped");
    let order: Vec<Section> = rows(&h)
        .iter()
        .filter_map(|r| match r.kind {
            RowKind::Section(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(order, [Section::Apps, Section::Background, Section::Windows]);
    assert_eq!(label(&h, section_row(&h, Section::Apps)), "Apps (4)");
    assert_eq!(label(&h, section_row(&h, Section::Background)), "Background (3)");
    assert_eq!(label(&h, section_row(&h, Section::Windows)), "Part of Windows (9)");
    // The count is its own tertiary span.
    let c = cell(&h, section_row(&h, Section::Apps), COL_NAME);
    assert_eq!(c.n_spans, 1);
    assert_eq!(&c.text[c.spans[0].start as usize..c.spans[0].end as usize], " (4)");
    assert_eq!(c.spans[0].fg, oled_ui::model::FgRole::Tertiary);
    h.assert_named();
}

#[test]
fn game_stays_its_own_app() {
    let h = open("t3a_grouped");
    let g = h.screen().parts().grouped.grouping();
    let of = |pid: u32| g.groups.iter().find(|x| x.members.iter().any(|m| m.pid == pid)).expect("a group");
    let ember = of(3000);
    assert_eq!(ember.section, Section::Apps);
    assert_eq!(ember.members.iter().map(|m| m.pid).collect::<Vec<_>>(), [3000, 3100], "its own window, its own child");
    assert_ne!(of(1000).key, ember.key, "the shell that launched it does not absorb it");
    let helper = of(3500);
    assert_eq!(helper.section, Section::Background, "a windowless child of the shell stays out of the shell's app");
}

#[test]
fn children_absorbed_into_app() {
    let h = open("t3a_grouped");
    let orbit = h
        .screen()
        .parts()
        .grouped
        .grouping()
        .groups
        .iter()
        .find(|x| x.members.iter().any(|m| m.pid == 2110))
        .unwrap()
        .clone();
    assert_eq!(orbit.section, Section::Apps);
    assert_eq!(orbit.members.len(), 5, "a three-level tree is one app");
    // Its row shows the count; the resolver's name leads.
    let r = group_row(&h, 2000);
    assert_eq!(label(&h, r), "Orbit (5)");
    assert!(rows(&h)[r].expandable && !rows(&h)[r].expanded);
    // The ApplicationFrameHost-hosted app is its own app, its host stays in Part of Windows.
    let calc = h
        .screen()
        .parts()
        .grouped
        .grouping()
        .groups
        .iter()
        .find(|x| x.members.iter().any(|m| m.pid == 4100))
        .unwrap()
        .clone();
    assert_eq!(calc.section, Section::Apps);
    let host = h
        .screen()
        .parts()
        .grouped
        .grouping()
        .groups
        .iter()
        .find(|x| x.members.iter().any(|m| m.pid == 4000))
        .unwrap()
        .clone();
    assert_eq!(host.section, Section::Windows);
}

#[test]
fn members_show_under_an_open_group() {
    let mut h = open("t3a_grouped");
    let r = group_row(&h, 2000);
    let p = centre(&h, r);
    h.click(p);
    h.key("Right");
    let r = group_row(&h, 2000);
    assert!(rows(&h)[r].expanded, "the chevron opened");
    let key = rows(&h)[r].key;
    let kids: Vec<&RowEntry> = rows(&h)[r + 1..r + 6].iter().collect();
    assert!(kids.iter().all(|k| k.parent == Some(key) && k.depth == 1), "members sit under their group");
    assert!(label(&h, r + 1).starts_with("orbit.exe \u{b7} "), "image name and pid: {}", label(&h, r + 1));
    assert!(h.screen().parts().grouped.is_expanded(key));
    h.key("Left");
    assert!(!rows(&h)[group_row(&h, 2000)].expanded);
    h.assert_named();
}

#[test]
fn hot_rows_escape() {
    let h = open("t3a_part_of_windows_collapsed_hot");
    let at_windows = section_row(&h, Section::Windows);
    let shown: Vec<u32> = rows(&h)[at_windows + 1..]
        .iter()
        .map(|r| match r.kind {
            RowKind::Proc(k) => k.pid,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(shown, [950, 800], "the two hot rows, the busiest first; nothing else of the section");
    assert!(!rows(&h)[at_windows].expanded);
    // Five hot processes: never more than three.
    let mut s = load("t3a_part_of_windows_collapsed_hot");
    for (i, pid) in [700, 710, 900].into_iter().enumerate() {
        let p: &mut SceneProc = s.procs.iter_mut().find(|p| p.pid == pid).unwrap();
        p.cpu = vec![10.0 + i as f32];
    }
    let h = rig(&s);
    let at_windows = section_row(&h, Section::Windows);
    assert_eq!(rows(&h).len() - at_windows - 1, 3, "at most three");
}

#[test]
fn hardware_row_value() {
    let h = open("t3a_grouped");
    let r = rows(&h).iter().position(|r| r.kind == RowKind::Hardware).expect("the hardware row");
    assert_eq!(r, section_row(&h, Section::Windows) + 1, "first under Part of Windows");
    let c = cell(&h, r, COL_CPU);
    assert_eq!(c.text, "1.4", "the interrupt and driver share of the scene");
    assert!(cell(&h, r, COL_MEMORY).text.is_empty() && cell(&h, r, COL_IO).text.is_empty(), "no memory, no I/O");
    assert_eq!(label(&h, r), "Hardware and drivers");
    assert!(rows(&h)[r].out.disabled, "the row is marked as no target");
    // A collapsed section has none.
    let h = open("t3a_part_of_windows_collapsed_hot");
    assert!(rows(&h).iter().all(|r| r.kind != RowKind::Hardware));
}

#[test]
fn service_host_name() {
    let h = open("t3a_grouped");
    let r = proc_row(&h, 800);
    assert_eq!(label(&h, r), "Service host: Windows Audio +1");
    let c = cell(&h, r, COL_NAME);
    assert_eq!(&c.text[c.spans[0].start as usize..c.spans[0].end as usize], " +1");
    assert_eq!(label(&h, proc_row(&h, 804)), "Service host: DHCP Client");
    assert_eq!(label(&h, proc_row(&h, 808)), "svchost.exe", "no service known: the image name");
}

#[test]
fn section_rows_have_no_heat() {
    let h = open("t3a_grouped");
    for s in [Section::Apps, Section::Background, Section::Windows] {
        let r = section_row(&h, s);
        assert!(rows(&h)[r].out.section);
        for col in [COL_CPU, COL_MEMORY, COL_IO] {
            assert_eq!(cell(&h, r, col).heat, None, "{s:?}");
        }
        assert!(!cell(&h, r, COL_CPU).text.is_empty(), "the total is there");
    }
    // A group row has the heat of its totals.
    let r = group_row(&h, 3000);
    assert!(cell(&h, r, COL_CPU).heat.is_some(), "ember: 10 % of the processor");
}

#[test]
fn collapsed_state_saved() {
    let mut h = open("t3a_part_of_windows_collapsed_hot");
    let state = h.screen().deps().state.clone();
    assert_eq!(state.collapsed(Section::Windows), Some(true), "told at open");
    assert_eq!(state.collapsed(Section::Apps), None);
    let r = section_row(&h, Section::Windows);
    let p = centre(&h, r);
    h.click(p);
    h.key("Right");
    assert_eq!(state.collapsed(Section::Windows), Some(false));
    assert!(rows(&h).iter().any(|r| r.kind == RowKind::Hardware), "open now");
    let r = section_row(&h, Section::Apps);
    let p = centre(&h, r);
    h.click(p);
    h.key("Left");
    assert_eq!(state.collapsed(Section::Apps), Some(true));
    // Closed, Apps keeps only its hot rows.
    let (a, b) = (section_row(&h, Section::Apps), section_row(&h, Section::Background));
    let shown: Vec<&RowEntry> = rows(&h)[a + 1..b].iter().collect();
    assert!(
        shown.len() <= 3 && shown.iter().all(|r| r.vals.heat.iter().any(|x| *x >= oled_sysmon::heat::HeatStep::S3))
    );
}

#[test]
fn grouped_order_is_per_section() {
    let mut h = open("t3a_grouped");
    // Open the biggest group, then sort by Memory: no group leaves its section, members stay under their group.
    let r = group_row(&h, 2000);
    let p = centre(&h, r);
    h.click(p);
    h.key("Right");
    h.key("Ctrl+Shift+3");
    let mut section = None;
    let mut group = None;
    for r in rows(&h) {
        match r.kind {
            RowKind::Section(s) => {
                section = Some(s);
                group = None;
                assert_eq!(r.parent, None);
            }
            RowKind::Group(_) => {
                assert_eq!(r.section, section, "a group sits under its own section");
                group = Some(r.key);
                assert_eq!(r.parent, Some(tasks::ui::rows::section_key(section.unwrap())));
            }
            RowKind::Proc(_) if r.depth == 1 => assert_eq!(r.parent, group, "a member stays under its group"),
            RowKind::Proc(_) | RowKind::Hardware | RowKind::Ghost(_) => {}
        }
    }
    // Memory descending inside Apps.
    let a = section_row(&h, Section::Apps);
    let b = section_row(&h, Section::Background);
    let mem: Vec<u64> = rows(&h)[a + 1..b].iter().filter(|r| r.depth == 0).map(|r| r.vals.mem.unwrap()).collect();
    assert!(mem.windows(2).all(|w| w[0] >= w[1]), "{mem:?}");
}

#[test]
fn windows_rows_visible_rule() {
    // Collapsed, one escaped svchost row (7 % of the processor): the service names are needed.
    let h = open("t3a_part_of_windows_collapsed_hot");
    assert!(h.screen().control_inputs().windows_rows_visible);
    // Collapsed, no escaped svchost.
    let mut s = load("t3a_part_of_windows_collapsed_hot");
    s.procs.iter_mut().find(|p| p.pid == 800).unwrap().cpu = vec![0.3];
    let h = rig(&s);
    assert!(rows(&h)
        .iter()
        .skip(section_row(&h, Section::Windows) + 1)
        .all(|r| !matches!(r.kind, RowKind::Proc(k) if k.pid == 800)));
    assert!(
        !h.screen().control_inputs().windows_rows_visible,
        "no Services sampler while nothing shows a service host"
    );
    // Open.
    let h = open("t3a_grouped");
    assert!(h.screen().control_inputs().windows_rows_visible);
}

#[test]
fn hung_group_says_so() {
    let h = open("t3a_hung");
    let r = group_row(&h, 2000);
    let c = cell(&h, r, COL_NAME);
    let (text, fg) = c.note.clone().expect("a note");
    assert_eq!(&*text, "Not responding");
    assert_eq!(fg, oled_ui::model::FgRole::Attention);
    assert!(cell(&h, group_row(&h, 3000), COL_NAME).note.is_none());
}

// ---- the view commands

#[test]
fn view_commands_switch_and_translate_the_selection() {
    let mut h = open("t3a_grouped");
    let state = h.screen().deps().state.clone();
    // Select the orbit group, go to List: its oldest process.
    let r = group_row(&h, 2000);
    let p = centre(&h, r);
    h.click(p);
    h.key("Ctrl+L");
    assert_eq!(h.screen().view().mode(), ViewMode::List);
    assert_eq!(state.view(), Some(ViewMode::List));
    let sel = h.screen().table().selection().clone();
    let root = proc_key(oled_sysmon::ProcKey { pid: 2000, created: 133_500_000_000_000_000 + 2000 * 1000 });
    assert_eq!(sel, Selection::Single(root));
    // A process back to its group.
    h.key("Ctrl+G");
    assert_eq!(h.screen().view().mode(), ViewMode::Grouped);
    let r = group_row(&h, 2000);
    assert_eq!(*h.screen().table().selection(), Selection::Single(rows(&h)[r].key));
    // Tree keeps the process.
    h.key("Ctrl+T");
    assert_eq!(h.screen().view().mode(), ViewMode::Tree);
    assert!(matches!(h.screen().table().selection(), Selection::Single(k) if *k == root));
}

// ---- Tree and List

#[test]
fn tree_depths_and_orphans() {
    let h = open("t3a_tree");
    let depth = |pid: u32| rows(&h)[proc_row(&h, pid)].depth;
    assert_eq!(depth(2110), depth(2100) + 1);
    assert_eq!(depth(2100), depth(2000) + 1);
    assert_eq!(depth(2000), depth(1000) + 1);
    assert_eq!(depth(8000), 0, "a process whose parent is gone is a root");
    assert_eq!(depth(0), 0);
    let r = &rows(&h)[proc_row(&h, 2000)];
    assert!(r.expandable && r.expanded);
    assert!(!rows(&h)[proc_row(&h, 2110)].expandable);
    assert_eq!(
        rows(&h)[proc_row(&h, 2110)].parent,
        Some(proc_key(oled_sysmon::ProcKey { pid: 2100, created: 133_500_000_000_000_000 + 2100 * 1000 }))
    );
    // The image name leads, the friendly name follows in tertiary.
    let c = cell(&h, proc_row(&h, 7000), COL_NAME);
    assert_eq!(c.text, "syncer.exe \u{b7} Syncer");
    assert_eq!(&c.text[c.spans[0].start as usize..c.spans[0].end as usize], " \u{b7} Syncer");
    h.assert_named();
}

#[test]
fn tree_sort_reorders_siblings_only() {
    let mut h = open("t3a_tree");
    let pids = |h: &Harness<TasksScreen>| -> Vec<u32> {
        rows(h)
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Proc(k) => Some(k.pid),
                _ => None,
            })
            .collect()
    };
    // CPU descending: the orbit children by CPU, each with its own subtree right after it.
    let p = pids(&h);
    let i = p.iter().position(|x| *x == 2000).unwrap();
    assert_eq!(&p[i..i + 5], [2000, 2200, 2100, 2110, 2300]);
    // Ascending: the siblings flip, the subtree of 2100 stays under 2100.
    h.key("Ctrl+Shift+2");
    let p = pids(&h);
    let a = p.iter().position(|x| *x == 2300).unwrap();
    let b = p.iter().position(|x| *x == 2100).unwrap();
    let c = p.iter().position(|x| *x == 2200).unwrap();
    assert!(a < b && b < c, "{p:?}");
    assert_eq!(p[b + 1], 2110, "a child stays right under its parent");
    h.assert_named();
}

#[test]
fn collapsed_tree_nodes_are_remembered() {
    let mut h = open("t3a_tree");
    let r = proc_row(&h, 2000);
    let p = centre(&h, r);
    h.click(p);
    h.key("Left");
    assert!(rows(&h).iter().all(|r| !matches!(r.kind, RowKind::Proc(k) if k.pid == 2110)), "hidden under 2000");
    assert!(!rows(&h)[proc_row(&h, 2000)].expanded);
    h.key("Right");
    assert!(rows(&h).iter().any(|r| matches!(r.kind, RowKind::Proc(k) if k.pid == 2110)));
}

#[test]
fn list_shows_pid_column_only_in_list() {
    let mut h = open("t3a_list");
    let pid_visible =
        |h: &Harness<TasksScreen>| h.screen().table().columns().iter().any(|c| c.id == COL_PID && c.visible);
    assert!(pid_visible(&h));
    let r = proc_row(&h, 2000);
    let c = cell(&h, r, COL_PID);
    assert_eq!(c.text, "2000");
    assert_eq!(c.align, oled_ui::model::Align::End);
    // The image name leads, the friendly name follows.
    assert_eq!(label(&h, r), "orbit.exe \u{b7} Orbit");
    h.key("Ctrl+G");
    assert!(!pid_visible(&h), "Grouped has no PID column");
    h.key("Ctrl+T");
    assert!(!pid_visible(&h), "nor has Tree");
    h.key("Ctrl+L");
    assert!(pid_visible(&h));
    h.assert_named();
}
