//! TA-T2M's goldens: the held look (outlined chevron, pause glyph, a ghost, a fresh tick) and the release half way
//! (70 ms into the slide and the fades), on the kit's matrix.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_sysmon::ProcKey;
use oled_ui::geom::PointL;
use oled_ui::harness::{golden, Harness};
use oled_ui::theme::{Corners, HcColors, Preset, SysOverlay, ThemeSpec, Tokens};
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectFrame, TasksScreen};

/// The calm scene, short enough that a row at the bottom is on screen; a few figures to sort by.
fn story() -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t1_list_calm.json");
    let mut s = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
    s.ticks = 2;
    s.sort = Some(scene::SceneSort { col: scene::SceneCol::Cpu, desc: true });
    s.procs.truncate(13);
    for (i, p) in s.procs.iter_mut().enumerate() {
        p.cpu = match i {
            1 => vec![6.0],
            2 => vec![1.0],
            _ => vec![0.0],
        };
        p.io = vec![0.0];
    }
    s
}

/// The scene open with its figures measured, the pointer resting on a row, then a frame in which process 5 ended, a new
/// one started and process 8 rose to 40 %: the rows hold still and the new one has settled in.
fn held(lang: PreviewLang, dpi: u32, tokens: Tokens) -> Harness<TasksScreen> {
    let s = story();
    let mut first = s.clone();
    first.ticks = 0;
    let mut h = scene::open_with(
        &first,
        lang,
        dpi,
        tokens,
        Arc::new(RecordingLauncher::default()),
        Arc::new(RecordingExecutor::default()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    h.advance(100);
    let mut f = s.frames().remove(1);
    f.seq = 2;
    h.send(Box::new(InjectFrame(f)));
    h.advance(1000);
    let row = h.screen().table().row_rect(1).unwrap_or_else(|| panic!("no row"));
    // hover takes logical px; the rects are device px.
    let to_logical = |v: i32| v * 96 / dpi as i32;
    h.hover(PointL { x: to_logical((row.l + row.r) / 2), y: to_logical((row.t + row.b) / 2) });
    let mut next = s.clone();
    next.procs[8].cpu = vec![40.0];
    let mut extra = next.procs[6].clone();
    extra.pid = 9001;
    extra.created += 1;
    extra.image = "fresh.exe".into();
    extra.cpu = vec![12.0];
    next.procs.push(extra.clone());
    let ended = ProcKey { pid: next.procs[5].pid, created: next.procs[5].created };
    next.procs.remove(5);
    let mut f = next.frames().remove(1);
    f.seq = 3;
    f.gone = vec![ended];
    f.added = vec![ProcKey { pid: extra.pid, created: extra.created }];
    h.send(Box::new(InjectFrame(f)));
    h.advance(200);
    h
}

/// The held scene, the pointer still until the hold ends, then 70 ms.
fn release_mid(lang: PreviewLang, dpi: u32, tokens: Tokens) -> Harness<TasksScreen> {
    let mut h = held(lang, dpi, tokens);
    h.advance(3600 - 200 + 70);
    h
}

const HC_BLACK: HcColors = HcColors {
    window: (0, 0, 0),
    window_text: (255, 255, 255),
    highlight: (26, 235, 255),
    highlight_text: (0, 0, 0),
    gray_text: (63, 242, 63),
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Oled,
    Contrast,
    HighContrast,
}

#[derive(Clone, Copy)]
struct Cell {
    lang: PreviewLang,
    dpi: u32,
    theme: Theme,
}

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

/// The kit's matrix: 12 cells.
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
fn golden_t2_every_scene_matches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for c in matrix() {
        let h = &mut held(c.lang, c.dpi, c.tokens());
        h.assert_named();
        assert!(h.screen().header().held(), "{}", c.name());
        golden::check(&root, &diff, &format!("t2_held/{}", c.name()), &h.render());
        let h = &mut release_mid(c.lang, c.dpi, c.tokens());
        assert!(!h.screen().header().held());
        assert!(!h.is_idle(), "the release is still moving");
        golden::check(&root, &diff, &format!("t2_release_mid/{}", c.name()), &h.render());
    }
}
