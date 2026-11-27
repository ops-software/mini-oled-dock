//! The key map of the beta (T5.4): every binding the kit does not own. The kit's table moves and selects; the command
//! table binds the chords of the window; this file takes the few keys that neither reaches: the page chords of the one
//! page, the density chords the focused table would swallow, and Ctrl+wheel.

use oled_ui::model::TableOut;
use oled_ui::widgets::VirtualTable;
use oled_ui::{Chord, CommandId, Event, EventCx, Key, Mods, NodeId, RectP, WheelDelta};

use super::chrome::SEARCH;
use super::commands::{DENSITY_COMPACT, DENSITY_STANDARD};
use super::rows::TasksTable;
use super::screen::{CHROME, HEADER, TABLE};
use crate::app::options::Density;

/// What the key map remembers: the density the table was last given.
#[derive(Default)]
pub struct KeyState {
    pub(crate) density: Option<Density>,
}

/// The Processes page is the only one: Ctrl+Tab and Ctrl+Shift+Tab have nothing to switch to and are taken so that no
/// other control sees them.
fn is_page_chord(key: Key, mods: Mods) -> bool {
    key == Key::TAB && mods.ctrl() && !mods.alt() && !mods.win()
}

/// The regions F6 walks: the toolbar, the header, the list.
fn region_of(f: Option<NodeId>) -> usize {
    match f.map(|n| n.owner()) {
        Some(CHROME | SEARCH) => 0,
        Some(HEADER) => 1,
        _ => 2,
    }
}

/// The node that takes the focus when a region is entered: the page tab, the first column, the table (which gives the
/// focus to its current row).
fn region_node(region: usize) -> NodeId {
    match region {
        0 => NodeId::new(CHROME, 1),
        1 => NodeId::new(HEADER, 1),
        _ => NodeId::new(TABLE, 0),
    }
}

/// A key other than Esc; true when handled. T5.4: F6 and Shift+F6 (the toasts have taken theirs before this),
/// Ctrl+Tab and Ctrl+Shift+Tab.
pub fn on_key(cx: &mut EventCx, st: &mut KeyState, ev: &Event, table: &mut VirtualTable<TasksTable>) -> bool {
    let _ = (st, table);
    let Event::Key { key, down: true, mods, .. } = ev else { return false };
    if is_page_chord(*key, *mods) {
        return true;
    }
    if *key == Key::F6 && !mods.ctrl() && !mods.alt() && !mods.win() {
        let here = region_of(cx.focused());
        let to = if mods.shift() { (here + 2) % 3 } else { (here + 1) % 3 };
        cx.set_focus(region_node(to));
        return true;
    }
    false
}

/// Plain Space while the table has the focus: what Enter does (the table keeps only Ctrl+Space).
pub fn space_out(cx: &EventCx, ev: &Event, table: &VirtualTable<TasksTable>) -> Option<TableOut> {
    let Event::Key { key, down: true, mods, .. } = ev else { return None };
    let plain = *key == Key::SPACE && !mods.ctrl() && !mods.shift() && !mods.alt() && !mods.win();
    let in_table = cx.focused().is_some_and(|f| f.owner() == TABLE);
    if plain && in_table {
        table.focused().map(TableOut::Activated)
    } else {
        None
    }
}

/// A command the app takes before the widgets see the event: Ctrl+= while the table has the focus (the table would
/// use it to fit the columns) and Ctrl+wheel over the list (up: Standard rows, down: Compact rows).
pub fn own_command(ev: &Event, table_focused: bool, list: RectP) -> Option<CommandId> {
    match ev {
        Event::Key { key, down: true, mods, .. } if table_focused => {
            let plus = *key == Key::OEM_PLUS && mods.ctrl() && !mods.alt() && !mods.win();
            plus.then_some(DENSITY_STANDARD)
        }
        Event::Wheel { pos, delta, horizontal: false, mods } if mods.ctrl() && list.contains(*pos) => {
            let (WheelDelta::Lines(n) | WheelDelta::Pixels(n)) = *delta;
            if n > 0.0 {
                Some(DENSITY_STANDARD)
            } else if n < 0.0 {
                Some(DENSITY_COMPACT)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The chord of a text such as "Ctrl+1" as the sheet and the menus show it (the text itself when it is no chord).
pub fn chord_label(chord: &str, lang: oled_ui::UiLang) -> String {
    Chord::parse(chord).map_or_else(|| chord.to_string(), |c| c.label(lang))
}
