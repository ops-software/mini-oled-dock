//! Folding and ranking of the launcher (SPEC 5.2.5): `fold`, the name index built once at load, the match tiers 0-8
//! (names, initials, hidden words, fuzzy) and the destructive default rule of 5.2.7. Windows' typo rewrite is not here:
//! it reads a file and so runs on the shell thread (L1a-12g); its rows carry the tiers of the query it read.

use super::keywords;
use super::model::{Item, Payload, Query, Score};
use crate::start_search::Keys;
use core::cmp::Reverse;

/// The base letters of U+00C0..=U+017F, one entry per code point; '\0' keeps the character as it is
/// (the multiplication and division signs, which the calculator reads).
const LATIN: [&str; 192] = [
    // U+00C0..U+00FF
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "ss", //
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "y", //
    // U+0100..U+017F
    "a", "a", "a", "a", "a", "a", "c", "c", "c", "c", "c", "c", "c", "c", "d", "d", //
    "d", "d", "e", "e", "e", "e", "e", "e", "e", "e", "e", "e", "g", "g", "g", "g", //
    "g", "g", "g", "g", "h", "h", "h", "h", "i", "i", "i", "i", "i", "i", "i", "i", //
    "i", "i", "ij", "ij", "j", "j", "k", "k", "k", "l", "l", "l", "l", "l", "l", "l", //
    "l", "l", "l", "n", "n", "n", "n", "n", "n", "n", "n", "n", "o", "o", "o", "o", //
    "o", "o", "oe", "oe", "r", "r", "r", "r", "r", "r", "s", "s", "s", "s", "s", "s", //
    "s", "s", "t", "t", "t", "t", "t", "t", "u", "u", "u", "u", "u", "u", "u", "u", //
    "u", "u", "u", "u", "w", "w", "y", "y", "y", "z", "z", "z", "z", "z", "z", "s", //
];

/// Appends `s` folded to `out`: lowercase, with the accented Latin letters of U+00C0..=U+017F reduced to their base
/// letters ("Attività" becomes "attivita", the sharp s becomes "ss"). Callers clear `out` first when they reuse it.
pub fn fold(s: &str, out: &mut String) {
    for c in s.chars() {
        fold_char(c, out);
    }
}

fn fold_char(c: char, out: &mut String) {
    let cp = c as u32;
    if (0xC0..=0x17F).contains(&cp) {
        match LATIN.get((cp - 0xC0) as usize) {
            Some(&"\0") | None => out.push(c),
            Some(base) => out.push_str(base),
        }
    } else {
        out.extend(c.to_lowercase());
    }
}

// ---------------------------------------------------------------- the name index

/// A name folded once, with the start offsets of its words and their initials (5.2.5). Built at load, never per
/// keystroke. Words are split on non-alphanumerics and on a lower-to-upper case change ("VisualStudio").
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Indexed {
    pub folded: String,
    /// Byte offsets into `folded` where a word starts.
    pub words: Vec<u16>,
    pub initials: String,
}

impl Indexed {
    pub fn new(name: &str) -> Indexed {
        let mut x = Indexed::default();
        x.reset(name);
        x
    }

    /// The index of another name, built in place: the buffers are kept, so a provider that indexes names per
    /// keystroke (window names and titles, the commands in the current language) allocates nothing once they are
    /// large enough.
    pub fn reset(&mut self, name: &str) {
        self.folded.clear();
        self.words.clear();
        self.initials.clear();
        let mut in_word = false;
        let mut prev: Option<char> = None;
        for c in name.chars() {
            if c.is_alphanumeric() {
                let starts = !in_word || prev.is_some_and(|p| p.is_lowercase() && c.is_uppercase());
                let before = self.folded.len();
                if starts {
                    if let Ok(off) = u16::try_from(before) {
                        self.words.push(off);
                    }
                }
                fold_char(c, &mut self.folded);
                if starts {
                    if let Some(first) = self.folded.get(before..).and_then(|t| t.chars().next()) {
                        self.initials.push(first);
                    }
                }
                in_word = true;
            } else {
                fold_char(c, &mut self.folded);
                in_word = false;
            }
            prev = Some(c);
        }
    }

    pub fn cand<'a>(&'a self, keys: &'a Keys, extra: &'a [&'static str], rank: u32) -> Cand<'a> {
        Cand { folded: &self.folded, words: &self.words, initials: &self.initials, keys, extra, rank }
    }
}

/// One candidate for matching: all of it borrowed from data built at load.
#[derive(Clone, Copy)]
pub struct Cand<'a> {
    pub folded: &'a str,
    /// Word start offsets into `folded`.
    pub words: &'a [u16],
    pub initials: &'a str,
    /// Windows' synonyms (start_search), already lowercase.
    pub keys: &'a Keys,
    /// The other names (aliases: an inbox program's names in both languages, a command's names and words), already
    /// folded: matched like the shown name.
    pub extra: &'a [&'static str],
    /// Microsoft's popularity rank, smaller is better.
    pub rank: u32,
}

impl Cand<'_> {
    /// The order key of 5.4.1 for a match of `tier`. `kind` is the row kind's rank (app, window, command...).
    pub fn score(&self, tier: u8, running: bool, kind: u8) -> Score {
        let len = u16::try_from(self.folded.chars().count()).unwrap_or(u16::MAX);
        Score { tier, learned: Reverse(0), idle: !running, kind, rank: self.rank, len }
    }
}

/// The score of a match of `tier`. In the hidden tiers (4, 6, 7) the synonym's penalty orders the rows before the kind:
/// `kind` is 0 and `rank` the penalty. Elsewhere it is `Cand::score` (5.4.1's order).
pub fn score_for(c: &Cand, q: &str, tier: u8, running: bool, kind: u8) -> Score {
    if matches!(tier, T_SYN_EQ | T_SYN_PREFIX | T_BAG_EQ) {
        let q = q.trim();
        return score_hidden(c, tier, running, kind, hidden_best(c, q, q.chars().count()));
    }
    c.score(tier, running, kind)
}

/// `score_for` when the hidden match is already known (the keyword dictionary's answer, L3 T9): nothing is scanned.
pub fn score_hidden(c: &Cand, tier: u8, running: bool, kind: u8, hidden: Option<(u8, u32)>) -> Score {
    let mut s = c.score(tier, running, kind);
    if let Some((t, penalty)) = hidden {
        if t == tier && matches!(tier, T_SYN_EQ | T_SYN_PREFIX | T_BAG_EQ) {
            s.kind = 0;
            s.rank = penalty;
        }
    }
    s
}

// ---------------------------------------------------------------- tiers

fn contains_every_word(c: &Cand, q: &str) -> bool {
    q.split_whitespace().all(|w| c.folded.contains(w))
}

/// Tiers 0-7 of 5.2.5 for the folded query `q`: the best tier that matches, or None. An empty query matches nothing
/// (the empty state shows no rows). Tier 8 is `match_fuzzy`, which the caller asks only when no candidate matched.
pub fn match_tiers(c: &Cand, q: &str) -> Option<u8> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    tiers_by(c, q, |chars| hidden_best(c, q, chars))
}

/// `match_tiers` when the hidden match of the candidate is already known (the keyword dictionary's answer, L3 T9): the
/// same stages, the same answers, no keyword text read.
pub fn match_tiers_with(c: &Cand, q: &str, hidden: Option<(u8, u32)>) -> Option<u8> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    tiers_by(c, q, |_| hidden)
}

/// Counts one stage of `tiers` (name, initials, hidden word, substring): at most 4 per candidate (the bench's linear
/// bound, LC-3).
#[cfg(test)]
fn stage() {
    TIER_CHECKS.with(|n| n.set(n.get() + 1));
}
#[cfg(not(test))]
#[inline(always)]
fn stage() {}

#[cfg(test)]
thread_local! {
    /// The tier stages `match_tiers` ran on this thread. Per thread, like the allocation counter of the test kit, so
    /// the tests running beside a measurement never add to it.
    pub(crate) static TIER_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// The tiers, best first (5.2.5, L3 T6). 8 is `match_fuzzy`; Windows' typo rewrite shows the rows of the query it read, at
// their own tiers.
/// A name equals the query.
pub(crate) const T_EXACT: u8 = 0;
/// A name starts with the query.
pub(crate) const T_PREFIX: u8 = 1;
/// A word of a name starts with the first query word and every query word is contained.
pub(crate) const T_WORD: u8 = 2;
/// The initials of the shown name (2+ characters, no space).
pub(crate) const T_INITIALS: u8 = 3;
/// A curated synonym (Windows' or ours) equals the query.
pub(crate) const T_SYN_EQ: u8 = 4;
/// Every query word is a substring of the shown name.
pub(crate) const T_SUBSTR: u8 = 5;
/// A curated synonym starts with the query (2+ characters).
pub(crate) const T_SYN_PREFIX: u8 = 6;
/// A settings XML keyword equals the query (never by prefix).
pub(crate) const T_BAG_EQ: u8 = 7;
/// Fuzzy: a subsequence or a near word prefix of a name.
pub(crate) const T_FUZZY: u8 = 8;

#[cfg(test)]
thread_local! {
    /// The penalties `hidden_best` parsed on this thread (L3 T4: one per matching key, none for the others).
    pub(crate) static PENALTY_PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One pass over the hidden keywords of `c`: the best (tier, penalty) of T_SYN_EQ, T_SYN_PREFIX and T_BAG_EQ, or None.
/// A key equal to `q` is a curated synonym (T_SYN_EQ) or an XML word (T_BAG_EQ, penalty = the sentinel); a curated key
/// that starts with `q` (2+ characters) is T_SYN_PREFIX; an XML key that only starts with `q` gives nothing. A penalty
/// is parsed only for a key that matched. Stops early on a T_SYN_EQ with penalty 1 or less. Allocates nothing.
pub(crate) fn hidden_best(c: &Cand, q: &str, chars: usize) -> Option<(u8, u32)> {
    let mut best: Option<(u8, u32)> = None;
    for (k, text) in c.keys.raw() {
        let eq = k == q;
        if !(eq || chars >= 2 && k.starts_with(q)) {
            continue;
        }
        #[cfg(test)]
        PENALTY_PARSES.with(|n| n.set(n.get() + 1));
        let penalty: u32 = text.parse().unwrap_or(u32::MAX);
        let bag = penalty == crate::start_search::XML_KEYWORD_PENALTY;
        let tier = match (eq, bag) {
            (true, false) => T_SYN_EQ,
            (true, true) => T_BAG_EQ,
            (false, false) => T_SYN_PREFIX,
            (false, true) => continue,
        };
        if best.is_none_or(|b| (tier, penalty) < b) {
            best = Some((tier, penalty));
        }
        if tier == T_SYN_EQ && penalty <= 1 {
            break;
        }
    }
    best
}

/// The name-like tier of one name (the shown name or an alias): 0 the name equals `q`, 1 it starts with `q`, 2 a word of
/// it starts with the first query word and every query word is contained in it. `starts` yields the byte offsets where
/// the words of `text` start.
fn name_like(text: &str, q: &str, mut starts: impl Iterator<Item = usize>) -> Option<u8> {
    if text == q {
        return Some(T_EXACT);
    }
    if text.starts_with(q) {
        return Some(T_PREFIX);
    }
    if q.split_whitespace().all(|w| text.contains(w)) {
        let first = q.split_whitespace().next().unwrap_or(q);
        if starts.any(|w| text.get(w..).is_some_and(|t| t.starts_with(first))) {
            return Some(T_WORD);
        }
    }
    None
}

/// The byte offsets where a word of an alias starts: an alphanumeric after a non-alphanumeric, or at 0.
fn alias_starts(a: &str) -> impl Iterator<Item = usize> + '_ {
    let mut prev = false;
    a.char_indices().filter_map(move |(i, ch)| {
        let alnum = ch.is_alphanumeric();
        let start = alnum && !prev;
        prev = alnum;
        start.then_some(i)
    })
}

/// The best name-like tier (0-2) over the shown name and every alias in `c.extra`. The UI language only chooses the shown
/// name: an alias in the other language matches the same way. Allocates nothing.
pub fn name_tier(c: &Cand, q: &str) -> Option<u8> {
    let mut best = name_like(c.folded, q, c.words.iter().map(|&w| usize::from(w)));
    for a in c.extra {
        if best == Some(0) {
            break;
        }
        if let Some(t) = name_like(a, q, alias_starts(a)) {
            best = Some(best.map_or(t, |b| b.min(t)));
        }
    }
    best
}

/// `match_tiers` for a trimmed query that is not empty: a name (0-2), the initials (3), a curated synonym equal to the
/// query (4), every word inside the shown name (5), then the other hidden word (6 or 7). Hidden words never beat a name.
fn tiers_by(c: &Cand, q: &str, hidden: impl FnOnce(usize) -> Option<(u8, u32)>) -> Option<u8> {
    stage();
    if let Some(t) = name_tier(c, q) {
        return Some(t);
    }
    let chars = q.chars().count();
    stage();
    if chars >= 2 && !q.contains(char::is_whitespace) && c.initials.contains(q) {
        return Some(T_INITIALS);
    }
    stage();
    let hidden = hidden(chars).map(|(t, _)| t);
    if hidden == Some(T_SYN_EQ) {
        return hidden;
    }
    stage();
    if contains_every_word(c, q) {
        return Some(T_SUBSTR);
    }
    hidden
}

/// The caller's policy in one place: every candidate through tiers 0-7; only when none matched, through tier 8.
/// Pushes (index, tier) into `out` (cleared first).
#[cfg(test)]
pub fn match_all(cands: &[Cand], q: &str, out: &mut Vec<(usize, u8)>) {
    out.clear();
    out.extend(cands.iter().enumerate().filter_map(|(i, c)| match_tiers(c, q).map(|t| (i, t))));
    if out.is_empty() {
        out.extend(cands.iter().enumerate().filter_map(|(i, c)| match_fuzzy(c, q).map(|t| (i, t))));
    }
}

/// The characters of the trimmed query `q` for the fuzzy tier, and how many; None when it has more than `FUZZY_MAX_Q`.
pub(crate) fn fuzzy_chars(q: &str) -> Option<([char; FUZZY_MAX_Q], usize)> {
    let mut qc = ['\0'; FUZZY_MAX_Q];
    let mut m = 0;
    for ch in q.trim().chars() {
        let slot = qc.get_mut(m)?;
        *slot = ch;
        m += 1;
    }
    Some((qc, m))
}

/// The edits tier 8 allows a query of `m` characters.
pub(crate) fn fuzzy_dist(m: usize) -> usize {
    match m {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// Tier 8's subsequence test: a query of 3+ characters without spaces runs through the shown name with at most 2 gaps.
pub(crate) fn subsequence_name(c: &Cand, qc: &[char]) -> bool {
    qc.len() >= 3 && qc.iter().all(|ch| !ch.is_whitespace()) && subsequence_gaps(c.folded, qc, 2)
}

const FUZZY_MAX_Q: usize = 32;
const FUZZY_BUF: usize = FUZZY_MAX_Q + 3;

/// Tier 8: the query is a subsequence of the shown name with at most 2 gaps (3+ characters), or within Damerau distance 1
/// (query of 4+ characters; 2 at 8+) of a word prefix of the shown name or of one of its other names (`extra`). Names
/// only: a hidden keyword is never compared. Allocates nothing.
pub fn match_fuzzy(c: &Cand, q: &str) -> Option<u8> {
    let (qc, m) = fuzzy_chars(q)?;
    let qc = qc.get(..m)?;
    let dist = fuzzy_dist(m);
    if subsequence_name(c, qc) {
        return Some(T_FUZZY);
    }
    if dist == 0 {
        return None;
    }
    let name =
        c.words.iter().filter_map(|&w| c.folded.get(usize::from(w)..)).any(|t| near_prefix_one_pass(t, qc, dist));
    let alias = |k: &str| {
        near_prefix_one_pass(k, qc, dist)
            || k.match_indices(' ').any(|(i, _)| k.get(i + 1..).is_some_and(|t| near_prefix_one_pass(t, qc, dist)))
    };
    if name || c.extra.iter().any(|k| alias(k)) {
        return Some(T_FUZZY);
    }
    None
}

/// `q` runs through `t` in order with at most `max_gaps` runs of skipped characters inside the match (greedy from every
/// place the first character occurs).
fn subsequence_gaps(t: &str, q: &[char], max_gaps: u8) -> bool {
    let Some((&first, rest)) = q.split_first() else { return false };
    t.char_indices().filter(|&(_, ch)| ch == first).any(|(at, ch)| {
        let mut want = rest.iter();
        let mut next = want.next();
        let mut gaps = 0u8;
        let mut in_gap = false;
        for ch in t.get(at + ch.len_utf8()..).unwrap_or("").chars() {
            let Some(&n) = next else { break };
            if ch == n {
                next = want.next();
                in_gap = false;
            } else if !in_gap {
                in_gap = true;
                gaps += 1;
                if gaps > max_gaps {
                    return false;
                }
            }
        }
        next.is_none()
    })
}

/// Is some prefix of `t` (the length of `q` plus or minus `dist`) within Damerau (optimal string alignment) distance
/// `dist` of `q`? One OSA matrix: its last row holds the distance from `q` to every prefix of `t` at once, so the
/// lengths `q - dist` to `q + dist` cost one pass instead of one each; a row whose smallest value exceeds `dist` ends it
/// (no later row can come back under it). Allocates nothing.
fn near_prefix_one_pass(t: &str, q: &[char], dist: usize) -> bool {
    type Row = [u8; FUZZY_BUF + 1];
    /// Above every distance that can matter: a cell outside the band or past the limit.
    const FAR: u8 = 100;
    let mut buf = ['\0'; FUZZY_BUF];
    let mut n = 0;
    for ch in t.chars().take(q.len() + dist) {
        if let Some(slot) = buf.get_mut(n) {
            *slot = ch;
            n += 1;
        }
    }
    let Some(b) = buf.get(..n) else { return false };
    let lo = q.len().saturating_sub(dist).max(1);
    // No prefix of `t` is long enough: nothing to compare.
    if n < lo {
        return false;
    }
    // `dist` edits spoil at most `dist` characters of `q` (an insertion spoils none, a swap keeps both): every other one
    // is in `t` within `dist` places of its own. Most words fail this after a few characters.
    let mut missing = 0;
    for (k, c) in q.iter().enumerate() {
        let near = b.get(k.saturating_sub(dist)..(k + dist + 1).min(n)).is_some_and(|w| w.contains(c));
        if !near {
            missing += 1;
            if missing > dist {
                return false;
            }
        }
    }
    let get = |r: &Row, i: usize| r.get(i).copied().unwrap_or(FAR);
    let limit = u8::try_from(dist).unwrap_or(u8::MAX);
    let mut older: Row = [FAR; FUZZY_BUF + 1];
    let mut prev: Row = [FAR; FUZZY_BUF + 1];
    for (j, s) in prev.iter_mut().enumerate().take(dist.min(n) + 1) {
        *s = j as u8;
    }
    // Only the cells within `dist` of the diagonal can hold a distance of `dist` or less.
    for (i, &ac) in q.iter().enumerate() {
        let row = i + 1;
        let mut cur: Row = [FAR; FUZZY_BUF + 1];
        let mut row_min = FAR;
        if row <= dist {
            if let Some(s) = cur.first_mut() {
                *s = row as u8;
            }
            row_min = row as u8;
        }
        for j in row.saturating_sub(dist).max(1)..=(row + dist).min(n) {
            #[cfg(test)]
            OSA_CELLS.with(|c| c.set(c.get() + 1));
            let bc = b.get(j - 1).copied().unwrap_or('\0');
            let sub = get(&prev, j - 1) + u8::from(ac != bc);
            let mut best = sub.min(get(&prev, j) + 1).min(get(&cur, j - 1) + 1);
            let swapped = i > 0 && j > 1 && q.get(i - 1) == Some(&bc) && b.get(j - 2) == Some(&ac);
            if swapped {
                best = best.min(get(&older, j - 2) + 1);
            }
            let best = best.min(FAR);
            if let Some(s) = cur.get_mut(j) {
                *s = best;
            }
            row_min = row_min.min(best);
        }
        if row_min > limit {
            return false;
        }
        older = prev;
        prev = cur;
    }
    (lo..=n).any(|len| get(&prev, len) <= limit)
}

/// `near_prefix_one_pass` over targets given in byte order, sharing the work of their common prefixes (a trie walked in
/// order). The distance table is computed one column per character of the target, only inside the band |i - j| <= dist,
/// and a target that shares with the previous one the prefix up to where that one matched, or where its band went all above
/// `dist`, is answered without computing a cell. Allocates nothing (a few hundred bytes on the stack).
pub struct PrefixWalk<'q> {
    q: &'q [char],
    dist: usize,
    /// The longest prefix of a target that is looked at: the query plus the distance.
    lim: usize,
    /// The shortest one that may match.
    lo: usize,
    /// Column j (the first j characters of the target) at `j * COL + i`, i the characters of the query.
    cols: [u8; COL * (FUZZY_BUF + 1)],
    /// The previous target's characters, 1-based.
    tch: [char; FUZZY_BUF + 1],
    /// Columns of the previous target that hold valid values.
    prev_len: usize,
    /// The prefix length where the previous target matched, or usize::MAX.
    found: usize,
    /// The prefix length where the previous target's band went all above `dist`, or usize::MAX.
    cut: usize,
}

const COL: usize = FUZZY_MAX_Q + 1;
/// Above every distance that can matter.
const FAR: u8 = 100;

impl<'q> PrefixWalk<'q> {
    /// A walk of `q` (at most `FUZZY_MAX_Q` characters) at distance `dist` (at most 2).
    pub fn new(q: &'q [char], dist: usize) -> PrefixWalk<'q> {
        let m = q.len();
        let mut w = PrefixWalk {
            q,
            dist,
            lim: m + dist,
            lo: m.saturating_sub(dist).max(1),
            cols: [FAR; COL * (FUZZY_BUF + 1)],
            tch: ['\0'; FUZZY_BUF + 1],
            prev_len: 0,
            found: usize::MAX,
            cut: usize::MAX,
        };
        for i in 0..=m.min(dist) {
            w.set(0, i, i as u8);
        }
        w
    }

    fn get(&self, j: usize, i: usize) -> u8 {
        self.cols.get(j * COL + i).copied().unwrap_or(FAR)
    }

    fn set(&mut self, j: usize, i: usize, v: u8) {
        if let Some(s) = self.cols.get_mut(j * COL + i) {
            *s = v;
        }
    }

    /// The answer of `near_prefix_one_pass(t, q, dist)` for the next target `t` (targets in byte order).
    pub fn next(&mut self, t: &str) -> bool {
        let (m, d) = (self.q.len(), self.dist);
        // The characters shared with the previous target, up to the prefix length that matters.
        let shared = self.lim.min(self.prev_len);
        let mut l = 0;
        let mut chars = t.chars();
        let mut pending: Option<char> = None;
        while l < shared {
            match chars.next() {
                Some(c) if Some(c) == self.tch.get(l + 1).copied() => l += 1,
                other => {
                    pending = other;
                    break;
                }
            }
        }
        if self.found <= l {
            self.prev_len = l.max(self.found);
            return true;
        }
        if self.cut <= l {
            self.prev_len = l;
            return false;
        }
        self.found = usize::MAX;
        self.cut = usize::MAX;
        let mut j = l;
        // When the loop above stopped at `shared` (not at a mismatch), the next character is still unread.
        let mut next = pending.or_else(|| if l < shared { None } else { chars.next() });
        while let Some(c) = next {
            if j >= self.lim {
                break;
            }
            j += 1;
            if let Some(slot) = self.tch.get_mut(j) {
                *slot = c;
            }
            let mut col = [FAR; COL];
            let mut min = FAR;
            if j <= d {
                if let Some(s) = col.first_mut() {
                    *s = j as u8;
                }
                min = j as u8;
            }
            let ilo = j.saturating_sub(d).max(1);
            let ihi = (j + d).min(m);
            #[cfg(test)]
            OSA_CELLS.with(|n| n.set(n.get() + (ihi + 1).saturating_sub(ilo) as u64));
            for i in ilo..=ihi {
                let a = self.q.get(i - 1).copied().unwrap_or('\0');
                let left = col.get(i - 1).copied().unwrap_or(FAR);
                let mut best = (self.get(j - 1, i - 1) + u8::from(a != c)).min(self.get(j - 1, i) + 1).min(left + 1);
                let swapped = i > 1 && j > 1 && self.q.get(i - 2) == Some(&c) && self.tch.get(j - 1) == Some(&a);
                if swapped {
                    best = best.min(self.get(j - 2, i - 2) + 1);
                }
                let best = best.min(FAR);
                if let Some(s) = col.get_mut(i) {
                    *s = best;
                }
                min = min.min(best);
            }
            for (i, &v) in col.iter().enumerate() {
                self.set(j, i, v);
            }
            if j >= self.lo && usize::from(col.get(m).copied().unwrap_or(FAR)) <= d {
                self.found = j;
                self.prev_len = j;
                return true;
            }
            if usize::from(min) > d {
                self.cut = j;
                self.prev_len = j;
                return false;
            }
            next = chars.next();
        }
        self.prev_len = j;
        false
    }
}

#[cfg(test)]
thread_local! {
    /// The OSA cells `near_prefix_one_pass` computed on this thread (KEY-3's count, per query on the bench).
    pub(crate) static OSA_CELLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The reference the one-pass version must equal: one OSA matrix per prefix length (the code before L3 T8).
#[cfg(test)]
fn near_prefix_reference(t: &str, q: &[char], dist: usize) -> bool {
    let mut buf = ['\0'; FUZZY_BUF];
    let mut n = 0;
    for ch in t.chars().take(q.len() + dist) {
        if let Some(slot) = buf.get_mut(n) {
            *slot = ch;
            n += 1;
        }
    }
    let Some(buf) = buf.get(..n) else { return false };
    (q.len().saturating_sub(dist).max(1)..=n).any(|len| buf.get(..len).is_some_and(|p| osa(q, p) <= dist))
}

/// Optimal string alignment distance (insert, delete, replace, swap of neighbours), three rows on the stack.
#[cfg(test)]
fn osa(a: &[char], b: &[char]) -> usize {
    type Row = [u8; FUZZY_BUF + 1];
    fn get(r: &Row, i: usize) -> u8 {
        r.get(i).copied().unwrap_or(u8::MAX)
    }
    fn set(r: &mut Row, i: usize, v: u8) {
        if let Some(s) = r.get_mut(i) {
            *s = v;
        }
    }
    let mut older: Row = [0; FUZZY_BUF + 1];
    let mut prev: Row = [0; FUZZY_BUF + 1];
    for j in 0..=b.len() {
        set(&mut prev, j, j as u8);
    }
    for (i, &ac) in a.iter().enumerate() {
        let mut cur: Row = [0; FUZZY_BUF + 1];
        set(&mut cur, 0, (i + 1) as u8);
        for (j, &bc) in b.iter().enumerate() {
            let sub = get(&prev, j) + u8::from(ac != bc);
            let mut best = sub.min(get(&prev, j + 1) + 1).min(get(&cur, j) + 1);
            let swapped = i > 0 && j > 0 && a.get(i - 1) == Some(&bc) && b.get(j - 1) == Some(&ac);
            if swapped {
                best = best.min(get(&older, j - 1) + 1);
            }
            set(&mut cur, j + 1, best);
        }
        older = prev;
        prev = cur;
    }
    usize::from(get(&prev, b.len()))
}

// ---------------------------------------------------------------- destructive rule

/// 5.2.7: may a destructive row be the default selection for the folded query `q`? Only when `q` equals one of its
/// `names` (name and aliases, folded), or has 4+ characters and is a prefix of a name or of a word of a name.
pub fn name_allows_default(q: &str, names: &[&str]) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return false;
    }
    let long = q.chars().count() >= 4;
    names.iter().any(|n| {
        *n == q
            || (long
                && (n.starts_with(q)
                    || n.match_indices(' ').any(|(i, _)| n.get(i + 1..).is_some_and(|w| w.starts_with(q)))))
    })
}

/// The destructive rule of 5.2.7 for a result row and the current query: true when the row may be the default
/// selection. A row that is not destructive always may.
pub fn default_allowed(item: &Item, q: &Query) -> bool {
    if !item.destructive {
        return true;
    }
    let mut title = String::new();
    fold(&item.title, &mut title);
    if name_allows_default(&q.folded, &[&title]) {
        return true;
    }
    match &item.payload {
        Payload::Command(id) => name_allows_default(&q.folded, keywords::destructive_names(*id)),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn folded(s: &str) -> String {
        let mut out = String::new();
        fold(s, &mut out);
        out
    }

    /// The walk answers like `near_prefix_reference` on each target, for sorted targets: the sharing of prefixes never changes an
    /// answer.
    #[test]
    fn prefix_walk_equals_the_reference() {
        use crate::launcher::testkit::XorShift;
        let mut all: Vec<String> = vec![String::new()];
        let mut from = 0;
        for _ in 0..5 {
            let end = all.len();
            for i in from..end {
                for ch in ['a', 'b', 'c'] {
                    all.push(format!("{}{ch}", all[i]));
                }
            }
            from = end;
        }
        let mut targets = all.clone();
        targets.extend(["ab c", "a bc", "é", "éa", "ab é"].map(String::from));
        targets.sort();
        let mut queries: Vec<String> = all.iter().filter(|s| !s.is_empty()).cloned().collect();
        queries.extend(["abcabc", "cbacba", "aaaaaa", "ab c", "éa"].map(String::from));
        let check = |targets: &[String], queries: &[String]| {
            for q in queries {
                let qc: Vec<char> = q.chars().collect();
                for dist in 0..=2 {
                    let mut walk = PrefixWalk::new(&qc, dist);
                    for t in targets {
                        assert_eq!(walk.next(t), near_prefix_reference(t, &qc, dist), "{t:?} {q:?} {dist}");
                    }
                }
            }
        };
        check(&targets, &queries);
        // Seeded: 300 sorted targets and 300 queries over six letters, up to 12 characters.
        let mut r = XorShift::new(0x0518);
        let mut word = |max: u64| -> String {
            let n = 1 + (r.next_u64() % max) as usize;
            (0..n).map(|_| char::from(b'a' + (r.next_u64() % 6) as u8)).collect()
        };
        let mut targets: Vec<String> = (0..300).map(|_| word(12)).collect();
        targets.sort();
        let queries: Vec<String> = (0..300).map(|_| word(12)).collect();
        check(&targets, &queries);
    }

    #[test]
    fn near_prefix_one_pass_equals_reference() {
        use crate::launcher::testkit::XorShift;
        // Every target and query over {a, b, c} up to 6 characters, at distances 0, 1 and 2.
        let mut words: Vec<Vec<char>> = vec![Vec::new()];
        let mut from = 0;
        for _ in 0..6 {
            let end = words.len();
            for i in from..end {
                for ch in ['a', 'b', 'c'] {
                    let mut w = words[i].clone();
                    w.push(ch);
                    words.push(w);
                }
            }
            from = end;
        }
        assert_eq!(words.len(), 1 + 3 + 9 + 27 + 81 + 243 + 729);
        let strings: Vec<String> = words.iter().map(|w| w.iter().collect()).collect();
        for t in &strings {
            for q in &words {
                for dist in 0..=2 {
                    assert_eq!(
                        near_prefix_one_pass(t, q, dist),
                        near_prefix_reference(t, q, dist),
                        "{t:?} {q:?} {dist}"
                    );
                }
            }
        }
        // 100,000 seeded pairs over six letters, up to 12 characters.
        let mut r = XorShift::new(0x0517);
        let word = |r: &mut XorShift| -> Vec<char> {
            let n = (r.next_u64() % 13) as usize;
            (0..n).map(|_| char::from(b'a' + (r.next_u64() % 6) as u8)).collect()
        };
        for _ in 0..100_000 {
            let (t, q) = (word(&mut r), word(&mut r));
            let t: String = t.into_iter().collect();
            let dist = (r.next_u64() % 3) as usize;
            assert_eq!(near_prefix_one_pass(&t, &q, dist), near_prefix_reference(&t, &q, dist), "{t:?} {q:?} {dist}");
        }
    }

    #[test]
    fn tier_order() {
        use crate::launcher::catalog::CatApp;
        use crate::launcher::model::SettingRow;
        use crate::start_search::XML_KEYWORD_PENALTY as XML;
        let app = |id: &str, name: &str| CatApp::new(id, name, Keys::default(), 1);
        let calc = app("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calculator");
        let notepad = app("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "Notepad");
        let rdc = app("Microsoft.Windows.RemoteDesktop", "Remote Desktop Connection");
        let control = app("Microsoft.Windows.ControlPanel", "Control Panel");
        let italian_calc = app("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calcolatrice");
        let remote = Indexed::new("Network");
        let remote_keys = Keys::from(&[("remote desktop", 900)][..]).tidy();
        let devices = SettingRow::new(0, "Gestione dispositivi", Keys::from(&[("device manager", XML)][..]).tidy(), 1);
        // One candidate per tier, the examples of the plan's Appendix B.1.
        let cases: [(Cand, &str, u8); 9] = [
            (calc.cand(), "calc", T_EXACT),
            (notepad.cand(), "notep", T_PREFIX),
            (rdc.cand(), "desktop conn", T_WORD),
            (rdc.cand(), "rdc", T_INITIALS),
            (rdc.cand(), "rdp", T_SYN_EQ),
            (control.cand(), "ntrol", T_SUBSTR),
            (remote.cand(&remote_keys, &[], 1), "remot", T_SYN_PREFIX),
            (devices.cand(), "device manager", T_BAG_EQ),
            (italian_calc.cand(), "calcolatirce", T_FUZZY),
        ];
        let mut last = None;
        for (c, q, want) in &cases {
            let got = match_tiers(c, q).or_else(|| match_fuzzy(c, q));
            assert_eq!(got, Some(*want), "{q}");
            assert!(last.is_none_or(|l| l < *want), "the tiers are in order");
            last = Some(*want);
        }
        assert_eq!(T_FUZZY, 8);
    }

    #[test]
    fn penalty_orders_hidden_tiers() {
        let x = Indexed::new("Wireless");
        let k = |p: u32| Keys::from(&[("wifi", p)][..]).tidy();
        let (k400, k650, k900) = (k(400), k(650), k(900));
        let (a, b, c) = (x.cand(&k400, &[], 5), x.cand(&k900, &[], 1), x.cand(&k650, &[], 9));
        let score = |c: &Cand, kind: u8| {
            let t = match_tiers(c, "wifi").unwrap();
            assert_eq!(t, T_SYN_EQ);
            score_for(c, "wifi", t, false, kind)
        };
        // Two settings (kind 3) with penalties 400 and 900 and a command (kind 2) with 650: the penalty decides before the
        // kind and before the popularity rank.
        let mut rows = [(score(&b, 3), "b"), (score(&c, 2), "c"), (score(&a, 3), "a")];
        rows.sort();
        assert_eq!(rows.map(|r| r.1), ["a", "c", "b"]);
        // Outside the hidden tiers the kind still comes first.
        let n = Indexed::new("Wifi tool");
        let by_name = |kind: u8, rank: u32| {
            let c = n.cand(Keys::none(), &[], rank);
            score_for(&c, "wifi", match_tiers(&c, "wifi").unwrap(), false, kind)
        };
        assert!(by_name(2, 9) < by_name(3, 1));
    }

    #[test]
    fn aliases_match_as_names() {
        use crate::launcher::catalog::CatApp;
        use crate::launcher::commands::COMMANDS;
        use crate::launcher::model::CommandId;
        let tier_of_command = |id: CommandId, q: &str| {
            let cmd = COMMANDS.iter().find(|c| c.id == id).unwrap();
            let x = Indexed::new(cmd.name());
            match_tiers(&x.cand(Keys::none(), cmd.keywords, 0), q)
        };
        crate::i18n::with_italian(true, || {
            // The shown name is Italian; the English names are names too.
            assert_eq!(tier_of_command(CommandId::ShutDown, "shut"), Some(1), "shut down");
            assert_eq!(tier_of_command(CommandId::Restart, "rest"), Some(1), "restart");
            assert_eq!(tier_of_command(CommandId::ShutDown, "shut down"), Some(0));
            let notepad = CatApp::new("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "Blocco note", Keys::default(), 1);
            assert_eq!(match_tiers(&notepad.cand(), "notep"), Some(1), "notepad");
            assert_eq!(match_tiers(&notepad.cand(), "blocco note"), Some(0));
            let ps = CatApp::new(
                "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}\\WindowsPowerShell\\v1.0\\powershell.exe",
                "Windows PowerShell",
                Keys::default(),
                1,
            );
            assert_eq!(match_tiers(&ps.cand(), "terminal"), None, "terminal is no name of Windows PowerShell");
            assert_eq!(match_tiers(&ps.cand(), "powershell"), Some(0));
        });
    }

    #[test]
    fn hidden_keys_are_classified() {
        use crate::start_search::XML_KEYWORD_PENALTY as XML;
        let keys =
            Keys::from(&[("wifi", 400), ("wifi", XML), ("remote desktop", 900), ("shut", XML), ("remotely", XML)][..])
                .tidy();
        let x = Indexed::new("Network");
        let c = x.cand(&keys, &[], 0);
        let best = |q: &str| hidden_best(&c, q, q.chars().count());
        assert_eq!(best("wifi"), Some((T_SYN_EQ, 400)), "the curated synonym wins over the XML word");
        assert_eq!(best("remot"), Some((T_SYN_PREFIX, 900)));
        assert_eq!(best("remote desktop"), Some((T_SYN_EQ, 900)));
        assert_eq!(best("shut"), Some((T_BAG_EQ, XML)));
        assert_eq!(best("shu"), None, "an XML word never matches by prefix");
        assert_eq!(best("remotel"), None);
        assert_eq!(best("x"), None);
        // One penalty parsed per matching key, none for the others.
        let parses = |q: &str| {
            PENALTY_PARSES.with(|n| n.set(0));
            let _ = best(q);
            PENALTY_PARSES.with(std::cell::Cell::get)
        };
        assert_eq!(parses("wifi"), 1);
        assert_eq!(parses("zzz"), 0);
        assert_eq!(parses("shu"), 1, "shut matched as a prefix: parsed, then dropped as XML");
        assert_eq!(parses("remot"), 2, "remote desktop and remotely both start with it");
    }

    #[test]
    fn fold_removes_accents_and_case() {
        assert_eq!(folded("Attività"), "attivita");
        assert_eq!(folded("Gestione ATTIVITÀ"), "gestione attivita");
        assert_eq!(folded("Straße"), "strasse");
        assert_eq!(folded("Ærø Łódź Çñ Œ"), "aero lodz cn oe");
        assert_eq!(folded("Ångström"), "angstrom");
        assert_eq!(folded("ÀÈÌÒÙ àèìòù é"), "aeiou aeiou e");
    }

    #[test]
    fn fold_keeps_operators_and_other_scripts() {
        assert_eq!(folded("2 × 3 ÷ 4"), "2 × 3 ÷ 4");
        assert_eq!(folded("ΑΒΓ"), "αβγ");
        assert_eq!(folded("日本語"), "日本語");
        assert_eq!(folded(""), "");
    }

    #[test]
    fn fold_table_covers_the_range_with_ascii() {
        for cp in 0xC0u32..=0x17F {
            let c = char::from_u32(cp).unwrap();
            let f = folded(&c.to_string());
            if cp == 0xD7 || cp == 0xF7 {
                assert_eq!(f, c.to_string());
            } else {
                assert!(!f.is_empty() && f.chars().all(|x| x.is_ascii_lowercase()), "U+{cp:04X} -> {f:?}");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod fixtures {
    use super::*;
    use crate::launcher::model::{CommandId, GlyphId, IconRef, ItemKey, Kind, Mode, NumberStyle, ProviderId, VerbSet};

    /// (English name, Italian name, folded id for the bilingual table, Windows synonyms).
    type App = (&'static str, &'static str, &'static str, &'static [(&'static str, u32)]);

    const NONE: &[(&str, u32)] = &[];
    const APPS: &[App] = &[
        ("Calculator", "Calcolatrice", "microsoft.windowscalculator_8wekyb3d8bbwe!app", NONE),
        ("Task Manager", "Gestione attività", "taskmgr.exe", NONE),
        ("File Explorer", "Esplora file", "explorer.exe", NONE),
        ("Notepad", "Blocco note", "notepad.exe", NONE),
        ("Control Panel", "Pannello di controllo", "control.exe", NONE),
        ("Command Prompt", "Prompt dei comandi", "cmd.exe", NONE),
        (
            "Settings",
            "Impostazioni",
            "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
            NONE,
        ),
        ("Paint", "Paint", "mspaint.exe", NONE),
        ("Snipping Tool", "Strumento di cattura", "microsoft.screensketch_8wekyb3d8bbwe!app", NONE),
        ("Terminal", "Terminale", "microsoft.windowsterminal_8wekyb3d8bbwe!app", NONE),
        ("Windows PowerShell", "Windows PowerShell", "powershell.exe", NONE),
        ("Registry Editor", "Editor del Registro di sistema", "regedit.exe", NONE),
        ("Services", "Servizi", "services.msc", NONE),
        ("Device Manager", "Gestione dispositivi", "devmgmt.msc", NONE),
        ("Disk Management", "Gestione disco", "diskmgmt.msc", NONE),
        ("Event Viewer", "Visualizzatore eventi", "eventvwr.msc", NONE),
        ("Remote Desktop Connection", "Connessione Desktop remoto", "mstsc.exe", &[("mstsc", 2518), ("rdp", 2210)]),
        ("Character Map", "Mappa caratteri", "charmap.exe", NONE),
        ("Magnifier", "Lente di ingrandimento", "magnify.exe", NONE),
        ("On-Screen Keyboard", "Tastiera su schermo", "osk.exe", NONE),
        ("Photos", "Foto", "microsoft.windows.photos_8wekyb3d8bbwe!app", &[("gallery", 900)]),
        ("Camera", "Fotocamera", "microsoft.windowscamera_8wekyb3d8bbwe!app", &[("webcam", 800)]),
        ("Clock", "Orologio", "microsoft.windowsalarms_8wekyb3d8bbwe!app", NONE),
        ("Mail", "Posta", "microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.mail", NONE),
        (
            "Calendar",
            "Calendario",
            "microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.calendar",
            NONE,
        ),
        ("Maps", "Mappe", "microsoft.windowsmaps_8wekyb3d8bbwe!app", &[("navigation", 700)]),
        ("Weather", "Meteo", "microsoft.bingweather_8wekyb3d8bbwe!app", &[("forecast", 600)]),
        ("Media Player", "Lettore multimediale", "microsoft.zunemusic_8wekyb3d8bbwe!microsoft.zunemusic", NONE),
        ("Sound Recorder", "Registratore di suoni", "microsoft.windowssoundrecorder_8wekyb3d8bbwe!app", NONE),
        ("Sticky Notes", "Note adesive", "microsoft.microsoftstickynotes_8wekyb3d8bbwe!app", NONE),
        ("Task Scheduler", "Utilità di pianificazione", "taskschd.msc", NONE),
        ("System Information", "Informazioni di sistema", "msinfo32.exe", NONE),
        ("Resource Monitor", "Monitoraggio risorse", "resmon.exe", NONE),
        ("Performance Monitor", "Monitor prestazioni", "perfmon.exe", NONE),
        ("Windows Security", "Sicurezza di Windows", "microsoft.sechealthui_8wekyb3d8bbwe!sechealthui", NONE),
        ("Microsoft Store", "Microsoft Store", "microsoft.windowsstore_8wekyb3d8bbwe!app", NONE),
        ("Microsoft Edge", "Microsoft Edge", "msedge.exe", NONE),
        ("Xbox", "Xbox", "microsoft.gamingapp_8wekyb3d8bbwe!microsoft.xboxapp", NONE),
        ("Word", "Word", "winword.exe", NONE),
        ("Excel", "Excel", "excel.exe", NONE),
        ("Outlook", "Outlook", "outlook.exe", NONE),
        ("Teams", "Teams", "msteams_8wekyb3d8bbwe!msteams", NONE),
        ("Visual Studio Code", "Visual Studio Code", "code.exe", NONE),
        ("Steam", "Steam", "steam.exe", NONE),
        ("Google Chrome", "Google Chrome", "chrome.exe", NONE),
        ("Firefox", "Firefox", "firefox.exe", NONE),
        ("Discord", "Discord", "discord.exe", NONE),
        ("Spotify", "Spotify", "spotify.exe", NONE),
        ("7-Zip File Manager", "7-Zip File Manager", "7zfm.exe", NONE),
        ("Git Bash", "Git Bash", "git-bash.exe", NONE),
        ("GitHub Desktop", "GitHub Desktop", "githubdesktop.exe", NONE),
        ("Docker Desktop", "Docker Desktop", "docker desktop.exe", NONE),
        ("Adobe Photoshop 2025", "Adobe Photoshop 2025", "photoshop.exe", NONE),
        ("VLC media player", "VLC media player", "vlc.exe", NONE),
        ("Notepad++", "Notepad++", "notepad++.exe", NONE),
        ("Microsoft Visual Studio", "Microsoft Visual Studio", "devenv.exe", NONE),
        ("OneDrive", "OneDrive", "onedrive.exe", NONE),
    ];

    /// English name and the XML words it carries in the fixture (tier 7).
    const XML_WORDS: &[(&str, &[&str])] = &[
        ("Photos", &["pictures"]),
        ("Camera", &["video"]),
        ("Maps", &["directions"]),
        ("Weather", &["temperature"]),
        ("Mail", &["inbox"]),
        ("Clock", &["stopwatch"]),
        ("Calendar", &["agenda"]),
    ];

    /// Commands: English name, Italian name, hidden keywords (commands.rs will own the real ones).
    const COMMANDS: &[(&str, &str, &[&str])] = &[
        ("Lock", "Blocca", &["lock", "blocca"]),
        ("Sleep", "Sospendi", &["sleep", "sospendi"]),
        ("Restart", "Riavvia", &["restart", "riavvia"]),
        ("Shut down", "Arresta", &["shut down", "arresta", "spegni"]),
        ("Sign out", "Disconnetti", &["sign out", "disconnetti"]),
        ("Window menu", "Menu finestra", &["window menu", "menu finestra", "sposta", "ridimensiona"]),
    ];

    struct Entry {
        en: &'static str,
        name: Indexed,
        keys: Keys,
        extra: &'static [&'static str],
        rank: u32,
        kind: u8,
    }

    fn pool(it: bool) -> Vec<Entry> {
        let mut v = Vec::new();
        for (i, (en, ita, id, keys)) in APPS.iter().enumerate() {
            let name = Indexed::new(if it { ita } else { en });
            // As `CatApp::new` builds it: the inbox entry's names, and its synonyms joining the keywords.
            let inbox = keywords::inbox_for(id, &name.folded).map(|(_, e)| e);
            let mut k = Keys::from(*keys);
            for s in inbox.map_or(&[][..], |e| e.synonyms) {
                k.push(s, 1);
            }
            // Settings-style XML words (the sentinel penalty) on a few entries: the T_BAG_EQ tier.
            for (app, words) in XML_WORDS {
                if app == en {
                    for w in *words {
                        k.push(w, crate::start_search::XML_KEYWORD_PENALTY);
                    }
                }
            }
            v.push(Entry {
                en,
                name,
                keys: k.tidy(),
                extra: inbox.map_or(&[][..], |e| e.names),
                rank: i as u32,
                kind: 0,
            });
        }
        for (i, (en, ita, extra)) in COMMANDS.iter().enumerate() {
            v.push(Entry {
                en,
                name: Indexed::new(if it { ita } else { en }),
                keys: Keys::default(),
                extra,
                rank: 5000 + i as u32,
                kind: 2,
            });
        }
        v
    }

    fn fold_str(s: &str) -> String {
        let mut o = String::new();
        fold(s, &mut o);
        o
    }

    /// The caller's policy: tiers 0-7 over everything, tier 8 only when they found nothing. First by Score, then by
    /// folded name.
    fn best(it: bool, query: &str) -> Option<(&'static str, u8)> {
        let pool = pool(it);
        let cands: Vec<Cand> = pool.iter().map(|e| e.name.cand(&e.keys, e.extra, e.rank)).collect();
        let mut hits = Vec::new();
        match_all(&cands, &fold_str(query), &mut hits);
        hits.into_iter()
            .min_by(|&(a, ta), &(b, tb)| {
                let (ca, cb) = (&cands[a], &cands[b]);
                let sa = ca.score(ta, false, pool[a].kind);
                let sb = cb.score(tb, false, pool[b].kind);
                sa.cmp(&sb).then_with(|| ca.folded.cmp(cb.folded))
            })
            .map(|(i, t)| (pool[i].en, t))
    }

    // L3 T5: every other-language name is a name (tiers 0-2), so the first answer of "map" and "vis" is the first of two
    // equal tier-1 rows (the fixture's ranks are list positions).
    const ROWS: &str = "
en|calculator|Calculator|0
en|notepad|Notepad|0
en|steam|Steam|0
en|paint|Paint|0
en|word|Word|0
en|excel|Excel|0
en|xbox|Xbox|0
en|outlook|Outlook|0
en|teams|Teams|0
en|photos|Photos|0
en|camera|Camera|0
en|clock|Clock|0
en|mail|Mail|0
en|maps|Maps|0
en|weather|Weather|0
en|services|Services|0
en|settings|Settings|0
en|terminal|Terminal|0
en|firefox|Firefox|0
en|discord|Discord|0
en|spotify|Spotify|0
en|onedrive|OneDrive|0
en|task manager|Task Manager|0
en|file explorer|File Explorer|0
en|control panel|Control Panel|0
en|notepad++|Notepad++|0
en|lock|Lock|0
en|sleep|Sleep|0
en|restart|Restart|0
en|shut down|Shut down|0
en|sign out|Sign out|0
en|window menu|Window menu|0
it|calcolatrice|Calculator|0
it|blocco note|Notepad|0
it|attività|Task Manager|2
it|gestione attività|Task Manager|0
it|esplora file|File Explorer|0
it|pannello di controllo|Control Panel|0
it|prompt dei comandi|Command Prompt|0
it|impostazioni|Settings|0
it|strumento di cattura|Snipping Tool|0
it|terminale|Terminal|0
it|servizi|Services|0
it|gestione dispositivi|Device Manager|0
it|gestione disco|Disk Management|0
it|visualizzatore eventi|Event Viewer|0
it|mappa caratteri|Character Map|0
it|lente di ingrandimento|Magnifier|0
it|tastiera su schermo|On-Screen Keyboard|0
it|foto|Photos|0
it|fotocamera|Camera|0
it|orologio|Clock|0
it|posta|Mail|0
it|calendario|Calendar|0
it|mappe|Maps|0
it|meteo|Weather|0
it|lettore multimediale|Media Player|0
it|registratore di suoni|Sound Recorder|0
it|note adesive|Sticky Notes|0
it|utilità di pianificazione|Task Scheduler|0
it|informazioni di sistema|System Information|0
it|monitoraggio risorse|Resource Monitor|0
it|monitor prestazioni|Performance Monitor|0
it|sicurezza di windows|Windows Security|0
it|arresta|Shut down|0
it|disconnetti|Sign out|0
it|sospendi|Sleep|0
it|riavvia|Restart|0
it|blocca|Lock|0
it|menu finestra|Window menu|0
en|calc|Calculator|0
en|task|Task Manager|1
en|file|File Explorer|1
en|control|Control Panel|1
en|comm|Command Prompt|1
en|set|Settings|1
en|snip|Snipping Tool|1
en|term|Terminal|1
en|regis|Registry Editor|1
en|serv|Services|1
en|dev|Device Manager|1
en|disk|Disk Management|1
en|event|Event Viewer|1
en|remote|Remote Desktop Connection|1
en|char|Character Map|1
en|magn|Magnifier|1
en|on-s|On-Screen Keyboard|1
en|pho|Photos|1
en|cam|Camera|1
en|clo|Clock|1
en|cal|Calculator|1
en|calen|Calendar|1
en|map|Character Map|1
en|weat|Weather|1
en|media|Media Player|1
en|sound|Sound Recorder|1
en|sticky|Sticky Notes|1
en|system|System Information|1
en|resource|Resource Monitor|1
en|perf|Performance Monitor|1
en|microsoft e|Microsoft Edge|1
en|microsoft s|Microsoft Store|1
en|vis|Event Viewer|1
en|goo|Google Chrome|1
en|fire|Firefox|1
en|disc|Discord|1
en|spot|Spotify|1
en|ste|Steam|1
en|git|Git Bash|1
en|doc|Docker Desktop|1
en|vlc|VLC media player|1
en|7-z|7-Zip File Manager|1
en|one|OneDrive|1
en|ex|Excel|1
en|out|Outlook|1
en|tea|Teams|1
en|xb|Xbox|1
en|wor|Word|1
en|rest|Restart|1
en|shut|Shut down|1
en|sign|Sign out|1
en|sle|Sleep|1
en|window m|Window menu|1
en|task man|Task Manager|1
en|note|Notepad|1
en|adobe|Adobe Photoshop 2025|1
en|rdp|Remote Desktop Connection|4
en|mstsc|Remote Desktop Connection|0
en|regedit|Registry Editor|0
en|cmd|Command Prompt|0
en|taskmgr|Task Manager|0
en|defender|Windows Security|4
en|browser|Microsoft Edge|4
en|edge|Microsoft Edge|2
en|explorer|File Explorer|2
en|spegni|Shut down|0
en|riavvia|Restart|0
en|disconnetti|Sign out|0
en|sospendi|Sleep|0
en|blocca|Lock|0
en|menu finestra|Window menu|0
en|sposta|Window menu|0
en|blocco note|Notepad|0
en|esplora file|File Explorer|0
en|calcolatrice|Calculator|0
en|gestione attivita|Task Manager|0
en|pannello di controllo|Control Panel|0
en|impostazioni|Settings|0
en|orologio|Clock|0
en|fotocamera|Camera|0
en|foto|Photos|0
en|meteo|Weather|0
en|posta|Mail|0
en|mappe|Maps|0
en|servizi|Services|0
en|calendario|Calendar|0
it|spegni|Shut down|0
it|calc|Calculator|0
it|esplora|File Explorer|1
it|blocco|Notepad|1
it|pannello|Control Panel|1
it|calculator|Calculator|0
it|task manager|Task Manager|0
it|notepad|Notepad|0
it|settings|Settings|0
it|file explorer|File Explorer|0
it|command prompt|Command Prompt|0
it|control panel|Control Panel|0
it|shut down|Shut down|0
it|photos|Photos|0
it|weather|Weather|0
it|clock|Clock|0
it|mail|Mail|0
it|camera|Camera|0
it|calendar|Calendar|0
it|maps|Maps|0
it|remote desktop|Remote Desktop Connection|0
en|studio|Visual Studio Code|2
en|studio code|Visual Studio Code|2
en|code|Visual Studio Code|2
en|manager|Task Manager|2
en|monitor|Resource Monitor|1
en|powershell|Windows PowerShell|0
en|power|Windows PowerShell|1
en|shell|Windows PowerShell|2
en|drive|OneDrive|2
en|desktop|Remote Desktop Connection|1
en|bash|Git Bash|2
en|sched|Task Scheduler|2
en|viewer|Event Viewer|2
en|keyboard|On-Screen Keyboard|2
en|recorder|Sound Recorder|2
en|tool|Snipping Tool|2
en|prompt|Command Prompt|1
en|panel|Control Panel|2
en|player|Media Player|2
en|security|Windows Security|2
en|store|Microsoft Store|2
en|chrome|Google Chrome|2
en|photoshop|Adobe Photoshop 2025|2
en|2025|Adobe Photoshop 2025|2
en|notes|Sticky Notes|2
en|menu|Window menu|1
en|desk rem|Remote Desktop Connection|2
en|manager task|Task Manager|2
en|explorer file|File Explorer|2
it|dispositivi|Device Manager|2
it|disco|Discord|1
it|eventi|Event Viewer|2
it|controllo|Control Panel|2
it|registro|Registry Editor|2
it|suoni|Sound Recorder|2
it|schermo|On-Screen Keyboard|2
it|caratteri|Character Map|2
it|risorse|Resource Monitor|2
it|prestazioni|Performance Monitor|2
it|pianificazione|Task Scheduler|2
it|cattura|Snipping Tool|2
it|comandi|Command Prompt|2
it|sicurezza|Windows Security|1
it|remoto|Remote Desktop Connection|2
it|ingrandimento|Magnifier|2
it|adesive|Sticky Notes|2
it|multimediale|Media Player|2
en|vsc|Visual Studio Code|3
en|fe|File Explorer|3
en|tm|Task Manager|3
en|cp|Control Panel|3
en|gb|Git Bash|3
en|gc|Google Chrome|3
en|dd|Docker Desktop|3
en|wps|Windows PowerShell|3
en|rdc|Remote Desktop Connection|3
en|ghd|GitHub Desktop|3
en|vmp|VLC media player|3
en|mvs|Microsoft Visual Studio|3
en|ws|Windows Security|3
en|dm|Device Manager|3
it|gd|Device Manager|3
it|ga|Task Manager|3
it|rds|Registry Editor|3
en|eam|Teams|5
en|oogle|Google Chrome|5
en|culator|Calculator|5
en|xplor|File Explorer|5
en|hrome|Google Chrome|5
en|ntrol|Control Panel|5
en|oto|Photos|5
en|ager|Task Manager|5
en|ntrol pa|Control Panel|5
en|calcol|Calculator|1
en|esplora|File Explorer|1
en|blocco|Notepad|1
en|pannello|Control Panel|1
en|gestione|Task Manager|1
en|impost|Settings|1
en|fotoc|Camera|1
en|prompt dei|Command Prompt|1
en|strumento|Snipping Tool|1
en|lettore|Media Player|1
en|registratore|Sound Recorder|1
en|registrator|Sound Recorder|1
en|lente|Magnifier|1
en|tastiera|On-Screen Keyboard|1
en|mappa|Character Map|1
en|visualizzatore|Event Viewer|1
en|utilita|Task Scheduler|1
en|sicurezza|Windows Security|1
en|informazioni|System Information|1
en|monitoraggio|Resource Monitor|1
en|arre|Shut down|1
en|riav|Restart|1
en|sosp|Sleep|1
en|discon|Sign out|1
en|bloc|Notepad|1
it|snipping|Snipping Tool|1
it|sticky|Sticky Notes|1
en|brows|Microsoft Edge|6
en|webcam|Camera|4
en|gallery|Photos|4
en|navigation|Maps|4
en|forecast|Weather|4
en|pictures|Photos|7
en|video|Camera|7
en|directions|Maps|7
en|temperature|Weather|7
en|inbox|Mail|7
en|stopwatch|Clock|7
en|agenda|Calendar|7
en|defen|Windows Security|6
en|webc|Camera|6
en|galle|Photos|6
en|forec|Weather|6
en|navig|Maps|6
en|clacolatrice|Calculator|8
en|notpead|Notepad|8
en|calclator|Calculator|8
en|firefx|Firefox|8
en|chorme|Google Chrome|8
en|spotfy|Spotify|8
en|discrod|Discord|8
en|exlorer|File Explorer|8
en|taks manager|Task Manager|8
en|steem|Steam|8
en|gogle|Google Chrome|8
it|notpad|Notepad|8
it|pannelo di controllo|Control Panel|8
it|calcolatrcie|Calculator|8
en|qqqqqq|-|0
en|xyz|-|0
en|hello world|-|0
en|lock screen|-|0
it|zzzz|-|0
";

    #[test]
    fn fixtures_150() {
        let mut n = 0;
        let mut bad = Vec::new();
        for line in ROWS.trim().lines() {
            let f: Vec<&str> = line.split('|').collect();
            let (it, q, want, tier) = (f[0] == "it", f[1], f[2], f[3].parse::<u8>().unwrap());
            n += 1;
            let got = best(it, q);
            let ok = if want == "-" { got.is_none() } else { got == Some((want, tier)) };
            if !ok {
                bad.push(format!("{line}  ->  {got:?}"));
            }
        }
        assert!(n >= 150, "only {n} rows");
        assert!(bad.is_empty(), "{} of {n} rows differ:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn fixtures_cover_every_tier() {
        for t in 0..=8 {
            assert!(ROWS.lines().filter(|l| l.ends_with(&format!("|{t}"))).count() >= 6, "tier {t}");
        }
    }

    #[test]
    fn fuzzy_only_when_nothing_else() {
        let pool = pool(false);
        let cands: Vec<Cand> = pool.iter().map(|e| e.name.cand(&e.keys, e.extra, e.rank)).collect();
        let mut out = Vec::new();
        // "notpead" matches nothing in tiers 0-7, so the fuzzy pass runs and every hit is tier 8.
        assert!(cands.iter().all(|c| match_tiers(c, "notpead").is_none()));
        match_all(&cands, "notpead", &mut out);
        assert!(!out.is_empty() && out.iter().all(|&(_, t)| t == T_FUZZY));
        // "notepad" has tier 0-7 matches: no fuzzy row may appear next to them, though "notpad" is also close.
        match_all(&cands, "notepad", &mut out);
        assert!(!out.is_empty() && out.iter().all(|&(_, t)| t <= T_BAG_EQ));
        // match_tiers itself never answers the fuzzy tier.
        for q in ["notpead", "clacolatrice", "xyz", "calc", "vsc", "bloc"] {
            assert!(cands.iter().all(|c| match_tiers(c, q).is_none_or(|t| t <= T_BAG_EQ)));
        }
        // An empty query matches nothing, in either pass.
        assert!(cands.iter().all(|c| match_tiers(c, " ").is_none() && match_fuzzy(c, "").is_none()));
    }

    #[test]
    fn index_splits_words_and_builds_initials() {
        let x = Indexed::new("VisualStudio Code");
        assert_eq!(x.folded, "visualstudio code");
        assert_eq!(x.words, vec![0, 6, 13]);
        assert_eq!(x.initials, "vsc");
        let y = Indexed::new("Gestione attività (x64)");
        assert_eq!(y.folded, "gestione attivita (x64)");
        assert_eq!(y.words, vec![0, 9, 19]);
        assert_eq!(y.initials, "gax");
        let z = Indexed::new("PowerShell 7-preview");
        assert_eq!(z.initials, "ps7p");
        assert_eq!(Indexed::new("").words, Vec::<u16>::new());
        assert_eq!(Indexed::new("---").initials, "");
        // Folding can lengthen a name; offsets stay inside the folded text.
        let w = Indexed::new("Straße Ærø");
        assert_eq!(w.folded, "strasse aero");
        assert_eq!(w.words, vec![0, 8]);
    }

    #[test]
    fn osa_is_damerau_with_adjacent_swaps() {
        let d = |a: &str, b: &str| {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            osa(&a, &b)
        };
        assert_eq!(d("", ""), 0);
        assert_eq!(d("abc", "abc"), 0);
        assert_eq!(d("abc", "acb"), 1);
        assert_eq!(d("abc", "abd"), 1);
        assert_eq!(d("abc", "ab"), 1);
        assert_eq!(d("ab", "abc"), 1);
        assert_eq!(d("kitten", "sitting"), 3);
        assert_eq!(d("ca", "abc"), 3);
    }

    #[test]
    fn subsequence_allows_two_gaps_only() {
        let q = |s: &str| s.chars().collect::<Vec<char>>();
        assert!(subsequence_gaps("google chrome", &q("gogle"), 2));
        assert!(subsequence_gaps("visual studio code", &q("vstudio"), 2));
        assert!(subsequence_gaps("abcdef", &q("acdf"), 2));
        assert!(!subsequence_gaps("abcdefgh", &q("acegh"), 2));
        assert!(!subsequence_gaps("abc", &q("abcd"), 2));
        assert!(!subsequence_gaps("abc", &[], 2));
    }

    #[test]
    fn long_queries_do_not_overflow_the_fuzzy_buffers() {
        let x = Indexed::new("Calculator");
        let c = x.cand(Keys::none(), &[], 0);
        let long = "a".repeat(200);
        assert_eq!(match_fuzzy(&c, &long), None);
        assert_eq!(match_tiers(&c, &long), None);
    }

    // ------------------------------------------------------------ the destructive rule

    fn query(folded: &str) -> Query<'static> {
        Query {
            raw: "",
            text: "",
            folded: folded.to_string(),
            mode: Mode::Mixed,
            gen: 1,
            num: NumberStyle { decimal: '.', group: ',' },
            chip: None,
            apps_also: false,
        }
    }

    fn command(title: &str, id: CommandId, destructive: bool) -> Item {
        Item {
            key: ItemKey::of(ProviderId::Commands, title),
            from: ProviderId::Commands,
            kind: Kind::Command,
            title: title.into(),
            detail: None,
            icon: IconRef::Glyph(GlyphId::Power),
            score: Score { tier: 1, learned: Reverse(0), idle: true, kind: 2, rank: 0, len: 4 },
            verbs: VerbSet::empty(),
            payload: Payload::Command(id),
            destructive,
        }
    }

    fn destructive_items() -> Vec<(Item, &'static str)> {
        vec![
            (command("Restart", CommandId::Restart, true), "restart"),
            (command("Shut down", CommandId::ShutDown, true), "shut down"),
            (command("Sign out", CommandId::SignOut, true), "sign out"),
            (command("Sleep", CommandId::Sleep, true), "sleep"),
            // The Italian display names.
            (command("Riavvia", CommandId::Restart, true), "riavvia"),
            (command("Arresta", CommandId::ShutDown, true), "arresta"),
            (command("Disconnetti", CommandId::SignOut, true), "disconnetti"),
            (command("Sospendi", CommandId::Sleep, true), "sospendi"),
        ]
    }

    #[test]
    fn destructive_never_default_for_short_queries() {
        for (item, _) in destructive_items() {
            for q in ["s", "sh", "re", "spe", "sl", "si", "sig", "shu", "sle", "ri", "riav"] {
                // "riav" and the 4-character prefixes are checked below; the short ones never select.
                if q.chars().count() < 4 {
                    assert!(!default_allowed(&item, &query(q)), "{} with {q:?}", item.title);
                }
            }
            assert!(!default_allowed(&item, &query("")), "{} with an empty query", item.title);
            assert!(!default_allowed(&item, &query("xyz")), "{} with {:?}", item.title, "xyz");
        }
    }

    #[test]
    fn destructive_default_for_names_and_long_prefixes() {
        let allowed = |title: &str, id: CommandId, q: &str| default_allowed(&command(title, id, true), &query(q));
        for q in ["shut", "shut d", "shut down", "shutdown", "spegni", "arre", "arresta", "down"] {
            assert!(allowed("Shut down", CommandId::ShutDown, q), "shut down {q:?}");
        }
        for q in ["sleep", "slee", "sosp", "sospendi", "sospensione"] {
            assert!(allowed("Sleep", CommandId::Sleep, q), "sleep {q:?}");
        }
        for q in ["riav", "riavvia", "restart", "rest", "reboot"] {
            assert!(allowed("Restart", CommandId::Restart, q), "restart {q:?}");
        }
        for q in ["sign", "sign out", "disc", "disconnetti", "logoff", "log"] {
            // "log" is three characters and not a name: only the first five count.
            assert_eq!(allowed("Sign out", CommandId::SignOut, q), q != "log", "sign out {q:?}");
        }
        // "power" and "off" belong to no destructive name (the Power options page must not default to Shut down).
        for q in ["power", "power off", "off", "powe"] {
            assert!(!allowed("Shut down", CommandId::ShutDown, q), "shut down {q:?}");
        }
        // Not another command's words.
        assert!(!allowed("Sleep", CommandId::Sleep, "shut"));
        assert!(!allowed("Restart", CommandId::Restart, "sleep"));
        // The Italian title alone is enough, with no command table behind it.
        let mut it = command("Spegni il PC", CommandId::ShutDown, true);
        it.payload = Payload::None;
        assert!(default_allowed(&it, &query("spegni")));
        assert!(!default_allowed(&it, &query("spe")));
    }

    #[test]
    fn non_destructive_rows_are_always_allowed() {
        let lock = command("Lock", CommandId::Lock, false);
        for q in ["", "l", "xyz", "lock"] {
            assert!(default_allowed(&lock, &query(q)));
        }
    }
}
