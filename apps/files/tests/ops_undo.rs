//! Undo (OP4): the check against the disk and the inverse operations, through the runner inside a TempRoot (plain).

mod common;
mod ops_util;

use common::temp::TempRoot;
use files_engine::ops::runner::Work;
use files_engine::ops::undo::{check, plan, OsFs};
use files_engine::ops_client::UndoRecord;
use oled_trust::frame::files::{ChangeKind, Conflicts, OpId, OpKind, OpResult, Refusal, UndoEntry, UndoKind, WidePath};
use ops_util::*;

#[test]
fn check_refuses_replaced() {
    let e = UndoEntry {
        from: wp(std::path::Path::new("C:\\a\\x")),
        to: wp(std::path::Path::new("C:\\b\\x")),
        size: 1,
        mtime: 1,
        dir: false,
        bin: None,
    };
    let rec = UndoRecord { of: OpId(1), kind: UndoKind::RecycleCopies, replaced: 2, entries: vec![e] };
    assert_eq!(check(&rec, &OsFs), Err((Refusal::Replaced, "2".into())), "n items replaced older files");
}

#[test]
fn check_refuses_changed() {
    let root = TempRoot::new("undo_changed");
    let items = vec![wp(&root.file("src/a.txt", b"a")), wp(&root.file("src/b.txt", b"b"))];
    let (_, evs) = run_in(20, job(OpKind::Copy, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &[&root]);
    let rec = record(20, &evs);
    assert_eq!(rec.kind, UndoKind::RecycleCopies);
    assert_eq!(rec.entries.len(), 2);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RecycleCopies), "unchanged copies can be undone");
    root.touch("dst/b.txt", 130_000_000_000_000_000);
    assert_eq!(check(&rec, &OsFs), Err((Refusal::Changed, "b.txt".into())));
    root.remove("dst/b.txt");
    assert_eq!(check(&rec, &OsFs), Err((Refusal::Changed, "b.txt".into())), "a missing copy");
}

#[test]
fn check_refuses_nonempty_folder() {
    let root = TempRoot::new("undo_nonempty");
    let w = Work::Staged(files_engine::ops::runner::OpJob {
        kind: OpKind::NewFolder,
        dest: Some(wp(root.path())),
        name: Some("New folder".into()),
        conflicts: Conflicts::AskSystem,
        staged: files_engine::ops::journal::StagedItems::from_vec(Vec::new()),
    });
    let (_, evs) = run_in(21, w, &[&root]);
    let rec = record(21, &evs);
    assert_eq!(rec.kind, UndoKind::RecycleNewFolder);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RecycleNewFolder));
    root.file("New folder/inside.txt", b"user data");
    assert_eq!(check(&rec, &OsFs), Err((Refusal::NotEmpty, "New folder".into())));
}

#[test]
fn undo_rename_back() {
    let root = TempRoot::new("undo_rename");
    let a = wp(&root.file("a.txt", b"a"));
    let (_, evs) = run_in(22, rename_job(a, "b.txt"), &[&root]);
    let rec = record(22, &evs);
    assert_eq!(rec.kind, UndoKind::RenameBack);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RenameBack));
    let p = plan(OpId(23), &rec);
    assert!(p.iter().all(|c| c.kind == OpKind::Rename));
    let (end, evs) = run_plan(23, p, &[&root]);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(root.at("a.txt").exists() && !root.at("b.txt").exists());
    assert_eq!(root.hash("a.txt"), common::temp::fnv1a64(b"a"));
    assert_eq!(changed(&evs, ChangeKind::Renamed), 1);
    assert_eq!(finished_undo(&evs), None, "an undo is not undone");
}

#[test]
fn undo_move_back() {
    let root = TempRoot::new("undo_move");
    root.file("src/sub/b.txt", b"b");
    let items = vec![wp(&root.file("src/a.txt", b"a")), wp(&root.at("src/sub"))];
    let before = root.hash_tree("src");
    let (_, evs) = run_in(24, job(OpKind::Move, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &[&root]);
    assert_eq!(result(&evs), OpResult::Done, "{evs:?}");
    assert!(!root.at("src/a.txt").exists());
    let rec = record(24, &evs);
    assert_eq!(rec.kind, UndoKind::MoveBack);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::MoveBack));
    let p = plan(OpId(25), &rec);
    assert!(
        p.iter().all(|c| c.kind == OpKind::Move && c.conflicts == Conflicts::AskSystem),
        "nothing is overwritten silently"
    );
    let (end, evs) = run_plan(25, p, &[&root]);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(root.hash_tree("src"), before, "every item is back with its bytes");
    assert_eq!(changed(&evs, ChangeKind::Moved), 2);
}

#[test]
fn plan_never_deletes_permanently() {
    let path = |s: &str| WidePath::parse(s).unwrap();
    let entries: Vec<UndoEntry> = (0..150)
        .map(|i| UndoEntry {
            from: path(&format!("C:\\src{}\\f{i}", i % 3)),
            to: path(&format!("C:\\dst\\f{i}")),
            size: 1,
            mtime: 2,
            dir: false,
            bin: Some(path(&format!("C:\\$Recycle.Bin\\S-1-5-21-1\\$R{i}"))),
        })
        .collect();
    for kind in [
        UndoKind::RenameBack,
        UndoKind::MoveBack,
        UndoKind::RecycleCopies,
        UndoKind::RecycleNewFolder,
        UndoKind::RestoreRecycled,
        UndoKind::OpenRecycleBin,
    ] {
        let rec = UndoRecord { of: OpId(1), kind, replaced: 0, entries: entries.clone() };
        let p = plan(OpId(2), &rec);
        assert!(p.iter().all(|c| c.kind != OpKind::DeletePermanent), "{kind:?}");
        assert!(p.iter().all(|c| c.items.len() <= 64 && c.op == OpId(2)), "{kind:?}: chunks of at most CHUNK_ITEMS");
        let n: usize = p.iter().map(|c| c.items.len()).sum();
        assert_eq!(n, if kind == UndoKind::OpenRecycleBin { 0 } else { 150 }, "{kind:?}");
        match kind {
            UndoKind::RecycleCopies | UndoKind::RecycleNewFolder => {
                assert!(p.iter().all(|c| c.kind == OpKind::Recycle))
            }
            UndoKind::MoveBack | UndoKind::RestoreRecycled => {
                assert!(p.iter().all(|c| c.kind == OpKind::Move && c.names.len() == c.items.len()));
                assert_eq!(
                    p.iter().filter_map(|c| c.dest.clone()).collect::<std::collections::HashSet<_>>().len(),
                    3,
                    "one destination per parent"
                );
            }
            UndoKind::RenameBack => assert!(p.iter().all(|c| c.kind == OpKind::Rename)),
            UndoKind::OpenRecycleBin => {}
        }
    }
    let rec = UndoRecord { of: OpId(1), kind: UndoKind::RestoreRecycled, replaced: 0, entries };
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::OpenRecycleBin), "RESTORE_FROM_BIN is false: the bin opens");
}

// ---- disposable (CI files-disposable job only: these recycle) ------------------------------------------------------

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_undo_copy_recycles_copies() {
    common::disposable();
    let root = TempRoot::new("undo_disp_copy");
    let items = vec![wp(&root.file("src/a.txt", b"a")), wp(&root.file("src/b.txt", b"b"))];
    let before = root.hash_tree("src");
    let (_, evs) = run_in(30, job(OpKind::Copy, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &[&root]);
    let rec = record(30, &evs);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RecycleCopies));
    let (end, evs) = run_plan(31, plan(OpId(31), &rec), &[&root]);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(changed(&evs, ChangeKind::Recycled), 2, "the copies went to the bin: {evs:?}");
    assert!(!root.at("dst/a.txt").exists() && !root.at("dst/b.txt").exists());
    assert_eq!(root.hash_tree("src"), before, "the sources are untouched");
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_undo_new_folder() {
    common::disposable();
    let root = TempRoot::new("undo_disp_folder");
    let w = Work::Staged(files_engine::ops::runner::OpJob {
        kind: OpKind::NewFolder,
        dest: Some(wp(root.path())),
        name: Some("New folder".into()),
        conflicts: Conflicts::AskSystem,
        staged: files_engine::ops::journal::StagedItems::from_vec(Vec::new()),
    });
    let (_, evs) = run_in(32, w, &[&root]);
    let rec = record(32, &evs);
    let (end, evs) = run_plan(33, plan(OpId(33), &rec), &[&root]);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(changed(&evs, ChangeKind::Recycled), 1);
    assert!(!root.at("New folder").exists());
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_undo_recycle_restores() {
    common::disposable();
    let root = TempRoot::new("undo_disp_recycle");
    let a = wp(&root.file("a.txt", b"a"));
    let (_, evs) = run_in(34, job(OpKind::Recycle, vec![a], None, Conflicts::AskSystem), &[&root]);
    assert_eq!(result(&evs), OpResult::Done, "{evs:?}");
    let entries = undo_entries(&evs);
    let rec = UndoRecord { of: OpId(34), kind: UndoKind::RestoreRecycled, replaced: 0, entries };
    if files_engine::ops::consts::RESTORE_FROM_BIN {
        assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RestoreRecycled));
        let (end, _) = run_plan(35, plan(OpId(35), &rec), &[&root]);
        assert_eq!(end.result, OpResult::Done);
        assert_eq!(root.hash("a.txt"), common::temp::fnv1a64(b"a"));
    } else {
        assert_eq!(finished_undo(&evs), Some(UndoKind::OpenRecycleBin));
        assert_eq!(check(&rec, &OsFs), Ok(UndoKind::OpenRecycleBin), "FS-3 g not green: the bin opens");
    }
}
