//! User aliases (plan L2 D9): a word typed alone in Mixed mode opens what the user set it to. Pure and by syntax only:
//! the table is built once per config change (`AliasTable::build`) and looked up once per key by a binary search that
//! allocates nothing. Every target ends in one of the launcher's de-elevated launch requests; none is ever run as
//! administrator, and none is learned.

use super::model::{FileClass, RunLine};
use super::providers::{file_class, names_folder};
use super::query::PrefixTable;
use super::rank;
use super::runline;
use super::web;
use std::collections::BTreeMap;

/// At most this many aliases are served (ALIAS-1); the config may hold more (its cap is 256), which are ignored.
pub const MAX_ALIASES: usize = 64;
/// A word is 1-16 characters.
pub const MAX_WORD_CHARS: usize = 16;
/// A target is 1-512 characters.
pub const MAX_TARGET_CHARS: usize = 512;

/// What an alias opens, by syntax.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AliasTarget {
    /// A catalogue app, by the id written after "app:" (matched against the catalogue when the alias is typed).
    App(Box<str>),
    /// An http or https address, as `web::parse_typed_url` gives it.
    Url(Box<str>),
    /// An "ms-settings:" page.
    Settings(Box<str>),
    /// A folder or a file that is not a program (a program's path is a `Command`).
    Path { path: Box<str>, dir: bool },
    /// A command line, as the run mode splits it.
    Command(RunLine),
}

/// Why a word or a target is not an alias (`strings::alias_refusal` says it to the user).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AliasRefusal {
    /// Not 1-16 characters, a letter first, then letters, digits and "- _ . +".
    Word,
    /// The word is one of the prefixes ("file").
    Prefix,
    /// Nothing to open.
    NoTarget,
    /// More than 512 characters.
    TooLong,
    /// A control, bidi or format character.
    Hidden,
    /// More than one line.
    Lines,
    /// An http(s) or settings address the launcher would not open (a user name before the host, a bad port, a space).
    Address,
    /// Any other scheme ("javascript:", "file:", "mailto:", a custom one) or a device path.
    Scheme,
    /// The table holds 64 aliases already (the alias prompt).
    Full,
}

/// A control, bidi or format character: invisible or reordering, so a target holding one could show one thing and open
/// another (the set `config::LauncherConfig::sanitize` refuses in prefixes, with the other format characters).
pub fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{600}'..='\u{605}'
                | '\u{61c}'
                | '\u{6dd}'
                | '\u{70f}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0001}'
                | '\u{e0020}'..='\u{e007f}'
        )
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `s` without `head` at its start, compared without regard to ASCII case.
fn strip_ci<'a>(s: &'a str, head: &str) -> Option<&'a str> {
    let n = head.len();
    s.get(..n).filter(|h| h.eq_ignore_ascii_case(head)).and_then(|_| s.get(n..))
}

/// The run mode's path syntax (`query::implicit`): a drive and ":\", a UNC name, a "shell:" folder.
fn is_path(t: &str) -> bool {
    let b = t.as_bytes();
    let drive = b.first().is_some_and(u8::is_ascii_alphabetic) && b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\');
    drive || (t.len() > 2 && t.starts_with("\\\\")) || strip_ci(t, "shell:").is_some_and(|r| !r.is_empty())
}

/// A URI scheme and its colon at the start ("javascript:", "C:" of a drive-relative name).
fn has_scheme(t: &str) -> bool {
    let Some((scheme, _)) = t.split_once(':') else { return false };
    let mut cs = scheme.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// What `target` opens, by syntax only (no file, registry or network is looked at): "app:<id>" an app; an http(s)
/// address as the typed-URL row takes it; an "ms-settings:" page; a path (drive, UNC, "shell:") a folder or a file, and a
/// program's path a command of the quoted path; any other scheme is refused; anything else is a command line as the run
/// mode splits it. %VAR% is left as typed.
pub fn classify(target: &str) -> Result<AliasTarget, AliasRefusal> {
    let t = target.trim();
    if t.is_empty() {
        return Err(AliasRefusal::NoTarget);
    }
    if t.chars().count() > MAX_TARGET_CHARS {
        return Err(AliasRefusal::TooLong);
    }
    if t.chars().any(is_line_break) {
        return Err(AliasRefusal::Lines);
    }
    if t.chars().any(is_hidden) {
        return Err(AliasRefusal::Hidden);
    }
    if let Some(id) = strip_ci(t, "app:") {
        let id = id.trim();
        return if id.is_empty() { Err(AliasRefusal::NoTarget) } else { Ok(AliasTarget::App(id.into())) };
    }
    if ["http://", "https://", "www."].iter().any(|h| strip_ci(t, h).is_some()) {
        return web::parse_typed_url(t).map(|u| AliasTarget::Url(u.into())).ok_or(AliasRefusal::Address);
    }
    if strip_ci(t, "ms-settings:").is_some() {
        if t.contains(char::is_whitespace) || t.contains('"') {
            return Err(AliasRefusal::Address);
        }
        return Ok(AliasTarget::Settings(t.into()));
    }
    if t.starts_with("\\\\?\\") || t.starts_with("\\\\.\\") {
        return Err(AliasRefusal::Scheme);
    }
    if is_path(t) {
        if t.contains('"') {
            return Err(AliasRefusal::Address);
        }
        let dir = names_folder(t);
        if !dir && file_class(t) == FileClass::Executable {
            let quoted = format!("\"{t}\"");
            return runline::classify(&quoted).map(AliasTarget::Command).ok_or(AliasRefusal::NoTarget);
        }
        return Ok(AliasTarget::Path { path: t.into(), dir });
    }
    if has_scheme(t) {
        return Err(AliasRefusal::Scheme);
    }
    runline::classify(t).map(AliasTarget::Command).ok_or(AliasRefusal::NoTarget)
}

/// The folded word of an alias: 1-16 characters, a letter first, then letters, digits and "- _ . +"; never one of the
/// prefixes of `prefixes` (its closing space trimmed, without regard to case), which would start a mode instead.
pub fn check_word(word: &str, prefixes: &PrefixTable) -> Result<Box<str>, AliasRefusal> {
    let mut cs = word.chars();
    let first = cs.next().is_some_and(char::is_alphabetic);
    let rest = cs.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
    if !first || !rest || word.chars().count() > MAX_WORD_CHARS {
        return Err(AliasRefusal::Word);
    }
    let mut folded = String::with_capacity(word.len());
    rank::fold(word, &mut folded);
    if prefixes.is_word_prefix(&folded) {
        return Err(AliasRefusal::Prefix);
    }
    Ok(folded.into())
}

/// FNV-1a 64 of a word as configured: what Settings > Launcher's Remove button carries, so a list changed in between
/// never removes another entry.
pub fn word_hash(word: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in word.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Takes out the entry whose word hashes to `hash` (the first of two entries sharing a hash); true when one went.
pub fn remove_by_hash(map: &mut BTreeMap<String, String>, hash: u64) -> bool {
    let found = map.keys().find(|w| word_hash(w) == hash).cloned();
    found.is_some_and(|w| map.remove(&w).is_some())
}

impl AliasTarget {
    /// The target as config.json holds it (`classify` gives this target back).
    pub fn to_config(&self) -> String {
        match self {
            AliasTarget::App(id) => format!("app:{id}"),
            AliasTarget::Url(s) | AliasTarget::Settings(s) => s.to_string(),
            AliasTarget::Path { path, .. } => path.to_string(),
            AliasTarget::Command(r) => r.line.to_string(),
        }
    }
}

/// One alias: the word as configured (the row's title, the key Remove takes out of the config), its folded form (what the
/// typed query is compared with) and its target.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alias {
    pub word: Box<str>,
    pub folded: Box<str>,
    pub target: AliasTarget,
}

/// The aliases the launcher serves, sorted by folded word: built once per config change, read by one binary search per
/// key. At most 64 of at most 16 + 512 characters.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct AliasTable(Vec<Alias>);

impl AliasTable {
    /// The table of config.json's `launcher.aliases`, and how many of its entries it ignores: a word or a target it
    /// refuses, the second of two words equal once folded (the first in the map's order stays), past the 64th. The
    /// config keeps every entry as written.
    pub fn build(map: &BTreeMap<String, String>, prefixes: &PrefixTable) -> (AliasTable, usize) {
        let mut list = Vec::new();
        let mut ignored = 0usize;
        for (word, target) in map {
            match (check_word(word, prefixes), classify(target)) {
                (Ok(folded), Ok(target)) => list.push(Alias { word: word.as_str().into(), folded, target }),
                _ => ignored += 1,
            }
        }
        // Stable: of two equal folded words the first of the map stays first, and `dedup_by` keeps it.
        list.sort_by(|a, b| a.folded.cmp(&b.folded));
        let before = list.len();
        list.dedup_by(|later, first| later.folded == first.folded);
        ignored += before - list.len();
        if list.len() > MAX_ALIASES {
            ignored += list.len() - MAX_ALIASES;
            list.truncate(MAX_ALIASES);
        }
        list.shrink_to_fit();
        (AliasTable(list), ignored)
    }

    /// The alias of a folded query typed whole: one binary search, no allocation.
    pub fn find(&self, folded: &str) -> Option<&Alias> {
        self.0.binary_search_by(|a| (*a.folded).cmp(folded)).ok().and_then(|i| self.0.get(i))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg_attr(not(test), allow(dead_code))] // clippy's len_without_is_empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg_attr(not(test), allow(dead_code))] // L2-A3: Settings lists them
    pub fn iter(&self) -> impl Iterator<Item = &Alias> {
        self.0.iter()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    fn defaults() -> PrefixTable {
        PrefixTable::defaults()
    }

    #[test]
    fn word_rules() {
        let p = defaults();
        for (word, folded) in [("gh", "gh"), ("GH", "gh"), ("Caffè", "caffe"), ("a1-b_c.d+e", "a1-b_c.d+e"), ("x", "x")]
        {
            assert_eq!(check_word(word, &p).as_deref(), Ok(folded), "{word:?}");
        }
        assert_eq!(check_word("sixteen-chars-ok", &p).as_deref(), Ok("sixteen-chars-ok"));
        for bad in ["", "1abc", "-a", "bad word", "a/b", "a:b", "seventeen-chars-x", "a\u{202e}", "a\tb", " gh", "gh "]
        {
            assert_eq!(check_word(bad, &p), Err(AliasRefusal::Word), "{bad:?}");
        }
        // A word prefix of the table, its space trimmed, in any case, is not an alias.
        assert_eq!(check_word("file", &p), Err(AliasRefusal::Prefix));
        assert_eq!(check_word("FILE", &p), Err(AliasRefusal::Prefix));
        let mut m = BTreeMap::new();
        m.insert("go ".to_string(), "web".to_string());
        let own = PrefixTable::from_pairs(&m);
        assert_eq!(check_word("Go", &own), Err(AliasRefusal::Prefix));
        assert_eq!(check_word("file", &own).as_deref(), Ok("file"), "only the table's own prefixes");
    }

    #[test]
    fn targets_by_syntax() {
        use AliasTarget as T;
        let calc = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(classify(&format!("app:{calc}")), Ok(T::App(calc.into())));
        assert_eq!(classify("https://example.com/x"), Ok(T::Url("https://example.com/x".into())));
        assert_eq!(
            classify("  HTTPS://example.com/x "),
            Ok(T::Url("https://example.com/x".into())),
            "as parse_typed_url"
        );
        assert_eq!(classify("www.example.com"), Ok(T::Url("https://www.example.com".into())));
        assert_eq!(classify("http://user@host"), Err(AliasRefusal::Address));
        assert_eq!(classify("https://host:99999/"), Err(AliasRefusal::Address));
        assert_eq!(classify("ms-settings:display"), Ok(T::Settings("ms-settings:display".into())));
        assert_eq!(classify("ms-settings:display x"), Err(AliasRefusal::Address));
        assert_eq!(classify("C:\\Windows"), Ok(T::Path { path: "C:\\Windows".into(), dir: true }));
        assert_eq!(classify("\\\\srv\\share\\a.txt"), Ok(T::Path { path: "\\\\srv\\share\\a.txt".into(), dir: false }));
        assert_eq!(classify("shell:startup"), Ok(T::Path { path: "shell:startup".into(), dir: true }));
        let exe = classify("C:\\Tools\\x.exe").unwrap();
        let T::Command(r) = &exe else { panic!("{exe:?}") };
        assert_eq!((&*r.line, r.file.clone(), r.console), ("\"C:\\Tools\\x.exe\"", 1..15, false));
        // %VAR% stays as typed: the shell gets the line the user wrote.
        let line = "notepad %USERPROFILE%\\x.txt";
        assert_eq!(classify(line), Ok(T::Command(runline::classify(line).unwrap())));
        assert_eq!(classify("ping 1.1.1.1"), Ok(T::Command(runline::classify("ping 1.1.1.1").unwrap())));
        for (bad, why) in [
            ("javascript:alert(1)", AliasRefusal::Scheme),
            ("file:///c:/x", AliasRefusal::Scheme),
            ("search-ms:query=x", AliasRefusal::Scheme),
            ("mailto:a@example.com", AliasRefusal::Scheme),
            ("steam://run/1", AliasRefusal::Scheme),
            ("C:x.txt", AliasRefusal::Scheme),
            ("\\\\?\\C:\\x", AliasRefusal::Scheme),
            ("\\\\.\\PhysicalDrive0", AliasRefusal::Scheme),
            ("C:\\a\"b.exe", AliasRefusal::Address),
            ("notepad \u{202e}txt.exe", AliasRefusal::Hidden),
            ("calc\u{200b}", AliasRefusal::Hidden),
            ("notepad\nx", AliasRefusal::Lines),
            ("notepad\u{2028}x", AliasRefusal::Lines),
            ("", AliasRefusal::NoTarget),
            ("   ", AliasRefusal::NoTarget),
            ("app:", AliasRefusal::NoTarget),
        ] {
            assert_eq!(classify(bad), Err(why), "{bad:?}");
        }
        assert_eq!(classify(&"a".repeat(512)).map(|_| ()), Ok(()));
        assert_eq!(classify(&"a".repeat(513)), Err(AliasRefusal::TooLong));
    }

    #[test]
    fn table_sorted_unique_capped() {
        let p = defaults();
        let mut m = BTreeMap::new();
        for (w, t) in [
            ("zz", "https://example.com/z"),
            ("GH", "https://example.com/upper"),
            ("gh", "https://example.com/lower"),
            ("aa", "C:\\Windows"),
            ("bad word", "x"),
            ("ok", "javascript:x"),
            ("file", "C:\\Windows"),
        ] {
            m.insert(w.to_string(), t.to_string());
        }
        let (t, ignored) = AliasTable::build(&m, &p);
        let words: Vec<&str> = t.iter().map(|a| &*a.word).collect();
        assert_eq!(words, ["aa", "GH", "zz"], "sorted by folded word; of GH and gh the first in the map stays");
        assert_eq!(ignored, 4, "a bad word, a bad target, a prefix and the second gh");
        assert_eq!(t.find("gh").map(|a| a.target.clone()), Some(AliasTarget::Url("https://example.com/upper".into())));
        assert!(t.find("g").is_none() && t.find("ghx").is_none() && t.find("").is_none());
        // At most 64: the rest are ignored and counted.
        let many: BTreeMap<String, String> = (0..100).map(|i| (format!("w{i:03}"), "calc".to_string())).collect();
        let (t, ignored) = AliasTable::build(&many, &p);
        assert_eq!((t.len(), ignored), (MAX_ALIASES, 36));
        assert!(t.iter().zip(t.iter().skip(1)).all(|(a, b)| a.folded < b.folded));
        for a in t.iter() {
            assert_eq!(t.find(&a.folded).map(|b| &*b.word), Some(&*a.word));
        }
        let (empty, none) = AliasTable::build(&BTreeMap::new(), &p);
        assert!(empty.is_empty() && none == 0 && empty == AliasTable::default());
    }

    #[test]
    fn to_config_round_trips() {
        for t in [
            "app:Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
            "https://example.com/x?q=1",
            "ms-settings:display",
            "C:\\Windows",
            "\\\\srv\\share\\a.txt",
            "shell:startup",
            "C:\\Tools\\x.exe",
            "notepad %USERPROFILE%\\x.txt",
            "\"C:\\Program Files\\Example\\tool.exe\" --flag",
            "ping 1.1.1.1",
        ] {
            let a = classify(t).unwrap();
            assert_eq!(classify(&a.to_config()), Ok(a.clone()), "{t:?}");
        }
        assert_eq!(classify("C:\\Tools\\x.exe").unwrap().to_config(), "\"C:\\Tools\\x.exe\"");
    }

    #[test]
    fn seeded_100k_never_panics() {
        let p = defaults();
        let mut rng = XorShift::new(0xA11A5);
        let heads = ["", "app:", "http://", "https://", "www.", "ms-settings:", "shell:", "C:\\", "\\\\", "\"", "x:"];
        let mut map = BTreeMap::new();
        for i in 0..100_000u32 {
            let head = *rng.pick(&heads);
            let body = rng.ascii_or_bmp(40);
            let target = format!("{head}{body}");
            if let Ok(t) = classify(&target) {
                // Whatever is accepted is one line of at most 512 characters, with no hidden character.
                let c = t.to_config();
                assert!(c.chars().count() <= MAX_TARGET_CHARS + 2 && !c.chars().any(is_hidden), "{target:?}");
            }
            let word = rng.ascii_or_bmp(20);
            if let Ok(f) = check_word(&word, &p) {
                assert!(f.chars().count() <= 2 * MAX_WORD_CHARS && !f.contains(' '), "{word:?}");
            }
            if i % 1_000 == 0 {
                map.insert(word, target);
            }
        }
        let (t, ignored) = AliasTable::build(&map, &p);
        assert_eq!(t.len() + ignored, map.len());
    }

    #[test]
    fn word_hash_is_fnv1a_and_removal_takes_one_entry() {
        assert_eq!(word_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(word_hash("a"), 0xaf63_dc4c_8601_ec8c);
        let mut m = BTreeMap::new();
        m.insert("gh".to_string(), "https://example.com/".to_string());
        m.insert("docs".to_string(), "D:\\Projects".to_string());
        assert!(!remove_by_hash(&mut m, word_hash("GH")), "the hash is of the word as configured");
        assert_eq!(m.len(), 2);
        assert!(remove_by_hash(&mut m, word_hash("gh")));
        assert_eq!(m.keys().map(String::as_str).collect::<Vec<_>>(), ["docs"]);
        assert!(!remove_by_hash(&mut m, word_hash("gh")), "gone: nothing else goes");
        assert_eq!(m.len(), 1);
    }
}
