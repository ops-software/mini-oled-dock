//! The system collators (StrCmpLogicalW, LCMapStringEx sort keys) and the WTF-8 helpers. The collators are the
//! reference of the natural order (F 5.5.3): natural::natural_cmp calls them for every name its ASCII fast path does
//! not cover, and the corpus gate (tests/natural.rs) holds the fast path to them.

use windows::core::PCWSTR;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Globalization::{LCMapStringEx, LCMAP_SORTKEY, NORM_IGNORECASE, SORT_DIGITSASNUMBERS};
use windows::Win32::UI::Shell::StrCmpLogicalW;

/// Longest name (in UTF-16 units, without the NUL) that is copied on the stack.
const STACK_UNITS: usize = 260;

/// The flags of the sort-key collator, besides LCMAP_SORTKEY: case-insensitive and digit runs as numbers, the pair of
/// settings under which the keys of this machine agree with StrCmpLogicalW on the corpus (the NAT4 gate pins it).
pub const SORTKEY_FLAGS: u32 = NORM_IGNORECASE.0 | SORT_DIGITSASNUMBERS.0;

pub trait Collator: Send + Sync {
    fn cmp_wide(&self, a: &[u16], b: &[u16]) -> core::cmp::Ordering;
}

pub struct LogicalCollator; // StrCmpLogicalW
pub struct SortKeyCollator; // LCMapStringEx sort keys (the fallback of F 5.5.3)

/// Calls `f` with a NUL-terminated copy of `s` (a stack buffer up to 260 units, the heap beyond). An embedded NUL
/// ends the string for the system call, as it would for any caller of it.
fn with_nul<R>(s: &[u16], f: impl FnOnce(PCWSTR) -> R) -> R {
    if s.len() <= STACK_UNITS {
        let mut buf = [0u16; STACK_UNITS + 1];
        buf[..s.len()].copy_from_slice(s);
        f(PCWSTR(buf.as_ptr()))
    } else {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s);
        v.push(0);
        f(PCWSTR(v.as_ptr()))
    }
}

impl Collator for LogicalCollator {
    fn cmp_wide(&self, a: &[u16], b: &[u16]) -> core::cmp::Ordering {
        with_nul(a, |pa| {
            with_nul(b, |pb| {
                // SAFETY: both pointers address NUL-terminated buffers that live until the call returns.
                let r = unsafe { StrCmpLogicalW(pa, pb) };
                r.cmp(&0)
            })
        })
    }
}

/// StrCmpLogicalW of two slices that already end in a NUL unit (their last element), without the copies of
/// `cmp_wide`; a slice without that NUL goes through `cmp_wide`.
pub(crate) fn logical_cmp_z(a: &[u16], b: &[u16]) -> core::cmp::Ordering {
    match (a.split_last(), b.split_last()) {
        (Some((0, _)), Some((0, _))) => {
            // SAFETY: both slices end in a NUL unit, so both pointers address NUL-terminated strings that live
            // until the call returns.
            let r = unsafe { StrCmpLogicalW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr())) };
            r.cmp(&0)
        }
        _ => LogicalCollator.cmp_wide(a, b),
    }
}

impl SortKeyCollator {
    /// The sort key of `s` under the user's locale: bytes whose memcmp order is the collation order. Empty for an
    /// empty name or when the system refuses (a refusal then orders as the empty name, and the ordinal tie-break of
    /// natural_cmp keeps the order total).
    pub fn key(&self, s: &[u16]) -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }
        let flags = LCMAP_SORTKEY | SORTKEY_FLAGS;
        // The locale name NULL is LOCALE_NAME_USER_DEFAULT. For LCMAP_SORTKEY the destination size counts bytes, so a
        // buffer of n units (2n bytes) is always large enough for n bytes.
        let name = PCWSTR::null();
        // A guess first (a key is about three to four bytes per unit); the exact size is asked only when it fails.
        let mut units = vec![0u16; s.len() * 6 + 16];
        // SAFETY: plain calls with valid slices; nothing is retained.
        let mut w = unsafe { LCMapStringEx(name, flags, s, Some(&mut units), None, None, LPARAM(0)) };
        if w <= 0 {
            // SAFETY: as above; no destination, the call only measures.
            let n = unsafe { LCMapStringEx(name, flags, s, None, None, None, LPARAM(0)) };
            if n <= 0 {
                return Vec::new();
            }
            units = vec![0u16; n as usize];
            // SAFETY: as above; the system writes at most n bytes into the 2n bytes of the buffer.
            w = unsafe { LCMapStringEx(name, flags, s, Some(&mut units), None, None, LPARAM(0)) };
        }
        let mut key: Vec<u8> = units.iter().flat_map(|u| u.to_ne_bytes()).take(w.max(0) as usize).collect();
        key.shrink_to_fit();
        key
    }
}

impl Collator for SortKeyCollator {
    fn cmp_wide(&self, a: &[u16], b: &[u16]) -> core::cmp::Ordering {
        self.key(a).cmp(&self.key(b))
    }
}

/// Appends the UTF-16 form of the WTF-8 bytes `s` to `out`: a lone surrogate (ED A0..BF 80..BF) becomes its own
/// unit, 4-byte sequences become a pair. A byte that starts no valid sequence becomes U+FFFD.
pub fn wtf8_to_utf16(s: &[u8], out: &mut Vec<u16>) {
    wtf8_units(s, |u| out.push(u));
}

/// Calls `emit` with every UTF-16 unit of the WTF-8 bytes `s`, in order (at most one unit per byte).
pub fn wtf8_units(s: &[u8], mut emit: impl FnMut(u16)) {
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        let cont = |k: usize| s.get(i + k).filter(|c| (**c & 0xC0) == 0x80).map(|c| u32::from(*c & 0x3F));
        let (cp, len) = match b {
            0x00..=0x7F => (u32::from(b), 1),
            0xC2..=0xDF => match cont(1) {
                Some(c1) => ((u32::from(b & 0x1F) << 6) | c1, 2),
                None => (0xFFFD, 1),
            },
            0xE0..=0xEF => match (cont(1), cont(2)) {
                (Some(c1), Some(c2)) => {
                    let cp = (u32::from(b & 0x0F) << 12) | (c1 << 6) | c2;
                    if cp < 0x800 {
                        (0xFFFD, 1)
                    } else {
                        (cp, 3)
                    }
                }
                _ => (0xFFFD, 1),
            },
            0xF0..=0xF4 => match (cont(1), cont(2), cont(3)) {
                (Some(c1), Some(c2), Some(c3)) => {
                    let cp = (u32::from(b & 0x07) << 18) | (c1 << 12) | (c2 << 6) | c3;
                    if (0x1_0000..=0x10_FFFF).contains(&cp) {
                        (cp, 4)
                    } else {
                        (0xFFFD, 1)
                    }
                }
                _ => (0xFFFD, 1),
            },
            _ => (0xFFFD, 1),
        };
        if cp >= 0x1_0000 {
            let v = cp - 0x1_0000;
            emit(0xD800 | (v >> 10) as u16);
            emit(0xDC00 | (v & 0x3FF) as u16);
        } else {
            emit(cp as u16);
        }
        i += len;
    }
}

/// Appends the WTF-8 form of the UTF-16 units `s` to `out`: a valid pair becomes one 4-byte sequence, a lone
/// surrogate its 3-byte generalised form, so `wtf8_to_utf16` gives the same units back.
pub fn utf16_to_wtf8(s: &[u16], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < s.len() {
        let u = u32::from(s[i]);
        i += 1;
        let cp = if (0xD800..0xDC00).contains(&u) && s.get(i).is_some_and(|l| (0xDC00..0xE000).contains(l)) {
            let lo = u32::from(s[i]);
            i += 1;
            0x1_0000 + ((u - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            u
        };
        match cp {
            0..=0x7F => out.push(cp as u8),
            0x80..=0x7FF => out.extend_from_slice(&[0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]),
            0x800..=0xFFFF => out.extend_from_slice(&[
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]),
            _ => out.extend_from_slice(&[
                0xF0 | (cp >> 18) as u8,
                0x80 | ((cp >> 12) & 0x3F) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;

    use super::*;
    use crate::listing::natural::corpus::corpus;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn wtf8_round_trips_ten_thousand_corpus_names() {
        let names = corpus(1);
        // Every 5th name covers every class (the corpus is class after class, 50,000 names).
        let mut units = Vec::new();
        let mut back = Vec::new();
        let mut n = 0;
        for name in names.iter().step_by(5) {
            units.clear();
            back.clear();
            wtf8_to_utf16(name, &mut units);
            utf16_to_wtf8(&units, &mut back);
            assert_eq!(&back, name, "round trip of {name:?}");
            n += 1;
        }
        assert_eq!(n, 10_000);
    }

    #[test]
    fn lone_surrogates_and_pairs() {
        let mut v = Vec::new();
        wtf8_to_utf16(&[b'a', 0xED, 0xA0, 0x80, b'b'], &mut v);
        assert_eq!(v, [0x61, 0xD800, 0x62]);
        v.clear();
        wtf8_to_utf16("\u{1F600}".as_bytes(), &mut v);
        assert_eq!(v, [0xD83D, 0xDE00]);
        let mut w = Vec::new();
        utf16_to_wtf8(&[0xDC00, 0x41, 0xD83D], &mut w);
        assert_eq!(w, [0xED, 0xB0, 0x80, 0x41, 0xED, 0xA0, 0xBD]);
        w.clear();
        utf16_to_wtf8(&v, &mut w);
        assert_eq!(w, "\u{1F600}".as_bytes());
        // Garbage in does not panic and gives replacement characters.
        v.clear();
        wtf8_to_utf16(&[0xFF, 0xC0, 0xE2, 0x82], &mut v);
        assert!(v.iter().all(|u| *u == 0xFFFD));
    }

    #[test]
    fn logical_collator_orders_numbers_by_value_and_ignores_case() {
        let c = LogicalCollator;
        assert_eq!(c.cmp_wide(&wide("a2"), &wide("a10")), Ordering::Less);
        assert_eq!(c.cmp_wide(&wide("a10"), &wide("a2")), Ordering::Greater);
        assert_eq!(c.cmp_wide(&wide("A"), &wide("a")), Ordering::Equal);
        assert_eq!(c.cmp_wide(&wide(""), &wide("a")), Ordering::Less);
        let long: Vec<u16> = std::iter::repeat_n(0x61u16, 400).collect();
        assert_eq!(c.cmp_wide(&long, &long), Ordering::Equal, "the heap path");
    }

    #[test]
    fn sort_key_collator_agrees_with_logical_on_ascii_corpus_pairs() {
        let names = corpus(1);
        let (l, k) = (LogicalCollator, SortKeyCollator);
        let ascii: Vec<Vec<u16>> = names
            .iter()
            .filter(|n| n.is_ascii())
            .take(2_000)
            .map(|n| n.iter().map(|b| u16::from(*b)).collect())
            .collect();
        let mut checked = 0;
        let mut disagree = Vec::new();
        for pair in ascii.chunks_exact(2).take(1_000) {
            let (a, b) = (&pair[0], &pair[1]);
            checked += 1;
            let (x, y) = (l.cmp_wide(a, b), k.cmp_wide(a, b));
            if x != Ordering::Equal && y != Ordering::Equal && x != y {
                disagree.push((String::from_utf16_lossy(a), String::from_utf16_lossy(b)));
            }
        }
        assert_eq!(checked, 1_000);
        assert!(
            disagree.is_empty(),
            "orders differ on {} of 1000 pairs, first: {:?}",
            disagree.len(),
            disagree.first()
        );
    }

    #[test]
    fn sort_key_is_empty_for_empty_input() {
        assert!(SortKeyCollator.key(&[]).is_empty());
        assert!(!SortKeyCollator.key(&wide("a")).is_empty());
    }
}
