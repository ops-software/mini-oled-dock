//! UI language of the whole dock: English or Italian, chosen in Settings (`config.language`).
//!
//! `"auto"` (the default) follows the Windows display language: Italian when it is Italian, English
//! otherwise. Every visible string is a key (`t(k::KEY)`) into the generated tables compiled into the binary from
//! strings/en.toml and strings/it.toml: no allocation, no file; the only state is one byte. `apply` is called by
//! `config::set`, so every way the config changes (start, Settings, hand edit) switches the language;
//! the bar is rebuilt and every popup reads the labels again when it is (re)built.
//!
//! The NLS calls (clock, date, calendar day and month names) use `locale()`: the user locale with
//! `"auto"`, `en-US` / `it-IT` when a language is chosen, so names follow the dock language too.
//! Preview mode (`--preview`) is always English.

use oled_sys::i18n::Lang;

use crate::config::Language;

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

pub use oled_sys::i18n::locale;

/// True once preview mode was switched on in this process: previews never read the data file.
static PREVIEW_SEEN: AtomicBool = AtomicBool::new(false);
/// True once the load of lang\it.lang was tried (once per process).
static LOAD_TRIED: AtomicBool = AtomicBool::new(false);

/// Preview mode (`--preview`): English labels whatever the config says.
pub fn set_preview(on: bool) {
    if on {
        PREVIEW_SEEN.store(true, Relaxed);
    }
    oled_sys::i18n::set_preview(on);
}

/// Pure: the data file is read only by an Italian UI of the installed dock.exe, never in a preview.
fn should_load(italian: bool, runs_installed_image: bool, preview: bool) -> bool {
    italian && runs_installed_image && !preview
}

/// Why a file is not used, or the checked file. Pure over the bytes and the running version.
fn check_lang(bytes: Vec<u8>, version: (u16, u16, u16)) -> Result<oled_sys::lang::LangFile, String> {
    let f = oled_sys::lang::LangFile::parse(bytes).map_err(|e| format!("{e:?}"))?;
    if f.tag() != "it" {
        return Err("tag".to_string());
    }
    if f.suite_version() != version {
        return Err("version".to_string());
    }
    Ok(f)
}

/// Reads <install dir>\lang\it.lang the first time the UI is Italian in the installed dock.
fn load_lang_file() {
    if LOAD_TRIED.swap(true, Relaxed) {
        return;
    }
    let Some(path) =
        crate::autostart::installed_exe().ok().and_then(|e| e.parent().map(|d| d.join("lang").join("it.lang")))
    else {
        return;
    };
    let v = crate::update_release::Version::current();
    let want = (v.major as u16, v.minor as u16, v.patch as u16);
    let why = match oled_sys::datadir::read_file(&path, oled_sys::lang::LANG_MAX_BYTES as u64, true) {
        Ok(bytes) => match check_lang(bytes, want) {
            Ok(f) => {
                oled_sys::i18n::set_lang_file(Box::leak(Box::new(f)));
                return;
            }
            Err(w) => w,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => e.to_string(),
    };
    crate::log!("i18n: lang\\it.lang not used ({why})");
}

/// The runtime's name for a config language.
fn lang_of(l: Language) -> Lang {
    match l {
        Language::Auto => Lang::Auto,
        Language::En => Lang::En,
        Language::It => Lang::It,
    }
}

/// Sets the language from the config value (called by `config::set`).
pub fn apply(lang: Language) {
    oled_sys::i18n::apply(lang_of(lang));
    if !LOAD_TRIED.load(Relaxed)
        && oled_sys::i18n::italian()
        && should_load(true, crate::update::guard::runs_installed(), PREVIEW_SEEN.load(Relaxed))
    {
        load_lang_file();
    }
}

/// True when the dock speaks Italian.
pub fn italian() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_ITALIAN.with(std::cell::Cell::get) {
        return on;
    }
    oled_sys::i18n::italian()
}

#[cfg(test)]
thread_local! {
    /// A test's own UI language: the process-wide state is never flipped, so tests running in parallel keep seeing
    /// English.
    static TEST_ITALIAN: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with the UI language of this thread set to Italian or English; restored afterwards, also on unwind.
#[cfg(test)]
pub fn with_italian<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_ITALIAN.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(TEST_ITALIAN.with(|c| c.replace(Some(on))));
    f()
}

/// The generated string tables of the dock (strings/en.toml and it.toml).
pub mod strings {
    include!(concat!(env!("OUT_DIR"), "/strings.rs"));
}
pub use strings::k;

/// The text of a key of the kit's own table (the suite apps' display names) in the dock language.
pub fn kit(key: oled_sys::i18n::S) -> &'static str {
    match (oled_ui::strings::UiLang { italian: italian(), pseudo: false }).kit(key) {
        std::borrow::Cow::Borrowed(s) => s,
        std::borrow::Cow::Owned(_) => "",
    }
}

/// The text of a key in the dock language.
pub fn t(key: oled_sys::i18n::S) -> &'static str {
    strings::TABLE.get(key, italian())
}

/// The string in the dock language, for the two launcher data tables whose texts are identity and search data.
#[inline]
pub fn pick(en: &'static str, it: &'static str) -> &'static str {
    if italian() {
        it
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_of_maps_every_variant() {
        // No wildcard: a new Language variant fails to compile here.
        for l in [Language::Auto, Language::En, Language::It] {
            let want = match l {
                Language::Auto => Lang::Auto,
                Language::En => Lang::En,
                Language::It => Lang::It,
            };
            assert_eq!(lang_of(l), want);
        }
    }

    /// Release-build cost of one `t()`: 1,000,000 lookups cycling over every key, prints ns per lookup (budget: at most 60).
    /// `cargo test --release --bin dock t_lookup_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "a bench: run it on a release build"]
    fn t_lookup_cost() {
        let hashes = strings::TABLE.hashes;
        assert!(!hashes.is_empty());
        let start = std::time::Instant::now();
        let mut bytes = 0usize;
        for i in 0..1_000_000usize {
            bytes += t(oled_sys::i18n::S(hashes[i % hashes.len()])).len();
        }
        let ns = start.elapsed().as_nanos() as f64 / 1_000_000.0;
        println!("t_lookup_cost: {ns:.1} ns per lookup over {} keys ({bytes} bytes)", hashes.len());
        assert!(bytes > 0);
    }

    #[test]
    fn should_load_table() {
        for it in [false, true] {
            for inst in [false, true] {
                for pv in [false, true] {
                    assert_eq!(should_load(it, inst, pv), it && inst && !pv);
                }
            }
        }
    }

    #[test]
    fn a_bad_file_keeps_the_compiled_table() {
        let ok = oled_sys::lang::encode("it", (1, 8, 0), &[(1, "a")]).unwrap();
        assert!(check_lang(ok.clone(), (1, 8, 0)).is_ok());
        assert_eq!(check_lang(ok, (1, 8, 1)).err().as_deref(), Some("version"));
        let en = oled_sys::lang::encode("en", (1, 8, 0), &[(1, "a")]).unwrap();
        assert_eq!(check_lang(en, (1, 8, 0)).err().as_deref(), Some("tag"));
        let mut bad = oled_sys::lang::encode("it", (1, 8, 0), &[(1, "a")]).unwrap();
        bad[0] = b'X';
        assert_eq!(check_lang(bad, (1, 8, 0)).err().as_deref(), Some("Magic"));
    }

    #[test]
    fn the_override_steers_italian() {
        with_italian(true, || assert!(italian()));
        with_italian(false, || assert!(!italian()));
    }
}
