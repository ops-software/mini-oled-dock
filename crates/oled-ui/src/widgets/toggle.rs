//! Toggle (F-K1, P2-kit 2.6): the frozen type; filled by K1b.1 (the dock's pixels, R1).

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// What a toggle tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleOut {
    Changed(bool),
}

/// An on/off switch with its label.
#[allow(dead_code)] // The fields are read by K1b.1.
pub struct Toggle {
    id: WidgetId,
    label: Text,
    on: bool,
    enabled: bool,
    busy: bool,
}

impl Toggle {
    pub fn new(id: WidgetId, label: Text, on: bool) -> Toggle {
        Toggle { id, label, on, enabled: true, busy: false }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn set_on(&mut self, v: bool) {
        self.on = v;
    }

    pub fn set_enabled(&mut self, v: bool) {
        self.enabled = v;
    }

    pub fn set_busy(&mut self, v: bool) {
        self.busy = v;
    }
}

impl Widget for Toggle {
    type Out = ToggleOut;

    /// Filled by K1b.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ToggleOut> {
        let _ = (cx, ev);
        None
    }
}
