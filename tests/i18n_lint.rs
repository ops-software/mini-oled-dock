//! Lints of the keyed-strings migration (plan P1-workspace-v2, W4a, C2). Each rule is a pure checker over (path, text)
//! with an inline bad-snippet test, then run over src/ and strings/. The migration is closed (C6): a finding in the tree
//! fails the test (`ENFORCE`); an inventory mode, which only prints, was used while it ran.
//!
//! L1: no `tr(` call in src/, and `pick(` only in src/i18n.rs, launcher/commands.rs and launcher/runline.rs.
//! L2: `italian()` is called only in src/i18n.rs and inside a test module (after `#[cfg(test)] mod tests`).
//! L3: every key of strings/en.toml is referenced as `k::<CONST>` somewhere in src/ (no dead keys).
//! L4: strings/en.toml and strings/it.toml hold the same keys and no empty text.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// True once the migration is closed (C6): the over-the-tree assertions fail on a finding instead of printing it.
const ENFORCE: bool = true;

/// Where `pick(` may stay: the two launcher tables whose texts are identity and search data.
const PICK_FILES: [&str; 3] = ["src/i18n.rs", "src/launcher/commands.rs", "src/launcher/runline.rs"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The text with comments, string literals and char literals blanked (newlines kept, so offsets and lines hold).
fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut blank = |from: usize, to: usize| {
        for c in &mut out[from..to.min(b.len())] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        let word_before = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if rest.starts_with(b"//") {
            let end = text[i..].find('\n').map_or(b.len(), |p| i + p);
            blank(i, end);
            i = end;
        } else if rest.starts_with(b"/*") {
            let (mut depth, mut j) = (1, i + 2);
            while j < b.len() && depth > 0 {
                if b[j..].starts_with(b"/*") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"*/") {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(i, j);
            i = j;
        } else if rest[0] == b'r' && !word_before && matches!(rest.get(1), Some(b'"') | Some(b'#')) {
            let hashes = rest[1..].iter().take_while(|&&c| c == b'#').count();
            if rest.get(1 + hashes) == Some(&b'"') {
                let closer = format!("\"{}", "#".repeat(hashes));
                let body = i + 2 + hashes;
                let end = text[body..].find(&closer).map_or(b.len(), |p| body + p + closer.len());
                blank(i, end);
                i = end;
            } else {
                i += 1;
            }
        } else if rest[0] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            blank(i, j + 1);
            i = j + 1;
        } else if rest[0] == b'\'' {
            let lit = if rest.get(1) == Some(&b'\\') {
                rest.iter().skip(2).position(|&c| c == b'\'').map(|p| p + 3)
            } else {
                text[i + 1..]
                    .chars()
                    .next()
                    .and_then(|c| (rest.get(1 + c.len_utf8()) == Some(&b'\'')).then(|| 2 + c.len_utf8()))
            };
            match lit {
                Some(n) => {
                    blank(i, i + n);
                    i += n;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset].iter().filter(|&&c| c == b'\n').count() + 1
}

/// Byte offsets of `name(` in blanked code: the word alone (not part of a longer identifier), white space allowed before the paren.
fn calls(code: &str, name: &str) -> Vec<usize> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = code[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let mut j = from;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if before_ok && b.get(j) == Some(&b'(') {
            out.push(at);
        }
    }
    out
}

/// True when the code names the dock's picker: `i18n::pick`, or `pick` inside a `use crate::i18n::{..}` list. Other
/// functions and methods that happen to be called pick (a random choice in a test, a local helper) are not the picker.
fn names_the_picker(code: &str) -> bool {
    code.contains("i18n::pick")
        || code
            .match_indices("i18n::{")
            .any(|(at, _)| code[at..].split('}').next().is_some_and(|l| calls_word(l, "pick")))
}

/// True when `word` stands alone in `s`.
fn calls_word(s: &str, word: &str) -> bool {
    let b = s.as_bytes();
    s.match_indices(word).any(|(at, _)| {
        let before = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let after = !b.get(at + word.len()).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        before && after
    })
}

/// L1: the lines of every `tr(` token, and of a free call of the dock's `pick` outside its three files.
fn l1_violations(path: &str, text: &str) -> Vec<String> {
    let code = code_only(text);
    let mut out: Vec<String> =
        calls(&code, "tr").into_iter().map(|o| format!("{path}:{}: tr(", line_of(text, o))).collect();
    if !PICK_FILES.contains(&path) && names_the_picker(&code) {
        let free = |o: &usize| {
            let before = code[..*o].trim_end();
            !before.ends_with('.') && !before.ends_with("fn")
        };
        out.extend(
            calls(&code, "pick").into_iter().filter(free).map(|o| format!("{path}:{}: pick(", line_of(text, o))),
        );
    }
    out
}

/// L2: the lines of an `italian()` call outside src/i18n.rs and outside a test module.
fn l2_violations(path: &str, text: &str) -> Vec<String> {
    if path == "src/i18n.rs" {
        return Vec::new();
    }
    let code = code_only(text);
    let tests_from = code
        .match_indices("#[cfg(test)]")
        .find(|(at, m)| code[at + m.len()..].trim_start().starts_with("mod tests"))
        .map_or(usize::MAX, |(at, _)| at);
    calls(&code, "italian")
        .into_iter()
        .filter(|&o| o < tests_from && !code[..o].trim_end().ends_with("fn"))
        .map(|o| format!("{path}:{}: italian()", line_of(text, o)))
        .collect()
}

/// The constant name of a key (the generator's rule): upper case, dots to underscores.
fn const_name(key: &str) -> String {
    key.to_ascii_uppercase().replace('.', "_")
}

/// The keys of a strings file with their texts and line numbers (comment and blank lines skipped).
fn toml_entries(text: &str) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else { continue };
        let Some(close) = rest.find('"') else { continue };
        let value = rest[close + 1..].trim_start().trim_start_matches('=').trim();
        let value = value.strip_prefix('"').and_then(|v| v.rfind('"').map(|e| &v[..e])).unwrap_or("");
        out.push((rest[..close].to_string(), value.to_string(), i + 1));
    }
    out
}

/// The constants referenced as `k::CONST` in blanked code.
fn k_refs(code: &str) -> BTreeSet<String> {
    let b = code.as_bytes();
    let mut out = BTreeSet::new();
    let mut from = 0;
    while let Some(p) = code[from..].find("k::") {
        let at = from + p;
        from = at + 3;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_') {
            continue;
        }
        let name: String = code[from..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            out.insert(name);
        }
    }
    out
}

/// L3: the keys of en.toml that no `k::<CONST>` references.
fn l3_dead_keys(en: &str, sources: &[(String, String)]) -> Vec<String> {
    let mut used = BTreeSet::new();
    for (_, text) in sources {
        used.extend(k_refs(&code_only(text)));
    }
    toml_entries(en).into_iter().filter(|(k, _, _)| !used.contains(&const_name(k))).map(|(k, _, _)| k).collect()
}

/// L4: keys present in one file only, and empty texts, each with its line.
fn l4_problems(en: &str, it: &str) -> Vec<String> {
    let (e, i) = (toml_entries(en), toml_entries(it));
    let mut out = Vec::new();
    for (name, own, other) in [("en.toml", &e, &i), ("it.toml", &i, &e)] {
        for (k, v, line) in own {
            if other.iter().all(|(o, _, _)| o != k) {
                out.push(format!("{name}:{line}: key {k} is missing from the other file"));
            }
            if v.is_empty() {
                out.push(format!("{name}:{line}: key {k} has an empty text"));
            }
        }
    }
    out
}

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, String)>) {
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).map(|e| e.file_name()).collect();
        names.sort();
        for n in names {
            let n = n.to_string_lossy().into_owned();
            let p = dir.join(&n);
            if p.is_dir() {
                walk(&p, &format!("{rel}/{n}"), out);
            } else if n.ends_with(".rs") {
                out.push((format!("{rel}/{n}"), std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join("src"), "src", &mut out);
    out
}

fn strings(name: &str) -> String {
    std::fs::read_to_string(root().join("strings").join(name)).unwrap_or_else(|e| panic!("strings/{name}: {e}"))
}

/// Prints the findings of one rule; fails on any when the migration is closed.
fn report(rule: &str, found: &[String]) {
    println!("{rule}: {} finding(s)", found.len());
    if ENFORCE {
        assert!(found.is_empty(), "{rule}:\n{}", found.join("\n"));
    }
}

#[test]
fn l1_flags_tr_and_pick_outside_their_files() {
    let bad = "use crate::i18n::{pick, t};\nfn f() { let a = tr(\"A\", \"B\"); let b = crate::i18n::tr (\"A\", \"B\"); let c = pick(\"A\", \"B\"); }\n";
    assert_eq!(l1_violations("src/a.rs", bad).len(), 3);
    let qualified = "fn f() { let c = crate::i18n::pick(\"A\", \"B\"); }\n";
    assert_eq!(l1_violations("src/a.rs", qualified).len(), 1);
    let other = "fn pick(i: usize) -> usize { i }\nfn f(r: &mut R) { let _ = pick(1); let _ = r.pick(&[1]); }\n";
    assert!(l1_violations("src/a.rs", other).is_empty(), "a helper or a method named pick is not the picker");
    assert_eq!(l1_violations("src/launcher/runline.rs", bad).len(), 2, "pick is allowed there");
    let ok = "// tr(\"A\", \"B\")\nfn f() { let _ = \"tr(x)\"; let _ = str(1); let _ = attr(2); let _ = t(k::X); }\n";
    assert!(l1_violations("src/a.rs", ok).is_empty());
}

#[test]
fn l2_flags_italian_outside_i18n_and_test_modules() {
    let bad = "fn f() -> bool { crate::i18n::italian() }\n#[cfg(test)]\nmod tests {\n    fn g() -> bool { crate::i18n::italian() }\n}\n";
    assert_eq!(l2_violations("src/a.rs", bad), ["src/a.rs:1: italian()"]);
    assert!(l2_violations("src/i18n.rs", bad).is_empty());
    assert!(l2_violations("src/a.rs", "// italian()\nfn italian() {}\n").is_empty());
}

#[test]
fn l3_names_every_dead_key() {
    let en = "# header\n\"a.used\" = \"One\"\n\"a.dead\" = \"Two\"\n\"b.used_too\" = \"Three\"\n";
    let src = vec![(
        "src/a.rs".to_string(),
        "fn f() { t(k::A_USED); t(crate::i18n::k::B_USED_TOO); } // k::A_DEAD\n".to_string(),
    )];
    assert_eq!(l3_dead_keys(en, &src), ["a.dead"]);
}

#[test]
fn l4_names_the_line_of_every_problem() {
    let en = "\"a.one\" = \"One\"\n\"a.two\" = \"\"\n\"a.three\" = \"Three\"\n";
    let it = "\"a.one\" = \"Uno\"\n\"a.two\" = \"Due\"\n\"a.four\" = \"Quattro\"\n";
    let p = l4_problems(en, it);
    assert_eq!(
        p,
        [
            "en.toml:2: key a.two has an empty text",
            "en.toml:3: key a.three is missing from the other file",
            "it.toml:3: key a.four is missing from the other file",
        ]
    );
    assert!(
        l4_problems(&en.replace("\"\"", "\"Due\"").replace("three", "four").replace("Three", "Quattro"), it).is_empty()
    );
}

#[test]
fn the_tree_follows_the_rules() {
    let src = sources();
    let (en, it) = (strings("en.toml"), strings("it.toml"));
    let l1: Vec<String> = src.iter().flat_map(|(p, t)| l1_violations(p, t)).collect();
    let tr_count = l1.iter().filter(|l| l.ends_with("tr(")).count();
    let pick_count = l1.len() - tr_count;
    println!("inventory: {tr_count} tr( tokens in src/ ({} outside src/i18n.rs), {pick_count} pick( calls outside their files", l1.iter().filter(|l| l.ends_with("tr(") && !l.starts_with("src/i18n.rs:")).count());
    report("L1", &l1);
    let l2: Vec<String> = src.iter().flat_map(|(p, t)| l2_violations(p, t)).collect();
    report("L2", &l2);
    let l3 = l3_dead_keys(&en, &src);
    report("L3 (run i18n-codemod prune)", &l3.iter().map(|k| format!("dead key {k}")).collect::<Vec<_>>());
    let l4 = l4_problems(&en, &it);
    // L4 holds today (the generator refuses a file pair that breaks it): it is enforced from the start.
    assert!(l4.is_empty(), "L4:\n{}", l4.join("\n"));
    println!("inventory: {} keys in strings/en.toml", toml_entries(&en).len());
}
