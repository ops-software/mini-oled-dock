//! Config fixtures of every shape from 1.0.0 to 1.4.2, and their snapshots.
//!
//! Included from `src/config.rs` as a child module (`#[path]`), so the private `parse` and `render` are
//! reachable. Fixture inputs are never normalised (the BOM, line endings and truncation are the point);
//! only the snapshots are compared with line endings normalised. `UPDATE_SNAPSHOTS=1` rewrites them.

use super::*;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config")
}

fn name(f: &Path) -> String {
    f.file_name().unwrap().to_string_lossy().into_owned()
}

/// Every fixture input (`*.json` except `*.snap.json`), sorted by name.
fn fixtures() -> impl Iterator<Item = PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| name(p).ends_with(".json") && !name(p).ends_with(".snap.json"))
        .collect();
    v.sort();
    v.into_iter()
}

/// The file as text, a BOM kept (`parse` strips it itself).
fn read_utf8_keep_bom(f: &Path) -> String {
    String::from_utf8(std::fs::read(f).unwrap()).unwrap_or_else(|e| panic!("{f:?} is not UTF-8: {e}"))
}

fn read(file: &str) -> String {
    read_utf8_keep_bom(&dir().join(file))
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim_end().to_string()
}

fn snapshot_of(text: &str) -> String {
    match parse(text) {
        Ok(c) => serde_json::to_string_pretty(&c).unwrap(),
        Err(e) => format!("{{\"error\": {e:?}}}"),
    }
}

#[test]
fn every_fixture_parses_to_its_snapshot() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut n = 0;
    for f in fixtures() {
        let got = snapshot_of(&read_utf8_keep_bom(&f));
        let snap = f.with_extension("snap.json");
        if update {
            std::fs::write(&snap, &got).unwrap();
        } else {
            let want = std::fs::read_to_string(&snap).expect("missing snapshot: run with UPDATE_SNAPSHOTS=1");
            assert_eq!(norm(&got), norm(&want), "{f:?}");
        }
        n += 1;
    }
    assert!(n >= 26, "fixtures went missing: {n}"); // 16 tag shapes + 9 edge shapes + the owner's file
}

#[test]
fn fixtures_that_parse_roundtrip() {
    // parse(render(c)) == c and render is idempotent, for every fixture that parses
    for f in fixtures() {
        if let Ok(c) = parse(&read_utf8_keep_bom(&f)) {
            let r = render(&c);
            assert_eq!(parse(&r).unwrap(), c, "{f:?}");
            assert_eq!(render(&parse(&r).unwrap()), r, "{f:?}");
        }
    }
}

#[test]
fn every_release_fixture_is_accepted() {
    for f in fixtures().filter(|f| name(f).starts_with("v1.")) {
        assert!(parse(&read_utf8_keep_bom(&f)).is_ok(), "{f:?}");
    }
}

#[test]
fn the_bom_fixture_keeps_its_bytes() {
    let b = std::fs::read(dir().join("bom.json")).unwrap();
    assert_eq!(&b[..3], &[0xEF, 0xBB, 0xBF], "a checkout rewrote bom.json: check .gitattributes");
}

#[test]
fn the_truncated_fixture_is_still_truncated() {
    let b = std::fs::read(dir().join("invalid-truncated.json")).unwrap();
    assert!(!b.trim_ascii_end().ends_with(b"}"), "a checkout or editor repaired invalid-truncated.json");
}

#[test]
fn the_1_4_2_default_file_is_todays_default() {
    assert_eq!(parse(&read("v1.4.2-default.json")).unwrap(), Config::default());
}
