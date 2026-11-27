//! --preview <dir> (K1a.6, KA1): every page x the golden matrix to <dir>/<page>/<lang>-<dpi>[-<theme>].png, and the matrix
//! itself, which the golden test shares.

use std::path::Path;

use crate::cli::{PreviewLang, PreviewOpts};
use crate::pages;
use oled_ui::geom::SizeL;
use oled_ui::harness::{Harness, Image};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::{Corners, HcColors, Preset, SysOverlay, ThemeSpec, Tokens};

/// A cell's language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellLang {
    En,
    It,
    Pseudo,
}

/// A cell's theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellTheme {
    Oled,
    Contrast,
    HighContrast,
}

/// One image of a page: language, DPI and theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub lang: CellLang,
    pub dpi: u32,
    pub theme: CellTheme,
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
    /// "<lang>-<dpi>" and "-contrast" or "-hc" for the other themes.
    pub fn name(&self) -> String {
        let lang = match self.lang {
            CellLang::En => "en",
            CellLang::It => "it",
            CellLang::Pseudo => "pseudo",
        };
        let theme = match self.theme {
            CellTheme::Oled => "",
            CellTheme::Contrast => "-contrast",
            CellTheme::HighContrast => "-hc",
        };
        format!("{lang}-{}{theme}", self.dpi)
    }

    pub fn ui_lang(&self) -> UiLang {
        UiLang { italian: self.lang == CellLang::It, pseudo: self.lang == CellLang::Pseudo }
    }

    /// The tokens of the cell's theme. Contrast is the gallery's own sample of the preset (square corners, full white):
    /// the preset's data belongs to the dock.
    pub fn tokens(&self) -> Tokens {
        let mut spec = ThemeSpec::oled();
        match self.theme {
            CellTheme::Oled => Tokens::resolve(&spec, &SysOverlay::none()),
            CellTheme::Contrast => {
                spec.look.preset = Preset::Contrast;
                spec.look.knobs.corners = Corners::Square;
                Tokens::resolve(&spec, &SysOverlay::none())
            }
            CellTheme::HighContrast => {
                Tokens::resolve(&spec, &SysOverlay { high_contrast: Some(HC_BLACK), ..SysOverlay::none() })
            }
        }
    }
}

/// The golden matrix of a page that carries text: OLED x {en, it} x {96, 120, 144, 192} DPI, plus the pseudo-locale,
/// Contrast and high contrast at 144 DPI, plus high contrast at 96: 12 cells.
pub fn matrix() -> Vec<Cell> {
    let mut v = Vec::new();
    for lang in [CellLang::En, CellLang::It] {
        for dpi in [96, 120, 144, 192] {
            v.push(Cell { lang, dpi, theme: CellTheme::Oled });
        }
    }
    v.push(Cell { lang: CellLang::Pseudo, dpi: 144, theme: CellTheme::Oled });
    v.push(Cell { lang: CellLang::En, dpi: 144, theme: CellTheme::Contrast });
    v.push(Cell { lang: CellLang::En, dpi: 144, theme: CellTheme::HighContrast });
    v.push(Cell { lang: CellLang::En, dpi: 96, theme: CellTheme::HighContrast });
    v
}

/// The cells of one page: the glyphs page carries no text, so OLED English at the four DPIs is all it needs.
pub fn cells_for(page: &str) -> Vec<Cell> {
    if page == "glyphs" {
        [96, 120, 144, 192].into_iter().map(|dpi| Cell { lang: CellLang::En, dpi, theme: CellTheme::Oled }).collect()
    } else {
        matrix()
    }
}

/// What rendering one cell gives: the image and the accessibility dump.
pub struct Rendered {
    pub image: Image,
    pub dump: String,
}

/// The page in the cell, offscreen; None when the page is not ready. Panics (assert_named) when an interactive node of
/// the page has no name.
pub fn render(page: &str, cell: &Cell) -> Option<Rendered> {
    let p = pages::by_name(page, cell.ui_lang())?;
    let size: SizeL = p.size;
    let lang = if cell.lang == CellLang::It { Lang::It } else { Lang::En };
    let mut h = Harness::new(p.screen, size, cell.dpi, cell.tokens()).lang(lang).pseudo(cell.lang == CellLang::Pseudo);
    h.assert_named();
    let dump = h.a11y_dump();
    Some(Rendered { image: h.render(), dump })
}

/// Pages per row of a contact sheet.
pub const SHEET_PER_ROW: usize = 4;
/// The gap between tiles and round the sheet, logical px.
const SHEET_GAP: i32 = 16;

/// Every ready page in the cell tiled on one image, SHEET_PER_ROW to a row, each at its own size (a row is as high as
/// its tallest page); None when no page is ready. For a quick look at the whole gallery.
pub fn contact_sheet(cell: &Cell) -> Option<Image> {
    let tiles: Vec<Image> =
        pages::REGISTRY.iter().filter_map(|(name, _)| render(name, cell)).map(|r| r.image).collect();
    let first = tiles.first()?;
    // The window colour of the cell's theme: the corner pixel of a page.
    let bg = first.px.first().copied().unwrap_or(0xFF00_0000);
    let gap = (SHEET_GAP * cell.dpi as i32 + 48) / 96;
    let rows: Vec<&[Image]> = tiles.chunks(SHEET_PER_ROW).collect();
    let row_w = |row: &[Image]| row.iter().map(|t| t.w as i32 + gap).sum::<i32>() + gap;
    let row_h = |row: &[Image]| row.iter().map(|t| t.h as i32).max().unwrap_or(0) + gap;
    let width = rows.iter().map(|r| row_w(r)).max().unwrap_or(0);
    let height = rows.iter().map(|r| row_h(r)).sum::<i32>() + gap;
    let mut px = vec![bg; (width * height) as usize];
    let mut y = gap;
    for row in rows {
        let mut x = gap;
        for t in row {
            for line in 0..t.h as usize {
                let to = (y as usize + line) * width as usize + x as usize;
                px[to..to + t.w as usize].copy_from_slice(&t.px[line * t.w as usize..(line + 1) * t.w as usize]);
            }
            x += t.w as i32 + gap;
        }
        y += row_h(row);
    }
    Some(Image { w: width as u32, h: height as u32, px, text_rects: Vec::new() })
}

/// Renders the pages offscreen: every ready page (or the one asked for) in its cells, as PNGs, and, when no page was
/// asked for, one contact sheet per language and DPI of the OLED theme (`contact/<lang>-<dpi>.png`). --lang and --dpi
/// pick cells of the matrix, or make one when the matrix has none like it. Exit 0, or 1 when an image fails to write.
pub fn run(opts: &PreviewOpts) -> i32 {
    let names: Vec<&str> = match &opts.page {
        Some(p) => vec![p.as_str()],
        None => pages::REGISTRY.iter().map(|(n, _)| *n).collect(),
    };
    let want_lang = opts.lang.map(|l| match l {
        PreviewLang::En => CellLang::En,
        PreviewLang::It => CellLang::It,
        PreviewLang::Pseudo => CellLang::Pseudo,
    });
    let mut written = 0;
    for name in names {
        let mut cells = cells_for(name);
        cells.retain(|c| want_lang.is_none_or(|l| c.lang == l) && opts.dpi.is_none_or(|d| c.dpi == d));
        if cells.is_empty() {
            let lang = want_lang.unwrap_or(CellLang::En);
            cells.push(Cell { lang, dpi: opts.dpi.unwrap_or(96), theme: CellTheme::Oled });
        }
        for cell in cells {
            let Some(r) = render(name, &cell) else { break };
            let path = opts.dir.join(name).join(format!("{}.png", cell.name()));
            if let Err(e) = r.image.write_png(&path) {
                eprintln!("preview: {}: {e}", path.display());
                return 1;
            }
            written += 1;
        }
    }
    if opts.page.is_none() {
        let mut sheets: Vec<Cell> =
            matrix().into_iter().filter(|c| c.theme == CellTheme::Oled && c.lang != CellLang::Pseudo).collect();
        sheets.retain(|c| want_lang.is_none_or(|l| c.lang == l) && opts.dpi.is_none_or(|d| c.dpi == d));
        if sheets.is_empty() {
            let lang = want_lang.unwrap_or(CellLang::En);
            sheets.push(Cell { lang, dpi: opts.dpi.unwrap_or(96), theme: CellTheme::Oled });
        }
        for cell in sheets {
            let Some(sheet) = contact_sheet(&cell) else { break };
            let path = opts.dir.join("contact").join(format!("{}.png", cell.name()));
            if let Err(e) = sheet.write_png(&path) {
                eprintln!("preview: {}: {e}", path.display());
                return 1;
            }
            written += 1;
        }
    }
    eprintln!("preview: {written} images under {}", Path::new(&opts.dir).display());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_has_twelve_cells_with_unique_names() {
        let m = matrix();
        assert_eq!(m.len(), 12);
        let mut names: Vec<String> = m.iter().map(Cell::name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 12);
        assert!(names.contains(&"pseudo-144".to_string()) && names.contains(&"en-96-hc".to_string()));
        assert!(names.contains(&"en-144-contrast".to_string()));
        assert_eq!(cells_for("glyphs").len(), 4);
    }

    #[test]
    fn themes_change_the_tokens() {
        let c = |theme| Cell { lang: CellLang::En, dpi: 96, theme }.tokens();
        assert!(!c(CellTheme::Oled).high_contrast && c(CellTheme::HighContrast).high_contrast);
        assert_eq!(c(CellTheme::Contrast).radius.s, 0);
        assert_eq!(c(CellTheme::Oled).radius.s, 7);
    }

    #[test]
    fn the_preview_writes_every_cell_of_a_page() {
        let dir = std::env::temp_dir().join(format!("gallery-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = PreviewOpts { dir: dir.clone(), lang: None, dpi: None, page: Some("glyphs".to_string()) };
        assert_eq!(run(&opts), 0);
        let count = std::fs::read_dir(dir.join("glyphs")).unwrap().count();
        assert_eq!(count, 4);
        let one =
            PreviewOpts { lang: Some(PreviewLang::It), dpi: Some(120), page: Some("buttons".to_string()), ..opts };
        assert_eq!(run(&one), 0);
        assert!(dir.join("buttons").join("it-120.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_contact_sheet_tiles_every_ready_page_at_its_size() {
        let cell = Cell { lang: CellLang::En, dpi: 96, theme: CellTheme::Oled };
        let sheet = contact_sheet(&cell).unwrap();
        let ready: Vec<_> = pages::all(UiLang::EN).into_iter().map(|p| (p.name, p.size)).collect();
        assert!(ready.len() > SHEET_PER_ROW, "more than one row of pages");
        let gap = SHEET_GAP;
        let rows: Vec<&[(&str, SizeL)]> = ready.chunks(SHEET_PER_ROW).collect();
        let want_w = rows.iter().map(|r| r.iter().map(|p| p.1.w + gap).sum::<i32>() + gap).max().unwrap();
        let want_h = rows.iter().map(|r| r.iter().map(|p| p.1.h).max().unwrap() + gap).sum::<i32>() + gap;
        assert_eq!((sheet.w as i32, sheet.h as i32), (want_w, want_h));
        // The first tile is the first page, pixel for pixel, at (gap, gap).
        let first = render(ready[0].0, &cell).unwrap().image;
        let at = oled_ui::geom::RectP::wh(gap, gap, first.w as i32, first.h as i32);
        assert_eq!(sheet.crop(at).px, first.px);
        // The second row starts below the tallest tile of the first.
        let row0_h = rows[0].iter().map(|p| p.1.h).max().unwrap();
        let second = render(ready[SHEET_PER_ROW].0, &cell).unwrap().image;
        let at = oled_ui::geom::RectP::wh(gap, gap + row0_h + gap, second.w as i32, second.h as i32);
        assert_eq!(sheet.crop(at).px, second.px);
    }

    #[test]
    fn the_preview_writes_one_contact_sheet_per_language_and_dpi() {
        let dir = std::env::temp_dir().join(format!("gallery-contact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = PreviewOpts { dir: dir.clone(), lang: Some(PreviewLang::It), dpi: None, page: None };
        assert_eq!(run(&opts), 0);
        let mut names: Vec<String> = std::fs::read_dir(dir.join("contact"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["it-120.png", "it-144.png", "it-192.png", "it-96.png"]);
        // One page alone writes no sheet.
        let _ = std::fs::remove_dir_all(&dir);
        let one = PreviewOpts { page: Some("glyphs".to_string()), ..opts };
        assert_eq!(run(&one), 0);
        assert!(!dir.join("contact").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
