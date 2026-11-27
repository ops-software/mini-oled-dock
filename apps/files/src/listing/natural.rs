//! Explorer's natural order on WTF-8 names (F 5.5.3). Names made only of "plain" ASCII bytes are compared by a fast
//! path that reproduces StrCmpLogicalW on this machine; every other name goes through the system collator on UTF-16
//! copies. Both end in an ordinal compare of the bytes, so natural_cmp is a total order (a name is Equal only to
//! itself). The fast path is off until self_check has proved it against the collator in this process.

use core::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as Atomic};
use std::sync::OnceLock;

use super::collate::{logical_cmp_z, wtf8_to_utf16, wtf8_units, SortKeyCollator};

/// The plain punctuation bytes in the order StrCmpLogicalW sorts them (derived on this machine and pinned here; the
/// corpus gate of tests/natural.rs re-derives it on every machine that runs the tests). Hyphen and apostrophe are not
/// plain: Windows' word sort treats them specially.
const PUNCT: &[u8] = b" !#$%&(),.;@[]^_{}~+=";

/// The primary weight of the digits (all equal: digit runs are compared by value): after the punctuation, before letters.
const DIGIT_WEIGHT: u8 = PUNCT.len() as u8 + 1;

/// The primary weight of every plain byte, upper and lower case equal; 0 for a byte that is not plain.
const WEIGHT: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut i = 0;
    while i < PUNCT.len() {
        t[PUNCT[i] as usize] = i as u8 + 1;
        i += 1;
    }
    let mut d = b'0';
    while d <= b'9' {
        t[d as usize] = DIGIT_WEIGHT;
        d += 1;
    }
    let mut l = 0;
    while l < 26 {
        t[(b'a' + l) as usize] = DIGIT_WEIGHT + 1 + l;
        t[(b'A' + l) as usize] = DIGIT_WEIGHT + 1 + l;
        l += 1;
    }
    t
};

/// Longest name (bytes) whose UTF-16 copy lives on the stack: a name never has more units than bytes.
const STACK_UNITS: usize = 260;

/// The most digits StrCmpLogicalW reads into one number (10^19 fits a u64).
const DIGIT_CHUNK: usize = 19;

/// True while the ASCII fast path is enabled: set by a passing self_check, cleared (for good) by a failing one.
static FAST: AtomicBool = AtomicBool::new(false);

fn weight(b: u8) -> u8 {
    WEIGHT.get(usize::from(b)).copied().unwrap_or(0)
}

fn is_plain(b: u8) -> bool {
    weight(b) != 0
}

fn all_plain(s: &[u8]) -> bool {
    s.iter().all(|b| is_plain(*b))
}

/// The longest digit chunk at the start of `s`: at most 19 digits (a run longer than that is compared chunk by chunk,
/// as StrCmpLogicalW does on this machine, which reads each chunk into a 64-bit number), its length and its value.
fn chunk(s: &[u8]) -> (usize, u64) {
    let digits = s.iter().take(DIGIT_CHUNK).take_while(|d| d.is_ascii_digit());
    digits.fold((0, 0u64), |(n, v), d| (n + 1, v * 10 + u64::from(d - b'0')))
}

/// The order of two plain names (the fast path). Digits compare by value in chunks of at most 19; chunks of equal value
/// order the one with more leading zeros first, at once (as StrCmpLogicalW does: "a1b" sorts after "a01c"); everything
/// else by primary weight (case-insensitive); the end of a name sorts first; a full tie ends in an ordinal compare of
/// the bytes.
fn fast_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    loop {
        let (x, y) = match (a.get(i), b.get(j)) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (*x, *y),
        };
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let (la, va) = chunk(&a[i..]);
            let (lb, vb) = chunk(&b[j..]);
            // Equal values: the chunk with more leading zeros (the longer text) first, at once.
            let o = va.cmp(&vb).then_with(|| lb.cmp(&la));
            if o != Ordering::Equal {
                return o;
            }
            (i, j) = (i + la, j + lb);
        } else {
            let (wx, wy) = (weight(x), weight(y));
            if wx != wy {
                return wx.cmp(&wy);
            }
            i += 1;
            j += 1;
        }
    }
}

/// Calls `f` with the UTF-16 form of the WTF-8 name `s`: on the stack up to 260 bytes, on the heap beyond.
fn with_wide<R>(s: &[u8], f: impl FnOnce(&[u16]) -> R) -> R {
    if s.len() <= STACK_UNITS {
        let mut buf = [0u16; STACK_UNITS + 1];
        let mut n = 0;
        wtf8_units(s, |u| {
            buf[n] = u;
            n += 1;
        });
        f(&buf[..=n])
    } else {
        let mut v = Vec::with_capacity(s.len() + 1);
        wtf8_units(s, |u| v.push(u));
        v.push(0);
        f(&v)
    }
}

/// The system order of any two names, then ordinal.
fn collator_cmp(a: &[u8], b: &[u8]) -> Ordering {
    with_wide(a, |wa| with_wide(b, |wb| logical_cmp_z(wa, wb))).then_with(|| a.cmp(b))
}

/// WTF-8; Explorer's order on this machine, then ordinal.
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    if FAST.load(Atomic::Relaxed) && all_plain(a) && all_plain(b) {
        fast_cmp(a, b)
    } else {
        collator_cmp(a, b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameKey {
    pub prefix: u32,
    pub plain: bool,
}

/// The key of a name: `plain` when the fast path covers it, and `prefix`, the first four characters packed one byte
/// each (0 for the end of the name, else weight + 2; the digits share one weight and the prefix stops after the first
/// of them, because digit runs compare by value). For two plain names, different prefixes are their order.
pub fn name_key(name: &[u8]) -> NameKey {
    let mut prefix = 0u32;
    let mut digit = false;
    for k in 0..4 {
        let byte = match name.get(k) {
            Some(b) if !digit => {
                digit = b.is_ascii_digit();
                weight(*b) + 2
            }
            _ => 0,
        };
        prefix |= u32::from(byte) << (24 - 8 * k);
    }
    NameKey { prefix, plain: all_plain(name) }
}

pub fn natural_cmp_keyed(a: &[u8], ka: NameKey, b: &[u8], kb: NameKey) -> Ordering {
    if ka.plain && kb.plain && FAST.load(Atomic::Relaxed) {
        if ka.prefix != kb.prefix {
            return ka.prefix.cmp(&kb.prefix);
        }
        return fast_cmp(a, b);
    }
    natural_cmp(a, b)
}

/// A row of `sort_rows`: the key and the WTF-8 name.
pub type Row<'a> = (NameKey, &'a [u8]);

/// Sorts above this many rows precompute the system sort keys of the non-plain names (see `sort_rows`).
pub const KEYED_SORT_ROWS: usize = 20_000;

/// Sorts rows (key and WTF-8 name) into natural order. Up to `KEYED_SORT_ROWS` rows sort with natural_cmp_keyed. Above
/// that, one StrCmpLogicalW call per comparison is the cost (about 200 ns), so the plain names sort by the fast path,
/// the others by system sort keys computed once each (SortKeyCollator, which tests/natural.rs holds to the system
/// order on the corpus), and the two sorted runs merge. The result is the order of sorting by natural_cmp.
pub fn sort_rows(rows: &mut Vec<Row>) {
    let by_keyed = |a: &Row, b: &Row| natural_cmp_keyed(a.1, a.0, b.1, b.0);
    if rows.len() <= KEYED_SORT_ROWS {
        rows.sort_unstable_by(by_keyed);
        return;
    }
    let (mut plain, other): (Vec<_>, Vec<_>) = rows.drain(..).partition(|r| r.0.plain);
    plain.sort_unstable_by(by_keyed);
    let mut wide = Vec::new();
    let mut keyed: Vec<(Vec<u8>, Row)> = other
        .into_iter()
        .map(|r| {
            wide.clear();
            wtf8_to_utf16(r.1, &mut wide);
            (SortKeyCollator.key(&wide), r)
        })
        .collect();
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .1.cmp(b.1 .1)));
    let mut other = keyed.into_iter().map(|k| k.1).peekable();
    let mut plain = plain.into_iter().peekable();
    rows.reserve(plain.len() + other.len());
    while let (Some(p), Some(o)) = (plain.peek(), other.peek()) {
        // A non-plain name is involved: this is the collator path.
        let next = if natural_cmp(o.1, p.1) == Ordering::Less { other.next() } else { plain.next() };
        rows.extend(next);
    }
    rows.extend(plain);
    rows.extend(other);
}

/// A stable merge sort that never panics on a comparator that is not a total order (the probe must survive the very
/// disagreement it looks for).
fn merge_sort(v: &mut Vec<Vec<u8>>, cmp: &dyn Fn(&[u8], &[u8]) -> Ordering) {
    if v.len() < 2 {
        return;
    }
    let mut right = v.split_off(v.len() / 2);
    merge_sort(v, cmp);
    merge_sort(&mut right, cmp);
    let left = std::mem::take(v);
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(x), Some(y)) = (l.peek(), r.peek()) {
        let next = if cmp(y, x) == Ordering::Less { r.next() } else { l.next() };
        v.extend(next);
    }
    v.extend(l);
    v.extend(r);
}

/// The probe of self_check: 600 plain names, every plain byte against every other with digits and case.
fn probe() -> Vec<Vec<u8>> {
    let bytes: Vec<u8> = (0u8..128).filter(|b| is_plain(*b)).collect();
    let mut v: Vec<Vec<u8>> = Vec::with_capacity(600);
    for &c in &bytes {
        v.push(vec![c]);
        v.push(vec![b'a', c]);
        v.push(vec![c, b'a']);
        v.push(vec![b'1', c]);
        v.push(vec![c, b'1']);
    }
    for z in 0..4 {
        for n in [0u32, 1, 2, 5, 9, 10, 11, 19, 20, 100] {
            for tail in ["", "a", "B", ".x", " 1"] {
                v.push(format!("a{}{n}{tail}", "0".repeat(z)).into_bytes());
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// The probe sorted by the fast path and by the collator: true when the two sequences are the same.
pub(crate) fn self_check_uncached() -> bool {
    let mut by_fast = probe();
    let mut by_system = by_fast.clone();
    merge_sort(&mut by_fast, &fast_cmp);
    merge_sort(&mut by_system, &collator_cmp);
    by_fast == by_system
}

/// Once per process; true enables the ASCII fast path. Called by the store's first sort of more than 256 rows, never
/// before the first frame (the first 256 rows sort with the collator).
pub fn self_check() -> bool {
    static RESULT: OnceLock<bool> = OnceLock::new();
    *RESULT.get_or_init(|| {
        let ok = self_check_uncached();
        FAST.store(ok, Atomic::Relaxed);
        ok
    })
}

#[cfg(test)]
#[path = "../../tests/common/corpus.rs"]
pub(crate) mod corpus;

#[cfg(test)]
mod corpus_tests {
    use super::corpus::{corpus, fingerprint, MIX};

    #[test]
    fn corpus_is_stable() {
        let names = corpus(1);
        assert_eq!(names.len(), 50_000);
        assert_eq!(
            fingerprint(&names),
            4_854_537_797_673_666_820,
            "a generator change must bump the version, not this pin"
        );
    }

    #[test]
    fn corpus_mix_counts() {
        assert_eq!(MIX.iter().map(|(_, n)| n).sum::<usize>(), 50_000);
        let names = corpus(1);
        let mut at = 0;
        let class = |name: &str| MIX.iter().position(|(n, _)| *n == name).unwrap_or(usize::MAX);
        let mut starts = Vec::new();
        for (_, n) in MIX {
            starts.push(at);
            at += n;
        }
        let slice = |name: &str| {
            let i = class(name);
            &names[starts[i]..starts[i] + MIX[i].1]
        };
        assert_eq!(at, names.len());
        // Each class holds what its name says.
        assert!(slice("ascii_words").iter().all(|n| n.is_ascii()));
        assert!(slice("leading_zeros")
            .iter()
            .all(|n| n.is_ascii() && n.windows(2).any(|w| w[0] == b'0' && w[1].is_ascii_digit())));
        assert!(slice("diacritics").iter().all(|n| !n.is_ascii()));
        assert!(slice("cjk").iter().all(|n| !n.is_ascii()));
        assert!(slice("dots").iter().all(|n| n.contains(&b'.')));
        assert!(slice("spaces").iter().all(|n| n.contains(&b' ')));
        assert!(slice("punctuation").iter().all(|n| n.iter().any(|b| matches!(b, b'-' | b'\'' | b'_'))));
        assert!(slice("long_digit_runs").iter().all(|n| {
            let run = n.split(|b| !b.is_ascii_digit()).map(<[u8]>::len).max().unwrap_or(0);
            (20..=40).contains(&run)
        }));
        assert!(slice("emoji").iter().all(|n| n.contains(&0xF0)));
        assert!(slice("lone_surrogates")
            .iter()
            .all(|n| n.windows(2).any(|w| w[0] == 0xED && (0xA0..=0xBF).contains(&w[1]))));
        assert!(slice("unicode_digits")
            .iter()
            .all(|n| String::from_utf8(n.clone()).is_ok_and(|s| s.chars().any(|c| c.is_numeric() && !c.is_ascii()))));
        assert!(slice("scripts").iter().all(|n| !n.is_ascii()));
        assert!(slice("case_variants").iter().all(|n| n.is_ascii()));
    }

    #[test]
    fn corpus_has_no_duplicates() {
        let names = corpus(1);
        let set: std::collections::HashSet<&Vec<u8>> = names.iter().collect();
        assert_eq!(set.len(), names.len());
        assert!(names
            .iter()
            .all(|n| !n.is_empty() && !n.contains(&0) && !n.iter().any(|b| b"\\/:*?\"<>|".contains(b))));
    }
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering::{self, Equal, Greater, Less};

    use super::corpus::corpus;
    use super::*;

    /// xorshift64* for the random picks of the tests.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            ((self.next() >> 11) % n as u64) as usize
        }
    }

    fn nc(a: &str, b: &str) -> Ordering {
        natural_cmp(a.as_bytes(), b.as_bytes())
    }

    #[test]
    fn self_check_passes_here() {
        let n = probe().len();
        assert!((500..=700).contains(&n), "the probe holds about 600 names: {n}");
        assert!(self_check_uncached(), "the fast path orders the probe as the collator does");
        assert!(self_check());
    }

    #[test]
    fn hand_cases() {
        // Both the collator path and the fast path must give each answer.
        for cmp in [collator_cmp as fn(&[u8], &[u8]) -> Ordering, fast_cmp] {
            let nc = |a: &str, b: &str| cmp(a.as_bytes(), b.as_bytes());
            assert_eq!(nc("a2", "a10"), Less);
            assert_eq!(nc("a10", "a2"), Greater);
            assert_eq!(nc("a01", "a1"), Less, "more leading zeros first");
            assert_eq!(nc("a1b", "a01c"), Greater, "the zero tie decides at once, not after the rest");
            assert_eq!(nc("a001", "a01"), Less);
            assert_eq!(nc("a00", "a0"), Less);
            assert_eq!(nc("a b", "ab"), Less);
            assert_eq!(nc("x.txt", "x (1).txt"), Greater);
            assert_eq!(nc("", ""), Equal);
            assert_eq!(nc("", "a"), Less);
            assert_eq!(nc("a", ""), Greater);
            assert_eq!(nc("A", "a"), Less, "case is only the ordinal tie-break");
            assert_eq!(nc("a", "A"), Greater);
            assert_eq!(nc("a", "a1"), Less);
            assert_eq!(nc("1", "_"), Greater);
            assert_eq!(nc("1", "a"), Less);
            let big = format!("n{}", "9".repeat(40));
            let longer = format!("n{}", "9".repeat(41));
            assert_eq!(nc(&big, &longer), Less, "40-digit runs compare beyond u64");
            assert_eq!(nc(&longer, &big), Greater);
            let a = format!("n{}1", "9".repeat(39));
            assert_eq!(nc(&a, &big), Less, "the chunks of a long run compare in turn");
            assert_eq!(nc(&format!("n{}5", "0".repeat(30)), "n5"), Less);
            assert_eq!(
                nc(&format!("n{}a", "9".repeat(19)), &format!("n{}", "9".repeat(20))),
                Greater,
                "19-digit chunks"
            );
        }
    }

    #[test]
    fn non_plain_names_use_the_collator() {
        assert_eq!(nc("caf\u{e9}2", "caf\u{e9}10"), Less);
        assert_eq!(nc("a-2", "a-10"), Less);
        assert_eq!(nc("\u{4e00}", "\u{4e00}"), Equal);
        // A lone surrogate (WTF-8) is ordered without a panic.
        let lone = [b'a', 0xED, 0xA0, 0x80];
        assert_eq!(natural_cmp(&lone, &lone), Equal);
        assert_ne!(natural_cmp(&lone, b"a"), Equal);
        // A name longer than the stack buffer.
        let long = "a".repeat(400) + "\u{e9}";
        assert_eq!(nc(&long, &long), Equal);
        assert_eq!(nc(&long, "b"), Less);
    }

    #[test]
    fn natural_cmp_is_total() {
        assert!(self_check());
        let names = corpus(1);
        let mut r = Rng(0xC0FF_EE00_1234_5678);
        for _ in 0..2_000 {
            let (a, b, c) = (&names[r.below(names.len())], &names[r.below(names.len())], &names[r.below(names.len())]);
            let (ab, ba) = (natural_cmp(a, b), natural_cmp(b, a));
            assert_eq!(ab, ba.reverse(), "antisymmetric: {a:?} {b:?}");
            assert_eq!(ab == Equal, a == b, "equal only for equal bytes: {a:?} {b:?}");
            let mut t = [a, b, c];
            t.sort_by(|x, y| natural_cmp(x, y));
            assert_ne!(natural_cmp(t[0], t[1]), Greater, "transitive: {t:?}");
            assert_ne!(natural_cmp(t[1], t[2]), Greater, "transitive: {t:?}");
            assert_ne!(natural_cmp(t[0], t[2]), Greater, "transitive: {t:?}");
        }
    }

    #[test]
    fn keyed_equals_plain() {
        assert!(self_check());
        let names = corpus(1);
        let keys: Vec<NameKey> = names.iter().map(|n| name_key(n)).collect();
        let mut r = Rng(0x1357_9BDF_2468_ACE0);
        let mut plain_pairs = 0;
        for _ in 0..200_000 {
            let (i, j) = (r.below(names.len()), r.below(names.len()));
            plain_pairs += usize::from(keys[i].plain && keys[j].plain);
            assert_eq!(
                natural_cmp_keyed(&names[i], keys[i], &names[j], keys[j]),
                natural_cmp(&names[i], &names[j]),
                "{:?} {:?}",
                names[i],
                names[j]
            );
        }
        assert!(plain_pairs > 20_000, "the random pairs reach the plain case often: {plain_pairs}");
    }

    #[test]
    fn name_key_shapes() {
        assert_eq!(name_key(b""), NameKey { prefix: 0, plain: true });
        let k = name_key(b"ab12");
        assert!(k.plain);
        assert_eq!(k.prefix >> 24, u32::from(weight(b'a')) + 2);
        assert_eq!(k.prefix & 0xFF, 0, "the prefix stops after the first digit");
        assert!(!name_key("caf\u{e9}".as_bytes()).plain);
        assert!(!name_key(b"a-b").plain, "the hyphen is not plain");
        assert!(!name_key(b"it's").plain, "the apostrophe is not plain");
        assert_eq!(name_key(b"A").prefix, name_key(b"a").prefix, "case does not show in the prefix");
        assert!(name_key(b"a").prefix < name_key(b"a1").prefix);
        assert!(name_key(b"_").prefix < name_key(b"1").prefix);
        assert!(name_key(b"1").prefix < name_key(b"a").prefix);
    }

    #[test]
    fn weight_table_is_pinned() {
        // The order of the plain bytes, as derived on this machine; the corpus gate re-derives it.
        let order: Vec<u8> = (0u8..128).filter(|b| is_plain(*b) && !b.is_ascii_uppercase()).collect();
        let mut by_weight = order.clone();
        by_weight.sort_by_key(|b| (weight(*b), *b));
        let text: String = by_weight.iter().map(|b| *b as char).collect();
        assert_eq!(text, " !#$%&(),.;@[]^_{}~+=0123456789abcdefghijklmnopqrstuvwxyz");
        assert_eq!(order.len(), 21 + 10 + 26);
    }

    #[test]
    fn sort_rows_equals_sorting_by_natural_cmp() {
        assert!(self_check());
        let names = corpus(1);
        let rows: Vec<(NameKey, &[u8])> = names.iter().map(|n| (name_key(n), n.as_slice())).collect();
        assert!(rows.len() > KEYED_SORT_ROWS);
        let mut expect = rows.clone();
        expect.sort_by(|a, b| natural_cmp(a.1, b.1));
        let mut got = rows.clone();
        sort_rows(&mut got);
        assert!(got.iter().map(|r| r.1).eq(expect.iter().map(|r| r.1)), "the keyed sort is the natural sort");
        let mut small: Vec<_> = rows.into_iter().step_by(7).take(5_000).collect();
        let mut small_expect = small.clone();
        small_expect.sort_by(|a, b| natural_cmp(a.1, b.1));
        sort_rows(&mut small);
        assert!(small.iter().map(|r| r.1).eq(small_expect.iter().map(|r| r.1)));
    }

    /// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0: the shared CI runner is slower.
    fn time_slack() -> f64 {
        let v = std::env::var("OLED_BENCH_SLACK").ok();
        v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
    }

    #[test]
    #[ignore = "bench: CI bench job, release, serial"]
    fn bench_natural_sort_100k() {
        assert!(self_check());
        let names = corpus(1);
        let (plain, other): (Vec<&Vec<u8>>, Vec<&Vec<u8>>) = names.iter().partition(|n| name_key(n).plain);
        let mut r = Rng(0xABCD_EF01_2345_6789);
        // A real folder: 70 % plain names, 30 % not.
        let rows: Vec<(NameKey, &[u8])> = (0..100_000)
            .map(|i| {
                let pool = if i % 10 < 7 { &plain } else { &other };
                let n: &[u8] = pool[r.below(pool.len())];
                (name_key(n), n)
            })
            .collect();
        let median = |f: &dyn Fn(&mut Vec<Row>)| -> f64 {
            let mut ms: Vec<f64> = (0..5)
                .map(|_| {
                    let mut v = rows.clone();
                    let t = std::time::Instant::now();
                    f(&mut v);
                    let e = t.elapsed().as_secs_f64() * 1000.0;
                    assert!(v.windows(2).all(|w| natural_cmp(w[0].1, w[1].1) != Greater));
                    e
                })
                .collect();
            ms.sort_by(f64::total_cmp);
            ms[2]
        };
        let direct = median(&|v| v.sort_by(|a, b| natural_cmp_keyed(a.1, a.0, b.1, b.0)));
        println!("bench: files natural_sort_100k_direct {direct:.1} ms");
        // The budget of F 5.5.3 is met by what the store runs above KEYED_SORT_ROWS rows.
        let keyed = median(&|v| sort_rows(v));
        println!("bench: files natural_sort_100k {keyed:.1} ms");
        let budget = 50.0 * time_slack();
        assert!(keyed <= budget, "median {keyed} ms over {budget} ms (direct sort: {direct} ms)");
    }

    #[test]
    #[ignore = "bench: CI bench job, release, serial"]
    fn bench_natural_self_check() {
        let mut ms: Vec<f64> = (0..50)
            .map(|_| {
                let t = std::time::Instant::now();
                assert!(self_check_uncached());
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        let (median, p99) = ((ms[24] + ms[25]) / 2.0, ms[49]);
        println!("bench: files natural_self_check {median:.3} ms (p99 {p99:.3} ms)");
        let slack = time_slack();
        assert!(median <= 3.0 * slack, "median {median} ms over 3 ms (slack {slack})");
        assert!(p99 <= 5.0 * slack, "p99 {p99} ms over 5 ms (slack {slack})");
    }
}
