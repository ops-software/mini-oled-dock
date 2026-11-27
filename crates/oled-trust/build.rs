//! Reads the suite version from the root Cargo.toml (the first `version = "X.Y.Z"` line, which is the
//! root package's) and hands it to the crate as OLED_SUITE_VERSION. Std only.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"));
    let root = dir.join("..").join("..").join("Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    let text = std::fs::read_to_string(&root).expect("the root Cargo.toml is readable");
    let version = text.lines().find_map(|l| {
        let v = l.strip_prefix("version = \"")?.strip_suffix('"')?;
        let mut parts = v.split('.');
        let ok = (0..3).all(|_| parts.next().is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())));
        (ok && parts.next().is_none()).then_some(v)
    });
    match version {
        Some(v) => println!("cargo:rustc-env=OLED_SUITE_VERSION={v}"),
        None => panic!("no version = \"X.Y.Z\" line in the root Cargo.toml"),
    }
}
