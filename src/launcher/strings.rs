//! Every user-visible string of the launcher: one function each, English and Italian through `i18n::tr`.
//! Later tasks append theirs.

use super::model::ProviderId;
use crate::i18n::tr;

pub fn placeholder() -> &'static str {
    tr("Search apps, files, settings, commands", "Cerca app, file, impostazioni, comandi")
}

/// The mode chip of a provider (5.2.4); None for the providers that have no prefix.
pub fn chip(p: ProviderId) -> Option<&'static str> {
    Some(match p {
        ProviderId::Calc | ProviderId::Units => tr("Calc", "Calcolo"),
        ProviderId::Run => tr("Run", "Esegui"),
        ProviderId::Windows => tr("Windows", "Finestre"),
        ProviderId::Web => tr("Web", "Web"),
        ProviderId::Settings => tr("Settings", "Impostazioni"),
        ProviderId::DateTime => tr("Time", "Data e ora"),
        ProviderId::Registry => tr("Registry", "Registro"),
        ProviderId::Files => tr("Files", "File"),
        ProviderId::Apps | ProviderId::Commands => return None,
    })
}

// ---- hints of the selected row
pub fn hint_open() -> &'static str {
    tr("Open · Enter", "Apri · Invio")
}
pub fn hint_switch() -> &'static str {
    tr("Switch · Enter", "Passa · Invio")
}
pub fn hint_copy() -> &'static str {
    tr("Copy · Enter", "Copia · Invio")
}
pub fn hint_run() -> &'static str {
    tr("Run · Enter", "Esegui · Invio")
}
pub fn hint_run_in_console() -> &'static str {
    tr("Run in console · Enter", "Esegui nella console · Invio")
}

// ---- verb labels (the action list and the hints)
pub fn verb_open() -> &'static str {
    tr("Open", "Apri")
}
pub fn verb_switch() -> &'static str {
    tr("Switch to", "Passa a")
}
pub fn verb_run_as_admin() -> &'static str {
    tr("Run as administrator", "Esegui come amministratore")
}
pub fn verb_reveal() -> &'static str {
    tr("Show in folder", "Mostra nella cartella")
}
pub fn verb_copy() -> &'static str {
    tr("Copy", "Copia")
}
pub fn verb_close_window() -> &'static str {
    tr("Close window", "Chiudi finestra")
}
pub fn verb_forget() -> &'static str {
    tr("Forget", "Dimentica")
}
pub fn verb_run_in_console() -> &'static str {
    tr("Run in console", "Esegui nella console")
}

// ---- detail lines and notices
/// Prefix of the detail line of a run line: the line itself follows.
pub fn run_by_windows() -> &'static str {
    tr("Run by Windows: ", "Eseguito da Windows: ")
}
pub fn line_too_long() -> &'static str {
    tr("Line too long to review", "Riga troppo lunga da verificare")
}
pub fn no_window_to_act_on() -> &'static str {
    tr("No window to act on", "Nessuna finestra su cui agire")
}
pub fn explorer_not_answering() -> &'static str {
    tr("Couldn't start it: Explorer isn't answering", "Impossibile avviarlo: Esplora risorse non risponde")
}
pub fn thousands() -> &'static str {
    tr("(thousands)", "(migliaia)")
}

// ---- the one-time offer (5.2.1)
pub fn offer_text() -> &'static str {
    tr(
        "New: a launcher for apps, files and commands. Use Alt+Space",
        "Novità: un launcher per app, file e comandi. Usa Alt+Spazio",
    )
}
pub fn offer_use_alt_space() -> &'static str {
    tr("Use Alt+Space", "Usa Alt+Spazio")
}
pub fn offer_use_alt_shift_space() -> &'static str {
    tr("Use Alt+Shift+Space", "Usa Alt+Maiusc+Spazio")
}
pub fn offer_dismiss() -> &'static str {
    tr("Don't show again", "Non mostrare più")
}

/// The window name (the launcher keeps this static name for screen readers).
pub fn window_name() -> &'static str {
    tr("Launcher", "Launcher")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn english_strings_are_not_empty_and_chips_cover_the_prefixed_providers() {
        // The test binary runs in English (the language byte is never set in tests).
        assert_eq!(placeholder(), "Search apps, files, settings, commands");
        assert_eq!(hint_open(), "Open · Enter");
        assert_eq!(run_by_windows(), "Run by Windows: ");
        assert_eq!(chip(ProviderId::Calc), Some("Calc"));
        assert_eq!(chip(ProviderId::Apps), None);
        for p in [
            ProviderId::Windows,
            ProviderId::Settings,
            ProviderId::Run,
            ProviderId::Web,
            ProviderId::DateTime,
            ProviderId::Registry,
            ProviderId::Files,
        ] {
            assert!(chip(p).is_some_and(|c| !c.is_empty()), "{p:?}");
        }
        assert_eq!(window_name(), "Launcher");
    }
}
