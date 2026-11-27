//! The index page and its widget (K1a.6; K4.3 turns it into a SidebarList): one entry per registered page, Up/Down and
//! Enter.

use super::Page;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::Event;
use oled_ui::geom::RectP;
use oled_ui::node::WidgetId;
use oled_ui::screen::Widget;
use oled_ui::strings::UiLang;

/// What the index tells the gallery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexOut {
    /// The page at this registry index.
    Open(usize),
}

/// The list of pages.
pub struct Index {
    id: WidgetId,
    names: Vec<&'static str>,
}

impl Index {
    pub fn new(id: WidgetId, names: Vec<&'static str>) -> Index {
        Index { id, names }
    }
}

impl Widget for Index {
    type Out = IndexOut;

    /// Filled by K1a.6; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect, self.id, self.names.len());
    }

    /// Filled by K1a.6; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1a.6; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<IndexOut> {
        let _ = (cx, ev);
        None
    }
}

/// Filled by K1a.6; until then not ready.
pub fn page(lang: UiLang) -> Option<Page> {
    let _ = lang;
    None
}
