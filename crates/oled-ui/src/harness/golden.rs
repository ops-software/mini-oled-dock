//! The golden comparator (SPEC-platform 5.7.1, ported: open point 12; the dock's own stays in its test files). Outside the
//! text rects grown by 4 px every pixel must match exactly; inside, per 8 x 8 tile the mean |delta| is at most 3/255 and
//! at most 1 % of the mask pixels differ by more than 64.

use std::path::{Path, PathBuf};

use super::Image;
use crate::geom::RectP;

/// The limits of the text rules.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Side of the tile of the mean test, px.
    pub tile: u32,
    /// Largest mean |delta| per colour sample in a tile, times 100 (300 = 3.0 of 255).
    pub mean_x100: u32,
    /// A mask pixel is an outlier when a channel differs by more than this.
    pub outlier_delta: u32,
    /// Largest share of outliers among the mask pixels, in hundredths of a percent.
    pub outlier_pct_x100: u32,
    /// The mask: the text rects grown by this many px.
    pub grow: i32,
}

pub(crate) const LIMITS: Limits = Limits { tile: 8, mean_x100: 300, outlier_delta: 64, outlier_pct_x100: 100, grow: 4 };

/// Why an image does not match its golden.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mismatch {
    Size {
        golden: (u32, u32),
        actual: (u32, u32),
    },
    /// Pixels outside the mask that differ at all; bbox is (x0, y0, x1, y1), inclusive.
    Exact {
        count: usize,
        first: (u32, u32),
        bbox: (u32, u32, u32, u32),
    },
    /// The first tile (in tile units) whose mean is over the limit.
    TextTile {
        tile: (u32, u32),
        mean_x100: u32,
    },
    TextOutliers {
        count: usize,
        of: usize,
    },
}

impl Mismatch {
    /// True for the two text rules, which GOLDEN_TEXT_ADVISORY turns into warnings.
    fn is_text_rule(&self) -> bool {
        matches!(self, Mismatch::TextTile { .. } | Mismatch::TextOutliers { .. })
    }
}

/// The text rects grown by `grow` px and clipped to the image, as one flag per pixel.
pub(crate) fn mask_of(rects: &[RectP], w: u32, h: u32, grow: i32) -> Vec<bool> {
    let mut m = vec![false; (w as usize) * (h as usize)];
    let img = RectP { l: 0, t: 0, r: w as i32, b: h as i32 };
    for r in rects {
        let g = r.inset(-grow, -grow).intersect(img);
        for y in g.t..g.b {
            m[(y as usize) * (w as usize) + g.l as usize..(y as usize) * (w as usize) + g.r as usize].fill(true);
        }
    }
    m
}

/// Sum of the per-channel deltas of two 0xAARRGGBB pixels and the largest channel delta (alpha included in the
/// maximum, not in the sum).
fn delta(g: u32, a: u32) -> (u32, u32) {
    let ch = |s: u32| ((g >> s) & 0xFF).abs_diff((a >> s) & 0xFF);
    let (r, gr, b, al) = (ch(16), ch(8), ch(0), ch(24));
    (r + gr + b, r.max(gr).max(b).max(al))
}

/// The rules, in this order: size, exact outside the mask, tile mean inside it, outliers inside it.
pub(crate) fn compare(golden: &Image, actual: &Image, mask: &[bool], lim: &Limits) -> Result<(), Mismatch> {
    if (golden.w, golden.h) != (actual.w, actual.h) {
        return Err(Mismatch::Size { golden: (golden.w, golden.h), actual: (actual.w, actual.h) });
    }
    assert_eq!(mask.len(), golden.px.len(), "the mask does not fit the image");
    let w = golden.w;
    let (mut count, mut first, mut bbox) = (0usize, (0, 0), (u32::MAX, u32::MAX, 0, 0));
    for (i, (&g, &a)) in golden.px.iter().zip(&actual.px).enumerate() {
        if g != a && !mask[i] {
            let (x, y) = (i as u32 % w, i as u32 / w);
            if count == 0 {
                first = (x, y);
            }
            count += 1;
            bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
        }
    }
    if count > 0 {
        return Err(Mismatch::Exact { count, first, bbox });
    }
    let tile = lim.tile.max(1);
    let (tw, th) = (golden.w.div_ceil(tile), golden.h.div_ceil(tile));
    let mut sums = vec![(0u64, 0u64); (tw * th) as usize]; // (sum of deltas, mask pixels)
    let (mut masked, mut outliers) = (0usize, 0usize);
    for (i, (&g, &a)) in golden.px.iter().zip(&actual.px).enumerate() {
        if !mask[i] {
            continue;
        }
        let (x, y) = (i as u32 % w, i as u32 / w);
        let (sum, max) = delta(g, a);
        let t = &mut sums[((y / tile) * tw + x / tile) as usize];
        t.0 += u64::from(sum);
        t.1 += 1;
        masked += 1;
        if max > lim.outlier_delta {
            outliers += 1;
        }
    }
    for (i, &(sum, n)) in sums.iter().enumerate() {
        if n > 0 && sum * 100 > u64::from(lim.mean_x100) * 3 * n {
            let mean_x100 = (sum * 100 / (3 * n)) as u32;
            return Err(Mismatch::TextTile { tile: (i as u32 % tw, i as u32 / tw), mean_x100 });
        }
    }
    if (outliers as u64) * 10_000 > u64::from(lim.outlier_pct_x100) * masked as u64 {
        return Err(Mismatch::TextOutliers { count: outliers, of: masked });
    }
    Ok(())
}

/// The diff image: golden | actual | marks (red: differs outside the mask, amber: a text pixel that differs, dark grey:
/// equal). Images of different size are laid on the larger canvas.
fn diff_image(golden: &Image, actual: &Image, mask: &[bool]) -> Image {
    let (cw, ch) = (golden.w.max(actual.w), golden.h.max(actual.h));
    let mut out = vec![0xFF00_0000u32; (cw * 3 * ch) as usize];
    let at = |img: &Image, x: u32, y: u32| (x < img.w && y < img.h).then(|| img.px[(y * img.w + x) as usize]);
    let same = (golden.w, golden.h) == (actual.w, actual.h);
    for y in 0..ch {
        for x in 0..cw {
            let (g, a) = (at(golden, x, y), at(actual, x, y));
            let row = (y * cw * 3) as usize;
            out[row + x as usize] = g.unwrap_or(0xFF00_0000);
            out[row + (cw + x) as usize] = a.unwrap_or(0xFF00_0000);
            let masked = same && mask.get((y * golden.w + x) as usize) == Some(&true);
            out[row + (2 * cw + x) as usize] = match (g, a) {
                (Some(g), Some(a)) if g == a => 0xFF20_2020,
                (Some(_), Some(_)) if masked => 0xFFFF_B000,
                _ => 0xFFFF_0000,
            };
        }
    }
    Image { w: cw * 3, h: ch, px: out, text_rects: Vec::new() }
}

/// What to do with a cell whose golden may be missing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Compare,
    /// Print "no base goldens for <page>" and pass (a coder's local gates between a page's first commit and its base).
    Skip,
    Fail,
}

/// The pure decision: a cell whose file exists is compared; a page that has no folder in the set at all fails on CI and
/// is skipped elsewhere; a cell missing from a page that has a folder always fails (the matrix changed).
pub(crate) fn decision(ci: bool, page_dir_exists: bool, file_exists: bool) -> Decision {
    match (file_exists, page_dir_exists) {
        (true, _) => Decision::Compare,
        (false, true) => Decision::Fail,
        (false, false) if ci => Decision::Fail,
        (false, false) => Decision::Skip,
    }
}

/// UPDATE_GOLDEN is refused on CI unless GOLDEN_CI_REGEN is set (the regen-golden job).
pub(crate) fn update_allowed(ci: bool, regen: bool) -> bool {
    !ci || regen
}

/// The environment of one check.
#[derive(Clone, Debug)]
pub(crate) struct Opts {
    pub set: String,
    pub ci: bool,
    pub regen: bool,
    pub update: bool,
    pub text_advisory: bool,
}

impl Opts {
    fn from_env() -> Opts {
        let has = |k: &str| std::env::var_os(k).is_some();
        Opts {
            set: std::env::var("GOLDEN_SET").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "base".to_string()),
            ci: has("CI"),
            regen: has("GOLDEN_CI_REGEN"),
            update: has("UPDATE_GOLDEN"),
            text_advisory: has("GOLDEN_TEXT_ADVISORY"),
        }
    }
}

/// The result of a check that did not fail.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Matched,
    Written,
    Skipped,
    /// The text rules failed and GOLDEN_TEXT_ADVISORY made them a warning.
    Warned,
}

fn cell_path(root: &Path, set: &str, name: &str) -> PathBuf {
    root.join(set).join(format!("{name}.png"))
}

/// The check, with its environment given: Err is the failure message.
pub(crate) fn run_check(opts: &Opts, root: &Path, diff: &Path, name: &str, img: &Image) -> Result<Outcome, String> {
    let path = cell_path(root, &opts.set, name);
    if opts.update {
        if !update_allowed(opts.ci, opts.regen) {
            return Err("UPDATE_GOLDEN is refused on CI".to_string());
        }
        img.write_png(&path)?;
        return Ok(Outcome::Written);
    }
    let page = name.split('/').next().unwrap_or(name);
    match decision(opts.ci, root.join(&opts.set).join(page).is_dir(), path.is_file()) {
        Decision::Skip => {
            eprintln!("no base goldens for {page}: run the regen-golden job");
            return Ok(Outcome::Skipped);
        }
        Decision::Fail if !path.is_file() && !root.join(&opts.set).join(page).is_dir() => {
            return Err(format!("no base goldens for {page}: run the regen-golden job"));
        }
        Decision::Fail => {
            return Err(format!("no golden {}: the set of images changed, UPDATE_GOLDEN=1 and look", path.display()));
        }
        Decision::Compare => {}
    }
    super::image::init_com();
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bmp = crate::gfx::wic::decode_png_exact(&bytes).map_err(|e| format!("decode {}: {e}", path.display()))?;
    let golden = Image { w: bmp.width, h: bmp.height, px: bmp.pixels, text_rects: Vec::new() };
    let mask = if (golden.w, golden.h) == (img.w, img.h) {
        mask_of(&img.text_rects, img.w, img.h, LIMITS.grow)
    } else {
        Vec::new()
    };
    let mask = if mask.is_empty() { vec![false; golden.px.len()] } else { mask };
    let Err(m) = (if (golden.w, golden.h) == (img.w, img.h) {
        compare(&golden, img, &mask, &LIMITS)
    } else {
        Err(Mismatch::Size { golden: (golden.w, golden.h), actual: (img.w, img.h) })
    }) else {
        return Ok(Outcome::Matched);
    };
    let diff_path = diff.join(format!("{name}.png"));
    let same = (golden.w, golden.h) == (img.w, img.h);
    let marks = diff_image(&golden, img, if same { &mask } else { &[] });
    if let Err(e) = marks.write_png(&diff_path) {
        eprintln!("golden: could not write the diff {}: {e}", diff_path.display());
    }
    if opts.text_advisory && m.is_text_rule() {
        eprintln!("golden (advisory): {name}: {m:?}; diff {}", diff_path.display());
        return Ok(Outcome::Warned);
    }
    Err(format!("golden {name} differs: {m:?}; diff {}", diff_path.display()))
}

/// Compares `img` with the golden `<root>/<set>/<name>.png`; a mismatch writes a diff PNG under `diff` and panics with its
/// path. root = apps/gallery/tests/golden; the set is GOLDEN_SET (default "base", the CI runner's set); name =
/// "<page>/<cell>"; UPDATE_GOLDEN=1 writes the set (refused when CI is set unless GOLDEN_CI_REGEN); a page missing from
/// the set fails on CI and only prints a line elsewhere; GOLDEN_TEXT_ADVISORY makes the two text rules warnings.
pub fn check(root: &Path, diff: &Path, name: &str, img: &Image) {
    if let Err(e) = run_check(&Opts::from_env(), root, diff, name, img) {
        panic!("{e}");
    }
}

/// The text snapshot check (the a11y dump): the same rules for the set, a missing page and UPDATE_GOLDEN as the images,
/// compared exactly. `name` includes its extension ("<page>/a11y.txt").
pub(crate) fn run_check_text(opts: &Opts, root: &Path, diff: &Path, name: &str, text: &str) -> Result<Outcome, String> {
    let path = root.join(&opts.set).join(name);
    if opts.update {
        if !update_allowed(opts.ci, opts.regen) {
            return Err("UPDATE_GOLDEN is refused on CI".to_string());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
        return Ok(Outcome::Written);
    }
    let page = name.split('/').next().unwrap_or(name);
    let page_dir = root.join(&opts.set).join(page).is_dir();
    match decision(opts.ci, page_dir, path.is_file()) {
        Decision::Skip => {
            eprintln!("no base goldens for {page}: run the regen-golden job");
            return Ok(Outcome::Skipped);
        }
        Decision::Fail if !page_dir => return Err(format!("no base goldens for {page}: run the regen-golden job")),
        Decision::Fail => return Err(format!("no golden {}: UPDATE_GOLDEN=1 and look", path.display())),
        Decision::Compare => {}
    }
    let want = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    // A checkout may have changed the line ends of the committed file.
    if want.replace("\r\n", "\n") == text {
        return Ok(Outcome::Matched);
    }
    let actual = diff.join(format!("{name}.actual"));
    if let Some(dir) = actual.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&actual, text);
    Err(format!("snapshot {name} differs from {}; the actual text is in {}", path.display(), actual.display()))
}

/// Compares `text` with `<root>/<set>/<name>` (see `check` for the set and the environment); panics on a difference.
pub fn check_text(root: &Path, diff: &Path, name: &str, text: &str) {
    if let Err(e) = run_check_text(&Opts::from_env(), root, diff, name, text) {
        panic!("{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, v: u32) -> Image {
        Image { w, h, px: vec![v; (w * h) as usize], text_rects: Vec::new() }
    }

    fn with_noise(base: &Image, x0: u32, y0: u32, x1: u32, y1: u32, d: u32) -> Image {
        let mut o = Image { w: base.w, h: base.h, px: base.px.clone(), text_rects: Vec::new() };
        for y in y0..y1 {
            for x in x0..x1 {
                o.px[(y * o.w + x) as usize] = base.px[0].wrapping_add(d | (d << 8) | (d << 16));
            }
        }
        o
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("oled-ui-golden-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn comparator_rules() {
        let lim = LIMITS;
        let g = img(64, 32, 0xFF10_1010);
        let text = [RectP::wh(16, 8, 24, 12)];
        let mask = mask_of(&text, 64, 32, lim.grow);
        assert_eq!(compare(&g, &g, &mask, &lim), Ok(()), "identical");
        // One pixel off outside the grown text rect: exact.
        let bad = with_noise(&g, 60, 30, 61, 31, 5);
        assert_eq!(
            compare(&g, &bad, &mask, &lim),
            Err(Mismatch::Exact { count: 1, first: (60, 30), bbox: (60, 30, 60, 30) })
        );
        // The same change inside the text rect grown by 4 px is text noise within the limits.
        let noisy = with_noise(&g, 16, 8, 40, 20, 2);
        assert_eq!(compare(&g, &noisy, &mask, &lim), Ok(()), "text noise of 2/255");
        let edge = with_noise(&g, 12, 8, 13, 9, 5);
        assert_eq!(compare(&g, &edge, &mask, &lim), Ok(()), "the grown border counts as text");
        // Heavy text noise: a tile mean over 3/255.
        let heavy = with_noise(&g, 16, 8, 24, 16, 40);
        assert!(matches!(compare(&g, &heavy, &mask, &lim), Err(Mismatch::TextTile { .. })));
        // A few strong outliers in otherwise equal text pixels: the share rule.
        let mut spikes = Image { w: g.w, h: g.h, px: g.px.clone(), text_rects: Vec::new() };
        for x in 12..44 {
            spikes.px[(8 * 64 + x) as usize] = 0xFFFF_FFFF;
        }
        assert!(matches!(
            compare(&g, &spikes, &mask, &Limits { mean_x100: 100_000, ..lim }),
            Err(Mismatch::TextOutliers { .. })
        ));
        // A change of size fails before anything else.
        assert_eq!(
            compare(&g, &img(64, 33, 0xFF10_1010), &mask, &lim),
            Err(Mismatch::Size { golden: (64, 32), actual: (64, 33) })
        );
    }

    #[test]
    fn text_snapshots_follow_the_same_rules() {
        let root = tmp("text");
        let o = |update| Opts { set: "local".into(), ci: false, regen: false, update, text_advisory: false };
        let diff = root.join("diff");
        assert_eq!(run_check_text(&o(true), &root, &diff, "p/a11y.txt", "Button \"x\"\n"), Ok(Outcome::Written));
        assert_eq!(run_check_text(&o(false), &root, &diff, "p/a11y.txt", "Button \"x\"\n"), Ok(Outcome::Matched));
        let e = run_check_text(&o(false), &root, &diff, "p/a11y.txt", "Button \"y\"\n").unwrap_err();
        assert!(e.contains("differs") && diff.join("p/a11y.txt.actual").is_file(), "{e}");
        assert_eq!(run_check_text(&o(false), &root, &diff, "q/a11y.txt", "z"), Ok(Outcome::Skipped));
        let ci = Opts { ci: true, ..o(false) };
        assert!(run_check_text(&ci, &root, &diff, "q/a11y.txt", "z").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_base_fails_only_on_ci() {
        assert_eq!(decision(false, false, false), Decision::Skip);
        assert_eq!(decision(true, false, false), Decision::Fail);
        assert_eq!(decision(false, true, false), Decision::Fail, "a page whose matrix lost a cell");
        assert_eq!(decision(true, true, true), Decision::Compare);
        assert_eq!(decision(false, false, true), Decision::Compare);
    }

    #[test]
    fn update_refused_on_ci() {
        assert!(update_allowed(false, false));
        assert!(!update_allowed(true, false));
        assert!(update_allowed(true, true));
        let root = tmp("refuse");
        let opts = Opts { set: "base".into(), ci: true, regen: false, update: true, text_advisory: false };
        let e = run_check(&opts, &root, &root.join("diff"), "p/a", &img(4, 4, 0xFF00_0000)).unwrap_err();
        assert!(e.contains("refused"));
        assert!(!root.exists(), "nothing was written");
    }

    #[test]
    fn write_then_compare_round_trips_through_png() {
        let root = tmp("rt");
        let opts = |update| Opts { set: "local".into(), ci: false, regen: false, update, text_advisory: false };
        let a = img(40, 24, 0xFF20_3040);
        let diff = root.join("diff");
        assert_eq!(run_check(&opts(true), &root, &diff, "page/en-96", &a), Ok(Outcome::Written));
        assert_eq!(run_check(&opts(false), &root, &diff, "page/en-96", &a), Ok(Outcome::Matched));
        let b = with_noise(&a, 3, 3, 5, 5, 9);
        let e = run_check(&opts(false), &root, &diff, "page/en-96", &b).unwrap_err();
        assert!(e.contains("Exact") && e.contains("en-96.png"), "{e}");
        assert!(diff.join("page/en-96.png").is_file(), "the diff PNG is written");
        // A page with no folder passes off CI, with a line.
        assert_eq!(run_check(&opts(false), &root, &diff, "other/en-96", &a), Ok(Outcome::Skipped));
        let ci = Opts { ci: true, ..opts(false) };
        assert!(run_check(&ci, &root, &diff, "other/en-96", &a).unwrap_err().contains("no base goldens for other"));
        // The text rules can be made advisory.
        let mut t = img(40, 24, 0xFF20_3040);
        t.text_rects = vec![RectP::wh(10, 6, 12, 8)];
        let heavy = with_noise(&t, 10, 6, 22, 14, 60);
        let adv = Opts { text_advisory: true, ..opts(false) };
        let heavy = Image { text_rects: t.text_rects.clone(), ..heavy };
        assert_eq!(run_check(&adv, &root, &diff, "page/en-96", &heavy), Ok(Outcome::Warned));
        let _ = std::fs::remove_dir_all(&root);
    }
}
