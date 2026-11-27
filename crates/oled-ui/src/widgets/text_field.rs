//! The text field (F-K2b, P2-kit 2.9): FieldKind and FieldOut frozen in K0.1a (the app models need them), the widget's
//! type in K0.1c; filled by K2b.3 on K2b.1's FieldModel, with cluster carets from K2b.2.

use std::ops::Range;

use super::field_model::{Edit, FieldModel, ModelOut, Motion};
use crate::cx::{Anchor, EventCx, LayoutCx, MenuId, PaintCx};
use crate::event::{A11yAction, Event, ImeEvent, Key, Mods, MouseButton, TimerId};
use crate::geom::{PointP, RectP, SizeP};
use crate::glyph::Glyph;
use crate::node::{Actions, Cursor, Focusable, Node, NodeFlags, NodeId, NodeValue, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::strings::k;
use crate::text::{FontId, LayoutOpts, TextEngine, TextLayout, Trim};
use crate::theme::{Ink, TypeRole};

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

/// The most completions a path field lists.
const MAX_COMPLETIONS: usize = 8;
/// The timer slots of a field: the blink step and the end of the blink.
const SLOT_BLINK: u8 = 0;
const SLOT_BLINK_END: u8 = 1;
/// The node slot of the clear button.
const SLOT_CLEAR: u32 = 1;

/// A one-line text field: search, plain, path or rename.
pub struct TextField {
    id: WidgetId,
    kind: FieldKind,
    /// Boxed: its undo ring is large and a PropertySheet row holds a field by value.
    model: Box<FieldModel>,
    placeholder: Option<Text>,
    label: Option<Text>,
    error: Option<Text>,
    busy: bool,
    clear_button: bool,
    enabled: bool,
    /// The plate (physical px), the text area inside it and the clear button, from the last layout.
    plate: RectP,
    text_rect: RectP,
    clear_rect: RectP,
    font: FontId,
    /// How far the text is scrolled to the left (physical px).
    scroll_x: i32,
    caret_on: bool,
    /// The time of the last edit or caret move (the blink runs for CARET_BLINK_FOR after it).
    last_activity: u64,
    /// The open completions list, its items and the highlighted one.
    list: Option<MenuId>,
    completions: Vec<Text>,
    highlight: Option<usize>,
    /// The fixed end of a mouse selection while the button is down.
    drag_anchor: Option<usize>,
    /// A gallery page paints the field as focused without the dispatcher (preview_state).
    look_focused: bool,
    /// The width the field asks for, logical px.
    width: i32,
}

impl TextField {
    pub fn new(id: WidgetId, kind: FieldKind) -> TextField {
        TextField {
            id,
            kind,
            model: Box::new(FieldModel::new(kind)),
            placeholder: None,
            label: None,
            error: None,
            busy: false,
            clear_button: false,
            enabled: true,
            plate: RectP::EMPTY,
            text_rect: RectP::EMPTY,
            clear_rect: RectP::EMPTY,
            font: FontId(0),
            scroll_x: 0,
            caret_on: true,
            last_activity: 0,
            list: None,
            completions: Vec::new(),
            highlight: None,
            drag_anchor: None,
            look_focused: false,
            width: 240,
        }
    }

    pub fn text(&self) -> &str {
        self.model.text()
    }

    /// Replaces the text; the caret goes to the end, or everything is selected.
    pub fn set_text(&mut self, cx: &mut EventCx, s: &str, select_all: bool) {
        if select_all {
            self.model.set_text_selected(s);
        } else {
            self.model.set_text(s);
        }
        self.after_edit(cx);
    }

    /// The selected byte range (empty: the caret).
    pub fn selection(&self) -> Range<usize> {
        self.model.selection().unwrap_or(self.model.caret()..self.model.caret())
    }

    /// Selects a byte range, kept within the text.
    pub fn select(&mut self, cx: &mut EventCx, r: Range<usize>) {
        self.model.select(r);
        self.after_edit(cx);
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
        if self.error != msg {
            self.error = msg;
            cx.relayout();
        }
    }

    pub fn set_busy(&mut self, on: bool) {
        self.busy = on;
    }

    pub fn set_clear_button(&mut self, on: bool) {
        self.clear_button = on;
    }

    pub fn set_max_chars(&mut self, n: u32) {
        self.model.set_max_chars(n);
    }

    /// Characters refused as typed (files: \ / : * ? " < > |).
    pub fn set_filter(&mut self, refuse: Option<fn(char) -> bool>) {
        self.model.set_filter(refuse);
    }

    /// The width the field asks for in a toolbar, logical px (default 240; the tasks' search asks 200).
    pub fn set_width(&mut self, logical: i32) {
        self.width = logical.max(1);
    }

    /// A disabled field takes no focus and no input; it paints at FILL_CONTROL_OFF and FG_DISABLED.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// For galleries and goldens: sets the text, an error line and an IME composition, and paints the field as
    /// focused (rim and caret) with a selection, without a window or a dispatcher around it.
    pub fn preview(
        &mut self,
        text: &str,
        focused: bool,
        selection: Option<Range<usize>>,
        composition: Option<(&str, usize)>,
        error: Option<Text>,
    ) {
        self.model.set_text(text);
        self.look_focused = focused;
        self.caret_on = true;
        self.error = error;
        if let Some(r) = selection {
            self.model.select(r);
        }
        if let Some((t, c)) = composition {
            self.model.set_composition(t, c);
        }
    }

    /// Path: up to 8, a list-mode menu under the field (K2b.3).
    pub fn set_completions(&mut self, cx: &mut EventCx, items: Vec<Text>) {
        let mut items = items;
        items.truncate(MAX_COMPLETIONS);
        self.completions = items;
        self.highlight = None;
        if self.completions.is_empty() {
            self.close_list(cx);
            return;
        }
        let items = self.completions.clone();
        match self.list {
            Some(id) => cx.update_list(id, items, None),
            None => self.list = Some(cx.open_list(items, Anchor::Below(self.plate), None)),
        }
    }

    /// The field is going away: its timers and its caret report stop (what losing the focus does).
    pub(crate) fn release(&mut self, cx: &mut EventCx) {
        cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK));
        cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK_END));
        self.close_list(cx);
        self.caret_on = false;
        cx.set_caret(None);
    }

    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    /// True while an IME composition is open.
    pub fn is_composing(&self) -> bool {
        self.model.is_composing()
    }

    // ---- geometry and scrolling

    fn clear_visible(&self) -> bool {
        self.clear_button && self.enabled && !self.model.is_empty()
    }

    fn line_layout(&self, eng: &mut dyn TextEngine, s: &str) -> std::rc::Rc<TextLayout> {
        eng.layout(s, self.font, &LayoutOpts { max_width: None, trim: Trim::None, wrap: false })
    }

    /// The x of the caret in the text (physical px from its start), composition included.
    fn caret_offset(&self, eng: &mut dyn TextEngine) -> i32 {
        let shown = self.model.display_text();
        let l = self.line_layout(eng, &shown);
        eng.caret_x(&l, self.model.display_caret())
    }

    /// Scrolls so that the caret stays margin px inside the text area; no scroll while the text fits.
    fn ensure_visible(&mut self, eng: &mut dyn TextEngine, margin: i32) {
        if self.text_rect.is_empty() {
            // Not laid out yet: the font is not known, and a layout made with it would be cached empty.
            return;
        }
        let shown = self.model.display_text().into_owned();
        let l = self.line_layout(eng, &shown);
        let view = self.text_rect.w();
        if view <= 0 || l.width <= view {
            self.scroll_x = 0;
            return;
        }
        let caret = eng.caret_x(&l, self.model.display_caret());
        let m = margin.min(view / 4);
        if caret - self.scroll_x > view - m {
            self.scroll_x = caret - view + m;
        } else if caret - self.scroll_x < m {
            self.scroll_x = (caret - m).max(0);
        }
        self.scroll_x = self.scroll_x.clamp(0, (l.width - view + m).max(0));
    }

    /// The caret rectangle in client coordinates (the IME's anchor).
    fn caret_rect(&self, eng: &mut dyn TextEngine, px1: i32) -> RectP {
        let lh = eng.metrics(self.font).line_height;
        let x = self.text_rect.l + self.caret_offset(eng) - self.scroll_x;
        let y = self.plate.t + (self.plate.h() - lh) / 2;
        RectP::wh(x, y, px1, lh)
    }

    /// Restarts the caret blink and tells the host where the caret is.
    fn touch(&mut self, cx: &mut EventCx) {
        self.caret_on = true;
        self.last_activity = cx.now_ms();
        let m = cx.tokens().motion;
        cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK));
        cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK_END));
        if !m.reduced && cx.focused() == Some(self.node_id()) {
            cx.set_timer(TimerId::kit(self.id, SLOT_BLINK), m.caret_blink);
            cx.set_timer(TimerId::kit(self.id, SLOT_BLINK_END), m.caret_blink_for);
        }
        if cx.focused() == Some(self.node_id()) && !self.text_rect.is_empty() {
            let px1 = cx.px(1);
            let margin = cx.px(8);
            self.ensure_visible(cx.text(), margin);
            let r = self.caret_rect(cx.text(), px1);
            cx.set_caret(Some(r));
        }
    }

    /// Repaints the field after the text, the caret or the selection changed.
    fn after_edit(&mut self, cx: &mut EventCx) {
        self.touch(cx);
        let margin = cx.px(8);
        self.ensure_visible(cx.text(), margin);
        cx.damage(self.plate);
        if self.error.is_some() || self.clear_visible() == self.clear_rect.is_empty() {
            cx.relayout();
        }
    }

    // ---- editing

    fn clip_text(&mut self, cx: &mut EventCx) {
        if let Some(t) = self.model.selected_text() {
            cx.clipboard().set_text(t);
        }
    }

    /// What an edit of the model means for the screen.
    fn finish(&mut self, cx: &mut EventCx, out: ModelOut) -> Option<FieldOut> {
        match out {
            ModelOut::Unchanged => None,
            ModelOut::CaretOnly => {
                self.after_edit(cx);
                None
            }
            ModelOut::Changed => {
                self.after_edit(cx);
                Some(FieldOut::Changed)
            }
            ModelOut::Cut(s) => {
                cx.clipboard().set_text(&s);
                self.after_edit(cx);
                Some(FieldOut::Changed)
            }
            ModelOut::Refused(c) => Some(FieldOut::Refused(c)),
        }
    }

    fn edit(&mut self, cx: &mut EventCx, e: Edit) -> Option<FieldOut> {
        let now = cx.now_ms();
        let out = self.model.apply_at(e, now);
        self.finish(cx, out)
    }

    fn close_list(&mut self, cx: &mut EventCx) {
        if self.list.take().is_some() {
            cx.close_menus();
        }
        self.highlight = None;
    }

    /// Takes completion `i` as the text, with a backslash after it.
    fn accept(&mut self, cx: &mut EventCx, i: usize) -> Option<FieldOut> {
        let item = self.completions.get(i)?.to_string();
        let mut s = item;
        if !s.ends_with('\\') {
            s.push('\\');
        }
        self.model.set_text(&s);
        self.close_list(cx);
        self.completions.clear();
        self.after_edit(cx);
        Some(FieldOut::CompletionAccepted(s))
    }

    fn move_highlight(&mut self, cx: &mut EventCx, down: bool) {
        let n = self.completions.len();
        if n == 0 {
            return;
        }
        let next = match (self.highlight, down) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1).min(n - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.highlight = Some(next);
        if let Some(id) = self.list {
            cx.update_list(id, self.completions.clone(), self.highlight);
        }
    }

    fn key(&mut self, cx: &mut EventCx, key: Key, mods: Mods) -> Option<FieldOut> {
        let ctrl = mods.ctrl() && !mods.alt();
        let shift = mods.shift();
        let list_open = self.list.is_some();
        let at_end = self.model.caret() == self.model.text().len() && self.model.selection().is_none();
        let mv = |by| Edit::Move { by, extend: shift };
        let out = match key {
            _ if self.model.is_composing() && key != Key::ESC => return None,
            Key::LEFT => {
                let out = self.edit(cx, mv(if ctrl { Motion::WordLeft } else { Motion::CharLeft }));
                self.snap_caret(cx, true, shift);
                out
            }
            Key::RIGHT if list_open && at_end && !shift => match self.highlight.or(Some(0)) {
                Some(i) => self.accept(cx, i),
                None => None,
            },
            Key::RIGHT => {
                let out = self.edit(cx, mv(if ctrl { Motion::WordRight } else { Motion::CharRight }));
                self.snap_caret(cx, false, shift);
                out
            }
            Key::HOME => self.edit(cx, mv(Motion::Home)),
            Key::END => self.edit(cx, mv(Motion::End)),
            Key::BACK => self.edit(cx, if ctrl { Edit::DeleteWordLeft } else { Edit::Backspace }),
            Key::DELETE => self.edit(cx, if ctrl { Edit::DeleteWordRight } else { Edit::Delete }),
            Key::UP if list_open => {
                self.move_highlight(cx, false);
                None
            }
            Key::DOWN if list_open => {
                self.move_highlight(cx, true);
                None
            }
            Key::DOWN => {
                cx.consume();
                return Some(FieldOut::CompletionRequested(self.model.text().to_string()));
            }
            Key::UP => None,
            Key::ENTER => {
                self.close_list(cx);
                cx.consume();
                return Some(FieldOut::Committed(self.model.text().to_string()));
            }
            Key::TAB if list_open && !shift => {
                let i = self.highlight.unwrap_or(0);
                cx.consume();
                return self.accept(cx, i);
            }
            Key::TAB if matches!(self.kind, FieldKind::Rename { .. }) => {
                cx.consume();
                return Some(FieldOut::CommitAndMove { text: self.model.text().to_string(), back: shift });
            }
            Key::F2 if matches!(self.kind, FieldKind::Rename { .. }) => self.edit(cx, Edit::CycleRename),
            Key::ESC => return self.escape(cx),
            Key(0x41) if ctrl => self.edit(cx, Edit::SelectAll),
            Key(0x43) if ctrl => {
                self.clip_text(cx);
                None
            }
            Key(0x58) if ctrl => self.edit(cx, Edit::Cut),
            Key(0x56) if ctrl => {
                let pasted = cx.clipboard().text();
                match pasted {
                    Some(t) => self.edit(cx, Edit::Insert(t)),
                    None => None,
                }
            }
            Key(0x5A) if ctrl && shift => self.edit(cx, Edit::Redo),
            Key(0x5A) if ctrl => self.edit(cx, Edit::Undo),
            Key(0x59) if ctrl => self.edit(cx, Edit::Redo),
            _ => return None,
        };
        cx.consume();
        out
    }

    /// A caret that a character move left inside a cluster goes on to the cluster's edge (back, or forward).
    fn snap_caret(&mut self, cx: &mut EventCx, back: bool, extend: bool) {
        let (text, caret) = (self.model.text(), self.model.caret());
        let edge = if back { crate::text::hit::snap_down(text, caret) } else { crate::text::hit::snap_up(text, caret) };
        if edge != caret {
            if extend {
                let anchor = self.model.anchor();
                self.model.select(anchor..edge);
            } else {
                self.model.set_caret(edge);
            }
            self.after_edit(cx);
        }
    }

    /// The Esc ladder: the list, then the composition, then the field's text; nothing to cancel leaves Esc to the
    /// window.
    fn escape(&mut self, cx: &mut EventCx) -> Option<FieldOut> {
        if self.list.is_some() {
            self.close_list(cx);
            cx.consume();
            return None;
        }
        if self.model.cancel_composition() {
            self.after_edit(cx);
            cx.consume();
            return None;
        }
        if matches!(self.kind, FieldKind::Rename { .. }) || !self.model.is_empty() {
            cx.consume();
            return Some(FieldOut::Cancelled);
        }
        None
    }

    // ---- the pointer

    /// The byte offset of the caret stop under x (client coordinates).
    fn index_at(&self, cx: &mut EventCx, x: i32) -> usize {
        let shown = self.model.display_text().into_owned();
        let l = self.line_layout(cx.text(), &shown);
        let hit = cx.text().hit_test(&l, x - self.text_rect.l + self.scroll_x);
        let at = if hit.trailing { crate::text::hit::next_stop(&shown, hit.index) } else { hit.index };
        if self.model.is_composing() {
            self.model.caret()
        } else {
            at
        }
    }

    fn word_range(&self, at: usize) -> Range<usize> {
        let t = self.model.text();
        let w = |c: char| c.is_alphanumeric() || c == '_';
        let start = t[..at].char_indices().rev().take_while(|(_, c)| w(*c)).last().map_or(at, |(i, _)| i);
        let end = t[at..].char_indices().take_while(|(_, c)| w(*c)).last().map_or(at, |(i, c)| at + i + c.len_utf8());
        if start == end {
            // On a separator: that one character.
            let next = crate::text::hit::next_stop(t, at);
            at..next
        } else {
            start..end
        }
    }

    fn pointer_down(&mut self, cx: &mut EventCx, pos: PointP, clicks: u8, mods: Mods) {
        if self.model.is_composing() {
            return;
        }
        let at = self.index_at(cx, pos.x);
        match clicks {
            0 | 1 => {
                if mods.shift() {
                    let anchor = self.model.anchor();
                    self.model.select(anchor..at);
                } else {
                    self.model.set_caret(at);
                }
                self.drag_anchor = Some(self.model.anchor());
            }
            2 => {
                let r = self.word_range(at);
                self.drag_anchor = Some(r.start);
                self.model.select(r);
            }
            _ => {
                let n = self.model.text().len();
                self.drag_anchor = Some(0);
                self.model.select(0..n);
            }
        }
        self.after_edit(cx);
    }

    fn drag_to(&mut self, cx: &mut EventCx, x: i32) {
        if let Some(a) = self.drag_anchor {
            let at = self.index_at(cx, x);
            self.model.select(a..at);
            self.after_edit(cx);
        }
    }

    fn ime(&mut self, cx: &mut EventCx, e: &ImeEvent) -> Option<FieldOut> {
        match e {
            ImeEvent::Start => {
                self.touch(cx);
                None
            }
            ImeEvent::Composition { text, caret } => {
                self.model.set_composition(text, *caret);
                self.after_edit(cx);
                None
            }
            ImeEvent::Result(s) => {
                let now = cx.now_ms();
                let out = self.model.commit(s, now);
                self.finish(cx, out)
            }
            ImeEvent::End => {
                if self.model.cancel_composition() {
                    self.after_edit(cx);
                }
                None
            }
        }
    }

    fn timer(&mut self, cx: &mut EventCx, id: TimerId) {
        if self.text_rect.is_empty() {
            return;
        }
        let m = cx.tokens().motion;
        let caret = {
            let px1 = cx.px(1);
            self.caret_rect(cx.text(), px1)
        };
        if id == TimerId::kit(self.id, SLOT_BLINK) {
            if cx.now_ms() >= self.last_activity + u64::from(m.caret_blink_for) {
                self.caret_on = true;
            } else {
                self.caret_on = !self.caret_on;
                cx.set_timer(id, m.caret_blink);
            }
            cx.damage(caret);
        } else if id == TimerId::kit(self.id, SLOT_BLINK_END) {
            self.caret_on = true;
            cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK));
            cx.damage(caret);
        }
    }
}

impl Widget for TextField {
    type Out = FieldOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.font = cx.font(TypeRole::Body);
        let h = cx.px(cx.tokens().size.control_h);
        let rename = matches!(self.kind, FieldKind::Rename { .. });
        self.plate = if rename || rect.h() <= h {
            rect
        } else {
            let t = rect.t + (rect.h() - h) / 2;
            RectP { l: rect.l, t, r: rect.r, b: t + h }
        };
        let side = cx.px(20);
        self.clear_rect = if self.clear_visible() {
            let y = self.plate.t + (self.plate.h() - side) / 2;
            RectP::wh(self.plate.r - cx.px(4) - side, y, side, side)
        } else {
            RectP::EMPTY
        };
        let left = if self.kind == FieldKind::Search { cx.px(30) } else { cx.px(8) };
        let right = if self.clear_rect.is_empty() { self.plate.r - cx.px(8) } else { self.clear_rect.l - cx.px(4) };
        self.text_rect =
            RectP { l: self.plate.l + left, t: self.plate.t, r: right.max(self.plate.l + left), b: self.plate.b };
        let margin = cx.px(8);
        self.ensure_visible(cx.text(), margin);

        let mut flags = NodeFlags::default();
        if !self.enabled {
            flags = flags | NodeFlags::DISABLED;
        }
        if self.error.is_some() {
            flags = flags | NodeFlags::INVALID;
        }
        if self.busy {
            flags = flags | NodeFlags::BUSY;
        }
        let role = if self.kind == FieldKind::Search { Role::SearchInput } else { Role::TextInput };
        let name = self.label.clone().or_else(|| self.placeholder.clone()).unwrap_or(Text::Borrowed(""));
        let mut node = Node::new(self.node_id(), role, self.plate)
            .flags(flags)
            .focus(if self.enabled { Focusable::Tab } else { Focusable::No })
            .label(name)
            .value(NodeValue::Text(Text::Owned(self.model.text().to_string())))
            .actions(Actions::FOCUS | Actions::SET_VALUE)
            .cursor(Cursor::IBeam);
        if let Some(e) = &self.error {
            node = node.description(e.clone());
        }
        cx.push(node);
        if !self.clear_rect.is_empty() {
            let clear = Node::new(NodeId::new(self.id, SLOT_CLEAR), Role::Button, self.clear_rect)
                .parent(self.node_id())
                .label(cx.lang().kit(k::KIT_CLEAR))
                .actions(Actions::CLICK);
            cx.push(clear);
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let id = self.node_id();
        let focused = cx.focused(id) || self.look_focused;
        let rename = matches!(self.kind, FieldKind::Rename { .. });
        let radius = cx.px(if rename { cx.tokens().radius.xs } else { cx.tokens().radius.s });
        let c = cx.tokens().color.clone();
        let plate = self.plate;
        let fill = if !self.enabled {
            c.fill_control_off
        } else if cx.hot(id) && !focused {
            c.fill_control_hot
        } else {
            c.fill_control
        };
        if rename {
            // Over a row: opaque first, so that the row's plate does not show through.
            cx.fill(plate, radius, c.bg);
        }
        cx.fill(plate, radius, fill);
        let rim = if self.error.is_some() {
            c.accent_danger
        } else if focused {
            Ink { a: 170, ..c.fg_secondary }
        } else {
            c.rim_control
        };
        if rename && self.error.is_none() {
            // The kit's ring width in RIM_FOCUS.
            cx.stroke(plate, radius, cx.env.scale.ring(), c.rim_focus);
        } else {
            cx.stroke(plate, radius, 1, rim);
        }
        let ink = if self.enabled { c.fg_primary } else { c.fg_disabled };
        if self.kind == FieldKind::Search {
            let g = cx.px(16);
            let r = RectP::wh(plate.l + cx.px(8), plate.t + (plate.h() - g) / 2, g, g);
            cx.glyph(r, Glyph::Search, if self.enabled { c.fg_secondary } else { c.fg_disabled });
        }
        let shown = self.model.display_text().into_owned();
        let l = self.line_layout(&mut *cx.res.text, &shown);
        let ty = plate.t + (plate.h() - l.height) / 2;
        let tx = self.text_rect.l - self.scroll_x;
        let clip = RectP { l: self.text_rect.l, t: plate.t, r: self.text_rect.r, b: plate.b };
        let px1 = cx.px(1).max(1);
        let comp = self.model.composition().map(|(t, _)| t.len());
        let sel = if self.model.is_composing() { None } else { self.model.selection() };
        let caret_x = cx.res.text.caret_x(&l, self.model.display_caret());
        let sel_x = sel.map(|r| (cx.res.text.caret_x(&l, r.start), cx.res.text.caret_x(&l, r.end)));
        let comp_x = comp.map(|n| {
            let s = self.model.caret().min(self.model.anchor());
            (cx.res.text.caret_x(&l, s), cx.res.text.caret_x(&l, s + n))
        });
        let empty = self.model.is_empty() && !self.model.is_composing();
        let ph = self.placeholder.clone();
        let (fg_t, fg_s, sel_fill, hot_fg) = (c.fg_tertiary, c.fg_secondary, c.fill_text_selection, c.fg_hot);
        let text_ink = ink;
        let show_caret = focused && self.caret_on && self.enabled;
        let tr = self.text_rect;
        cx.with_clip(clip, &mut |cx: &mut PaintCx| {
            if let Some((x0, x1)) = sel_x {
                cx.fill(RectP { l: tx + x0, t: ty, r: tx + x1, b: ty + l.height }, 0, sel_fill);
            }
            if empty {
                if let Some(p) = &ph {
                    let inset = cx.px(3);
                    cx.text_in(
                        p,
                        TypeRole::Body,
                        RectP { l: tr.l + inset, t: plate.t, r: tr.r, b: plate.b },
                        crate::model::Align::Start,
                        Trim::End,
                        fg_t,
                    );
                }
            } else {
                cx.text(&l, PointP { x: tx, y: ty }, text_ink);
            }
            if let Some((x0, x1)) = comp_x {
                cx.hline(tx + x0, tx + x1, ty + l.height - px1, fg_s);
            }
            if show_caret {
                for i in 0..px1 {
                    cx.vline(tx + caret_x + i, ty, ty + l.height, hot_fg);
                }
            }
        });
        if !self.clear_rect.is_empty() {
            let cid = NodeId::new(self.id, SLOT_CLEAR);
            let r = self.clear_rect;
            if cx.hot(cid) {
                cx.fill(r, cx.px(5), c.fill_hover);
            }
            let icon = cx.px(12);
            let g = RectP::wh(r.l + (r.w() - icon) / 2, r.t + (r.h() - icon) / 2, icon, icon);
            cx.glyph(g, Glyph::Close, if cx.hot(cid) { c.fg_hot } else { c.fg_secondary });
        }
        if self.busy {
            let h = cx.px(2);
            let inset = radius / 2;
            cx.fill(RectP { l: plate.l + inset, t: plate.b - h, r: plate.r - inset, b: plate.b }, 0, c.fg_secondary);
        }
        if let Some(e) = &self.error {
            let top = plate.b + cx.px(4);
            let r = RectP { l: plate.l, t: top, r: plate.r, b: top + cx.px(16) };
            cx.text_in(e, TypeRole::Caption, r, crate::model::Align::Start, Trim::End, c.accent_danger);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<FieldOut> {
        let mine = cx.target().is_some_and(|t| t.owner() == self.id);
        match ev {
            Event::Timer(id) if id.is_kit() && TimerId::kit(self.id, SLOT_BLINK).0 >> 8 == id.0 >> 8 => {
                self.timer(cx, *id);
                None
            }
            Event::ListPicked { menu, index } if Some(*menu) == self.list => {
                self.list = None;
                self.accept(cx, *index)
            }
            Event::MenuClosed(menu) if Some(*menu) == self.list => {
                self.list = None;
                self.highlight = None;
                None
            }
            _ if !mine || !self.enabled => None,
            Event::FocusIn(_) => {
                self.after_edit(cx);
                None
            }
            Event::FocusOut => {
                cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK));
                cx.cancel_timer(TimerId::kit(self.id, SLOT_BLINK_END));
                self.caret_on = false;
                self.drag_anchor = None;
                self.model.cancel_composition();
                self.close_list(cx);
                cx.set_caret(None);
                cx.damage(self.plate);
                None
            }
            Event::Key { key, down: true, mods, .. } => self.key(cx, *key, *mods),
            Event::Key { .. } => None,
            Event::Text(c) if !c.is_control() => {
                let now = cx.now_ms();
                let out = self.model.apply_at(Edit::Insert(c.to_string()), now);
                cx.consume();
                self.finish(cx, out)
            }
            Event::PointerDown { pos, button: MouseButton::Left, clicks, mods }
                if cx.target() == Some(self.node_id()) =>
            {
                self.pointer_down(cx, *pos, *clicks, *mods);
                None
            }
            Event::DragMove { pos, .. } if cx.target() == Some(self.node_id()) => {
                self.drag_to(cx, pos.x);
                None
            }
            Event::PointerUp { .. } | Event::DragEnd { .. } => {
                self.drag_anchor = None;
                None
            }
            Event::Click { button: MouseButton::Left, .. } if cx.target() == Some(NodeId::new(self.id, SLOT_CLEAR)) => {
                self.model.clear();
                self.after_edit(cx);
                cx.set_focus(self.node_id());
                Some(FieldOut::Changed)
            }
            Event::Ime(e) => self.ime(cx, e),
            Event::A11y(_, A11yAction::SetValue(s)) => {
                self.model.set_text(s);
                self.after_edit(cx);
                Some(FieldOut::Changed)
            }
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: cx.px(self.width), h: cx.px(cx.tokens().size.control_h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx, PaintCx};
    use crate::event::Reply;
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    type Op = Box<dyn FnOnce(&mut TextField, &mut EventCx) + Send>;

    /// One field in a window, with a hook for calls that need an EventCx.
    struct Fx {
        w: TextField,
        outs: Vec<FieldOut>,
        closes: u32,
        cmds: CommandTable,
    }

    impl Screen for Fx {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.w.layout(cx, RectP::wh(cx.px(10), cx.px(10), cx.px(240), cx.px(28)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.w.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if *ev == Event::CloseRequested {
                self.closes += 1;
            }
            if let Some(o) = self.w.event(cx, ev) {
                self.outs.push(o);
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(op) = msg.downcast::<Op>() {
                (*op)(&mut self.w, cx);
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn field(kind: FieldKind, f: impl FnOnce(&mut TextField)) -> Harness<Fx> {
        let mut w = TextField::new(WidgetId(1), kind);
        f(&mut w);
        let fx = Fx { w, outs: Vec::new(), closes: 0, cmds: CommandTable::new() };
        Harness::new(fx, SizeL { w: 280, h: 120 }, 96, Tokens::oled())
    }

    fn op(h: &mut Harness<Fx>, f: impl FnOnce(&mut TextField, &mut EventCx) + Send + 'static) {
        let op: Op = Box::new(f);
        h.send(Box::new(op));
    }

    fn focused(kind: FieldKind) -> Harness<Fx> {
        let mut h = field(kind, |_| {});
        h.key("Tab");
        h
    }

    fn text(h: &Harness<Fx>) -> String {
        h.screen().w.text().to_string()
    }

    #[test]
    fn typing_and_keys() {
        let mut h = focused(FieldKind::Plain);
        h.type_text("hello world");
        assert_eq!(text(&h), "hello world");
        assert!(h.screen().outs.iter().all(|o| *o == FieldOut::Changed));
        h.key("Ctrl+Left").key("Ctrl+Backspace");
        assert_eq!(text(&h), "world");
        h.key("End").key("Shift+Home");
        assert_eq!(h.screen().w.selection(), 0..5);
        h.key("Ctrl+A").key("Delete");
        assert_eq!(text(&h), "");
        h.type_text("ab").key("Left").key("Backspace");
        assert_eq!(text(&h), "b");
        h.key("Ctrl+Z");
        assert_eq!(text(&h), "ab");
        h.key("Ctrl+Y");
        assert_eq!(text(&h), "b");
        h.key("Ctrl+Z").key("Ctrl+Shift+Z");
        assert_eq!(text(&h), "b");
    }

    #[test]
    fn enter_commits_esc_cancels() {
        let mut h = focused(FieldKind::Search);
        h.type_text("abc").key("Enter");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::Committed("abc".into())));
        h.key("Esc");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::Cancelled));
        h.key("Down");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::CompletionRequested("abc".into())));
    }

    #[test]
    fn esc_unconsumed_when_empty() {
        let mut h = focused(FieldKind::Plain);
        h.dispatcher().esc_closes = true;
        h.key("Esc");
        assert_eq!(h.screen().closes, 1, "an empty field leaves Esc to the window");
        let mut h = focused(FieldKind::Plain);
        h.dispatcher().esc_closes = true;
        h.type_text("x").key("Esc");
        assert_eq!(h.screen().closes, 0);
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::Cancelled));
    }

    #[test]
    fn clipboard_cut_copy_paste() {
        let mut h = focused(FieldKind::Plain);
        h.type_text("one two").key("Ctrl+A").key("Ctrl+C");
        assert_eq!(h.clipboard().text.as_deref(), Some("one two"));
        h.key("Ctrl+X");
        assert_eq!(text(&h), "");
        h.key("Ctrl+V");
        assert_eq!(text(&h), "one two");
    }

    #[test]
    fn paste_reads_clipboard_once() {
        let mut h = focused(FieldKind::Plain);
        h.clipboard().text = Some("abc\r\ndef".into());
        h.type_text("x");
        assert_eq!(text(&h), "x");
        h.key("Ctrl+V");
        assert_eq!(text(&h), "xabc  def", "line breaks become spaces");
        h.clipboard().text = Some("ZZZ".into());
        h.type_text("y");
        assert_eq!(text(&h), "xabc  defy", "typing and painting never read the clipboard");
    }

    #[test]
    fn refused_character_is_reported() {
        let mut h = field(FieldKind::Plain, |f| f.set_filter(Some(|c| c == '/')));
        h.key("Tab").type_text("a/b");
        assert_eq!(text(&h), "ab");
        assert!(h.screen().outs.contains(&FieldOut::Refused('/')));
    }

    #[test]
    fn caret_blinks_five_seconds_then_idle() {
        let mut h = focused(FieldKind::Plain);
        assert!(!h.is_idle(), "the blink timers run while the field has the focus");
        assert!(h.screen().w.caret_on);
        h.advance(520);
        assert!(!h.screen().w.caret_on, "off after the first half second");
        h.advance(500);
        assert!(h.screen().w.caret_on);
        h.advance(4200);
        assert!(h.screen().w.caret_on, "solid once the blink time is over");
        assert!(h.is_idle(), "no timer survives the blink");
        h.type_text("a");
        assert!(!h.is_idle(), "an edit restarts it");
        h.advance(5100);
        assert!(h.is_idle());
    }

    #[test]
    fn reduced_motion_never_blinks() {
        let mut t = Tokens::oled();
        t.motion.reduced = true;
        let fx = Fx {
            w: TextField::new(WidgetId(1), FieldKind::Plain),
            outs: Vec::new(),
            closes: 0,
            cmds: CommandTable::new(),
        };
        let mut h = Harness::new(fx, SizeL { w: 280, h: 120 }, 96, t);
        h.key("Tab").type_text("a");
        assert!(h.is_idle());
    }

    #[test]
    fn placeholder_never_under_caret() {
        let mut h = field(FieldKind::Search, |f| f.set_placeholder(Text::from("Search")));
        h.key("Tab");
        let img = h.render();
        let f = &h.screen().w;
        let caret_x = f.text_rect.l;
        assert_eq!(f.text_rect.l - f.plate.l, 30, "the text starts at x + 30");
        assert!(img.text_rects.iter().all(|r| r.l > caret_x), "{:?} vs caret {caret_x}", img.text_rects);
    }

    #[test]
    fn completions_keys() {
        let mut h = focused(FieldKind::Path);
        h.type_text("C:\\U");
        op(&mut h, |f, cx| f.set_completions(cx, vec!["C:\\Docs".into(), "C:\\Dox".into(), "C:\\Dig".into()]));
        assert_eq!(h.menus().len(), 1);
        assert_eq!(h.menus()[0].items.len(), 3);
        h.key("Down");
        assert_eq!(h.menus()[0].highlight, Some(0));
        h.key("Down").key("Down").key("Down");
        assert_eq!(h.menus()[0].highlight, Some(2));
        h.key("Up");
        assert_eq!(h.menus()[0].highlight, Some(1));
        h.key("Esc");
        assert!(h.menus().is_empty(), "Esc closes the list first");
        assert_eq!(text(&h), "C:\\U", "and restores nothing else");
        op(&mut h, |f, cx| f.set_completions(cx, vec!["C:\\Docs".into(), "C:\\Dox".into()]));
        h.key("Down").key("Down").key("Tab");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::CompletionAccepted("C:\\Dox\\".into())));
        assert_eq!(text(&h), "C:\\Dox\\");
        assert!(h.menus().is_empty());
        op(&mut h, |f, cx| f.set_completions(cx, vec!["C:\\Dox\\bin".into()]));
        h.key("Right");
        assert_eq!(text(&h), "C:\\Dox\\bin\\", "Right at the end accepts");
        let many: Vec<Text> = (0..12).map(|i| Text::from(format!("p{i}"))).collect();
        op(&mut h, move |f, cx| f.set_completions(cx, many));
        assert_eq!(h.menus()[0].items.len(), 8, "at most eight rows");
    }

    #[test]
    fn rename_field_keys() {
        let mut h = field(FieldKind::Rename { stem_end: 4 }, |_| {});
        h.key("Tab");
        op(&mut h, |f, cx| f.set_text(cx, "note.txt", true));
        assert_eq!(h.screen().w.selection(), 0..4, "the stem is selected");
        h.key("F2");
        assert_eq!(h.screen().w.selection(), 0..8, "then the whole name");
        h.key("Tab");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::CommitAndMove { text: "note.txt".into(), back: false }));
        h.key("Shift+Tab");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::CommitAndMove { text: "note.txt".into(), back: true }));
        h.key("Esc");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::Cancelled));
    }

    #[test]
    fn mouse_places_selects_and_drags() {
        let mut h = field(FieldKind::Plain, |_| {});
        h.key("Tab").type_text("alpha beta");
        let f = &h.screen().w;
        let (y, x0) = (f.plate.t + 14, f.text_rect.l);
        h.click(PointL { x: x0 + 1, y });
        assert_eq!(h.screen().w.selection(), 0..0);
        h.double_click(PointL { x: x0 + 3, y });
        assert_eq!(h.screen().w.selection(), 0..5, "a double click selects the word");
        h.drag(PointL { x: x0 + 1, y }, PointL { x: x0 + 400, y });
        assert_eq!(h.screen().w.selection(), 0..10, "a drag selects to the end");
    }

    #[test]
    fn caret_skips_a_combining_mark() {
        let mut h = focused(FieldKind::Plain);
        h.type_text("cafe\u{301}");
        h.key("Left");
        assert_eq!(h.screen().w.selection(), 3..3, "one stop for the e and its mark");
        h.key("Right");
        assert_eq!(h.screen().w.selection(), 6..6);
    }

    #[test]
    fn ime_sequence_reaches_the_field() {
        let mut h = focused(FieldKind::Plain);
        h.type_text("ab");
        h.ime("xy", None);
        assert!(h.screen().w.is_composing());
        assert_eq!(text(&h), "ab", "the composition is not in the text yet");
        h.key("Esc");
        assert!(!h.screen().w.is_composing(), "Esc cancels the composition only");
        assert_eq!(h.screen().outs.last(), Some(&FieldOut::Changed));
        h.ime("xy", Some("XY"));
        assert_eq!(text(&h), "abXY");
        h.key("Ctrl+Z");
        assert_eq!(text(&h), "ab", "the result is one undo step");
    }

    #[test]
    fn focus_loss_drops_an_open_composition() {
        let mut h = focused(FieldKind::Plain);
        h.type_text("ab");
        h.ime("xy", None);
        assert!(h.screen().w.is_composing());
        op(&mut h, |f, cx| {
            f.event(cx, &Event::FocusOut);
        });
        assert!(!h.screen().w.is_composing(), "leaving the field ends the composition");
        h.key("Tab").type_text("c");
        assert_eq!(text(&h), "abc", "the field takes typing again");
    }

    #[test]
    fn long_text_scrolls_to_keep_the_caret_in() {
        let mut h = focused(FieldKind::Plain);
        h.type_text(&"w".repeat(80));
        assert!(h.screen().w.scroll_x > 0);
        h.key("Home");
        assert_eq!(h.screen().w.scroll_x, 0);
    }

    #[test]
    fn clear_button_empties_the_field() {
        let mut h = field(FieldKind::Search, |f| f.set_clear_button(true));
        h.key("Tab").type_text("abc");
        assert!(h.a11y_dump().contains("Clear"));
        let r = h.screen().w.clear_rect;
        h.click(PointL { x: r.l + 4, y: r.t + 4 });
        assert_eq!(text(&h), "");
        assert!(!h.a11y_dump().contains("Clear"), "no clear button on an empty field");
    }

    #[test]
    fn a11y_node() {
        let mut h = field(FieldKind::Search, |f| f.set_label(Text::from("Search tasks")));
        assert_eq!(h.a11y_dump(), "SearchInput \"Search tasks\" [focusable] \"\"\n");
        h.assert_named();
        op(&mut h, |f, cx| f.set_error(cx, Some(Text::from("Bad"))));
        assert!(h.a11y_dump().contains("invalid"));
    }

    #[test]
    fn disabled_takes_no_focus() {
        let mut h = field(FieldKind::Plain, |f| f.set_enabled(false));
        h.key("Tab").type_text("a");
        assert_eq!(text(&h), "");
    }
}
