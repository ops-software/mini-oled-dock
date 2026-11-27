//! The 17 tests of the tool.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::ops::{self, Old};

const HEADER: &str = "# Strings of the test tree.\n";

struct Tree(PathBuf);

impl Tree {
    fn new(files: &[(&str, &str)], en: &str, it: &str) -> Tree {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "i18n-codemod-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("strings")).unwrap();
        let t = Tree(dir);
        for (p, text) in files {
            t.put(p, text);
        }
        fs::write(t.0.join("strings/en.toml"), format!("{HEADER}\n{en}")).unwrap();
        fs::write(t.0.join("strings/it.toml"), format!("{HEADER}\n{it}")).unwrap();
        t
    }
    fn put(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn get(&self, rel: &str) -> String {
        fs::read_to_string(self.0.join(rel)).unwrap()
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Snapshot(HashMap<String, String>);

impl Old for Snapshot {
    fn files(&self) -> Result<Vec<String>, String> {
        Ok(self.0.keys().cloned().collect())
    }
    fn read(&self, rel: &str) -> Result<String, String> {
        self.0.get(rel).cloned().ok_or_else(|| format!("{rel}: not in the snapshot"))
    }
}

fn snapshot(t: &Tree, files: &[&str]) -> Snapshot {
    Snapshot(files.iter().map(|f| (f.to_string(), t.get(f))).collect())
}

fn toml_lines(t: &Tree, name: &str) -> Vec<String> {
    t.get(name).lines().filter(|l| l.starts_with('"')).map(str::to_string).collect()
}

#[test]
fn a_plain_pair_becomes_a_keyed_call() {
    let t = Tree::new(&[("src/menu.rs", "fn f() -> &'static str {\n    tr(\"Open\", \"Apri\")\n}\n")], "", "");
    let r = ops::apply(t.path()).unwrap();
    assert_eq!((r.converted, r.new_keys), (1, 1));
    assert!(t.get("src/menu.rs").contains("t(k::MENU_OPEN)"));
    assert_eq!(toml_lines(&t, "strings/en.toml"), ["\"menu.open\" = \"Open\""]);
    assert_eq!(toml_lines(&t, "strings/it.toml"), ["\"menu.open\" = \"Apri\""]);
}

#[test]
fn escapes_decode_and_the_ellipsis_is_written_as_the_character() {
    let t = Tree::new(
        &[("src/a.rs", "fn f() { let _ = tr(\"Wait\\u{2026} \\\"x\\\"\\n\", \"Attendi\\u{2026}\\\\\"); }\n")],
        "",
        "",
    );
    let rows = ops::extract(t.path(), None, None).unwrap().rows;
    assert_eq!(rows[0].site.en, "Wait\u{2026} \"x\"\n");
    assert_eq!(rows[0].site.it, "Attendi\u{2026}\\");
    ops::apply(t.path()).unwrap();
    let en = toml_lines(&t, "strings/en.toml");
    assert_eq!(en, ["\"a.wait_x\" = \"Wait\u{2026} \\\"x\\\"\\n\""]);
    assert_eq!(toml_lines(&t, "strings/it.toml"), ["\"a.wait_x\" = \"Attendi\u{2026}\\\\\""]);
}

#[test]
fn a_call_over_three_lines_is_one_site() {
    let src = "fn f() {\n    let _ = tr(\n        \"Long text\",\n        \"Testo lungo\",\n    );\n}\n";
    let t = Tree::new(&[("src/a.rs", src)], "", "");
    let e = ops::extract(t.path(), Some(1), None).unwrap();
    assert_eq!((e.rows.len(), e.rows[0].site.line, e.skipped.len()), (1, 2, 0));
    ops::apply(t.path()).unwrap();
    assert_eq!(t.get("src/a.rs"), "fn f() {\n    let _ = t(k::A_LONG_TEXT);\n}\n");
}

#[test]
fn tr_inside_a_string_or_a_comment_is_not_a_site() {
    let src = "// tr(\"a\", \"b\")\n/* tr(\"c\", \"d\") /* nested */ tr(\"e\", \"f\") */\nfn f() { let _ = \"tr(\\\"g\\\", \\\"h\\\")\"; let _ = r#\"tr(\"i\", \"j\")\"#; let _ = 'x'; let _ = str(1); }\n";
    let t = Tree::new(&[("src/a.rs", src)], "", "");
    let e = ops::extract(t.path(), Some(0), None).unwrap();
    assert_eq!((e.rows.len(), e.skipped.len(), e.independent), (0, 0, 0));
}

#[test]
fn an_inline_path_keeps_its_prefix() {
    let src = "fn f() -> &'static str { crate::i18n::tr(\"Save\", \"Salva\") }\nfn g() -> &'static str { i18n::tr(\"Save\", \"Salva\") }\n";
    let t = Tree::new(&[("src/a.rs", src)], "", "");
    ops::apply(t.path()).unwrap();
    assert_eq!(
        t.get("src/a.rs"),
        "fn f() -> &'static str { crate::i18n::t(crate::i18n::k::A_SAVE) }\nfn g() -> &'static str { i18n::t(i18n::k::A_SAVE) }\n"
    );
    // A local alias of the function becomes a block-scoped import.
    t.put("src/b.rs", "fn h() -> &'static str {\n    let tr = crate::i18n::tr;\n    tr(\"Save\", \"Salva\")\n}\n");
    ops::apply(t.path()).unwrap();
    assert_eq!(t.get("src/b.rs"), "fn h() -> &'static str {\n    use crate::i18n::{k, t};\n    t(k::B_SAVE)\n}\n");
}

#[test]
fn a_non_literal_call_is_skipped_and_listed_and_keeps_its_import() {
    let src = "use crate::i18n::tr;\nstruct P { en: &'static str, it: &'static str }\nimpl P {\n    fn a(&self) -> &'static str { tr(self.en, self.it) }\n    fn b(&self) -> &'static str { tr(\"X\", \"Y\") }\n}\n";
    let t = Tree::new(&[("src/launcher/commands.rs", src)], "", "");
    let e = ops::extract(t.path(), None, None).unwrap();
    assert_eq!((e.rows.len(), e.skipped), (1, vec!["src/launcher/commands.rs:4".to_string()]));
    ops::apply(t.path()).unwrap();
    let out = t.get("src/launcher/commands.rs");
    assert!(out.starts_with("use crate::i18n::{k, t, tr};\n"), "{out}");
    assert!(out.contains("tr(self.en, self.it)") && out.contains("t(k::LAUNCHER_COMMANDS_X)"));
}

#[test]
fn a_brace_list_import_is_merged() {
    let src = "use crate::i18n::{t, tr, with_italian};\nfn f() { let _ = tr(\"A\", \"B\"); }\n";
    let t = Tree::new(&[("src/a.rs", src)], "", "");
    ops::apply(t.path()).unwrap();
    assert!(t.get("src/a.rs").starts_with("use crate::i18n::{k, t, with_italian};\n"), "{}", t.get("src/a.rs"));
    let single = Tree::new(&[("src/b.rs", "use crate::i18n::tr;\nfn f() { let _ = tr(\"A\", \"B\"); }\n")], "", "");
    ops::apply(single.path()).unwrap();
    assert!(single.get("src/b.rs").starts_with("use crate::i18n::{k, t};\n"));
}

#[test]
fn one_item_with_two_pairs_gets_a_numbered_key() {
    let src = "fn f() { tr(\"Open\", \"Apri\"); tr(\"Open!\", \"Apri!\"); tr(\"Open\", \"Apri\"); tr(\"Open?\", \"Apri?\"); }\n";
    let t = Tree::new(&[("src/a.rs", src)], "", "");
    ops::apply(t.path()).unwrap();
    assert_eq!(t.get("src/a.rs"), "fn f() { t(k::A_OPEN); t(k::A_OPEN_2); t(k::A_OPEN); t(k::A_OPEN_3); }\n");
}

#[test]
fn a_pair_in_two_files_becomes_a_common_key() {
    let t = Tree::new(
        &[
            ("src/popup_audio.rs", "fn f() { tr(\"Close\", \"Chiudi\"); tr(\"Mute\", \"Silenzia\"); }\n"),
            ("src/settings_view.rs", "fn g() { tr(\"Close\", \"Chiudi\"); }\n"),
        ],
        "",
        "",
    );
    ops::apply(t.path()).unwrap();
    assert!(t.get("src/popup_audio.rs").contains("t(k::COMMON_CLOSE); t(k::AUDIO_MUTE)"));
    assert!(t.get("src/settings_view.rs").contains("t(k::COMMON_CLOSE)"));
    assert_eq!(toml_lines(&t, "strings/en.toml"), ["\"audio.mute\" = \"Mute\"", "\"common.close\" = \"Close\""]);
}

#[test]
fn a_lone_glyph_is_keyed_by_its_ordinal() {
    let t = Tree::new(&[("src/a.rs", "fn f() { tr(\"A\", \"B\"); tr(\"\\u{2715}\", \"\\u{2715}\"); }\n")], "", "");
    ops::apply(t.path()).unwrap();
    assert!(t.get("src/a.rs").contains("t(k::A_S2)"), "{}", t.get("src/a.rs"));
    assert!(toml_lines(&t, "strings/en.toml").contains(&"\"a.s2\" = \"\u{2715}\"".to_string()));
}

#[test]
fn apply_twice_equals_apply_once() {
    let t = Tree::new(
        &[
            ("src/a.rs", "use crate::i18n::tr;\nfn f() { tr(\"One\", \"Uno\"); tr(\"Two\", \"Due\"); }\n"),
            ("src/b.rs", "fn g() { crate::i18n::tr(\"One\", \"Uno\"); }\n"),
        ],
        "",
        "",
    );
    ops::apply(t.path()).unwrap();
    let once: Vec<String> =
        ["src/a.rs", "src/b.rs", "strings/en.toml", "strings/it.toml"].iter().map(|f| t.get(f)).collect();
    let r = ops::apply(t.path()).unwrap();
    assert_eq!((r.converted, r.new_keys), (0, 0));
    let twice: Vec<String> =
        ["src/a.rs", "src/b.rs", "strings/en.toml", "strings/it.toml"].iter().map(|f| t.get(f)).collect();
    assert_eq!(once, twice);
}

#[test]
fn a_site_added_on_a_rebased_branch_gets_a_new_key_and_no_old_key_changes() {
    let t = Tree::new(&[("src/a.rs", "use crate::i18n::tr;\nfn f() { tr(\"One\", \"Uno\"); }\n")], "", "");
    ops::apply(t.path()).unwrap();
    let (en0, it0) = (t.get("strings/en.toml"), t.get("strings/it.toml"));
    t.put(
        "src/a.rs",
        &format!(
            "{}fn g() {{ tr(\"Three\", \"Tre\"); tr(\"One\", \"Uno\"); }}\n",
            t.get("src/a.rs").replace("use crate::i18n::{k, t};", "use crate::i18n::{k, t, tr};")
        ),
    );
    let r = ops::apply(t.path()).unwrap();
    assert_eq!((r.new_keys, r.reused), (1, 1));
    let (en1, it1) = (t.get("strings/en.toml"), t.get("strings/it.toml"));
    for line in en0.lines().chain(it0.lines()) {
        assert!(en1.contains(line) || it1.contains(line), "old line lost: {line}");
    }
    assert!(en1.contains("\"a.three\" = \"Three\"") && it1.contains("\"a.three\" = \"Tre\""));
}

#[test]
fn verify_catches_a_changed_italian_text() {
    let t = Tree::new(&[("src/a.rs", "fn f() { tr(\"One\", \"Uno\"); tr(\"Two\", \"Due\"); }\n")], "", "");
    let old = snapshot(&t, &["src/a.rs"]);
    ops::apply(t.path()).unwrap();
    assert_eq!(ops::verify(t.path(), &old, &[]), Ok(2));
    let it = t.get("strings/it.toml").replace("\"Due\"", "\"Due!\"");
    fs::write(t.path().join("strings/it.toml"), it).unwrap();
    let bad = ops::verify(t.path(), &old, &[]).unwrap_err();
    assert_eq!(bad.len(), 1);
    assert!(bad[0].contains("Due!") && bad[0].contains("site 2"), "{bad:?}");
}

#[test]
fn extract_stops_on_a_token_it_did_not_count() {
    let t = Tree::new(
        &[("src/a.rs", "fn f() { tr(\"One\", \"Uno\"); }\n"), ("src/i18n.rs", "pub fn tr(en: &str, it: &str) {}\n")],
        "",
        "",
    );
    let ok = ops::extract(t.path(), Some(1), None).unwrap();
    assert_eq!((ok.rows.len(), ok.own, ok.independent), (1, 1, 2));
    // A counter that sees one more token than the lexer does: the cross-check fails.
    let err = ops::extract_with(t.path(), None, None, |s| ops::independent_count(s) + 1).unwrap_err();
    assert!(err.contains("self-check failed"), "{err}");
    assert!(ops::extract(t.path(), Some(2), None).unwrap_err().contains("expected 2"));
}

#[test]
fn tidy_sorts_the_union_and_fails_on_one_key_with_two_texts() {
    let en = "\"b.two\" = \"Two\"\n\"a.one\" = \"One\"\n\"b.two\" = \"Two\"\n";
    let it = "\"b.two\" = \"Due\"\n\"a.one\" = \"Uno\"\n";
    let t = Tree::new(&[], en, it);
    assert_eq!(ops::tidy(t.path()), Ok(2));
    assert_eq!(toml_lines(&t, "strings/en.toml"), ["\"a.one\" = \"One\"", "\"b.two\" = \"Two\""]);
    assert_eq!(toml_lines(&t, "strings/it.toml"), ["\"a.one\" = \"Uno\"", "\"b.two\" = \"Due\""]);
    assert!(t.get("strings/en.toml").starts_with(HEADER));
    let clash = Tree::new(&[], "\"a.one\" = \"One\"\n\"a.one\" = \"1\"\n", "\"a.one\" = \"Uno\"\n");
    let err = ops::tidy(clash.path()).unwrap_err();
    assert!(err.contains("a.one") && err.contains("One") && err.contains("\"1\""), "{err}");
}

#[test]
fn prune_deletes_exactly_the_unreferenced_keys() {
    let t = Tree::new(
        &[("src/a.rs", "fn f() { let _ = t(k::A_ONE); let _ = crate::i18n::k::B_TWO; }\n// k::A_DEAD in a comment is not a reference\n")],
        "\"a.dead\" = \"Dead\"\n\"a.one\" = \"One\"\n\"b.two\" = \"Two\"\n\"c.three\" = \"Three\"\n",
        "\"a.dead\" = \"Morto\"\n\"a.one\" = \"Uno\"\n\"b.two\" = \"Due\"\n\"c.three\" = \"Tre\"\n",
    );
    assert_eq!(ops::prune(t.path()).unwrap(), ["a.dead", "c.three"]);
    assert_eq!(toml_lines(&t, "strings/en.toml"), ["\"a.one\" = \"One\"", "\"b.two\" = \"Two\""]);
    assert_eq!(toml_lines(&t, "strings/it.toml"), ["\"a.one\" = \"Uno\"", "\"b.two\" = \"Due\""]);
}

#[test]
fn a_rebased_branch_that_changed_a_text_gets_a_new_key_and_the_old_one_is_pruned() {
    let main = "use crate::i18n::tr;\nfn f() { tr(\"Save\", \"Salva\"); tr(\"Quit\", \"Esci\"); }\n";
    let t = Tree::new(&[("src/a.rs", main)], "", "");
    let old = snapshot(&t, &["src/a.rs"]);
    ops::apply(t.path()).unwrap();
    assert_eq!(ops::verify(t.path(), &old, &[]), Ok(2));
    // The branch changed the English text of the first line: it takes its own line, which is a tr site again.
    let rebased = t
        .get("src/a.rs")
        .replace("t(k::A_SAVE)", "tr(\"Save as\", \"Salva\")")
        .replace("use crate::i18n::{k, t};", "use crate::i18n::{k, t, tr};");
    t.put("src/a.rs", &rebased);
    let (en_before, it_before) = (toml_lines(&t, "strings/en.toml"), toml_lines(&t, "strings/it.toml"));
    let r = ops::apply(t.path()).unwrap();
    assert_eq!((r.new_keys, r.reused), (1, 0));
    assert_eq!(ops::prune(t.path()).unwrap(), ["a.save"]);
    assert_eq!(t.get("src/a.rs"), "use crate::i18n::{k, t};\nfn f() { t(k::A_SAVE_AS); t(k::A_QUIT); }\n");
    let expect = |before: &[String], new_key: &str| {
        let mut v: Vec<String> = before.iter().filter(|l| !l.starts_with("\"a.save\"")).cloned().collect();
        v.push(new_key.to_string());
        v.sort();
        v
    };
    assert_eq!(toml_lines(&t, "strings/en.toml"), expect(&en_before, "\"a.save_as\" = \"Save as\""));
    assert_eq!(toml_lines(&t, "strings/it.toml"), expect(&it_before, "\"a.save_as\" = \"Salva\""));
}
