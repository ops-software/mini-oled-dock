//! The shell host's process (SH4a-SH4c): arguments, the Hello, the reader that routes requests, the parent watch and
//! the idle exit. Third-party shell code runs only on the STAs (sta.rs), each call inside a crumbs guard.

use std::fs::File;
use std::io::Read;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oled_trust::frame::files::{CallKind, Gen, ReqId, ShellEvt, ShellReq, SHELL};
use oled_trust::frame::{self, Assembler, FrameError};
use windows::Win32::Foundation::{E_NOTIMPL, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CreateWaitableTimerW, GetCurrentProcess, SetWaitableTimer, TerminateProcess, WaitForMultipleObjects, INFINITE,
};

use crate::exit;
use crate::shell::sta::{self, Job, Sta, StaSlot};
use crate::shell::writer::Writer;
use crate::shell::{class_icons, crumbs, exec, images, menu, type_names};

/// A test knob of the environment: read only in a debug build (FILES_TEST_*, FILES_FAULT_*); None in a release build.
pub fn test_var(k: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(k).ok().filter(|v| !v.is_empty())
    } else {
        None
    }
}

/// What the host reads from its environment once, at start (every field is a debug-build test knob).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCfg {
    /// FILES_TEST_NO_UI=1: no menu is shown, no error box (SEE_MASK_FLAG_NO_UI, CMIC_MASK_FLAG_NO_UI). Read in every
    /// build: it only takes UI away (the release probe's menu-build mode).
    pub test_no_ui: bool,
    /// FILES_TEST_MENU_PICK: in test mode, the verb picked from a built menu.
    pub pick: Option<String>,
    /// FILES_TEST_MENU_DISMISS_MS: a shown menu dismisses itself after that many ms. Read in every build: it only
    /// ends a menu early (the release probe's menu-show mode, which sends no input).
    pub dismiss_ms: Option<u32>,
    /// FILES_FAULT_DLL: the fault DLL's classes replace the shell's (registration-free).
    pub fault_dll: Option<PathBuf>,
    /// FILES_FAULT_POINT: "icon" or "thumb" make the image calls go to the fault DLL's classes too.
    pub fault_point: Option<String>,
    /// FILES_TEST_SHELL_IDLE_MS: the idle exit's time instead of IDLE_MS.
    pub idle_ms: Option<u64>,
}

impl HostCfg {
    pub fn from_env() -> HostCfg {
        HostCfg {
            test_no_ui: std::env::var("FILES_TEST_NO_UI").is_ok_and(|v| v == "1"),
            pick: test_var("FILES_TEST_MENU_PICK"),
            dismiss_ms: std::env::var("FILES_TEST_MENU_DISMISS_MS").ok().and_then(|v| v.parse().ok()),
            fault_dll: test_var("FILES_FAULT_DLL").map(PathBuf::from),
            fault_point: test_var("FILES_FAULT_POINT"),
            idle_ms: test_var("FILES_TEST_SHELL_IDLE_MS").and_then(|v| v.parse().ok()),
        }
    }
}

/// The idle exit: a host ends after this long without a request, when nothing holds it (busy_now).
pub const IDLE_MS: u64 = 120_000;
/// A busy host outlives its UI by at most this long; then its call is cut (open point 3's cap).
pub const BUSY_CAP: Duration = Duration::from_secs(3600);
/// How often a pending kill reads the busy word (only while a kill is pending).
pub const KILL_POLL: Duration = Duration::from_millis(50);

/// GetTickCount64 of the last request (or of the start).
static LAST_REQUEST_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    // SAFETY: no preconditions.
    unsafe { GetTickCount64() }
}

/// What the idle timer does when it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleStep {
    /// End the host (through try_kill).
    Exit,
    /// Fire again after this many ms.
    Wait(u64),
}

/// The idle rule (pure): `idle_ms` after the last request the host ends, unless something holds it (`busy`), which
/// puts the check off by a whole period.
pub fn idle_step(idle_ms: u64, now: u64, last: u64, busy: bool) -> IdleStep {
    let due = last.saturating_add(idle_ms);
    if now < due {
        IdleStep::Wait(due - now)
    } else if busy {
        IdleStep::Wait(idle_ms)
    } else {
        IdleStep::Exit
    }
}

/// Something holds the host: a protected call (InvokeCommand, Exec), a job on an STA (a menu on screen is one), or a
/// shell thread that took the process reference (a property sheet a verb opened).
fn busy_now() -> bool {
    crumbs::installed().is_some_and(|s| s.busy() & !crumbs::KILLING != 0)
        || sta::jobs_running() > 0
        || process_ref::held()
}

/// The process reference of the shell's own threads (SHSetInstanceExplorer): a shell thread that must outlive the
/// call that started it, such as a property sheet, takes a reference to it (SHGetInstanceExplorer) and releases it at
/// its end. A static object whose count the idle exit reads.
pub mod process_ref {
    use std::ffi::c_void;
    use std::mem::ManuallyDrop;
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows::core::{IUnknown, Interface, GUID, HRESULT};
    use windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER, S_OK};
    use windows::Win32::UI::Shell::SHSetInstanceExplorer;
    use windows_core::IUnknown_Vtbl;

    /// The references held now, and those held right after the registration (the shell's own, if it takes one).
    static REFS: AtomicU32 = AtomicU32::new(0);
    static BASE: AtomicU32 = AtomicU32::new(0);

    #[repr(C)]
    struct Object {
        vtbl: &'static IUnknown_Vtbl,
    }

    static VTBL: IUnknown_Vtbl = IUnknown_Vtbl { QueryInterface: query, AddRef: add_ref, Release: release };
    static OBJECT: Object = Object { vtbl: &VTBL };

    unsafe extern "system" fn query(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        // SAFETY: `out` is the caller's non-null out pointer; `iid` is the caller's GUID when it is not null.
        unsafe {
            if !iid.is_null() && *iid == IUnknown::IID {
                REFS.fetch_add(1, Ordering::AcqRel);
                *out = this;
                S_OK
            } else {
                *out = std::ptr::null_mut();
                E_NOINTERFACE
            }
        }
    }

    unsafe extern "system" fn add_ref(_: *mut c_void) -> u32 {
        REFS.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    unsafe extern "system" fn release(_: *mut c_void) -> u32 {
        REFS.fetch_sub(1, Ordering::AcqRel).wrapping_sub(1)
    }

    /// The object as an interface this copy never releases (the object is static).
    pub fn object() -> ManuallyDrop<IUnknown> {
        // SAFETY: a COM object for the life of the process; ManuallyDrop keeps this copy from releasing it.
        ManuallyDrop::new(unsafe { IUnknown::from_raw(std::ptr::from_ref(&OBJECT).cast_mut().cast()) })
    }

    /// The references held now.
    pub fn count() -> u32 {
        REFS.load(Ordering::Acquire)
    }

    /// Hands the object to the shell (once, at the host's start); the references held then are the base.
    pub fn register() {
        // SAFETY: an IUnknown that lives for the whole process.
        unsafe { SHSetInstanceExplorer(&*object()) };
        BASE.store(count(), Ordering::Release);
    }

    /// A shell thread holds a reference beyond the base.
    pub fn held() -> bool {
        count() > BASE.load(Ordering::Acquire)
    }
}

/// Ends this process now, without running anything of the extensions' (no DLL detach).
fn exit_now(why: &str) -> ! {
    oled_sys::log!("shell: host ends ({why})");
    // The log thread gets a moment for the line; then the process ends.
    std::thread::sleep(Duration::from_millis(20));
    loop {
        // SAFETY: this process's pseudo handle. Every caller won try_kill first, or holds the 1 h cap.
        let _ = unsafe { TerminateProcess(GetCurrentProcess(), 0) };
        std::thread::sleep(KILL_POLL);
    }
}

/// Ends the host once no protected call runs: try_kill, polling every 50 ms while it fails; after BUSY_CAP, KILLING is
/// forced and the call is cut. The parent watch, the end of input and Bye come here.
pub fn end_when_idle(why: &str) -> ! {
    let start = Instant::now();
    loop {
        if crumbs::try_kill() {
            exit_now(why);
        }
        if start.elapsed() >= BUSY_CAP {
            if let Some(s) = crumbs::installed() {
                s.force_killing();
            }
            exit_now("a call still busy after an hour is cut");
        }
        std::thread::sleep(KILL_POLL);
    }
}

/// Arms the one-shot `timer` to fire after `ms`.
fn arm(timer: HANDLE, ms: u64) {
    let due = -(i64::try_from(ms.saturating_mul(10_000)).unwrap_or(i64::MAX));
    // SAFETY: a waitable timer of this thread's; no completion routine.
    if let Err(e) = unsafe { SetWaitableTimer(timer, &due, 0, None, None, false) } {
        oled_sys::log!("shell: the idle timer cannot be set ({e})");
    }
}

/// Starts the lifetime thread: it waits, without a timeout of its own, on the UI process and the idle timer.
fn start_lifetime(parent: OwnedHandle, idle_ms: u64) -> std::io::Result<()> {
    // SAFETY: an unnamed auto-reset timer, owned below.
    let timer = unsafe { CreateWaitableTimerW(None, false, None) }?;
    // SAFETY: just created, owned here once.
    let timer = unsafe { OwnedHandle::from_raw_handle(timer.0) };
    LAST_REQUEST_MS.store(now_ms(), Ordering::Release);
    std::thread::Builder::new()
        .name("shell-life".into())
        .stack_size(64 * 1024)
        .spawn(move || lifetime(&parent, &timer, idle_ms))?;
    Ok(())
}

fn lifetime(parent: &OwnedHandle, timer: &OwnedHandle, idle_ms: u64) {
    use std::os::windows::io::AsRawHandle;
    let (p, t) = (HANDLE(parent.as_raw_handle()), HANDLE(timer.as_raw_handle()));
    arm(t, idle_ms);
    loop {
        // SAFETY: two handles owned by this thread's caller for its whole life.
        let got = unsafe { WaitForMultipleObjects(&[p, t], false, INFINITE) };
        if got == WAIT_OBJECT_0 {
            end_when_idle("the UI is gone");
        }
        if got.0 != WAIT_OBJECT_0.0 + 1 {
            oled_sys::log!("shell: the lifetime wait failed ({:#x})", got.0);
            return;
        }
        let last = LAST_REQUEST_MS.load(Ordering::Acquire);
        match idle_step(idle_ms, now_ms(), last, busy_now()) {
            IdleStep::Exit if crumbs::try_kill() => exit_now("idle"),
            IdleStep::Exit => arm(t, idle_ms),
            IdleStep::Wait(ms) => arm(t, ms),
        }
    }
}

/// The fault DLL's classes (debug builds only): the files_faultext example, loaded from FILES_FAULT_DLL without the
/// registry, replaces the shell's handlers so the tests can make a real extension crash, hang or work slowly.
pub mod fault {
    use std::ffi::c_void;
    use std::path::Path;

    use windows::core::{s, Error, Interface, Result, GUID, HRESULT, HSTRING};
    use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
    use windows::Win32::System::Com::IClassFactory;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

    /// FaultMenu, FaultIcon and FaultThumb, pinned with the DLL's own (fault_clsids_are_pinned) and
    /// tools/ci/files-disposable.ps1's.
    pub const CLSID_FAULT_MENU: GUID = GUID::from_u128(0x08f7748e_e7c9_4950_ac48_6ebab3ca29dc);
    pub const CLSID_FAULT_ICON: GUID = GUID::from_u128(0xe3fec5ef_f489_427f_879f_ef723f66d403);
    pub const CLSID_FAULT_THUMB: GUID = GUID::from_u128(0x5a4db2eb_6c01_444f_be94_ebff37f11b35);

    type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;

    /// An object of class `clsid` from the DLL at `dll` (LoadLibraryExW, DllGetClassObject, CreateInstance). The DLL
    /// stays loaded. A release build refuses (E_NOTIMPL).
    pub fn create<T: Interface>(dll: &Path, clsid: &GUID) -> Result<T> {
        if !cfg!(debug_assertions) {
            return Err(E_NOTIMPL.into());
        }
        let wide = HSTRING::from(dll.as_os_str());
        // SAFETY: a full path; the DLL is ours and stays loaded for the life of the process.
        let module = unsafe { LoadLibraryExW(&wide, None, LOAD_WITH_ALTERED_SEARCH_PATH) }?;
        // SAFETY: the module just loaded; a static NUL-terminated name.
        let proc = unsafe { GetProcAddress(module, s!("DllGetClassObject")) }.ok_or_else(|| Error::from(E_FAIL))?;
        // SAFETY: DllGetClassObject has exactly this signature (the COM in-process server contract).
        let get = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, GetClassObject>(proc) };
        let mut cf = std::ptr::null_mut();
        // SAFETY: valid GUID pointers and a writable out pointer.
        unsafe { get(clsid, &IClassFactory::IID, &mut cf) }.ok()?;
        // SAFETY: DllGetClassObject succeeded: `cf` is an owned IClassFactory.
        let cf = unsafe { IClassFactory::from_raw(cf) };
        // SAFETY: no aggregation.
        unsafe { cf.CreateInstance(None::<&windows::core::IUnknown>) }
    }
}

/// Image work of generations below this one is dropped (CancelGen).
static MIN_GEN: AtomicU64 = AtomicU64::new(0);

/// true when CancelGen dropped `gen`: image work stops between items.
pub fn cancelled(gen: Gen) -> bool {
    gen.0 < MIN_GEN.load(Ordering::Acquire)
}

/// The reader's side of the host: the STAs and where each request goes.
pub struct Host {
    writer: Writer,
    cfg: Arc<HostCfg>,
    menu: StaSlot,
    exec: StaSlot,
    images: [StaSlot; 2],
    next_image: u8,
}

impl Host {
    pub fn new(writer: Writer, cfg: HostCfg) -> Host {
        let [i0, i1] = crumbs::SLOT_IMAGE;
        Host {
            writer,
            cfg: Arc::new(cfg),
            menu: StaSlot::new("shell-menu", crumbs::SLOT_MENU, CallKind::HandleMenuMsg, true, sta::RETIRE_AFTER),
            exec: StaSlot::new("shell-exec", crumbs::SLOT_EXEC, CallKind::Exec, false, u32::MAX),
            images: [
                StaSlot::new("shell-image0", i0, CallKind::Icon, false, sta::RETIRE_AFTER),
                StaSlot::new("shell-image1", i1, CallKind::Icon, false, sta::RETIRE_AFTER),
            ],
            next_image: 0,
        }
    }

    /// Routes one request (Bye is the reader's).
    pub fn dispatch(&mut self, req: ShellReq) {
        if let ShellReq::CancelGen { gen } = req {
            MIN_GEN.fetch_max(gen.0, Ordering::AcqRel);
            for s in &self.images {
                for r in s.drop_older(gen) {
                    self.writer.send(&ShellEvt::ReqDone { req: r });
                }
            }
            return;
        }
        let Some(to) = sta::route(&req, &mut self.next_image) else {
            return;
        };
        let gen = match &req {
            ShellReq::Icons { gen, .. } | ShellReq::Thumbs { gen, .. } => Some(*gen),
            _ => None,
        };
        let id = req_id(&req);
        let (w, cfg) = (self.writer.clone(), self.cfg.clone());
        let job = Job { gen, req: id, run: Box::new(move || handle(req, &w, &cfg)) };
        match to {
            Sta::Menu => self.menu.post(job),
            Sta::Exec => self.exec.post(job),
            Sta::Image(i) => {
                if let Some(s) = self.images.get(usize::from(i)) {
                    s.post(job);
                }
            }
        }
    }
}

/// Serves one request on its STA.
fn handle(req: ShellReq, w: &Writer, cfg: &HostCfg) {
    match req {
        ShellReq::Menu { req, items, at, extended, owner } => {
            menu::run(menu::MenuReq { req, items, at, extended, owner }, w, cfg);
        }
        ShellReq::Exec { req, verb, items } => exec::run(req, verb, &items, w, cfg),
        ShellReq::RunTyped { req, text, dir } => exec::run_typed(req, &text, &dir, w, cfg),
        ShellReq::Icons { req, gen, items, px } => images::run(req, gen, &items, px, None, w, cfg),
        ShellReq::Thumbs { req, gen, items, px, cache_only } => {
            images::run(req, gen, &items, px, Some(cache_only), w, cfg);
        }
        ShellReq::ClassIcons { req, exts, px } => match class_icons::serve(req, &exts, px, &mut |e| w.send(&e)) {
            Ok(()) => w.send(&ShellEvt::ReqDone { req }),
            Err(hr) => w.send(&ShellEvt::Failed { req, hr }),
        },
        ShellReq::TypeNames { req, exts } => match type_names::serve(req, &exts, &mut |e| w.send(&e)) {
            Ok(()) => w.send(&ShellEvt::ReqDone { req }),
            Err(hr) => w.send(&ShellEvt::Failed { req, hr }),
        },
        other => {
            if let Some(req) = req_id(&other) {
                w.send(&ShellEvt::Failed { req, hr: E_NOTIMPL.0 });
            }
        }
    }
}

/// The handle values the client passed on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostArgs {
    /// Requests from the client (the read end of its pipe).
    pub input: usize,
    /// Events to the client.
    pub output: usize,
    /// The crumbs section.
    pub crumbs: usize,
    /// A SYNCHRONIZE handle of the UI process.
    pub parent: usize,
}

/// --in=<h> --out=<h> --crumbs=<h> --parent=<h>, each once, each a non-zero decimal handle value; anything else is
/// refused.
pub fn parse_args(args: &[String]) -> Option<HostArgs> {
    let mut v = [None; 4];
    for a in args {
        let (k, val) = a.split_once('=')?;
        let i = ["--in", "--out", "--crumbs", "--parent"].iter().position(|n| *n == k)?;
        let n: usize = val.parse().ok().filter(|&n| n != 0)?;
        let slot = v.get_mut(i)?;
        if slot.replace(n).is_some() {
            return None;
        }
    }
    let [Some(input), Some(output), Some(crumbs), Some(parent)] = v else {
        return None;
    };
    Some(HostArgs { input, output, crumbs, parent })
}

/// The arguments of a host whose handles are `a` (the client's side of parse_args).
pub fn args_of(a: &HostArgs) -> [String; 4] {
    [
        format!("--in={}", a.input),
        format!("--out={}", a.output),
        format!("--crumbs={}", a.crumbs),
        format!("--parent={}", a.parent),
    ]
}

/// Why the Hello failed.
#[derive(Debug, PartialEq, Eq)]
enum HelloError {
    /// The client speaks another protocol version.
    Proto,
    /// Anything else: a bad magic, a broken pipe, end of input.
    Failed,
}

/// Reads the client's Hello from `input`; bytes after it stay in `asm`.
fn read_hello(input: &mut impl Read, asm: &mut Assembler) -> Result<(), HelloError> {
    let mut buf = [0u8; 4096];
    loop {
        match asm.next_frame() {
            Ok(Some(f)) => {
                return match frame::decode_hello(&SHELL, &f) {
                    Ok(_) => Ok(()),
                    Err(FrameError::BadProto { got, want }) => {
                        oled_sys::log!("shell: the client speaks protocol {got}, this host {want}");
                        Err(HelloError::Proto)
                    }
                    Err(e) => {
                        oled_sys::log!("shell: bad Hello ({e:?})");
                        Err(HelloError::Failed)
                    }
                };
            }
            Ok(None) => {}
            Err(_) => return Err(HelloError::Failed),
        }
        match input.read(&mut buf) {
            Ok(0) | Err(_) => return Err(HelloError::Failed),
            Ok(n) => asm.push(buf.get(..n).unwrap_or_default()),
        }
    }
}

/// The request id of a request, if it has one.
pub fn req_id(r: &ShellReq) -> Option<ReqId> {
    use ShellReq::*;
    match r {
        Menu { req, .. }
        | ModernItems { req, .. }
        | Exec { req, .. }
        | RunTyped { req, .. }
        | Icons { req, .. }
        | Thumbs { req, .. }
        | ClassIcons { req, .. }
        | TypeNames { req, .. }
        | ShellNewTypes { req }
        | QuickAccessPins { req } => Some(*req),
        CancelGen { .. } | Bye => None,
    }
}

/// files.exe --role=shell after role_shell's checks: maps the crumbs, says Hello and Ready, then serves requests until
/// the client goes away. Returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let Some(a) = parse_args(args) else {
        oled_sys::log!("shell: bad arguments");
        return exit::USAGE;
    };
    // SAFETY: the client passed these handle values for this process, each once; they are owned from here on.
    let (mut input, output, parent) = unsafe {
        (
            File::from_raw_handle(a.input as _),
            File::from_raw_handle(a.output as _),
            OwnedHandle::from_raw_handle(a.parent as _),
        )
    };
    let section = match crumbs::Mapping::open(HANDLE(a.crumbs as _)) {
        Ok(m) => m.leak(),
        Err(e) => {
            oled_sys::log!("shell: no crumbs section ({e})");
            return exit::HELLO_FAILED;
        }
    };
    let pid = std::process::id();
    section.set_host_pid(pid);
    crumbs::install(section);
    let Ok(writer) = Writer::start(output) else {
        return exit::HELLO_FAILED;
    };
    let mut hello = Vec::new();
    frame::encode_hello(&SHELL, crate::BUILD_ID, &mut hello);
    writer.raw(hello);
    let mut asm = Assembler::new(SHELL.max_payload);
    match read_hello(&mut input, &mut asm) {
        Ok(()) => {}
        Err(HelloError::Proto) => return exit::PROTO_MISMATCH,
        Err(HelloError::Failed) => return exit::HELLO_FAILED,
    }
    writer.send(&ShellEvt::Ready { pid });
    oled_sys::log!("shell: host {pid} ready");
    process_ref::register();
    let cfg = HostCfg::from_env();
    if let Err(e) = start_lifetime(parent, cfg.idle_ms.unwrap_or(IDLE_MS)) {
        oled_sys::log!("shell: no lifetime thread ({e})");
        return exit::HELLO_FAILED;
    }
    let mut host = Host::new(writer, cfg);
    let why = serve(&mut input, &mut asm, &mut host);
    end_when_idle(why)
}

/// The reader: one request at a time to its STA, until end of input, Bye or a frame that is not ours; why it stopped.
fn serve(input: &mut impl Read, asm: &mut Assembler, host: &mut Host) -> &'static str {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        loop {
            match asm.next_frame() {
                Ok(Some(f)) => match frame::decode::<ShellReq>(&f) {
                    Ok((ShellReq::Bye, _)) => return "Bye",
                    Ok((req, _)) => {
                        LAST_REQUEST_MS.store(now_ms(), Ordering::Release);
                        host.dispatch(req);
                    }
                    Err(e) => {
                        oled_sys::log!("shell: a request that is not ours ({e:?})");
                        return "a bad request";
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    oled_sys::log!("shell: the request stream broke ({e:?})");
                    return "a broken request stream";
                }
            }
        }
        match input.read(&mut buf) {
            Ok(0) | Err(_) => return "end of input",
            Ok(n) => asm.push(buf.get(..n).unwrap_or_default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args_parse_table() {
        let ok = HostArgs { input: 1, output: 2, crumbs: 3, parent: 4 };
        assert_eq!(parse_args(&s(&["--in=1", "--out=2", "--crumbs=3", "--parent=4"])), Some(ok));
        assert_eq!(parse_args(&s(&["--parent=4", "--crumbs=3", "--out=2", "--in=1"])), Some(ok), "any order");
        assert_eq!(parse_args(&args_of(&ok)), Some(ok), "args_of round-trips");
        for bad in [
            &["--in=1", "--out=2", "--crumbs=3"][..],
            &["--in=1", "--out=2", "--crumbs=3", "--parent=4", "--in=5"],
            &["--in=1", "--out=2", "--crumbs=3", "--parent=0"],
            &["--in=1", "--out=2", "--crumbs=3", "--parent=x"],
            &["--in=1", "--out=2", "--crumbs=3", "--parent=4", "--x=1"],
            &["--in", "--out=2", "--crumbs=3", "--parent=4"],
            &[],
        ] {
            assert_eq!(parse_args(&s(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn fault_clsids_are_pinned() {
        let dll = include_str!("../../examples/files_faultext/lib.rs");
        let script = include_str!("../../../../tools/ci/files-disposable.ps1");
        for (g, text) in [
            (fault::CLSID_FAULT_MENU, "0x08f7748e_e7c9_4950_ac48_6ebab3ca29dc"),
            (fault::CLSID_FAULT_ICON, "0xe3fec5ef_f489_427f_879f_ef723f66d403"),
            (fault::CLSID_FAULT_THUMB, "0x5a4db2eb_6c01_444f_be94_ebff37f11b35"),
        ] {
            assert_eq!(
                g,
                windows::core::GUID::from_u128(u128::from_str_radix(&text[2..].replace('_', ""), 16).unwrap())
            );
            assert!(dll.contains(text), "the DLL pins {text}");
            let hex = text.trim_start_matches("0x").replace('_', "");
            let braced = format!("{{{}-{}-{}-{}-{}}}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
            assert!(script.to_lowercase().contains(&braced), "files-disposable.ps1 registers {braced}");
        }
    }

    #[test]
    fn process_reference_counts_its_holders() {
        use windows::core::{IUnknown, Interface};
        let object = process_ref::object();
        let base = process_ref::count();
        let held: IUnknown = (*object).clone();
        assert_eq!(process_ref::count(), base + 1, "AddRef");
        let again: IUnknown = held.cast().expect("QueryInterface for IUnknown");
        assert_eq!(process_ref::count(), base + 2, "QueryInterface takes a reference");
        assert!(held.cast::<windows::Win32::System::Com::IClassFactory>().is_err(), "nothing but IUnknown");
        drop((held, again));
        assert_eq!(process_ref::count(), base, "Release");
    }

    #[test]
    fn idle_exit_state_machine() {
        let idle = IDLE_MS;
        assert_eq!(idle_step(idle, 1_000, 1_000, false), IdleStep::Wait(idle), "just started");
        assert_eq!(idle_step(idle, 100_000, 50_000, false), IdleStep::Wait(70_000), "a request moved the deadline");
        assert_eq!(idle_step(idle, 121_000, 1_000, false), IdleStep::Exit, "120 s without a request");
        assert_eq!(idle_step(idle, 121_000, 1_000, true), IdleStep::Wait(idle), "busy puts it off by a period");
        assert_eq!(idle_step(idle, 0, u64::MAX, false), IdleStep::Wait(u64::MAX), "no overflow");
        assert_eq!(idle_step(500, 600, 0, false), IdleStep::Exit, "the test override");
    }

    #[test]
    fn hello_errors_are_told_apart() {
        let mut good = Vec::new();
        frame::encode_hello(&SHELL, crate::BUILD_ID, &mut good);
        let mut asm = Assembler::new(SHELL.max_payload);
        assert_eq!(read_hello(&mut &good[..], &mut asm), Ok(()));
        let mut other = Vec::new();
        let v2 = oled_trust::frame::Proto { version: SHELL.version + 1, ..SHELL };
        frame::encode_hello(&v2, crate::BUILD_ID, &mut other);
        assert_eq!(read_hello(&mut &other[..], &mut Assembler::new(SHELL.max_payload)), Err(HelloError::Proto));
        assert_eq!(read_hello(&mut &[][..], &mut Assembler::new(SHELL.max_payload)), Err(HelloError::Failed));
        let ops = oled_trust::frame::files::OPS;
        let mut wrong = Vec::new();
        frame::encode_hello(&ops, crate::BUILD_ID, &mut wrong);
        assert_eq!(read_hello(&mut &wrong[..], &mut Assembler::new(SHELL.max_payload)), Err(HelloError::Failed));
    }
}
