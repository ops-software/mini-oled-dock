//! Every user-visible string of the launcher: one function each, English and Italian through `i18n::tr`.
//! Later tasks append theirs.

use super::aliases::AliasRefusal;
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
        // The alias prompt (plan L2-A2).
        ProviderId::Alias => tr("Alias", "Alias"),
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
/// A file or folder row that has no Open (an executable while the Mark of the Web check is open): Ctrl+Enter shows it.
pub fn hint_reveal() -> &'static str {
    tr("Show in folder · Ctrl+Enter", "Mostra nella cartella · Ctrl+Invio")
}
pub fn hint_run() -> &'static str {
    tr("Run · Enter", "Esegui · Invio")
}
/// The alias prompt's row: Enter saves.
pub fn hint_save() -> &'static str {
    tr("Save · Enter", "Salva · Invio")
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

// ---- aliases (plan L2-A2)

/// The action list's entry that opens the alias prompt for the selected row.
pub fn verb_assign_alias() -> &'static str {
    tr("Assign alias…", "Assegna alias…")
}
/// The action list's entry of an alias row.
pub fn verb_remove_alias() -> &'static str {
    tr("Remove alias", "Rimuovi alias")
}
/// The alias prompt's row when its word and target can be saved.
pub fn save_alias(word: &str) -> String {
    format!("{} “{word}”", tr("Save alias", "Salva alias"))
}
/// The alias prompt's row while the field is empty (a row's target given, or the "word target" form).
pub fn alias_type_word() -> &'static str {
    tr("Type a word for this alias", "Scrivi una parola per l'alias")
}
pub fn alias_type_word_target() -> &'static str {
    tr("Type a word, a space and what it opens", "Scrivi una parola, uno spazio e cosa apre")
}
/// An app alias whose app is not in the catalogue.
pub fn app_not_found() -> &'static str {
    tr("App not found", "App non trovata")
}
/// The notice row of an alias that could not be written to config.json.
pub fn alias_not_saved() -> &'static str {
    tr("Couldn't save the alias", "Impossibile salvare l'alias")
}
/// Why the alias prompt cannot save.
pub fn alias_refusal(why: AliasRefusal) -> &'static str {
    match why {
        AliasRefusal::Word => tr(
            "An alias is 1-16 letters, digits or - _ . +, starting with a letter",
            "Un alias ha da 1 a 16 lettere, cifre o - _ . +, e inizia con una lettera",
        ),
        AliasRefusal::Prefix => tr("This word starts a search mode", "Questa parola avvia una modalità di ricerca"),
        AliasRefusal::NoTarget => tr("Nothing to open after the word", "Niente da aprire dopo la parola"),
        AliasRefusal::TooLong => tr("Longer than 512 characters", "Più lungo di 512 caratteri"),
        AliasRefusal::Hidden => {
            tr("Holds a hidden or control character", "Contiene un carattere nascosto o di controllo")
        }
        AliasRefusal::Lines => tr("More than one line", "Più di una riga"),
        AliasRefusal::Address => tr("Not an address the launcher opens", "Non è un indirizzo che il launcher apre"),
        AliasRefusal::Scheme => {
            tr("This kind of address can't be an alias", "Questo tipo di indirizzo non può essere un alias")
        }
        AliasRefusal::Full => tr("64 aliases already: remove one first", "Ci sono già 64 alias: rimuovine uno"),
    }
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
/// The notice after "Use Alt+Shift+Space instead" while another app holds that chord (the RegisterHotKey probe): the
/// chord and the question stay as they were.
pub fn pt_alt_shift_space_held() -> &'static str {
    tr("Another app holds Alt+Shift+Space", "Un'altra app usa Alt+Maiusc+Spazio")
}
/// The notice after two passed chords that Command Palette did not answer: a short title, whole at 560 px.
pub fn pt_took_over() -> &'static str {
    tr("Command Palette did not answer Alt+Space", "Command Palette non ha risposto ad Alt+Spazio")
}
/// Its detail: the chord is the launcher's now.
pub fn pt_took_over_detail() -> &'static str {
    tr("It is running, but the launcher has Alt+Space now", "È attivo, ma ora Alt+Spazio è del launcher")
}

// ---- refused launches (5.5.3)
/// The error row of a launch that did not reach the desktop shell.
pub fn launch_failed() -> &'static str {
    tr("Couldn't start it: Explorer isn't answering", "Impossibile avviarlo: Esplora risorse non risponde")
}

/// The notice of a refused paste: the app that copied the text marked it private, so it was never read (plan L2 D8).
pub fn paste_private() -> &'static str {
    tr(
        "Not pasted: the app that copied it marked it private",
        "Non incollato: l'app che l'ha copiato lo ha segnato come privato",
    )
}

/// The window name (the launcher keeps this static name for screen readers).
pub fn window_name() -> &'static str {
    tr("Launcher", "Launcher")
}

// ---- date and time rows (5.4.10): the names of the days and months, and the label of each row

/// The name of a weekday, 0 = Monday.
pub fn day_name(wd: u8) -> &'static str {
    match wd {
        0 => tr("Monday", "lunedì"),
        1 => tr("Tuesday", "martedì"),
        2 => tr("Wednesday", "mercoledì"),
        3 => tr("Thursday", "giovedì"),
        4 => tr("Friday", "venerdì"),
        5 => tr("Saturday", "sabato"),
        _ => tr("Sunday", "domenica"),
    }
}

/// The name of a month, 1 = January.
pub fn month_name(mo: u8) -> &'static str {
    match mo {
        1 => tr("January", "gennaio"),
        2 => tr("February", "febbraio"),
        3 => tr("March", "marzo"),
        4 => tr("April", "aprile"),
        5 => tr("May", "maggio"),
        6 => tr("June", "giugno"),
        7 => tr("July", "luglio"),
        8 => tr("August", "agosto"),
        9 => tr("September", "settembre"),
        10 => tr("October", "ottobre"),
        11 => tr("November", "novembre"),
        _ => tr("December", "dicembre"),
    }
}

pub fn week_number(n: u8) -> String {
    format!("{} {n}", tr("Week", "Settimana"))
}

pub fn day_of_year(n: u16) -> String {
    if crate::i18n::italian() {
        format!("Giorno {n} dell'anno")
    } else {
        format!("Day {n} of the year")
    }
}

pub fn label_utc() -> &'static str {
    "UTC"
}

pub fn label_unix() -> &'static str {
    "Unix"
}

pub fn label_iso8601() -> &'static str {
    "ISO 8601"
}

/// What the detail line of each date and time row says (the row's own title is the value).
pub fn label_long_date() -> &'static str {
    tr("Date and time", "Data e ora")
}
pub fn label_short_date() -> &'static str {
    tr("Date", "Data")
}
pub fn label_time_seconds() -> &'static str {
    tr("Time with seconds", "Ora con secondi")
}
pub fn label_iso_week() -> &'static str {
    tr("ISO week", "Settimana ISO")
}
pub fn label_day_of_year() -> &'static str {
    tr("Day of the year", "Giorno dell'anno")
}

// ---- files (plan L2-F1): the state rows of "file " mode and its placeholder
pub fn files_search_off() -> &'static str {
    tr("Windows Search is off", "Ricerca di Windows non attiva")
}
pub fn files_open_indexing() -> &'static str {
    tr("Open indexing options", "Apri Opzioni di indicizzazione")
}
pub fn files_not_answering() -> &'static str {
    tr("File search isn't answering", "La ricerca file non risponde")
}
pub fn files_admin_protection() -> &'static str {
    tr(
        "File search is unavailable while Administrator protection is on",
        "Ricerca file non disponibile con la protezione amministratore attiva",
    )
}
pub fn files_unavailable() -> &'static str {
    tr("File search is unavailable", "Ricerca file non disponibile")
}
pub fn files_placeholder() -> &'static str {
    tr("Names starting with the typed words", "Nomi che iniziano con le parole scritte")
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
            ProviderId::Alias,
        ] {
            assert!(chip(p).is_some_and(|c| !c.is_empty()), "{p:?}");
        }
        assert_eq!(save_alias("gh"), "Save alias “gh”");
        assert_eq!(verb_assign_alias(), "Assign alias…");
        assert!(alias_refusal(AliasRefusal::Full).starts_with("64 aliases"));
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
        assert_eq!(pt_took_over(), "Command Palette did not answer Alt+Space");
        assert_eq!(pt_took_over_detail(), "It is running, but the launcher has Alt+Space now");
        assert_eq!(hint_pt_take(), "Take Alt+Space · Enter");
        assert_eq!(pt_alt_shift_space_held(), "Another app holds Alt+Shift+Space");
    }
}
