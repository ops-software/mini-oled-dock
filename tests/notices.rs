//! THIRD_PARTY_NOTICES.md must cover what ships in dock.exe.
//! Regenerate the crate part with `pwsh tools/gen-notices.ps1`.

use std::process::Command;

fn read(rel: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn notices_list_every_crate_in_the_exe() {
    let out = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--offline",
            "-e",
            "normal",
            "--target",
            "x86_64-pc-windows-msvc",
            "--prefix",
            "none",
            "-f",
            "{p}",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let notices = String::from_utf8(read("THIRD_PARTY_NOTICES.md")).expect("notices are UTF-8");
    let used: Vec<&str> = notices
        .lines()
        .filter_map(|l| l.strip_prefix("Used by: "))
        .collect();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen = 0;
    let mut missing = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue; // the root package, dock itself
        }
        let line = line.trim_end_matches(" (*)").trim_end_matches(" (proc-macro)");
        let Some((name, ver)) = line.split_once(" v") else {
            continue;
        };
        seen += 1;
        let want = format!("{name} {ver}");
        let found = used
            .iter()
            .any(|l| l.split(", ").any(|item| item == want));
        if !found && !missing.contains(&want) {
            missing.push(want);
        }
    }
    assert!(seen > 0, "cargo tree listed no dependency");
    assert!(
        missing.is_empty(),
        "THIRD_PARTY_NOTICES.md lacks {missing:?}: run pwsh tools/gen-notices.ps1"
    );
}

#[test]
fn eartrumpet_licence_is_verbatim() {
    let lic = read("tools/notices/EarTrumpet-LICENSE.txt");
    let notices = read("THIRD_PARTY_NOTICES.md");
    assert!(
        contains(&notices, &lic),
        "THIRD_PARTY_NOTICES.md does not contain tools/notices/EarTrumpet-LICENSE.txt verbatim"
    );
    let s = String::from_utf8(lic).expect("licence is UTF-8");
    for who in [
        "Yellow Elephant Productions",
        "Tidal Media Inc.",
        "Articent Group LLC",
    ] {
        assert!(s.contains(who), "licence file does not name {who}");
    }
}

#[test]
fn notices_have_lf_only() {
    assert!(
        !read("THIRD_PARTY_NOTICES.md").contains(&13),
        "THIRD_PARTY_NOTICES.md contains CR"
    );
}
