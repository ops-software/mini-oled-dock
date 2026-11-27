//! T5a's scenes: the toolbar, the search, their goldens (the CI runner's base set, or the local one for looking) and the
//! pure tests over scene frames.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::node::NodeId;
use oled_ui::theme::Tokens;
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::model::view::Section;
use tasks::ui::chrome::SEARCH;
use tasks::ui::rows::{RowEntry, RowKind};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{TasksScreen, CHROME, TABLE};

const SCENES: [&str; 5] =
    ["t5a_toolbar", "t5a_toolbar_focus", "t5a_toolbar_hover_more", "t5a_search_hits", "t5a_no_match"];
// t5a_esc_cascade carries no picture of its own: a notice and the toolbar, both golden elsewhere.

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

fn open(name: &str) -> Harness<TasksScreen> {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    scene::open(&load(name), PreviewLang::En, 96, launcher, exec).unwrap_or_else(|e| panic!("{e}"))
}

fn rows(h: &Harness<TasksScreen>) -> &[RowEntry] {
    &h.screen().table().model().rows
}

fn pids(h: &Harness<TasksScreen>) -> Vec<u32> {
    rows(h)
        .iter()
        .filter_map(|r| match r.kind {
            RowKind::Proc(k) => Some(k.pid),
            _ => None,
        })
        .collect()
}

fn focus_table(h: &mut Harness<TasksScreen>) {
    let r = h.screen().table().row_rect(1).expect("a row on screen");
    h.click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
    assert_eq!(h.focused().map(|n| n.owner()), Some(TABLE));
}

// ---- the goldens

/// OLED in English and Italian at four DPIs, and the pseudo-locale at 144.
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

#[test]
fn golden_every_scene_matches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for name in SCENES {
        let scene = load(name);
        for (lang, dpi) in matrix() {
            let launcher = Arc::new(RecordingLauncher::default());
            let exec = Arc::new(RecordingExecutor::default());
            let mut h = scene::open_with(&scene, lang, dpi, Tokens::oled(), launcher, exec)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            h.assert_named();
            let cell = match lang {
                PreviewLang::En => "en",
                PreviewLang::It => "it",
                PreviewLang::Pseudo => "pseudo",
            };
            golden::check(&root, &diff, &format!("{name}/{cell}-{dpi}"), &h.render());
            if lang == PreviewLang::En && dpi == 96 {
                golden::check_text(&root, &diff, &format!("{name}/a11y.txt"), &h.a11y_dump());
            }
        }
    }
}

// ---- the toolbar

#[test]
fn toolbar_one_tab_stop() {
    let mut h = open("t5a_toolbar");
    focus_table(&mut h);
    // One full turn of Tab: the toolbar's buttons and tab are one stop, the field another.
    let mut owners = Vec::new();
    for _ in 0..6 {
        h.key("Tab");
        owners.push(h.focused().map(|n| n.owner()));
    }
    let in_toolbar = owners.iter().filter(|o| **o == Some(CHROME)).count();
    assert!((1..=2).contains(&in_toolbar), "the tab and the button share one stop: {owners:?}");
    assert!(owners.contains(&Some(SEARCH)), "the field is a stop of its own: {owners:?}");
    // Roving: Left and Right move inside the toolbar, from the tab to the More button.
    h.key("Ctrl+F").key("Shift+Tab");
    assert_eq!(h.focused().map(|n| n.owner()), Some(CHROME), "Shift+Tab leaves the field for the toolbar");
    h.key("Right");
    let at = h.focused().expect("a focus");
    assert_eq!(at.owner(), CHROME);
    h.key("Left");
    assert_ne!(h.focused(), Some(at), "roving moved");
    h.assert_named();
}

#[test]
fn status_switches_to_count() {
    let mut h = open("t5a_toolbar");
    assert_eq!(h.screen().chrome().counted(), None);
    assert!(h.screen().chrome().status().is_some(), "the sampler's words stand while nothing is searched");
    h.key("Ctrl+F").type_text("orbit");
    assert_eq!(h.screen().chrome().counted(), Some((5, 23)));
    h.key("Esc");
    assert_eq!(h.screen().chrome().counted(), None);
    assert!(h.screen().chrome().status().is_some());
}

#[test]
fn more_button_is_where_the_menu_anchors() {
    let mut h = open("t5a_toolbar");
    let more = h.screen().chrome().more_rect();
    h.click(PointL { x: (more.l + more.r) / 2, y: (more.t + more.b) / 2 });
    assert_eq!(h.menus().len(), 1, "a click on the computed place opens the More menu");
}

// ---- the search

#[test]
fn printable_key_starts_search() {
    let mut h = open("t5a_toolbar");
    let all = rows(&h).len();
    focus_table(&mut h);
    h.type_text("o");
    assert_eq!(h.focused(), Some(NodeId::new(SEARCH, 0)), "the field took the focus");
    assert_eq!(h.screen().chrome().query(), "o", "and the character");
    assert_eq!(h.screen().parts().search.query(), "o");
    assert!(rows(&h).len() < all, "the list is filtered at once");
    h.type_text("rbit");
    assert_eq!(h.screen().chrome().query(), "orbit");
}

#[test]
fn slash_starts_search_without_inserting() {
    let mut h = open("t5a_toolbar");
    focus_table(&mut h);
    h.type_text("/");
    assert_eq!(h.focused(), Some(NodeId::new(SEARCH, 0)));
    assert_eq!(h.screen().chrome().query(), "");
    assert!(!tasks::ui::search::query_stands(&h.screen().parts().search));
}

#[test]
fn esc_clears_then_cascades() {
    let mut h = open("t5a_esc_cascade");
    assert!(h.screen().notices_shown(), "the scene shows a notice, which Esc takes away");
    let all = rows(&h).len();
    h.key("Ctrl+F").type_text("orbit");
    assert!(rows(&h).len() < all);
    h.key("Esc");
    assert_eq!(h.screen().chrome().query(), "", "the first Esc cleared the field");
    assert_eq!(rows(&h).len(), all, "and the list is whole again");
    assert!(h.screen().notices_shown(), "it went no further");
    h.key("Esc");
    assert!(!h.screen().notices_shown(), "the second Esc went on down the cascade");
    assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"));
}

#[test]
fn search_hits_open_the_group() {
    let mut h = open("t5a_toolbar");
    h.key("Ctrl+F").type_text("orbit");
    let mut found = pids(&h);
    found.sort_unstable();
    assert_eq!(found, [2000, 2100, 2110, 2200, 2300], "the five processes of the app, nothing else");
    let group = rows(&h).iter().find(|r| matches!(r.kind, RowKind::Group(_))).expect("the group row");
    assert!(group.expanded, "the group shows open");
    let sections: Vec<Section> = rows(&h)
        .iter()
        .filter_map(|r| match r.kind {
            RowKind::Section(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(sections, [Section::Apps], "only the section that still has a row");
}

#[test]
fn group_through_child() {
    let mut h = open("t5a_toolbar");
    // Only one process of the group answers: the group stays, open, with that one member.
    h.key("Ctrl+F").type_text("2110");
    let kinds: Vec<RowKind> = rows(&h).iter().map(|r| r.kind).collect();
    assert_eq!(kinds.len(), 3, "{kinds:?}");
    assert!(matches!(kinds[0], RowKind::Section(Section::Apps)));
    assert!(matches!(kinds[1], RowKind::Group(_)));
    assert!(matches!(kinds[2], RowKind::Proc(k) if k.pid == 2110));
    assert!(rows(&h)[1].expanded);
    assert_eq!(rows(&h)[2].depth, 1);
}

#[test]
fn services_and_publishers_are_searched() {
    let mut h = open("t5a_toolbar");
    // The display name of a hosted service finds its host, in Part of Windows, which opens for the search.
    h.key("Ctrl+F").type_text("endpoint builder");
    assert_eq!(pids(&h), [800]);
    assert!(rows(&h).iter().any(|r| r.kind == RowKind::Section(Section::Windows)));
}

#[test]
fn tree_keeps_ancestors() {
    let mut h = open("t5a_toolbar");
    h.key("Ctrl+T");
    h.key("Ctrl+F").type_text("2110");
    assert_eq!(pids(&h), [0, 1000, 2000, 2100, 2110], "the chain of parents stays above the match");
    let depth: Vec<u16> = rows(&h).iter().map(|r| r.depth).collect();
    assert_eq!(depth, [0, 1, 2, 3, 4]);
}

#[test]
fn no_match_leaves_an_empty_table() {
    let h = open("t5a_no_match");
    assert_eq!(rows(&h).len(), 0);
    assert!(tasks::ui::search::query_stands(&h.screen().parts().search));
    assert_eq!(h.screen().chrome().counted(), Some((0, 23)));
}

#[test]
fn query_applies_in_every_mode_and_ends_cleanly() {
    let mut h = open("t5a_toolbar");
    for chord in ["Ctrl+L", "Ctrl+T", "Ctrl+G"] {
        h.key(chord);
        let whole = rows(&h).len();
        h.key("Ctrl+F").type_text("syncer");
        assert!(pids(&h).iter().all(|p| *p == 7000 || *p == 7001 || *p == 1000 || *p == 0 || *p == 4), "{chord}");
        h.key("Esc");
        assert_eq!(rows(&h).len(), whole, "{chord}: Esc gives every row back");
    }
}
