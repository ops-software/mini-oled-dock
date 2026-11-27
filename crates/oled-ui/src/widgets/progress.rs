//! Progress (F-K3, P2-kit 2.10): the frozen type; filled by K3.2.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::WidgetId;
use crate::screen::Widget;

/// A 2 px progress line: a value, or indeterminate while running.
#[allow(dead_code)] // The fields are read by K3.2.
pub struct Progress {
    id: WidgetId,
    value: Option<f32>,
    running: bool,
}

impl Progress {
    pub fn new(id: WidgetId) -> Progress {
        Progress { id, value: None, running: false }
    }

    /// v None = indeterminate (it sweeps only while running).
    pub fn set(&mut self, cx: &mut EventCx, v: Option<f32>, running: bool) {
        let _ = cx;
        self.value = v;
        self.running = running;
    }
}

impl Widget for Progress {
    type Out = ();

    /// Filled by K3.2; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K3.2; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K3.2; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        let _ = (cx, ev);
        None
    }
}
