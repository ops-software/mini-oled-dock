//! Build-time generator of the keyed string tables (std only; FZ-I18N of the workspace plan).
//!
//! A crate keeps `strings/en.toml` and `strings/it.toml`, a strict subset of TOML: comment lines (`#`), blank lines and
//! lines `"area.item" = "text"` whose text may use the escapes \" \\ \n \t \uXXXX and plain UTF-8 otherwise.
//! `generate` turns the pair into `$OUT_DIR/strings.rs`: a `k` module of `S` constants and a `TABLE` of
//! `oled_sys::i18n::Table`, sorted by hash.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;

/// FNV-1a 64 of a key name; the same function as `oled_sys::i18n::fnv1a64` (this crate has no dependency).
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Reads `<strings_dir>/en.toml` and `it.toml` and writes the generated tables to `out_file`; returns the key count.
/// Prints the `cargo:rerun-if-changed` lines for both files.
pub fn generate(strings_dir: &Path, out_file: &Path) -> Result<usize, String> {
    let mut parsed = Vec::new();
    for name in ["en.toml", "it.toml"] {
        let path = strings_dir.join(name);
        println!("cargo:rerun-if-changed={}", path.display().to_string().replace('\\', "/"));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let entries = parse(&text).map_err(|e| format!("{}:{e}", path.display()))?;
        parsed.push(entries);
    }
    let it = parsed.pop().unwrap_or_default();
    let en = parsed.pop().unwrap_or_default();
    check(&en, &it)?;
    let out = emit(&en, &it);
    std::fs::write(out_file, out).map_err(|e| format!("{}: {e}", out_file.display()))?;
    Ok(en.len())
}

/// True for `[a-z0-9_]+(\.[a-z0-9_]+)+`.
fn valid_key(k: &str) -> bool {
    let mut parts = 0;
    for p in k.split('.') {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return false;
        }
        parts += 1;
    }
    parts >= 2
}

/// Parses one strings file: the (key, text) pairs in file order. Errors read "<line>: <what>".
fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let rest = line.strip_prefix('"').ok_or_else(|| format!("{n}: a line must be \"key\" = \"text\""))?;
        let close = rest.find('"').ok_or_else(|| format!("{n}: unterminated key"))?;
        let key = &rest[..close];
        if !valid_key(key) {
            return Err(format!("{n}: bad key \"{key}\" (want area.item in [a-z0-9_])"));
        }
        let rest = rest[close + 1..].trim_start();
        let rest = rest.strip_prefix('=').ok_or_else(|| format!("{n}: missing = after the key"))?.trim_start();
        let body = rest.strip_prefix('"').ok_or_else(|| format!("{n}: the text must start with a quote"))?;
        let mut text = String::new();
        let mut chars = body.chars();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next() {
                    Some('"') => text.push('"'),
                    Some('\\') => text.push('\\'),
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                            return Err(format!("{n}: \\u wants four hex digits"));
                        }
                        let cp = u32::from_str_radix(&hex, 16).map_err(|e| format!("{n}: {e}"))?;
                        text.push(char::from_u32(cp).ok_or_else(|| format!("{n}: \\u{hex} is not a character"))?);
                    }
                    Some(o) => return Err(format!("{n}: unknown escape \\{o}")),
                    None => return Err(format!("{n}: unterminated string")),
                },
                c => text.push(c),
            }
        }
        if !closed {
            return Err(format!("{n}: unterminated string"));
        }
        let tail = chars.as_str().trim();
        if !tail.is_empty() && !tail.starts_with('#') {
            return Err(format!("{n}: text after the closing quote"));
        }
        out.push((key.to_string(), text));
    }
    Ok(out)
}

/// The sorted, de-duplicated `{name}` placeholders of a text.
fn placeholders(text: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && close > 0 => {
                set.insert(after[..close].to_string());
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    set
}

/// The constant name of a key: upper case, dots to underscores.
fn const_name(key: &str) -> String {
    key.to_ascii_uppercase().replace('.', "_")
}

/// Every failure of FZ-I18N over the two parsed files.
fn check(en: &[(String, String)], it: &[(String, String)]) -> Result<(), String> {
    let mut maps: [HashMap<&str, &str>; 2] = [HashMap::new(), HashMap::new()];
    for (side, (file, entries)) in [("en.toml", en), ("it.toml", it)].into_iter().enumerate() {
        for (k, v) in entries {
            if maps[side].insert(k, v).is_some() {
                return Err(format!("{file}: duplicate key \"{k}\""));
            }
            if v.is_empty() {
                return Err(format!("{file}: empty value for \"{k}\""));
            }
        }
    }
    for (k, _) in en {
        if !maps[1].contains_key(k.as_str()) {
            return Err(format!("key \"{k}\" is in en.toml only"));
        }
    }
    for (k, _) in it {
        if !maps[0].contains_key(k.as_str()) {
            return Err(format!("key \"{k}\" is in it.toml only"));
        }
    }
    let mut hashes: HashMap<u64, &str> = HashMap::new();
    let mut names: HashMap<String, &str> = HashMap::new();
    for (k, e) in en {
        if placeholders(e) != placeholders(maps[1][k.as_str()]) {
            return Err(format!("key \"{k}\": the {{placeholders}} of en.toml and it.toml differ"));
        }
        if let Some(other) = hashes.insert(fnv1a64(k), k) {
            return Err(format!("keys \"{other}\" and \"{k}\" have the same hash"));
        }
        if let Some(other) = names.insert(const_name(k), k) {
            return Err(format!("keys \"{other}\" and \"{k}\" have the same constant name"));
        }
    }
    Ok(())
}

/// The generated module: `k` constants and `TABLE`, entries sorted by hash, one blob per language.
fn emit(en: &[(String, String)], it: &[(String, String)]) -> String {
    let it_of: HashMap<&str, &str> = it.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut rows: Vec<(u64, &str, &str, &str)> = en
        .iter()
        .map(|(k, v)| (fnv1a64(k), k.as_str(), v.as_str(), it_of.get(k.as_str()).copied().unwrap_or("")))
        .collect();
    rows.sort();
    let mut out = String::from("// Generated by oled-i18n-build from strings/*.toml. Do not edit.\n\n");
    out.push_str("#[allow(dead_code)]\npub mod k {\n");
    for (h, key, text, _) in &rows {
        let doc = text.replace('\n', "\\n").replace('\t', "\\t").replace('\r', "\\r");
        let _ = writeln!(out, "    /// {doc}");
        let _ = writeln!(out, "    pub const {}: oled_sys::i18n::S = oled_sys::i18n::S({h:#018x});", const_name(key));
    }
    out.push_str("}\n\n");
    let strings = |italian: bool| {
        let (mut blob, mut spans) = (String::new(), String::new());
        for r in &rows {
            let t = if italian { r.3 } else { r.2 };
            let _ = write!(spans, "({}, {}), ", blob.len(), t.len());
            blob.push_str(t);
        }
        format!("oled_sys::i18n::Strings {{ blob: {blob:?}, spans: &[{}] }}", spans.trim_end_matches([',', ' ']))
    };
    let hashes: Vec<String> = rows.iter().map(|r| format!("{:#018x}", r.0)).collect();
    let _ = writeln!(out, "pub static TABLE: oled_sys::i18n::Table = oled_sys::i18n::Table {{");
    let _ = writeln!(out, "    hashes: &[{}],", hashes.join(", "));
    let _ = writeln!(out, "    en: {},", strings(false));
    let _ = writeln!(out, "    it: {},", strings(true));
    out.push_str("};\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temp directory removed on drop.
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Dir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "oled-i18n-build-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(k, t)| (k.to_string(), t.to_string())).collect()
    }

    #[test]
    fn parse_accepts_the_subset() {
        let text = "# header\n\n\"a.b\" = \"plain\"\n  \"c.d_e\"=\"q\\\" b\\\\ n\\n t\\t u\\u00e8 \u{e0}\"  # tail\n";
        let got = parse(text).unwrap();
        assert_eq!(got, pairs(&[("a.b", "plain"), ("c.d_e", "q\" b\\ n\n t\t u\u{e8} \u{e0}")]));
    }

    #[test]
    fn parse_rejects_what_is_not_the_subset() {
        for bad in [
            "a.b = \"x\"",
            "[table]",
            "\"a.b\" = \"open",
            "\"a.b\" = \"\\u{2026}\"",
            "\"a.b\" = \"\\q\"",
            "\"ab\" = \"x\"",
            "\"A.b\" = \"x\"",
            "\"a.b\" = \"x\" y",
            "\"a.b\" \"x\"",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("# ok\n\n[x]").unwrap_err().starts_with("3:"), "errors name the line");
    }

    #[test]
    fn check_rejects() {
        let ok = pairs(&[("a.b", "x {n}")]);
        assert!(check(&ok, &pairs(&[("a.b", "y {n}")])).is_ok());
        assert!(check(&pairs(&[("a.b", "x"), ("a.b", "y")]), &pairs(&[("a.b", "x")]))
            .unwrap_err()
            .contains("duplicate"));
        assert!(check(&ok, &[]).unwrap_err().contains("en.toml only"));
        assert!(check(&[], &ok).unwrap_err().contains("it.toml only"));
        assert!(check(&pairs(&[("a.b", "")]), &pairs(&[("a.b", "y")])).unwrap_err().contains("empty"));
        assert!(check(&pairs(&[("a.b", "{n}")]), &pairs(&[("a.b", "y")])).unwrap_err().contains("placeholders"));
        assert!(check(&pairs(&[("a.b", "x")]), &pairs(&[("a.b", "{n}")])).unwrap_err().contains("placeholders"));
        // Two keys with one constant name: "a_b.c" and "a.b_c" both give A_B_C.
        let both = pairs(&[("a_b.c", "x"), ("a.b_c", "y")]);
        assert!(check(&both, &both).unwrap_err().contains("constant name"));
    }

    #[test]
    fn const_name_of_a_key() {
        assert_eq!(const_name("launcher_strings.placeholder"), "LAUNCHER_STRINGS_PLACEHOLDER");
        assert_eq!(const_name("a.b.c"), "A_B_C");
    }

    #[test]
    fn fnv1a64_matches_the_runtime() {
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64("foobar"), 0x8594_4171_f739_67e8);
    }

    /// The hex numbers inside `&[ ... ]` after `marker`.
    fn hex_list(text: &str, marker: &str) -> Vec<u64> {
        let start = text.find(marker).unwrap() + marker.len();
        let body = &text[start..start + text[start..].find(']').unwrap()];
        body.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| u64::from_str_radix(&s[2..], 16).unwrap())
            .collect()
    }

    #[test]
    fn emit_sorts_by_hash_and_spans_the_blob() {
        let en = pairs(&[("t.zeta", "Zed"), ("t.alpha", "Alpha"), ("t.mid", "Middle"), ("t.last", "L")]);
        let it = pairs(&[("t.zeta", "Zeta"), ("t.alpha", "Alfa"), ("t.mid", "Mezzo"), ("t.last", "U")]);
        let text = emit(&en, &it);
        let hashes = hex_list(&text, "hashes: &[");
        assert_eq!(hashes.len(), 4);
        assert!(hashes.windows(2).all(|w| w[0] < w[1]), "ascending");
        for (lang, table) in [("en", &en), ("it", &it)] {
            let at = text.find(&format!("{lang}: oled_sys::i18n::Strings {{ blob: \"")).unwrap();
            let blob_start = text[at..].find('"').unwrap() + at + 1;
            let blob_end = text[blob_start..].find('"').unwrap() + blob_start;
            let blob = &text[blob_start..blob_end];
            let spans_at = text[blob_end..].find("spans: &[").unwrap() + blob_end + "spans: &[".len();
            let spans_txt = &text[spans_at..spans_at + text[spans_at..].find(']').unwrap()];
            let spans: Vec<(usize, usize)> = spans_txt
                .split("), (")
                .map(|p| p.trim_matches(|c| c == '(' || c == ')' || c == ' '))
                .map(|p| {
                    let (a, b) = p.split_once(", ").unwrap();
                    (a.parse().unwrap(), b.parse().unwrap())
                })
                .collect();
            assert_eq!(spans.len(), 4);
            for (h, (off, len)) in hashes.iter().zip(spans) {
                let want = table.iter().find(|(k, _)| fnv1a64(k) == *h).unwrap();
                assert_eq!(&blob[off..off + len], want.1, "{lang} {}", want.0);
            }
        }
        assert!(text.contains("pub const T_ALPHA: oled_sys::i18n::S = oled_sys::i18n::S("));
    }

    #[test]
    fn generate_writes_the_file_and_counts_keys() {
        let d = Dir::new();
        std::fs::write(d.0.join("en.toml"), "# c\n\"a.b\" = \"One\"\n\"a.c\" = \"Two\"\n").unwrap();
        std::fs::write(d.0.join("it.toml"), "\"a.b\" = \"Uno\"\n\"a.c\" = \"Due\"\n").unwrap();
        let out = d.0.join("strings.rs");
        assert_eq!(generate(&d.0, &out), Ok(2));
        assert!(std::fs::read_to_string(&out).unwrap().contains("pub static TABLE"));
    }

    #[test]
    fn generate_fails_on_a_missing_file_and_accepts_two_empty_ones() {
        let d = Dir::new();
        let out = d.0.join("strings.rs");
        std::fs::write(d.0.join("en.toml"), "").unwrap();
        assert!(generate(&d.0, &out).unwrap_err().contains("it.toml"));
        std::fs::write(d.0.join("it.toml"), "").unwrap();
        assert_eq!(generate(&d.0, &out), Ok(0));
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("hashes: &[]"));
    }
}
