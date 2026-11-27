//! Hit testing and caret positions in a laid-out text (P2-kit 2.13: called by GdiTextEngine; K2b.2). A caret stands
//! only between clusters: a base character with its combining marks, a joined emoji sequence, a flag (two regional
//! indicators) and CR LF are one stop. The clusters are cut here from the characters (the engine hands over the
//! cumulative width of each character, not a device context), the fallback the plan names for a failed Uniscribe
//! analysis, and the only path.

#![allow(dead_code)] // The stop helpers are the TextField's (K2b.3).

use super::engine::{TextHit, TextLayout};

/// True for a character that continues the cluster before it: a combining mark, a joiner, a variation selector, an
/// emoji modifier or a tag.
fn extends(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8
        | 0x06EA..=0x06ED | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x200C | 0x200D | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF)
}

fn regional(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// The cluster stops of `text`: for each cluster the byte offset where it starts and the index of its last character.
fn clusters(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut prev: Option<char> = None;
    let mut ri_run = 0;
    for (i, (byte, c)) in text.char_indices().enumerate() {
        let join = match prev {
            None => false,
            Some(p) => {
                extends(c) && c != '\r' && c != '\n'
                    || p == '\u{200D}'
                    || (p == '\r' && c == '\n')
                    || (regional(p) && regional(c) && ri_run % 2 == 1)
            }
        };
        ri_run = if regional(c) { ri_run + 1 } else { 0 };
        if join {
            if let Some(last) = out.last_mut() {
                last.1 = i;
            }
        } else {
            out.push((byte, i));
        }
        prev = Some(c);
    }
    out
}

/// Every caret stop of `text` as byte offsets, the end included.
pub(crate) fn stops(text: &str) -> Vec<usize> {
    let mut v: Vec<usize> = clusters(text).into_iter().map(|c| c.0).collect();
    v.push(text.len());
    v
}

/// The stop at or before `index` (a byte offset inside a cluster snaps to its start).
pub(crate) fn snap_down(text: &str, index: usize) -> usize {
    stops(text).into_iter().rev().find(|s| *s <= index).unwrap_or(0)
}

/// The stop at or after `index`.
pub(crate) fn snap_up(text: &str, index: usize) -> usize {
    stops(text).into_iter().find(|s| *s >= index).unwrap_or(text.len())
}

/// The next stop after `index` (the end of the text past the last one).
pub(crate) fn next_stop(text: &str, index: usize) -> usize {
    stops(text).into_iter().find(|s| *s > index).unwrap_or(text.len())
}

/// The previous stop before `index` (0 at the start).
pub(crate) fn prev_stop(text: &str, index: usize) -> usize {
    stops(text).into_iter().rev().find(|s| *s < index).unwrap_or(0)
}

/// The cluster boundary of `l.text` nearest to `x`. `extents[i]` is the width up to and including character i
/// (Gdi::char_extents); missing extents count as 0. `index` is the start of the cluster under `x`; `trailing` is true
/// on its right half.
pub(crate) fn hit_test(l: &TextLayout, extents: &[i32], x: i32) -> TextHit {
    let mut left = 0;
    for (byte, last) in clusters(&l.text) {
        let right = extents.get(last).copied().unwrap_or(left);
        if x < right {
            let trailing = x * 2 >= left + right;
            return TextHit { index: byte, trailing };
        }
        left = right;
    }
    TextHit { index: l.text.len(), trailing: false }
}

/// The x of the caret before the cluster holding the character at byte `index` (the end of the text past the last
/// one).
pub(crate) fn caret_x(l: &TextLayout, extents: &[i32], index: usize) -> i32 {
    let mut left = 0;
    let cl = clusters(&l.text);
    for (n, (byte, last)) in cl.iter().enumerate() {
        let end = cl.get(n + 1).map_or(l.text.len(), |c| c.0);
        if index <= *byte || index < end {
            // At the cluster's start, or inside it: it snaps to the start.
            return left;
        }
        left = extents.get(*last).copied().unwrap_or(left);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::FontId;

    fn layout(s: &str) -> TextLayout {
        TextLayout {
            text: s.to_string(),
            font: FontId(0),
            width: 0,
            height: 0,
            lines: vec![(0..s.len(), 0)],
            trimmed: false,
        }
    }

    /// Ten pixels per character, cumulative.
    fn ext(s: &str) -> Vec<i32> {
        (1..=s.chars().count() as i32).map(|i| i * 10).collect()
    }

    #[test]
    fn boundaries_by_cumulative_extents() {
        let l = layout("a\u{e8}c");
        let ext = [10, 20, 30];
        assert_eq!(hit_test(&l, &ext, 3), TextHit { index: 0, trailing: false });
        assert_eq!(hit_test(&l, &ext, 7), TextHit { index: 0, trailing: true });
        assert_eq!(hit_test(&l, &ext, 12), TextHit { index: 1, trailing: false });
        assert_eq!(hit_test(&l, &ext, 99), TextHit { index: 4, trailing: false });
        assert_eq!(caret_x(&l, &ext, 0), 0);
        assert_eq!(caret_x(&l, &ext, 1), 10);
        assert_eq!(caret_x(&l, &ext, 3), 20);
        assert_eq!(caret_x(&l, &ext, 4), 30);
        assert_eq!(hit_test(&l, &[], 5), TextHit { index: 4, trailing: false });
    }

    #[test]
    fn caret_skips_combining_mark() {
        let s = "cafe\u{301}";
        // c a f and the e with its mark: four clusters, five stops with the end (not six).
        assert_eq!(stops(s), [0, 1, 2, 3, 6]);
        assert_eq!(snap_down(s, 4), 3);
        assert_eq!(next_stop(s, 3), 6);
        assert_eq!(prev_stop(s, 6), 3);
        let l = layout(s);
        let ext = ext(s);
        // The mark has its own width entry; the cluster spans both.
        assert_eq!(caret_x(&l, &ext, 3), 30);
        assert_eq!(caret_x(&l, &ext, 4), 30, "inside the cluster: snaps to its start");
        assert_eq!(caret_x(&l, &ext, 6), 50);
        assert_eq!(hit_test(&l, &ext, 41), TextHit { index: 3, trailing: true });
    }

    #[test]
    fn emoji_pair_is_one_stop() {
        // A flag is two regional indicators; a family is joined with zero-width joiners.
        let flag = "\u{1F1EE}\u{1F1F9}x";
        assert_eq!(stops(flag), [0, 8, 9]);
        let two_flags = "\u{1F1EE}\u{1F1F9}\u{1F1EB}\u{1F1F7}";
        assert_eq!(stops(two_flags), [0, 8, 16]);
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}!";
        assert_eq!(stops(family), [0, family.len() - 1, family.len()]);
        assert_eq!(stops("\r\nx"), [0, 2, 3]);
        assert_eq!(stops("a\u{1F44D}\u{1F3FD}"), [0, 1, 9]);
    }

    #[test]
    fn x_past_end_is_trailing_at_len() {
        let l = layout("ab");
        let e = ext("ab");
        assert_eq!(hit_test(&l, &e, 500).index, 2);
        assert_eq!(hit_test(&l, &e, 20).index, 2);
        assert_eq!(hit_test(&layout(""), &[], 5), TextHit { index: 0, trailing: false });
    }

    #[test]
    fn caret_x_monotonic() {
        let s = "a\u{301}b\u{1F1EE}\u{1F1F9}c";
        let l = layout(s);
        let e = ext(s);
        let mut last = -1;
        for i in 0..=s.len() {
            if !s.is_char_boundary(i) {
                continue;
            }
            let x = caret_x(&l, &e, i);
            assert!(x >= last, "x went back at {i}: {x} < {last}");
            last = x;
        }
    }
}
