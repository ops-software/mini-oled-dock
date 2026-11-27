//! Tabs (P3-files-app 2.2; FB-CHROME the basics, FD-TABS the rest, F1b.3 and F4.1): each tab's navigation, table,
//! watch, sort and kept state. TabIds come only from Shared::new_tab, so they are unique in the process.

use oled_trust::frame::files::Location;
use oled_ui::model::{SelectionMode, SortDir, TableStyle};
use oled_ui::widgets::{TabOut, TabSpec, VirtualTable};
use oled_ui::{Anchor, EventCx, Reply, UiLang, WidgetId};

use crate::keys::Cmd;
use crate::listing::{Gen, TabId, WidePath};
use crate::model::{ColKind, FilesTable};
use crate::nav::NavState;
use crate::view::columns::default_columns;
use crate::watch::WatchId;
use crate::window::{FilesScreen, Shared};

/// The widget id of every tab's table: one table is shown at a time.
pub const TABLE_ID: WidgetId = WidgetId(100);

pub struct Tab {
    pub id: TabId,
    pub nav: NavState,
    pub table: VirtualTable<FilesTable>,
    pub watch: Option<WatchId>,
    pub sort: (ColKind, SortDir),
    pub kept: Option<Kept>,
    /// F2.2's icon-request counter.
    pub icon_gen: Gen,
}

/// A background tab whose listing was dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// Selected names, at most 64.
    pub names: Vec<Box<[u8]>>,
    pub focus: Option<Box<[u8]>>,
    pub anchor: Option<(Box<[u8]>, i32)>,
}

/// The tabs of one window: never empty, one selected, the last 10 closed locations.
pub struct Tabs {
    tabs: Vec<Tab>,
    selected: usize,
    closed: Vec<Location>,
}

/// Closed locations kept for Reopen.
const CLOSED_MAX: usize = 10;

impl Tabs {
    pub fn new(first: Tab) -> Tabs {
        Tabs { tabs: vec![first], selected: 0, closed: Vec::new() }
    }

    pub fn active(&self) -> &Tab {
        &self.tabs[self.selected]
    }

    pub fn active_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.selected]
    }

    pub fn by_id(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn by_id_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Tab> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Tab> {
        self.tabs.iter_mut()
    }

    /// Inserted right of the active one.
    pub fn open(&mut self, t: Tab, activate: bool) -> TabId {
        let id = t.id;
        let at = (self.selected + 1).min(self.tabs.len());
        self.tabs.insert(at, t);
        if activate {
            self.selected = at;
        }
        id
    }

    /// Activates the right neighbour, else the left; None when it was the last (it stays).
    pub fn close(&mut self, id: TabId) -> Option<Location> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let i = self.tabs.iter().position(|t| t.id == id)?;
        let gone = self.tabs.remove(i);
        if i < self.selected || self.selected >= self.tabs.len() {
            self.selected = self.selected.saturating_sub(1);
        }
        let at = gone.nav.location().clone();
        if self.closed.len() >= CLOSED_MAX {
            self.closed.remove(0);
        }
        self.closed.push(at.clone());
        Some(at)
    }

    pub fn select(&mut self, id: TabId) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(i) => {
                self.selected = i;
                true
            }
            None => false,
        }
    }

    /// The last closed location.
    pub fn reopen(&mut self) -> Option<Location> {
        self.closed.pop()
    }

    pub fn move_to(&mut self, id: TabId, to: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let active = self.tabs[self.selected].id;
        let t = self.tabs.remove(from);
        self.tabs.insert(to.min(self.tabs.len()), t);
        self.selected = self.tabs.iter().position(|t| t.id == active).unwrap_or(0);
    }

    /// Label = the folder name (never a path), tip = the full path, busy = nav.busy(). Filled by FB-CHROME; until
    /// then none.
    pub fn specs(&self, lang: UiLang) -> Vec<TabSpec> {
        let _ = lang;
        Vec::new()
    }
}

/// A new tab of window `win` at `at`: a fresh TabId, routed to `win` before its first enumeration.
pub fn new_tab(shared: &Shared, win: u32, at: Location) -> Tab {
    let id = shared.new_tab();
    tab_with_id(shared, win, at, id)
}

/// A tab with a given id (STARTUP_TAB at start-up, else one from Shared::new_tab): a NavState, an empty table over
/// enumr.new_listing, and hub.route(id, win).
pub fn tab_with_id(shared: &Shared, win: u32, at: Location, id: TabId) -> Tab {
    let lang = shared.lang.get();
    let listing = shared.services.enumr.new_listing(listing_dir(&at), Gen(0));
    let fmt = shared.fmt.borrow().clone();
    let opts = shared.opts.borrow().view_opts();
    let table = FilesTable::new(listing, fmt, shared.types.clone(), opts, shared.services.clock.local_now());
    let table = VirtualTable::new(TABLE_ID, table, default_columns(lang), SelectionMode::Multi, TableStyle::FILES);
    shared.hub.route(id, win);
    Tab {
        id,
        nav: NavState::new(id, at),
        table,
        watch: None,
        sort: (ColKind::Name, SortDir::Asc),
        kept: None,
        icon_gen: Gen(0),
    }
}

/// The folder a location lists; Home and the Options page list none and get a placeholder root that is never
/// enumerated.
fn listing_dir(at: &Location) -> WidePath {
    match at {
        Location::Fs(p) => p.clone(),
        Location::Search { root, .. } => root.clone(),
        Location::Home | Location::Options => placeholder_root(),
    }
}

fn placeholder_root() -> WidePath {
    const ROOT: [u16; 3] = [b'C' as u16, b':' as u16, b'\\' as u16];
    match WidePath::new(&ROOT) {
        Ok(p) => p,
        Err(e) => unreachable!("a drive root is a valid path: {e:?}"),
    }
}

/// NewTab, CloseTab, NextTab, PrevTab, Tab1-Tab8, LastTab, ReopenTab, OpenNewTab. Filled by FB-CHROME and FD-TABS;
/// until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}

/// Filled by FB-CHROME; until then nothing.
pub fn on_out(s: &mut FilesScreen, cx: &mut EventCx, out: TabOut) -> Reply {
    let _ = (s, cx, out);
    Reply::None
}

/// Ctrl+Enter, a middle click, the menu's Open in new tab (F4.2). Filled by FD-TABS; until then false (the caller
/// shows not_in_beta).
pub fn open_background(s: &mut FilesScreen, cx: &mut EventCx, at: Location) -> bool {
    let _ = (s, cx, at);
    false
}

/// InstanceCmd::OpenTab (F1a.16) and NewTab. Filled by FB-CHROME; until then false (the caller asks LoopReq::Main).
pub fn open_here(s: &mut FilesScreen, cx: &mut EventCx, at: Location, activate: bool) -> bool {
    let _ = (s, cx, at, activate);
    false
}

/// Back or Forward's history (F4.3). Filled by FD-TABS; until then false.
pub fn history_menu(s: &mut FilesScreen, cx: &mut EventCx, back: bool, at: Anchor) -> bool {
    let _ = (s, cx, back, at);
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_root_is_a_root() {
        let r = super::placeholder_root();
        assert!(r.is_root() && !r.is_unc());
    }
}
