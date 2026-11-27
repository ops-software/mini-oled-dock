//! The Harness's image (P2-kit 2.5): premultiplied pixels and the text rects the golden comparator masks.

use crate::geom::RectP;

/// A rendered window: w x h premultiplied 0xAARRGGBB pixels and the rects text was drawn in.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
    pub text_rects: Vec<RectP>,
}

impl Image {
    /// Writes a PNG through gfx::wic. Filled by K1a.5 (with the COM rule of golden::check); until then Err.
    pub fn write_png(&self, path: &std::path::Path) -> Result<(), String> {
        let _ = path;
        Err(format!("write_png of a {} x {} image: not ready, K1a.5", self.w, self.h))
    }

    /// The part inside `r` (clipped to the image), with the text rects that meet it, moved to its origin.
    pub fn crop(&self, r: RectP) -> Image {
        let r = r.intersect(RectP { l: 0, t: 0, r: self.w as i32, b: self.h as i32 });
        let (w, h) = (r.w().max(0) as u32, r.h().max(0) as u32);
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in r.t..r.b {
            let row = (y as u32 * self.w) as usize;
            px.extend_from_slice(&self.px[row + r.l as usize..row + r.r as usize]);
        }
        let text_rects = self
            .text_rects
            .iter()
            .map(|t| t.intersect(r))
            .filter(|t| !t.is_empty())
            .map(|t| t.offset(-r.l, -r.t))
            .collect();
        Image { w, h, px, text_rects }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_clips_and_moves() {
        let img = Image { w: 4, h: 3, px: (0..12).collect(), text_rects: vec![RectP::wh(1, 1, 2, 2)] };
        let c = img.crop(RectP::wh(1, 1, 10, 10));
        assert_eq!((c.w, c.h), (3, 2));
        assert_eq!(c.px, vec![5, 6, 7, 9, 10, 11]);
        assert_eq!(c.text_rects, vec![RectP::wh(0, 0, 2, 2)]);
        assert_eq!(img.crop(RectP::wh(9, 9, 1, 1)).px.len(), 0);
    }
}
