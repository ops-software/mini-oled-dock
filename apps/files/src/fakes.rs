//! Fakes of every service (P3-files-app 2.2): scripted behaviour and a record of every call, so Harness tests, scenes
//! and benches drive a FilesScreen with no host, no thread of the engine and no real settings folder. The binary uses
//! them only for --preview's scenes.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use oled_trust::frame::files::{CallKind, ItemRef, OpId, ReqId, ShellReq};
use oled_ui::harness::Harness;
use oled_ui::{Screen, UiLang};

use crate::bg::{JobFn, Lane};
use crate::decor::cache::IconRef;
use crate::format::LocalTime;
use crate::hub::{Collect, Hub};
use crate::listing::{
    natural_cmp, Anchor, ApplyStats, Batch, BatchSink, CancelToken, Diff, Entry, EntryId, EntryStore, EnumError,
    EnumJob, Gen, GroupBy, IdMap, RawEntry, SortDir, SortDone, SortJob, SortKey, TabId, Visibility, WidePath,
};
use crate::model::Listing;
use crate::msg::Msg;
use crate::ops_client::{OpsError, OpsEvent, UndoRecord};
use crate::options::FilesOptions;
use crate::services::{
    BgService, Clock, EnumService, IconService, OpsService, OptionsService, OwnFiles, Services, ShellService,
    WatchService,
};
use crate::session::Session;
use crate::shell_client::{HostState, ShellReply};
use crate::types::TypeNames;
use crate::watch::{WatchError, WatchEvent, WatchId};
use crate::window::Shared;

/// Rounds of pump(): a screen that answers a message with another one settles well within it.
const PUMP_ROUNDS: usize = 64;

/// FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM.
const ATTR_DIR: u32 = 0x10;
const ATTR_ARCHIVE: u32 = 0x20;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;

/// 2026-11-21 00:00 UTC as a FILETIME.
pub const FT_2026_11_21: i64 = 134_396_928_000_000_000;

pub struct Fakes {
    pub enumr: Rc<FakeEnum>,
    pub ops: Rc<FakeOps>,
    pub shell: Rc<FakeShell>,
    pub watch: Rc<FakeWatch>,
    pub icons: Rc<FakeIcons>,
    pub bg: Rc<FakeBg>,
    pub clock: Rc<FakeClock>,
    pub own: Rc<FakeOwn>,
    pub options: Rc<FakeOptions>,
    pub hub: Arc<Hub>,
    /// Window 1's messages.
    pub posted: Arc<Collect>,
    windows: RefCell<Vec<(u32, Arc<Collect>)>>,
}

impl Default for Fakes {
    fn default() -> Fakes {
        Fakes::new()
    }
}

impl Fakes {
    /// A Hub with a Collect attached as window 1 (more windows: attach_window).
    pub fn new() -> Fakes {
        let hub = Hub::new();
        let posted = Arc::new(Collect::new());
        hub.attach(1, Box::new(Arc::clone(&posted)));
        Fakes {
            enumr: Rc::new(FakeEnum::new(hub.sink())),
            ops: Rc::new(FakeOps::default()),
            shell: Rc::new(FakeShell::default()),
            watch: Rc::new(FakeWatch::default()),
            icons: Rc::new(FakeIcons::default()),
            bg: Rc::new(FakeBg::default()),
            clock: Rc::new(FakeClock::default()),
            own: Rc::new(FakeOwn::default()),
            options: Rc::new(FakeOptions::default()),
            windows: RefCell::new(vec![(1, Arc::clone(&posted))]),
            hub,
            posted,
        }
    }

    pub fn services(&self) -> Services {
        Services {
            enumr: self.enumr.clone(),
            ops: self.ops.clone(),
            shell: self.shell.clone(),
            watch: self.watch.clone(),
            icons: self.icons.clone(),
            bg: self.bg.clone(),
            clock: self.clock.clone(),
            own: self.own.clone(),
            options: self.options.clone(),
        }
    }

    /// Shared::new over services(), an empty session and no type names.
    pub fn shared(&self, lang: UiLang) -> Rc<Shared> {
        Shared::new(Arc::clone(&self.hub), self.services(), lang, Session::default(), TypeNames::empty())
    }

    /// Another window's Collect, attached to the hub.
    pub fn attach_window(&self, win: u32) -> Arc<Collect> {
        let c = Arc::new(Collect::new());
        self.hub.attach(win, Box::new(Arc::clone(&c)));
        self.windows.borrow_mut().push((win, Arc::clone(&c)));
        c
    }

    /// The Collect of a window attached through new() or attach_window().
    pub fn collect(&self, win: u32) -> Option<Arc<Collect>> {
        self.windows.borrow().iter().find(|(w, _)| *w == win).map(|(_, c)| Arc::clone(c))
    }

    /// Runs FakeBg jobs, delivers queued batches and engine output, then feeds every Msg posted to `win` into
    /// h.send, until nothing more arrives. Returns how many messages were fed.
    pub fn pump<S: Screen>(&self, h: &mut Harness<S>, win: u32) -> usize {
        let mut sent = 0;
        for _ in 0..PUMP_ROUNDS {
            self.deliver();
            let msgs = self.collect(win).map(|c| c.take()).unwrap_or_default();
            if msgs.is_empty() {
                break;
            }
            for m in msgs {
                h.send(Box::new(m));
                sent += 1;
            }
        }
        sent
    }

    /// Everything the fakes hold back until a pump: jobs, batches and engine output (the engines then wake the hub, as
    /// the real clients' notifiers do).
    pub fn deliver(&self) {
        for (win, job) in self.bg.take_jobs() {
            if let Some(done) = job() {
                self.hub.post(Msg::Job { win, done });
            }
        }
        self.enumr.deliver();
        let shell = self.shell.release();
        let ops = self.ops.release();
        let watch = self.watch.release();
        if shell || ops || watch {
            self.hub.wake();
        }
    }
}

// ---- listings ---------------------------------------------------------------------------------------------------

/// One done batch: (name, folder, size, mtime FILETIME) per entry, in the order given.
pub fn listing(names: &[(&str, bool, u64, i64)]) -> Batch {
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    for &(name, dir, size, mtime) in names {
        push_raw(&mut b, name.as_bytes(), dir, if dir { 0 } else { size }, mtime);
    }
    b
}

fn push_raw(b: &mut Batch, name: &[u8], dir: bool, size: u64, mtime: i64) {
    let name_off = b.names.len() as u32;
    b.names.extend_from_slice(name);
    let attrs = if dir { ATTR_DIR } else { ATTR_ARCHIVE };
    let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    b.entries.push(RawEntry { name_off, name_len, attrs, reparse_tag: 0, size, mtime, ctime: mtime });
}

/// `n` synthetic entries for scenes and benches: made-up names of 24 bytes on average, 10 % folders, sizes and dates
/// from `seed`.
pub fn listing_n(n: u32, seed: u64) -> Batch {
    const WORDS: [&str; 12] = [
        "report", "notes", "draft", "invoice", "photo", "budget", "slides", "backup", "scan", "letter", "plan", "data",
    ];
    const EXTS: [&str; 8] = ["pdf", "txt", "docx", "png", "zip", "xlsx", "md", "jpg"];
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    let mut s = seed | 1;
    let mut next = move || {
        // xorshift64*
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for i in 0..n {
        let r = next();
        let dir = r % 10 == 0;
        let word = WORDS[(r >> 8) as usize % WORDS.len()];
        let other = WORDS[(r >> 16) as usize % WORDS.len()];
        let name = if dir {
            format!("{word}-{other}-{i:06}")
        } else {
            format!("{word}-{other}-{i:06}.{}", EXTS[(r >> 24) as usize % EXTS.len()])
        };
        let size = (next() % (64 << 20)) + 1;
        let age = (next() % (365 * 86_400)) as i64 * 10_000_000;
        push_raw(&mut b, name.as_bytes(), dir, size, FT_2026_11_21 - age);
    }
    b
}

struct Slot {
    name: Box<[u8]>,
    ext: Box<[u8]>,
    e: Entry,
}

/// A simple, total model of EntryStore over owned entries: natural order, folders first, ids kept across changes;
/// merge_fresh by name.
pub struct FakeListing {
    dir: WidePath,
    gen: Gen,
    done: bool,
    error: Option<EnumError>,
    slots: Vec<Option<Slot>>,
    pending: Vec<EntryId>,
    order: Vec<EntryId>,
    key: SortKey,
    sdir: SortDir,
    vis: Visibility,
    dead: u32,
    asked: Cell<Option<(SortKey, SortDir)>>,
}

/// The lower-case extension of a file name ("" for none or a leading dot only).
fn ext_of(name: &[u8], dir: bool) -> Box<[u8]> {
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if !dir && i > 0 && i + 1 < name.len() => {
            name.get(i + 1..).unwrap_or_default().to_ascii_lowercase().into()
        }
        _ => Box::default(),
    }
}

impl FakeListing {
    pub fn new(dir: WidePath, gen: Gen) -> FakeListing {
        FakeListing {
            dir,
            gen,
            done: false,
            error: None,
            slots: Vec::new(),
            pending: Vec::new(),
            order: Vec::new(),
            key: SortKey::Name,
            sdir: SortDir::Asc,
            vis: Visibility::default(),
            dead: 0,
            asked: Cell::new(None),
        }
    }

    fn slot(&self, id: EntryId) -> Option<&Slot> {
        self.slots.get(id.0 as usize).and_then(Option::as_ref)
    }

    fn push(&mut self, name: &[u8], raw: &RawEntry) -> EntryId {
        let id = EntryId(self.slots.len() as u32);
        let dir = raw.attrs & ATTR_DIR != 0;
        let e = Entry {
            name_off: 0,
            name_len: raw.name_len,
            ext: 0,
            attrs: raw.attrs,
            reparse_tag: raw.reparse_tag,
            size: raw.size,
            mtime: raw.mtime,
            ctime: raw.ctime,
        };
        self.slots.push(Some(Slot { name: name.into(), ext: ext_of(name, dir), e }));
        id
    }

    fn live(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.slots.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| EntryId(i as u32))
    }

    fn visible(&self, s: &Slot) -> bool {
        let hidden = s.e.attrs & ATTR_HIDDEN != 0;
        let system = hidden && s.e.attrs & ATTR_SYSTEM != 0;
        (!hidden || self.vis.hidden) && (!system || self.vis.protected_system)
    }

    fn cmp(&self, a: EntryId, b: EntryId) -> Ordering {
        let (Some(x), Some(y)) = (self.slot(a), self.slot(b)) else { return a.cmp(&b) };
        let (dx, dy) = (x.e.attrs & ATTR_DIR != 0, y.e.attrs & ATTR_DIR != 0);
        if dx != dy {
            return dy.cmp(&dx);
        }
        let by_key = match self.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Modified => x.e.mtime.cmp(&y.e.mtime),
            SortKey::Type => x.ext.cmp(&y.ext),
            SortKey::Size => x.e.size.cmp(&y.e.size),
            SortKey::Created => x.e.ctime.cmp(&y.e.ctime),
        };
        let o = by_key.then_with(|| natural_cmp(&x.name, &y.name)).then_with(|| a.cmp(&b));
        if self.sdir == SortDir::Desc {
            o.reverse()
        } else {
            o
        }
    }

    fn rebuild(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let mut order: Vec<EntryId> =
            self.live().filter(|id| self.slot(*id).is_some_and(|s| self.visible(s)) && !pending.contains(id)).collect();
        order.sort_by(|a, b| self.cmp(*a, *b));
        self.order = order;
        self.pending = pending;
    }

    fn kill(&mut self, id: EntryId) {
        if let Some(s) = self.slots.get_mut(id.0 as usize) {
            if s.take().is_some() {
                self.dead += 1;
            }
        }
    }
}

#[allow(clippy::len_without_is_empty)] // the frozen interface has len() only
impl Listing for FakeListing {
    fn gen(&self) -> Gen {
        self.gen
    }
    fn done(&self) -> bool {
        self.done
    }
    fn error(&self) -> Option<EnumError> {
        self.error
    }
    fn dir(&self) -> &WidePath {
        &self.dir
    }
    fn order(&self) -> &[EntryId] {
        &self.order
    }
    fn row_of(&self, id: EntryId) -> Option<u32> {
        self.order.iter().position(|x| *x == id).map(|r| r as u32)
    }
    fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.slot(id).map(|s| &s.e)
    }
    fn name(&self, id: EntryId) -> Option<&[u8]> {
        self.slot(id).map(|s| &*s.name)
    }
    fn ext(&self, id: EntryId) -> Option<&[u8]> {
        self.slot(id).map(|s| &*s.ext).filter(|e| !e.is_empty())
    }
    fn is_dir(&self, id: EntryId) -> bool {
        self.slot(id).is_some_and(|s| s.e.attrs & ATTR_DIR != 0)
    }
    fn find(&self, name: &[u8]) -> Option<EntryId> {
        self.live().find(|id| self.slot(*id).is_some_and(|s| s.name.eq_ignore_ascii_case(name)))
    }
    fn len(&self) -> u32 {
        self.order.len() as u32
    }
    fn heap_bytes(&self) -> usize {
        let names: usize = self.slots.iter().flatten().map(|s| s.name.len() + s.ext.len()).sum();
        names + self.slots.len() * std::mem::size_of::<Option<Slot>>() + self.order.len() * 4
    }
    fn append(&mut self, b: Batch) {
        for raw in &b.entries {
            let start = raw.name_off as usize;
            let name = b.names.get(start..start + usize::from(raw.name_len)).unwrap_or_default().to_vec();
            let id = self.push(&name, raw);
            self.pending.push(id);
        }
        if b.done {
            self.done = true;
            self.error = b.error;
        }
    }
    fn merge_pending(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        self.pending.clear();
        self.rebuild();
        true
    }
    fn apply(&mut self, d: &Diff, anchor: Option<Anchor>) -> (ApplyStats, Option<Anchor>) {
        let mut st = ApplyStats::default();
        let name_at =
            |off: u32, len: u16| d.names.get(off as usize..off as usize + usize::from(len)).unwrap_or_default();
        for &(off, len) in &d.removes {
            if let Some(id) = self.find(name_at(off, len)) {
                self.kill(id);
                st.removed += 1;
            }
        }
        for raw in &d.upserts {
            let name = name_at(raw.name_off, raw.name_len).to_vec();
            match self.find(&name) {
                Some(id) => {
                    if let Some(Some(s)) = self.slots.get_mut(id.0 as usize) {
                        s.e = Entry { attrs: raw.attrs, size: raw.size, mtime: raw.mtime, ctime: raw.ctime, ..s.e };
                    }
                    st.changed += 1;
                }
                None => {
                    self.push(&name, raw);
                    st.added += 1;
                }
            }
        }
        self.rebuild();
        let anchor = anchor.filter(|a| self.slot(a.id).is_some());
        (st, anchor)
    }
    fn sort(&mut self, key: SortKey, dir: SortDir) {
        self.key = key;
        self.sdir = dir;
        self.rebuild();
    }
    /// The job is an empty one of the engine: adopt() applies the sort asked for here.
    fn sort_job(&self, key: SortKey, dir: SortDir) -> SortJob {
        self.asked.set(Some((key, dir)));
        EntryStore::new(self.dir.clone(), self.gen).sort_job(key, dir, GroupBy::None)
    }
    fn adopt(&mut self, done: SortDone) {
        drop(done);
        let (key, dir) = self.asked.take().unwrap_or((self.key, self.sdir));
        self.sort(key, dir);
    }
    fn set_visibility(&mut self, v: Visibility) {
        self.vis = v;
        self.rebuild();
    }
    fn needs_compact(&self) -> bool {
        self.dead > 64 && self.dead as usize > self.order.len()
    }
    fn compact(&mut self) -> IdMap {
        let mut map = Vec::with_capacity(self.slots.len());
        let mut slots = Vec::with_capacity(self.slots.len() - self.dead as usize);
        for s in std::mem::take(&mut self.slots) {
            match s {
                Some(s) => {
                    map.push(Some(EntryId(slots.len() as u32)));
                    slots.push(Some(s));
                }
                None => map.push(None),
            }
        }
        self.slots = slots;
        self.dead = 0;
        self.pending = self.pending.iter().filter_map(|id| map.get(id.0 as usize).copied().flatten()).collect();
        self.rebuild();
        IdMap(map)
    }
    fn merge_fresh(&mut self, fresh: Box<dyn Listing>) -> ApplyStats {
        let mut st = ApplyStats::default();
        let mut seen: Vec<EntryId> = Vec::new();
        for &fid in fresh.order() {
            let (Some(name), Some(e)) = (fresh.name(fid), fresh.entry(fid)) else { continue };
            let raw = RawEntry {
                name_off: 0,
                name_len: e.name_len,
                attrs: e.attrs,
                reparse_tag: e.reparse_tag,
                size: e.size,
                mtime: e.mtime,
                ctime: e.ctime,
            };
            match self.find(name) {
                Some(id) => {
                    if let Some(Some(s)) = self.slots.get_mut(id.0 as usize) {
                        if s.e.size != e.size || s.e.mtime != e.mtime || s.e.attrs != e.attrs {
                            st.changed += 1;
                        }
                        s.e = Entry { attrs: e.attrs, size: e.size, mtime: e.mtime, ctime: e.ctime, ..s.e };
                    }
                    seen.push(id);
                }
                None => {
                    let id = self.push(name, &raw);
                    seen.push(id);
                    st.added += 1;
                }
            }
        }
        let gone: Vec<EntryId> = self.live().filter(|id| !seen.contains(id)).collect();
        for id in gone {
            self.kill(id);
            st.removed += 1;
        }
        self.done = fresh.done();
        self.error = fresh.error();
        self.pending.clear();
        self.rebuild();
        st
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

// ---- the services -----------------------------------------------------------------------------------------------

/// A submitted enumeration, as FakeEnum recorded it.
#[derive(Clone, Debug)]
pub struct JobRec {
    pub tab: TabId,
    pub gen: Gen,
    pub dir: WidePath,
    pub cancel: CancelToken,
}

/// Records every job; on the next pump delivers the batches (or the error) the test queued for its folder, with the
/// job's generation, through the hub's sink.
pub struct FakeEnum {
    sink: BatchSink,
    jobs: RefCell<Vec<JobRec>>,
    open: RefCell<Vec<JobRec>>,
    queued: RefCell<HashMap<WidePath, VecDeque<Result<Batch, EnumError>>>>,
}

impl FakeEnum {
    pub fn new(sink: BatchSink) -> FakeEnum {
        FakeEnum { sink, jobs: RefCell::new(Vec::new()), open: RefCell::new(Vec::new()), queued: RefCell::default() }
    }

    /// A batch the next job for `dir` delivers (its generation is the job's).
    pub fn queue(&self, dir: &WidePath, b: Batch) {
        self.queued.borrow_mut().entry(dir.clone()).or_default().push_back(Ok(b));
    }

    /// The next job for `dir` fails with `e` (a done batch carrying the error).
    pub fn queue_error(&self, dir: &WidePath, e: EnumError) {
        self.queued.borrow_mut().entry(dir.clone()).or_default().push_back(Err(e));
    }

    pub fn jobs(&self) -> Vec<JobRec> {
        self.jobs.borrow().clone()
    }

    fn deliver(&self) {
        let open = std::mem::take(&mut *self.open.borrow_mut());
        let mut keep = Vec::new();
        for job in open {
            if job.cancel.is_cancelled() {
                continue;
            }
            let items: Vec<_> =
                self.queued.borrow_mut().get_mut(&job.dir).map(|q| q.drain(..).collect()).unwrap_or_default();
            let mut finished = false;
            for item in items {
                let b = match item {
                    Ok(b) => Batch { gen: job.gen, ..b },
                    Err(e) => Batch {
                        gen: job.gen,
                        names: Vec::new(),
                        entries: Vec::new(),
                        done: true,
                        error: Some(e),
                        elapsed_us: 0,
                    },
                };
                finished |= b.done;
                (self.sink)(job.tab, b);
            }
            if !finished {
                keep.push(job);
            }
        }
        self.open.borrow_mut().extend(keep);
    }
}

impl EnumService for FakeEnum {
    fn submit(&self, job: EnumJob) {
        let rec = JobRec { tab: job.tab, gen: job.gen, dir: job.dir, cancel: job.cancel };
        self.jobs.borrow_mut().push(rec.clone());
        self.open.borrow_mut().push(rec);
    }

    fn new_listing(&self, dir: WidePath, gen: Gen) -> Box<dyn Listing> {
        Box::new(FakeListing::new(dir, gen))
    }
}

/// One call of FakeOps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpsCall {
    Recycle(Vec<ItemRef>),
    DeletePermanent(Vec<ItemRef>),
    Rename(WidePath, String),
    NewFolder(WidePath, String),
    Undo(OpId),
    ResumeRest(OpId),
    Cancel(OpId),
    Pause(OpId, bool),
    BringForward(OpId),
    AckDone(OpId),
    Shutdown,
}

/// Records every call and returns OpId(1), OpId(2), ... (or the error the test set); emits the events the test queued
/// on the next pump.
#[derive(Default)]
pub struct FakeOps {
    calls: RefCell<Vec<OpsCall>>,
    next: Cell<u32>,
    fail: RefCell<Option<OpsError>>,
    queued: RefCell<Vec<OpsEvent>>,
    ready: RefCell<Vec<OpsEvent>>,
}

impl FakeOps {
    pub fn calls(&self) -> Vec<OpsCall> {
        self.calls.borrow().clone()
    }

    /// Every mutating call fails with `e` until set to None.
    pub fn set_fail(&self, e: Option<OpsError>) {
        *self.fail.borrow_mut() = e;
    }

    /// An event drain() returns after the next pump.
    pub fn emit(&self, e: OpsEvent) {
        self.queued.borrow_mut().push(e);
    }

    fn release(&self) -> bool {
        let q = std::mem::take(&mut *self.queued.borrow_mut());
        let any = !q.is_empty();
        self.ready.borrow_mut().extend(q);
        any
    }

    fn mutate(&self, c: OpsCall) -> Result<OpId, OpsError> {
        self.calls.borrow_mut().push(c);
        if let Some(e) = self.fail.borrow().clone() {
            return Err(e);
        }
        let n = self.next.get() + 1;
        self.next.set(n);
        Ok(OpId(n))
    }
}

impl OpsService for FakeOps {
    fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::Recycle(items.to_vec()))
    }
    fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::DeletePermanent(items.to_vec()))
    }
    fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::Rename(item.clone(), to.to_string()))
    }
    fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::NewFolder(parent.clone(), name.to_string()))
    }
    fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::Undo(rec.of))
    }
    fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError> {
        self.mutate(OpsCall::ResumeRest(of))
    }
    fn cancel(&self, op: OpId) {
        self.calls.borrow_mut().push(OpsCall::Cancel(op));
    }
    fn pause(&self, op: OpId, on: bool) {
        self.calls.borrow_mut().push(OpsCall::Pause(op, on));
    }
    fn bring_forward(&self, op: OpId) {
        self.calls.borrow_mut().push(OpsCall::BringForward(op));
    }
    fn ack_done(&self, of: OpId) {
        self.calls.borrow_mut().push(OpsCall::AckDone(of));
    }
    fn host_pid(&self) -> Option<u32> {
        None
    }
    fn drain(&self) -> Vec<OpsEvent> {
        std::mem::take(&mut *self.ready.borrow_mut())
    }
    fn shutdown(&self) {
        self.calls.borrow_mut().push(OpsCall::Shutdown);
    }
}

/// Records every request; replies with what the test queued, on the next pump.
pub struct FakeShell {
    next: Cell<u32>,
    requests: RefCell<Vec<ShellReq>>,
    queued: RefCell<Vec<ShellReply>>,
    ready: RefCell<Vec<ShellReply>>,
    state: Cell<HostState>,
    prespawns: Cell<u32>,
    shutdowns: Cell<u32>,
    disabled: RefCell<Vec<(CallKind, String)>>,
}

impl Default for FakeShell {
    fn default() -> FakeShell {
        FakeShell {
            next: Cell::new(0),
            requests: RefCell::new(Vec::new()),
            queued: RefCell::new(Vec::new()),
            ready: RefCell::new(Vec::new()),
            state: Cell::new(HostState::Down),
            prespawns: Cell::new(0),
            shutdowns: Cell::new(0),
            disabled: RefCell::new(Vec::new()),
        }
    }
}

impl FakeShell {
    pub fn requests(&self) -> Vec<ShellReq> {
        self.requests.borrow().clone()
    }

    /// A reply drain() returns after the next pump.
    pub fn reply(&self, r: ShellReply) {
        self.queued.borrow_mut().push(r);
    }

    pub fn set_state(&self, s: HostState) {
        self.state.set(s);
    }

    pub fn disable(&self, kind: CallKind, ext: &str) {
        self.disabled.borrow_mut().push((kind, ext.to_string()));
    }

    pub fn prespawns(&self) -> u32 {
        self.prespawns.get()
    }

    pub fn shutdowns(&self) -> u32 {
        self.shutdowns.get()
    }

    fn release(&self) -> bool {
        let q = std::mem::take(&mut *self.queued.borrow_mut());
        let any = !q.is_empty();
        self.ready.borrow_mut().extend(q);
        any
    }
}

impl ShellService for FakeShell {
    fn next_req(&self) -> ReqId {
        let n = self.next.get() + 1;
        self.next.set(n);
        ReqId(n)
    }
    fn request(&self, r: ShellReq) {
        self.requests.borrow_mut().push(r);
    }
    fn drain(&self) -> Vec<ShellReply> {
        std::mem::take(&mut *self.ready.borrow_mut())
    }
    fn state(&self) -> HostState {
        self.state.get()
    }
    fn prespawn(&self) {
        self.prespawns.set(self.prespawns.get() + 1);
    }
    fn host_pid(&self) -> Option<u32> {
        None
    }
    fn disabled(&self, kind: CallKind, ext: &str) -> bool {
        self.disabled.borrow().iter().any(|(k, e)| *k == kind && e == ext)
    }
    fn shutdown(&self) {
        self.shutdowns.set(self.shutdowns.get() + 1);
    }
}

/// Records watches; emits the events the test queued on the next pump.
#[derive(Default)]
pub struct FakeWatch {
    next: Cell<u32>,
    watches: RefCell<Vec<(WatchId, WidePath, Gen)>>,
    unwatched: RefCell<Vec<WatchId>>,
    drives_on: RefCell<Vec<bool>>,
    pokes: Cell<u32>,
    fail: RefCell<Option<WatchError>>,
    queued: RefCell<Vec<WatchEvent>>,
    ready: RefCell<Vec<WatchEvent>>,
}

impl FakeWatch {
    pub fn watches(&self) -> Vec<(WatchId, WidePath, Gen)> {
        self.watches.borrow().clone()
    }

    pub fn unwatched(&self) -> Vec<WatchId> {
        self.unwatched.borrow().clone()
    }

    pub fn drives_calls(&self) -> Vec<bool> {
        self.drives_on.borrow().clone()
    }

    pub fn pokes(&self) -> u32 {
        self.pokes.get()
    }

    /// Every watch fails with `e` until set to None.
    pub fn set_fail(&self, e: Option<WatchError>) {
        *self.fail.borrow_mut() = e;
    }

    /// An event drain() returns after the next pump.
    pub fn emit(&self, e: WatchEvent) {
        self.queued.borrow_mut().push(e);
    }

    fn release(&self) -> bool {
        let q = std::mem::take(&mut *self.queued.borrow_mut());
        let any = !q.is_empty();
        self.ready.borrow_mut().extend(q);
        any
    }
}

impl WatchService for FakeWatch {
    fn watch(&self, dir: &WidePath, gen: Gen) -> Result<WatchId, WatchError> {
        if let Some(e) = *self.fail.borrow() {
            return Err(e);
        }
        let n = self.next.get() + 1;
        self.next.set(n);
        self.watches.borrow_mut().push((WatchId(n), dir.clone(), gen));
        Ok(WatchId(n))
    }
    fn unwatch(&self, id: WatchId) {
        self.unwatched.borrow_mut().push(id);
    }
    fn drives(&self, on: bool) {
        self.drives_on.borrow_mut().push(on);
    }
    fn poke_devices(&self) {
        self.pokes.set(self.pokes.get() + 1);
    }
    fn drain(&self) -> Vec<WatchEvent> {
        std::mem::take(&mut *self.ready.borrow_mut())
    }
}

/// Generic for every extension, or a glyph-like bitmap for the extensions the test set; counts assoc_changed and
/// rebuild_if_due.
#[derive(Default)]
pub struct FakeIcons {
    set: RefCell<Vec<Box<[u8]>>>,
    noted: RefCell<Vec<Box<[u8]>>>,
    assoc: Cell<u32>,
    rebuilds: Cell<u32>,
}

impl FakeIcons {
    /// `ext` gets a bitmap from now on.
    pub fn set_icon(&self, ext: &[u8]) {
        self.set.borrow_mut().push(ext.into());
    }

    pub fn noted(&self) -> Vec<Box<[u8]>> {
        self.noted.borrow().clone()
    }

    pub fn assoc_changed_calls(&self) -> u32 {
        self.assoc.get()
    }

    pub fn rebuild_calls(&self) -> u32 {
        self.rebuilds.get()
    }
}

/// A white square outline with a dot inside, `px` square, premultiplied BGRA.
fn glyph_icon(px: u16) -> Arc<[u8]> {
    let n = usize::from(px.max(1));
    let mut v = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let edge = x == 1 || y == 1 || x + 2 == n || y + 2 == n;
            let dot = (n / 3..n - n / 3).contains(&x) && (n / 3..n - n / 3).contains(&y);
            if edge || dot {
                let i = (y * n + x) * 4;
                if let Some(p) = v.get_mut(i..i + 4) {
                    p.copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    v.into()
}

impl IconService for FakeIcons {
    fn get(&self, ext: &[u8], px: u16) -> IconRef {
        if self.set.borrow().iter().any(|e| &**e == ext) {
            IconRef::Ready(glyph_icon(px))
        } else {
            IconRef::Generic
        }
    }
    fn note(&self, ext: &[u8]) {
        self.noted.borrow_mut().push(ext.into());
    }
    fn on_shell_reply(&self, r: &ShellReply) -> bool {
        let _ = r;
        false
    }
    fn assoc_changed(&self) {
        self.assoc.set(self.assoc.get() + 1);
    }
    fn rebuild_if_due(&self) {
        self.rebuilds.set(self.rebuilds.get() + 1);
    }
}

/// Runs jobs inline on pump(), in the order they came (win, lane recorded).
#[derive(Default)]
pub struct FakeBg {
    jobs: RefCell<Vec<(u32, JobFn)>>,
    runs: RefCell<Vec<(u32, Lane)>>,
}

impl FakeBg {
    pub fn runs(&self) -> Vec<(u32, Lane)> {
        self.runs.borrow().clone()
    }

    pub fn queued(&self) -> usize {
        self.jobs.borrow().len()
    }

    fn take_jobs(&self) -> Vec<(u32, JobFn)> {
        std::mem::take(&mut *self.jobs.borrow_mut())
    }
}

impl BgService for FakeBg {
    fn run(&self, win: u32, lane: Lane, job: JobFn) {
        self.runs.borrow_mut().push((win, lane));
        self.jobs.borrow_mut().push((win, job));
    }
}

/// Settable; 2026-11-21 14:40 local by default.
pub struct FakeClock {
    now: Cell<u64>,
    local: Cell<LocalTime>,
}

impl Default for FakeClock {
    fn default() -> FakeClock {
        FakeClock {
            now: Cell::new(0),
            local: Cell::new(LocalTime { year: 2026, month: 11, day: 21, hour: 14, minute: 40 }),
        }
    }
}

impl FakeClock {
    pub fn set_now(&self, ms: u64) {
        self.now.set(ms);
    }

    pub fn advance(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }

    pub fn set_local(&self, t: LocalTime) {
        self.local.set(t);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
    fn local_now(&self) -> LocalTime {
        self.local.get()
    }
}

/// An in-memory map of relative name -> bytes with a log of writes.
#[derive(Default)]
pub struct FakeOwn {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
    writes: RefCell<Vec<String>>,
}

impl FakeOwn {
    /// A file as if it had been there (not logged as a write).
    pub fn put(&self, rel: &str, bytes: &[u8]) {
        self.files.borrow_mut().insert(rel.to_string(), bytes.to_vec());
    }

    pub fn file(&self, rel: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(rel).cloned()
    }

    /// The names written, in order.
    pub fn writes(&self) -> Vec<String> {
        self.writes.borrow().clone()
    }
}

impl OwnFiles for FakeOwn {
    fn read(&self, rel: &str, cap: usize) -> Option<Vec<u8>> {
        self.files.borrow().get(rel).filter(|b| b.len() <= cap).cloned()
    }
    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> bool {
        self.files.borrow_mut().insert(rel.to_string(), bytes.to_vec());
        self.writes.borrow_mut().push(rel.to_string());
        true
    }
}

/// An in-memory FilesOptions with a log of every modify.
#[derive(Default)]
pub struct FakeOptions {
    opts: RefCell<FilesOptions>,
    log: RefCell<Vec<FilesOptions>>,
}

impl FakeOptions {
    pub fn set(&self, o: FilesOptions) {
        *self.opts.borrow_mut() = o;
    }

    /// The options after each modify, in order.
    pub fn modifications(&self) -> Vec<FilesOptions> {
        self.log.borrow().clone()
    }
}

impl OptionsService for FakeOptions {
    fn get(&self) -> FilesOptions {
        self.opts.borrow().clone()
    }
    fn modify(&self, f: &mut dyn FnMut(&mut FilesOptions)) -> bool {
        let mut o = self.opts.borrow_mut();
        f(&mut o);
        self.log.borrow_mut().push(o.clone());
        true
    }
}
