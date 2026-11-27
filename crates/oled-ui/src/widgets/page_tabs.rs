//! PageTabs (F-K2b, P2-kit 2.9): the frozen types; filled by K2b.5. Ctrl+1..n go through the screen's CommandTable.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// What page tabs tell their screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTabsOut {
    Selected(usize),
}

/// The page tabs of a toolbar.
#[allow(dead_code)] // The fields are read by K2b.5.
pub struct PageTabs {
    id: WidgetId,
    tabs: Vec<Text>,
    selected: usize,
}

impl PageTabs {
    pub fn new(id: WidgetId, tabs: Vec<Text>) -> PageTabs {
        PageTabs { id, tabs, selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Selects tab `i` (kept within the tabs).
    pub fn set_selected(&mut self, cx: &mut EventCx, i: usize) {
        let _ = cx;
        self.selected = i.min(self.tabs.len().saturating_sub(1));
    }
}

impl Widget for PageTabs {
    type Out = PageTabsOut;

    /// Filled by K2b.5; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2b.5; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2b.5; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PageTabsOut> {
        let _ = (cx, ev);
        None
    }
}
