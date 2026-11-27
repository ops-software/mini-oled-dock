//! Presenting a window's pixels (P2-kit 2.5): the pure part apps may read; the presenters are internal (K1a.9, K2a.10).

pub(crate) mod banded;
pub(crate) mod layered;
pub(crate) mod scroll;

use crate::geom::RectP;

/// Rows per paint band at a width in physical px: clamp(1 MiB / (4 * width), 32, 256), so a band DIB never exceeds
/// 1 MiB at any width.
pub fn band_height(width_px: i32) -> i32 {
    if width_px <= 0 {
        return 256;
    }
    ((1 << 20) / (4 * i64::from(width_px))).clamp(32, 256) as i32
}

/// What a window repaints (SPEC-platform 5.3.1): at most 8 rects, a new one merged into any it overlaps; a ninth, or a
/// total above 60 % of the client, collapses everything to the bounding box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Damage {
    rects: [RectP; Damage::MAX],
    n: u8,
}

impl Damage {
    pub(crate) const MAX: usize = 8;

    /// Adds `r`, clipped to `client`.
    pub(crate) fn add(&mut self, r: RectP, client: RectP) {
        let mut r = r.intersect(client);
        if r.is_empty() {
            return;
        }
        // A merged rect may now overlap another one: merge until it overlaps none.
        while let Some(i) = self.rects().iter().position(|x| !x.intersect(r).is_empty()) {
            r = r.union(self.rects[i]);
            self.remove(i);
        }
        if usize::from(self.n) == Self::MAX {
            r = self.bbox().union(r);
            self.n = 0;
        }
        self.rects[usize::from(self.n)] = r;
        self.n += 1;
        let total: i64 = self.rects().iter().map(RectP::area).sum();
        if self.n > 1 && total * 10 > client.area() * 6 {
            self.rects[0] = self.bbox();
            self.n = 1;
        }
    }

    /// The whole client.
    pub(crate) fn all(&mut self, client: RectP) {
        self.n = 0;
        self.add(client, client);
    }

    fn remove(&mut self, i: usize) {
        let n = usize::from(self.n);
        self.rects.copy_within(i + 1..n, i);
        self.n -= 1;
    }

    pub(crate) fn rects(&self) -> &[RectP] {
        &self.rects[..usize::from(self.n)]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The smallest rect holding every damaged one (EMPTY when there is none).
    pub(crate) fn bbox(&self) -> RectP {
        self.rects().iter().fold(RectP::EMPTY, |a, r| a.union(*r))
    }

    /// The rects, leaving it empty.
    pub(crate) fn take(&mut self) -> Damage {
        std::mem::take(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{Env, EventCx, LayoutCx, PaintCx, Res};
    use crate::dispatch::{Dispatcher, Effect, Input};
    use crate::event::{Event, Mods, Reply};
    use crate::geom::{PointP, Scale, SizeP};
    use crate::harness::FakeClipboard;
    use crate::host::Caption;
    use crate::node::{Focusable, Node, NodeId, Role, Text, WidgetId};
    use crate::screen::Screen;
    use crate::strings::UiLang;
    use crate::text::GdiTextEngine;
    use crate::theme::Tokens;

    #[test]
    fn bands_stay_under_one_mib() {
        assert_eq!((band_height(1920), band_height(3840), band_height(100), band_height(20000)), (136, 68, 256, 32));
        for w in [1, 640, 1366, 2560, 4096, 7680] {
            assert!(i64::from(band_height(w)) * i64::from(w) * 4 <= 1 << 20 || band_height(w) == 32);
        }
        assert_eq!(band_height(0), 256);
    }

    #[test]
    fn damage_merges_and_collapses() {
        let client = RectP::wh(0, 0, 1000, 1000);
        let cell = |i: i32| RectP::wh(10 + 120 * i, 10, 50, 50);
        let mut d = Damage::default();
        for i in 0..8 {
            d.add(cell(i), client);
        }
        assert_eq!(d.rects().len(), 8, "8 disjoint rects stay apart");
        assert_eq!(d.rects()[3], cell(3));
        d.add(RectP::wh(10, 500, 50, 50), client);
        assert_eq!(d.rects(), [RectP { l: 10, t: 10, r: 900, b: 550 }], "a ninth collapses to the bounding box");

        let mut d = Damage::default();
        d.add(RectP::wh(0, 0, 100, 100), client);
        d.add(RectP::wh(300, 0, 100, 100), client);
        d.add(RectP::wh(50, 50, 300, 20), client);
        assert_eq!(d.rects(), [RectP { l: 0, t: 0, r: 400, b: 100 }], "a bridge merges both");

        let mut d = Damage::default();
        d.add(RectP::wh(0, 0, 1000, 300), client);
        d.add(RectP::wh(0, 400, 1000, 300), client);
        assert_eq!(d.rects().len(), 2, "60 % stays apart");
        d.add(RectP::wh(0, 800, 100, 100), client);
        assert_eq!(d.rects(), [RectP { l: 0, t: 0, r: 1000, b: 900 }], "61 % collapses");

        let mut d = Damage::default();
        d.add(RectP::wh(-50, 990, 100, 100), client);
        assert_eq!(d.rects(), [RectP { l: 0, t: 990, r: 50, b: 1000 }], "clipped to the client");
        d.add(RectP::wh(2000, 0, 5, 5), client);
        assert_eq!(d.rects().len(), 1, "an outside rect adds nothing");
        assert_eq!(d.take().rects().len(), 1);
        assert!(d.is_empty() && d.bbox() == RectP::EMPTY);
        d.all(client);
        assert_eq!(d.rects(), [client]);
    }

    /// Twelve rows of a list, 30 px apart.
    struct Rows {
        commands: CommandTable,
    }

    fn row(i: i32) -> RectP {
        RectP::wh(0, 30 * i, 200, 30)
    }

    impl Screen for Rows {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            for i in 0..12 {
                let n = Node::new(NodeId::new(WidgetId(1), i as u32), Role::ListItem, row(i));
                cx.push(n.label(Text::from("row")).focus(Focusable::Roving));
            }
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            let _ = cx;
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            let _ = (cx, ev);
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.commands
        }
    }

    #[test]
    fn two_hovered_rows_damage_two_rects() {
        let tokens = Tokens::oled();
        let env = Env {
            scale: Scale { dpi: 96 },
            tokens: &tokens,
            lang: UiLang::EN,
            now_ms: 0,
            caption: Caption::System,
            caption_spike: false,
        };
        let mut screen = Rows { commands: CommandTable::new() };
        let mut disp = Dispatcher::new();
        let mut res = Res::new(Box::new(GdiTextEngine::new().unwrap()));
        let mut clip = FakeClipboard { text: None, files: false };
        let client = RectP::wh(0, 0, 200, 360);
        disp.layout(&mut screen, env, &mut res, SizeP { w: 200, h: 360 });
        let centre = |i: i32| PointP { x: 100, y: 30 * i + 15 };
        disp.input(&mut screen, Input::Move(centre(3), Mods::NONE), env, &mut res, &mut clip);
        disp.effects.clear();
        disp.input(&mut screen, Input::Move(centre(9), Mods::NONE), env, &mut res, &mut clip);
        let mut d = Damage::default();
        for e in std::mem::take(&mut disp.effects) {
            if let Effect::Damage(r) = e {
                d.add(r, client);
            }
        }
        let g = env.scale.px(tokens.size.focus_gap) + env.scale.ring();
        assert_eq!(d.rects(), [row(3).inset(-g, -g).intersect(client), row(9).inset(-g, -g).intersect(client)]);
    }
}
