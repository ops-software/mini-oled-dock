//! The kit's languages and strings (F-K1, P2-kit 2.4, 2.4.1). The kit's own texts are the keyed table generated from
//! crates/oled-ui/strings/{en,it}.toml; it is read with an explicit language, never through the process-wide state, so
//! English and Italian tests run side by side.

use std::borrow::Cow;

use crate::node::Text;
pub use oled_sys::i18n::Lang;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/strings.rs"));
}
pub use generated::k;

/// The language a window speaks, resolved once per EventLoop or Harness; what every cx.lang() returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct UiLang {
    pub italian: bool,
    pub pseudo: bool,
}

impl UiLang {
    pub const EN: UiLang = UiLang { italian: false, pseudo: false };
    pub const IT: UiLang = UiLang { italian: true, pseudo: false };

    /// En and It as they are; Auto takes oled_sys::i18n::italian() at that moment.
    pub fn from_lang(l: Lang) -> UiLang {
        match l {
            Lang::En => UiLang::EN,
            Lang::It => UiLang::IT,
            Lang::Auto => UiLang { italian: oled_sys::i18n::italian(), pseudo: false },
        }
    }

    /// A key of `table` in this language, pseudo-localised on top when pseudo is on.
    pub fn text(self, table: &'static oled_sys::i18n::Table, k: oled_sys::i18n::S) -> Text {
        let s = table.get(k, self.italian);
        if self.pseudo {
            Cow::Owned(pseudo(s))
        } else {
            Cow::Borrowed(s)
        }
    }

    /// A key of the kit's own table.
    pub fn kit(self, k: oled_sys::i18n::S) -> Text {
        self.text(&generated::TABLE, k)
    }
}

/// The pseudo-locale (B00 5.5): letters accented, 40 % padding, in brackets: "Settings" -> "[Ŝéţţîñĝš~~~]".
pub fn pseudo(s: &str) -> String {
    const LOWER: [char; 26] = [
        'á', 'ƀ', 'ç', 'ð', 'é', 'ƒ', 'ĝ', 'ĥ', 'î', 'ĵ', 'ķ', 'ļ', 'ɱ', 'ñ', 'ö', 'þ', 'ǫ', 'ŕ', 'š', 'ţ', 'û', 'ṽ',
        'ŵ', 'ẋ', 'ý', 'ž',
    ];
    const UPPER: [char; 26] = [
        'Å', 'Ɓ', 'Ç', 'Ð', 'É', 'Ƒ', 'Ĝ', 'Ĥ', 'Î', 'Ĵ', 'Ķ', 'Ļ', 'Ṁ', 'Ñ', 'Ö', 'Þ', 'Ǫ', 'Ŕ', 'Ŝ', 'Ţ', 'Û', 'Ṽ',
        'Ŵ', 'Ẋ', 'Ý', 'Ž',
    ];
    let n = s.chars().count();
    let pad = (n * 4 + 5) / 10;
    let mut out = String::with_capacity(s.len() * 2 + pad + 2);
    out.push('[');
    for c in s.chars() {
        out.push(match c {
            'a'..='z' => LOWER[(c as u8 - b'a') as usize],
            'A'..='Z' => UPPER[(c as u8 - b'A') as usize],
            other => other,
        });
    }
    out.extend(std::iter::repeat_n('~', pad.max(1)));
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kit_table_in_both_languages() {
        assert_eq!(UiLang::EN.kit(k::KIT_CLOSE), "Close");
        assert_eq!(UiLang::IT.kit(k::KIT_CLOSE), "Chiudi");
        assert_eq!(UiLang::IT.kit(k::KIT_KEY_SHIFT), "Maiusc");
        assert_eq!(UiLang::EN.kit(k::KIT_VERB_CLOSE_DESC), "Asks it to close. It may ask to save.");
        assert_eq!(UiLang::from_lang(Lang::It), UiLang::IT);
        assert_eq!(UiLang::from_lang(Lang::En), UiLang::EN);
    }

    #[test]
    fn pseudo_accents_and_pads() {
        assert_eq!(pseudo("Settings"), "[Ŝéţţîñĝš~~~]");
        assert_eq!(pseudo("OK 1"), "[ÖĶ 1~~]");
        assert_eq!(pseudo(""), "[~]");
        let p = UiLang { italian: false, pseudo: true }.kit(k::KIT_CLOSE);
        assert!(p.starts_with('[') && p.chars().count() > "Close".len(), "{p}");
    }
}
