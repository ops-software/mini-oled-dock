//! A diff of one folder (complete): names in a WTF-8 buffer, upserts and removes by name, or a full rescan. The
//! watcher builds them off the UI thread; the store applies them (F-STORE).

use super::types::RawEntry;
use super::Gen;

pub struct Diff {
    pub gen: Gen,
    pub names: Vec<u8>,
    pub upserts: Vec<RawEntry>,
    pub removes: Vec<(u32, u16)>,
    pub rescan: bool,
}

impl Diff {
    pub fn new(gen: Gen) -> Self {
        Diff { gen, names: Vec::new(), upserts: Vec::new(), removes: Vec::new(), rescan: false }
    }

    /// Copies `name` into names and records `e` with its name_off and name_len set to that copy.
    pub fn upsert(&mut self, name: &[u8], e: RawEntry) {
        let (name_off, name_len) = self.push_name(name);
        self.upserts.push(RawEntry { name_off, name_len, ..e });
    }

    /// Records the removal of `name` (its copy in names, as (offset, length)).
    pub fn remove(&mut self, name: &[u8]) {
        let at = self.push_name(name);
        self.removes.push(at);
    }

    /// Nothing to upsert, nothing to remove and no rescan asked.
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.removes.is_empty() && !self.rescan
    }

    fn push_name(&mut self, name: &[u8]) -> (u32, u16) {
        // A name is at most 255 UTF-16 units, 765 WTF-8 bytes.
        debug_assert!(name.len() <= usize::from(u16::MAX), "a file name is far shorter");
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        (off, name.len().min(usize::from(u16::MAX)) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(size: u64) -> RawEntry {
        RawEntry { name_off: 99, name_len: 99, attrs: 0x20, reparse_tag: 0, size, mtime: 1, ctime: 2 }
    }

    fn name(d: &Diff, off: u32, len: u16) -> &[u8] {
        &d.names[off as usize..off as usize + usize::from(len)]
    }

    #[test]
    fn diff_upsert_and_remove_keep_names() {
        let mut d = Diff::new(Gen(7));
        assert!(d.is_empty());
        d.upsert(b"a.txt", raw(1));
        d.remove(b"old name.bin");
        d.upsert("é.txt".as_bytes(), raw(2));
        assert!(!d.is_empty());
        assert_eq!(d.gen, Gen(7));
        assert_eq!(d.upserts.len(), 2);
        let (a, e) = (d.upserts[0], d.upserts[1]);
        assert_eq!(name(&d, a.name_off, a.name_len), b"a.txt");
        assert_eq!(name(&d, e.name_off, e.name_len), "é.txt".as_bytes());
        assert_eq!((a.size, e.size, a.attrs, a.mtime, a.ctime), (1, 2, 0x20, 1, 2), "the other fields are kept");
        let (off, len) = d.removes[0];
        assert_eq!(name(&d, off, len), b"old name.bin");
        let mut r = Diff::new(Gen(1));
        r.rescan = true;
        assert!(!r.is_empty(), "a rescan is not an empty diff");
    }
}
