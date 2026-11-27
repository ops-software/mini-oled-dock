//! The popups' scroll area: the dock's wrapper around oled_ui::scroll::ScrollModel. The model keeps the
//! numbers; this type adds the two things that need the dock: the popup's early tick after a scroll and the
//! paint on the popup Surface.

use crate::popup::tick_soon;
use crate::surface::{Surface, WHITE};

pub(crate) struct ScrollArea {
    model: oled_ui::scroll::ScrollModel,
}

impl std::ops::Deref for ScrollArea {
    type Target = oled_ui::scroll::ScrollModel;
    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

impl std::ops::DerefMut for ScrollArea {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.model
    }
}

impl ScrollArea {
    pub(crate) fn new() -> ScrollArea {
        ScrollArea { model: oled_ui::scroll::ScrollModel::new() }
    }

    /// As before: moves, shows the bar and asks the popup's tick to hide it on time.
    pub(crate) fn by(&mut self, delta: i32, now: u64) -> bool {
        let moved = self.model.by(delta, now);
        tick_soon(oled_ui::scroll::SCROLLBAR_HIDE_MS as u32 + 16);
        moved
    }

    pub(crate) fn paint(&self, s: &mut Surface, now: u64) {
        if let Some((t, a)) = self.model.paint_rect(now) {
            s.fill_rr(t, (t.right - t.left) / 2, WHITE, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Metrics;
    use windows::Win32::Foundation::RECT;

    #[test]
    fn wrapper_paints_what_the_model_returns() {
        let mut s = Surface::new(Metrics { dpi: 96 }).expect("surface");
        s.resize(300, 200).unwrap();
        s.fill_rr(crate::surface::rect_wh(0, 0, 300, 200), 0, (0, 0, 0), 255);
        let mut a = ScrollArea::new();
        a.set(Metrics { dpi: 96 }, RECT { left: 0, top: 0, right: 300, bottom: 200 }, 500, 0);
        assert!(a.scrollable());
        let t = a.thumb();
        a.paint(&mut s, 10);
        let (w, _, px) = s.snapshot_pixels().unwrap();
        let (x, y) = ((t.left + t.right) / 2, (t.top + t.bottom) / 2);
        let p = px[(y * w + x) as usize];
        assert_eq!(p >> 24, 0xFF);
        for sh in [0, 8, 16] {
            assert!(((p >> sh) & 255).abs_diff(50) <= 1, "channel at {sh}: {p:#x}");
        }
        // Past the hide time nothing is drawn: the pixel stays black.
        s.fill_rr(crate::surface::rect_wh(0, 0, 300, 200), 0, (0, 0, 0), 255);
        a.paint(&mut s, 5_000);
        assert_eq!(s.snapshot_pixels().unwrap().2[(y * w + x) as usize], 0xFF00_0000);
    }
}
