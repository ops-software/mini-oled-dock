//! ScrollBar (F-K1, P2-kit 2.6): the frozen type; filled by K1b.3 (on W3's ScrollModel, with its own hide timer).

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::WidgetId;
use crate::screen::Widget;

/// What a scrollbar tells its screen: the new offset, px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollOut {
    Scrolled(i64),
}

/// A thin overlay scrollbar.
#[allow(dead_code)] // The fields are read by K1b.3.
pub struct ScrollBar {
    id: WidgetId,
    viewport: RectP,
    content_h: i64,
    offset: i64,
}

impl ScrollBar {
    pub fn new(id: WidgetId) -> ScrollBar {
        ScrollBar { id, viewport: RectP::EMPTY, content_h: 0, offset: 0 }
    }

    /// The viewport, the content height and the offset, physical px.
    pub fn set(&mut self, viewport: RectP, content_h: i64, offset: i64) {
        self.viewport = viewport;
        self.content_h = content_h;
        self.offset = offset;
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }
}

impl Widget for ScrollBar {
    type Out = ScrollOut;

    /// Filled by K1b.3; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.3; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.3; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ScrollOut> {
        let _ = (cx, ev);
        None
    }
}
