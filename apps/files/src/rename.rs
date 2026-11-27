//! Rename in place and new folder (P3-files-app 2.2; FC-RENAME fills it, F3.7, F3.8): the field sits on the cell;
//! every change goes through ops_ui::request.

use oled_trust::frame::files::{Location, OpId};
use oled_ui::model::{CellOut, ColId, ListModel, RowKey, ScrollTo, Selection, TableModel};
use oled_ui::widgets::FieldKind;
use oled_ui::{Event, EventCx, Reply, Widget};

use crate::keys::Cmd;
use crate::listing::collate::utf16_to_wtf8;
use crate::listing::{TabId, WidePath};
use crate::model::{id_of, key_of, push_wtf8_lossy, ColKind};
use crate::ops_ui::{self, MutReq};
use crate::tr::{k, t, tf};
use crate::window::FilesScreen;

/// The most characters a name holds.
const NAME_MAX_CHARS: u32 = 255;

/// What the open field was begun with.
#[derive(Debug)]
struct Field {
    key: RowKey,
    /// The tab, the folder and the entry's name the field was begun on: a listing swapped under the field (a
    /// navigation) must never turn its commit into a rename of whatever now has the same row key.
    tab: TabId,
    dir: WidePath,
    name: Vec<u8>,
    /// The extension the table does not show (extensions hidden): appended to what is typed.
    hidden_ext: String,
}

#[derive(Debug, Default)]
pub struct RenameState {
    field: Option<Field>,
    /// The new-folder operation whose Created row enters rename.
    new_folder: Option<OpId>,
    /// A field was just begun: the focus the previous field gave up (Tab) may still arrive as a FocusOut aimed at the
    /// same widget id; the first event after the begin takes it for nothing.
    fresh: bool,
}

impl RenameState {
    pub fn new() -> RenameState {
        RenameState::default()
    }

    /// True while a rename field is open.
    pub fn active(&self) -> bool {
        self.field.is_some()
    }
}

/// The characters a name never holds (and the control characters).
fn forbidden(c: char) -> bool {
    matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
}

/// CON, PRN, AUX, NUL, COM1-COM9 and LPT1-LPT9, with or without an extension, in any case.
fn is_reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let up = base.to_ascii_uppercase();
    match up.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let b = up.as_bytes();
            b.len() == 4 && (up.starts_with("COM") || up.starts_with("LPT")) && (b'1'..=b'9').contains(&b[3])
        }
    }
}

fn name_col() -> ColId {
    ColId(ColKind::Name as u16)
}

/// The table's F2 or slow second click: the field opens over the row's name.
pub fn begin(s: &mut FilesScreen, cx: &mut EventCx, key: RowKey) -> Reply {
    // After a Tab whose commit was refused the field is open again on its own row: the neighbour waits.
    if s.rename.field.as_ref().is_some_and(|f| f.key != key) {
        return Reply::None;
    }
    let lang = cx.lang();
    let tab = s.tabs.active_mut();
    let model = tab.table.model();
    let Some(row) = model.row_of(key) else { return Reply::None };
    let id = id_of(key);
    let Some(name) = model.listing().name(id) else { return Reply::None };
    let mut full = String::new();
    push_wtf8_lossy(&mut full, name);
    let name = name.to_vec();
    let dir = model.listing().dir().clone();
    let is_dir = model.listing().is_dir(id);
    let mut cell = CellOut::default();
    model.cell(row, name_col(), &mut cell);
    let shown = cell.text.len();
    let dot = if is_dir { None } else { full.rfind('.').filter(|&d| d > 0) };
    let (hidden_ext, stem_end) = match dot {
        // The table shows the stem only: the field holds it whole and the extension is put back at the commit.
        Some(d) if shown < full.len() => (full[d..].to_string(), shown),
        Some(d) => (String::new(), d),
        // No extension (a folder, a name that starts with the only dot): the whole name is the stem.
        None => (String::new(), shown),
    };
    if !tab.table.begin_rename(cx, key, name_col(), FieldKind::Rename { stem_end }) {
        return Reply::None;
    }
    if let Some(f) = tab.table.rename_field_mut() {
        f.set_filter(Some(forbidden));
        f.set_max_chars(NAME_MAX_CHARS);
        f.set_label(t(k::FILES_MENU_RENAME, lang));
    }
    tab.table.set_rename_refusal(Some(t(k::FILES_RENAME_FORBIDDEN, lang)));
    let tab_id = tab.id;
    s.rename.field = Some(Field { key, tab: tab_id, dir, name, hidden_ext });
    s.rename.fresh = true;
    Reply::Redraw
}

/// The row of a path in the active tab's folder.
fn row_key_of(s: &FilesScreen, p: &WidePath) -> Option<RowKey> {
    let listing = s.tabs.active().table.model().listing();
    if p.parent().as_ref() != Some(listing.dir()) {
        return None;
    }
    let mut name = Vec::new();
    utf16_to_wtf8(p.file_name()?, &mut name);
    listing.find(&name).map(key_of)
}

/// Selects, focuses and scrolls to the row, then begins its rename.
fn begin_on(s: &mut FilesScreen, cx: &mut EventCx, key: RowKey) -> Reply {
    let t = s.tabs.active_mut();
    t.table.set_selection(cx, Selection::Single(key));
    t.table.set_focused(cx, key, ScrollTo::Center);
    begin(s, cx, key)
}

/// The menu's Rename and an intercepted Rename.
pub fn begin_path(s: &mut FilesScreen, cx: &mut EventCx, p: &WidePath) -> Reply {
    match row_key_of(s, p) {
        Some(key) => begin_on(s, cx, key),
        None => Reply::None,
    }
}

/// The field gave its text: refused names reopen the field with the reason; a good one goes through the gate.
pub fn commit(s: &mut FilesScreen, cx: &mut EventCx, key: RowKey, text: String) -> Reply {
    let f = match s.rename.field.take() {
        Some(f) if f.key == key => f,
        other => {
            // No field of this row: nothing to commit.
            s.rename.field = other;
            return Reply::Redraw;
        }
    };
    let lang = cx.lang();
    let typed = text.trim_end_matches([' ', '.']);
    let tab = s.tabs.active();
    let model = tab.table.model();
    let id = id_of(key);
    // Only the folder, the tab and the entry the field was begun on may be renamed; anything else is a cancel.
    if tab.id != f.tab || model.listing().dir() != &f.dir || model.listing().name(id) != Some(&f.name[..]) {
        return Reply::Redraw;
    }
    let hidden_ext = f.hidden_ext;
    let mut old = String::new();
    push_wtf8_lossy(&mut old, &f.name);
    let new = format!("{typed}{hidden_ext}");
    if typed.is_empty() || new == old {
        return Reply::Redraw;
    }
    let refusal = if is_reserved(&new) {
        Some(tf(k::FILES_RENAME_RESERVED, lang, &[("name", &new)]))
    } else if model.listing().find(new.as_bytes()).is_some_and(|other| other != id) {
        Some(tf(k::FILES_RENAME_CLASH, lang, &[("name", &new)]))
    } else {
        None
    };
    if let Some(msg) = refusal {
        // The kit closed the field with its answer: it opens again over the same row, holding what was typed.
        begin(s, cx, key);
        if let Some(f) = s.tabs.active_mut().table.rename_field_mut() {
            f.set_text(cx, typed, false);
            f.set_error(cx, Some(msg));
        }
        return Reply::Relayout;
    }
    let Some(item) = model.path_of(key) else { return Reply::Redraw };
    if ops_ui::request(s, cx, MutReq::Rename { item, to: new }).is_none() {
        // Refused (the gate, the host): the field opens again over the row, so Tab does not go on to the neighbour.
        begin(s, cx, key);
        if let Some(f) = s.tabs.active_mut().table.rename_field_mut() {
            f.set_text(cx, typed, false);
        }
        return Reply::Relayout;
    }
    Reply::Redraw
}

/// Esc: the field is gone, the name stays.
pub fn cancel(s: &mut FilesScreen, cx: &mut EventCx, key: RowKey) -> Reply {
    let _ = (cx, key);
    s.rename.field = None;
    Reply::Redraw
}

/// While a field is open: the kit's field cycles F2 itself (stem, whole name, extension), so nothing is taken here;
/// a field the table closed on its own ends the state.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if std::mem::take(&mut s.rename.fresh) && matches!(ev, Event::FocusOut) {
        return Some(Reply::None);
    }
    // The field belongs to the tab it was begun in, not to the one shown now.
    let tab = s.rename.field.as_ref().map(|f| f.tab)?;
    if !s.tabs.by_id(tab).is_some_and(|t| t.table.is_renaming()) {
        s.rename.field = None;
        return None;
    }
    // The window leaves the kit's timers to nobody: the field's caret and the refusal line's 3 s are the table's.
    if matches!(ev, Event::Timer(id) if id.is_kit()) {
        if let Some(t) = s.tabs.by_id_mut(tab) {
            let _ = t.table.event(cx, ev);
        }
        return Some(Reply::Redraw);
    }
    None
}

/// Ctrl+Shift+N: a folder with a free name in the shown folder; its row enters rename once the host reports it.
pub fn new_folder(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let Location::Fs(dir) = s.tabs.active().nav.location().clone() else { return Reply::None };
    let lang = cx.lang();
    let base = t(k::FILES_RENAME_NEW_FOLDER_NAME, lang).to_string();
    let listing = s.tabs.active().table.model().listing();
    let name = (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base} ({n})") })
        .find(|c| listing.find(c.as_bytes()).is_none())
        .unwrap_or(base);
    s.rename.new_folder = ops_ui::request(s, cx, MutReq::NewFolder { parent: dir, name });
    Reply::Redraw
}

/// A Created change of this window's new-folder operation: select, scroll, begin its rename. Anyone else's: false.
pub fn on_created(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, path: &WidePath) -> bool {
    if s.rename.new_folder != Some(op) {
        return false;
    }
    s.rename.new_folder = None;
    match row_key_of(s, path) {
        Some(key) => {
            begin_on(s, cx, key);
            true
        }
        None => false,
    }
}

/// Rename (the focused row, or the one selected row), NewFolder.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::NewFolder => new_folder(s, cx),
        Cmd::Rename => {
            let table = &s.tabs.active().table;
            match table.focused().or_else(|| table.selection().single()) {
                Some(key) => begin(s, cx, key),
                None => Reply::None,
            }
        }
        _ => Reply::None,
    }
}
