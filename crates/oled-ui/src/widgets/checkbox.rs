//! Checkbox (F-K1, P2-kit 2.6): the frozen type; filled by K1b.1.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// What a checkbox tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOut {
    Changed(bool),
}

/// A checkbox with its label; mixed shows the third state.
#[allow(dead_code)] // The fields are read by K1b.1.
pub struct Checkbox {
    id: WidgetId,
    label: Text,
    checked: bool,
    mixed: bool,
    enabled: bool,
}

impl Checkbox {
    pub fn new(id: WidgetId, label: Text, checked: bool) -> Checkbox {
        Checkbox { id, label, checked, mixed: false, enabled: true }
    }

    pub fn set_mixed(&mut self, v: bool) {
        self.mixed = v;
    }

    pub fn checked(&self) -> bool {
        self.checked
    }

    pub fn set_checked(&mut self, v: bool) {
        self.checked = v;
    }

    pub fn set_enabled(&mut self, v: bool) {
        self.enabled = v;
    }
}

impl Widget for Checkbox {
    type Out = CheckOut;

    /// Filled by K1b.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<CheckOut> {
        let _ = (cx, ev);
        None
    }
}
