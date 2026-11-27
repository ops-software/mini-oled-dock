//! The text field (F-K2b, P2-kit 2.9): FieldKind and FieldOut frozen in K0.1a (the app models need them), the widget's
//! type in K0.1c; filled by K2b.3 on K2b.1's FieldModel.

use std::ops::Range;

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{NodeId, Text, WidgetId};
use crate::screen::Widget;

/// What a field edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Search,
    Plain,
    Path,
    /// A file name: the stem ends at `stem_end` (bytes), selected first.
    Rename {
        stem_end: usize,
    },
}

/// What a field tells its screen.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FieldOut {
    Changed,
    Committed(String),
    Cancelled,
    CompletionRequested(String),
    CompletionAccepted(String),
    /// Rename: Tab / Shift+Tab.
    CommitAndMove {
        text: String,
        back: bool,
    },
    Refused(char),
}

/// A one-line text field: search, plain, path or rename.
#[allow(dead_code)] // The fields are read by K2b.3.
pub struct TextField {
    id: WidgetId,
    kind: FieldKind,
    text: String,
    selection: Range<usize>,
    placeholder: Option<Text>,
    label: Option<Text>,
    error: Option<Text>,
    busy: bool,
    clear_button: bool,
    max_chars: Option<u32>,
    filter: Option<fn(char) -> bool>,
    completions: Vec<Text>,
}

impl TextField {
    pub fn new(id: WidgetId, kind: FieldKind) -> TextField {
        TextField {
            id,
            kind,
            text: String::new(),
            selection: 0..0,
            placeholder: None,
            label: None,
            error: None,
            busy: false,
            clear_button: false,
            max_chars: None,
            filter: None,
            completions: Vec::new(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text; the caret goes to the end, or everything is selected.
    pub fn set_text(&mut self, cx: &mut EventCx, s: &str, select_all: bool) {
        let _ = cx;
        self.text.clear();
        self.text.push_str(s);
        self.selection = if select_all { 0..s.len() } else { s.len()..s.len() };
    }

    /// The selected byte range (empty: the caret).
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    /// Selects a byte range, kept within the text.
    pub fn select(&mut self, cx: &mut EventCx, r: Range<usize>) {
        let _ = cx;
        let end = r.end.min(self.text.len());
        self.selection = r.start.min(end)..end;
    }

    pub fn set_placeholder(&mut self, t: Text) {
        self.placeholder = Some(t);
    }

    /// The accessible name.
    pub fn set_label(&mut self, t: Text) {
        self.label = Some(t);
    }

    /// An ACCENT_DANGER rim and a CAPTION line under the field.
    pub fn set_error(&mut self, cx: &mut EventCx, msg: Option<Text>) {
        let _ = cx;
        self.error = msg;
    }

    pub fn set_busy(&mut self, on: bool) {
        self.busy = on;
    }

    pub fn set_clear_button(&mut self, on: bool) {
        self.clear_button = on;
    }

    pub fn set_max_chars(&mut self, n: u32) {
        self.max_chars = Some(n);
    }

    /// Characters refused as typed (files: \ / : * ? " < > |).
    pub fn set_filter(&mut self, refuse: Option<fn(char) -> bool>) {
        self.filter = refuse;
    }

    /// Path: up to 8, a list-mode menu under the field (K2b.3).
    pub fn set_completions(&mut self, cx: &mut EventCx, items: Vec<Text>) {
        let _ = cx;
        self.completions = items;
    }

    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// True while an IME composition is open (K2b.1); until then false.
    pub fn is_composing(&self) -> bool {
        false
    }
}

impl Widget for TextField {
    type Out = FieldOut;

    /// Filled by K2b.3; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2b.3; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2b.3; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<FieldOut> {
        let _ = (cx, ev);
        None
    }
}
