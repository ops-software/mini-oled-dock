//! The hub (P3-files-app 2.2, F1a.6): one per process. Routes worker and engine output to the window it belongs to:
//! batches by their tab, jobs by the window that ran them, wakes to the primary window, instance commands to the most
//! recently active window. A message for a window not attached yet waits in a capped queue.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use oled_ui::host::{AppMsg, Proxy};

use crate::listing::{Batch, BatchSink, TabId};
use crate::msg::Msg;
use crate::window::STARTUP_TAB;

/// Where a window's messages go: its proxy, or a test's Collect.
pub trait Poster: Send + Sync {
    fn post(&self, m: AppMsg) -> bool;
}

impl Poster for Proxy {
    fn post(&self, m: AppMsg) -> bool {
        self.send(m)
    }
}

impl<P: Poster + ?Sized> Poster for Arc<P> {
    fn post(&self, m: AppMsg) -> bool {
        (**self).post(m)
    }
}

/// A test Poster: keeps every Msg in order (Harness tests feed them to Harness::send).
#[derive(Default)]
pub struct Collect(pub Mutex<Vec<Msg>>);

impl Collect {
    pub fn new() -> Collect {
        Collect::default()
    }

    /// Every message kept so far, in order; the Collect is empty afterwards.
    pub fn take(&self) -> Vec<Msg> {
        std::mem::take(&mut *lock(&self.0))
    }

    pub fn len(&self) -> usize {
        lock(&self.0).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Poster for Collect {
    fn post(&self, m: AppMsg) -> bool {
        match m.downcast::<Msg>() {
            Ok(m) => {
                lock(&self.0).push(*m);
                true
            }
            Err(_) => false,
        }
    }
}

/// Messages kept for windows that are not attached yet; past it the oldest batch (else the oldest message) goes.
pub const QUEUE_CAP: usize = 4_096;

/// A queued message's destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    Win(u32),
    Primary,
}

/// A message waiting in a window's outbox; `ack` (post_to) learns whether the poster took it.
struct Out {
    msg: Msg,
    ack: Option<Arc<AtomicU8>>,
}

const ACK_FAILED: u8 = 2;
const ACK_POSTED: u8 = 1;

/// An attached window: its poster and the messages not posted yet, in order. At most one post to a poster runs at a
/// time (`sending`), so a window sees its messages in the order they were routed.
struct Win {
    id: u32,
    poster: Arc<dyn Poster>,
    outbox: VecDeque<Out>,
    sending: bool,
}

struct Route {
    wins: Vec<Win>,
    tabs: HashMap<TabId, u32>,
    active: u32,
    pending: VecDeque<(Dest, Msg)>,
    /// Windows that were attached and are gone: their messages are dropped, never queued.
    gone: Vec<u32>,
    /// The primary was dropped since the hub last looked: an EnginesWake in its queue went with it.
    primary_lost: bool,
}

impl Route {
    fn attached(&self, win: u32) -> bool {
        self.wins.iter().any(|w| w.id == win)
    }

    fn primary(&self) -> Option<u32> {
        self.wins.iter().map(|w| w.id).min()
    }

    fn fail_outbox(w: &mut Win) {
        for o in w.outbox.drain(..) {
            if let Some(a) = o.ack {
                a.store(ACK_FAILED, Ordering::Release);
            }
        }
    }

    fn drop_window(&mut self, win: u32) {
        if self.primary() == Some(win) {
            self.primary_lost = true;
        }
        for w in self.wins.iter_mut().filter(|w| w.id == win) {
            Route::fail_outbox(w);
        }
        self.wins.retain(|w| w.id != win);
        self.tabs.retain(|_, w| *w != win);
        self.pending.retain(|(d, _)| *d != Dest::Win(win));
        if !self.gone.contains(&win) {
            self.gone.push(win);
        }
        if self.active == win {
            self.active = 0;
        }
    }

    /// Puts a message in an attached window's outbox; Hub::flush posts it, outside the lock.
    fn enqueue(&mut self, win: u32, m: Msg, ack: Option<Arc<AtomicU8>>) -> bool {
        match self.wins.iter_mut().find(|w| w.id == win) {
            Some(w) => {
                w.outbox.push_back(Out { msg: m, ack });
                true
            }
            None => false,
        }
    }

    fn send(&mut self, win: u32, m: Msg) -> bool {
        self.enqueue(win, m, None)
    }

    /// An EnginesWake to the primary's outbox; with none attached the wake waits in the queue. A primary whose post
    /// fails passes it on (Hub::flush). Afterwards a wake is on its way to the current primary, so an earlier lost
    /// one is covered.
    fn wake_primary(&mut self) {
        match self.primary() {
            Some(p) => {
                self.send(p, Msg::EnginesWake);
            }
            None => self.queue(Dest::Primary, Msg::EnginesWake),
        }
        self.primary_lost = false;
    }

    fn queue(&mut self, d: Dest, m: Msg) {
        if self.pending.len() >= QUEUE_CAP {
            let oldest_batch = self.pending.iter().position(|(_, m)| matches!(m, Msg::Batch { .. }));
            match oldest_batch {
                Some(i) => {
                    self.pending.remove(i);
                }
                None => {
                    self.pending.pop_front();
                }
            }
        }
        self.pending.push_back((d, m));
    }

    fn deliver(&mut self, d: Dest, m: Msg) {
        match d {
            Dest::Win(w) if self.attached(w) => {
                self.send(w, m);
            }
            Dest::Win(w) if self.gone.contains(&w) => {}
            Dest::Primary => match self.primary() {
                Some(p) => {
                    self.send(p, m);
                }
                None => self.queue(d, m),
            },
            Dest::Win(_) => self.queue(d, m),
        }
    }
}

/// The start-up batch: the first batch of STARTUP_TAB, taken by wait_first_batch before the first window exists.
struct FirstSlot {
    batch: Option<Batch>,
    closed: bool,
}

pub struct Hub {
    route: Mutex<Route>,
    wake_in_flight: AtomicBool,
    first: Mutex<FirstSlot>,
    first_ready: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub {
            route: Mutex::new(Route {
                wins: Vec::new(),
                tabs: HashMap::new(),
                active: 0,
                pending: VecDeque::new(),
                gone: Vec::new(),
                primary_lost: false,
            }),
            wake_in_flight: AtomicBool::new(false),
            first: Mutex::new(FirstSlot { batch: None, closed: false }),
            first_ready: Condvar::new(),
        })
    }

    /// Flushes win's pending messages (and the primary's, when win becomes the primary). The first attach closes the
    /// start-up slot: a batch still in it is delivered first.
    pub fn attach(&self, win: u32, p: Box<dyn Poster>) {
        {
            let mut r = lock(&self.route);
            for w in r.wins.iter_mut().filter(|w| w.id == win) {
                Route::fail_outbox(w);
            }
            r.wins.retain(|w| w.id != win);
            r.wins.push(Win { id: win, poster: Arc::from(p), outbox: VecDeque::new(), sending: false });
            let early = {
                let mut s = lock(&self.first);
                s.closed = true;
                s.batch.take()
            };
            if let (Some(batch), Some(w)) = (early, r.tabs.get(&STARTUP_TAB).copied()) {
                r.pending.push_front((Dest::Win(w), Msg::Batch { tab: STARTUP_TAB, batch }));
            }
            let primary = r.primary() == Some(win);
            let mut keep = VecDeque::with_capacity(r.pending.len());
            let mut wake = false;
            while let Some((d, m)) = r.pending.pop_front() {
                let mine = d == Dest::Win(win) || (primary && d == Dest::Primary);
                if !mine {
                    keep.push_back((d, m));
                } else if matches!(m, Msg::EnginesWake) {
                    // Sent last, through wake_primary: it reaches a primary even when win's post fails.
                    wake = true;
                } else {
                    r.send(win, m);
                }
            }
            r.pending = keep;
            if wake {
                r.wake_primary();
            }
            self.rewake_if_lost(&mut r);
        }
        self.flush();
    }

    /// The window closed: its tab routes and its pending messages are dropped. When it was the primary, a wake in
    /// flight goes to the next primary.
    pub fn detach(&self, win: u32) {
        {
            let mut r = lock(&self.route);
            r.drop_window(win);
            self.rewake_if_lost(&mut r);
        }
        self.flush();
    }

    /// After anything that may have dropped the primary (its window closed, or a post to it failed): an EnginesWake
    /// still in its queue went with it, so the next primary gets one (or it waits for the next window). Without this
    /// wake_in_flight would stay set and no engine output would be drained again in this process.
    fn rewake_if_lost(&self, r: &mut Route) {
        if std::mem::take(&mut r.primary_lost) && self.wake_in_flight.load(Ordering::Acquire) {
            r.wake_primary();
        }
    }

    /// Posts every outbox, in order per window. The route lock is held only to pick a message and to record the
    /// outcome: Poster::post runs without it, so a poster may call back into the hub. A window whose post is running
    /// is left to the thread that runs it, which goes on with that window's next message.
    fn flush(&self) {
        let mut r = lock(&self.route);
        loop {
            let Some(i) = r.wins.iter().position(|w| !w.sending && !w.outbox.is_empty()) else {
                return;
            };
            let w = &mut r.wins[i];
            let Some(out) = w.outbox.pop_front() else {
                return;
            };
            w.sending = true;
            let (id, poster) = (w.id, Arc::clone(&w.poster));
            let is_wake = matches!(out.msg, Msg::EnginesWake);
            drop(r);
            let ok = poster.post(Box::new(out.msg));
            r = lock(&self.route);
            if let Some(a) = &out.ack {
                a.store(if ok { ACK_POSTED } else { ACK_FAILED }, Ordering::Release);
            }
            // The window may have been replaced (attach) or dropped while the post ran.
            let same = match r.wins.iter_mut().find(|w| w.id == id && Arc::ptr_eq(&w.poster, &poster)) {
                Some(w) => {
                    w.sending = false;
                    true
                }
                None => false,
            };
            if !ok && same {
                r.drop_window(id);
                if is_wake {
                    r.wake_primary();
                }
                self.rewake_if_lost(&mut r);
            }
        }
    }

    /// A tab created in (or moved to) win; called before the tab's first enumeration.
    pub fn route(&self, tab: TabId, win: u32) {
        lock(&self.route).tabs.insert(tab, win);
    }

    /// A closed tab: its later batches are dropped.
    pub fn unroute(&self, tab: TabId) {
        lock(&self.route).tabs.remove(&tab);
    }

    /// A main window became active.
    pub fn set_active(&self, win: u32) {
        lock(&self.route).active = win;
    }

    /// The lowest attached win (a lingering hidden window and the operations window count).
    pub fn primary(&self) -> Option<u32> {
        lock(&self.route).primary()
    }

    /// Batch -> its tab's window (dropped when the tab is unrouted); Job { win: 0 } and EnginesWake -> the primary;
    /// Job { win } -> win; Instance -> the active window, else the primary; Updated and Lang -> every window. Fwd and
    /// FirstFrame travel only through post_to (dropped here). The route lock is held to look up and queue, never
    /// while a Poster runs.
    pub fn post(&self, m: Msg) {
        {
            let mut r = lock(&self.route);
            let d = match &m {
                Msg::Batch { tab, .. } => match r.tabs.get(tab) {
                    Some(w) => Dest::Win(*w),
                    None => return,
                },
                Msg::EnginesWake => {
                    r.wake_primary();
                    drop(r);
                    self.flush();
                    return;
                }
                Msg::Job { win: 0, .. } => Dest::Primary,
                Msg::Job { win, .. } => Dest::Win(*win),
                Msg::Instance(_) if r.active != 0 && r.attached(r.active) => Dest::Win(r.active),
                Msg::Instance(_) => Dest::Primary,
                Msg::Updated { .. } | Msg::Lang => {
                    let wins: Vec<u32> = r.wins.iter().map(|w| w.id).collect();
                    if wins.is_empty() {
                        r.queue(Dest::Primary, m);
                    } else {
                        for w in wins {
                            let copy = match &m {
                                Msg::Updated { version } => Msg::Updated { version: *version },
                                _ => Msg::Lang,
                            };
                            r.send(w, copy);
                        }
                    }
                    drop(r);
                    self.flush();
                    return;
                }
                Msg::Fwd(_) | Msg::FirstFrame => return,
            };
            r.deliver(d, m);
        }
        self.flush();
    }

    /// False (dropped) when win is not attached, or when its poster refused the message.
    pub fn post_to(&self, win: u32, m: Msg) -> bool {
        let ack = Arc::new(AtomicU8::new(0));
        if !lock(&self.route).enqueue(win, m, Some(Arc::clone(&ack))) {
            return false;
        }
        self.flush();
        // Still queued behind another sender's post counts as taken: its failure drops the window.
        ack.load(Ordering::Acquire) != ACK_FAILED
    }

    /// EnginesWake, at most one in flight until wake_done(). A primary that closes, or whose post fails, before it
    /// handled the wake passes it on to the next primary.
    pub fn wake(&self) {
        if !self.wake_in_flight.swap(true, Ordering::AcqRel) {
            self.post(Msg::EnginesWake);
        }
    }

    pub fn wake_done(&self) {
        self.wake_in_flight.store(false, Ordering::Release);
    }

    /// Calls wake(): for OpsClient, ShellClient, Watcher and ClassIcons.
    pub fn notifier(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let hub = Arc::clone(self);
        Arc::new(move || hub.wake())
    }

    /// post(Batch); the first batch of window::STARTUP_TAB fills the start-up slot instead while no window is attached.
    pub fn sink(self: &Arc<Self>) -> BatchSink {
        let hub = Arc::clone(self);
        Arc::new(move |tab: TabId, batch: Batch| hub.on_batch(tab, batch))
    }

    fn on_batch(&self, tab: TabId, batch: Batch) {
        let batch = if tab == STARTUP_TAB {
            let mut s = lock(&self.first);
            if !s.closed && s.batch.is_none() {
                s.batch = Some(batch);
                self.first_ready.notify_all();
                return;
            }
            batch
        } else {
            batch
        };
        self.post(Msg::Batch { tab, batch });
    }

    /// Start-up only, before the first window exists: the first batch of STARTUP_TAB if it arrives by `deadline`.
    /// Closes the slot either way, so later batches travel as messages.
    pub fn wait_first_batch(&self, deadline: Instant) -> Option<Batch> {
        let mut s = lock(&self.first);
        while s.batch.is_none() && !s.closed {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            s = match self.first_ready.wait_timeout(s, deadline - now) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        s.closed = true;
        s.batch.take()
    }
}
