//! TabStrip (F-K3, P2-kit 2.10): the frozen types; filled by K3.1. One Tab stop with roving keys; the app's chords
//! (Ctrl+Tab, Ctrl+PgDn, Ctrl+1..9) call select_next and select_index.

use std::rc::Rc;

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::{PointP, RectP};
use crate::node::{Text, WidgetId};
use crate::screen::{CaptionHit, Widget};
use crate::w3::IconBitmap;

/// One tab.
pub struct TabSpec {
    pub key: u64,
    pub label: Text,
    pub tip: Option<Text>,
    pub icon: Option<Rc<IconBitmap>>,
    pub busy: bool,
    pub error: bool,
}

/// What a tab strip tells its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TabOut {
    Selected(u64),
    CloseRequested(u64),
    NewRequested,
    Moved { key: u64, to: usize },
    ContextMenu(u64, PointP),
}

/// The window's tab strip (widths shrink evenly within TAB_W_MIN..TAB_W_MAX).
#[allow(dead_code)] // The fields are read by K3.1.
pub struct TabStrip {
    id: WidgetId,
    tabs: Vec<TabSpec>,
    selected: u64,
}

impl TabStrip {
    pub fn new(id: WidgetId) -> TabStrip {
        TabStrip { id, tabs: Vec::new(), selected: 0 }
    }

    pub fn set_tabs(&mut self, cx: &mut EventCx, tabs: Vec<TabSpec>, selected: u64) {
        let _ = cx;
        self.tabs = tabs;
        self.selected = selected;
    }

    /// The selected tab's key.
    pub fn selected(&self) -> u64 {
        self.selected
    }

    /// Empty strip space is Caption (K3.1); until then Client.
    pub fn caption_hit(&self, p: PointP) -> CaptionHit {
        let _ = p;
        CaptionHit::Client
    }

    /// The next tab (back: the previous one), wrapping; the key now selected. Filled by K3.1; until then None.
    pub fn select_next(&mut self, cx: &mut EventCx, back: bool) -> Option<u64> {
        let _ = (cx, back);
        None
    }

    /// Tab i (Ctrl+1..8 = 0..7, None past the count; usize::MAX = the last tab, Ctrl+9). Filled by K3.1; until then None.
    pub fn select_index(&mut self, cx: &mut EventCx, i: usize) -> Option<u64> {
        let _ = (cx, i);
        None
    }
}

impl Widget for TabStrip {
    type Out = TabOut;

    /// Filled by K3.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K3.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K3.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<TabOut> {
        let _ = (cx, ev);
        None
    }
}
