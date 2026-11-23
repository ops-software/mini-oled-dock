//! The launcher's text field model (SPEC 5.5.3): the text, a caret and an anchor as byte offsets on char boundaries,
//! and the edits of the keyboard (word moves, selection by Shift, Ctrl+Backspace, Ctrl+Delete, cut). Pure.

use super::model::Mods;

/// One key event as the host's key handler passes it on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub vk: u16,
    pub mods: Mods,
    pub repeat: bool,
    /// A WM_SYSKEYDOWN (Alt held).
    pub sys: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    Insert(String),
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    Move { by: Motion, extend: bool },
    SelectAll,
    Cut,
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FieldOut {
    /// Nothing changed: no repaint.
    Unchanged,
    /// The text changed: the query is parsed again.
    Changed,
    /// Only the caret or the selection moved.
    CaretOnly,
    /// The selection was removed (Ctrl+X): the text changed too, and this is what the clipboard gets.
    Cut(String),
}

/// The longest text the field holds, in characters.
pub const MAX_CHARS: u16 = 256;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextField {
    text: String,
    caret: usize,
    anchor: usize,
    max_chars: u16,
}

impl Default for TextField {
    fn default() -> Self {
        TextField::new()
    }
}

/// A character that belongs to a word (the word moves and deletes step over runs of these and runs of the others).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextField {
    pub fn new() -> TextField {
        TextField { text: String::new(), caret: 0, anchor: 0, max_chars: MAX_CHARS }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The selected byte range, None when the caret and the anchor are together.
    pub fn selection(&self) -> Option<core::ops::Range<usize>> {
        (self.caret != self.anchor).then(|| self.caret.min(self.anchor)..self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().and_then(|r| self.text.get(r))
    }

    /// Replaces the whole text (a restored query, a completion): cut to the limit, caret at the end, no selection.
    pub fn set_text(&mut self, s: &str) {
        self.text.clear();
        self.text.extend(s.chars().take(usize::from(self.max_chars)));
        self.caret = self.text.len();
        self.anchor = self.caret;
    }

    /// The same text, fully selected, so that typing replaces it (the keep_query option).
    pub fn set_text_selected(&mut self, s: &str) {
        self.set_text(s);
        self.anchor = 0;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
    }

    /// Puts the caret at a byte offset (clamped, moved back to a char boundary) and drops the selection.
    pub fn set_caret(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.caret = at;
        self.anchor = at;
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

    fn delete_range(&mut self, r: core::ops::Range<usize>) -> FieldOut {
        if r.is_empty() || r.end > self.text.len() {
            return FieldOut::Unchanged;
        }
        self.text.replace_range(r.clone(), "");
        self.caret = r.start;
        self.anchor = r.start;
        FieldOut::Changed
    }

    fn move_to(&mut self, at: usize) -> FieldOut {
        self.move_caret(at, false)
    }

    /// The caret goes to `at`; the anchor follows it unless the selection is being extended.
    fn move_caret(&mut self, at: usize, extend: bool) -> FieldOut {
        let anchor = if extend { self.anchor } else { at };
        let moved = self.caret != at || self.anchor != anchor;
        self.caret = at;
        self.anchor = anchor;
        if moved {
            FieldOut::CaretOnly
        } else {
            FieldOut::Unchanged
        }
    }

    /// Where a motion takes the caret, counted from the caret itself.
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

    pub fn apply(&mut self, e: Edit) -> FieldOut {
        match e {
            Edit::Insert(s) => self.insert(&s),
            Edit::Backspace => match self.selection() {
                Some(r) => self.delete_range(r),
                None => {
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
                self.move_to(at)
            }
            Edit::Move { by: Motion::CharRight, extend: false } if self.selection().is_some() => {
                let at = self.selection().map_or(self.caret, |r| r.end);
                self.move_to(at)
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
                    FieldOut::Cut(removed)
                }
                None => FieldOut::Unchanged,
            },
        }
    }

    fn select_all(&mut self) -> FieldOut {
        if self.text.is_empty() || (self.anchor == 0 && self.caret == self.text.len()) {
            return FieldOut::Unchanged;
        }
        self.anchor = 0;
        self.caret = self.text.len();
        FieldOut::CaretOnly
    }

    /// Replaces the selection with `s`: line breaks and tabs become spaces, other control characters stay (the view
    /// draws markers for them), and the whole text is cut at the limit.
    fn insert(&mut self, s: &str) -> FieldOut {
        let sel = self.selection();
        let removed = sel.as_ref().map_or(0, |r| self.text.get(r.clone()).map_or(0, |t| t.chars().count()));
        let room = usize::from(self.max_chars).saturating_sub(self.char_count() - removed);
        let clean: String =
            s.chars().take(room).map(|c| if matches!(c, '\r' | '\n' | '\t') { ' ' } else { c }).collect();
        if clean.is_empty() && sel.is_none() {
            return FieldOut::Unchanged;
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
        FieldOut::Changed
    }
}

/// The privacy marks a clipboard can carry (plan L2 D8), read before its text: the registered formats
/// `ExcludeClipboardContentFromMonitorProcessing` and `Clipboard Viewer Ignore` (present or not), and the DWORD of
/// `CanIncludeInClipboardHistory`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ClipMarks {
    pub exclude_monitor: bool,
    pub viewer_ignore: bool,
    /// None: the format is absent; the reader gives 0 for one it cannot read whole.
    pub history: Option<u32>,
}

/// A paste the app that copied the text marked private: refused, and its text is never read (CLIP-1).
pub fn paste_refused(marks: ClipMarks) -> bool {
    marks.exclude_monitor || marks.viewer_ignore || marks.history == Some(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    fn at_end(s: &str) -> TextField {
        let mut f = TextField::new();
        f.set_text(s);
        f
    }

    fn at(s: &str, caret: usize) -> TextField {
        let mut f = at_end(s);
        f.set_caret(caret);
        f
    }

    fn ins(f: &mut TextField, s: &str) -> FieldOut {
        f.apply(Edit::Insert(s.to_string()))
    }

    fn mv(f: &mut TextField, by: Motion) -> FieldOut {
        f.apply(Edit::Move { by, extend: false })
    }

    #[test]
    fn f01_insert_ascii() {
        let mut f = TextField::new();
        assert_eq!(ins(&mut f, "abc"), FieldOut::Changed);
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
        assert_eq!(ins(&mut f, ""), FieldOut::Unchanged);
    }

    #[test]
    fn f06_backspace_at_zero_is_unchanged() {
        let mut f = at("ab", 0);
        assert_eq!(f.apply(Edit::Backspace), FieldOut::Unchanged);
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn f07_backspace_ascii() {
        let mut f = at_end("abc");
        assert_eq!(f.apply(Edit::Backspace), FieldOut::Changed);
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
        assert_eq!(f.apply(Edit::Delete), FieldOut::Unchanged);
    }

    #[test]
    fn f12_delete_forward() {
        let mut f = at("abc", 1);
        assert_eq!(f.apply(Edit::Delete), FieldOut::Changed);
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
        assert_eq!(mv(&mut f, Motion::Home), FieldOut::CaretOnly);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn f16_end() {
        let mut f = at("abc", 1);
        assert_eq!(mv(&mut f, Motion::End), FieldOut::CaretOnly);
        assert_eq!(f.caret(), 3);
    }

    #[test]
    fn f17_home_at_zero_is_unchanged() {
        let mut f = at("abc", 0);
        assert_eq!(mv(&mut f, Motion::Home), FieldOut::Unchanged);
    }

    #[test]
    fn f18_end_at_the_end_is_unchanged() {
        let mut f = at_end("abc");
        assert_eq!(mv(&mut f, Motion::End), FieldOut::Unchanged);
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
        assert_eq!(mv(&mut f, Motion::CharLeft), FieldOut::Unchanged);
    }

    #[test]
    fn f22_right_at_the_end_is_unchanged() {
        let mut f = at_end("ab");
        assert_eq!(mv(&mut f, Motion::CharRight), FieldOut::Unchanged);
    }

    #[test]
    fn f23_the_caret_is_never_inside_a_char() {
        let mut rng = XorShift::new(0xF1E1D);
        let mut f = TextField::new();
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
        assert_eq!(f.apply(Edit::SelectAll), FieldOut::CaretOnly);
        assert_eq!(f.selection(), Some(0..3));
        assert_eq!(f.selected_text(), Some("abc"));
        assert_eq!(f.apply(Edit::SelectAll), FieldOut::Unchanged);
        assert_eq!(TextField::new().apply(Edit::SelectAll), FieldOut::Unchanged);
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
        assert_eq!(f.apply(Edit::Backspace), FieldOut::Changed);
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
        assert_eq!(mv(&mut f, Motion::CharLeft), FieldOut::CaretOnly);
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
        let mut f = TextField::new();
        ins(&mut f, "a\r\nb\tc");
        assert_eq!(f.text(), "a  b c");
    }

    #[test]
    fn f31_a_300_character_paste_keeps_256() {
        let mut f = TextField::new();
        ins(&mut f, &"x".repeat(300));
        assert_eq!(f.text().chars().count(), 256);
        assert_eq!(ins(&mut f, "y"), FieldOut::Unchanged);
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
        assert_eq!(f.apply(Edit::DeleteWordLeft), FieldOut::Changed);
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
        assert_eq!(f.apply(Edit::DeleteWordLeft), FieldOut::Unchanged);
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
        let mut f = TextField::new();
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

    fn ext(f: &mut TextField, by: Motion) -> FieldOut {
        f.apply(Edit::Move { by, extend: true })
    }

    const PATH: &str = "C:\\Program Files\\x.exe";

    #[test]
    fn f41_word_left_over_a_path() {
        let mut f = at_end(PATH);
        let mut seen = Vec::new();
        while mv(&mut f, Motion::WordLeft) == FieldOut::CaretOnly {
            seen.push(f.caret());
        }
        assert_eq!(seen, [19, 17, 11, 3, 0]);
    }

    #[test]
    fn f42_word_right_over_a_path() {
        let mut f = at(PATH, 0);
        let mut seen = Vec::new();
        while mv(&mut f, Motion::WordRight) == FieldOut::CaretOnly {
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
        assert_eq!(ext(&mut f, Motion::CharRight), FieldOut::CaretOnly);
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
        assert_eq!(ext(&mut f, Motion::End), FieldOut::Unchanged);
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
        assert_eq!(ins(&mut f, "1 "), FieldOut::Changed);
        assert_eq!((f.text(), f.caret(), f.selection()), ("1 two", 2, None));
    }

    #[test]
    fn f51_a_plain_word_move_drops_the_selection_and_goes_on_from_the_caret() {
        let mut f = at("one two", 0);
        ext(&mut f, Motion::WordRight);
        assert_eq!(mv(&mut f, Motion::WordRight), FieldOut::CaretOnly);
        assert_eq!((f.caret(), f.selection()), (7, None));
    }

    #[test]
    fn f52_ctrl_delete_removes_the_word_and_the_gap_after_it() {
        let mut f = at("open  file", 0);
        assert_eq!(f.apply(Edit::DeleteWordRight), FieldOut::Changed);
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
        assert_eq!(f.apply(Edit::DeleteWordRight), FieldOut::Unchanged);
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
        assert_eq!(f.apply(Edit::Cut), FieldOut::Cut(" world".to_string()));
        assert_eq!((f.text(), f.caret(), f.selection()), ("hello", 5, None));
    }

    #[test]
    fn f58_cut_of_a_four_byte_char() {
        let mut f = at("a\u{1F600}b", 1);
        ext(&mut f, Motion::CharRight);
        assert_eq!(f.apply(Edit::Cut), FieldOut::Cut("\u{1F600}".to_string()));
        assert_eq!((f.text(), f.caret()), ("ab", 1));
    }

    #[test]
    fn f59_cut_without_a_selection_changes_nothing() {
        let mut f = at("abc", 1);
        let before = f.clone();
        assert_eq!(f.apply(Edit::Cut), FieldOut::Unchanged);
        assert_eq!(f, before);
    }

    #[test]
    fn f60_seeded_edits_keep_the_invariants_and_cut_returns_what_left() {
        let mut rng = XorShift::new(0xC07);
        let mut f = TextField::new();
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
                assert_eq!(out, FieldOut::Cut(removed));
                assert_eq!((f.text(), f.caret(), f.selection()), (expect.as_str(), sel.start, None));
            } else if is_cut {
                assert_eq!(out, FieldOut::Unchanged);
            }
        }
    }

    #[test]
    fn paste_refused_vectors() {
        let none = ClipMarks::default();
        assert!(!paste_refused(none), "no mark: pasted");
        assert!(!paste_refused(ClipMarks { history: Some(1), ..none }), "history allowed and no mark: pasted");
        // Each mark alone refuses.
        assert!(paste_refused(ClipMarks { exclude_monitor: true, ..none }));
        assert!(paste_refused(ClipMarks { viewer_ignore: true, ..none }));
        assert!(paste_refused(ClipMarks { history: Some(0), ..none }));
        // A mark refuses whatever the history flag says.
        assert!(paste_refused(ClipMarks { exclude_monitor: true, history: Some(1), ..none }));
        assert!(paste_refused(ClipMarks { viewer_ignore: true, history: Some(1), ..none }));
        assert!(paste_refused(ClipMarks { exclude_monitor: true, viewer_ignore: true, history: Some(0) }));
    }
}
