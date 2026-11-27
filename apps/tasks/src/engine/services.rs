//! Services and the process that hosts each; the SCM is never waited for.
//!
//! The SCM call runs on its own worker thread ("tasks-services"), which sleeps on an auto-reset event with no
//! timeout, so an idle worker never wakes. The sampler only takes a Mutex slot and signals the event.
use crate::engine::frame::{Frame, Sampler, ServiceEntry, ServiceMap};
use crate::engine::need::Need;
use oled_sysmon::SysmonError;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW, SC_ENUM_PROCESS_INFO,
    SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

pub const SERVICE_PERIOD_MS: u32 = 5000;
pub const SERVICE_SLOW_MS: u32 = 500;
pub const SERVICE_BACKOFF_MS: u32 = 60_000;
/// A request in flight this long reports services_error 258 (WAIT_TIMEOUT).
pub const SERVICE_HUNG_MS: u32 = 5000;

/// Largest enumeration buffer the SCM reader grows to.
const MAX_BUF: usize = 4 * 1024 * 1024;

pub trait ServiceSource: Send {
    fn enumerate(&mut self, out: &mut Vec<ServiceEntry>) -> Result<(), u32>;
}

pub struct ScmServices;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads the NUL-terminated UTF-16 string at byte offset `off` of `buf`, never past the buffer.
fn string_at(buf: &[u8], off: usize) -> Option<Box<str>> {
    let tail = buf.get(off..)?;
    let mut units = Vec::new();
    for pair in tail.chunks_exact(2) {
        let u = u16::from_le_bytes([pair[0], pair[1]]);
        if u == 0 {
            return Some(String::from_utf16_lossy(&units).into_boxed_str());
        }
        units.push(u);
    }
    None
}

impl ScmServices {
    /// One call of EnumServicesStatusExW into `buf`: Ok((entries returned)) or Err(win32 code, bytes needed).
    fn call(scm: SC_HANDLE, buf: &mut [u8]) -> Result<u32, (u32, usize)> {
        let mut needed = 0u32;
        let mut returned = 0u32;
        // SAFETY: scm is an open SCM handle; buf, needed and returned outlive the call.
        let r = unsafe {
            EnumServicesStatusExW(
                scm,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_ACTIVE,
                Some(buf),
                &mut needed,
                &mut returned,
                None,
                PCWSTR::null(),
            )
        };
        match r {
            Ok(()) => Ok(returned),
            Err(e) => Err(((e.code().0 as u32) & 0xFFFF, needed as usize)),
        }
    }

    fn read(scm: SC_HANDLE, out: &mut Vec<ServiceEntry>) -> Result<(), u32> {
        let mut size = 64 * 1024usize;
        loop {
            // u64 backing keeps the buffer 8-byte aligned for the records the SCM writes into it.
            let mut backing = vec![0u64; size / 8];
            // SAFETY: the u64 allocation is reinterpreted as bytes of the same length (alignment 8 >= 1).
            let bytes: &mut [u8] =
                unsafe { std::slice::from_raw_parts_mut(backing.as_mut_ptr().cast::<u8>(), backing.len() * 8) };
            match Self::call(scm, bytes) {
                Ok(n) => {
                    let base = bytes.as_ptr() as usize;
                    let rec = std::mem::size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
                    for i in 0..n as usize {
                        let Some(slice) = bytes.get(i * rec..(i + 1) * rec) else {
                            break;
                        };
                        // SAFETY: slice holds exactly one record's bytes; the read is unaligned-safe.
                        let r: ENUM_SERVICE_STATUS_PROCESSW =
                            unsafe { std::ptr::read_unaligned(slice.as_ptr().cast()) };
                        let pid = r.ServiceStatusProcess.dwProcessId;
                        if pid == 0 {
                            continue;
                        }
                        // The string pointers point into the same buffer: turn them into offsets and bound them.
                        let off = |p: *mut u16| (p as usize).checked_sub(base);
                        let (Some(n_off), Some(d_off)) = (off(r.lpServiceName.0), off(r.lpDisplayName.0)) else {
                            continue;
                        };
                        let (Some(name), Some(display)) = (string_at(bytes, n_off), string_at(bytes, d_off)) else {
                            continue;
                        };
                        out.push(ServiceEntry { pid, name, display });
                    }
                    return Ok(());
                }
                Err((code, needed)) if code == ERROR_MORE_DATA.0 => {
                    let next = needed.max(size + 8).next_multiple_of(8);
                    if next > MAX_BUF {
                        return Err(code);
                    }
                    size = next;
                }
                Err((code, _)) => return Err(code),
            }
        }
    }
}

impl ServiceSource for ScmServices {
    fn enumerate(&mut self, out: &mut Vec<ServiceEntry>) -> Result<(), u32> {
        out.clear();
        // SAFETY: null machine and database names select the local active database.
        let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
            .map_err(|e| (e.code().0 as u32) & 0xFFFF)?;
        let r = Self::read(scm, out);
        // SAFETY: scm was opened above and is closed once, on every path.
        let _ = unsafe { CloseServiceHandle(scm) };
        out.sort_by(|a, b| (a.pid, &a.name).cmp(&(b.pid, &b.name)));
        r
    }
}

/// An auto-reset event, closed when its last owner goes.
struct Event(isize);

impl Event {
    fn new() -> Result<Event, u32> {
        // SAFETY: no security attributes, auto-reset, initially not signalled, unnamed.
        unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map(|h| Event(h.0 as isize))
            .map_err(|e| (e.code().0 as u32) & 0xFFFF)
    }
    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
    fn signal(&self) {
        // SAFETY: the handle is open while self lives.
        let _ = unsafe { SetEvent(self.handle()) };
    }
    fn wait(&self) {
        // SAFETY: the handle is open while self lives; INFINITE: the worker costs nothing while idle.
        unsafe { WaitForSingleObject(self.handle(), INFINITE) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        let _ = unsafe { CloseHandle(self.handle()) };
    }
}

struct Done {
    result: Result<(), u32>,
    elapsed_us: u64,
    entries: Vec<ServiceEntry>,
}

struct Shared<S> {
    /// Taken by the worker when it starts; stays here when the thread could not be spawned.
    src: Mutex<Option<S>>,
    slot: Mutex<Option<Done>>,
    exit: AtomicBool,
    wakes: AtomicU64,
}

fn worker<S: ServiceSource>(shared: &Shared<S>, event: &Event) {
    let Some(mut src) = lock(&shared.src).take() else {
        return;
    };
    loop {
        event.wait();
        shared.wakes.fetch_add(1, Ordering::Relaxed);
        if shared.exit.load(Ordering::Acquire) {
            return;
        }
        let t0 = Instant::now();
        let mut entries = Vec::new();
        let result = src.enumerate(&mut entries);
        let elapsed_us = t0.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        if shared.exit.load(Ordering::Acquire) {
            return;
        }
        *lock(&shared.slot) = Some(Done { result, elapsed_us, entries });
    }
}

/// The source until first use; the worker thread "tasks-services", its event and a Mutex slot join with it.
pub struct ServiceSampler<S: ServiceSource + 'static> {
    shared: Arc<Shared<S>>,
    event: Option<Arc<Event>>,
    started: bool,
    map: Arc<ServiceMap>,
    error: Option<u32>,
    in_flight: bool,
    /// u64::MAX until the first request.
    last_request_us: u64,
    in_flight_since_us: u64,
    backoff_until_us: u64,
}

impl<S: ServiceSource + 'static> ServiceSampler<S> {
    pub fn new(src: S) -> Self {
        ServiceSampler {
            shared: Arc::new(Shared {
                src: Mutex::new(Some(src)),
                slot: Mutex::new(None),
                exit: AtomicBool::new(false),
                wakes: AtomicU64::new(0),
            }),
            event: None,
            started: false,
            map: Arc::new(ServiceMap::default()),
            error: None,
            in_flight: false,
            last_request_us: u64::MAX,
            in_flight_since_us: 0,
            backoff_until_us: 0,
        }
    }

    /// Times the worker woke up (zero while no request was made).
    pub fn worker_wakes(&self) -> u64 {
        self.shared.wakes.load(Ordering::Relaxed)
    }

    fn start_worker(&mut self) -> Result<Arc<Event>, u32> {
        let event = match &self.event {
            Some(e) => Arc::clone(e),
            None => {
                let e = Arc::new(Event::new()?);
                self.event = Some(Arc::clone(&e));
                e
            }
        };
        if !self.started {
            let shared = Arc::clone(&self.shared);
            let ev = Arc::clone(&event);
            std::thread::Builder::new()
                .name("tasks-services".into())
                .stack_size(64 * 1024)
                .spawn(move || worker(&shared, &ev))
                .map_err(|e| e.raw_os_error().map_or(8, |c| c as u32))?;
            self.started = true;
        }
        Ok(event)
    }
}

/// need() = Services; never waits for the SCM (TW2).
impl<S: ServiceSource + 'static> Sampler for ServiceSampler<S> {
    fn need(&self) -> Need {
        Need::Services
    }
    fn sample(&mut self, now_us: u64, out: &mut Frame) -> Result<(), SysmonError> {
        out.services_fresh = false;
        let done = lock(&self.shared.slot).take();
        if let Some(d) = done {
            self.in_flight = false;
            match d.result {
                Ok(()) => {
                    self.map = Arc::new(ServiceMap { entries: d.entries });
                    out.services_fresh = true;
                    self.error = None;
                    if d.elapsed_us > u64::from(SERVICE_SLOW_MS) * 1000 {
                        self.backoff_until_us = now_us.saturating_add(u64::from(SERVICE_BACKOFF_MS) * 1000);
                    }
                }
                Err(code) => {
                    self.error = Some(code);
                    self.backoff_until_us = now_us.saturating_add(u64::from(SERVICE_BACKOFF_MS) * 1000);
                }
            }
        }
        let period_over = self.last_request_us == u64::MAX
            || now_us.saturating_sub(self.last_request_us) >= u64::from(SERVICE_PERIOD_MS) * 1000;
        if !self.in_flight && now_us >= self.backoff_until_us && period_over {
            self.last_request_us = now_us;
            match self.start_worker() {
                Ok(event) => {
                    self.in_flight = true;
                    self.in_flight_since_us = now_us;
                    event.signal();
                }
                Err(code) => self.error = Some(code),
            }
        }
        if self.in_flight && now_us.saturating_sub(self.in_flight_since_us) >= u64::from(SERVICE_HUNG_MS) * 1000 {
            self.error = Some(WAIT_TIMEOUT.0);
        }
        out.services = Arc::clone(&self.map);
        out.services_error = self.error;
        Ok(())
    }
}

/// Tells the worker to exit; never joins it (it may sit inside a hung SCM call and ends with the process).
impl<S: ServiceSource + 'static> Drop for ServiceSampler<S> {
    fn drop(&mut self) {
        self.shared.exit.store(true, Ordering::Release);
        if let Some(e) = &self.event {
            e.signal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;

    type Gate = Arc<(Mutex<bool>, Condvar)>;
    use std::time::Duration;

    const S: u64 = 1_000_000;

    fn entry(pid: u32, name: &str) -> ServiceEntry {
        ServiceEntry { pid, name: name.into(), display: name.into() }
    }

    #[derive(Default)]
    struct Probe {
        calls: AtomicU64,
    }

    struct Fake {
        probe: Arc<Probe>,
        sleep_ms: u64,
        fail: Arc<Mutex<Option<u32>>>,
        gate: Option<Gate>,
        list: Vec<ServiceEntry>,
    }

    impl Fake {
        fn new(list: Vec<ServiceEntry>) -> (Fake, Arc<Probe>) {
            let probe = Arc::new(Probe::default());
            (Fake { probe: probe.clone(), sleep_ms: 0, fail: Arc::new(Mutex::new(None)), gate: None, list }, probe)
        }
    }

    impl ServiceSource for Fake {
        fn enumerate(&mut self, out: &mut Vec<ServiceEntry>) -> Result<(), u32> {
            self.probe.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(g) = &self.gate {
                let mut open = lock(&g.0);
                while !*open {
                    open = g.1.wait(open).unwrap_or_else(|e| e.into_inner());
                }
            }
            if self.sleep_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.sleep_ms));
            }
            if let Some(c) = *lock(&self.fail) {
                return Err(c);
            }
            out.clear();
            out.extend(self.list.iter().cloned());
            Ok(())
        }
    }

    /// Polls sample() with the same synthetic time until the slot has been taken (real time passes in the worker).
    fn settle(s: &mut ServiceSampler<Fake>, now: u64, f: &mut Frame) {
        for _ in 0..400 {
            s.sample(now, f).unwrap();
            if !s.in_flight {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the worker did not finish");
    }

    fn two() -> Vec<ServiceEntry> {
        vec![entry(4, "a"), entry(9, "b")]
    }

    #[test]
    fn every_5_s() {
        let (fake, probe) = Fake::new(two());
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        settle(&mut s, 0, &mut f);
        assert_eq!(f.services.entries.len(), 2);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        s.sample(4 * S, &mut f).unwrap();
        assert!(!f.services_fresh);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        settle(&mut s, 5 * S, &mut f);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
        assert!(f.services_fresh);
        assert_eq!(f.services_error, None);
    }

    #[test]
    fn slow_call_backs_off_60_s() {
        let (mut fake, probe) = Fake::new(two());
        fake.sleep_ms = 600;
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        settle(&mut s, 0, &mut f);
        assert_eq!(f.services.entries.len(), 2, "a slow Ok result is still used");
        for t in [5 * S, 30 * S, 60 * S - 1] {
            s.sample(t, &mut f).unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1, "no request before 60 s");
        settle(&mut s, 60 * S, &mut f);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failure_keeps_the_last_map() {
        let (fake, probe) = Fake::new(two());
        let fail = fake.fail.clone();
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        settle(&mut s, 0, &mut f);
        *lock(&fail) = Some(5);
        settle(&mut s, 5 * S, &mut f);
        assert_eq!(f.services_error, Some(5));
        assert_eq!(f.services.entries.len(), 2);
        assert!(!f.services_fresh);
        s.sample(10 * S, &mut f).unwrap();
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2, "backoff after a failure");
        assert_eq!(f.services_error, Some(5));
        *lock(&fail) = None;
        settle(&mut s, 66 * S, &mut f);
        assert_eq!(f.services_error, None);
    }

    #[test]
    fn for_pid_groups_svchost() {
        let list = vec![entry(9, "z"), entry(4, "b"), entry(4, "a")];
        let (fake, _) = Fake::new(list);
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        settle(&mut s, 0, &mut f);
        // The fake returns its list unsorted: the sampler publishes what the source gives (the SCM reader sorts).
        let mut names: Vec<_> = f.services.entries.iter().map(|e| e.pid).collect();
        names.sort();
        assert_eq!(names, vec![4, 4, 9]);
        let sorted = ServiceMap { entries: vec![entry(4, "a"), entry(4, "b"), entry(9, "z")] };
        assert_eq!(sorted.for_pid(4).len(), 2);
    }

    fn gated() -> (Fake, Arc<Probe>, Gate) {
        let (mut fake, probe) = Fake::new(two());
        let gate: Gate = Arc::new((Mutex::new(false), Condvar::new()));
        fake.gate = Some(gate.clone());
        (fake, probe, gate)
    }

    fn release(gate: &(Mutex<bool>, Condvar)) {
        *lock(&gate.0) = true;
        gate.1.notify_all();
    }

    #[test]
    fn hung_source_never_blocks_a_tick() {
        let (fake, probe, gate) = gated();
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        let mut worst = Duration::ZERO;
        for now in [0, S, 4 * S, 5 * S, 6 * S] {
            let t0 = Instant::now();
            s.sample(now, &mut f).unwrap();
            worst = worst.max(t0.elapsed());
            if now < 5 * S {
                assert_eq!(f.services_error, None);
            }
        }
        assert!(worst < Duration::from_millis(50), "{worst:?}");
        assert_eq!(f.services_error, Some(258));
        assert!(f.services.entries.is_empty());
        for _ in 0..400 {
            if probe.calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1, "no new request while one hangs");
        release(&gate);
        settle(&mut s, 7 * S, &mut f);
        assert_eq!(f.services.entries.len(), 2);
        assert_eq!(f.services_error, None);
    }

    #[test]
    fn drop_never_waits_for_a_hung_source() {
        let (fake, _, gate) = gated();
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        s.sample(0, &mut f).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let t0 = Instant::now();
        drop(s);
        assert!(t0.elapsed() < Duration::from_millis(100));
        release(&gate);
    }

    #[test]
    fn idle_worker_never_wakes() {
        let (fake, probe) = Fake::new(two());
        let s = ServiceSampler::new(fake);
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
        assert_eq!(s.worker_wakes(), 0);
    }

    #[test]
    fn worker_wakes_once_per_request() {
        let (fake, _) = Fake::new(two());
        let mut s = ServiceSampler::new(fake);
        let mut f = Frame::default();
        settle(&mut s, 0, &mut f);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.worker_wakes(), 1);
    }

    #[test]
    fn string_reader_stays_inside_the_buffer() {
        let buf: Vec<u8> = [b'h', 0, b'i', 0, 0, 0].to_vec();
        assert_eq!(string_at(&buf, 0).as_deref(), Some("hi"));
        assert_eq!(string_at(&buf, 4).as_deref(), Some(""));
        assert_eq!(string_at(&buf, 6), None);
        assert_eq!(string_at(&buf[..4], 0), None, "no terminator before the end");
        assert_eq!(string_at(&buf, 99), None);
    }

    #[test]
    fn scm_enumerates() {
        let mut out = Vec::new();
        assert_eq!(ScmServices.enumerate(&mut out), Ok(()));
        assert!(out.iter().any(|e| e.pid != 0));
        assert!(out.windows(2).all(|p| (p[0].pid, &p[0].name) <= (p[1].pid, &p[1].name)));
    }
}
