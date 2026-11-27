//! The TempRoot fence (plan rule D2): plain tests inside TempRoots only. LiveRoot's refusals: tests/live_root.rs.

mod common;

use std::panic::{catch_unwind, AssertUnwindSafe};

use common::temp::{artifact, fnv1a64, TempRoot};

#[test]
fn forged_path_drop_panics_without_deleting() {
    let real = TempRoot::new("forged_real");
    let keep = real.file("sub/keep.txt", b"keep");
    let forged = TempRoot::forged(real.path().join("sub"));
    assert!(catch_unwind(AssertUnwindSafe(move || drop(forged))).is_err(), "a nested path is refused");
    assert!(keep.is_file(), "nothing was deleted");
    let absent = std::env::temp_dir().join(format!("mod-files-absent-{}", std::process::id()));
    let forged = TempRoot::forged(absent);
    assert!(catch_unwind(AssertUnwindSafe(move || drop(forged))).is_err(), "another prefix is refused");
    let forged = TempRoot::forged(real.path().join("..").join(real.path().file_name().unwrap()));
    assert!(catch_unwind(AssertUnwindSafe(move || drop(forged))).is_err(), "a dotted path is refused");
    assert!(keep.is_file(), "nothing was deleted");
}

#[test]
fn drop_never_follows_a_junction() {
    let a = TempRoot::new("junction_a");
    let b = TempRoot::new("junction_b");
    b.file("data/1.bin", &[1u8; 1000]);
    b.file("data/2.bin", &[2u8; 2000]);
    let before = b.hash_tree("");
    a.file("src/a.txt", b"a");
    a.junction("src/j", &b.path().join("data"));
    let b_name = b.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(a.hash_tree("src").get("j").map(String::as_str), Some(format!("junction -> {b_name}/data").as_str()));
    assert!(a.path().join("src/j/1.bin").is_file(), "the junction works");
    let a_path = a.path().to_path_buf();
    drop(a);
    assert!(!a_path.exists(), "the root is gone");
    assert_eq!(b.hash_tree(""), before, "the junction's target is untouched");
}

#[test]
fn junction_target_must_be_a_temp_root() {
    let a = TempRoot::new("junction_target");
    let temp = std::env::temp_dir();
    assert!(catch_unwind(AssertUnwindSafe(|| a.junction("j", &temp))).is_err());
    assert!(!a.path().join("j").exists());
}

#[test]
fn drop_clears_readonly() {
    let a = TempRoot::new("readonly");
    a.file("ro/x.txt", b"x");
    a.set_readonly("ro/x.txt", true);
    a.set_readonly("ro", true);
    assert!(std::fs::metadata(a.at("ro/x.txt")).unwrap().permissions().readonly());
    let p = a.path().to_path_buf();
    drop(a);
    assert!(!p.exists());
}

#[test]
fn ads_round_trip() {
    let a = TempRoot::new("ads");
    a.file("s.txt", b"main");
    let stream = vec![7u8; 4096];
    a.ads("s.txt", "s", &stream);
    let tree = a.hash_tree("");
    assert_eq!(tree.get("s.txt").map(String::as_str), Some(format!("file {:016x}", fnv1a64(b"main")).as_str()));
    assert_eq!(tree.get("s.txt:s").map(String::as_str), Some(format!("stream {:016x}", fnv1a64(&stream)).as_str()));
    let mut p = a.at("s.txt").into_os_string();
    p.push(":s");
    assert_eq!(std::fs::read(p).unwrap(), stream);
    assert_eq!(a.hash("s.txt"), fnv1a64(b"main"));
}

#[test]
fn rename_touch_write_remove() {
    let a = TempRoot::new("rename_touch");
    a.file("a.txt", b"one");
    a.write("a.txt", b"two");
    assert_eq!(a.hash("a.txt"), fnv1a64(b"two"));
    a.rename("a.txt", "b.txt");
    assert!(!a.at("a.txt").exists() && a.at("b.txt").is_file());
    let mtime: i64 = 0x01D0_0000_0000_0000;
    a.touch("b.txt", mtime);
    let modified = std::fs::metadata(a.at("b.txt")).unwrap().modified().unwrap();
    let since_1601 = modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() / 100 + 116_444_736_000_000_000;
    assert_eq!(since_1601 as i64, mtime);
    a.remove("b.txt");
    assert!(!a.at("b.txt").exists());
    a.dir("d/e");
    a.remove_dir("d/e");
    assert!(a.at("d").is_dir() && !a.at("d/e").exists());
    let lone = [u16::from(b'x'), 0xD800, u16::from(b'y')];
    let p = a.file_wide(&lone, b"lone");
    assert_eq!(std::fs::read(&p).unwrap(), b"lone");
    let names: Vec<_> = std::fs::read_dir(a.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(names.iter().any(|n| std::os::windows::ffi::OsStrExt::encode_wide(n.as_os_str()).eq(lone.iter().copied())));
}

#[test]
fn long_paths_beyond_260() {
    let a = TempRoot::new("long");
    let seg = "a".repeat(40);
    let rel = format!("{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/{seg}/f.txt");
    let p = a.file(&rel, b"deep");
    assert!(p.as_os_str().len() > 300);
    assert_eq!(a.hash(&rel), fnv1a64(b"deep"));
    a.set_readonly(&rel, true);
    let root = a.path().to_path_buf();
    drop(a);
    assert!(!root.exists());
}

#[test]
fn deny_listing_round_trip() {
    let a = TempRoot::new("deny");
    a.file("locked/x.txt", b"x");
    a.deny_listing("locked");
    let denied = std::fs::read_dir(a.at("locked")).and_then(|mut it| it.next().transpose());
    assert_eq!(denied.err().map(|e| e.kind()), Some(std::io::ErrorKind::PermissionDenied));
    a.allow_listing("locked");
    assert_eq!(std::fs::read_dir(a.at("locked")).unwrap().count(), 1);
    a.deny_listing("locked");
    let p = a.path().to_path_buf();
    drop(a);
    assert!(!p.exists(), "Drop restores the listing before removing");
}

#[test]
fn artifact_refuses_other_folders() {
    for rel in ["other/x.txt", "natural/../x.txt", "natural\\x.txt", "disposable/c:x", "/natural/x"] {
        assert!(catch_unwind(|| artifact(rel, b"")).is_err(), "{rel}");
    }
}

#[test]
fn test_tags_are_unique_and_plain() {
    let tags: Vec<Box<str>> = (0..50).map(|_| common::test_tag()).collect();
    for t in &tags {
        assert_eq!(t.len(), 9, "{t}");
        assert!(t.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()), "{t}");
    }
    let mut sorted = tags.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), tags.len(), "unique within a run");
    assert_eq!(common::base36(36 * 36 + 35, 3), "10z");
}

#[test]
fn hooks_name_the_test_root() {
    let root = TempRoot::new("hooks");
    let h = common::hooks(&root);
    let env = |k: &str| h.env.iter().find(|(n, _)| &**n == k).map(|(_, v)| v.to_string());
    assert_eq!(env("FILES_TEST_ROOT"), Some(root.path().display().to_string()));
    assert_eq!(env("FILES_DATA_DIR"), Some(root.path().join("data").display().to_string()));
    assert_eq!(env("FILES_TEST_NO_UI").as_deref(), Some("1"));
    assert_eq!(env("FILES_TEST_PARENT"), Some(std::process::id().to_string()));
    assert_eq!(h.pipe_tag.as_deref().map(str::len), Some(9));
    assert!(h.build_tag.is_none());
}
