//! The popup panel background (moved from surface.rs by W3).

use super::pixel::{argb, over, rr_cov, white};

/// What `panel_px` needs besides the position.
#[derive(Clone, Copy)]
struct PanelSpec {
    w: i32,
    h: i32,
    radius: i32,
    bw: i32,
    rgb: (u32, u32, u32),
    bg_alpha: u32,
}

/// One panel pixel: the exact formula (two rounded-rectangle coverages, the border over the tone).
fn panel_px(x: i32, y: i32, p: &PanelSpec) -> u32 {
    let PanelSpec { w, h, radius, bw, rgb, bg_alpha } = *p;
    let outer = rr_cov(x, y, 0, 0, w, h, radius);
    if outer <= 0.0 {
        return 0;
    }
    let inner = rr_cov(x, y, bw, bw, w - bw, h - bw, radius - bw);
    let base = argb((bg_alpha as f32 * outer + 0.5) as u32, rgb.0, rgb.1, rgb.2);
    let ring = (outer - inner).max(0.0);
    let ba = (crate::tokens::RIM_SURFACE as f32 * ring + 0.5) as u32;
    over(base, white(ba))
}

/// The panel background of a `w` x `h` buffer, pixel for pixel what evaluating the formula everywhere gives, but the
/// exact value is computed only where it varies: with `k = max(radius, bw) + 1`, the `k` columns at each side of a
/// row; the columns between repeat the value at column `k`; the rows clear of both corner bands repeat one row.
/// A buffer smaller than `2k + 1` in either direction uses the exact formula everywhere.
pub fn panel_pixels(buf: &mut [u32], w: i32, h: i32, radius: i32, bw: i32, rgb: (u32, u32, u32), bg_alpha: u32) {
    if w <= 0 || h <= 0 || buf.len() < (w as usize) * (h as usize) {
        return;
    }
    let spec = PanelSpec { w, h, radius, bw, rgb, bg_alpha };
    let k = radius.max(bw) + 1;
    let wu = w as usize;
    if w < 2 * k + 1 || h < 2 * k + 1 {
        for y in 0..h {
            for x in 0..w {
                buf[(y * w + x) as usize] = panel_px(x, y, &spec);
            }
        }
        return;
    }
    let row = |buf: &mut [u32], y: i32| {
        let base = (y * w) as usize;
        for x in (0..k).chain(w - k..w) {
            buf[base + x as usize] = panel_px(x, y, &spec);
        }
        let mid = panel_px(k, y, &spec);
        if let Some(s) = buf.get_mut(base + k as usize..base + (w - k) as usize) {
            s.fill(mid);
        }
    };
    for y in (0..k).chain(h - k..h) {
        row(buf, y);
    }
    row(buf, k);
    let first = (k as usize) * wu;
    for y in k + 1..h - k {
        buf.copy_within(first..first + wu, (y as usize) * wu);
    }
}

/// Today's loop, kept to prove `panel_pixels` equal to it.
#[cfg(test)]
fn panel_pixels_reference(buf: &mut [u32], w: i32, h: i32, radius: i32, bw: i32, rgb: (u32, u32, u32), bg_alpha: u32) {
    let spec = PanelSpec { w, h, radius, bw, rgb, bg_alpha };
    for y in 0..h {
        for x in 0..w {
            buf[(y * w + x) as usize] = panel_px(x, y, &spec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::pixel::RR_COV_CALLS;
    use crate::gfx::scale::Scale;

    /// Corner radius of the dock's panel (logical px), as the dock sets it.
    const PANEL_RADIUS: i32 = 14;

    #[test]
    fn panel_spans_equal_reference() {
        for (w, h) in [(1, 1), (2, 2), (10, 3), (640, 62), (800, 528), (960, 634)] {
            for dpi in [96, 120, 144, 192] {
                let m = Scale { dpi };
                let (radius, bw) = (m.px(PANEL_RADIUS), 1.max(m.px(1)));
                for alpha in [1, 128, 255] {
                    let mut a = vec![0xDEAD_BEEFu32; (w * h) as usize];
                    let mut b = a.clone();
                    panel_pixels(&mut a, w, h, radius, bw, (7, 9, 11), alpha);
                    panel_pixels_reference(&mut b, w, h, radius, bw, (7, 9, 11), alpha);
                    let diff = a.iter().zip(&b).filter(|(x, y)| x != y).count();
                    assert_eq!(diff, 0, "{w}x{h} dpi {dpi} alpha {alpha}");
                }
            }
        }
    }

    #[test]
    fn panel_coverage_evaluations_bounded() {
        let m = Scale { dpi: 120 };
        let mut buf = vec![0u32; 800 * 528];
        RR_COV_CALLS.with(|c| c.set(0));
        panel_pixels(&mut buf, 800, 528, m.px(PANEL_RADIUS), 1.max(m.px(1)), (0, 0, 0), 255);
        let n = RR_COV_CALLS.with(|c| c.get());
        assert!(n <= 8_000, "{n} rr_cov calls");
    }
}
