//! The store's WTF-8 name arena, the extension table and the case folding of names (ST1). Names are copied once into
//! 64 KiB chunks and never move; an entry holds a global offset (chunk index x 65,536 + offset in the chunk).

use std::collections::HashMap;
use std::sync::Arc;

use super::sort::Names;

/// Entries per chunk of the store: a chunk is allocated once at this capacity and never pushed beyond it.
pub const ENTRY_CHUNK: usize = 4_096;
/// Bytes per chunk of the name arena.
pub const NAME_CHUNK: usize = 65_536;

/// Names in 64 KiB chunks: full chunks are shared (Arc, so a sort snapshot clones pointers, not bytes), the open one
/// is a Vec allocated once at its final size.
pub struct NameArena {
    full: Vec<Arc<[u8]>>,
    open: Vec<u8>,
    holes: usize,
}

impl NameArena {
    pub fn new() -> Self {
        NameArena { full: Vec::new(), open: Vec::new(), holes: 0 }
    }

    /// Copies a name (at most 65,535 bytes) and returns its global offset and length. A name never crosses a chunk.
    pub fn push(&mut self, name: &[u8]) -> (u32, u16) {
        let name = name.get(..usize::from(u16::MAX)).unwrap_or(name);
        if self.open.capacity() == 0 {
            self.open = Vec::with_capacity(NAME_CHUNK);
        }
        if self.open.len() + name.len() > NAME_CHUNK {
            let done = std::mem::replace(&mut self.open, Vec::with_capacity(NAME_CHUNK));
            self.full.push(Arc::from(done));
        }
        let off = ((self.full.len() as u32) << 16) | self.open.len() as u32;
        self.open.extend_from_slice(name);
        (off, name.len() as u16)
    }

    pub fn get(&self, off: u32, len: u16) -> Option<&[u8]> {
        let chunk: &[u8] = match self.full.get((off >> 16) as usize) {
            Some(c) => c,
            None if (off >> 16) as usize == self.full.len() => &self.open,
            None => return None,
        };
        let start = (off & 0xFFFF) as usize;
        chunk.get(start..start + usize::from(len))
    }

    /// A name that is no longer used (removed, or replaced by a new copy).
    pub fn add_hole(&mut self, len: u16) {
        self.holes += usize::from(len);
    }

    pub fn holes(&self) -> usize {
        self.holes
    }

    /// Bytes of names written so far (holes included).
    pub fn used(&self) -> usize {
        self.full.iter().map(|c| c.len()).sum::<usize>() + self.open.len()
    }

    /// Chunk table, full chunks at their size and the open one at its capacity.
    pub fn heap_bytes(&self) -> usize {
        self.full.iter().map(|c| c.len()).sum::<usize>()
            + self.open.capacity()
            + self.full.capacity() * std::mem::size_of::<Arc<[u8]>>()
    }

    /// A copy of the chunk pointers and of the open chunk's bytes (at most 64 KiB), readable on another thread.
    pub fn snapshot(&self) -> ArenaSnap {
        ArenaSnap { full: self.full.clone(), open: Arc::from(self.open.as_slice()) }
    }
}

impl Default for NameArena {
    fn default() -> Self {
        Self::new()
    }
}

impl Names for NameArena {
    fn name(&self, off: u32, len: u16) -> &[u8] {
        self.get(off, len).unwrap_or(&[])
    }
}

/// The arena as a sort job sees it: immutable, Send.
pub struct ArenaSnap {
    full: Vec<Arc<[u8]>>,
    open: Arc<[u8]>,
}

impl Names for ArenaSnap {
    fn name(&self, off: u32, len: u16) -> &[u8] {
        let chunk: &[u8] = match self.full.get((off >> 16) as usize) {
            Some(c) => c,
            None => &self.open,
        };
        let start = (off & 0xFFFF) as usize;
        chunk.get(start..start + usize::from(len)).unwrap_or(&[])
    }
}

/// Interned lower-case extensions: id 0 is "none", ids 1..=65,535 are texts.
#[derive(Default)]
pub struct ExtTable {
    map: HashMap<Box<[u8]>, u16>,
    texts: Vec<Box<[u8]>>,
}

impl ExtTable {
    /// The id of the extension of `name` (the text after its last dot, lower-cased; none when there is no dot or
    /// nothing after it). Allocates only for a new extension, a non-ASCII one or one longer than 64 bytes.
    pub fn intern_name(&mut self, name: &[u8]) -> u16 {
        let Some(dot) = name.iter().rposition(|b| *b == b'.') else { return 0 };
        let tail = name.get(dot + 1..).unwrap_or(&[]);
        if tail.is_empty() {
            return 0;
        }
        if tail.len() <= 64 && tail.is_ascii() {
            let mut buf = [0u8; 64];
            let lower = buf.get_mut(..tail.len()).unwrap_or(&mut []);
            for (d, s) in lower.iter_mut().zip(tail) {
                *d = s.to_ascii_lowercase();
            }
            if let Some(id) = self.map.get(&*lower) {
                return *id;
            }
            return self.add(lower);
        }
        let lower = lower_wtf8(tail);
        match self.map.get(lower.as_slice()) {
            Some(id) => *id,
            None => self.add(&lower),
        }
    }

    fn add(&mut self, lower: &[u8]) -> u16 {
        if self.texts.len() >= usize::from(u16::MAX) {
            return 0;
        }
        let id = self.texts.len() as u16 + 1;
        let text: Box<[u8]> = Box::from(lower);
        self.texts.push(text.clone());
        self.map.insert(text, id);
        id
    }

    /// The text of an id; empty for 0.
    pub fn text(&self, id: u16) -> &[u8] {
        match id.checked_sub(1) {
            Some(i) => self.texts.get(usize::from(i)).map(|t| &**t).unwrap_or(&[]),
            None => &[],
        }
    }

    /// Ids 0..=len() as a rank table: rank[id] is the position of its text in natural order.
    pub fn ranks(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = (0..=self.texts.len() as u16).collect();
        ids.sort_unstable_by(|a, b| super::natural::natural_cmp(self.text(*a), self.text(*b)));
        let mut rank = vec![0u16; ids.len()];
        for (r, id) in ids.iter().enumerate() {
            if let Some(slot) = rank.get_mut(usize::from(*id)) {
                *slot = r as u16;
            }
        }
        rank
    }

    /// Capacities of the table, its keys and its texts (the hash table counted at its slot size).
    pub fn heap_bytes(&self) -> usize {
        let slot = std::mem::size_of::<(Box<[u8]>, u16)>() + 1;
        let keys: usize = self.texts.iter().map(|t| t.len()).sum();
        self.map.capacity() * slot + self.texts.capacity() * std::mem::size_of::<Box<[u8]>>() + keys * 2
    }
}

/// The next code point of lenient WTF-8 (a lone surrogate stays a surrogate, a bad byte is U+FFFD).
fn next_cp(b: &[u8], i: &mut usize) -> u32 {
    let at = |k: usize| b.get(*i + k).map_or(0, |c| u32::from(*c & 0x3F));
    let c = b.get(*i).copied().unwrap_or(0);
    let (cp, n) = match c {
        0x00..=0x7F => (u32::from(c), 1),
        0xC0..=0xDF if *i + 1 < b.len() => ((u32::from(c & 0x1F) << 6) | at(1), 2),
        0xE0..=0xEF if *i + 2 < b.len() => ((u32::from(c & 0x0F) << 12) | (at(1) << 6) | at(2), 3),
        0xF0..=0xF7 if *i + 3 < b.len() => ((u32::from(c & 0x07) << 18) | (at(1) << 12) | (at(2) << 6) | at(3), 4),
        _ => (0xFFFD, 1),
    };
    *i += n;
    cp
}

/// Calls `emit` with every code point of `name` folded to lower case (ASCII fold, then char::to_lowercase).
fn fold_each(name: &[u8], mut emit: impl FnMut(u32)) {
    let mut i = 0;
    while i < name.len() {
        let cp = next_cp(name, &mut i);
        if cp < 0x80 {
            emit(u32::from((cp as u8).to_ascii_lowercase()));
        } else if let Some(c) = char::from_u32(cp) {
            for l in c.to_lowercase() {
                emit(u32::from(l));
            }
        } else {
            emit(cp);
        }
    }
}

/// FNV-1a over the folded code points: names equal under NTFS-style case folding hash alike.
pub fn fold_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    fold_each(name, |cp| h = (h ^ cp).wrapping_mul(0x0100_0193));
    h
}

/// Equality under the same folding (ß stays distinct from ss, as on NTFS).
pub fn fold_eq(a: &[u8], b: &[u8]) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    let (mut x, mut y) = (Vec::new(), Vec::new());
    fold_each(a, |c| x.push(c));
    fold_each(b, |c| y.push(c));
    x == y
}

/// Appends the WTF-8 form of a code point (surrogates included, as three bytes).
fn push_wtf8(cp: u32, out: &mut Vec<u8>) {
    match cp {
        0..=0x7F => out.push(cp as u8),
        0x80..=0x7FF => out.extend_from_slice(&[0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]),
        0x800..=0xFFFF => {
            out.extend_from_slice(&[0xE0 | (cp >> 12) as u8, 0x80 | ((cp >> 6) & 0x3F) as u8, 0x80 | (cp & 0x3F) as u8])
        }
        _ => out.extend_from_slice(&[
            0xF0 | (cp >> 18) as u8,
            0x80 | ((cp >> 12) & 0x3F) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
        ]),
    }
}

/// The folded (lower-case) WTF-8 form of `name`.
pub fn lower_wtf8(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    fold_each(name, |cp| push_wtf8(cp, &mut out));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_never_cross_a_chunk() {
        let mut a = NameArena::new();
        let name = [b'x'; 700];
        let mut at = Vec::new();
        for _ in 0..200 {
            at.push(a.push(&name));
        }
        for (off, len) in at {
            assert!((off & 0xFFFF) as usize + usize::from(len) <= NAME_CHUNK);
            assert_eq!(a.get(off, len), Some(&name[..]));
        }
        assert!(a.full.len() >= 2);
    }

    #[test]
    fn fold_keeps_sharp_s_apart() {
        assert!(fold_eq(b"Readme.TXT", b"README.txt"));
        assert!(!fold_eq("ß".as_bytes(), b"ss"));
        assert!(fold_eq("ÄÖ".as_bytes(), "äö".as_bytes()));
        assert_eq!(fold_hash("É.txt".as_bytes()), fold_hash("é.TXT".as_bytes()));
        assert_eq!(lower_wtf8("AÉ".as_bytes()), "aé".as_bytes());
    }

    #[test]
    fn ext_intern_rules() {
        let mut t = ExtTable::default();
        let txt = t.intern_name(b"a.TXT");
        assert_ne!(txt, 0);
        assert_eq!(t.intern_name(b"b.txt"), txt);
        assert_eq!(t.text(txt), b"txt");
        assert_eq!(t.intern_name(b"noext"), 0);
        assert_eq!(t.intern_name(b"trailing."), 0);
        assert_eq!(t.text(0), b"");
    }
}
