//! The catalogue arena (L3 T16, SPEC appendix C): one buffer per catalogue, searched in place. The apps and the settings are
//! each a single byte buffer (strings, one 48-byte record per entry, the word starts and the dictionary sections of
//! `index`), so a catalogue costs two blocks instead of thousands and a later change can lend the settings as a read-only
//! file mapping. `encode_*` builds a buffer (in a child, or in process when the child fails); `check` is the one walk that
//! validates a buffer from anywhere (a child's pipe, a cache file: untrusted bytes in the elevated dock) and `CatalogView`
//! reads it with checked sub-slices only. Pure: no Windows, no files, no `unsafe`.

use std::collections::HashMap;

use super::catalog::CatApp;
use super::index::{self, IndexView, Strs};
use super::keywords::INBOX;
use super::model::SettingRow;
use super::rank::{Cand, KeysRef, Words};

const APPS_HEADER: &[u8] = b"mini-oled-dock apps-catalog 1\n";
const SETTINGS_HEADER: &[u8] = b"mini-oled-dock settings-catalog 2\n";

/// Sections, in file order (C.1's ids).
const STR: usize = 0;
const ENT: usize = 1;
const WORDS: usize = 2;
const CUR: usize = 3;
const CURP: usize = 4;
const XML: usize = 5;
const XMLP: usize = 6;
const FUZ: usize = 7;
const FUZP: usize = 8;
const CHARS: usize = 9;
const TGT: usize = 10;
const SECTIONS: usize = 11;

/// Record sizes.
const ENT_SIZE: usize = 48;
const DICT_REC: usize = 16;
const CUR_POST: usize = 8;
const POST: usize = 4;
const TGT_REC: usize = 8;
/// The key line of a settings buffer, at most.
const KEY_MAX: usize = 2048;
/// A string, at most; the words of a name, at most.
const STR_MAX: usize = 64 * 1024;
const WORDS_MAX: usize = 64;
/// An app without an inbox entry.
const NO_INBOX: u16 = 0xFFFF;
/// Settings arguments: none.
const NO_ARGS: u32 = u32::MAX;

/// The bounds a buffer must keep: the dock passes `APPS` or `SETTINGS` to the encoder and to `check`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caps {
    pub entries: usize,
    pub bytes: usize,
}

impl Caps {
    pub const APPS: Caps = Caps { entries: 2_000, bytes: 2 << 20 };
    pub const SETTINGS: Caps = Caps { entries: 8_192, bytes: 4 << 20 };
    #[cfg(test)]
    pub const BENCH: Caps = Caps { entries: 16_384, bytes: 32 << 20 };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    Apps,
    Settings,
}

/// Why a buffer is not an arena (or an arena cannot be made): one variant per invariant of C.2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArenaError {
    /// Longer than the caps allow.
    TooBig,
    BadHeader,
    BadKey,
    /// A padding byte is not zero, or a buffer ends inside a field.
    BadPadding,
    /// `rest_len` does not match the buffer, or a byte trails.
    BadLength,
    BadChecksum,
    /// The section count is not 11.
    BadSections,
    /// A section is misaligned, out of order, overlapping or outside the buffer.
    BadSectionLayout,
    /// A section length is not a multiple of its record or does not fit its entries.
    BadSectionSize,
    TooManyEntries,
    /// A string reference outside STR.
    BadString,
    BadUtf8,
    StringTooLong,
    EmptyName,
    EmptyId,
    BadWords,
    /// A dictionary or fuzzy target out of order, empty, or with a record field that must be zero.
    BadDict,
    /// A posting range outside its section, empty, not consecutive, or sections left over.
    BadPostings,
    /// A posting entry at or above `entries`, or not increasing within its keyword.
    BadPostingEntry,
    BadChars,
    BadInbox,
    BadTarget,
    BadArgs,
}

impl std::fmt::Display for ArenaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

// ---------------------------------------------------------------- reading helpers

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    index::u32_at(b, at)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    index::u16_at(b, at)
}

fn align(n: usize, to: usize) -> Option<usize> {
    n.checked_add(to.checked_sub(1)?)?.checked_div(to)?.checked_mul(to)
}

/// FNV-1a 64 over the little-endian u64 words of `body` (a multiple of 8 long; a short tail is ignored).
fn checksum(body: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in body.chunks_exact(8) {
        if let Ok(w) = <[u8; 8]>::try_from(c) {
            h = (h ^ u64::from_le_bytes(w)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

// ---------------------------------------------------------------- layout and check

/// Where the sections of a checked buffer are: a few dozen bytes kept next to the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Layout {
    kind: Kind,
    entries: u32,
    /// (absolute offset, length) of each section.
    secs: [(u32, u32); SECTIONS],
}

impl Layout {
    pub fn entries(&self) -> usize {
        self.entries as usize
    }

    #[allow(dead_code)] // read by the cache file code of T18 and T19
    pub fn kind(&self) -> Kind {
        self.kind
    }
}

/// The strings of STR as a checked reader.
fn str_at(strs: &[u8], off: u32, len: usize) -> Result<&str, ArenaError> {
    let start = off as usize;
    let bytes = strs.get(start..start.checked_add(len).ok_or(ArenaError::BadString)?).ok_or(ArenaError::BadString)?;
    if len > STR_MAX {
        return Err(ArenaError::StringTooLong);
    }
    std::str::from_utf8(bytes).map_err(|_| ArenaError::BadUtf8)
}

fn section<'a>(bytes: &'a [u8], secs: &[(u32, u32); SECTIONS], id: usize) -> &'a [u8] {
    let (off, len) = secs.get(id).copied().unwrap_or((0, 0));
    let off = off as usize;
    bytes.get(off..off.saturating_add(len as usize)).unwrap_or_default()
}

/// The whole validation walk of C.2: the header, the sizes, every reference, every order, the checksum. After it every
/// accessor of a `CatalogView` over the same bytes and the returned `Layout` answers within the buffer. No panic on any input;
/// every count and offset is bounded by the buffer before it sizes a loop or an allocation (each loop below runs at most
/// once per 4 bytes of the buffer).
pub fn check(bytes: &[u8], kind: Kind, key: Option<&str>, caps: Caps) -> Result<Layout, ArenaError> {
    if bytes.len() > caps.bytes {
        return Err(ArenaError::TooBig);
    }
    // 1. Header, key line, padding.
    let head = if kind == Kind::Apps { APPS_HEADER } else { SETTINGS_HEADER };
    if !bytes.starts_with(head) {
        return Err(ArenaError::BadHeader);
    }
    let mut at = head.len();
    if kind == Kind::Settings {
        let rest = bytes.get(at..).unwrap_or_default();
        let window = rest.get(..rest.len().min(KEY_MAX + 1)).unwrap_or_default();
        let nl = window.iter().position(|&b| b == b'\n').ok_or(ArenaError::BadKey)?;
        let line = window.get(..nl).unwrap_or_default();
        if line.is_empty()
            || !line.iter().all(|b| (0x20..=0x7E).contains(b))
            || key.is_some_and(|k| k.as_bytes() != line)
        {
            return Err(ArenaError::BadKey);
        }
        at = at.checked_add(nl + 1).ok_or(ArenaError::BadKey)?;
    }
    let next = align(at, 4).ok_or(ArenaError::BadPadding)?;
    if !bytes.get(at..next).ok_or(ArenaError::BadPadding)?.iter().all(|&b| b == 0) {
        return Err(ArenaError::BadPadding);
    }
    // 2. rest_len, the total, the checksum.
    let rest_len = u32_at(bytes, next).ok_or(ArenaError::BadPadding)? as usize;
    let fields = next + 4;
    if fields.checked_add(rest_len) != Some(bytes.len()) {
        return Err(ArenaError::BadLength);
    }
    let body = bytes.get(..bytes.len().saturating_sub(8)).ok_or(ArenaError::BadLength)?;
    let tail = bytes.get(body.len()..).and_then(|t| <[u8; 8]>::try_from(t).ok()).ok_or(ArenaError::BadLength)?;
    if body.len() % 8 != 0 {
        return Err(ArenaError::BadLength);
    }
    if checksum(body) != u64::from_le_bytes(tail) {
        return Err(ArenaError::BadChecksum);
    }
    // 3. The section table.
    let entries = u32_at(bytes, fields).ok_or(ArenaError::BadPadding)?;
    if u32_at(bytes, fields + 4).ok_or(ArenaError::BadPadding)? as usize != SECTIONS {
        return Err(ArenaError::BadSections);
    }
    let table = fields + 8;
    let data = table + SECTIONS * 8;
    if data > body.len() {
        return Err(ArenaError::BadSectionLayout);
    }
    let data_len = body.len() - data;
    let mut secs = [(0u32, 0u32); SECTIONS];
    let mut rel = 0usize;
    for (id, slot) in secs.iter_mut().enumerate() {
        let off = u32_at(bytes, table + id * 8).ok_or(ArenaError::BadSectionLayout)? as usize;
        let len = u32_at(bytes, table + id * 8 + 4).ok_or(ArenaError::BadSectionLayout)? as usize;
        let end = off.checked_add(len).ok_or(ArenaError::BadSectionLayout)?;
        if !off.is_multiple_of(4) || Some(off) != align(rel, 4) || end > data_len {
            return Err(ArenaError::BadSectionLayout);
        }
        // The gap before the section is zero padding.
        if !bytes.get(data + rel..data + off).ok_or(ArenaError::BadSectionLayout)?.iter().all(|&b| b == 0) {
            return Err(ArenaError::BadPadding);
        }
        rel = end;
        *slot = (
            u32::try_from(data + off).map_err(|_| ArenaError::TooBig)?,
            u32::try_from(len).map_err(|_| ArenaError::TooBig)?,
        );
    }
    // After the last section: zero padding up to the checksum, nothing else.
    let end = data + rel;
    if end.next_multiple_of(8) != body.len() {
        return Err(ArenaError::BadLength);
    }
    if !bytes.get(end..body.len()).unwrap_or_default().iter().all(|&b| b == 0) {
        return Err(ArenaError::BadPadding);
    }
    // 4. Entries and the section sizes.
    if entries as usize > caps.entries {
        return Err(ArenaError::TooManyEntries);
    }
    let n = entries as usize;
    let layout = Layout { kind, entries, secs };
    let sec = |id: usize| section(bytes, &secs, id);
    let len_of = |id: usize| sec(id).len();
    let sizes = [
        (ENT, ENT_SIZE),
        (WORDS, 2),
        (CUR, DICT_REC),
        (CURP, CUR_POST),
        (XML, DICT_REC),
        (XMLP, POST),
        (FUZ, DICT_REC),
        (FUZP, POST),
        (CHARS, 4),
        (TGT, TGT_REC),
    ];
    if sizes.iter().any(|&(id, size)| len_of(id) % size != 0) || len_of(ENT) / ENT_SIZE != n {
        return Err(ArenaError::BadSectionSize);
    }
    if kind == Kind::Apps && len_of(TGT) != 0 {
        return Err(ArenaError::BadSectionSize);
    }
    // 5-9. Entries.
    let (strs, ent, words, tgt) = (sec(STR), sec(ENT), sec(WORDS), sec(TGT));
    let (word_n, tgt_n) = (words.len() / 2, tgt.len() / TGT_REC);
    for rec in ent.chunks_exact(ENT_SIZE) {
        let field = |at: usize| u32_at(rec, at).ok_or(ArenaError::BadPadding);
        let short = |at: usize| u16_at(rec, at).map(usize::from).ok_or(ArenaError::BadPadding);
        let name = str_at(strs, field(0)?, short(4)?)?;
        let folded = str_at(strs, field(6)?, short(10)?)?;
        str_at(strs, field(12)?, short(16)?)?;
        if name.is_empty() {
            return Err(ArenaError::EmptyName);
        }
        let (first, count) = (field(18)? as usize, short(22)?);
        let end = first.checked_add(count).ok_or(ArenaError::BadWords)?;
        if count > WORDS_MAX || end > word_n {
            return Err(ArenaError::BadWords);
        }
        let mut prev: Option<usize> = None;
        for w in first..end {
            let off = usize::from(u16_at(words, w * 2).ok_or(ArenaError::BadWords)?);
            if prev.is_some_and(|p| p >= off) || off >= folded.len() || !folded.is_char_boundary(off) {
                return Err(ArenaError::BadWords);
            }
            prev = Some(off);
        }
        str_at(strs, field(24)?, field(28)? as usize)?;
        let pad = if kind == Kind::Apps {
            if str_at(strs, field(36)?, short(40)?)?.is_empty() {
                return Err(ArenaError::EmptyId);
            }
            let inbox = u16_at(rec, 42).ok_or(ArenaError::BadPadding)?;
            if inbox != NO_INBOX && usize::from(inbox) >= INBOX.len() {
                return Err(ArenaError::BadInbox);
            }
            44
        } else {
            if short(36)? >= tgt_n {
                return Err(ArenaError::BadTarget);
            }
            let (off, len) = (field(38)?, field(42)?);
            if len == NO_ARGS {
                if off != 0 {
                    return Err(ArenaError::BadArgs);
                }
            } else {
                str_at(strs, off, len as usize).map_err(|_| ArenaError::BadArgs)?;
            }
            46
        };
        if !rec.get(pad..).unwrap_or_default().iter().all(|&b| b == 0) {
            return Err(ArenaError::BadPadding);
        }
    }
    for rec in tgt.chunks_exact(TGT_REC) {
        let len = u16_at(rec, 4).map(usize::from).ok_or(ArenaError::BadTarget)?;
        let off = u32_at(rec, 0).ok_or(ArenaError::BadTarget)?;
        if len == 0 || u16_at(rec, 6) != Some(0) || str_at(strs, off, len).is_err() {
            return Err(ArenaError::BadTarget);
        }
    }
    // Dictionaries and fuzzy targets.
    check_dict(sec(CUR), sec(CURP), CUR_POST, strs, entries)?;
    check_dict(sec(XML), sec(XMLP), POST, strs, entries)?;
    check_dict(sec(FUZ), sec(FUZP), POST, strs, entries)?;
    let mut prev: Option<u32> = None;
    for c in sec(CHARS).chunks_exact(4) {
        let v = u32_at(c, 0).ok_or(ArenaError::BadChars)?;
        if prev.is_some_and(|p| p >= v) || char::from_u32(v).is_none() {
            return Err(ArenaError::BadChars);
        }
        prev = Some(v);
    }
    Ok(layout)
}

/// One dictionary and its postings: texts strictly increasing, non-empty runs that follow one another from the start of
/// the posting section and end at its end (so the runs cannot overlap and their total is the section's size), posting
/// entries below `entries` and strictly increasing within a run.
fn check_dict(dict: &[u8], posts: &[u8], post_size: usize, strs: &[u8], entries: u32) -> Result<(), ArenaError> {
    let post_n = posts.len() / post_size;
    let mut prev: Option<&[u8]> = None;
    let mut next_first = 0usize;
    for rec in dict.chunks_exact(DICT_REC) {
        let off = u32_at(rec, 0).ok_or(ArenaError::BadDict)? as usize;
        let len = usize::from(u16_at(rec, 4).ok_or(ArenaError::BadDict)?);
        let text = strs.get(off..off.checked_add(len).ok_or(ArenaError::BadString)?).ok_or(ArenaError::BadString)?;
        if len == 0 || u16_at(rec, 6) != Some(0) || prev.is_some_and(|p| p >= text) {
            return Err(ArenaError::BadDict);
        }
        std::str::from_utf8(text).map_err(|_| ArenaError::BadUtf8)?;
        prev = Some(text);
        let first = u32_at(rec, 8).ok_or(ArenaError::BadDict)? as usize;
        let count = u32_at(rec, 12).ok_or(ArenaError::BadDict)? as usize;
        let end = first.checked_add(count).ok_or(ArenaError::BadPostings)?;
        if count == 0 || first != next_first || end > post_n {
            return Err(ArenaError::BadPostings);
        }
        let mut last: Option<u32> = None;
        for p in first..end {
            let e = u32_at(posts, p * post_size).ok_or(ArenaError::BadPostings)?;
            if e >= entries || last.is_some_and(|l| l >= e) {
                return Err(ArenaError::BadPostingEntry);
            }
            last = Some(e);
        }
        next_first = end;
    }
    if next_first != post_n {
        return Err(ArenaError::BadPostings);
    }
    Ok(())
}

// ---------------------------------------------------------------- the owned buffer and the view

/// A checked buffer with its layout: the bytes of one catalogue.
#[derive(Clone, Default, Debug)]
pub struct Arena {
    bytes: Vec<u8>,
    layout: Layout,
}

impl Arena {
    /// `bytes` if `check` accepts them.
    pub fn new(bytes: Vec<u8>, kind: Kind, key: Option<&str>, caps: Caps) -> Result<Arena, ArenaError> {
        let layout = check(&bytes, kind, key, caps)?;
        Ok(Arena { bytes, layout })
    }

    pub fn view(&self) -> CatalogView<'_> {
        CatalogView { bytes: &self.bytes, layout: &self.layout }
    }

    pub fn len(&self) -> usize {
        self.layout.entries()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes this arena keeps resident.
    pub fn bytes(&self) -> usize {
        self.bytes.capacity()
    }

    #[cfg(test)]
    pub fn raw(&self) -> &[u8] {
        &self.bytes
    }
}

/// A catalogue read in place. Every accessor is checked: on bytes that did not pass `check` (or a layout of other bytes) it
/// answers an empty value, never panics.
#[derive(Clone, Copy, Debug)]
pub struct CatalogView<'a> {
    bytes: &'a [u8],
    layout: &'a Layout,
}

/// The empty catalogue (nothing held yet).
impl Default for CatalogView<'static> {
    fn default() -> CatalogView<'static> {
        static EMPTY: Layout = Layout { kind: Kind::Apps, entries: 0, secs: [(0, 0); SECTIONS] };
        CatalogView { bytes: &[], layout: &EMPTY }
    }
}

impl<'a> CatalogView<'a> {
    #[allow(dead_code)] // T19 lends the mapped file's bytes with the layout `check` made of them
    pub fn new(bytes: &'a [u8], layout: &'a Layout) -> CatalogView<'a> {
        CatalogView { bytes, layout }
    }

    fn sec(&self, id: usize) -> &'a [u8] {
        section(self.bytes, &self.layout.secs, id)
    }

    fn rec(&self, i: usize) -> &'a [u8] {
        let at = i.saturating_mul(ENT_SIZE);
        self.sec(ENT).get(at..at.saturating_add(ENT_SIZE)).unwrap_or_default()
    }

    fn text(&self, off: Option<u32>, len: Option<usize>) -> &'a str {
        let (Some(off), Some(len)) = (off, len) else { return "" };
        let off = off as usize;
        let bytes = self.sec(STR).get(off..off.saturating_add(len)).unwrap_or_default();
        std::str::from_utf8(bytes).unwrap_or_default()
    }

    /// The string whose (u32 offset, u16 length) sit at `at` of record `i`.
    fn short(&self, i: usize, at: usize) -> &'a str {
        let rec = self.rec(i);
        self.text(u32_at(rec, at), u16_at(rec, at + 4).map(usize::from))
    }

    pub fn len(&self) -> usize {
        self.layout.entries()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn name(&self, i: usize) -> &'a str {
        self.short(i, 0)
    }

    /// The AppsFolder id (apps).
    pub fn id(&self, i: usize) -> &'a str {
        if self.layout.kind == Kind::Apps {
            self.short(i, 36)
        } else {
            ""
        }
    }

    pub fn rank(&self, i: usize) -> u32 {
        u32_at(self.rec(i), 32).unwrap_or(u32::MAX)
    }

    /// The other names of an inbox program (apps), by the stored index.
    pub fn names(&self, i: usize) -> &'static [&'static str] {
        if self.layout.kind != Kind::Apps {
            return &[];
        }
        match u16_at(self.rec(i), 42) {
            Some(x) if x != NO_INBOX => INBOX.get(usize::from(x)).map_or(&[], |e| e.names),
            _ => &[],
        }
    }

    /// The launch target and the arguments of a setting.
    #[allow(dead_code)] // T18 launches from the arena instead of the held list
    pub fn target(&self, i: usize) -> (&'a str, Option<&'a str>) {
        if self.layout.kind != Kind::Settings {
            return ("", None);
        }
        let rec = self.rec(i);
        let t = usize::from(u16_at(rec, 36).unwrap_or(u16::MAX));
        let rt = self.sec(TGT).get(t.saturating_mul(TGT_REC)..).unwrap_or_default();
        let target = self.text(u32_at(rt, 0), u16_at(rt, 4).map(usize::from));
        let args = match u32_at(rec, 42) {
            Some(NO_ARGS) | None => None,
            len => Some(self.text(u32_at(rec, 38), len.map(|l| l as usize))),
        };
        (target, args)
    }

    /// Start's packed keywords of a setting (lowercase, unfolded).
    #[allow(dead_code)] // T18 lets Start read them from the arena
    pub fn start_keys(&self, i: usize) -> &'a str {
        let rec = self.rec(i);
        self.text(u32_at(rec, 24), u32_at(rec, 28).map(|l| l as usize))
    }

    /// Entry `i` for matching: names and words borrowed from the buffer, no keywords (the hidden tiers come from `index`).
    pub fn cand(&self, i: usize) -> Cand<'a> {
        let rec = self.rec(i);
        let first = u32_at(rec, 18).map_or(0, |f| f as usize);
        let count = u16_at(rec, 22).map_or(0, usize::from);
        let words = self.sec(WORDS).get(first.saturating_mul(2)..first.saturating_add(count).saturating_mul(2));
        Cand {
            folded: self.short(i, 6),
            words: Words::new(words.unwrap_or_default()),
            initials: self.short(i, 12),
            keys: KeysRef::EMPTY,
            extra: self.names(i),
            rank: self.rank(i),
        }
    }

    /// The dictionaries and fuzzy targets of the buffer.
    pub fn index(&self) -> IndexView<'a> {
        IndexView::from_parts(
            self.layout.entries,
            self.sec(STR),
            [
                self.sec(CUR),
                self.sec(CURP),
                self.sec(XML),
                self.sec(XMLP),
                self.sec(FUZ),
                self.sec(FUZP),
                self.sec(CHARS),
            ],
        )
    }
}

// ---------------------------------------------------------------- the encoder

/// A setting as the encoder takes it: the folded row, its launch target and arguments, and Start's packed keywords.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SettingIn {
    pub row: SettingRow,
    pub target: String,
    pub args: Option<String>,
    pub start_keys: String,
}

/// One entry, ready to write.
struct Ent<'a> {
    name: &'a str,
    folded: &'a str,
    initials: &'a str,
    words: Vec<u16>,
    rank: u32,
    start_keys: &'a str,
    id: &'a str,
    inbox: u16,
    target: &'a str,
    args: Option<&'a str>,
    cand: Cand<'a>,
}

fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn pad_to(v: &mut Vec<u8>, multiple: usize) {
    while !v.len().is_multiple_of(multiple) {
        v.push(0);
    }
}

fn small(n: usize) -> Result<u32, ArenaError> {
    u32::try_from(n).map_err(|_| ArenaError::TooBig)
}

/// The buffer of the apps catalogue (apps already sorted as the dock shows them).
pub fn encode_apps(entries: &[CatApp], caps: Caps) -> Result<Vec<u8>, ArenaError> {
    let ents: Vec<Ent> = entries
        .iter()
        .map(|a| Ent {
            name: &a.name,
            folded: &a.folded,
            initials: &a.initials,
            words: normalized_words(&a.cand()),
            rank: a.rank,
            start_keys: "",
            id: &a.id,
            inbox: a.inbox.unwrap_or(NO_INBOX),
            target: "",
            args: None,
            cand: a.cand(),
        })
        .collect();
    encode(Kind::Apps, "", &ents, caps)
}

/// The buffer of the settings catalogue; `key` is the line that names what it was built from (printable ASCII, at most
/// 2 KiB). The position of a setting in `entries` is its index.
pub fn encode_settings(key: &str, entries: &[SettingIn], caps: Caps) -> Result<Vec<u8>, ArenaError> {
    let ents: Vec<Ent> = entries
        .iter()
        .map(|s| Ent {
            name: &s.row.name,
            folded: &s.row.folded,
            initials: &s.row.initials,
            words: normalized_words(&s.row.cand()),
            rank: s.row.rank,
            start_keys: &s.start_keys,
            id: "",
            inbox: NO_INBOX,
            target: &s.target,
            args: s.args.as_deref(),
            cand: s.row.cand(),
        })
        .collect();
    encode(Kind::Settings, key, &ents, caps)
}

/// The word starts of `c` as C.2 wants them: increasing, inside the folded name, on character boundaries, at most 64.
fn normalized_words(c: &Cand) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for w in c.words.iter() {
        let at = usize::from(w);
        let last = out.last().map(|&l| usize::from(l));
        if at < c.folded.len() && c.folded.is_char_boundary(at) && last.is_none_or(|l| l < at) && out.len() < WORDS_MAX
        {
            out.push(w);
        }
    }
    out
}

fn encode(kind: Kind, key: &str, ents: &[Ent], caps: Caps) -> Result<Vec<u8>, ArenaError> {
    if ents.len() > caps.entries {
        return Err(ArenaError::TooManyEntries);
    }
    if kind == Kind::Settings
        && (key.is_empty() || key.len() > KEY_MAX || !key.bytes().all(|b| (0x20..=0x7E).contains(&b)))
    {
        return Err(ArenaError::BadKey);
    }
    let mut st = Strs::default();
    let mut ent_sec: Vec<u8> = Vec::with_capacity(ents.len().saturating_mul(ENT_SIZE));
    let mut word_sec: Vec<u8> = Vec::new();
    let mut tgt_ix: HashMap<&str, u16> = HashMap::new();
    let mut tgt_sec: Vec<u8> = Vec::new();
    for e in ents {
        for s in [e.name, e.folded, e.initials, e.start_keys, e.id, e.target].into_iter().chain(e.args) {
            if s.len() > STR_MAX {
                return Err(ArenaError::StringTooLong);
            }
        }
        if e.name.is_empty() {
            return Err(ArenaError::EmptyName);
        }
        if kind == Kind::Apps && e.id.is_empty() {
            return Err(ArenaError::EmptyId);
        }
        let first = small(word_sec.len() / 2)?;
        for &w in &e.words {
            put_u16(&mut word_sec, w);
        }
        let (name, folded, initials) = (st.intern(e.name), st.intern(e.folded), st.intern(e.initials));
        for (off, s) in [(name, e.name), (folded, e.folded), (initials, e.initials)] {
            put_u32(&mut ent_sec, small(off)?);
            put_u16(&mut ent_sec, u16::try_from(s.len()).map_err(|_| ArenaError::StringTooLong)?);
        }
        put_u32(&mut ent_sec, first);
        put_u16(&mut ent_sec, u16::try_from(e.words.len()).map_err(|_| ArenaError::BadWords)?);
        put_u32(&mut ent_sec, small(st.intern(e.start_keys))?);
        put_u32(&mut ent_sec, small(e.start_keys.len())?);
        put_u32(&mut ent_sec, e.rank);
        if kind == Kind::Apps {
            put_u32(&mut ent_sec, small(st.intern(e.id))?);
            put_u16(&mut ent_sec, u16::try_from(e.id.len()).map_err(|_| ArenaError::StringTooLong)?);
            put_u16(&mut ent_sec, e.inbox);
        } else {
            if e.target.is_empty() {
                return Err(ArenaError::BadTarget);
            }
            let next = u16::try_from(tgt_ix.len()).map_err(|_| ArenaError::BadTarget)?;
            let t = *tgt_ix.entry(e.target).or_insert(next);
            if t == next {
                put_u32(&mut tgt_sec, small(st.intern(e.target))?);
                put_u16(&mut tgt_sec, u16::try_from(e.target.len()).map_err(|_| ArenaError::BadTarget)?);
                put_u16(&mut tgt_sec, 0);
            }
            put_u16(&mut ent_sec, t);
            match e.args {
                Some(a) => {
                    put_u32(&mut ent_sec, small(st.intern(a))?);
                    put_u32(&mut ent_sec, small(a.len())?);
                }
                None => {
                    put_u32(&mut ent_sec, 0);
                    put_u32(&mut ent_sec, NO_ARGS);
                }
            }
        }
        pad_to(&mut ent_sec, ENT_SIZE);
    }
    let (_, [cur, curp, xml, xmlp, fuz, fuzp, chars]) = index::build_sections(
        ents.iter().enumerate().map(|(i, e)| (u32::try_from(i).unwrap_or(u32::MAX), e.cand)),
        &mut st,
    );
    let secs: [&[u8]; SECTIONS] =
        [&st.bytes, &ent_sec, &word_sec, &cur, &curp, &xml, &xmlp, &fuz, &fuzp, &chars, &tgt_sec];
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(if kind == Kind::Apps { APPS_HEADER } else { SETTINGS_HEADER });
    if kind == Kind::Settings {
        out.extend_from_slice(key.as_bytes());
        out.push(b'\n');
    }
    pad_to(&mut out, 4);
    let rest_at = out.len();
    put_u32(&mut out, 0);
    put_u32(&mut out, small(ents.len())?);
    put_u32(&mut out, small(SECTIONS)?);
    let mut rel = 0usize;
    let mut offsets = [0usize; SECTIONS];
    for (id, s) in secs.iter().enumerate() {
        rel = align(rel, 4).ok_or(ArenaError::TooBig)?;
        if let Some(o) = offsets.get_mut(id) {
            *o = rel;
        }
        put_u32(&mut out, small(rel)?);
        put_u32(&mut out, small(s.len())?);
        rel = rel.checked_add(s.len()).ok_or(ArenaError::TooBig)?;
    }
    let data = out.len();
    for (s, off) in secs.iter().zip(offsets) {
        out.resize(data.checked_add(off).ok_or(ArenaError::TooBig)?, 0);
        out.extend_from_slice(s);
    }
    pad_to(&mut out, 8);
    let rest_len = out.len().checked_add(8).and_then(|t| t.checked_sub(rest_at + 4)).ok_or(ArenaError::TooBig)?;
    out.get_mut(rest_at..rest_at + 4).ok_or(ArenaError::TooBig)?.copy_from_slice(&small(rest_len)?.to_le_bytes());
    let sum = checksum(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    if out.len() > caps.bytes {
        return Err(ArenaError::TooBig);
    }
    Ok(out)
}

// ---------------------------------------------------------------- test builders

/// The arena of in-memory apps, through the real encoder (`Caps::BENCH`).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]
pub fn of_apps(apps: &[CatApp]) -> Arena {
    let bytes = encode_apps(apps, Caps::BENCH).expect("the apps encode");
    Arena::new(bytes, Kind::Apps, None, Caps::BENCH).expect("the encoding checks")
}

/// The arena of in-memory settings rows (each opens `ms-settings:`, no arguments): the position is the index.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]
pub fn of_settings(rows: &[SettingRow]) -> Arena {
    let ins: Vec<SettingIn> = rows
        .iter()
        .map(|row| SettingIn { row: row.clone(), target: "ms-settings:".into(), args: None, start_keys: String::new() })
        .collect();
    let bytes = encode_settings("test", &ins, Caps::BENCH).expect("the rows encode");
    Arena::new(bytes, Kind::Settings, Some("test"), Caps::BENCH).expect("the encoding checks")
}

/// An apps arena from (id, name, keywords, rank) rows, through the real encoder (`Caps::BENCH`).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]
pub fn test_apps(rows: &[(&str, &str, &[(&str, u32)], u32)]) -> Arena {
    let apps: Vec<CatApp> = rows
        .iter()
        .map(|&(id, name, keys, rank)| CatApp::new(id, name, crate::start_search::Keys::from(keys).tidy(), rank))
        .collect();
    let bytes = encode_apps(&apps, Caps::BENCH).expect("the rows encode");
    Arena::new(bytes, Kind::Apps, None, Caps::BENCH).expect("the encoding checks")
}

/// A settings arena from (name, keywords, rank) rows; every setting opens `ms-settings:` with no arguments.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]
pub fn test_settings(rows: &[(&str, &[(&str, u32)], u32)]) -> Arena {
    let ins: Vec<SettingIn> = rows
        .iter()
        .enumerate()
        .map(|(i, &(name, keys, rank))| SettingIn {
            row: SettingRow::new(i as u32, name, crate::start_search::Keys::from(keys).tidy(), rank),
            target: "ms-settings:".into(),
            args: None,
            start_keys: String::new(),
        })
        .collect();
    let bytes = encode_settings("test", &ins, Caps::BENCH).expect("the rows encode");
    Arena::new(bytes, Kind::Settings, Some("test"), Caps::BENCH).expect("the encoding checks")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::index::IndexBuf;
    use crate::launcher::rank;
    use crate::launcher::testkit::{peak_bytes_during, XorShift};
    use crate::start_search::{Keys, XML_KEYWORD_PENALTY};

    const WORDS_LIST: [&str; 24] = [
        "shut", "down", "wifi", "wireless", "remote", "desktop", "display", "monitor", "screen", "sound", "audio",
        "mouse", "pointer", "keyboard", "update", "windows", "restart", "reset", "network", "proxy", "battery",
        "power", "sleep", "color",
    ];

    /// Apps (some inbox programs, some with accents), settings (targets, arguments, Start's keys), curated and XML words.
    fn catalogue() -> (Vec<CatApp>, Vec<SettingIn>) {
        let mut r = XorShift::new(0xA2E7A);
        let keys = |r: &mut XorShift| {
            let mut k = Keys::default();
            for _ in 0..(r.next_u64() % 6) {
                let w = *r.pick(&WORDS_LIST);
                let w =
                    if r.next_u64().is_multiple_of(4) { format!("{w} {}", r.pick(&WORDS_LIST)) } else { w.to_string() };
                let p = *r.pick(&[0, 1, 400, 900, XML_KEYWORD_PENALTY]);
                k.push(&w, p);
            }
            k.tidy()
        };
        let mut apps: Vec<CatApp> = (0..40)
            .map(|i| {
                let name = format!("{} {}", WORDS_LIST[i % 24], WORDS_LIST[(i * 5 + 1) % 24]);
                CatApp::new(&format!("Vendor.App{i}!App"), &name, keys(&mut r), (r.next_u64() % 500) as u32)
            })
            .collect();
        apps.push(CatApp::new("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calcolatrice", Keys::default(), 7));
        apps.push(CatApp::new("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "Blocco note", Keys::default(), 8));
        apps.push(CatApp::new("Zeta.exe", "Àlfa Straße VisualStudio", Keys::default(), 9));
        let settings = (0..60)
            .map(|i| {
                let name = format!("{} {}", WORDS_LIST[(i * 3) % 24], WORDS_LIST[(i * 7 + 2) % 24]);
                SettingIn {
                    row: SettingRow::new(i as u32, &name, keys(&mut r), (r.next_u64() % 500) as u32),
                    target: format!("ms-settings:page{}", i % 7),
                    args: (i % 3 == 0).then(|| format!("--arg {i}")),
                    start_keys: if i % 2 == 0 { "wifi\t4\nreti\t9\n".into() } else { String::new() },
                }
            })
            .collect();
        (apps, settings)
    }

    fn apps_arena() -> (Vec<CatApp>, Arena) {
        let (apps, _) = catalogue();
        let a = Arena::new(encode_apps(&apps, Caps::BENCH).unwrap(), Kind::Apps, None, Caps::BENCH).unwrap();
        (apps, a)
    }

    fn settings_arena() -> (Vec<SettingIn>, Arena) {
        let (_, s) = catalogue();
        let bytes = encode_settings("a|b|fmt 2", &s, Caps::BENCH).unwrap();
        (s, Arena::new(bytes, Kind::Settings, Some("a|b|fmt 2"), Caps::BENCH).unwrap())
    }

    #[test]
    fn round_trip() {
        let (apps, a) = apps_arena();
        let v = a.view();
        assert_eq!(v.len(), apps.len());
        for (i, app) in apps.iter().enumerate() {
            let (c, want) = (v.cand(i), app.cand());
            assert_eq!((v.name(i), v.id(i), v.rank(i)), (&*app.name, &*app.id, app.rank));
            assert_eq!((c.folded, c.initials, c.rank, c.extra), (want.folded, want.initials, want.rank, want.extra));
            assert_eq!(c.words.iter().collect::<Vec<_>>(), want.words.iter().collect::<Vec<_>>());
            assert_eq!(c.words.len(), want.words.len());
            assert_eq!(v.names(i), app.extra);
            assert_eq!(v.start_keys(i), "");
        }
        let calc = (0..v.len()).find(|&i| v.name(i) == "Calcolatrice").unwrap();
        assert!(v.names(calc).contains(&"calculator"));
        let (settings, s) = settings_arena();
        let v = s.view();
        assert_eq!(v.len(), settings.len());
        for (i, st) in settings.iter().enumerate() {
            let (c, want) = (v.cand(i), st.row.cand());
            assert_eq!((v.name(i), v.rank(i)), (&*st.row.name, st.row.rank));
            assert_eq!((c.folded, c.initials), (want.folded, want.initials));
            assert_eq!(c.words.iter().collect::<Vec<_>>(), want.words.iter().collect::<Vec<_>>());
            assert_eq!(v.target(i), (st.target.as_str(), st.args.as_deref()));
            assert_eq!(v.start_keys(i), st.start_keys);
            assert_eq!(v.id(i), "");
        }
        // Out of range answers empty.
        assert_eq!((v.name(999), v.cand(999).folded, v.target(999), v.rank(999)), ("", "", ("", None), u32::MAX));
        // The empty catalogue is a valid buffer.
        let empty = Arena::new(encode_apps(&[], Caps::APPS).unwrap(), Kind::Apps, None, Caps::APPS).unwrap();
        assert_eq!((empty.len(), empty.view().cand(0).folded), (0, ""));
        assert!(Arena::default().view().is_empty());
    }

    #[test]
    fn arena_cands_carry_no_keys() {
        let (apps, a) = apps_arena();
        let (_, s) = settings_arena();
        assert!(apps.iter().any(|x| x.cand().keys.raw().next().is_some()), "the fixture has keywords");
        for v in [a.view(), s.view()] {
            for i in 0..v.len() {
                assert!(v.cand(i).keys.raw().next().is_none());
            }
        }
    }

    /// The dictionaries inside the arena answer as the in-memory build over the same candidates.
    #[test]
    fn view_answers_like_the_build() {
        let (apps, a) = apps_arena();
        let (settings, s) = settings_arena();
        let (mut hits, mut want) = (Vec::new(), Vec::new());
        let (mut near, mut near_want) = (Vec::new(), Vec::new());
        let mut qs: Vec<String> = Vec::new();
        for w in WORDS_LIST {
            qs.extend([
                w.to_string(),
                w[..w.len() - 1].to_string(),
                w[..2].to_string(),
                format!("{w}x"),
                format!("{w} {w}"),
            ]);
        }
        qs.extend(["calc", "calcolatrice", "blocco", "note", "alfa", "zzz", "q"].map(String::from));
        let sets: [(Vec<Cand>, CatalogView); 2] = [
            (apps.iter().map(CatApp::cand).collect(), a.view()),
            (settings.iter().map(|x| x.row.cand()).collect(), s.view()),
        ];
        for (cands, view) in &sets {
            let buf = IndexBuf::build(cands.iter().enumerate().map(|(i, c)| (i as u32, *c)));
            for q in &qs {
                let chars = q.chars().count();
                view.index().hidden(q, chars, &mut hits);
                buf.view().hidden(q, chars, &mut want);
                assert_eq!(hits, want, "{q:?}");
                if let Some((c, m)) = rank::fuzzy_chars(q) {
                    let (qc, dist) = (&c[..m], rank::fuzzy_dist(m));
                    view.index().fuzzy(qc, dist, &mut near);
                    buf.view().fuzzy(qc, dist, &mut near_want);
                    assert_eq!(near, near_want, "{q:?}");
                }
            }
            assert_eq!(view.index().entries(), cands.len());
        }
    }

    // ------------------------------------------------------------ mutations

    fn refix(b: &mut [u8]) {
        let n = b.len() - 8;
        let sum = checksum(&b[..n]);
        b[n..].copy_from_slice(&sum.to_le_bytes());
    }

    fn set32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn set16(b: &mut [u8], at: usize, v: u16) {
        b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn start(l: &Layout, id: usize) -> usize {
        l.secs[id].0 as usize
    }

    /// The offset of the `entries` field (the section table follows it).
    fn fields(l: &Layout) -> usize {
        start(l, STR) - SECTIONS * 8 - 8
    }

    /// Mutates a copy of `valid`, fixes the checksum and returns what `check` says.
    fn mutated(valid: &Arena, kind: Kind, f: impl FnOnce(&mut Vec<u8>, &Layout)) -> Result<Layout, ArenaError> {
        let mut b = valid.raw().to_vec();
        f(&mut b, &valid.layout);
        refix(&mut b);
        check(&b, kind, None, Caps::BENCH)
    }

    #[test]
    fn rejects_bad_input() {
        use ArenaError::*;
        let (_, a) = apps_arena();
        let (_, s) = settings_arena();
        let apps = |f: &dyn Fn(&mut Vec<u8>, &Layout)| mutated(&a, Kind::Apps, f);
        let sets = |f: &dyn Fn(&mut Vec<u8>, &Layout)| mutated(&s, Kind::Settings, f);
        // Untouched: valid.
        assert!(apps(&|_, _| {}).is_ok() && sets(&|_, _| {}).is_ok());
        let ent = |l: &Layout, i: usize| start(l, ENT) + i * ENT_SIZE;
        // 1. Header, key, padding.
        assert_eq!(apps(&|b, _| b[0] ^= 1), Err(BadHeader));
        assert_eq!(sets(&|b, _| b[0] ^= 1), Err(BadHeader));
        assert_eq!(check(a.raw(), Kind::Settings, None, Caps::BENCH), Err(BadHeader));
        assert_eq!(sets(&|b, _| b[SETTINGS_HEADER.len()] = 1), Err(BadKey));
        assert_eq!(check(s.raw(), Kind::Settings, Some("other|fmt 2"), Caps::BENCH), Err(BadKey));
        assert_eq!(check(s.raw(), Kind::Settings, Some("a|b|fmt 2"), Caps::BENCH).map(|l| l.entries()), Ok(60));
        assert_eq!(apps(&|b, l| b[ent(l, 0) + ENT_SIZE - 1] = 1), Err(BadPadding));
        assert_eq!(sets(&|b, l| b[ent(l, 0) + ENT_SIZE - 1] = 1), Err(BadPadding));
        // 2. rest_len, the total, the caps, the checksum.
        assert_eq!(
            apps(&|b, l| {
                let at = fields(l) - 4;
                let v = u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
                set32(b, at, v + 4)
            }),
            Err(BadLength)
        );
        assert_eq!(apps(&|b, _| b.extend_from_slice(&[0; 8])), Err(BadLength));
        let mut raw = a.raw().to_vec();
        raw.truncate(raw.len() - 1);
        assert_eq!(check(&raw, Kind::Apps, None, Caps::BENCH), Err(BadLength));
        assert_eq!(check(&[], Kind::Apps, None, Caps::BENCH), Err(BadHeader));
        let n = a.raw().len();
        assert_eq!(check(a.raw(), Kind::Apps, None, Caps { entries: 100, bytes: n - 1 }), Err(TooBig));
        assert_eq!(check(a.raw(), Kind::Apps, None, Caps { entries: a.len() - 1, bytes: n }), Err(TooManyEntries));
        let mut raw = a.raw().to_vec();
        raw[n - 1] ^= 1;
        assert_eq!(check(&raw, Kind::Apps, None, Caps::BENCH), Err(BadChecksum));
        // A flipped byte inside a launch target or its arguments: only the checksum sees it.
        let mut raw = s.raw().to_vec();
        let at = start(&s.layout, STR)
            + s.raw()[start(&s.layout, STR)..].windows(11).position(|w| w == b"ms-settings").unwrap();
        raw[at] ^= 0x20;
        assert_eq!(check(&raw, Kind::Settings, None, Caps::BENCH), Err(BadChecksum));
        // 3-4. The section table and the sizes.
        assert_eq!(apps(&|b, l| set32(b, fields(l) + 4, 10)), Err(BadSections));
        assert_eq!(
            apps(&|b, l| set32(b, fields(l) + 8 + 3 * 8, 8)),
            Err(BadSectionLayout),
            "a section out of its place"
        );
        assert_eq!(apps(&|b, l| set32(b, fields(l) + 8 + 3 * 8 + 4, u32::MAX)), Err(BadSectionLayout));
        assert_eq!(
            apps(&|b, l| set32(b, fields(l) + 8 + 8 + 4, u32::MAX - 3)),
            Err(BadSectionLayout),
            "a length that wraps"
        );
        assert_eq!(apps(&|b, l| set32(b, fields(l), 41)), Err(BadSectionSize), "entries against ENT's length");
        assert_eq!(sets(&|b, l| set32(b, fields(l), u32::MAX)), Err(TooManyEntries));
        // 5. Strings.
        assert_eq!(apps(&|b, l| set32(b, ent(l, 3), 0xFFFF_FFF0)), Err(BadString));
        assert_eq!(apps(&|b, l| set16(b, ent(l, 3) + 4, 0xFFFF)), Err(BadString));
        assert_eq!(apps(&|b, l| b[start(l, STR)] = 0xFF), Err(BadUtf8));
        assert_eq!(apps(&|b, l| set16(b, ent(l, 3) + 4, 0)), Err(EmptyName));
        assert_eq!(apps(&|b, l| set16(b, ent(l, 3) + 40, 0)), Err(EmptyId));
        assert_eq!(apps(&|b, l| set32(b, ent(l, 3) + 28, u32::MAX)), Err(BadString), "Start's keys outside STR");
        // 6. Words.
        assert_eq!(apps(&|b, l| set16(b, ent(l, 0) + 22, 65)), Err(BadWords));
        assert_eq!(apps(&|b, l| set32(b, ent(l, 0) + 18, u32::MAX)), Err(BadWords));
        assert_eq!(apps(&|b, l| set16(b, start(l, WORDS), 0xFFFF)), Err(BadWords), "a word start outside the name");
        assert_eq!(
            apps(&|b, l| {
                let f = start(l, WORDS);
                set16(b, f + 2, 0);
                set16(b, f, 0)
            }),
            Err(BadWords),
            "not increasing"
        );
        // 7. Dictionaries and postings.
        assert_eq!(apps(&|b, l| set16(b, start(l, CUR) + 4, 0)), Err(BadDict));
        assert_eq!(apps(&|b, l| set16(b, start(l, CUR) + 6, 1)), Err(BadDict));
        assert_eq!(
            apps(&|b, l| {
                let c = start(l, CUR);
                // The texts of the first two records trade places (their runs stay where they were).
                let (x, y) = (b[c..c + 8].to_vec(), b[c + 16..c + 24].to_vec());
                b[c..c + 8].copy_from_slice(&y);
                b[c + 16..c + 24].copy_from_slice(&x);
            }),
            Err(BadDict)
        );
        assert_eq!(apps(&|b, l| set32(b, start(l, CUR) + 12, 0)), Err(BadPostings), "an empty run");
        assert_eq!(apps(&|b, l| set32(b, start(l, CUR) + 12, u32::MAX)), Err(BadPostings), "a run past its section");
        assert_eq!(apps(&|b, l| set32(b, start(l, CUR) + 16 + 8, 0)), Err(BadPostings), "runs that overlap");
        assert_eq!(apps(&|b, l| set32(b, start(l, CURP), u32::MAX)), Err(BadPostingEntry));
        assert_eq!(apps(&|b, l| set32(b, start(l, FUZP), u32::MAX)), Err(BadPostingEntry));
        assert_eq!(
            apps(&|b, l| {
                // The last run gives up a posting: one is left over.
                let r = start(l, FUZ) + l.secs[FUZ].1 as usize - DICT_REC;
                let v = u32::from_le_bytes(b[r + 12..r + 16].try_into().unwrap());
                set32(b, r + 12, v - 1)
            }),
            Err(BadPostings)
        );
        // 8. Characters.
        assert_eq!(apps(&|b, l| set32(b, start(l, CHARS), 0xD800)), Err(BadChars));
        assert_eq!(
            apps(&|b, l| {
                let c = start(l, CHARS);
                set32(b, c + 4, 0)
            }),
            Err(BadChars)
        );
        // 9. Inbox, targets, arguments.
        assert_eq!(apps(&|b, l| set16(b, ent(l, 0) + 42, 0x7777)), Err(BadInbox));
        assert_eq!(sets(&|b, l| set16(b, ent(l, 5) + 36, 0xFFFF)), Err(BadTarget));
        assert_eq!(sets(&|b, l| set16(b, start(l, TGT) + 4, 0)), Err(BadTarget));
        assert_eq!(sets(&|b, l| set32(b, ent(l, 0) + 38, 0xFFFF_FF00)), Err(BadArgs));
        assert_eq!(
            sets(&|b, l| {
                set32(b, ent(l, 1) + 38, 5);
                set32(b, ent(l, 1) + 42, u32::MAX)
            }),
            Err(BadArgs),
            "none with an offset"
        );
        assert_eq!(sets(&|b, l| set16(b, ent(l, 0) + 36, 0xFFFF)), Err(BadTarget));
        // An app buffer with a settings-only section.
        assert_eq!(apps(&|b, l| set32(b, fields(l) + 8 + TGT * 8 + 4, 8)), Err(BadSectionLayout));
    }

    /// Every accessor and every query of a buffer `check` accepted: nothing panics, nothing is longer than the buffer.
    fn exercise(b: &[u8], kind: Kind) -> bool {
        let Ok(l) = check(b, kind, None, Caps::BENCH) else { return false };
        let v = CatalogView::new(b, &l);
        let ((), peak) = peak_bytes_during(|| {
            for i in 0..v.len() + 2 {
                let c = v.cand(i);
                let _ = (
                    v.name(i),
                    v.id(i),
                    v.target(i),
                    v.start_keys(i),
                    v.names(i),
                    c.words.iter().count(),
                    c.folded.len(),
                );
                assert!(c.keys.raw().next().is_none());
            }
            let ix = v.index();
            let (mut out, mut near) = (Vec::new(), Vec::new());
            for q in ["a", "wi", "wifi", "shut", "remote", "sleep", "zzz", "calc"] {
                ix.hidden(q, q.chars().count(), &mut out);
                assert!(out.len() <= b.len() / 4 && out.windows(2).all(|w| w[0].0 < w[1].0));
                if let Some((c, m)) = rank::fuzzy_chars(q) {
                    ix.fuzzy(&c[..m], rank::fuzzy_dist(m), &mut near);
                    assert!(near.len() <= b.len() / 4);
                }
            }
        });
        assert!(peak <= 8 * b.len() + 4096, "peak {peak} for {} bytes", b.len());
        true
    }

    #[test]
    fn never_panics() {
        let (_, a) = apps_arena();
        let (_, s) = settings_arena();
        let mut r = XorShift::new(0xFEED);
        let mut accepted = 0;
        for (valid, kind) in [(&a, Kind::Apps), (&s, Kind::Settings)] {
            let raw = valid.raw();
            assert!(exercise(raw, kind));
            // Random buffers, with and without the right header.
            for i in 0..10_000 {
                let n = (r.next_u64() % 600) as usize;
                let mut b: Vec<u8> = (0..n).map(|_| r.next_u64() as u8).collect();
                if i % 2 == 0 {
                    let head = if kind == Kind::Apps { APPS_HEADER } else { SETTINGS_HEADER };
                    b.splice(0..0, head.iter().copied());
                }
                accepted += usize::from(exercise(&b, kind));
            }
            // Every truncation (and 10,000 random ones, with the checksum fixed): never accepted by luck.
            for n in 0..raw.len() {
                assert!(!exercise(&raw[..n], kind), "{n}");
            }
            for _ in 0..10_000 {
                let mut b = raw[..(r.next_u64() as usize % raw.len()).max(8)].to_vec();
                refix(&mut b);
                exercise(&b, kind);
            }
            // Extensions.
            for extra in 1..40 {
                let mut b = raw.to_vec();
                b.extend((0..extra).map(|i| i as u8));
                assert!(!exercise(&b, kind));
            }
            // Single-byte flips: every position raw (always rejected), 10,000 random ones with the checksum fixed.
            for at in 0..raw.len() {
                let mut b = raw.to_vec();
                b[at] ^= 0x5A;
                assert!(!exercise(&b, kind), "raw flip at {at}");
            }
            for _ in 0..10_000 {
                let mut b = raw.to_vec();
                let at = r.next_u64() as usize % (b.len() - 8);
                b[at] ^= (r.next_u64() as u8) | 1;
                refix(&mut b);
                exercise(&b, kind);
            }
            // Huge counts and offsets in every 4-aligned field.
            for at in (0..raw.len() - 8).step_by(4) {
                for v in [u32::MAX, 0x7FFF_FFFF, 0x8000_0000, 0x1_0000, 0xFFFF, 0xFFFF_FFFC] {
                    let mut b = raw.to_vec();
                    set32(&mut b, at, v);
                    refix(&mut b);
                    accepted += usize::from(exercise(&b, kind));
                }
            }
        }
        println!(
            "{accepted} of the random and huge-count mutations still passed check (ranks, string bytes: harmless)"
        );
    }

    /// The view does not rely on `check`: counts and offsets that lie make no answer longer than the buffer and no
    /// allocation larger than it, even when the bytes never passed it.
    #[test]
    fn index_view_bounds_lying_buffers() {
        let (_, s) = settings_arena();
        let l = s.layout;
        let (mut worst, mut tried) = (0usize, 0usize);
        for sec in [CUR, XML, FUZ] {
            for field in [8usize, 12] {
                for v in [u32::MAX, 0x7FFF_FFFF, 0x1_0000, 1_000_000] {
                    // Every record of the dictionary claims the same huge run.
                    let mut b = s.raw().to_vec();
                    let (at, n) = (start(&l, sec), l.secs[sec].1 as usize / DICT_REC);
                    for i in 0..n {
                        set32(&mut b, at + i * DICT_REC + field, if field == 8 { v / 2 } else { v });
                        set32(&mut b, at + i * DICT_REC + 12, if field == 12 { v } else { 3 });
                    }
                    let v = CatalogView::new(&b, &l);
                    let ((), peak) = peak_bytes_during(|| {
                        let (mut out, mut near) = (Vec::new(), Vec::new());
                        for q in WORDS_LIST.iter().chain(&["wi", "re", "a"]) {
                            v.index().hidden(q, q.chars().count(), &mut out);
                            worst = worst.max(out.len());
                            assert!(out.len() <= b.len() / 4, "{} hits in {} bytes", out.len(), b.len());
                            if let Some((c, m)) = rank::fuzzy_chars(q) {
                                v.index().fuzzy(&c[..m], rank::fuzzy_dist(m), &mut near);
                                assert!(near.len() <= b.len() / 4);
                            }
                            tried += 1;
                        }
                    });
                    assert!(peak <= 8 * b.len(), "peak {peak} for {} bytes", b.len());
                }
            }
        }
        assert!(tried > 100);
        let _ = worst;
    }

    #[test]
    fn random_garbage_sections_never_panic_the_view() {
        let (_, a) = apps_arena();
        let mut r = XorShift::new(7);
        let l = a.layout;
        for _ in 0..300 {
            let mut b = a.raw().to_vec();
            for _ in 0..40 {
                let at = r.next_u64() as usize % b.len();
                b[at] = r.next_u64() as u8;
            }
            let v = CatalogView::new(&b, &l);
            for i in 0..v.len() + 1 {
                let c = v.cand(i);
                let _ = (c.folded, c.words.iter().count(), v.name(i), v.id(i), v.names(i));
            }
            let mut out = Vec::new();
            for q in ["wifi", "shut", "xx"] {
                v.index().hidden(q, q.len(), &mut out);
                v.index().fuzzy(&['w', 'i', 'f'], 1, &mut out.iter().map(|h| h.0).collect());
            }
            // A layout that belongs to other bytes.
            let short = CatalogView::new(&b[..b.len() / 2], &l);
            let _ = (short.cand(3).folded, short.index().entries());
        }
    }

    #[test]
    #[ignore = "prints the size of the bench's settings arena"]
    fn settings_arena_size_printed() {
        let (_, s) = settings_arena();
        let l = s.layout;
        for (id, name) in
            ["STR", "ENT", "WORDS", "CUR", "CURP", "XML", "XMLP", "FUZ", "FUZP", "CHARS", "TGT"].iter().enumerate()
        {
            println!("{name:6} {:8} B", l.secs[id].1);
        }
        let mut times: Vec<std::time::Duration> = (0..51)
            .map(|_| {
                let t = std::time::Instant::now();
                std::hint::black_box(check(s.raw(), Kind::Settings, None, Caps::BENCH)).unwrap();
                t.elapsed()
            })
            .collect();
        times.sort();
        println!("total {} B, check median {:?}", s.raw().len(), times[25]);
    }
}
