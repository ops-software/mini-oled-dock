//! The runner (OP3): one operation on its own STA thread, through IFileOperation only, in chunks of at most
//! CHUNK_ITEMS top-level items. For each chunk: check_item on every item and the destination (before any
//! IFileOperation exists), the pre-flight, op_flags with the chunk's own collision count, a new IFileOperation with
//! the sink advised, one Copy/Move/Delete/Rename/NewItem per item, PerformOperations, GetAnyOperationsAborted. Between
//! chunks a cancel (the flag, or a system dialog the user cancelled) stops the operation. Every HRESULT is kept;
//! nothing retries. The journal's header and a "queued" record for every item held in memory are written and
//! flushed before the first PerformOperations.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use oled_trust::frame::files::{Conflicts, OpEvt, OpId, OpKind, OpResult, Refusal, UndoKind, WidePath};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
    SHCreateItemFromRelativeName, COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, FILEOPERATION_FLAGS,
};

use super::binrec::{bin_record, forget_record, Forgot};
use super::checks::check_item;
use super::classify::bin_turned_off;
use super::consts::{CHUNK_ITEMS, DIALOGS_UNDER_SILENT, OWNER_ACROSS_PROCESSES, RESTORE_FROM_BIN};
use super::flags::op_flags;
use super::journal::{EndKind, Header, JournalWriter, PathRepr, Pre, Record, St, StagedItems, VERSION};
use super::preflight::{preflight, skip_present, target_name};
use super::sink::{PostHook, Sink, SinkState};
use crate::own_files::OwnDir;

/// One chunk of an operation, ready to run: at most CHUNK_ITEMS top-level items of one concrete kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub op: OpId,
    /// Copy, Move, Recycle, DeletePermanent, Rename or NewFolder.
    pub kind: OpKind,
    pub items: Vec<WidePath>,
    /// Copy and Move: the folder; NewFolder: the parent.
    pub dest: Option<WidePath>,
    /// Rename and NewFolder: the one new name.
    pub name: Option<Box<str>>,
    pub conflicts: Conflicts,
    /// The pre-flight states to journal (a resume plan's group R carries the journaled ones); empty: the runner's own.
    pub pre: Vec<Pre>,
    /// A new name per item (undo and restore plans), or empty.
    pub names: Vec<Box<str>>,
}

/// An operation as the UI asked for it: the runner cuts it into chunks itself, reading the spill as it goes.
pub struct OpJob {
    pub kind: OpKind,
    pub dest: Option<WidePath>,
    pub name: Option<Box<str>>,
    pub conflicts: Conflicts,
    pub staged: StagedItems,
}

pub enum Work {
    Staged(OpJob),
    /// An undo or resume plan, already cut into chunks of at most CHUNK_ITEMS; the kind is the operation's own
    /// (Undo or Resume) for its events.
    Planned(OpKind, Vec<Prepared>),
}

impl Work {
    /// The operation's kind in its events.
    pub fn kind(&self) -> OpKind {
        match self {
            Work::Staged(j) => j.kind,
            Work::Planned(k, _) => *k,
        }
    }

    /// Top-level items.
    pub fn items(&self) -> u64 {
        match self {
            Work::Staged(j) if j.kind == OpKind::NewFolder => 1,
            Work::Staged(j) => j.staged.len(),
            Work::Planned(_, p) => {
                p.iter().map(|c| c.items.len().max(usize::from(c.kind == OpKind::NewFolder)) as u64).sum()
            }
        }
    }
}

/// Where the runner journals: the data folder, and this host's pid and creation time.
#[derive(Clone, Debug)]
pub struct JournalCtx {
    pub dir: OwnDir,
    pub pid: u32,
    pub created: u64,
}

/// Debug counters of one runner context (tests prove with them what ran).
#[derive(Debug, Default)]
pub struct RunStats {
    /// IFileOperation objects created.
    pub created: AtomicU32,
    /// PerformOperations calls.
    pub performs: AtomicU32,
}

/// Called with each $I record a restore proved to be its item's own, between the proof and the removal (tests try to
/// swap the record there).
pub type RecordHook = Arc<dyn Fn(&WidePath) + Send + Sync>;

/// The HRESULT of a Recycle item left where it is because its volume's bin is turned off (bin_turned_off).
pub const RECYCLE_BIN_OFF: i32 = COPYENGINE_E_RECYCLE_BIN_NOT_FOUND.0;

/// Everything one run needs besides its work.
pub struct RunCtx {
    /// FILES_TEST_ROOT (debug builds): every item and destination outside is refused.
    pub roots: Option<Vec<WidePath>>,
    pub test_no_ui: bool,
    pub dialogs_under_silent: bool,
    /// The UI's window, used as the dialogs' owner only when OWNER_ACROSS_PROCESSES.
    pub ui_owner: Option<isize>,
    /// The host's own hidden window.
    pub host_window: Option<isize>,
    pub journal: Option<JournalCtx>,
    pub cancel: Arc<AtomicBool>,
    /// A UI is attached (an UndoPart due while none is drops the undo record).
    pub attached: Arc<AtomicBool>,
    pub events: Sender<OpEvt>,
    pub stats: Arc<RunStats>,
    pub hook_post: Option<PostHook>,
    pub hook_record: Option<RecordHook>,
    /// Debug builds' FILES_TEST_SLOW_POST_MS: a pause after each top-level item, so a test can stop a host mid-way.
    pub slow_post_ms: u32,
    /// Whether an item's volume has its Recycle Bin turned off: classify::bin_turned_off (tests pass their own).
    pub bin_off: fn(&WidePath) -> bool,
}

impl RunCtx {
    /// A context with no fence, no journal, a UI attached and the production flags.
    pub fn new(events: Sender<OpEvt>) -> RunCtx {
        RunCtx {
            roots: None,
            test_no_ui: false,
            dialogs_under_silent: DIALOGS_UNDER_SILENT,
            ui_owner: None,
            host_window: None,
            journal: None,
            cancel: Arc::new(AtomicBool::new(false)),
            attached: Arc::new(AtomicBool::new(true)),
            events,
            stats: Arc::new(RunStats::default()),
            hook_post: None,
            hook_record: None,
            slow_post_ms: 0,
            bin_off: bin_turned_off,
        }
    }
}

/// COM on this thread, single-threaded apartment, for the life of the guard.
struct Com(bool);

impl Com {
    fn init() -> Com {
        // SAFETY: plain COM initialisation of the calling thread; balanced by Drop when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Com(hr.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx of this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The undo an operation of `kind` offers when it did something.
pub fn undo_kind(kind: OpKind) -> Option<UndoKind> {
    match kind {
        OpKind::Copy => Some(UndoKind::RecycleCopies),
        OpKind::Move => Some(UndoKind::MoveBack),
        OpKind::Recycle if RESTORE_FROM_BIN => Some(UndoKind::RestoreRecycled),
        OpKind::Recycle => Some(UndoKind::OpenRecycleBin),
        OpKind::Rename => Some(UndoKind::RenameBack),
        OpKind::NewFolder => Some(UndoKind::RecycleNewFolder),
        OpKind::DeletePermanent | OpKind::Undo | OpKind::Resume => None,
    }
}

/// The string form SHCreateItemFromParsingName gets: the plain path up to 259 units, the \\?\ form beyond.
fn parsing_form(p: &WidePath) -> Vec<u16> {
    if p.units().len() <= 259 {
        let mut v = p.units().to_vec();
        v.push(0);
        v
    } else {
        p.raw()
    }
}

fn shell_item(p: &WidePath) -> windows::core::Result<IShellItem> {
    let form = parsing_form(p);
    // SAFETY: `form` is NUL-terminated and lives for the call; no bind context.
    let direct = unsafe { SHCreateItemFromParsingName(PCWSTR(form.as_ptr()), None) };
    if direct.is_ok() || p.units().len() <= 259 {
        return direct;
    }
    // The shell refuses a \\?\ parsing name: walk from the root, one component at a time.
    let mut chain = Vec::new();
    let mut at = p.clone();
    while let Some(parent) = at.parent() {
        chain.push(at.file_name().map(<[u16]>::to_vec).unwrap_or_default());
        at = parent;
    }
    let root = parsing_form(&at);
    // SAFETY: `root` is NUL-terminated and lives for the call; no bind context.
    let mut item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(root.as_ptr()), None) }?;
    for name in chain.iter().rev() {
        let mut z = name.clone();
        z.push(0);
        // SAFETY: `item` is a live shell item of this thread; `z` is NUL-terminated and lives for the call.
        item = unsafe { SHCreateItemFromRelativeName(&item, PCWSTR(z.as_ptr()), None) }?;
    }
    Ok(item)
}

/// What one run ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunEnd {
    pub result: OpResult,
    pub items: u32,
    pub failed: u32,
    pub replaced: u32,
}

/// Runs one operation on the calling thread (a new thread of the host, or a test's own thread) and sends its events.
pub fn run(op: OpId, work: Work, ctx: &RunCtx) -> RunEnd {
    let _com = Com::init();
    let kind = work.kind();
    let total = work.items();
    let st = Rc::new(RefCell::new(SinkState::new(op, ctx.events.clone(), ctx.cancel.clone(), ctx.attached.clone())));
    st.borrow_mut().hook_post = ctx.hook_post.clone();
    st.borrow_mut().slow_post_ms = ctx.slow_post_ms;
    let _ = ctx.events.send(OpEvt::Started { op, kind, items: u32::try_from(total).unwrap_or(u32::MAX), bytes: 0 });
    let (dest, conflicts) = match &work {
        Work::Staged(j) => (j.dest.clone(), j.conflicts),
        Work::Planned(_, p) => {
            (p.first().and_then(|c| c.dest.clone()), p.first().map_or(Conflicts::AskSystem, |c| c.conflicts))
        }
    };
    if let Some(jc) = &ctx.journal {
        let journal_kind = match &work {
            Work::Planned(OpKind::Resume, p) => p.first().map_or(OpKind::Resume, |c| c.kind),
            w => w.kind(),
        };
        let header = Header {
            v: VERSION,
            op: op.0,
            kind: journal_kind.into(),
            host_pid: jc.pid,
            host_created: jc.created,
            dest: dest.as_ref().map(PathRepr::of),
            conflicts: conflicts.into(),
            items: total,
            started: super::journal::now_filetime(),
        };
        if let Ok(mut w) = JournalWriter::create(&jc.dir, op, jc.pid, &header, Instant::now()) {
            queued(&mut w, &work);
            st.borrow_mut().journal = Some(w);
        }
    }
    let mut out = Outcome { result: None, replaced: 0, index: 0 };
    match work {
        Work::Staged(mut job) => {
            if job.kind == OpKind::NewFolder {
                let p = prepared(op, &job, Vec::new());
                run_chunk(&p, ctx, &st, &mut out);
            } else {
                let template = prepared(op, &job, Vec::new());
                match job.staged.chunks(CHUNK_ITEMS) {
                    Ok(chunks) => {
                        for chunk in chunks {
                            if out.result.is_some() {
                                break;
                            }
                            match chunk {
                                Ok(items) => {
                                    let p = Prepared { items, ..template.clone() };
                                    run_chunk(&p, ctx, &st, &mut out);
                                }
                                Err(_) => out.result = Some(OpResult::Failed),
                            }
                        }
                    }
                    Err(_) => out.result = Some(OpResult::Failed),
                }
                job.staged.remove_spill();
            }
        }
        Work::Planned(_, chunks) => {
            for p in &chunks {
                if out.result.is_some() {
                    break;
                }
                run_chunk(p, ctx, &st, &mut out);
            }
        }
    }
    let mut s = st.borrow_mut();
    let (done, failed, failed_top) = (s.done, s.failed, s.failed_top);
    let result = out.result.unwrap_or(if failed_top > 0 && done == 0 { OpResult::Failed } else { OpResult::Done });
    if let Some(j) = s.journal.as_mut() {
        let end = match result {
            OpResult::Done => EndKind::Done,
            OpResult::Cancelled => EndKind::Cancelled,
            OpResult::Failed | OpResult::Refused => EndKind::Failed,
        };
        let _ = j.end(end, Instant::now());
    }
    // The journal is closed before Finished leaves: the host may remove it as soon as Finished arrives.
    s.journal = None;
    let undo = if done > 0 && result != OpResult::Refused { undo_kind(kind) } else { None };
    let attached = ctx.attached.load(Ordering::Acquire);
    let evs = s.co.finish(result, undo, out.replaced, done, failed, attached);
    s.send(evs);
    RunEnd { result, items: done, failed, replaced: out.replaced }
}

/// The "queued" records: every item held in memory, before anything runs.
fn queued(w: &mut JournalWriter, work: &Work) {
    let now = Instant::now();
    let mut put = |i: usize, src: &WidePath, dst: Option<WidePath>, pre: Pre| {
        let r = Record {
            i: i as u32,
            src: PathRepr::of(src),
            dst: dst.as_ref().map(PathRepr::of),
            pre,
            st: St::Queued,
            size: 0,
            mtime: 0,
        };
        let _ = w.record(&r, now);
    };
    match work {
        Work::Staged(j) => {
            for (i, src) in j.staged.memory().iter().enumerate() {
                put(i, src, dest_of(j.kind, src, j.dest.as_ref(), j.name.as_deref(), &[], 0), Pre::Absent);
            }
        }
        Work::Planned(_, chunks) => {
            let mut i = 0;
            for c in chunks {
                for (k, src) in c.items.iter().enumerate() {
                    let pre = c.pre.get(k).copied().unwrap_or(Pre::Absent);
                    put(i, src, dest_of(c.kind, src, c.dest.as_ref(), c.name.as_deref(), &c.names, k), pre);
                    i += 1;
                }
            }
        }
    }
    let _ = w.flush(Instant::now());
}

/// Where item `k` of a chunk lands (for the journal): dest\name for Copy and Move, its new name for Rename.
fn dest_of(
    kind: OpKind,
    src: &WidePath,
    dest: Option<&WidePath>,
    name: Option<&str>,
    names: &[Box<str>],
    k: usize,
) -> Option<WidePath> {
    match kind {
        OpKind::Copy | OpKind::Move => dest?.join(&target_name(src, names, k)?).ok(),
        OpKind::Rename => {
            let n: Vec<u16> = names
                .get(k)
                .map(|n| n.encode_utf16().collect())
                .or_else(|| name.map(|n| n.encode_utf16().collect()))?;
            src.parent()?.join(&n).ok()
        }
        _ => None,
    }
}

fn prepared(op: OpId, job: &OpJob, items: Vec<WidePath>) -> Prepared {
    Prepared {
        op,
        kind: job.kind,
        items,
        dest: job.dest.clone(),
        name: job.name.clone(),
        conflicts: job.conflicts,
        pre: Vec::new(),
        names: Vec::new(),
    }
}

/// The running state between chunks.
struct Outcome {
    /// Set when the operation stops before its last chunk (or must report this result).
    result: Option<OpResult>,
    replaced: u32,
    /// Top-level index of the next chunk's first item.
    index: u32,
}

/// Refuses the operation at this chunk: Refused with the item's name, and no further chunk.
fn refuse(ctx: &RunCtx, op: OpId, why: Refusal, item: &WidePath, out: &mut Outcome) {
    let name = item.file_name().map(String::from_utf16_lossy).unwrap_or_else(|| item.to_string_lossy());
    let mut end = name.len().min(520);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let detail: Box<str> = name.get(..end).unwrap_or_default().into();
    let _ = ctx.events.send(OpEvt::Refused { op, why, detail });
    out.result = Some(OpResult::Refused);
}

fn run_chunk(p: &Prepared, ctx: &RunCtx, st: &Rc<RefCell<SinkState>>, out: &mut Outcome) {
    let base = out.index;
    out.index = out.index.saturating_add(p.items.len() as u32);
    if ctx.cancel.load(Ordering::Acquire) {
        out.result = Some(OpResult::Cancelled);
        return;
    }
    // Every item and destination passes the checks before any IFileOperation exists.
    let roots = ctx.roots.as_deref();
    for item in &p.items {
        if let Err(why) = check_item(item, p.kind, roots) {
            return refuse(ctx, p.op, why, item, out);
        }
    }
    if let Some(d) = &p.dest {
        if let Err(why) = check_item(d, OpKind::Copy, roots) {
            return refuse(ctx, p.op, why, d, out);
        }
    }
    if !matches!(
        p.kind,
        OpKind::Copy | OpKind::Move | OpKind::Recycle | OpKind::DeletePermanent | OpKind::Rename | OpKind::NewFolder
    ) {
        let _ = ctx.events.send(OpEvt::Refused { op: p.op, why: Refusal::UnknownOp, detail: "".into() });
        out.result = Some(OpResult::Refused);
        return;
    }
    let pf = preflight(p.kind, &p.items, &p.names, p.dest.as_ref());
    let pre = if p.pre.len() == p.items.len() { p.pre.clone() } else { pf.pre.clone() };
    let dsts: Vec<Option<WidePath>> = p
        .items
        .iter()
        .enumerate()
        .map(|(k, s)| dest_of(p.kind, s, p.dest.as_ref(), p.name.as_deref(), &p.names, k))
        .collect();
    st.borrow_mut().begin_chunk(p.kind, base, p.items.clone(), pre, dsts);
    // SkipAll: the items whose destination exists are journaled as skipped and left out.
    let mut keep: Vec<usize> = (0..p.items.len()).collect();
    let mut collisions = pf.collisions;
    if p.conflicts == Conflicts::SkipAll && matches!(p.kind, OpKind::Copy | OpKind::Move) {
        keep = skip_present(&pf.pre);
        let mut s = st.borrow_mut();
        for i in (0..p.items.len()).filter(|i| !keep.contains(i)) {
            s.journal_item(i, St::Skipped, None, 0, 0);
        }
        collisions = 0;
    } else if matches!(p.kind, OpKind::Copy | OpKind::Move) {
        out.replaced = out.replaced.saturating_add(pf.collisions);
    }
    if keep.is_empty() && p.kind != OpKind::NewFolder {
        return;
    }
    let flags = op_flags(p.kind, p.conflicts, collisions, ctx.dialogs_under_silent, ctx.test_no_ui);
    ctx.stats.created.fetch_add(1, Ordering::Relaxed);
    // SAFETY: COM is initialised on this thread (run); FileOperation is the shell's in-process class.
    let fo: IFileOperation = match unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) } {
        Ok(fo) => fo,
        Err(e) => {
            let mut s = st.borrow_mut();
            for &i in &keep {
                if let Some(item) = p.items.get(i).cloned() {
                    s.item_failed(item, e.code().0);
                }
            }
            out.result = Some(OpResult::Failed);
            return;
        }
    };
    // SAFETY: a live IFileOperation of this thread; flags is a plain value.
    let _ = unsafe { fo.SetOperationFlags(FILEOPERATION_FLAGS(flags)) };
    let owner = if OWNER_ACROSS_PROCESSES { ctx.ui_owner.or(ctx.host_window) } else { ctx.host_window };
    if let Some(h) = owner {
        // SAFETY: a window handle value; IFileOperation only uses it as the dialogs' owner.
        let _ = unsafe { fo.SetOwnerWindow(HWND(h as *mut std::ffi::c_void)) };
    }
    let sink: IFileOperationProgressSink = Sink { state: st.clone() }.into();
    // SAFETY: a live IFileOperation and a live sink; the cookie is given back to Unadvise below.
    let cookie = unsafe { fo.Advise(&sink) }.ok();
    let queued_any = queue_items(&fo, p, &keep, st, ctx.bin_off);
    if queued_any {
        ctx.stats.performs.fetch_add(1, Ordering::Relaxed);
        // SAFETY: a live IFileOperation with its operations queued on this thread.
        let performed = unsafe { fo.PerformOperations() };
        // SAFETY: as above.
        let aborted = unsafe { fo.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
        let cancelled = performed.as_ref().err().is_some_and(|e| e.code() == ERROR_CANCELLED.to_hresult());
        if aborted || cancelled || ctx.cancel.load(Ordering::Acquire) {
            out.result = Some(OpResult::Cancelled);
        }
    }
    if let Some(c) = cookie {
        // SAFETY: the cookie Advise returned on this IFileOperation.
        let _ = unsafe { fo.Unadvise(c) };
    }
    drop(sink);
    drop(fo);
    let moved = std::mem::take(&mut st.borrow_mut().from_bin);
    forget_records(&moved, ctx, st);
}

/// Removes the bin's $I records of the items a chunk moved out of the Recycle Bin, each only once it is proven to be
/// that item's own orphan, and through the very handle it was proven by (undo::forget_record): IFileOperation deletes
/// by name, and a name can be swapped between a check and a delete. The fence comes first, so a record outside it is
/// never opened. A record that stays is reported as a failed item with the HRESULT; the items stay done.
fn forget_records(moved: &[(WidePath, Vec<WidePath>)], ctx: &RunCtx, st: &Rc<RefCell<SinkState>>) {
    let roots = ctx.roots.as_deref();
    let between = |r: &WidePath| ctx.hook_record.iter().for_each(|h| h(r));
    for (bin, restored) in moved {
        let Some(rec) = bin_record(bin) else { continue };
        if check_item(&rec, OpKind::DeletePermanent, roots).is_err() {
            continue;
        }
        if let Forgot::Failed(hr) = forget_record(bin, &rec, restored, &between) {
            st.borrow_mut().item_failed(rec, hr);
        }
    }
}

/// Queues one operation per kept item; an item or destination the shell cannot parse fails alone, and so does a
/// Recycle item on a volume whose bin is turned off. True when at least one operation was queued.
fn queue_items(
    fo: &IFileOperation,
    p: &Prepared,
    keep: &[usize],
    st: &Rc<RefCell<SinkState>>,
    bin_off: fn(&WidePath) -> bool,
) -> bool {
    let none = None::<&IFileOperationProgressSink>;
    let dest_item = match &p.dest {
        Some(d) => match shell_item(d) {
            Ok(si) => Some(si),
            Err(e) => {
                let mut s = st.borrow_mut();
                for &i in keep {
                    if let Some(item) = p.items.get(i).cloned() {
                        s.item_failed(item, e.code().0);
                    }
                }
                if p.kind == OpKind::NewFolder {
                    s.item_failed(d.clone(), e.code().0);
                }
                return false;
            }
        },
        None => None,
    };
    if p.kind == OpKind::NewFolder {
        let (Some(parent), Some(name)) = (dest_item.as_ref(), p.name.as_deref()) else { return false };
        let name = HSTRING::from(name);
        // SAFETY: live shell items of this thread; `name` lives for the call (the operation copies it).
        return unsafe { fo.NewItem(parent, FILE_ATTRIBUTE_DIRECTORY.0, &name, PCWSTR::null(), none) }.is_ok();
    }
    let mut any = false;
    for &i in keep {
        let Some(item) = p.items.get(i) else { continue };
        if p.kind == OpKind::Recycle && bin_off(item) {
            // The shell would delete it for good without asking: it stays where it is, and the UI's bar alone sends a
            // permanent delete.
            st.borrow_mut().top_failed(i, item.clone(), RECYCLE_BIN_OFF);
            continue;
        }
        let si = match shell_item(item) {
            Ok(si) => si,
            Err(e) => {
                st.borrow_mut().top_failed(i, item.clone(), e.code().0);
                continue;
            }
        };
        let name: Option<HSTRING> = p.names.get(i).map(|n| HSTRING::from(&**n)).or_else(|| match p.kind {
            OpKind::Rename => p.name.as_deref().map(HSTRING::from),
            _ => None,
        });
        let pname = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        // SAFETY: live shell items of this thread; `name` outlives the call (the operation copies it).
        let queued = unsafe {
            match (p.kind, dest_item.as_ref()) {
                (OpKind::Copy, Some(d)) => fo.CopyItem(&si, d, pname, none),
                (OpKind::Move, Some(d)) => fo.MoveItem(&si, d, pname, none),
                (OpKind::Recycle | OpKind::DeletePermanent, _) => fo.DeleteItem(&si, none),
                (OpKind::Rename, _) if !pname.is_null() => fo.RenameItem(&si, pname, none),
                _ => Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)),
            }
        };
        match queued {
            Ok(()) => any = true,
            Err(e) => st.borrow_mut().top_failed(i, item.clone(), e.code().0),
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_kinds() {
        assert_eq!(undo_kind(OpKind::Copy), Some(UndoKind::RecycleCopies));
        assert_eq!(undo_kind(OpKind::Move), Some(UndoKind::MoveBack));
        assert_eq!(undo_kind(OpKind::Recycle), Some(UndoKind::RestoreRecycled), "RESTORE_FROM_BIN is true");
        assert_eq!(undo_kind(OpKind::Rename), Some(UndoKind::RenameBack));
        assert_eq!(undo_kind(OpKind::NewFolder), Some(UndoKind::RecycleNewFolder));
        for k in [OpKind::DeletePermanent, OpKind::Undo, OpKind::Resume] {
            assert_eq!(undo_kind(k), None);
        }
    }

    #[test]
    fn parsing_form_switches_at_260() {
        let short = WidePath::parse(&format!("C:\\{}", "a".repeat(256))).unwrap();
        assert_eq!(short.units().len(), 259);
        assert_eq!(parsing_form(&short).len(), 260, "plain, NUL-terminated");
        let long = WidePath::parse(&format!("C:\\{}", "a".repeat(257))).unwrap();
        assert!(String::from_utf16_lossy(&parsing_form(&long)).starts_with("\\\\?\\C:\\"));
    }
}
