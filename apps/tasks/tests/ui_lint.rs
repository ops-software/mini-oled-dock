//! The app's string lints (T1.5, the 3.1 gate a): no literal UI text passed to a text call, the display name only as
//! the kit's key, every key in Italian. A source scan like the kit's lint_strings. (A key unused by the code is not
//! checked here: later groups own keys that their tasks have not yet used.)

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Every .rs file under `dir`, recursively.
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

/// The part of a file before its test module (the cut is at the `#[cfg(test)]` whose next item is a `mod`).
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

/// The calls whose arguments hold text a person reads (a name that ends in `(` or `{`, scanned to its closing bracket).
const TEXT_CALLS: [&str; 11] = [
    "text_in(",
    "TextLabel::new(",
    "Button::new(",
    "IconButton::new(",
    "Text::Borrowed(",
    "Cow::from(",
    "Cow::Borrowed(",
    "Text::from(",
    "MenuItem::Header(",
    "MenuItem::Custom {",
    "MenuItem::Submenu {",
];

/// Fields that hold text, up to the end of their expression: a menu item's `label: "..".into()`.
const TEXT_FIELDS: [&str; 3] = ["label: ", "tip: ", "note: "];

/// Literals that are not UI: an empty string, one without a letter, and the few the code needs (file names, key names).
const NOT_UI: [&str; 2] = ["tasks.json", "state.json"];

/// True when `arg` holds a string literal that has a letter (comment lines do not count).
fn literal_in_args(arg: &str) -> bool {
    let mut in_str = false;
    let mut has_letter = false;
    let mut lit = String::new();
    let mut prev = ' ';
    let code: String = arg.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for c in code.chars() {
        if c == '"' && prev != '\\' {
            if in_str && has_letter && !NOT_UI.contains(&lit.as_str()) {
                return true;
            }
            in_str = !in_str;
            has_letter = false;
            lit.clear();
        } else if in_str {
            lit.push(c);
            if c.is_alphabetic() {
                has_letter = true;
            }
        }
        prev = c;
    }
    false
}

/// What lies between the bracket just before `at` and its match, across lines.
fn balanced(text: &str, at: usize) -> &str {
    let mut depth = 1;
    let mut in_str = false;
    let mut prev = ' ';
    for (i, c) in text[at..].char_indices() {
        if in_str {
            if c == '"' && prev != '\\' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '(' | '{' | '[' => depth += 1,
                ')' | '}' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return &text[at..at + i];
                    }
                }
                _ => {}
            }
        }
        prev = c;
    }
    &text[at..]
}

/// The lines of `text` (outside its test module) that open a text call, or a text field, with a letter-bearing
/// literal among its arguments; a call split over several lines is read to its closing bracket.
fn violations(path: &str, text: &str) -> Vec<String> {
    let text = non_test(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut hits = std::collections::BTreeSet::new();
    let mut check = |at: usize, args: &str| {
        let n = text[..at].matches('\n').count();
        let line = lines.get(n).copied().unwrap_or("");
        let skip = line.trim_start().starts_with("//") || line.contains("// lint: not UI") || line.contains("log!(");
        if !skip && literal_in_args(args) {
            hits.insert(n);
        }
    };
    for call in TEXT_CALLS {
        for (i, _) in text.match_indices(call) {
            let at = i + call.len();
            check(i, balanced(text, at));
        }
    }
    for field in TEXT_FIELDS {
        for (i, _) in text.match_indices(field) {
            let at = i + field.len();
            let rest = &text[at..];
            let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
            check(i, &rest[..end]);
        }
    }
    hits.into_iter().map(|n| format!("{path}:{}: {}", n + 1, lines[n].trim())).collect()
}

#[test]
fn no_literal_is_passed_to_a_text_call() {
    let mut files = Vec::new();
    sources(&manifest().join("src"), &mut files);
    let mut all = Vec::new();
    for (p, t) in &files {
        all.extend(violations(&p.display().to_string(), t));
    }
    assert!(
        all.is_empty(),
        "literal text in a text call (use a key of strings/, or mark the line `// lint: not UI`):\n{all:#?}"
    );
}

#[test]
fn the_lint_catches_a_planted_literal() {
    let planted = "fn f(cx: &mut PaintCx) {\n    cx.text_in(\"Close\", role, r, a, t, ink);\n}\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "a planted text_in(\"Close\") fails");
    let planted = "fn f() {\n    let b = Button::new(id, Cow::from(\"Close\"), kind);\n}\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "a planted Button::new with a literal fails");
    // rustfmt splits a long call: the literal is on a later line.
    let planted =
        "fn f() {\n    let b = Button::new(\n        id,\n        \"Close\".into(),\n        kind,\n    );\n}\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "a literal on the next line fails");
    let planted = "fn f() {\n    cx.text_in(\n        &label,\n        role,\n    );\n    let x = y;\n}\nfn g() { Cow::from(\n  \"x y\") }\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "only the call that holds the literal");
    // Menu labels.
    let planted = "fn f() {\n    MenuItem::Custom { id, label: \"Close\".into(), glyph: None }\n}\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "a literal MenuItem label fails");
    let planted = "fn f() -> MenuItem {\n    MenuItem::Custom {\n        id,\n        label: \"Close\".into(),\n        enabled: true,\n    }\n}\n";
    assert!(!violations("w.rs", planted).is_empty(), "a split MenuItem literal fails");
    let planted = "fn f() {\n    let m = MenuItem::Header(\"Close\".into());\n}\n";
    assert_eq!(violations("w.rs", planted).len(), 1, "a literal header fails");
    let ok = "fn f() {\n    cx.text_in(&label, role, r, a, t, ink);\n    let b = Button::new(id, t(lang, k::X), kind);\n    log!(\"x: {}\", 1);\n    let m = MenuItem::Custom { id, label, glyph: None };\n}\n";
    assert!(violations("w.rs", ok).is_empty(), "keys, variables and log lines pass");
    let in_test = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn g() { Text::Borrowed(\"Close\"); }\n}\n";
    assert!(violations("w.rs", in_test).is_empty(), "a test module may use literals");
}

/// Every Harness test of this group ends by checking that each widget has its name.
#[test]
fn every_harness_test_checks_the_names() {
    let text = read(&manifest().join("tests").join("golden_t1.rs"));
    let mut missing = Vec::new();
    for part in text.split("#[test]\nfn ").skip(1) {
        let name = part.split('(').next().unwrap_or("");
        let body = part.split("\n}\n").next().unwrap_or("");
        let opens = ["open(", "rig(", "Harness::new("].iter().any(|c| body.contains(c));
        if opens && !body.contains("assert_named()") {
            missing.push(name.to_string());
        }
    }
    assert!(missing.is_empty(), "Harness tests that never call assert_named(): {missing:?}");
}

/// The display name is the kit's: "Tasks" and "Attivit\u{e0}" appear in no other string of the app, nor in the code.
#[test]
fn the_display_name_is_only_the_kits() {
    for file in ["strings/en.toml", "strings/it.toml"] {
        for line in read(&manifest().join(file)).lines().filter(|l| !l.trim_start().starts_with('#')) {
            let value = line.split_once(" = ").map_or("", |x| x.1);
            assert!(
                !value.contains("Tasks") && !value.contains("Attivit\u{e0}"),
                "{file}: the display name is written in a string ({line}); use {{name}}"
            );
        }
    }
}

#[test]
fn the_display_name_is_not_in_the_code() {
    let mut files = Vec::new();
    sources(&manifest().join("src"), &mut files);
    for (p, t) in &files {
        for (n, line) in non_test(t).lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("\"Tasks\"") && !code.contains("Attivit\u{e0}"),
                "{}:{}: the display name is written in code; use strings::app_name",
                p.display(),
                n + 1
            );
        }
    }
}

fn entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter_map(|l| l.split_once(" = "))
        .map(|(k, v)| (k.trim().trim_matches('"').to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

#[test]
fn every_key_is_in_italian() {
    let en = entries(&read(&manifest().join("strings/en.toml")));
    let it = entries(&read(&manifest().join("strings/it.toml")));
    assert!(!en.is_empty());
    assert_eq!(en.len(), it.len(), "a key is in one file only");
    for (k, _) in &en {
        assert!(it.iter().any(|(ik, iv)| ik == k && !iv.trim().is_empty()), "{k} has no Italian text");
    }
}
