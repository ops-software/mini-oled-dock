//! SplitView (F-K3, P2-kit 2.10): the frozen types; filled by K3.2.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::WidgetId;
use crate::screen::Widget;

/// What a split view tells its screen: the first pane's new width (logical px), or a reset to the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOut {
    Moved(i32),
    Reset,
}

/// Two panes side by side with a splitter.
#[allow(dead_code)] // The fields are read by K3.2.
pub struct SplitView {
    id: WidgetId,
    min: i32,
    max: i32,
    default: i32,
    first: i32,
}

impl SplitView {
    /// min, max and default: the first pane's width, logical px.
    pub fn new(id: WidgetId, min: i32, max: i32, default: i32) -> SplitView {
        SplitView { id, min, max, default, first: default.clamp(min, max.max(min)) }
    }

    /// The first pane's width, logical px.
    pub fn first(&self) -> i32 {
        self.first
    }

    /// Sets it, kept within min..=max.
    pub fn set_first(&mut self, w: i32) {
        self.first = w.clamp(self.min, self.max.max(self.min));
    }

    /// The two panes inside `rect` (physical px). Filled by K3.2; until then the whole rect and an empty one.
    pub fn panes(&self, rect: RectP) -> (RectP, RectP) {
        (rect, RectP::EMPTY)
    }
}

impl Widget for SplitView {
    type Out = SplitOut;

    /// Filled by K3.2; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K3.2; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K3.2; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SplitOut> {
        let _ = (cx, ev);
        None
    }
}
