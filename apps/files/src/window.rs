//! One main window (P3-files-app 2.2, 2.3). FZ2 writes what every group shares: Shared (one per process), the loop
//! requests that open, show and move windows outside any window's event, the engine routing, and FilesScreen as the
//! hook skeleton every region plugs into. FA-WIN fills the bodies of the entry points (navigate, shed, revalidate,
//! cmd) and keeps every hook and the routing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use oled_trust::frame::files::{Location, ReqId};
use oled_ui::host::{AppMsg, Caption, WindowKind, WindowRef, WindowSpec};
use oled_ui::model::{ListModel, TableOut};
use oled_ui::{
    CaptionHit, CommandTable, Event, EventCx, LayoutCx, MouseButton, PaintCx, PointP, RectP, Reply, Screen, SizeP,
    TimerId, UiLang, Widget,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};

use crate::drives::DriveCache;
use crate::format::Fmt;
use crate::hub::Hub;
use crate::keys::{self, Cmd};
use crate::listing::{TabId, WidePath};
use crate::model::IconKey;
use crate::msg::{Fwd, JobDone, Msg};
use crate::nav::Page;
use crate::navcache::NavCache;
use crate::ops_ui::OpsModel;
use crate::options::FilesOptions;
use crate::services::{ReqOwner, Services};
use crate::session::Session;
use crate::tab::Tabs;
use crate::tr::{k, tf};
use crate::types::TypeNames;
use crate::undo::UndoStack;
use crate::watch::WatchId;
use crate::{
    bar, chrome, complete, drives, home, icons, instance, linger, live, menu, names, notices, open, ops_ui, opswin,
    pathline, pins, rename, sidebar, status, tab, toolbar, types, view,
};

/// The start-up window's first tab: app.rs enumerates for it before Shared exists (F1a.10 step 5).
pub const STARTUP_TAB: TabId = TabId(1);

/// How a loop request shows a window: FK-LOOP's oled_ui::Show (2.1), held here until the kit has it; FK-LOOP turns
/// this enum into `pub use oled_ui::Show;` and every caller compiles unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// SW_HIDE.
    Hide,
    /// SW_RESTORE if iconic, SW_SHOW if hidden, then SetForegroundWindow.
    Front,
}

/// One per process, on the UI thread.
pub struct Shared {
    pub hub: Arc<Hub>,
    pub services: Services,
    pub fmt: RefCell<Rc<Fmt>>,
    pub types: Rc<TypeNames>,
    /// services.options.get(), refreshed after each modify.
    pub opts: RefCell<FilesOptions>,
    pub session: RefCell<Session>,
    pub undo: RefCell<UndoStack>,
    pub ops: RefCell<OpsModel>,
    pub lang: Cell<UiLang>,
    pub windows: RefCell<Vec<WinRec>>,
    pub active_win: Cell<u32>,
    /// Starts at 1.
    pub next_win: Cell<u32>,
    /// Starts at 2: STARTUP_TAB is 1.
    pub next_tab: Cell<u32>,
    pub req_owner: RefCell<HashMap<ReqId, ReqOwner>>,
    pub watch_owner: RefCell<HashMap<WatchId, u32>>,
    pub drives: RefCell<DriveCache>,
    pub navcache: RefCell<NavCache>,
    pub next_req: Cell<u64>,
    /// The process-wide first-frame duties ran (F1a.10).
    pub first_frame_done: Cell<bool>,
    /// A screen cannot open, show or move a window itself (no LoopCx in EventCx): it pushes a request here.
    pub loop_requests: RefCell<Vec<LoopReq>>,
    pub loop_event: OwnedEvent,
}

impl Shared {
    pub fn new(hub: Arc<Hub>, services: Services, lang: UiLang, session: Session, types: TypeNames) -> Rc<Shared> {
        let opts = services.options.get();
        Rc::new(Shared {
            hub,
            fmt: RefCell::new(Rc::new(Fmt::system(lang))),
            types: Rc::new(types),
            opts: RefCell::new(opts),
            session: RefCell::new(session),
            undo: RefCell::new(UndoStack::new()),
            ops: RefCell::new(OpsModel::new()),
            lang: Cell::new(lang),
            windows: RefCell::new(Vec::new()),
            active_win: Cell::new(0),
            next_win: Cell::new(1),
            next_tab: Cell::new(STARTUP_TAB.0 + 1),
            req_owner: RefCell::new(HashMap::new()),
            watch_owner: RefCell::new(HashMap::new()),
            drives: RefCell::new(DriveCache::new()),
            navcache: RefCell::new(NavCache::new()),
            next_req: Cell::new(1),
            first_frame_done: Cell::new(false),
            loop_requests: RefCell::new(Vec::new()),
            loop_event: OwnedEvent::new(),
            services,
        })
    }

    /// 1, 2, 3, ... never reused.
    pub fn new_win(&self) -> u32 {
        let w = self.next_win.get();
        self.next_win.set(w + 1);
        w
    }

    /// 2, 3, ... never reused: TabIds are process-unique, so batches cannot reach another window's tab.
    pub fn new_tab(&self) -> TabId {
        let t = self.next_tab.get();
        self.next_tab.set(t + 1);
        TabId(t)
    }

    /// A copy of the record.
    pub fn win(&self, win: u32) -> Option<WinRec> {
        self.windows.borrow().iter().find(|w| w.win == win).cloned()
    }

    /// The main windows, in the order they opened.
    pub fn main_windows(&self) -> Vec<u32> {
        self.windows.borrow().iter().filter(|w| w.kind == WinKind::Main).map(|w| w.win).collect()
    }
}

#[derive(Clone, Debug)]
pub struct WinRec {
    pub win: u32,
    pub kind: WinKind,
    pub wref: Option<WindowRef>,
    /// HWND as isize; None in the Harness.
    pub hwnd: Option<isize>,
    /// The active tab's, for Select reuse (F1a.16).
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinKind {
    Main,
    Ops,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopReq {
    /// A new main window (Ctrl+N, NewWindow and Select of InstanceCmd, the menu's Open in new window).
    Main { first: Location, select: Option<WidePath>, cascade_from: Option<u32> },
    /// The operations window (F3.5).
    Ops,
    /// Hide (linger, F4.12); Front (instance::apply, the linger's reuse).
    Show { win: u32, how: Show },
}

/// An auto-reset event, closed on drop: app.rs waits on it with EventLoop::add_handle.
pub struct OwnedEvent(HANDLE);

impl OwnedEvent {
    /// Unsignalled. A failure leaves a null handle: set() then does nothing and the loop is never woken.
    pub fn new() -> OwnedEvent {
        // SAFETY: no attributes and no name; the handle is owned by the returned value and closed on drop.
        let h = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.unwrap_or_default();
        OwnedEvent(h)
    }

    pub fn handle(&self) -> HANDLE {
        self.0
    }

    /// SetEvent; false when there is no event.
    pub fn set(&self) -> bool {
        if self.0.is_invalid() {
            return false;
        }
        // SAFETY: the handle is ours and open until drop.
        unsafe { SetEvent(self.0) }.is_ok()
    }
}

impl Default for OwnedEvent {
    fn default() -> OwnedEvent {
        OwnedEvent::new()
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle is ours; nothing uses it after drop.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Pushes the request and signals loop_event; app.rs registered the event with EventLoop::add_handle.
pub fn request_loop(shared: &Shared, r: LoopReq) {
    shared.loop_requests.borrow_mut().push(r);
    shared.loop_event.set();
}

/// What run_loop_requests needs; app.rs implements it over LoopCx (after FK-LOOP), tests record the calls.
pub trait LoopOps {
    fn open(&mut self, spec: WindowSpec, screen: Box<dyn Screen>) -> Option<WindowRef>;
    fn proxy(&self, w: WindowRef) -> Option<oled_ui::host::Proxy>;
    fn hwnd(&self, w: WindowRef) -> Option<isize>;
    fn show(&mut self, w: WindowRef, how: Show) -> bool;
    fn move_to(&mut self, w: WindowRef, at: PointP) -> bool;
}

/// Runs the pending loop requests in order, outside any window's event. Main: a new FilesScreen on a fresh TabId,
/// opened with main_spec, recorded, attached to the hub, moved by cascade_pos, then shown in front. Ops: the same with
/// OpsScreen and ops_spec. Show: the window's show (a window that is gone is ignored).
pub fn run_loop_requests(shared: &Rc<Shared>, ops: &mut dyn LoopOps) {
    loop {
        let next = {
            let mut q = shared.loop_requests.borrow_mut();
            if q.is_empty() {
                break;
            }
            q.remove(0)
        };
        match next {
            LoopReq::Main { first, select, cascade_from } => {
                let win = shared.new_win();
                let tab = shared.new_tab();
                let screen = FilesScreen::new(Rc::clone(shared), win, first.clone(), select, tab);
                let Some(wref) = ops.open(main_spec(shared), Box::new(screen)) else {
                    shared.hub.unroute(tab);
                    continue;
                };
                let rec =
                    WinRec { win, kind: WinKind::Main, wref: Some(wref), hwnd: ops.hwnd(wref), location: Some(first) };
                shared.windows.borrow_mut().push(rec);
                if let Some(p) = ops.proxy(wref) {
                    shared.hub.attach(win, Box::new(p));
                }
                if let Some(at) = cascade_from.and_then(|from| cascade_pos(shared, from)) {
                    ops.move_to(wref, at);
                }
                ops.show(wref, Show::Front);
            }
            LoopReq::Ops => {
                let win = shared.new_win();
                let screen = opswin::OpsScreen::new(Rc::clone(shared), win);
                let Some(wref) = ops.open(opswin::ops_spec(shared), Box::new(screen)) else { continue };
                let rec = WinRec { win, kind: WinKind::Ops, wref: Some(wref), hwnd: ops.hwnd(wref), location: None };
                shared.windows.borrow_mut().push(rec);
                if let Some(p) = ops.proxy(wref) {
                    shared.hub.attach(win, Box::new(p));
                }
                ops.show(wref, Show::Front);
            }
            LoopReq::Show { win, how } => {
                if let Some(wref) = shared.win(win).and_then(|r| r.wref) {
                    ops.show(wref, how);
                }
            }
        }
    }
}

/// The main window's spec: Caption::System in F1a (F1b.1 switches this one line to Custom), MIN_SIZE, WINDOW_CLASS,
/// "<name> Beta" while STAGE_BETA, DEFAULT_SIZE, the placement. FA-WIN keeps the placement for window 1 only.
pub fn main_spec(shared: &Shared) -> WindowSpec {
    let lang = shared.lang.get();
    let name = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
    let title = if names::STAGE_BETA { tf(k::FILES_APP_TITLE_BETA, lang, &[("name", &*name)]) } else { name };
    WindowSpec {
        kind: WindowKind::Opaque { min: names::MIN_SIZE, caption: Caption::System },
        class: names::WINDOW_CLASS,
        title,
        size: names::DEFAULT_SIZE,
        owner: None,
        esc_closes: false,
        placement: Some(names::MAIN_PLACEMENT),
        icon_resource: Some(1),
        opened_by_chord: false,
    }
}

/// The from window's top-left plus CASCADE x n, clamped to its monitor's work area. Filled by FA-WIN; until then None.
pub fn cascade_pos(shared: &Shared, from: u32) -> Option<PointP> {
    let _ = (shared, from);
    None
}

/// Called on EnginesWake by whichever screen is the primary: drains every engine and re-posts each reply or event to
/// the window it belongs to.
pub fn drain_engines(shared: &Shared) {
    shared.hub.wake_done();
    let out = ops_ui::drain(shared).into_iter().chain(open::drain_shell(shared)).chain(live::drain(shared));
    for (win, fwd) in out {
        shared.hub.post_to(win, Msg::Fwd(fwd));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Tabs,
    Toolbar,
    Path,
    Sidebar,
    List,
    Status,
}

/// The memory pressure order (4.1): the navigation cache first, then background listings, then per-item icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shed {
    NavCache,
    Background,
    ItemIcons,
    Everything,
}

pub struct FirstFrame {
    /// Rows from 0 painted across the full width.
    pub covered: Cell<i32>,
    pub done: Cell<bool>,
    pub created_ms: u64,
}

#[allow(dead_code)] // the regions' state: their owner groups read it as they land
pub struct FilesScreen {
    pub(crate) shared: Rc<Shared>,
    pub(crate) win: u32,
    pub(crate) tabs: Tabs,
    pub(crate) chrome: chrome::Chrome,
    pub(crate) toolbar: toolbar::Toolbar,
    /// Owns its SplitView (F4.9).
    pub(crate) side: sidebar::Sidebar,
    pub(crate) home: home::Home,
    pub(crate) status: status::Status,
    pub(crate) bar: bar::RecycleBar,
    pub(crate) flyout: ops_ui::Flyout,
    pub(crate) rename: rename::RenameState,
    pub(crate) region: Region,
    pub(crate) commands: CommandTable,
    pub(crate) rects: Layout,
    pub(crate) first: FirstFrame,
    /// Set by layout: the rows now visible want their icons and type names (asked on the next event or message,
    /// since both requests need an EventCx).
    pub(crate) visible_due: bool,
}

impl FilesScreen {
    /// `tab`: STARTUP_TAB from app.rs, shared.new_tab() from run_loop_requests; routed to `win` here.
    pub fn new(shared: Rc<Shared>, win: u32, first: Location, select: Option<WidePath>, tab: TabId) -> FilesScreen {
        let _ = select;
        let t = tab::tab_with_id(&shared, win, first, tab);
        let commands = keys::command_table(shared.lang.get());
        let created_ms = shared.services.clock.now_ms();
        FilesScreen {
            shared,
            win,
            tabs: Tabs::new(t),
            chrome: chrome::Chrome::new(),
            toolbar: toolbar::Toolbar::new(),
            side: sidebar::Sidebar::new(),
            home: home::Home::new(),
            status: status::Status::new(),
            bar: bar::RecycleBar::new(),
            flyout: ops_ui::Flyout::new(),
            rename: rename::RenameState::new(),
            region: Region::List,
            commands,
            rects: Layout::default(),
            first: FirstFrame { covered: Cell::new(0), done: Cell::new(false), created_ms },
            visible_due: false,
        }
    }

    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    pub fn win(&self) -> u32 {
        self.win
    }

    pub fn tabs(&self) -> &Tabs {
        &self.tabs
    }

    pub fn rects(&self) -> &Layout {
        &self.rects
    }

    fn page_is_home(&self) -> bool {
        matches!(self.tabs.active().nav.page(), Page::Home)
    }

    /// The first-frame check: a band clip that spans the full width from the covered edge extends it; the first time
    /// the whole client is covered, one log line and Msg::FirstFrame to this window.
    fn first_frame_check(&self, cx: &PaintCx) {
        if self.first.done.get() {
            return;
        }
        let (w, h) = (self.rects.client.r, self.rects.client.b);
        let clip = cx.clip();
        let covered = self.first.covered.get();
        if clip.l <= 0 && clip.r >= w && clip.t <= covered && clip.b > covered {
            self.first.covered.set(clip.b);
        }
        if h > 0 && self.first.covered.get() >= h {
            self.first.done.set(true);
            let ms = self.shared.services.clock.now_ms().saturating_sub(self.first.created_ms);
            let entries = self.tabs.active().table.model().listing().len();
            oled_sys::log!("files: first frame {ms} ms (entries {entries}, {w}x{h} @ {})", cx.scale().dpi);
            self.shared.hub.post_to(self.win, Msg::FirstFrame);
        }
    }

    /// The window closes: its watch, its tabs' routes, its hub entry and its record go.
    fn closing(&mut self) {
        live::unwatch(self);
        for t in self.tabs.iter() {
            self.shared.hub.unroute(t.id);
        }
        self.shared.hub.detach(self.win);
        self.shared.windows.borrow_mut().retain(|w| w.win != self.win);
    }

    fn on_table_out(&mut self, cx: &mut EventCx, out: TableOut) -> Reply {
        match out {
            TableOut::Activated(key) => {
                let model = self.tabs.active().table.model();
                let folder = model.listing().is_dir(crate::model::id_of(key));
                match model.path_of(key) {
                    Some(p) if folder => {
                        navigate(self, cx, Location::Fs(p), None);
                        Reply::Redraw
                    }
                    _ => open::activate_items(self, cx, &[key]),
                }
            }
            TableOut::ContextMenu(row, at) => menu::open_context(self, cx, row, at),
            TableOut::RenameRequested(key) => rename::begin(self, cx, key),
            TableOut::RenameCommitted(key, text) => rename::commit(self, cx, key, text),
            TableOut::RenameCancelled(key) => rename::cancel(self, cx, key),
            TableOut::SortRequested(col, dir) => view::details::sort(self, cx, col, dir),
            TableOut::SelectionChanged | TableOut::FocusChanged(_) => {
                status::selection_changed(self, cx);
                Reply::Redraw
            }
            TableOut::ColumnsChanged => {
                view::details::columns_changed(self, cx);
                Reply::Relayout
            }
            _ => Reply::None,
        }
    }

    fn on_timer(&mut self, cx: &mut EventCx, id: TimerId) -> Reply {
        match (id.0 >> 24) as u8 {
            names::TIMER_NAV_FIRST..=names::TIMER_NAV_LAST => Reply::None,
            names::TIMER_CHIP => {
                ops_ui::on_chip_timer(self, cx);
                Reply::None
            }
            names::TIMER_LINGER => linger::on_linger_timer(self, cx),
            names::TIMER_PRESPAWN => {
                open::on_prespawn_timer(self, cx);
                Reply::None
            }
            names::TIMER_NOTICE => {
                status::on_notice_timer(self, cx);
                Reply::None
            }
            _ => Reply::None,
        }
    }

    fn on_job(&mut self, cx: &mut EventCx, done: JobDone) -> Reply {
        match done {
            JobDone::FreeSpace { .. } => Reply::None,
            JobDone::Drives(list) => {
                for d in list {
                    self.shared.drives.borrow_mut().set(d);
                }
                bump_all(&self.shared);
                Reply::None
            }
            JobDone::DriveFacts(d) => {
                self.shared.drives.borrow_mut().set(d);
                bump_all(&self.shared);
                Reply::None
            }
            JobDone::Children { req, parent, dirs, done } => {
                let taken = complete::on_children(self, cx, req, &parent, &dirs, done)
                    || sidebar::on_children(self, cx, req, &parent, &dirs, done);
                if !taken {
                    pathline::on_siblings(self, cx, req, dirs);
                }
                Reply::None
            }
            JobDone::Facts { req, items } => {
                ops_ui::on_facts(self, cx, req, items);
                Reply::None
            }
            JobDone::Resolved { tab, gen, diff } => {
                live::on_resolved(self, cx, tab, gen, diff);
                Reply::None
            }
            JobDone::Stat { req, path, found } => {
                pathline::on_stat(self, cx, req, path, found);
                Reply::None
            }
            JobDone::Sorted { tab, gen, done } => {
                view::details::on_sorted(self, cx, tab, gen, done);
                Reply::None
            }
            JobDone::KnownFolders(list) => {
                let mut s = self.shared.session.borrow_mut();
                for (name, path) in list {
                    s.known.insert(format!("{name:?}").to_ascii_lowercase(), path.to_string_lossy());
                }
                Reply::None
            }
            JobDone::StockFolderIcon(Some((px, bgra))) => {
                for t in self.tabs.iter_mut() {
                    t.table.model_mut().set_icon(IconKey::Folder, px, Arc::clone(&bgra));
                }
                Reply::Redraw
            }
            JobDone::StockFolderIcon(None) => Reply::None,
        }
    }

    fn on_fwd(&mut self, cx: &mut EventCx, f: Fwd) -> Reply {
        match f {
            Fwd::Shell { owner, reply } => match owner {
                ReqOwner::Icons { .. } => icons::on_icon_reply(self, cx, owner, &reply),
                ReqOwner::Menu { .. } => menu::on_menu_reply(self, cx, &reply),
                ReqOwner::Exec { .. } => open::on_exec_reply(self, cx, &reply),
                ReqOwner::TypeNames | ReqOwner::Pins | ReqOwner::ShellNew => {}
            },
            Fwd::Watch(ev) => live::on_watch(self, cx, ev),
            Fwd::Ops => ops_ui::on_fwd_ops(self, cx),
            Fwd::Own { op, changes } => live::own_changes(self, cx, op, &changes),
            Fwd::Bump => {
                for t in self.tabs.iter_mut() {
                    t.table.model_mut().bump();
                }
                sidebar::on_bump(self, cx);
                return Reply::Relayout;
            }
        }
        Reply::None
    }

    /// Msg::FirstFrame: the prespawn timer and this window's watch; once per process the drive scan, the pins import
    /// and the class icons' rebuild; types.json may be saved from then on.
    fn on_first_frame(&mut self, cx: &mut EventCx) -> Reply {
        cx.set_timer(timer(names::TIMER_PRESPAWN, self.tabs.active().id), 500);
        live::watch_active(self, cx);
        if !self.shared.first_frame_done.replace(true) {
            drives::scan(&self.shared);
            pins::import_if_due(&self.shared);
            icons::on_first_frame(&self.shared);
        }
        Reply::None
    }

    /// A middle click on a folder row opens it in a background tab (F4.2); None when it is not on a folder row.
    fn middle_click(&mut self, cx: &mut EventCx, pos: PointP) -> Option<Reply> {
        let table = &self.tabs.active().table;
        let row = table.visible_rows().find(|&r| table.row_rect(r).is_some_and(|rect| rect.contains(pos)))?;
        let key = table.model().key(row);
        if !table.model().listing().is_dir(crate::model::id_of(key)) {
            return None;
        }
        let path = table.model().path_of(key)?;
        tab::open_background(self, cx, Location::Fs(path));
        Some(Reply::Redraw)
    }

    /// The requests layout could not make (they need an EventCx).
    fn after_layout(&mut self, cx: &mut EventCx) {
        if std::mem::take(&mut self.visible_due) {
            icons::request_visible(self, cx);
            types::request_missing(self, cx);
        }
    }
}

/// Fwd::Bump to every main window.
fn bump_all(shared: &Shared) {
    for w in shared.main_windows() {
        shared.hub.post_to(w, Msg::Fwd(Fwd::Bump));
    }
}

impl Screen for FilesScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let client = RectP::wh(0, 0, avail.w, avail.h);
        let below = chrome::layout(self, cx, client);
        let (tb_h, st_h) = (cx.px(cx.tokens().size.toolbar_h), cx.px(cx.tokens().size.status_h));
        let toolbar = RectP { b: (below.t + tb_h).min(below.b), ..below };
        let status_r = RectP { t: (below.b - st_h).max(toolbar.b), ..below };
        let middle = RectP { t: toolbar.b, b: status_r.t, ..below };
        toolbar::layout(self, cx, toolbar);
        let body = sidebar::layout(self, cx, middle);
        let side = RectP { r: body.l, ..middle };
        if self.page_is_home() {
            home::home_layout(self, cx, body);
        } else {
            view::details::layout(self, cx, body);
        }
        let rest = ops_ui::chip_layout(self, cx, status_r);
        status::layout(self, cx, rest);
        bar::bar_layout(self, cx, rest);
        ops_ui::flyout_layout(self, cx, client);
        self.rects = Layout { client, chrome: RectP { b: below.t, ..client }, toolbar, side, body, status: status_r };
        self.visible_due = true;
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        chrome::paint(self, cx);
        toolbar::paint(self, cx);
        sidebar::paint(self, cx);
        let page = self.tabs.active().nav.page();
        match page {
            Page::Home => home::home_paint(self, cx),
            Page::Listing => view::details::paint(self, cx),
            other => {
                view::details::paint(self, cx);
                view::states::paint_state(self, other, self.rects.body, cx);
            }
        }
        status::paint(self, cx);
        bar::bar_paint(self, cx);
        ops_ui::chip_paint(self, cx);
        ops_ui::flyout_paint(self, cx);
        self.first_frame_check(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.after_layout(cx);
        // First: the open flyout and bar take everything, then the rename field, then the banners' buttons.
        if let Some(r) = ops_ui::flyout_event(self, cx, ev) {
            return r;
        }
        if let Some(r) = bar::bar_event(self, cx, ev) {
            return r;
        }
        if self.rename.active() {
            if let Some(r) = rename::event(self, cx, ev) {
                return r;
            }
        }
        if let Some(r) = view::states::state_event(self, cx, ev) {
            return r;
        }
        match ev {
            Event::Command(id) => {
                return match keys::cmd_of(*id) {
                    Some(c) => keys::run(self, cx, c),
                    None => menu::on_pick(self, cx, *id),
                };
            }
            Event::Text(c) if self.region == Region::List && pathline::type_into(self, cx, *c) => {
                return Reply::Redraw;
            }
            Event::WindowActive(true) => {
                self.shared.active_win.set(self.win);
                self.shared.hub.set_active(self.win);
                self.shared.services.watch.poke_devices();
                live::on_activate(self, cx);
            }
            Event::CloseRequested => {
                if opswin::on_last_close(self, cx) || linger::on_last_close(self, cx) {
                    return Reply::None;
                }
                self.closing();
                return Reply::Close;
            }
            Event::Timer(id) => return self.on_timer(cx, *id),
            Event::PointerDown { pos, button: MouseButton::Right, .. } if self.rects.body.contains(*pos) => {
                open::prespawn(&self.shared);
            }
            Event::PointerUp { button: MouseButton::X1, .. } => return keys::run(self, cx, Cmd::Back),
            Event::PointerUp { button: MouseButton::X2, .. } => return keys::run(self, cx, Cmd::Forward),
            _ => {}
        }
        if let Some(r) = chrome::event(self, cx, ev) {
            return r;
        }
        if let Some(r) = toolbar::event(self, cx, ev) {
            return r;
        }
        if let Some(r) = sidebar::event(self, cx, ev) {
            return r;
        }
        if let Some(r) = status::event(self, cx, ev) {
            return r;
        }
        if self.page_is_home() {
            return home::home_event(self, cx, ev).unwrap_or(Reply::None);
        }
        if let Event::Click { pos, button: MouseButton::Middle, .. } = ev {
            if let Some(r) = self.middle_click(cx, *pos) {
                return r;
            }
        }
        let table = &mut self.tabs.active_mut().table;
        let before = table.visible_rows();
        let out = table.event(cx, ev);
        // Rows a scroll brought into view (the wheel, the keys, the scroll bar) get their icons; a wheel's scroll may
        // still be animating after the event, so it asks in any case.
        let scrolled = matches!(ev, Event::Wheel { .. }) || table.visible_rows() != before;
        if scrolled {
            icons::request_visible(self, cx);
        }
        match out {
            Some(out) => self.on_table_out(cx, out),
            None => Reply::None,
        }
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        self.after_layout(cx);
        let Ok(m) = msg.downcast::<Msg>() else { return Reply::None };
        match *m {
            Msg::Batch { tab, batch } => {
                // FA-WIN: the tab's nav and listing.
                let _ = (tab, batch);
                Reply::None
            }
            Msg::EnginesWake => {
                drain_engines(&self.shared);
                Reply::None
            }
            Msg::Job { done, .. } => self.on_job(cx, done),
            Msg::Fwd(f) => self.on_fwd(cx, f),
            Msg::Instance(c) => instance::apply(self, cx, c),
            Msg::Updated { version } => {
                notices::on_updated(self, cx, version);
                Reply::None
            }
            Msg::Lang => {
                self.commands = keys::command_table(self.shared.lang.get());
                Reply::Relayout
            }
            Msg::FirstFrame => self.on_first_frame(cx),
        }
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }

    fn caption_hit(&self, p: PointP) -> CaptionHit {
        chrome::caption_hit(self, p)
    }
}

/// The regions of the last layout, physical px.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The whole client area.
    pub client: RectP,
    pub chrome: RectP,
    pub toolbar: RectP,
    pub side: RectP,
    pub body: RectP,
    pub status: RectP,
}

/// TimerId(((kind as u32) << 24) | (tab.0 & 0x00FF_FFFF)); kinds in names::TIMER_*.
pub fn timer(kind: u8, tab: TabId) -> TimerId {
    TimerId((u32::from(kind) << 24) | (tab.0 & 0x00FF_FFFF))
}

/// The active tab goes to `to`: NavState::go, its effects, the navigation cache, the sidebar, the watch. Filled by
/// FA-WIN; until then nothing.
pub fn navigate(s: &mut FilesScreen, cx: &mut EventCx, to: Location, focus: Option<Box<[u8]>>) {
    let _ = (s, cx, to, focus);
}

/// The memory pressure order (4.1) and the linger: navcache::clear, background listings, icons::drop_item_icons.
/// Filled by FA-WIN; until then nothing.
pub fn shed(s: &mut FilesScreen, upto: Shed) {
    let _ = (s, upto);
}

/// Re-enumerates the tab's folder beside the shown listing and merges it with the anchor pinned (F3.9, F4.1, F4.3).
/// Filled by FA-WIN; until then nothing.
pub fn revalidate(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId) {
    let _ = (s, cx, tab);
}

/// Back, Forward, Up, Refresh, NextRegion, PrevRegion, FitColumns, NewWindow, SelectAll, SelectNone, Invert, Escape.
/// Filled by FA-WIN; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
