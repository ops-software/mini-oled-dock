//! The keyword dictionary of a catalogue (L3 T9, SPEC 5.2.5 tiers 4, 6 and 7): every hidden keyword, de-duplicated and
//! sorted by its bytes, each with the run of entries that carry it, so a keystroke answers the hidden tiers with two
//! binary searches and one contiguous range instead of a scan of every candidate's keyword text. The buffer is plain bytes
//! (little-endian, checked sub-slices only), so a later arena can embed the same sections and a mapped file is searched
//! without a copy (SPEC appendix C.1, sections 3-9). Pure: no Windows, no files.

use super::rank::{Cand, PrefixWalk, T_BAG_EQ, T_SYN_EQ, T_SYN_PREFIX};
use crate::start_search::XML_KEYWORD_PENALTY;
use std::collections::{BTreeSet, HashMap};

/// Sections of the buffer, in order (C.1's ids 3 to 9).
const CUR: usize = 0;
const CURP: usize = 1;
const XML: usize = 2;
const XMLP: usize = 3;
const FUZ: usize = 4;
const FUZP: usize = 5;
const CHARS: usize = 6;
const SECTIONS: usize = 7;

/// A dictionary record: STR offset u32, length u16, 0u16, first posting u32, posting count u32.
const REC: usize = 16;
/// A curated posting: entry u32, penalty u32.
const CUR_POST: usize = 8;
/// An XML posting: entry u32.
const XML_POST: usize = 4;

/// The hidden match of one entry: (entry, tier, penalty).
pub type Hit = (u32, u8, u32);

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(b.get(at..end)?.try_into().ok()?))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(b.get(at..end)?.try_into().ok()?))
}

fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn align4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// The owned bytes of an index: the strings and the sections.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct IndexBuf {
    entries: u32,
    strs: Vec<u8>,
    secs: Vec<u8>,
    bounds: [(u32, u32); SECTIONS],
}

impl IndexBuf {
    /// The index of the keywords and names of `entries` ((position, candidate)). A keyword whose penalty is
    /// `XML_KEYWORD_PENALTY` goes to the XML dictionary, any other to the curated one with its penalty. The fuzzy targets are the
    /// word suffixes of every shown name and of every other name (L3 T10), each with the entries that have it.
    pub fn build<'a>(entries: impl Iterator<Item = (u32, Cand<'a>)>) -> IndexBuf {
        let mut cur: Vec<(&str, u32, u32)> = Vec::new();
        let mut xml: Vec<(&str, u32)> = Vec::new();
        let mut targets: HashMap<&str, (u32, Vec<u32>)> = HashMap::new();
        let mut seen = [false; 128];
        let mut other: BTreeSet<char> = BTreeSet::new();
        let mut st = Strs::default();
        let mut n = 0u32;
        for (e, c) in entries {
            n = n.max(e.saturating_add(1));
            for (k, text) in c.keys.raw() {
                if k.len() > usize::from(u16::MAX) {
                    continue;
                }
                let penalty: u32 = text.parse().unwrap_or(u32::MAX);
                if penalty == XML_KEYWORD_PENALTY {
                    xml.push((k, e));
                } else {
                    cur.push((k, e, penalty));
                }
            }
            // Names are stored once; a target is a slice of one (the first occurrence of its text).
            let mut add = |t: &'a str, off: usize| {
                if t.is_empty() || t.len() > usize::from(u16::MAX) {
                    return;
                }
                let at = u32::try_from(off).unwrap_or(u32::MAX);
                targets.entry(t).or_insert_with(|| (at, Vec::new())).1.push(e);
            };
            let off = st.intern(c.folded);
            for &w in c.words {
                let w = usize::from(w);
                if let Some(t) = c.folded.get(w..) {
                    add(t, off + w);
                }
            }
            for a in c.extra {
                let off = st.intern(a);
                add(a, off);
                for (i, _) in a.match_indices(' ') {
                    if let Some(t) = a.get(i + 1..) {
                        add(t, off + i + 1);
                    }
                }
            }
            for ch in c.folded.chars().chain(c.extra.iter().flat_map(|a| a.chars())) {
                match seen.get_mut(ch as usize) {
                    Some(s) => *s = true,
                    None => {
                        other.insert(ch);
                    }
                }
            }
        }
        // Sorted by (keyword, entry, penalty): a keyword twice on one entry keeps its best penalty.
        cur.sort_unstable();
        cur.dedup_by(|later, first| later.0 == first.0 && later.1 == first.1);
        xml.sort_unstable();
        xml.dedup();
        let mut sections: [Vec<u8>; SECTIONS] = Default::default();
        let mut i = 0;
        while let Some(&(k, ..)) = cur.get(i) {
            let run = cur.get(i..).map_or(0, |r| r.iter().take_while(|e| e.0 == k).count());
            let first = sections.get(CURP).map_or(0, |p| p.len() / CUR_POST);
            for &(_, e, p) in cur.get(i..i + run).unwrap_or_default() {
                if let Some(s) = sections.get_mut(CURP) {
                    put_u32(s, e);
                    put_u32(s, p);
                }
            }
            let off = st.intern(k);
            if let Some(s) = sections.get_mut(CUR) {
                record(s, off, k.len(), first, run);
            }
            i += run;
        }
        let mut i = 0;
        while let Some(&(k, _)) = xml.get(i) {
            let run = xml.get(i..).map_or(0, |r| r.iter().take_while(|e| e.0 == k).count());
            let first = sections.get(XMLP).map_or(0, |p| p.len() / XML_POST);
            for &(_, e) in xml.get(i..i + run).unwrap_or_default() {
                if let Some(s) = sections.get_mut(XMLP) {
                    put_u32(s, e);
                }
            }
            let off = st.intern(k);
            if let Some(s) = sections.get_mut(XML) {
                record(s, off, k.len(), first, run);
            }
            i += run;
        }
        let mut targets: Vec<(&str, (u32, Vec<u32>))> = targets.into_iter().collect();
        targets.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (t, (off, mut es)) in targets {
            es.sort_unstable();
            es.dedup();
            let first = sections.get(FUZP).map_or(0, |p| p.len() / XML_POST);
            if let Some(s) = sections.get_mut(FUZP) {
                for e in &es {
                    put_u32(s, *e);
                }
            }
            if let Some(s) = sections.get_mut(FUZ) {
                record(s, off as usize, t.len(), first, es.len());
            }
        }
        if let Some(s) = sections.get_mut(CHARS) {
            let ascii = seen.iter().enumerate().filter(|(_, on)| **on).filter_map(|(c, _)| char::from_u32(c as u32));
            for ch in ascii.chain(other.iter().copied()) {
                put_u32(s, ch as u32);
            }
        }
        let mut b = IndexBuf { entries: n, strs: st.bytes, ..IndexBuf::default() };
        for (id, s) in sections.iter().enumerate() {
            align4(&mut b.secs);
            let start = u32::try_from(b.secs.len()).unwrap_or(u32::MAX);
            b.secs.extend_from_slice(s);
            if let Some(slot) = b.bounds.get_mut(id) {
                *slot = (start, u32::try_from(s.len()).unwrap_or(u32::MAX));
            }
        }
        b.strs.shrink_to_fit();
        b.secs.shrink_to_fit();
        b
    }

    pub fn view(&self) -> IndexView<'_> {
        let sec = |id: usize| {
            let (start, len) = self.bounds.get(id).copied().unwrap_or((0, 0));
            let (start, len) = (start as usize, len as usize);
            self.secs.get(start..start.saturating_add(len)).unwrap_or_default()
        };
        IndexView::from_parts(
            self.entries,
            &self.strs,
            [sec(CUR), sec(CURP), sec(XML), sec(XMLP), sec(FUZ), sec(FUZP), sec(CHARS)],
        )
    }

    /// The bytes this index keeps resident.
    pub fn bytes(&self) -> usize {
        self.strs.capacity() + self.secs.capacity()
    }
}

/// The strings of an index, each once.
#[derive(Default)]
struct Strs<'a> {
    seen: HashMap<&'a str, u32>,
    bytes: Vec<u8>,
}

impl<'a> Strs<'a> {
    /// The offset of `s` in the bytes, stored on the first call.
    fn intern(&mut self, s: &'a str) -> usize {
        if let Some(&off) = self.seen.get(s) {
            return off as usize;
        }
        let off = self.bytes.len();
        self.bytes.extend_from_slice(s.as_bytes());
        self.seen.insert(s, u32::try_from(off).unwrap_or(u32::MAX));
        off
    }
}

/// One dictionary record.
fn record(sec: &mut Vec<u8>, off: usize, len: usize, first: usize, count: usize) {
    put_u32(sec, u32::try_from(off).unwrap_or(u32::MAX));
    sec.extend_from_slice(&u16::try_from(len).unwrap_or(u16::MAX).to_le_bytes());
    sec.extend_from_slice(&[0, 0]);
    put_u32(sec, u32::try_from(first).unwrap_or(u32::MAX));
    put_u32(sec, u32::try_from(count).unwrap_or(u32::MAX));
}

/// One dictionary record, read.
struct Rec<'a> {
    text: &'a [u8],
    first: usize,
    count: usize,
}

/// A searchable index over borrowed bytes (an owned `IndexBuf`, later a mapped arena).
#[derive(Clone, Copy, Debug)]
pub struct IndexView<'a> {
    entries: u32,
    strs: &'a [u8],
    secs: [&'a [u8]; SECTIONS],
}

impl<'a> IndexView<'a> {
    pub fn from_parts(entries: u32, strs: &'a [u8], secs: [&'a [u8]; SECTIONS]) -> IndexView<'a> {
        IndexView { entries, strs, secs }
    }

    /// The number of entries the index was built over: a caller whose catalogue has another length does not use it.
    pub fn entries(&self) -> usize {
        self.entries as usize
    }

    fn sec(&self, id: usize) -> &'a [u8] {
        self.secs.get(id).copied().unwrap_or_default()
    }

    fn rec(&self, sec: &[u8], i: usize) -> Option<Rec<'a>> {
        let at = i.checked_mul(REC)?;
        let off = u32_at(sec, at)? as usize;
        let len = usize::from(u16_at(sec, at.checked_add(4)?)?);
        let text = self.strs.get(off..off.checked_add(len)?)?;
        Some(Rec {
            text,
            first: u32_at(sec, at.checked_add(8)?)? as usize,
            count: u32_at(sec, at.checked_add(12)?)? as usize,
        })
    }

    /// The first record of `sec` whose text is not below `q`.
    fn lower_bound(&self, sec: &[u8], q: &[u8]) -> usize {
        let (mut lo, mut hi) = (0, sec.len() / REC);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.rec(sec, mid) {
                Some(r) if r.text < q => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo
    }

    /// How many characters of `qc` occur in no name of the catalogue (each costs an edit wherever it goes).
    pub fn foreign(&self, qc: &[char]) -> usize {
        let chars = self.sec(CHARS);
        let n = chars.len() / 4;
        let has = |ch: char| {
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                match u32_at(chars, mid * 4) {
                    Some(v) if v < ch as u32 => lo = mid + 1,
                    Some(v) if v == ch as u32 => return true,
                    _ => hi = mid,
                }
            }
            false
        };
        qc.iter().filter(|&&ch| !has(ch)).count()
    }

    /// The entries that have a word suffix of a name within `dist` edits of a prefix of `qc` (tier 8's near-prefix test, as
    /// `rank::match_fuzzy` makes it per candidate), sorted. `dist` 0 matches nothing; more foreign characters than
    /// `dist` cannot match, and no target is looked at.
    pub fn fuzzy(&self, qc: &[char], dist: usize, out: &mut Vec<u32>) {
        out.clear();
        if dist == 0 || qc.is_empty() || self.foreign(qc) > dist {
            return;
        }
        let (fuz, fuzp) = (self.sec(FUZ), self.sec(FUZP));
        let mut walk = PrefixWalk::new(qc, dist);
        for t in 0..fuz.len() / REC {
            let Some(r) = self.rec(fuz, t) else { continue };
            let Ok(text) = std::str::from_utf8(r.text) else { continue };
            if walk.next(text) {
                for p in r.first..r.first.saturating_add(r.count) {
                    if let Some(e) = u32_at(fuzp, p.saturating_mul(XML_POST)) {
                        out.push(e);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }

    /// The hidden match of every entry for the trimmed, folded query `q` (`chars` its characters), as `rank::hidden_best`
    /// gives it per candidate: a curated keyword equal to `q` is T_SYN_EQ, one that starts with it (2+ characters)
    /// T_SYN_PREFIX, an XML keyword equal to it T_BAG_EQ (penalty = the sentinel; an XML word never matches by prefix).
    /// `out` is cleared, then holds the best (tier, penalty) of each entry that has one, sorted by entry.
    pub fn hidden(&self, q: &str, chars: usize, out: &mut Vec<Hit>) {
        out.clear();
        let q = q.as_bytes();
        if q.is_empty() {
            return;
        }
        let cur = self.sec(CUR);
        let curp = self.sec(CURP);
        let lo = self.lower_bound(cur, q);
        let prefixes = |f: &mut dyn FnMut(&Rec<'a>, bool)| {
            let mut k = lo;
            while let Some(r) = self.rec(cur, k) {
                if !r.text.starts_with(q) {
                    break;
                }
                let eq = r.text.len() == q.len();
                if !eq && chars < 2 {
                    break;
                }
                f(&r, eq);
                k += 1;
            }
        };
        let mut total = 0usize;
        prefixes(&mut |r, _| total = total.saturating_add(r.count));
        let xml = self.sec(XML);
        let xmlp = self.sec(XMLP);
        let xr = self.rec(xml, self.lower_bound(xml, q)).filter(|r| r.text == q);
        out.reserve(total.saturating_add(xr.as_ref().map_or(0, |r| r.count)));
        prefixes(&mut |r, eq| {
            let tier = if eq { T_SYN_EQ } else { T_SYN_PREFIX };
            for p in r.first..r.first.saturating_add(r.count) {
                let at = p.saturating_mul(CUR_POST);
                if let (Some(e), Some(pen)) = (u32_at(curp, at), at.checked_add(4).and_then(|a| u32_at(curp, a))) {
                    out.push((e, tier, pen));
                }
            }
        });
        if let Some(r) = xr {
            for p in r.first..r.first.saturating_add(r.count) {
                if let Some(e) = u32_at(xmlp, p.saturating_mul(XML_POST)) {
                    out.push((e, T_BAG_EQ, XML_KEYWORD_PENALTY));
                }
            }
        }
        out.sort_unstable();
        out.dedup_by_key(|h| h.0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::catalog::{self, CatApp};
    use crate::launcher::model::SettingRow;
    use crate::launcher::rank;
    use crate::launcher::testkit::XorShift;
    use crate::start_search::Keys;

    const WORDS: [&str; 40] = [
        "shut",
        "down",
        "wifi",
        "wireless",
        "remote",
        "desktop",
        "display",
        "monitor",
        "screen",
        "sound",
        "audio",
        "mouse",
        "pointer",
        "keyboard",
        "update",
        "windows",
        "restart",
        "reset",
        "network",
        "vpn",
        "proxy",
        "battery",
        "power",
        "sleep",
        "bluetooth",
        "printer",
        "scanner",
        "camera",
        "privacy",
        "location",
        "storage",
        "disk",
        "clean",
        "theme",
        "color",
        "font",
        "language",
        "region",
        "time",
        "date",
    ];

    /// Settings and apps with curated synonyms (several penalties, some above the XML sentinel's neighbours), XML words,
    /// a word that is curated on one entry and XML on another, multi-word keywords and entries with no keyword.
    fn catalogue() -> (Vec<CatApp>, Vec<SettingRow>) {
        let mut r = XorShift::new(0x1DE5);
        let word = |r: &mut XorShift| *r.pick(&WORDS);
        let keys = |r: &mut XorShift| {
            let mut k = Keys::default();
            for _ in 0..(r.next_u64() % 9) {
                let w = if r.next_u64().is_multiple_of(4) {
                    format!("{} {}", word(r), word(r))
                } else {
                    word(r).to_string()
                };
                let penalty = match r.next_u64() % 5 {
                    0 => 0,
                    1 => 1,
                    2 => 400,
                    3 => 900,
                    _ => crate::start_search::XML_KEYWORD_PENALTY,
                };
                k.push(&w, penalty);
            }
            k.tidy()
        };
        let apps = (0..120)
            .map(|i| {
                let name = format!("{} {}", WORDS[i % 40], WORDS[(i * 7 + 3) % 40]);
                CatApp::new(&format!("Vendor.App{i}!App"), &name, keys(&mut r), (r.next_u64() % 500) as u32)
            })
            .collect();
        let settings = (0..300)
            .map(|i| {
                let name = format!("{} {}", WORDS[(i * 3) % 40], WORDS[(i * 11 + 1) % 40]);
                SettingRow::new(i as u32, &name, keys(&mut r), (r.next_u64() % 500) as u32)
            })
            .collect();
        (apps, settings)
    }

    /// The queries of the dictionary tests: every word, every prefix of every word, pairs, and words that match nothing.
    fn queries() -> Vec<String> {
        let mut qs: Vec<String> = Vec::new();
        for w in WORDS {
            let mut end = 0;
            for ch in w.chars() {
                end += ch.len_utf8();
                qs.push(w[..end].to_string());
            }
        }
        for i in 0..40 {
            qs.push(format!("{} {}", WORDS[i], WORDS[(i + 5) % 40]));
            let two = format!("{} {}", WORDS[i], WORDS[(i * 7 + 3) % 40]);
            qs.push(two[..two.len() - 1].to_string());
        }
        qs.extend(["zzz", "q", "x", "shut d", "rem de", " wifi ", "w", "wi"].map(String::from));
        qs
    }

    fn check(cands: &[rank::Cand], ix: &IndexBuf) {
        let view = ix.view();
        assert_eq!(view.entries(), cands.len());
        let mut hits = Vec::new();
        for raw in queries() {
            let q = raw.trim();
            let chars = q.chars().count();
            view.hidden(q, chars, &mut hits);
            assert!(hits.windows(2).all(|w| w[0].0 < w[1].0), "{q:?}: sorted by entry, one hit each");
            for (i, c) in cands.iter().enumerate() {
                let found = hits.iter().find(|h| h.0 as usize == i).map(|h| (h.1, h.2));
                assert_eq!(found, rank::hidden_best(c, q, chars), "{q:?} entry {i}");
                assert_eq!(rank::match_tiers_with(c, q, found), rank::match_tiers(c, q), "{q:?} entry {i}");
                if let Some(t) = rank::match_tiers(c, q) {
                    let a = rank::score_hidden(c, t, false, 3, found);
                    assert_eq!(a, rank::score_for(c, q, t, false, 3), "{q:?} entry {i}");
                }
            }
        }
    }

    #[test]
    fn dictionary_equals_scan() {
        let (apps, settings) = catalogue();
        let cands: Vec<_> = apps.iter().map(CatApp::cand).collect();
        check(&cands, &catalog::index_apps(&apps));
        let cands: Vec<_> = settings.iter().map(SettingRow::cand).collect();
        check(&cands, &catalog::index_settings(&settings));
    }

    /// The fuzzy dictionary finds exactly the entries `match_fuzzy` finds by near prefix (names and other names only).
    #[test]
    fn fuzzy_dictionary_equals_scan() {
        let (apps, settings) = catalogue();
        let (apps, settings): (Vec<_>, Vec<_>) =
            (apps.iter().map(CatApp::cand).collect(), settings.iter().map(SettingRow::cand).collect());
        let mut qs = queries();
        for w in WORDS {
            let c: Vec<char> = w.chars().collect();
            if c.len() < 4 {
                continue;
            }
            let mut a = c.clone();
            a.swap(1, 2);
            qs.push(a.iter().collect());
            let mut b = c.clone();
            b.remove(0);
            b.push('q');
            qs.push(b.iter().collect());
            let mut d = c;
            d.insert(2, 'x');
            d.remove(0);
            qs.push(d.iter().collect());
        }
        let mut found_any = false;
        for cands in [&apps, &settings] {
            let ix = IndexBuf::build(cands.iter().enumerate().map(|(i, c)| (i as u32, *c)));
            let mut near = Vec::new();
            for raw in &qs {
                let Some((chars, m)) = rank::fuzzy_chars(raw) else { continue };
                let qc = &chars[..m];
                let dist = rank::fuzzy_dist(m);
                ix.view().fuzzy(qc, dist, &mut near);
                for (i, c) in cands.iter().enumerate() {
                    let in_dict = near.binary_search(&(i as u32)).is_ok();
                    let sub = rank::subsequence_name(c, qc);
                    assert_eq!(in_dict || sub, rank::match_fuzzy(c, raw).is_some(), "{raw:?} entry {i}");
                    assert!(dist > 0 || !in_dict);
                    found_any |= in_dict;
                }
            }
        }
        assert!(found_any);
    }

    #[test]
    fn dictionary_reads_the_three_hidden_tiers() {
        let mut k0 = Keys::default();
        k0.push("wifi", 400);
        k0.push("remote desktop", 900);
        k0.push("shut", crate::start_search::XML_KEYWORD_PENALTY);
        let rows = vec![
            SettingRow::new(0, "Alpha", k0.tidy(), 1),
            SettingRow::new(1, "Beta", Keys::default(), 2),
            SettingRow::new(2, "Gamma", Keys::from(&[("wifi", crate::start_search::XML_KEYWORD_PENALTY)][..]), 3),
        ];
        let ix = catalog::index_settings(&rows);
        let mut out = Vec::new();
        ix.view().hidden("wifi", 4, &mut out);
        assert_eq!(out, vec![(0, rank::T_SYN_EQ, 400), (2, rank::T_BAG_EQ, crate::start_search::XML_KEYWORD_PENALTY)]);
        ix.view().hidden("remot", 5, &mut out);
        assert_eq!(out, vec![(0, rank::T_SYN_PREFIX, 900)]);
        ix.view().hidden("shut", 4, &mut out);
        assert_eq!(out, vec![(0, rank::T_BAG_EQ, crate::start_search::XML_KEYWORD_PENALTY)]);
        ix.view().hidden("shu", 3, &mut out);
        assert!(out.is_empty(), "an XML word never matches by prefix");
        ix.view().hidden("w", 1, &mut out);
        assert!(out.is_empty(), "one character matches no prefix");
        ix.view().hidden("", 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn empty_and_foreign_buffers_answer_nothing() {
        let mut near = vec![3];
        let mut out = vec![(1, 4, 1)];
        IndexBuf::default().view().hidden("a", 1, &mut out);
        assert!(out.is_empty());
        let junk = [7u8; 37];
        let view = IndexView::from_parts(
            5,
            &junk,
            [&junk, &junk[..13], &junk[..16], &junk, &junk[..32], &junk[..5], &junk[..7]],
        );
        view.hidden("ab", 2, &mut out);
        assert!(out.is_empty());
        view.fuzzy(&['a', 'b', 'c', 'd'], 1, &mut near);
        assert!(near.is_empty());
        assert_eq!(view.foreign(&['a', 'b']), 2);
        assert!(out.is_empty());
    }

    /// The keyword dictionaries cost at most 1.3 x the packed keyword text they replace (the settings rows' `Keys`); the
    /// fuzzy sections are measured apart.
    #[test]
    fn index_size_bounded() {
        let (_, settings) = catalogue();
        let packed: usize =
            settings.iter().map(|r| r.keys.raw().map(|(k, p)| k.len() + p.len() + 2).sum::<usize>()).sum();
        // Nameless candidates: only the keyword sections and their strings are built.
        fn bare(r: &SettingRow) -> rank::Cand<'_> {
            rank::Cand { folded: "", words: &[], initials: "", keys: &r.keys, extra: &[], rank: 0 }
        }
        let keywords = IndexBuf::build(settings.iter().enumerate().map(|(i, r)| (i as u32, bare(r))));
        let all = catalog::index_settings(&settings);
        println!(
            "packed keyword text {packed} B, keyword dictionaries {} B, with the fuzzy sections {} B",
            keywords.bytes(),
            all.bytes()
        );
        assert!(keywords.bytes() * 10 <= packed * 13, "{} B for {packed} B of keyword text", keywords.bytes());
    }
}
