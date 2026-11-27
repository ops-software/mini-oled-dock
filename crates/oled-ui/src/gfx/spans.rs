//! The column-span graph (PaintCx::spans, P2-kit 2.13, K4.1): one vertical run per physical column, the primitive the
//! charts draw their strokes and their area fill with.

use windows::Win32::Foundation::RECT;

use super::canvas::Canvas;
use super::pixel::{argb, over};
use crate::cx::PaintCx;
use crate::theme::Ink;

impl Canvas<'_> {
    /// Fills the run `(top, bottom)` (half-open rows, `dy` added to both) of each column from `x0` with the
    /// premultiplied `src`, inside the buffer and the clip. An empty or reversed run draws nothing.
    pub(crate) fn spans(&mut self, x0: i32, dy: i32, spans: &[(i16, i16)], src: u32) {
        let (mut cl, mut ct, mut cr, mut cb) = (0, 0, self.w, self.h);
        if let Some(RECT { left, top, right, bottom }) = self.clip {
            (cl, ct, cr, cb) = (cl.max(left), ct.max(top), cr.min(right), cb.min(bottom));
        }
        for (i, &(top, bottom)) in spans.iter().enumerate() {
            let x = x0 + i as i32;
            if x < cl || x >= cr {
                continue;
            }
            let (y0, y1) = ((i32::from(top) + dy).max(ct), (i32::from(bottom) + dy).min(cb));
            for y in y0..y1 {
                let at = (y * self.w + x) as usize;
                self.buf[at] = over(self.buf[at], src);
            }
        }
    }
}

/// Draws one vertical run (top, bottom) per column from x0, in window coordinates, in `ink`.
pub(crate) fn draw(cx: &mut PaintCx<'_>, x0: i32, spans: &[(i16, i16)], ink: Ink) {
    if ink.a == 0 || spans.is_empty() {
        return;
    }
    cx.sync_clip();
    let src = argb(ink.a, ink.rgb.0, ink.rgb.1, ink.rgb.2);
    let (ox, oy) = (cx.origin.x, cx.origin.y);
    cx.canvas.spans(x0 - ox, -oy, spans, src);
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xFFFF_0000;

    fn canvas(buf: &mut [u32], w: i32, h: i32) -> Canvas<'_> {
        Canvas { buf, w, h, clip: None }
    }

    #[test]
    fn a_run_fills_its_rows_in_each_column() {
        let mut buf = vec![0u32; 5 * 4];
        canvas(&mut buf, 5, 4).spans(1, 0, &[(1, 3), (0, 4), (2, 2)], RED);
        let col = |x: usize| (0..4).map(|y| buf[y * 5 + x] == RED).collect::<Vec<_>>();
        assert_eq!(col(0), [false; 4]);
        assert_eq!(col(1), [false, true, true, false]);
        assert_eq!(col(2), [true; 4]);
        assert_eq!(col(3), [false; 4], "an empty run draws nothing");
    }

    #[test]
    fn runs_stay_inside_the_buffer_and_the_clip() {
        let mut buf = vec![0u32; 4 * 4];
        let mut cv = canvas(&mut buf, 4, 4);
        cv.clip = Some(RECT { left: 1, top: 1, right: 3, bottom: 3 });
        cv.spans(-1, 0, &[(-5, 9), (-5, 9), (-5, 9), (-5, 9), (-5, 9), (-5, 9)], RED);
        let set: Vec<(usize, usize)> = (0..16).filter(|i| buf[*i] == RED).map(|i| (i % 4, i / 4)).collect();
        assert_eq!(set, [(1, 1), (2, 1), (1, 2), (2, 2)]);
    }

    #[test]
    fn the_row_shift_moves_window_rows_into_the_band() {
        let mut buf = vec![0u32; 2 * 4];
        canvas(&mut buf, 2, 4).spans(0, -10, &[(11, 13)], RED);
        assert_eq!([buf[2], buf[4], buf[0], buf[6]], [RED, RED, 0, 0]);
    }

    #[test]
    fn a_translucent_run_blends_over_what_is_there() {
        let mut buf = vec![0xFF00_0000u32; 2];
        canvas(&mut buf, 2, 1).spans(0, 0, &[(0, 1)], argb(24, 255, 255, 255));
        assert_eq!(buf[0], over(0xFF00_0000, argb(24, 255, 255, 255)));
        assert_ne!(buf[0], 0xFF00_0000);
        assert_eq!(buf[1], 0xFF00_0000);
    }
}
