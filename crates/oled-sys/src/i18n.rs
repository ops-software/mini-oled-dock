//! Keyed strings (FZ-I18N): a string is looked up by the FNV-1a hash of its key name in generated tables.

use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// A string key: FNV-1a 64 of the key name ("area.item").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct S(pub u64);

/// FNV-1a 64 (offset 0xcbf29ce484222325, prime 0x100000001b3).
pub const fn fnv1a64(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

/// The key of a name.
pub const fn key(name: &str) -> S {
    S(fnv1a64(name))
}

/// One language's texts: `spans` holds (offset, len) into `blob`, in hash order.
pub struct Strings {
    pub blob: &'static str,
    pub spans: &'static [(u32, u32)],
}

/// The strings of one crate; `hashes` ascending, no duplicate.
pub struct Table {
    pub hashes: &'static [u64],
    pub en: Strings,
    pub it: Strings,
}

impl Strings {
    fn at(&self, i: usize) -> &'static str {
        match self.spans.get(i) {
            Some(&(off, len)) => self.blob.get(off as usize..(off + len) as usize).unwrap_or(""),
            None => "",
        }
    }
}

#[allow(clippy::len_without_is_empty)]
impl Table {
    /// Binary search by hash; an empty IT span falls back to EN; an unknown key gives "".
    pub fn get(&self, k: S, italian: bool) -> &'static str {
        let Ok(i) = self.hashes.binary_search(&k.0) else {
            return "";
        };
        if italian {
            let it = self.it.at(i);
            if !it.is_empty() {
                return it;
            }
        }
        self.en.at(i)
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }
}

const SLOTS: usize = 8;
static TABLES: [OnceLock<&'static Table>; SLOTS] = [const { OnceLock::new() }; SLOTS];

/// Registers a table (up to 8 per process); false if full or already registered.
pub fn register(t: &'static Table) -> bool {
    for slot in &TABLES {
        match slot.set(t) {
            Ok(()) => return true,
            Err(_) => {
                // The slot is taken, perhaps by a racing caller: the same table means "already registered".
                if slot.get().is_some_and(|have| std::ptr::eq(*have, t)) {
                    return false;
                }
            }
        }
    }
    false
}

/// The text of `k` in the first registered table that has it, in the language of `italian()`; "" if none.
pub fn lookup(k: S) -> &'static str {
    let it = italian();
    for slot in &TABLES {
        let Some(t) = slot.get() else {
            break;
        };
        if t.hashes.binary_search(&k.0).is_ok() {
            return t.get(k, it);
        }
    }
    ""
}

/// Replaces `{name}` by its argument; unknown braces stay.
pub fn subst(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let hit = after
            .find('}')
            .and_then(|close| args.iter().find(|(n, _)| *n == &after[..close]).map(|(_, v)| (close, *v)));
        match hit {
            Some((close, v)) => {
                out.push_str(v);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---- language state (moved from the dock's src/i18n.rs)

/// `PRIMARYLANGID` of Italian (`LANG_ITALIAN`; its feature module is not enabled for one constant).
const LANG_ITALIAN: u16 = 0x10;

/// Bit 0: Italian. Bit 1: chosen explicitly (not `auto`). Bit 2: preview mode (English, `en-US`).
static STATE: AtomicU8 = AtomicU8::new(0);
const IT: u8 = 1;
const EXPLICIT: u8 = 2;
const PREVIEW: u8 = 4;

/// True when the user's Windows UI language is Italian (`PRIMARYLANGID == LANG_ITALIAN`).
fn windows_is_italian() -> bool {
    // SAFETY: no arguments, no preconditions; returns a LANGID.
    let langid = unsafe { GetUserDefaultUILanguage() };
    langid & 0x3ff == LANG_ITALIAN
}

/// The language setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Auto,
    En,
    It,
}

/// The state byte after setting `lang`: the language bits from `lang` (and `windows_is_it` for `Auto`), the preview bit
/// kept from `prev`.
fn state_bits(lang: Lang, windows_is_it: bool, prev: u8) -> u8 {
    let bits = match lang {
        Lang::Auto => u8::from(windows_is_it) * IT,
        Lang::En => EXPLICIT,
        Lang::It => EXPLICIT | IT,
    };
    bits | (prev & PREVIEW)
}

/// Sets the language from the config value (called by `config::set` through the dock's shim).
pub fn apply(lang: Lang) {
    STATE.store(state_bits(lang, windows_is_italian(), STATE.load(Relaxed)), Relaxed);
}

/// Preview mode (`--preview`): English labels and `en-US` names whatever the config says.
pub fn set_preview(on: bool) {
    if on {
        STATE.fetch_or(PREVIEW, Relaxed);
    } else {
        STATE.fetch_and(!PREVIEW, Relaxed);
    }
}

/// True when the UI speaks Italian.
pub fn italian() -> bool {
    italian_in(STATE.load(Relaxed))
}

fn italian_in(s: u8) -> bool {
    s & PREVIEW == 0 && s & IT != 0
}

/// Locale name for the NLS calls (time, date, day and month names): NULL = the user locale
/// (language `auto`), `en-US` / `it-IT` for a chosen language, `en-US` in preview mode.
pub fn locale() -> PCWSTR {
    locale_in(STATE.load(Relaxed))
}

fn locale_in(s: u8) -> PCWSTR {
    if s & PREVIEW != 0 {
        w!("en-US")
    } else if s & EXPLICIT == 0 {
        PCWSTR::null()
    } else if s & IT != 0 {
        w!("it-IT")
    } else {
        w!("en-US")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a table the way the generator will: hash order, one blob per language.
    fn leak_table(entries: &[(&str, &str, &str)]) -> &'static Table {
        let mut e: Vec<(u64, &str, &str)> = entries.iter().map(|(k, en, it)| (fnv1a64(k), *en, *it)).collect();
        e.sort();
        let (mut en_blob, mut it_blob) = (String::new(), String::new());
        let (mut en_spans, mut it_spans) = (Vec::new(), Vec::new());
        for (_, en, it) in &e {
            en_spans.push((en_blob.len() as u32, en.len() as u32));
            en_blob.push_str(en);
            it_spans.push((it_blob.len() as u32, it.len() as u32));
            it_blob.push_str(it);
        }
        let hashes: Vec<u64> = e.iter().map(|x| x.0).collect();
        Box::leak(Box::new(Table {
            hashes: Box::leak(hashes.into_boxed_slice()),
            en: Strings { blob: Box::leak(en_blob.into_boxed_str()), spans: Box::leak(en_spans.into_boxed_slice()) },
            it: Strings { blob: Box::leak(it_blob.into_boxed_str()), spans: Box::leak(it_spans.into_boxed_slice()) },
        }))
    }

    #[test]
    fn fnv1a64_known_vectors() {
        const K: S = key("a");
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64("foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(K, S(0xaf63_dc4c_8601_ec8c));
    }

    #[test]
    fn table_get_finds_by_hash_and_falls_back() {
        let t = leak_table(&[("t.one", "One", "Uno"), ("t.two", "Two", ""), ("t.three", "Three", "Tre")]);
        assert_eq!(t.len(), 3);
        assert_eq!(t.get(key("t.one"), false), "One");
        assert_eq!(t.get(key("t.one"), true), "Uno");
        assert_eq!(t.get(key("t.three"), true), "Tre");
        assert_eq!(t.get(key("t.two"), true), "Two", "an empty IT span gives the EN text");
        assert_eq!(t.get(key("t.nope"), true), "");
    }

    #[test]
    fn subst_replaces_named_placeholders() {
        assert_eq!(subst("{n} of {m}", &[("n", "1"), ("m", "2")]), "1 of 2");
        assert_eq!(subst("{x}", &[("n", "1")]), "{x}");
        assert_eq!(subst("plain", &[("n", "1")]), "plain");
        assert_eq!(subst("a { b {n} }", &[("n", "1")]), "a { b 1 }");
    }

    #[test]
    fn register_refuses_a_second_copy() {
        let t = leak_table(&[("reg.a", "A", "A")]);
        assert!(register(t));
        assert!(!register(t));
    }

    #[test]
    fn lookup_searches_every_registered_table() {
        let a = leak_table(&[("look.a", "Alpha", "Alfa")]);
        let b = leak_table(&[("look.b", "Beta", "Beta")]);
        assert!(register(a));
        assert!(register(b));
        assert_eq!(lookup(key("look.a")), "Alpha");
        assert_eq!(lookup(key("look.b")), "Beta");
        assert_eq!(lookup(key("look.none")), "");
    }

    // Pure functions on a state value: the global byte is never touched, so tests running in
    // parallel keep seeing English.
    fn name(p: PCWSTR) -> String {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: the non-null values are the nul-terminated `w!` literals above.
            unsafe { p.to_string() }.unwrap_or_default()
        }
    }

    #[test]
    fn states() {
        assert!(!italian_in(0));
        assert!(italian_in(IT));
        assert!(italian_in(EXPLICIT | IT));
        assert!(!italian_in(EXPLICIT | IT | PREVIEW));
        assert_eq!(name(locale_in(0)), "");
        assert_eq!(name(locale_in(IT)), "");
        assert_eq!(name(locale_in(EXPLICIT)), "en-US");
        assert_eq!(name(locale_in(EXPLICIT | IT)), "it-IT");
        assert_eq!(name(locale_in(EXPLICIT | IT | PREVIEW)), "en-US");
        assert_eq!(name(locale_in(PREVIEW)), "en-US");
    }

    #[test]
    fn state_bits_maps_every_language() {
        assert_eq!(state_bits(Lang::Auto, false, 0), 0);
        assert_eq!(state_bits(Lang::Auto, true, 0), IT);
        assert_eq!(state_bits(Lang::En, true, 0), EXPLICIT);
        assert_eq!(state_bits(Lang::It, false, 0), EXPLICIT | IT);
        assert_eq!(state_bits(Lang::En, false, PREVIEW), EXPLICIT | PREVIEW);
        assert_eq!(state_bits(Lang::Auto, true, PREVIEW), IT | PREVIEW);
        assert_eq!(state_bits(Lang::Auto, false, IT | EXPLICIT | PREVIEW), PREVIEW);
    }
}
