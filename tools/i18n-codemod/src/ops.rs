//! The commands. Everything reads and writes under a root folder holding src/ and strings/.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::lexer::{lex, line_of, Kind, Tok};
use crate::toml::{const_name, Entry, Toml};

/// Keys written by hand in the conversions C5 (the `t(k::KEY)` calls they add have no `tr` site before them):
/// `verify` leaves them out when it pairs the sites of a file with the keys now at those places.
pub const EXEMPT_KEY_PREFIXES: &[&str] = &[
    "settings_pages.preset_",
    "settings_pages.import_done_",
    "app.close_windows_n",
    "calendar.day_",
    "launcher_strings.day_of_year",
    "update.",
];

const I18N_RS: &str = "src/i18n.rs";

// ---- sites ----------------------------------------------------------------------------------------------------

/// A `tr("en", "it")` call with two string literals.
#[derive(Clone, Debug)]
pub struct Site {
    /// 1-based position among the literal sites of its file.
    pub ordinal: usize,
    pub line: usize,
    pub en: String,
    pub it: String,
    /// Byte span of the whole call, path included.
    pub start: usize,
    pub end: usize,
    /// The path written before `tr` ("" or "crate::i18n::" ...).
    pub prefix: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub sites: Vec<Site>,
    /// Lines of the `tr(` tokens that are not a two-literal call (and of a `fn tr(` definition).
    pub skipped: Vec<usize>,
    /// Every `tr(` identifier token outside comments, strings and char literals.
    pub tokens: usize,
    /// `tr` identifiers that are not called (imports).
    pub bare: Vec<usize>,
}

pub fn scan(src: &str) -> Scan {
    let toks = lex(src);
    scan_tokens(src, &toks)
}

fn path_start(toks: &[Tok], idx: usize) -> usize {
    let mut j = idx;
    while j >= 3 && toks[j - 1].is_punct(':') && toks[j - 2].is_punct(':') && toks[j - 3].kind == Kind::Ident {
        j -= 3;
    }
    j
}

fn scan_tokens(src: &str, toks: &[Tok]) -> Scan {
    let mut s = Scan::default();
    for idx in 0..toks.len() {
        if !toks[idx].is_ident(src, "tr") {
            continue;
        }
        if !toks.get(idx + 1).is_some_and(|t| t.is_punct('(')) {
            s.bare.push(idx);
            continue;
        }
        s.tokens += 1;
        let line = line_of(src, toks[idx].start);
        let j = path_start(toks, idx);
        let prev = j.checked_sub(1).map(|p| &toks[p]);
        if prev.is_some_and(|p| p.is_ident(src, "fn") || p.is_punct('.')) {
            s.skipped.push(line);
            continue;
        }
        let lit = |k: usize| toks.get(k).filter(|t| t.kind == Kind::Str).and_then(|t| t.value.clone());
        let mut close = idx + 5;
        if toks.get(idx + 5).is_some_and(|t| t.is_punct(',')) {
            close += 1;
        }
        let call = match (lit(idx + 2), lit(idx + 4)) {
            (Some(en), Some(it))
                if toks.get(idx + 3).is_some_and(|t| t.is_punct(','))
                    && toks.get(close).is_some_and(|t| t.is_punct(')')) =>
            {
                Some((en, it))
            }
            _ => None,
        };
        match call {
            Some((en, it)) => s.sites.push(Site {
                ordinal: s.sites.len() + 1,
                line,
                en,
                it,
                start: toks[j].start,
                end: toks[close].end,
                prefix: src[toks[j].start..toks[idx].start].to_string(),
            }),
            None => s.skipped.push(line),
        }
    }
    s
}

/// A second count of `tr(` tokens, written without the lexer: the source is masked (comments, strings and char
/// literals become blanks) and the masked text is searched.
pub fn independent_count(src: &str) -> usize {
    let b = src.as_bytes();
    let mut m = b.to_vec();
    let blank = |m: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut m[from..to.min(b.len())] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.starts_with(b"//") {
            let end = src[i..].find('\n').map_or(b.len(), |p| i + p);
            blank(&mut m, i, end);
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
            blank(&mut m, i, j);
            i = j;
        } else if rest[0] == b'"' || (rest[0] == b'r' && i > 0 && !is_word(b[i - 1]) && raw_open(rest).is_some()) {
            let (body, closer): (usize, String) = match raw_open(rest) {
                Some((n, hashes)) if rest[0] == b'r' => (i + n, format!("\"{}", "#".repeat(hashes))),
                _ => (i + 1, String::new()),
            };
            let end = if closer.is_empty() {
                let mut j = body;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                j + 1
            } else {
                src[body..].find(&closer).map_or(b.len(), |p| body + p + closer.len())
            };
            blank(&mut m, i, end);
            i = end.max(i + 1);
        } else if rest[0] == b'\'' {
            // 'x' or '\n' is a literal, 'name is a lifetime.
            let lit = if rest.get(1) == Some(&b'\\') {
                rest.iter().skip(2).position(|&c| c == b'\'').map(|p| p + 3)
            } else {
                src[i + 1..]
                    .chars()
                    .next()
                    .and_then(|c| (rest.get(1 + c.len_utf8()) == Some(&b'\'')).then(|| 2 + c.len_utf8()))
            };
            match lit {
                Some(n) => {
                    blank(&mut m, i, i + n);
                    i += n;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    let mut count = 0;
    let mut i = 0;
    while let Some(p) = m[i..].windows(2).position(|w| w == b"tr") {
        let at = i + p;
        let before_ok = at == 0 || !is_word(m[at - 1]);
        let mut j = at + 2;
        while j < m.len() && m[j].is_ascii_whitespace() {
            j += 1;
        }
        if before_ok && m.get(j) == Some(&b'(') && !(at > 0 && at + 2 < m.len() && is_word(m[at + 2])) {
            count += 1;
        }
        i = at + 2;
    }
    count
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// For a slice starting at an `r`: the length of the `r#*"` opener and its number of hashes.
fn raw_open(rest: &[u8]) -> Option<(usize, usize)> {
    let hashes = rest[1..].iter().take_while(|&&c| c == b'#').count();
    (rest.get(1 + hashes) == Some(&b'"')).then_some((2 + hashes, hashes))
}

// ---- files ----------------------------------------------------------------------------------------------------

/// Every .rs file under root/src as (path with '/', text), sorted by path.
pub fn src_files(root: &Path) -> Result<Vec<(String, String)>, String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, String)>) -> Result<(), String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for n in names {
            let p = dir.join(&n);
            let r = format!("{rel}/{n}");
            if p.is_dir() {
                walk(&p, &r, out)?;
            } else if n.ends_with(".rs") {
                out.push((r, fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&root.join("src"), "src", &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn read_toml(root: &Path, name: &str) -> Result<Toml, String> {
    let p = root.join("strings").join(name);
    let text = fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Toml::parse(&text).map_err(|e| format!("{}: {e}", p.display()))
}

fn write_toml(root: &Path, name: &str, t: &Toml) -> Result<(), String> {
    let p = root.join("strings").join(name);
    let new = t.render();
    if fs::read_to_string(&p).ok().as_deref() != Some(new.as_str()) {
        fs::write(&p, new).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

// ---- extract --------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct Row {
    pub file: String,
    pub site: Site,
}

#[derive(Debug)]
pub struct Extracted {
    pub rows: Vec<Row>,
    /// "file:line" of every token that is not a two-literal call.
    pub skipped: Vec<String>,
    /// `tr(` tokens of src/i18n.rs itself.
    pub own: usize,
    pub independent: usize,
}

pub fn extract(root: &Path, expect: Option<usize>, tsv: Option<&Path>) -> Result<Extracted, String> {
    extract_with(root, expect, tsv, independent_count)
}

/// `extract` with the independent counter as a parameter (a test plants a token the lexer cannot see).
pub fn extract_with(
    root: &Path,
    expect: Option<usize>,
    tsv: Option<&Path>,
    independent: fn(&str) -> usize,
) -> Result<Extracted, String> {
    let mut e = Extracted { rows: Vec::new(), skipped: Vec::new(), own: 0, independent: 0 };
    for (rel, text) in src_files(root)? {
        e.independent += independent(&text);
        let s = scan(&text);
        if rel == I18N_RS {
            e.own = s.tokens;
            continue;
        }
        for l in s.skipped {
            e.skipped.push(format!("{rel}:{l}"));
        }
        for site in s.sites {
            e.rows.push(Row { file: rel.clone(), site });
        }
    }
    let counted = e.rows.len() + e.skipped.len() + e.own;
    if counted != e.independent {
        return Err(format!(
            "self-check failed: {} literal + {} skipped + {} in src/i18n.rs = {counted}, the independent count is {}",
            e.rows.len(),
            e.skipped.len(),
            e.own,
            e.independent
        ));
    }
    if let Some(n) = expect {
        if n != e.rows.len() {
            return Err(format!("expected {n} literal sites, found {}", e.rows.len()));
        }
    }
    if let Some(p) = tsv {
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tsv_field =
            |s: &str| s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r");
        let mut out = String::new();
        for r in &e.rows {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                r.file,
                r.site.ordinal,
                r.site.line,
                tsv_field(&r.site.en),
                tsv_field(&r.site.it)
            ));
        }
        fs::write(p, out).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(e)
}

// ---- keys -----------------------------------------------------------------------------------------------------

/// The area of a file: the path under src/ without ".rs", "/" to "_", a leading "popup_" removed.
pub fn area_of(rel: &str) -> String {
    let p = rel.strip_prefix("src/").unwrap_or(rel);
    let p = p.strip_suffix(".rs").unwrap_or(p).replace('/', "_");
    let p = p.strip_prefix("popup_").unwrap_or(&p).to_ascii_lowercase();
    p.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '_' }).collect()
}

/// The item of a key: the EN text lower-cased, runs outside [a-z0-9] to "_", the first five words, at most 40 chars.
pub fn item_of(en: &str, ordinal: usize) -> String {
    let mut s = String::new();
    for c in en.to_lowercase().chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit();
        if ok {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let words: Vec<&str> = s.split('_').filter(|w| !w.is_empty()).take(5).collect();
    let mut item = words.join("_");
    item.truncate(40);
    let item = item.trim_end_matches('_').to_string();
    if item.is_empty() {
        format!("s{ordinal}")
    } else {
        item
    }
}

struct Keys {
    en: Toml,
    it: Toml,
    by_pair: HashMap<(String, String), Vec<String>>,
    keys: HashSet<String>,
    consts: HashSet<String>,
}

fn area_of_key(key: &str) -> &str {
    key.split('.').next().unwrap_or("")
}

impl Keys {
    fn new(en: Toml, it: Toml) -> Keys {
        let mut k = Keys { en, it, by_pair: HashMap::new(), keys: HashSet::new(), consts: HashSet::new() };
        for e in k.en.entries.iter().chain(k.it.entries.iter()) {
            k.keys.insert(e.key.clone());
            k.consts.insert(const_name(&e.key));
        }
        for e in &k.en.entries {
            if let Some(t) = k.it.get(&e.key) {
                k.by_pair.entry((e.text.clone(), t.to_string())).or_default().push(e.key.clone());
            }
        }
        k
    }

    /// The key of a site: an existing key of the area or a common one with the same pair, else a new key.
    /// Returns (key, is_new).
    fn key_for(&mut self, area: &str, ordinal: usize, en: &str, it: &str, common: bool) -> (String, bool) {
        if let Some(list) = self.by_pair.get(&(en.to_string(), it.to_string())) {
            if let Some(k) = list
                .iter()
                .find(|k| area_of_key(k) == area)
                .or_else(|| list.iter().find(|k| area_of_key(k) == "common"))
            {
                return (k.clone(), false);
            }
        }
        let base = if common { "common" } else { area };
        let item = item_of(en, ordinal);
        let mut n = 1;
        let mut key = format!("{base}.{item}");
        while self.keys.contains(&key) || self.consts.contains(&const_name(&key)) {
            n += 1;
            key = format!("{base}.{item}_{n}");
        }
        self.keys.insert(key.clone());
        self.consts.insert(const_name(&key));
        self.en.entries.push(Entry { key: key.clone(), text: en.to_string(), pre: Vec::new() });
        self.it.entries.push(Entry { key: key.clone(), text: it.to_string(), pre: Vec::new() });
        self.by_pair.entry((en.to_string(), it.to_string())).or_default().push(key.clone());
        (key, true)
    }
}

// ---- apply ----------------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Applied {
    pub converted: usize,
    pub new_keys: usize,
    pub reused: usize,
    pub warnings: Vec<String>,
}

pub fn apply(root: &Path) -> Result<Applied, String> {
    let files: Vec<(String, String, Scan)> = src_files(root)?
        .into_iter()
        .filter(|(rel, _)| rel != I18N_RS)
        .map(|(rel, text)| {
            let s = scan(&text);
            (rel, text, s)
        })
        .collect();
    let en = read_toml(root, "en.toml")?;
    let it = read_toml(root, "it.toml")?;
    let first_run = !en.has_marker();
    // Pairs found in two or more areas (decided on the first full run only).
    let mut areas: HashMap<(&str, &str), BTreeSet<String>> = HashMap::new();
    for (rel, _, s) in &files {
        for site in &s.sites {
            areas.entry((site.en.as_str(), site.it.as_str())).or_default().insert(area_of(rel));
        }
    }
    let mut keys = Keys::new(en, it);
    let mut report = Applied::default();
    for (rel, text, s) in &files {
        if s.sites.is_empty() {
            continue;
        }
        let area = area_of(rel);
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for site in &s.sites {
            let common = first_run && areas.get(&(site.en.as_str(), site.it.as_str())).is_some_and(|a| a.len() >= 2);
            let (key, is_new) = keys.key_for(&area, site.ordinal, &site.en, &site.it, common);
            if is_new {
                report.new_keys += 1;
            } else {
                report.reused += 1;
            }
            let c = const_name(&key);
            let p = &site.prefix;
            edits.push((site.start, site.end, format!("{p}t({p}k::{c})")));
            report.converted += 1;
        }
        let toks = lex(text);
        let remaining = s.tokens - s.sites.len() > 0;
        let needs = s.sites.iter().any(|x| x.prefix.is_empty());
        if needs {
            // Only a binding with a converted call within the next 40 lines can matter (the scope is not parsed).
            let call_lines: Vec<usize> = s.sites.iter().filter(|x| x.prefix.is_empty()).map(|x| x.line).collect();
            let mut lines = shadowing_bindings(text, &toks);
            lines.dedup();
            for line in lines.into_iter().filter(|l| call_lines.iter().any(|c| (*l..*l + 40).contains(c))) {
                report.warnings.push(format!("{rel}:{line}: a binding named `t` or `k` may shadow the keyed call"));
            }
        }
        let mut skip: HashSet<usize> = HashSet::new();
        for &idx in &s.bare {
            if skip.contains(&idx) {
                continue;
            }
            let found = if idx > 0 && toks[idx - 1].is_ident(text, "let") {
                alias_edit(text, &toks, idx, remaining, needs).map(|o| {
                    o.map(|(e, end)| {
                        skip.insert(end);
                        e
                    })
                })
            } else {
                import_edit(text, &toks, idx, remaining, needs)
            };
            match found {
                Ok(Some(e)) => edits.push(e),
                Ok(None) => {}
                Err(w) => report.warnings.push(format!("{rel}:{}: {w}", line_of(text, toks[idx].start))),
            }
        }
        edits.sort_by(|a, b| b.0.cmp(&a.0));
        let mut out = text.clone();
        for (from, to, with) in edits {
            out.replace_range(from..to, &with);
        }
        let p = root.join(rel);
        fs::write(&p, out).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    if report.converted > 0 || first_run {
        keys.en.add_marker();
        keys.it.add_marker();
    }
    keys.en.sort();
    keys.it.sort();
    write_toml(root, "en.toml", &keys.en)?;
    write_toml(root, "it.toml", &keys.it)?;
    Ok(report)
}

/// Lines of bindings named `t` or `k` (let, for, closure and match-arm patterns): in their scope `t(k::KEY)` would not compile.
fn shadowing_bindings(src: &str, toks: &[Tok]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        if !(tok.is_ident(src, "t") || tok.is_ident(src, "k")) || i == 0 {
            continue;
        }
        let prev = &toks[i - 1];
        let bound = prev.is_ident(src, "let")
            || prev.is_ident(src, "mut")
            || prev.is_ident(src, "for")
            || prev.is_punct('|')
            || (prev.is_punct('(')
                && toks.get(i + 1).is_some_and(|n| n.is_punct(')'))
                && toks.get(i + 2).is_some_and(|n| n.is_punct('=') || n.is_ident(src, "if")));
        if bound {
            out.push(line_of(src, tok.start));
        }
    }
    out
}

/// A byte span of the source and its replacement.
type Edit = (usize, usize, String);

/// `let tr = crate::i18n::tr;` inside a function becomes `use crate::i18n::{k, t};` (same scope). Returns the edit and the
/// index of the `tr` that ends the path.
fn alias_edit(
    src: &str,
    toks: &[Tok],
    idx: usize,
    remaining: bool,
    needs: bool,
) -> Result<Option<(Edit, usize)>, String> {
    let unhandled = || "a `let tr = ..` alias the tool does not rewrite".to_string();
    let mut end = idx + 2;
    while end < toks.len() && !toks[end].is_punct(';') {
        end += 1;
    }
    if !toks.get(idx + 1).is_some_and(|t| t.is_punct('=')) || end >= toks.len() || end < idx + 5 {
        return Err(unhandled());
    }
    let last = end - 1;
    let ok = toks[last].is_ident(src, "tr") && toks[last - 1].is_punct(':') && toks[last - 2].is_punct(':');
    if !ok || remaining || !needs {
        return Err(unhandled());
    }
    let path = &src[toks[idx + 2].start..toks[last - 2].start];
    Ok(Some(((toks[idx - 1].start, toks[end].end, format!("use {path}::{{k, t}};")), last)))
}

/// The edit of one bare `tr` identifier (an import): `use ..::i18n::tr;` or `use ..::i18n::{.., tr, ..};`.
fn import_edit(
    src: &str,
    toks: &[Tok],
    idx: usize,
    remaining: bool,
    needs: bool,
) -> Result<Option<(usize, usize, String)>, String> {
    let unhandled = || "`tr` is imported in a form the tool does not rewrite".to_string();
    let after_i18n = |j: usize| {
        j >= 3 && toks[j - 1].is_punct(':') && toks[j - 2].is_punct(':') && toks[j - 3].is_ident(src, "i18n")
    };
    let make = |present: &[String]| -> Vec<String> {
        let mut items: Vec<String> = present.iter().filter(|i| *i != "tr").cloned().collect();
        if remaining {
            items.push("tr".into());
        }
        if needs {
            for w in ["k", "t"] {
                if !items.iter().any(|i| i == w) {
                    items.push(w.into());
                }
            }
        }
        items.sort_by(|a, b| (a != "self", a).cmp(&(b != "self", b)));
        items.dedup();
        items
    };
    // use a::b::i18n::tr;
    if after_i18n(idx) && toks.get(idx + 1).is_some_and(|t| t.is_punct(';')) {
        let items = make(&["tr".to_string()]);
        if items == ["tr"] {
            return Ok(None);
        }
        let text = match items.len() {
            0 => return Err("the `tr` import is not used any more".into()),
            1 => items[0].clone(),
            _ => format!("{{{}}}", items.join(", ")),
        };
        return Ok(Some((toks[idx].start, toks[idx].end, text)));
    }
    // use a::b::i18n::{.., tr, ..};
    let mut open = idx;
    while open > 0 && !toks[open].is_punct('{') {
        if toks[open].kind != Kind::Ident && !toks[open].is_punct(',') {
            return Err(unhandled());
        }
        open -= 1;
    }
    if !toks[open].is_punct('{') || !after_i18n(open) {
        return Err(unhandled());
    }
    let mut items = Vec::new();
    let mut close = open + 1;
    while close < toks.len() && !toks[close].is_punct('}') {
        match &toks[close].kind {
            Kind::Ident if toks[close].is_ident(src, "as") => return Err(unhandled()),
            Kind::Ident => items.push(src[toks[close].start..toks[close].end].to_string()),
            Kind::Punct(',') => {}
            _ => return Err(unhandled()),
        }
        close += 1;
    }
    if close >= toks.len() {
        return Err(unhandled());
    }
    let new = make(&items);
    if new == items {
        return Ok(None);
    }
    Ok(Some((toks[open].start, toks[close].end, format!("{{{}}}", new.join(", ")))))
}

// ---- tidy and prune -------------------------------------------------------------------------------------------

/// Sorts both files by key, drops exact duplicates and fails on one key with two texts. Returns the key count.
pub fn tidy(root: &Path) -> Result<usize, String> {
    let mut n = 0;
    for name in ["en.toml", "it.toml"] {
        let mut t = read_toml(root, name)?;
        t.sort();
        let mut out: Vec<Entry> = Vec::new();
        for e in std::mem::take(&mut t.entries) {
            match out.last_mut() {
                Some(last) if last.key == e.key => {
                    if last.text != e.text {
                        return Err(format!(
                            "{name}: key \"{}\" has two texts: {:?} and {:?}",
                            e.key, last.text, e.text
                        ));
                    }
                    last.pre.extend(e.pre);
                }
                _ => out.push(e),
            }
        }
        t.entries = out;
        n = t.entries.len();
        write_toml(root, name, &t)?;
    }
    Ok(n)
}

/// The constants referenced as `k::CONST` in the lexed source.
pub fn k_refs(src: &str) -> Vec<(usize, String)> {
    let toks = lex(src);
    let mut out = Vec::new();
    for i in 0..toks.len().saturating_sub(3) {
        if toks[i].is_ident(src, "k")
            && toks[i + 1].is_punct(':')
            && toks[i + 2].is_punct(':')
            && toks[i + 3].kind == Kind::Ident
        {
            out.push((i, src[toks[i + 3].start..toks[i + 3].end].to_string()));
        }
    }
    out
}

/// Deletes every key no `k::CONST` in src/ references (the rule of lint L3); returns the deleted keys.
pub fn prune(root: &Path) -> Result<Vec<String>, String> {
    let mut used: HashSet<String> = HashSet::new();
    for (_, text) in src_files(root)? {
        used.extend(k_refs(&text).into_iter().map(|(_, c)| c));
    }
    let mut en = read_toml(root, "en.toml")?;
    let mut it = read_toml(root, "it.toml")?;
    let mut gone = BTreeSet::new();
    for t in [&mut en, &mut it] {
        t.entries.retain(|e| {
            let keep = used.contains(&const_name(&e.key));
            if !keep {
                gone.insert(e.key.clone());
            }
            keep
        });
    }
    write_toml(root, "en.toml", &en)?;
    write_toml(root, "it.toml", &it)?;
    Ok(gone.into_iter().collect())
}

// ---- verify ---------------------------------------------------------------------------------------------------

/// The source tree of the base revision.
pub trait Old {
    /// Every path of src/ (with '/').
    fn files(&self) -> Result<Vec<String>, String>;
    fn read(&self, rel: &str) -> Result<String, String>;
}

pub struct GitOld {
    pub root: PathBuf,
    pub rev: String,
}

impl GitOld {
    fn git(&self, args: &[&str]) -> Result<String, String> {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(&self.root).args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let o = c.output().map_err(|e| format!("git: {e}"))?;
        if !o.status.success() {
            return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

impl Old for GitOld {
    fn files(&self) -> Result<Vec<String>, String> {
        let out = self.git(&["ls-tree", "-r", "--name-only", &self.rev, "--", "src"])?;
        Ok(out.lines().filter(|l| l.ends_with(".rs")).map(str::to_string).collect())
    }
    fn read(&self, rel: &str) -> Result<String, String> {
        self.git(&["show", &format!("{}:{rel}", self.rev)])
    }
}

/// The keys of the `t(k::CONST)` calls of a file, in order (`t` called on a `k::` constant, any path prefix).
fn t_calls(src: &str) -> Vec<String> {
    let toks = lex(src);
    let mut out = Vec::new();
    for (i, c) in k_refs(src) {
        let j = path_start(&toks, i);
        let called = j >= 2 && toks[j - 1].is_punct('(') && toks[j - 2].is_ident(src, "t");
        let closed = toks.get(i + 4).is_some_and(|t| t.is_punct(')'));
        if called && closed {
            out.push(c);
        }
    }
    out
}

/// Pairs the sites of every file at `old` with the keys now at those places; returns the number of sites checked or
/// the mismatches.
pub fn verify(root: &Path, old: &dyn Old, exempt: &[String]) -> Result<usize, Vec<String>> {
    let en = read_toml(root, "en.toml").map_err(|e| vec![e])?;
    let it = read_toml(root, "it.toml").map_err(|e| vec![e])?;
    let by_const: HashMap<String, &str> = en.entries.iter().map(|e| (const_name(&e.key), e.key.as_str())).collect();
    let mut bad = Vec::new();
    let mut checked = 0;
    let new_files: BTreeMap<String, String> = src_files(root).map_err(|e| vec![e])?.into_iter().collect();
    let mut names: BTreeSet<String> = new_files.keys().cloned().collect();
    names.extend(old.files().map_err(|e| vec![e])?);
    names.remove(I18N_RS);
    for rel in names {
        // Sites keyed before the run (t(k::..) in the base) are in the new file too and are not counted either way.
        let (before, mut keyed_before) = match old.read(&rel) {
            Ok(t) => (scan(&t).sites, t_calls(&t)),
            Err(_) => (Vec::new(), Vec::new()),
        };
        let now: Vec<String> = new_files
            .get(&rel)
            .map(|t| t_calls(t))
            .unwrap_or_default()
            .into_iter()
            .filter(|c| match keyed_before.iter().position(|k| k == c) {
                Some(p) => {
                    keyed_before.remove(p);
                    false
                }
                None => true,
            })
            .map(|c| by_const.get(&c).map_or(c.clone(), |k| k.to_string()))
            .filter(|k| !exempt.iter().any(|p| k.starts_with(p.as_str())))
            .collect();
        if before.len() != now.len() {
            bad.push(format!("{rel}: {} sites before, {} keyed sites now", before.len(), now.len()));
            continue;
        }
        for (site, key) in before.iter().zip(&now) {
            checked += 1;
            let (e, i) = (en.get(key), it.get(key));
            if e != Some(site.en.as_str()) || i != Some(site.it.as_str()) {
                bad.push(format!(
                    "{rel}: site {} (line {} before): expected EN {:?} IT {:?}, key {key} holds EN {:?} IT {:?}",
                    site.ordinal, site.line, site.en, site.it, e, i
                ));
            }
        }
    }
    if bad.is_empty() {
        Ok(checked)
    } else {
        Err(bad)
    }
}
