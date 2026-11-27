//! The runner through IFileOperation inside a TempRoot (OP3): plain tests, run in the test process on the test's own
//! thread, with FOF_NOERRORUI (test_no_ui) and never a collision under AskSystem (rule D4), and FILES_TEST_ROOT's
//! fence passed in as the context's roots.

mod common;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use common::temp::TempRoot;
use files_engine::ops::checks::check_item;
use files_engine::ops::flags::op_flags;
use files_engine::ops::journal::{Pre, StagedItems};
use files_engine::ops::preflight::{preflight, skip_present};
use files_engine::ops::runner::{run, OpJob, RunCtx, RunEnd, Work};
use files_engine::ops::sink::attrs;
use oled_trust::frame::files::{
    ChangeKind, Conflicts, OpEvt, OpId, OpKind, OpResult, Refusal, UndoEntry, UndoKind, WidePath,
};

fn wp(p: &Path) -> WidePath {
    WidePath::parse(&p.display().to_string()).unwrap()
}

/// A test context: the roots as the only fence, no system UI.
fn ctx(roots: &[&TempRoot]) -> (RunCtx, Receiver<OpEvt>) {
    let (tx, rx) = channel();
    let mut c = RunCtx::new(tx);
    c.roots = Some(roots.iter().map(|r| wp(r.path())).collect());
    c.test_no_ui = true;
    (c, rx)
}

fn job(kind: OpKind, items: Vec<WidePath>, dest: Option<WidePath>, conflicts: Conflicts) -> Work {
    Work::Staged(OpJob { kind, dest, name: None, conflicts, staged: StagedItems::from_vec(items) })
}

fn changes(evs: &[OpEvt]) -> Vec<(ChangeKind, Option<WidePath>, Option<WidePath>)> {
    evs.iter()
        .filter_map(|e| match e {
            OpEvt::ItemsDone { changes, .. } => Some(changes.clone()),
            _ => None,
        })
        .flatten()
        .map(|c| (c.kind, c.from, c.to))
        .collect()
}

fn undo_entries(evs: &[OpEvt]) -> Vec<UndoEntry> {
    evs.iter()
        .filter_map(|e| match e {
            OpEvt::UndoPart { entries, .. } => Some(entries.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn finished(evs: &[OpEvt]) -> (OpResult, Option<UndoKind>, u32, u32, u32) {
    match evs.last() {
        Some(OpEvt::Finished { result, undo, replaced, items, failed, .. }) => {
            (*result, *undo, *replaced, *items, *failed)
        }
        other => panic!("the last event is not Finished: {other:?}"),
    }
}

fn run_all(op: u32, work: Work, c: &RunCtx, rx: &Receiver<OpEvt>) -> (RunEnd, Vec<OpEvt>) {
    let end = run(OpId(op), work, c);
    let evs: Vec<OpEvt> = rx.try_iter().collect();
    assert!(matches!(evs.first(), Some(OpEvt::Started { .. })), "Started first: {:?}", evs.first());
    (end, evs)
}

fn make_files(root: &TempRoot, dir: &str, n: u32) -> Vec<WidePath> {
    (0..n).map(|i| wp(&root.file(&format!("{dir}/f{i:03}.bin"), format!("content {i}").as_bytes()))).collect()
}

#[test]
fn copy_three_files() {
    let root = TempRoot::new("ops_copy3");
    let items: Vec<WidePath> =
        ["a.txt", "b.txt", "c.txt"].iter().map(|n| wp(&root.file(&format!("src/{n}"), n.as_bytes()))).collect();
    let dst = wp(&root.dir("dst"));
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(1, job(OpKind::Copy, items.clone(), Some(dst.clone()), Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(root.hash(&format!("dst/{n}")), root.hash(&format!("src/{n}")));
    }
    let ch = changes(&evs);
    assert_eq!(ch.len(), 3, "{ch:?}");
    assert!(ch.iter().all(|(k, f, t)| *k == ChangeKind::Copied && f.is_some() && t.is_some()));
    let undo = undo_entries(&evs);
    assert_eq!(undo.len(), 3);
    for e in &undo {
        let (size, mtime, dir) = attrs(&e.to).unwrap();
        assert_eq!((e.size, e.mtime, e.dir), (size, mtime, dir), "size and mtime of the copy");
        assert!(e.to.parent().unwrap() == dst);
    }
    assert_eq!(finished(&evs), (OpResult::Done, Some(UndoKind::RecycleCopies), 0, 3, 0));
}

/// The 8.3 form of `p` (GetShortPathNameW); None when the volume keeps no short name for it.
fn short_form(p: &Path) -> Option<WidePath> {
    use std::os::windows::ffi::OsStrExt;
    let long: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    let mut buf = vec![0u16; long.len()];
    // SAFETY: `long` is NUL-terminated and lives for the call; the buffer is writable for its whole length (a short
    // form is never longer than the long one).
    let n = unsafe {
        windows::Win32::Storage::FileSystem::GetShortPathNameW(windows::core::PCWSTR(long.as_ptr()), Some(&mut buf))
    } as usize;
    let short = WidePath::new(buf.get(..n)?).ok()?;
    (n > 0 && short != wp(p)).then_some(short)
}

#[test]
fn short_names_are_top_level_items() {
    // Items, destination and fence given with 8.3 components, as on the CI runner (%TEMP% under C:\Users\RUNNER~1):
    // the shell reports its items by their long names, and they are still the operation's top-level items.
    let root = TempRoot::new("ops_short_names");
    let long: Vec<_> = ["first item.txt", "second item.txt"]
        .iter()
        .map(|n| root.file(&format!("source folder/{n}"), n.as_bytes()))
        .collect();
    let dst = root.dir("target folder");
    let short = |p: &Path| short_form(p).unwrap_or_else(|| wp(p));
    let items: Vec<WidePath> = long.iter().map(|p| short(p)).collect();
    if items.iter().all(|i| files_engine::ops::sink::long_form(i).is_none()) {
        println!("not applicable: no 8.3 component on the temp volume's paths");
        return;
    }
    let (mut c, rx) = ctx(&[&root]);
    c.roots = Some(vec![short(root.path())]);
    let (end, evs) = run_all(70, job(OpKind::Copy, items.clone(), Some(short(&dst)), Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    let ch = changes(&evs);
    assert_eq!(ch.len(), 2, "one change per top-level item: {ch:?}");
    assert!(ch.iter().all(|(k, f, _)| *k == ChangeKind::Copied && f.as_ref().is_some_and(|f| items.contains(f))));
    assert_eq!(undo_entries(&evs).len(), 2);
    assert_eq!(finished(&evs), (OpResult::Done, Some(UndoKind::RecycleCopies), 0, 2, 0));
}

#[test]
fn recycle_leaves_an_item_whose_bin_is_off() {
    // FS-3 c4 (the disposable job): on a volume whose bin the user turned off, the shell deletes a Recycle for good
    // without a dialog. The runner leaves such an item where it is and reports it failed, before the shell sees it.
    let root = TempRoot::new("ops_bin_off");
    let a = wp(&root.file("a.txt", b"a"));
    let (mut c, rx) = ctx(&[&root]);
    c.bin_off = |_| true;
    let (end, evs) = run_all(71, job(OpKind::Recycle, vec![a.clone()], None, Conflicts::AskSystem), &c, &rx);
    assert_eq!(root.hash("a.txt"), common::temp::fnv1a64(b"a"), "the item is where it was");
    let off = files_engine::ops::runner::RECYCLE_BIN_OFF;
    assert!(evs.iter().any(|e| matches!(e, OpEvt::ItemFailed { item, hr, .. } if *item == a && *hr == off)), "{evs:?}");
    assert_eq!(end.result, OpResult::Failed);
    assert_eq!(c.stats.performs.load(Ordering::Relaxed), 0, "nothing reached the shell");
    let (_, undo, _, items, failed) = finished(&evs);
    assert_eq!((undo, items, failed), (None, 0, 1));
}

#[test]
fn copy_folder_reports_top_level_only() {
    let root = TempRoot::new("ops_copydir");
    for i in 0..3 {
        root.file(&format!("src/f/{i}.txt"), &vec![b'x'; 1000 * (i + 1)]);
    }
    for i in 0..2 {
        root.file(&format!("src/f/sub/{i}.txt"), b"nested");
    }
    let (c, rx) = ctx(&[&root]);
    let w = job(OpKind::Copy, vec![wp(&root.at("src/f"))], Some(wp(&root.dir("dst"))), Conflicts::AskSystem);
    let (end, evs) = run_all(2, w, &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(root.hash_tree("src/f").len(), root.hash_tree("dst/f").len());
    let ch = changes(&evs);
    assert_eq!(ch.len(), 1, "one change, for the folder: {ch:?}");
    assert_eq!(ch[0].1.as_ref(), Some(&wp(&root.at("src/f"))));
    let undo = undo_entries(&evs);
    assert_eq!(undo.len(), 1);
    assert!(undo[0].dir);
    let progress: Vec<(u64, u64)> = evs
        .iter()
        .filter_map(|e| match e {
            OpEvt::Progress { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert!(progress.iter().all(|(d, t)| d <= t), "{progress:?}");
    if let Some((d, t)) = progress.last() {
        assert_eq!(d, t, "the last Progress reaches its total: {progress:?}");
    }
}

#[test]
fn chunks_of_64() {
    let root = TempRoot::new("ops_chunks");
    let items = make_files(&root, "src", 150);
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(3, job(OpKind::Copy, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Done);
    assert_eq!(c.stats.performs.load(Ordering::Relaxed), 3, "64 + 64 + 22");
    assert_eq!(c.stats.created.load(Ordering::Relaxed), 3);
    for i in 0..150 {
        assert_eq!(root.hash(&format!("dst/f{i:03}.bin")), root.hash(&format!("src/f{i:03}.bin")));
    }
    assert_eq!(changes(&evs).len(), 150);
    assert_eq!(finished(&evs).3, 150);
}

#[test]
fn cancel_between_chunks() {
    let root = TempRoot::new("ops_cancel");
    let items = make_files(&root, "src", 150);
    let before = root.hash_tree("src");
    let (mut c, rx) = ctx(&[&root]);
    let cancel = c.cancel.clone();
    c.hook_post = Some(Arc::new(move |_, i| {
        if i == 9 {
            cancel.store(true, Ordering::Release);
        }
    }));
    let (end, evs) = run_all(4, job(OpKind::Copy, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Cancelled, "{:?}", evs.last());
    let copied = root.hash_tree("dst").len();
    assert!((10..=64).contains(&copied), "at most one chunk reached the destination: {copied}");
    assert_eq!(root.hash_tree("src"), before, "nothing at the source is lost");
    assert_eq!(c.stats.performs.load(Ordering::Relaxed), 1, "no further chunk");
    assert_eq!(finished(&evs).0, OpResult::Cancelled);
}

#[test]
fn move_within_volume() {
    let root = TempRoot::new("ops_move");
    let items = make_files(&root, "src", 2);
    let hashes: Vec<u64> = (0..2).map(|i| root.hash(&format!("src/f{i:03}.bin"))).collect();
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(5, job(OpKind::Move, items, Some(wp(&root.dir("dst"))), Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(root.hash(&format!("dst/f{i:03}.bin")), *h);
        assert!(!root.at(&format!("src/f{i:03}.bin")).exists());
    }
    let ch = changes(&evs);
    assert_eq!(ch.len(), 2);
    assert!(ch.iter().all(|(k, _, _)| *k == ChangeKind::Moved));
    assert_eq!(finished(&evs).1, Some(UndoKind::MoveBack));
}

#[test]
fn rename_one() {
    let root = TempRoot::new("ops_rename");
    let a = wp(&root.file("a.txt", b"a"));
    let (c, rx) = ctx(&[&root]);
    let w = Work::Staged(OpJob {
        kind: OpKind::Rename,
        dest: None,
        name: Some("b.txt".into()),
        conflicts: Conflicts::AskSystem,
        staged: StagedItems::from_vec(vec![a.clone()]),
    });
    let (end, evs) = run_all(6, w, &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!root.at("a.txt").exists());
    assert_eq!(root.hash("b.txt"), common::temp::fnv1a64(b"a"));
    assert_eq!(changes(&evs), vec![(ChangeKind::Renamed, Some(a), Some(wp(&root.at("b.txt"))))]);
    assert_eq!(finished(&evs).1, Some(UndoKind::RenameBack));
}

#[test]
fn new_folder_twice_renames() {
    let root = TempRoot::new("ops_newfolder");
    let parent = wp(root.path());
    let mut made = Vec::new();
    for op in [7, 8] {
        let (c, rx) = ctx(&[&root]);
        let w = Work::Staged(OpJob {
            kind: OpKind::NewFolder,
            dest: Some(parent.clone()),
            name: Some("New folder".into()),
            conflicts: Conflicts::AskSystem,
            staged: StagedItems::from_vec(Vec::new()),
        });
        let (end, evs) = run_all(op, w, &c, &rx);
        assert_eq!(end.result, OpResult::Done, "{evs:?}");
        let ch = changes(&evs);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].0, ChangeKind::Created);
        made.push(ch[0].2.clone().unwrap());
        assert_eq!(finished(&evs).1, Some(UndoKind::RecycleNewFolder));
    }
    assert_eq!(made[0], wp(&root.at("New folder")));
    assert_eq!(made[1], wp(&root.at("New folder (2)")), "the second Created names the real result");
    assert!(root.at("New folder (2)").is_dir());
}

#[test]
fn delete_permanent_in_temp() {
    let root = TempRoot::new("ops_delete");
    let items = make_files(&root, "src", 2);
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(9, job(OpKind::DeletePermanent, items, None, Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(root.hash_tree("src").values().all(|v| v == "dir"), "the files are gone: {:?}", root.hash_tree("src"));
    let ch = changes(&evs);
    assert_eq!(ch.len(), 2);
    assert!(ch.iter().all(|(k, _, to)| *k == ChangeKind::Deleted && to.is_none()), "{ch:?}");
    assert!(undo_entries(&evs).is_empty(), "no undo entry");
    assert_eq!(finished(&evs).1, None);
}

#[test]
fn preflight_counts_replaced() {
    let root = TempRoot::new("ops_preflight");
    let other = TempRoot::new("ops_preflight_target");
    other.file("data/x.bin", b"keep");
    let before = other.hash_tree("");
    root.file("src/j/x", b"x");
    let items = vec![wp(&root.file("src/a.txt", b"a")), wp(&root.at("src/j")), wp(&root.file("src/new.txt", b"n"))];
    root.file("dst/a.txt", b"old");
    root.junction("dst/j", &other.path().join("data"));
    let pf = preflight(OpKind::Copy, &items, &[], Some(&wp(&root.at("dst"))));
    assert_eq!(pf.pre, vec![Pre::File, Pre::Reparse, Pre::Absent]);
    assert_eq!(pf.collisions, 2, "replaced 2");
    assert_eq!(other.hash_tree(""), before, "the pre-flight reads only");
    let into_own = preflight(OpKind::Copy, &items[..1], &[], Some(&wp(&root.at("src"))));
    assert_eq!(into_own.pre, vec![Pre::Absent], "a copy into its own folder makes 'name - Copy'");
}

#[test]
fn skipall_removes_present() {
    let root = TempRoot::new("ops_skipall");
    let items = vec![wp(&root.file("src/a.txt", b"a")), wp(&root.file("src/b.txt", b"b"))];
    root.file("dst/a.txt", b"old");
    let pf = preflight(OpKind::Move, &items, &[], Some(&wp(&root.at("dst"))));
    assert_eq!(skip_present(&pf.pre), vec![1], "only b.txt is left");
}

#[test]
fn replace_all_has_no_dialog() {
    let root = TempRoot::new("ops_replace");
    let items = make_files(&root, "src", 3);
    for i in 0..3 {
        root.file(&format!("dst/f{i:03}.bin"), b"older");
    }
    let flags = op_flags(OpKind::Copy, Conflicts::ReplaceAll, 3, false, true);
    assert_ne!(flags & 4, 0, "FOF_SILENT");
    assert_ne!(flags & 16, 0, "FOF_NOCONFIRMATION");
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(10, job(OpKind::Copy, items, Some(wp(&root.at("dst"))), Conflicts::ReplaceAll), &c, &rx);
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    for i in 0..3 {
        assert_eq!(root.hash(&format!("dst/f{i:03}.bin")), root.hash(&format!("src/f{i:03}.bin")));
    }
    assert_eq!(finished(&evs).2, 3, "replaced counts the top-level collisions");
}

#[test]
fn refuses_outside_test_root() {
    let root = TempRoot::new("ops_fence");
    let n = std::process::id()
        ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos());
    let absent = std::env::temp_dir().join(format!("mod-files-absent-{n:x}")).join("x");
    assert!(!absent.exists());
    let (c, rx) = ctx(&[&root]);
    let (end, evs) = run_all(11, job(OpKind::DeletePermanent, vec![wp(&absent)], None, Conflicts::AskSystem), &c, &rx);
    assert_eq!(end.result, OpResult::Refused);
    assert!(evs.iter().any(|e| matches!(e, OpEvt::Refused { why: Refusal::OutsideTestRoot, .. })), "{evs:?}");
    assert_eq!(c.stats.created.load(Ordering::Relaxed), 0, "refused before CoCreateInstance");
    assert_eq!(finished(&evs).0, OpResult::Refused);
}

#[test]
fn refuses_root() {
    for root in ["C:\\", "\\\\srv\\share"] {
        let p = WidePath::parse(root).unwrap();
        for kind in [OpKind::Recycle, OpKind::DeletePermanent, OpKind::Move, OpKind::Rename] {
            assert_eq!(check_item(&p, kind, None), Err(Refusal::Root), "{root} {kind:?}");
        }
    }
}
