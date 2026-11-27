//! The engine's user-visible strings (OP5c): the ops host's shutdown-block reason, in English and Italian. The UI
//! moves these keys into its own string table later; the language follows the user's UI language.

use oled_trust::frame::files::OpKind;
use windows::Win32::Globalization::GetUserDefaultUILanguage;

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

/// The table: key, English, Italian. {n} is the number of items.
pub const STRINGS: [(&str, &str, &str); 3] = [
    ("files.ops.block.copy", "Copying {n} items", "Copia di {n} elementi in corso"),
    ("files.ops.block.move", "Moving {n} items", "Spostamento di {n} elementi in corso"),
    ("files.ops.block.delete", "Deleting {n} items", "Eliminazione di {n} elementi in corso"),
];

/// The text of `key` in `lang`, or the key itself when the table has no such key.
pub fn text(lang: Lang, key: &str) -> &str {
    STRINGS.iter().find(|(k, _, _)| *k == key).map_or(key, |(_, en, it)| if lang == Lang::It { it } else { en })
}

/// The reason Windows shows while the host blocks a sign-out (files.ops.block): what the first running operation
/// does, and how many items all of them hold.
pub fn block_text(lang: Lang, kind: OpKind, n: u64) -> String {
    let key = match kind {
        OpKind::Move | OpKind::Rename | OpKind::Undo => "files.ops.block.move",
        OpKind::Recycle | OpKind::DeletePermanent => "files.ops.block.delete",
        OpKind::Copy | OpKind::Resume | OpKind::NewFolder => "files.ops.block.copy",
    };
    text(lang, key).replace("{n}", &n.to_string())
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
}
