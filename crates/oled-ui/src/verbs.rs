//! The suite's verb glossary (SPEC-platform 5.3.5): one meaning per verb and per key across the apps. K0.1c writes the
//! types with the labels from the kit's table; K2a.8 fills the chords and check_table.

use crate::command::{Chord, Command, CommandId, CommandTable};
use crate::node::Text;
use crate::strings::{k, UiLang};

/// A verb of the glossary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verb {
    Close,
    End,
    EndNow,
    Delete,
    DeletePermanently,
    Restart,
}

impl Verb {
    /// The verb's label and description keys (kit.verb.*).
    fn keys(self) -> (oled_sys::i18n::S, oled_sys::i18n::S) {
        match self {
            Verb::Close => (k::KIT_VERB_CLOSE, k::KIT_VERB_CLOSE_DESC),
            Verb::End => (k::KIT_VERB_END, k::KIT_VERB_END_DESC),
            Verb::EndNow => (k::KIT_VERB_END_NOW, k::KIT_VERB_END_NOW_DESC),
            Verb::Delete => (k::KIT_VERB_DELETE, k::KIT_VERB_DELETE_DESC),
            Verb::DeletePermanently => (k::KIT_VERB_DELETE_PERMANENTLY, k::KIT_VERB_DELETE_PERMANENTLY_DESC),
            Verb::Restart => (k::KIT_VERB_RESTART, k::KIT_VERB_RESTART_DESC),
        }
    }

    /// "Close", "Chiudi".
    pub fn label(self, lang: UiLang) -> Text {
        lang.kit(self.keys().0)
    }

    /// "Asks it to close. It may ask to save."
    pub fn description(self, lang: UiLang) -> Text {
        lang.kit(self.keys().1)
    }

    /// Del / Shift+Del; Restart: menu only. Filled by K2a.8; until then None.
    pub fn chord(self) -> Option<Chord> {
        None
    }

    /// Delete: Ctrl+D. Filled by K2a.8; until then None.
    pub fn alt_chord(self) -> Option<Chord> {
        None
    }

    /// The verb as a command: its label, description and chords.
    pub fn command(self, id: CommandId, lang: UiLang) -> Command {
        Command {
            id,
            label: self.label(lang),
            glyph: None,
            chord: self.chord(),
            alt_chord: self.alt_chord(),
            enabled: true,
            checked: None,
            description: Some(self.description(lang)),
        }
    }
}

/// Fails on a glossary chord bound to another meaning or a reworded verb. Filled by K2a.8; until then Ok.
pub fn check_table(t: &CommandTable, lang: UiLang) -> Result<(), String> {
    let _ = (t, lang);
    Ok(())
}
