//! SidebarList (F-K1, P2-kit 2.6): the frozen types; filled by K1b.2 (today's Settings nav pixels, R1).

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::{PointP, RectP};
use crate::node::{Text, WidgetId};
use crate::screen::Widget;
use std::rc::Rc;

use crate::glyph::Glyph;
use crate::model::FgRole;
use crate::w3::IconBitmap;

/// One entry of a sidebar.
pub struct SidebarItem {
    pub key: u64,
    pub label: Text,
    pub glyph: Option<Glyph>,
    pub icon: Option<Rc<IconBitmap>>,
    pub depth: u16,
    pub expandable: bool,
    pub expanded: bool,
    pub section: bool,
    pub trailing: Option<(Text, FgRole)>,
    pub disabled: bool,
    pub busy: bool,
}

/// A sidebar's row height and inset, logical px.
pub struct SidebarStyle {
    /// 36 = today's Settings nav, 28 = files.
    pub row_h: i32,
    /// 8.
    pub inset: i32,
}

/// What a sidebar tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarOut {
    Activated(u64),
    Expand(u64, bool),
    ContextMenu(u64, PointP),
}

/// A navigation list with sections, a tree and a current entry.
#[allow(dead_code)] // The fields are read by K1b.2.
pub struct SidebarList {
    id: WidgetId,
    style: SidebarStyle,
    items: Vec<SidebarItem>,
    current: Option<u64>,
    drop_target: Option<u64>,
}

impl SidebarList {
    pub fn new(id: WidgetId, style: SidebarStyle) -> SidebarList {
        SidebarList { id, style, items: Vec::new(), current: None, drop_target: None }
    }

    /// The entries (K1b.2 keeps the focused one by key and relayouts).
    pub fn set_items(&mut self, cx: &mut EventCx, items: Vec<SidebarItem>) {
        let _ = cx;
        self.items = items;
    }

    pub fn set_current(&mut self, key: Option<u64>) {
        self.current = key;
    }

    /// The entry a drag would drop on.
    pub fn set_drop_target(&mut self, key: Option<u64>) {
        self.drop_target = key;
    }
}

impl Widget for SidebarList {
    type Out = SidebarOut;

    /// Filled by K1b.2; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.2; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.2; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SidebarOut> {
        let _ = (cx, ev);
        None
    }
}
