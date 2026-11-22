//! Every user-visible string of the launcher: one function each, English and Italian through `i18n::tr`.
//! Later tasks append theirs.

use super::model::{Choice, KeyChord, Mods, ProviderId};
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
    tr("Open file location", "Apri percorso file")
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

/// "New window": Open on a running app starts a new instance (the action list, Shift+Enter).
pub fn verb_new_window() -> &'static str {
    tr("New window", "Nuova finestra")
}
/// The notice's own entry of the action list.
pub fn verb_dont_show_again() -> &'static str {
    tr("Don't show again", "Non mostrare più")
}

/// The name of a key chord as the action list shows it (right-aligned, CAPTION): "Ctrl+Shift+Enter", "Shift+Del".
pub fn key_name(c: KeyChord) -> String {
    let mut s = String::new();
    for (m, name) in [(Mods::ALT, "Alt"), (Mods::CTRL, "Ctrl"), (Mods::SHIFT, tr("Shift", "Maiusc"))] {
        if c.mods.has(m) {
            s.push_str(name);
            s.push('+');
        }
    }
    match c.vk {
        0x0D => s.push_str(tr("Enter", "Invio")),
        0x2E => s.push_str(tr("Del", "Canc")),
        0x20 => s.push_str(tr("Space", "Spazio")),
        vk @ (0x30..=0x39 | 0x41..=0x5A) => s.extend(char::from_u32(u32::from(vk))),
        _ => s.push('?'),
    }
    s
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
pub fn thousands() -> &'static str {
    tr("(thousands)", "(migliaia)")
}
/// Appended to the detail line of a window that does not answer ("<" mode).
pub fn not_responding() -> &'static str {
    tr(" · Not responding", " · Non risponde")
}

// ---- web, registry
/// The web row (5.4.8): always names the host of the engine's template.
pub fn search_web(host: &str, query: &str) -> String {
    // The host passed validate_template (letters, digits, hyphens, dots), so it holds no "{q}" to replace again.
    tr("Search {host} for '{q}'", "Cerca '{q}' su {host}").replace("{host}", host).replacen("{q}", query, 1)
}
/// The hint row of "??" alone.
pub fn type_to_search(host: &str) -> String {
    tr("Type to search {host}", "Scrivi per cercare su {host}").replace("{host}", host)
}
/// The one row of the registry mode (":").
pub fn open_registry_editor() -> &'static str {
    tr("Open Registry Editor", "Apri l'Editor del Registro di sistema")
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
/// The offer row after the probe found Alt+Space taken: Alt+Shift+Space is proposed.
pub fn offer_alt_space_held() -> &'static str {
    tr("Another app holds Alt+Space. Use Alt+Shift+Space", "Un'altra app usa Alt+Spazio. Usa Alt+Maiusc+Spazio")
}
/// The offer row after the probe found both chords taken.
pub fn offer_both_held() -> &'static str {
    tr("Another app holds Alt+Space and Alt+Shift+Space", "Un'altra app usa Alt+Spazio e Alt+Maiusc+Spazio")
}

// ---- PowerToys (5.7)
/// The notice of the first chord the dock sees while Command Palette runs.
pub fn pt_shared() -> &'static str {
    tr(
        "PowerToys Command Palette is running and also uses Alt+Space.",
        "PowerToys Command Palette è attivo e usa anche Alt+Spazio.",
    )
}
/// Its detail: what taking the chord leaves to do.
pub fn pt_turn_off() -> &'static str {
    tr(
        "Turn its shortcut off in PowerToys > Command Palette, or Command Palette may still open on some presses",
        "Disattiva la sua scorciatoia in PowerToys > Command Palette, o Command Palette potrebbe ancora aprirsi a volte",
    )
}
/// The hint of that notice: Enter takes the chord.
pub fn hint_pt_take() -> &'static str {
    tr("Take Alt+Space · Enter", "Prendi Alt+Spazio · Invio")
}
/// The notice's choices in its action list.
pub fn choice_label(c: Choice) -> &'static str {
    match c {
        Choice::TakeChord => tr("Take Alt+Space", "Prendi Alt+Spazio"),
        Choice::LeaveChord => tr(
            "Leave it to Command Palette until I turn its shortcut off",
            "Lascialo a Command Palette finché non disattivo la sua scorciatoia",
        ),
        Choice::UseAltShiftSpace => tr("Use Alt+Shift+Space instead", "Usa invece Alt+Maiusc+Spazio"),
    }
}
/// The notice after two passed chords that Command Palette did not answer.
pub fn pt_took_over() -> &'static str {
    tr(
        "Command Palette is running but did not answer Alt+Space, so the launcher has it now",
        "Command Palette è attivo ma non ha risposto ad Alt+Spazio, quindi ora lo usa il launcher",
    )
}

// ---- refused launches (5.5.3)
/// The error row of a launch that did not reach the desktop shell.
pub fn launch_failed() -> &'static str {
    tr("Couldn't start it: Explorer isn't answering", "Impossibile avviarlo: Esplora risorse non risponde")
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

    #[test]
    fn key_names_of_the_verb_chords() {
        use crate::launcher::verbs::VERBS;
        let names: Vec<String> = VERBS.iter().filter_map(|v| v.chord).map(key_name).collect();
        assert_eq!(
            names,
            ["Enter", "Enter", "Ctrl+Shift+Enter", "Ctrl+Enter", "Ctrl+C", "Ctrl+W", "Shift+Del", "Shift+Enter"]
        );
        assert_eq!(key_name(KeyChord { vk: 0x20, mods: Mods::ALT }), "Alt+Space");
        assert!(verb_new_window().len() > 3 && verb_dont_show_again().len() > 3);
    }

    #[test]
    fn powertoys_strings_follow_the_spec() {
        assert_eq!(pt_shared(), "PowerToys Command Palette is running and also uses Alt+Space.");
        assert_eq!(choice_label(Choice::TakeChord), "Take Alt+Space");
        assert_eq!(choice_label(Choice::LeaveChord), "Leave it to Command Palette until I turn its shortcut off");
        assert_eq!(choice_label(Choice::UseAltShiftSpace), "Use Alt+Shift+Space instead");
        assert!(pt_turn_off().starts_with("Turn its shortcut off in PowerToys > Command Palette"));
        assert!(pt_took_over().ends_with("so the launcher has it now"));
        assert_eq!(hint_pt_take(), "Take Alt+Space · Enter");
    }
}
