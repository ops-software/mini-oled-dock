//! The keyword dictionary of a catalogue (L3 T9, SPEC 5.2.5 tiers 4, 6 and 7): every hidden keyword, de-duplicated and
//! sorted by its bytes, each with the run of entries that carry it, so a keystroke answers the hidden tiers with two
//! binary searches and one contiguous range instead of a scan of every candidate's keyword text. The buffer is plain bytes
//! (little-endian, checked sub-slices only), so a later arena can embed the same sections and a mapped file is searched
//! without a copy (SPEC appendix C.1, sections 3-9). Pure: no Windows, no files.

use super::rank::{Cand, T_BAG_EQ, T_SYN_EQ, T_SYN_PREFIX};
use crate::start_search::XML_KEYWORD_PENALTY;
use std::collections::HashMap;

/// Sections of the buffer, in order (C.1's ids 3 to 9).
const CUR: usize = 0;
const CURP: usize = 1;
const XML: usize = 2;
const XMLP: usize = 3;
const SECTIONS: usize = 4;

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
    /// The index of the keywords of `entries` ((position, candidate)): a keyword whose penalty is
    /// `XML_KEYWORD_PENALTY` goes to the XML dictionary, any other to the curated one with its penalty.
    pub fn build<'a>(entries: impl Iterator<Item = (u32, Cand<'a>)>) -> IndexBuf {
        let mut cur: Vec<(&str, u32, u32)> = Vec::new();
        let mut xml: Vec<(&str, u32)> = Vec::new();
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
        }
        // Sorted by (keyword, entry, penalty): a keyword twice on one entry keeps its best penalty.
        cur.sort_unstable();
        cur.dedup_by(|later, first| later.0 == first.0 && later.1 == first.1);
        xml.sort_unstable();
        xml.dedup();
        let mut b = IndexBuf { entries: n, ..IndexBuf::default() };
        let mut strs: HashMap<&str, u32> = HashMap::new();
        let mut put = |b: &mut IndexBuf, s: &'a str| -> u32 {
            if let Some(&off) = strs.get(s) {
                return off;
            }
            let off = u32::try_from(b.strs.len()).unwrap_or(u32::MAX);
            b.strs.extend_from_slice(s.as_bytes());
            strs.insert(s, off);
            off
        };
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
            let off = put(&mut b, k);
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
            let off = put(&mut b, k);
            if let Some(s) = sections.get_mut(XML) {
                record(s, off, k.len(), first, run);
            }
            i += run;
        }
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
        IndexView::from_parts(self.entries, &self.strs, [sec(CUR), sec(CURP), sec(XML), sec(XMLP)])
    }

    /// The bytes this index keeps resident.
    pub fn bytes(&self) -> usize {
        self.strs.capacity() + self.secs.capacity()
    }
}

/// One dictionary record.
fn record(sec: &mut Vec<u8>, off: u32, len: usize, first: usize, count: usize) {
    put_u32(sec, off);
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
        let mut out = vec![(1, 4, 1)];
        IndexBuf::default().view().hidden("a", 1, &mut out);
        assert!(out.is_empty());
        let junk = [7u8; 37];
        IndexView::from_parts(5, &junk, [&junk, &junk[..13], &junk[..16], &junk]).hidden("ab", 2, &mut out);
        assert!(out.is_empty());
    }

    /// The dictionary costs at most 1.3 x the packed keyword text it replaces (the settings rows' `Keys`).
    #[test]
    fn index_size_bounded() {
        let (_, settings) = catalogue();
        let packed: usize =
            settings.iter().map(|r| r.keys.raw().map(|(k, p)| k.len() + p.len() + 2).sum::<usize>()).sum();
        let ix = catalog::index_settings(&settings);
        println!("packed keyword text {packed} B, dictionary {} B", ix.bytes());
        assert!(ix.bytes() * 10 <= packed * 13, "{} B for {packed} B of keyword text", ix.bytes());
    }
}
