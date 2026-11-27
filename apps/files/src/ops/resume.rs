//! Copy the rest, Move the rest (OP7): the plan that resumes an interrupted copy or move from its journal, pure over
//! the parsed journal and a stat function, then run by the runner under one new operation. Per top-level item
//! (nested items are not journaled; a top-level folder that was started is resumed as a whole):
//!
//! - Copy: done with a destination equal to the source (size and mtime; folders: a folder) -> skip; started, or done
//!   but different, with pre absent -> group R (copied again with ReplaceAll: the destination is ours); anything else
//!   with a destination, or no destination -> group A (AskSystem); no source -> ItemFailed(NotFound); pre reparse ->
//!   A, never R.
//! - Move: source and no destination -> A; both, started, pre absent -> R (our partial cross-volume move: the shell
//!   deletes a source only after its copy completed, so moving it again over our own partial destination loses
//!   nothing); both otherwise -> A; destination and no source -> moved (skip); neither -> ItemFailed(NotFound);
//!   pre reparse -> A.
//!
//! R runs first, then A. A destination that existed before the operation (pre file, dir or reparse) never enters R.
//! Group R keeps the journaled pre-flight (absent: the partial destination is ours); group A journals the runner's own
//! fresh pre-flight, because an item that never started carries only the "queued" placeholder (absent, written before
//! any pre-flight looked at its destination), and a destination found there now must never read as ours at a later
//! resume.

use oled_trust::frame::files::{Conflicts, OpId, OpKind, Refusal, WidePath};

use super::consts::{CHUNK_ITEMS, STAGE_IN_MEMORY};
use super::journal::{Journal, PathRepr, Pre, St};
use super::runner::Prepared;
use super::undo::UndoFs;

/// What to do with one top-level item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Skip,
    /// Again, replacing our own partial destination.
    R,
    /// Again, asking the system about any collision.
    A,
    NotFound,
}

/// Size, mtime and folder flag.
pub type Stat = (u64, i64, bool);

/// The rule of one item. `st` is its latest journal state (Queued when it never started). Pure.
pub fn decide(kind: OpKind, pre: Pre, st: St, src: Option<Stat>, dst: Option<Stat>) -> Decision {
    match kind {
        OpKind::Move => match (src, dst) {
            (Some(_), None) => Decision::A,
            (Some(_), Some(_)) if pre == Pre::Absent && st == St::Started => Decision::R,
            (Some(_), Some(_)) => Decision::A,
            (None, Some(_)) => Decision::Skip,
            (None, None) => Decision::NotFound,
        },
        _ => {
            let Some(s) = src else { return Decision::NotFound };
            if pre == Pre::Reparse {
                return Decision::A;
            }
            let equal = |d: Stat| if s.2 { d.2 } else { !d.2 && d.0 == s.0 && d.1 == s.1 };
            match (st, dst) {
                (St::Done, Some(d)) if equal(d) => Decision::Skip,
                (St::Started | St::Done, _) if pre == Pre::Absent => Decision::R,
                _ => Decision::A,
            }
        }
    }
}

/// One top-level item of the journal: its source, its pre-flight state and its latest step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub src: WidePath,
    pub pre: Pre,
    pub st: St,
}

/// The items of an interrupted operation in index order: the journal's records, then the stage spill's items
/// (`spill`, which start at index STAGE_IN_MEMORY) that have no record.
pub fn items(j: &Journal, spill: &[WidePath]) -> Vec<Item> {
    let mut by: std::collections::BTreeMap<u32, Item> = std::collections::BTreeMap::new();
    for r in j.latest() {
        if let Some(src) = r.src.to_path() {
            by.insert(r.i, Item { src, pre: r.pre, st: r.st });
        }
    }
    for (k, src) in spill.iter().enumerate() {
        let i = (STAGE_IN_MEMORY + k) as u32;
        by.entry(i).or_insert_with(|| Item { src: src.clone(), pre: Pre::Absent, st: St::Queued });
    }
    by.into_values().collect()
}

/// The resume of `j` under the new operation `op`: its chunks (R first, then A) and the items whose source and
/// destination are both gone. Only copies and moves resume.
pub fn plan(
    op: OpId,
    j: &Journal,
    spill: &[WidePath],
    fs: &dyn UndoFs,
) -> Result<(Vec<Prepared>, Vec<WidePath>), Refusal> {
    let kind: OpKind = j.header.kind.into();
    if !matches!(kind, OpKind::Copy | OpKind::Move) {
        return Err(Refusal::UnknownOp);
    }
    let dest = j.header.dest.as_ref().and_then(PathRepr::to_path).ok_or(Refusal::BadPath)?;
    let (mut r, mut a, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    for it in items(j, spill) {
        let dst = it.src.file_name().and_then(|n| dest.join(n).ok());
        let d = decide(kind, it.pre, it.st, fs.stat(&it.src), dst.as_ref().and_then(|p| fs.stat(p)));
        match d {
            Decision::R => r.push(it),
            Decision::A => a.push(it),
            Decision::NotFound => missing.push(it.src),
            Decision::Skip => {}
        }
    }
    let mut out = Vec::new();
    for (group, conflicts) in [(r, Conflicts::ReplaceAll), (a, Conflicts::AskSystem)] {
        for part in group.chunks(CHUNK_ITEMS) {
            // R: the journaled pre-flight; A: none, so the runner journals its own (a "queued" placeholder never
            // reaches a new journal as a pre-flight).
            let pre =
                if conflicts == Conflicts::ReplaceAll { part.iter().map(|i| i.pre).collect() } else { Vec::new() };
            out.push(Prepared {
                op,
                kind,
                items: part.iter().map(|i| i.src.clone()).collect(),
                dest: Some(dest.clone()),
                name: None,
                conflicts,
                pre,
                names: Vec::new(),
            });
        }
    }
    Ok((out, missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    const F: Option<Stat> = Some((10, 7, false));
    const F2: Option<Stat> = Some((11, 7, false));
    const D: Option<Stat> = Some((0, 9, true));

    #[test]
    fn copy_rows() {
        let k = OpKind::Copy;
        assert_eq!(decide(k, Pre::Absent, St::Done, F, F), Skip, "done and equal");
        assert_eq!(decide(k, Pre::Absent, St::Done, D, D), Skip, "a folder done");
        assert_eq!(decide(k, Pre::Absent, St::Started, F, F2), R, "started, ours");
        assert_eq!(decide(k, Pre::Absent, St::Started, F, None), R, "started, nothing written yet");
        assert_eq!(decide(k, Pre::Absent, St::Done, F, F2), R, "done but different");
        assert_eq!(decide(k, Pre::Absent, St::Queued, F, None), A, "missing destination");
        assert_eq!(decide(k, Pre::Absent, St::Queued, F, F), A, "a destination that appeared");
        assert_eq!(decide(k, Pre::Absent, St::Failed, F, F2), A);
        assert_eq!(decide(k, Pre::Absent, St::Skipped, F, F2), A);
        assert_eq!(decide(k, Pre::File, St::Started, F, F2), A, "anything else with an existing destination");
        assert_eq!(decide(k, Pre::Absent, St::Started, None, F), NotFound, "missing source");
        assert_eq!(decide(k, Pre::Reparse, St::Started, F, None), A, "reparse: A, never R");
    }

    #[test]
    fn move_rows() {
        let k = OpKind::Move;
        assert_eq!(decide(k, Pre::Absent, St::Queued, F, None), A, "source, no destination");
        assert_eq!(decide(k, Pre::Absent, St::Started, F, F2), R, "our partial move");
        assert_eq!(decide(k, Pre::File, St::Started, F, F2), A, "both, the destination was there before");
        assert_eq!(decide(k, Pre::Absent, St::Queued, F, F2), A, "both, never started");
        assert_eq!(decide(k, Pre::Absent, St::Done, F, F2), A);
        assert_eq!(decide(k, Pre::Absent, St::Started, None, F), Skip, "moved");
        assert_eq!(decide(k, Pre::Absent, St::Done, None, F), Skip);
        assert_eq!(decide(k, Pre::Absent, St::Started, None, None), NotFound, "neither");
        assert_eq!(decide(k, Pre::Reparse, St::Started, F, F2), A, "reparse: A");
    }

    /// A fake disk: the stats of the paths it holds.
    struct Disk(std::collections::HashMap<Vec<u16>, Stat>);

    impl UndoFs for Disk {
        fn stat(&self, p: &WidePath) -> Option<Stat> {
            self.0.get(p.units()).copied()
        }
        fn is_empty_dir(&self, _p: &WidePath) -> bool {
            false
        }
    }

    #[test]
    fn group_a_journals_its_own_preflight() {
        use super::super::journal::{Header, JConflicts, JKind, Record, VERSION};
        let w = |s: &str| WidePath::parse(s).unwrap();
        let rec = |i: u32, n: &str, st: St| Record {
            i,
            src: PathRepr::of(&w(&format!("C:\\s\\{n}"))),
            dst: None,
            pre: Pre::Absent,
            st,
            size: 0,
            mtime: 0,
        };
        let header = Header {
            v: VERSION,
            op: 1,
            kind: JKind::Copy,
            host_pid: 2,
            host_created: 3,
            dest: Some(PathRepr::of(&w("C:\\d"))),
            conflicts: JConflicts::Ask,
            items: 2,
            started: 4,
        };
        // Item 0 started (ours, group R); item 1 never started, and its destination holds a file.
        let records = vec![rec(0, "a", St::Queued), rec(1, "b", St::Queued), rec(0, "a", St::Started)];
        let j = Journal { header, records, end: None };
        let disk = Disk(
            [("C:\\s\\a", (5, 1, false)), ("C:\\s\\b", (5, 1, false)), ("C:\\d\\b", (9, 2, false))]
                .into_iter()
                .map(|(p, s)| (w(p).units().to_vec(), s))
                .collect(),
        );
        let (plan, missing) = plan(OpId(9), &j, &[], &disk).unwrap();
        assert!(missing.is_empty());
        assert_eq!(plan.len(), 2);
        assert_eq!((plan[0].conflicts, plan[0].pre.as_slice()), (Conflicts::ReplaceAll, &[Pre::Absent][..]));
        assert_eq!(plan[1].conflicts, Conflicts::AskSystem);
        assert_eq!(plan[1].items, vec![w("C:\\s\\b")]);
        assert!(plan[1].pre.is_empty(), "group A: the runner's own pre-flight, never the queued placeholder");
    }

    #[test]
    fn never_replaces_preexisting() {
        let stats = [None, F, F2, D];
        for kind in [OpKind::Copy, OpKind::Move] {
            for pre in [Pre::File, Pre::Dir, Pre::Reparse] {
                for st in [St::Queued, St::Started, St::Done, St::Failed, St::Skipped] {
                    for src in stats {
                        for dst in stats {
                            assert_ne!(decide(kind, pre, st, src, dst), R, "{kind:?} {pre:?} {st:?} {src:?} {dst:?}");
                        }
                    }
                }
            }
        }
    }
}
