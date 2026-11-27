//! Button (F-K1, P2-kit 2.6): the frozen type; filled by K1a.6 (SPEC-platform 5.3.4 states, the mockups).

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{NodeId, Text, WidgetId};
use crate::screen::Widget;

/// A secondary (FILL_CONTROL plate) or primary (FILL_SELECTED plate) button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Secondary,
    Primary,
}

/// What a button (or an IconButton) tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonOut {
    Clicked,
}

/// A text button.
#[allow(dead_code)] // The fields are read by K1a.6.
pub struct Button {
    id: WidgetId,
    label: Text,
    kind: ButtonKind,
    enabled: bool,
    busy: bool,
}

impl Button {
    pub fn new(id: WidgetId, label: Text, kind: ButtonKind) -> Button {
        Button { id, label, kind, enabled: true, busy: false }
    }

    pub fn set_label(&mut self, t: Text) {
        self.label = t;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// The label in FG_SECONDARY with a progress line, inert.
    pub fn set_busy(&mut self, on: bool) {
        self.busy = on;
    }

    /// The button's node (slot 0 of its id).
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }
}

impl Widget for Button {
    type Out = ButtonOut;

    /// Filled by K1a.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1a.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1a.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ButtonOut> {
        let _ = (cx, ev);
        None
    }
}
