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

#[test]
fn owner_fixture_sets_alt_space() {
    let c = parse(&read("owner-1.5.0.json")).unwrap();
    assert_eq!(c.launcher.chord, "alt+space");
    assert!(c.launcher.chord_spec().is_some());
    // the rest of the file is the 1.4.2 owner file
    let mut old = parse(&read("owner-1.4.2.json")).unwrap();
    old.launcher.chord = "alt+space".into();
    assert_eq!(c, old);
}

#[test]
fn launcher_chord_off_by_default() {
    // the default and every file written by 1.4.3 (its config is the 1.4.2 shape) leave the chord unset
    assert_eq!(Config::default().launcher.chord_spec(), None);
    for f in ["v1.4.2-default.json", "v1.4.2-custom.json", "owner-1.4.2.json"] {
        let c = parse(&read(f)).unwrap();
        assert_eq!(c.launcher.chord, "off", "{f}");
        assert_eq!(c.launcher.chord_spec(), None, "{f}");
    }
}

#[test]
fn unknown_keys_survive_a_save() {
    // Keys a newer release wrote are kept at this release's saves, so going back to it and forward again
    // keeps that release's settings.
    let first = parse(&read("unknown-keys.json")).unwrap();
    let want: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"a_key_from_the_future": {"nested": [1, 2, 3]}, "another_future_key": true}"#)
            .unwrap();
    assert_eq!(first.extra, want);
    let r = render(&first);
    assert_eq!(r.matches("\"a_key_from_the_future\":").count(), 1, "{r}");
    let again = parse(&r).unwrap();
    assert_eq!(again.extra, want);
    assert_eq!(again, first); // every known field too
    assert_eq!(render(&again), r);
}

#[test]
fn known_and_legacy_keys_are_never_extra() {
    assert!(parse(&read("v1.4.2-custom.json")).unwrap().extra.is_empty());
    assert!(parse("{\"bar_size\":\"large\"}").unwrap().extra.is_empty());
    assert!(Config::default().extra.is_empty());
    // No unknown key: not one byte of the file changes.
    assert!(render(&Config::default()).ends_with("  \"notif_prompted\": false\n}\n"));
}

#[test]
fn rendered_keys_are_known_and_never_extra() {
    // The known keys are every key serde writes for a `Config`, and the legacy `bar_size`.
    let serde_keys = match serde_json::to_value(Config::default()).unwrap() {
        serde_json::Value::Object(m) => m.keys().cloned().collect::<Vec<_>>(),
        other => panic!("{other}"),
    };
    let mut want: Vec<&str> = serde_keys.iter().map(String::as_str).chain(["bar_size"]).collect();
    let mut have = KNOWN_KEYS.to_vec();
    want.sort_unstable();
    have.sort_unstable();
    assert_eq!(have, want);
    // A key `render` writes that a later parse took for an unknown one (a future top-level
    // `skip_serializing_if`, say) would be kept as extra and written twice.
    let r = strip_comments(&render(&Config::default()));
    let written: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&r).unwrap();
    assert!(parse(&r).unwrap().extra.is_empty());
    for f in fixtures() {
        if let Ok(c) = parse(&read_utf8_keep_bom(&f)) {
            for k in c.extra.keys() {
                assert!(!written.contains_key(k) && k != "bar_size", "{f:?}: {k}");
            }
        }
    }
}
