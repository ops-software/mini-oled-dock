//! The progress sink of one operation (OP3): IFileOperationProgressSink on the operation's STA thread. A callback's
//! item is top-level when its parsing name is in the chunk's map (folded name -> index, at most CHUNK_ITEMS entries),
//! else nested (FS-3 e3: nested callbacks may or may not fire; both are handled). Top-level items are journaled
//! ("started" at Pre, "done", "failed" or "skipped" at Post) and give one change and one undo entry; a nested item
//! adds nothing but its failure, so the user learns which file failed. Nothing about nested items is held.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use oled_trust::frame::files::{Change, ChangeKind, OpEvt, OpId, OpKind, UndoEntry, WidePath};
use windows::core::{implement, Ref, Result as WinResult, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Storage::FileSystem::{
    GetFileAttributesExW, GetFileExInfoStandard, GetLongPathNameW, GetShortPathNameW, FILE_ATTRIBUTE_DIRECTORY,
    WIN32_FILE_ATTRIBUTE_DATA,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_FILESYSPATH,
};

use super::checks::folded;
use super::coalesce::Coalescer;
use super::journal::{JournalWriter, PathRepr, Pre, Record, St};

/// Called with (op, top-level index) after each top-level item's successful Post (tests stop or kill there).
pub type PostHook = Arc<dyn Fn(OpId, u32) + Send + Sync>;

/// What a running operation's sink and runner share (one thread: Rc and RefCell).
pub struct SinkState {
    pub op: OpId,
    kind: OpKind,
    map: HashMap<Vec<u16>, usize>,
    base: u32,
    items: Vec<WidePath>,
    pre: Vec<Pre>,
    dsts: Vec<Option<WidePath>>,
    pub journal: Option<JournalWriter>,
    pub co: Coalescer,
    pub events: Sender<OpEvt>,
    pub cancel: Arc<AtomicBool>,
    pub attached: Arc<AtomicBool>,
    pub hook_post: Option<PostHook>,
    pub slow_post_ms: u32,
    /// Top-level items done.
    pub done: u32,
    /// ItemFailed events sent (top-level and nested).
    pub failed: u32,
    /// Top-level items that failed.
    pub failed_top: u32,
}

impl SinkState {
    pub fn new(op: OpId, events: Sender<OpEvt>, cancel: Arc<AtomicBool>, attached: Arc<AtomicBool>) -> SinkState {
        SinkState {
            op,
            kind: OpKind::Copy,
            map: HashMap::new(),
            base: 0,
            items: Vec::new(),
            pre: Vec::new(),
            dsts: Vec::new(),
            journal: None,
            co: Coalescer::new(op),
            events,
            cancel,
            attached,
            hook_post: None,
            slow_post_ms: 0,
            done: 0,
            failed: 0,
            failed_top: 0,
        }
    }

    /// The next chunk: its kind, the index of its first item among the operation's top-level items, its items with
    /// their pre-flight states and their expected destinations.
    pub fn begin_chunk(
        &mut self,
        kind: OpKind,
        base: u32,
        items: Vec<WidePath>,
        pre: Vec<Pre>,
        dsts: Vec<Option<WidePath>>,
    ) {
        let mut map = HashMap::with_capacity(items.len());
        for (i, p) in items.iter().enumerate() {
            map.insert(folded(p.units()), i);
            // The shell names its items by their long names: an item given with 8.3 components (a %TEMP% under
            // C:\Users\RUNNER~1, a path another program hands over) is known by its long form too.
            if let Some(long) = long_form(p) {
                map.entry(folded(long.units())).or_insert(i);
            }
            // Beyond MAX_PATH the shell names an item by its 8.3 path where the volume keeps short names.
            if p.units().len() > 259 {
                if let Some(short) = short_form(p) {
                    map.entry(folded(short.units())).or_insert(i);
                }
            }
        }
        self.map = map;
        self.kind = kind;
        self.base = base;
        self.items = items;
        self.pre = pre;
        self.dsts = dsts;
    }

    pub fn send(&self, evs: Vec<OpEvt>) {
        for e in evs {
            let _ = self.events.send(e);
        }
    }

    /// Top-level item `i` failed before the shell saw it: journaled, counted and reported.
    pub fn top_failed(&mut self, i: usize, item: WidePath, hr: i32) {
        self.failed_top = self.failed_top.saturating_add(1);
        self.journal_item(i, St::Failed, None, 0, 0);
        self.item_failed(item, hr);
    }

    /// One item failed: ItemFailed now.
    pub fn item_failed(&mut self, item: WidePath, hr: i32) {
        self.failed = self.failed.saturating_add(1);
        let _ = self.events.send(OpEvt::ItemFailed { op: self.op, item, hr });
    }

    /// Writes a journal record of chunk item `i` (an unwritable journal costs only its record).
    pub fn journal_item(&mut self, i: usize, st: St, dst: Option<&WidePath>, size: u64, mtime: i64) {
        let Some(src) = self.items.get(i) else { return };
        let rec = Record {
            i: self.base + i as u32,
            src: PathRepr::of(src),
            dst: dst.or(self.dsts.get(i).and_then(Option::as_ref)).map(PathRepr::of),
            pre: self.pre.get(i).copied().unwrap_or(Pre::Absent),
            st,
            size,
            mtime,
        };
        if let Some(j) = self.journal.as_mut() {
            let _ = j.record(&rec, Instant::now());
        }
    }

    fn index_of(&self, p: &WidePath) -> Option<usize> {
        self.map.get(&folded(p.units())).copied()
    }

    /// The top-level index of a callback's item (the shell passes its own shell items, so they are known by their
    /// path); NewItem has no source and is the chunk's one item.
    fn top(&self, src: &Src) -> Option<usize> {
        match &src.path {
            Some(p) => self.index_of(p),
            None if self.kind == OpKind::NewFolder && !src.item => Some(0),
            None => None,
        }
    }

    fn pre_item(&mut self, src: Src) -> WinResult<()> {
        if let Some(i) = self.top(&src) {
            self.journal_item(i, St::Started, None, 0, 0);
            if let Some(name) = self.items.get(i).and_then(WidePath::file_name) {
                self.co.current(&String::from_utf16_lossy(name));
            }
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(windows::core::Error::from_hresult(ERROR_CANCELLED.to_hresult()));
        }
        Ok(())
    }

    fn post_item(&mut self, src: Src, hr: HRESULT, created: Option<WidePath>, kind: ChangeKind) {
        let Some(i) = self.top(&src) else {
            // Nested: only a failure is reported.
            if hr.is_err() {
                if let Some(p) = src.path.or(created) {
                    self.item_failed(p, hr.0);
                }
            }
            return;
        };
        let item = self.items.get(i).cloned();
        if hr.is_err() {
            self.failed_top = self.failed_top.saturating_add(1);
            self.journal_item(i, St::Failed, None, 0, 0);
            if let Some(p) = item.or(created) {
                self.item_failed(p, hr.0);
            }
            return;
        }
        if skipped(hr) || (kind != ChangeKind::Deleted && created.is_none()) {
            self.journal_item(i, St::Skipped, None, 0, 0);
            return;
        }
        let (size, mtime, dir) = created.as_ref().and_then(attrs).unwrap_or((0, 0, false));
        self.journal_item(i, St::Done, created.as_ref(), size, mtime);
        self.done = self.done.saturating_add(1);
        let (change, undo) = outcome(self.kind, kind, item, created, size, mtime, dir);
        let now = Instant::now();
        let mut out = self.co.change(change, now);
        if let Some(e) = undo {
            out.extend(self.co.undo(e, self.attached.load(Ordering::Acquire)));
        }
        self.send(out);
        if let Some(h) = &self.hook_post {
            h(self.op, self.base + i as u32);
        }
        if self.slow_post_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(u64::from(self.slow_post_ms)));
        }
    }
}

/// Success codes of a Post callback that mean the item was left as it was (winerror.h COPYENGINE_S_*): the user
/// skipped it, or the engine did not handle it, will retry it or still has it pending. Every other success code is
/// done: a move, rename or delete reports COPYENGINE_S_DONT_PROCESS_CHILDREN (0x00270008), a merge or "keep both"
/// their own codes.
const SKIP_CODES: [i32; 4] = [0x0027_0005, 0x0027_0003, 0x0027_0004, 0x0027_000B];

fn skipped(hr: HRESULT) -> bool {
    SKIP_CODES.contains(&hr.0)
}

/// The change and the undo entry of one top-level item done.
fn outcome(
    op_kind: OpKind,
    kind: ChangeKind,
    item: Option<WidePath>,
    created: Option<WidePath>,
    size: u64,
    mtime: i64,
    dir: bool,
) -> (Change, Option<UndoEntry>) {
    let kind = match kind {
        ChangeKind::Deleted if created.is_some() => ChangeKind::Recycled,
        k => k,
    };
    let change = Change { kind, from: item.clone(), to: created.clone() };
    let undo = match (kind, item, created) {
        (ChangeKind::Deleted, _, _) => None,
        (_, _, _) if op_kind == OpKind::DeletePermanent => None,
        (ChangeKind::Created, _, Some(to)) => Some(UndoEntry { from: to.clone(), to, size, mtime, dir, bin: None }),
        (ChangeKind::Recycled, Some(from), Some(bin)) => {
            Some(UndoEntry { from, to: bin.clone(), size, mtime, dir, bin: Some(bin) })
        }
        (_, Some(from), Some(to)) => Some(UndoEntry { from, to, size, mtime, dir, bin: None }),
        _ => None,
    };
    (change, undo)
}

/// Size, last-write time (FILETIME) and folder flag of `p` (folders: size 0).
pub fn attrs(p: &WidePath) -> Option<(u64, i64, bool)> {
    let raw = p.raw();
    let mut d = WIN32_FILE_ATTRIBUTE_DATA::default();
    // SAFETY: `raw` is NUL-terminated; `d` is the structure GetFileExInfoStandard fills.
    unsafe { GetFileAttributesExW(PCWSTR(raw.as_ptr()), GetFileExInfoStandard, std::ptr::from_mut(&mut d).cast()) }
        .ok()?;
    let dir = d.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    let size = if dir { 0 } else { (u64::from(d.nFileSizeHigh) << 32) | u64::from(d.nFileSizeLow) };
    let mtime = (i64::from(d.ftLastWriteTime.dwHighDateTime) << 32) | i64::from(d.ftLastWriteTime.dwLowDateTime);
    Some((size, mtime, dir))
}

/// The long form of `p` (GetLongPathNameW: every 8.3 component expanded) when it exists and differs from `p`.
pub fn long_form(p: &WidePath) -> Option<WidePath> {
    let raw = p.raw();
    let mut buf = vec![0u16; raw.len() + 260];
    for _ in 0..2 {
        // SAFETY: `raw` is NUL-terminated and lives for the call; the buffer is writable for its whole length.
        let n = unsafe { GetLongPathNameW(PCWSTR(raw.as_ptr()), Some(&mut buf)) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            let long = WidePath::new(buf.get(..n)?).ok()?;
            return (long.units() != p.units()).then_some(long);
        }
        buf.resize(n, 0);
    }
    None
}

/// The 8.3 form of `p` (GetShortPathNameW) when it exists and differs from `p`.
pub fn short_form(p: &WidePath) -> Option<WidePath> {
    let raw = p.raw();
    let mut buf = vec![0u16; raw.len() + 1];
    // SAFETY: `raw` is NUL-terminated and lives for the call; the buffer is writable for its whole length (an 8.3 form
    // is never longer than the path it shortens).
    let n = unsafe { GetShortPathNameW(PCWSTR(raw.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let short = WidePath::new(buf.get(..n)?).ok()?;
    (short.units() != p.units()).then_some(short)
}

/// The file-system path of a shell item (its desktop parsing name when it has none), as a WidePath.
pub fn item_path(si: &IShellItem) -> Option<WidePath> {
    for form in [SIGDN_FILESYSPATH, SIGDN_DESKTOPABSOLUTEPARSING] {
        // SAFETY: a plain query of a live shell item; the string returned is freed below.
        let Ok(s) = (unsafe { si.GetDisplayName(form) }) else { continue };
        let units = pwstr_units(s);
        // SAFETY: allocated by the shell with CoTaskMemAlloc; not used afterwards.
        unsafe { CoTaskMemFree(Some(s.0 as *const std::ffi::c_void)) };
        if let Ok(p) = WidePath::new(&units) {
            return Some(p);
        }
    }
    None
}

fn pwstr_units(s: PWSTR) -> Vec<u16> {
    if s.is_null() {
        return Vec::new();
    }
    // SAFETY: a NUL-terminated string returned by the shell, read up to its NUL.
    unsafe { s.as_wide().to_vec() }
}

fn path_of(r: &Ref<IShellItem>) -> Option<WidePath> {
    r.as_ref().and_then(item_path)
}

/// A callback's source item: whether there is one, and its path.
struct Src {
    item: bool,
    path: Option<WidePath>,
}

fn src_of(r: &Ref<IShellItem>) -> Src {
    Src { item: !r.is_null(), path: path_of(r) }
}

/// NewItem's callbacks have no source item.
const NO_SRC: Src = Src { item: false, path: None };

/// The destination of a NewItem: the folder's path and the name.
fn new_item_path(folder: &Ref<IShellItem>, name: &PCWSTR) -> Option<WidePath> {
    let dir = path_of(folder)?;
    if name.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string the shell passes for the duration of the callback.
    let units = unsafe { name.as_wide() };
    dir.join(units).ok()
}

#[implement(IFileOperationProgressSink)]
pub struct Sink {
    pub state: Rc<RefCell<SinkState>>,
}

impl Sink {
    fn with<R>(&self, f: impl FnOnce(&mut SinkState) -> R) -> R {
        f(&mut self.state.borrow_mut())
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> WinResult<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hr: HRESULT) -> WinResult<()> {
        self.with(|s| {
            if let Some(j) = s.journal.as_mut() {
                let _ = j.flush(Instant::now());
            }
        });
        Ok(())
    }
    fn PreRenameItem(&self, _f: u32, item: Ref<IShellItem>, _name: &PCWSTR) -> WinResult<()> {
        let src = src_of(&item);
        self.with(|s| s.pre_item(src))
    }
    fn PostRenameItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> WinResult<()> {
        let (src, to) = (src_of(&item), path_of(&created));
        self.with(|s| s.post_item(src, hr, to, ChangeKind::Renamed));
        Ok(())
    }
    fn PreMoveItem(&self, _f: u32, item: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> WinResult<()> {
        let src = src_of(&item);
        self.with(|s| s.pre_item(src))
    }
    fn PostMoveItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _d: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> WinResult<()> {
        let (src, to) = (src_of(&item), path_of(&created));
        self.with(|s| s.post_item(src, hr, to, ChangeKind::Moved));
        Ok(())
    }
    fn PreCopyItem(&self, _f: u32, item: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> WinResult<()> {
        let src = src_of(&item);
        self.with(|s| s.pre_item(src))
    }
    fn PostCopyItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _d: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> WinResult<()> {
        let (src, to) = (src_of(&item), path_of(&created));
        self.with(|s| s.post_item(src, hr, to, ChangeKind::Copied));
        Ok(())
    }
    fn PreDeleteItem(&self, _f: u32, item: Ref<IShellItem>) -> WinResult<()> {
        let src = src_of(&item);
        self.with(|s| s.pre_item(src))
    }
    fn PostDeleteItem(&self, _f: u32, item: Ref<IShellItem>, hr: HRESULT, created: Ref<IShellItem>) -> WinResult<()> {
        let (src, bin) = (src_of(&item), path_of(&created));
        self.with(|s| s.post_item(src, hr, bin, ChangeKind::Deleted));
        Ok(())
    }
    fn PreNewItem(&self, _f: u32, _folder: Ref<IShellItem>, _name: &PCWSTR) -> WinResult<()> {
        self.with(|s| s.pre_item(NO_SRC))
    }
    fn PostNewItem(
        &self,
        _f: u32,
        folder: Ref<IShellItem>,
        name: &PCWSTR,
        _template: &PCWSTR,
        _attrs: u32,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> WinResult<()> {
        let to = path_of(&created).or_else(|| new_item_path(&folder, name));
        self.with(|s| s.post_item(NO_SRC, hr, to, ChangeKind::Created));
        Ok(())
    }
    fn UpdateProgress(&self, total: u32, so_far: u32) -> WinResult<()> {
        self.with(|s| {
            let out = s.co.progress(u64::from(so_far), u64::from(total), Instant::now());
            s.send(out);
        });
        Ok(())
    }
    fn ResetTimer(&self) -> WinResult<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> WinResult<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> WinResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WidePath {
        WidePath::parse(s).unwrap()
    }

    #[test]
    fn post_codes() {
        assert!(!skipped(HRESULT(0)), "S_OK");
        assert!(!skipped(HRESULT(0x0027_0008)), "COPYENGINE_S_DONT_PROCESS_CHILDREN: a move, rename or delete done");
        assert!(!skipped(HRESULT(0x0027_000C)), "COPYENGINE_S_KEEP_BOTH");
        assert!(skipped(HRESULT(0x0027_0005)), "COPYENGINE_S_USER_IGNORED");
    }

    #[test]
    fn outcomes_per_kind() {
        let (a, b) = (p("C:\\a\\x"), p("C:\\b\\x"));
        let (c, u) = outcome(OpKind::Copy, ChangeKind::Copied, Some(a.clone()), Some(b.clone()), 3, 4, false);
        assert_eq!(c, Change { kind: ChangeKind::Copied, from: Some(a.clone()), to: Some(b.clone()) });
        assert_eq!(u, Some(UndoEntry { from: a.clone(), to: b.clone(), size: 3, mtime: 4, dir: false, bin: None }));
        let (c, u) = outcome(OpKind::DeletePermanent, ChangeKind::Deleted, Some(a.clone()), None, 0, 0, false);
        assert_eq!(c.kind, ChangeKind::Deleted);
        assert_eq!(u, None, "a permanent delete has no undo");
        let bin = p("C:\\$Recycle.Bin\\S-1-5-21-1\\$R1.txt");
        let (c, u) = outcome(OpKind::Recycle, ChangeKind::Deleted, Some(a.clone()), Some(bin.clone()), 1, 2, false);
        assert_eq!(c.kind, ChangeKind::Recycled);
        assert_eq!(u.unwrap().bin, Some(bin.clone()));
        let (_, u) = outcome(OpKind::DeletePermanent, ChangeKind::Deleted, Some(a.clone()), Some(bin), 1, 2, false);
        assert_eq!(u, None, "never an undo entry for DeletePermanent");
        let (c, u) = outcome(OpKind::NewFolder, ChangeKind::Created, None, Some(b.clone()), 0, 9, true);
        assert_eq!(c, Change { kind: ChangeKind::Created, from: None, to: Some(b.clone()) });
        assert_eq!(u.unwrap().to, b);
    }
}
