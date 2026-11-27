//! The keyboard (P3-files-app 2.2, SPEC-files 5.4.1 within F1-F4): every command with its chords, label, parity with
//! File Explorer and slice; the command table built from it; and run(), which hands each command to its owner's
//! module. Keys the tables do not name (Ctrl+C/X/V, the filter chords, F7, F8, F11 and the rest of F5-F7) are not
//! bound, so Windows' or the kit's default applies.

use oled_sys::i18n::S;
// Through an alias: the spawn lints read the plain "Command" + "::new(" as a process start.
use oled_ui::command::Command as UiCmd;
use oled_ui::{Chord, CommandId, CommandTable, EventCx, Key, Mods, Reply, UiLang};

use crate::tr::{k, t_any, tf};
use crate::window::FilesScreen;
use crate::{about, home, menu, open, ops_ui, pathline, rename, sidebar, tab, toolmenus, window};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Cmd {
    Back = 1,
    Forward,
    Up,
    Home,
    Refresh,
    EditPath,
    EditPathHistory,
    NextRegion,
    PrevRegion,
    ExpandTree,
    FitColumns,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    Tab5,
    Tab6,
    Tab7,
    Tab8,
    LastTab,
    ReopenTab,
    NewWindow,
    Open,
    OpenNewTab,
    SelectAll,
    SelectNone,
    Invert,
    Properties,
    Rename,
    NewFolder,
    CopyPath,
    Recycle,
    DeletePermanent,
    Undo,
    Redo,
    OurMenu,
    SystemMenu,
    HiddenItems,
    Sidebar,
    OpsFlyout,
    OpenInExplorer,
    Escape,
    About,
}

/// How a row compares with File Explorer's key: the same, ours only, or to verify live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parity {
    Same,
    Ours,
    Verify,
}

/// The slice that brings the command to life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slice {
    F1a,
    F1b,
    F2,
    F3,
    F4,
}

/// A chord as written: a name in the kit's grammar (Chord::parse: letters, digits, F keys, the kit.key.* names and
/// "Ctrl+," style), or a code for keys the grammar does not name (arrows, Apps, numpad +).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordSrc {
    Named(&'static str),
    Code(Chord),
}

impl ChordSrc {
    /// Named -> Chord::parse; Code -> Some.
    pub fn chord(&self) -> Option<Chord> {
        match self {
            ChordSrc::Named(s) => Chord::parse(s),
            ChordSrc::Code(c) => Some(*c),
        }
    }
}

pub struct KeyRow {
    pub cmd: Cmd,
    pub chords: &'static [ChordSrc],
    pub label: S,
    pub parity: Parity,
    pub slice: Slice,
}

/// The second CommandId of a row with a third chord: 0x1000 + cmd.
pub const THIRD_CHORD: u32 = 0x1000;

const fn row(cmd: Cmd, chords: &'static [ChordSrc], label: S, parity: Parity, slice: Slice) -> KeyRow {
    KeyRow { cmd, chords, label, parity, slice }
}

use ChordSrc::{Code, Named};
use Parity::{Ours, Same, Verify};
use Slice::{F1a, F1b, F2, F3, F4};

/// SPEC-files 5.4.1, the rows of F1-F4. CloseTab's Ctrl+W is Same and its Ctrl+F4 to verify: the row says Verify.
pub const TABLE: &[KeyRow] = &[
    row(Cmd::Back, &[Code(Chord::new(Mods::ALT, Key::LEFT)), Named("Backspace")], k::FILES_NAV_BACK, Same, F1a),
    row(Cmd::Forward, &[Code(Chord::new(Mods::ALT, Key::RIGHT))], k::FILES_NAV_FORWARD, Same, F1a),
    row(Cmd::Up, &[Code(Chord::new(Mods::ALT, Key::UP))], k::FILES_NAV_UP, Same, F1a),
    row(Cmd::Home, &[Named("Alt+Home")], k::FILES_APP_HOME, Same, F4),
    row(Cmd::Refresh, &[Named("F5"), Named("Ctrl+R")], k::FILES_NAV_REFRESH, Same, F1a),
    row(Cmd::EditPath, &[Named("Ctrl+L"), Named("Alt+D")], k::FILES_CMD_EDIT_PATH, Same, F4),
    row(Cmd::EditPathHistory, &[Named("F4")], k::FILES_CMD_PATH_HISTORY, Same, F4),
    row(Cmd::NextRegion, &[Named("F6")], k::FILES_CMD_NEXT_REGION, Same, F1a),
    row(Cmd::PrevRegion, &[Named("Shift+F6")], k::FILES_CMD_PREV_REGION, Same, F1a),
    row(Cmd::ExpandTree, &[Named("Ctrl+Shift+E")], k::FILES_CMD_EXPAND_TREE, Same, F4),
    row(Cmd::FitColumns, &[Code(Chord::new(Mods::CTRL, Key::ADD))], k::FILES_CMD_FIT_COLUMNS, Same, F1a),
    row(Cmd::NewTab, &[Named("Ctrl+T")], oled_ui::strings::k::KIT_NEW_TAB, Same, F1b),
    row(Cmd::CloseTab, &[Named("Ctrl+W"), Named("Ctrl+F4")], oled_ui::strings::k::KIT_CLOSE_TAB, Verify, F1b),
    row(Cmd::NextTab, &[Named("Ctrl+Tab"), Named("Ctrl+PgDn")], k::FILES_CMD_NEXT_TAB, Same, F1b),
    row(Cmd::PrevTab, &[Named("Ctrl+Shift+Tab"), Named("Ctrl+PgUp")], k::FILES_CMD_PREV_TAB, Same, F1b),
    row(Cmd::Tab1, &[Named("Ctrl+1")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab2, &[Named("Ctrl+2")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab3, &[Named("Ctrl+3")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab4, &[Named("Ctrl+4")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab5, &[Named("Ctrl+5")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab6, &[Named("Ctrl+6")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab7, &[Named("Ctrl+7")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::Tab8, &[Named("Ctrl+8")], k::FILES_CMD_TAB_N, Verify, F4),
    row(Cmd::LastTab, &[Named("Ctrl+9")], k::FILES_CMD_LAST_TAB, Verify, F4),
    row(Cmd::ReopenTab, &[Named("Ctrl+Shift+T")], k::FILES_TAB_REOPEN, Ours, F4),
    row(Cmd::NewWindow, &[Named("Ctrl+N")], k::FILES_CMD_NEW_WINDOW, Same, F1a),
    row(Cmd::Open, &[], k::FILES_MENU_OPEN, Same, F1a),
    row(Cmd::OpenNewTab, &[Named("Ctrl+Enter")], k::FILES_MENU_OPEN_NEW_TAB, Verify, F4),
    row(Cmd::SelectAll, &[Named("Ctrl+A")], k::FILES_MORE_SELECT_ALL, Same, F1a),
    row(Cmd::SelectNone, &[Named("Ctrl+Shift+A")], k::FILES_MORE_SELECT_NONE, Ours, F1a),
    row(Cmd::Invert, &[Named("Ctrl+I")], k::FILES_MORE_INVERT, Ours, F1a),
    row(Cmd::Properties, &[Named("Alt+Enter")], k::FILES_MENU_PROPERTIES, Same, F2),
    row(Cmd::Rename, &[], k::FILES_MENU_RENAME, Same, F3),
    row(Cmd::NewFolder, &[Named("Ctrl+Shift+N")], k::FILES_MENU_NEW_FOLDER, Same, F3),
    row(Cmd::CopyPath, &[Named("Ctrl+Shift+C")], k::FILES_MENU_COPY_PATH, Same, F2),
    row(Cmd::Recycle, &[Named("Del"), Named("Ctrl+D")], oled_ui::strings::k::KIT_VERB_DELETE, Same, F3),
    row(Cmd::DeletePermanent, &[Named("Shift+Del")], oled_ui::strings::k::KIT_VERB_DELETE_PERMANENTLY, Ours, F3),
    row(Cmd::Undo, &[Named("Ctrl+Z")], k::FILES_MORE_UNDO_PLAIN, Same, F3),
    row(Cmd::Redo, &[Named("Ctrl+Y"), Named("Ctrl+Shift+Z")], k::FILES_MORE_REDO, Same, F3),
    row(Cmd::OurMenu, &[Code(Chord::new(Mods::NONE, Key::APPS))], k::FILES_CMD_CONTEXT_MENU, Same, F2),
    row(Cmd::SystemMenu, &[Named("Shift+F10")], k::FILES_MENU_MORE, Same, F2),
    row(Cmd::HiddenItems, &[Named("Ctrl+H")], k::FILES_VIEW_HIDDEN, Ours, F2),
    row(Cmd::Sidebar, &[Named("Ctrl+B")], k::FILES_CMD_SIDEBAR, Ours, F4),
    row(Cmd::OpsFlyout, &[Named("Ctrl+J")], k::FILES_CMD_OPS, Ours, F3),
    row(Cmd::OpenInExplorer, &[Named("Ctrl+Shift+O")], k::FILES_ACTION_OPEN_EXPLORER, Ours, F2),
    row(Cmd::Escape, &[Named("Esc")], k::FILES_CMD_ESCAPE, Same, F1a),
    row(Cmd::About, &[], k::FILES_MORE_ABOUT, Ours, F4),
];

/// A row's label: its key (the window's table or the kit's), Tab1-Tab8 with their digit, About with the app's name.
fn label(r: &KeyRow, lang: UiLang) -> oled_ui::node::Text {
    let n = (r.cmd as u32).wrapping_sub(Cmd::Tab1 as u32);
    if r.label == k::FILES_CMD_TAB_N && n < 8 {
        let digit = (n + 1).to_string();
        return tf(r.label, lang, &[("n", &digit)]);
    }
    if r.label == k::FILES_MORE_ABOUT {
        let name = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
        return tf(r.label, lang, &[("name", &*name)]);
    }
    t_any(r.label, lang)
}

/// CommandId(cmd as u32) for every row: its first chord as the chord, its second as the alt chord, a third through a
/// second command (THIRD_CHORD + cmd). A chord the table cannot bind is left out and logged.
pub fn command_table(lang: UiLang) -> CommandTable {
    let mut t = CommandTable::new();
    for r in TABLE {
        let chords: Vec<Chord> = r.chords.iter().filter_map(ChordSrc::chord).collect();
        let text = label(r, lang);
        let mut c = UiCmd::new(CommandId(r.cmd as u32), text.clone());
        if let Some(&first) = chords.first() {
            c = c.chord(first);
        }
        if let Some(&second) = chords.get(1) {
            c = c.alt_chord(second);
        }
        if let Err(e) = t.add(c) {
            oled_sys::log!("files: key table: {e}");
        }
        if let Some(&third) = chords.get(2) {
            let extra = UiCmd::new(CommandId(THIRD_CHORD + r.cmd as u32), text).chord(third);
            if let Err(e) = t.add(extra) {
                oled_sys::log!("files: key table: {e}");
            }
        }
    }
    t
}

/// The command of an id of command_table (either of a row's ids).
pub fn cmd_of(id: oled_ui::CommandId) -> Option<Cmd> {
    let raw = if id.0 > THIRD_CHORD { id.0 - THIRD_CHORD } else { id.0 };
    TABLE.iter().map(|r| r.cmd).find(|c| *c as u32 == raw)
}

/// One arm per Cmd: each to its owner module's cmd (P3-files-app 2.2's callee list).
pub fn run(s: &mut FilesScreen, cx: &mut EventCx, cmd: Cmd) -> Reply {
    match cmd {
        Cmd::Back
        | Cmd::Forward
        | Cmd::Up
        | Cmd::Refresh
        | Cmd::NextRegion
        | Cmd::PrevRegion
        | Cmd::FitColumns
        | Cmd::NewWindow
        | Cmd::SelectAll
        | Cmd::SelectNone
        | Cmd::Invert
        | Cmd::Escape => window::cmd(s, cx, cmd),
        Cmd::NewTab
        | Cmd::CloseTab
        | Cmd::NextTab
        | Cmd::PrevTab
        | Cmd::Tab1
        | Cmd::Tab2
        | Cmd::Tab3
        | Cmd::Tab4
        | Cmd::Tab5
        | Cmd::Tab6
        | Cmd::Tab7
        | Cmd::Tab8
        | Cmd::LastTab
        | Cmd::ReopenTab
        | Cmd::OpenNewTab => tab::cmd(s, cx, cmd),
        Cmd::EditPath | Cmd::EditPathHistory => pathline::cmd(s, cx, cmd),
        Cmd::ExpandTree | Cmd::Sidebar => sidebar::cmd(s, cx, cmd),
        Cmd::Home => home::cmd(s, cx, cmd),
        Cmd::Open | Cmd::Properties | Cmd::OpenInExplorer => open::cmd(s, cx, cmd),
        Cmd::OurMenu | Cmd::SystemMenu | Cmd::CopyPath => menu::cmd(s, cx, cmd),
        Cmd::HiddenItems => toolmenus::cmd(s, cx, cmd),
        Cmd::Recycle | Cmd::DeletePermanent | Cmd::Undo | Cmd::Redo | Cmd::OpsFlyout => ops_ui::cmd(s, cx, cmd),
        Cmd::Rename | Cmd::NewFolder => rename::cmd(s, cx, cmd),
        Cmd::About => about::cmd(s, cx, cmd),
    }
}
