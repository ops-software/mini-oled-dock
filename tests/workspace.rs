//! Invariants of the Cargo workspace (plans P1-workspace-v2, FZ-WS): the root package stays first with a literal
//! version, the shared dependency versions are pinned once, members follow one template and the crate graph
//! points one way. Pure reads of the manifests and of `cargo metadata`; nothing is built.

use std::collections::BTreeMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// `CREATE_NO_WINDOW` (winbase.h): every process a test starts carries it (lint `every_spawned_process_has_no_window`).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The dependencies whose versions live in `[workspace.dependencies]`.
const SHARED: [&str; 6] = ["serde", "serde_json", "windows", "windows-core", "windows-future", "windows-collections"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_manifest(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of the table `[name]` (header excluded), up to the next table header.
fn table<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let header = format!("[{name}]");
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with('[') {
            inside = line.trim_end() == header;
        } else if inside {
            out.push(line);
        }
    }
    out
}

fn holds_a_package(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|it| it.flatten().any(|e| e.path().join("Cargo.toml").is_file())).unwrap_or(false)
}

/// The quoted entries of the `members = [...]` line of `[workspace]`.
fn members_of(workspace: &[&str]) -> Vec<String> {
    let line = workspace.iter().find(|l| l.starts_with("members")).expect("[workspace] has a members line");
    let list = line.split_once('[').and_then(|(_, r)| r.split_once(']')).expect("members is a one-line list").0;
    list.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
}

/// Every manifest of a member: crates/*/Cargo.toml and apps/*/Cargo.toml.
fn member_manifests() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for group in ["crates", "apps"] {
        if let Ok(it) = std::fs::read_dir(root().join(group)) {
            for e in it.flatten() {
                let m = e.path().join("Cargo.toml");
                if m.is_file() {
                    out.push(m);
                }
            }
        }
    }
    out.sort();
    out
}

/// A dependency line that starts with one of the shared names (the name followed by a space or an equals sign).
fn names_dep(line: &str, name: &str) -> bool {
    line.strip_prefix(name).is_some_and(|r| r.starts_with([' ', '=']))
}

fn is_shared_line(line: &str) -> bool {
    SHARED.iter().any(|n| names_dep(line, n))
}

fn metadata() -> Value {
    let out = Command::new(env!("CARGO"))
        .creation_flags(CREATE_NO_WINDOW)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked", "--offline"])
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON")
}

fn packages(meta: &Value) -> &Vec<Value> {
    meta["packages"].as_array().expect("metadata has packages")
}

/// FZ-WS: who may depend on whom, one row per package of the suite (apps included, so no lane edits this table).
fn allowed() -> BTreeMap<&'static str, &'static [&'static str]> {
    BTreeMap::from([
        ("oled-trust", &[][..]),
        ("oled-sys", &["oled-trust"][..]),
        ("oled-config", &["oled-sys", "oled-trust"][..]),
        ("oled-ui", &["oled-config", "oled-sys", "oled-trust"][..]),
        ("oled-sysmon", &["oled-sys", "oled-trust"][..]),
        ("oled-shellmodel", &["oled-sys", "oled-trust"][..]),
        ("oled-i18n-build", &[][..]),
        (
            "dock",
            &["oled-trust", "oled-sys", "oled-config", "oled-ui", "oled-sysmon", "oled-shellmodel", "oled-i18n-build"]
                [..],
        ),
        (
            "tasks",
            &["oled-ui", "oled-config", "oled-sys", "oled-trust", "oled-sysmon", "oled-shellmodel", "oled-i18n-build"]
                [..],
        ),
        ("files", &["oled-ui", "oled-config", "oled-sys", "oled-trust", "oled-shellmodel", "oled-i18n-build"][..]),
        ("gallery", &["oled-ui", "oled-config", "oled-sys", "oled-trust", "oled-i18n-build"][..]),
    ])
}

#[test]
fn root_package_comes_first_with_a_literal_version() {
    let text = read_manifest(&root().join("Cargo.toml"));
    let first = text.lines().find(|l| l.starts_with('[')).expect("a table header");
    assert_eq!(first.trim_end(), "[package]", "[package] must be the first table of the root manifest");
    let package = table(&text, "package");
    assert!(package.iter().any(|l| l.trim_end() == "name = \"dock\""), "the root package is named dock");
    assert!(package.iter().any(|l| l.starts_with("version = \"")), "[package] holds a literal version line");
    // release.yml reads the first line of the file that matches ^version = "(.+)".
    let lines: Vec<&str> = text.lines().collect();
    let first_version = lines.iter().position(|l| l.starts_with("version = \"")).expect("a version line");
    let package_start = lines.iter().position(|l| l.starts_with('[')).unwrap();
    let package_end =
        lines.iter().skip(package_start + 1).position(|l| l.starts_with('[')).map(|i| i + package_start + 1).unwrap();
    assert!(
        first_version > package_start && first_version < package_end,
        "the first version line of the file must lie inside [package] (release.yml's tag check)"
    );
}

#[test]
fn workspace_tables_pin_the_shared_versions() {
    let text = read_manifest(&root().join("Cargo.toml"));
    let workspace = table(&text, "workspace");
    assert!(
        workspace.iter().any(|l| l.trim_end() == "exclude = [\"tools/i18n-codemod\"]"),
        "[workspace] excludes tools/i18n-codemod"
    );
    assert!(workspace.iter().any(|l| l.trim_end() == "resolver = \"2\""), "[workspace] sets resolver 2");
    let want: Vec<&str> = [("crates", "crates/*"), ("apps", "apps/*")]
        .into_iter()
        .filter(|(d, _)| holds_a_package(&root().join(d)))
        .map(|(_, g)| g)
        .collect();
    assert_eq!(
        members_of(&workspace),
        want,
        "a member glob must be listed exactly when its folder holds a package (cargo refuses a glob over a missing folder)"
    );
    let shared = table(&text, "workspace.dependencies");
    for name in SHARED {
        assert!(shared.iter().any(|l| names_dep(l, name)), "[workspace.dependencies] has {name}");
    }
    let mut checked: Vec<(String, Vec<&str>)> = vec![("Cargo.toml".to_string(), table(&text, "dependencies"))];
    let member_texts: Vec<(PathBuf, String)> =
        member_manifests().into_iter().map(|p| (p.clone(), read_manifest(&p))).collect();
    for (p, t) in &member_texts {
        for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
            checked.push((format!("{} [{kind}]", p.display()), table(t, kind)));
        }
    }
    for (what, lines) in checked {
        for line in lines.into_iter().filter(|l| is_shared_line(l)) {
            assert!(line.contains("workspace = true"), "{what}: \"{line}\" must take its version from the workspace");
        }
    }
}

#[test]
fn crate_graph_follows_the_direction() {
    let meta = metadata();
    let allowed = allowed();
    let names: Vec<&str> = packages(&meta).iter().map(|p| p["name"].as_str().unwrap()).collect();
    for p in packages(&meta) {
        let name = p["name"].as_str().unwrap();
        let row = allowed.get(name).unwrap_or_else(|| panic!("package {name} has no row in the dependency table"));
        for d in p["dependencies"].as_array().unwrap() {
            let dep = d["name"].as_str().unwrap();
            if !names.contains(&dep) {
                continue;
            }
            if dep == "oled-i18n-build" {
                assert_eq!(d["kind"].as_str(), Some("build"), "{name}: oled-i18n-build is a build-dependency only");
                continue; // a build-dependency of anyone (FZ-WS), so no row of the table lists it
            }
            assert!(row.contains(&dep), "{name} may not depend on {dep}");
        }
    }
}

#[test]
fn members_follow_the_template() {
    let meta = metadata();
    for p in packages(&meta) {
        let name = p["name"].as_str().unwrap();
        let manifest = p["manifest_path"].as_str().unwrap().replace('\\', "/");
        if name != "dock" {
            assert_eq!(p["edition"].as_str(), Some("2021"), "{name}: edition");
            assert_eq!(p["license"].as_str(), Some("MIT"), "{name}: license");
            assert_eq!(p["publish"], serde_json::json!([]), "{name}: publish = false");
            assert_eq!(p["rust_version"].as_str(), Some("1.87"), "{name}: rust-version");
        }
        if manifest.contains("/crates/") {
            assert_eq!(p["version"].as_str(), Some("0.0.0"), "{name}: crates carry version 0.0.0");
        }
        for t in p["targets"].as_array().unwrap() {
            let is_lib = t["kind"].as_array().unwrap().iter().any(|k| k.as_str() == Some("lib"));
            if is_lib && name != "dock" {
                assert_eq!(t["doctest"], serde_json::json!(false), "{name}: [lib] doctest = false");
            }
        }
    }
}

#[test]
fn apps_carry_the_suite_version() {
    let meta = metadata();
    let dock = packages(&meta).iter().find(|p| p["name"] == "dock").expect("the dock package");
    let suite = dock["version"].as_str().unwrap();
    for p in packages(&meta) {
        let manifest = p["manifest_path"].as_str().unwrap().replace('\\', "/");
        if manifest.contains("/apps/") {
            assert_eq!(p["version"].as_str(), Some(suite), "{}: apps carry the dock's version", p["name"]);
        }
    }
}

// ---- Suite rules for code under crates/ and apps/ (plans P1-workspace-v2, S5) -----------------------------------------

/// Files that may hold a `thread_local!` without a test-only guard, with the reason. Nothing is added without the kit owner.
const THREAD_LOCAL_ALLOWLIST: [(&str, &str); 1] =
    [("crates/oled-ui/src/gfx/glyphs.rs", "LEGACY: removed by K1a (PaintCx.scale)")];

/// A code line: not a comment.
fn is_code(line: &str) -> bool {
    !line.trim_start().starts_with("//")
}

/// Rule: no `static mut`; `thread_local!` only under a test-only cfg on the line above, or in an allowlisted file.
fn thread_local_violations(path: &str, text: &str) -> Vec<String> {
    let path = path.replace('\\', "/");
    let allowed = THREAD_LOCAL_ALLOWLIST.iter().any(|(p, _)| *p == path);
    let lines: Vec<&str> = text.lines().collect();
    let mut v = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if !is_code(line) {
            continue;
        }
        if line.contains("static mut ") {
            v.push(format!("{path}:{}: static mut", n + 1));
        }
        if line.contains("thread_local!") && !allowed {
            let above = if n > 0 { lines[n - 1].trim() } else { "" };
            let guarded = above == "#[cfg(test)]" || above == "#[cfg(any(test, feature = \"test-counters\"))]";
            if !guarded {
                v.push(format!("{path}:{}: thread_local! outside a test-only cfg", n + 1));
            }
        }
    }
    v
}

/// How many lines around a spawn may hold the no-window flag (the dock's own lint uses the same window).
const SPAWN_WINDOW: usize = 12;

/// Rule: every spawn in a crate or app hides its window. The tokens are built with concat! so that the dock's own
/// lint (which reads this file too) does not take the checker for a spawn.
fn spawn_violations(path: &str, text: &str) -> Vec<String> {
    let std_spawn = concat!("Command::", "new(");
    let win_spawn = concat!("CreateProcess", "W(");
    let lines: Vec<&str> = text.lines().collect();
    let mut v = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if !is_code(line) || line.trim_start().starts_with("use ") {
            continue;
        }
        let starts_std = line.contains(std_spawn);
        let starts_win = line.contains(win_spawn);
        if !starts_std && !starts_win {
            continue;
        }
        let from = if starts_win { n.saturating_sub(SPAWN_WINDOW) } else { n };
        let to = (n + SPAWN_WINDOW + 1).min(lines.len());
        let near = lines[from..to].join("\n");
        let flags = concat!("creation_", "flags(");
        if !(near.contains("CREATE_NO_WINDOW") && (starts_win || near.contains(flags))) {
            v.push(format!("{path}:{}: a process is started without CREATE_NO_WINDOW", n + 1));
        }
    }
    v
}

/// The key names of a strings file (FZ-I18N subset: lines `"area.item" = "text"`).
fn string_keys(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix('"'))
        .filter_map(|r| r.split_once('"').map(|(k, _)| k))
        .collect()
}

/// Rule: a key name belongs to one file. Takes (path, text) pairs; names both files of a clash.
fn duplicate_keys(files: &[(String, String)]) -> Vec<String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let mut out = Vec::new();
    for (path, text) in files {
        for key in string_keys(text) {
            match seen.get(key) {
                Some(first) if first != path => out.push(format!("key {key} is in both {first} and {path}")),
                _ => {
                    seen.insert(key, path);
                }
            }
        }
    }
    out
}

/// Every .rs file under crates/ and apps/ as (relative path, text).
fn crate_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(it) = std::fs::read_dir(dir) else { return };
        for e in it.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    for group in ["crates", "apps"] {
        walk(&root().join(group), &mut files);
    }
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(root()).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            (rel, text)
        })
        .collect()
}

#[test]
fn no_thread_local_in_shared_crates() {
    let tl = concat!("thread_", "local!");
    let bad = format!("static mut X: u32 = 0;\n{tl} {{ static A: u32 = 0; }}");
    assert_eq!(thread_local_violations("crates/x/src/a.rs", &bad).len(), 2);
    let ok = format!("#[cfg(test)]\n{tl} {{ static A: u32 = 0; }}");
    assert!(thread_local_violations("crates/x/src/a.rs", &ok).is_empty());
    let counters = format!("#[cfg(any(test, feature = \"test-counters\"))]\n{tl} {{ static A: u32 = 0; }}");
    assert!(thread_local_violations("crates/x/src/a.rs", &counters).is_empty());
    let loose = format!("{tl} {{ static A: u32 = 0; }}");
    assert_eq!(thread_local_violations("crates/x/src/a.rs", &loose).len(), 1);
    assert!(thread_local_violations("crates/oled-ui/src/gfx/glyphs.rs", &loose).is_empty(), "the allowlisted file");
    assert!(thread_local_violations("crates/x/src/a.rs", &format!("// {tl}")).is_empty(), "comments do not count");
    let mut all = Vec::new();
    for (path, text) in crate_sources() {
        all.extend(thread_local_violations(&path, &text));
    }
    assert!(all.is_empty(), "{all:#?}");
}

#[test]
fn spawned_processes_in_crates_hide_their_window() {
    let (new, create) = (concat!("Command::", "new("), concat!("CreateProcess", "W("));
    let good = format!("let c = {new}exe)\n    .creation_flags(CREATE_NO_WINDOW)\n    .spawn();");
    assert!(spawn_violations("crates/x/src/a.rs", &good).is_empty());
    let bad = format!("let c = {new}exe)\n    .spawn();");
    assert_eq!(spawn_violations("crates/x/src/a.rs", &bad).len(), 1);
    let win_bad = format!("unsafe {{ {create}app, line) }};");
    assert_eq!(spawn_violations("apps/x/src/a.rs", &win_bad).len(), 1);
    let win_good = format!("let flags = CREATE_NO_WINDOW;\nunsafe {{ {create}app, line) }};");
    assert!(spawn_violations("apps/x/src/a.rs", &win_good).is_empty());
    assert!(spawn_violations("crates/x/src/a.rs", &format!("// {new}x)")).is_empty(), "comments do not count");
    let mut all = Vec::new();
    for (path, text) in crate_sources() {
        all.extend(spawn_violations(&path, &text));
    }
    assert!(all.is_empty(), "{all:#?}");
}

#[test]
fn oled_trust_stays_pure() {
    let manifest = read_manifest(&root().join("crates/oled-trust/Cargo.toml"));
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        assert!(
            !table(&manifest, kind).iter().any(|l| names_dep(l, "windows") || names_dep(l, "windows-core")),
            "oled-trust must not depend on windows ([{kind}])"
        );
    }
    let lib = read_manifest(&root().join("crates/oled-trust/src/lib.rs"));
    let mut lines = lib.lines();
    assert!(lines.next().is_some_and(|l| l.starts_with("//!")), "lib.rs starts with its doc line");
    assert_eq!(lines.next(), Some("#![forbid(unsafe_code)]"), "lib.rs forbids unsafe code right after the doc line");
}

#[test]
fn string_keys_belong_to_one_crate() {
    let a = ("crates/a/strings/en.toml".to_string(), "# c\n\"kit.ok\" = \"OK\"\n\"kit.no\" = \"No\"\n".to_string());
    let b =
        ("crates/b/strings/en.toml".to_string(), "\"tasks.title\" = \"Tasks\"\n\"kit.ok\" = \"Okay\"\n".to_string());
    let clash = duplicate_keys(&[a.clone(), b.clone()]);
    assert_eq!(clash.len(), 1);
    assert!(clash[0].contains("kit.ok") && clash[0].contains("crates/a") && clash[0].contains("crates/b"), "{clash:?}");
    assert!(duplicate_keys(&[a]).is_empty());
    let mut files = Vec::new();
    let mut paths = vec![root().join("strings/en.toml")];
    for group in ["crates", "apps"] {
        if let Ok(it) = std::fs::read_dir(root().join(group)) {
            paths.extend(it.flatten().map(|e| e.path().join("strings/en.toml")));
        }
    }
    for p in paths.into_iter().filter(|p| p.is_file()) {
        let rel = p.strip_prefix(root()).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        files.push((rel, read_manifest(&p)));
    }
    let all = duplicate_keys(&files);
    assert!(all.is_empty(), "{all:#?}");
}

/// `cargo tree` of what depends on oled-ui, with the given edge kinds, for the release target.
fn oled_ui_tree(edges: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .creation_flags(CREATE_NO_WINDOW)
        .current_dir(root())
        .args(["tree", "--locked", "--offline", "-e", edges, "-i", "oled-ui", "--target", "x86_64-pc-windows-msvc"])
        .output()
        .expect("run cargo tree");
    assert!(out.status.success(), "cargo tree failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Section 6: the dev-dependency switches oled-ui's `test-counters` on for tests only; a release build must not see it.
#[test]
fn release_build_has_no_test_counters() {
    let normal = oled_ui_tree("normal,features");
    assert!(!normal.contains("test-counters"), "a release build resolves test-counters:\n{normal}");
    let with_dev = oled_ui_tree("features");
    assert!(with_dev.contains("test-counters"), "the query cannot see the feature at all:\n{with_dev}");
}
