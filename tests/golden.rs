//! Golden images of the `--preview` matrix (SPEC-platform 5.7.1).

mod common;

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
        write_diff(&path, &g, &a, &vec![false; 64]);
        let d = decode_png(&path).unwrap();
        assert_eq!((d.w, d.h), (24, 8));
        assert_eq!(d.get(16 + 1, 1), 0xFFFF_0000);
    }
}
