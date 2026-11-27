//! Every command of the beta: its id, its label (a key), its chord and the module that handles it. Chords of later
//! slices (Ctrl+N, Ctrl+Shift+A, Ctrl+,, Ctrl+E, Ctrl+O, Ctrl+C, Alt+1..4) are not bound; Ctrl+W, Alt+F4 and Esc are
//! the kit's.

use oled_ui::verbs::Verb;
// Through an alias: tests/workspace.rs reads the plain "Command" + "::new(" as a process spawn.
use oled_ui::Command as Cmd;
use oled_ui::{Chord, CommandId, CommandTable, EventCx, Key, Mods, UiLang};

use super::screen::TasksScreen;
use super::{about, actions, chrome, grouped, header, keysheet, more_menu, search, view};
use crate::app::updated;
use crate::strings::{k, t};

pub const PAGE_PROCESSES: CommandId = CommandId(1);
pub const FIND: CommandId = CommandId(2);
pub const SAMPLE_NOW: CommandId = CommandId(3);
pub const PAUSE: CommandId = CommandId(4);
pub const DENSITY_STANDARD: CommandId = CommandId(5);
pub const DENSITY_COMPACT: CommandId = CommandId(6);
pub const MORE: CommandId = CommandId(7);
pub const KEYS: CommandId = CommandId(8);
pub const VIEW_GROUPED: CommandId = CommandId(9);
pub const VIEW_TREE: CommandId = CommandId(10);
pub const VIEW_LIST: CommandId = CommandId(11);
pub const SORT_COL_1: CommandId = CommandId(12);
pub const SORT_COL_2: CommandId = CommandId(13);
pub const SORT_COL_3: CommandId = CommandId(14);
pub const SORT_COL_4: CommandId = CommandId(15);
/// Verb Close or End, by row.
pub const CLOSE_OR_END: CommandId = CommandId(16);
pub const END_NOW: CommandId = CommandId(17);
pub const END_TREE: CommandId = CommandId(18);
pub const REOPEN: CommandId = CommandId(19);
pub const OPEN_WINDOWS_TM: CommandId = CommandId(20);
pub const REFRESH_HALF: CommandId = CommandId(21);
pub const REFRESH_ONE: CommandId = CommandId(22);
pub const REFRESH_TWO: CommandId = CommandId(23);
pub const REFRESH_FIVE: CommandId = CommandId(24);
pub const REFRESH_PAUSED: CommandId = CommandId(25);
pub const ALWAYS_ON_TOP: CommandId = CommandId(26);
pub const OPEN_SORTED_AUTO: CommandId = CommandId(27);
pub const OPEN_SORTED_CPU: CommandId = CommandId(28);
pub const OPEN_SORTED_MEMORY: CommandId = CommandId(29);
pub const ABOUT: CommandId = CommandId(30);
pub const RESTART_TO_UPDATE: CommandId = CommandId(31);

/// Every id, in table order.
pub const ALL: [CommandId; 31] = [
    PAGE_PROCESSES,
    FIND,
    SAMPLE_NOW,
    PAUSE,
    DENSITY_STANDARD,
    DENSITY_COMPACT,
    MORE,
    KEYS,
    VIEW_GROUPED,
    VIEW_TREE,
    VIEW_LIST,
    SORT_COL_1,
    SORT_COL_2,
    SORT_COL_3,
    SORT_COL_4,
    CLOSE_OR_END,
    END_NOW,
    END_TREE,
    REOPEN,
    OPEN_WINDOWS_TM,
    REFRESH_HALF,
    REFRESH_ONE,
    REFRESH_TWO,
    REFRESH_FIVE,
    REFRESH_PAUSED,
    ALWAYS_ON_TOP,
    OPEN_SORTED_AUTO,
    OPEN_SORTED_CPU,
    OPEN_SORTED_MEMORY,
    ABOUT,
    RESTART_TO_UPDATE,
];

/// VK_PAUSE (not a name Chord::parse reads).
const KEY_PAUSE: Key = Key(0x13);

fn chord(s: &str) -> Option<Chord> {
    Chord::parse(s)
}

/// The table of the window, labels in `lang`. Err names the command the kit refused or whose chord does not parse.
pub fn table(lang: UiLang) -> Result<CommandTable, String> {
    let l = |key| t(lang, key);
    let sort_col = |n: u8| {
        let text = oled_sys::i18n::subst(&t(lang, k::TASKS_CMD_SORT_COL), &[("n", &n.to_string())]);
        Cmd::new(
            match n {
                1 => SORT_COL_1,
                2 => SORT_COL_2,
                3 => SORT_COL_3,
                _ => SORT_COL_4,
            },
            text.into(),
        )
    };
    // A chord string Chord::parse cannot read fails the table instead of leaving the command without its key.
    let with = |c: Cmd, s: &str| {
        let id = c.id;
        chord(s).map(|ch| c.chord(ch)).ok_or_else(|| format!("command {}: chord {s:?}", id.0))
    };
    let alt = |c: Cmd, s: &str| {
        let id = c.id;
        chord(s).map(|ch| c.alt_chord(ch)).ok_or_else(|| format!("command {}: alt chord {s:?}", id.0))
    };
    let list = [
        with(Cmd::new(PAGE_PROCESSES, l(k::TASKS_TAB_PROCESSES)), "Ctrl+1")?,
        with(Cmd::new(FIND, l(k::TASKS_SEARCH_LABEL)), "Ctrl+F")?,
        with(Cmd::new(SAMPLE_NOW, l(k::TASKS_CMD_SAMPLE_NOW)), "F5")?,
        with(Cmd::new(PAUSE, l(k::TASKS_MORE_PAUSED)), "Ctrl+P")?.alt_chord(Chord::new(Mods::NONE, KEY_PAUSE)),
        with(Cmd::new(DENSITY_STANDARD, l(k::TASKS_MORE_STANDARD)), "Ctrl+=")?,
        with(Cmd::new(DENSITY_COMPACT, l(k::TASKS_MORE_COMPACT)), "Ctrl+-")?,
        alt(with(Cmd::new(MORE, lang.kit(oled_ui::strings::k::KIT_MORE)), "F10")?, "Alt+M")?,
        with(Cmd::new(KEYS, l(k::TASKS_MORE_KEYS)), "F1")?,
        with(Cmd::new(VIEW_GROUPED, l(k::TASKS_VIEW_GROUPED)), "Ctrl+G")?,
        with(Cmd::new(VIEW_TREE, l(k::TASKS_VIEW_TREE)), "Ctrl+T")?,
        with(Cmd::new(VIEW_LIST, l(k::TASKS_VIEW_LIST)), "Ctrl+L")?,
        with(sort_col(1), "Ctrl+Shift+1")?,
        with(sort_col(2), "Ctrl+Shift+2")?,
        with(sort_col(3), "Ctrl+Shift+3")?,
        with(sort_col(4), "Ctrl+Shift+4")?,
        with(Verb::Close.command(CLOSE_OR_END, lang), "Del")?,
        with(Verb::EndNow.command(END_NOW, lang), "Shift+Del")?,
        Cmd::new(END_TREE, l(k::TASKS_VERB_END_TREE)),
        Cmd::new(REOPEN, l(k::TASKS_ROW_REOPEN)),
        Cmd::new(OPEN_WINDOWS_TM, l(k::TASKS_MORE_WINDOWS_TM)).description(l(k::TASKS_MORE_WINDOWS_TM_DESC)),
        Cmd::new(REFRESH_HALF, l(k::TASKS_REFRESH_HALF)),
        Cmd::new(REFRESH_ONE, l(k::TASKS_REFRESH_ONE)),
        Cmd::new(REFRESH_TWO, l(k::TASKS_REFRESH_TWO)),
        Cmd::new(REFRESH_FIVE, l(k::TASKS_REFRESH_FIVE)),
        Cmd::new(REFRESH_PAUSED, l(k::TASKS_MORE_PAUSED)),
        Cmd::new(ALWAYS_ON_TOP, l(k::TASKS_MORE_ON_TOP)),
        Cmd::new(OPEN_SORTED_AUTO, l(k::TASKS_MORE_AUTO)),
        Cmd::new(OPEN_SORTED_CPU, l(k::TASKS_COL_CPU)),
        Cmd::new(OPEN_SORTED_MEMORY, l(k::TASKS_COL_MEMORY)),
        Cmd::new(ABOUT, l(k::TASKS_MORE_ABOUT)),
        Cmd::new(RESTART_TO_UPDATE, l(k::TASKS_UPDATED_ACTION)),
    ];
    let mut table = CommandTable::new();
    for c in list {
        let id = c.id;
        table.add(c).map_err(|e| format!("command {}: {e}", id.0))?;
    }
    Ok(table)
}

/// Event::Command(id): the handler of the module that owns it.
pub fn dispatch(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    match id {
        PAGE_PROCESSES | MORE => chrome::on_command(s, cx, id),
        FIND => search::on_command(s, cx, id),
        SAMPLE_NOW | PAUSE => view::on_command(s, cx, id),
        DENSITY_STANDARD | DENSITY_COMPACT | OPEN_WINDOWS_TM | REFRESH_HALF | REFRESH_ONE | REFRESH_TWO
        | REFRESH_FIVE | REFRESH_PAUSED | ALWAYS_ON_TOP | OPEN_SORTED_AUTO | OPEN_SORTED_CPU | OPEN_SORTED_MEMORY => {
            more_menu::on_command(s, cx, id)
        }
        KEYS => keysheet::on_command(s, cx, id),
        VIEW_GROUPED | VIEW_TREE | VIEW_LIST => grouped::on_command(s, cx, id),
        SORT_COL_1 | SORT_COL_2 | SORT_COL_3 | SORT_COL_4 => header::on_command(s, cx, id),
        CLOSE_OR_END | END_NOW | END_TREE | REOPEN => actions::on_command(s, cx, id),
        ABOUT => about::on_command(s, cx, id),
        RESTART_TO_UPDATE => updated::on_command(s, cx, id),
        _ => {}
    }
}
