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

pub use oled_sys::i18n::{locale, set_preview};

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
    fn the_override_steers_italian() {
        with_italian(true, || assert!(italian()));
        with_italian(false, || assert!(!italian()));
    }
}
