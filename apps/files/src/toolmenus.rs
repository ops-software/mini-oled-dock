//! The toolbar's menus (P3-files-app 2.2, F2.6): New, Sort, View and More, and the three view switches they share
//! with the background menu (Hidden items, File name extensions, Compact spacing).

use oled_ui::model::{ColId, SortDir};
use oled_ui::node::Text;
use oled_ui::{Anchor, CommandId, EventCx, Glyph, MenuItem, Reply};

use crate::keys::Cmd;
use crate::listing::{Gen, Visibility, WidePath};
use crate::menu::{self, pick};
use crate::model::{ColKind, Listing};
use crate::tr::{k, t};
use crate::view::columns::sort_key;
use crate::window::{FilesScreen, Shared};
use crate::{status, view};

/// Folder is enabled when FC-RENAME lands (F3); until then the menu shows it disabled with the note.
pub const NEW_FOLDER_LIVE: bool = false;

const KINDS: [ColKind; 7] = [
    ColKind::Name,
    ColKind::Modified,
    ColKind::Type,
    ColKind::Size,
    ColKind::Created,
    ColKind::Attributes,
    ColKind::Status,
];

/// New: Folder (the system's ShellNew types are not in the first version).
pub fn new_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    vec![menu::row(
        s,
        Cmd::NewFolder as u32,
        t(k::FILES_MENU_NEW_FOLDER, lang),
        Some(Glyph::Folder),
        Some(Cmd::NewFolder),
        None,
        NEW_FOLDER_LIVE,
        true,
    )]
}

/// Sort by: the sortable columns with the current one checked, then ascending and descending.
pub fn sort_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let tab = s.tabs.active();
    let (now, dir) = tab.sort;
    let mut items = Vec::new();
    for c in tab.table.columns() {
        let Some(kind) = KINDS.into_iter().find(|kd| *kd as u16 == c.id.0) else { continue };
        if sort_key(kind).is_none() {
            continue;
        }
        items.push(menu::row(
            s,
            pick::SORT_COL + kind as u32,
            c.title.clone(),
            None,
            None,
            Some(kind == now),
            true,
            false,
        ));
    }
    items.push(MenuItem::Separator);
    items.push(menu::row(
        s,
        pick::SORT_ASC,
        t(k::FILES_SORT_ASC, lang),
        None,
        None,
        Some(dir == SortDir::Asc),
        true,
        false,
    ));
    items.push(menu::row(
        s,
        pick::SORT_DESC,
        t(k::FILES_SORT_DESC, lang),
        None,
        None,
        Some(dir == SortDir::Desc),
        true,
        false,
    ));
    items
}

/// The toolbar's View: Details (checked; list and icon views are F7) and the three switches.
pub fn toolbar_view_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let mut items =
        vec![menu::row(s, pick::VIEW_DETAILS, t(k::FILES_VIEW_DETAILS, lang), None, None, Some(true), true, false)];
    items.push(MenuItem::Separator);
    items.extend(view_items(s));
    items
}

/// The three switches, as the background menu's View submenu shows them.
pub fn view_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let o = s.shared.opts.borrow();
    vec![
        menu::row(
            s,
            Cmd::HiddenItems as u32,
            t(k::FILES_VIEW_HIDDEN, lang),
            None,
            Some(Cmd::HiddenItems),
            Some(o.hidden_items),
            true,
            false,
        ),
        menu::row(
            s,
            pick::VIEW_EXTENSIONS,
            t(k::FILES_VIEW_EXTENSIONS, lang),
            None,
            None,
            Some(o.show_extensions),
            true,
            false,
        ),
        menu::row(s, pick::VIEW_COMPACT, t(k::FILES_VIEW_COMPACT, lang), None, None, Some(o.compact), true, false),
    ]
}

/// More: Undo and Redo (F3 fills the labels), the selection commands, Open in File Explorer and About.
pub fn more_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let (undo, redo) = {
        let u = s.shared.undo.borrow();
        (u.label(lang), u.redo_label(lang))
    };
    let undo_on = undo.is_some();
    let redo_on = redo.is_some();
    let undo_label: Text = undo.unwrap_or_else(|| t(k::FILES_MORE_UNDO_PLAIN, lang));
    let redo_label: Text = redo.unwrap_or_else(|| t(k::FILES_MORE_REDO, lang));
    vec![
        menu::row(s, Cmd::Undo as u32, undo_label, None, Some(Cmd::Undo), None, undo_on, false),
        menu::row(s, Cmd::Redo as u32, redo_label, None, Some(Cmd::Redo), None, redo_on, false),
        MenuItem::Separator,
        MenuItem::Command(CommandId(Cmd::SelectAll as u32)),
        MenuItem::Command(CommandId(Cmd::SelectNone as u32)),
        MenuItem::Command(CommandId(Cmd::Invert as u32)),
        MenuItem::Separator,
        MenuItem::Command(CommandId(Cmd::OpenInExplorer as u32)),
        MenuItem::Command(CommandId(Cmd::About as u32)),
    ]
}

pub fn open_new(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let items = new_items(s);
    cx.open_menu(items, at);
    Reply::Redraw
}

pub fn open_sort(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let items = sort_items(s);
    cx.open_menu(items, at);
    Reply::Redraw
}

pub fn open_view(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let items = toolbar_view_items(s);
    cx.open_menu(items, at);
    Reply::Redraw
}

pub fn open_more(s: &mut FilesScreen, cx: &mut EventCx, at: Anchor) -> Reply {
    let items = more_items(s);
    cx.open_menu(items, at);
    Reply::Redraw
}

/// The picks of Sort and View.
pub fn on_pick(s: &mut FilesScreen, cx: &mut EventCx, id: CommandId) -> Reply {
    let now = s.tabs.active().sort;
    match id.0 {
        pick::SORT_ASC => view::details::sort(s, cx, ColId(now.0 as u16), SortDir::Asc),
        pick::SORT_DESC => view::details::sort(s, cx, ColId(now.0 as u16), SortDir::Desc),
        n => match menu::col_of(n) {
            Some(col) => view::details::sort(s, cx, col, now.1),
            None => match n {
                pick::VIEW_EXTENSIONS => toggle(s, cx, |o| o.show_extensions = !o.show_extensions),
                pick::VIEW_COMPACT => toggle(s, cx, |o| o.compact = !o.compact),
                _ => Reply::None,
            },
        },
    }
}

/// HiddenItems (Ctrl+H).
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::HiddenItems => toggle(s, cx, |o| o.hidden_items = !o.hidden_items),
        _ => Reply::None,
    }
}

/// Flips one option through the store, then shows it in every tab of this window.
fn toggle(s: &mut FilesScreen, cx: &mut EventCx, f: fn(&mut crate::options::FilesOptions)) -> Reply {
    s.shared.services.options.modify(&mut |o| f(o));
    let now = s.shared.services.options.get();
    *s.shared.opts.borrow_mut() = now;
    apply_view(s, cx);
    // The other windows show the new switches too.
    crate::window::bump_all(&s.shared);
    Reply::Relayout
}

/// The hidden-items filter the options ask for: every listing made while they hold starts with it.
pub fn visibility(shared: &Shared) -> Visibility {
    let o = shared.opts.borrow();
    Visibility { hidden: o.hidden_items, protected_system: o.protected_system }
}

/// A new listing of `dir` with the options' visibility already on it (the enumeration service makes none of its own).
pub fn listing(shared: &Shared, dir: WidePath, gen: Gen) -> Box<dyn Listing> {
    let mut l = shared.services.enumr.new_listing(dir, gen);
    l.set_visibility(visibility(shared));
    l
}

/// The options' view switches onto every tab of this window: the cells, the hidden-items filter and the row height.
pub fn apply_view(s: &mut FilesScreen, cx: &mut EventCx) {
    let o = s.shared.opts.borrow().clone();
    let vis = visibility(&s.shared);
    let dense = cx.tokens().size.row_dense;
    for tab in s.tabs.iter_mut() {
        let m = tab.table.model_mut();
        m.set_opts(o.view_opts());
        m.listing_mut().set_visibility(vis);
        tab.table.set_row_height(cx, o.compact.then_some(dense));
        tab.table.model_changed(cx, true);
    }
    status::selection_changed(s, cx);
}
