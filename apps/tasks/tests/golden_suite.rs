//! TA-SUITE's scenes: the Updated notice and About as goldens (the CI runner's base set, or the local one for looking).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_ui::harness::golden;
use oled_ui::theme::Tokens;
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::ui::scene::{self, Scene};

const SCENES: [&str; 2] = ["suite_notice", "suite_about"];

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn golden_suite_scenes_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden").join("tasks");
    for name in SCENES {
        let scene = load(name);
        let mut cells = Vec::new();
        for lang in [PreviewLang::En, PreviewLang::It] {
            for dpi in [96, 120, 144, 192] {
                cells.push((lang, dpi));
            }
        }
        cells.push((PreviewLang::Pseudo, 144));
        for (lang, dpi) in cells {
            let launcher = Arc::new(RecordingLauncher::default());
            let exec = Arc::new(RecordingExecutor::default());
            let mut h = scene::open_with(&scene, lang, dpi, Tokens::oled(), launcher, exec)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            h.assert_named();
            let l = match lang {
                PreviewLang::En => "en",
                PreviewLang::It => "it",
                PreviewLang::Pseudo => "pseudo",
            };
            golden::check(&root, &diff, &format!("{name}/{l}-{dpi}"), &h.render());
            if lang == PreviewLang::En && dpi == 96 {
                golden::check_text(&root, &diff, &format!("{name}/a11y.txt"), &h.a11y_dump());
            }
        }
    }
}
