//! Data-safety fixtures (OP4b, rule D10): every operation kind and its undo over a tree that holds a junction, an
//! alternate data stream, a read-only file and a path longer than 260 units. Root A holds the tree; root B holds the
//! junction's target, hashed before and after every test: no file of a junction's target is ever written, moved or
//! deleted. Plain tests inside two TempRoots, the fence naming both.

mod common;
mod ops_util;

use std::collections::BTreeMap;

use common::temp::TempRoot;
use files_engine::ops::journal::Pre;
use files_engine::ops::preflight::preflight;
use files_engine::ops::undo::{check, plan, OsFs};
use oled_trust::frame::files::{ChangeKind, Conflicts, OpId, OpKind, OpResult, UndoKind, WidePath};
use ops_util::*;

/// Three folder names of 100 units: deep\<x>\<y>\<z>\f.txt lies more than 300 units below the root.
fn deep_rel(top: &str) -> String {
    let seg = |c: char| std::iter::repeat_n(c, 100).collect::<String>();
    format!("{top}/deep/{}/{}/{}/f.txt", seg('x'), seg('y'), seg('z'))
}

struct Fixture {
    a: TempRoot,
    b: TempRoot,
    b_before: BTreeMap<String, String>,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let a = TempRoot::new(&format!("{tag}_a"));
        let b = TempRoot::new(&format!("{tag}_b"));
        b.file("data/1.bin", &[1u8; 3000]);
        b.file("data/2.bin", &[2u8; 5000]);
        a.file("src/a.txt", b"plain");
        a.file("src/ro.txt", b"read only");
        a.set_readonly("src/ro.txt", true);
        a.file("src/s.txt", b"main stream");
        a.ads("src/s.txt", "s", &[7u8; 4096]);
        a.file(&deep_rel("src"), b"deep");
        a.junction("src/j", &b.path().join("data"));
        let b_before = b.hash_tree("");
        assert!(b_before.len() >= 3, "data, 1.bin, 2.bin: {b_before:?}");
        assert!(wp(&a.at(&deep_rel("src"))).units().len() > 300);
        Fixture { a, b, b_before }
    }

    fn roots(&self) -> [&TempRoot; 2] {
        [&self.a, &self.b]
    }

    fn at(&self, rel: &str) -> WidePath {
        wp(&self.a.at(rel))
    }

    /// Root B is byte for byte what it was.
    fn b_unchanged(&self) {
        assert_eq!(self.b.hash_tree(""), self.b_before, "the junction's target changed");
    }
}

fn readonly(t: &TempRoot, rel: &str) -> bool {
    std::fs::symlink_metadata(t.at(rel)).map(|m| m.permissions().readonly()).unwrap_or(false)
}

#[test]
fn junction_copy_leaves_target() {
    let fx = Fixture::new("safe_copy");
    let w = job(OpKind::Copy, vec![fx.at("src")], Some(wp(&fx.a.dir("dst"))), Conflicts::AskSystem);
    let (end, evs) = run_in(40, w, &fx.roots());
    assert_ne!(end.result, OpResult::Refused, "{evs:?}");
    fx.b_unchanged();
    let tree = fx.a.hash_tree("dst/src");
    // What the shell made of the junction is recorded, not asserted.
    let failed: Vec<(String, i32)> = evs
        .iter()
        .filter_map(|e| match e {
            oled_trust::frame::files::OpEvt::ItemFailed { item, hr, .. } => Some((item.to_string_lossy(), *hr)),
            _ => None,
        })
        .collect();
    println!("dst/src/j: {:?}; {end:?}; failed: {failed:?}", tree.get("j"));
    assert_eq!(fx.a.hash("dst/src/a.txt"), fx.a.hash("src/a.txt"));
}

#[test]
fn junction_move_and_undo_leave_target() {
    let fx = Fixture::new("safe_move");
    let before = fx.a.hash_tree("src");
    let w = job(OpKind::Move, vec![fx.at("src")], Some(wp(&fx.a.dir("dst"))), Conflicts::AskSystem);
    let (end, evs) = run_in(41, w, &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!fx.a.at("src").exists());
    fx.b_unchanged();
    let rec = record(41, &evs);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::MoveBack));
    let (end, evs) = run_plan(42, plan(OpId(42), &rec), &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(fx.a.hash_tree("src"), before, "src restored: junction, stream, read-only file and long path");
    fx.b_unchanged();
}

#[test]
fn rename_and_back_leave_target() {
    let fx = Fixture::new("safe_rename");
    let before = fx.a.hash_tree("src");
    let (end, evs) = run_in(56, rename_job(fx.at("src"), "src2"), &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!fx.a.at("src").exists() && fx.a.at("src2").is_dir());
    assert_eq!(fx.a.hash_tree("src2"), before, "the tree is renamed whole");
    fx.b_unchanged();
    let rec = record(56, &evs);
    assert_eq!(check(&rec, &OsFs), Ok(UndoKind::RenameBack));
    let p = plan(OpId(57), &rec);
    assert!(p.iter().all(|c| c.kind == OpKind::Rename));
    let (end, evs) = run_plan(57, p, &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!fx.a.at("src2").exists());
    assert_eq!(fx.a.hash_tree("src"), before, "src renamed back: junction, stream, read-only file and long path");
    fx.b_unchanged();
}

#[test]
fn junction_delete_permanent_leaves_target() {
    let fx = Fixture::new("safe_delete");
    let (end, evs) =
        run_in(43, job(OpKind::DeletePermanent, vec![fx.at("src")], None, Conflicts::AskSystem), &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!fx.a.at("src").exists(), "A\\src is gone");
    fx.b_unchanged();
}

#[test]
fn junction_preflight_is_read_only() {
    let fx = Fixture::new("safe_preflight");
    fx.a.dir("other");
    fx.a.junction("other/j", &fx.b.path().join("data"));
    let pf = preflight(OpKind::Copy, &[fx.at("src/j")], &[], Some(&fx.at("other")));
    assert_eq!(pf.pre, vec![Pre::Reparse]);
    fx.b_unchanged();
}

#[test]
fn ads_survives_copy_move_and_undo() {
    let fx = Fixture::new("safe_ads");
    let s = fx.a.hash_tree("src").into_iter().filter(|(k, _)| k.starts_with("s.txt")).collect::<BTreeMap<_, _>>();
    assert_eq!(s.len(), 2, "the file and its stream: {s:?}");
    let w = job(OpKind::Copy, vec![fx.at("src/s.txt")], Some(wp(&fx.a.dir("copy"))), Conflicts::AskSystem);
    assert_eq!(run_in(44, w, &fx.roots()).0.result, OpResult::Done);
    let copied = fx.a.hash_tree("copy");
    assert_eq!(copied, s, "Copy keeps stream s with equal bytes");
    let w = job(OpKind::Move, vec![fx.at("src/s.txt")], Some(wp(&fx.a.dir("moved"))), Conflicts::AskSystem);
    let (end, evs) = run_in(45, w, &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(fx.a.hash_tree("moved"), s);
    let (end, _) = run_plan(46, plan(OpId(46), &record(45, &evs)), &fx.roots());
    assert_eq!(end.result, OpResult::Done);
    let back = fx.a.hash_tree("src").into_iter().filter(|(k, _)| k.starts_with("s.txt")).collect::<BTreeMap<_, _>>();
    assert_eq!(back, s, "Move then MoveBack keeps it");
    fx.b_unchanged();
}

#[test]
fn readonly_survives() {
    let fx = Fixture::new("safe_readonly");
    let w = job(OpKind::Copy, vec![fx.at("src/ro.txt")], Some(wp(&fx.a.dir("copy"))), Conflicts::AskSystem);
    assert_eq!(run_in(47, w, &fx.roots()).0.result, OpResult::Done);
    assert_eq!(fx.a.hash("copy/ro.txt"), fx.a.hash("src/ro.txt"));
    assert!(readonly(&fx.a, "copy/ro.txt"), "the copy is read-only too");
    fx.a.file("tree/keep.txt", b"keep");
    let w = job(OpKind::Copy, vec![fx.at("src/ro.txt")], Some(wp(&fx.a.dir("tree/sub"))), Conflicts::AskSystem);
    assert_eq!(run_in(48, w, &fx.roots()).0.result, OpResult::Done);
    let (end, evs) =
        run_in(49, job(OpKind::DeletePermanent, vec![fx.at("tree/sub")], None, Conflicts::AskSystem), &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert!(!fx.a.at("tree/sub").exists(), "the tree holding ro.txt is removed (FOF_NOCONFIRMATION)");
    assert_eq!(fx.a.hash("tree/keep.txt"), common::temp::fnv1a64(b"keep"), "and nothing else");
    assert_eq!(fx.a.hash("src/ro.txt"), common::temp::fnv1a64(b"read only"));
    fx.b_unchanged();
}

#[test]
fn long_path_round_trip() {
    let fx = Fixture::new("safe_long");
    let deep = deep_rel("src");
    let h = fx.a.hash(&deep);
    let item = fx.at(&deep);
    assert!(item.units().len() > 260);
    let w = job(OpKind::Copy, vec![item.clone()], Some(wp(&fx.a.dir("copy"))), Conflicts::AskSystem);
    let (end, evs) = run_in(50, w, &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(fx.a.hash("copy/f.txt"), h, "Copy beyond 260 units");
    let w = job(OpKind::Move, vec![item.clone()], Some(wp(&fx.a.dir("moved"))), Conflicts::AskSystem);
    let (end, evs) = run_in(51, w, &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(fx.a.hash("moved/f.txt"), h, "Move out of a long path");
    let moved = changes(&evs).iter().filter(|c| c.kind == ChangeKind::Moved).count();
    assert_eq!(moved, 1, "{evs:?}; {}", forms_of(&item));
    let (end, evs) = run_plan(52, plan(OpId(52), &record(51, &evs)), &fx.roots());
    assert_eq!(end.result, OpResult::Done, "{evs:?}");
    assert_eq!(fx.a.hash(&deep), h, "MoveBack into the long path");
    fx.b_unchanged();
}

/// The forms of `item` and the shell's name for it (SIGDN_FILESYSPATH of a shell item made from its 8.3 form), for the
/// message of a long-path item that was not reported as a change.
fn forms_of(item: &WidePath) -> String {
    use files_engine::ops::sink::{item_path, long_form, short_form};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName};
    let short = short_form(item);
    // SAFETY: COM on this test thread for the one shell call below, balanced when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let shell = short.as_ref().and_then(|s| {
        let z: Vec<u16> = s.units().iter().copied().chain([0]).collect();
        // SAFETY: `z` is NUL-terminated and lives for the call; no bind context.
        let si: IShellItem = unsafe { SHCreateItemFromParsingName(windows::core::PCWSTR(z.as_ptr()), None) }.ok()?;
        item_path(&si)
    });
    if com {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    let s = |p: Option<WidePath>| p.map_or_else(|| "none".to_string(), |p| p.to_string_lossy());
    format!("long form {}, 8.3 form {}, the shell's name {}", s(long_form(item)), s(short), s(shell))
}

/// Forges the journal of an interrupted copy of \`names\` (children of A\src) into A\dst by a dead host: every item
/// queued, \`started\` started, all with pre absent.
fn forge_copy(fx: &Fixture, op: u32, names: &[&str], started: &[&str]) -> files_engine::ops::journal::Journal {
    use files_engine::ops::journal::{
        journal_rel, parse, Header, JConflicts, JKind, JournalWriter, PathRepr, Record, St, VERSION,
    };
    let data = files_engine::own_files::OwnDir::open(&fx.a.dir("data")).unwrap();
    let dead = 0xFFFF_FFF0;
    let header = Header {
        v: VERSION,
        op,
        kind: JKind::Copy,
        host_pid: dead,
        host_created: 1,
        dest: Some(PathRepr::of(&wp(&fx.a.dir("dst")))),
        conflicts: JConflicts::Ask,
        items: names.len() as u64,
        started: 1,
    };
    let now = std::time::Instant::now();
    let mut w = JournalWriter::create(&data, OpId(op), dead, &header, now).unwrap();
    let rec = |i: usize, n: &str, st: St| Record {
        i: i as u32,
        src: PathRepr::of(&fx.at(&format!("src/{n}"))),
        dst: None,
        pre: Pre::Absent,
        st,
        size: 0,
        mtime: 0,
    };
    for (i, n) in names.iter().enumerate() {
        w.record(&rec(i, n, St::Queued), now).unwrap();
    }
    for (i, n) in names.iter().enumerate().filter(|(_, n)| started.contains(n)) {
        w.record(&rec(i, n, St::Started), now).unwrap();
    }
    drop(w);
    parse(&data.read(&journal_rel(OpId(op), dead)).unwrap()).unwrap()
}

// The forged copies leave out src\deep: a nested path over 260 units does not copy in the test process (it is not
// long-path aware); host_copies_a_long_tree (ops_client.rs) copies one through the host.

#[test]
fn resume_group_r_leaves_target() {
    let fx = Fixture::new("safe_resume");
    fx.a.file("dst/j/partial.bin", b"half");
    let names = ["a.txt", "ro.txt", "s.txt", "j"];
    let j = forge_copy(&fx, 60, &names, &["j"]);
    let (plan, missing) = files_engine::ops::resume::plan(OpId(61), &j, &[], &OsFs).unwrap();
    assert!(missing.is_empty());
    assert_eq!(plan[0].conflicts, Conflicts::ReplaceAll);
    assert_eq!(plan[0].items, vec![fx.at("src/j")], "j's folder is ours: group R");
    assert!(plan[1..].iter().all(|c| c.conflicts == Conflicts::AskSystem));
    let (end, evs) = run_in(61, files_engine::ops::runner::Work::Planned(OpKind::Resume, plan), &fx.roots());
    assert_ne!(end.result, OpResult::Refused, "{evs:?}");
    fx.b_unchanged();
    for n in ["a.txt", "ro.txt", "s.txt"] {
        assert_eq!(fx.a.hash(&format!("dst/{n}")), fx.a.hash(&format!("src/{n}")), "{n}");
    }
    assert!(fx.a.at("dst/j").is_dir(), "dst is complete");
}

#[test]
fn readonly_partial_in_group_r() {
    let fx = Fixture::new("safe_resume_ro");
    fx.a.file("dst/ro.txt", b"rea");
    fx.a.set_readonly("dst/ro.txt", true);
    let src_before = fx.a.hash_tree("src");
    let j = forge_copy(&fx, 62, &["ro.txt"], &["ro.txt"]);
    let (plan, _) = files_engine::ops::resume::plan(OpId(63), &j, &[], &OsFs).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].conflicts, Conflicts::ReplaceAll);
    let (_, evs) = run_in(63, files_engine::ops::runner::Work::Planned(OpKind::Resume, plan), &fx.roots());
    let equal = fx.a.hash("dst/ro.txt") == fx.a.hash("src/ro.txt");
    let named = evs.iter().any(|e| {
        matches!(e, oled_trust::frame::files::OpEvt::ItemFailed { item, .. } if item.file_name() == Some(&"ro.txt".encode_utf16().collect::<Vec<_>>()[..]))
    });
    assert!(equal || named, "either the copy ends equal to the source or an ItemFailed names it: {evs:?}");
    assert_eq!(fx.a.hash_tree("src"), src_before, "the source is unchanged");
    fx.b_unchanged();
}

/// A resumed item of group A journals the runner's own pre-flight, never the "queued" placeholder: a user's file at
/// its destination keeps it out of group R when the resumed operation is interrupted after its Pre callback and
/// resumed again.
#[test]
fn resume_group_a_never_marks_a_user_file_ours() {
    use files_engine::ops::journal::{journal_rel, parse, St};
    use files_engine::ops::runner::{run, JournalCtx, Prepared, Work};
    use std::sync::atomic::Ordering;
    let fx = Fixture::new("safe_resume_a");
    fx.a.file("dst/s.txt", b"the user's own file");
    let user = fx.a.hash("dst/s.txt");
    let j = forge_copy(&fx, 64, &["a.txt", "s.txt"], &[]);
    let (plan, _) = files_engine::ops::resume::plan(OpId(65), &j, &[], &OsFs).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].conflicts, Conflicts::AskSystem, "never started: group A");
    // The run stops at s.txt's Pre: the cancel flag, set after a.txt's Post, makes the sink journal s.txt started and
    // refuse it (FS-3 e1: the item is not copied). ReplaceAll instead of the plan's AskSystem keeps the colliding
    // chunk free of system dialogs (D4); what the runner journals as the pre-flight does not depend on it.
    let chunk = Prepared { conflicts: Conflicts::ReplaceAll, ..plan[0].clone() };
    let (mut c, _rx) = ctx(&fx.roots());
    let data = files_engine::own_files::OwnDir::open(&fx.a.dir("data")).unwrap();
    let pid = 4_242;
    c.journal = Some(JournalCtx { dir: data.clone(), pid, created: 1 });
    let cancel = c.cancel.clone();
    c.hook_post = Some(std::sync::Arc::new(move |_, i| {
        if i == 0 {
            cancel.store(true, Ordering::Release);
        }
    }));
    let end = run(OpId(65), Work::Planned(OpKind::Resume, vec![chunk]), &c);
    assert_eq!(end.result, OpResult::Cancelled, "{end:?}");
    assert_eq!(fx.a.hash("dst/s.txt"), user, "the user's file is untouched");
    assert_eq!(fx.a.hash("dst/a.txt"), fx.a.hash("src/a.txt"));
    // The resumed operation's journal as an interruption right after s.txt's Pre callback leaves it.
    let mut again = parse(&data.read(&journal_rel(OpId(65), pid)).unwrap()).unwrap();
    let cut = again.records.iter().position(|r| r.i == 1 && r.st == St::Started).expect("s.txt was started") + 1;
    again.records.truncate(cut);
    again.end = None;
    assert_eq!(again.records[cut - 1].pre, Pre::File, "the resume journals its own pre-flight");
    let (plan, _) = files_engine::ops::resume::plan(OpId(66), &again, &[], &OsFs).unwrap();
    let s = fx.at("src/s.txt");
    assert!(plan.iter().any(|c| c.items.contains(&s)), "s.txt is resumed again: {plan:?}");
    assert!(
        plan.iter().filter(|c| c.conflicts == Conflicts::ReplaceAll).all(|c| !c.items.contains(&s)),
        "a user's file never enters group R: {plan:?}"
    );
    fx.b_unchanged();
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_safety_recycle() {
    common::disposable();
    let fx = Fixture::new("safe_recycle");
    let w = job(OpKind::Copy, vec![fx.at("src")], Some(wp(&fx.a.dir("copy"))), Conflicts::AskSystem);
    let (_, copy_evs) = run_in(53, w, &fx.roots());
    let items: Vec<WidePath> =
        ["a.txt", "ro.txt", "s.txt", "deep", "j"].iter().map(|n| fx.at(&format!("src/{n}"))).collect();
    let (end, evs) = run_in(54, job(OpKind::Recycle, items.clone(), None, Conflicts::AskSystem), &fx.roots());
    assert_ne!(end.result, OpResult::Refused, "{evs:?}");
    fx.b_unchanged();
    let recycled = changes(&evs);
    for item in &items {
        let in_bin = recycled.iter().any(|c| c.kind == ChangeKind::Recycled && c.from.as_ref() == Some(item));
        let at_source = std::path::Path::new(&item.to_string_lossy()).exists();
        assert!(in_bin || at_source, "{} is neither in the bin nor at its source", item.to_string_lossy());
    }
    let (end, evs) = run_plan(55, plan(OpId(55), &record(53, &copy_evs)), &fx.roots());
    assert_ne!(end.result, OpResult::Refused, "{evs:?}");
    fx.b_unchanged();
}
