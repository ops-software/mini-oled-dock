//! The it.lang string file format (F-P12).
//!
//! Layout (all little-endian): 0 "OLNG" | 4 u16 format = 1 | 6 u16 reserved = 0 | 8 u32 count | 12 u32 blob_len
//! | 16 [u8; 2] tag (ASCII lowercase) | 18 u16 major | 20 u16 minor | 22 u16 patch
//! | 24 count x (u64 hash, u32 offset, u32 len) | blob (UTF-8). Hashes are strictly ascending, every span lies inside
//! the blob and is valid UTF-8, and nothing follows the blob.
//!
//! The file is read by an elevated process from a folder that a standard user may be able to influence, so the
//! parser is total: it validates everything once, in bounded time, and never panics.

pub const LANG_MAGIC: [u8; 4] = *b"OLNG";
pub const LANG_FORMAT: u16 = 1;
pub const LANG_MAX_BYTES: usize = 1024 * 1024;
pub const LANG_MAX_KEYS: u32 = 16_384;

const HEADER: usize = 24;
const ENTRY: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LangError {
    /// The first four bytes are not "OLNG".
    Magic,
    /// The format number is not 1 (carries the number), or the reserved field is not 0 (carries its value).
    Format(u16),
    /// Fewer bytes than the header and the declared table and blob need.
    Short,
    /// More keys than LANG_MAX_KEYS, or a table that does not fit the bytes.
    Count,
    /// Hashes not strictly ascending (or equal).
    Order,
    /// A span outside the blob, or bytes after the blob.
    Span,
    /// A span that is not valid UTF-8.
    Utf8,
    /// The tag is not two ASCII lowercase letters.
    Tag,
    /// More than LANG_MAX_BYTES.
    TooLarge,
}

/// A validated it.lang: the owned bytes; the entry table and the blob are read in place.
pub struct LangFile {
    bytes: Vec<u8>,
    count: usize,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

fn tag_ok(t: [u8; 2]) -> bool {
    t.iter().all(u8::is_ascii_lowercase)
}

impl LangFile {
    /// Total: validates everything once, then get() cannot fail.
    pub fn parse(bytes: Vec<u8>) -> Result<LangFile, LangError> {
        if bytes.len() > LANG_MAX_BYTES {
            return Err(LangError::TooLarge);
        }
        if bytes.len() < HEADER {
            return Err(if bytes.len() >= 4 && bytes[..4] != LANG_MAGIC { LangError::Magic } else { LangError::Short });
        }
        if bytes[..4] != LANG_MAGIC {
            return Err(LangError::Magic);
        }
        let format = u16_at(&bytes, 4);
        if format != LANG_FORMAT {
            return Err(LangError::Format(format));
        }
        let reserved = u16_at(&bytes, 6);
        if reserved != 0 {
            return Err(LangError::Format(reserved));
        }
        let count = u32_at(&bytes, 8);
        let blob_len = u32_at(&bytes, 12) as usize;
        if count > LANG_MAX_KEYS {
            return Err(LangError::Count);
        }
        let count = count as usize;
        if !tag_ok([bytes[16], bytes[17]]) {
            return Err(LangError::Tag);
        }
        // count <= 16 384 and blob_len < 2^32, so these sums cannot overflow a usize on a 64-bit target;
        // checked arithmetic keeps a 32-bit build honest too.
        let table_end = count * ENTRY + HEADER;
        if table_end > bytes.len() {
            return Err(LangError::Count);
        }
        let total = table_end.checked_add(blob_len).ok_or(LangError::Short)?;
        if total > bytes.len() {
            return Err(LangError::Short);
        }
        if total < bytes.len() {
            return Err(LangError::Span);
        }
        // One UTF-8 pass over the blob keeps validation linear even when spans overlap.
        let blob = std::str::from_utf8(&bytes[table_end..]).map_err(|_| LangError::Utf8)?;
        let mut prev: Option<u64> = None;
        for i in 0..count {
            let at = HEADER + i * ENTRY;
            let hash = u64_at(&bytes, at);
            if prev.is_some_and(|p| hash <= p) {
                return Err(LangError::Order);
            }
            prev = Some(hash);
            let off = u32_at(&bytes, at + 8) as usize;
            let len = u32_at(&bytes, at + 12) as usize;
            let end = off.checked_add(len).ok_or(LangError::Span)?;
            if end > blob.len() {
                return Err(LangError::Span);
            }
            if !blob.is_char_boundary(off) || !blob.is_char_boundary(end) {
                return Err(LangError::Utf8);
            }
        }
        Ok(LangFile { bytes, count })
    }

    /// The language tag, "it".
    pub fn tag(&self) -> &str {
        std::str::from_utf8(&self.bytes[16..18]).unwrap_or("")
    }

    pub fn suite_version(&self) -> (u16, u16, u16) {
        (u16_at(&self.bytes, 18), u16_at(&self.bytes, 20), u16_at(&self.bytes, 22))
    }

    #[allow(clippy::len_without_is_empty)] // the frozen interface has no is_empty
    pub fn len(&self) -> usize {
        self.count
    }

    /// Binary search by key hash.
    pub fn get(&self, hash: u64) -> Option<&str> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = HEADER + mid * ENTRY;
            let h = u64_at(&self.bytes, at);
            match h.cmp(&hash) {
                std::cmp::Ordering::Equal => {
                    let blob = &self.bytes[HEADER + self.count * ENTRY..];
                    let off = u32_at(&self.bytes, at + 8) as usize;
                    let len = u32_at(&self.bytes, at + 12) as usize;
                    // Validated by parse; the checked slice keeps get() total whatever happens.
                    return blob.get(off..off.checked_add(len)?).and_then(|s| std::str::from_utf8(s).ok());
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }
}

/// Writes the file; entries are sorted by hash. Two entries with one hash are refused (Order).
pub fn encode(tag: &str, suite_version: (u16, u16, u16), entries: &[(u64, &str)]) -> Result<Vec<u8>, LangError> {
    let t = tag.as_bytes();
    if t.len() != 2 || !tag_ok([t[0], t[1]]) {
        return Err(LangError::Tag);
    }
    if entries.len() > LANG_MAX_KEYS as usize {
        return Err(LangError::Count);
    }
    let mut sorted: Vec<(u64, &str)> = entries.to_vec();
    sorted.sort_by_key(|e| e.0);
    if sorted.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(LangError::Order);
    }
    let blob_len: usize = sorted.iter().map(|e| e.1.len()).sum();
    let total = HEADER + sorted.len() * ENTRY + blob_len;
    if total > LANG_MAX_BYTES {
        return Err(LangError::TooLarge);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&LANG_MAGIC);
    out.extend_from_slice(&LANG_FORMAT.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    out.extend_from_slice(&(blob_len as u32).to_le_bytes());
    out.extend_from_slice(t);
    for v in [suite_version.0, suite_version.1, suite_version.2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut off = 0u32;
    for (h, s) in &sorted {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        off += s.len() as u32;
    }
    for (_, s) in &sorted {
        out.extend_from_slice(s.as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::fnv1a64;

    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/lang-sample.lang");
    const SAMPLE_IT: &str = include_str!("../tests/fixtures/lang-sample/it.toml");

    /// The (key, text) pairs of the fixture: lines "key" = "text" without escapes.
    fn sample_pairs() -> Vec<(String, String)> {
        SAMPLE_IT
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(|l| {
                let (k, v) = l.split_once(" = ").unwrap();
                (k.trim_matches('"').to_string(), v.trim().trim_matches('"').to_string())
            })
            .collect()
    }

    #[test]
    fn parse_accepts_the_sample() {
        let f = LangFile::parse(SAMPLE.to_vec()).unwrap();
        assert_eq!(f.tag(), "it");
        assert_eq!(f.suite_version(), (1, 8, 0));
        let pairs = sample_pairs();
        assert!(pairs.len() >= 4);
        assert_eq!(f.len(), pairs.len());
        for (k, t) in &pairs {
            assert_eq!(f.get(fnv1a64(k)), Some(t.as_str()), "{k}");
        }
        assert_eq!(f.get(fnv1a64("no.such_key")), None);
        let e: Vec<(u64, &str)> = pairs.iter().map(|(k, t)| (fnv1a64(k), t.as_str())).collect();
        assert_eq!(encode("it", (1, 8, 0), &e).unwrap(), SAMPLE);
    }

    #[test]
    fn overlapping_spans_parse_in_linear_time() {
        let n = 16_384usize;
        let blob = "\u{20ac}".repeat(1000);
        let mut b = Vec::new();
        b.extend_from_slice(&LANG_MAGIC);
        b.extend_from_slice(&[1, 0, 0, 0]);
        b.extend_from_slice(&(n as u32).to_le_bytes());
        b.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        b.extend_from_slice(b"it");
        b.extend_from_slice(&[1, 0, 8, 0, 0, 0]);
        for i in 0..n {
            b.extend_from_slice(&(i as u64 + 1).to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        }
        b.extend_from_slice(blob.as_bytes());
        let f = LangFile::parse(b).unwrap();
        assert_eq!(f.len(), n);
    }

    #[test]
    fn layout_is_frozen() {
        let got = encode("it", (1, 8, 0), &[(2, "bc"), (1, "a")]).unwrap();
        #[rustfmt::skip]
        let want: [u8; 59] = [
            b'O', b'L', b'N', b'G', 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0,
            b'i', b't', 1, 0, 8, 0, 0, 0,
            1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
            2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0,
            b'a', b'b', b'c',
        ];
        assert_eq!(got, want);
        assert_eq!(got.len(), 59);
    }

    fn base() -> Vec<u8> {
        encode("it", (1, 8, 0), &[(1, "a"), (2, "bc")]).unwrap()
    }

    #[test]
    fn refusals() {
        let ok = base();
        assert!(LangFile::parse(ok.clone()).is_ok());
        let with = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut b = ok.clone();
            f(&mut b);
            LangFile::parse(b).err()
        };
        assert_eq!(with(&|b| b[0] = b'X'), Some(LangError::Magic));
        assert_eq!(with(&|b| b[4] = 2), Some(LangError::Format(2)));
        assert_eq!(with(&|b| b[6] = 1), Some(LangError::Format(1)));
        assert_eq!(with(&|b| b[8..12].copy_from_slice(&(LANG_MAX_KEYS + 1).to_le_bytes())), Some(LangError::Count));
        assert_eq!(with(&|b| b[8..12].copy_from_slice(&3u32.to_le_bytes())), Some(LangError::Count));
        assert_eq!(with(&|b| b[8..12].copy_from_slice(&1u32.to_le_bytes())), Some(LangError::Span));
        assert_eq!(with(&|b| b[8..12].copy_from_slice(&1000u32.to_le_bytes())), Some(LangError::Count));
        // Hashes equal, then descending.
        assert_eq!(with(&|b| b[40] = 1), Some(LangError::Order));
        assert_eq!(with(&|b| b[40] = 0), Some(LangError::Order));
        // A span past the blob; a span starting past the blob with length 0 is outside too.
        assert_eq!(with(&|b| b[36] = 9), Some(LangError::Span));
        assert_eq!(with(&|b| b[52..56].copy_from_slice(&u32::MAX.to_le_bytes())), Some(LangError::Span));
        assert_eq!(with(&|b| b[56] = 0xff), Some(LangError::Utf8));
        assert_eq!(with(&|b| b[16] = b'I'), Some(LangError::Tag));
        assert_eq!(with(&|b| b[17] = b'1'), Some(LangError::Tag));
        assert_eq!(with(&|b| b.push(0)), Some(LangError::Span));
        assert_eq!(with(&|b| b.truncate(58)), Some(LangError::Short));
        assert_eq!(with(&|b| b.truncate(23)), Some(LangError::Short));
        assert_eq!(with(&|b| b.clear()), Some(LangError::Short));
        assert_eq!(LangFile::parse(vec![0u8; LANG_MAX_BYTES + 1]).err(), Some(LangError::TooLarge));
        assert!(encode("IT", (1, 0, 0), &[]).is_err());
        assert!(encode("ita", (1, 0, 0), &[]).is_err());
        assert_eq!(encode("it", (1, 0, 0), &[(1, "a"), (1, "b")]), Err(LangError::Order));
        let empty = encode("it", (1, 0, 0), &[]).unwrap();
        assert_eq!(LangFile::parse(empty).unwrap().len(), 0);
        let big = "x".repeat(LANG_MAX_BYTES);
        assert_eq!(encode("it", (1, 0, 0), &[(1, big.as_str())]), Err(LangError::TooLarge));
    }

    /// A tiny xorshift generator: the fuzz tests need no dependency and are reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn mix(h: u64) -> u64 {
        h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (h >> 29)
    }

    #[test]
    fn get_is_binary_search() {
        let texts: Vec<String> = (0..10_000u64).map(|i| format!("t{i}")).collect();
        let entries: Vec<(u64, &str)> =
            texts.iter().enumerate().map(|(i, t)| (mix(i as u64 + 1), t.as_str())).collect();
        let f = LangFile::parse(encode("it", (1, 8, 0), &entries).unwrap()).unwrap();
        assert_eq!(f.len(), 10_000);
        for (h, t) in &entries {
            assert_eq!(f.get(*h), Some(*t));
        }
        let present: std::collections::HashSet<u64> = entries.iter().map(|e| e.0).collect();
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut absent = 0;
        while absent < 10_000 {
            let h = rng.next();
            if !present.contains(&h) {
                assert_eq!(f.get(h), None);
                absent += 1;
            }
        }
        assert_eq!(f.get(0), None);
        assert_eq!(f.get(u64::MAX), None);
    }

    fn fuzz(rounds: u32) {
        let seeds = [SAMPLE.to_vec(), base()];
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for n in 0..rounds {
            let mut b = if n % 8 == 7 {
                (0..(rng.next() % 200) as usize).map(|_| rng.next() as u8).collect::<Vec<u8>>()
            } else {
                let mut b = seeds[(n % 2) as usize].clone();
                for _ in 0..1 + rng.next() % 4 {
                    if b.is_empty() {
                        break;
                    }
                    let at = (rng.next() % b.len() as u64) as usize;
                    match rng.next() % 4 {
                        0 => b[at] = rng.next() as u8,
                        1 => b[at] ^= 1 << (rng.next() % 8),
                        2 => b.truncate(at),
                        _ => b.insert(at, rng.next() as u8),
                    }
                }
                b
            };
            if n % 64 == 0 {
                b.extend_from_slice(&[0xff; 3]);
            }
            let hashes: Vec<u64> = {
                let n = b.get(8..12).map_or(0, |c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize);
                (0..n.min(b.len() / 16))
                    .filter_map(|i| b.get(24 + i * 16..32 + i * 16))
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .collect()
            };
            if let Ok(f) = LangFile::parse(b) {
                for &h in &hashes {
                    assert!(f.get(h).is_some(), "table key not found");
                    let _ = f.get(h ^ 1);
                    let _ = f.get(h.wrapping_add(1));
                }
                let _ = (f.tag(), f.suite_version(), f.len());
                for _ in 0..8 {
                    let _ = f.get(rng.next());
                }
                let _ = f.get(0);
                let _ = f.get(u64::MAX);
            }
        }
    }

    #[test]
    fn fuzz_lang_10k() {
        fuzz(10_000);
    }

    #[test]
    #[ignore = "nightly: one million rounds"]
    fn fuzz_lang_1m() {
        fuzz(1_000_000);
    }
}
