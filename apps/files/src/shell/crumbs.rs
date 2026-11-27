//! Breadcrumbs shared with the client (SH3): which extension call each STA of the shell host is in, and the busy word
//! of the kill protocol. The client creates the section (4 KiB of shared memory) and the host maps the handle it
//! inherits. Layout, pinned (layout_offsets_pinned):
//! - header, 64 bytes: magic "FCRB" at 0, version u16 1 at 4, slots u16 4 at 6, host_pid u32 at 8, busy u32 at 12;
//! - then 4 slots of 128 bytes: seq u32 at 0 (odd while inside a call), kind u16 at 4 (CallKind), flags u16 at 6,
//!   started_ms u64 at 8 (GetTickCount64 of the host), ext [u16; 16] at 16 (lower case, truncated), calls u32 at 48
//!   (calls since the STA started).
//!
//! The busy word: bit 31 = KILLING, bits 0-30 = the calls in progress that must not be cut (InvokeCommand, Exec).
//! busy_enter fails once KILLING is set; a killer must win compare_exchange(0, KILLING) before it may end the host,
//! and KILLING is never cleared. So no invoke starts after a kill decision and no kill lands on a started invoke.
// Read by another process: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{fence, AtomicPtr, AtomicU16, AtomicU32, AtomicU64};

use oled_trust::frame::files::{CallKind, WidePath};
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
    PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentThreadId;

/// Bytes of the section.
pub const SIZE: usize = 4096;
pub const HEADER_LEN: usize = 64;
pub const SLOTS: usize = 4;
pub const SLOT_LEN: usize = 128;
pub const MAGIC: [u8; 4] = *b"FCRB";
pub const VERSION: u16 = 1;
/// UTF-16 units of the extension kept in a slot.
pub const EXT_UNITS: usize = 16;
/// Bit 31 of the busy word: a kill was decided; no invoke may start.
pub const KILLING: u32 = 1 << 31;

/// Header offsets.
pub const OFF_MAGIC: usize = 0;
pub const OFF_VERSION: usize = 4;
pub const OFF_SLOTS: usize = 6;
pub const OFF_HOST_PID: usize = 8;
pub const OFF_BUSY: usize = 12;
/// Offsets inside a slot.
pub const SLOT_SEQ: usize = 0;
pub const SLOT_KIND: usize = 4;
pub const SLOT_FLAGS: usize = 6;
pub const SLOT_STARTED: usize = 8;
pub const SLOT_EXT: usize = 16;
pub const SLOT_CALLS: usize = 48;

// The slots fit the section, and a slot's extension ends before its call count.
const _: () = assert!(HEADER_LEN + SLOTS * SLOT_LEN <= SIZE && SLOT_EXT + 2 * EXT_UNITS <= SLOT_CALLS);

/// The STAs' slots.
pub const SLOT_MENU: u8 = 0;
pub const SLOT_EXEC: u8 = 1;
pub const SLOT_IMAGE: [u8; 2] = [2, 3];

/// A crumbs region: SIZE bytes, aligned to 8, possibly shared with another process. Only atomics touch it.
#[derive(Clone, Copy, Debug)]
pub struct Section {
    base: *mut u8,
}

// SAFETY: every access to the region goes through atomics, and the region outlives every copy (from_raw's contract).
unsafe impl Send for Section {}
// SAFETY: as above.
unsafe impl Sync for Section {}

/// One slot as read by the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotView {
    pub seq: u32,
    pub in_call: bool,
    pub kind: Option<CallKind>,
    pub flags: u16,
    pub started_ms: u64,
    pub ext: String,
    pub calls: u32,
}

/// The CallKind of a slot's kind field.
pub fn call_kind(v: u16) -> Option<CallKind> {
    use CallKind::*;
    [
        QueryContextMenu,
        HandleMenuMsg,
        InvokeCommand,
        ModernItems,
        Exec,
        Icon,
        Thumb,
        ClassIcon,
        ShellNew,
        Pins,
        VerbName,
    ]
    .into_iter()
    .find(|k| *k as u16 == v)
}

/// The extension of a path's last component, lower case, without the dot, at most EXT_UNITS units ("" for none).
pub fn ext_of(path: &WidePath) -> String {
    let name = path.file_name().map(String::from_utf16_lossy).unwrap_or_default();
    match name.rfind('.') {
        Some(dot) => truncate(&name.get(dot + 1..).unwrap_or_default().to_lowercase()),
        None => String::new(),
    }
}

/// `s` cut to at most EXT_UNITS UTF-16 units, at a character boundary.
fn truncate(s: &str) -> String {
    let mut units = 0;
    s.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= EXT_UNITS
        })
        .collect()
}

impl Section {
    /// # Safety
    /// `base` points to SIZE writable bytes aligned to 8 that stay mapped while any copy of the Section is used.
    pub unsafe fn from_raw(base: *mut u8) -> Section {
        Section { base }
    }

    fn u16_at(&self, off: usize) -> &AtomicU16 {
        debug_assert!(off + 2 <= SIZE && off.is_multiple_of(2));
        // SAFETY: in bounds and aligned (the layout's offsets, checked above in debug builds); from_raw's contract
        // keeps the region alive; AtomicU16 has the size and alignment of u16.
        unsafe { &*self.base.add(off).cast::<AtomicU16>() }
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        debug_assert!(off + 4 <= SIZE && off.is_multiple_of(4));
        // SAFETY: as u16_at, for a 4-aligned offset.
        unsafe { &*self.base.add(off).cast::<AtomicU32>() }
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        debug_assert!(off + 8 <= SIZE && off.is_multiple_of(8));
        // SAFETY: as u16_at, for an 8-aligned offset (the base is aligned to 8).
        unsafe { &*self.base.add(off).cast::<AtomicU64>() }
    }

    fn slot_off(slot: u8) -> usize {
        HEADER_LEN + usize::from(slot.min(SLOTS as u8 - 1)) * SLOT_LEN
    }

    /// Writes the header (the client, once, before the host starts).
    pub fn init(&self) {
        self.u32_at(OFF_MAGIC).store(u32::from_le_bytes(MAGIC), Relaxed);
        self.u16_at(OFF_VERSION).store(VERSION, Relaxed);
        self.u16_at(OFF_SLOTS).store(SLOTS as u16, Release);
    }

    /// The header is ours.
    pub fn valid(&self) -> bool {
        self.u32_at(OFF_MAGIC).load(Acquire) == u32::from_le_bytes(MAGIC)
            && self.u16_at(OFF_VERSION).load(Relaxed) == VERSION
            && self.u16_at(OFF_SLOTS).load(Relaxed) == SLOTS as u16
    }

    pub fn set_host_pid(&self, pid: u32) {
        self.u32_at(OFF_HOST_PID).store(pid, Release);
    }

    pub fn host_pid(&self) -> u32 {
        self.u32_at(OFF_HOST_PID).load(Acquire)
    }

    /// The busy word as it is.
    pub fn busy(&self) -> u32 {
        self.u32_at(OFF_BUSY).load(Acquire)
    }

    /// Enters a call that must not be cut; Err once a killer has set KILLING.
    pub fn busy_enter(&self) -> Result<BusyGuard, Killing> {
        let busy = self.u32_at(OFF_BUSY);
        let mut n = busy.load(Acquire);
        loop {
            if n & KILLING != 0 {
                return Err(Killing);
            }
            match busy.compare_exchange_weak(n, n + 1, AcqRel, Acquire) {
                Ok(_) => return Ok(BusyGuard { section: Some(*self) }),
                Err(now) => n = now,
            }
        }
    }

    /// A killer's only way to a kill: true when no protected call runs and KILLING is now set (never cleared).
    pub fn try_kill(&self) -> bool {
        self.u32_at(OFF_BUSY).compare_exchange(0, KILLING, AcqRel, Acquire).is_ok()
    }

    /// Sets KILLING whatever runs (the host's 1 h cap: no new invoke may start, the running one is cut).
    pub fn force_killing(&self) {
        self.u32_at(OFF_BUSY).fetch_or(KILLING, AcqRel);
    }

    /// Opens a call in `slot`: the fields first, then seq odd with Release.
    pub fn begin(&self, slot: u8, kind: CallKind, ext: &str, now_ms: u64) {
        let at = Self::slot_off(slot);
        let seq = self.u32_at(at + SLOT_SEQ);
        let s = seq.load(Relaxed);
        let even = s.wrapping_add(s & 1);
        self.u16_at(at + SLOT_KIND).store(kind as u16, Relaxed);
        self.u16_at(at + SLOT_FLAGS).store(0, Relaxed);
        self.u64_at(at + SLOT_STARTED).store(now_ms, Relaxed);
        let mut units = truncate(ext).encode_utf16().collect::<Vec<u16>>();
        units.resize(EXT_UNITS, 0);
        for (i, u) in units.iter().enumerate() {
            self.u16_at(at + SLOT_EXT + 2 * i).store(*u, Relaxed);
        }
        seq.store(even.wrapping_add(1), Release);
    }

    /// Closes the call in `slot`: seq even, one more call counted.
    pub fn end(&self, slot: u8) {
        let at = Self::slot_off(slot);
        self.u32_at(at + SLOT_CALLS).fetch_add(1, Relaxed);
        let seq = self.u32_at(at + SLOT_SEQ);
        let s = seq.load(Relaxed);
        seq.store(s.wrapping_add(s & 1), Release);
    }

    /// One slot by the seqlock rule: re-read while seq moved during the read and the host is alive; after the host
    /// died, the values as they are.
    pub fn read(&self, slot: u8, host_alive: bool) -> SlotView {
        let at = Self::slot_off(slot);
        let seq = self.u32_at(at + SLOT_SEQ);
        let mut tries = 0;
        loop {
            let s1 = seq.load(Acquire);
            let kind = self.u16_at(at + SLOT_KIND).load(Relaxed);
            let flags = self.u16_at(at + SLOT_FLAGS).load(Relaxed);
            let started_ms = self.u64_at(at + SLOT_STARTED).load(Relaxed);
            let units: Vec<u16> = (0..EXT_UNITS)
                .map(|i| self.u16_at(at + SLOT_EXT + 2 * i).load(Relaxed))
                .take_while(|&u| u != 0)
                .collect();
            let calls = self.u32_at(at + SLOT_CALLS).load(Relaxed);
            fence(Acquire);
            let s2 = seq.load(Relaxed);
            tries += 1;
            if s1 == s2 || !host_alive || tries >= 1000 {
                return SlotView {
                    seq: s1,
                    in_call: s1 & 1 == 1,
                    kind: call_kind(kind),
                    flags,
                    started_ms,
                    ext: String::from_utf16_lossy(&units),
                    calls,
                };
            }
            std::hint::spin_loop();
        }
    }
}

/// A guard of a call that must not be cut; dropping it ends the call.
pub struct BusyGuard {
    section: Option<Section>,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        if let Some(s) = self.section {
            s.u32_at(OFF_BUSY).fetch_sub(1, Release);
        }
    }
}

/// A kill decision was taken: no invoke may start.
#[derive(Debug, PartialEq, Eq)]
pub struct Killing;

/// A guard of one extension call in this thread's slot; dropping it closes the call.
pub struct CallGuard {
    section: Option<(Section, u8)>,
    slot: Option<u8>,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(d) = self.slot.and_then(|s| SLOT_DEPTH.get(usize::from(s))) {
            d.fetch_sub(1, Relaxed);
        }
        if let Some((s, slot)) = self.section {
            s.end(slot);
        }
    }
}

/// The host's section, set once at start (install).
static SECTION: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// The OS thread bound to each slot (0: none) and the call depth of each slot, touched by its own thread only (the
/// workspace keeps thread-local storage out of its sources).
static SLOT_THREAD: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
static SLOT_DEPTH: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];

fn this_thread() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentThreadId() }
}

/// The slot bound to this thread, if any.
fn my_slot() -> Option<u8> {
    let tid = this_thread();
    SLOT_THREAD.iter().position(|t| t.load(Acquire) == tid).map(|i| i as u8)
}

/// Makes `s` the host's section (once, before any STA starts; the mapping is never unmapped).
pub fn install(s: Section) {
    SECTION.store(s.base, Release);
}

/// The host's section, if installed.
pub fn installed() -> Option<Section> {
    let p = SECTION.load(Acquire);
    // SAFETY: only install stores here, with a section whose mapping lives for the rest of the process.
    (!p.is_null()).then(|| unsafe { Section::from_raw(p) })
}

/// The STA binds its slot once. Windows reuses thread ids, so another slot that still names this thread's id (left by
/// a thread that ended) is cleared first: my_slot then finds this slot only.
pub fn bind(slot: u8) {
    let tid = this_thread();
    let i = usize::from(slot.min(SLOTS as u8 - 1));
    for (j, t) in SLOT_THREAD.iter().enumerate() {
        if j != i {
            let _ = t.compare_exchange(tid, 0, AcqRel, Acquire);
        }
    }
    if let (Some(t), Some(d)) = (SLOT_THREAD.get(i), SLOT_DEPTH.get(i)) {
        d.store(0, Relaxed);
        t.store(tid, Release);
    }
}

/// The STA gives its slot up before its thread ends (no call of its is open by then), so a later thread that gets the
/// same id writes no crumb into it.
pub fn unbind() {
    let tid = this_thread();
    for t in &SLOT_THREAD {
        let _ = t.compare_exchange(tid, 0, AcqRel, Acquire);
    }
}

/// Every extension call runs inside an enter guard.
pub fn enter(kind: CallKind, ext: &str) -> CallGuard {
    let Some(slot) = my_slot() else {
        return CallGuard { section: None, slot: None };
    };
    let outer = SLOT_DEPTH.get(usize::from(slot)).is_some_and(|d| d.fetch_add(1, Relaxed) == 0);
    let section = match (outer, installed()) {
        (true, Some(s)) => {
            // SAFETY: GetTickCount64 has no preconditions.
            s.begin(slot, kind, ext, unsafe { GetTickCount64() });
            Some((s, slot))
        }
        _ => None,
    };
    CallGuard { section, slot: Some(slot) }
}

/// Every invoke runs inside a busy guard; fails once a killer has set KILLING. Without a section (no client) there is
/// nobody to kill the host, and the guard does nothing.
pub fn busy_enter() -> Result<BusyGuard, Killing> {
    match installed() {
        Some(s) => s.busy_enter(),
        None => Ok(BusyGuard { section: None }),
    }
}

/// The host's own kill decision (parent watch, idle exit, the reader at end of input): true when it may end now.
pub fn try_kill() -> bool {
    installed().is_none_or(|s| s.try_kill())
}

/// The shared memory of one host: created by the client, inherited by the host.
pub struct Mapping {
    handle: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the view is plain shared memory touched only through atomics; the handle is a kernel handle.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// The client's new section, its header written, not inheritable (anon::inheritable for the spawn).
    pub fn create() -> io::Result<Mapping> {
        // SAFETY: a pagefile-backed mapping of SIZE bytes; the returned handle is owned below.
        let h = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, SIZE as u32, None) }?;
        // SAFETY: just created, owned here once.
        let handle = unsafe { OwnedHandle::from_raw_handle(h.0) };
        let m = Self::map(handle)?;
        m.section().init();
        Ok(m)
    }

    /// The host's view of the section whose handle it inherited (owned from here on); refused when the header is not
    /// ours.
    pub fn open(h: HANDLE) -> io::Result<Mapping> {
        // SAFETY: the handle value the client passed for this process; owned from here on.
        let handle = unsafe { OwnedHandle::from_raw_handle(h.0) };
        let m = Self::map(handle)?;
        if !m.section().valid() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a crumbs section"));
        }
        Ok(m)
    }

    fn map(handle: OwnedHandle) -> io::Result<Mapping> {
        // SAFETY: a valid section handle; the view is unmapped in Drop.
        let view = unsafe { MapViewOfFile(HANDLE(handle.as_raw_handle()), FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, SIZE) };
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping { handle, view })
    }

    pub fn handle(&self) -> &OwnedHandle {
        &self.handle
    }

    /// The section of this view; valid while the Mapping lives (a view is page-aligned).
    pub fn section(&self) -> Section {
        // SAFETY: the view holds SIZE bytes, page-aligned, mapped until Drop; callers keep the Mapping alive.
        unsafe { Section::from_raw(self.view.Value.cast()) }
    }

    /// Keeps the view mapped for the rest of the process (the host's own section).
    pub fn leak(self) -> Section {
        let s = self.section();
        std::mem::forget(self);
        s
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: mapped in `map`, unmapped once, here.
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    /// A section in this process's heap, aligned to 8.
    struct Owned(Box<[AtomicU64]>);

    impl Owned {
        fn new() -> Owned {
            Owned((0..SIZE / 8).map(|_| AtomicU64::new(0)).collect())
        }
        fn section(&self) -> Section {
            // SAFETY: SIZE bytes aligned to 8, alive while `self` is.
            unsafe { Section::from_raw(self.0.as_ptr().cast_mut().cast()) }
        }
        fn bytes(&self) -> Vec<u8> {
            self.0.iter().flat_map(|w| w.load(Relaxed).to_le_bytes()).collect()
        }
    }

    #[test]
    fn layout_offsets_pinned() {
        assert_eq!((SIZE, HEADER_LEN, SLOTS, SLOT_LEN, EXT_UNITS), (4096, 64, 4, 128, 16));
        assert_eq!((OFF_MAGIC, OFF_VERSION, OFF_SLOTS, OFF_HOST_PID, OFF_BUSY), (0, 4, 6, 8, 12));
        assert_eq!((SLOT_SEQ, SLOT_KIND, SLOT_FLAGS, SLOT_STARTED, SLOT_EXT, SLOT_CALLS), (0, 4, 6, 8, 16, 48));
        let o = Owned::new();
        let s = o.section();
        s.init();
        s.set_host_pid(0x0102_0304);
        let _busy = s.busy_enter().unwrap();
        s.begin(1, CallKind::Thumb, "ab", 0x1122_3344_5566_7788);
        s.end(1);
        s.begin(1, CallKind::Thumb, "ab", 0x1122_3344_5566_7788);
        let b = o.bytes();
        assert_eq!(&b[0..4], b"FCRB");
        assert_eq!(&b[4..8], &[1, 0, 4, 0]);
        assert_eq!(&b[8..12], &[4, 3, 2, 1]);
        assert_eq!(&b[12..16], &[1, 0, 0, 0], "one busy call");
        let at = HEADER_LEN + SLOT_LEN;
        assert_eq!(&b[at..at + 4], &[3, 0, 0, 0], "seq 3: the second call, in progress");
        assert_eq!(&b[at + 4..at + 6], &(CallKind::Thumb as u16).to_le_bytes());
        assert_eq!(&b[at + 8..at + 16], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&b[at + 16..at + 22], &[b'a', 0, b'b', 0, 0, 0]);
        assert_eq!(&b[at + 48..at + 52], &[1, 0, 0, 0], "one call ended");
        assert!(b[HEADER_LEN..at].iter().all(|&x| x == 0), "slot 0 untouched");
    }

    #[test]
    fn slot_round_trip() {
        let o = Owned::new();
        let s = o.section();
        s.init();
        assert!(s.valid());
        let idle = s.read(2, true);
        assert!(!idle.in_call && idle.kind.is_none() && idle.calls == 0);
        s.begin(2, CallKind::Icon, "JPEG-Long-Extension-Name", 77);
        let v = s.read(2, true);
        assert!(v.in_call);
        assert_eq!((v.kind, v.started_ms, v.ext.as_str()), (Some(CallKind::Icon), 77, "JPEG-Long-Extens"));
        s.end(2);
        let v = s.read(2, true);
        assert!(!v.in_call && v.calls == 1 && v.seq == 2);
        for k in 1..=11u16 {
            assert_eq!(call_kind(k).map(|c| c as u16), Some(k));
        }
        assert_eq!(call_kind(0), None);
        assert_eq!(call_kind(12), None);
    }

    #[test]
    fn odd_seq_reads_as_in_call() {
        let o = Owned::new();
        let s = o.section();
        s.u32_at(HEADER_LEN + SLOT_SEQ).store(5, Release);
        s.u16_at(HEADER_LEN + SLOT_KIND).store(CallKind::QueryContextMenu as u16, Release);
        let v = s.read(0, false);
        assert!(v.in_call && v.seq == 5 && v.kind == Some(CallKind::QueryContextMenu));
        s.end(0);
        assert!(!s.read(0, true).in_call, "end makes it even");
        s.begin(0, CallKind::VerbName, "", 1);
        assert_eq!(s.read(0, true).seq, 7);
    }

    #[test]
    fn killing_is_sticky() {
        let o = Owned::new();
        let s = o.section();
        let g = s.busy_enter().unwrap();
        assert!(!s.try_kill(), "busy: no kill");
        drop(g);
        assert!(s.try_kill());
        assert_eq!(s.busy(), KILLING);
        assert_eq!(s.busy_enter().err(), Some(Killing));
        assert!(!s.try_kill(), "a second killer does not win again");
        s.force_killing();
        assert_eq!(s.busy(), KILLING, "never cleared");
        let o2 = Owned::new();
        let s2 = o2.section();
        let _g = s2.busy_enter().unwrap();
        s2.force_killing();
        assert_eq!(s2.busy(), KILLING | 1, "the 1 h cap: KILLING over a running call");
        assert_eq!(s2.busy_enter().err(), Some(Killing));
    }

    #[test]
    fn kill_never_races_invoke() {
        const ROUNDS: usize = 1000;
        const PER_ROUND: usize = 200; // 200,000 busy_enter iterations in all
        let clock = Arc::new(AtomicU64::new(0));
        for _ in 0..ROUNDS {
            let o = Owned::new();
            let s = o.section();
            let starts = Mutex::new(Vec::new());
            let kill_at = AtomicU64::new(u64::MAX);
            let gate = Barrier::new(2);
            std::thread::scope(|sc| {
                sc.spawn(|| {
                    gate.wait();
                    let mut mine = Vec::new();
                    for _ in 0..PER_ROUND {
                        match s.busy_enter() {
                            Ok(g) => {
                                mine.push(clock.fetch_add(1, AcqRel));
                                drop(g);
                            }
                            Err(Killing) => break,
                        }
                    }
                    starts.lock().unwrap().extend(mine);
                });
                sc.spawn(|| {
                    gate.wait();
                    while !s.try_kill() {
                        std::hint::spin_loop();
                    }
                    kill_at.store(clock.fetch_add(1, AcqRel), Release);
                });
            });
            let k = kill_at.load(Acquire);
            assert!(starts.lock().unwrap().iter().all(|&t| t < k), "an invoke started after the kill decision");
            for _ in 0..3 {
                assert_eq!(s.busy_enter().err(), Some(Killing), "busy_enter after KILLING always fails");
            }
        }
    }

    #[test]
    fn ext_of_table() {
        for (p, want) in [
            (r"C:\a\b.TXT", "txt"),
            (r"C:\a\noext", ""),
            (r"C:\a.dir\file", ""),
            (r"C:\a\archive.tar.GZ", "gz"),
            (r"C:\a\.hidden", "hidden"),
            (r"C:\a\x.ÄBÇ", "äbç"),
            (r"C:\a\x.abcdefghijklmnopqrst", "abcdefghijklmnop"),
        ] {
            assert_eq!(ext_of(&WidePath::parse(p).unwrap()), want, "{p}");
        }
    }

    #[test]
    fn a_thread_id_is_bound_to_one_slot() {
        // The image slots, which no other unit test binds, on a thread of this test's own.
        std::thread::spawn(|| {
            let [a, b] = SLOT_IMAGE;
            bind(a);
            assert_eq!(my_slot(), Some(a));
            // The same id bound to another slot: Windows reused the id of an STA that ended without a successor.
            bind(b);
            assert_eq!(my_slot(), Some(b), "the slot bound last");
            assert_eq!(SLOT_THREAD[usize::from(a)].load(Acquire), 0, "the old slot no longer names this id");
            unbind();
            assert_eq!(my_slot(), None, "an STA that ends leaves no slot naming its id");
            assert_eq!(SLOT_THREAD[usize::from(b)].load(Acquire), 0);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn guards_without_a_section_do_nothing() {
        bind(SLOT_MENU);
        let g = enter(CallKind::Icon, "txt");
        drop(g);
        let b = busy_enter();
        assert!(b.is_ok());
    }
}
