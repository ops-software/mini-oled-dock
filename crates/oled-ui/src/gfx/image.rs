//! Icon bitmaps and their pure pixel maths (moved from icons.rs by W3).
//!
//! FORMAT: `IconBitmap::pixels` is a row-major, top-down `Vec<u32>`, each pixel 0xAARRGGBB
//! PREMULTIPLIED (the same layout as the 32-bit DIB of the dock renderer, so blending is a direct
//! read).

/// Alpha threshold below which a pixel is considered "empty" for border cropping.
const CROP_ALPHA_MIN: u32 = 8;

/// Premultiplied RGBA image (see FORMAT in the module doc).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IconBitmap {
    pub width: u32,
    pub height: u32,
    /// `width * height` premultiplied 0xAARRGGBB pixels, top-down.
    pub pixels: Vec<u32>,
}

impl IconBitmap {
    /// Empty (transparent) `w` x `h` bitmap.
    pub fn new(w: u32, h: u32) -> IconBitmap {
        IconBitmap { width: w, height: h, pixels: vec![0; (w as usize) * (h as usize)] }
    }

    /// true if it contains no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.pixels.is_empty()
    }

    /// true if at least one pixel has alpha != 0 (used to discard "empty" extractions).
    pub fn has_visible_pixels(&self) -> bool {
        self.pixels.iter().any(|p| p >> 24 != 0)
    }
}

/// Premultiplies a "straight" 0xAARRGGBB pixel.
pub fn premultiply_px(p: u32) -> u32 {
    let a = p >> 24;
    match a {
        255 => p,
        0 => 0,
        _ => {
            let m = |c: u32| (c * a + 127) / 255;
            (a << 24) | (m((p >> 16) & 255) << 16) | (m((p >> 8) & 255) << 8) | m(p & 255)
        }
    }
}

/// Inverse of `premultiply_px` (for writing non-premultiplied PNGs).
pub fn unpremultiply_px(p: u32) -> u32 {
    let a = p >> 24;
    match a {
        255 => p,
        0 => 0,
        _ => {
            let u = |c: u32| ((c * 255 + a / 2) / a).min(255);
            (a << 24) | (u((p >> 16) & 255) << 16) | (u((p >> 8) & 255) << 8) | u(p & 255)
        }
    }
}

/// Rectangle (x, y, w, h) of pixels with alpha >= `CROP_ALPHA_MIN`; `None` if there are none.
pub fn opaque_bbox(src: &IconBitmap) -> Option<(usize, usize, usize, usize)> {
    let (w, h) = (src.width as usize, src.height as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        let row = &src.pixels[y * w..(y + 1) * w];
        for (x, p) in row.iter().enumerate() {
            if p >> 24 >= CROP_ALPHA_MIN {
                x0 = x0.min(x);
                x1 = x1.max(x + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 > x0 && y1 > y0 {
        Some((x0, y0, x1 - x0, y1 - y0))
    } else {
        None
    }
}

/// True when `resize_area(src, size_px)` would return `src` unchanged: already `size_px` square
/// and nothing to crop (no transparent border, or fully transparent).
pub fn resize_is_identity(src: &IconBitmap, size_px: u32) -> bool {
    let (w, h) = (src.width as usize, src.height as usize);
    size_px > 0
        && src.width == size_px
        && src.height == size_px
        && src.pixels.len() == w * h
        && opaque_bbox(src).is_none_or(|b| b == (0, 0, w, h))
}

/// Resizes to `size_px` x `size_px` (square side) with an area (box) filter using
/// fractional weights, on premultiplied data. The source is first cropped to its
/// transparent borders and centered in a square (jumbo icons often have an empty margin).
/// If the magnification is > 1 (source smaller than the destination) it uses bilinear
/// interpolation. `size_px == 0` or an empty/inconsistent source -> empty bitmap.
/// Pure, usable from any thread.
pub fn resize_area(src: &IconBitmap, size_px: u32) -> IconBitmap {
    let (sw, sh) = (src.width as usize, src.height as usize);
    if size_px == 0 || src.is_empty() || src.pixels.len() != sw * sh {
        return IconBitmap::default();
    }
    let (cx, cy, cw, ch) = opaque_bbox(src).unwrap_or((0, 0, sw, sh));
    let side = cw.max(ch);
    // Offset of the crop inside the virtual `side` x `side` square.
    let (ox, oy) = ((side - cw) / 2, (side - ch) / 2);
    let n = size_px as usize;
    let scale = side as f32 / n as f32; // source pixels per destination pixel

    // Premultiplied pixel as [a, r, g, b] in f32; outside the crop = transparent.
    let fetch = |vx: isize, vy: isize| -> [f32; 4] {
        let (x, y) = (vx - ox as isize, vy - oy as isize);
        if x < 0 || y < 0 || x >= cw as isize || y >= ch as isize {
            return [0.0; 4];
        }
        let p = src.pixels[(cy + y as usize) * sw + cx + x as usize];
        [(p >> 24) as f32, ((p >> 16) & 255) as f32, ((p >> 8) & 255) as f32, (p & 255) as f32]
    };

    let mut out = IconBitmap::new(size_px, size_px);
    for dy in 0..n {
        for dx in 0..n {
            let acc: [f32; 4] = if scale >= 1.0 {
                // Area filter: weighted average of the source pixels covered by [dx*s, (dx+1)*s).
                let (fx0, fx1) = (dx as f32 * scale, (dx + 1) as f32 * scale);
                let (fy0, fy1) = (dy as f32 * scale, (dy + 1) as f32 * scale);
                let mut sum = [0.0f32; 4];
                let (ix0, ix1) = (fx0.floor() as isize, (fx1.ceil() as isize).max(fx0.floor() as isize + 1));
                let (iy0, iy1) = (fy0.floor() as isize, (fy1.ceil() as isize).max(fy0.floor() as isize + 1));
                for sy in iy0..iy1 {
                    let wy = (fy1.min((sy + 1) as f32) - fy0.max(sy as f32)).max(0.0);
                    for sx in ix0..ix1 {
                        let wx = (fx1.min((sx + 1) as f32) - fx0.max(sx as f32)).max(0.0);
                        let wgt = wx * wy;
                        if wgt <= 0.0 {
                            continue;
                        }
                        let v = fetch(sx, sy);
                        for k in 0..4 {
                            sum[k] += v[k] * wgt;
                        }
                    }
                }
                let area = scale * scale;
                [sum[0] / area, sum[1] / area, sum[2] / area, sum[3] / area]
            } else {
                // Magnification: bilinear at the center of the destination pixel.
                let sxf = (dx as f32 + 0.5) * scale - 0.5;
                let syf = (dy as f32 + 0.5) * scale - 0.5;
                // Destination pixels whose centre falls in the square padding stay transparent;
                // inside the crop the taps are clamped to it (edge extension, no faded fringe).
                let inside = |c: f32, lo: usize, len: usize| c >= lo as f32 && c < (lo + len) as f32;
                if !inside(sxf + 0.5, ox, cw) || !inside(syf + 0.5, oy, ch) {
                    [0.0; 4]
                } else {
                    let (x0, y0) = (sxf.floor(), syf.floor());
                    let (tx, ty) = (sxf - x0, syf - y0);
                    let (x0, y0) = (x0 as isize, y0 as isize);
                    let (lx, hx) = (ox as isize, (ox + cw) as isize - 1);
                    let (ly, hy) = (oy as isize, (oy + ch) as isize - 1);
                    let (xa, xb) = (x0.clamp(lx, hx), (x0 + 1).clamp(lx, hx));
                    let (ya, yb) = (y0.clamp(ly, hy), (y0 + 1).clamp(ly, hy));
                    let (p00, p10) = (fetch(xa, ya), fetch(xb, ya));
                    let (p01, p11) = (fetch(xa, yb), fetch(xb, yb));
                    let mut v = [0.0f32; 4];
                    for k in 0..4 {
                        let top = p00[k] + (p10[k] - p00[k]) * tx;
                        let bot = p01[k] + (p11[k] - p01[k]) * tx;
                        v[k] = top + (bot - top) * ty;
                    }
                    v
                }
            };
            let a = (acc[0] + 0.5).clamp(0.0, 255.0) as u32;
            let c = |v: f32| ((v + 0.5).clamp(0.0, 255.0) as u32).min(a);
            out.pixels[dy * n + dx] = (a << 24) | (c(acc[1]) << 16) | (c(acc[2]) << 8) | c(acc[3]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, p: u32) -> IconBitmap {
        IconBitmap { width: w, height: h, pixels: vec![p; (w * h) as usize] }
    }

    #[test]
    fn resize_down_keeps_solid_colour() {
        let s = solid(256, 256, 0xFF80_4020);
        let r = resize_area(&s, 24);
        assert_eq!((r.width, r.height), (24, 24));
        assert!(r.pixels.iter().all(|&p| p == 0xFF80_4020));
    }

    #[test]
    fn resize_up_keeps_solid_colour() {
        let r = resize_area(&solid(16, 16, 0x8040_2010), 64);
        assert!(r.pixels.iter().all(|&p| p == 0x8040_2010));
    }

    #[test]
    fn transparent_border_is_cropped() {
        let mut s = IconBitmap::new(100, 100);
        for y in 40..60 {
            for x in 40..60 {
                s.pixels[y * 100 + x] = 0xFFFF_FFFF;
            }
        }
        let r = resize_area(&s, 10);
        assert!(r.pixels.iter().all(|&p| p == 0xFFFF_FFFF));
    }

    #[test]
    fn channels_never_exceed_alpha() {
        let mut s = IconBitmap::new(8, 8);
        for (i, p) in s.pixels.iter_mut().enumerate() {
            *p = premultiply_px(0xFF00_0000 | ((i as u32 * 7919) & 0xFF_FFFF) | (((i as u32 * 37) & 0xFF) << 24));
        }
        let r = resize_area(&s, 5);
        for p in r.pixels {
            let a = p >> 24;
            assert!((p >> 16) & 255 <= a && (p >> 8) & 255 <= a && p & 255 <= a);
        }
    }

    #[test]
    fn zero_size_is_empty() {
        assert!(resize_area(&solid(4, 4, 0xFFFF_FFFF), 0).is_empty());
    }

    #[test]
    fn premultiply_roundtrip() {
        let p = 0x80FF_8000;
        let q = unpremultiply_px(premultiply_px(p));
        assert_eq!(q >> 24, 0x80);
        assert!(((q >> 16) & 255).abs_diff(0xFF) <= 2 && ((q >> 8) & 255).abs_diff(0x80) <= 2);
    }
}
