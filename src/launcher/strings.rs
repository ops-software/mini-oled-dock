//! Every user-visible string of the launcher: one function each, English and Italian through `i18n::tr`.
//! Later tasks append theirs.

use super::aliases::AliasRefusal;
use super::model::{Choice, KeyChord, Mods, ProviderId};
use crate::i18n::{k, t};

pub fn placeholder() -> &'static str {
    t(k::LAUNCHER_STRINGS_PLACEHOLDER)
}

/// The mode chip of a provider (5.2.4); None for the providers that have no prefix.
pub fn chip(p: ProviderId) -> Option<&'static str> {
    Some(match p {
        ProviderId::Calc | ProviderId::Units => t(k::LAUNCHER_STRINGS_CALC),
        ProviderId::Run => t(k::LAUNCHER_STRINGS_RUN),
        ProviderId::Windows => t(k::LAUNCHER_STRINGS_WINDOWS),
        ProviderId::Web => t(k::LAUNCHER_STRINGS_WEB),
        ProviderId::Settings => t(k::COMMON_SETTINGS),
        ProviderId::DateTime => t(k::LAUNCHER_STRINGS_TIME),
        ProviderId::Registry => t(k::LAUNCHER_STRINGS_REGISTRY),
        ProviderId::Files => t(k::COMMON_FILES),
        // The alias prompt (plan L2-A2).
        ProviderId::Alias => t(k::LAUNCHER_STRINGS_ALIAS),
        ProviderId::Apps | ProviderId::Commands => return None,
    })
}

// ---- hints of the selected row
pub fn hint_open() -> &'static str {
    t(k::LAUNCHER_STRINGS_OPEN_ENTER)
}
pub fn hint_switch() -> &'static str {
    t(k::LAUNCHER_STRINGS_SWITCH_ENTER)
}
pub fn hint_copy() -> &'static str {
    t(k::LAUNCHER_STRINGS_COPY_ENTER)
}
/// A conversion's row: Enter copies the number without its unit.
pub fn hint_copy_number() -> &'static str {
    t(k::LAUNCHER_STRINGS_COPY_NUMBER_ENTER)
}
/// A file or folder row that has no Open (an executable while the Mark of the Web check is open): Ctrl+Enter shows it.
pub fn hint_reveal() -> &'static str {
    t(k::LAUNCHER_STRINGS_OPEN_FILE_LOCATION_CTRL_ENTER)
}
pub fn hint_run() -> &'static str {
    t(k::LAUNCHER_STRINGS_RUN_ENTER)
}
/// The alias prompt's row: Enter saves.
pub fn hint_save() -> &'static str {
    t(k::LAUNCHER_STRINGS_SAVE_ENTER)
}
pub fn hint_run_in_console() -> &'static str {
    t(k::LAUNCHER_STRINGS_RUN_IN_CONSOLE_ENTER)
}

// ---- verb labels (the action list and the hints)
pub fn verb_open() -> &'static str {
    t(k::COMMON_OPEN)
}
pub fn verb_switch() -> &'static str {
    t(k::LAUNCHER_STRINGS_SWITCH_TO)
}
pub fn verb_run_as_admin() -> &'static str {
    t(k::COMMON_RUN_AS_ADMINISTRATOR)
}
pub fn verb_reveal() -> &'static str {
    t(k::COMMON_OPEN_FILE_LOCATION)
}
pub fn verb_copy() -> &'static str {
    t(k::LAUNCHER_STRINGS_COPY)
}
pub fn verb_close_window() -> &'static str {
    t(k::COMMON_CLOSE_WINDOW)
}
pub fn verb_forget() -> &'static str {
    t(k::COMMON_FORGET)
}
pub fn verb_run_in_console() -> &'static str {
    t(k::LAUNCHER_STRINGS_RUN_IN_CONSOLE)
}

/// "New window": Open on a running app starts a new instance (the action list, Shift+Enter).
pub fn verb_new_window() -> &'static str {
    t(k::COMMON_NEW_WINDOW)
}
/// The notice's own entry of the action list.
pub fn verb_dont_show_again() -> &'static str {
    t(k::LAUNCHER_STRINGS_DON_T_SHOW_AGAIN)
}

// ---- aliases (plan L2-A2)

/// The action list's entry that opens the alias prompt for the selected row.
pub fn verb_assign_alias() -> &'static str {
    t(k::LAUNCHER_STRINGS_ASSIGN_ALIAS)
}
/// The action list's entry of an alias row.
pub fn verb_remove_alias() -> &'static str {
    t(k::LAUNCHER_STRINGS_REMOVE_ALIAS)
}
/// The alias prompt's row when its word and target can be saved.
pub fn save_alias(word: &str) -> String {
    format!("{} “{word}”", t(k::LAUNCHER_STRINGS_SAVE_ALIAS))
}
/// The alias prompt's row while the field is empty (a row's target given, or the "word target" form).
pub fn alias_type_word() -> &'static str {
    t(k::LAUNCHER_STRINGS_TYPE_A_WORD_FOR_THIS)
}
pub fn alias_type_word_target() -> &'static str {
    t(k::LAUNCHER_STRINGS_TYPE_A_WORD_A_SPACE)
}
/// An app alias whose app is not in the catalogue.
pub fn app_not_found() -> &'static str {
    t(k::LAUNCHER_STRINGS_APP_NOT_FOUND)
}
/// The notice row of an alias that could not be written to config.json.
pub fn alias_not_saved() -> &'static str {
    t(k::LAUNCHER_STRINGS_COULDN_T_SAVE_THE_ALIAS)
}
/// Why the alias prompt cannot save.
pub fn alias_refusal(why: AliasRefusal) -> &'static str {
    match why {
        AliasRefusal::Word => t(k::LAUNCHER_STRINGS_AN_ALIAS_IS_1_16),
        AliasRefusal::Prefix => t(k::LAUNCHER_STRINGS_THIS_WORD_STARTS_A_SEARCH),
        AliasRefusal::NoTarget => t(k::LAUNCHER_STRINGS_NOTHING_TO_OPEN_AFTER_THE),
        AliasRefusal::TooLong => t(k::LAUNCHER_STRINGS_LONGER_THAN_512_CHARACTERS),
        AliasRefusal::Hidden => t(k::LAUNCHER_STRINGS_HOLDS_A_HIDDEN_OR_CONTROL),
        AliasRefusal::Lines => t(k::LAUNCHER_STRINGS_MORE_THAN_ONE_LINE),
        AliasRefusal::Address => t(k::LAUNCHER_STRINGS_NOT_AN_ADDRESS_THE_LAUNCHER),
        AliasRefusal::Scheme => t(k::LAUNCHER_STRINGS_THIS_KIND_OF_ADDRESS_CAN),
        AliasRefusal::Full => t(k::LAUNCHER_STRINGS_64_ALIASES_ALREADY_REMOVE_ONE),
    }
}

/// The name of a key chord as the action list shows it (right-aligned, CAPTION): "Ctrl+Shift+Enter", "Shift+Del".
pub fn key_name(c: KeyChord) -> String {
    let mut s = String::new();
    for (m, name) in [(Mods::ALT, "Alt"), (Mods::CTRL, "Ctrl"), (Mods::SHIFT, t(k::LAUNCHER_STRINGS_SHIFT))] {
        if c.mods.has(m) {
            s.push_str(name);
            s.push('+');
        }
    }
    match c.vk {
        0x0D => s.push_str(t(k::LAUNCHER_STRINGS_ENTER)),
        0x2E => s.push_str(t(k::LAUNCHER_STRINGS_DEL)),
        0x20 => s.push_str(t(k::LAUNCHER_STRINGS_SPACE)),
        vk @ (0x30..=0x39 | 0x41..=0x5A) => s.extend(char::from_u32(u32::from(vk))),
        _ => s.push('?'),
    }
    s
}

// ---- detail lines and notices
/// Prefix of the detail line of a run line: the line itself follows.
pub fn run_by_windows() -> &'static str {
    t(k::LAUNCHER_STRINGS_RUN_BY_WINDOWS)
}
pub fn line_too_long() -> &'static str {
    t(k::LAUNCHER_STRINGS_LINE_TOO_LONG_TO_REVIEW)
}
pub fn no_window_to_act_on() -> &'static str {
    t(k::LAUNCHER_STRINGS_NO_WINDOW_TO_ACT_ON)
}
pub fn thousands() -> &'static str {
    t(k::LAUNCHER_STRINGS_THOUSANDS)
}
/// Appended to the detail line of a window that does not answer ("<" mode).
pub fn not_responding() -> &'static str {
    t(k::LAUNCHER_STRINGS_NOT_RESPONDING)
}

// ---- web, registry
/// The web row (5.4.8): always names the host of the engine's template.
pub fn search_web(host: &str, query: &str) -> String {
    // The host passed validate_template (letters, digits, hyphens, dots), so it holds no "{q}" to replace again.
    t(k::LAUNCHER_STRINGS_SEARCH_HOST_FOR_Q).replace("{host}", host).replacen("{q}", query, 1)
}
/// The hint row of "??" alone.
pub fn type_to_search(host: &str) -> String {
    t(k::LAUNCHER_STRINGS_TYPE_TO_SEARCH_HOST).replace("{host}", host)
}
/// The one row of the registry mode (":").
pub fn open_registry_editor() -> &'static str {
    t(k::LAUNCHER_STRINGS_OPEN_REGISTRY_EDITOR)
}

// ---- the one-time offer (5.2.1)
pub fn offer_text() -> &'static str {
    t(k::LAUNCHER_STRINGS_NEW_A_LAUNCHER_FOR_APPS)
}
pub fn offer_use_alt_space() -> &'static str {
    t(k::LAUNCHER_STRINGS_USE_ALT_SPACE)
}
pub fn offer_use_alt_shift_space() -> &'static str {
    t(k::LAUNCHER_STRINGS_USE_ALT_SHIFT_SPACE)
}
pub fn offer_dismiss() -> &'static str {
    t(k::LAUNCHER_STRINGS_DON_T_SHOW_AGAIN)
}
/// The offer row after the probe found Alt+Space taken: Alt+Shift+Space is proposed.
pub fn offer_alt_space_held() -> &'static str {
    t(k::LAUNCHER_STRINGS_ANOTHER_APP_HOLDS_ALT_SPACE)
}
/// The offer row after the probe found both chords taken.
pub fn offer_both_held() -> &'static str {
    t(k::LAUNCHER_STRINGS_ANOTHER_APP_HOLDS_ALT_SPACE_2)
}

// ---- PowerToys (5.7)
/// The notice of the first chord the dock sees while Command Palette runs.
pub fn pt_shared() -> &'static str {
    t(k::LAUNCHER_STRINGS_POWERTOYS_COMMAND_PALETTE_IS_RUNNING)
}
/// Its detail: what taking the chord leaves to do.
pub fn pt_turn_off() -> &'static str {
    t(k::LAUNCHER_STRINGS_TURN_ITS_SHORTCUT_OFF_IN)
}
/// The hint of that notice: Enter takes the chord.
pub fn hint_pt_take() -> &'static str {
    t(k::LAUNCHER_STRINGS_TAKE_ALT_SPACE_ENTER)
}
/// The notice's choices in its action list.
pub fn choice_label(c: Choice) -> &'static str {
    match c {
        Choice::TakeChord => t(k::LAUNCHER_STRINGS_TAKE_ALT_SPACE),
        Choice::LeaveChord => t(k::LAUNCHER_STRINGS_LEAVE_IT_TO_COMMAND_PALETTE),
        Choice::UseAltShiftSpace => t(k::LAUNCHER_STRINGS_USE_ALT_SHIFT_SPACE_INSTEAD),
    }
}
/// The notice after "Use Alt+Shift+Space instead" while another app holds that chord (the RegisterHotKey probe): the
/// chord and the question stay as they were.
pub fn pt_alt_shift_space_held() -> &'static str {
    t(k::COMMON_ANOTHER_APP_HOLDS_ALT_SHIFT)
}
/// The notice after two passed chords that Command Palette did not answer: a short title, whole at 560 px.
pub fn pt_took_over() -> &'static str {
    t(k::LAUNCHER_STRINGS_COMMAND_PALETTE_DID_NOT_ANSWER)
}
/// Its detail: the chord is the launcher's now.
pub fn pt_took_over_detail() -> &'static str {
    t(k::LAUNCHER_STRINGS_IT_IS_RUNNING_BUT_THE)
}

// ---- refused launches (5.5.3)
/// The error row of a launch that did not reach the desktop shell.
pub fn launch_failed() -> &'static str {
    t(k::LAUNCHER_STRINGS_COULDN_T_START_IT_EXPLORER)
}

/// The notice of a refused paste: the app that copied the text marked it private, so it was never read (plan L2 D8).
pub fn paste_private() -> &'static str {
    t(k::LAUNCHER_STRINGS_NOT_PASTED_THE_APP_THAT)
}

/// The window name (the launcher keeps this static name for screen readers).
pub fn window_name() -> &'static str {
    t(k::COMMON_LAUNCHER)
}

// ---- date and time rows (5.4.10): the names of the days and months, and the label of each row

/// The name of a weekday, 0 = Monday.
pub fn day_name(wd: u8) -> &'static str {
    match wd {
        0 => t(k::LAUNCHER_STRINGS_MONDAY),
        1 => t(k::LAUNCHER_STRINGS_TUESDAY),
        2 => t(k::LAUNCHER_STRINGS_WEDNESDAY),
        3 => t(k::LAUNCHER_STRINGS_THURSDAY),
        4 => t(k::LAUNCHER_STRINGS_FRIDAY),
        5 => t(k::LAUNCHER_STRINGS_SATURDAY),
        _ => t(k::LAUNCHER_STRINGS_SUNDAY),
    }
}

/// The name of a month, 1 = January.
pub fn month_name(mo: u8) -> &'static str {
    match mo {
        1 => t(k::LAUNCHER_STRINGS_JANUARY),
        2 => t(k::LAUNCHER_STRINGS_FEBRUARY),
        3 => t(k::LAUNCHER_STRINGS_MARCH),
        4 => t(k::LAUNCHER_STRINGS_APRIL),
        5 => t(k::LAUNCHER_STRINGS_MAY),
        6 => t(k::LAUNCHER_STRINGS_JUNE),
        7 => t(k::LAUNCHER_STRINGS_JULY),
        8 => t(k::LAUNCHER_STRINGS_AUGUST),
        9 => t(k::LAUNCHER_STRINGS_SEPTEMBER),
        10 => t(k::LAUNCHER_STRINGS_OCTOBER),
        11 => t(k::LAUNCHER_STRINGS_NOVEMBER),
        _ => t(k::LAUNCHER_STRINGS_DECEMBER),
    }
}

pub fn week_number(n: u8) -> String {
    format!("{} {n}", t(k::LAUNCHER_STRINGS_WEEK))
}

pub fn day_of_year(n: u16) -> String {
    oled_sys::i18n::subst(t(k::LAUNCHER_STRINGS_DAY_OF_YEAR), &[("n", &n.to_string())])
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
    t(k::LAUNCHER_STRINGS_DATE_AND_TIME)
}
pub fn label_short_date() -> &'static str {
    t(k::LAUNCHER_STRINGS_DATE)
}
pub fn label_time_seconds() -> &'static str {
    t(k::LAUNCHER_STRINGS_TIME_WITH_SECONDS)
}
pub fn label_iso_week() -> &'static str {
    t(k::LAUNCHER_STRINGS_ISO_WEEK)
}
pub fn label_day_of_year() -> &'static str {
    t(k::LAUNCHER_STRINGS_DAY_OF_THE_YEAR)
}

// ---- files (plan L2-F1): the state rows of "file " mode and its placeholder
pub fn files_search_off() -> &'static str {
    t(k::LAUNCHER_STRINGS_WINDOWS_SEARCH_IS_OFF)
}
pub fn files_open_indexing() -> &'static str {
    t(k::LAUNCHER_STRINGS_OPEN_INDEXING_OPTIONS)
}
pub fn files_not_answering() -> &'static str {
    t(k::LAUNCHER_STRINGS_FILE_SEARCH_ISN_T_ANSWERING)
}
pub fn files_admin_protection() -> &'static str {
    t(k::LAUNCHER_STRINGS_FILE_SEARCH_IS_UNAVAILABLE_WHILE)
}
pub fn files_unavailable() -> &'static str {
    t(k::LAUNCHER_STRINGS_FILE_SEARCH_IS_UNAVAILABLE)
}
pub fn files_placeholder() -> &'static str {
    t(k::LAUNCHER_STRINGS_NAMES_STARTING_WITH_THE_TYPED)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn day_of_year_equals_the_old_text_in_both_languages() {
        use crate::i18n::with_italian;
        for n in [1u16, 60, 365, 366] {
            assert_eq!(with_italian(false, || day_of_year(n)), format!("Day {n} of the year"));
            assert_eq!(with_italian(true, || day_of_year(n)), format!("Giorno {n} dell'anno"));
        }
    }

    #[test]
    fn placeholder_reads_the_table_in_both_languages() {
        use crate::i18n::with_italian;
        with_italian(false, || assert_eq!(placeholder(), "Search apps, files, settings, commands"));
        with_italian(true, || assert_eq!(placeholder(), "Cerca app, file, impostazioni, comandi"));
    }

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
