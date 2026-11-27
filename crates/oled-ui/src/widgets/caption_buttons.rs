//! CaptionButtons (F-K3, P2-kit 2.10): the frozen types; filled by K3.5 (46 x 40 each).

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::{PointP, RectP};
use crate::node::WidgetId;
use crate::screen::{CaptionHit, Widget};

/// What the caption buttons tell their screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionOut {
    Minimize,
    MaximizeRestore,
    Close,
}

/// The custom caption's minimise, maximise/restore and close buttons.
#[allow(dead_code)] // The fields are read by K3.5.
pub struct CaptionButtons {
    id: WidgetId,
}

impl CaptionButtons {
    pub fn new(id: WidgetId) -> CaptionButtons {
        CaptionButtons { id }
    }

    /// Which button a point is over (K3.5); until then Client.
    pub fn hit(&self, p: PointP) -> CaptionHit {
        let _ = p;
        CaptionHit::Client
    }
}

impl Widget for CaptionButtons {
    type Out = CaptionOut;

    /// Filled by K3.5; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K3.5; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K3.5; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<CaptionOut> {
        let _ = (cx, ev);
        None
    }
}
