//! T5b's key map, one Harness script per row of the plan's table, the multi-selection, the density and the accessibility
//! names; plus the goldens of its scenes (the CI runner's base set, or the local one for looking).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::model::ListModel;
use oled_ui::theme::Tokens;
use oled_ui::{Event, Key, Mods, RectP, Screen, WheelDelta};
use tasks::app::args::PreviewLang;
use tasks::app::exec::{ActVerb, RecordingExecutor};
use tasks::app::launch::RecordingLauncher;
use tasks::app::options::Density;
use tasks::model::view::{ColumnId, Section, ViewMode};
use tasks::ui::chrome::SEARCH;
use tasks::ui::rows::{RowEntry, RowKind};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{TasksScreen, CHROME, HEADER, TABLE};

const SCENES: [&str; 4] = ["t5b_keyboard", "t5b_compact", "t5b_multi", "t5b_keysheet"];

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

struct Rig {
    h: Harness<TasksScreen>,
    exec: Arc<RecordingExecutor>,
}

fn rig(name: &str) -> Rig {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(&load(name), PreviewLang::En, 96, launcher, exec.clone()).unwrap_or_else(|e| panic!("{e}"));
    Rig { h, exec }
}

fn open(name: &str) -> Harness<TasksScreen> {
    rig(name).h
}

fn rows(h: &Harness<TasksScreen>) -> &[RowEntry] {
    &h.screen().table().model().rows
}

fn center(r: RectP) -> PointL {
    PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
}

/// Clicks the row at `index` (a plain click: selected and focused).
fn click_row(h: &mut Harness<TasksScreen>, index: usize) {
    let r = h.screen().table().row_rect(index).expect("a row on screen");
    h.click(center(r));
}

fn index_of(h: &Harness<TasksScreen>, f: impl Fn(&RowEntry) -> bool) -> usize {
    rows(h).iter().position(f).expect("the row")
}

fn section_row(h: &Harness<TasksScreen>, s: Section) -> usize {
    index_of(h, |r| r.kind == RowKind::Section(s))
}

fn selected(h: &Harness<TasksScreen>) -> Vec<usize> {
    let t = h.screen().table();
    let mut v = Vec::new();
    t.selection().for_each(t.model(), &mut |row, _| v.push(row));
    v
}

fn closes(h: &Harness<TasksScreen>) -> bool {
    h.last_outputs().iter().any(|o| o == "CloseWindow")
}

fn batches(r: &Rig) -> Vec<tasks::app::exec::Batch> {
    r.exec.batches.lock().unwrap().clone()
}

fn owner(h: &Harness<TasksScreen>) -> Option<oled_ui::WidgetId> {
    h.focused().map(|n| n.owner())
}

// ---- the goldens

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

// ---- the key table, row by row

#[test]
fn page_chords_keep_the_one_page() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    let at = h.focused();
    let before: Vec<_> = rows(&h).iter().map(|r| r.key).collect();
    h.key("Ctrl+1").key("Ctrl+Tab").key("Ctrl+Shift+Tab");
    assert_eq!(h.focused(), at, "no control took the chord");
    assert_eq!(rows(&h).iter().map(|r| r.key).collect::<Vec<_>>(), before);
    assert!(!closes(&h));
}

#[test]
fn tab_and_shift_tab_move_between_controls() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    assert_eq!(owner(&h), Some(TABLE));
    h.key("Shift+Tab");
    assert_ne!(owner(&h), Some(TABLE), "Shift+Tab left the list");
    h.key("Tab");
    assert_eq!(owner(&h), Some(TABLE), "Tab came back to it");
}

#[test]
fn f6_cycles_the_regions() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    let mut seen = vec![owner(&h)];
    for _ in 0..3 {
        h.key("F6");
        seen.push(owner(&h));
    }
    assert!(seen.contains(&Some(CHROME)) || seen.contains(&Some(SEARCH)), "the toolbar is a region: {seen:?}");
    assert!(seen.contains(&Some(HEADER)), "the header is a region: {seen:?}");
    assert_eq!(seen[0], seen[3], "three regions, one turn: {seen:?}");
    h.key("Shift+F6");
    assert_ne!(owner(&h), seen[0], "Shift+F6 goes back");
}

#[test]
fn search_chords_move_to_the_field() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    h.key("Ctrl+F");
    assert_eq!(owner(&h), Some(SEARCH));
    h.key("Esc");
    click_row(&mut h, 1);
    h.type_text("/");
    assert_eq!(owner(&h), Some(SEARCH));
}

#[test]
fn f5_samples_now_also_while_paused() {
    let mut h = open("t5b_keyboard");
    let n = h.screen().deps().sampling.sample_now_calls();
    h.key("F5");
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), n + 1);
    h.key("Ctrl+P");
    assert!(h.screen().view().paused());
    h.key("F5");
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), n + 2, "paused or not");
    h.key("Ctrl+P");
    assert!(!h.screen().view().paused(), "Ctrl+P again resumes");
}

#[test]
fn pause_key_is_bound() {
    let h = open("t5b_keyboard");
    let chord = oled_ui::Chord::new(Mods::NONE, Key(0x13));
    let id = h.screen().commands().by_chord(chord).map(|c| c.id);
    assert_eq!(id, Some(tasks::ui::commands::PAUSE));
}

#[test]
fn f10_and_alt_m_open_the_more_menu() {
    for chord in ["F10", "Alt+M"] {
        let mut h = open("t5b_keyboard");
        click_row(&mut h, 1);
        h.key(chord);
        assert_eq!(h.menus().len(), 1, "{chord}");
        h.key("Esc");
        assert_eq!(h.menus().len(), 0, "{chord}: Esc closes the menu first");
        assert!(!closes(&h), "{chord}");
    }
}

#[test]
fn f1_opens_the_sheet_and_esc_closes_it() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    h.key("F1");
    let dump = h.a11y_dump();
    assert!(dump.contains("Keyboard shortcuts"), "{dump}");
    assert!(
        tasks::ui::keysheet::lines(oled_ui::UiLang::EN)[0].contains("Ctrl+Shift+Tab"),
        "the sheet lists the page chords"
    );
    h.key("Esc");
    assert!(!h.a11y_dump().contains("Keyboard shortcuts"));
    assert!(!closes(&h), "Esc closed the sheet, not the window");
}

#[test]
fn list_keys_move_and_hold_the_order() {
    let mut h = open("t5b_keyboard");
    // The list gets the focus by keyboard, so no pointer hold is in play.
    for _ in 0..3 {
        if owner(&h) == Some(TABLE) {
            break;
        }
        h.key("F6");
    }
    assert_eq!(owner(&h), Some(TABLE));
    assert!(!h.screen().header().held(), "nothing holds the order yet");
    let before = h.screen().table().focused();
    h.key("Down");
    assert_ne!(h.screen().table().focused(), before, "Down moved the focus");
    assert!(h.screen().header().held(), "a list key holds the order");
}

#[test]
fn space_expands_and_collapses() {
    let mut h = open("t5b_keyboard");
    let w = section_row(&h, Section::Background);
    click_row(&mut h, w);
    let n = rows(&h).len();
    h.key("Space");
    assert!(rows(&h).len() < n, "Space closed the open section");
    h.key("Space");
    assert_eq!(rows(&h).len(), n, "and opened it");
}

#[test]
fn f5_lets_go_of_the_held_order() {
    let mut h = open("t5b_keyboard");
    let r = h.screen().table().row_rect(2).expect("a row");
    h.hover(center(r));
    assert!(h.screen().header().held(), "the pointer over the list holds the order");
    h.key("F5");
    assert!(!h.screen().header().held(), "F5 released it");
    assert!(!tasks::ui::order::is_held(&h.screen().parts().order, 0));
}

#[test]
fn esc_moves_the_anchor_too() {
    let mut h = open("t5b_keyboard");
    let apps = section_row(&h, Section::Apps);
    click_row(&mut h, apps + 1);
    h.key("Shift+Down").key("Shift+Down");
    assert_eq!(selected(&h).len(), 3);
    h.key("Esc");
    assert_eq!(selected(&h).len(), 1);
    h.key("Shift+Down");
    assert_eq!(selected(&h).len(), 2, "the range starts at the kept row, the cleared rows stay cleared");
}

#[test]
fn right_left_enter_expand_and_collapse() {
    let mut h = open("t5b_keyboard");
    let w = section_row(&h, Section::Background);
    click_row(&mut h, w);
    let n = rows(&h).len();
    h.key("Left");
    assert!(rows(&h).len() < n, "Left closed the open section");
    h.key("Right");
    assert_eq!(rows(&h).len(), n, "Right opened it");
    h.key("Enter");
    assert!(rows(&h).len() < n, "Enter closed it");
    h.key("Enter");
    assert_eq!(rows(&h).len(), n, "and opened it");
}

#[test]
fn left_goes_to_the_parent() {
    let mut h = open("t5b_keyboard");
    let g = index_of(&h, |r| matches!(r.kind, RowKind::Group(_)) && r.expandable);
    click_row(&mut h, g);
    h.key("Right");
    assert!(rows(&h)[g].expanded);
    h.key("Right");
    assert_eq!(h.screen().table().focused(), Some(rows(&h)[g + 1].key), "Right on an open group steps into it");
    h.key("Left");
    assert_eq!(h.screen().table().focused(), Some(rows(&h)[g].key), "Left goes to the parent");
}

#[test]
fn del_on_part_of_windows_refuses() {
    let mut r = rig("t5b_keyboard");
    let w = section_row(&r.h, Section::Windows);
    click_row(&mut r.h, w);
    r.h.key("Right");
    r.h.key("Down");
    r.h.key("Del");
    assert!(r.h.screen().notices_shown(), "the notice says why");
    assert!(batches(&r).is_empty(), "no Batch");
}

#[test]
fn del_ends_through_the_executor_and_shift_del_ends_now() {
    let mut r = rig("t5b_keyboard");
    let p = index_of(&r.h, |x| matches!(x.kind, RowKind::Group(_)));
    click_row(&mut r.h, p);
    r.h.key("Shift+Del");
    let b = batches(&r);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].verb, ActVerb::EndNow);
}

#[test]
fn context_menu_keys() {
    for chord in ["Shift+F10", "Menu"] {
        let mut h = open("t5b_keyboard");
        click_row(&mut h, 1);
        h.key(chord);
        assert_eq!(h.menus().len(), 1, "{chord} on a row");
    }
}

#[test]
fn view_chords() {
    let mut h = open("t5b_keyboard");
    for (chord, mode) in [("Ctrl+T", ViewMode::Tree), ("Ctrl+L", ViewMode::List), ("Ctrl+G", ViewMode::Grouped)] {
        h.key(chord);
        assert_eq!(h.screen().view().mode(), mode, "{chord}");
    }
}

#[test]
fn sort_chords_pick_the_nth_visible_column() {
    let mut h = open("t5b_keyboard");
    for (chord, col) in [
        ("Ctrl+Shift+2", ColumnId::Cpu),
        ("Ctrl+Shift+3", ColumnId::Memory),
        ("Ctrl+Shift+4", ColumnId::Io),
        ("Ctrl+Shift+1", ColumnId::Name),
    ] {
        h.key(chord);
        assert_eq!(h.screen().view().sort().column, col, "{chord}");
    }
}

#[test]
fn header_cells_are_one_roving_stop() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    h.key("F6");
    h.key("F6");
    let at = h.focused().expect("a focus");
    assert_eq!(at.owner(), HEADER, "two regions on: the header");
    h.key("Right");
    let next = h.focused().expect("a focus");
    assert_eq!(next.owner(), HEADER);
    assert_ne!(next, at, "Right moved to the next column");
    let before = h.screen().view().sort();
    h.key("Enter");
    assert_ne!(h.screen().view().sort(), before, "Enter sorts by it");
}

#[test]
fn header_menu_key_opens_the_view_menu() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    h.key("F6");
    h.key("F6");
    assert_eq!(owner(&h), Some(HEADER));
    h.key("Shift+F10");
    assert_eq!(h.menus().len(), 1, "the view-mode menu");
}

#[test]
fn ctrl_w_and_alt_f4_close() {
    for chord in ["Ctrl+W", "Alt+F4"] {
        let mut h = open("t5b_keyboard");
        h.key(chord);
        assert!(closes(&h), "{chord}");
    }
}

#[test]
fn unbound_chords_do_nothing() {
    let mut h = open("t5b_keyboard");
    click_row(&mut h, 1);
    let keys: Vec<_> = rows(&h).iter().map(|r| r.key).collect();
    let (mode, sort, opts, focus) =
        (h.screen().view().mode(), h.screen().view().sort(), (*h.screen().deps().options.get()).clone(), h.focused());
    for chord in ["Ctrl+N", "Ctrl+E", "Ctrl+O", "Ctrl+C", "Alt+1"] {
        h.key(chord);
        assert!(h.menus().is_empty() && !closes(&h) && !h.screen().notices_shown(), "{chord}");
    }
    assert_eq!(rows(&h).iter().map(|r| r.key).collect::<Vec<_>>(), keys);
    assert_eq!((h.screen().view().mode(), h.screen().view().sort(), h.focused()), (mode, sort, focus));
    assert_eq!(*h.screen().deps().options.get(), opts);
}

// ---- multi-select

#[test]
fn shift_and_ctrl_select_rows() {
    let mut h = open("t5b_keyboard");
    let g = index_of(&h, |r| matches!(r.kind, RowKind::Group(_)));
    click_row(&mut h, g);
    h.key("Shift+Down").key("Shift+Down");
    assert_eq!(selected(&h).len(), 3, "a range of three");
    h.key("Ctrl+Down").key("Ctrl+Space");
    assert_eq!(selected(&h).len(), 4, "Ctrl+Space added the row the focus moved to");
}

#[test]
fn selection_never_spans_sections() {
    let mut r = rig("t5b_keyboard");
    let apps = section_row(&r.h, Section::Apps);
    click_row(&mut r.h, apps + 1);
    // Shift+End runs through every section and ends in the last one: the selection is that section's.
    r.h.key("Shift+End");
    let sel = selected(&r.h);
    assert!(!sel.is_empty());
    let sections: std::collections::BTreeSet<_> = sel.iter().map(|&i| rows(&r.h)[i].section).collect();
    assert_eq!(sections.len(), 1, "one section: {sections:?}");
    let focused = r.h.screen().table().focused().expect("a focus");
    let at = r.h.screen().table().model().row_of(focused).expect("the focused row");
    assert_eq!(sections.into_iter().next().unwrap(), rows(&r.h)[at].section, "the section of the focused row");
    assert_eq!(rows(&r.h)[at].section, Some(Section::Windows));
    r.h.key("Del");
    assert!(batches(&r).is_empty(), "Part of Windows is never ended");
}

#[test]
fn a_view_switch_does_not_empty_the_selection() {
    let mut h = open("t5b_keyboard");
    let apps = section_row(&h, Section::Apps);
    click_row(&mut h, apps + 1);
    h.key("Ctrl+L");
    h.key("Shift+Down");
    assert_eq!(selected(&h).len(), 2, "the range starts from the row that kept the focus");
    h.key("Shift+Down");
    assert_eq!(selected(&h).len(), 3);
}

#[test]
fn a_view_switch_keeps_one_section() {
    let mut h = open("t5b_keyboard");
    h.key("Ctrl+L");
    click_row(&mut h, 1);
    h.key("Ctrl+A");
    assert_eq!(selected(&h).len(), rows(&h).len());
    h.key("Ctrl+G");
    let sel = selected(&h);
    let sections: std::collections::BTreeSet<_> = sel.iter().map(|&i| rows(&h)[i].section).collect();
    assert!(sections.len() <= 1, "one section after the switch: {sections:?}");
}

#[test]
fn ctrl_a_selects_the_focused_section() {
    let mut h = open("t5b_keyboard");
    let bg = section_row(&h, Section::Background);
    click_row(&mut h, bg + 1);
    h.key("Ctrl+A");
    let sel = selected(&h);
    assert!(sel.len() > 1);
    assert!(sel.iter().all(|&i| rows(&h)[i].section == Some(Section::Background)), "{sel:?}");
    let all_background = rows(&h).iter().filter(|r| r.section == Some(Section::Background)).count();
    assert_eq!(sel.len(), all_background, "all of it");
}

#[test]
fn ctrl_a_in_list_mode_selects_every_row() {
    let mut h = open("t5b_keyboard");
    h.key("Ctrl+L");
    click_row(&mut h, 1);
    h.key("Ctrl+A");
    assert_eq!(selected(&h).len(), rows(&h).len());
}

#[test]
fn del_on_three_apps_is_one_batch() {
    let mut r = rig("t5b_keyboard");
    let apps = section_row(&r.h, Section::Apps);
    click_row(&mut r.h, apps + 1);
    r.h.key("Shift+Down").key("Shift+Down");
    assert_eq!(selected(&r.h).len(), 3);
    r.h.key("Del");
    let b = batches(&r);
    assert_eq!(b.len(), 1, "one Batch for the verb");
    assert!(b[0].targets.len() >= 2, "{} targets", b[0].targets.len());
}

#[test]
fn esc_clears_multi_first() {
    let mut h = open("t5b_keyboard");
    let apps = section_row(&h, Section::Apps);
    click_row(&mut h, apps + 1);
    h.key("Shift+Down");
    assert_eq!(selected(&h).len(), 2);
    h.key("Esc");
    assert_eq!(selected(&h).len(), 1, "the first Esc went down to the focused row");
    assert!(!closes(&h), "and did not close the window");
    // The second Esc has nothing left to clear: the kit's esc_closes (WindowSpec, set by app::main) takes it.
    h.key("Esc");
    assert_eq!(selected(&h).len(), 1);
}

#[test]
fn the_multi_scene_shows_three_rows_selected() {
    let h = open("t5b_multi");
    assert_eq!(selected(&h).len(), 3);
}

// ---- density

#[test]
fn compact_rows_are_24() {
    let mut h = open("t5b_keyboard");
    let standard = h.screen().table().row_rect(1).expect("row").h();
    assert!(standard > 24, "standard rows are taller: {standard}");
    h.key("Ctrl+-");
    assert_eq!(h.screen().table().row_rect(1).expect("row").h(), 24, "24 logical px at 96 DPI");
    assert_eq!(h.screen().deps().options.get().density, Density::Compact);
    h.key("Ctrl+=");
    assert_eq!(h.screen().table().row_rect(1).expect("row").h(), standard);
    assert_eq!(h.screen().deps().options.get().density, Density::Standard);
}

#[test]
fn ctrl_equals_reaches_the_command_while_the_table_has_the_focus() {
    let mut h = open("t5b_keyboard");
    h.key("Ctrl+-");
    click_row(&mut h, 1);
    assert_eq!(owner(&h), Some(TABLE));
    h.key("Ctrl+=");
    assert_eq!(h.screen().deps().options.get().density, Density::Standard);
}

#[test]
fn density_is_saved_and_the_menu_checks_it() {
    let mut h = open("t5b_keyboard");
    h.key("Ctrl+-").key("F10");
    assert_eq!(h.menus().len(), 1);
    let o = h.screen().deps().options.get();
    assert_eq!(o.density, Density::Compact, "the option the file would keep");
    let items = tasks::ui::more_menu::items(oled_ui::UiLang::EN, &o, h.screen().commands(), String::new());
    let density = items
        .iter()
        .find_map(|i| match i {
            oled_ui::MenuItem::Submenu { label, items, .. } if label.as_ref() == "Density" => Some(items),
            _ => None,
        })
        .expect("the Density submenu");
    let checked: Vec<_> = density
        .iter()
        .filter_map(|i| match i {
            oled_ui::MenuItem::Custom { label, checked: Some(true), .. } => Some(label.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(checked, ["Compact"]);
}

#[test]
fn ctrl_wheel_changes_the_density() {
    use tasks::ui::commands::{DENSITY_COMPACT, DENSITY_STANDARD};
    let area = RectP::wh(0, 100, 700, 500);
    let at = oled_ui::PointP { x: 50, y: 200 };
    let wheel =
        |lines: f32, mods: Mods| Event::Wheel { pos: at, delta: WheelDelta::Lines(lines), horizontal: false, mods };
    assert_eq!(tasks::ui::keys::own_command(&wheel(-1.0, Mods::CTRL), false, area), Some(DENSITY_COMPACT));
    assert_eq!(tasks::ui::keys::own_command(&wheel(1.0, Mods::CTRL), false, area), Some(DENSITY_STANDARD));
    assert_eq!(tasks::ui::keys::own_command(&wheel(1.0, Mods::NONE), false, area), None, "a plain wheel scrolls");
    let outside = Event::Wheel {
        pos: oled_ui::PointP { x: 50, y: 20 },
        delta: WheelDelta::Lines(1.0),
        horizontal: false,
        mods: Mods::CTRL,
    };
    assert_eq!(tasks::ui::keys::own_command(&outside, false, area), None, "only over the list");
}

// ---- accessibility

#[test]
fn a11y_dump_names_every_node() {
    let h = open("t5b_keyboard");
    h.assert_named();
    let dump = h.a11y_dump();
    assert!(dump.contains("Row \"Apps, 4 items, expanded\""), "{dump}");
    assert!(dump.contains("Row \"Calculator, CPU "), "a row says its figures: {dump}");
    assert!(dump.contains(", memory ") && dump.contains(", I/O "), "{dump}");
    assert!(dump.contains("ColumnHeader \"CPU, total "), "{dump}");
    assert!(dump.contains("\"More options (F10)\""), "the More button is named as the plan says: {dump}");
}

#[test]
fn the_italian_row_says_memoria() {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(&load("t5b_keyboard"), PreviewLang::It, 96, launcher, exec).unwrap_or_else(|e| panic!("{e}"));
    let dump = h.a11y_dump();
    assert!(dump.contains(", memoria "), "{dump}");
    assert!(dump.contains("4 elementi, espansa"), "{dump}");
    assert!(dump.contains("ColumnHeader \"CPU, totale "), "{dump}");
    assert!(dump.contains("\"Altre opzioni (F10)\""), "{dump}");
}

#[test]
fn an_unprimed_row_says_so() {
    let mut s = load("t5b_keyboard");
    s.ticks = 0;
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(&s, PreviewLang::En, 96, launcher, exec).unwrap_or_else(|e| panic!("{e}"));
    assert!(h.a11y_dump().contains("CPU not measured yet"), "{}", h.a11y_dump());
}
