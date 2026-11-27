//! The engine's user-visible strings (OP5c): the ops host's shutdown-block reason. The texts are the window's own
//! (files.ops.block_copy, block_move and block_delete in tr's table, F3.6); the language follows the user's UI
//! language, Italian for every Italian sublanguage, else English.

use std::sync::LazyLock;

use oled_sys::i18n::S;
use oled_trust::frame::files::OpKind;
use oled_ui::UiLang;
use windows::Win32::Globalization::GetUserDefaultUILanguage;

use crate::tr::{self, k};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    It,
}

/// The primary language id of Italian (winnt.h LANG_ITALIAN).
const LANG_ITALIAN: u16 = 0x10;

/// The language of a LANGID: Italian for every Italian sublanguage, else English. Pure.
pub fn lang_of(langid: u16) -> Lang {
    if langid & 0x3FF == LANG_ITALIAN {
        Lang::It
    } else {
        Lang::En
    }
}

/// The user's UI language.
pub fn lang() -> Lang {
    // SAFETY: no arguments; reads the user's setting.
    lang_of(unsafe { GetUserDefaultUILanguage() })
}

/// The keys of the block texts, in tr's table.
const KEYS: [(&str, S); 3] = [
    ("files.ops.block_copy", k::FILES_OPS_BLOCK_COPY),
    ("files.ops.block_move", k::FILES_OPS_BLOCK_MOVE),
    ("files.ops.block_delete", k::FILES_OPS_BLOCK_DELETE),
];

/// The table as (key, English, Italian), read from tr's generated table once.
#[derive(Clone, Copy, Debug)]
pub struct Strings;

type Row = (&'static str, &'static str, &'static str);

static ROWS: LazyLock<[Row; 3]> =
    LazyLock::new(|| KEYS.map(|(key, s)| (key, tr::gen::TABLE.get(s, false), tr::gen::TABLE.get(s, true))));

impl Strings {
    pub fn iter(&self) -> std::slice::Iter<'static, Row> {
        ROWS.iter()
    }

    pub fn len(&self) -> usize {
        ROWS.len()
    }

    pub fn is_empty(&self) -> bool {
        ROWS.is_empty()
    }
}

impl IntoIterator for Strings {
    type Item = &'static Row;
    type IntoIter = std::slice::Iter<'static, Row>;
    fn into_iter(self) -> Self::IntoIter {
        ROWS.iter()
    }
}

/// The table: key, English, Italian. {n} is the number of items.
pub static STRINGS: Strings = Strings;

/// The text of the key in the language, or the key itself when the table has no such key.
pub fn text(lang: Lang, key: &str) -> &str {
    STRINGS.iter().find(|(k, _, _)| *k == key).map_or(key, |(_, en, it)| if lang == Lang::It { it } else { en })
}

/// The reason Windows shows while the host blocks a sign-out (files.ops.block): what the first running operation
/// does, and how many items all of them hold.
pub fn block_text(lang: Lang, kind: OpKind, n: u64) -> String {
    let key = match kind {
        OpKind::Move | OpKind::Rename | OpKind::Undo => k::FILES_OPS_BLOCK_MOVE,
        OpKind::Recycle | OpKind::DeletePermanent => k::FILES_OPS_BLOCK_DELETE,
        OpKind::Copy | OpKind::Resume | OpKind::NewFolder => k::FILES_OPS_BLOCK_COPY,
    };
    let ui = UiLang { italian: lang == Lang::It, pseudo: false };
    tr::tf(key, ui, &[("n", &n.to_string())]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_text_en_it() {
        assert_eq!(block_text(Lang::En, OpKind::Copy, 3), "Copying 3 items");
        assert_eq!(block_text(Lang::En, OpKind::Resume, 1), "Copying 1 items");
        assert_eq!(block_text(Lang::En, OpKind::Move, 12), "Moving 12 items");
        assert_eq!(block_text(Lang::En, OpKind::Recycle, 2), "Deleting 2 items");
        assert_eq!(block_text(Lang::En, OpKind::DeletePermanent, 2), "Deleting 2 items");
        assert_eq!(block_text(Lang::It, OpKind::Copy, 3), "Copia di 3 elementi in corso");
        assert_eq!(block_text(Lang::It, OpKind::Move, 4), "Spostamento di 4 elementi in corso");
        assert_eq!(block_text(Lang::It, OpKind::DeletePermanent, 5), "Eliminazione di 5 elementi in corso");
        assert_eq!(lang_of(0x0410), Lang::It, "it-IT");
        assert_eq!(lang_of(0x0810), Lang::It, "it-CH");
        assert_eq!(lang_of(0x0409), Lang::En, "en-US");
        assert_eq!(lang_of(0x0407), Lang::En, "de-DE falls back to English");
        assert_eq!(text(Lang::It, "files.missing"), "files.missing");
        let keys: std::collections::HashSet<_> = STRINGS.iter().map(|(k, _, _)| *k).collect();
        assert_eq!(keys.len(), STRINGS.len(), "unique keys");
        for (_, en, it) in STRINGS {
            assert!(en.contains("{n}") && it.contains("{n}"));
        }
    }

    #[test]
    fn block_texts_from_the_table() {
        for (kind, key) in [
            (OpKind::Copy, k::FILES_OPS_BLOCK_COPY),
            (OpKind::Move, k::FILES_OPS_BLOCK_MOVE),
            (OpKind::DeletePermanent, k::FILES_OPS_BLOCK_DELETE),
        ] {
            for (lang, italian) in [(Lang::En, false), (Lang::It, true)] {
                let table = tr::gen::TABLE.get(key, italian).replace("{n}", "7");
                assert_eq!(block_text(lang, kind, 7), table, "{kind:?} {lang:?}");
            }
        }
        for (key, en, it) in STRINGS.iter() {
            let s = KEYS.iter().find(|(k, _)| k == key).map(|(_, s)| *s).expect("a known key");
            assert_eq!((*en, *it), (tr::gen::TABLE.get(s, false), tr::gen::TABLE.get(s, true)));
        }
        assert_eq!(text(Lang::It, "files.ops.block_delete"), "Eliminazione di {n} elementi in corso");
    }
}
