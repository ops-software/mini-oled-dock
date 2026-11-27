//! The keyboard sheet (F1): a wide kit Dialog listing the key map, one line per keys.row_<n> (T5.4).

use oled_ui::widgets::DialogSpec;
use oled_ui::{CommandId, EventCx, UiLang};

use super::commands::KEYS;
use super::keys::chord_label;
use super::screen::TasksScreen;
use crate::strings::{k, t};

/// What the sheet's one button answers: the dialog closes itself and dispatch ignores the id.
const CLOSE: CommandId = CommandId(0x4001);

/// A piece that starts with this is joined to the one before it without a separator ("Ctrl+Shift+1" "–4").
const RANGE: char = '–';

/// The piece that stands for the Menu key (a word, not a chord).
const MENU_KEY: &str = "@menu";

/// The chords of each row of the map, in the order of the strings keys.row_1 to keys.row_21.
const ROWS: [&[&str]; 21] = [
    &["Ctrl+1", "Ctrl+Tab", "Ctrl+Shift+Tab"],
    &["Tab", "Shift+Tab"],
    &["F6", "Shift+F6"],
    &["Ctrl+F", "/"],
    &["F5"],
    &["Ctrl+P", "Pause"],
    &["Ctrl+=", "Ctrl+-"],
    &["F10", "Alt+M"],
    &["F1"],
    &["Up", "Down", "PgUp", "PgDn", "Home", "End"],
    &["Shift+Down", "Ctrl+Down", "Ctrl+Space", "Ctrl+A"],
    &["Right", "Left"],
    &["Space", "Enter"],
    &["Del"],
    &["Shift+Del"],
    &["Shift+F10", MENU_KEY],
    &["Ctrl+G", "Ctrl+T", "Ctrl+L"],
    &["Ctrl+Shift+1", "–4"],
    &["Left", "Right", "Enter"],
    &["Esc"],
    &["Ctrl+W", "Alt+F4"],
];

const TEXTS: [oled_sys::i18n::S; 21] = [
    k::TASKS_KEYS_ROW_1,
    k::TASKS_KEYS_ROW_2,
    k::TASKS_KEYS_ROW_3,
    k::TASKS_KEYS_ROW_4,
    k::TASKS_KEYS_ROW_5,
    k::TASKS_KEYS_ROW_6,
    k::TASKS_KEYS_ROW_7,
    k::TASKS_KEYS_ROW_8,
    k::TASKS_KEYS_ROW_9,
    k::TASKS_KEYS_ROW_10,
    k::TASKS_KEYS_ROW_11,
    k::TASKS_KEYS_ROW_12,
    k::TASKS_KEYS_ROW_13,
    k::TASKS_KEYS_ROW_14,
    k::TASKS_KEYS_ROW_15,
    k::TASKS_KEYS_ROW_16,
    k::TASKS_KEYS_ROW_17,
    k::TASKS_KEYS_ROW_18,
    k::TASKS_KEYS_ROW_19,
    k::TASKS_KEYS_ROW_20,
    k::TASKS_KEYS_ROW_21,
];

/// The keys of one row as the sheet shows them.
fn keys_of(row: usize, lang: UiLang) -> String {
    let mut out = String::new();
    for piece in ROWS[row] {
        if let Some(rest) = piece.strip_prefix(RANGE) {
            out.push(RANGE);
            out.push_str(rest);
            continue;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        if *piece == MENU_KEY {
            out.push_str(&t(lang, k::TASKS_KEYS_MENU_KEY));
        } else {
            out.push_str(&chord_label(piece, lang));
        }
    }
    out
}

/// One line per row: its keys, then what they do.
pub fn lines(lang: UiLang) -> Vec<String> {
    (0..ROWS.len()).map(|i| format!("{}: {}", keys_of(i, lang), t(lang, TEXTS[i]))).collect()
}

/// The dialog: the title, the lines, one button that closes it.
pub fn spec(lang: UiLang) -> DialogSpec {
    DialogSpec {
        title: t(lang, k::TASKS_KEYS_TITLE),
        body: lines(lang).join("\n").into(),
        buttons: vec![(t(lang, k::TASKS_ABOUT_CLOSE), CLOSE)],
        wide: true,
    }
}

/// KEYS (F1 and the More menu): opens the sheet; its button (or Esc) closes it.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    if id != KEYS || s.notices.dialog_open() {
        return;
    }
    s.notices.open_dialog(cx, spec(cx.lang()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_has_its_keys_and_its_words() {
        for lang in [UiLang::EN, UiLang::IT] {
            let lines = lines(lang);
            assert_eq!(lines.len(), 21);
            for (i, l) in lines.iter().enumerate() {
                let (keys, text) = l.split_once(": ").unwrap_or_else(|| panic!("row {i}: {l}"));
                assert!(!keys.is_empty() && !text.is_empty(), "row {i}: {l}");
            }
        }
    }
}
