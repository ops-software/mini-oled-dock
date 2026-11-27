//! TA-HDR's goldens: the header's three traces (a CPU ramp, a memory plateau and an I/O spike) on the kit's matrix, and the
//! pure checks over the traces on screen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_sysmon::cpu::CpuDelta;
use oled_ui::harness::{golden, Harness};
use oled_ui::theme::{Corners, HcColors, Preset, SysOverlay, ThemeSpec, Tokens};
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectFrame, TasksScreen};

const SCENES: [&str; 1] = ["hdr_traces"];

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

/// The scene's frames with the machine's curves of the scene's story: the CPU share ramps from 4 to 92 %, the memory
/// load climbs to a plateau at 61 % (the scene file has one value for the whole run).
fn story(scene: &Scene) -> Vec<tasks::engine::frame::Frame> {
    let mut frames = scene.frames();
    let n = frames.len();
    for (i, f) in frames.iter_mut().enumerate() {
        let cpu = 4.0 + 88.0 * i as f32 / (n - 1) as f32;
        f.sys.cpu = Some(CpuDelta { d_idle: (1000.0 - cpu * 10.0) as u64, d_total: 1000 });
        if let Some(m) = f.sys.mem.as_mut() {
            m.load_pct = (35 + i as u8 * 2).min(61);
        }
    }
    frames
}

/// The scene opened on its first frame, then the others one by one, a second apart.
fn open_with(scene: &Scene, lang: PreviewLang, dpi: u32, tokens: Tokens) -> Harness<TasksScreen> {
    let mut first = scene.clone();
    first.ticks = 0;
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let mut h = scene::open_with(&first, lang, dpi, tokens, launcher, exec).unwrap_or_else(|e| panic!("{e}"));
    for f in story(scene).into_iter().skip(1) {
        h.advance(1000);
        h.send(Box::new(InjectFrame(f)));
    }
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
fn golden_hdr_every_scene_matches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for name in SCENES {
        let scene = load(name);
        for c in matrix() {
            let mut h = open_with(&scene, c.lang, c.dpi, c.tokens());
            h.assert_named();
            golden::check(&root, &diff, &format!("{name}/{}", c.name()), &h.render());
        }
    }
}

#[test]
fn traces_follow_the_frames() {
    let h = open_with(&load("hdr_traces"), PreviewLang::En, 96, Tokens::oled());
    let hist = h.screen().header().history();
    assert_eq!(hist.len(), 41, "one sample per good frame");
    let cpu = hist.cpu();
    assert!(cpu.windows(2).all(|w| w[0] <= w[1]) && cpu[0] < 10.0 && cpu[40] > 85.0, "a CPU ramp");
    let mem = hist.mem();
    assert_eq!(mem[40], 61.0);
    assert!(mem[30..].iter().all(|v| *v == 61.0), "a plateau");
    // The spike (50 MB/s at tick 29, smoothed by the rate) and the scale that holds it.
    let io = hist.io();
    assert!(io.iter().cloned().fold(0.0, f32::max) > 1.2e7, "{io:?}");
    assert_eq!(hist.io_ceiling(), 2.0e7);
    assert!(h.is_idle(), "no timer is armed between frames");
}

#[test]
fn an_error_frame_and_a_repeat_add_no_sample() {
    let scene = load("hdr_traces");
    let mut h = open_with(&scene, PreviewLang::En, 96, Tokens::oled());
    let before = h.screen().header().history().len();
    let mut err = story(&scene).remove(1);
    err.error = Some(oled_sysmon::SysmonError::Status(-1073741820));
    err.seq = 1000;
    h.send(Box::new(InjectFrame(err)));
    assert_eq!(h.screen().header().history().len(), before, "a failed snapshot is not a sample");
    // The same frame again (same sequence number) is not a second sample either.
    h.send(Box::new(InjectFrame(story(&scene).remove(40))));
    let n = h.screen().header().history().len();
    h.send(Box::new(InjectFrame(story(&scene).remove(40))));
    assert_eq!(h.screen().header().history().len(), n);
}
