//! The headless EntryStore (ST1-ST6): entries in fixed chunks, names in a WTF-8 arena, the order and its inverse and a
//! name index rebuilt at their exact length, so that heap_bytes counts what is held and no Vec doubles.
//!
//! Entries are never moved: an id is chunk x 4,096 + slot and stays valid until compact(). Removal sets a tombstone.
//! Entries appended since the last merge (the ids from `merged_total` up) are the pending run: they are in no order and
//! no index yet, merge_pending() sorts them and merges them in.

use std::sync::atomic::{AtomicBool, Ordering as Atomic};

use super::arena::{fold_eq, fold_hash, ExtTable, NameArena, ENTRY_CHUNK};
use super::diff::Diff;
use super::filter::{FilterQuery, Needle};
use super::sort::{rec_cmp, rec_of, sort_recs, GroupBy, Rec, SortDir, SortDone, SortJob, SortKey, ATTR_DIR};
use super::types::{Batch, Entry, EntryId, EnumError, RawEntry};
use super::{Gen, WidePath};

const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
/// An id that is in no order (the inverse's empty value).
const NO_ROW: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub hidden: bool,
    pub protected_system: bool,
} // Default: false, false (Explorer's)

#[allow(clippy::derivable_impls)] // the derive list above is frozen
impl Default for Visibility {
    fn default() -> Self {
        Visibility { hidden: false, protected_system: false }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permutation(pub Vec<EntryId>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub id: EntryId,
    pub dy: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub added: u32,
    pub removed: u32,
    pub changed: u32,
}

pub struct IdMap(pub Vec<Option<EntryId>>); // old id -> new id after compact

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapParts {
    pub entries: usize,
    pub names: usize,
    pub order: usize,
    pub inverse: usize,
    pub index: usize,
    pub other: usize,
}

/// What make_order leaves out of its base order: given rows, or whatever a test rejects.
enum Skip<'a> {
    Rows(&'a [u32]),
    Keep(&'a dyn Fn(EntryId) -> bool),
}

pub struct EntryStore {
    dir: WidePath,
    gen: Gen,
    done: bool,
    error: Option<EnumError>,
    arena: NameArena,
    chunks: Vec<Vec<Entry>>,
    /// Slots used (the next id).
    total: u32,
    /// Ids from here up are the pending run.
    merged_total: u32,
    /// Tombstone bitset: one u64 per 64 slots, grown by one chunk's worth (64 words) at a time.
    dead: Vec<u64>,
    dead_count: u32,
    exts: ExtTable,
    order: Permutation,
    /// Row of each id (NO_ROW: not in the order), at exact length.
    inverse: Vec<u32>,
    /// (fold hash, id) of every live entry before merged_total, sorted, at exact length.
    index: Vec<(u32, EntryId)>,
    key: SortKey,
    sdir: SortDir,
    vis: Visibility,
    /// Bumped by everything that makes an outstanding SortJob stale (a new sort, visibility, compaction).
    sort_gen: u64,
    /// A SortJob has been made and not yet adopted: changes are listed in `changed`.
    job_out: AtomicBool,
    changed: Vec<EntryId>,
}

#[allow(clippy::len_without_is_empty)] // the frozen interface has len() only
impl EntryStore {
    pub fn new(dir: WidePath, gen: Gen) -> Self {
        EntryStore {
            dir,
            gen,
            done: false,
            error: None,
            arena: NameArena::new(),
            chunks: Vec::new(),
            total: 0,
            merged_total: 0,
            dead: Vec::new(),
            dead_count: 0,
            exts: ExtTable::default(),
            order: Permutation::default(),
            inverse: Vec::new(),
            index: Vec::new(),
            key: SortKey::Name,
            sdir: SortDir::Asc,
            vis: Visibility::default(),
            sort_gen: 0,
            job_out: AtomicBool::new(false),
            changed: Vec::new(),
        }
    }
    pub fn dir(&self) -> &WidePath {
        &self.dir
    }
    pub fn gen(&self) -> Gen {
        self.gen
    }
    pub fn done(&self) -> bool {
        self.done
    }
    pub fn error(&self) -> Option<EnumError> {
        self.error
    }

    /// Ignores another gen; cheap: entries go to a pending run.
    pub fn append(&mut self, b: Batch) {
        if b.gen != self.gen {
            return;
        }
        for r in &b.entries {
            let start = r.name_off as usize;
            if let Some(name) = b.names.get(start..start + usize::from(r.name_len)) {
                self.push_entry(name, r);
            }
        }
        if b.done {
            self.done = true;
        }
        if b.error.is_some() {
            self.error = b.error;
        }
    }

    /// Sorts the pending run and merges it into order(); true if anything changed.
    pub fn merge_pending(&mut self) -> bool {
        self.rebuild(&[], &[])
    }

    pub fn set_visibility(&mut self, v: Visibility) {
        self.vis = v;
        self.sort_gen += 1;
        self.job_out.store(false, Atomic::Relaxed);
        self.changed.clear();
        self.full_sort();
    }

    pub fn sort(&mut self, key: SortKey, dir: SortDir, group: GroupBy) -> &Permutation {
        let GroupBy::None = group;
        self.key = key;
        self.sdir = dir;
        self.sort_gen += 1;
        self.job_out.store(false, Atomic::Relaxed);
        self.changed.clear();
        self.full_sort();
        &self.order
    }

    /// Send; for more than 20,000 rows, run on a worker.
    pub fn sort_job(&self, key: SortKey, dir: SortDir, group: GroupBy) -> SortJob {
        let GroupBy::None = group;
        let ranks = if key == SortKey::Type { self.exts.ranks() } else { Vec::new() };
        let recs: Vec<Rec> = (0..self.total)
            .filter_map(|i| {
                let id = EntryId(i);
                let e = self.live_entry(id).filter(|e| self.visible(e))?;
                Some(rec_of(id, e, self.arena.get(e.name_off, e.name_len)?, key, &ranks))
            })
            .collect();
        self.job_out.store(true, Atomic::Relaxed);
        SortJob { recs, names: self.arena.snapshot(), key, dir, sort_gen: self.sort_gen, snap_total: self.total }
    }

    /// Folds in what changed since the snapshot.
    pub fn adopt(&mut self, done: SortDone) -> &Permutation {
        if done.sort_gen != self.sort_gen {
            return &self.order; // stale: a sort, a visibility change or a compaction came in between
        }
        self.job_out.store(false, Atomic::Relaxed);
        (self.key, self.sdir) = (done.key, done.dir);
        let ranks = self.type_ranks();
        let mut changed = std::mem::take(&mut self.changed);
        changed.retain(|id| id.0 < done.snap_total);
        changed.sort_unstable();
        changed.dedup();
        // Re-inserted: what changed since the snapshot (it may sit elsewhere now) and what came after it.
        let mut inserts: Vec<Rec> = changed
            .iter()
            .copied()
            .chain((done.snap_total..self.total).map(EntryId))
            .filter_map(|id| self.visible_rec(&ranks, id))
            .collect();
        sort_recs(&mut inserts, &self.arena, self.key, self.desc());
        let keep = |id: EntryId| !self.is_dead(id) && changed.binary_search(&id).is_err();
        let order = self.make_order(&ranks, &done.order, Skip::Keep(&keep), &inserts);
        self.install(order);
        &self.order
    }

    /// Upserts and removes by name; order stays sorted. Every change of the diff is applied to the entries first, then
    /// the order, the inverse and the index are rebuilt once. The anchor stays as it is when its entry survives, else it
    /// moves to the next surviving row (the previous one when it was the last) with dy 0.
    pub fn apply(&mut self, d: &Diff, anchor: Option<Anchor>) -> (ApplyStats, Option<Anchor>) {
        if d.rescan {
            debug_assert!(false, "a rescan goes through rescan_merge");
            return (ApplyStats::default(), anchor);
        }
        if d.gen != self.gen {
            return (ApplyStats::default(), anchor);
        }
        if self.merged_total != self.total {
            self.merge_pending();
        }
        let ranks = self.type_ranks();
        let at = |off: u32, len: u16| d.names.get(off as usize..off as usize + usize::from(len));
        let mut stats = ApplyStats::default();
        let mut moved: Vec<EntryId> = Vec::new();
        let mut removed: Vec<EntryId> = Vec::new();
        for &(off, len) in &d.removes {
            if let Some(id) = at(off, len).and_then(|n| self.find_indexed(n)) {
                self.tombstone(id);
                removed.push(id);
                stats.removed += 1;
            }
        }
        let last = last_upserts(d);
        for (r, _) in d.upserts.iter().zip(&last).filter(|(_, keep)| **keep) {
            let Some(name) = at(r.name_off, r.name_len) else { continue };
            match self.find_indexed(name) {
                None => {
                    self.push_entry(name, r);
                    stats.added += 1;
                }
                Some(id) => {
                    let (changed, moves) = self.update_entry(id, name, r, &ranks);
                    stats.changed += u32::from(changed);
                    if moves {
                        moved.push(id);
                    }
                }
            }
        }
        let anchor = anchor.and_then(|a| self.settle_anchor(a));
        if stats.added > 0 || stats.removed > 0 || !moved.is_empty() {
            self.rebuild(&moved, &removed);
        }
        (stats, anchor)
    }

    /// Visible entries, sorted, folders first.
    pub fn order(&self) -> &Permutation {
        &self.order
    }
    pub fn row_of(&self, id: EntryId) -> Option<u32> {
        self.inverse.get(id.0 as usize).copied().filter(|r| *r != NO_ROW)
    }

    /// A subsequence of order(); empty if gen != self.gen().
    pub fn filter(&self, q: &FilterQuery, gen: Gen) -> Permutation {
        if gen != self.gen {
            return Permutation::default();
        }
        let needle = Needle::new(q);
        if needle.is_empty() {
            return self.order.clone();
        }
        Permutation(
            self.order.0.iter().copied().filter(|id| self.name(*id).is_some_and(|n| needle.matches(n))).collect(),
        )
    }

    /// Live entries, visible or not.
    pub fn len(&self) -> u32 {
        self.total - self.dead_count
    }

    /// None for a removed id.
    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.live_entry(id)
    }
    pub fn name(&self, id: EntryId) -> Option<&[u8]> {
        let e = self.live_entry(id)?;
        self.arena.get(e.name_off, e.name_len)
    }
    pub fn ext(&self, id: EntryId) -> Option<&[u8]> {
        let e = self.live_entry(id)?;
        Some(self.exts.text(e.ext))
    }
    pub fn is_dir(&self, id: EntryId) -> bool {
        self.live_entry(id).is_some_and(|e| e.attrs & ATTR_DIR != 0)
    }

    /// NTFS-style case-insensitive.
    pub fn find(&self, name: &[u8]) -> Option<EntryId> {
        self.find_indexed(name).or_else(|| {
            // Not merged yet: the pending run is short (one batch).
            (self.merged_total..self.total).map(EntryId).find(|id| self.name(*id).is_some_and(|n| fold_eq(n, name)))
        })
    }

    /// Capacities, not lengths; == the sum of heap_parts().
    pub fn heap_bytes(&self) -> usize {
        let p = self.heap_parts();
        p.entries + p.names + p.order + p.inverse + p.index + p.other
    }

    pub fn heap_parts(&self) -> HeapParts {
        let word = std::mem::size_of::<u32>();
        HeapParts {
            entries: self.chunks.capacity() * std::mem::size_of::<Vec<Entry>>()
                + self.chunks.iter().map(|c| c.capacity() * std::mem::size_of::<Entry>()).sum::<usize>(),
            names: self.arena.heap_bytes(),
            order: self.order.0.capacity() * word,
            inverse: self.inverse.capacity() * word,
            index: self.index.capacity() * std::mem::size_of::<(u32, EntryId)>(),
            other: self.dead.capacity() * std::mem::size_of::<u64>()
                + self.exts.heap_bytes()
                + self.changed.capacity() * word,
        }
    }

    pub fn needs_compact(&self) -> bool {
        u64::from(self.dead_count) * 4 > u64::from(self.total) || self.arena.holes() * 4 > self.arena.used()
    }

    /// Rebuilds the arena, the entry chunks, the order and the index without tombstones and holes. Ids are renumbered in
    /// their old order; the map tells every old id's new one (None for a removed id). Any outstanding SortJob is stale.
    pub fn compact(&mut self) -> IdMap {
        self.merge_pending();
        let mut map: Vec<Option<EntryId>> = vec![None; self.total as usize];
        let mut arena = NameArena::new();
        let (mut chunks, mut dead) = (Vec::new(), Vec::new());
        let mut next = 0u32;
        for old in 0..self.total {
            let Some(e) = self.live_entry(EntryId(old)) else { continue };
            let Some(name) = self.arena.get(e.name_off, e.name_len) else { continue };
            let (name_off, name_len) = arena.push(name);
            place_entry(&mut chunks, &mut dead, Entry { name_off, name_len, ..*e });
            if let Some(slot) = map.get_mut(old as usize) {
                *slot = Some(EntryId(next));
            }
            next += 1;
        }
        let remap = |id: &EntryId| map.get(id.0 as usize).copied().flatten();
        let order: Vec<EntryId> = self.order.0.iter().filter_map(remap).collect();
        let index: Vec<(u32, EntryId)> = self.index.iter().filter_map(|(h, id)| Some((*h, remap(id)?))).collect();
        let mut inverse = vec![NO_ROW; next as usize];
        for (row, id) in order.iter().enumerate() {
            if let Some(slot) = inverse.get_mut(id.0 as usize) {
                *slot = row as u32;
            }
        }
        self.arena = arena;
        self.chunks = chunks;
        self.dead = dead;
        self.dead_count = 0;
        self.total = next;
        self.merged_total = next;
        self.order = Permutation(order);
        self.inverse = inverse;
        self.index = index;
        self.sort_gen += 1;
        self.job_out.store(false, Atomic::Relaxed);
        self.changed.clear();
        IdMap(map)
    }

    // ---- internals

    fn live_entry(&self, id: EntryId) -> Option<&Entry> {
        if self.is_dead(id) {
            return None;
        }
        self.chunks.get(id.0 as usize / ENTRY_CHUNK)?.get(id.0 as usize % ENTRY_CHUNK)
    }

    /// True for a tombstone and for an id that was never used.
    fn is_dead(&self, id: EntryId) -> bool {
        id.0 >= self.total || self.dead.get(id.0 as usize / 64).is_none_or(|w| (w >> (id.0 % 64)) & 1 == 1)
    }

    fn visible(&self, e: &Entry) -> bool {
        let hidden = e.attrs & ATTR_HIDDEN != 0;
        let system = e.attrs & ATTR_SYSTEM != 0;
        (!hidden || self.vis.hidden) && !(hidden && system && !self.vis.protected_system)
    }

    fn desc(&self) -> bool {
        self.sdir == SortDir::Desc
    }

    fn type_ranks(&self) -> Vec<u16> {
        if self.key == SortKey::Type {
            self.exts.ranks()
        } else {
            Vec::new()
        }
    }

    /// The sort row of a live, visible entry.
    fn visible_rec(&self, ranks: &[u16], id: EntryId) -> Option<Rec> {
        let e = self.live_entry(id)?;
        if !self.visible(e) {
            return None;
        }
        Some(rec_of(id, e, self.arena.get(e.name_off, e.name_len)?, self.key, ranks))
    }

    /// Copies the name, interns the extension and stores the entry in the last chunk (a new chunk when it is full).
    fn push_entry(&mut self, name: &[u8], r: &RawEntry) -> EntryId {
        let (name_off, name_len) = self.arena.push(name);
        let ext = self.exts.intern_name(name);
        let e = Entry {
            name_off,
            name_len,
            ext,
            attrs: r.attrs,
            reparse_tag: r.reparse_tag,
            size: r.size,
            mtime: r.mtime,
            ctime: r.ctime,
        };
        place_entry(&mut self.chunks, &mut self.dead, e);
        let id = EntryId(self.total);
        self.total += 1;
        id
    }

    /// The name of an entry whether or not it is tombstoned.
    fn stored_name(&self, id: EntryId) -> Option<&[u8]> {
        let e = self.chunks.get(id.0 as usize / ENTRY_CHUNK)?.get(id.0 as usize % ENTRY_CHUNK)?;
        self.arena.get(e.name_off, e.name_len)
    }

    /// Sets the tombstone of a live entry; its name becomes a hole of the arena.
    fn tombstone(&mut self, id: EntryId) {
        let Some(len) = self.live_entry(id).map(|e| e.name_len) else { return };
        if let Some(w) = self.dead.get_mut(id.0 as usize / 64) {
            *w |= 1 << (id.0 % 64);
        }
        self.dead_count += 1;
        self.arena.add_hole(len);
    }

    /// Writes an upsert into an existing entry. Returns (anything differs, the row must move): a move is a change of
    /// the name, of the folder flag, of the sort value or of visibility.
    fn update_entry(&mut self, id: EntryId, name: &[u8], r: &RawEntry, ranks: &[u16]) -> (bool, bool) {
        let Some(old) = self.live_entry(id).copied() else { return (false, false) };
        let renamed = self.arena.get(old.name_off, old.name_len) != Some(name);
        let mut e =
            Entry { attrs: r.attrs, reparse_tag: r.reparse_tag, size: r.size, mtime: r.mtime, ctime: r.ctime, ..old };
        if renamed {
            self.arena.add_hole(old.name_len);
            (e.name_off, e.name_len) = self.arena.push(name);
            e.ext = self.exts.intern_name(name);
        }
        if e == old {
            return (false, false);
        }
        let place = |e: &Entry| rec_of(id, e, b"", self.key, ranks).place();
        let moves = renamed || place(&e) != place(&old) || self.visible(&e) != self.visible(&old);
        if let Some(slot) =
            self.chunks.get_mut(id.0 as usize / ENTRY_CHUNK).and_then(|c| c.get_mut(id.0 as usize % ENTRY_CHUNK))
        {
            *slot = e;
        }
        if self.job_out.load(Atomic::Relaxed) {
            self.changed.push(id);
        }
        (true, moves)
    }

    /// The anchor after a change, before the order is rebuilt: itself when its entry stays visible, else the next row
    /// that stays (else the previous one), else None.
    fn settle_anchor(&self, a: Anchor) -> Option<Anchor> {
        let stays = |id: &EntryId| self.live_entry(*id).is_some_and(|e| self.visible(e));
        if stays(&a.id) {
            return Some(a);
        }
        let row = self.row_of(a.id)? as usize;
        let (before, after) = self.order.0.split_at(row);
        let id = after.iter().skip(1).find(|id| stays(id)).or_else(|| before.iter().rev().find(|id| stays(id)))?;
        Some(Anchor { id: *id, dy: 0 })
    }

    /// The index lookup alone (what a merge has put there).
    fn find_indexed(&self, name: &[u8]) -> Option<EntryId> {
        let h = fold_hash(name);
        let start = self.index.partition_point(|(k, _)| *k < h);
        self.index
            .iter()
            .skip(start)
            .take_while(|(k, _)| *k == h)
            .map(|(_, id)| *id)
            .find(|id| self.name(*id).is_some_and(|n| fold_eq(n, name)))
    }

    /// Sorts every visible live entry (pending included) and installs the result.
    fn full_sort(&mut self) {
        let ranks = self.type_ranks();
        let mut recs = Vec::with_capacity(self.len() as usize);
        recs.extend((0..self.total).filter_map(|i| self.visible_rec(&ranks, EntryId(i))));
        sort_recs(&mut recs, &self.arena, self.key, self.desc());
        let order: Vec<EntryId> = recs.iter().map(|r| EntryId(r.id)).collect();
        self.install_rows(order);
        // Only the order changed when the index already holds every live entry and nothing else.
        if self.merged_total != self.total || self.index.len() != self.len() as usize {
            self.rebuild_index();
        }
    }

    /// Merges the pending run and the `moved` entries (live ones whose place changed) into the order, and leaves
    /// the `removed` ones (tombstoned just now) out of the order and the index.
    fn rebuild(&mut self, moved: &[EntryId], removed: &[EntryId]) -> bool {
        let ranks = self.type_ranks();
        let mut inserts: Vec<Rec> = (self.merged_total..self.total)
            .map(EntryId)
            .chain(moved.iter().copied())
            .filter_map(|id| self.visible_rec(&ranks, id))
            .collect();
        sort_recs(&mut inserts, &self.arena, self.key, self.desc());
        // The rows to leave out of the old order: from the inverse, so no row is tested one by one.
        let mut rows: Vec<u32> = moved.iter().chain(removed).filter_map(|id| self.row_of(*id)).collect();
        rows.sort_unstable();
        rows.dedup();
        let order = self.make_order(&ranks, &self.order.0, Skip::Rows(&rows), &inserts);
        let changed = !inserts.is_empty() || !rows.is_empty();
        let mut gone: Vec<(u32, EntryId)> =
            removed.iter().filter_map(|id| Some((fold_hash(self.stored_name(*id)?), *id))).collect();
        gone.sort_unstable();
        self.install_rows(order);
        self.update_index(&gone);
        changed
    }

    /// The survivors of `base` with the sorted `inserts` placed among them, at exact length. Each insert is
    /// found by binary search and the runs between are copied as blocks: no per-row compare for the rows in place.
    fn make_order(&self, ranks: &[u16], base: &[EntryId], skip: Skip, inserts: &[Rec]) -> Vec<EntryId> {
        let surv: Vec<EntryId> = match skip {
            Skip::Rows(rows) => {
                let mut v = Vec::with_capacity(base.len().saturating_sub(rows.len()));
                let mut lo = 0;
                for r in rows {
                    v.extend_from_slice(base.get(lo..*r as usize).unwrap_or(&[]));
                    lo = *r as usize + 1;
                }
                v.extend_from_slice(base.get(lo..).unwrap_or(&[]));
                v
            }
            Skip::Keep(keep) => base.iter().copied().filter(|id| keep(*id)).collect(),
        };
        let desc = self.desc();
        let mut out = Vec::with_capacity(surv.len() + inserts.len());
        let mut lo = 0;
        for r in inserts {
            let rest = surv.get(lo..).unwrap_or(&[]);
            let p = lo
                + rest.partition_point(|s| {
                    self.visible_rec(ranks, *s).is_none_or(|sr| rec_cmp(&sr, r, &self.arena, desc).is_lt())
                });
            out.extend_from_slice(surv.get(lo..p).unwrap_or(&[]));
            out.push(EntryId(r.id));
            lo = p;
        }
        out.extend_from_slice(surv.get(lo..).unwrap_or(&[]));
        out
    }

    /// Installs a new order and rebuilds the inverse at exact length.
    fn install_rows(&mut self, order: Vec<EntryId>) {
        let mut inverse = vec![NO_ROW; self.total as usize];
        for (row, id) in order.iter().enumerate() {
            if let Some(slot) = inverse.get_mut(id.0 as usize) {
                *slot = row as u32;
            }
        }
        self.order = Permutation(order);
        self.inverse = inverse;
    }

    /// Installs a new order and rebuilds the index from scratch (tombstoned ids dropped, the pending run merged in).
    fn install(&mut self, order: Vec<EntryId>) {
        self.install_rows(order);
        self.rebuild_index();
    }

    /// The index without the (hash, id) pairs of `removed` (sorted) and with the pending run merged in, at exact
    /// length: the runs between the changes are copied as blocks.
    fn update_index(&mut self, removed: &[(u32, EntryId)]) {
        let mut adds: Vec<(u32, EntryId)> = (self.merged_total..self.total)
            .map(EntryId)
            .filter_map(|id| Some((fold_hash(self.name(id)?), id)))
            .collect();
        adds.sort_unstable();
        let mut kept: Vec<(u32, EntryId)> = Vec::new();
        if !removed.is_empty() {
            kept.reserve_exact(self.index.len());
            let mut lo = 0;
            for key in removed {
                if let Ok(p) = self.index.binary_search(key) {
                    kept.extend_from_slice(self.index.get(lo..p).unwrap_or(&[]));
                    lo = p + 1;
                }
            }
            kept.extend_from_slice(self.index.get(lo..).unwrap_or(&[]));
        }
        let base: &[(u32, EntryId)] = if removed.is_empty() { &self.index } else { &kept };
        let mut out = Vec::with_capacity(base.len() + adds.len());
        let mut lo = 0;
        for a in &adds {
            let p = lo + base.get(lo..).unwrap_or(&[]).partition_point(|x| x < a);
            out.extend_from_slice(base.get(lo..p).unwrap_or(&[]));
            out.push(*a);
            lo = p;
        }
        out.extend_from_slice(base.get(lo..).unwrap_or(&[]));
        self.index = out;
        self.merged_total = self.total;
    }

    /// Drops the tombstoned ids from the index and merges in the pending run (hashed and sorted once), at exact length.
    fn rebuild_index(&mut self) {
        let mut adds: Vec<(u32, EntryId)> = (self.merged_total..self.total)
            .map(EntryId)
            .filter_map(|id| Some((fold_hash(self.name(id)?), id)))
            .collect();
        adds.sort_unstable();
        let mut out = Vec::with_capacity(self.len() as usize);
        let mut old = self.index.iter().copied().filter(|(_, id)| !self.is_dead(*id)).peekable();
        let mut new = adds.into_iter().peekable();
        loop {
            let take_old = match (old.peek(), new.peek()) {
                (Some(o), Some(n)) => o <= n,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            out.extend(if take_old { old.next() } else { new.next() });
        }
        drop(old);
        self.index = out;
        self.merged_total = self.total;
    }
}

/// Stores an entry in the last chunk, starting a new one (fixed capacity, never grown) when it is full, and grows the
/// tombstone bitset by one chunk's worth of words with it.
fn place_entry(chunks: &mut Vec<Vec<Entry>>, dead: &mut Vec<u64>, e: Entry) {
    if chunks.last().is_none_or(|c| c.len() == ENTRY_CHUNK) {
        chunks.push(Vec::with_capacity(ENTRY_CHUNK));
        dead.reserve_exact(ENTRY_CHUNK / 64);
        dead.resize(dead.len() + ENTRY_CHUNK / 64, 0);
    }
    if let Some(c) = chunks.last_mut() {
        c.push(e);
    }
}

/// Overflow: merge by name, survivors keep their ids. Names in both keep the store's id and take the fresh metadata,
/// names only in `fresh` are added, names only in the store are removed.
/// For each upsert of the diff, whether it is the last one of its (case-folded) name: a name upserted twice in one diff
/// must not be pushed twice, because the first push is in the pending run and not yet indexed. O(n log n).
fn last_upserts(d: &Diff) -> Vec<bool> {
    let mut keep = vec![true; d.upserts.len()];
    if d.upserts.len() < 2 {
        return keep;
    }
    let name = |r: &RawEntry| d.names.get(r.name_off as usize..r.name_off as usize + usize::from(r.name_len));
    let mut by_hash: Vec<(u32, usize)> =
        d.upserts.iter().enumerate().filter_map(|(i, r)| Some((fold_hash(name(r)?), i))).collect();
    by_hash.sort_unstable();
    for run in by_hash.chunk_by(|a, b| a.0 == b.0).filter(|run| run.len() > 1) {
        for (k, &(_, i)) in run.iter().enumerate() {
            let (Some(a), Some(rest)) = (d.upserts.get(i).and_then(name), run.get(k + 1..)) else { continue };
            if rest.iter().any(|&(_, j)| d.upserts.get(j).and_then(name).is_some_and(|b| fold_eq(a, b))) {
                keep[i] = false;
            }
        }
    }
    keep
}

pub fn rescan_merge(store: &mut EntryStore, mut fresh: EntryStore) -> ApplyStats {
    fresh.merge_pending();
    let mut d = Diff::new(store.gen);
    for i in 0..fresh.total {
        let id = EntryId(i);
        if let (Some(e), Some(name)) = (fresh.entry(id), fresh.name(id)) {
            let raw = RawEntry {
                name_off: 0,
                name_len: 0,
                attrs: e.attrs,
                reparse_tag: e.reparse_tag,
                size: e.size,
                mtime: e.mtime,
                ctime: e.ctime,
            };
            d.upsert(name, raw);
        }
    }
    for i in 0..store.total {
        if let Some(name) = store.name(EntryId(i)) {
            if fresh.find_indexed(name).is_none() {
                d.remove(name);
            }
        }
    }
    let (stats, _) = store.apply(&d, None);
    if fresh.done {
        store.done = true;
        store.error = fresh.error;
    }
    stats
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::natural::natural_cmp;
    use super::*;

    pub(crate) fn raw(attrs: u32, size: u64) -> RawEntry {
        RawEntry { name_off: 0, name_len: 0, attrs, reparse_tag: 0, size, mtime: 0, ctime: 0 }
    }

    /// A batch of (name, attrs) rows.
    pub(crate) fn batch(gen: u64, rows: &[(String, u32)], done: bool) -> Batch {
        let mut b = Batch { gen: Gen(gen), names: Vec::new(), entries: Vec::new(), done, error: None, elapsed_us: 0 };
        for (name, attrs) in rows {
            let off = b.names.len() as u32;
            b.names.extend_from_slice(name.as_bytes());
            b.entries.push(RawEntry { name_off: off, name_len: name.len() as u16, ..raw(*attrs, 7) });
        }
        b
    }

    pub(crate) fn store() -> EntryStore {
        EntryStore::new(WidePath::parse("C:\\data").unwrap(), Gen(1))
    }

    /// Deterministic mixed names: letters, digits, dots, a few folders.
    pub(crate) fn names(n: usize) -> Vec<(String, u32)> {
        let mut x: u32 = 12345;
        (0..n)
            .map(|i| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let stem = ["alpha", "Beta", "gamma", "DELTA", "file", "doc"][(x >> 16) as usize % 6];
                let ext = ["txt", "RS", "md", "", "bin"][(x >> 8) as usize % 5];
                let dot = if ext.is_empty() { "" } else { "." };
                (format!("{stem} {}{dot}{ext}", i), if (x >> 4) & 7 == 0 { ATTR_DIR } else { 0x20 })
            })
            .collect()
    }

    pub(crate) fn order_names(s: &EntryStore) -> Vec<String> {
        s.order().0.iter().map(|id| String::from_utf8(s.name(*id).unwrap().to_vec()).unwrap()).collect()
    }

    #[test]
    fn append_ignores_other_gen() {
        let mut s = store();
        s.append(batch(2, &names(10), true));
        assert_eq!(s.len(), 0);
        assert!(!s.done());
        s.append(batch(1, &names(10), true));
        assert_eq!(s.len(), 10);
        assert!(s.done());
    }

    #[test]
    fn append_then_merge_is_sorted() {
        let all = names(300);
        let mut two = store();
        two.append(batch(1, &all[..150], false));
        assert!(two.merge_pending());
        two.append(batch(1, &all[150..], true));
        assert!(two.merge_pending());
        let mut one = store();
        one.append(batch(1, &all, true));
        one.merge_pending();
        assert_eq!(order_names(&two), order_names(&one));
        let mut want = all.clone();
        want.sort_by(|a, b| {
            (b.1 & ATTR_DIR).cmp(&(a.1 & ATTR_DIR)).then_with(|| natural_cmp(a.0.as_bytes(), b.0.as_bytes()))
        });
        let want: Vec<String> = want.into_iter().map(|r| r.0).collect();
        assert_eq!(order_names(&two), want);
        assert!(!two.merge_pending(), "nothing pending: nothing changed");
    }

    #[test]
    fn entry_chunks_never_grow() {
        let mut s = store();
        for k in 0..10 {
            s.append(batch(
                1,
                &names(1000).into_iter().map(|(n, a)| (format!("{k}-{n}"), a)).collect::<Vec<_>>(),
                false,
            ));
            s.merge_pending();
        }
        assert_eq!(s.len(), 10_000);
        assert_eq!(s.chunks.len(), 3);
        assert!(
            s.chunks.iter().all(|c| c.capacity() == ENTRY_CHUNK),
            "{:?}",
            s.chunks.iter().map(|c| c.capacity()).collect::<Vec<_>>()
        );
        assert_eq!(s.dead.len(), 3 * 64);
    }

    #[test]
    fn exact_length_after_merge() {
        let mut s = store();
        s.append(batch(1, &names(500), false));
        s.merge_pending();
        s.append(batch(1, &names(700).into_iter().map(|(n, a)| (format!("x{n}"), a)).collect::<Vec<_>>(), true));
        s.merge_pending();
        assert_eq!(s.order.0.capacity(), s.order.0.len());
        assert_eq!(s.inverse.capacity(), s.inverse.len());
        assert_eq!(s.index.capacity(), s.index.len());
        assert_eq!((s.order.0.len(), s.inverse.len(), s.index.len()), (1200, 1200, 1200));
        let p = s.heap_parts();
        assert_eq!(s.heap_bytes(), p.entries + p.names + p.order + p.inverse + p.index + p.other);
    }

    #[test]
    fn first_batch_alone_is_complete_order() {
        let mut s = store();
        s.append(batch(1, &names(200), false));
        assert!(s.order().0.is_empty(), "nothing is ordered before the merge");
        assert!(s.merge_pending());
        let o = order_names(&s);
        assert_eq!(o.len(), 200);
        for (i, id) in s.order().0.iter().enumerate() {
            assert_eq!(s.row_of(*id), Some(i as u32));
        }
    }

    #[test]
    fn find_is_case_insensitive() {
        let mut s = store();
        s.append(batch(
            1,
            &[("README.txt".into(), 0x20), ("ss".into(), 0x20), ("ß".into(), 0x20), ("Äb".into(), 0x20)],
            true,
        ));
        let id = s.find(b"Readme.TXT").unwrap();
        assert_eq!(s.name(id), Some(&b"README.txt"[..]), "found before any merge, in the pending run");
        s.merge_pending();
        assert_eq!(s.find(b"Readme.TXT"), Some(id), "and through the index after it");
        assert_ne!(s.find("ß".as_bytes()), s.find(b"ss"));
        assert!(s.find("ß".as_bytes()).is_some());
        assert!(s.find("äB".as_bytes()).is_some());
        assert_eq!(s.find(b"missing"), None);
    }

    #[test]
    fn ext_of_leading_dot() {
        let mut s = store();
        s.append(batch(
            1,
            &[(".gitignore".into(), 0x20), ("a.TAR.GZ".into(), 0x20), ("noext".into(), 0x20), ("dot.".into(), 0x20)],
            true,
        ));
        let ext = |n: &[u8]| s.ext(s.find(n).unwrap()).unwrap().to_vec();
        assert_eq!(ext(b".gitignore"), b"gitignore");
        assert_eq!(ext(b"a.tar.gz"), b"gz");
        assert_eq!(ext(b"noext"), b"");
        assert_eq!(ext(b"dot."), b"");
    }

    fn up(d: &mut Diff, name: &str, attrs: u32, size: u64, mtime: i64) {
        d.upsert(name.as_bytes(), RawEntry { name_off: 0, name_len: 0, attrs, reparse_tag: 0, size, mtime, ctime: 0 });
    }

    fn letters(n: u8) -> EntryStore {
        let mut s = store();
        s.append(batch(1, &(0..n).map(|i| (((b'a' + i) as char).to_string(), 0x20)).collect::<Vec<_>>(), true));
        s.merge_pending();
        s
    }

    #[test]
    fn apply_keeps_order() {
        use std::collections::BTreeMap;
        let mut s = store();
        let mut model: BTreeMap<String, (String, u32, u64, i64)> = BTreeMap::new();
        for (n, a) in names(300) {
            model.insert(n.to_lowercase(), (n, a, 7, 0));
        }
        s.append(batch(1, &names(300), true));
        s.sort(SortKey::Modified, SortDir::Desc, GroupBy::None);
        let mut x: u64 = 99;
        let mut rnd = move |n: u64| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) % n
        };
        let mut serial = 0;
        for _round in 0..50 {
            let mut d = Diff::new(Gen(1));
            let mut used = std::collections::BTreeSet::new();
            for _ in 0..10 {
                let keys: Vec<String> = model.keys().cloned().collect();
                let pick = keys[rnd(keys.len() as u64) as usize].clone();
                if !used.insert(pick.clone()) {
                    continue;
                }
                match rnd(3) {
                    0 => {
                        d.remove(model[&pick].0.as_bytes());
                        model.remove(&pick);
                    }
                    1 => {
                        let (n, a, _, _) = model[&pick].clone();
                        let (size, mtime) = (rnd(50), rnd(20) as i64 - 10);
                        let a = if rnd(5) == 0 { a ^ 0x2 } else { a };
                        up(&mut d, &n, a, size, mtime);
                        model.insert(pick, (n, a, size, mtime));
                    }
                    _ => {
                        serial += 1;
                        let n = format!("new {serial}");
                        used.insert(n.to_lowercase());
                        let mtime = rnd(20) as i64 - 10;
                        up(&mut d, &n, 0x20, 1, mtime);
                        model.insert(n.to_lowercase(), (n, 0x20, 1, mtime));
                    }
                }
            }
            s.apply(&d, None);
        }
        let mut fresh = store();
        let rows: Vec<(String, u32)> = model.values().map(|v| (v.0.clone(), v.1)).collect();
        let mut b = batch(1, &rows, true);
        for (e, v) in b.entries.iter_mut().zip(model.values()) {
            (e.size, e.mtime) = (v.2, v.3);
        }
        fresh.append(b);
        fresh.sort(SortKey::Modified, SortDir::Desc, GroupBy::None);
        assert_eq!(order_names(&s), order_names(&fresh));
        assert_eq!(s.len() as usize, model.len());
        for (id, name) in s.order().0.iter().zip(order_names(&s)) {
            assert_eq!(s.name(*id), Some(name.as_bytes()));
            assert!(s.find(name.as_bytes()).is_some());
        }
        assert_eq!(s.order().0.capacity(), s.order().0.len());
    }

    #[test]
    fn anchor_follows_survivor() {
        let mut s = letters(10);
        let e = s.find(b"e").unwrap();
        let mut d = Diff::new(Gen(1));
        d.remove(b"a");
        up(&mut d, "zz", 0x20, 1, 0);
        let (stats, anchor) = s.apply(&d, Some(Anchor { id: e, dy: 17 }));
        assert_eq!(stats, ApplyStats { added: 1, removed: 1, changed: 0 });
        assert_eq!(anchor, Some(Anchor { id: e, dy: 17 }), "unchanged, dy included");
        assert_eq!(s.find(b"zz").map(|id| s.row_of(id)), Some(Some(9)));
    }

    #[test]
    fn anchor_moves_to_neighbour() {
        let mut s = letters(10);
        let (e, j) = (s.find(b"e").unwrap(), s.find(b"j").unwrap());
        let mut d = Diff::new(Gen(1));
        d.remove(b"e");
        let (_, anchor) = s.apply(&d, Some(Anchor { id: e, dy: 17 }));
        assert_eq!(anchor, Some(Anchor { id: s.find(b"f").unwrap(), dy: 0 }), "the next row");
        let mut d = Diff::new(Gen(1));
        d.remove(b"j");
        let (_, anchor) = s.apply(&d, Some(Anchor { id: j, dy: 3 }));
        assert_eq!(anchor, Some(Anchor { id: s.find(b"i").unwrap(), dy: 0 }), "the previous one at the end");
        let mut d = Diff::new(Gen(1));
        for n in ["a", "b", "c", "d", "f", "g", "h", "i"] {
            d.remove(n.as_bytes());
        }
        let last = s.order().0[0];
        assert_eq!(s.apply(&d, Some(Anchor { id: last, dy: 0 })).1, None, "nothing left");
    }

    #[test]
    fn rescan_merge_keeps_ids() {
        let mut s = letters(3);
        let (a, b) = (s.find(b"a").unwrap(), s.find(b"b").unwrap());
        let mut fresh = store();
        let mut batch2 = batch(1, &[("b".into(), 0x20), ("c".into(), 0x20), ("w".into(), 0x20)], true);
        batch2.entries[0].size = 99;
        fresh.append(batch2);
        let stats = rescan_merge(&mut s, fresh);
        assert_eq!(stats, ApplyStats { added: 1, removed: 1, changed: 1 });
        assert_eq!(s.find(b"a"), None);
        assert_eq!(s.find(b"b"), Some(b));
        assert_eq!(s.entry(b).unwrap().size, 99);
        assert!(s.entry(a).is_none());
        assert_eq!(order_names(&s), ["b", "c", "w"]);
    }

    #[test]
    fn compact_maps_ids() {
        let mut s = store();
        let rows: Vec<(String, u32)> = (0..5000).map(|i| (format!("f{i:05}"), 0x20)).collect();
        s.append(batch(1, &rows, true));
        s.merge_pending();
        assert!(!s.needs_compact());
        let mut d = Diff::new(Gen(1));
        for i in (0..5000).step_by(2) {
            d.remove(format!("f{i:05}").as_bytes());
        }
        s.apply(&d, None);
        assert_eq!(s.len(), 2500);
        assert!(s.needs_compact());
        let (before, heap) = (order_names(&s), s.heap_bytes());
        let kept = s.find(b"f00003").unwrap();
        let map = s.compact();
        assert_eq!(map.0.len(), 5000);
        assert_eq!(map.0[0], None);
        assert_eq!(map.0[kept.0 as usize], Some(EntryId(1)), "dense, in the old order");
        assert_eq!(order_names(&s), before);
        assert_eq!(s.find(b"f00003"), Some(EntryId(1)));
        assert_eq!(s.find(b"f00002"), None);
        assert!(!s.needs_compact());
        assert!(s.heap_bytes() < heap);
        assert_eq!(s.row_of(EntryId(1)), Some(1), "f00003 is the second kept name");
        assert_eq!(s.index.len(), 2500);
    }

    #[test]
    fn idempotent_upsert() {
        let mut s = letters(5);
        let mut d = Diff::new(Gen(1));
        up(&mut d, "c", 0x20, 42, 5);
        up(&mut d, "new", 0x20, 1, 1);
        let (first, _) = s.apply(&d, None);
        assert_eq!(first, ApplyStats { added: 1, removed: 0, changed: 1 });
        let order = s.order().clone();
        // The watcher repeats what the app itself did: the second time nothing is added and nothing changes.
        let mut d2 = Diff::new(Gen(1));
        up(&mut d2, "c", 0x20, 42, 5);
        let (second, _) = s.apply(&d2, None);
        assert_eq!(second, ApplyStats::default());
        assert_eq!(*s.order(), order);
        assert_eq!(s.len(), 6);
    }

    #[test]
    fn duplicate_upserts_in_one_diff_add_once() {
        let mut s = letters(5);
        let mut d = Diff::new(Gen(1));
        up(&mut d, "New", 0x20, 1, 1);
        up(&mut d, "new", 0x20, 2, 2);
        up(&mut d, "NEW", 0x20, 3, 3);
        let (stats, _) = s.apply(&d, None);
        assert_eq!(stats.added, 1);
        assert_eq!(s.len(), 6);
        assert_eq!(s.order().0.len(), 6);
        let id =
            s.order().0.iter().copied().find(|id| s.name(*id) == Some(b"NEW".as_slice())).expect("the last one wins");
        assert_eq!(s.entry(id).map(|e| e.size), Some(3));
        // The same name twice in the same case.
        let mut d2 = Diff::new(Gen(1));
        up(&mut d2, "x", 0x20, 1, 1);
        up(&mut d2, "x", 0x20, 2, 2);
        s.apply(&d2, None);
        assert_eq!(s.len(), 7);
    }

    #[test]
    fn visibility_change_clears_job_state() {
        let mut s = letters(5);
        let job = s.sort_job(SortKey::Name, SortDir::Asc, GroupBy::None);
        let mut d = Diff::new(Gen(1));
        up(&mut d, "c", 0x20, 42, 5);
        s.apply(&d, None);
        assert!(!s.changed.is_empty());
        s.set_visibility(Visibility { hidden: true, protected_system: false });
        assert!(s.changed.is_empty());
        s.adopt(job.run()); // stale: ignored
        let mut d2 = Diff::new(Gen(1));
        up(&mut d2, "c", 0x20, 43, 5);
        s.apply(&d2, None);
        assert!(s.changed.is_empty(), "no job is out: nothing is recorded");
    }

    #[test]
    fn entry_is_40_bytes() {
        assert_eq!(std::mem::size_of::<Entry>(), 40);
    }
}
