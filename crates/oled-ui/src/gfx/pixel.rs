//! Pixel helpers: blend maths, rounded-rectangle coverage, rectangles (moved from surface.rs by W3).

use super::image::IconBitmap;
#[cfg(test)]
use std::cell::Cell;
use windows::Win32::Foundation::RECT;

/// `x / 255` rounded, exact for `x <= 255 * 255`.
#[inline]
pub fn div255(x: u32) -> u32 {
    (x + 128 + ((x + 128) >> 8)) >> 8
}

/// Premultiplied 0xAARRGGBB pixel from a non-premultiplied color and alpha.
#[inline]
pub fn argb(a: u32, r: u32, g: u32, b: u32) -> u32 {
    (a << 24) | (div255(r * a) << 16) | (div255(g * a) << 8) | div255(b * a)
}

/// White with alpha `a`, premultiplied.
#[inline]
pub fn white(a: u32) -> u32 {
    argb(a, 255, 255, 255)
}

/// "Source over" composition of `src` onto `dst`, both premultiplied.
#[inline]
pub fn over(dst: u32, src: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    let ch = |s: u32, d: u32| (s + div255(d * inv)).min(255);
    (ch(src >> 24, dst >> 24) << 24)
        | (ch((src >> 16) & 255, (dst >> 16) & 255) << 16)
        | (ch((src >> 8) & 255, (dst >> 8) & 255) << 8)
        | ch(src & 255, dst & 255)
}

/// The radius `rr_cov` actually uses for the rectangle `[l, r) x [t, b)`.
#[inline]
pub(super) fn eff_radius(l: i32, t: i32, r: i32, b: i32, rad: i32) -> i32 {
    rad.min((r - l) / 2).min((b - t) / 2).max(0)
}

/// Coverage (0..1) of pixel `(x, y)` with respect to the rounded rectangle `[l, r) x [t, b)` with
/// radius `rad`: manual antialiasing at the corners only (the sides are pixel-aligned).
pub fn rr_cov(x: i32, y: i32, l: i32, t: i32, r: i32, b: i32, rad: i32) -> f32 {
    #[cfg(test)]
    RR_COV_CALLS.with(|c| c.set(c.get() + 1));
    if x < l || x >= r || y < t || y >= b {
        return 0.0;
    }
    let rad = rad.min((r - l) / 2).min((b - t) / 2).max(0);
    let cx = if x < l + rad {
        l + rad
    } else if x >= r - rad {
        r - rad
    } else {
        return 1.0;
    };
    let cy = if y < t + rad {
        t + rad
    } else if y >= b - rad {
        b - rad
    } else {
        return 1.0;
    };
    let dx = (x as f32 + 0.5) - cx as f32;
    let dy = (y as f32 + 0.5) - cy as f32;
    (rad as f32 + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
}

// Number of `rr_cov` calls on this thread (the evaluation-count gates).
#[cfg(test)]
thread_local! {
    pub(crate) static RR_COV_CALLS: Cell<u64> = const { Cell::new(0) };
}

/// Average (premultiplied) of the source pixels covered by destination pixel `(dx, dy)` of a
/// `size` x `size` image drawn from `bmp`.
pub fn area_sample(bmp: &IconBitmap, dx: i32, dy: i32, size: i32) -> u32 {
    let (sw, sh) = (bmp.width as i32, bmp.height as i32);
    let (x0, x1) = (dx * sw / size, ((dx + 1) * sw / size).max(dx * sw / size + 1).min(sw));
    let (y0, y1) = (dy * sh / size, ((dy + 1) * sh / size).max(dy * sh / size + 1).min(sh));
    let mut acc = [0u32; 4];
    for yy in y0..y1 {
        for xx in x0..x1 {
            let p = bmp.pixels[(yy * sw + xx) as usize];
            for (k, a) in acc.iter_mut().enumerate() {
                *a += (p >> (8 * k)) & 255;
            }
        }
    }
    let n = (((x1 - x0) * (y1 - y0)).max(1)) as u32;
    acc.iter().enumerate().fold(0, |out, (k, a)| out | (((a + n / 2) / n).min(255) << (8 * k)))
}

/// `RECT` from origin and size.
pub fn rect_wh(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// true if `(x, y)` is inside `rc`.
pub fn in_rect(rc: &RECT, x: i32, y: i32) -> bool {
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}
