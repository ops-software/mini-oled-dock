//! The system commands of 1.5.0 (5.4.5): a static table with English and Italian names and keywords. What each command
//! does (power requests, the window menu, the registry editor) is the session's and the shell thread's job.

use super::model::CommandId;
use crate::i18n::tr;

/// One command. Keywords are folded (lowercase, no accents) and include both display names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Command {
    pub id: CommandId,
    pub en: &'static str,
    pub it: &'static str,
    pub keywords: &'static [&'static str],
    /// Sleep, Restart, Shut down and Sign out: never the default row for a short query (5.2.7).
    pub destructive: bool,
}

impl Command {
    pub fn name(&self) -> &'static str {
        tr(self.en, self.it)
    }
}

pub static COMMANDS: [Command; 8] = [
    Command {
        id: CommandId::Lock,
        en: "Lock",
        it: "Blocca",
        keywords: &["lock", "blocca", "lock screen", "blocca schermo"],
        destructive: false,
    },
    Command {
        id: CommandId::Sleep,
        en: "Sleep",
        it: "Sospendi",
        keywords: &["sleep", "sospendi", "sospensione", "standby"],
        destructive: true,
    },
    Command {
        id: CommandId::Restart,
        en: "Restart",
        it: "Riavvia",
        keywords: &["restart", "riavvia", "riavvio", "reboot"],
        destructive: true,
    },
    Command {
        id: CommandId::ShutDown,
        en: "Shut down",
        it: "Arresta",
        keywords: &["shut down", "shutdown", "arresta", "arresta il sistema", "spegni", "spegnimento", "power off"],
        destructive: true,
    },
    Command {
        id: CommandId::SignOut,
        en: "Sign out",
        it: "Disconnetti",
        keywords: &["sign out", "signout", "log off", "logoff", "disconnetti", "esci", "esci dall'account"],
        destructive: true,
    },
    Command {
        id: CommandId::Registry,
        en: "Registry Editor",
        it: "Editor del Registro di sistema",
        keywords: &[
            "registry editor",
            "registry",
            "regedit",
            "editor del registro",
            "editor del registro di sistema",
            "registro",
        ],
        destructive: false,
    },
    Command {
        id: CommandId::WindowMenu,
        en: "Window menu",
        it: "Menu finestra",
        keywords: &["window menu", "menu finestra", "menu", "move", "size", "sposta", "ridimensiona", "finestra"],
        destructive: false,
    },
    // Windows' own history (Win+V, plan L2 D8): the dock never reads it.
    Command {
        id: CommandId::ClipboardHistory,
        en: "Clipboard history",
        it: "Cronologia degli appunti",
        keywords: &[
            "clipboard history",
            "cronologia degli appunti",
            "clipboard",
            "appunti",
            "cronologia appunti",
            "win+v",
        ],
        destructive: false,
    },
];

pub fn command(id: CommandId) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::keywords::destructive_names;
    use crate::launcher::rank::fold;

    fn folded(s: &str) -> String {
        let mut out = String::new();
        fold(s, &mut out);
        out
    }

    #[test]
    fn names_in_both_languages() {
        for c in &COMMANDS {
            assert!(!c.en.is_empty() && !c.it.is_empty() && !c.name().is_empty(), "{:?}", c.id);
            assert_ne!(c.en, c.it, "{:?}: both names read the same", c.id);
            assert!(c.keywords.contains(&folded(c.en).as_str()), "{:?}: English name is a keyword", c.id);
            assert!(c.keywords.contains(&folded(c.it).as_str()) || c.id == CommandId::Registry, "{:?}", c.id);
            for k in c.keywords {
                assert_eq!(folded(k), *k, "{:?}: keyword {k:?} is not folded", c.id);
            }
        }
        // Every command is in the table once.
        for id in [
            CommandId::Lock,
            CommandId::Sleep,
            CommandId::Restart,
            CommandId::ShutDown,
            CommandId::SignOut,
            CommandId::Registry,
            CommandId::WindowMenu,
            CommandId::ClipboardHistory,
        ] {
            assert_eq!(COMMANDS.iter().filter(|c| c.id == id).count(), 1, "{id:?}");
            assert!(command(id).is_some());
        }
        let wm = command(CommandId::WindowMenu).unwrap();
        for k in ["menu", "move", "size", "sposta", "ridimensiona", "finestra"] {
            assert!(wm.keywords.contains(&k), "{k}");
        }
        let reg = command(CommandId::Registry).unwrap();
        assert!(reg.keywords.contains(&"registry editor") && reg.keywords.contains(&"editor del registro"));
    }

    #[test]
    fn clipboard_history_row() {
        let c = command(CommandId::ClipboardHistory).unwrap();
        assert_eq!((c.en, c.it), ("Clipboard history", "Cronologia degli appunti"));
        for k in ["clipboard", "appunti", "cronologia appunti", "win+v"] {
            assert!(c.keywords.contains(&k), "{k}");
        }
        assert!(!c.destructive, "it only opens a panel");
        assert!(destructive_names(c.id).is_empty());
        crate::i18n::with_italian(true, || assert_eq!(c.name(), "Cronologia degli appunti"));
    }

    #[test]
    fn destructive_matches_the_rank_names() {
        let destructive: Vec<CommandId> = COMMANDS.iter().filter(|c| c.destructive).map(|c| c.id).collect();
        assert_eq!(destructive, [CommandId::Sleep, CommandId::Restart, CommandId::ShutDown, CommandId::SignOut]);
        for c in &COMMANDS {
            let names = destructive_names(c.id);
            assert_eq!(!names.is_empty(), c.destructive, "{:?}", c.id);
            for n in names {
                assert!(c.keywords.contains(n), "{:?}: {n:?} missing from the keywords", c.id);
            }
        }
    }
}
