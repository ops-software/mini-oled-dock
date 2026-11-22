//! Parsing of what the user typed (SPEC 5.2.4): the mode prefixes and the implicit forms. Pure, bounded, never panics.

use super::model::{Mode, NumberStyle, ProviderId, Query};
use super::rank;
use std::collections::BTreeMap;

/// The longest prefix the table accepts, in characters (5.6).
const MAX_PREFIX_CHARS: usize = 5;

/// A word prefix has a letter in it ("file "); the others are symbols ("=", "??").
fn is_word_prefix(p: &str) -> bool {
    p.chars().any(char::is_alphabetic)
}

/// The prefixes and the providers they select, longest first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PrefixTable(Vec<(Box<str>, ProviderId)>);

impl PrefixTable {
    /// The owner's eight aliases (5.6).
    pub fn defaults() -> PrefixTable {
        let pairs = [
            ("=", ProviderId::Calc),
            (">", ProviderId::Run),
            ("<", ProviderId::Windows),
            ("??", ProviderId::Web),
            ("$", ProviderId::Settings),
            (")", ProviderId::DateTime),
            (":", ProviderId::Registry),
            ("file ", ProviderId::Files),
        ];
        PrefixTable::from_list(pairs.iter().map(|(p, id)| (Box::from(*p), *id)).collect())
    }

    /// From the config's `prefixes` map, already validated by the config; an entry that is still wrong is skipped here, so a bad
    /// map can never route a prefix to nothing: an unknown provider name, an empty or longer than 5 characters prefix, a word
    /// prefix without its closing space.
    pub fn from_pairs(pairs: &BTreeMap<String, String>) -> PrefixTable {
        let list = pairs
            .iter()
            .filter_map(|(prefix, name)| {
                let id = ProviderId::from_name(name)?;
                let chars = prefix.chars().count();
                let word = is_word_prefix(prefix);
                let closed = !word || prefix.ends_with(' ');
                // No control characters; white space only as the single trailing space of a word prefix; not only digits.
                let body = if word { prefix.strip_suffix(' ').unwrap_or(prefix) } else { prefix };
                let plain = !prefix.chars().any(char::is_control)
                    && !body.chars().any(char::is_whitespace)
                    && !prefix.chars().all(|c| c.is_ascii_digit());
                (chars != 0 && chars <= MAX_PREFIX_CHARS && closed && plain).then(|| (Box::from(prefix.as_str()), id))
            })
            .collect();
        PrefixTable::from_list(list)
    }

    fn from_list(mut list: Vec<(Box<str>, ProviderId)>) -> PrefixTable {
        list.sort_by_key(|(p, _)| core::cmp::Reverse(p.len()));
        PrefixTable(list)
    }

    /// Whether `folded` (a folded word) is one of the prefixes, its closing space trimmed and folded: "file" for "file ".
    /// An alias may not be one (`aliases::check_word`).
    pub fn is_word_prefix(&self, folded: &str) -> bool {
        self.0.iter().any(|(p, _)| {
            let mut f = String::with_capacity(p.len());
            rank::fold(p.trim(), &mut f);
            f == folded
        })
    }

    /// The prefix at the start of `raw`, if any: its length in bytes, its provider and whether it is a word prefix. The longest
    /// wins; word prefixes match without regard to ASCII case.
    pub fn find(&self, raw: &str) -> Option<(usize, ProviderId, bool)> {
        self.0.iter().find_map(|(prefix, id)| {
            let head = raw.get(..prefix.len())?;
            let word = is_word_prefix(prefix);
            let hit = if word { head.eq_ignore_ascii_case(prefix) } else { head == &**prefix };
            hit.then_some((prefix.len(), *id, word))
        })
    }
}

/// Parses the field's text: finds the prefix, folds the rest once. `text` is always a suffix of `raw`; `folded` is the folded
/// `text` without leading and trailing white space.
pub fn parse_query<'a>(raw: &'a str, prefixes: &PrefixTable, num: NumberStyle, gen: u64) -> Query<'a> {
    let (text, chip, word) = match prefixes.find(raw) {
        Some((len, id, word)) => (raw.get(len..).unwrap_or(raw), Some(id), word),
        None => (raw, None, false),
    };
    let mut folded = String::with_capacity(text.len());
    rank::fold(text.trim(), &mut folded);
    Query {
        raw,
        text,
        folded,
        mode: chip.map_or(Mode::Mixed, Mode::Only),
        gen,
        num,
        chip,
        apps_also: chip.is_some() && word,
    }
}

/// What a Mixed query looks like by syntax alone (the dock makes no file-system, registry or network call to decide).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Implicit {
    Url,
    Path,
    Command,
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len()).is_some_and(|h| h.eq_ignore_ascii_case(prefix))
}

fn is_url(t: &str) -> bool {
    if t.contains(char::is_whitespace) {
        return false;
    }
    let rest = if starts_with_ignore_case(t, "https://") {
        t.get(8..)
    } else if starts_with_ignore_case(t, "http://") {
        t.get(7..)
    } else if starts_with_ignore_case(t, "www.") {
        t.get(4..)
    } else {
        None
    };
    rest.is_some_and(|r| !r.is_empty())
}

fn is_path(t: &str) -> bool {
    let b = t.as_bytes();
    // A drive letter and ":\".
    if let (Some(d), Some(b':'), Some(b'\\')) = (b.first(), b.get(1), b.get(2)) {
        return d.is_ascii_alphabetic();
    }
    if let Some(rest) = t.strip_prefix("\\\\") {
        return !rest.is_empty();
    }
    if starts_with_ignore_case(t, "shell:") {
        return t.len() > 6;
    }
    starts_with_ignore_case(t, "ms-settings:")
}

/// The implicit form of a Mixed query, if any: a URL, a path or address, or a line whose first word is a known command.
/// `is_known` compares a first word with the known-command table (case-insensitively).
pub fn implicit(text: &str, is_known: &dyn Fn(&str) -> bool) -> Option<Implicit> {
    let t = text.trim();
    if is_url(t) {
        Some(Implicit::Url)
    } else if is_path(t) {
        Some(Implicit::Path)
    } else if t.split_whitespace().next().is_some_and(is_known) {
        Some(Implicit::Command)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    const NUM: NumberStyle = NumberStyle { decimal: '.', group: ',' };

    fn parse<'a>(raw: &'a str, t: &PrefixTable) -> Query<'a> {
        parse_query(raw, t, NUM, 1)
    }

    #[test]
    fn all_eight_aliases() {
        let t = PrefixTable::defaults();
        let cases = [
            ("=2+2", ProviderId::Calc, "2+2"),
            (">ping x", ProviderId::Run, "ping x"),
            ("<code", ProviderId::Windows, "code"),
            ("??rust", ProviderId::Web, "rust"),
            ("$wifi", ProviderId::Settings, "wifi"),
            (")now", ProviderId::DateTime, "now"),
            (":reg", ProviderId::Registry, "reg"),
            ("file report", ProviderId::Files, "report"),
        ];
        for (raw, id, text) in cases {
            let q = parse(raw, &t);
            assert_eq!((q.mode, q.chip, q.text), (Mode::Only(id), Some(id), text), "{raw:?}");
            assert_eq!(q.raw, raw);
        }
        let plain = parse("calc", &t);
        assert_eq!((plain.mode, plain.chip, plain.text), (Mode::Mixed, None, "calc"));
        assert!(!plain.apps_also);
    }

    #[test]
    fn longest_prefix_wins() {
        let t = PrefixTable::defaults();
        let q = parse("??x", &t);
        assert_eq!((q.mode, q.text), (Mode::Only(ProviderId::Web), "x"));
        // "?" alone is no prefix; "?x" stays plain text.
        let q = parse("?x", &t);
        assert_eq!((q.mode, q.text), (Mode::Mixed, "?x"));
        // A longer symbol prefix beats a shorter one that starts it.
        let mut m = BTreeMap::new();
        m.insert("?".to_string(), "run".to_string());
        m.insert("??".to_string(), "web".to_string());
        let t = PrefixTable::from_pairs(&m);
        assert_eq!(parse("??x", &t).mode, Mode::Only(ProviderId::Web));
        assert_eq!(parse("?x", &t).mode, Mode::Only(ProviderId::Run));
    }

    #[test]
    fn word_prefix_needs_its_space() {
        let t = PrefixTable::defaults();
        for raw in ["filex", "file", "fil", "xfile "] {
            let q = parse(raw, &t);
            assert_eq!((q.mode, q.text), (Mode::Mixed, raw), "{raw:?}");
        }
        assert_eq!(parse("File x", &t).mode, Mode::Only(ProviderId::Files));
    }

    #[test]
    fn bare_prefix_gives_empty_text() {
        let t = PrefixTable::defaults();
        for (raw, id) in [("=", ProviderId::Calc), ("file ", ProviderId::Files), ("??", ProviderId::Web)] {
            let q = parse(raw, &t);
            assert_eq!((q.mode, q.text, q.folded.as_str()), (Mode::Only(id), "", ""), "{raw:?}");
        }
        let q = parse("", &t);
        assert_eq!((q.mode, q.text), (Mode::Mixed, ""));
    }

    #[test]
    fn file_explorer_still_finds_apps() {
        let t = PrefixTable::defaults();
        let q = parse("file explorer", &t);
        assert_eq!((q.mode, q.text), (Mode::Only(ProviderId::Files), "explorer"));
        assert!(q.apps_also);
        assert_eq!(q.raw, "file explorer");
        // Symbol prefixes do not ask the apps provider.
        assert!(!parse("=1+1", &t).apps_also);
        assert!(!parse("??x", &t).apps_also);
    }

    #[test]
    fn custom_prefix_routes_to_provider() {
        let mut m = BTreeMap::new();
        m.insert("#".to_string(), "windows".to_string());
        m.insert("=".to_string(), "calc".to_string());
        let t = PrefixTable::from_pairs(&m);
        let q = parse("#x", &t);
        assert_eq!((q.mode, q.text), (Mode::Only(ProviderId::Windows), "x"));
        let q = parse("??x", &t);
        assert_eq!((q.mode, q.text), (Mode::Mixed, "??x"));
        assert_eq!(parse("=1", &t).mode, Mode::Only(ProviderId::Calc));
    }

    #[test]
    fn a_bad_map_is_filtered() {
        let mut m = BTreeMap::new();
        for (k, v) in [
            ("!", "nothing"),
            ("", "calc"),
            ("toolong", "calc"),
            ("go", "web"),
            ("ok ", "run"),
            ("@", "units"),
            ("%", "files"),
            (" ", "web"),
            ("\t", "web"),
            ("1", "web"),
            ("12", "web"),
            ("a b ", "web"),
            ("\u{7}", "web"),
        ] {
            m.insert(k.to_string(), v.to_string());
        }
        let t = PrefixTable::from_pairs(&m);
        assert_eq!(parse("!x", &t).mode, Mode::Mixed);
        assert_eq!(parse("gox", &t).mode, Mode::Mixed);
        assert_eq!(parse("@x", &t).mode, Mode::Mixed);
        assert_eq!(parse("ok x", &t).mode, Mode::Only(ProviderId::Run));
        assert_eq!(parse("%x", &t).mode, Mode::Only(ProviderId::Files));
        for raw in [" x", "\tx", "1x", "12x", "a b x", "\u{7}x"] {
            assert_eq!(parse(raw, &t).mode, Mode::Mixed, "{raw:?}");
        }
    }

    #[test]
    fn folding_happens_once() {
        let t = PrefixTable::defaults();
        let q = parse("  Attività  ", &t);
        assert_eq!((q.text, q.folded.as_str()), ("  Attività  ", "attivita"));
        let q = parse("$ Résumé", &t);
        assert_eq!((q.text, q.folded.as_str()), (" Résumé", "resume"));
        assert_eq!((q.gen, q.num), (1, NUM));
    }

    #[test]
    fn implicit_forms() {
        let known = |w: &str| w.eq_ignore_ascii_case("ipconfig") || w.eq_ignore_ascii_case("ping");
        let cases: [(&str, Option<Implicit>); 17] = [
            ("C:\\x", Some(Implicit::Path)),
            ("c:\\Program Files\\x", Some(Implicit::Path)),
            ("\\\\srv\\s", Some(Implicit::Path)),
            ("shell:startup", Some(Implicit::Path)),
            ("ms-settings:display", Some(Implicit::Path)),
            ("www.a.it", Some(Implicit::Url)),
            ("https://a", Some(Implicit::Url)),
            ("HTTP://a.b/c?d", Some(Implicit::Url)),
            ("ping 8.8.8.8", Some(Implicit::Command)),
            ("IPCONFIG /all", Some(Implicit::Command)),
            ("http:/a", None),
            ("c:x", None),
            ("1:\\x", None),
            ("https://", None),
            ("www.", None),
            ("https://a b", None),
            ("calculator", None),
        ];
        for (text, want) in cases {
            assert_eq!(implicit(text, &known), want, "{text:?}");
        }
        assert_eq!(implicit("", &known), None);
        assert_eq!(implicit("   ", &known), None);
        assert_eq!(implicit("\\\\", &known), None);
        assert_eq!(implicit("shell:", &known), None);
    }

    #[test]
    fn seeded_100k_never_panics() {
        let mut rng = XorShift::new(0x9E57);
        let mut m = BTreeMap::new();
        m.insert("#".to_string(), "windows".to_string());
        m.insert("go ".to_string(), "web".to_string());
        let tables = [PrefixTable::defaults(), PrefixTable::from_pairs(&m)];
        let heads =
            ["", "", "=", ">", "<", "??", "$", ")", ":", "file ", "#", "go ", " ", "C:\\", "www.", "https://", "é"];
        let known = |w: &str| w.len() == 3;
        for i in 0..100_000 {
            let raw = format!("{}{}", rng.pick(&heads), rng.ascii_or_bmp(300));
            for t in &tables {
                let q = parse_query(&raw, t, NUM, i);
                assert!(raw.ends_with(q.text), "{raw:?}");
                assert_eq!(q.mode == Mode::Mixed, q.chip.is_none());
                let _ = implicit(&raw, &known);
                let _ = implicit(q.text, &known);
            }
        }
    }
}
