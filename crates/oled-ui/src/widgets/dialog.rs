//! Dialog (F-K2b, P2-kit 2.9): the frozen types; filled by K2b.6. An in-window surface: the screen paints it last and
//! routes events to it first.

use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// A dialog's content; the last button is the primary one.
pub struct DialogSpec {
    pub title: Text,
    pub body: Text,
    pub buttons: Vec<(Text, CommandId)>,
    pub wide: bool,
}

/// What a dialog tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogOut {
    Chose(CommandId),
    Cancelled,
}

/// A modal surface inside the window.
#[allow(dead_code)] // The fields are read by K2b.6.
pub struct Dialog {
    id: WidgetId,
    spec: DialogSpec,
}

impl Dialog {
    pub fn new(id: WidgetId, spec: DialogSpec) -> Dialog {
        Dialog { id, spec }
    }
}

impl Widget for Dialog {
    type Out = DialogOut;

    /// Filled by K2b.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2b.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2b.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<DialogOut> {
        let _ = (cx, ev);
        None
    }
}
