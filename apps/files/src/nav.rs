//! A tab's navigation (P3-files-app 2.2; FA-NAV fills it, F1a.5): location, history, generation, page state and the
//! effects the screen runs. Pure: it never touches a service.

use oled_trust::frame::files::Location;

use crate::listing::collate::utf16_to_wtf8;
use crate::listing::{Batch, CancelToken, EnumError, Gen, TabId, WidePath};

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Listing,
    Loading { since_ms: u64 },
    Busy,
    StillLoading,
    Empty,
    Denied,
    Connecting,
    Offline,
    NotFound { typed: String },
    TooLong,
    Unsupported,
    HandOff,
    Home,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavTimer {
    Blank60,
    Busy300,
    Slow1000,
    Offline4000,
    Still10000,
    Merge100,
}

#[derive(Debug)]
pub enum NavEffect {
    Enumerate {
        dir: WidePath,
        gen: Gen,
        cancel: CancelToken,
    },
    Cancel(CancelToken),
    Arm(NavTimer, u32),
    Disarm(NavTimer),
    ShowNew,
    KeepOld,
    /// The name to focus and select.
    Focus(Option<Box<[u8]>>),
    Watch(WidePath),
    Unwatch,
    FreeSpace(WidePath),
    HandOff(Location),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchVerdict {
    Drop,
    First,
    More,
    Done,
    Failed(EnumError),
}

/// Everything a refused navigation needs to put the tab back where it was.
#[derive(Debug)]
struct Revert {
    location: Location,
    history: Vec<Location>,
    pos: usize,
    page: Page,
    gen: Gen,
    /// The saved state's listing was still running when it was left (start() cancelled it).
    loading: bool,
}

const ALL_TIMERS: [NavTimer; 6] = [
    NavTimer::Blank60,
    NavTimer::Busy300,
    NavTimer::Slow1000,
    NavTimer::Offline4000,
    NavTimer::Still10000,
    NavTimer::Merge100,
];

fn bit(t: NavTimer) -> u8 {
    1 << (t as u8)
}

/// Location, history (at most 100), position, generation, page, the pending listing and the cancel token. Pure: it
/// returns the effects the screen runs. A tab built by `new` expects its first batches with gen() (the start-up
/// enumeration, or the screen's first refresh()).
#[derive(Debug)]
pub struct NavState {
    tab: TabId,
    location: Location,
    history: Vec<Location>,
    pos: usize,
    gen: Gen,
    next_gen: u64,
    page: Page,
    cancel: CancelToken,
    /// An enumeration of `gen` is running.
    loading: bool,
    /// The old listing is still on screen (no batch yet, under 60 ms).
    pending: bool,
    started_ms: u64,
    got_first: bool,
    count: u64,
    focus: Option<Box<[u8]>>,
    busy: bool,
    /// Bitmask of the armed timers.
    armed: u8,
    last_merge_ms: u64,
    revert: Option<Revert>,
}

/// History entries kept.
const HISTORY_CAP: usize = 100;

impl NavState {
    pub fn new(tab: TabId, start: Location) -> NavState {
        let fs = matches!(start, Location::Fs(_));
        let page = if fs { Page::Loading { since_ms: 0 } } else { Page::Home };
        NavState {
            tab,
            history: vec![start.clone()],
            location: start,
            pos: 0,
            gen: Gen(1),
            next_gen: 2,
            page,
            cancel: CancelToken::new(),
            loading: fs,
            pending: false,
            started_ms: 0,
            got_first: false,
            count: 0,
            focus: None,
            busy: false,
            armed: 0,
            last_merge_ms: 0,
            revert: None,
        }
    }

    fn snapshot(&self) -> Revert {
        Revert {
            location: self.location.clone(),
            history: self.history.clone(),
            pos: self.pos,
            page: self.page.clone(),
            gen: self.gen,
            loading: self.loading,
        }
    }

    fn arm(&mut self, t: NavTimer, ms: u32, fx: &mut Vec<NavEffect>) {
        self.armed |= bit(t);
        fx.push(NavEffect::Arm(t, ms));
    }

    fn disarm(&mut self, t: NavTimer, fx: &mut Vec<NavEffect>) {
        if self.armed & bit(t) != 0 {
            self.armed &= !bit(t);
            fx.push(NavEffect::Disarm(t));
        }
    }

    fn disarm_all(&mut self, fx: &mut Vec<NavEffect>) {
        for t in ALL_TIMERS {
            self.disarm(t, fx);
        }
    }

    /// The new location replaces the old listing on screen.
    fn show_new(&mut self, fx: &mut Vec<NavEffect>) {
        self.pending = false;
        fx.push(NavEffect::Unwatch);
        fx.push(NavEffect::ShowNew);
    }

    /// Starts listing `self.location` under a new generation.
    fn start(&mut self, now_ms: u64) -> Vec<NavEffect> {
        let mut fx = Vec::new();
        self.disarm_all(&mut fx);
        if self.loading {
            fx.insert(0, NavEffect::Cancel(self.cancel.clone()));
        }
        // Whatever is not blank yet (a listing, Home, an error page) stays on screen until the first batch or 60 ms.
        let had_listing = !matches!(self.page, Page::Loading { .. } | Page::Busy);
        self.loading = false;
        self.busy = false;
        self.pending = false;
        self.got_first = false;
        self.count = 0;
        self.started_ms = now_ms;
        self.last_merge_ms = now_ms;
        let dir = match &self.location {
            Location::Fs(p) => p.clone(),
            other => {
                // Home needs no enumeration; Search and Options are not browsed by a tab yet.
                self.page = if *other == Location::Options || matches!(other, Location::Search { .. }) {
                    Page::Unsupported
                } else {
                    Page::Home
                };
                self.focus = None;
                self.show_new(&mut fx);
                return fx;
            }
        };
        self.gen = Gen(self.next_gen);
        self.next_gen += 1;
        self.cancel = CancelToken::new();
        self.loading = true;
        fx.push(NavEffect::Enumerate { dir: dir.clone(), gen: self.gen, cancel: self.cancel.clone() });
        if had_listing {
            self.pending = true;
            fx.push(NavEffect::KeepOld);
            self.arm(NavTimer::Blank60, 60, &mut fx);
        } else {
            self.page = Page::Loading { since_ms: now_ms };
            self.show_new(&mut fx);
        }
        self.arm_wait_timers(dir.is_unc(), &mut fx);
        fx
    }

    /// The timers of a running listing: busy sweep, network pages, "still loading".
    fn arm_wait_timers(&mut self, unc: bool, fx: &mut Vec<NavEffect>) {
        self.arm(NavTimer::Busy300, 300, fx);
        if unc {
            self.arm(NavTimer::Slow1000, 1000, fx);
            self.arm(NavTimer::Offline4000, 4000, fx);
        }
        self.arm(NavTimer::Still10000, 10_000, fx);
    }

    /// The start-up listing was submitted by the app before any window existed, under `gen()` and `cancel`. Takes
    /// that job over: leaving the folder cancels it, and its timers run from `now_ms` and end with Done or a failure.
    /// An addition to the 2.2 interface (the frozen items are unchanged).
    pub fn adopt_start(&mut self, cancel: CancelToken, now_ms: u64) -> Vec<NavEffect> {
        let mut fx = Vec::new();
        self.cancel = cancel;
        if !self.loading {
            return fx;
        }
        self.started_ms = now_ms;
        self.last_merge_ms = now_ms;
        if matches!(self.page, Page::Loading { .. }) {
            self.page = Page::Loading { since_ms: now_ms };
        }
        let unc = matches!(&self.location, Location::Fs(p) if p.is_unc());
        self.arm_wait_timers(unc, &mut fx);
        fx
    }

    fn navigate(&mut self, focus: Option<Box<[u8]>>, revert: Option<Revert>, now_ms: u64) -> Vec<NavEffect> {
        self.revert = revert;
        let fx = self.start(now_ms);
        // Start sets no focus for a non-listing location; the name waits for the Done batch otherwise.
        if self.loading {
            self.focus = focus;
        }
        fx
    }

    pub fn go(&mut self, to: Location, focus: Option<Box<[u8]>>, now_ms: u64) -> Vec<NavEffect> {
        if to == self.location {
            return self.navigate(focus, None, now_ms);
        }
        let snap = self.snapshot();
        self.history.truncate(self.pos + 1);
        self.history.push(to.clone());
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.pos = self.history.len() - 1;
        self.location = to;
        self.navigate(focus, Some(snap), now_ms)
    }

    pub fn back(&mut self, now_ms: u64) -> Vec<NavEffect> {
        match self.pos.checked_sub(1) {
            Some(i) => self.jump(i, now_ms),
            None => Vec::new(),
        }
    }

    pub fn forward(&mut self, now_ms: u64) -> Vec<NavEffect> {
        self.jump(self.pos + 1, now_ms)
    }

    /// Focus = the folder we came from.
    pub fn up(&mut self, now_ms: u64) -> Vec<NavEffect> {
        let Some(parent) = parent_of(&self.location) else {
            return Vec::new();
        };
        let came_from = match &self.location {
            Location::Fs(p) => p.file_name().map(|n| {
                let mut v = Vec::new();
                utf16_to_wtf8(n, &mut v);
                v.into_boxed_slice()
            }),
            _ => None,
        };
        self.go(parent, came_from, now_ms)
    }

    pub fn refresh(&mut self, now_ms: u64) -> Vec<NavEffect> {
        self.navigate(None, None, now_ms)
    }

    pub fn jump(&mut self, history_index: usize, now_ms: u64) -> Vec<NavEffect> {
        if history_index == self.pos {
            return Vec::new();
        }
        let Some(to) = self.history.get(history_index).cloned() else {
            return Vec::new();
        };
        let snap = self.snapshot();
        self.pos = history_index;
        self.location = to;
        self.navigate(None, Some(snap), now_ms)
    }

    pub fn on_batch(&mut self, b: &Batch, now_ms: u64) -> (BatchVerdict, Vec<NavEffect>) {
        let _ = self.tab;
        if b.gen != self.gen || !self.loading {
            return (BatchVerdict::Drop, Vec::new());
        }
        let mut fx = Vec::new();
        if let Some(err) = b.error {
            self.fail(err, &mut fx, now_ms);
            return (BatchVerdict::Failed(err), fx);
        }
        let first = !self.got_first;
        self.got_first = true;
        self.count += b.entries.len() as u64;
        if first {
            self.last_merge_ms = now_ms;
            self.disarm(NavTimer::Blank60, &mut fx);
            self.disarm(NavTimer::Slow1000, &mut fx);
            self.disarm(NavTimer::Offline4000, &mut fx);
            if self.pending {
                self.page = Page::Loading { since_ms: self.started_ms };
                self.show_new(&mut fx);
            }
        }
        if self.count > 0 {
            self.page = Page::Listing;
        }
        if b.done {
            self.disarm_all(&mut fx);
            self.loading = false;
            self.busy = false;
            self.revert = None;
            self.page = if self.count == 0 { Page::Empty } else { Page::Listing };
            if let Location::Fs(p) = &self.location {
                fx.push(NavEffect::Watch(p.clone()));
                fx.push(NavEffect::FreeSpace(p.clone()));
            }
            if let Some(name) = self.focus.take() {
                fx.push(NavEffect::Focus(Some(name)));
            }
            return (BatchVerdict::Done, fx);
        }
        if first {
            return (BatchVerdict::First, fx);
        }
        if self.armed & bit(NavTimer::Merge100) == 0 {
            let elapsed = now_ms.saturating_sub(self.last_merge_ms);
            let wait = 100u64.saturating_sub(elapsed).clamp(1, 100) as u32;
            self.arm(NavTimer::Merge100, wait, &mut fx);
        }
        (BatchVerdict::More, fx)
    }

    fn fail(&mut self, err: EnumError, fx: &mut Vec<NavEffect>, now_ms: u64) {
        self.disarm_all(fx);
        self.loading = false;
        self.busy = false;
        if err == EnumError::NotFound {
            if let Some(r) = self.revert.take() {
                // Stay where we were: the location and history revert, the old listing stays.
                let was_pending = self.pending;
                self.location = r.location;
                self.history = r.history;
                self.pos = r.pos;
                self.focus = None;
                if was_pending && r.loading {
                    // The old folder was still listing when we left it (cancelled): list it again, the old page stays.
                    self.page = r.page;
                    fx.extend(self.start(now_ms));
                } else if was_pending {
                    self.pending = false;
                    self.page = r.page;
                    self.gen = r.gen;
                    fx.push(NavEffect::KeepOld);
                } else {
                    // The new (empty) page is on screen: show the old folder's listing again from scratch.
                    self.page = Page::Loading { since_ms: now_ms };
                    fx.extend(self.start(now_ms));
                }
                return;
            }
        }
        self.revert = None;
        self.focus = None;
        if self.pending {
            self.show_new(fx);
        }
        self.page = match err {
            EnumError::AccessDenied => Page::Denied,
            EnumError::Offline { .. } => Page::Offline,
            EnumError::TooLong => Page::TooLong,
            EnumError::NotFound => Page::NotFound {
                typed: match &self.location {
                    Location::Fs(p) => p.to_string_lossy(),
                    _ => String::new(),
                },
            },
            EnumError::Unsupported | EnumError::Win32(_) => Page::Unsupported,
        };
    }

    pub fn on_timer(&mut self, t: NavTimer, now_ms: u64) -> Vec<NavEffect> {
        let mut fx = Vec::new();
        self.armed &= !bit(t);
        if !self.loading {
            return fx;
        }
        let waiting = matches!(self.page, Page::Loading { .. } | Page::Busy);
        match t {
            NavTimer::Blank60 => {
                if self.pending {
                    self.page = Page::Loading { since_ms: self.started_ms };
                    self.show_new(&mut fx);
                }
            }
            NavTimer::Busy300 => {
                self.busy = true;
                if matches!(self.page, Page::Loading { .. }) {
                    self.page = Page::Busy;
                }
            }
            NavTimer::Slow1000 => {
                if !self.got_first && waiting {
                    self.page = Page::Connecting;
                }
            }
            NavTimer::Offline4000 => {
                if !self.got_first && (waiting || self.page == Page::Connecting) {
                    self.page = Page::Offline;
                }
            }
            NavTimer::Still10000 => {
                self.busy = false;
                if waiting {
                    self.page = Page::StillLoading;
                }
            }
            NavTimer::Merge100 => self.last_merge_ms = now_ms,
        }
        fx
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn gen(&self) -> Gen {
        self.gen
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The 2 px sweep runs: from 300 ms until the listing is done, fails or 10 s pass.
    pub fn busy(&self) -> bool {
        self.loading && self.busy
    }

    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.history.len()
    }

    pub fn can_up(&self) -> bool {
        parent_of(&self.location).is_some()
    }

    /// Entries before (back) or after the current one, nearest first, with their history index.
    pub fn history(&self, back: bool, max: usize) -> Vec<(usize, Location)> {
        let idx: Vec<usize> =
            if back { (0..self.pos).rev().collect() } else { (self.pos + 1..self.history.len()).collect() };
        idx.into_iter().take(max).filter_map(|i| self.history.get(i).map(|l| (i, l.clone()))).collect()
    }
}

/// A drive root's parent is Home; Home, a UNC share root and the other locations have none.
pub fn parent_of(l: &Location) -> Option<Location> {
    match l {
        Location::Fs(p) if p.is_root() => (!p.is_unc()).then_some(Location::Home),
        Location::Fs(p) => p.parent().map(Location::Fs),
        Location::Home | Location::Search { .. } | Location::Options => None,
    }
}
