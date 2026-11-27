//! The software canvas (moved from surface.rs by W3).

use super::image::IconBitmap;
use super::pixel::{area_sample, argb, div255, eff_radius, over, rr_cov, white};
use windows::Win32::Foundation::RECT;

/// Drawing target: the pixels of a main DIB (premultiplied 0xAARRGGBB) with its size and an
/// optional clipping rectangle. Made by `Gdi::canvas`.
pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    /// Clipping rectangle (physical px) applied to every blend; `None` = the whole buffer.
    pub clip: Option<RECT>,
}

impl Canvas<'_> {
    /// "Source over" of the premultiplied `src` onto pixel `(x, y)`, if inside the buffer and the clip.
    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, src: u32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        if let Some(c) = self.clip {
            if x < c.left || x >= c.right || y < c.top || y >= c.bottom {
                return;
            }
        }
        let i = (y * self.w + x) as usize;
        self.buf[i] = over(self.buf[i], src);
    }

    /// "Source over" of the premultiplied `src` onto the pixels `[x0, x1)` of row `y`, clipped to the buffer and
    /// the clip rectangle. A run of identical destination pixels gives an identical result, so the last
    /// `over(dst, src)` is reused while the destination value repeats.
    pub fn blend_run(&mut self, y: i32, x0: i32, x1: i32, src: u32) {
        let (mut x0, mut x1) = (x0.max(0), x1.min(self.w));
        if y < 0 || y >= self.h {
            return;
        }
        if let Some(c) = self.clip {
            if y < c.top || y >= c.bottom {
                return;
            }
            x0 = x0.max(c.left);
            x1 = x1.min(c.right);
        }
        if x0 >= x1 {
            return;
        }
        let base = (y * self.w) as usize;
        let Some(run) = self.buf.get_mut(base + x0 as usize..base + x1 as usize) else { return };
        let mut memo: Option<(u32, u32)> = None;
        for px in run {
            let d = *px;
            *px = match memo {
                Some((from, to)) if from == d => to,
                _ => {
                    let to = over(d, src);
                    memo = Some((d, to));
                    to
                }
            };
        }
    }

    /// Filled AA rounded rectangle (physical px; `radius` in physical px), colour `rgb` and alpha `alpha`. Only the
    /// corner pixels of the corner rows are evaluated exactly; the rest of a row is one value.
    pub fn fill_rr(&mut self, rc: RECT, radius: i32, rgb: (u32, u32, u32), alpha: u32) {
        if alpha == 0 {
            return;
        }
        let rad = eff_radius(rc.left, rc.top, rc.right, rc.bottom, radius);
        let (xa, xb) = (rc.left.max(0), rc.right.min(self.w));
        let full = argb((alpha as f32 + 0.5) as u32, rgb.0, rgb.1, rgb.2);
        for y in rc.top.max(0)..rc.bottom.min(self.h) {
            if y >= rc.top + rad && y < rc.bottom - rad {
                self.blend_run(y, xa, xb, full);
                continue;
            }
            let (le, rs) = (rc.left + rad, rc.right - rad);
            for x in xa..xb.min(le) {
                self.fill_px(x, y, rc, radius, rgb, alpha);
            }
            self.blend_run(y, xa.max(le), xb.min(rs), full);
            for x in xa.max(rs).max(le)..xb {
                self.fill_px(x, y, rc, radius, rgb, alpha);
            }
        }
    }

    #[inline]
    fn fill_px(&mut self, x: i32, y: i32, rc: RECT, radius: i32, rgb: (u32, u32, u32), alpha: u32) {
        let cov = rr_cov(x, y, rc.left, rc.top, rc.right, rc.bottom, radius);
        let a = (alpha as f32 * cov + 0.5) as u32;
        if a > 0 {
            self.blend(x, y, argb(a, rgb.0, rgb.1, rgb.2));
        }
    }

    /// Inner ring of `bw` physical px of a rounded rectangle, white with alpha `alpha`. Nothing is evaluated or drawn
    /// inside the inner rectangle; the straight parts of the ring are one value per row.
    pub fn stroke_rr(&mut self, rc: RECT, radius: i32, bw: i32, alpha: u32) {
        if alpha == 0 {
            return;
        }
        let rad = eff_radius(rc.left, rc.top, rc.right, rc.bottom, radius);
        let irad = eff_radius(rc.left + bw, rc.top + bw, rc.right - bw, rc.bottom - bw, radius - bw);
        // Columns and rows nearer than `m` to a side are the corner bands; beyond them the outer coverage is 1
        // and the inner one repeats along the row.
        let m = rad.max(bw + irad);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if bw < 1 || w < 2 * m + 1 || h < 2 * m + 1 {
            self.stroke_rr_exact(rc, radius, bw, alpha);
            return;
        }
        let (xa, xb) = (rc.left.max(0), rc.right.min(self.w));
        let flat = white((alpha as f32 + 0.5) as u32);
        for y in rc.top.max(0)..rc.bottom.min(self.h) {
            if y >= rc.top + m && y < rc.bottom - m {
                self.blend_run(y, xa, xb.min(rc.left + bw), flat);
                self.blend_run(y, xa.max(rc.right - bw), xb, flat);
                continue;
            }
            for x in (xa..xb.min(rc.left + m)).chain(xa.max(rc.right - m)..xb) {
                self.stroke_px(x, y, rc, radius, bw, alpha);
            }
            let mx = rc.left + m;
            let (outer, inner) = (
                rr_cov(mx, y, rc.left, rc.top, rc.right, rc.bottom, radius),
                rr_cov(mx, y, rc.left + bw, rc.top + bw, rc.right - bw, rc.bottom - bw, radius - bw),
            );
            let a = (alpha as f32 * (outer - inner).max(0.0) + 0.5) as u32;
            if a > 0 {
                self.blend_run(y, xa.max(mx), xb.min(rc.right - m), white(a));
            }
        }
    }

    #[inline]
    fn stroke_px(&mut self, x: i32, y: i32, rc: RECT, radius: i32, bw: i32, alpha: u32) {
        let outer = rr_cov(x, y, rc.left, rc.top, rc.right, rc.bottom, radius);
        let inner = rr_cov(x, y, rc.left + bw, rc.top + bw, rc.right - bw, rc.bottom - bw, radius - bw);
        let a = (alpha as f32 * (outer - inner).max(0.0) + 0.5) as u32;
        if a > 0 {
            self.blend(x, y, white(a));
        }
    }

    /// The ring evaluated at every pixel of the rectangle (rectangles too small for the spans, and the reference).
    fn stroke_rr_exact(&mut self, rc: RECT, radius: i32, bw: i32, alpha: u32) {
        for y in rc.top.max(0)..rc.bottom.min(self.h) {
            for x in rc.left.max(0)..rc.right.min(self.w) {
                self.stroke_px(x, y, rc, radius, bw, alpha);
            }
        }
    }

    /// Opaque fill of the rounded rectangle `rc` (AA corners) with a colour computed per pixel by
    /// `color(x, y)`.
    pub fn fill_fn(&mut self, rc: RECT, radius: i32, color: &dyn Fn(i32, i32) -> (u32, u32, u32)) {
        for y in rc.top.max(0)..rc.bottom.min(self.h) {
            for x in rc.left.max(0)..rc.right.min(self.w) {
                let cov = rr_cov(x, y, rc.left, rc.top, rc.right, rc.bottom, radius);
                let a = (255.0 * cov + 0.5) as u32;
                if a > 0 {
                    let (r, g, b) = color(x, y);
                    self.blend(x, y, argb(a, r.min(255), g.min(255), b.min(255)));
                }
            }
        }
    }

    /// Blends a premultiplied bitmap into the `size` x `size` square with corner `(x, y)` and
    /// opacity `opacity`. A different size is scaled: area average when shrinking (stays crisp,
    /// e.g. tray icons rendered for a larger bar), nearest when enlarging (transitional case: the
    /// icon at the right size is arriving from the shell thread).
    pub fn blit(&mut self, bmp: &IconBitmap, x: i32, y: i32, size: i32, opacity: u32) {
        let (sw, sh) = (bmp.width as i32, bmp.height as i32);
        if bmp.is_empty() || size <= 0 || bmp.pixels.len() < (sw as usize) * (sh as usize) {
            return;
        }
        let same = sw == size && sh == size;
        for dy in 0..size {
            let yy = y + dy;
            if yy < 0 || yy >= self.h {
                continue;
            }
            let sy = if same { dy } else { dy * sh / size };
            for dx in 0..size {
                let xx = x + dx;
                if xx < 0 || xx >= self.w {
                    continue;
                }
                let sx = if same { dx } else { dx * sw / size };
                let mut s = if same || (sw <= size && sh <= size) {
                    bmp.pixels[(sy * sw + sx) as usize]
                } else {
                    area_sample(bmp, dx, dy, size)
                };
                if s >> 24 == 0 {
                    continue;
                }
                if opacity < 255 {
                    s = (div255((s >> 24) * opacity) << 24)
                        | (div255(((s >> 16) & 255) * opacity) << 16)
                        | (div255(((s >> 8) & 255) * opacity) << 8)
                        | div255((s & 255) * opacity);
                }
                self.blend(xx, yy, s);
            }
        }
    }

    /// Shape from `crate::glyphs` (16x16 space, `shape(u, v)` true = inside) in colour `rgb`
    /// with alpha `alpha`, in a `size` px square with corner `(x, y)`, 4x4 supersampling.
    pub fn glyph(
        &mut self,
        x: i32,
        y: i32,
        size: i32,
        rgb: (u32, u32, u32),
        alpha: u32,
        shape: &dyn Fn(f32, f32) -> bool,
    ) {
        if size <= 0 {
            return;
        }
        let hits = glyph_hit_counts(size, shape);
        self.glyph_from_hits(x, y, size, rgb, alpha, &hits);
    }

    /// Blends a glyph from its 4 x 4 hit counts (`glyph_hit_counts`), `size` x `size` of them, row by row.
    pub fn glyph_from_hits(&mut self, x: i32, y: i32, size: i32, rgb: (u32, u32, u32), alpha: u32, hits: &[u8]) {
        if size <= 0 || hits.len() < (size as usize) * (size as usize) {
            return;
        }
        for py in 0..size {
            for px in 0..size {
                let n = hits.get((py * size + px) as usize).copied().unwrap_or(0) as u32;
                if n > 0 {
                    self.blend(x + px, y + py, argb(alpha * n / 16, rgb.0, rgb.1, rgb.2));
                }
            }
        }
    }
}

/// How many of the 4 x 4 samples of each pixel of a `size` x `size` glyph are inside `shape` (16x16 space).
pub fn glyph_hit_counts(size: i32, shape: &dyn Fn(f32, f32) -> bool) -> Box<[u8]> {
    let mut out = Vec::with_capacity((size.max(0) as usize).pow(2));
    super::glyphs::set_scale(size);
    let k = 16.0 / size as f32;
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let u = (px as f32 + (sx as f32 + 0.5) / 4.0) * k;
                    let v = (py as f32 + (sy as f32 + 0.5) / 4.0) * k;
                    if shape(u, v) {
                        hits += 1;
                    }
                }
            }
            out.push(hits);
        }
    }
    out.into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::pixel::{rect_wh, RR_COV_CALLS};

    /// Today's bodies of `fill_rr` and `stroke_rr`, kept to prove the span versions equal to them.
    fn fill_reference(cv: &mut Canvas, rc: RECT, radius: i32, rgb: (u32, u32, u32), alpha: u32) {
        if alpha == 0 {
            return;
        }
        for y in rc.top.max(0)..rc.bottom.min(cv.h) {
            for x in rc.left.max(0)..rc.right.min(cv.w) {
                let cov = rr_cov(x, y, rc.left, rc.top, rc.right, rc.bottom, radius);
                let a = (alpha as f32 * cov + 0.5) as u32;
                if a > 0 {
                    cv.blend(x, y, argb(a, rgb.0, rgb.1, rgb.2));
                }
            }
        }
    }

    fn stroke_reference(cv: &mut Canvas, rc: RECT, radius: i32, bw: i32, alpha: u32) {
        if alpha == 0 {
            return;
        }
        cv.stroke_rr_exact(rc, radius, bw, alpha);
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: i32) -> i32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as i32
        }
    }

    #[test]
    fn fill_stroke_spans_equal_reference() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (cw, ch) = (70, 50);
        for case in 0..2000 {
            let mut a: Vec<u32> =
                (0..cw * ch).map(|_| (rng.next(256) as u32) << 24 | rng.next(0x100_0000) as u32).collect();
            // premultiplied-valid backgrounds are not required: both paths read the same values
            let mut b = a.clone();
            let (l, t) = (rng.next(90) - 15, rng.next(70) - 10);
            let rc = RECT { left: l, top: t, right: l + rng.next(80), bottom: t + rng.next(60) };
            let clip = (rng.next(2) == 0).then(|| {
                let (cl, ct) = (rng.next(60), rng.next(40));
                RECT { left: cl, top: ct, right: cl + rng.next(60), bottom: ct + rng.next(45) }
            });
            let radius = rng.next(21);
            let bw = 1 + rng.next(3);
            let alpha = rng.next(256) as u32;
            let rgb = (rng.next(256) as u32, rng.next(256) as u32, rng.next(256) as u32);
            let stroke = case % 2 == 1;
            {
                let mut cv = Canvas { buf: &mut a, w: cw, h: ch, clip };
                if stroke {
                    cv.stroke_rr(rc, radius, bw, alpha);
                } else {
                    cv.fill_rr(rc, radius, rgb, alpha);
                }
            }
            {
                let mut cv = Canvas { buf: &mut b, w: cw, h: ch, clip };
                if stroke {
                    stroke_reference(&mut cv, rc, radius, bw, alpha);
                } else {
                    fill_reference(&mut cv, rc, radius, rgb, alpha);
                }
            }
            let (rl, rt, rr, rb) = (rc.left, rc.top, rc.right, rc.bottom);
            assert!(
                a == b,
                "case {case} stroke {stroke} rc ({rl},{rt},{rr},{rb}) radius {radius} bw {bw} clip {clip:?}"
            );
        }
    }

    #[test]
    fn fill_stroke_evaluations_bounded() {
        let (cw, ch) = (700, 100);
        let mut buf = vec![0u32; (cw * ch) as usize];
        let mut cv = Canvas { buf: &mut buf, w: cw, h: ch, clip: None };
        let rc = rect_wh(10, 10, 600, 48);
        RR_COV_CALLS.with(|c| c.set(0));
        cv.fill_rr(rc, 12, (1, 2, 3), 200);
        let fill = RR_COV_CALLS.with(|c| c.get());
        assert!(fill <= 4 * 12 * 12, "{fill} rr_cov calls for the fill");
        RR_COV_CALLS.with(|c| c.set(0));
        cv.stroke_rr(rc, 12, 1, 60);
        let stroke = RR_COV_CALLS.with(|c| c.get());
        assert!(stroke <= 2 * 2 * 12 * (2 * 12 + 1), "{stroke} rr_cov calls for the stroke");
    }
}
