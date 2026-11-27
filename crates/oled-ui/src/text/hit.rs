//! Hit testing and caret positions in a laid-out text (P2-kit 2.13: called by GdiTextEngine; filled by K2b.2 with
//! clusters). Until then: cumulative character extents at character boundaries.

use super::engine::{TextHit, TextLayout};

/// The character boundary of `l.text` nearest to `x`. `extents[i]` is the width up to and including character i
/// (Gdi::char_extents); missing extents count as 0.
pub(crate) fn hit_test(l: &TextLayout, extents: &[i32], x: i32) -> TextHit {
    let mut left = 0;
    for (i, (byte, _)) in l.text.char_indices().enumerate() {
        let right = extents.get(i).copied().unwrap_or(left);
        if x < right {
            let trailing = x * 2 >= left + right;
            return TextHit { index: byte, trailing };
        }
        left = right;
    }
    TextHit { index: l.text.len(), trailing: false }
}

/// The x of the caret before the character at byte `index` (the end of the text past the last one).
pub(crate) fn caret_x(l: &TextLayout, extents: &[i32], index: usize) -> i32 {
    let mut left = 0;
    for (i, (byte, _)) in l.text.char_indices().enumerate() {
        if byte >= index {
            return left;
        }
        left = extents.get(i).copied().unwrap_or(left);
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

    #[test]
    fn boundaries_by_cumulative_extents() {
        let l = layout("aèc");
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
}
