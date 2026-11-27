//! The kit's string lints (the 3.1 gate a, for the kit): Italian for every key, no dead keys, no literal UI text in the
//! widgets and the host.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The entries of a strings file: `"key" = "value"` lines.
fn entries(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(" = ") else { continue };
        let unq = |s: &str| s.trim().trim_matches('"').to_string();
        out.push((unq(k), unq(v)));
    }
    out
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Keys whose Italian text is the English one on purpose: key names every keyboard prints the same, and the names of the
/// apps.
const SAME_IN_ITALIAN: [&str; 9] = [
    "kit.key.esc",
    "kit.key.tab",
    "kit.key.home",
    "kit.key.ins",
    "kit.key.backspace",
    "kit.key.ctrl",
    "kit.key.alt",
    "kit.key.win",
    "kit.app.files",
];

#[test]
fn kit_strings_have_italian() {
    let en = entries(&read(&manifest().join("strings/en.toml")));
    let it = entries(&read(&manifest().join("strings/it.toml")));
    assert!(!en.is_empty());
    for (k, e) in &en {
        let Some((_, i)) = it.iter().find(|(ik, _)| ik == k) else { panic!("{k} has no Italian entry") };
        assert!(!i.trim().is_empty(), "{k}: the Italian text is empty");
        if !SAME_IN_ITALIAN.contains(&k.as_str()) {
            assert_ne!(e, i, "{k}: the Italian text equals the English one (add it to the allowlist if that is right)");
        }
    }
    assert_eq!(en.len(), it.len(), "a key is in one file only");
}

/// Every .rs file under `dir`, recursively, with its path.
fn sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let t = read(&p);
            out.push((p, t));
        }
    }
}

/// The part of a source file before its test module: the cut is at the `#[cfg(test)]` whose next non-blank line
/// starts a `mod`; a test-only item elsewhere (a field, a helper) does not hide the rest of the file.
fn non_test(text: &str) -> &str {
    let mut from = 0;
    while let Some(i) = text[from..].find("#[cfg(test)]") {
        let at = from + i;
        let after = &text[at + "#[cfg(test)]".len()..];
        let next = after.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("#["));
        if next.is_some_and(|l| l.starts_with("mod ") || l.starts_with("pub mod ")) {
            return &text[..at];
        }
        from = at + "#[cfg(test)]".len();
    }
    text
}

/// Keys whose widgets land in later kit groups: not used yet, and not dead. A key used by its widget needs no entry;
/// a stale entry is harmless.
const PENDING: [&str; 18] = [
    "kit.close",
    "kit.minimize",
    "kit.maximize",
    "kit.restore",
    "kit.clear",
    "kit.cancel",
    "kit.more",
    "kit.search",
    "kit.back",
    "kit.working",
    "kit.new_tab",
    "kit.close_tab",
    "kit.sort_ascending",
    "kit.sort_descending",
    "kit.hide_column",
    "kit.columns",
    "kit.move_left",
    "kit.move_right",
];

#[test]
fn every_kit_key_is_used() {
    let mut files = Vec::new();
    sources(&manifest().join("src"), &mut files);
    let code: String = files.iter().map(|(_, t)| non_test(t)).collect::<Vec<_>>().concat();
    let mut dead = Vec::new();
    for (key, _) in entries(&read(&manifest().join("strings/en.toml"))) {
        let constant = key.to_uppercase().replace('.', "_");
        if !code.contains(&constant)
            && !PENDING.contains(&key.as_str())
            && key != "kit.app.tasks"
            && key != "kit.app.files"
        {
            dead.push(key);
        }
    }
    assert!(dead.is_empty(), "kit keys named nowhere in src/ outside tests: {dead:?}");
}

/// True when a line is allowed to hold a string literal with a letter.
fn allowed_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || [
            "log!(",
            "w!(",
            "debug_assert",
            "expect(",
            "panic!(",
            "mini-oled-dock.",
            "MiniOled",
            "// lint: not UI",
            "#[",
            "unreachable!(",
            "assert",
            // A developer-facing diagnostic of the host, never shown to a person.
            "NotReady(",
            "extern \"",
        ]
        .iter()
        .any(|m| line.contains(m))
}

/// The lines of `text` (outside its test module) with a string literal that holds a letter and is not allowed.
fn literal_violations(path: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (n, line) in non_test(text).lines().enumerate() {
        if allowed_line(line) {
            continue;
        }
        let mut in_str = false;
        let mut has_letter = false;
        let mut chars = line.chars().peekable();
        let mut prev = ' ';
        while let Some(c) = chars.next() {
            if c == '\'' && !in_str {
                // A char literal ('"' included) or a lifetime: skip the quote and what it quotes.
                if chars.peek() == Some(&'\\') || chars.clone().nth(1) == Some('\'') {
                    chars.next();
                    chars.next();
                }
                prev = c;
                continue;
            }
            if c == '"' && prev != '\\' {
                if in_str && has_letter {
                    out.push(format!("{path}:{}: {}", n + 1, line.trim()));
                    break;
                }
                in_str = !in_str;
                has_letter = false;
            } else if in_str && c.is_alphabetic() {
                has_letter = true;
            }
            prev = if c == '\\' && prev == '\\' { ' ' } else { c };
        }
    }
    out
}

#[test]
fn no_literal_ui_strings() {
    let mut files = Vec::new();
    sources(&manifest().join("src/widgets"), &mut files);
    sources(&manifest().join("src/host"), &mut files);
    let mut all = Vec::new();
    for (p, t) in &files {
        all.extend(literal_violations(&p.display().to_string(), t));
    }
    assert!(
        all.is_empty(),
        "literal text in widgets or host (use the kit table, or mark the line `// lint: not UI`):\n{all:#?}"
    );
}

#[test]
fn the_literal_lint_catches_a_planted_literal() {
    let planted = "fn f() {\n    let t = Text::from(\"Close\");\n}\n";
    assert_eq!(literal_violations("w.rs", planted).len(), 1, "a planted Text::from(\"Close\") fails");
    let ok = "fn f() {\n    log!(\"surface: failed\");\n    let a = \"1\";\n    let c = '\"';\n    // \"Close\"\n    let s = \"x\"; // lint: not UI\n}\n";
    assert!(literal_violations("w.rs", ok).is_empty(), "{:?}", literal_violations("w.rs", ok));
    let in_tests = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn g() { let _ = \"Close\"; }\n}\n";
    assert!(literal_violations("w.rs", in_tests).is_empty(), "test modules are exempt");
    let early = "struct H {\n    #[cfg(test)]\n    calls: u32,\n}\nfn f() {\n    let t = Text::from(\"Close\");\n}\n#[cfg(test)]\nmod tests {}\n";
    assert_eq!(
        literal_violations("w.rs", early).len(),
        1,
        "an early test-only item does not hide the rest of the file"
    );
}
