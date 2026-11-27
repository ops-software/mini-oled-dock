//! TextLabel (F-K1, P2-kit 2.6): the frozen type; filled by K1a.6.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::model::FgRole;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::TypeRole;

/// One run of text in a type role and a text colour role.
#[allow(dead_code)] // The fields are read by K1a.6.
pub struct TextLabel {
    id: WidgetId,
    text: Text,
    role: TypeRole,
    fg: FgRole,
    trim: Trim,
}

impl TextLabel {
    pub fn new(id: WidgetId, text: Text, role: TypeRole, fg: FgRole) -> TextLabel {
        TextLabel { id, text, role, fg, trim: Trim::End }
    }

    pub fn set_text(&mut self, t: Text) {
        self.text = t;
    }

    pub fn set_trim(&mut self, t: Trim) {
        self.trim = t;
    }
}

impl Widget for TextLabel {
    type Out = ();

    /// Filled by K1a.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1a.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1a.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        let _ = (cx, ev);
        None
    }
}
