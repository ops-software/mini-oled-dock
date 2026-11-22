//! Frecency (SPEC 5.2.5, 5.2.9): what the user picks rises in its tier. Keyed by ItemKey, so a key survives restarts and
//! nothing the user typed is kept: no query, no title, only a hash, a count and the time of the last pick. Reading and
//! writing the file is the shell thread's job; this module only turns bytes into entries.
//!
//! The file "launcher-frecency.txt": "mini-oled-dock launcher-frecency 1", then at most 1,000 lines
//! "<ItemKey, 16 lowercase hex digits> <count 1-65535> <last pick, Unix seconds>", at most 64 KiB. The parser is
//! strict: one bad line, a duplicate key or another version gives an empty state.
#![deny(clippy::arithmetic_side_effects)]

use super::model::ItemKey;

pub const HEADER: &str = "mini-oled-dock launcher-frecency 1";
pub const MAX_ENTRIES: usize = 1000;
pub const MAX_BYTES: usize = 64 * 1024;

/// 2020-01-01T00:00:00Z: the entries keep the time of the last pick as seconds since this, in a u32 (good until 2156).
const FRECENCY_EPOCH: i64 = 1_577_836_800;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
/// The boost never reaches the pinned rows' u16::MAX.
const MAX_BOOST: u16 = u16::MAX - 1;

/// One learned key: the key, the number of picks and the second of the last one since `FRECENCY_EPOCH` (saturating).
type Entry = (u64, u16, u32);

fn since_epoch(unix: i64) -> u32 {
    u32::try_from(unix.saturating_sub(FRECENCY_EPOCH).max(0)).unwrap_or(u32::MAX)
}

fn unix_of(since: u32) -> i64 {
    FRECENCY_EPOCH.saturating_add(i64::from(since))
}

/// The picks learned so far: per key the number of picks and the second of the last one. A vector sorted by key and
/// searched by bisection: 16 bytes an entry, 16 KiB at the cap, no hash table and no spare capacity.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Frecency {
    entries: Vec<Entry>,
}

/// The weight of a count by the age of the last pick, in quarters: x4 under an hour, x2 under a day, /2 under a week,
/// /4 older. Whole numbers all the way, so a single old pick still counts for one quarter and never rounds to nothing.
fn weight_quarters(age: i64) -> u32 {
    match age {
        a if a < HOUR => 16,
        a if a < DAY => 8,
        a if a < WEEK => 2,
        _ => 1,
    }
}

impl Frecency {
    pub fn new() -> Frecency {
        Frecency::default()
    }

    fn find(&self, key: u64) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&key, |e| e.0)
    }

    /// Sets the entry of `key`, keeping the vector sorted.
    fn put(&mut self, key: u64, count: u16, last: i64) {
        let e = (key, count, since_epoch(last));
        match self.find(key) {
            Ok(i) => {
                if let Some(slot) = self.entries.get_mut(i) {
                    *slot = e;
                }
            }
            Err(i) => self.entries.insert(i, e),
        }
    }

    /// The entry with the oldest last pick (the lowest key among equals) leaves.
    fn evict_oldest(&mut self) {
        let oldest = self.entries.iter().enumerate().min_by_key(|(_, e)| (e.2, e.0)).map(|(i, _)| i);
        if let Some(i) = oldest {
            self.entries.remove(i);
        }
    }

    /// A state that the picking rules would never make (a destructive row, a hand-edited file).
    #[cfg(test)]
    pub fn entries_for_test(&mut self, key: ItemKey) {
        self.put(key.0, 500, 1_790_000_000);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The rank boost of a row (0 when it was never picked): count x weight, saturating. It only orders rows inside one
    /// tier (it is the second field of Score, after the tier). No allocation, no disk.
    pub fn boost(&self, key: ItemKey, now: i64) -> u16 {
        let Some(&(_, count, last)) = self.find(key.0).ok().and_then(|i| self.entries.get(i)) else { return 0 };
        let w = weight_quarters(now.saturating_sub(unix_of(last)));
        u16::try_from(u32::from(count).saturating_mul(w).min(u32::from(MAX_BOOST))).unwrap_or(MAX_BOOST)
    }

    /// +1 for a pick. Ignored (false) for a destructive row. At 1,000 entries the one with the oldest last pick makes
    /// room.
    pub fn pick(&mut self, key: ItemKey, now: i64, destructive: bool) -> bool {
        if destructive || now < 0 {
            return false;
        }
        if let Ok(i) = self.find(key.0) {
            if let Some(e) = self.entries.get_mut(i) {
                e.1 = e.1.saturating_add(1);
                e.2 = since_epoch(now);
            }
            return true;
        }
        if self.entries.len() >= MAX_ENTRIES {
            self.evict_oldest();
        }
        self.put(key.0, 1, now);
        true
    }

    /// Shift+Delete on a row.
    pub fn forget(&mut self, key: ItemKey) -> bool {
        match self.find(key.0) {
            Ok(i) => {
                self.entries.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// Adds picks made in memory while the file was still being read: the counts add, the later pick wins.
    pub fn absorb(&mut self, newer: &Frecency) {
        for &(key, count, last) in &newer.entries {
            match self.find(key) {
                Ok(i) => {
                    if let Some(e) = self.entries.get_mut(i) {
                        e.1 = e.1.saturating_add(count);
                        e.2 = e.2.max(last);
                    }
                }
                Err(i) => self.entries.insert(i, (key, count, last)),
            }
        }
        while self.entries.len() > MAX_ENTRIES {
            self.evict_oldest();
        }
    }

    pub fn clear(&mut self) {
        self.entries = Vec::new();
    }

    /// The entries of a file, or an empty state for anything that is not exactly the format.
    pub fn parse(bytes: &[u8]) -> Frecency {
        Frecency::parse_strict(bytes).unwrap_or_default()
    }

    fn parse_strict(bytes: &[u8]) -> Option<Frecency> {
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let body = text.strip_prefix(HEADER)?.strip_prefix('\n')?;
        let mut f = Frecency::new();
        if body.is_empty() {
            return Some(f);
        }
        for line in body.strip_suffix('\n')?.split('\n') {
            if f.entries.len() >= MAX_ENTRIES {
                return None;
            }
            let mut parts = line.split(' ');
            let (key, count, last) = (parts.next()?, parts.next()?, parts.next()?);
            if parts.next().is_some() {
                return None;
            }
            let key = parse_key(key)?;
            let count = u16::try_from(parse_number(count)?).ok().filter(|c| *c >= 1)?;
            let last = i64::try_from(parse_number(last)?).ok()?;
            let at = f.find(key).err()?; // a duplicate key
            f.entries.insert(at, (key, count, since_epoch(last)));
        }
        Some(f)
    }

    /// The bytes of the file, the most recent pick first (a stable order, so an unchanged state writes the same bytes).
    pub fn render(&self) -> Vec<u8> {
        let mut rows: Vec<&Entry> = self.entries.iter().collect();
        rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        let mut s = String::with_capacity(HEADER.len().saturating_add(1).saturating_add(rows.len().saturating_mul(34)));
        s.push_str(HEADER);
        s.push('\n');
        for &(key, count, last) in rows.into_iter().take(MAX_ENTRIES) {
            s.push_str(&format!("{key:016x} {count} {}\n", unix_of(last)));
        }
        s.into_bytes()
    }
}

/// Exactly 16 lowercase hex digits.
fn parse_key(s: &str) -> Option<u64> {
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// A canonical decimal: digits only, no sign, no leading zero (except "0"), at most 18 digits.
fn parse_number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::model::ProviderId;
    use crate::launcher::testkit::XorShift;

    fn key(n: u64) -> ItemKey {
        ItemKey(n)
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn decay_steps() {
        let mut f = Frecency::new();
        let k = key(7);
        assert_eq!(f.boost(k, NOW), 0, "never picked");
        assert!(f.pick(k, NOW, false));
        assert!(f.pick(k, NOW, false));
        // Two picks: x4, x2, /2, /4 of the count, in quarters.
        assert_eq!(f.boost(k, NOW), 32);
        assert_eq!(f.boost(k, NOW + HOUR - 1), 32);
        assert_eq!(f.boost(k, NOW + HOUR), 16);
        assert_eq!(f.boost(k, NOW + DAY - 1), 16);
        assert_eq!(f.boost(k, NOW + DAY), 4);
        assert_eq!(f.boost(k, NOW + WEEK - 1), 4);
        assert_eq!(f.boost(k, NOW + WEEK), 2);
        assert_eq!(f.boost(k, NOW + 400 * DAY), 2);
        // A clock that went back is a fresh pick, not a negative age.
        assert_eq!(f.boost(k, NOW - 5 * DAY), 32);
        // One old pick still counts.
        let mut g = Frecency::new();
        g.pick(k, NOW, false);
        assert_eq!(g.boost(k, NOW + 30 * DAY), 1);
        // Saturates below the pinned rows' value.
        f.put(9, u16::MAX, NOW);
        assert_eq!(f.boost(key(9), NOW), u16::MAX - 1);
    }

    #[test]
    fn entries_fit_16_kib() {
        assert!(std::mem::size_of::<Entry>() * MAX_ENTRIES <= 16_384);
        // Full, the vector holds no more than the cap's worth (no spare capacity after a parse).
        let lines: String = (0..1000u64).map(|i| format!("{i:016x} 1 1700000000\n")).collect();
        let text = format!("{HEADER}\n{lines}");
        let f = Frecency::parse(text.as_bytes());
        assert_eq!(f.len(), 1000);
        assert!(f.entries.capacity() * std::mem::size_of::<Entry>() <= 2 * 16_384, "{}", f.entries.capacity());
    }

    #[test]
    fn destructive_never_learned() {
        let mut f = Frecency::new();
        assert!(!f.pick(key(1), NOW, true));
        assert!(f.is_empty());
        assert!(f.pick(key(1), NOW, false));
        // Picking a known key as destructive changes nothing either.
        assert!(!f.pick(key(1), NOW + 5, true));
        assert_eq!(f.boost(key(1), NOW + 5), 16);
        assert!(!f.pick(key(2), -1, false), "no negative times");
    }

    #[test]
    fn round_trip() {
        let mut f = Frecency::new();
        for i in 0..40u64 {
            for _ in 0..=(i % 5) {
                f.pick(ItemKey::of(ProviderId::Apps, &format!("app{i}")), NOW + i as i64, false);
            }
        }
        let bytes = f.render();
        assert!(bytes.starts_with(b"mini-oled-dock launcher-frecency 1\n"));
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(Frecency::parse(&bytes), f);
        assert_eq!(Frecency::parse(&bytes).render(), bytes, "the same state writes the same bytes");
        assert_eq!(Frecency::parse(&Frecency::new().render()), Frecency::new());
        // The line format.
        let mut one = Frecency::new();
        one.pick(key(0xAB), 1_700_000_000, false);
        assert_eq!(one.render(), b"mini-oled-dock launcher-frecency 1\n00000000000000ab 1 1700000000\n");
    }

    #[test]
    fn version_header_required() {
        let line = "00000000000000ab 3 1700000000\n";
        assert_eq!(Frecency::parse(format!("{HEADER}\n{line}").as_bytes()).len(), 1);
        for bad in [
            format!("mini-oled-dock launcher-frecency 2\n{line}"),
            format!("mini-oled-dock launcher-frecency 1 \n{line}"),
            format!("Mini-oled-dock launcher-frecency 1\n{line}"),
            format!("mini-oled-dock launcher-run 1\n{line}"),
            line.to_string(),
            String::new(),
            HEADER.to_string(),
            format!("\u{feff}{HEADER}\n{line}"),
            format!("{HEADER}\r\n{line}"),
        ] {
            assert!(Frecency::parse(bad.as_bytes()).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn corrupt_or_oversized_ignored() {
        let ok = "00000000000000ab 3 1700000000";
        for bad in [
            "00000000000000AB 3 1700000000\n",
            "00000000000000a 3 1700000000\n",
            "00000000000000abc 3 1700000000\n",
            "00000000000000ab 0 1700000000\n",
            "00000000000000ab 65536 1700000000\n",
            "00000000000000ab 03 1700000000\n",
            "00000000000000ab +3 1700000000\n",
            "00000000000000ab 3 -1\n",
            "00000000000000ab 3 01\n",
            "00000000000000ab 3\n",
            "00000000000000ab 3 1700000000 x\n",
            "00000000000000ab  3 1700000000\n",
            "00000000000000ab 3 1700000000 \n",
            "00000000000000zz 3 1700000000\n",
            "00000000000000ab 3 99999999999999999999\n",
            "\n",
        ] {
            let text = format!("{HEADER}\n{ok}\n{bad}");
            assert!(Frecency::parse(text.as_bytes()).is_empty(), "{bad:?}");
            assert!(Frecency::parse(format!("{HEADER}\n{bad}").as_bytes()).is_empty(), "{bad:?}");
        }
        // A missing final newline, a duplicate key, a blank line in the middle, bytes that are not UTF-8.
        assert!(Frecency::parse(format!("{HEADER}\n{ok}").as_bytes()).is_empty());
        assert!(Frecency::parse(format!("{HEADER}\n{ok}\n{ok}\n").as_bytes()).is_empty());
        assert!(Frecency::parse(format!("{HEADER}\n{ok}\n\n{ok}\n").as_bytes()).is_empty());
        assert!(Frecency::parse(b"mini-oled-dock launcher-frecency 1\n\xff\n").is_empty());
        // 1,000 lines are fine, 1,001 are not, and nothing over 64 KiB is read.
        let many = |n: u64| -> String { (0..n).map(|i| format!("{i:016x} 1 1700000000\n")).collect() };
        assert_eq!(Frecency::parse(format!("{HEADER}\n{}", many(1000)).as_bytes()).len(), 1000);
        assert!(Frecency::parse(format!("{HEADER}\n{}", many(1001)).as_bytes()).is_empty());
        let mut big = format!("{HEADER}\n").into_bytes();
        big.resize(MAX_BYTES + 1, b'\n');
        assert!(Frecency::parse(&big).is_empty());
        // The largest legal file stays under the cap.
        let mut f = Frecency::new();
        for i in 0..2000u64 {
            f.pick(key(u64::MAX - i), 9_999_999_999, false);
        }
        assert_eq!(f.len(), MAX_ENTRIES);
        assert!(f.render().len() <= MAX_BYTES);
    }

    #[test]
    fn at_most_1000_the_oldest_dropped() {
        let mut f = Frecency::new();
        for i in 0..1000u64 {
            f.pick(key(i), NOW + i as i64, false);
        }
        assert_eq!(f.len(), 1000);
        // Picking an existing key never evicts; a new key pushes out the oldest.
        f.pick(key(500), NOW + 2000, false);
        assert_eq!(f.len(), 1000);
        assert!(f.boost(key(0), NOW + 2000) > 0);
        f.pick(key(5000), NOW + 2001, false);
        assert_eq!(f.len(), 1000);
        assert_eq!(f.boost(key(0), NOW + 2001), 0);
        assert!(f.boost(key(1), NOW + 2001) > 0);
        assert!(f.boost(key(5000), NOW + 2001) > 0);
    }

    #[test]
    fn forget_and_clear() {
        let mut f = Frecency::new();
        f.pick(key(1), NOW, false);
        f.pick(key(2), NOW, false);
        assert!(f.forget(key(1)));
        assert!(!f.forget(key(1)));
        assert_eq!(f.boost(key(1), NOW), 0);
        assert!(f.boost(key(2), NOW) > 0);
        f.clear();
        assert!(f.is_empty());
    }

    #[test]
    fn boost_orders_inside_a_tier_only() {
        use crate::launcher::model::Score;
        use std::cmp::Reverse;
        let mut f = Frecency::new();
        for _ in 0..200 {
            f.pick(key(1), NOW, false);
        }
        let base = Score { tier: 2, learned: Reverse(0), idle: true, kind: 0, rank: 5, len: 4 };
        let hot = Score { learned: Reverse(f.boost(key(1), NOW)), ..base };
        assert!(hot < base, "inside a tier a boost wins");
        assert!(hot > Score { tier: 1, ..base }, "a better tier still wins whatever was learned");
        // An older pick of the same count ranks below a recent one.
        let mut g = Frecency::new();
        g.pick(key(1), NOW, false);
        g.pick(key(2), NOW - 3 * DAY, false);
        assert!(g.boost(key(1), NOW) > g.boost(key(2), NOW));
    }

    #[test]
    fn seeded_100k_parser_never_panics() {
        let mut r = XorShift::new(0xF4EC);
        let atoms: [&[u8]; 14] = [
            HEADER.as_bytes(),
            b"\n",
            b"\r\n",
            b" ",
            b"00000000000000ab",
            b"ffffffffffffffff",
            b"7",
            b"65535",
            b"65536",
            b"1700000000",
            b"-1",
            b"0",
            b"\xff",
            "\u{202e}".as_bytes(),
        ];
        for i in 0..100_000 {
            let mut bytes: Vec<u8> = Vec::new();
            if i % 2 == 0 {
                bytes.extend_from_slice(HEADER.as_bytes());
                bytes.push(b'\n');
            }
            let n = r.next_u64() % 14;
            for _ in 0..n {
                let piece = *r.pick(&atoms);
                bytes.extend_from_slice(piece);
            }
            let parsed = Frecency::parse(&bytes);
            // Whatever is accepted writes back to a state that parses to the same entries.
            if !parsed.is_empty() {
                assert_eq!(Frecency::parse(&parsed.render()), parsed);
            }
        }
    }
}
