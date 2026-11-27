//! Sort keys and the worker sort (SortJob, SortDone): ST2 (the comparator) and ST5 (the worker).
//!
//! The order of rows: folders first (a directory attribute, also on a reparse point that is a directory), then the
//! key, then the natural name (which ends in an ordinal compare), then the id. Descending reverses all of it inside
//! the folder group and inside the file group; folders stay first.

use core::cmp::Ordering;

use super::arena::ArenaSnap;
use super::collate::{wtf8_to_utf16, SortKeyCollator};
use super::natural::{name_key, natural_cmp, natural_cmp_keyed, self_check, NameKey, KEYED_SORT_ROWS};
use super::types::{Entry, EntryId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Modified,
    Type,
    Size,
    Created,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GroupBy {
    None,
}

/// FILE_ATTRIBUTE_DIRECTORY.
pub(crate) const ATTR_DIR: u32 = 0x10;

/// Names by (offset, length): the store's arena, or a snapshot of it on a worker.
pub(crate) trait Names {
    fn name(&self, off: u32, len: u16) -> &[u8];
}

/// One row of a sort: 24 bytes, so a sort reads no store state beyond the names.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rec {
    pub id: u32,
    pub off: u32,
    pub key: u64,
    pub prefix: u32,
    pub len: u16,
    flags: u8,
}

const _: () = assert!(std::mem::size_of::<Rec>() == 24);

impl Rec {
    pub fn is_dir(&self) -> bool {
        self.flags & 1 != 0
    }

    /// What decides where the row sits apart from its name: a change of it means a move.
    pub fn place(&self) -> (bool, u64) {
        (self.is_dir(), self.key)
    }

    fn plain(&self) -> bool {
        self.flags & 2 != 0
    }

    fn nk(&self) -> NameKey {
        NameKey { prefix: self.prefix, plain: self.plain() }
    }
}

/// An i64 as a u64 with the same order.
fn ordered(v: i64) -> u64 {
    (v as u64) ^ (1 << 63)
}

/// The row of an entry. `ranks` maps an extension id to its place in natural order (used by the Type key only).
pub(crate) fn rec_of(id: EntryId, e: &Entry, name: &[u8], key: SortKey, ranks: &[u16]) -> Rec {
    let dir = e.attrs & ATTR_DIR != 0;
    let k = match key {
        SortKey::Name => 0,
        SortKey::Modified => ordered(e.mtime),
        SortKey::Created => ordered(e.ctime),
        SortKey::Size if dir => 0,
        SortKey::Size => e.size,
        SortKey::Type if dir => 0,
        SortKey::Type => u64::from(ranks.get(usize::from(e.ext)).copied().unwrap_or(0)),
    };
    let nk = name_key(name);
    Rec {
        id: id.0,
        off: e.name_off,
        key: k,
        prefix: nk.prefix,
        len: e.name_len,
        flags: u8::from(dir) | (u8::from(nk.plain) << 1),
    }
}

pub(crate) fn rec_cmp<N: Names>(a: &Rec, b: &Rec, names: &N, desc: bool) -> Ordering {
    match (a.is_dir(), b.is_dir()) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let o = a
        .key
        .cmp(&b.key)
        .then_with(|| natural_cmp_keyed(names.name(a.off, a.len), a.nk(), names.name(b.off, b.len), b.nk()))
        .then_with(|| a.id.cmp(&b.id));
    if desc {
        o.reverse()
    } else {
        o
    }
}

/// Sorts rows into the order described at the top. The first sort of more than 256 rows runs natural's self-check.
pub(crate) fn sort_recs<N: Names>(recs: &mut Vec<Rec>, names: &N, key: SortKey, desc: bool) {
    if recs.len() > 256 {
        self_check();
    }
    if key != SortKey::Name || recs.len() <= KEYED_SORT_ROWS {
        recs.sort_unstable_by(|a, b| rec_cmp(a, b, names, desc));
        return;
    }
    let (dirs, files): (Vec<Rec>, Vec<Rec>) = std::mem::take(recs).into_iter().partition(Rec::is_dir);
    let (mut dirs, mut files) = (sort_names_big(dirs, names), sort_names_big(files, names));
    if desc {
        dirs.reverse();
        files.reverse();
    }
    recs.reserve_exact(dirs.len() + files.len());
    recs.extend(dirs);
    recs.extend(files);
}

/// Ascending natural order of many rows, as natural::sort_rows does it (that function cannot carry ids): the plain
/// names by the fast path, the others by system sort keys computed once each, the two runs merged.
fn sort_names_big<N: Names>(rows: Vec<Rec>, names: &N) -> Vec<Rec> {
    let nm = |r: &Rec| names.name(r.off, r.len);
    let (mut plain, other): (Vec<Rec>, Vec<Rec>) = rows.into_iter().partition(Rec::plain);
    plain.sort_unstable_by(|a, b| natural_cmp_keyed(nm(a), a.nk(), nm(b), b.nk()).then_with(|| a.id.cmp(&b.id)));
    let mut wide = Vec::new();
    let mut keyed: Vec<(Vec<u8>, Rec)> = other
        .into_iter()
        .map(|r| {
            wide.clear();
            wtf8_to_utf16(nm(&r), &mut wide);
            (SortKeyCollator.key(&wide), r)
        })
        .collect();
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| nm(&a.1).cmp(nm(&b.1))).then_with(|| a.1.id.cmp(&b.1.id)));
    let mut out = Vec::with_capacity(plain.len() + keyed.len());
    let mut other = keyed.into_iter().map(|k| k.1).peekable();
    let mut plain = plain.into_iter().peekable();
    while let (Some(p), Some(o)) = (plain.peek(), other.peek()) {
        let next = if natural_cmp(nm(o), nm(p)) == Ordering::Less { other.next() } else { plain.next() };
        out.extend(next);
    }
    out.extend(plain);
    out.extend(other);
    out
}

/// A snapshot of the rows to sort: 24-byte rows and the arena's chunks, so the worker reads no store state. Send; for
/// more than 20,000 rows run it on a worker and hand the result to EntryStore::adopt.
pub struct SortJob {
    pub(crate) recs: Vec<Rec>,
    pub(crate) names: ArenaSnap,
    pub(crate) key: SortKey,
    pub(crate) dir: SortDir,
    pub(crate) sort_gen: u64,
    pub(crate) snap_total: u32,
}

impl SortJob {
    pub fn run(mut self) -> SortDone {
        sort_recs(&mut self.recs, &self.names, self.key, self.dir == SortDir::Desc);
        SortDone {
            order: self.recs.iter().map(|r| EntryId(r.id)).collect(),
            key: self.key,
            dir: self.dir,
            sort_gen: self.sort_gen,
            snap_total: self.snap_total,
        }
    }
}

/// The result of a SortJob: the order of the rows it saw.
pub struct SortDone {
    pub(crate) order: Vec<EntryId>,
    pub(crate) key: SortKey,
    pub(crate) dir: SortDir,
    pub(crate) sort_gen: u64,
    pub(crate) snap_total: u32,
}

#[cfg(test)]
mod tests {
    use super::super::diff::Diff;
    use super::super::store::tests::{batch, names, order_names, store};
    use super::super::store::Visibility;
    use super::*;
    use crate::listing::{EntryStore, Gen, RawEntry};

    /// One row of the fixture, as the test itself sorts it (never through the store).
    #[derive(Clone)]
    struct Row {
        name: String,
        dir: bool,
        size: u64,
        mtime: i64,
        ctime: i64,
    }

    /// 40 names of letters, spaces and dots with digit runs of equal length only, so that Explorer's natural order and
    /// the plain byte order of the folded names agree; sizes and times repeat so that the tie-breaks are used.
    fn fixture() -> Vec<Row> {
        let stems = ["a b", "a.b", "Ab", "ab", "B x", "b y", "c", "C"];
        let exts = ["txt", "RS", "md", "", "bin"];
        (0..40usize)
            .map(|i| {
                let ext = exts[i % 5];
                let dot = if ext.is_empty() { "" } else { "." };
                Row {
                    name: format!("{} {:02}{dot}{ext}", stems[i % 8], i),
                    dir: i % 6 == 0,
                    size: ((i * 37) % 11) as u64 * 100,
                    mtime: ((i * 13) % 9) as i64 - 4,
                    ctime: ((i * 7) % 5) as i64,
                }
            })
            .collect()
    }

    fn load(rows: &[Row]) -> EntryStore {
        let mut s = store();
        let mut b = batch(
            1,
            &rows.iter().map(|r| (r.name.clone(), if r.dir { ATTR_DIR } else { 0x20 })).collect::<Vec<_>>(),
            true,
        );
        for (e, r) in b.entries.iter_mut().zip(rows) {
            *e = RawEntry { size: r.size, mtime: r.mtime, ctime: r.ctime, ..*e };
        }
        s.append(b);
        s.merge_pending();
        s
    }

    fn ext_of(name: &str) -> String {
        name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default()
    }

    /// The expected order, written the plain way: folders first, then the key, then the folded name.
    fn expected(rows: &[Row], key: SortKey, desc: bool) -> Vec<String> {
        let mut v = rows.to_vec();
        let folded = |r: &Row| r.name.to_lowercase();
        let k = |r: &Row| -> (i64, String) {
            match key {
                SortKey::Name => (0, String::new()),
                SortKey::Modified => (r.mtime, String::new()),
                SortKey::Created => (r.ctime, String::new()),
                SortKey::Size => (if r.dir { 0 } else { r.size as i64 }, String::new()),
                SortKey::Type => (0, if r.dir { String::new() } else { ext_of(&r.name) }),
            }
        };
        v.sort_by(|a, b| {
            let o = k(a).cmp(&k(b)).then_with(|| folded(a).cmp(&folded(b)));
            if desc && a.dir == b.dir {
                o.reverse()
            } else {
                b.dir.cmp(&a.dir).then(o)
            }
        });
        v.into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn sort_each_key_both_dirs() {
        let rows = fixture();
        let mut s = load(&rows);
        for key in [SortKey::Name, SortKey::Modified, SortKey::Type, SortKey::Size, SortKey::Created] {
            for desc in [false, true] {
                let dir = if desc { SortDir::Desc } else { SortDir::Asc };
                s.sort(key, dir, GroupBy::None);
                assert_eq!(order_names(&s), expected(&rows, key, desc), "{key:?} {dir:?}");
            }
        }
    }

    #[test]
    fn ties_are_deterministic() {
        let rows = fixture();
        let mut rev = rows.clone();
        rev.reverse();
        let (mut a, mut b) = (load(&rows), load(&rev));
        for key in [SortKey::Modified, SortKey::Size, SortKey::Type, SortKey::Created] {
            a.sort(key, SortDir::Asc, GroupBy::None);
            b.sort(key, SortDir::Asc, GroupBy::None);
            assert_eq!(order_names(&a), order_names(&b), "{key:?}: the same names in the same order");
        }
    }

    #[test]
    fn visibility_hides_and_shows() {
        let rows: Vec<(String, u32)> = vec![
            ("plain".into(), 0x20),
            ("sys".into(), 0x20 | 0x4),
            ("hid".into(), 0x20 | 0x2),
            ("hidsys".into(), 0x20 | 0x2 | 0x4),
            ("hiddir".into(), ATTR_DIR | 0x2),
        ];
        let mut s = store();
        s.append(batch(1, &rows, true));
        s.merge_pending();
        let shown = |s: &EntryStore| {
            let mut v = order_names(s);
            v.sort();
            v
        };
        assert_eq!(shown(&s), ["plain", "sys"], "Explorer's default");
        s.set_visibility(Visibility { hidden: true, protected_system: false });
        assert_eq!(shown(&s), ["hid", "hiddir", "plain", "sys"]);
        s.set_visibility(Visibility { hidden: true, protected_system: true });
        assert_eq!(shown(&s), ["hid", "hiddir", "hidsys", "plain", "sys"]);
        s.set_visibility(Visibility::default());
        assert_eq!(shown(&s), ["plain", "sys"]);
        assert_eq!(s.len(), 5, "hidden entries are still live");
        let hid = s.find(b"HID").unwrap();
        assert_eq!(s.row_of(hid), None);
    }

    #[test]
    fn job_equals_sync_sort() {
        let rows = fixture();
        let mut a = load(&rows);
        let mut b = load(&rows);
        for (key, dir) in
            [(SortKey::Type, SortDir::Desc), (SortKey::Size, SortDir::Asc), (SortKey::Name, SortDir::Desc)]
        {
            let job = a.sort_job(key, dir, GroupBy::None);
            let done = std::thread::spawn(move || job.run()).join().unwrap();
            a.adopt(done);
            b.sort(key, dir, GroupBy::None);
            assert_eq!(order_names(&a), order_names(&b), "{key:?} {dir:?}");
            assert_eq!(a.order(), b.order());
        }
    }

    #[test]
    fn adopt_after_changes() {
        let all = names(1000);
        let mut a = store();
        a.append(batch(1, &all, true));
        a.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
        let job = a.sort_job(SortKey::Modified, SortDir::Desc, GroupBy::None);
        // 50 changes while the job is out: removals, new names, and mtime changes of names that stay.
        let mut d = Diff::new(Gen(1));
        for (i, (n, attrs)) in all.iter().enumerate().take(50) {
            match i % 3 {
                0 => d.remove(n.as_bytes()),
                1 => d.upsert(
                    n.as_bytes(),
                    RawEntry {
                        name_off: 0,
                        name_len: 0,
                        attrs: *attrs,
                        reparse_tag: 0,
                        size: 7,
                        mtime: i as i64,
                        ctime: 0,
                    },
                ),
                _ => d.upsert(
                    format!("late {i}").as_bytes(),
                    RawEntry {
                        name_off: 0,
                        name_len: 0,
                        attrs: 0x20,
                        reparse_tag: 0,
                        size: 1,
                        mtime: 1000 - i as i64,
                        ctime: 0,
                    },
                ),
            }
        }
        a.apply(&d, None);
        let done = std::thread::spawn(move || job.run()).join().unwrap();
        a.adopt(done);
        let mut b = store();
        b.append(batch(1, &all, true));
        b.apply(&d, None);
        b.sort(SortKey::Modified, SortDir::Desc, GroupBy::None);
        assert_eq!(order_names(&a), order_names(&b));
        assert_eq!(a.len(), b.len());
        for (row, id) in a.order().0.iter().enumerate() {
            assert_eq!(a.row_of(*id), Some(row as u32));
        }
        assert_eq!(a.order().0.capacity(), a.order().0.len());
    }

    #[test]
    fn stale_job_is_ignored() {
        let mut s = load(&fixture());
        let job = s.sort_job(SortKey::Size, SortDir::Asc, GroupBy::None);
        s.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
        let before = s.order().clone();
        s.adopt(job.run());
        assert_eq!(*s.order(), before);
    }

    #[test]
    fn job_is_send() {
        fn send<T: Send>() {}
        send::<SortJob>();
        send::<SortDone>();
    }

    #[test]
    fn row_of_is_inverse() {
        let rows = fixture();
        let mut s = load(&rows);
        s.sort(SortKey::Size, SortDir::Desc, GroupBy::None);
        for (row, id) in s.order().0.iter().enumerate() {
            assert_eq!(s.row_of(*id), Some(row as u32));
        }
        assert_eq!(s.order().0.len(), 40);
        assert_eq!(s.row_of(crate::listing::EntryId(9_999)), None);
    }
}
