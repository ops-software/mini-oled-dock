//! The raw process snapshot (SystemProcessInformation) and its bounds-checked walk.
use crate::error::SysmonError;
use crate::table::{ProcKey, ProcRow, ProcTable};
use std::ops::Range;
use windows::core::PWSTR;
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::GetLongPathNameW;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
};

/// SYSTEM_PROCESS_INFORMATION, x64.
pub const ENTRY_SIZE: usize = 0x100;
/// SYSTEM_THREAD_INFORMATION, x64.
pub const THREAD_SIZE: usize = 0x50;
pub const INITIAL_BUFFER: usize = 256 * 1024;
pub const MAX_BUFFER: usize = 16 << 20;

const MAX_THREADS: u32 = 65_535;
const MAX_ENTRIES: u32 = 65_536;
const MAX_NAME_BYTES: usize = 520;
const GROW_STEP: usize = 64 * 1024;
const MAX_ATTEMPTS: u8 = 4;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

// Field offsets inside one SYSTEM_PROCESS_INFORMATION entry (x64).
const O_NEXT: usize = 0;
const O_THREADS: usize = 4;
const O_WS_PRIVATE: usize = 8;
const O_CREATE: usize = 32;
const O_USER: usize = 40;
const O_KERNEL: usize = 48;
const O_NAME_LEN: usize = 56;
const O_NAME_MAX: usize = 58;
const O_NAME_PTR: usize = 64;
const O_PRIO: usize = 72;
const O_PID: usize = 80;
const O_PARENT: usize = 88;
const O_HANDLES: usize = 96;
const O_SESSION: usize = 100;
const O_KEY: usize = 104;
const O_WS_PEAK: usize = 136;
const O_WS: usize = 144;
const O_PRIVATE: usize = 200;
const O_IO_READ: usize = 232;
const O_IO_WRITE: usize = 240;
const O_IO_OTHER: usize = 248;

// Field offsets inside one SYSTEM_THREAD_INFORMATION (x64).
const T_CREATE: usize = 16;
const T_WAIT: usize = 24;
const T_START: usize = 32;
/// Length, in UTF-16 units, every masked image name gets ("x"): no name length or shape survives a capture.
pub const MASK_UNITS: usize = 1;
/// FILETIME of the invented instant (2000-01-01 00:00:00 UTC) at which the earliest process of a sanitised capture starts.
pub const INVENTED_START: u64 = 125_911_584_000_000_000;

/// Base address every sanitised buffer's name pointers are rebased on (an invented constant; the capture header holds it).
pub const INVENTED_BASE: usize = 0x0000_0200_0000_0000;
/// Create times below this (FILETIME, about year 1830) are not wall-clock values and are never shifted.
pub const REAL_TIME_MIN: i64 = 1 << 56;
/// After the shift every wall-clock create time lies in [INVENTED_START, INVENTED_START + this] (one year).
pub const TIME_WINDOW: i64 = 365 * 864_000_000_000;

/// Image names the capture sanitiser keeps (ASCII case-insensitive); every other name is overwritten.
pub const SYSTEM_IMAGES: [&str; 40] = [
    "System",
    "Registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "Memory Compression",
    "Secure System",
    "LsaIso.exe",
    "svchost.exe",
    "dwm.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "ctfmon.exe",
    "conhost.exe",
    "RuntimeBroker.exe",
    "taskhostw.exe",
    "SearchHost.exe",
    "SearchIndexer.exe",
    "StartMenuExperienceHost.exe",
    "ShellExperienceHost.exe",
    "TextInputHost.exe",
    "ApplicationFrameHost.exe",
    "SystemSettings.exe",
    "dllhost.exe",
    "audiodg.exe",
    "spoolsv.exe",
    "WmiPrvSE.exe",
    "MsMpEng.exe",
    "NisSrv.exe",
    "SecurityHealthService.exe",
    "smartscreen.exe",
    "WUDFHost.exe",
    "dasHost.exe",
    "backgroundTaskHost.exe",
    "dock.exe",
    "tasks.exe",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    pub bytes: u32,
    pub entries: u32,
    pub threads: u32,
    pub retries: u8,
    pub capacity: u32,
}

fn le_u32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0)
}

fn le_u16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).and_then(|s| s.try_into().ok()).map(u16::from_le_bytes).unwrap_or(0)
}

fn le_u64(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes).unwrap_or(0)
}

/// A signed 64-bit counter; a negative value reads as 0.
fn le_i64_pos(b: &[u8], o: usize) -> u64 {
    let v = le_u64(b, o) as i64;
    v.max(0) as u64
}

fn layout(offset: usize, what: &'static str) -> SysmonError {
    SysmonError::Layout { offset: offset.min(u32::MAX as usize) as u32, what }
}

/// First pass: hops NextEntryOffset (the cheap rules only) and counts the entries.
fn count_entries(buf: &[u8]) -> Result<usize, SysmonError> {
    let mut off = 0usize;
    let mut count = 0usize;
    loop {
        if count >= MAX_ENTRIES as usize {
            return Err(layout(off, "too many entries"));
        }
        match off.checked_add(ENTRY_SIZE) {
            Some(end) if end <= buf.len() => {}
            _ => return Err(layout(off, "truncated entry")),
        }
        count += 1;
        let next = le_u32(buf, off + O_NEXT) as usize;
        if next == 0 {
            return Ok(count);
        }
        if next < ENTRY_SIZE || off.checked_add(next).is_none_or(|n| n > buf.len()) {
            return Err(layout(off, "next entry offset"));
        }
        off += next;
    }
}

/// What a finished walk counted.
struct Walked {
    entries: u32,
    threads: u32,
}

/// The full walk, every rule of the layout; visit gets each entry's 0x100 bytes, the byte range of its image name inside
/// the buffer and its offset.
fn walk(
    buf: &[u8],
    base: usize,
    mut visit: impl FnMut(&[u8], Option<Range<usize>>, usize) -> Result<(), SysmonError>,
) -> Result<Walked, SysmonError> {
    let mut off = 0usize;
    let mut entries = 0u32;
    let mut threads_total = 0u32;
    loop {
        if entries >= MAX_ENTRIES {
            return Err(layout(off, "too many entries"));
        }
        let end = match off.checked_add(ENTRY_SIZE) {
            Some(end) if end <= buf.len() => end,
            _ => return Err(layout(off, "truncated entry")),
        };
        if !off.is_multiple_of(8) {
            return Err(layout(off, "unaligned entry"));
        }
        let e = &buf[off..end];
        let next = le_u32(e, O_NEXT) as usize;
        let threads = le_u32(e, O_THREADS);
        if threads > MAX_THREADS {
            return Err(layout(off, "thread count"));
        }
        let tail = ENTRY_SIZE + THREAD_SIZE * threads as usize;
        if next != 0 {
            if next < tail {
                return Err(layout(off, "next entry offset too small"));
            }
            if off.checked_add(next).is_none_or(|n| n > buf.len()) {
                return Err(layout(off, "next entry offset past the end"));
            }
        } else if off + tail > buf.len() {
            return Err(layout(off, "truncated thread array"));
        }
        let name_len = le_u16(e, O_NAME_LEN) as usize;
        if !name_len.is_multiple_of(2) || name_len > MAX_NAME_BYTES {
            return Err(layout(off, "image name length"));
        }
        let name = if name_len > 0 {
            let ptr = le_u64(e, O_NAME_PTR) as usize;
            let rel = match ptr.checked_sub(base) {
                Some(rel) => rel,
                None => return Err(layout(off, "image name pointer")),
            };
            if !rel.is_multiple_of(2) || rel.checked_add(name_len).is_none_or(|n| n > buf.len()) {
                return Err(layout(off, "image name pointer"));
            }
            Some(rel..rel + name_len)
        } else {
            None
        };
        visit(e, name, off)?;
        entries += 1;
        threads_total = threads_total.saturating_add(threads);
        if next == 0 {
            return Ok(Walked { entries, threads: threads_total });
        }
        off += next;
    }
}

/// Pure walk of one SystemProcessInformation buffer captured at address base. Clears out, fills it, calls out.finish().
pub fn parse(buf: &[u8], base: usize, out: &mut ProcTable) -> Result<ReadStats, SysmonError> {
    out.clear();
    let count = count_entries(buf)?;
    out.reserve_rows(count);
    let walked = walk(buf, base, |e, name, off| {
        let pid = le_u64(e, O_PID);
        let pid = u32::try_from(pid).map_err(|_| layout(off, "process id"))?;
        let mut units = [0u16; MAX_NAME_BYTES / 2];
        let mut n = 0usize;
        if let Some(r) = name {
            if let Some(bytes) = buf.get(r) {
                for (slot, pair) in units.iter_mut().zip(bytes.chunks_exact(2)) {
                    *slot = u16::from_le_bytes([pair[0], pair[1]]);
                    n += 1;
                }
            }
        }
        let image = out.names.intern(&units[..n]);
        let parent_pid = u32::try_from(le_u64(e, O_PARENT)).unwrap_or(0);
        out.push(ProcRow {
            key: ProcKey { pid, created: le_i64_pos(e, O_CREATE) },
            parent_pid,
            parent: None,
            session: le_u32(e, O_SESSION),
            image,
            cpu_100ns: le_i64_pos(e, O_USER).saturating_add(le_i64_pos(e, O_KERNEL)),
            ws: le_u64(e, O_WS),
            ws_peak: le_u64(e, O_WS_PEAK),
            ws_private: le_i64_pos(e, O_WS_PRIVATE),
            private_bytes: le_u64(e, O_PRIVATE),
            io_read: le_i64_pos(e, O_IO_READ),
            io_write: le_i64_pos(e, O_IO_WRITE),
            io_other: le_i64_pos(e, O_IO_OTHER),
            threads: le_u32(e, O_THREADS),
            handles: le_u32(e, O_HANDLES),
            base_prio: le_u32(e, O_PRIO) as i32,
        });
        Ok(())
    });
    let walked = match walked {
        Ok(w) => w,
        Err(e) => {
            out.clear();
            return Err(e);
        }
    };
    out.finish();
    if out.rows.windows(2).any(|w| w[0].key.pid == w[1].key.pid) {
        out.clear();
        return Err(layout(0, "duplicate process id"));
    }
    Ok(ReadStats {
        bytes: buf.len().min(u32::MAX as usize) as u32,
        entries: walked.entries,
        threads: walked.threads,
        retries: 0,
        capacity: 0,
    })
}

/// Anonymises a captured buffer in place and returns the number of names rewritten (the plan's TS4 signature). See
/// sanitize_len; the buffer keeps its slice length, the bytes after the last entry are zero and the name pointers are
/// rebased on INVENTED_BASE (parse the result with that base).
pub fn sanitize(buf: &mut [u8], base: usize) -> Result<u32, SysmonError> {
    sanitize_len(buf, base).map(|(n, _)| n)
}

/// The sanitiser proper: REBUILDS the buffer compactly, so that no field and no spacing depends on an old name. Returns
/// (names rewritten, length of the rebuilt snapshot); a capture writes only that many bytes. For each entry, in walk order:
/// the fixed part and the thread array are copied, the name is laid out right after them (a name outside SYSTEM_IMAGES
/// becomes "x" with Length 2 and MaximumLength 4; a system name keeps its text, Length and MaximumLength), the entry is
/// padded to 8 bytes, NextEntryOffset is recomputed (0 on the last) and the name pointer points at the new place under
/// INVENTED_BASE. Bytes after the last entry are zeroed. Create times (processes and threads) at or above REAL_TIME_MIN
/// are shifted by one constant so that the earliest starts at INVENTED_START; after the shift each must lie within
/// TIME_WINDOW of it, else the call fails and leaves buf untouched. The unique process key, the thread start addresses and
/// the thread wait ticks are zeroed. Counts, sizes, pids and CPU times stay. buf must be a valid snapshot captured at base.
pub fn sanitize_len(buf: &mut [u8], base: usize) -> Result<(u32, usize), SysmonError> {
    struct Item {
        off: usize,
        threads: usize,
        name: Option<Range<usize>>,
        max: usize,
    }
    let mut items: Vec<Item> = Vec::new();
    walk(buf, base, |e, name, off| {
        items.push(Item { off, threads: le_u32(e, O_THREADS) as usize, name, max: le_u16(e, O_NAME_MAX) as usize });
        Ok(())
    })?;
    let real = |v: i64| v >= REAL_TIME_MIN;
    let mut earliest: Option<i64> = None;
    for it in &items {
        let times = std::iter::once(le_u64(buf, it.off + O_CREATE) as i64)
            .chain((0..it.threads).map(|t| le_u64(buf, it.off + ENTRY_SIZE + THREAD_SIZE * t + T_CREATE) as i64));
        for v in times.filter(|&v| real(v)) {
            earliest = Some(earliest.map_or(v, |m| m.min(v)));
        }
    }
    let delta = earliest.map_or(0, |m| (INVENTED_START as i64).wrapping_sub(m));
    // A real time moves by delta and must then lie in the window; a time below the floor stays.
    let moved = |off: usize, v: i64| -> Result<i64, SysmonError> {
        if !real(v) {
            return Ok(v);
        }
        let n = v.saturating_add(delta);
        if n < INVENTED_START as i64 || n > (INVENTED_START as i64).saturating_add(TIME_WINDOW) {
            return Err(layout(off, "create time window"));
        }
        Ok(n)
    };
    let mut out: Vec<u8> = Vec::with_capacity(buf.len());
    let mut done = 0u32;
    let last = items.len().saturating_sub(1);
    for (i, it) in items.iter().enumerate() {
        let start = out.len();
        let tail = ENTRY_SIZE + THREAD_SIZE * it.threads;
        out.extend_from_slice(&buf[it.off..it.off + tail]);
        let created = moved(it.off, le_u64(&out, start + O_CREATE) as i64)?;
        out[start + O_CREATE..start + O_CREATE + 8].copy_from_slice(&created.to_le_bytes());
        out[start + O_KEY..start + O_KEY + 8].fill(0);
        for t in 0..it.threads {
            let th = start + ENTRY_SIZE + THREAD_SIZE * t;
            let c = moved(it.off, le_u64(&out, th + T_CREATE) as i64)?;
            out[th + T_CREATE..th + T_CREATE + 8].copy_from_slice(&c.to_le_bytes());
            out[th + T_WAIT..th + T_WAIT + 4].fill(0);
            out[th + T_START..th + T_START + 8].fill(0);
        }
        let (len, max, ptr) = match &it.name {
            None => (0usize, 0usize, 0usize),
            Some(r) => {
                let units: Vec<u16> =
                    buf[r.clone()].chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
                let text = String::from_utf16_lossy(&units);
                let (bytes, max) = if SYSTEM_IMAGES.iter().any(|s| s.eq_ignore_ascii_case(&text)) {
                    (buf[r.clone()].to_vec(), it.max.max(r.len()))
                } else {
                    done += 1;
                    let x: Vec<u8> = std::iter::repeat_n([b'x', 0], MASK_UNITS).flatten().collect();
                    let max = x.len() + 2;
                    (x, max)
                };
                let at = out.len();
                out.extend_from_slice(&bytes);
                out.resize(at + max, 0);
                (bytes.len(), max, INVENTED_BASE + at)
            }
        };
        out.resize(out.len().div_ceil(8) * 8, 0);
        let size = if i == last { 0 } else { out.len() - start };
        out[start + O_NEXT..start + O_NEXT + 4].copy_from_slice(&(size as u32).to_le_bytes());
        out[start + O_NAME_LEN..start + O_NAME_LEN + 2].copy_from_slice(&(len as u16).to_le_bytes());
        out[start + O_NAME_MAX..start + O_NAME_MAX + 2].copy_from_slice(&(max as u16).to_le_bytes());
        out[start + O_NAME_PTR..start + O_NAME_PTR + 8].copy_from_slice(&(ptr as u64).to_le_bytes());
    }
    if out.len() > buf.len() {
        return Err(layout(0, "rebuilt snapshot larger than the buffer"));
    }
    buf[..out.len()].copy_from_slice(&out);
    buf[out.len()..].fill(0);
    Ok((done, out.len()))
}

/// Next buffer size after a length mismatch: max(returned, current) x 5/4 rounded up to 64 KiB.
pub fn next_capacity(current: usize, returned: u32) -> Result<usize, SysmonError> {
    let want = (returned as usize).max(current).saturating_mul(5) / 4;
    let want = want.div_ceil(GROW_STEP).saturating_mul(GROW_STEP);
    if want > MAX_BUFFER {
        return Err(SysmonError::TooLarge { need: returned });
    }
    Ok(want)
}

/// The own-process cross-check: created, session and threads are fatal (the first failing one is named), the image
/// name alone is a note.
pub fn self_test_verdict(
    created: bool,
    session: bool,
    threads: bool,
    name: bool,
) -> Result<Option<&'static str>, SysmonError> {
    if !created {
        return Err(SysmonError::SelfTest("own process: created"));
    }
    if !session {
        return Err(SysmonError::SelfTest("own process: session"));
    }
    if !threads {
        return Err(SysmonError::SelfTest("own process: threads"));
    }
    Ok(if name { None } else { Some("own process: image name") })
}

/// The snapshot's image name equals the reported final component or the long-path one, ASCII case-insensitive.
pub fn name_matches(snapshot: &[u16], reported: &str, long: Option<&str>) -> bool {
    let text = String::from_utf16_lossy(snapshot);
    text.eq_ignore_ascii_case(reported) || long.is_some_and(|l| text.eq_ignore_ascii_case(l))
}

fn final_component(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Final component of QueryFullProcessImageNameW(own process) and of its GetLongPathNameW form; a failed call drops its form.
pub fn own_image_names() -> (Option<String>, Option<String>) {
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    // SAFETY: buf has len writable elements; len is in/out; the pseudo handle of the own process needs no rights.
    let ok = unsafe {
        QueryFullProcessImageNameW(GetCurrentProcess(), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    };
    if ok.is_err() || len == 0 || len as usize > buf.len() {
        return (None, None);
    }
    let reported = String::from_utf16_lossy(&buf[..len as usize]);
    let mut wide: Vec<u16> = reported.encode_utf16().collect();
    wide.push(0);
    let mut long_buf = vec![0u16; 32768];
    // SAFETY: wide is NUL-terminated and outlives the call; long_buf is a writable slice of its stated length.
    let n = unsafe { GetLongPathNameW(windows::core::PCWSTR(wide.as_ptr()), Some(&mut long_buf)) } as usize;
    let long = (n > 0 && n < long_buf.len()).then(|| String::from_utf16_lossy(&long_buf[..n]));
    (Some(final_component(&reported).to_string()), long.map(|l| final_component(&l).to_string()))
}

fn filetime_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

#[derive(Debug, Default)]
pub struct SnapshotReader {
    buf: Vec<u64>,
    len: usize,
    stats: ReadStats,
    checked: bool,
    fatal: Option<SysmonError>,
    note: Option<&'static str>,
}

impl SnapshotReader {
    pub fn new() -> SnapshotReader {
        SnapshotReader::default()
    }

    /// The first call also runs self_test.
    pub fn read(&mut self, out: &mut ProcTable) -> Result<(), SysmonError> {
        if let Some(e) = self.fatal {
            return Err(e);
        }
        if !self.checked {
            self.self_test()?;
        }
        self.read_raw(out)
    }

    /// One NtQuerySystemInformation round (growing and retrying) and the parse of what it returned.
    fn read_raw(&mut self, out: &mut ProcTable) -> Result<(), SysmonError> {
        if self.buf.is_empty() {
            self.buf = vec![0u64; INITIAL_BUFFER / 8];
        }
        let mut retries = 0u8;
        let mut attempt = 0u8;
        let ret = loop {
            attempt += 1;
            let cap = self.buf.len() * 8;
            let mut ret = 0u32;
            // SAFETY: the Vec owns cap initialised, 8-aligned bytes; the call writes at most cap bytes and ret.
            let st = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    self.buf.as_mut_ptr().cast(),
                    cap.min(u32::MAX as usize) as u32,
                    &mut ret,
                )
            };
            if st.0 >= 0 {
                break ret as usize;
            }
            if st.0 == STATUS_INFO_LENGTH_MISMATCH && attempt < MAX_ATTEMPTS {
                let cap = next_capacity(cap, ret)?;
                self.buf = vec![0u64; cap / 8];
                retries += 1;
                continue;
            }
            return Err(SysmonError::Status(st.0));
        };
        let cap = self.buf.len() * 8;
        if ret > cap {
            return Err(layout(0, "returned length past the buffer"));
        }
        self.len = ret;
        let base = self.buf.as_ptr() as usize;
        let mut stats = parse(self.bytes(), base, out)?;
        stats.retries = retries;
        stats.capacity = cap.min(u32::MAX as usize) as u32;
        self.stats = stats;
        Ok(())
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the Vec owns buf.len() x 8 initialised bytes, u8 has alignment 1, and len never exceeds them.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.len.min(self.buf.len() * 8)) }
    }

    /// created, session, threads fatal; a name-only mismatch is a note.
    pub fn self_test(&mut self) -> Result<(), SysmonError> {
        if let Some(e) = self.fatal {
            return Err(e);
        }
        let mut table = ProcTable::new();
        self.read_raw(&mut table)?;
        let pid = {
            // SAFETY: no arguments, no side effects.
            unsafe { GetCurrentProcessId() }
        };
        let Some(row) = table.find_pid(pid).copied() else {
            let e = SysmonError::SelfTest("own process: missing");
            self.fatal = Some(e);
            return Err(e);
        };
        let (mut c, mut x, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: four valid local FILETIMEs; the pseudo handle of the own process needs no rights.
        let got = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut x, &mut k, &mut u) };
        let created = got.is_ok() && filetime_u64(c) == row.key.created;
        let mut session = 0u32;
        // SAFETY: session is a valid local out-parameter.
        let got = unsafe { ProcessIdToSessionId(pid, &mut session) };
        let session_ok = got.is_ok() && session == row.session;
        let threads = row.threads >= 1;
        let (reported, long) = own_image_names();
        let snap_name = table.names.get(row.image);
        let name = reported.as_deref().is_some_and(|r| name_matches(snap_name, r, long.as_deref()));
        match self_test_verdict(created, session_ok, threads, name) {
            Ok(note) => {
                self.note = note;
                self.checked = true;
                Ok(())
            }
            Err(e) => {
                self.fatal = Some(e);
                Err(e)
            }
        }
    }

    /// Some("own process: image name") after a non-fatal name mismatch.
    pub fn self_test_note(&self) -> Option<&'static str> {
        self.note
    }

    pub fn stats(&self) -> ReadStats {
        self.stats
    }

    /// The last buffer (filled length) and its base address, for capture.
    pub fn raw(&self) -> (&[u8], usize) {
        (self.bytes(), self.buf.as_ptr() as usize)
    }
}
