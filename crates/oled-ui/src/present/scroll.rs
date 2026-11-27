//! Retained scroll (K2a.10, KC2): Effect::Scroll moves the viewport's pixels by dy and only the exposed strip is painted.
//! An opaque window moves the pixels its DWM redirection surface holds with ScrollDC on the window DC and invalidates the
//! strip, which WM_PAINT paints in bands; a layered window moves the rows of its presenter's DIB (Canvas::scroll) and
//! damages the strip. When damage is pending inside the viewport (its pixels are stale) or |dy| reaches the viewport's
//! height, the whole viewport is repainted instead. The Harness emulates the move on its last image.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetDC, GetUpdateRect, ReleaseDC};
use windows::Win32::UI::WindowsAndMessaging::ScrollDC;

use crate::geom::RectP;
use crate::harness::Image;
use crate::host::window::{Presenter, WinHost};
use crate::w3::Canvas;

/// What a scroll repaints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollPlan {
    /// Nothing moves: dy is 0 or the viewport is empty.
    Nothing,
    /// The pixels move by dy and the exposed strip is painted.
    Move { strip: RectP },
    /// The whole viewport is painted.
    Full,
}

/// The rule: damage pending inside the viewport, or |dy| at least its height, repaints it whole; otherwise the pixels
/// move and the strip they leave is painted: the |dy| rows at the bottom when the content went up (dy < 0), at the top
/// when it went down.
pub(crate) fn plan(viewport: RectP, dy: i32, pending: &[RectP]) -> ScrollPlan {
    if viewport.is_empty() || dy == 0 {
        return ScrollPlan::Nothing;
    }
    if i64::from(dy).abs() >= i64::from(viewport.h()) || pending.iter().any(|r| !r.intersect(viewport).is_empty()) {
        return ScrollPlan::Full;
    }
    let strip =
        if dy < 0 { RectP { t: viewport.b + dy, ..viewport } } else { RectP { b: viewport.t + dy, ..viewport } };
    ScrollPlan::Move { strip }
}

impl Canvas<'_> {
    /// Moves the pixels inside `r` by `dy` rows (down for dy > 0), each row's span copied whole; the rows the move
    /// exposes keep their old pixels for the caller to paint. Clipped to the canvas; nothing moves when |dy| reaches
    /// the height.
    pub(crate) fn scroll(&mut self, r: RectP, dy: i32) {
        let r = r.intersect(RectP::wh(0, 0, self.w, self.h));
        if r.is_empty() || dy == 0 || i64::from(dy).abs() >= i64::from(r.h()) {
            return;
        }
        let w = self.w as usize;
        let (l, right) = (r.l as usize, r.r as usize);
        let mut copy = |y: i32| {
            let (dst, src) = (y as usize * w, (y - dy) as usize * w);
            self.buf.copy_within(src + l..src + right, dst + l);
        };
        if dy < 0 {
            // The content goes up: top to bottom, each row from the one |dy| below it.
            (r.t..r.b + dy).for_each(&mut copy);
        } else {
            // The content goes down: bottom to top.
            (r.t + dy..r.b).rev().for_each(&mut copy);
        }
    }
}

/// The bounding box of the window's update region (rects invalidated and not painted yet); None when it is empty.
fn update_rect(hwnd: HWND) -> Option<RectP> {
    let mut rc = RECT::default();
    // SAFETY: our window (or a null handle, which fails); `rc` is writable; with bErase false it sends no message.
    let some = unsafe { GetUpdateRect(hwnd, Some(&mut rc), false) }.as_bool();
    some.then(|| RectP::from(rc))
}

/// ScrollDC on the window DC: the retained pixels of `r` move by dy, clipped to `r`. False when it did not happen.
fn scroll_dc(hwnd: HWND, r: RectP, dy: i32) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    // SAFETY: the window's own DC, released below; GetDC sends no message.
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.is_invalid() {
        return false;
    }
    let rc = RECT::from(r);
    // SAFETY: a DC of our window; both rects live for the call; no update region or rect is asked for. ScrollDC is a GDI
    // copy inside the DC and sends no message.
    let ok = unsafe { ScrollDC(hdc, 0, dy, Some(&rc), Some(&rc), None, None) }.is_ok();
    // SAFETY: the DC GetDC gave for this window.
    unsafe { ReleaseDC(Some(hwnd), hdc) };
    ok
}

/// Effect::Scroll in a window: the plan's move, or the whole viewport when the pixels could not move.
pub(crate) fn apply(host: &mut WinHost, viewport: RectP, dy: i32) {
    let viewport = viewport.intersect(host.client_rect());
    let mut pending: Vec<RectP> = host.damage.rects().to_vec();
    if matches!(host.presenter, Presenter::Banded(_)) {
        pending.extend(update_rect(host.hwnd));
    }
    match plan(viewport, dy, &pending) {
        ScrollPlan::Nothing => {}
        ScrollPlan::Full => host.add_damage(viewport),
        ScrollPlan::Move { strip } => {
            let moved = match &mut host.presenter {
                Presenter::Layered(l) => {
                    let mut c = l.canvas();
                    let fits = c.w >= viewport.r && c.h >= viewport.b;
                    if fits {
                        c.scroll(viewport, dy);
                    }
                    fits
                }
                Presenter::Banded(_) => scroll_dc(host.hwnd, viewport, dy),
            };
            host.add_damage(if moved { strip } else { viewport });
        }
    }
}

/// Effect::Scroll in the Harness: the image's viewport pixels move as a window's would; true when they did.
pub(crate) fn emulate(img: &mut Image, viewport: RectP, dy: i32) -> bool {
    let (w, h) = (img.w as i32, img.h as i32);
    if !matches!(plan(viewport.intersect(RectP::wh(0, 0, w, h)), dy, &[]), ScrollPlan::Move { .. }) {
        return false;
    }
    Canvas { buf: &mut img.px, w, h, clip: None }.scroll(viewport, dy);
    true
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx, PaintCx};
    use crate::dispatch::Input;
    use crate::event::{Event, Mods, Reply, WheelDelta};
    use crate::geom::{PointP, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::model::{
        Align, CellKind, CellOut, ColId, Column, ListModel, RowKey, SelectionMode, SortDir, TableModel, TableStyle,
    };
    use crate::node::WidgetId;
    use crate::screen::{Screen, Widget};
    use crate::theme::Tokens;
    use crate::widgets::VirtualTable;

    #[test]
    fn scroll_plan_rules() {
        let vp = RectP::wh(0, 40, 300, 200);
        assert_eq!(
            plan(vp, -30, &[]),
            ScrollPlan::Move { strip: RectP { l: 0, t: 210, r: 300, b: 240 } },
            "up: the bottom"
        );
        assert_eq!(
            plan(vp, 25, &[]),
            ScrollPlan::Move { strip: RectP { l: 0, t: 40, r: 300, b: 65 } },
            "down: the top"
        );
        assert_eq!(plan(vp, 200, &[]), ScrollPlan::Full, "a whole viewport or more");
        assert_eq!(plan(vp, -600, &[]), ScrollPlan::Full);
        assert_eq!(plan(vp, -30, &[RectP::wh(10, 100, 20, 20)]), ScrollPlan::Full, "stale pixels inside");
        let outside = [RectP::wh(0, 0, 300, 40), RectP::wh(0, 240, 300, 10)];
        assert!(
            matches!(plan(vp, -30, &outside), ScrollPlan::Move { .. }),
            "damage outside the viewport does not matter"
        );
        assert_eq!(plan(vp, 0, &[]), ScrollPlan::Nothing);
        assert_eq!(plan(RectP::EMPTY, 10, &[]), ScrollPlan::Nothing);
    }

    #[test]
    fn canvas_scroll_moves_rows_inside_the_rect_only() {
        let (w, h) = (6, 8);
        let px = |x: i32, y: i32| (y * 10 + x) as u32;
        let mut buf: Vec<u32> = (0..h).flat_map(|y| (0..w).map(move |x| px(x, y))).collect();
        let r = RectP { l: 1, t: 2, r: 5, b: 7 };
        Canvas { buf: &mut buf, w, h, clip: None }.scroll(r, -2);
        for y in 0..h {
            for x in 0..w {
                let want = if r.contains(PointP { x, y }) && y < 5 { px(x, y + 2) } else { px(x, y) };
                assert_eq!(buf[(y * w + x) as usize], want, "({x}, {y}) up");
            }
        }
        let mut buf: Vec<u32> = (0..h).flat_map(|y| (0..w).map(move |x| px(x, y))).collect();
        Canvas { buf: &mut buf, w, h, clip: None }.scroll(r, 3);
        for y in 0..h {
            for x in 0..w {
                let want = if r.contains(PointP { x, y }) && y >= 5 { px(x, y - 3) } else { px(x, y) };
                assert_eq!(buf[(y * w + x) as usize], want, "({x}, {y}) down");
            }
        }
        let before = buf.clone();
        Canvas { buf: &mut buf, w, h, clip: None }.scroll(r, 5);
        assert_eq!(buf, before, "|dy| = the height: nothing moves");
    }

    /// A hundred thousand files without storage: name, size and kind, as the table100k page shows them.
    struct Files;

    impl ListModel for Files {
        fn len(&self) -> usize {
            100_000
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(row as u64)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            Some(key.0 as usize)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Owned(format!("item {row}.txt"))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Files {
        fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
            match col.0 {
                0 => out.set_text(CellKind::Text, &format!("item {row}.txt")),
                1 => out.set_text(CellKind::Bytes, &format!("{} KB", (row * 37) % 9000)),
                _ => out.set_text(CellKind::Text, if row.is_multiple_of(3) { "Text document" } else { "Picture" }),
            };
        }
    }

    struct Sc(VirtualTable<Files>, CommandTable);

    impl Screen for Sc {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.0.layout(cx, RectP::wh(cx.px(16), cx.px(16), cx.px(688), cx.px(408)));
            avail
        }
        fn paint(&self, cx: &mut PaintCx) {
            self.0.paint(cx);
        }
        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            self.0.event(cx, ev);
            Reply::None
        }
        fn commands(&self) -> &CommandTable {
            &self.1
        }
    }

    fn table100k(dpi: u32) -> Harness<Sc> {
        let col = |id: u16, title: &'static str, width: i32, flex: bool| Column {
            id: ColId(id),
            title: Cow::Borrowed(title),
            width,
            min: 48,
            flex,
            align: Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable: true,
            tip: None,
        };
        let cols = vec![col(0, "Name", 300, true), col(1, "Size", 100, false), col(2, "Type", 160, false)];
        let t = VirtualTable::new(WidgetId(1), Files, cols, SelectionMode::Multi, TableStyle::FILES);
        Harness::new(Sc(t, CommandTable::new()), SizeL { w: 720, h: 440 }, dpi, Tokens::oled())
    }

    /// The table's offset: where row r is drawn tells it (rows are uniform).
    fn offset(h: &Harness<Sc>) -> i64 {
        let t = &h.screen().0;
        let r = t.visible_rows().start;
        let plate = t.row_rect(r).unwrap();
        i64::from(t.viewport().t) + r as i64 * i64::from(plate.h()) - i64::from(plate.t)
    }

    #[test]
    fn retained_scroll_equals_full_repaint() {
        for dpi in [96, 144] {
            let mut h = table100k(dpi);
            let vp = h.screen().0.viewport();
            let inside = PointP { x: vp.l + 40, y: vp.t + 40 };
            // Start in the middle of the hundred thousand rows.
            h.input(Input::Wheel(inside, WheelDelta::Pixels(-1_000_000.0), false, Mods::NONE));
            let mut old = h.render();
            let mut seed: u32 = 0x2545_F491 ^ dpi;
            let mut moves = 0;
            for _ in 0..200 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let dy = (seed >> 8) as i32 % 1200 - 600;
                let before = offset(&h);
                h.input(Input::Wheel(inside, WheelDelta::Pixels(dy as f32), false, Mods::NONE));
                let moved = (before - offset(&h)) as i32;
                assert_eq!(moved, dy, "the table moved by what was asked");
                let new = h.render();
                // What a window keeps: the old pixels moved, then the strip (or the whole viewport) painted.
                let mut kept = Image { w: old.w, h: old.h, px: old.px.clone(), text_rects: Vec::new() };
                let fresh = match plan(vp, moved, &[]) {
                    ScrollPlan::Move { strip } => {
                        assert!(emulate(&mut kept, vp, moved));
                        moves += 1;
                        strip
                    }
                    ScrollPlan::Full => vp,
                    ScrollPlan::Nothing => RectP::EMPTY,
                };
                for y in fresh.t..fresh.b {
                    let row = (y as u32 * new.w) as usize;
                    let (a, b) = (row + fresh.l as usize, row + fresh.r as usize);
                    kept.px[a..b].copy_from_slice(&new.px[a..b]);
                }
                assert!(kept.px == new.px, "dpi {dpi}, dy {moved}: the retained pixels differ from a full repaint");
                old = new;
            }
            assert!(moves > 100, "most moves were retained ({moves} of 200)");
            assert!(h.is_idle());
        }
    }

    #[test]
    fn a_layered_window_moves_its_dib_and_paints_the_strip() {
        use crate::host::{AppId, EventLoop};
        use crate::present::layered::LayeredPresenter;
        use crate::strings::Lang;
        use crate::theme::ThemeSpec;
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let col = |id: u16, title: &'static str| Column {
            id: ColId(id),
            title: Cow::Borrowed(title),
            width: 200,
            min: 48,
            flex: true,
            align: Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable: true,
            tip: None,
        };
        let t = VirtualTable::new(WidgetId(1), Files, vec![col(0, "Name")], SelectionMode::Single, TableStyle::FILES);
        let screen = Box::new(Sc(t, CommandTable::new()));
        let mut host = WinHost::headless(lp.shared(), screen, 96, SizeP { w: 720, h: 440 });
        host.presenter = Presenter::Layered(LayeredPresenter::new());
        host.layout();
        host.redraw();
        host.flush_damage();
        host.deferred.clear();
        // The table at (16, 16, 688, 408) under its 28 px header, at 96 DPI.
        let vp = RectP::wh(16, 44, 688, 380);
        let canvas = |host: &mut WinHost| match &mut host.presenter {
            Presenter::Layered(l) => l.canvas().buf.to_vec(),
            Presenter::Banded(_) => Vec::new(),
        };
        let before = canvas(&mut host);
        apply(&mut host, vp, -56);
        assert_eq!(host.damage.rects(), [RectP { t: vp.b - 56, ..vp }], "only the strip is damaged");
        let after = canvas(&mut host);
        let row = |buf: &[u32], y: i32| buf[(y * 720 + vp.l) as usize..(y * 720 + vp.r) as usize].to_vec();
        assert_eq!(row(&after, vp.t + 3), row(&before, vp.t + 59), "the rows moved up 56 px in the DIB");
        // Damage pending inside the viewport: the whole viewport instead.
        host.damage = crate::present::Damage::default();
        host.add_damage(RectP::wh(vp.l + 10, vp.t + 10, 20, 20));
        apply(&mut host, vp, -56);
        assert_eq!(host.damage.bbox(), vp);
        assert!(host.deferred.is_empty(), "no window call");
    }
}
