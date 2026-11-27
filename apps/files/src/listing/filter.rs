//! The name filter (ST4): a case-insensitive substring of the name. ASCII names against an ASCII query (the common
//! case) are compared byte by byte with an ASCII fold; any other pair is folded to lower-case WTF-8 first.

use super::arena::lower_wtf8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterQuery {
    pub text: Box<str>,
} // v1: case-insensitive substring of the name

/// A query prepared once for a whole list.
pub(crate) struct Needle {
    lower: Vec<u8>,
    ascii: bool,
}

impl Needle {
    pub fn new(q: &FilterQuery) -> Self {
        let lower = lower_wtf8(q.text.as_bytes());
        let ascii = lower.is_ascii();
        Needle { lower, ascii }
    }

    pub fn is_empty(&self) -> bool {
        self.lower.is_empty()
    }

    pub fn matches(&self, name: &[u8]) -> bool {
        let n = self.lower.len();
        if n == 0 {
            return true;
        }
        match (self.ascii, name.is_ascii()) {
            (true, true) => name.windows(n).any(|w| w.eq_ignore_ascii_case(&self.lower)),
            (false, true) => false, // the folded query holds a non-ASCII character that no ASCII name has
            _ => lower_wtf8(name).windows(n).any(|w| w == self.lower.as_slice()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::tests::{batch, names, order_names, store};
    use super::super::Gen;
    use super::*;

    fn q(t: &str) -> FilterQuery {
        FilterQuery { text: t.into() }
    }

    #[test]
    fn filter_subsequence_of_order() {
        let mut s = store();
        s.append(batch(1, &names(400), true));
        s.merge_pending();
        let all = order_names(&s);
        let got = s.filter(&q("ta"), Gen(1));
        let names_got: Vec<String> =
            got.0.iter().map(|id| String::from_utf8(s.name(*id).unwrap().to_vec()).unwrap()).collect();
        let want: Vec<String> = all.iter().filter(|n| n.to_lowercase().contains("ta")).cloned().collect();
        assert!(!want.is_empty() && want.len() < all.len());
        assert_eq!(names_got, want, "the matches, in the order of order()");
        assert_eq!(s.filter(&q(""), Gen(1)), *s.order(), "no text: every row");
        assert!(s.filter(&q("no such name"), Gen(1)).0.is_empty());
    }

    #[test]
    fn filter_case_folds() {
        let mut s = store();
        s.append(batch(1, &[("Straße.TXT".into(), 0x20), ("ÉCOLE".into(), 0x20), ("plain".into(), 0x20)], true));
        s.merge_pending();
        let count = |t: &str| s.filter(&q(t), Gen(1)).0.len();
        assert_eq!(count("txt"), 1);
        assert_eq!(count("TXT"), 1);
        assert_eq!(count("école"), 1);
        assert_eq!(count("École"), 1);
        assert_eq!(count("ss"), 0, "sharp s is not folded to ss");
        assert_eq!(count("ß"), 1);
        assert_eq!(count("é"), 1);
        assert_eq!(count("LAIN"), 1);
    }

    #[test]
    fn filter_stale_gen_is_empty() {
        let mut s = store();
        s.append(batch(1, &names(20), true));
        s.merge_pending();
        assert!(s.filter(&q("a"), Gen(2)).0.is_empty());
        assert!(!s.filter(&q("a"), Gen(1)).0.is_empty());
    }
}
