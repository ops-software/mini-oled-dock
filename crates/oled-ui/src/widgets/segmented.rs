//! Segmented (F-K1, P2-kit 2.6): the frozen type; filled by K1b.1.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// What a segmented control tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegOut {
    Selected(usize),
}

/// A row of exclusive options.
#[allow(dead_code)] // The fields are read by K1b.1.
pub struct Segmented {
    id: WidgetId,
    label: Text,
    options: Vec<Text>,
    selected: Option<usize>,
}

impl Segmented {
    pub fn new(id: WidgetId, label: Text, options: Vec<Text>, selected: Option<usize>) -> Segmented {
        Segmented { id, label, options, selected }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_selected(&mut self, i: Option<usize>) {
        self.selected = i;
    }
}

impl Widget for Segmented {
    type Out = SegOut;

    /// Filled by K1b.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SegOut> {
        let _ = (cx, ev);
        None
    }
}
