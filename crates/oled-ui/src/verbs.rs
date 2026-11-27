//! The suite's verb glossary (SPEC-platform 5.3.5): one meaning per verb and per key across the apps. The labels and
//! descriptions come from the kit's table (kit.verb.*), the chords and check_table are K2a.8's.

use crate::command::{Chord, Command, CommandId, CommandTable};
use crate::event::{Key, Mods};
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
    /// Every verb, in the glossary's order.
    pub const ALL: [Verb; 6] =
        [Verb::Close, Verb::End, Verb::EndNow, Verb::Delete, Verb::DeletePermanently, Verb::Restart];

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

    /// Del (Close, End, Delete) or Shift+Del (End now, Delete permanently); Restart: menu only.
    pub fn chord(self) -> Option<Chord> {
        match self {
            Verb::Close | Verb::End | Verb::Delete => Some(DEL),
            Verb::EndNow | Verb::DeletePermanently => Some(SHIFT_DEL),
            Verb::Restart => None,
        }
    }

    /// Delete: Ctrl+D; no other verb has a second chord.
    pub fn alt_chord(self) -> Option<Chord> {
        match self {
            Verb::Delete => Some(CTRL_D),
            _ => None,
        }
    }

    /// Every chord the glossary lets this verb's command carry: its own two, and for Close the window chords too.
    fn allowed(self) -> Vec<Chord> {
        let mut v: Vec<Chord> = [self.chord(), self.alt_chord()].into_iter().flatten().collect();
        if self == Verb::Close {
            v.extend([CTRL_W, ALT_F4]);
        }
        v
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

const DEL: Chord = Chord::new(Mods::NONE, Key::DELETE);
const SHIFT_DEL: Chord = Chord::new(Mods::SHIFT, Key::DELETE);
const CTRL_D: Chord = Chord::new(Mods::CTRL, Key::letter('D'));
const CTRL_W: Chord = Chord::new(Mods::CTRL, Key::letter('W'));
const ALT_F4: Chord = Chord::new(Mods::ALT, Key::F4);
const F1: Chord = Chord::new(Mods::NONE, Key::F1);
const CTRL_COMMA: Chord = Chord::new(Mods::CTRL, Key::OEM_COMMA);

/// The chords the glossary reserves for one meaning across the apps. Those that belong to a verb are checked against
/// the verbs' labels; F1 (help) and Ctrl+, (settings) have no verb label to compare, so only their use is reserved.
const RESERVED: [Chord; 7] = [DEL, SHIFT_DEL, CTRL_D, CTRL_W, ALT_F4, F1, CTRL_COMMA];

/// Fails on a glossary chord bound to a command whose label is not the glossary's for it, or on a glossary label that
/// carries another chord (Restart carries none). `lang` is the language the table's labels were made in.
pub fn check_table(t: &CommandTable, lang: UiLang) -> Result<(), String> {
    for c in t.iter() {
        let chords = [c.chord, c.alt_chord];
        let verb = Verb::ALL.into_iter().find(|v| v.label(lang) == c.label);
        if let Some(v) = verb {
            let allowed = v.allowed();
            for chord in chords.into_iter().flatten() {
                if !allowed.contains(&chord) {
                    return Err(format!(
                        "command {:#x} is labelled \"{}\" but carries {}, which the glossary gives it no part in",
                        c.id.0,
                        c.label,
                        chord.label(UiLang::EN)
                    ));
                }
            }
            continue;
        }
        let close_tab = c.label == lang.kit(k::KIT_CLOSE_TAB);
        for chord in chords.into_iter().flatten().filter(|ch| RESERVED.contains(ch)) {
            // The window chords also close a tab (SPEC-platform 5.3.5: Ctrl+W closes the tab, else the window).
            if close_tab && (chord == CTRL_W || chord == ALT_F4) {
                continue;
            }
            let owners: Vec<Verb> = Verb::ALL.into_iter().filter(|v| v.allowed().contains(&chord)).collect();
            if owners.is_empty() {
                continue; // F1 and Ctrl+,: reserved, but there is no verb label to hold them to
            }
            return Err(format!(
                "command {:#x} (\"{}\") is bound to {}, which the glossary keeps for {}",
                c.id.0,
                c.label,
                chord.label(UiLang::EN),
                owners.iter().map(|v| format!("\"{}\"", v.label(lang))).collect::<Vec<_>>().join(" / ")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Through an alias: tests/workspace.rs reads the plain "Command" + "::new(" as a process spawn.
    use crate::command::Command as Cmd;

    fn table(cmds: Vec<Command>) -> CommandTable {
        let mut t = CommandTable::new();
        for c in cmds {
            t.add(c).unwrap();
        }
        t
    }

    #[test]
    fn glossary_text_is_verbatim() {
        let expect: [(Verb, &str, &str, &str, &str); 6] = [
            (
                Verb::Close,
                "Close",
                "Chiudi",
                "Asks it to close. It may ask to save.",
                "Gli chiede di chiudersi. Potrebbe chiedere di salvare.",
            ),
            (
                Verb::End,
                "End",
                "Termina",
                "Stops it. Anything unsaved in it is lost.",
                "Lo arresta. Quello che non è salvato va perso.",
            ),
            (
                Verb::EndNow,
                "End now",
                "Termina subito",
                "Stops it at once, without asking it to close.",
                "Lo arresta all'istante, senza chiedergli di chiudersi.",
            ),
            (
                Verb::Delete,
                "Delete",
                "Elimina",
                "Moves it to the Recycle Bin. You can restore it.",
                "Lo sposta nel Cestino. Puoi ripristinarlo.",
            ),
            (
                Verb::DeletePermanently,
                "Delete permanently",
                "Elimina definitivamente",
                "Skips the Recycle Bin. It cannot be undone.",
                "Non passa dal Cestino. Non si può annullare.",
            ),
            (Verb::Restart, "Restart", "Riavvia", "Stops it and starts it again.", "Lo arresta e lo avvia di nuovo."),
        ];
        for (v, en, it, en_d, it_d) in expect {
            assert_eq!((&*v.label(UiLang::EN), &*v.description(UiLang::EN)), (en, en_d));
            assert_eq!((&*v.label(UiLang::IT), &*v.description(UiLang::IT)), (it, it_d));
        }
        let chords: Vec<(Option<Chord>, Option<Chord>)> =
            Verb::ALL.iter().map(|v| (v.chord(), v.alt_chord())).collect();
        assert_eq!(
            chords,
            [
                (Some(DEL), None),
                (Some(DEL), None),
                (Some(SHIFT_DEL), None),
                (Some(DEL), Some(CTRL_D)),
                (Some(SHIFT_DEL), None),
                (None, None)
            ]
        );
        let c = Verb::Delete.command(CommandId(5), UiLang::EN);
        assert_eq!((c.chord, c.alt_chord), (Some(DEL), Some(CTRL_D)));
        assert_eq!(c.description.as_deref(), Some("Moves it to the Recycle Bin. You can restore it."));
    }

    #[test]
    fn check_table_accepts_the_glossary() {
        for lang in [UiLang::EN, UiLang::IT] {
            let mut cmds: Vec<Command> = Vec::new();
            // One table cannot hold Del three times: each verb in a table of its own, plus the whole set minus the
            // verbs that share a chord.
            for (i, v) in Verb::ALL.into_iter().enumerate() {
                assert!(check_table(&table(vec![v.command(CommandId(i as u32 + 1), lang)]), lang).is_ok());
            }
            cmds.push(Verb::Close.command(CommandId(1), lang).alt_chord(CTRL_W));
            cmds.push(Verb::EndNow.command(CommandId(2), lang));
            cmds.push(Verb::Restart.command(CommandId(3), lang));
            cmds.push(Cmd::new(CommandId(4), Text::from("Help")).chord(F1));
            cmds.push(Cmd::new(CommandId(6), lang.kit(k::KIT_CLOSE)).chord(ALT_F4));
            let mut t = table(cmds);
            assert!(check_table(&t, lang).is_ok());
            // The window chord on a tab's close command (a table of its own: Ctrl+W is Close's alt chord above).
            let tab = table(vec![Cmd::new(CommandId(5), lang.kit(k::KIT_CLOSE_TAB)).chord(CTRL_W)]);
            assert!(check_table(&tab, lang).is_ok());
            // An unrelated chord on a command of another name is none of the glossary's business.
            t.add(Cmd::new(CommandId(9), Text::from("Rename")).chord(Chord::parse("F2").unwrap())).unwrap();
            assert!(check_table(&t, lang).is_ok());
        }
    }

    #[test]
    fn check_table_rejects_rebound_del() {
        let lang = UiLang::EN;
        // Del on a command that is not Close, End or Delete.
        let t = table(vec![Cmd::new(CommandId(1), Text::from("Remove")).chord(DEL)]);
        let e = check_table(&t, lang).unwrap_err();
        assert!(e.contains("Remove") && e.contains("Del"), "{e}");
        // Shift+Del on "Delete" (that is Delete permanently's), Ctrl+D on "Remove", Alt+F4 on "Quit".
        let shifted = table(vec![Verb::Delete.command(CommandId(1), lang).chord(SHIFT_DEL)]);
        assert!(check_table(&shifted, lang).is_err());
        for (label, chord) in [("Remove", CTRL_D), ("Quit", ALT_F4), ("Hide", CTRL_W), ("Purge", SHIFT_DEL)] {
            let t = table(vec![Cmd::new(CommandId(1), Text::from(label)).chord(chord)]);
            assert!(check_table(&t, lang).is_err(), "{label}");
        }
        // "Close tab" holds the window chords only, not the row chords.
        let tab_del = table(vec![Cmd::new(CommandId(1), lang.kit(k::KIT_CLOSE_TAB)).chord(DEL)]);
        assert!(check_table(&tab_del, lang).is_err());
        // The glossary's label with another chord, and Restart with any chord.
        let end = table(vec![Cmd::new(CommandId(1), Verb::End.label(lang)).chord(F1)]);
        assert!(check_table(&end, lang).unwrap_err().contains("End"));
        let restart = table(vec![Cmd::new(CommandId(1), Verb::Restart.label(lang)).chord(CTRL_COMMA)]);
        assert!(check_table(&restart, lang).is_err());
        // Italian labels are held to the same rule.
        let it = table(vec![Cmd::new(CommandId(1), Verb::Delete.label(UiLang::IT)).chord(SHIFT_DEL)]);
        assert!(check_table(&it, UiLang::IT).is_err());
        // F1 and Ctrl+, have no verb label: any command may carry them.
        let free = table(vec![Cmd::new(CommandId(1), Text::from("Anything")).chord(CTRL_COMMA)]);
        assert!(check_table(&free, lang).is_ok());
    }
}
