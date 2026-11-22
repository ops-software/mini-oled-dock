//! Golden images of the `--preview` matrix (SPEC-platform 5.7.1).

mod common;

use std::env;
use std::path::{Path, PathBuf};

use common::compare::{compare, mask_union, text_mask, write_diff, Limits, Mismatch};
use common::run_preview;
use common::wic::{decode_png, Image};

// ------------------------------------------------------------------ the cells

fn png_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".png"))
        .collect();
    v.sort();
    v
}

fn decode(path: &Path) -> Image {
    decode_png(path).unwrap_or_else(|e| panic!("{e}"))
}

/// Replaces the PNGs at the top level of `to` with those of `from` (sub-folders of `to` stay).
fn replace_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for old in png_names(to) {
        std::fs::remove_file(to.join(old)).unwrap();
    }
    for n in png_names(from) {
        std::fs::copy(from.join(&n), to.join(&n)).unwrap();
    }
}

fn refuse_in_ci() {
    if env::var_os("CI").is_some() && env::var_os("GOLDEN_CI_REGEN").is_none() {
        panic!("UPDATE_GOLDEN is refused on CI");
    }
}

fn case(lang: &str, dpi: u32) {
    if env::var_os("GOLDEN_SKIP").is_some() {
        eprintln!("golden {lang}-{dpi}: skipped (GOLDEN_SKIP)");
        return;
    }
    // TextTile and TextOutliers only warn; Size and Exact still fail.
    let advisory = env::var_os("GOLDEN_TEXT_ADVISORY").is_some();
    // "base" = committed, made on the runner image; anything else = a set of this machine (a folder name
    // under tests/golden, or a relative path to an untracked place).
    let set = env::var("GOLDEN_SET").unwrap_or_else(|_| "base".into());
    let tmp: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden").join(format!("{lang}-{dpi}"));
    run_preview(&tmp.join("actual"), lang, dpi, false);
    run_preview(&tmp.join("notext"), lang, dpi, true);
    let gold = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(&set).join(format!("{lang}-{dpi}"));
    if env::var_os("UPDATE_GOLDEN").is_some() {
        refuse_in_ci();
        replace_dir(&tmp.join("actual"), &gold);
        // The golden's own text twin: the mask of the golden is made from it.
        replace_dir(&tmp.join("notext"), &gold.join("notext"));
        return;
    }
    assert!(gold.is_dir(), "no goldens for set {set}: run with UPDATE_GOLDEN=1, then look at the PNGs");
    assert_eq!(
        png_names(&gold),
        png_names(&tmp.join("actual")),
        "the set of images changed: UPDATE_GOLDEN=1, then look"
    );
    let mut bad = vec![];
    for n in png_names(&gold) {
        let (g, a, t) =
            (decode(&gold.join(&n)), decode(&tmp.join("actual").join(&n)), decode(&tmp.join("notext").join(&n)));
        let gt = decode(&gold.join("notext").join(&n));
        let same = |x: &Image, y: &Image| (x.w, x.h) == (y.w, y.h);
        let mask = if same(&g, &a) && same(&a, &t) && same(&g, &gt) {
            // Text of either render is text.
            mask_union(&text_mask(&a, &t, Limits::DEFAULT.grow), &text_mask(&g, &gt, Limits::DEFAULT.grow))
        } else {
            // A size change is reported as Size by compare.
            vec![false; (a.w * a.h) as usize]
        };
        if let Err(m) = compare(&g, &a, &mask, &Limits::DEFAULT) {
            if advisory && matches!(m, Mismatch::TextTile { .. } | Mismatch::TextOutliers { .. }) {
                eprintln!("golden {lang}-{dpi}/{n}: text tolerance exceeded (advisory): {m:?}");
                continue;
            }
            write_diff(&tmp.join("diff").join(&n), &g, &a, &mask);
            bad.push(format!("{lang}-{dpi}/{n}: {m:?}"));
        }
    }
    assert!(bad.is_empty(), "golden mismatch, diffs in {}:\n{}", tmp.join("diff").display(), bad.join("\n"));
}

macro_rules! cells {
    ($($name:ident: $lang:literal, $dpi:literal;)*) => { $( #[test] fn $name() { case($lang, $dpi); } )* };
}

cells! {
    en_96: "en", 96;
    en_120: "en", 120;
    en_144: "en", 144;
    en_192: "en", 192;
    it_96: "it", 96;
    it_120: "it", 120;
    it_144: "it", 144;
    it_192: "it", 192;
}

#[test]
fn the_child_gets_only_preview_and_a_folder() {
    let args = common::preview_args(Path::new("some/dir"));
    assert_eq!(args, vec![std::ffi::OsString::from("--preview"), std::ffi::OsString::from("some/dir")]);
}

// ------------------------------------------------------------------ comparator tests

#[cfg(test)]
mod comparator {
    use super::common::compare::*;
    use super::common::wic::*;

    const BLACK: u32 = 0xFF00_0000;
    const L: Limits = Limits::DEFAULT;

    fn mask_of(w: u32, h: u32, rect: (u32, u32, u32, u32)) -> Vec<bool> {
        let mut m = vec![false; (w * h) as usize];
        for y in rect.1..rect.3 {
            for x in rect.0..rect.2 {
                m[(y * w + x) as usize] = true;
            }
        }
        m
    }

    #[test]
    fn identical_images_pass() {
        let a = Image::new(16, 16, 0xFF12_3456);
        assert_eq!(compare(&a, &a.clone(), &vec![false; 256], &L), Ok(()));
    }

    #[test]
    fn one_pixel_outside_the_mask_is_exact() {
        let g = Image::new(16, 16, BLACK);
        let mut a = g.clone();
        a.set(9, 5, 0xFF00_0001);
        let r = compare(&g, &a, &vec![false; 256], &L);
        assert_eq!(r, Err(Mismatch::Exact { count: 1, first: (9, 5), bbox: (9, 5, 9, 5) }));
    }

    #[test]
    fn one_unit_inside_a_full_tile_passes() {
        let g = Image::new(16, 16, BLACK);
        let mut a = g.clone();
        a.set(3, 3, 0xFF00_0001);
        assert_eq!(compare(&g, &a, &mask_of(16, 16, (0, 0, 8, 8)), &L), Ok(()));
    }

    #[test]
    fn tile_mean_limit_is_inclusive() {
        let g = Image::new(16, 16, BLACK);
        let mask = mask_of(16, 16, (0, 0, 8, 8));
        let tile = |d: u32| {
            let mut a = g.clone();
            for y in 0..8 {
                for x in 0..8 {
                    a.set(x, y, 0xFF00_0000 | d << 16 | d << 8 | d);
                }
            }
            a
        };
        assert_eq!(compare(&g, &tile(3), &mask, &L), Ok(())); // 57,600 <= 300 * 3 * 64
        let r = compare(&g, &tile(4), &mask, &L);
        assert!(matches!(r, Err(Mismatch::TextTile { tile: (0, 0), .. })), "{r:?}");
    }

    #[test]
    fn outliers_are_counted_over_the_whole_mask() {
        let g = Image::new(32, 8, BLACK);
        let mut a = g.clone();
        a.set(2, 2, 0xFFC8_0000); // red channel off by 200: tile mean 200 / 192 = 1.04, one pixel
        let r = compare(&g, &a, &mask_of(32, 8, (0, 0, 8, 8)), &L); // 64 mask pixels: 1.56 %
        assert_eq!(r, Err(Mismatch::TextOutliers { count: 1, of: 64 }));
        assert_eq!(compare(&g, &a, &mask_of(32, 8, (0, 0, 25, 8)), &L), Ok(()));
        // 200 mask pixels: 0.5 %
    }

    #[test]
    fn size_mismatch_is_reported() {
        let r = compare(&Image::new(4, 4, BLACK), &Image::new(5, 4, BLACK), &[], &L);
        assert_eq!(r, Err(Mismatch::Size { golden: (4, 4), actual: (5, 4) }));
    }

    #[test]
    fn text_mask_of_one_pixel_is_a_5x5_block_clipped_at_the_border() {
        let a = Image::new(12, 12, BLACK);
        let mut b = a.clone();
        b.set(6, 6, 0xFFFF_FFFF);
        let m = text_mask(&b, &a, 2);
        assert_eq!(m.iter().filter(|&&x| x).count(), 25);
        assert!(m[(4 * 12 + 4) as usize] && m[(8 * 12 + 8) as usize] && !m[(3 * 12 + 6) as usize]);
        let mut c = a.clone();
        c.set(0, 0, 0xFFFF_FFFF);
        assert_eq!(text_mask(&c, &a, 2).iter().filter(|&&x| x).count(), 9);
    }

    #[test]
    fn an_image_without_text_is_compared_exactly() {
        let g = Image::new(16, 16, BLACK);
        let mask = text_mask(&g, &g.clone(), 4);
        assert!(mask.iter().all(|&m| !m));
        let mut a = g.clone();
        a.set(1, 1, 0xFF00_0001);
        assert!(matches!(compare(&g, &a, &mask, &L), Err(Mismatch::Exact { .. })));
    }

    /// Actual: a 10x10 text block at 20..30. Golden: the same plus a strip of text 3 px wide and 4 rows tall
    /// just right of it, red +20.
    fn strip_case() -> (Image, Image, Image) {
        let (mut actual, notext) = (Image::new(64, 64, BLACK), Image::new(64, 64, BLACK));
        for y in 20..30 {
            for x in 20..30 {
                actual.set(x, y, 0xFFFF_FFFF);
            }
        }
        let mut golden = actual.clone();
        for y in 20..24 {
            for x in 30..33 {
                golden.set(x, y, 0xFF14_0000);
            }
        }
        (golden, actual, notext)
    }

    #[test]
    fn text_margin_is_four_pixels() {
        let (golden, actual, notext) = strip_case();
        let ok = Limits { grow: 4, ..L };
        assert_eq!(compare(&golden, &actual, &text_mask(&actual, &notext, ok.grow), &ok), Ok(()));
        let tight = Limits { grow: 2, ..L };
        let r = compare(&golden, &actual, &text_mask(&actual, &notext, tight.grow), &tight);
        assert!(matches!(r, Err(Mismatch::Exact { .. })), "{r:?}");
    }

    #[test]
    fn text_of_the_golden_counts_through_the_union() {
        let (actual, notext) = {
            let mut a = Image::new(64, 64, BLACK);
            for y in 20..30 {
                for x in 20..30 {
                    a.set(x, y, 0xFFFF_FFFF);
                }
            }
            (a, Image::new(64, 64, BLACK))
        };
        // The golden's text reaches 10 px further right; those pixels are 2 off from the actual's black.
        let mut golden = actual.clone();
        for y in 20..30 {
            for x in 30..40 {
                golden.set(x, y, 0xFF02_0202);
            }
        }
        let from_actual = text_mask(&actual, &notext, 4);
        let from_golden = text_mask(&golden, &notext, 4);
        let r = compare(&golden, &actual, &from_actual, &L);
        assert!(matches!(r, Err(Mismatch::Exact { .. })), "{r:?}");
        assert_eq!(compare(&golden, &actual, &mask_union(&from_actual, &from_golden), &L), Ok(()));
    }

    #[test]
    fn png_round_trip_keeps_every_pixel() {
        let mut img = Image::new(7, 5, 0xFF10_2030);
        img.set(3, 2, 0x80FF_0000);
        img.set(6, 4, 0xFF00_FF00);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-roundtrip").join("t.png");
        write_png(&path, &img).unwrap();
        assert_eq!(decode_png(&path).unwrap(), img);
    }

    #[test]
    fn diff_png_is_written() {
        let g = Image::new(8, 8, BLACK);
        let mut a = g.clone();
        a.set(1, 1, 0xFF00_0001);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-roundtrip").join("diff.png");
        let _ = std::fs::remove_file(&path);
        write_diff(&path, &g, &a, &[false; 64]);
        let d = decode_png(&path).unwrap();
        assert_eq!((d.w, d.h), (24, 8));
        assert_eq!(d.get(16 + 1, 1), 0xFFFF_0000);
    }
}
