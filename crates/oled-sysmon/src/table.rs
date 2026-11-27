//! The process table: rows, interned image names, parent links and the added/gone diff.
use std::collections::HashMap;

/// created = CreateTime FILETIME (100 ns since 1601); the PID-reuse guard. Defined in oled-trust (the admin frames
/// carry it); re-exported here so `oled_sysmon::ProcKey` keeps its path and derives.
pub use oled_trust::frame::admin::ProcKey;

/// A NameId indexes ONE table's arena: tables intern in their own order and finish() may compact, so a NameId is
/// never compared across tables or ticks and never used as a key (the model's stable key is group::ImageId).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameId(pub u32);

impl NameId {
    /// A process with no image name (pid 0).
    pub const NONE: NameId = NameId(u32::MAX);
}

impl Default for NameId {
    fn default() -> NameId {
        NameId::NONE
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcRow {
    pub key: ProcKey,
    /// InheritedFromUniqueProcessId as reported.
    pub parent_pid: u32,
    /// Set by ProcTable::finish: same snapshot, pid == parent_pid and created < this row's created.
    pub parent: Option<ProcKey>,
    pub session: u32,
    pub image: NameId,
    /// UserTime + KernelTime.
    pub cpu_100ns: u64,
    pub ws: u64,
    pub ws_peak: u64,
    pub ws_private: u64,
    pub private_bytes: u64,
    /// Read/Write/OtherTransferCount, bytes.
    pub io_read: u64,
    pub io_write: u64,
    pub io_other: u64,
    pub threads: u32,
    pub handles: u32,
    pub base_prio: i32,
}

#[derive(Debug, Default)]
pub struct NameArena {
    map: HashMap<Box<[u16]>, NameId>,
    items: Vec<Box<[u16]>>,
}

impl NameArena {
    pub fn new() -> NameArena {
        NameArena::default()
    }

    /// Empty slice -> NameId::NONE; equal slices share an id; no allocation on a hit.
    pub fn intern(&mut self, utf16: &[u16]) -> NameId {
        if utf16.is_empty() {
            return NameId::NONE;
        }
        if let Some(id) = self.map.get(utf16) {
            return *id;
        }
        let id = NameId(self.items.len() as u32);
        let boxed: Box<[u16]> = utf16.into();
        self.items.push(boxed.clone());
        self.map.insert(boxed, id);
        id
    }

    /// Empty for NONE or an unknown id.
    pub fn get(&self, id: NameId) -> &[u16] {
        self.items.get(id.0 as usize).map(|b| &b[..]).unwrap_or(&[])
    }

    pub fn to_string_lossy(&self, id: NameId) -> String {
        String::from_utf16_lossy(self.get(id))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Estimate from capacities (no allocator overhead): sum of 4 x units over the items (two copies: map key and
    /// item), + items.capacity() x 16 + map.capacity() x (size_of::<(Box<[u16]>, NameId)>() + 1).
    pub fn heap_bytes(&self) -> usize {
        let units: usize = self.items.iter().map(|b| b.len()).sum();
        4 * units
            + self.items.capacity() * std::mem::size_of::<Box<[u16]>>()
            + self.map.capacity() * (std::mem::size_of::<(Box<[u16]>, NameId)>() + 1)
    }
}

/// ProcTable::finish compacts the arena above this many names.
pub const NAME_COMPACT_AT: usize = 4096;

#[derive(Debug, Default)]
pub struct ProcTable {
    pub rows: Vec<ProcRow>,
    pub names: NameArena,
}

impl ProcTable {
    pub fn new() -> ProcTable {
        ProcTable::default()
    }

    /// Rows only; keeps capacity and names.
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    pub fn push(&mut self, row: ProcRow) {
        self.rows.push(row);
    }

    /// Sort rows by key, link parents, compact names if needed.
    pub fn finish(&mut self) {
        self.rows.sort_unstable_by_key(|r| r.key);
        for i in 0..self.rows.len() {
            let row = self.rows[i];
            let parent = self
                .find_pid(row.parent_pid)
                .filter(|p| p.key.created < row.key.created && p.key.pid != row.key.pid)
                .map(|p| p.key);
            self.rows[i].parent = parent;
        }
        if self.names.len() > NAME_COMPACT_AT {
            let old = std::mem::take(&mut self.names);
            let mut fresh = NameArena::new();
            for row in &mut self.rows {
                row.image = fresh.intern(old.get(row.image));
            }
            self.names = fresh;
        }
    }

    /// Binary search, after finish.
    pub fn find(&self, key: ProcKey) -> Option<&ProcRow> {
        self.index_of(key).map(|i| &self.rows[i])
    }

    /// Pids are unique inside one snapshot.
    pub fn find_pid(&self, pid: u32) -> Option<&ProcRow> {
        let i = self.rows.partition_point(|r| r.key.pid < pid);
        self.rows.get(i).filter(|r| r.key.pid == pid)
    }

    pub fn index_of(&self, key: ProcKey) -> Option<usize> {
        self.rows.binary_search_by_key(&key, |r| r.key).ok()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Reuses this table's allocations; grows rows by reserve_rows.
    pub fn copy_from(&mut self, other: &ProcTable) {
        self.rows.clear();
        self.reserve_rows(other.rows.len());
        self.rows.extend_from_slice(&other.rows);
        self.names.map.clear();
        self.names.items.clear();
        // Ids are the item positions of the other arena, so interning its items in order reproduces them.
        for item in &other.names.items {
            self.names.intern(item);
        }
    }

    /// Capacity only grows, to n rounded up to a multiple of 64 (reserve_exact).
    pub fn reserve_rows(&mut self, n: usize) {
        let want = n.div_ceil(64) * 64;
        if self.rows.capacity() < want {
            self.rows.reserve_exact(want - self.rows.len());
        }
    }

    /// rows.capacity() x size_of::<ProcRow>() + names.heap_bytes().
    pub fn heap_bytes(&self) -> usize {
        self.rows.capacity() * std::mem::size_of::<ProcRow>() + self.names.heap_bytes()
    }
}

/// Keys only in now (added) and only in prev (gone), both sorted; O(n) merge of two finished tables.
pub fn diff(prev: &ProcTable, now: &ProcTable, added: &mut Vec<ProcKey>, gone: &mut Vec<ProcKey>) {
    added.clear();
    gone.clear();
    let (mut i, mut j) = (0, 0);
    while i < prev.rows.len() && j < now.rows.len() {
        let (a, b) = (prev.rows[i].key, now.rows[j].key);
        match a.cmp(&b) {
            std::cmp::Ordering::Less => {
                gone.push(a);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                added.push(b);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    gone.extend(prev.rows[i..].iter().map(|r| r.key));
    added.extend(now.rows[j..].iter().map(|r| r.key));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(pid: u32, created: u64) -> ProcKey {
        ProcKey { pid, created }
    }

    fn row(pid: u32, created: u64, parent_pid: u32) -> ProcRow {
        ProcRow { key: k(pid, created), parent_pid, ..ProcRow::default() }
    }

    fn name(i: usize, units: usize) -> Vec<u16> {
        let mut v: Vec<u16> = format!("{i:0>width$}", width = units).encode_utf16().collect();
        v.truncate(units);
        v
    }

    #[test]
    fn finish_sorts_and_links_parents() {
        let mut t = ProcTable::new();
        t.push(row(30, 300, 10));
        t.push(row(10, 100, 0));
        t.push(row(20, 200, 10));
        t.push(row(40, 50, 10));
        t.finish();
        let pids: Vec<u32> = t.rows.iter().map(|r| r.key.pid).collect();
        assert_eq!(pids, vec![10, 20, 30, 40]);
        assert_eq!(t.find_pid(30).unwrap().parent, Some(k(10, 100)));
        assert_eq!(t.find_pid(20).unwrap().parent, Some(k(10, 100)));
        assert_eq!(t.find_pid(40).unwrap().parent, None);
        assert_eq!(t.find_pid(10).unwrap().parent, None);
    }

    #[test]
    fn find_and_find_pid() {
        let mut t = ProcTable::new();
        t.push(row(5, 50, 0));
        t.push(row(3, 30, 0));
        t.push(row(9, 90, 0));
        t.finish();
        assert_eq!(t.find(k(5, 50)).unwrap().key.pid, 5);
        assert!(t.find(k(5, 51)).is_none());
        assert_eq!(t.find_pid(9).unwrap().key.created, 90);
        assert!(t.find_pid(4).is_none());
        assert_eq!(t.index_of(k(3, 30)), Some(0));
        assert_eq!(t.len(), 3);
        assert!(!t.is_empty());
    }

    #[test]
    fn diff_reports_added_and_gone() {
        let mut prev = ProcTable::new();
        for (p, c) in [(1, 10), (2, 20), (3, 30)] {
            prev.push(row(p, c, 0));
        }
        let mut now = ProcTable::new();
        for (p, c) in [(2, 20), (3, 35), (4, 40)] {
            now.push(row(p, c, 0));
        }
        prev.finish();
        now.finish();
        let (mut added, mut gone) = (Vec::new(), Vec::new());
        diff(&prev, &now, &mut added, &mut gone);
        assert_eq!(added, vec![k(3, 35), k(4, 40)]);
        assert_eq!(gone, vec![k(1, 10), k(3, 30)]);
    }

    #[test]
    fn intern_shares_ids_and_empty_is_none() {
        let mut a = NameArena::new();
        let x: Vec<u16> = "abc".encode_utf16().collect();
        let y: Vec<u16> = "abd".encode_utf16().collect();
        let i = a.intern(&x);
        assert_eq!(a.intern(&x), i);
        assert_ne!(a.intern(&y), i);
        assert_eq!(a.intern(&[]), NameId::NONE);
        assert_eq!(a.len(), 2);
        assert_eq!(a.get(i), &x[..]);
        assert!(a.get(NameId::NONE).is_empty());
        assert_eq!(a.to_string_lossy(i), "abc");
        assert!(!a.is_empty());
    }

    #[test]
    fn copy_from_keeps_capacity() {
        let mut src = ProcTable::new();
        for p in 0..10 {
            let id = src.names.intern(&name(p as usize, 8));
            src.push(ProcRow { image: id, ..row(p, p as u64 + 1, 0) });
        }
        src.finish();
        let mut dst = ProcTable::new();
        dst.reserve_rows(500);
        let cap = dst.rows.capacity();
        dst.copy_from(&src);
        assert!(dst.rows.capacity() >= cap);
        assert_eq!(dst.rows, src.rows);
        for r in &dst.rows {
            assert_eq!(dst.names.get(r.image), src.names.get(r.image));
        }
    }

    #[test]
    fn reserve_rows_rounds_to_64() {
        let mut t = ProcTable::new();
        t.reserve_rows(300);
        assert_eq!(t.rows.capacity(), 320);
        t.reserve_rows(310);
        assert_eq!(t.rows.capacity(), 320);
        t.reserve_rows(1000);
        assert_eq!(t.rows.capacity(), 1024);
        t.reserve_rows(10);
        assert_eq!(t.rows.capacity(), 1024);
    }

    fn four_tables(rows: usize, names: usize, units: usize) -> usize {
        let mut total = 0;
        for _ in 0..4 {
            let mut t = ProcTable::new();
            t.reserve_rows(rows);
            for i in 0..names {
                t.names.intern(&name(i, units));
            }
            total += t.heap_bytes();
        }
        total
    }

    #[test]
    fn tables_fit_their_budget() {
        assert!(std::mem::size_of::<ProcRow>() <= 144);
        const MIB: f64 = 1024.0 * 1024.0;
        let typical = four_tables(300, 120, 16);
        let budget = four_tables(1000, 300, 16);
        let worst = four_tables(1000, 1000, 24);
        println!(
            "typical {:.3} MiB, budget {:.3} MiB, worst {:.3} MiB",
            typical as f64 / MIB,
            budget as f64 / MIB,
            worst as f64 / MIB
        );
        assert!(typical as f64 <= 0.25 * MIB);
        assert!(budget as f64 <= 0.75 * MIB);
        assert!(worst as f64 <= 1.25 * MIB);
    }

    #[test]
    fn compact_above_4096_names_keeps_live_names() {
        let mut t = ProcTable::new();
        let mut ids = Vec::new();
        for i in 0..5000 {
            ids.push(t.names.intern(&name(i, 10)));
        }
        let kept = [0usize, 7, 999, 2048, 4095, 4096, 4500, 4998, 4999, 1234];
        for (n, &i) in kept.iter().enumerate() {
            t.push(ProcRow { image: ids[i], ..row(n as u32 + 1, 10, 0) });
        }
        t.finish();
        assert!(t.names.len() <= 11);
        for (n, &i) in kept.iter().enumerate() {
            let r = t.find_pid(n as u32 + 1).unwrap();
            assert_eq!(t.names.get(r.image), &name(i, 10)[..]);
        }
    }
}
