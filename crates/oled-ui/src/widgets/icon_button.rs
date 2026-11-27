//! IconButton (F-K1, P2-kit 2.6): the frozen type; filled by K1a.6.

use super::button::ButtonOut;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::glyph::Glyph;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// A 28 x 28 glyph button; its tip is the chip text and the accessible name.
#[allow(dead_code)] // The fields are read by K1a.6.
pub struct IconButton {
    id: WidgetId,
    glyph: Glyph,
    tip: Text,
    toggled: Option<bool>,
    enabled: bool,
}

impl IconButton {
    pub fn new(id: WidgetId, glyph: Glyph, tip: Text) -> IconButton {
        IconButton { id, glyph, tip, toggled: None, enabled: true }
    }

    /// Some(on) makes it a toggle button.
    pub fn set_toggled(&mut self, v: Option<bool>) {
        self.toggled = v;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }
}

impl Widget for IconButton {
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
