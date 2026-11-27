//! T1's scenes: the goldens (the CI runner's base set, or the local one for looking) and the pure tests over scene frames
//! (the merge, the rates, the sort, the header, the More menu and the error band).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_sysmon::heat::HeatStep;
use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::model::{CellOut, FgRole, ListModel, TableModel};
use oled_ui::theme::{Corners, HcColors, Preset, SysOverlay, ThemeSpec, Tokens};
use oled_ui::MenuItem;
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::{LaunchReq, RecordingLauncher};
use tasks::engine::cadence::Refresh;
use tasks::model::view::ColumnId;
use tasks::ui::rows::{RowKind, COL_CPU, COL_IO, COL_MEMORY, COL_NAME};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectFrame, TasksScreen};

/// Every scene of the group.
const SCENES: [&str; 8] = [
    "t1_list_calm",
    "t1_list_busy",
    "t1_first_frame",
    "t1_paused",
    "t1_read_error",
    "t1_more_menu",
    "t1_focus",
    "t1_inactive",
];

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

struct Rig {
    h: Harness<TasksScreen>,
    launcher: Arc<RecordingLauncher>,
    #[allow(dead_code)]
    exec: Arc<RecordingExecutor>,
}

fn rig(scene: &Scene, lang: PreviewLang, dpi: u32) -> Rig {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(scene, lang, dpi, launcher.clone(), exec.clone()).unwrap_or_else(|e| panic!("{e}"));
    Rig { h, launcher, exec }
}

fn open(name: &str) -> Rig {
    rig(&load(name), PreviewLang::En, 96)
}

fn cell(h: &Harness<TasksScreen>, row: usize, col: oled_ui::model::ColId) -> CellOut {
    let mut out = CellOut::default();
    h.screen().table().model().cell(row, col, &mut out);
    out
}

fn pt(x: i32, y: i32) -> PointL {
    PointL { x, y }
}

// ---- the goldens

/// One image of a scene: language, DPI and theme, as the kit's matrix.
#[derive(Clone, Copy)]
struct Cell {
    lang: PreviewLang,
    dpi: u32,
    theme: Theme,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Oled,
    Contrast,
    HighContrast,
}

/// The system's high-contrast black palette, the Harness's stand-in for a machine in that mode.
const HC_BLACK: HcColors = HcColors {
    window: (0, 0, 0),
    window_text: (255, 255, 255),
    highlight: (26, 235, 255),
    highlight_text: (0, 0, 0),
    gray_text: (63, 242, 63),
};

impl Cell {
    fn name(&self) -> String {
        let lang = match self.lang {
            PreviewLang::En => "en",
            PreviewLang::It => "it",
            PreviewLang::Pseudo => "pseudo",
        };
        let theme = match self.theme {
            Theme::Oled => "",
            Theme::Contrast => "-contrast",
            Theme::HighContrast => "-hc",
        };
        format!("{lang}-{}{theme}", self.dpi)
    }

    fn tokens(&self) -> Tokens {
        let mut spec = ThemeSpec::oled();
        match self.theme {
            Theme::Oled => Tokens::resolve(&spec, &SysOverlay::none()),
            Theme::Contrast => {
                spec.look.preset = Preset::Contrast;
                spec.look.knobs.corners = Corners::Square;
                Tokens::resolve(&spec, &SysOverlay::none())
            }
            Theme::HighContrast => {
                Tokens::resolve(&spec, &SysOverlay { high_contrast: Some(HC_BLACK), ..SysOverlay::none() })
            }
        }
    }
}

/// The kit's matrix: OLED in English and Italian at four DPIs, the pseudo-locale, Contrast and high contrast at 144, and
/// high contrast at 96: 12 cells.
fn matrix() -> Vec<Cell> {
    let mut v = Vec::new();
    for lang in [PreviewLang::En, PreviewLang::It] {
        for dpi in [96, 120, 144, 192] {
            v.push(Cell { lang, dpi, theme: Theme::Oled });
        }
    }
    v.push(Cell { lang: PreviewLang::Pseudo, dpi: 144, theme: Theme::Oled });
    v.push(Cell { lang: PreviewLang::En, dpi: 144, theme: Theme::Contrast });
    v.push(Cell { lang: PreviewLang::En, dpi: 144, theme: Theme::HighContrast });
    v.push(Cell { lang: PreviewLang::En, dpi: 96, theme: Theme::HighContrast });
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
            let mut h = scene::open_with(&scene, c.lang, c.dpi, c.tokens(), launcher, exec)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            h.assert_named();
            golden::check(&root, &diff, &format!("{name}/{}", c.name()), &h.render());
            if c.lang == PreviewLang::En && c.dpi == 96 && c.theme == Theme::Oled {
                // The a11y dump of the scene, as text: what a screen reader is told.
                golden::check_text(&root, &diff, &format!("{name}/a11y.txt"), &h.a11y_dump());
            }
            if !h.menus().is_empty() {
                if let Some(img) = h.menu_image(0) {
                    golden::check(&root, &diff, &format!("{name}/{}-menu", c.name()), &img);
                }
            }
        }
    }
}

// ---- the merge and the rows

#[test]
fn list_has_every_process() {
    let r = open("t1_list_calm");
    let m = r.h.screen().table().model();
    assert_eq!(m.len(), 41, "one row per process");
    // The idle process is the unused processor, not a workload: it is listed last, whatever its figure.
    let cpu: Vec<Option<f32>> = m.rows.iter().filter(|e| e.vals.pid != Some(0)).map(|e| e.vals.cpu).collect();
    assert!(cpu.iter().all(Option::is_some), "primed after two ticks");
    assert!(cpu.windows(2).all(|w| w[0] >= w[1]), "sorted by CPU, highest first");
    assert!(r.h.is_idle(), "no timer is armed between ticks");
    r.h.assert_named();
}

#[test]
fn the_idle_process_is_not_a_workload() {
    let r = open("t1_list_calm");
    let m = r.h.screen().table().model();
    let at = m.rows.iter().position(|e| matches!(e.kind, RowKind::Proc(k) if k.pid == 0)).expect("the idle row");
    assert_ne!(at, 0, "the unused processor does not top the CPU list");
    assert!(m.rows[at].vals.heat.iter().all(|h| *h == HeatStep::S0), "an idle machine shows no heat on its idle row");
    assert!(m.rows[..at].iter().all(|e| e.vals.cpu.is_some()));
    assert_eq!(at, m.len() - 1, "it sorts after every process");
    // The same when the list is read by I/O and by memory, ascending as well.
    let mut r = open("t1_list_calm");
    // I/O, Memory, CPU, CPU again (ascending): the idle row stays last.
    for chord in ["Ctrl+Shift+4", "Ctrl+Shift+3", "Ctrl+Shift+2", "Ctrl+Shift+2"] {
        r.h.key(chord);
        let m = r.h.screen().table().model();
        assert!(matches!(m.rows[m.len() - 1].kind, RowKind::Proc(k) if k.pid == 0), "{chord}");
    }
    r.h.assert_named();
}

#[test]
fn merge_primes_rates() {
    // One frame: no rate yet, so CPU and I/O draw the dash with the tip; Memory is there.
    let first = open("t1_first_frame");
    let c = cell(&first.h, 0, COL_CPU);
    assert_eq!(c.text, "\u{2014}");
    assert_eq!(c.fg, FgRole::Tertiary);
    assert!(c.tip.is_some(), "the dash explains itself");
    assert_eq!(cell(&first.h, 0, COL_IO).text, "\u{2014}");
    assert!(cell(&first.h, 0, COL_MEMORY).text.contains(' '), "a size and its unit");
    // A second frame: numbers.
    let mut s = load("t1_first_frame");
    s.ticks = 1;
    let second = rig(&s, PreviewLang::En, 96);
    let m = second.h.screen().table().model();
    assert!(m.rows.iter().all(|e| e.vals.cpu.is_some() && e.vals.io.is_some()));
    assert_ne!(cell(&second.h, 0, COL_CPU).text, "\u{2014}");
    assert!(first.h.is_idle() && second.h.is_idle(), "no timer is armed after a frame");
    first.h.assert_named();
    second.h.assert_named();
}

#[test]
fn read_error_keeps_rows() {
    let calm = open("t1_list_calm");
    let mut r = open("t1_read_error");
    assert_eq!(r.h.screen().view().read_error(), Some(0xC000_0004));
    let (a, b) = (calm.h.screen().table().model(), r.h.screen().table().model());
    assert_eq!(a.len(), b.len(), "the last rows stay");
    assert_eq!(a.rows[0].key, b.rows[0].key);
    // The text dims to the secondary colour while the band shows; a figure that was tertiary stays tertiary.
    assert_eq!(cell(&r.h, 0, COL_NAME).fg, FgRole::Secondary);
    let (mut quiet, mut lit) = (0, 0);
    for row in 0..a.len() {
        for col in [COL_CPU, COL_MEMORY, COL_IO] {
            let (was, now) = (cell(&calm.h, row, col).fg, cell(&r.h, row, col).fg);
            if was == FgRole::Tertiary {
                quiet += 1;
                assert_eq!(now, FgRole::Tertiary, "row {row}: dimming never lifts a figure");
            } else {
                lit += 1;
                assert_eq!(now, FgRole::Secondary, "row {row}: a brighter figure caps at secondary");
            }
        }
    }
    assert!(quiet > 0, "the scene has quiet figures ({quiet} quiet, {lit} lit)");
    // A good frame takes the band away.
    let good = load("t1_read_error").frames().remove(0);
    r.h.send(Box::new(InjectFrame(good)));
    assert_eq!(r.h.screen().view().read_error(), None);
    assert_eq!(cell(&r.h, 0, COL_NAME).fg, FgRole::Primary);
    calm.h.assert_named();
    r.h.assert_named();
}

#[test]
fn error_band_shows_and_hides() {
    let mut r = open("t1_read_error");
    assert!(r.h.a11y_dump().contains("Open Windows Task Manager"), "the band's button is on screen");
    let good = load("t1_read_error").frames().remove(0);
    r.h.send(Box::new(InjectFrame(good)));
    assert!(!r.h.a11y_dump().contains("Open Windows Task Manager"), "gone with the error");
    r.h.assert_named();
}

#[test]
fn open_windows_tm_records_one_request() {
    let mut r = open("t1_read_error");
    assert!(r.launcher.reqs.lock().unwrap().is_empty());
    // The error band's button: 16 px from the right edge, centred in the 40 px band under the 44 px toolbar.
    r.h.click(pt(600, 64));
    assert_eq!(*r.launcher.reqs.lock().unwrap(), vec![LaunchReq::TaskMgr]);
    // The menu's item goes through the same handler.
    tasks::ui::more_menu::open_taskmgr(r.h.screen());
    assert_eq!(*r.launcher.reqs.lock().unwrap(), vec![LaunchReq::TaskMgr, LaunchReq::TaskMgr]);
    r.h.assert_named();
}

#[test]
fn auto_sort_rules() {
    use oled_trust::frame::tasks::SortBy;
    use tasks::app::options::OpenSort;
    use tasks::ui::view::open_sort;
    assert_eq!(open_sort(90, OpenSort::Auto, SortBy::Auto), ColumnId::Memory);
    assert_eq!(open_sort(89, OpenSort::Auto, SortBy::Auto), ColumnId::Cpu);
    assert_eq!(open_sort(95, OpenSort::Cpu, SortBy::Auto), ColumnId::Cpu, "the pin wins over the rule");
    assert_eq!(open_sort(10, OpenSort::Cpu, SortBy::Memory), ColumnId::Memory, "Show.sort wins over the pin");
    // A scene at 90 % opens sorted by Memory.
    let mut s = load("t1_list_calm");
    s.machine.mem_load = 90;
    let r = rig(&s, PreviewLang::En, 96);
    assert_eq!(r.h.screen().view().sort().column, ColumnId::Memory);
    assert!(r.h.screen().view().sort().auto);
    let calm = open("t1_list_calm");
    assert_eq!(calm.h.screen().view().sort().column, ColumnId::Cpu);
    r.h.assert_named();
    calm.h.assert_named();
}

#[test]
fn only_visible_rows_are_formatted() {
    let mut s = load("t1_list_calm");
    // 300 processes of the same kind.
    let proto = s.procs[5].clone();
    s.procs = (0..300u32)
        .map(|i| {
            let mut p = proto.clone();
            p.pid = 1000 + i * 4;
            p.created += u64::from(i);
            p
        })
        .collect();
    let mut r = rig(&s, PreviewLang::En, 96);
    let before = r.h.screen().table().model().cell_calls();
    let _ = r.h.render();
    let calls = r.h.screen().table().model().cell_calls() - before;
    let visible = r.h.screen().table().visible_rows().len() as u64;
    assert!(visible > 0 && visible < 30, "{visible} rows fit");
    assert!(calls <= (visible + 3) * 5, "{calls} cells for {visible} visible rows");
    assert_eq!(r.h.screen().table().model().len(), 300);
    // The signatures (each formats the row's three metric cells) are computed for the rows on screen only: every
    // publish of the three frames signs at most the visible rows, whatever the process count.
    let signed = r.h.screen().table().model().signature_calls();
    assert!(signed <= visible * 3, "{signed} row signatures over three frames for {visible} visible rows");
    let more = InjectFrame(s_frames_last(&load("t1_list_calm")));
    r.h.send(Box::new(more));
    let delta = r.h.screen().table().model().signature_calls() - signed;
    assert!(delta <= visible, "{delta} row signatures for one frame with {visible} rows on screen");
    r.h.assert_named();
}

fn s_frames_last(s: &Scene) -> tasks::engine::frame::Frame {
    s.frames().pop().expect("a frame")
}

#[test]
fn a_failed_snapshot_does_not_inflate_the_next_rates() {
    // A steady load: the same rate every tick, so the smoothing has nothing to add.
    let steady = || {
        let mut s = load("t1_list_calm");
        s.ticks = 2;
        for p in &mut s.procs {
            p.cpu = vec![p.cpu.last().copied().unwrap_or(1.0)];
            p.io = vec![p.io.last().copied().unwrap_or(0.0)];
        }
        s
    };
    let cpu_io = |r: &Rig| -> Vec<(Option<f32>, Option<f32>)> {
        r.h.screen().table().model().rows.iter().map(|e| (e.vals.cpu, e.vals.io)).collect()
    };
    let mut first = steady();
    first.ticks = 0;
    // Without an error: frames 0, 1, 2.
    let frame = |i: usize| steady().frames().remove(i);
    let mut plain = rig(&first, PreviewLang::En, 96);
    for i in [1, 2] {
        plain.h.send(Box::new(InjectFrame(frame(i))));
    }
    // With one: frame 1, a failed snapshot one tick later, then frame 2 two ticks after frame 1 with two ticks of work.
    let mut gap = rig(&first, PreviewLang::En, 96);
    gap.h.send(Box::new(InjectFrame(frame(1))));
    let one = frame(1);
    let tick_us = u64::from(one.dt_ms) * 1000;
    let failed = tasks::engine::frame::Frame {
        error: Some(oled_sysmon::SysmonError::Status(0xC000_0004u32 as i32)),
        t_us: one.t_us + tick_us,
        dt_ms: one.dt_ms,
        ..Default::default()
    };
    gap.h.send(Box::new(InjectFrame(failed)));
    let mut late = frame(2);
    late.t_us += tick_us;
    for (row, prev) in late.procs.rows.iter_mut().zip(&one.procs.rows) {
        row.cpu_100ns = prev.cpu_100ns + 2 * (row.cpu_100ns - prev.cpu_100ns);
        row.io_read = prev.io_read + 2 * (row.io_read - prev.io_read);
    }
    gap.h.send(Box::new(InjectFrame(late)));
    let (a, b) = (cpu_io(&plain), cpu_io(&gap));
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        let close = |p: Option<f32>, q: Option<f32>| match (p, q) {
            (Some(p), Some(q)) => (p - q).abs() <= 0.02 * p.abs().max(1.0),
            (None, None) => true,
            _ => false,
        };
        assert!(close(x.0, y.0) && close(x.1, y.1), "{x:?} against {y:?}: the error must not change the rates");
    }
    plain.h.assert_named();
    gap.h.assert_named();
}

#[test]
fn the_stored_always_on_top_reaches_the_window() {
    use oled_ui::harness::WindowRequest;
    let deps = tasks::app::Deps::inert(
        oled_ui::UiLang::EN,
        Arc::new(RecordingLauncher::default()),
        Arc::new(RecordingExecutor::default()),
    );
    let mut options = (*deps.options.get()).clone();
    options.always_on_top = true;
    deps.options.set(options);
    let mut h = Harness::new(TasksScreen::new(deps), oled_ui::geom::SizeL { w: 640, h: 480 }, 96, Tokens::oled());
    assert!(!h.window_requests().contains(&WindowRequest::Topmost(true)), "nothing before the first frame");
    for f in load("t1_list_calm").frames() {
        h.send(Box::new(InjectFrame(f)));
    }
    let on = h.window_requests().iter().filter(|r| **r == WindowRequest::Topmost(true)).count();
    assert_eq!(on, 1, "asked once, at the first frame");
    h.assert_named();
}

// ---- the header

#[test]
fn header_aligns_with_columns() {
    let mut r = open("t1_list_calm");
    let img = r.h.render();
    // At 96 DPI the List columns are 352 | 64 | 80 | 88 | 56 wide (PID is List mode's): the labels end 12 px before
    // their column's right edge.
    for edge in [416, 496, 584] {
        let want = edge - 12;
        assert!(
            img.text_rects.iter().any(|t| t.t >= 44 && t.b <= 44 + 24 && t.r == want),
            "no header label ends at x = {want}"
        );
        // The totals are right-aligned on the same line, lower in the header.
        assert!(
            img.text_rects.iter().any(|t| t.t >= 44 + 20 && t.b <= 104 && t.r == want),
            "no header total ends at x = {want}"
        );
    }
    r.h.assert_named();
}

#[test]
fn click_sorts_and_reverses() {
    let mut r = open("t1_list_calm");
    let sort = |r: &Rig| r.h.screen().view().sort();
    assert_eq!((sort(&r).column, sort(&r).descending), (ColumnId::Cpu, true));
    r.h.click(pt(384, 60));
    assert_eq!((sort(&r).column, sort(&r).descending, sort(&r).auto), (ColumnId::Cpu, false, false), "again reverses");
    let m = r.h.screen().table().model();
    let cpu: Vec<f32> = m.rows.iter().filter_map(|e| e.vals.cpu).collect();
    assert!(cpu.windows(2).all(|w| w[0] <= w[1]), "lowest first");
    r.h.click(pt(456, 60));
    assert_eq!((sort(&r).column, sort(&r).descending), (ColumnId::Memory, true), "numbers go down first");
    r.h.click(pt(100, 60));
    assert_eq!((sort(&r).column, sort(&r).descending), (ColumnId::Name, false), "the name goes up first");
    r.h.assert_named();
}

#[test]
fn sort_col_commands() {
    let mut r = open("t1_list_calm");
    let col = |r: &Rig| r.h.screen().view().sort().column;
    r.h.key("Ctrl+Shift+3");
    assert_eq!(col(&r), ColumnId::Memory);
    r.h.key("Ctrl+Shift+4");
    assert_eq!(col(&r), ColumnId::Io);
    r.h.key("Ctrl+Shift+1");
    assert_eq!(col(&r), ColumnId::Name);
    r.h.key("Ctrl+Shift+2");
    assert_eq!(col(&r), ColumnId::Cpu);
    r.h.assert_named();
}

// ---- the More menu

fn submenu(items: &[MenuItem], index: usize) -> &[MenuItem] {
    match &items[index] {
        MenuItem::Submenu { items, .. } => items,
        other => panic!("not a submenu: {other:?}"),
    }
}

fn checks(items: &[MenuItem]) -> Vec<Option<bool>> {
    items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Custom { checked, .. } => Some(*checked),
            _ => None,
        })
        .collect()
}

#[test]
fn more_menu_items() {
    let r = open("t1_more_menu");
    assert_eq!(r.h.menus().len(), 1, "F10 opens the menu");
    let items = r.h.menus()[0].items.clone();
    assert!(matches!(items.last(), Some(MenuItem::Header(_))), "the cost line is last");
    // Refresh: 0.5 s, 1 s, 2 s, 5 s, Paused; the default one second is checked.
    assert_eq!(checks(submenu(&items, 0)), [Some(false), Some(true), Some(false), Some(false), Some(false)]);
    // Density (T5.6): Standard is checked.
    assert_eq!(checks(submenu(&items, 1)), [Some(true), Some(false)]);
    // Open sorted by: Automatic is checked.
    assert_eq!(checks(submenu(&items, 3)), [Some(true), Some(false), Some(false)]);
    assert!(items.iter().any(|i| matches!(i, MenuItem::Command(id) if *id == tasks::ui::commands::ABOUT)));
    r.h.assert_named();
    // Pause (Ctrl+P): the sampler's control follows and the menu shows it checked.
    let mut r = open("t1_list_calm");
    r.h.key("Ctrl+P");
    let c = r.h.screen().deps().sampling.last_control().expect("a control was set");
    assert_eq!(c.refresh, Refresh::Paused);
    assert_eq!(r.h.screen().chrome().status(), Some(tasks::app::sampling::Status::Paused));
    r.h.key("F10");
    let items = r.h.menus().last().expect("a menu").items.clone();
    assert_eq!(checks(submenu(&items, 0)), [Some(false), Some(false), Some(false), Some(false), Some(true)]);
    // And again: back to the speed it had.
    let mut r = open("t1_list_calm");
    r.h.key("Ctrl+P");
    r.h.key("Ctrl+P");
    assert_eq!(r.h.screen().deps().sampling.last_control().map(|c| c.refresh), Some(Refresh::One));
    assert!(!r.h.screen().view().paused());
    r.h.assert_named();
}

#[test]
fn the_menu_picture_does_not_depend_on_the_machine() {
    let (a, b) = (open("t1_more_menu"), open("t1_more_menu"));
    let line = |r: &Rig| match r.h.menus()[0].items.last() {
        Some(MenuItem::Header(t)) => t.to_string(),
        other => panic!("no cost line: {other:?}"),
    };
    assert_eq!(line(&a), line(&b));
    assert_eq!(line(&a), "Tasks itself: 0.3 % \u{b7} 38.0 MB");
    a.h.assert_named();
    b.h.assert_named();
}

#[test]
fn paused_scene_says_so() {
    let r = open("t1_paused");
    assert_eq!(r.h.screen().chrome().status(), Some(tasks::app::sampling::Status::Paused));
    assert!(r.h.screen().view().paused());
    r.h.assert_named();
}
