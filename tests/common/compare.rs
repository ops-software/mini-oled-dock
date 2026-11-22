//! Golden comparator (SPEC-platform 5.7.1): outside the text mask every pixel must match exactly; inside
//! it a per-tile mean and a global outlier budget apply.

use std::path::Path;

use super::wic::{write_png, Image};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Side of the tile of the mean test, in pixels.
    pub tile: u32,
    /// Largest mean |delta| per colour sample inside a tile, times 100 (300 is 3.0 of 255).
    pub mean_x100: u32,
    /// A mask pixel counts as an outlier when some channel differs by more than this.
    pub outlier_delta: u32,
    /// Largest share of outliers among the mask pixels of the whole image, in hundredths of a percent.
    pub outlier_pct_x100: u32,
    /// The text of one render may sit up to this many pixels away from the text of the other.
    pub grow: i32,
}

impl Limits {
    pub const DEFAULT: Limits = Limits { tile: 8, mean_x100: 300, outlier_delta: 64, outlier_pct_x100: 100, grow: 4 };
}

#[derive(Debug, PartialEq, Eq)]
pub enum Mismatch {
    Size {
        golden: (u32, u32),
        actual: (u32, u32),
    },
    /// Pixels outside the mask that differ at all; `bbox` is (x0, y0, x1, y1), inclusive.
    Exact {
        count: usize,
        first: (u32, u32),
        bbox: (u32, u32, u32, u32),
    },
    /// First tile (in tile units) whose mean is over the limit.
    TextTile {
        tile: (u32, u32),
        mean_x100: u32,
    },
    TextOutliers {
        count: usize,
        of: usize,
    },
}

fn same_size(a: &Image, b: &Image) -> bool {
    (a.w, a.h) == (b.w, b.h)
}

/// True where the two renders differ, grown by `grow` pixels (Chebyshev distance), clipped at the border.
/// Images of different size give an empty mask (the comparison reports the size).
pub fn text_mask(with: &Image, without: &Image, grow: i32) -> Vec<bool> {
    let (w, h) = (with.w as usize, with.h as usize);
    if !same_size(with, without) {
        return vec![false; w * h];
    }
    let mut m: Vec<bool> = with.px.iter().zip(&without.px).map(|(a, b)| a != b).collect();
    let g = grow.max(0) as usize;
    if g == 0 || m.is_empty() {
        return m;
    }
    // Separable dilation with prefix sums: rows first, then columns.
    let mut tmp = vec![false; m.len()];
    for y in 0..h {
        let mut pre = vec![0u32; w + 1];
        for x in 0..w {
            pre[x + 1] = pre[x] + m[y * w + x] as u32;
        }
        for x in 0..w {
            tmp[y * w + x] = pre[(x + g + 1).min(w)] > pre[x.saturating_sub(g)];
        }
    }
    for x in 0..w {
        let mut pre = vec![0u32; h + 1];
        for y in 0..h {
            pre[y + 1] = pre[y] + tmp[y * w + x] as u32;
        }
        for y in 0..h {
            m[y * w + x] = pre[(y + g + 1).min(h)] > pre[y.saturating_sub(g)];
        }
    }
    m
}

pub fn mask_union(a: &[bool], b: &[bool]) -> Vec<bool> {
    assert_eq!(a.len(), b.len(), "masks of different size");
    a.iter().zip(b).map(|(x, y)| *x || *y).collect()
}

/// Sum and maximum of the per-channel absolute deltas of two 0xAARRGGBB pixels (alpha included in the
/// maximum, not in the sum: the sum is the colour sample sum the mean test uses).
fn delta(g: u32, a: u32) -> (u32, u32) {
    let ch = |s: u32| ((g >> s) & 0xFF).abs_diff((a >> s) & 0xFF);
    let (r, gr, b, al) = (ch(16), ch(8), ch(0), ch(24));
    (r + gr + b, r.max(gr).max(b).max(al))
}

/// Order of checks: size, exact outside the mask, tile mean inside it, outliers inside it.
pub fn compare(golden: &Image, actual: &Image, mask: &[bool], lim: &Limits) -> Result<(), Mismatch> {
    if !same_size(golden, actual) {
        return Err(Mismatch::Size { golden: (golden.w, golden.h), actual: (actual.w, actual.h) });
    }
    assert_eq!(mask.len(), golden.px.len(), "mask does not fit the image");
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
        t.0 += sum as u64;
        t.1 += 1;
        masked += 1;
        if max > lim.outlier_delta {
            outliers += 1;
        }
    }
    for (i, &(sum, n)) in sums.iter().enumerate() {
        if n > 0 && sum * 100 > lim.mean_x100 as u64 * 3 * n {
            let mean_x100 = (sum * 100 / (3 * n)) as u32;
            return Err(Mismatch::TextTile { tile: (i as u32 % tw, i as u32 / tw), mean_x100 });
        }
    }
    if (outliers as u64) * 10_000 > lim.outlier_pct_x100 as u64 * masked as u64 {
        return Err(Mismatch::TextOutliers { count: outliers, of: masked });
    }
    Ok(())
}

/// Writes a PNG: golden | actual | marks (red = differs outside the mask, amber = text pixel that
/// differs, dark grey = equal). Differently sized images are laid on the larger canvas.
pub fn write_diff(path: &Path, golden: &Image, actual: &Image, mask: &[bool]) {
    let (cw, ch) = (golden.w.max(actual.w), golden.h.max(actual.h));
    let mut out = Image::new(cw * 3, ch, 0xFF00_0000);
    let at = |img: &Image, x: u32, y: u32| (x < img.w && y < img.h).then(|| img.get(x, y));
    let masked = |x: u32, y: u32| same_size(golden, actual) && mask.get((y * golden.w + x) as usize) == Some(&true);
    for y in 0..ch {
        for x in 0..cw {
            let (g, a) = (at(golden, x, y), at(actual, x, y));
            out.set(x, y, g.unwrap_or(0xFF00_0000));
            out.set(cw + x, y, a.unwrap_or(0xFF00_0000));
            let mark = match (g, a) {
                (Some(g), Some(a)) if g == a => 0xFF20_2020,
                (Some(_), Some(_)) if masked(x, y) => 0xFFFF_B000,
                _ => 0xFFFF_0000,
            };
            out.set(2 * cw + x, y, mark);
        }
    }
    if let Err(e) = write_png(path, &out) {
        eprintln!("golden: could not write the diff {}: {e}", path.display());
    }
}
