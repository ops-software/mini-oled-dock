//! Class icons per (ext, px) over icons.bin and a source, the decor thread or the shell host (IC3, IC4).
//!
//! get answers from memory, then from the mapped icons.bin, else queues the extension and answers Pending (Generic
//! after a failure). The source is the decor thread (a thread with COM, created on the first miss) or the shell host
//! (a ClassIcons request through the ShellClient, one in flight at a time, the misses that arrive meanwhile going out
//! together). Results enter memory; icons.bin is written only by a rebuild (rebuild_if_due, assoc_changed).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use oled_trust::frame::files::{CallKind, ReqId, ShellEvt, ShellReq};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

use crate::decor::extract;
use crate::decor::iconsbin::{self, IconsBin, Item, MAX_EXTS, MAX_PX};
use crate::own_files::OwnDir;
use crate::shell_client::{ShellClient, ShellReply};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassIconSource {
    InProcess,
    ShellHost,
}

/// Premultiplied BGRA, px * px * 4 bytes.
pub enum IconRef {
    Ready(Arc<[u8]>),
    Pending,
    Generic,
}

/// The extension of a class icon: at most 64 bytes of UTF-8, no dot, separator or control character.
const MAX_EXT_BYTES: usize = 64;
/// Extensions of one ClassIcons request.
const BATCH: usize = 32;
/// Distinct extensions kept per size in memory, and counted by note.
const MAX_KEPT: usize = 4096;
/// A week in FILETIME ticks (100 ns).
pub const WEEK: i64 = 7 * 24 * 3600 * 10_000_000;

/// True when an icons.bin built at built (FILETIME, 0 = none) is due for a rebuild at now.
pub fn due(built: i64, now: i64) -> bool {
    built == 0 || now.saturating_sub(built) >= WEEK
}

type Notify = Arc<dyn Fn() + Send + Sync>;
/// ext and px to premultiplied BGRA (px * px * 4 bytes), or None.
pub type Extractor = Arc<dyn Fn(&str, u16) -> Option<Vec<u8>> + Send + Sync>;
/// The time as a FILETIME.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
type Key = (Box<str>, u16);

enum Slot {
    Ready(Arc<[u8]>),
    Failed,
}

/// A request in flight at the shell host.
struct Flight {
    req: ReqId,
    px: u16,
    epoch: u64,
    exts: Vec<Box<str>>,
    got: HashSet<Box<str>>,
}

/// A rebuild under way: every (ext, px) it waits for, what arrived, the hits to write.
struct Rebuild {
    pending: HashSet<Key>,
    items: Vec<Item>,
    hits: HashMap<Box<str>, u32>,
}

enum Backend {
    /// The decor thread (created on the first miss) running the extractor.
    Thread(Extractor),
    /// The shell host; None when there is no client (every miss fails).
    Host(Option<Arc<ShellClient>>),
}

struct State {
    bin: Option<IconsBin>,
    /// After assoc_changed the mapped file is out of date: it is not read again this run.
    stale: bool,
    mem: HashMap<u16, HashMap<Box<[u8]>, Slot>>,
    /// Asked for and not yet answered (the epoch it was asked in is not kept: an old answer is dropped on arrival).
    queued: HashSet<Key>,
    /// The host path's misses not yet sent.
    want: VecDeque<Key>,
    flight: Option<Flight>,
    tx: Option<Sender<(Box<str>, u16, u64)>>,
    /// Bumped by assoc_changed: answers of an older epoch are dropped.
    epoch: u64,
    hits: HashMap<Box<[u8]>, u32>,
    /// The sizes asked for, the newest last, at most 2 distinct.
    sizes: Vec<u16>,
    rebuild: Option<Rebuild>,
    /// FILETIME of the last rebuild finished in this run.
    rebuilt: i64,
}

struct Inner {
    data_dir: PathBuf,
    notify: Notify,
    now: Clock,
    backend: Backend,
    st: Mutex<State>,
}

/// A cache per (ext, px) over IconsBin and the source.
pub struct ClassIcons {
    inner: Arc<Inner>,
}

fn file_time_now() -> i64 {
    // SAFETY: no preconditions; returns the current time.
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime)
}

fn valid_ext(ext: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(ext).ok()?;
    let bad = |c: char| matches!(c, '.' | '/' | '\\' | ':') || c.is_control();
    (ext.len() <= MAX_EXT_BYTES && !s.chars().any(bad)).then_some(s)
}

impl ClassIcons {
    pub fn new(source: ClassIconSource, data_dir: PathBuf, shell: Option<Arc<ShellClient>>, notify: Notify) -> Self {
        let backend = match source {
            ClassIconSource::InProcess => Backend::Thread(Arc::new(extract::class_icon)),
            ClassIconSource::ShellHost => Backend::Host(shell),
        };
        Self::with_backend(backend, data_dir, notify, Arc::new(file_time_now))
    }

    /// The in-process path with another extractor and another clock (tests).
    #[doc(hidden)]
    pub fn with_extractor(data_dir: PathBuf, extract: Extractor, notify: Notify, now: Clock) -> Self {
        Self::with_backend(Backend::Thread(extract), data_dir, notify, now)
    }

    fn with_backend(backend: Backend, data_dir: PathBuf, notify: Notify, now: Clock) -> Self {
        let bin = IconsBin::open(&data_dir);
        let st = State {
            bin,
            stale: false,
            mem: HashMap::new(),
            queued: HashSet::new(),
            want: VecDeque::new(),
            flight: None,
            tx: None,
            epoch: 0,
            hits: HashMap::new(),
            sizes: Vec::new(),
            rebuild: None,
            rebuilt: 0,
        };
        ClassIcons { inner: Arc::new(Inner { data_dir, notify, now, backend, st: Mutex::new(st) }) }
    }

    /// Never blocks.
    pub fn get(&self, ext: &[u8], px: u16) -> IconRef {
        let lower;
        let ext = if ext.iter().any(u8::is_ascii_uppercase) {
            lower = ext.to_ascii_lowercase();
            &lower[..]
        } else {
            ext
        };
        let Some(name) = valid_ext(ext) else { return IconRef::Generic };
        if px == 0 || px > MAX_PX {
            return IconRef::Generic;
        }
        let mut st = self.inner.lock();
        if st.sizes.last() != Some(&px) {
            st.sizes.retain(|&s| s != px);
            st.sizes.push(px);
            if st.sizes.len() > 2 {
                st.sizes.remove(0);
            }
        }
        match st.mem.get(&px).and_then(|m| m.get(ext)) {
            Some(Slot::Ready(a)) => return IconRef::Ready(Arc::clone(a)),
            Some(Slot::Failed) => return IconRef::Generic,
            None => {}
        }
        if !st.stale {
            let from_bin = st.bin.as_ref().and_then(|b| b.get(ext, px)).map(Arc::<[u8]>::from);
            if let Some(a) = from_bin {
                st.insert(px, ext, Slot::Ready(Arc::clone(&a)));
                return IconRef::Ready(a);
            }
        }
        if let Backend::Host(Some(c)) = &self.inner.backend {
            if c.disabled(CallKind::ClassIcon, name) {
                st.insert(px, ext, Slot::Failed);
                return IconRef::Generic;
            }
        }
        let key: Key = (name.into(), px);
        if st.queued.insert(key.clone()) {
            self.inner.dispatch(&mut st, key);
        }
        IconRef::Pending
    }

    /// A painted row: counts toward the top 256.
    pub fn note(&self, ext: &[u8]) {
        let ext = ext.to_ascii_lowercase();
        let mut st = self.inner.lock();
        if let Some(n) = st.hits.get_mut(&ext[..]) {
            *n = n.saturating_add(1);
        } else if st.hits.len() < MAX_KEPT && valid_ext(&ext).is_some() {
            st.hits.insert(ext.into(), 1);
        }
    }

    /// True if it was a ClassIcon of ours.
    pub fn on_shell_reply(&self, r: &ShellReply) -> bool {
        if !matches!(self.inner.backend, Backend::Host(_)) {
            return false;
        }
        let mut st = self.inner.lock();
        let Some(f) = st.flight.as_ref() else { return false };
        let (flight_req, flight_px, epoch) = (f.req, f.px, f.epoch);
        match r {
            ShellReply::Evt(ShellEvt::ClassIcon { req, ext, px }) if *req == flight_req => {
                let sized =
                    px.w == flight_px && px.h == flight_px && px.bgra.len() == usize::from(flight_px).pow(2) * 4;
                let known = st.flight.as_ref().is_some_and(|f| f.exts.contains(ext));
                if sized && known {
                    if let Some(f) = st.flight.as_mut() {
                        f.got.insert(ext.clone());
                    }
                    self.inner.deliver(&mut st, ext, flight_px, epoch, Some(px.bgra.to_vec()));
                }
                true
            }
            ShellReply::Evt(ShellEvt::ReqDone { req } | ShellEvt::Failed { req, .. })
            | ShellReply::Fallback { req, .. }
                if *req == flight_req =>
            {
                if let Some(f) = st.flight.take() {
                    for ext in f.exts.iter().filter(|e| !f.got.contains(*e)) {
                        self.inner.deliver(&mut st, ext, f.px, f.epoch, None);
                    }
                }
                self.inner.pump(&mut st);
                true
            }
            _ => false,
        }
    }

    /// The file associations changed: memory and the mapped file are out of date; the source answers again, and a
    /// rebuild writes a new file.
    pub fn assoc_changed(&self) {
        {
            let mut st = self.inner.lock();
            st.mem.clear();
            st.queued.clear();
            st.want.clear();
            st.rebuild = None;
            st.stale = true;
            st.epoch += 1;
            self.inner.start_rebuild(&mut st);
        }
        (self.inner.notify)();
    }

    /// Weekly; call after the first frame.
    pub fn rebuild_if_due(&self) {
        let mut st = self.inner.lock();
        if st.rebuild.is_some() {
            return;
        }
        let built = st.bin.as_ref().map_or(0, IconsBin::built).max(st.rebuilt);
        let has_data = st.bin.is_some() || !st.hits.is_empty() || st.mem.values().any(|m| !m.is_empty());
        if has_data && due(built, (self.inner.now)()) {
            self.inner.start_rebuild(&mut st);
        }
    }
}

impl State {
    fn insert(&mut self, px: u16, ext: &[u8], slot: Slot) {
        let m = self.mem.entry(px).or_default();
        if m.len() >= MAX_KEPT {
            m.clear();
        }
        m.insert(ext.into(), slot);
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends one miss to its source.
    fn dispatch(self: &Arc<Self>, st: &mut State, key: Key) {
        match &self.backend {
            Backend::Thread(extract) => {
                if st.tx.is_none() {
                    let (tx, rx) = channel();
                    let spawned = std::thread::Builder::new().name("decor".into()).spawn({
                        let (weak, extract) = (Arc::downgrade(self), Arc::clone(extract));
                        move || worker(weak, extract, rx)
                    });
                    if spawned.is_ok() {
                        st.tx = Some(tx);
                    }
                }
                let sent = st.tx.as_ref().is_some_and(|tx| tx.send((key.0.clone(), key.1, st.epoch)).is_ok());
                if !sent {
                    let epoch = st.epoch;
                    self.deliver(st, &key.0, key.1, epoch, None);
                }
            }
            Backend::Host(_) => {
                st.want.push_back(key);
                self.pump(st);
            }
        }
    }

    /// The host path: when nothing is in flight, sends the misses waiting, up to BATCH of the first one's size.
    fn pump(&self, st: &mut State) {
        if st.flight.is_some() {
            return;
        }
        let Some((_, px)) = st.want.front().cloned() else { return };
        let Backend::Host(client) = &self.backend else { return };
        let mut exts: Vec<Box<str>> = Vec::new();
        let mut rest = VecDeque::new();
        while let Some(k) = st.want.pop_front() {
            if k.1 == px && exts.len() < BATCH {
                exts.push(k.0);
            } else {
                rest.push_back(k);
            }
        }
        st.want = rest;
        let epoch = st.epoch;
        let Some(c) = client else {
            for e in &exts {
                self.deliver(st, e, px, epoch, None);
            }
            return;
        };
        let req = c.next_req();
        st.flight = Some(Flight { req, px, epoch, exts: exts.clone(), got: HashSet::new() });
        c.request(ShellReq::ClassIcons { req, exts, px });
    }

    /// An answer (None: the source failed). Returns true when memory changed.
    fn deliver(&self, st: &mut State, ext: &str, px: u16, epoch: u64, bytes: Option<Vec<u8>>) -> bool {
        if epoch != st.epoch {
            return false;
        }
        st.queued.remove(&(ext.into(), px));
        let ready = bytes.filter(|b| b.len() == usize::from(px) * usize::from(px) * 4);
        let kept = st.mem.get(&px).and_then(|m| m.get(ext.as_bytes())).is_some_and(|s| matches!(s, Slot::Ready(_)));
        match (&ready, kept) {
            (Some(b), _) => st.insert(px, ext.as_bytes(), Slot::Ready(Arc::from(b.as_slice()))),
            (None, false) => st.insert(px, ext.as_bytes(), Slot::Failed),
            (None, true) => {}
        }
        if let Some(rb) = st.rebuild.as_mut() {
            if rb.pending.remove(&(ext.into(), px)) {
                if let Some(bgra) = ready {
                    let hits = rb.hits.get(ext).copied().unwrap_or(0);
                    rb.items.push(Item { ext: ext.to_string(), px, bgra, hits });
                }
                if rb.pending.is_empty() {
                    self.finish_rebuild(st);
                }
            }
        }
        true
    }

    /// The candidates of a rebuild with their hits: the file's (halved, so old use fades), the counted ones and the
    /// ones in memory; the top MAX_EXTS by hits, then by name.
    fn start_rebuild(self: &Arc<Self>, st: &mut State) {
        if st.rebuild.is_some() {
            return;
        }
        let sizes: Vec<u16> = if st.sizes.is_empty() {
            st.bin.as_ref().map(|b| b.sizes().iter().rev().take(2).copied().collect()).unwrap_or_default()
        } else {
            st.sizes.clone()
        };
        let mut hits: HashMap<Box<str>, u32> = HashMap::new();
        if let Some(b) = st.bin.as_ref() {
            for (ext, _, h) in b.list() {
                let e = hits.entry(ext.into()).or_insert(0);
                *e = (*e).max(h / 2);
            }
        }
        for (ext, n) in &st.hits {
            if let Some(s) = valid_ext(ext) {
                let e = hits.entry(s.into()).or_insert(0);
                *e = e.saturating_add(*n);
            }
        }
        for m in st.mem.values() {
            for (ext, slot) in m {
                if let (Some(s), Slot::Ready(_)) = (valid_ext(ext), slot) {
                    hits.entry(s.into()).or_insert(0);
                }
            }
        }
        let mut ranked: Vec<(Box<str>, u32)> = hits.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(MAX_EXTS);
        if sizes.is_empty() || ranked.is_empty() {
            return;
        }
        let mut pending = HashSet::new();
        for (e, _) in &ranked {
            for &px in &sizes {
                pending.insert((e.clone(), px));
            }
        }
        let keys: Vec<Key> = pending.iter().cloned().collect();
        st.rebuild = Some(Rebuild { pending, items: Vec::new(), hits: ranked.into_iter().collect() });
        for key in keys {
            if st.queued.insert(key.clone()) {
                self.dispatch(st, key);
            }
        }
    }

    /// Writes the new file from what arrived, on a short thread (the write is flushed to disk).
    fn finish_rebuild(&self, st: &mut State) {
        let Some(rb) = st.rebuild.take() else { return };
        let built = (self.now)();
        st.rebuilt = built;
        if rb.items.is_empty() {
            return;
        }
        let dir = self.data_dir.clone();
        let _ = std::thread::Builder::new().name("decor-write".into()).spawn(move || {
            let bytes = iconsbin::build(rb.items, built);
            let written = OwnDir::open(&dir).and_then(|d| iconsbin::write_next(&d, &bytes));
            if let Err(e) = written {
                oled_sys::log!("icons: cannot write icons.bin ({e})");
            }
        });
    }
}

/// The decor thread: one extraction at a time on an STA, each answer delivered and the UI woken.
fn worker(inner: Weak<Inner>, extract: Extractor, rx: Receiver<(Box<str>, u16, u64)>) {
    // SAFETY: one init on this thread, paired below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    while let Ok((ext, px, epoch)) = rx.recv() {
        let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract(&ext, px))).ok().flatten();
        let Some(inner) = inner.upgrade() else { break };
        let changed = {
            let mut st = inner.lock();
            inner.deliver(&mut st, &ext, px, epoch, bytes)
        };
        if changed {
            (inner.notify)();
        }
    }
    if com {
        // SAFETY: pairs the init above.
        unsafe { CoUninitialize() };
    }
}

/// FS-6 sets it (live check LIVE-F1): until then class icons come from the shell host and icons.bin is read in the UI.
pub const CLASS_ICONS_IN_UI: bool = false;
