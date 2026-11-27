//! StatusLine and its notice (F-K2b, P2-kit 2.9): the frozen types; filled by K2b.6.

use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::model::FgRole;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// The notice line: a 2 px ACCENT_ATTENTION bar, hidden after 6 s or Esc.
pub struct Notice {
    pub text: Text,
    pub action: Option<(Text, CommandId)>,
}

/// What a status line tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusOut {
    Action(CommandId),
}

/// The status line at the bottom of a window.
#[allow(dead_code)] // The fields are read by K2b.6.
pub struct StatusLine {
    id: WidgetId,
    items: Vec<(Text, FgRole)>,
    notice: Option<Notice>,
}

impl StatusLine {
    pub fn new(id: WidgetId) -> StatusLine {
        StatusLine { id, items: Vec::new(), notice: None }
    }

    pub fn set_items(&mut self, items: Vec<(Text, FgRole)>) {
        self.items = items;
    }

    pub fn set_notice(&mut self, cx: &mut EventCx, n: Option<Notice>) {
        let _ = cx;
        self.notice = n;
    }
}

impl Widget for StatusLine {
    type Out = StatusOut;

    /// Filled by K2b.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2b.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2b.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<StatusOut> {
        let _ = (cx, ev);
        None
    }
}
