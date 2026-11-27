//! Image paths: bounded queries per tick, shared as an Arc.
use crate::engine::frame::{Frame, Sampler};
use crate::engine::need::Need;
use crate::model::classify::{ImagePath, PathMap};
use oled_sysmon::{ProcKey, SysmonError};
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Instant;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Queries on the sampler's first tick (every process of a normal machine).
pub const PATH_FIRST_CAP: usize = 1024;
/// Queries per later tick; the rest stay pending, oldest first next tick.
pub const PATH_TICK_CAP: usize = 32;

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const SHORT_BUF: usize = 1024;
const LONG_BUF: usize = 32_768;

/// Ok: the Win32 path in out; Err: Win32 code.
pub trait PathSource: Send {
    fn query(&mut self, key: ProcKey, out: &mut Vec<u16>) -> Result<(), u32>;
}

pub struct Win32Paths {
    buf: Vec<u16>,
}

impl Win32Paths {
    pub fn new() -> Win32Paths {
        Win32Paths { buf: Vec::new() }
    }
}

impl Default for Win32Paths {
    fn default() -> Win32Paths {
        Win32Paths::new()
    }
}

/// Closes the handle on every path out of query.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by OpenProcess, is owned by this guard and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn last_error() -> u32 {
    // SAFETY: plain query of the calling thread's last error.
    unsafe { GetLastError() }.0
}

fn filetime(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// OpenProcess(LIMITED) + GetProcessTimes check + QueryFullProcessImageNameW + CloseHandle.
impl PathSource for Win32Paths {
    fn query(&mut self, key: ProcKey, out: &mut Vec<u16>) -> Result<(), u32> {
        out.clear();
        // SAFETY: plain call, no pointers; the handle is closed by the guard below.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) }.map_err(|_| last_error())?;
        let h = Handle(h);
        let (mut created, mut exit, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the handle is open; the four out pointers are locals.
        unsafe { GetProcessTimes(h.0, &mut created, &mut exit, &mut kernel, &mut user) }.map_err(|_| last_error())?;
        if filetime(created) != key.created {
            // The pid was reused by another process since the snapshot.
            return Err(ERROR_INVALID_PARAMETER);
        }
        let mut size = SHORT_BUF;
        loop {
            self.buf.clear();
            self.buf.resize(size, 0);
            let mut len = size as u32;
            // SAFETY: the handle is open; the buffer holds `len` units, which is the capacity passed in, and len is a
            // local that receives the length written without the terminator.
            let r =
                unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(self.buf.as_mut_ptr()), &mut len) };
            match r {
                Ok(()) => {
                    out.extend_from_slice(&self.buf[..(len as usize).min(size)]);
                    return Ok(());
                }
                Err(_) => {
                    let e = last_error();
                    if e == ERROR_INSUFFICIENT_BUFFER && size < LONG_BUF {
                        size = LONG_BUF;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
}

pub struct PathSampler<S: PathSource> {
    src: S,
    map: Arc<PathMap>,
    first: bool,
    buf: Vec<u16>,
    next: Vec<(ProcKey, ImagePath)>,
}

impl<S: PathSource> PathSampler<S> {
    pub fn new(src: S) -> Self {
        PathSampler { src, map: Arc::new(PathMap::default()), first: true, buf: Vec::new(), next: Vec::new() }
    }

    /// Path for a drive-letter or UNC path; NoImage for a bare name; Unknown for anything else.
    fn classify(text: &[u16]) -> ImagePath {
        const BACKSLASH: u16 = b'\\' as u16;
        let drive =
            matches!(text, [d, colon, sep, ..] if *colon == b':' as u16 && *sep == BACKSLASH && d.is_ascii_letter());
        let unc = matches!(text, [BACKSLASH, BACKSLASH, ..]);
        if drive || unc {
            ImagePath::Path(Arc::from(String::from_utf16_lossy(text)))
        } else if !text.is_empty() && !text.iter().any(|c| *c == BACKSLASH || *c == b'/' as u16) {
            ImagePath::NoImage
        } else {
            ImagePath::Unknown
        }
    }
}

trait AsciiLetter {
    fn is_ascii_letter(&self) -> bool;
}

impl AsciiLetter for u16 {
    fn is_ascii_letter(&self) -> bool {
        u8::try_from(*self).is_ok_and(|b| b.is_ascii_alphabetic())
    }
}

/// need() = Procs; fills out.paths, paths_fresh, paths_us; runs after ProcSampler.
impl<S: PathSource> Sampler for PathSampler<S> {
    fn need(&self) -> Need {
        Need::Procs
    }
    fn sample(&mut self, _now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
        let started = Instant::now();
        if out.error.is_some() {
            // The snapshot failed: its empty table must not empty the cache.
            out.paths = Arc::clone(&self.map);
            out.paths_fresh = false;
            return Ok(());
        }
        let cap = if self.first { PATH_FIRST_CAP } else { PATH_TICK_CAP };
        let (mut queries, mut changed) = (0usize, false);
        let rows = &out.procs.rows;
        let entries = &self.map.entries;
        self.next.clear();
        let (mut i, mut j) = (0, 0);
        while i < rows.len() || j < entries.len() {
            let order = match (rows.get(i), entries.get(j)) {
                (Some(r), Some(e)) => r.key.cmp(&e.0),
                (Some(_), None) => Ordering::Less,
                (None, _) => Ordering::Greater,
            };
            match order {
                Ordering::Equal => {
                    self.next.push(entries[j].clone());
                    i += 1;
                    j += 1;
                }
                // Only in the map: the process is gone.
                Ordering::Greater => {
                    changed = true;
                    j += 1;
                }
                // Only in the table: a new key.
                Ordering::Less => {
                    let key = rows[i].key;
                    i += 1;
                    if key.pid == 0 || key.pid == 4 {
                        self.next.push((key, ImagePath::NoImage));
                        changed = true;
                    } else if queries < cap {
                        queries += 1;
                        let found = match self.src.query(key, &mut self.buf) {
                            Ok(()) => Self::classify(&self.buf),
                            Err(_) => ImagePath::Unknown,
                        };
                        self.next.push((key, found));
                        changed = true;
                    }
                    // Over the cap: no entry, the key stays pending for the next tick.
                }
            }
        }
        self.first = false;
        if changed {
            self.map = Arc::new(PathMap { entries: std::mem::take(&mut self.next) });
        } else {
            self.next.clear();
        }
        out.paths = Arc::clone(&self.map);
        out.paths_fresh = changed;
        out.paths_us = started.elapsed().as_micros().min(u32::MAX as u128) as u32;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sysmon::{ProcRow, SnapshotReader};
    use std::collections::HashMap;

    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: pid as u64 * 10 + 1 }
    }

    /// Answers from a table (a default of "C:\\Apps\\p<pid>.exe") and counts the queries.
    #[derive(Default)]
    struct Fake {
        answers: HashMap<u32, Result<String, u32>>,
        queries: usize,
    }

    impl PathSource for Fake {
        fn query(&mut self, key: ProcKey, out: &mut Vec<u16>) -> Result<(), u32> {
            self.queries += 1;
            let a = self.answers.get(&key.pid).cloned().unwrap_or_else(|| Ok(format!("C:\\Apps\\p{}.exe", key.pid)));
            out.clear();
            a.map(|s| out.extend(s.encode_utf16()))
        }
    }

    fn frame(pids: impl IntoIterator<Item = u32>) -> Frame {
        let mut f = Frame::default();
        for pid in pids {
            f.procs.push(ProcRow { key: key(pid), ..ProcRow::default() });
        }
        f.procs.finish();
        f
    }

    fn tick(s: &mut PathSampler<Fake>, f: &mut Frame) {
        f.error = None;
        s.sample(0, f).unwrap();
    }

    #[test]
    fn resolves_each_key_once() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame(100..400);
        for _ in 0..3 {
            tick(&mut s, &mut f);
        }
        assert_eq!(s.src.queries, 300);
        assert_eq!(f.paths.entries.len(), 300);
        assert_eq!(f.paths.get(key(150)), Some(&ImagePath::Path("C:\\Apps\\p150.exe".into())));
    }

    #[test]
    fn caps_per_tick() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame(100..400);
        tick(&mut s, &mut f);
        assert_eq!((f.paths.entries.len(), s.src.queries), (300, 300));
        assert!(f.paths_fresh);
        let mut f = frame(100..500);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let before = s.src.queries;
            tick(&mut s, &mut f);
            seen.push((s.src.queries - before, f.paths.entries.len()));
        }
        assert_eq!(seen, vec![(32, 332), (32, 364), (32, 396), (4, 400)]);
        // In between, the pending keys are absent; the oldest-created resolve first.
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame(100..400);
        tick(&mut s, &mut f);
        let mut f = frame(100..500);
        tick(&mut s, &mut f);
        assert!(f.paths.get(key(431)).is_some());
        assert!(f.paths.get(key(432)).is_none());
        // Nothing left to ask: a steady tick queries nothing.
        for _ in 0..3 {
            tick(&mut s, &mut f);
        }
        assert_eq!(s.src.queries, 300 + 100);
    }

    #[test]
    fn drops_gone_keys() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame([10, 20, 30]);
        tick(&mut s, &mut f);
        let mut f = frame([10, 30]);
        tick(&mut s, &mut f);
        assert!(f.paths_fresh);
        assert_eq!(f.paths.entries.iter().map(|e| e.0.pid).collect::<Vec<_>>(), vec![10, 30]);
        // A reused pid is a different key: the old entry goes, the new one is queried.
        let mut f = frame([10]);
        f.procs.rows[0].key = ProcKey { pid: 10, created: 99 };
        let before = s.src.queries;
        tick(&mut s, &mut f);
        assert_eq!(s.src.queries, before + 1);
        assert_eq!(f.paths.entries.len(), 1);
        assert_eq!(f.paths.entries[0].0.created, 99);
    }

    #[test]
    fn system_and_idle_need_no_query() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame([0, 4]);
        tick(&mut s, &mut f);
        assert_eq!(s.src.queries, 0);
        assert_eq!(f.paths.get(key(0)), Some(&ImagePath::NoImage));
        assert_eq!(f.paths.get(key(4)), Some(&ImagePath::NoImage));
    }

    #[test]
    fn bare_name_is_no_image() {
        let mut fake = Fake::default();
        fake.answers.insert(88, Ok("Registry".into()));
        fake.answers.insert(89, Ok("MemCompression".into()));
        fake.answers.insert(90, Ok("\\\\server\\share\\x.exe".into()));
        fake.answers.insert(91, Ok("d:\\x\\y.exe".into()));
        fake.answers.insert(92, Ok("\\SystemRoot\\odd.exe".into()));
        fake.answers.insert(93, Ok(String::new()));
        let mut s = PathSampler::new(fake);
        let mut f = frame(88..94);
        tick(&mut s, &mut f);
        let get = |p| f.paths.get(key(p)).cloned().unwrap();
        assert_eq!(get(88), ImagePath::NoImage);
        assert_eq!(get(89), ImagePath::NoImage);
        assert_eq!(get(90), ImagePath::Path("\\\\server\\share\\x.exe".into()));
        assert_eq!(get(91), ImagePath::Path("d:\\x\\y.exe".into()));
        assert_eq!(get(92), ImagePath::Unknown);
        assert_eq!(get(93), ImagePath::Unknown);
    }

    #[test]
    fn failure_is_unknown_and_not_retried() {
        let mut fake = Fake::default();
        fake.answers.insert(7, Err(5));
        let mut s = PathSampler::new(fake);
        let mut f = frame([7, 8]);
        tick(&mut s, &mut f);
        assert_eq!(f.paths.get(key(7)), Some(&ImagePath::Unknown));
        assert_eq!(s.src.queries, 2);
        for _ in 0..3 {
            tick(&mut s, &mut f);
        }
        assert_eq!(s.src.queries, 2);
    }

    #[test]
    fn unchanged_tick_shares_the_map() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame(100..130);
        tick(&mut s, &mut f);
        let prev = Arc::clone(&f.paths);
        let q = s.src.queries;
        tick(&mut s, &mut f);
        assert!(Arc::ptr_eq(&f.paths, &prev));
        assert!(!f.paths_fresh);
        assert_eq!(s.src.queries, q);
    }

    #[test]
    fn error_frame_keeps_the_map() {
        let mut s = PathSampler::new(Fake::default());
        let mut f = frame(100..130);
        tick(&mut s, &mut f);
        let prev = Arc::clone(&f.paths);
        let mut bad = Frame { error: Some(SysmonError::Status(-1)), ..Frame::default() };
        s.sample(0, &mut bad).unwrap();
        assert!(Arc::ptr_eq(&bad.paths, &prev));
        assert!(!bad.paths_fresh);
        // The map survived: the next good tick asks nothing.
        let q = s.src.queries;
        tick(&mut s, &mut f);
        assert_eq!(s.src.queries, q);
        assert_eq!(f.paths.entries.len(), 30);
    }

    #[test]
    fn win32_paths_resolve_this_process() {
        let mut reader = SnapshotReader::new();
        let mut table = oled_sysmon::ProcTable::new();
        reader.read(&mut table).expect("snapshot");
        let me = table.find_pid(std::process::id()).expect("own row").key;
        let mut src = Win32Paths::new();
        let mut out = Vec::new();
        src.query(me, &mut out).expect("own path");
        let text = String::from_utf16_lossy(&out);
        let exe = std::env::current_exe().unwrap();
        let want = exe.file_name().unwrap().to_string_lossy().into_owned();
        let got = text.rsplit('\\').next().unwrap();
        assert!(got.eq_ignore_ascii_case(&want), "{got} vs {want}");
        let stale = ProcKey { pid: me.pid, created: me.created + 1 };
        assert_eq!(src.query(stale, &mut out), Err(ERROR_INVALID_PARAMETER));
    }
}
