//! tasks.exe --preview <dir> [--scene <name>] [--lang en|it|pseudo] [--dpi <n>] (preview builds): offscreen renders
//! of the embedded scenes to PNG files. Filled by TA-T1a (T1.6).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use super::scene::{self, Scene};
use crate::app::args::PreviewLang;
use crate::app::exec::RecordingExecutor;
use crate::app::launch::RecordingLauncher;

/// The scenes/*.json of the build, by name (build.rs).
pub mod embedded {
    include!(concat!(env!("OUT_DIR"), "/scenes.rs"));
}

/// The DPIs rendered when --dpi names none.
const DEFAULT_DPIS: [u32; 2] = [96, 144];

fn lang_name(l: PreviewLang) -> &'static str {
    match l {
        PreviewLang::En => "en",
        PreviewLang::It => "it",
        PreviewLang::Pseudo => "pseudo",
    }
}

/// Renders every scene (or the one asked for) at the DPI and language into <dir>\<scene>-<lang>-<dpi>.png (and
/// <scene>-<lang>-<dpi>-menu<n>.png for each popup the scene leaves open). No window, no mutex, no sampler. Exit 0, or 1
/// with the first error logged.
pub fn run(dir: &Path, scene: Option<&str>, lang: PreviewLang, dpi: Option<u32>) -> ExitCode {
    match render_all(dir, scene, lang, dpi) {
        Ok(n) if n > 0 => ExitCode::SUCCESS,
        Ok(_) => {
            oled_sys::log!("tasks: preview: no scene matched");
            ExitCode::from(1)
        }
        Err(e) => {
            oled_sys::log!("tasks: preview: {e}");
            ExitCode::from(1)
        }
    }
}

fn render_all(dir: &Path, only: Option<&str>, lang: PreviewLang, dpi: Option<u32>) -> Result<usize, String> {
    let dpis: Vec<u32> = dpi.map_or_else(|| DEFAULT_DPIS.to_vec(), |d| vec![d]);
    let mut written = 0;
    for (name, text) in embedded::SCENES {
        if only.is_some_and(|o| o != *name) {
            continue;
        }
        let parsed = Scene::parse(text).map_err(|e| format!("{name}: {e}"))?;
        for &d in &dpis {
            let launcher = Arc::new(RecordingLauncher::default());
            let exec = Arc::new(RecordingExecutor::default());
            let mut h = scene::open(&parsed, lang, d, launcher, exec).map_err(|e| format!("{name}: {e}"))?;
            let stem = format!("{name}-{}-{d}", lang_name(lang));
            h.render().write_png(&dir.join(format!("{stem}.png")))?;
            written += 1;
            for i in 0..h.menus().len() {
                if let Some(img) = h.menu_image(i) {
                    img.write_png(&dir.join(format!("{stem}-menu{i}.png")))?;
                }
            }
        }
    }
    Ok(written)
}
