//! Tabs (P3-files-app 2.2; FB-CHROME the basics, FD-TABS the rest, F1b.3 and F4.1): each tab's navigation, table,
//! watch, sort and kept state. TabIds come only from Shared::new_tab, so they are unique in the process.

use oled_trust::frame::files::Location;
use oled_ui::model::{SelectionMode, SortDir, TableStyle};
use oled_ui::node::Text;
use oled_ui::widgets::{TabOut, TabSpec, VirtualTable};
use oled_ui::{Anchor, CommandId, EventCx, MenuItem, Reply, Screen, UiLang, WidgetId};

use crate::keys::Cmd;
use crate::listing::{Gen, TabId, WidePath};
use crate::model::{ColKind, FilesTable};
use crate::nav::{NavEffect, NavState, Page};
use crate::tr::{k, t, t_any};
use crate::view::columns::default_columns;
use crate::watch::WatchId;
use crate::window::{self, FilesScreen, Shared};
use crate::{chrome, live, names};

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
    /// The Tabs clock when the tab was last the shown one: the least recently active listing is dropped first.
    used: u64,
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
    clock: u64,
    /// The tab the open tab menu belongs to.
    pub menu_for: Option<TabId>,
}

/// Closed locations kept for Reopen.
const CLOSED_MAX: usize = 10;

impl Tabs {
    pub fn new(first: Tab) -> Tabs {
        Tabs { tabs: vec![first], selected: 0, closed: Vec::new(), clock: 0, menu_for: None }
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
        let after = self.tabs[self.selected].id;
        self.open_after(after, t, activate)
    }

    /// Inserted right of tab `after` (right of the active one when there is no such tab).
    pub fn open_after(&mut self, after: TabId, t: Tab, activate: bool) -> TabId {
        let id = t.id;
        let near = self.tabs.iter().position(|x| x.id == after).unwrap_or(self.selected);
        let at = (near + 1).min(self.tabs.len());
        if activate {
            self.stamp();
        }
        self.tabs.insert(at, t);
        if activate {
            self.selected = at;
        } else if at <= self.selected {
            self.selected += 1;
        }
        id
    }

    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn can_reopen(&self) -> bool {
        !self.closed.is_empty()
    }

    /// The shown tab is about to become a background one: it is the most recently active of them.
    fn stamp(&mut self) {
        self.clock += 1;
        self.tabs[self.selected].used = self.clock;
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
                if i != self.selected {
                    self.stamp();
                }
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

    /// Label = the folder name (never a path), tip = the full path, busy = nav.busy(), error = the page is Offline.
    /// The key is the TabId.
    pub fn specs(&self, lang: UiLang) -> Vec<TabSpec> {
        self.tabs
            .iter()
            .map(|t| {
                let at = t.nav.location();
                let name = label_of(at, lang);
                TabSpec {
                    key: u64::from(t.id.0),
                    label: Text::Owned(name),
                    tip: Some(Text::Owned(tip_of(at, lang))),
                    icon: None,
                    busy: t.nav.busy(),
                    error: *t.nav.page() == Page::Offline,
                }
            })
            .collect()
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
    let listing = crate::toolmenus::listing(shared, listing_dir(&at), Gen(0));
    let fmt = shared.fmt.borrow().clone();
    let opts = shared.opts.borrow().view_opts();
    let sort = crate::window::default_sort_for(shared, &at);
    let columns = crate::view::details::columns_with_sort(&default_columns(lang), sort.0, sort.1);
    let mut table = FilesTable::new(listing, fmt, shared.types.clone(), opts, shared.services.clock.local_now());
    table.set_lang(lang);
    let table = VirtualTable::new(TABLE_ID, table, columns, SelectionMode::Multi, TableStyle::FILES);
    shared.hub.route(id, win);
    Tab { id, nav: NavState::new(id, at), table, watch: None, sort, kept: None, icon_gen: Gen(0), used: 0 }
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

/// The folder name of a location (the drive for a root, the app's word for Home): never a path.
fn label_of(at: &Location, lang: UiLang) -> String {
    let name = match at {
        Location::Search { root, .. } => crate::toolbar::place_name(&Location::Fs(root.clone()), lang),
        other => crate::toolbar::place_name(other, lang),
    };
    if name.is_empty() {
        crate::toolbar::place_name(&Location::Home, lang)
    } else {
        name
    }
}

/// The full path of a location, for the tab's tip.
fn tip_of(at: &Location, lang: UiLang) -> String {
    match at {
        Location::Fs(p) => p.to_string_lossy(),
        Location::Search { root, .. } => root.to_string_lossy(),
        other => crate::toolbar::place_name(other, lang),
    }
}

/// Where a new tab opens: Home, or the folder of the tab you are in when options.new_tab says "current".
fn new_tab_location(s: &FilesScreen) -> Location {
    if s.shared.opts.borrow().new_tab == "current" {
        s.tabs.active().nav.location().clone()
    } else {
        Location::Home
    }
}

/// NewTab, CloseTab, NextTab, PrevTab, Ctrl+1..8 and Ctrl+9, ReopenTab and OpenNewTab (Ctrl+Enter on folders).
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::NewTab => new_tab_here(s, cx),
        Cmd::CloseTab => {
            let id = s.tabs.active().id;
            close(s, cx, id)
        }
        Cmd::NextTab | Cmd::PrevTab => {
            chrome::sync(s, cx);
            match s.chrome.strip_mut().select_next(cx, c == Cmd::PrevTab) {
                Some(key) => activate(s, cx, key),
                None => Reply::None,
            }
        }
        Cmd::Tab1 | Cmd::Tab2 | Cmd::Tab3 | Cmd::Tab4 | Cmd::Tab5 | Cmd::Tab6 | Cmd::Tab7 | Cmd::Tab8 => {
            select_nth(s, cx, (c as u32 - Cmd::Tab1 as u32) as usize)
        }
        Cmd::LastTab => select_nth(s, cx, usize::MAX),
        Cmd::ReopenTab => match s.tabs.reopen() {
            Some(at) => {
                open_here(s, cx, at, true);
                Reply::Relayout
            }
            None => Reply::None,
        },
        Cmd::OpenNewTab => {
            let folders = selected_folders(s);
            // Each one lands right of the shown tab: the last first keeps their order.
            for p in folders.into_iter().take(NEW_TABS_MAX).rev() {
                open_background(s, cx, Location::Fs(p));
            }
            Reply::Relayout
        }
        _ => Reply::None,
    }
}

/// At most this many tabs are opened by one Open in new tab.
const NEW_TABS_MAX: usize = 8;

/// Ctrl+1..8 (index 0..7) and Ctrl+9 (usize::MAX, the last): no effect past the count.
fn select_nth(s: &mut FilesScreen, cx: &mut EventCx, i: usize) -> Reply {
    chrome::sync(s, cx);
    match s.chrome.strip_mut().select_index(cx, i) {
        Some(key) => activate(s, cx, key),
        None => Reply::None,
    }
}

/// The folders of the selection, in model order; with nothing selected the focused row when it is a folder.
fn selected_folders(s: &FilesScreen) -> Vec<WidePath> {
    let t = s.tabs.active();
    let m = t.table.model();
    let mut out = Vec::new();
    let mut take = |key| {
        if m.listing().is_dir(crate::model::id_of(key)) {
            out.extend(m.path_of(key));
        }
    };
    if t.table.selection().is_empty() {
        t.table.focused().into_iter().for_each(&mut take);
    } else {
        t.table.selection().for_each(m, &mut |_, key| take(key));
    }
    out
}

/// Ids of the tab menu's and the history menus' picks; menu::on_pick hands them over.
pub mod pick {
    pub const DUPLICATE: u32 = 0x2201;
    pub const CLOSE: u32 = 0x2202;
    pub const CLOSE_OTHERS: u32 = 0x2203;
    pub const CLOSE_RIGHT: u32 = 0x2204;
    pub const REOPEN: u32 = 0x2205;
    /// HISTORY + the index in the tab's history (at most 100 entries).
    pub const HISTORY: u32 = 0x2300;
}

/// The strip's answers: select, close, "+", drag to a new place, the tab's menu.
pub fn on_out(s: &mut FilesScreen, cx: &mut EventCx, out: TabOut) -> Reply {
    match out {
        TabOut::Selected(key) => activate(s, cx, key),
        TabOut::CloseRequested(key) => match tab_of(s, key) {
            Some(id) => close(s, cx, id),
            None => Reply::None,
        },
        TabOut::NewRequested => new_tab_here(s, cx),
        TabOut::Moved { key, to } => {
            if let Some(id) = tab_of(s, key) {
                s.tabs.move_to(id, to);
            }
            Reply::Relayout
        }
        TabOut::ContextMenu(key, at) => {
            let Some(id) = tab_of(s, key) else { return Reply::None };
            s.tabs.menu_for = Some(id);
            let items = menu_items(s, id);
            cx.open_menu(items, Anchor::Point(at));
            Reply::Redraw
        }
        _ => Reply::None,
    }
}

/// Duplicate, Close tab, Close other tabs, Close tabs to the right, Reopen closed tab.
fn menu_items(s: &FilesScreen, id: TabId) -> Vec<MenuItem> {
    let lang = s.shared.lang.get();
    let (n, at) = (s.tabs.len(), s.tabs.index_of(id).unwrap_or(0));
    let row = |pick: u32, label: Text, chord: Option<Cmd>, enabled: bool| {
        crate::menu::row(s, pick, label, None, chord, None, enabled, false)
    };
    vec![
        row(pick::DUPLICATE, t(k::FILES_TAB_DUPLICATE, lang), None, true),
        row(pick::CLOSE, t_any(oled_ui::strings::k::KIT_CLOSE_TAB, lang), Some(Cmd::CloseTab), n > 1),
        row(pick::CLOSE_OTHERS, t(k::FILES_TAB_CLOSE_OTHERS, lang), None, n > 1),
        row(pick::CLOSE_RIGHT, t(k::FILES_TAB_CLOSE_RIGHT, lang), None, at + 1 < n),
        MenuItem::Separator,
        row(pick::REOPEN, t(k::FILES_TAB_REOPEN, lang), Some(Cmd::ReopenTab), s.tabs.can_reopen()),
    ]
}

/// The picks of the tab menu and of the history menus; None for an id that is not theirs.
pub fn on_pick(s: &mut FilesScreen, cx: &mut EventCx, id: CommandId) -> Option<Reply> {
    if let Some(index) = id.0.checked_sub(pick::HISTORY).filter(|i| *i < 100) {
        window::jump(s, cx, index as usize);
        return Some(Reply::Relayout);
    }
    if !(pick::DUPLICATE..=pick::REOPEN).contains(&id.0) {
        return None;
    }
    if id.0 == pick::REOPEN {
        return Some(cmd(s, cx, Cmd::ReopenTab));
    }
    let Some(me) = s.tabs.menu_for.take().filter(|m| s.tabs.by_id(*m).is_some()) else { return Some(Reply::None) };
    Some(match id.0 {
        pick::DUPLICATE => {
            let at = s.tabs.by_id(me).map(|t| t.nav.location().clone());
            if let Some(at) = at {
                open_after(s, cx, me, at, true);
            }
            Reply::Relayout
        }
        pick::CLOSE => close(s, cx, me),
        _ => {
            // The tab the menu was opened on stays and is shown; the others go (each can be reopened).
            activate(s, cx, u64::from(me.0));
            let at = s.tabs.index_of(me).unwrap_or(0);
            let gone: Vec<TabId> = s
                .tabs
                .iter()
                .enumerate()
                .filter(|(i, _)| if id.0 == pick::CLOSE_OTHERS { *i != at } else { *i > at })
                .map(|(_, t)| t.id)
                .collect();
            for g in gone {
                close(s, cx, g);
            }
            Reply::Relayout
        }
    })
}

fn tab_of(s: &FilesScreen, key: u64) -> Option<TabId> {
    s.tabs.iter().map(|t| t.id).find(|id| u64::from(id.0) == key)
}

fn new_tab_here(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let at = new_tab_location(s);
    open_here(s, cx, at, true);
    Reply::Relayout
}

/// Makes the tab with this key the shown one: its table as it was, the watch and the status line follow.
fn activate(s: &mut FilesScreen, cx: &mut EventCx, key: u64) -> Reply {
    let Some(id) = tab_of(s, key) else { return Reply::None };
    if id != s.tabs.active().id {
        s.tabs.select(id);
        shown_changed(s, cx);
    }
    chrome::sync(s, cx);
    Reply::Relayout
}

/// Background listings of all tabs together hold at most this many bytes (F4.1).
pub const BACKGROUND_BUDGET: usize = 2 * 1024 * 1024;

/// While the background tabs' listings hold more than BACKGROUND_BUDGET, the least recently active one is dropped
/// (its selection, focus and first row stay as Kept). A listing still being filled is never dropped.
pub(crate) fn enforce_budget(s: &mut FilesScreen) {
    let active = s.tabs.active().id;
    loop {
        let mut total = 0usize;
        let mut lru: Option<(u64, TabId)> = None;
        for t in s.tabs.iter().filter(|t| t.id != active) {
            let l = t.table.model().listing();
            total += l.heap_bytes();
            let droppable = l.done() && l.len() > 0 && !s.pending.contains_key(&t.id);
            if droppable && lru.is_none_or(|(used, _)| t.used < used) {
                lru = Some((t.used, t.id));
            }
        }
        match lru {
            Some((_, id)) if total > BACKGROUND_BUDGET => window::drop_listing(s, id),
            _ => return,
        }
    }
}

/// The tab just shown has a listing: it is revalidated (the old rows stay, the fresh ones are merged by name); a tab
/// whose listing was dropped enumerates again and gets its Kept state back when the listing is done.
fn wake(s: &mut FilesScreen, cx: &mut EventCx) {
    let id = s.tabs.active().id;
    let t = s.tabs.active();
    if !matches!(t.nav.location(), Location::Fs(_))
        || s.pending.contains_key(&id)
        || !matches!(t.nav.page(), Page::Listing | Page::Empty)
    {
        return;
    }
    if t.kept.is_some() {
        let now = s.shared.services.clock.now_ms();
        let fx = s.tabs.active_mut().nav.refresh(now);
        window::run_effects(s, Some(cx), id, fx);
    } else if t.table.model().listing().done() {
        window::revalidate(s, cx, id);
    }
}

/// The active tab changed: its watch, the sidebar's selection, the focus and the status line.
fn shown_changed(s: &mut FilesScreen, cx: &mut EventCx) {
    live::unwatch(s);
    live::watch_active(s, cx);
    let at = s.tabs.active().nav.location().clone();
    crate::sidebar::on_navigated(&s.shared, &at);
    crate::pathline::set_location(s, cx, &at);
    if s.region == window::Region::List && !matches!(s.tabs.active().nav.page(), Page::Home) {
        cx.set_focus(oled_ui::NodeId::new(TABLE_ID, 0));
    }
    crate::status::selection_changed(s, cx);
    wake(s, cx);
    enforce_budget(s);
}

/// Closes a tab: the last one closes the window (as its close button does); else the enumeration and timers of the
/// tab stop, its routes and leftovers go, and the right neighbour (else the left) is shown.
fn close(s: &mut FilesScreen, cx: &mut EventCx, id: TabId) -> Reply {
    if s.tabs.len() <= 1 {
        return s.event(cx, &oled_ui::Event::CloseRequested);
    }
    let was_active = s.tabs.active().id == id;
    stop(s, cx, id);
    s.tabs.close(id);
    chrome::sync(s, cx);
    if was_active {
        shown_changed(s, cx);
    }
    Reply::Relayout
}

/// Stops what a tab left running and forgets it: its enumeration is cancelled (a refresh hands out the token to
/// cancel; only the Cancel and Disarm effects run), its timers, pending and revalidating listings and cached hits go.
fn stop(s: &mut FilesScreen, cx: &mut EventCx, id: TabId) {
    let now = s.shared.services.clock.now_ms();
    if let Some(t) = s.tabs.by_id_mut(id) {
        let fx: Vec<NavEffect> = t
            .nav
            .refresh(now)
            .into_iter()
            .filter(|e| matches!(e, NavEffect::Cancel(_) | NavEffect::Disarm(_)))
            .collect();
        window::run_effects(s, Some(cx), id, fx);
    }
    for kind in names::TIMER_NAV_FIRST..=names::TIMER_NAV_LAST {
        cx.cancel_timer(window::timer(kind, id));
    }
    s.pending.remove(&id);
    if let Some((_, c, _)) = s.fresh.remove(&id) {
        c.cancel();
    }
    s.hits.remove(&id);
    s.deferred.retain(|(t, _)| *t != id);
    s.sort_chosen.remove(&id);
    s.shared.hub.unroute(id);
}

/// Ctrl+Enter, a middle click, the menu's Open in new tab: a tab at `at` right of the shown one, listing in the
/// background; the shown tab and the focus stay. True.
pub fn open_background(s: &mut FilesScreen, cx: &mut EventCx, at: Location) -> bool {
    open_here(s, cx, at, false)
}

/// InstanceCmd::OpenTab (F1a.16) and NewTab: a tab at `at` right of the active one, listing at once; shown when
/// `activate`. True.
pub fn open_here(s: &mut FilesScreen, cx: &mut EventCx, at: Location, activate: bool) -> bool {
    let after = s.tabs.active().id;
    open_after(s, cx, after, at, activate)
}

/// A tab at `at` right of the tab `after`.
fn open_after(s: &mut FilesScreen, cx: &mut EventCx, after: TabId, at: Location, activate: bool) -> bool {
    let t = new_tab(&s.shared, s.win, at);
    let id = s.tabs.open_after(after, t, activate);
    let now = s.shared.services.clock.now_ms();
    let fx = s.tabs.by_id_mut(id).map(|t| t.nav.refresh(now)).unwrap_or_default();
    window::run_effects(s, Some(cx), id, fx);
    if activate {
        shown_changed(s, cx);
    }
    chrome::sync(s, cx);
    true
}

/// The history of Back (`back`) or Forward: the last HISTORY_MAX locations in that direction, nearest first, named by
/// their folder with the path under the name; a pick jumps there (window::jump). False when there is none.
pub fn history_menu(s: &mut FilesScreen, cx: &mut EventCx, back: bool, at: Anchor) -> bool {
    let lang = s.shared.lang.get();
    let list = s.tabs.active().nav.history(back, HISTORY_MAX);
    if list.is_empty() {
        return false;
    }
    let items = list
        .into_iter()
        .map(|(index, loc)| MenuItem::Custom {
            id: CommandId(pick::HISTORY + index as u32),
            label: Text::Owned(label_of(&loc, lang)),
            glyph: None,
            chord: None,
            enabled: true,
            checked: None,
            note: Some(Text::Owned(tip_of(&loc, lang))),
            danger: false,
        })
        .collect();
    cx.open_menu(items, at);
    true
}

/// Locations a history menu lists.
pub const HISTORY_MAX: usize = 12;

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_root_is_a_root() {
        let r = super::placeholder_root();
        assert!(r.is_root() && !r.is_unc());
    }
}
