//! oled-lang end to end over a temp workspace.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn oled_lang_cli() {
    let root = std::env::temp_dir().join(format!("oled-lang-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../oled-sys/tests/fixtures");
    for f in ["en.toml", "it.toml"] {
        let text = std::fs::read_to_string(fx.join("lang-sample").join(f)).unwrap();
        put(&root, &format!("strings/{f}"), &text);
    }
    // A second folder with its own keys: the output holds both, so it differs from the fixture file.
    put(&root, "crates/x/strings/en.toml", "\"other.key\" = \"Other\"\n");
    put(&root, "crates/x/strings/it.toml", "\"other.key\" = \"Altro\"\n");
    let out = root.join("it.lang");
    let run = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_oled-lang"));
        c.creation_flags(CREATE_NO_WINDOW).args(args).output().unwrap()
    };
    let root_s = root.to_str().unwrap();
    let out_s = out.to_str().unwrap();
    let r = run(&["--lang", "it", "--version", "1.8.0", "--root", root_s, "--out", out_s]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..4], b"OLNG");
    assert_eq!(&bytes[16..18], b"it");
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 5, "four sample keys and one more");
    // Without the second folder the file is the fixture, byte for byte.
    std::fs::remove_dir_all(root.join("crates")).unwrap();
    let r = run(&["--lang", "it", "--version", "1.8.0", "--root", root_s, "--out", out_s]);
    assert!(r.status.success());
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(fx.join("lang-sample.lang")).unwrap());
    // Refusals: exit 2, no file touched.
    assert_eq!(run(&["--lang", "it", "--version", "1.8", "--root", root_s, "--out", out_s]).status.code(), Some(2));
    assert_eq!(run(&["--bogus"]).status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&root);
}
