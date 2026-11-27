//! Trimming a run of text to a width (P2-kit 2.3) and wrapping it over lines. Both take the measuring function as a
//! parameter, so they are tested without a font.

use std::ops::Range;

/// How a layout too wide for its max width is shortened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Trim {
    /// Never shortened.
    None,
    /// "Long na…" (the dock's ellipsize).
    #[default]
    End,
    /// "very-long…ft-07.docx": keeps the last `tail` chars (files uses 10).
    Middle { tail: u8 },
    /// "…\folder\name": keeps the end (paths).
    Start,
}

const ELLIPSIS: char = '\u{2026}';

/// `s` shortened as `trim` says so that `width(result) <= max_w`; `None` when it fits already or when `trim` is
/// Trim::None. Trim::End is the dock's ellipsize: the same binary search, the prefix's trailing blanks cut before the
/// ellipsis.
pub(crate) fn trim_to(s: &str, trim: Trim, max_w: i32, width: &mut dyn FnMut(&str) -> i32) -> Option<String> {
    if trim == Trim::None || width(s) <= max_w {
        return None;
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    // The longest k in 0..=hi for which build(k) fits (build(0) is the fallback when nothing fits).
    let mut search = |hi: usize, build: &dyn Fn(usize) -> String| -> String {
        let (mut lo, mut hi) = (0usize, hi);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if width(&build(mid)) <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        build(lo)
    };
    Some(match trim {
        Trim::None => return None,
        Trim::End => search(n, &|k| {
            let mut out: String = chars[..k].iter().collect();
            out = out.trim_end().to_string();
            out.push(ELLIPSIS);
            out
        }),
        Trim::Middle { tail } => {
            let tail = usize::from(tail).min(n);
            search(n - tail, &|k| {
                let mut out: String = chars[..k].iter().collect();
                out = out.trim_end().to_string();
                out.push(ELLIPSIS);
                out.extend(chars[n - tail..].iter());
                out
            })
        }
        Trim::Start => search(n, &|k| {
            let mut out = String::new();
            out.push(ELLIPSIS);
            out.extend(chars[n - k..].iter());
            out
        }),
    })
}

/// The lines of `s` greedily wrapped at spaces to `max_w` (a newline always breaks): byte ranges into `s`, trailing
/// blanks left out. A line is wider than `max_w` only when a single word is.
pub(crate) fn wrap_lines(s: &str, max_w: i32, width: &mut dyn FnMut(&str) -> i32) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut para_start = 0;
    for para in s.split('\n') {
        let mut line: Option<(usize, usize)> = None; // the first word's start and the last word's end
        let mut pos = para_start;
        for word in para.split(' ') {
            let (ws, we) = (pos, pos + word.len());
            pos = we + 1;
            if word.is_empty() {
                continue;
            }
            line = match line {
                None => Some((ws, we)),
                Some((ls, _)) if width(&s[ls..we]) <= max_w => Some((ls, we)),
                Some((ls, le)) => {
                    lines.push(ls..le);
                    Some((ws, we))
                }
            };
        }
        lines.push(line.map_or(para_start..para_start, |(a, b)| a..b));
        para_start += para.len() + 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w8(s: &str) -> i32 {
        s.chars().count() as i32 * 8
    }

    #[test]
    fn end_keeps_the_longest_prefix() {
        assert_eq!(trim_to("hello", Trim::End, 100, &mut w8), None);
        assert_eq!(trim_to("hello world", Trim::End, 5 * 8, &mut w8).unwrap(), "hell\u{2026}");
        // The blank before the ellipsis is cut.
        assert_eq!(trim_to("hello world", Trim::End, 7 * 8, &mut w8).unwrap(), "hello\u{2026}");
        assert_eq!(trim_to("hello world", Trim::None, 8, &mut w8), None);
        assert_eq!(trim_to("hello", Trim::End, 0, &mut w8).unwrap(), "\u{2026}");
    }

    #[test]
    fn middle_and_start_keep_the_tail() {
        let r =
            trim_to("very-long-name-of-the-report-draft-07.docx", Trim::Middle { tail: 10 }, 20 * 8, &mut w8).unwrap();
        assert!(r.ends_with("ft-07.docx") && r.contains('\u{2026}') && w8(&r) <= 160, "{r}");
        let p = trim_to("C:\\folder\\sub\\name.txt", Trim::Start, 12 * 8, &mut w8).unwrap();
        assert!(p.starts_with('\u{2026}') && p.ends_with("name.txt") && w8(&p) <= 96, "{p}");
    }

    #[test]
    fn wrap_is_greedy_and_keeps_long_words() {
        let s = "one two three four";
        let l: Vec<&str> = wrap_lines(s, 9 * 8, &mut w8).iter().map(|r| &s[r.clone()]).collect();
        assert_eq!(l, ["one two", "three", "four"]);
        let s = "a verylongword b";
        let l: Vec<&str> = wrap_lines(s, 5 * 8, &mut w8).iter().map(|r| &s[r.clone()]).collect();
        assert_eq!(l, ["a", "verylongword", "b"]);
        let s = "x\ny";
        let l: Vec<&str> = wrap_lines(s, 100, &mut w8).iter().map(|r| &s[r.clone()]).collect();
        assert_eq!(l, ["x", "y"]);
        assert_eq!(wrap_lines("", 100, &mut w8), vec![0..0]);
    }
}
