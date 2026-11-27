//! Our context menu and the system menu (P3-files-app 2.2, F2.5; SPEC-files 5.4.4): our first level is built in the UI
//! from the selection alone and never waits on the shell host; the system menu is requested only from Shift+F10,
//! Shift+right-click or "Show more options", and its reply says how it ended.

use std::sync::{Mutex, PoisonError};
use std::thread::ThreadId;

use oled_trust::frame::files::{CanonVerb, ItemRef, Location, MenuOutcome, ShellEvt, ShellReq, Verb};
use oled_ui::model::{ColId, ListModel, RowKey, Selection};
use oled_ui::node::Text;
use oled_ui::{Anchor, CommandId, EventCx, Glyph, MenuItem, PointP, RectP, Reply};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::model::id_of;
use crate::services::ReqOwner;
use crate::shell_client::ShellReply;
use crate::status::{self, Notice, NoticeAct};
use crate::tr::{k, t};
use crate::window::{self, FilesScreen, LoopReq};
use crate::{ops_ui, rename, toolmenus};

/// Open in new tab (F4.2).
pub const TABS_LIVE: bool = true;
/// Pin to Quick access is enabled when the pins group lands (F4.8).
pub const PINS_LIVE: bool = false;

/// The ids of our own picks (the key table's ids are 1..=0x1030): Event::Command reaches on_pick with them.
pub mod pick {
    pub const OPEN_WITH: u32 = 0x2001;
    pub const OPEN_NEW_WINDOW: u32 = 0x2002;
    pub const PIN: u32 = 0x2003;
    /// The background menu's "Show more options": the system menu of the folder itself.
    pub const MORE_BACKGROUND: u32 = 0x2004;
    /// The toolbar View menu's Details row (the only view of the first version: checked, nothing to switch).
    pub const VIEW_DETAILS: u32 = 0x2132;
    /// SORT_COL + the column's ColKind.
    pub const SORT_COL: u32 = 0x2100;
    pub const SORT_ASC: u32 = 0x2120;
    pub const SORT_DESC: u32 = 0x2121;
    pub const VIEW_EXTENSIONS: u32 = 0x2130;
    pub const VIEW_COMPACT: u32 = 0x2131;
}

/// Windows are opened at most this many at a time by one "Open in new window" pick.
const NEW_WINDOWS_MAX: usize = 8;

/// Tests: the Shift state each test thread forces (a thread not in the table reads the keyboard).
static SHIFT_FORCED: Mutex<Vec<(ThreadId, bool)>> = Mutex::new(Vec::new());

/// Tests: forces the Shift state the menu reads on this thread (None: the keyboard).
#[doc(hidden)]
pub fn set_shift_for_tests(v: Option<bool>) {
    let me = std::thread::current().id();
    let mut t = SHIFT_FORCED.lock().unwrap_or_else(PoisonError::into_inner);
    t.retain(|(id, _)| *id != me);
    if let Some(down) = v {
        t.push((me, down));
    }
}

fn shift_down() -> bool {
    let me = std::thread::current().id();
    let forced =
        SHIFT_FORCED.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(id, _)| *id == me).map(|f| f.1);
    match forced {
        Some(down) => down,
        // SAFETY: GetKeyState only reads this thread's key state.
        None => unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 },
    }
}

/// The chord of a command as the menu prints it.
pub(crate) fn chord_of(s: &FilesScreen, c: Cmd) -> Option<Text> {
    let lang = s.shared.lang.get();
    s.commands.get(CommandId(c as u32)).and_then(|cmd| cmd.chord).map(|ch| Text::from(ch.label(lang)))
}

/// A Custom row with the chord of command `chord`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn row(
    s: &FilesScreen,
    id: u32,
    label: Text,
    glyph: Option<Glyph>,
    chord: Option<Cmd>,
    checked: Option<bool>,
    enabled: bool,
    note: bool,
) -> MenuItem {
    let lang = s.shared.lang.get();
    MenuItem::Custom {
        id: CommandId(id),
        label,
        glyph,
        chord: chord.and_then(|c| chord_of(s, c)),
        enabled,
        checked,
        note: (!enabled && note).then(|| t(k::FILES_MENU_NOT_IN_BETA, lang)),
        danger: false,
    }
}

fn cmd_item(c: Cmd) -> MenuItem {
    MenuItem::Command(CommandId(c as u32))
}

/// What the selection holds, read from the model without building paths.
struct Held {
    count: usize,
    folders: usize,
}

fn held(s: &FilesScreen) -> Held {
    let t = s.tabs.active();
    let m = t.table.model();
    let sel = t.table.selection();
    let mut h = Held { count: 0, folders: 0 };
    sel.for_each(m, &mut |_, key| {
        h.count += 1;
        if m.listing().is_dir(id_of(key)) {
            h.folders += 1;
        }
    });
    h
}

/// The selected paths, in model order.
pub(crate) fn selected(s: &FilesScreen) -> Vec<WidePath> {
    let t = s.tabs.active();
    t.table.model().selected_paths(t.table.selection())
}

/// The folder the active tab lists, when it lists one.
fn folder(s: &FilesScreen) -> Option<WidePath> {
    match s.tabs.active().nav.location() {
        Location::Fs(p) => Some(p.clone()),
        _ => None,
    }
}

/// Our first level for the selection (files and folders) or, with nothing selected, for the folder itself. Built from
/// the selection alone: no shell host, no file access.
pub fn build_context(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let h = held(s);
    if h.count == 0 {
        return background_items(s);
    }
    let folders = h.folders == h.count;
    let mut items = vec![cmd_item(Cmd::Open)];
    if folders {
        items.push(row(
            s,
            Cmd::OpenNewTab as u32,
            t(k::FILES_MENU_OPEN_NEW_TAB, lang),
            None,
            Some(Cmd::OpenNewTab),
            None,
            TABS_LIVE,
            true,
        ));
        items.push(row(
            s,
            pick::OPEN_NEW_WINDOW,
            t(k::FILES_MENU_OPEN_NEW_WINDOW, lang),
            None,
            None,
            None,
            true,
            false,
        ));
    } else if h.count == 1 {
        items.push(row(s, pick::OPEN_WITH, t(k::FILES_MENU_OPEN_WITH, lang), None, None, None, true, false));
    }
    items.push(row(s, pick::PIN, t(k::FILES_MENU_PIN, lang), Some(Glyph::Pin), None, None, PINS_LIVE, true));
    items.push(cmd_item(Cmd::CopyPath));
    items.push(cmd_item(Cmd::Properties));
    let paths = selected(s);
    let f3 = ops_ui::f3_items(s, &paths);
    if !f3.is_empty() {
        items.push(MenuItem::Separator);
        items.extend(f3);
    }
    items.push(MenuItem::Separator);
    items.push(cmd_item(Cmd::SystemMenu));
    items
}

/// The menu of the empty space of the list.
fn background_items(s: &FilesScreen) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    vec![
        MenuItem::Submenu {
            label: t(k::FILES_MENU_SORT, lang),
            glyph: Some(Glyph::SortUp),
            items: toolmenus::sort_items(s),
        },
        MenuItem::Submenu {
            label: t(k::FILES_MENU_VIEW, lang),
            glyph: Some(Glyph::List),
            items: toolmenus::view_items(s),
        },
        cmd_item(Cmd::Refresh),
        MenuItem::Separator,
        MenuItem::Submenu {
            label: t(k::FILES_MENU_NEW, lang),
            glyph: Some(Glyph::Plus),
            items: toolmenus::new_items(s),
        },
        MenuItem::Separator,
        cmd_item(Cmd::OpenInExplorer),
        cmd_item(Cmd::Properties),
        MenuItem::Separator,
        row(s, pick::MORE_BACKGROUND, t(k::FILES_MENU_MORE, lang), None, Some(Cmd::SystemMenu), None, true, false),
    ]
}

/// Where a menu of the focused row opens: under its left part, or the list's corner with no row.
fn focus_anchor(s: &FilesScreen) -> PointP {
    let t = s.tabs.active();
    let rect = t.table.focused().and_then(|key| t.table.model().row_of(key)).and_then(|row| t.table.row_rect(row));
    match rect {
        Some(r) => PointP { x: r.l + (r.r - r.l) / 4, y: r.b },
        None => {
            let b: RectP = s.rects.body;
            PointP { x: b.l + 24, y: b.t + 24 }
        }
    }
}

/// Right click, the Menu key: Shift held -> the system menu; else our first level at `at`.
pub fn open_context(s: &mut FilesScreen, cx: &mut EventCx, row: Option<RowKey>, at: PointP) -> Reply {
    if folder(s).is_none() {
        return Reply::None;
    }
    if row.is_none() {
        // The empty space: the folder's own menu, whatever the selection was (the system menu of it too).
        s.tabs.active_mut().table.set_selection(cx, Selection::None);
        status::selection_changed(s, cx);
    }
    if shift_down() {
        return open_system(s, cx, at);
    }
    let items = timed_context(s);
    cx.open_menu(items, Anchor::Point(at));
    Reply::Redraw
}

/// build_context, with the time it took in the log (the first level must not wait on anything).
fn timed_context(s: &FilesScreen) -> Vec<MenuItem> {
    let t0 = std::time::Instant::now();
    let items = build_context(s);
    oled_sys::log!("files: menu built {} us ({} items)", t0.elapsed().as_micros(), items.len());
    items
}

/// The window's client point in screen coordinates (the Harness has no window: the point itself).
fn to_screen(s: &FilesScreen, at: PointP) -> (i32, i32, u64) {
    let hwnd = s.shared.win(s.win).and_then(|w| w.hwnd);
    let Some(raw) = hwnd else { return (at.x, at.y, 0) };
    let mut p = POINT { x: at.x, y: at.y };
    // SAFETY: the handle is this window's; ClientToScreen only converts the point.
    let ok = unsafe { ClientToScreen(HWND(raw as *mut _), &mut p) }.as_bool();
    if ok {
        (p.x, p.y, raw as u64)
    } else {
        (at.x, at.y, raw as u64)
    }
}

/// The system menu of the selection (or of the folder): one request to the shell host; the window stays responsive.
pub fn open_system(s: &mut FilesScreen, cx: &mut EventCx, at: PointP) -> Reply {
    let mut paths = selected(s);
    if paths.is_empty() {
        paths.extend(folder(s));
    }
    open_system_for(s, cx, at, paths)
}

/// The system menu of exactly `paths`.
fn open_system_for(s: &mut FilesScreen, cx: &mut EventCx, at: PointP, paths: Vec<WidePath>) -> Reply {
    let win = s.win;
    if paths.is_empty()
        || s.shared.req_owner.borrow().values().any(|o| matches!(o, ReqOwner::Menu { win: w } if *w == win))
    {
        return Reply::None;
    }
    let (x, y, owner) = to_screen(s, at);
    let shell = &s.shared.services.shell;
    if owner != 0 {
        if let Some(pid) = shell.host_pid() {
            // SAFETY: the process id is the host's; the call only lets that process take the foreground.
            let _ = unsafe { AllowSetForegroundWindow(pid) };
        }
    }
    let req = shell.next_req();
    s.shared.req_owner.borrow_mut().insert(req, ReqOwner::Menu { win });
    shell.request(ShellReq::Menu {
        req,
        items: paths.into_iter().map(ItemRef::Path).collect(),
        at: (x, y),
        extended: shift_down(),
        owner,
    });
    let _ = cx;
    Reply::None
}

/// A pick of our menus that is not a key-table command.
pub fn on_pick(s: &mut FilesScreen, cx: &mut EventCx, id: CommandId) -> Reply {
    // The sidebar's own menu (F4.8, F4.9): its ids are its own.
    if let Some(r) = crate::sidebar::on_pick(s, cx, id) {
        return r;
    }
    if let Some(r) = crate::tab::on_pick(s, cx, id) {
        return r;
    }
    match id.0 {
        pick::OPEN_WITH => exec_selected(s, Verb::OpenWith),
        pick::OPEN_NEW_WINDOW => {
            let paths = selected(s);
            for p in paths.into_iter().take(NEW_WINDOWS_MAX) {
                let first = Location::Fs(p);
                window::request_loop(&s.shared, LoopReq::Main { first, select: None, cascade_from: Some(s.win) });
            }
            Reply::None
        }
        pick::PIN => Reply::None,
        pick::MORE_BACKGROUND => {
            let at = focus_anchor(s);
            let dir: Vec<WidePath> = folder(s).into_iter().collect();
            open_system_for(s, cx, at, dir)
        }
        _ => toolmenus::on_pick(s, cx, id),
    }
}

/// A verb of the shell host on the selection, answered to open::on_exec_reply.
fn exec_selected(s: &mut FilesScreen, verb: Verb) -> Reply {
    let paths = selected(s);
    if paths.is_empty() {
        return Reply::None;
    }
    let shell = &s.shared.services.shell;
    let req = shell.next_req();
    s.shared.req_owner.borrow_mut().insert(req, ReqOwner::Exec { win: s.win });
    shell.request(ShellReq::Exec { req, verb, items: paths.into_iter().map(ItemRef::Path).collect() });
    Reply::None
}

fn host_down(s: &mut FilesScreen, cx: &mut EventCx) {
    let lang = cx.lang();
    let retry = (t(k::FILES_ACTION_RETRY, lang), NoticeAct::Retry);
    status::notice(s, cx, Notice { action: Some(retry), ..Notice::new(t(k::FILES_MENU_HOST_DOWN, lang)) });
}

fn not_in_beta(s: &mut FilesScreen, cx: &mut EventCx) {
    let lang = cx.lang();
    let open = (t(k::FILES_ACTION_OPEN_EXPLORER_ARROW, lang), NoticeAct::OpenInExplorer);
    status::notice(s, cx, Notice { action: Some(open), ..Notice::new(t(k::FILES_MENU_NOT_IN_BETA, lang)) });
}

/// The menu host's answers: shown and done say nothing; an intercepted verb is mapped; a failure or a fallback is the
/// host_down notice.
pub fn on_menu_reply(s: &mut FilesScreen, cx: &mut EventCx, reply: &ShellReply) {
    let ShellReply::Evt(e) = reply else {
        host_down(s, cx);
        return;
    };
    match e {
        ShellEvt::MenuDone { outcome: MenuOutcome::Intercepted(verb), items, .. } => {
            let paths: Vec<WidePath> = items
                .iter()
                .filter_map(|i| match i {
                    ItemRef::Path(p) => Some(p.clone()),
                    ItemRef::Pidl(_) => None,
                })
                .collect();
            intercepted(s, cx, *verb, paths);
        }
        ShellEvt::MenuDone { outcome: MenuOutcome::Failed(_), .. } | ShellEvt::Failed { .. } => host_down(s, cx),
        _ => {}
    }
}

/// What an intercepted verb becomes (U4).
#[derive(Debug, PartialEq, Eq)]
pub enum Mapped {
    /// ops_ui::delete_flow: the classified Del flow, never a permanent delete.
    Delete(Vec<WidePath>),
    /// rename::begin_path: the first item, when it is in the folder shown.
    Rename(WidePath),
    /// The not_in_beta notice with [Open in File Explorer].
    NotInBeta,
    Nothing,
}

/// U4's mapping of the verbs the shell host does not run itself; `dir` is the folder the tab shows.
pub fn map_verb(verb: CanonVerb, paths: Vec<WidePath>, dir: &WidePath) -> Mapped {
    match verb {
        CanonVerb::Delete => Mapped::Delete(paths),
        CanonVerb::Rename => match paths.into_iter().next() {
            Some(first) if first.parent().as_ref() == Some(dir) => Mapped::Rename(first),
            _ => Mapped::Nothing,
        },
        CanonVerb::Cut | CanonVerb::Copy | CanonVerb::Paste | CanonVerb::PasteLink => Mapped::NotInBeta,
    }
}

fn intercepted(s: &mut FilesScreen, cx: &mut EventCx, verb: CanonVerb, paths: Vec<WidePath>) {
    let dir = s.tabs.active().table.model().listing().dir().clone();
    match map_verb(verb, paths, &dir) {
        Mapped::Delete(items) => {
            ops_ui::delete_flow(s, cx, items);
        }
        Mapped::Rename(p) => {
            rename::begin_path(s, cx, &p);
        }
        Mapped::NotInBeta => not_in_beta(s, cx),
        Mapped::Nothing => {}
    }
}

/// "path" per line, CRLF between lines.
pub fn copy_text(paths: &[WidePath]) -> String {
    let lines: Vec<String> = paths.iter().map(|p| format!("\"{}\"", p.to_string_lossy())).collect();
    lines.join("\r\n")
}

/// OurMenu, SystemMenu, CopyPath.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::OurMenu | Cmd::SystemMenu => {
            if folder(s).is_none() {
                return Reply::None;
            }
            let at = focus_anchor(s);
            // The focused row is the target when nothing is selected.
            let t = s.tabs.active_mut();
            if t.table.selection().is_empty() {
                if let Some(key) = t.table.focused() {
                    t.table.set_selection(cx, Selection::Single(key));
                }
            }
            if c == Cmd::SystemMenu {
                open_system(s, cx, at)
            } else {
                let items = timed_context(s);
                cx.open_menu(items, Anchor::Point(at));
                Reply::Redraw
            }
        }
        Cmd::CopyPath => {
            let mut paths = selected(s);
            if paths.is_empty() {
                paths.extend(folder(s));
            }
            if !paths.is_empty() {
                let text = copy_text(&paths);
                cx.clipboard().set_text(&text);
            }
            Reply::None
        }
        _ => Reply::None,
    }
}

/// The sort column a pick names, kept for the callers that need the id of a column.
pub(crate) fn col_of(id: u32) -> Option<ColId> {
    id.checked_sub(pick::SORT_COL).filter(|n| *n <= 6).map(|n| ColId(n as u16))
}
