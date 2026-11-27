//! Undo (OP4): the check against the disk and the inverse operations, through the runner inside a TempRoot (plain).

mod common;
mod ops_util;

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::temp::{fence, fnv1a64, TempRoot};
use files_engine::ops::binrec::{bin_record, forget_record, Forgot};
use files_engine::ops::consts::CHUNK_ITEMS;
use files_engine::ops::runner::{run, RunEnd, Work};
use files_engine::ops::sink::attrs;
use files_engine::ops::undo::{check, plan, OsFs};
use files_engine::ops_client::UndoRecord;
use oled_trust::frame::files::{
    ChangeKind, Conflicts, OpEvt, OpId, OpKind, OpResult, Refusal, UndoEntry, UndoKind, WidePath,
};
use ops_util::*;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{ReOpenFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_SHARE_READ};

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
    let total = 2 * CHUNK_ITEMS + 22;
    let entries: Vec<UndoEntry> = (0..total)
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
        assert!(p.iter().all(|c| c.items.len() <= CHUNK_ITEMS && c.op == OpId(2)), "{kind:?}: chunks of CHUNK_ITEMS");
        let n: usize = p.iter().map(|c| c.items.len()).sum();
        assert_eq!(n, if kind == UndoKind::OpenRecycleBin { 0 } else { total }, "{kind:?}");
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
    // A restore reads the bin first: an item that is no longer there refuses it.
    let root = TempRoot::new("undo_plan_gone");
    let gone = UndoEntry { bin: Some(wp(&root.at("$R0"))), ..entries[0].clone() };
    let rec = UndoRecord { of: OpId(1), kind: UndoKind::RestoreRecycled, replaced: 0, entries: vec![gone] };
    assert_eq!(check(&rec, &OsFs), Err((Refusal::Changed, "f0".into())), "RESTORE_FROM_BIN is true");
}

// ---- a restore and the bin's $I records (a fake bin folder inside the TempRoot, never the real Recycle Bin) ------

/// The user folder of a made-up account in every fake bin.
const SID: &str = "S-1-5-21-1000-2000-3000-1001";

/// An $I record as Windows 10 and later write it (version 2): the size, the deletion time, then the count of units of
/// the original path (its NUL included) and the units.
fn record_v2(original: &Path, size: u64) -> Vec<u8> {
    let units: Vec<u16> = original.as_os_str().encode_wide().chain([0]).collect();
    let mut b = Vec::new();
    b.extend(2u64.to_le_bytes());
    b.extend(size.to_le_bytes());
    b.extend(134_080_000_000_000_000i64.to_le_bytes());
    b.extend((units.len() as u32).to_le_bytes());
    units.iter().for_each(|u| b.extend(u.to_le_bytes()));
    b
}

/// An item recycled from `original` into the fake bin of the volume folder `vol`: $R<id> holds `bytes`, $I<id> names
/// `original`. Their paths relative to the root.
fn binned(root: &TempRoot, vol: &str, id: &str, original: &str, bytes: &[u8]) -> (String, String) {
    let dir = format!("{vol}/$Recycle.Bin/{SID}");
    let (r, i) = (format!("{dir}/$R{id}"), format!("{dir}/$I{id}"));
    root.file(&r, bytes);
    root.file(&i, &record_v2(&root.at(original), bytes.len() as u64));
    (r, i)
}

/// The undo record of the recycle of binned items (each its $R and its original path), checked as the host checks it.
fn restore_record(root: &TempRoot, op: u32, items: &[(&str, &str)]) -> UndoRecord {
    let entries = items
        .iter()
        .map(|(bin, original)| {
            let bin = wp(&root.at(bin));
            let (size, mtime, dir) = attrs(&bin).expect("the binned item exists");
            UndoEntry { from: wp(&root.at(original)), to: bin.clone(), size, mtime, dir, bin: Some(bin) }
        })
        .collect();
    let rec = UndoRecord { of: OpId(op), kind: UndoKind::RestoreRecycled, replaced: 0, entries };
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RestoreRecycled), "the host checks before it plans");
    rec
}

/// Restores binned items (each its $R and its original path) as the undo of their recycle does, through the runner.
fn restore(root: &TempRoot, op: u32, items: &[(&str, &str)]) -> (RunEnd, Vec<OpEvt>) {
    run_plan(op + 1, plan(OpId(op + 1), &restore_record(root, op, items)), &[root])
}

/// The file `rel` exists and its bytes hash to `h`.
fn holds(root: &TempRoot, rel: &str, h: u64) -> bool {
    root.at(rel).is_file() && root.hash(rel) == h
}

#[test]
fn restore_removes_its_own_record_only() {
    // Two items recycled from the same path: each $R pairs with the $I of its own id, so the restore of one leaves the
    // other's record alone although both name the same original.
    let root = TempRoot::new("undo_bin_own");
    root.dir("vol/docs");
    let (ra, ia) = binned(&root, "vol", "AB12CD.txt", "vol/docs/a.txt", b"first");
    let (rb, ib) = binned(&root, "vol", "EF34GH.txt", "vol/docs/a.txt", b"second");
    let other = (root.hash(&rb), root.hash(&ib));
    let (end, evs) = restore(&root, 40, &[(&ra, "vol/docs/a.txt")]);
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, 1, 0), "{evs:?}");
    assert_eq!(root.hash("vol/docs/a.txt"), fnv1a64(b"first"));
    assert!(!root.at(&ra).exists() && !root.at(&ia).exists(), "the item and its record left the bin");
    assert_eq!((root.hash(&rb), root.hash(&ib)), other, "the other item and its record are untouched");
}

#[test]
fn restore_keeps_records_that_are_not_its_own() {
    // A record of the paired name that names another path, and one that is no record at all, stay as they are.
    let root = TempRoot::new("undo_bin_other");
    root.dir("vol/docs");
    let (rc, ic) = binned(&root, "vol", "CC0001.txt", "vol/elsewhere/c.txt", b"c");
    let (re, ie) = binned(&root, "vol", "EE0002.txt", "vol/docs/e.txt", b"e");
    root.write(&ie, b"not a record of any version");
    let before = (root.hash(&ic), root.hash(&ie));
    let (end, evs) = restore(&root, 42, &[(&rc, "vol/docs/c.txt"), (&re, "vol/docs/e.txt")]);
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, 2, 0), "{evs:?}");
    assert!(root.at("vol/docs/c.txt").exists() && root.at("vol/docs/e.txt").exists());
    assert_eq!((root.hash(&ic), root.hash(&ie)), before);
    // An item still in the bin keeps its record, even one that names where it is asked about.
    let (rf, i_f) = binned(&root, "vol", "FF0003.txt", "vol/docs/f.txt", b"f");
    let kept = root.hash(&i_f);
    let bin = wp(&root.at(&rf));
    let rec = bin_record(&bin).expect("an $R item of a bin folder");
    let restored = [wp(&root.at("vol/docs/f.txt"))];
    assert_eq!(forget_record(&bin, &rec, &restored, &|_: &WidePath| panic!("not proven")), Forgot::Kept);
    assert_eq!(root.hash(&i_f), kept);
}

#[test]
fn restore_keeps_a_record_that_is_a_folder_or_a_link() {
    // The record is opened as itself, whatever it is, and the guard reads its handle's attributes: a folder, a junction
    // and a file symbolic link stay. Opened through, the link would reach a record that names the restored path.
    let root = TempRoot::new("undo_bin_links");
    root.dir("vol/docs");
    root.file("vol/target/keep.txt", b"keep");
    let (rg, ig) = binned(&root, "vol", "GG0004.txt", "vol/docs/g.txt", b"g");
    let (rh, ih) = binned(&root, "vol", "HH0005.txt", "vol/docs/h.txt", b"h");
    let (rl, il) = binned(&root, "vol", "LL0008.txt", "vol/docs/l.txt", b"l");
    root.remove(&ig);
    root.file(&format!("{ig}/inside.txt"), b"inside");
    root.remove(&ih);
    root.junction(&ih, &root.at("vol/target"));
    let real = root.file("vol/records/l.rec", &record_v2(&root.at("vol/docs/l.txt"), 1));
    root.remove(&il);
    let linked = root.file_symlink(&il, &real);
    let mut items = vec![(rg.as_str(), "vol/docs/g.txt"), (rh.as_str(), "vol/docs/h.txt")];
    if linked {
        items.push((rl.as_str(), "vol/docs/l.txt"));
    } else {
        eprintln!("skip: the file symbolic link case (this user may not create one)");
    }
    let (target, record) = (root.hash_tree("vol/target"), root.hash("vol/records/l.rec"));
    let (end, evs) = restore(&root, 44, &items);
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, items.len() as u32, 0), "{evs:?}");
    assert_eq!(root.hash(&format!("{ig}/inside.txt")), fnv1a64(b"inside"), "a folder $I stays");
    assert!(root.at(&ih).exists(), "a junction $I stays");
    assert_eq!(root.hash_tree("vol/target"), target, "and its target is untouched");
    if linked {
        assert_eq!(root.hash("vol/docs/l.txt"), fnv1a64(b"l"), "the item is back");
        let link = std::fs::symlink_metadata(root.at(&il));
        assert!(link.is_ok_and(|m| m.file_type().is_symlink()), "a file link $I is never removed");
        assert_eq!(root.hash("vol/records/l.rec"), record, "nor the record it points to");
    }
}

#[test]
fn restore_from_the_bin_of_another_volume() {
    // Every volume has its own bin: the restore takes the record beside its item, never the one of the same id in
    // another volume's bin, even when that one names the same path.
    let root = TempRoot::new("undo_bin_volumes");
    root.dir("d/docs");
    let (rc, ic) = binned(&root, "c", "JJ0006.txt", "d/docs/j.txt", b"c");
    let (rd, id) = binned(&root, "d", "JJ0006.txt", "d/docs/j.txt", b"d");
    let other = (root.hash(&rc), root.hash(&ic));
    let (end, evs) = restore(&root, 46, &[(&rd, "d/docs/j.txt")]);
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, 1, 0), "{evs:?}");
    assert_eq!(root.hash("d/docs/j.txt"), fnv1a64(b"d"));
    assert!(!root.at(&id).exists(), "its own record is gone");
    assert_eq!((root.hash(&rc), root.hash(&ic)), other, "the other volume's bin is untouched");
}

#[test]
fn restore_succeeds_when_its_record_cannot_be_removed() {
    let root = TempRoot::new("undo_bin_stuck");
    root.dir("vol/docs");
    let (rk, ik) = binned(&root, "vol", "KK0007.txt", "vol/docs/k.txt", b"k");
    // Another program has the record open and does not share its deletion.
    let opened = std::fs::File::open(root.at(&ik)).unwrap();
    // SAFETY: the live handle just opened; the new one is closed below.
    let held = unsafe {
        ReOpenFile(HANDLE(opened.as_raw_handle()), FILE_GENERIC_READ.0, FILE_SHARE_READ, FILE_FLAGS_AND_ATTRIBUTES(0))
    }
    .unwrap();
    let (end, evs) = restore(&root, 48, &[(&rk, "vol/docs/k.txt")]);
    // SAFETY: the handle ReOpenFile returned, closed once.
    unsafe { CloseHandle(held) }.unwrap();
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, 1, 1), "{evs:?}");
    assert_eq!(root.hash("vol/docs/k.txt"), fnv1a64(b"k"), "the item is back");
    assert_eq!(changed(&evs, ChangeKind::Moved), 1);
    let failed: Vec<(WidePath, i32)> = evs
        .iter()
        .filter_map(|e| match e {
            OpEvt::ItemFailed { item, hr, .. } => Some((item.clone(), *hr)),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "{evs:?}");
    assert_eq!(failed[0].0, wp(&root.at(&ik)), "the record that stays is reported");
    assert!(failed[0].1 < 0, "with a failure HRESULT: {:#x}", failed[0].1);
    assert!(root.at(&ik).exists());
}

#[test]
fn restore_removes_the_record_it_checked_even_when_the_name_is_swapped() {
    // Between the proof and the removal another program renames the item's record away and moves another item's
    // record (same original path) into its name. The removal must still take the record that was checked: the other
    // item keeps its own, whatever the name holds by then. A record that is not the item's own never reaches it.
    let root = TempRoot::new("undo_bin_swap");
    root.dir("vol/docs");
    let (ra, ia) = binned(&root, "vol", "AA0009.txt", "vol/docs/a.txt", b"a");
    let (rb, ib) = binned(&root, "vol", "BB0010.txt", "vol/docs/a.txt", b"b");
    let (rc, ic) = binned(&root, "vol", "CC0011.txt", "vol/elsewhere/c.txt", b"c");
    let aside = "vol/aside.bin";
    let (own, other, not_own) = (root.hash(&ia), root.hash(&ib), root.hash(&ic));
    let (path, from_a, from_b) = (root.path().to_path_buf(), ia.clone(), ib.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (mut c, rx) = ctx(&[&root]);
    let saw = seen.clone();
    c.hook_record = Some(Arc::new(move |r: &WidePath| {
        saw.lock().unwrap().push(r.clone());
        if fence::try_rename(&path, &from_a, aside).is_ok() {
            let _ = fence::try_rename(&path, &from_b, &from_a);
        }
    }));
    let rec = restore_record(&root, 50, &[(&ra, "vol/docs/a.txt"), (&rc, "vol/docs/c.txt")]);
    let end = run(OpId(51), Work::Planned(OpKind::Undo, plan(OpId(51), &rec)), &c);
    let evs: Vec<OpEvt> = rx.try_iter().collect();
    assert_eq!((end.result, end.items, end.failed), (OpResult::Done, 2, 0), "{evs:?}");
    assert_eq!(root.hash("vol/docs/a.txt"), fnv1a64(b"a"));
    assert_eq!(*seen.lock().unwrap(), [wp(&root.at(&ia))], "only the proven record reaches the removal");
    assert!(holds(&root, &ib, other) || holds(&root, &ia, other), "the other item's record is never removed");
    assert!(!holds(&root, &ia, own) && !holds(&root, aside, own), "the record that was checked is gone");
    assert!(root.at(&rb).exists() && holds(&root, &ic, not_own), "the other item and the foreign record stay");
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
        let bin = rec.entries[0].bin.clone().expect("the item went to the bin");
        let record = files_engine::ops::binrec::bin_record(&bin).expect("an $R item of a bin folder");
        let record = std::path::PathBuf::from(record.to_string_lossy());
        assert!(record.exists(), "the bin holds the item's $I record");
        // The fence also admits the one bin folder the item went to, so the restore can take it from there.
        let (mut c, rx) = ctx(&[&root]);
        c.roots.get_or_insert_with(Vec::new).extend(bin.parent());
        let end = files_engine::ops::runner::run(OpId(35), Work::Planned(OpKind::Undo, plan(OpId(35), &rec)), &c);
        let evs: Vec<OpEvt> = rx.try_iter().collect();
        assert_eq!((end.result, end.failed), (OpResult::Done, 0), "{evs:?}");
        assert_eq!(root.hash("a.txt"), common::temp::fnv1a64(b"a"));
        assert!(!record.exists(), "the $I record left the bin with its item");
    } else {
        assert_eq!(finished_undo(&evs), Some(UndoKind::OpenRecycleBin));
        assert_eq!(check(&rec, &OsFs), Ok(UndoKind::OpenRecycleBin), "FS-3 g not green: the bin opens");
    }
}
