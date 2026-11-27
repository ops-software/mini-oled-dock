//! The edit model of a text field (K2b.1): the text, a caret and an anchor as byte offsets on char boundaries, the
//! keyboard edits of the launcher's shipped field written again (word = alphanumeric or '_'), plus what the kit adds:
//! an undo ring of 32 steps, the rename selection cycle, a character filter and an IME composition that stays out of
//! the undo history until it is committed. Pure: no window, no clock (the caller passes the time of an edit).

use std::collections::VecDeque;

use super::text_field::FieldKind;

/// The longest text a model holds, in characters, until the owner sets another limit.
pub const DEFAULT_MAX_CHARS: u32 = 256;
/// Steps the undo ring keeps.
pub const UNDO_STEPS: usize = 32;
/// Typing that pauses longer than this starts a new undo step.
pub const COALESCE_MS: u64 = 1000;

/// One edit as the field's key handler passes it on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    Insert(String),
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    Move {
        by: Motion,
        extend: bool,
    },
    SelectAll,
    Cut,
    Undo,
    Redo,
    /// F2 in a rename field: stem, whole name, extension, stem.
    CycleRename,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
    Home,
    End,
}

/// What an edit did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ModelOut {
    /// Nothing changed: no repaint.
    Unchanged,
    /// The text changed.
    Changed,
    /// Only the caret or the selection moved.
    CaretOnly,
    /// The selection was removed (Ctrl+X): the text changed too, and this is what the clipboard gets.
    Cut(String),
    /// A character the filter refuses: nothing changed.
    Refused(char),
}

/// A character that belongs to a word (the word moves and deletes step over runs of these and runs of the others).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Snap {
    text: String,
    caret: usize,
    anchor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    None,
    Typing,
    Backspacing,
}

/// The undo history: up to 32 steps, each the state before the step; redo steps after an undo.
#[derive(Clone, Debug)]
struct UndoRing {
    undo: VecDeque<Snap>,
    redo: Vec<Snap>,
    group: Group,
    last_ms: u64,
    /// The caret the last grouped edit left: a caret that is elsewhere now is a jump and ends the group.
    after_caret: usize,
    /// A space ends the typing group: the next character starts a step of its own.
    broken: bool,
}

impl UndoRing {
    fn new() -> UndoRing {
        UndoRing {
            undo: VecDeque::new(),
            redo: Vec::new(),
            group: Group::None,
            last_ms: 0,
            after_caret: 0,
            broken: false,
        }
    }

    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.group = Group::None;
    }

    fn push(&mut self, s: Snap) {
        if self.undo.len() == UNDO_STEPS {
            self.undo.pop_front();
        }
        self.undo.push_back(s);
    }
}

/// The edit model of one text field.
#[derive(Clone, Debug)]
pub struct FieldModel {
    text: String,
    caret: usize,
    anchor: usize,
    max_chars: u32,
    kind: FieldKind,
    undo: UndoRing,
    /// The IME's text and the caret inside it (a char offset into the composition text, in bytes).
    comp: Option<(String, usize)>,
    refuse: Option<fn(char) -> bool>,
    /// A high surrogate that arrived alone (WM_CHAR delivers UTF-16 units), waiting for its low half.
    pending_high: Option<u16>,
}

impl FieldModel {
    pub fn new(kind: FieldKind) -> FieldModel {
        FieldModel {
            text: String::new(),
            caret: 0,
            anchor: 0,
            max_chars: DEFAULT_MAX_CHARS,
            kind,
            undo: UndoRing::new(),
            comp: None,
            refuse: None,
            pending_high: None,
        }
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn anchor(&self) -> usize {
        self.anchor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn max_chars(&self) -> u32 {
        self.max_chars
    }

    /// Sets the limit and cuts the text to it.
    pub fn set_max_chars(&mut self, n: u32) {
        self.max_chars = n;
        if self.char_count() > n as usize {
            let keep: usize = self.text.chars().take(n as usize).map(char::len_utf8).sum();
            self.text.truncate(keep);
            self.caret = self.caret.min(keep);
            self.anchor = self.anchor.min(keep);
        }
    }

    pub fn set_filter(&mut self, refuse: Option<fn(char) -> bool>) {
        self.refuse = refuse;
    }

    /// The selected byte range, None when the caret and the anchor are together.
    pub fn selection(&self) -> Option<core::ops::Range<usize>> {
        (self.caret != self.anchor).then(|| self.caret.min(self.anchor)..self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().and_then(|r| self.text.get(r))
    }

    /// Replaces the whole text (a restored value, a completion): cut to the limit, caret at the end, no selection.
    /// The undo history is dropped: this is not an edit of the person.
    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.extend(s.chars().take(self.max_chars as usize));
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.undo.clear();
        self.comp = None;
        self.pending_high = None;
    }

    /// The same text fully selected so that typing replaces it; a rename field selects its stem instead.
    pub fn set_text_selected(&mut self, s: &str) {
        self.set_text(s);
        if let FieldKind::Rename { .. } = self.kind {
            if let Some(r) = self.rename_ranges().first().cloned() {
                self.anchor = r.start;
                self.caret = r.end;
            }
        } else {
            self.anchor = 0;
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        self.undo.clear();
        self.comp = None;
        self.pending_high = None;
    }

    /// Puts the caret at a byte offset (clamped, moved back to a char boundary) and drops the selection.
    pub fn set_caret(&mut self, at: usize) {
        let at = self.floor_boundary(at);
        self.caret = at;
        self.anchor = at;
        self.undo.group = Group::None;
    }

    /// Selects a byte range (clamped to char boundaries); the caret ends at its end.
    pub fn select(&mut self, r: core::ops::Range<usize>) {
        self.anchor = self.floor_boundary(r.start);
        self.caret = self.floor_boundary(r.end);
        self.undo.group = Group::None;
    }

    fn floor_boundary(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text.get(..from).and_then(|t| t.char_indices().next_back()).map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text.get(from..).and_then(|t| t.chars().next()).map_or(self.text.len(), |c| from + c.len_utf8())
    }

    /// Start of the word before `from`: back over the non-word characters, then over the word.
    fn word_start_before(&self, from: usize) -> usize {
        let head = self.text.get(..from).unwrap_or("");
        let mut at = from;
        let mut in_word = false;
        for (i, c) in head.char_indices().rev() {
            if is_word(c) {
                in_word = true;
            } else if in_word {
                break;
            }
            at = i;
        }
        at
    }

    /// Start of the next word after `from`: forward over the rest of the word, then over the separators.
    fn word_start_after(&self, from: usize) -> usize {
        let tail = self.text.get(from..).unwrap_or("");
        let mut in_word = true;
        for (i, c) in tail.char_indices() {
            if in_word {
                in_word = is_word(c);
            } else if is_word(c) {
                return from + i;
            }
        }
        self.text.len()
    }

    fn delete_range(&mut self, r: core::ops::Range<usize>) -> ModelOut {
        if r.is_empty() || r.end > self.text.len() {
            return ModelOut::Unchanged;
        }
        self.text.replace_range(r.clone(), "");
        self.caret = r.start;
        self.anchor = r.start;
        ModelOut::Changed
    }

    /// The caret goes to `at`; the anchor follows it unless the selection is being extended.
    fn move_caret(&mut self, at: usize, extend: bool) -> ModelOut {
        let anchor = if extend { self.anchor } else { at };
        let moved = self.caret != at || self.anchor != anchor;
        self.caret = at;
        self.anchor = anchor;
        if moved {
            ModelOut::CaretOnly
        } else {
            ModelOut::Unchanged
        }
    }

    fn target(&self, by: Motion) -> usize {
        match by {
            Motion::CharLeft => self.prev_boundary(self.caret),
            Motion::CharRight => self.next_boundary(self.caret),
            Motion::WordLeft => self.word_start_before(self.caret),
            Motion::WordRight => self.word_start_after(self.caret),
            Motion::Home => 0,
            Motion::End => self.text.len(),
        }
    }

    fn snap(&self) -> Snap {
        Snap { text: self.text.clone(), caret: self.caret, anchor: self.anchor }
    }

    fn restore(&mut self, s: Snap) {
        self.text = s.text;
        self.caret = s.caret;
        self.anchor = s.anchor;
    }

    /// Applies an edit with no clock: its time is 0, so typing never ages out of its undo step. For callers and
    /// tests that do not care about the 1000 ms rule.
    pub fn apply(&mut self, e: Edit) -> ModelOut {
        self.apply_at(e, 0)
    }

    /// Applies an edit made at `now_ms` (any monotonic millisecond clock).
    pub fn apply_at(&mut self, e: Edit, now_ms: u64) -> ModelOut {
        self.pending_high = None;
        match e {
            Edit::Undo => return self.undo_step(),
            Edit::Redo => return self.redo_step(),
            Edit::CycleRename => return self.cycle_rename(),
            _ => {}
        }
        let before = self.snap();
        let sel_before = self.selection().is_some();
        let mut group = Group::None;
        let mut space = false;
        let out = match e {
            Edit::Insert(s) => {
                let mut it = s.chars();
                if let (Some(c), None) = (it.next(), it.next()) {
                    group = Group::Typing;
                    space = c == ' ' || matches!(c, '\r' | '\n' | '\t');
                }
                self.insert(&s)
            }
            Edit::Backspace => match self.selection() {
                Some(r) => self.delete_range(r),
                None => {
                    group = Group::Backspacing;
                    let from = self.prev_boundary(self.caret);
                    self.delete_range(from..self.caret)
                }
            },
            Edit::Delete => match self.selection() {
                Some(r) => self.delete_range(r),
                None => {
                    let to = self.next_boundary(self.caret);
                    self.delete_range(self.caret..to)
                }
            },
            Edit::DeleteWordLeft => match self.selection() {
                Some(r) => self.delete_range(r),
                None => {
                    let from = self.word_start_before(self.caret);
                    self.delete_range(from..self.caret)
                }
            },
            Edit::DeleteWordRight => match self.selection() {
                Some(r) => self.delete_range(r),
                None => {
                    let to = self.word_start_after(self.caret);
                    self.delete_range(self.caret..to)
                }
            },
            // A plain Left or Right over a selection collapses it to that side; the other motions go on from the caret.
            Edit::Move { by: Motion::CharLeft, extend: false } if self.selection().is_some() => {
                let at = self.selection().map_or(self.caret, |r| r.start);
                self.move_caret(at, false)
            }
            Edit::Move { by: Motion::CharRight, extend: false } if self.selection().is_some() => {
                let at = self.selection().map_or(self.caret, |r| r.end);
                self.move_caret(at, false)
            }
            Edit::Move { by, extend } => {
                let at = self.target(by);
                self.move_caret(at, extend)
            }
            Edit::SelectAll => self.select_all(),
            Edit::Cut => match self.selection() {
                Some(r) => {
                    let removed = self.text.get(r.clone()).unwrap_or("").to_string();
                    self.delete_range(r);
                    ModelOut::Cut(removed)
                }
                None => ModelOut::Unchanged,
            },
            Edit::Undo | Edit::Redo | Edit::CycleRename => ModelOut::Unchanged,
        };
        match out {
            ModelOut::Changed | ModelOut::Cut(_) => {
                let ring = &mut self.undo;
                let join = group != Group::None
                    && ring.group == group
                    && !sel_before
                    && !ring.broken
                    && now_ms.saturating_sub(ring.last_ms) <= COALESCE_MS
                    && before.caret == ring.after_caret;
                if !join {
                    ring.push(before);
                }
                ring.redo.clear();
                ring.group = group;
                ring.last_ms = now_ms;
                ring.after_caret = self.caret;
                ring.broken = space;
            }
            ModelOut::CaretOnly | ModelOut::Unchanged | ModelOut::Refused(_) => {
                if out == ModelOut::CaretOnly {
                    self.undo.group = Group::None;
                }
            }
        }
        out
    }

    fn select_all(&mut self) -> ModelOut {
        if self.text.is_empty() || (self.anchor == 0 && self.caret == self.text.len()) {
            return ModelOut::Unchanged;
        }
        self.anchor = 0;
        self.caret = self.text.len();
        ModelOut::CaretOnly
    }

    /// Replaces the selection with `s`: line breaks and tabs become spaces, other control characters stay (the view
    /// draws markers for them), and the whole text is cut at the limit. A refused character refuses all of `s`.
    fn insert(&mut self, s: &str) -> ModelOut {
        let clean: String = s.chars().map(|c| if matches!(c, '\r' | '\n' | '\t') { ' ' } else { c }).collect();
        if let Some(f) = self.refuse {
            if let Some(c) = clean.chars().find(|&c| f(c)) {
                return ModelOut::Refused(c);
            }
        }
        let sel = self.selection();
        let removed = sel.as_ref().map_or(0, |r| self.text.get(r.clone()).map_or(0, |t| t.chars().count()));
        let room = (self.max_chars as usize).saturating_sub(self.char_count() - removed);
        let clean: String = clean.chars().take(room).collect();
        if clean.is_empty() && sel.is_none() {
            return ModelOut::Unchanged;
        }
        let at = match sel {
            Some(r) => {
                self.text.replace_range(r.clone(), "");
                r.start
            }
            None => self.caret,
        };
        self.text.insert_str(at, &clean);
        self.caret = at + clean.len();
        self.anchor = self.caret;
        ModelOut::Changed
    }

    /// Types one UTF-16 unit as WM_CHAR delivers it: a high surrogate waits for its low half and the pair is one
    /// character, one edit and one undo step. A lone surrogate is dropped.
    pub fn type_unit(&mut self, unit: u16, now_ms: u64) -> ModelOut {
        let pending = self.pending_high.take();
        let c = match (pending, unit) {
            (_, 0xD800..=0xDBFF) => {
                self.pending_high = Some(unit);
                return ModelOut::Unchanged;
            }
            (Some(hi), 0xDC00..=0xDFFF) => char::decode_utf16([hi, unit]).next().and_then(Result::ok),
            (_, 0xDC00..=0xDFFF) => None,
            _ => char::from_u32(u32::from(unit)),
        };
        match c {
            Some(c) => self.apply_at(Edit::Insert(c.to_string()), now_ms),
            None => ModelOut::Unchanged,
        }
    }

    fn undo_step(&mut self) -> ModelOut {
        if self.comp.is_some() {
            return ModelOut::Unchanged;
        }
        let Some(prev) = self.undo.undo.pop_back() else {
            return ModelOut::Unchanged;
        };
        let cur = self.snap();
        self.undo.redo.push(cur);
        self.restore(prev);
        self.undo.group = Group::None;
        ModelOut::Changed
    }

    fn redo_step(&mut self) -> ModelOut {
        if self.comp.is_some() {
            return ModelOut::Unchanged;
        }
        let Some(next) = self.undo.redo.pop() else {
            return ModelOut::Unchanged;
        };
        let cur = self.snap();
        self.undo.push(cur);
        self.restore(next);
        self.undo.group = Group::None;
        ModelOut::Changed
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undo.redo.is_empty()
    }

    /// The selections a rename cycles through: the stem, the whole name, the extension (no dot); steps that would
    /// repeat the one before, or are empty, are left out.
    fn rename_ranges(&self) -> Vec<core::ops::Range<usize>> {
        let FieldKind::Rename { stem_end } = self.kind else {
            return Vec::new();
        };
        let stem_end = self.floor_boundary(stem_end);
        let len = self.text.len();
        let ext_start =
            if self.text.get(stem_end..).is_some_and(|t| t.starts_with('.')) { stem_end + 1 } else { stem_end };
        let mut v = Vec::with_capacity(3);
        v.push(0..stem_end);
        for r in [0..len, ext_start..len] {
            if !r.is_empty() && v.last() != Some(&r) {
                v.push(r);
            }
        }
        v
    }

    fn cycle_rename(&mut self) -> ModelOut {
        let ranges = self.rename_ranges();
        if ranges.is_empty() {
            return ModelOut::Unchanged;
        }
        let cur = self.caret.min(self.anchor)..self.caret.max(self.anchor);
        let next = ranges.iter().position(|r| *r == cur).map_or(0, |i| (i + 1) % ranges.len());
        let r = ranges[next].clone();
        let moved = self.anchor != r.start || self.caret != r.end;
        self.anchor = r.start;
        self.caret = r.end;
        self.undo.group = Group::None;
        if moved {
            ModelOut::CaretOnly
        } else {
            ModelOut::Unchanged
        }
    }

    // ---- composition

    /// Shows the IME's text at the caret without touching `text`; `caret` is a byte offset inside it. An empty text
    /// ends the composition.
    pub fn set_composition(&mut self, text: &str, caret: usize) {
        if text.is_empty() {
            self.comp = None;
            return;
        }
        let mut c = caret.min(text.len());
        while !text.is_char_boundary(c) {
            c -= 1;
        }
        self.comp = Some((text.to_string(), c));
    }

    pub fn is_composing(&self) -> bool {
        self.comp.is_some()
    }

    pub fn composition(&self) -> Option<(&str, usize)> {
        self.comp.as_ref().map(|(t, c)| (t.as_str(), *c))
    }

    /// Esc during a composition: drops it and nothing else. False when none was open.
    pub fn cancel_composition(&mut self) -> bool {
        self.comp.take().is_some()
    }

    /// The IME's result: the composition ends and `result` is inserted at the caret as one undo step.
    pub fn commit(&mut self, result: &str, now_ms: u64) -> ModelOut {
        self.comp = None;
        self.undo.group = Group::None;
        if result.chars().count() == 1 {
            // A single character must not join the typing step before it.
            let before = self.snap();
            let out = self.insert(result);
            if out == ModelOut::Changed {
                self.undo.push(before);
                self.undo.redo.clear();
                self.undo.last_ms = now_ms;
            }
            return out;
        }
        self.apply_at(Edit::Insert(result.to_string()), now_ms)
    }

    /// The text as drawn: the composition spliced in at the caret (it replaces the selection).
    pub fn display_text(&self) -> std::borrow::Cow<'_, str> {
        match &self.comp {
            None => std::borrow::Cow::Borrowed(self.text.as_str()),
            Some((c, _)) => {
                let r = self.selection().unwrap_or(self.caret..self.caret);
                let mut s = String::with_capacity(self.text.len() + c.len());
                s.push_str(self.text.get(..r.start).unwrap_or(""));
                s.push_str(c);
                s.push_str(self.text.get(r.end..).unwrap_or(""));
                std::borrow::Cow::Owned(s)
            }
        }
    }

    /// The caret as drawn, a byte offset into `display_text`.
    pub fn display_caret(&self) -> usize {
        match &self.comp {
            None => self.caret,
            Some((_, c)) => self.caret.min(self.anchor) + c,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// A seeded generator for the invariant tests (no dependency on another crate's test kit).
    struct XorShift(u64);

    impl XorShift {
        fn new(seed: u64) -> Self {
            XorShift(seed)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[(self.next_u64() % items.len() as u64) as usize]
        }

        fn ascii_or_bmp(&mut self, max: usize) -> String {
            let n = (self.next_u64() % max as u64) as usize + 1;
            (0..n)
                .map(|_| match self.next_u64() % 4 {
                    0 => char::from_u32(0xE0 + (self.next_u64() % 0x20) as u32).unwrap_or('a'),
                    1 => ' ',
                    2 => '\u{1F600}',
                    _ => char::from(b'a' + (self.next_u64() % 26) as u8),
                })
                .collect()
        }
    }

    fn at_end(s: &str) -> FieldModel {
        let mut f = FieldModel::new(FieldKind::Plain);
        f.set_text(s);
        f
    }

    fn at(s: &str, caret: usize) -> FieldModel {
        let mut f = at_end(s);
        f.set_caret(caret);
        f
    }

    fn ins(f: &mut FieldModel, s: &str) -> ModelOut {
        f.apply(Edit::Insert(s.to_string()))
    }

    fn mv(f: &mut FieldModel, by: Motion) -> ModelOut {
        f.apply(Edit::Move { by, extend: false })
    }

    #[test]
    fn f01_insert_ascii() {
        let mut f = FieldModel::new(FieldKind::Plain);
        assert_eq!(ins(&mut f, "abc"), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("abc", 3));
    }

    #[test]
    fn f02_insert_at_start() {
        let mut f = at("bc", 0);
        ins(&mut f, "a");
        assert_eq!((f.text(), f.caret()), ("abc", 1));
    }

    #[test]
    fn f03_insert_in_middle() {
        let mut f = at("ac", 1);
        ins(&mut f, "b");
        assert_eq!((f.text(), f.caret()), ("abc", 2));
    }

    #[test]
    fn f04_insert_at_end() {
        let mut f = at_end("ab");
        ins(&mut f, "c");
        assert_eq!((f.text(), f.caret()), ("abc", 3));
    }

    #[test]
    fn f05_insert_nothing_is_unchanged() {
        let mut f = at_end("ab");
        assert_eq!(ins(&mut f, ""), ModelOut::Unchanged);
    }

    #[test]
    fn f06_backspace_at_zero_is_unchanged() {
        let mut f = at("ab", 0);
        assert_eq!(f.apply(Edit::Backspace), ModelOut::Unchanged);
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn f07_backspace_ascii() {
        let mut f = at_end("abc");
        assert_eq!(f.apply(Edit::Backspace), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("ab", 2));
    }

    #[test]
    fn f08_backspace_over_a_two_byte_char() {
        let mut f = at_end("a\u{e9}");
        f.apply(Edit::Backspace);
        assert_eq!((f.text(), f.caret()), ("a", 1));
    }

    #[test]
    fn f09_backspace_over_a_four_byte_char() {
        let mut f = at_end("a\u{1F600}");
        f.apply(Edit::Backspace);
        assert_eq!((f.text(), f.caret()), ("a", 1));
    }

    #[test]
    fn f10_backspace_in_the_middle() {
        let mut f = at("abc", 2);
        f.apply(Edit::Backspace);
        assert_eq!((f.text(), f.caret()), ("ac", 1));
    }

    #[test]
    fn f11_delete_at_the_end_is_unchanged() {
        let mut f = at_end("ab");
        assert_eq!(f.apply(Edit::Delete), ModelOut::Unchanged);
    }

    #[test]
    fn f12_delete_forward() {
        let mut f = at("abc", 1);
        assert_eq!(f.apply(Edit::Delete), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("ac", 1));
    }

    #[test]
    fn f13_delete_over_a_two_byte_char() {
        let mut f = at("\u{e9}b", 0);
        f.apply(Edit::Delete);
        assert_eq!((f.text(), f.caret()), ("b", 0));
    }

    #[test]
    fn f14_delete_over_a_four_byte_char() {
        let mut f = at("\u{1F600}b", 0);
        f.apply(Edit::Delete);
        assert_eq!((f.text(), f.caret()), ("b", 0));
    }

    #[test]
    fn f15_home() {
        let mut f = at_end("abc");
        assert_eq!(mv(&mut f, Motion::Home), ModelOut::CaretOnly);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn f16_end() {
        let mut f = at("abc", 1);
        assert_eq!(mv(&mut f, Motion::End), ModelOut::CaretOnly);
        assert_eq!(f.caret(), 3);
    }

    #[test]
    fn f17_home_at_zero_is_unchanged() {
        let mut f = at("abc", 0);
        assert_eq!(mv(&mut f, Motion::Home), ModelOut::Unchanged);
    }

    #[test]
    fn f18_end_at_the_end_is_unchanged() {
        let mut f = at_end("abc");
        assert_eq!(mv(&mut f, Motion::End), ModelOut::Unchanged);
    }

    #[test]
    fn f19_left_steps_over_a_two_byte_char() {
        let mut f = at_end("a\u{e9}");
        mv(&mut f, Motion::CharLeft);
        assert_eq!(f.caret(), 1);
    }

    #[test]
    fn f20_right_steps_over_a_four_byte_char() {
        let mut f = at("a\u{1F600}b", 1);
        mv(&mut f, Motion::CharRight);
        assert_eq!(f.caret(), 5);
    }

    #[test]
    fn f21_left_at_zero_is_unchanged() {
        let mut f = at("ab", 0);
        assert_eq!(mv(&mut f, Motion::CharLeft), ModelOut::Unchanged);
    }

    #[test]
    fn f22_right_at_the_end_is_unchanged() {
        let mut f = at_end("ab");
        assert_eq!(mv(&mut f, Motion::CharRight), ModelOut::Unchanged);
    }

    #[test]
    fn f23_the_caret_is_never_inside_a_char() {
        let mut rng = XorShift::new(0xF1E1D);
        let mut f = FieldModel::new(FieldKind::Plain);
        for _ in 0..1000 {
            let e = match rng.next_u64() % 9 {
                0 | 1 => Edit::Insert(rng.ascii_or_bmp(20)),
                2 => Edit::Backspace,
                3 => Edit::Delete,
                4 => Edit::DeleteWordLeft,
                5 => Edit::Move { by: Motion::CharLeft, extend: false },
                6 => Edit::Move { by: Motion::CharRight, extend: false },
                7 => Edit::Move { by: *rng.pick(&[Motion::Home, Motion::End]), extend: false },
                _ => Edit::SelectAll,
            };
            f.apply(e);
            assert!(f.text().is_char_boundary(f.caret()));
            assert!(f.text().is_char_boundary(f.anchor));
            assert!(f.text().chars().count() <= 256);
        }
    }

    #[test]
    fn f24_select_all() {
        let mut f = at("abc", 1);
        assert_eq!(f.apply(Edit::SelectAll), ModelOut::CaretOnly);
        assert_eq!(f.selection(), Some(0..3));
        assert_eq!(f.selected_text(), Some("abc"));
        assert_eq!(f.apply(Edit::SelectAll), ModelOut::Unchanged);
        assert_eq!(FieldModel::new(FieldKind::Plain).apply(Edit::SelectAll), ModelOut::Unchanged);
    }

    #[test]
    fn f25_select_all_then_insert_replaces() {
        let mut f = at_end("abc");
        f.apply(Edit::SelectAll);
        ins(&mut f, "x");
        assert_eq!((f.text(), f.caret(), f.selection()), ("x", 1, None));
    }

    #[test]
    fn f26_select_all_then_backspace() {
        let mut f = at_end("abc");
        f.apply(Edit::SelectAll);
        assert_eq!(f.apply(Edit::Backspace), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("", 0));
    }

    #[test]
    fn f27_select_all_then_delete() {
        let mut f = at_end("abc");
        f.apply(Edit::SelectAll);
        f.apply(Edit::Delete);
        assert!(f.is_empty());
    }

    #[test]
    fn f28_left_collapses_a_selection_to_its_start() {
        let mut f = at_end("abc");
        f.apply(Edit::SelectAll);
        assert_eq!(mv(&mut f, Motion::CharLeft), ModelOut::CaretOnly);
        assert_eq!((f.caret(), f.selection()), (0, None));
    }

    #[test]
    fn f29_right_collapses_a_selection_to_its_end() {
        let mut f = at("abc", 0);
        f.apply(Edit::SelectAll);
        f.set_caret(3);
        f.apply(Edit::SelectAll);
        mv(&mut f, Motion::CharRight);
        assert_eq!((f.caret(), f.selection()), (3, None));
    }

    #[test]
    fn f30_line_breaks_and_tabs_become_spaces() {
        let mut f = FieldModel::new(FieldKind::Plain);
        ins(&mut f, "a\r\nb\tc");
        assert_eq!(f.text(), "a  b c");
    }

    #[test]
    fn f31_a_300_character_paste_keeps_256() {
        let mut f = FieldModel::new(FieldKind::Plain);
        ins(&mut f, &"x".repeat(300));
        assert_eq!(f.text().chars().count(), 256);
        assert_eq!(ins(&mut f, "y"), ModelOut::Unchanged);
    }

    #[test]
    fn f32_the_selection_makes_room_for_the_insert() {
        let mut f = at_end(&"x".repeat(256));
        f.apply(Edit::SelectAll);
        ins(&mut f, &"y".repeat(300));
        assert_eq!(f.text(), "y".repeat(256));
    }

    #[test]
    fn f33_ctrl_backspace_removes_the_last_word() {
        let mut f = at_end("open  file");
        assert_eq!(f.apply(Edit::DeleteWordLeft), ModelOut::Changed);
        assert_eq!(f.text(), "open  ");
        f.apply(Edit::DeleteWordLeft);
        assert_eq!(f.text(), "");
    }

    #[test]
    fn f34_ctrl_backspace_in_the_middle_of_a_word() {
        let mut f = at("hello world", 8);
        f.apply(Edit::DeleteWordLeft);
        assert_eq!((f.text(), f.caret()), ("hello rld", 6));
    }

    #[test]
    fn f35_punctuation_splits_words() {
        let mut f = at_end("a.b-c");
        f.apply(Edit::DeleteWordLeft);
        assert_eq!(f.text(), "a.b-");
        f.apply(Edit::DeleteWordLeft);
        assert_eq!(f.text(), "a.");
    }

    #[test]
    fn f36_ctrl_backspace_at_zero_is_unchanged() {
        let mut f = at("ab", 0);
        assert_eq!(f.apply(Edit::DeleteWordLeft), ModelOut::Unchanged);
    }

    #[test]
    fn f37_ctrl_backspace_deletes_the_selection_first() {
        let mut f = at_end("one two");
        f.apply(Edit::SelectAll);
        f.apply(Edit::DeleteWordLeft);
        assert_eq!(f.text(), "");
    }

    #[test]
    fn f39_set_text_and_scroll() {
        let mut f = FieldModel::new(FieldKind::Plain);
        f.set_text_selected("abc");
        assert_eq!(f.selection(), Some(0..3));
        f.set_text(&"z".repeat(400));
        assert_eq!((f.text().len(), f.caret(), f.selection()), (256, 256, None));
        f.clear();
        assert!(f.is_empty() && f.caret() == 0);
    }

    #[test]
    fn f40_paste_cut_at_256_on_a_char_boundary() {
        let mut f = at_end(&"x".repeat(255));
        ins(&mut f, "\u{1F600}\u{e9}");
        assert_eq!(f.text().chars().count(), 256);
        assert!(f.text().ends_with('\u{1F600}'));
        assert_eq!(f.caret(), f.text().len());
        f.set_caret(2);
        assert_eq!(f.caret(), 2);
        let mut g = at_end("\u{1F600}");
        g.set_caret(2);
        assert_eq!(g.caret(), 0);
    }

    fn ext(f: &mut FieldModel, by: Motion) -> ModelOut {
        f.apply(Edit::Move { by, extend: true })
    }

    const PATH: &str = "C:\\Program Files\\x.exe";

    #[test]
    fn f41_word_left_over_a_path() {
        let mut f = at_end(PATH);
        let mut seen = Vec::new();
        while mv(&mut f, Motion::WordLeft) == ModelOut::CaretOnly {
            seen.push(f.caret());
        }
        assert_eq!(seen, [19, 17, 11, 3, 0]);
    }

    #[test]
    fn f42_word_right_over_a_path() {
        let mut f = at(PATH, 0);
        let mut seen = Vec::new();
        while mv(&mut f, Motion::WordRight) == ModelOut::CaretOnly {
            seen.push(f.caret());
        }
        assert_eq!(seen, [3, 11, 17, 19, 22]);
    }

    #[test]
    fn f43_word_right_from_the_middle_of_a_word() {
        let mut f = at("hello world", 2);
        mv(&mut f, Motion::WordRight);
        assert_eq!(f.caret(), 6);
    }

    #[test]
    fn f44_word_moves_over_trailing_separators() {
        let mut f = at("ab  ", 0);
        mv(&mut f, Motion::WordRight);
        assert_eq!(f.caret(), 4);
        mv(&mut f, Motion::WordLeft);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn f45_word_moves_over_multibyte_words() {
        let mut f = at_end("caff\u{e8} \u{1F600}x citt\u{e0}");
        mv(&mut f, Motion::WordLeft);
        assert_eq!(f.text().get(f.caret()..), Some("citt\u{e0}"));
        mv(&mut f, Motion::WordLeft);
        assert_eq!(f.text().get(f.caret()..), Some("x citt\u{e0}"));
        mv(&mut f, Motion::WordLeft);
        assert_eq!(f.caret(), 0);
        mv(&mut f, Motion::WordRight);
        // The emoji is no letter: it separates words, so the next word starts after it.
        assert_eq!(f.text().get(f.caret()..), Some("x citt\u{e0}"));
    }

    #[test]
    fn f46_shift_right_extends_from_the_anchor() {
        let mut f = at("abc", 0);
        assert_eq!(ext(&mut f, Motion::CharRight), ModelOut::CaretOnly);
        ext(&mut f, Motion::CharRight);
        assert_eq!((f.selection(), f.selected_text(), f.caret()), (Some(0..2), Some("ab"), 2));
    }

    #[test]
    fn f47_shift_left_then_right_walks_through_the_anchor() {
        let mut f = at("abc", 2);
        ext(&mut f, Motion::CharLeft);
        assert_eq!((f.selection(), f.caret()), (Some(1..2), 1));
        ext(&mut f, Motion::CharRight);
        assert_eq!(f.selection(), None);
        ext(&mut f, Motion::CharRight);
        assert_eq!((f.selection(), f.caret()), (Some(2..3), 3));
    }

    #[test]
    fn f48_shift_home_and_end() {
        let mut f = at("abc", 1);
        ext(&mut f, Motion::Home);
        assert_eq!((f.selection(), f.caret()), (Some(0..1), 0));
        ext(&mut f, Motion::End);
        assert_eq!((f.selection(), f.caret()), (Some(1..3), 3));
        assert_eq!(ext(&mut f, Motion::End), ModelOut::Unchanged);
    }

    #[test]
    fn f49_shift_word_moves_select_words() {
        let mut f = at("one two three", 0);
        ext(&mut f, Motion::WordRight);
        assert_eq!(f.selected_text(), Some("one "));
        ext(&mut f, Motion::WordRight);
        assert_eq!(f.selected_text(), Some("one two "));
        ext(&mut f, Motion::WordLeft);
        assert_eq!(f.selected_text(), Some("one "));
    }

    #[test]
    fn f50_a_selection_by_shift_is_replaced_by_the_next_insert() {
        let mut f = at("one two", 0);
        ext(&mut f, Motion::WordRight);
        assert_eq!(ins(&mut f, "1 "), ModelOut::Changed);
        assert_eq!((f.text(), f.caret(), f.selection()), ("1 two", 2, None));
    }

    #[test]
    fn f51_a_plain_word_move_drops_the_selection_and_goes_on_from_the_caret() {
        let mut f = at("one two", 0);
        ext(&mut f, Motion::WordRight);
        assert_eq!(mv(&mut f, Motion::WordRight), ModelOut::CaretOnly);
        assert_eq!((f.caret(), f.selection()), (7, None));
    }

    #[test]
    fn f52_ctrl_delete_removes_the_word_and_the_gap_after_it() {
        let mut f = at("open  file", 0);
        assert_eq!(f.apply(Edit::DeleteWordRight), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("file", 0));
    }

    #[test]
    fn f53_ctrl_delete_in_the_middle_of_a_word() {
        let mut f = at("hello world", 2);
        f.apply(Edit::DeleteWordRight);
        assert_eq!((f.text(), f.caret()), ("heworld", 2));
    }

    #[test]
    fn f54_ctrl_delete_at_the_end_is_unchanged() {
        let mut f = at_end("ab");
        assert_eq!(f.apply(Edit::DeleteWordRight), ModelOut::Unchanged);
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn f55_ctrl_delete_deletes_the_selection_first() {
        let mut f = at("one two three", 0);
        ext(&mut f, Motion::CharRight);
        f.apply(Edit::DeleteWordRight);
        assert_eq!(f.text(), "ne two three");
    }

    #[test]
    fn f56_ctrl_delete_over_trailing_separators() {
        let mut f = at("ab  ", 2);
        f.apply(Edit::DeleteWordRight);
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn f57_cut_returns_the_selection_and_removes_it() {
        let mut f = at("hello world", 5);
        ext(&mut f, Motion::End);
        assert_eq!(f.apply(Edit::Cut), ModelOut::Cut(" world".to_string()));
        assert_eq!((f.text(), f.caret(), f.selection()), ("hello", 5, None));
    }

    #[test]
    fn f58_cut_of_a_four_byte_char() {
        let mut f = at("a\u{1F600}b", 1);
        ext(&mut f, Motion::CharRight);
        assert_eq!(f.apply(Edit::Cut), ModelOut::Cut("\u{1F600}".to_string()));
        assert_eq!((f.text(), f.caret()), ("ab", 1));
    }

    #[test]
    fn f59_cut_without_a_selection_changes_nothing() {
        let mut f = at("abc", 1);
        let before = f.clone();
        assert_eq!(f.apply(Edit::Cut), ModelOut::Unchanged);
        assert_eq!(
            (f.text(), f.caret(), f.anchor(), f.can_undo()),
            (before.text(), before.caret(), before.anchor(), before.can_undo())
        );
    }

    #[test]
    fn f60_seeded_edits_keep_the_invariants_and_cut_returns_what_left() {
        let mut rng = XorShift::new(0xC07);
        let mut f = FieldModel::new(FieldKind::Plain);
        let motions =
            [Motion::CharLeft, Motion::CharRight, Motion::WordLeft, Motion::WordRight, Motion::Home, Motion::End];
        for _ in 0..5000 {
            let before = f.clone();
            let e = match rng.next_u64() % 8 {
                0 | 1 => Edit::Insert(rng.ascii_or_bmp(12)),
                2 => Edit::DeleteWordRight,
                3 => Edit::DeleteWordLeft,
                4 => Edit::Cut,
                _ => Edit::Move { by: *rng.pick(&motions), extend: rng.next_u64().is_multiple_of(2) },
            };
            let is_cut = e == Edit::Cut;
            let out = f.apply(e);
            assert!(f.text().is_char_boundary(f.caret()) && f.text().is_char_boundary(f.anchor));
            assert!(f.text().chars().count() <= 256);
            if let (true, Some(sel)) = (is_cut, before.selection()) {
                let mut expect = before.text().to_string();
                let removed: String = expect.drain(sel.clone()).collect();
                assert_eq!(out, ModelOut::Cut(removed));
                assert_eq!((f.text(), f.caret(), f.selection()), (expect.as_str(), sel.start, None));
            } else if is_cut {
                assert_eq!(out, ModelOut::Unchanged);
            }
        }
    }

    fn typed(f: &mut FieldModel, s: &str, t0: u64, step: u64) -> u64 {
        let mut t = t0;
        for c in s.chars() {
            f.apply_at(Edit::Insert(c.to_string()), t);
            t += step;
        }
        t
    }

    #[test]
    fn undo_coalesces_typing() {
        let mut f = FieldModel::new(FieldKind::Plain);
        typed(&mut f, "hello", 0, 100);
        assert_eq!(f.text(), "hello");
        assert_eq!(f.apply(Edit::Undo), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("", 0));
        assert_eq!(f.apply(Edit::Undo), ModelOut::Unchanged);
        assert_eq!(f.apply(Edit::Redo), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("hello", 5));
        assert_eq!(f.apply(Edit::Redo), ModelOut::Unchanged);
    }

    #[test]
    fn undo_steps_break_at_a_space_a_pause_and_a_caret_jump() {
        let mut f = FieldModel::new(FieldKind::Plain);
        // A space ends the group: "ab " is one step, "cd" the next.
        typed(&mut f, "ab cd", 0, 10);
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "ab ");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "");
        // A pause of more than 1000 ms.
        let mut g = FieldModel::new(FieldKind::Plain);
        g.apply_at(Edit::Insert("a".into()), 0);
        g.apply_at(Edit::Insert("b".into()), 1000);
        g.apply_at(Edit::Insert("c".into()), 2001);
        g.apply(Edit::Undo);
        assert_eq!(g.text(), "ab");
        // A caret jump.
        let mut h = FieldModel::new(FieldKind::Plain);
        typed(&mut h, "abc", 0, 10);
        h.apply(Edit::Move { by: Motion::Home, extend: false });
        h.apply_at(Edit::Insert("x".into()), 50);
        assert_eq!(h.text(), "xabc");
        h.apply(Edit::Undo);
        assert_eq!(h.text(), "abc");
    }

    #[test]
    fn backspaces_coalesce_apart_from_typing() {
        let mut f = FieldModel::new(FieldKind::Plain);
        typed(&mut f, "abcd", 0, 10);
        for t in [100, 110] {
            f.apply_at(Edit::Backspace, t);
        }
        assert_eq!(f.text(), "ab");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "abcd");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "");
    }

    #[test]
    fn undo_restores_selection() {
        let mut f = at_end("one two");
        f.select(4..7);
        f.apply(Edit::Insert("x".into()));
        assert_eq!(f.text(), "one x");
        f.apply(Edit::Undo);
        assert_eq!((f.text(), f.selection()), ("one two", Some(4..7)));
        assert_eq!(f.caret(), 7);
        f.apply(Edit::Redo);
        assert_eq!((f.text(), f.caret(), f.selection()), ("one x", 5, None));
    }

    #[test]
    fn a_new_edit_drops_the_redo_steps() {
        let mut f = FieldModel::new(FieldKind::Plain);
        typed(&mut f, "ab", 0, 10);
        f.apply(Edit::Undo);
        assert!(f.can_redo());
        f.apply(Edit::Insert("z".into()));
        assert!(!f.can_redo());
    }

    #[test]
    fn the_undo_ring_holds_32_steps() {
        let mut f = FieldModel::new(FieldKind::Plain);
        for i in 0..40u64 {
            f.apply_at(Edit::Insert("ab".into()), i);
        }
        let mut n = 0;
        while f.apply(Edit::Undo) == ModelOut::Changed {
            n += 1;
        }
        assert_eq!(n, 32);
        assert_eq!(f.text().len(), 16);
    }

    #[test]
    fn rename_f2_cycles() {
        let mut f = FieldModel::new(FieldKind::Rename { stem_end: 6 });
        f.set_text_selected("report.txt");
        assert_eq!(f.selection(), Some(0..6));
        f.apply(Edit::CycleRename);
        assert_eq!(f.selection(), Some(0..10));
        f.apply(Edit::CycleRename);
        assert_eq!(f.selection(), Some(7..10));
        f.apply(Edit::CycleRename);
        assert_eq!(f.selection(), Some(0..6));
        // A name without an extension has two steps only: the stem is the whole name.
        let mut g = FieldModel::new(FieldKind::Rename { stem_end: 4 });
        g.set_text_selected("docs");
        g.apply(Edit::CycleRename);
        assert_eq!(g.selection(), Some(0..4));
        // A selection of the person's own goes to the stem.
        f.select(2..3);
        f.apply(Edit::CycleRename);
        assert_eq!(f.selection(), Some(0..6));
    }

    #[test]
    fn refused_char_changes_nothing() {
        fn bad(c: char) -> bool {
            "\\/:*?\"<>|".contains(c)
        }
        let mut f = at_end("ab");
        f.set_filter(Some(bad));
        assert_eq!(f.apply(Edit::Insert("?".into())), ModelOut::Refused('?'));
        assert_eq!(f.apply(Edit::Insert("x:y".into())), ModelOut::Refused(':'));
        assert_eq!((f.text(), f.caret()), ("ab", 2));
        assert!(!f.can_undo());
        assert_eq!(f.apply(Edit::Insert("c".into())), ModelOut::Changed);
        assert_eq!(f.text(), "abc");
    }

    #[test]
    fn composition_not_in_undo_until_commit() {
        let mut f = at_end("ab");
        f.set_composition("xyz", 2);
        assert!(f.is_composing());
        assert_eq!(f.text(), "ab");
        assert_eq!(f.display_text(), "abxyz");
        assert_eq!(f.display_caret(), 4);
        assert!(!f.can_undo());
        f.set_composition("xyzw", 4);
        assert!(!f.can_undo());
        assert_eq!(f.commit("XYZW", 5), ModelOut::Changed);
        assert!(!f.is_composing());
        assert_eq!((f.text(), f.caret()), ("abXYZW", 6));
        assert!(f.can_undo());
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "ab");
        assert!(!f.can_undo());
    }

    #[test]
    fn a_cancelled_composition_leaves_text_and_history() {
        let mut f = at_end("ab");
        f.set_composition("q", 1);
        assert!(f.cancel_composition());
        assert!(!f.cancel_composition());
        assert_eq!((f.text(), f.caret(), f.can_undo()), ("ab", 2, false));
    }

    #[test]
    fn a_committed_character_is_its_own_step() {
        let mut f = FieldModel::new(FieldKind::Plain);
        typed(&mut f, "ab", 0, 10);
        f.set_composition("c", 1);
        f.commit("c", 30);
        assert_eq!(f.text(), "abc");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn surrogate_pairs_are_one_step() {
        let mut f = FieldModel::new(FieldKind::Plain);
        let units: Vec<u16> = "\u{1F600}".encode_utf16().collect();
        assert_eq!(units.len(), 2);
        assert_eq!(f.type_unit(units[0], 0), ModelOut::Unchanged);
        assert_eq!(f.text(), "");
        assert_eq!(f.type_unit(units[1], 1), ModelOut::Changed);
        assert_eq!((f.text(), f.caret()), ("\u{1F600}", 4));
        f.apply(Edit::Backspace);
        assert_eq!(f.text(), "");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "\u{1F600}");
        f.apply(Edit::Undo);
        assert_eq!(f.text(), "");
        // A lone low surrogate and a high one followed by a plain unit are dropped without harm.
        assert_eq!(f.type_unit(0xDC00, 2), ModelOut::Unchanged);
        f.type_unit(0xD83D, 3);
        assert_eq!(f.type_unit(u16::from(b'a'), 4), ModelOut::Changed);
        assert_eq!(f.text(), "a");
    }

    #[test]
    fn the_limit_applies_to_set_max_chars() {
        let mut f = at_end("abcdef");
        f.set_max_chars(3);
        assert_eq!((f.text(), f.caret()), ("abc", 3));
    }
}
