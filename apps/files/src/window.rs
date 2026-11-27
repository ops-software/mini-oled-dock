//! One main window (P3-files-app 2.2, 2.3). FZ2 writes what every group shares: Shared (one per process), the loop
//! requests that open, show and move windows outside any window's event, the engine routing, and FilesScreen as the
//! hook skeleton every region plugs into. FA-WIN fills the bodies of the entry points (navigate, shed, revalidate,
//! cmd) and keeps every hook and the routing.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use oled_trust::frame::files::{Location, ReqId};
use oled_ui::host::{AppMsg, Caption, WindowKind, WindowRef, WindowSpec};
use oled_ui::model::{KeySet, ListModel, ScrollTo, Selection, SortDir, TableOut};
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
use crate::listing::{CancelToken, EnumJob, Gen, TabId, WidePath};
use crate::model::{key_of, ColKind, IconKey, Listing};
use crate::msg::{Fwd, JobDone, Msg};
use crate::nav::{BatchVerdict, NavEffect, NavTimer, Page};
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
    pathline, pins, rename, sidebar, status, tab, toolbar, toolmenus, types, view,
};

/// The start-up window's first tab: app.rs enumerates for it before Shared exists (F1a.10 step 5).
pub const STARTUP_TAB: TabId = TabId(1);

/// How a loop request shows a window: the kit's.
pub use oled_ui::Show;

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
    /// The exe was replaced while this process ran (notices::on_updated): no window of it lingers, the ones opened
    /// after the message included.
    pub exe_updated: Cell<bool>,
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
    /// The shell host's warm-up has begun (the prespawn timer or a right button down): per-item icons wait for it, so
    /// the host never starts before 500 ms after the first frame.
    pub host_warm: Cell<bool>,
    /// A screen cannot open, show or move a window itself (no LoopCx in EventCx): it pushes a request here.
    pub loop_requests: RefCell<Vec<LoopReq>>,
    pub loop_event: OwnedEvent,
    /// The process's private bytes now (memory check, 4.1). Reads 0 until app.rs installs private_bytes; a test
    /// installs a script.
    pub memory: RefCell<Box<dyn Fn() -> u64>>,
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
            exe_updated: Cell::new(false),
            next_win: Cell::new(1),
            next_tab: Cell::new(STARTUP_TAB.0 + 1),
            req_owner: RefCell::new(HashMap::new()),
            watch_owner: RefCell::new(HashMap::new()),
            drives: RefCell::new(DriveCache::new()),
            navcache: RefCell::new(NavCache::new()),
            next_req: Cell::new(1),
            first_frame_done: Cell::new(false),
            host_warm: Cell::new(false),
            loop_requests: RefCell::new(Vec::new()),
            loop_event: OwnedEvent::new(),
            memory: RefCell::new(Box::new(|| 0)),
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
    /// The active tab's (Select reuse, F1a.16; Reopen, F4.14): FilesScreen::settle keeps it current.
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
    /// The event loop ends with every window (notices::reopen, F4.14); app.rs then starts the exe again.
    Quit,
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
    /// LoopCx::quit: the loop ends once this callback returns.
    fn quit(&mut self) {}
}

/// Runs the pending loop requests in order, outside any window's event. Main: a new FilesScreen on a fresh TabId,
/// opened with main_spec, recorded, attached to the hub, moved by cascade_pos, then shown in front. Ops: the same with
/// OpsScreen and ops_spec. Show: the window's show (a window that is gone is ignored). Quit: LoopOps::quit, and the
/// requests after it are not run.
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
            LoopReq::Quit => {
                ops.quit();
                break;
            }
        }
    }
}

/// The main window's spec: Caption::Custom (chrome.rs draws the strip, the buttons and answers caption_hit; the kit
/// falls back to the system caption until its custom caption is on), MIN_SIZE, WINDOW_CLASS,
/// "<name> Beta" while STAGE_BETA, DEFAULT_SIZE, the placement. FA-WIN keeps the placement for window 1 only.
pub fn main_spec(shared: &Shared) -> WindowSpec {
    let lang = shared.lang.get();
    let name = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
    let title = if names::STAGE_BETA { tf(k::FILES_APP_TITLE_BETA, lang, &[("name", &*name)]) } else { name };
    WindowSpec {
        kind: WindowKind::Opaque { min: names::MIN_SIZE, caption: Caption::Custom },
        class: names::WINDOW_CLASS,
        title,
        size: names::DEFAULT_SIZE,
        owner: None,
        esc_closes: false,
        placement: shared.main_windows().is_empty().then_some(names::MAIN_PLACEMENT),
        icon_resource: Some(1),
        opened_by_chord: false,
    }
}

/// How many cascade steps a window may stand from the one it came from before the staircase starts over.
const CASCADE_MAX: i32 = 12;

/// Pure core of cascade_pos: the window `from` moved down and right by n steps, where n is the first one that no other
/// window already stands on (a staircase that starts over when the from window is moved), kept inside `work`.
pub fn cascade_target(from: RectP, others: &[PointP], work: RectP, step: i32) -> PointP {
    let at = |n: i32| PointP { x: from.l + n * step, y: from.t + n * step };
    let n = (1..=CASCADE_MAX).find(|n| !others.contains(&at(*n))).unwrap_or(1);
    let p = at(n);
    PointP { x: p.x.min(work.r - from.w()).max(work.l), y: p.y.min(work.b - from.h()).max(work.t) }
}

/// The window's rect on screen (GetWindowRect: a read, no message is sent).
fn window_rect(hwnd: isize) -> Option<RectP> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut r = RECT::default();
    // SAFETY: `r` is a valid out buffer; a window that is gone makes the call fail and nothing is read.
    unsafe { GetWindowRect(HWND(hwnd as *mut std::ffi::c_void), &mut r) }.ok()?;
    Some(RectP { l: r.left, t: r.top, r: r.right, b: r.bottom })
}

/// The from window's top-left plus CASCADE x n logical px, kept inside the work area of its monitor.
pub fn cascade_pos(shared: &Shared, from: u32) -> Option<PointP> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    let hwnd = shared.win(from)?.hwnd?;
    let rect = window_rect(hwnd)?;
    let h = HWND(hwnd as *mut std::ffi::c_void);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `h` is a window of this thread; `mi` has cbSize set and lives for the call.
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi) }.as_bool();
    if !ok {
        return None;
    }
    let work = RectP { l: mi.rcWork.left, t: mi.rcWork.top, r: mi.rcWork.right, b: mi.rcWork.bottom };
    // SAFETY: a window handle; the call only reads.
    let dpi = unsafe { GetDpiForWindow(h) }.max(96);
    let step = oled_ui::Scale { dpi }.px(names::CASCADE);
    let others: Vec<PointP> = shared
        .main_windows()
        .into_iter()
        .filter(|w| *w != from)
        .filter_map(|w| shared.win(w)?.hwnd)
        .filter_map(window_rect)
        .map(|r| PointP { x: r.l, y: r.t })
        .collect();
    Some(cascade_target(rect, &others, work, step))
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
    /// The process start, for the window that opens first; later windows measure from their own creation.
    pub origin: Option<std::time::Instant>,
    /// The paint ran before the hub knew this window: Msg::FirstFrame was dropped and the next event does its work.
    pub lost: Cell<bool>,
}

impl FirstFrame {
    /// The milliseconds the "first frame" line reports: from the process start when one was given, else from creation.
    pub fn ms(&self, now_ms: u64, now: std::time::Instant) -> u64 {
        match self.origin {
            Some(o) => u64::try_from(now.saturating_duration_since(o).as_millis()).unwrap_or(u64::MAX),
            None => now_ms.saturating_sub(self.created_ms),
        }
    }
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
    pub(crate) states: view::states::States,
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
    /// Listings filling beside the shown one: a navigation's, until its first batch or 60 ms.
    pub(crate) pending: HashMap<TabId, Box<dyn Listing>>,
    /// Revalidations running: tab -> (their generation, cancel token, the fresh listing).
    pub(crate) fresh: HashMap<TabId, (Gen, CancelToken, Box<dyn Listing>)>,
    pub(crate) next_fresh: u64,
    /// Tabs showing a listing from the navigation cache while their own enumeration fills beside it: the listing the
    /// tab left is held until the navigation is confirmed (put in the cache) or refused (shown again).
    pub(crate) hits: HashMap<TabId, Hit>,
    /// The list took the keyboard focus once, when the first rows arrived.
    pub(crate) focus_set: bool,
    /// Timer effects that arrived before an EventCx existed (the window's own first listing).
    pub(crate) deferred: Vec<(TabId, NavEffect)>,
    /// The folder the start-up listing could not find, reported on the first event (no EventCx before it).
    pub(crate) deferred_not_found: Option<String>,
    /// Tabs whose user picked a sort: the per-location default no longer applies to them.
    pub(crate) sort_chosen: HashSet<TabId>,
    /// The options' row height reaches the tabs on the first event (a table sets it with an EventCx).
    pub(crate) view_due: bool,
    /// This window's watch and the bookkeeping of live updates (F3.9, F3.10).
    pub(crate) live: live::LiveState,
    /// The shell host's work of this window: the last Exec for Retry, the item icon requests (FB-OPEN).
    pub(crate) open: open::OpenState,
    /// Hidden after the last close, waiting for a start (F4.12).
    pub(crate) linger: linger::Linger,
    /// The hand-off the loop guard held back, for its notice's [Open ↗] (F4.13).
    pub(crate) held: crate::handoff::Held,
    /// The About dialog over the window (F4.15).
    pub(crate) about: crate::about::About,
}

impl FilesScreen {
    /// `tab`: STARTUP_TAB from app.rs, shared.new_tab() from run_loop_requests; routed to `win` here.
    pub fn new(shared: Rc<Shared>, win: u32, first: Location, select: Option<WidePath>, tab: TabId) -> FilesScreen {
        let t = tab::tab_with_id(&shared, win, first, tab);
        let commands = keys::command_table(shared.lang.get());
        let created_ms = shared.services.clock.now_ms();
        let mut me = FilesScreen::bare(shared, win, t, commands, created_ms);
        if tab == STARTUP_TAB {
            // The table's first listing carries Gen(0); the start-up job's batches carry the generation NavState expects.
            let t = me.tabs.active_mut();
            if let Location::Fs(dir) = t.nav.location().clone() {
                let l = toolmenus::listing(&me.shared, dir, t.nav.gen());
                t.table.model_mut().replace(l);
            }
            // Its batches append in the folder's default sort (the header chevron comes from tab_with_id).
            let (kind, dir) = t.sort;
            if let Some(key) = crate::view::columns::sort_key(kind) {
                t.table.model_mut().listing_mut().sort(key, crate::view::details::dir_of(dir));
            }
        } else {
            // Nobody enumerated for this tab: its first listing is a refresh of the generation NavState expects next.
            let id = me.tabs.active().id;
            let now = me.shared.services.clock.now_ms();
            let fx = me.tabs.active_mut().nav.refresh(now);
            run_effects(&mut me, None, id, fx);
        }
        if let Some(name) = select.as_ref().and_then(|p| p.file_name()) {
            let mut wtf8 = Vec::new();
            crate::listing::collate::utf16_to_wtf8(name, &mut wtf8);
            me.tabs.active_mut().nav.want_focus(wtf8.into_boxed_slice());
        }
        me
    }

    /// The start-up window measures its first frame from the process start.
    pub fn set_origin(&mut self, origin: std::time::Instant) {
        self.first.origin = Some(origin);
    }

    /// What the first-frame line would report at `now` (`now_ms` on the services' clock).
    pub fn first_frame_ms(&self, now_ms: u64, now: std::time::Instant) -> u64 {
        self.first.ms(now_ms, now)
    }

    /// The first batch of the start-up listing, taken before the window exists: app.rs waited for it so that the first
    /// frame has rows. It goes where on_batch would put it, without an EventCx (the first layout reads the model).
    pub fn adopt_first_batch(&mut self, batch: crate::listing::Batch) {
        let now = self.shared.services.clock.now_ms();
        let tab = self.tabs.active().id;
        let t = self.tabs.active_mut();
        let before = t.nav.location().clone();
        let (verdict, fx) = t.nav.on_batch(&batch, now);
        if verdict == BatchVerdict::Drop {
            return;
        }
        if let (BatchVerdict::Failed(crate::listing::EnumError::NotFound), Location::Fs(p)) = (&verdict, &before) {
            self.deferred_not_found = Some(p.to_string_lossy());
        }
        let (done, first) = (batch.done, verdict == BatchVerdict::First);
        let failed = matches!(verdict, BatchVerdict::Failed(_));
        if !failed {
            t.table.model_mut().listing_mut().append(batch);
            if done || first {
                t.table.model_mut().listing_mut().merge_pending();
            }
        }
        run_effects(self, None, tab, fx);
    }

    /// The start-up window's tab: app.rs submitted the enumeration before any window existed; NavState takes it over.
    pub fn adopt_start(&mut self, cancel: CancelToken) {
        let id = self.tabs.active().id;
        let now = self.shared.services.clock.now_ms();
        let fx = self.tabs.active_mut().nav.adopt_start(cancel, now);
        run_effects(self, None, id, fx);
    }

    fn bare(shared: Rc<Shared>, win: u32, t: tab::Tab, commands: CommandTable, created_ms: u64) -> FilesScreen {
        FilesScreen {
            shared,
            win,
            tabs: Tabs::new(t),
            chrome: chrome::Chrome::new(),
            toolbar: toolbar::Toolbar::new(),
            side: sidebar::Sidebar::new(),
            home: home::Home::new(),
            status: status::Status::new(),
            states: view::states::States::new(),
            bar: bar::RecycleBar::new(),
            flyout: ops_ui::Flyout::new(),
            rename: rename::RenameState::new(),
            region: Region::List,
            commands,
            rects: Layout::default(),
            first: FirstFrame {
                covered: Cell::new(0),
                done: Cell::new(false),
                created_ms,
                origin: None,
                lost: Cell::new(false),
            },
            visible_due: false,
            pending: HashMap::new(),
            fresh: HashMap::new(),
            next_fresh: 0,
            hits: HashMap::new(),
            focus_set: false,
            deferred: Vec::new(),
            deferred_not_found: None,
            view_due: true,
            sort_chosen: HashSet::new(),
            live: live::LiveState::default(),
            open: open::OpenState::default(),
            linger: linger::Linger::default(),
            held: crate::handoff::Held::default(),
            about: crate::about::About::default(),
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

    /// Whether the tab shows a cached listing beside its running enumeration, and the folder it holds for the cache.
    #[doc(hidden)]
    pub fn hit_state(&self, tab: TabId) -> Option<Option<Location>> {
        self.hits.get(&tab).map(|h| h.left.as_ref().map(|(at, _)| at.clone()))
    }

    /// The tabs, for what opens them outside the window's own events (tests, the session's reopen).
    pub fn tabs_mut(&mut self) -> &mut Tabs {
        &mut self.tabs
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
            let ms = self.first.ms(self.shared.services.clock.now_ms(), std::time::Instant::now());
            let entries = self.tabs.active().table.model().listing().len();
            oled_sys::log!("files: first frame {ms} ms (entries {entries}, {w}x{h} @ {})", cx.scale().dpi);
            // A paint inside EventLoop::open comes before the hub attach: the message is dropped, the next event runs it.
            if !self.shared.hub.post_to(self.win, Msg::FirstFrame) {
                self.first.lost.set(true);
            }
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
            kind @ names::TIMER_NAV_FIRST..=names::TIMER_NAV_LAST => {
                on_nav_timer(self, cx, kind, TabId(id.0 & 0x00FF_FFFF))
            }
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
            JobDone::FreeSpace { root, free, .. } => {
                status::on_free_space(self, cx, root, free);
                Reply::None
            }
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
                if !live::on_stat(self, cx, req, &path, found) {
                    pathline::on_stat(self, cx, req, path, found);
                }
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
                toolmenus::apply_view(self, cx);
                for t in self.tabs.iter_mut() {
                    t.table.model_mut().bump();
                }
                sidebar::on_bump(self, cx);
                // The next event's after_layout may be long in coming (an idle window): ask now.
                icons::request_visible(self, cx);
                types::request_missing(self, cx);
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
        if std::mem::take(&mut self.view_due) {
            toolmenus::apply_view(self, cx);
            cx.relayout();
        }
        if self.first.lost.replace(false) {
            self.on_first_frame(cx);
        }
        if !self.focus_set && !self.page_is_home() && self.tabs.active().table.model().listing().len() > 0 {
            self.focus_set = true;
            cx.set_focus(oled_ui::NodeId::new(tab::TABLE_ID, 0));
        }
        for (tab, e) in std::mem::take(&mut self.deferred) {
            run_effects(self, Some(cx), tab, vec![e]);
        }
        if let Some(path) = self.deferred_not_found.take() {
            report_not_found(self, cx, &path);
        }
        if std::mem::take(&mut self.visible_due) {
            icons::request_visible(self, cx);
            types::request_missing(self, cx);
        }
    }
}

/// Fwd::Bump to every main window.
pub(crate) fn bump_all(shared: &Shared) {
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
            view::states::on_home(self);
            home::home_layout(self, cx, body);
        } else {
            view::details::layout(self, cx, body);
            view::states::layout(self, cx, body);
        }
        let rest = ops_ui::chip_layout(self, cx, status_r);
        status::layout(self, cx, rest);
        bar::bar_layout(self, cx, rest);
        ops_ui::flyout_layout(self, cx, client);
        crate::about::layout(self, cx, client);
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
        crate::about::paint(self, cx);
        self.first_frame_check(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        let r = self.event_inner(cx, ev);
        self.settle(r, false)
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        let r = self.data_inner(cx, msg);
        self.settle(r, true)
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }

    fn caption_hit(&self, p: PointP) -> CaptionHit {
        chrome::caption_hit(self, p)
    }
}

impl FilesScreen {
    /// What the status line and the page state show may have changed with this event or message: when it did and the
    /// reply would not lay out again, it does.
    fn settle(&mut self, r: Reply, with_selection: bool) -> Reply {
        self.record_location();
        let counted = status::refresh(self, with_selection);
        let paged = view::states::refresh(self);
        if (counted || paged) && matches!(r, Reply::None | Reply::Redraw | Reply::Damage(_)) {
            return Reply::Relayout;
        }
        r
    }

    /// This window's record follows its active tab. Every event and message ends in settle, so a navigation, a
    /// history jump, a switch to another tab and a closed tab are all caught here.
    fn record_location(&self) {
        let at = self.tabs.active().nav.location();
        let mut windows = self.shared.windows.borrow_mut();
        if let Some(rec) = windows.iter_mut().find(|w| w.win == self.win && w.location.as_ref() != Some(at)) {
            rec.location = Some(at.clone());
        }
    }

    fn event_inner(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.after_layout(cx);
        // First: the About dialog, the open flyout and bar take everything, then the rename field, then the banners'
        // buttons.
        if let Some(r) = crate::about::event(self, cx, ev) {
            return r;
        }
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
                // The kit's own commands (the caption buttons' maximised/restored state) belong to the chrome.
                if id.is_kit() {
                    if let Some(r) = chrome::event(self, cx, ev) {
                        return r;
                    }
                }
                return match keys::cmd_of(*id) {
                    Some(c) => keys::run(self, cx, c),
                    None => menu::on_pick(self, cx, *id),
                };
            }
            Event::Text(c) if self.region == Region::List && pathline::type_into(self, cx, *c) => {
                return Reply::Redraw;
            }
            Event::WindowActive(true) => {
                check_memory(self);
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
                // The rows that waited for the host ask on the next layout.
                self.visible_due = true;
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
        let mut reply = match out {
            Some(out) => self.on_table_out(cx, out),
            None => Reply::None,
        };
        // What the event queued beyond its one output (Tab in a rename field asks for the neighbour's rename).
        for extra in self.tabs.active_mut().table.take_extra() {
            let r = self.on_table_out(cx, extra);
            reply = match (reply, r) {
                (Reply::Close, _) | (_, Reply::Close) => Reply::Close,
                (Reply::Relayout, _) | (_, Reply::Relayout) => Reply::Relayout,
                (Reply::None, r) | (r, Reply::None) => r,
                _ => Reply::Redraw,
            };
        }
        reply
    }

    fn data_inner(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        self.after_layout(cx);
        let Ok(m) = msg.downcast::<Msg>() else { return Reply::None };
        match *m {
            Msg::Batch { tab, batch } => {
                on_batch(self, cx, tab, batch);
                Reply::Redraw
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
                let lang = self.shared.lang.get();
                self.commands = keys::command_table(lang);
                let fmt = self.shared.fmt.borrow().clone();
                let titles = view::columns::default_columns(lang);
                for t in self.tabs.iter_mut() {
                    t.table.model_mut().set_fmt(Rc::clone(&fmt));
                    t.table.model_mut().set_lang(lang);
                    // Titles only: the user's widths, order, visibility and sort stay.
                    let mut cols = t.table.columns().to_vec();
                    for c in cols.iter_mut() {
                        if let Some(d) = titles.iter().find(|d| d.id == c.id) {
                            c.title = d.title.clone();
                        }
                    }
                    t.table.set_columns(cx, cols);
                }
                Reply::Relayout
            }
            Msg::FirstFrame => self.on_first_frame(cx),
        }
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

/// The active tab goes to `to`: NavState::go, its effects, the listing the navigation cache holds for `to` (shown at
/// once, the navigation's own enumeration revalidates it), the sidebar, the watch. The listing the tab leaves is put in
/// the cache when the new one replaces it (show_new).
pub fn navigate(s: &mut FilesScreen, cx: &mut EventCx, to: Location, focus: Option<Box<[u8]>>) {
    // Only a real move asks the cache: going to the folder the tab is in revalidates it, as Refresh does.
    let hit = ask_cache(&s.shared, s.tabs.active().nav.location(), &to);
    navigate_with(s, cx, to, focus, hit);
}

/// navigate() with the cache's answer given.
#[doc(hidden)]
pub fn navigate_with(
    s: &mut FilesScreen,
    cx: &mut EventCx,
    to: Location,
    focus: Option<Box<[u8]>>,
    hit: Option<crate::navcache::Cached>,
) {
    let hit = hit.filter(|_| *s.tabs.active().nav.location() != to);
    let now = s.shared.services.clock.now_ms();
    let id = s.tabs.active().id;
    pathline::set_error(s, cx, None);
    let fx = s.tabs.active_mut().nav.go(to.clone(), focus, now);
    run_effects(s, Some(cx), id, fx);
    if let Some(c) = hit {
        show_hit(s, cx, id, c);
    }
    sidebar::on_navigated(&s.shared, &to);
}

/// What a navigation that showed a cached listing keeps while its own enumeration fills.
pub(crate) struct Hit {
    /// The folder the tab left and its listing, held for the cache (or to show again if the new folder is refused).
    left: Option<(Location, crate::navcache::Cached)>,
}

/// Shows the cache's listing of the folder the tab just navigated to, with the sort and columns it was left in and the
/// row it was scrolled to. The listing the tab leaves is held; the navigation's own enumeration keeps filling beside
/// the shown one (run_effects does not replace it) and is merged by name when it is done, which is the revalidation.
#[doc(hidden)]
pub fn show_hit(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, c: crate::navcache::Cached) {
    let mut c = c;
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    // A sort the user picked stays with the tab: the cached listing is put in it, whatever it was left in.
    if s.sort_chosen.contains(&tab) && c.sort != t.sort {
        let (kind, dir) = t.sort;
        if let Some(key) = crate::view::columns::sort_key(kind) {
            c.listing.sort(key, crate::view::details::dir_of(dir));
        }
        c.sort = t.sort;
    }
    let left = leaving(t, c.listing.dir());
    let old = install(t, cx, c);
    let left = if let std::collections::hash_map::Entry::Vacant(e) = s.pending.entry(tab) {
        // The window was not showing a finished page, so the new generation's listing was already put on show:
        // it goes back to the side to receive its batches.
        e.insert(old);
        None
    } else {
        left.map(|(at, anchor, sort)| (at, crate::navcache::Cached { listing: old, anchor, sort }))
    };
    s.hits.insert(tab, Hit { left });
}

/// Puts `c` on show; returns the listing it replaces.
fn install(t: &mut tab::Tab, cx: &mut EventCx, c: crate::navcache::Cached) -> Box<dyn Listing> {
    // What a dropped listing of another folder kept does not belong to this one.
    t.kept = None;
    let (kind, dir) = c.sort;
    t.sort = (kind, dir);
    let old = t.table.model_mut().replace(c.listing);
    let cols = crate::view::details::columns_with_sort(t.table.columns(), kind, dir);
    t.table.set_columns(cx, cols);
    t.table.set_selection(cx, Selection::None);
    t.table.model_changed(cx, false);
    // The kit has no scroll offset to set: the anchor row is scrolled to the top.
    if let Some(a) = c.anchor {
        let m = t.table.model();
        if let Some(row) = m.row_of(key_of(a.id)) {
            let k = ListModel::key(m, row);
            t.table.set_focused(cx, k, ScrollTo::Top);
        }
    }
    old
}

/// The navigation is over (confirmed or replaced): the listing the tab left goes to the cache.
fn flush_hit(s: &mut FilesScreen, tab: TabId) {
    if let Some(Hit { left: Some((at, c)) }) = s.hits.remove(&tab) {
        crate::navcache::put(&s.shared, &at, c);
    }
}

/// The enumeration beside a cached listing is done: it is merged by name into the shown one, keeping ids and the
/// anchor, like a revalidation.
fn finish_hit(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, gen: Gen) {
    if !s.hits.contains_key(&tab) || s.pending.get(&tab).is_none_or(|p| p.gen() != gen) {
        return;
    }
    if let Some(mut fresh) = s.pending.remove(&tab) {
        fresh.merge_pending();
        if let Some(t) = s.tabs.by_id_mut(tab) {
            if fresh.dir() == t.table.model().listing().dir() {
                t.table.model_mut().listing_mut().merge_fresh(fresh);
                t.table.model_changed(cx, true);
                let mut sel = t.table.selection().clone();
                sel.retain_present(t.table.model());
                t.table.set_selection(cx, sel);
            }
        }
    }
    flush_hit(s, tab);
}

/// The folder shown from the cache failed. When the failure sent the tab back to the folder it left, that listing is
/// shown again and the restarted enumeration of it revalidates it by name (a restart that already put its own,
/// still empty, listing on show hands it back to the side to receive its batches, as show_hit does). Any other failure
/// keeps the failed folder's empty listing on show and gives the listing the tab left to the cache.
fn restore_after_hit(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, hit: Hit) {
    let Some((at, held)) = hit.left else { return };
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    if *t.nav.location() != at {
        crate::navcache::put(&s.shared, &at, held);
        return;
    }
    let restart_shown = !s.pending.contains_key(&tab) && t.table.model().listing().gen() == t.nav.gen();
    let old = install(t, cx, held);
    if restart_shown {
        s.pending.insert(tab, old);
        s.hits.insert(tab, Hit { left: None });
    } else if s.pending.contains_key(&tab) {
        s.hits.insert(tab, Hit { left: None });
    }
}

/// A navigation that showed a cached listing failed: the cached rows of the refused folder go at once, so that neither
/// the page nor show_new (which would cache them) keeps them.
fn drop_shown_hit(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId) {
    let Some(dir) = s.tabs.by_id(tab).map(|t| t.table.model().listing().dir().clone()) else { return };
    let empty = toolmenus::listing(&s.shared, dir, Gen(0));
    if let Some(t) = s.tabs.by_id_mut(tab) {
        drop(t.table.model_mut().replace(empty));
        t.table.set_selection(cx, Selection::None);
        t.table.model_changed(cx, false);
    }
}

/// The cache's listing of `to`, when the tab really moves there: going nowhere (Back at the first entry, a refresh)
/// never asks, or another window's older copy would replace the live listing.
fn ask_cache(shared: &Shared, from: &Location, to: &Location) -> Option<crate::navcache::Cached> {
    (from != to).then(|| crate::navcache::take(shared, to)).flatten()
}

/// "Can't find {path}" in the path line (FA-WIN's NotFound).
fn report_not_found(s: &mut FilesScreen, cx: &mut EventCx, path: &str) {
    let lang = s.shared.lang.get();
    pathline::set_error(s, cx, Some(tf(k::FILES_STATE_NOT_FOUND, lang, &[("path", path)])));
}

fn nav_timer_of(kind: u8) -> &'static [NavTimer] {
    match kind {
        1 => &[NavTimer::Blank60],
        2 => &[NavTimer::Busy300],
        // Slow1000 runs before the first batch and Merge100 after it, so they share a kind.
        3 => &[NavTimer::Slow1000, NavTimer::Merge100],
        4 => &[NavTimer::Offline4000],
        _ => &[NavTimer::Still10000],
    }
}

fn kind_of(t: NavTimer) -> u8 {
    match t {
        NavTimer::Blank60 => 1,
        NavTimer::Busy300 => 2,
        NavTimer::Slow1000 | NavTimer::Merge100 => 3,
        NavTimer::Offline4000 => 4,
        NavTimer::Still10000 => 5,
    }
}

/// Runs NavState's effects for `tab`. Without an EventCx the timers wait in `deferred` for the first event.
pub(crate) fn run_effects(s: &mut FilesScreen, mut cx: Option<&mut EventCx>, tab: TabId, fx: Vec<NavEffect>) {
    for e in fx {
        if matches!(e, NavEffect::Enumerate { .. } | NavEffect::Cancel(_)) {
            // A new enumeration ends the navigation that showed a cached listing.
            flush_hit(s, tab);
        }
        if matches!(e, NavEffect::Enumerate { .. } | NavEffect::Cancel(_) | NavEffect::ShowNew) {
            // The tab navigates: a revalidation of the folder it leaves must not merge into the next one.
            drop_fresh(s, tab);
        }
        match e {
            NavEffect::Enumerate { dir, gen, cancel } => {
                let l = toolmenus::listing(&s.shared, dir.clone(), gen);
                s.pending.insert(tab, l);
                s.shared.services.enumr.submit(EnumJob { tab, gen, dir, cancel });
            }
            NavEffect::Cancel(c) => c.cancel(),
            NavEffect::Arm(t, ms) => match cx.as_deref_mut() {
                Some(cx) => cx.set_timer(timer(kind_of(t), tab), ms),
                None => s.deferred.push((tab, NavEffect::Arm(t, ms))),
            },
            NavEffect::Disarm(t) => match cx.as_deref_mut() {
                Some(cx) => cx.cancel_timer(timer(kind_of(t), tab)),
                None => s.deferred.retain(|(_, d)| !matches!(d, NavEffect::Arm(a, _) if *a == t)),
            },
            NavEffect::KeepOld => {}
            // A cached listing stays on show; the pending one is merged into it when it is done.
            NavEffect::ShowNew => {
                if !s.hits.contains_key(&tab) {
                    show_new(s, cx.as_deref_mut(), tab)
                }
            }
            NavEffect::Focus(name) => match (cx.as_deref_mut(), name) {
                (Some(cx), Some(name)) => focus_name(s, cx, tab, &name),
                (None, name @ Some(_)) => s.deferred.push((tab, NavEffect::Focus(name))),
                _ => {}
            },
            NavEffect::Watch(_) => {
                if let Some(cx) = cx.as_deref_mut() {
                    live::watch_active(s, cx);
                }
            }
            NavEffect::Unwatch => live::unwatch(s),
            NavEffect::FreeSpace(dir) => {
                let lane = crate::bg::lane_of(&dir, &s.shared.drives.borrow());
                let job: crate::bg::JobFn = Box::new(move || {
                    let (free, total) = disk_space(&dir);
                    Some(JobDone::FreeSpace { root: dir, free, total })
                });
                s.shared.services.bg.run(s.win, lane, job);
            }
            NavEffect::HandOff(_) => {
                if let Some(cx) = cx.as_deref_mut() {
                    let _ = open::hand_off_current(s, cx);
                }
            }
        }
    }
}

/// GetDiskFreeSpaceExW of a folder: (free bytes for the caller, total bytes).
fn disk_space(dir: &WidePath) -> (Option<u64>, Option<u64>) {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut wide: Vec<u16> = dir.units().to_vec();
    wide.push(0);
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and outlives the call; both out pointers are valid for the call.
    let ok = unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), Some(&mut total), None) };
    if ok.is_ok() {
        (Some(free), Some(total))
    } else {
        (None, None)
    }
}

/// The tab's pending listing replaces the shown one, in the tab's sort. A listing of another folder starts with nothing
/// selected and the first row focused; a refresh of the same folder keeps the selected and focused names.
fn show_new(s: &mut FilesScreen, cx: Option<&mut EventCx>, tab: TabId) {
    let Some(mut fresh) = s.pending.remove(&tab) else { return };
    fresh.merge_pending();
    let chosen = s.sort_chosen.contains(&tab);
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    let left = leaving(t, fresh.dir());
    let (kind, dir) = if chosen { t.sort } else { default_sort_for(&s.shared, t.nav.location()) };
    t.sort = (kind, dir);
    if let Some(key) = crate::view::columns::sort_key(kind) {
        fresh.sort(key, crate::view::details::dir_of(dir));
    }
    let kept = (fresh.dir() == t.table.model().listing().dir()).then(|| kept_names(t));
    if kept.is_none() {
        t.kept = None;
    }
    let old = t.table.model_mut().replace(fresh);
    if let Some((at, anchor, sort)) = left {
        crate::navcache::put(&s.shared, &at, crate::navcache::Cached { listing: old, anchor, sort });
    }
    let cols = crate::view::details::columns_with_sort(t.table.columns(), kind, dir);
    let Some(cx) = cx else { return };
    t.table.set_columns(cx, cols);
    // Row keys are entry ids of the listing just replaced: whatever they pointed at is meaningless now.
    t.table.set_selection(cx, Selection::None);
    t.table.model_changed(cx, false);
    match kept {
        Some((names, focus)) => {
            let m = t.table.model();
            let mut keys = KeySet::new();
            for n in &names {
                if let Some(id) = m.listing().find(n) {
                    keys.insert(key_of(id));
                }
            }
            let focus = focus.and_then(|n| m.listing().find(&n)).map(key_of);
            if !keys.is_empty() {
                t.table.set_selection(cx, Selection::Keys(keys));
            }
            if let Some(k) = focus {
                t.table.set_focused(cx, k, ScrollTo::Nearest);
            }
        }
        None => {
            let first = (ListModel::len(t.table.model()) > 0).then(|| ListModel::key(t.table.model(), 0));
            if let Some(k) = first {
                t.table.set_focused(cx, k, ScrollTo::Top);
            }
        }
    }
}

/// What the navigation cache gets of the tab's shown listing when a listing of another folder replaces it: the folder,
/// the first visible row with its offset, and the sort it was shown in. None for an unfinished or empty listing.
fn leaving(t: &tab::Tab, next: &WidePath) -> Option<(Location, Option<crate::listing::Anchor>, (ColKind, SortDir))> {
    let m = t.table.model();
    let l = m.listing();
    if !l.done() || l.len() == 0 || l.dir() == next {
        return None;
    }
    let first = t.table.visible_rows().start;
    let anchor = (first < ListModel::len(m)).then(|| crate::listing::Anchor {
        id: crate::model::id_of(ListModel::key(m, first)),
        dy: t.table.row_rect(first).map_or(0, |r| r.t - t.table.viewport().t),
    });
    Some((Location::Fs(l.dir().clone()), anchor, t.sort))
}

/// The names of the selected rows and the name of the focused one.
type Names = (Vec<Box<[u8]>>, Option<Box<[u8]>>);

fn kept_names(t: &tab::Tab) -> Names {
    let m = t.table.model();
    let mut names = Vec::new();
    t.table.selection().for_each(m, &mut |_, k| {
        if let Some(n) = m.listing().name(crate::model::id_of(k)) {
            names.push(Box::from(n));
        }
    });
    let focus = t.table.focused().and_then(|k| m.listing().name(crate::model::id_of(k))).map(Box::from);
    (names, focus)
}

/// The sort a folder starts with while the user has not picked one: Downloads by Date modified, newest first, every
/// other folder by Name.
pub(crate) fn default_sort_for(shared: &Shared, at: &Location) -> (ColKind, SortDir) {
    let known = shared.session.borrow();
    match (at, known.known.get("downloads")) {
        (Location::Fs(p), Some(d)) if p.to_string_lossy().eq_ignore_ascii_case(d) => (ColKind::Modified, SortDir::Desc),
        _ => (ColKind::Name, SortDir::Asc),
    }
}

/// The name we came from (or asked for) is focused, selected and scrolled to the middle.
fn focus_name(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, name: &[u8]) {
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    let Some(id) = t.table.model().listing().find(name) else { return };
    let key = key_of(id);
    t.table.set_selection(cx, Selection::Single(key));
    t.table.set_focused(cx, key, ScrollTo::Center);
}

/// Merges the batch into the tab's pending or shown listing, or into a running revalidation. Called by data().
fn on_batch(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, batch: crate::listing::Batch) {
    if s.fresh.get(&tab).is_some_and(|(g, _, _)| *g == batch.gen) {
        fresh_batch(s, cx, tab, batch);
        return;
    }
    let now = s.shared.services.clock.now_ms();
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    let before = t.nav.location().clone();
    let (verdict, fx) = t.nav.on_batch(&batch, now);
    if verdict == BatchVerdict::Drop {
        return;
    }
    if let (BatchVerdict::Failed(crate::listing::EnumError::NotFound), Location::Fs(p)) = (&verdict, &before) {
        report_not_found(s, cx, &p.to_string_lossy());
    }
    let (gen, done, first) = (batch.gen, batch.done, verdict == BatchVerdict::First);
    if done {
        // A listing finished: memory can have grown.
        check_memory(s);
        tab::enforce_budget(s);
    }
    let failed = matches!(verdict, BatchVerdict::Failed(_));
    if !failed {
        match s.pending.get_mut(&tab) {
            Some(p) => p.append(batch),
            None => {
                if let Some(t) = s.tabs.by_id_mut(tab) {
                    t.table.model_mut().listing_mut().append(batch);
                }
            }
        }
    }
    let hit_failed = if failed { s.hits.remove(&tab) } else { None };
    if hit_failed.is_some() {
        drop_shown_hit(s, cx, tab);
    }
    let had_pending = s.pending.contains_key(&tab);
    // The focus names a row of the listing the batch completes: it runs after the merge.
    let (focus, fx): (Vec<NavEffect>, Vec<NavEffect>) = fx.into_iter().partition(|e| matches!(e, NavEffect::Focus(_)));
    run_effects(s, Some(cx), tab, fx);
    let merged_into_shown = !had_pending || !s.pending.contains_key(&tab);
    // A restart (a refused folder reverted) may have queued a newer listing: only the failed one goes.
    if failed && s.pending.get(&tab).is_some_and(|p| p.gen() == gen) {
        s.pending.remove(&tab);
    }
    if let Some(t) = s.tabs.by_id_mut(tab) {
        let merged = merged_into_shown && (done || first) && t.table.model_mut().listing_mut().merge_pending();
        if merged || (merged_into_shown && failed) {
            t.table.model_changed(cx, true);
        }
    }
    run_effects(s, Some(cx), tab, focus);
    match hit_failed {
        Some(h) => restore_after_hit(s, cx, tab, h),
        None if done && !failed => {
            finish_hit(s, cx, tab, gen);
            apply_kept(s, cx, tab);
        }
        None => {}
    }
    // The rows of the last batch need no event to ask for their icons and type names.
    if done && !failed {
        icons::request_visible(s, cx);
        types::request_missing(s, cx);
    }
}

/// A tab that came back from a dropped listing has its listing again: the names it had selected are selected, the
/// first visible row and the focus are where they were (by name; names that are gone are skipped).
fn apply_kept(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId) {
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    if !t.table.model().listing().done() || t.kept.is_none() {
        return;
    }
    let Some(kept) = t.kept.take() else { return };
    let m = t.table.model();
    let find = |n: &[u8]| m.listing().find(n).map(key_of);
    let mut keys = KeySet::new();
    for n in &kept.names {
        if let Some(k) = find(n) {
            keys.insert(k);
        }
    }
    let top = kept.anchor.as_ref().and_then(|(n, _)| find(n));
    let focus = kept.focus.as_deref().and_then(find);
    if !keys.is_empty() {
        t.table.set_selection(cx, Selection::Keys(keys));
    }
    // The kit has no scroll offset to set: the anchor row is scrolled to the top.
    if let Some(k) = top {
        t.table.set_focused(cx, k, ScrollTo::Top);
    }
    if let Some(k) = focus {
        t.table.set_focused(cx, k, ScrollTo::Nearest);
    }
    crate::status::selection_changed(s, cx);
}

/// Drops the tab's running revalidation: its job is cancelled and its batches are ignored from now on.
fn drop_fresh(s: &mut FilesScreen, tab: TabId) {
    if let Some((_, c, _)) = s.fresh.remove(&tab) {
        c.cancel();
    }
}

fn fresh_batch(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, batch: crate::listing::Batch) {
    let (done, failed) = (batch.done, batch.error.is_some());
    if let Some((_, _, l)) = s.fresh.get_mut(&tab) {
        if !failed {
            l.append(batch);
        }
    }
    if failed {
        drop_fresh(s, tab);
        return;
    }
    if !done {
        return;
    }
    let Some((_, _, mut fresh)) = s.fresh.remove(&tab) else { return };
    fresh.merge_pending();
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    if fresh.dir() != t.table.model().listing().dir() {
        return;
    }
    t.table.model_mut().listing_mut().merge_fresh(fresh);
    t.table.model_changed(cx, true);
    let mut sel = t.table.selection().clone();
    sel.retain_present(t.table.model());
    t.table.set_selection(cx, sel);
}

/// A nav timer fired for the tab it names.
fn on_nav_timer(s: &mut FilesScreen, cx: &mut EventCx, kind: u8, tab: TabId) -> Reply {
    let now = s.shared.services.clock.now_ms();
    let Some(t) = s.tabs.by_id_mut(tab) else { return Reply::None };
    let mut fx = Vec::new();
    for nt in nav_timer_of(kind) {
        fx.extend(t.nav.on_timer(*nt, now));
    }
    run_effects(s, Some(cx), tab, fx);
    if let Some(t) = s.tabs.by_id_mut(tab) {
        if kind == 3 && t.table.model_mut().listing_mut().merge_pending() {
            t.table.model_changed(cx, true);
        }
    }
    Reply::Redraw
}

/// The memory pressure order (4.1) and the linger: navcache::clear, background listings (every listing for
/// Shed::Everything), icons::drop_item_icons.
pub fn shed(s: &mut FilesScreen, upto: Shed) {
    crate::navcache::clear(&s.shared);
    if upto >= Shed::Background {
        let active = s.tabs.active().id;
        let ids: Vec<TabId> =
            s.tabs.iter().map(|t| t.id).filter(|id| upto >= Shed::Everything || *id != active).collect();
        for id in ids {
            drop_listing(s, id);
        }
    }
    if upto >= Shed::ItemIcons {
        icons::drop_item_icons(s);
    }
}

/// Above this many private bytes the window sheds what it can rebuild (4.1).
pub const PRESSURE_BYTES: u64 = 18 * 1024 * 1024;

/// The process's private bytes (GetProcessMemoryInfo); 0 when the call fails.
pub fn private_bytes() -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo handle needs no closing; the counters are a valid out buffer of the size in cb. The EX
    // struct begins with the plain one, which is what the call is declared to fill.
    let ok =
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), c.cb) };
    if ok.is_ok() {
        c.PrivateUsage as u64
    } else {
        0
    }
}

/// The memory check (4.1), run when memory can have grown (a listing done, a window activation): while the reading
/// stays above PRESSURE_BYTES the navigation cache goes first, then the background listings, then the per-item icons,
/// each step followed by a new reading. Returns the last step taken.
pub fn check_memory(s: &mut FilesScreen) -> Option<Shed> {
    let mut last = None;
    for step in [Shed::NavCache, Shed::Background, Shed::ItemIcons] {
        if (s.shared.memory.borrow())() <= PRESSURE_BYTES {
            break;
        }
        shed(s, step);
        last = Some(step);
    }
    last
}

/// Names kept per dropped listing.
const KEPT_MAX: usize = 64;

/// Drops a tab's listings: its revalidation is cancelled, and a finished listing is replaced by an empty one with
/// what the tab needs to come back (Kept). A tab whose listing NavState still waits for is left alone: its job
/// belongs to NavState, and dropping the listing would send the rest of the batches into the old folder's.
pub(crate) fn drop_listing(s: &mut FilesScreen, id: TabId) {
    drop_fresh(s, id);
    if s.pending.contains_key(&id) {
        return;
    }
    let Some(t) = s.tabs.by_id(id) else { return };
    let l = t.table.model().listing();
    if !l.done() || l.len() == 0 {
        return;
    }
    let dir = l.dir().clone();
    let (mut names, focus) = kept_names(t);
    names.truncate(KEPT_MAX);
    let first = t.table.visible_rows().start;
    let anchor = (first < ListModel::len(t.table.model())).then(|| {
        let name = t.table.model().listing().name(crate::model::id_of(ListModel::key(t.table.model(), first)));
        let off = t.table.row_rect(first).map_or(0, |r| r.t - t.table.viewport().t);
        name.map(|n| (Box::from(n), off))
    });
    let kept = tab::Kept { names, focus, anchor: anchor.flatten() };
    let empty = toolmenus::listing(&s.shared, dir, Gen(0));
    if let Some(t) = s.tabs.by_id_mut(id) {
        t.kept = Some(kept);
        t.table.model_mut().replace(empty);
    }
}

/// Re-enumerates the tab's folder beside the shown listing and merges it with the anchor pinned (F3.9, F4.1, F4.3).
pub fn revalidate(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId) {
    let _ = cx;
    let Some(t) = s.tabs.by_id(tab) else { return };
    let Location::Fs(dir) = t.nav.location().clone() else { return };
    drop_fresh(s, tab);
    s.next_fresh += 1;
    let gen = Gen((1u64 << 40) + s.next_fresh);
    let cancel = CancelToken::new();
    let l = toolmenus::listing(&s.shared, dir.clone(), gen);
    s.fresh.insert(tab, (gen, cancel.clone(), l));
    s.shared.services.enumr.submit(EnumJob { tab, gen, dir, cancel });
}

/// The active tab jumps to entry `index` of its history (a pick of the Back or Forward menu): the cache's listing of that
/// folder is shown at once, the navigation's own enumeration revalidates it.
pub fn jump(s: &mut FilesScreen, cx: &mut EventCx, index: usize) {
    let now = s.shared.services.clock.now_ms();
    let id = s.tabs.active().id;
    let before = s.tabs.active().nav.location().clone();
    pathline::set_error(s, cx, None);
    let fx = s.tabs.active_mut().nav.jump(index, now);
    run_effects(s, Some(cx), id, fx);
    let at = s.tabs.active().nav.location().clone();
    if let Some(h) = ask_cache(&s.shared, &before, &at) {
        show_hit(s, cx, id, h);
    }
    sidebar::on_navigated(&s.shared, &at);
}

/// Back, Forward, Up, Refresh, NextRegion, PrevRegion, FitColumns, NewWindow, SelectAll, SelectNone, Invert, Escape.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let now = s.shared.services.clock.now_ms();
    let id = s.tabs.active().id;
    let nav_fx = |s: &mut FilesScreen, f: fn(&mut crate::nav::NavState, u64) -> Vec<NavEffect>| {
        f(&mut s.tabs.active_mut().nav, now)
    };
    match c {
        Cmd::Back | Cmd::Forward | Cmd::Up | Cmd::Refresh => {
            let f: fn(&mut crate::nav::NavState, u64) -> Vec<NavEffect> = match c {
                Cmd::Back => crate::nav::NavState::back,
                Cmd::Forward => crate::nav::NavState::forward,
                Cmd::Up => crate::nav::NavState::up,
                _ => crate::nav::NavState::refresh,
            };
            pathline::set_error(s, cx, None);
            let before = s.tabs.active().nav.location().clone();
            let fx = nav_fx(s, f);
            run_effects(s, Some(cx), id, fx);
            let at = s.tabs.active().nav.location().clone();
            // Refresh never takes from the cache: another tab's older copy would replace the live listing.
            if c != Cmd::Refresh {
                if let Some(h) = ask_cache(&s.shared, &before, &at) {
                    show_hit(s, cx, id, h);
                }
            }
            sidebar::on_navigated(&s.shared, &at);
            Reply::Relayout
        }
        Cmd::NextRegion | Cmd::PrevRegion => cycle_region(s, cx, c == Cmd::NextRegion),
        Cmd::FitColumns => {
            view::details::fit_columns(s, cx);
            Reply::Relayout
        }
        Cmd::NewWindow => {
            let first = s.tabs.active().nav.location().clone();
            request_loop(&s.shared, LoopReq::Main { first, select: None, cascade_from: Some(s.win) });
            Reply::None
        }
        Cmd::SelectAll => {
            let t = s.tabs.active_mut();
            let mut sel = t.table.selection().clone();
            sel.select_all();
            t.table.set_selection(cx, sel);
            Reply::Redraw
        }
        Cmd::SelectNone => {
            s.tabs.active_mut().table.set_selection(cx, Selection::None);
            Reply::Redraw
        }
        Cmd::Invert => {
            let t = s.tabs.active_mut();
            let mut keys = KeySet::new();
            let m = t.table.model();
            let sel = t.table.selection();
            for row in 0..ListModel::len(m) {
                let k = ListModel::key(m, row);
                if !sel.contains(k) {
                    keys.insert(k);
                }
            }
            t.table.set_selection(cx, Selection::Keys(keys));
            Reply::Redraw
        }
        Cmd::Escape => {
            if status::on_escape(s, cx) {
                return Reply::Redraw;
            }
            let t = s.tabs.active_mut();
            if t.table.selection().is_empty() {
                return Reply::None;
            }
            t.table.set_selection(cx, Selection::None);
            Reply::Redraw
        }
        _ => Reply::None,
    }
}

/// Path -> Sidebar -> List, skipping the regions that are empty (F1a has the list only).
fn cycle_region(s: &mut FilesScreen, cx: &mut EventCx, forward: bool) -> Reply {
    let mut order = Vec::new();
    // The path bar (a Path region) arrives with F1b.
    if s.rects.side.w() > 0 {
        order.push(Region::Sidebar);
    }
    if !s.page_is_home() {
        order.push(Region::List);
    }
    if order.is_empty() {
        return Reply::None;
    }
    let at = order.iter().position(|r| *r == s.region);
    let n = order.len();
    let next = match (at, forward) {
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    s.region = order[next];
    if s.region == Region::List {
        cx.set_focus(oled_ui::NodeId::new(tab::TABLE_ID, 0));
    } else if s.region == Region::Sidebar {
        sidebar::focus(s, cx);
    }
    Reply::Redraw
}
