//! Offscreen renders (P3-files-app 2.2, F1a.12): the registry of scenes every group adds to, the golden cells, and
//! files.exe --preview, which renders them through the kit Harness over fakes::Fakes (no window, no mutex, no host).

pub mod f1a;
pub mod f1b;
pub mod f2_menu;
pub mod f2_open;
pub mod f3_live;
pub mod f3_ops;
pub mod f3_rename;
pub mod f4_inst;
pub mod f4_path;
pub mod f4_side;
pub mod f4_tabs;
pub mod fixtures;

use std::path::Path;

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::theme::{HcColors, Preset, SysOverlay, ThemeSpec};
use oled_ui::{Lang, SizeL, Tokens};

use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::window::FilesScreen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cells {
    /// OLED x {en, it} x {96, 120, 144, 192}, pseudo 144, Contrast 144, high contrast 144 and 96.
    Full12,
    /// en 96, it 144, pseudo 144, high contrast 144.
    Four,
}

pub struct Scene {
    pub name: &'static str,
    pub size: SizeL,
    pub cells: Cells,
    /// System for every scene but chrome_custom.
    pub caption: Caption,
    pub build: fn(fakes: &Fakes, fmt: Fmt) -> FilesScreen,
    pub script: fn(h: &mut Harness<FilesScreen>, fakes: &Fakes),
}

/// Every file's scenes, concatenated; a file whose group has not landed has none.
pub fn scenes() -> Vec<Scene> {
    let mut all = Vec::new();
    for part in [
        f1a::scenes,
        f1b::scenes,
        f2_open::scenes,
        f2_menu::scenes,
        f3_ops::scenes,
        f3_rename::scenes,
        f3_live::scenes,
        f4_tabs::scenes,
        f4_path::scenes,
        f4_side::scenes,
        f4_inst::scenes,
    ] {
        all.extend(part());
    }
    all
}

/// One golden cell: its file tag, language, pseudo-locale, DPI and tokens.
pub struct Cell {
    pub tag: &'static str,
    pub lang: Lang,
    pub pseudo: bool,
    pub dpi: u32,
    pub tokens: fn() -> Tokens,
}

const fn cell(tag: &'static str, lang: Lang, pseudo: bool, dpi: u32, tokens: fn() -> Tokens) -> Cell {
    Cell { tag, lang, pseudo, dpi, tokens }
}

/// The OLED look.
fn oled() -> Tokens {
    Tokens::oled()
}

/// The Contrast look.
fn contrast() -> Tokens {
    let mut spec = ThemeSpec::oled();
    spec.look.preset = Preset::Contrast;
    Tokens::resolve(&spec, &SysOverlay::none())
}

/// Windows' high contrast black: black window, white text, cyan highlight, green grey text.
fn high_contrast() -> Tokens {
    let hc = HcColors {
        window: (0, 0, 0),
        window_text: (255, 255, 255),
        highlight: (26, 235, 255),
        highlight_text: (0, 0, 0),
        gray_text: (63, 242, 63),
    };
    let sys = SysOverlay { high_contrast: Some(hc), reduced_motion: false, text_scale: 1.0 };
    Tokens::resolve(&ThemeSpec::oled(), &sys)
}

pub fn cells(c: Cells) -> Vec<Cell> {
    match c {
        Cells::Full12 => vec![
            cell("en-96", Lang::En, false, 96, oled),
            cell("en-120", Lang::En, false, 120, oled),
            cell("en-144", Lang::En, false, 144, oled),
            cell("en-192", Lang::En, false, 192, oled),
            cell("it-96", Lang::It, false, 96, oled),
            cell("it-120", Lang::It, false, 120, oled),
            cell("it-144", Lang::It, false, 144, oled),
            cell("it-192", Lang::It, false, 192, oled),
            cell("pseudo-144", Lang::En, true, 144, oled),
            cell("contrast-144", Lang::En, false, 144, contrast),
            cell("hc-144", Lang::En, false, 144, high_contrast),
            cell("hc-96", Lang::En, false, 96, high_contrast),
        ],
        Cells::Four => vec![
            cell("en-96", Lang::En, false, 96, oled),
            cell("it-144", Lang::It, false, 144, oled),
            cell("pseudo-144", Lang::En, true, 144, oled),
            cell("hc-144", Lang::En, false, 144, high_contrast),
        ],
    }
}

/// The scene built over `fakes` with the cell's fixed formatter, in a Harness at the cell's DPI, tokens, language,
/// pseudo-locale and the scene's caption, then its script.
pub fn harness(scene: &Scene, cell: &Cell, fakes: &Fakes) -> Harness<FilesScreen> {
    let fmt = if cell.lang == Lang::It { Fmt::fixed_it() } else { Fmt::fixed_en() };
    let screen = (scene.build)(fakes, fmt);
    let mut h = Harness::new(screen, scene.size, cell.dpi, (cell.tokens)())
        .lang(cell.lang)
        .pseudo(cell.pseudo)
        .caption(scene.caption);
    (scene.script)(&mut h, fakes);
    h
}

/// files.exe --preview: every registered scene (or the one named) in every cell (or those of the language and DPI
/// asked for) to <dir>\<scene>-<cell>.png. The folder must exist: exit 2 otherwise (nothing here creates folders).
pub fn run(dir: &str, scene: Option<&str>, lang: Option<&str>, dpi: Option<u32>) -> i32 {
    let out = Path::new(dir);
    if !out.is_dir() {
        oled_sys::log!("files: preview: {dir} is not an existing folder");
        return 2;
    }
    let mut failed = 0;
    for s in scenes().iter().filter(|s| scene.is_none_or(|n| n == s.name)) {
        let want_lang = |c: &Cell| lang.is_none_or(|l| (l == "it") == (c.lang == Lang::It) && !c.pseudo);
        for c in cells(s.cells).iter().filter(|c| want_lang(c) && dpi.is_none_or(|d| d == c.dpi)) {
            let fakes = Fakes::new();
            let img = harness(s, c, &fakes).render();
            let file = out.join(format!("{}-{}.png", s.name, c.tag));
            if let Err(e) = img.write_png(&file) {
                oled_sys::log!("files: preview: {}: {e}", file.display());
                failed += 1;
            }
        }
    }
    i32::from(failed > 0)
}
