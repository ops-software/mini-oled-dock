//! PathBar (F-K3, P2-kit 2.10): the frozen types; filled by K3.3 (edit mode is a Path TextField).

use crate::cx::{Anchor, EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// One segment of a path.
pub struct PathSegment {
    pub key: u64,
    pub label: Text,
    pub tip: Option<Text>,
}

/// What a path bar tells its screen.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PathOut {
    Navigate(u64),
    SiblingsRequested(u64, Anchor),
    EditStarted,
    EditCommitted(String),
    EditCancelled,
    CompletionRequested(String),
    Refresh,
}

/// The path bar: segments, or an edit field.
#[allow(dead_code)] // The fields are read by K3.3.
pub struct PathBar {
    id: WidgetId,
    segs: Vec<PathSegment>,
    edit_text: String,
    completions: Vec<Text>,
    offline: bool,
    error: Option<Text>,
}

impl PathBar {
    pub fn new(id: WidgetId) -> PathBar {
        PathBar { id, segs: Vec::new(), edit_text: String::new(), completions: Vec::new(), offline: false, error: None }
    }

    /// The segments, and the text the edit mode starts with.
    pub fn set_path(&mut self, cx: &mut EventCx, segs: Vec<PathSegment>, edit_text: String) {
        let _ = cx;
        self.segs = segs;
        self.edit_text = edit_text;
    }

    /// Switches to the edit field (K3.3).
    pub fn begin_edit(&mut self, cx: &mut EventCx) {
        let _ = cx;
    }

    pub fn set_completions(&mut self, cx: &mut EventCx, items: Vec<Text>) {
        let _ = cx;
        self.completions = items;
    }

    pub fn set_offline(&mut self, on: bool) {
        self.offline = on;
    }

    pub fn set_error(&mut self, cx: &mut EventCx, msg: Option<Text>) {
        let _ = cx;
        self.error = msg;
    }
}

impl Widget for PathBar {
    type Out = PathOut;

    /// Filled by K3.3; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K3.3; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K3.3; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PathOut> {
        let _ = (cx, ev);
        None
    }
}
