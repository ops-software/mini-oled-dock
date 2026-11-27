//! Names and icons as the dock shows them (the shared shell model's names over the same sources, the Start Menu index,
//! names.bin, the dock's renames): a resolver thread at idle priority and the name book the rows read.
//!
//! The thread "tasks-resolver" runs at THREAD_PRIORITY_IDLE with COM (STA), waits on a condition variable with no
//! timeout (no wake while there is nothing to do) and answers in batches of at most 32 through the window's proxy. It
//! builds the Start Menu index once at start (and again on a miss when the index is older than ten minutes, the dock's
//! rule; never on a timer). Requests are served front first (visible rows ask at the front); each key is asked once per
//! session by the NameBook and a failure is kept as none.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use oled_shellmodel::icon::{self, IconDecode, IconQuery};
use oled_shellmodel::identity::key_from_exe_path;
use oled_shellmodel::names::{natural_name, override_name, NameQuery, NameSources};
use oled_shellmodel::process::{package_aumid, OwnedProcess};
use oled_shellmodel::startmenu::{IndexedSources, StartMenuIndex};
use oled_shellmodel::version::FileInfo;
use oled_shellmodel::window;
use oled_ui::gfx::image::{resize_area, IconBitmap};
use oled_ui::host::Proxy;
use oled_ui::UiLang;
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_IDLE};
use windows::Win32::UI::WindowsAndMessaging::HICON;

use super::paths::Paths;
use super::{dock_names, names_bin};
use crate::model::group::{ExeId, IdentityId};

/// Requests waiting at most; one more is refused.
pub const MAX_PENDING: usize = 512;
/// Answers in one message.
pub const BATCH: usize = 32;
/// Waits this long for the thread at stop, then lets it go (as the engine's host).
pub const STOP_WAIT_MS: u64 = 1000;
/// Icons kept at most, and their bytes at most (w x h x 4 each).
pub const MAX_ICONS: usize = 400;
pub const MAX_ICON_BYTES: usize = 838_860;

// ------------------------------------------------------------------ the requests and the answers

/// What the UI asks the thread for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The identity of a window: its AUMID, else its process' package AUMID, else the key of the exe.
    Identity { hwnd: isize, pid: u32, exe: String },
    /// The name of a program: the dock's rename, else the names.bin entry, else the shared model's natural name.
    Name { identity: Option<IdentityId>, exe_id: Option<ExeId>, key: String, exe: String, aumid: Option<String> },
    /// The icon of a program at `px` pixels (16, 24 or 32 by the window's DPI).
    Icon { identity: IdentityId, exe: String, aumid: Option<String>, px: u16 },
    /// The company of an exe's version strings.
    Publisher { exe_id: ExeId, exe: String },
}

/// One resolved name, icon, identity or publisher; none inside is a failure (kept, never asked again).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Answer {
    #[default]
    Nothing,
    Identity {
        hwnd: isize,
        pid: u32,
        key: String,
        aumid: Option<String>,
    },
    Name {
        identity: Option<IdentityId>,
        exe_id: Option<ExeId>,
        name: Option<String>,
    },
    Icon {
        identity: IdentityId,
        px: u16,
        bitmap: Option<IconBitmap>,
    },
    Publisher {
        exe_id: ExeId,
        publisher: Option<String>,
    },
}

/// AppMsg: a batch of answers.
pub struct ResolverDone(pub Vec<Answer>);

/// AppMsg: the resolver started after the window opened (it needs the window's proxy); the screen keeps it in Deps.
pub struct Started(pub Resolver);

// ------------------------------------------------------------------ the queue (pure)

/// The pending requests: front first, at most MAX_PENDING, a request equal to a waiting one is dropped.
#[derive(Debug, Default)]
pub struct Queue {
    items: VecDeque<Request>,
}

impl Queue {
    /// false when the request was dropped (a duplicate, or the queue is full).
    pub fn push(&mut self, r: Request, front: bool) -> bool {
        if self.items.len() >= MAX_PENDING || self.items.contains(&r) {
            return false;
        }
        if front {
            self.items.push_front(r);
        } else {
            self.items.push_back(r);
        }
        true
    }

    pub fn pop(&mut self) -> Option<Request> {
        self.items.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ------------------------------------------------------------------ the thread

/// What the thread does with a request; the real one reads the shell, a test's answers from a table.
pub trait Backend: Send + 'static {
    /// (identity key, AUMID) of a window.
    fn identity(&mut self, hwnd: isize, pid: u32, exe: &str) -> (String, Option<String>);
    fn name(&mut self, key: &str, exe: &str, aumid: Option<&str>) -> Option<String>;
    fn icon(&mut self, exe: &str, aumid: Option<&str>, px: u16) -> Option<IconBitmap>;
    fn publisher(&mut self, exe: &str) -> Option<String>;
    /// At close: whatever is kept on disk is written.
    fn finish(&mut self) {}
}

#[derive(Default)]
struct State {
    queue: Queue,
    exit: bool,
    finished: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.state.lock().unwrap_or_else(|e| e.into_inner())
}

/// The resolver thread; stopped within 1 s by stop (or on drop).
pub struct Resolver {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Resolver {
    /// Queues a request; visible rows ask at the front. false when it was a duplicate or the queue is full.
    pub fn request(&self, r: Request, visible: bool) -> bool {
        let queued = lock(&self.shared).queue.push(r, visible);
        if queued {
            self.shared.wake.notify_one();
        }
        queued
    }

    /// Requests waiting.
    pub fn pending(&self) -> usize {
        lock(&self.shared).queue.len()
    }

    /// Stops the thread: at most STOP_WAIT_MS for it, then it is let go (it ends by itself when its call returns, and
    /// touches nothing on the way).
    pub fn stop(self) {
        drop(self);
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        {
            let mut g = lock(&self.shared);
            g.exit = true;
            self.shared.wake.notify_all();
        }
        let deadline = Instant::now() + Duration::from_millis(STOP_WAIT_MS);
        let mut g = lock(&self.shared);
        while !g.finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return; // the JoinHandle is dropped: the thread detaches
            }
            g = self.shared.wake.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        drop(g);
        let _ = thread.join();
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Runs `make` on a new thread "tasks-resolver" (idle priority, COM STA) and serves requests with what it returns;
/// `sink` receives the answers in batches. None when the thread cannot be created.
pub fn spawn_with<B: Backend>(
    make: impl FnOnce() -> B + Send + 'static,
    sink: impl Fn(Vec<Answer>) + Send + 'static,
) -> Option<Resolver> {
    let shared = Arc::new(Shared { state: Mutex::new(State::default()), wake: Condvar::new() });
    let worker = Arc::clone(&shared);
    let thread = std::thread::Builder::new()
        .name("tasks-resolver".into())
        .spawn(move || {
            // SAFETY: lowers the priority of this thread only; COM STA for this thread, paired with CoUninitialize.
            let com = unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_IDLE);
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok()
            };
            let mut backend = make();
            serve(&worker, &mut backend, &sink);
            backend.finish();
            drop(backend);
            if com {
                // SAFETY: paired with the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
            let mut g = lock(&worker);
            g.finished = true;
            worker.wake.notify_all();
        })
        .ok()?;
    Some(Resolver { shared, thread: Some(thread) })
}

/// The loop: wait for work (no timeout), take up to BATCH requests, answer them, post them.
fn serve<B: Backend>(shared: &Shared, backend: &mut B, sink: &dyn Fn(Vec<Answer>)) {
    loop {
        let mut batch = Vec::with_capacity(BATCH);
        {
            let mut g = lock(shared);
            while g.queue.is_empty() && !g.exit {
                g = shared.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            if g.exit {
                return;
            }
            while batch.len() < BATCH {
                match g.queue.pop() {
                    Some(r) => batch.push(r),
                    None => break,
                }
            }
        }
        let mut answers = Vec::with_capacity(batch.len());
        for r in batch {
            answers.push(answer(backend, r));
            if lock(shared).exit {
                return;
            }
        }
        sink(answers);
    }
}

fn answer<B: Backend>(b: &mut B, r: Request) -> Answer {
    match r {
        Request::Identity { hwnd, pid, exe } => {
            let (key, aumid) = b.identity(hwnd, pid, &exe);
            Answer::Identity { hwnd, pid, key, aumid }
        }
        Request::Name { identity, exe_id, key, exe, aumid } => {
            Answer::Name { identity, exe_id, name: b.name(&key, &exe, aumid.as_deref()) }
        }
        Request::Icon { identity, exe, aumid, px } => {
            Answer::Icon { identity, px, bitmap: b.icon(&exe, aumid.as_deref(), px) }
        }
        Request::Publisher { exe_id, exe } => Answer::Publisher { exe_id, publisher: b.publisher(&exe) },
    }
}

// ------------------------------------------------------------------ the shell's side

/// What the resolver reads: our local folder (names.bin), the data root (config.json, read-only) and the language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolverDeps {
    pub local: Option<PathBuf>,
    pub data_root: Option<PathBuf>,
    pub italian: bool,
}

impl ResolverDeps {
    pub fn new(paths: &Paths, data_root: Option<&Path>, lang: UiLang) -> ResolverDeps {
        ResolverDeps { local: paths.local.clone(), data_root: data_root.map(Path::to_path_buf), italian: lang.italian }
    }
}

/// `%SystemRoot%` from the API (never from the environment).
pub fn system_root() -> String {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is writable for its whole length, which the slice carries.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    buf.get(..n).map(String::from_utf16_lossy).unwrap_or_default()
}

/// The identity of a window as the resolver gives it: (key, AUMID). COM STA on the calling thread.
pub fn resolve_identity(hwnd: isize, pid: u32, exe: &str) -> (String, Option<String>) {
    let from_window = if hwnd == 0 { None } else { window::aumid(hwnd) };
    let aumid = from_window
        .or_else(|| OwnedProcess::open(pid).and_then(|p| package_aumid(&p)))
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    let key = aumid.clone().unwrap_or_else(|| key_from_exe_path(exe));
    (key, aumid)
}

/// The name of a program as the dock shows it: the dock's rename ("config"), else the shared model's natural name and the
/// step that gave it. The one name function of the resolver and of the tests.
pub fn name_with(
    src: &impl NameSources,
    renames: &BTreeMap<String, String>,
    q: &NameQuery<'_>,
) -> (String, &'static str) {
    if let Some(o) = override_name(renames, q) {
        return (o, "config");
    }
    let (name, step) = natural_name(q, src);
    (name, step.as_str())
}

/// The real backend: the shell model over the dock's sources.
pub struct ShellBackend {
    index: StartMenuIndex,
    dock_names: BTreeMap<String, String>,
    bin: names_bin::NamesBin,
    local: Option<PathBuf>,
    system_root: String,
}

impl ShellBackend {
    /// A backend over an index that is already built (tests; `new` builds this machine's).
    pub fn with_parts(
        index: StartMenuIndex,
        dock_names: BTreeMap<String, String>,
        bin: names_bin::NamesBin,
        local: Option<PathBuf>,
        system_root: String,
    ) -> ShellBackend {
        ShellBackend { index, dock_names, bin, local, system_root }
    }

    fn new(dock_names: BTreeMap<String, String>, bin: names_bin::NamesBin, local: Option<PathBuf>) -> ShellBackend {
        let index = StartMenuIndex::build();
        let s = index.stats();
        oled_sys::log!(
            "tasks: Start Menu index: {} exe targets, per-user parsed {}, all-users {}",
            index.len(),
            s.rust_parsed,
            s.com_common
        );
        ShellBackend { index, dock_names, bin, local, system_root: system_root() }
    }

    fn persist_if_due(&mut self) {
        if self.bin.due() {
            names_bin::save(self.local.as_deref(), &mut self.bin);
        }
    }
}

impl Backend for ShellBackend {
    fn identity(&mut self, hwnd: isize, pid: u32, exe: &str) -> (String, Option<String>) {
        resolve_identity(hwnd, pid, exe)
    }

    fn name(&mut self, key: &str, exe: &str, aumid: Option<&str>) -> Option<String> {
        let q = NameQuery { key, exe_path: exe, aumid };
        if let Some(o) = override_name(&self.dock_names, &q) {
            return Some(o);
        }
        // A packaged program's name depends on its AUMID, not on the exe it runs in: only plain exes are cached.
        let stamp = if aumid.is_none() && !exe.is_empty() { names_bin::stamp_of(exe) } else { None };
        if let Some(s) = stamp {
            if let Some(e) = self.bin.lookup(exe, |_| Some(s)) {
                if !e.name.is_empty() {
                    return Some(e.name.clone());
                }
            }
        }
        // A miss on an old index rebuilds it (the dock's rule).
        if self.index.name(exe).is_none() && self.index.is_stale(Instant::now()) {
            self.index = StartMenuIndex::build();
        }
        let src = IndexedSources { index: &self.index, system_root: &self.system_root };
        let (name, _) = name_with(&src, &BTreeMap::new(), &q);
        if let Some(s) = stamp {
            self.bin.record_name(exe, s, &name);
            self.persist_if_due();
        }
        Some(name).filter(|n| !n.is_empty())
    }

    fn icon(&mut self, exe: &str, aumid: Option<&str>, px: u16) -> Option<IconBitmap> {
        let stamp = if aumid.is_none() && !exe.is_empty() { names_bin::stamp_of(exe) } else { None };
        if let Some(s) = stamp {
            if self.bin.side == px {
                if let Some(pixels) = self.bin.lookup(exe, |_| Some(s)).and_then(|e| e.icon.clone()) {
                    return Some(IconBitmap { width: u32::from(px), height: u32::from(px), pixels });
                }
            }
        }
        let (big, _) = icon::extract(&IconQuery { exe_path: exe, aumid, hwnd: 0 }, &KitIcons)?;
        let small = resize_area(&big, u32::from(px));
        if small.is_empty() || !small.has_visible_pixels() {
            return None;
        }
        if let Some(s) = stamp {
            self.bin.record_icon(exe, s, px, &small.pixels);
            self.persist_if_due();
        }
        Some(small)
    }

    fn publisher(&mut self, exe: &str) -> Option<String> {
        FileInfo::read(exe).company.filter(|c| !c.is_empty())
    }

    fn finish(&mut self) {
        if self.bin.fresh > 0 || !self.bin.entries.is_empty() {
            names_bin::save(self.local.as_deref(), &mut self.bin);
        }
    }
}

/// Starts the resolver; its answers come back as ResolverDone. The dock's renames and names.bin are read here, on the
/// calling (UI) thread, once. None when the thread cannot be created (names are then image names).
pub fn start(proxy: Proxy, deps: ResolverDeps) -> Option<Resolver> {
    let renames = dock_names::load(deps.data_root.as_deref());
    let bin = names_bin::load(deps.local.as_deref()).unwrap_or_default();
    let local = deps.local;
    oled_sys::log!("tasks: resolver started ({} renames, {} cached names)", renames.len(), bin.entries.len());
    spawn_with(
        move || ShellBackend::new(renames, bin, local),
        move |answers| {
            let _ = proxy.send(Box::new(ResolverDone(answers)));
        },
    )
}

// ------------------------------------------------------------------ the icons of the kit

/// The kit's bitmaps for the shell model's icon sources: an empty or invisible one is none.
pub struct KitIcons;

impl IconDecode for KitIcons {
    type Bitmap = IconBitmap;

    fn from_hicon(&self, icon: HICON) -> Option<IconBitmap> {
        gdi::hicon_to_bitmap(icon).filter(|b| !b.is_empty() && b.has_visible_pixels())
    }

    fn from_hbitmap(&self, bmp: HBITMAP) -> Option<IconBitmap> {
        gdi::hbitmap_to_bitmap(bmp).filter(|b| !b.is_empty() && b.has_visible_pixels())
    }
}

/// HICON and HBITMAP to premultiplied 0xAARRGGBB (a port of the dock's reader and of the Files app's).
// Decodes pixel data the system hands back: no unwrap, no expect, no indexing.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod gdi {
    use std::ffi::c_void;
    use std::mem::size_of;

    use oled_ui::gfx::image::{premultiply_px, IconBitmap};
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfoExW, HICON, ICONINFOEXW};

    /// The largest source side read.
    const MAX_SRC_SIDE: i32 = 2048;

    fn bitmap_size(hbm: HBITMAP) -> Option<(i32, i32)> {
        if hbm.0.is_null() {
            return None;
        }
        let mut bm = BITMAP::default();
        // SAFETY: `hbm` is a valid HBITMAP owned by the caller; `bm` is writable and as large as declared.
        let n = unsafe {
            GetObjectW(HGDIOBJ(hbm.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void))
        };
        if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            return None;
        }
        let h = bm.bmHeight.abs();
        (bm.bmWidth <= MAX_SRC_SIDE && h <= MAX_SRC_SIDE).then_some((bm.bmWidth, h))
    }

    /// A top-down 32-bit DIB of `h` rows (each u32 = B | G<<8 | R<<16 | A<<24). The HBITMAP must not be in a DC.
    fn read_dib32(hbm: HBITMAP, w: i32, h: i32) -> Option<Vec<u32>> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut buf = vec![0u32; (w as usize) * (h as usize)];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: GetDC(None) returns the screen DC, released right below. `buf` holds exactly w*h u32, as much as
        // GetDIBits writes for `h` 32-bit rows; `bmi` is valid.
        let lines = unsafe {
            let hdc = GetDC(None);
            if hdc.is_invalid() {
                return None;
            }
            let n = GetDIBits(hdc, hbm, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
            ReleaseDC(None, hdc);
            n
        };
        (lines != 0).then_some(buf)
    }

    fn delete_bitmap(hbm: HBITMAP) {
        if !hbm.0.is_null() {
            // SAFETY: a bitmap made by GetIconInfoExW and owned by us; it is not selected into any DC.
            let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
        }
    }

    /// Does not destroy `icon`. Native size; alpha all 0 in the color bitmap: the AND mask gives the alpha.
    pub fn hicon_to_bitmap(icon: HICON) -> Option<IconBitmap> {
        if icon.is_invalid() {
            return None;
        }
        let mut info = ICONINFOEXW { cbSize: size_of::<ICONINFOEXW>() as u32, ..Default::default() };
        // SAFETY: `icon` is a valid HICON (the caller owns it); `info` has cbSize set and lives for the whole call.
        if !unsafe { GetIconInfoExW(icon, &mut info) }.as_bool() {
            return None;
        }
        let (color, mask) = (info.hbmColor, info.hbmMask);
        let out = if color.0.is_null() { monochrome_icon(mask) } else { color_icon(color, mask) };
        // GetIconInfoExW creates two new bitmaps: we delete them.
        delete_bitmap(color);
        delete_bitmap(mask);
        out
    }

    fn color_icon(color: HBITMAP, mask: HBITMAP) -> Option<IconBitmap> {
        let (w, h) = bitmap_size(color)?;
        let mut px = read_dib32(color, w, h)?;
        if px.iter().any(|p| p >> 24 != 0) {
            for p in px.iter_mut() {
                *p = premultiply_px(*p);
            }
        } else {
            let m = match bitmap_size(mask) {
                Some((mw, mh)) if mw == w && mh >= h => read_dib32(mask, mw, mh),
                _ => None,
            };
            match m {
                Some(m) => {
                    for (p, mk) in px.iter_mut().zip(m.iter()) {
                        *p = if mk & 0x00FF_FFFF == 0 { 0xFF00_0000 | (*p & 0x00FF_FFFF) } else { 0 };
                    }
                }
                None => {
                    for p in px.iter_mut() {
                        *p |= 0xFF00_0000;
                    }
                }
            }
        }
        Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
    }

    /// No color bitmap: the mask is twice as tall (AND on top, XOR below).
    fn monochrome_icon(mask: HBITMAP) -> Option<IconBitmap> {
        let (w, h2) = bitmap_size(mask)?;
        let h = h2 / 2;
        if h <= 0 {
            return None;
        }
        let m = read_dib32(mask, w, h2)?;
        let n = (w as usize) * (h as usize);
        let (and_rows, xor_rows) = m.split_at_checked(n)?;
        let px: Vec<u32> = and_rows
            .iter()
            .zip(xor_rows.iter())
            .map(|(a, x)| match (a & 0x00FF_FFFF != 0, x & 0x00FF_FFFF != 0) {
                (false, false) => 0xFF00_0000,
                (false, true) | (true, true) => 0xFFFF_FFFF,
                (true, false) => 0,
            })
            .collect();
        Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
    }

    /// Does not destroy `bmp`; a bitmap without real alpha is opaque.
    pub fn hbitmap_to_bitmap(bmp: HBITMAP) -> Option<IconBitmap> {
        let (w, h) = bitmap_size(bmp)?;
        let mut px = read_dib32(bmp, w, h)?;
        if px.iter().any(|p| p >> 24 != 0) {
            for p in px.iter_mut() {
                *p = premultiply_px(*p);
            }
        } else {
            for p in px.iter_mut() {
                *p |= 0xFF00_0000;
            }
        }
        Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
    }
}

// ------------------------------------------------------------------ the name book

/// A window's identity as the resolver gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub key: String,
    pub aumid: Option<String>,
}

/// What the UI asks for once per session (see NameBook::first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ask {
    Identity(isize, u32),
    Name(IdentityId),
    ExeName(ExeId),
    Icon(IdentityId),
    Publisher(ExeId),
}

struct IconEntry {
    bitmap: IconBitmap,
    last: std::cell::Cell<u64>,
}

/// At most MAX_ICONS bitmaps and MAX_ICON_BYTES (w x h x 4 each); the least recently drawn goes first.
#[derive(Default)]
pub struct IconStore {
    map: HashMap<IdentityId, IconEntry>,
    bytes: usize,
    tick: std::cell::Cell<u64>,
}

fn icon_bytes(b: &IconBitmap) -> usize {
    (b.width as usize).saturating_mul(b.height as usize).saturating_mul(4)
}

impl IconStore {
    /// The icon of `id`; counts as drawn.
    pub fn get(&self, id: IdentityId) -> Option<&IconBitmap> {
        let e = self.map.get(&id)?;
        self.tick.set(self.tick.get() + 1);
        e.last.set(self.tick.get());
        Some(&e.bitmap)
    }

    /// Stores an icon (a bigger one than the whole budget is refused); evicts the least recently drawn until both limits
    /// hold. true when it was stored.
    pub fn insert(&mut self, id: IdentityId, bitmap: IconBitmap) -> bool {
        let size = icon_bytes(&bitmap);
        if size == 0 || size > MAX_ICON_BYTES {
            return false;
        }
        if let Some(old) = self.map.remove(&id) {
            self.bytes -= icon_bytes(&old.bitmap);
        }
        while self.map.len() >= MAX_ICONS || self.bytes + size > MAX_ICON_BYTES {
            let Some(oldest) = self.map.iter().min_by_key(|(_, e)| e.last.get()).map(|(k, _)| *k) else { break };
            if let Some(e) = self.map.remove(&oldest) {
                self.bytes -= icon_bytes(&e.bitmap);
            }
        }
        self.tick.set(self.tick.get() + 1);
        self.bytes += size;
        self.map.insert(id, IconEntry { bitmap, last: std::cell::Cell::new(self.tick.get()) });
        true
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The names the rows read, by identity, and the icons and publishers beside them.
#[derive(Default)]
pub struct NameBook {
    identities: HashMap<(isize, u32), Identity>,
    names: HashMap<IdentityId, Box<str>>,
    exe_names: HashMap<ExeId, Box<str>>,
    publishers: HashMap<ExeId, Box<str>>,
    icons: IconStore,
    asked: HashSet<Ask>,
    /// Scenes: identity key -> name.
    fixture: HashMap<String, String>,
}

impl NameBook {
    /// Scenes: identity -> name (the identity key as written in the scene).
    pub fn set_fixture_names(&mut self, names: &[(String, String)]) {
        self.fixture = names.iter().cloned().collect();
    }

    /// true the first time `a` is asked in this session: the caller then sends the request (each key once; a failure is
    /// kept as none, so it is not asked again).
    pub fn first(&mut self, a: Ask) -> bool {
        self.asked.insert(a)
    }

    pub fn identity(&self, hwnd: isize, pid: u32) -> Option<&Identity> {
        self.identities.get(&(hwnd, pid))
    }

    /// The name of an identity (`key` is its identity key, for scenes' fixture names).
    pub fn name_of_identity(&self, id: IdentityId, key: &str) -> Option<&str> {
        self.names.get(&id).map(|n| &**n).or_else(|| self.fixture.get(key).map(String::as_str))
    }

    pub fn name_of_exe(&self, id: ExeId) -> Option<&str> {
        self.exe_names.get(&id).map(|n| &**n)
    }

    pub fn publisher_of(&self, id: ExeId) -> Option<&str> {
        self.publishers.get(&id).map(|n| &**n)
    }

    /// The icon of an identity; counts as drawn.
    pub fn icon_of(&self, id: IdentityId) -> Option<&IconBitmap> {
        self.icons.get(id)
    }

    pub fn icons(&self) -> &IconStore {
        &self.icons
    }

    fn set_name<K: Copy + Eq + std::hash::Hash>(map: &mut HashMap<K, Box<str>>, k: K, v: &str) -> bool {
        if map.get(&k).is_some_and(|old| &**old == v) {
            return false;
        }
        map.insert(k, v.into());
        true
    }
}

/// true when a visible name or icon changed, or an identity arrived or changed (the pipeline runs again).
pub fn on_done(b: &mut NameBook, m: ResolverDone) -> bool {
    let mut changed = false;
    for a in m.0 {
        match a {
            Answer::Nothing => {}
            Answer::Identity { hwnd, pid, key, aumid } => {
                let id = Identity { key, aumid };
                if b.identities.get(&(hwnd, pid)) != Some(&id) {
                    b.identities.insert((hwnd, pid), id);
                    changed = true;
                }
            }
            Answer::Name { identity, exe_id, name } => {
                let Some(name) = name else { continue };
                if let Some(i) = identity {
                    changed |= NameBook::set_name(&mut b.names, i, &name);
                }
                if let Some(e) = exe_id {
                    changed |= NameBook::set_name(&mut b.exe_names, e, &name);
                }
            }
            Answer::Icon { identity, bitmap, .. } => {
                if let Some(bitmap) = bitmap {
                    changed |= b.icons.insert(identity, bitmap);
                }
            }
            Answer::Publisher { exe_id, publisher } => {
                if let Some(p) = publisher {
                    // A publisher is not drawn in a row: it changes nothing the pipeline shows.
                    NameBook::set_name(&mut b.publishers, exe_id, &p);
                }
            }
        }
    }
    changed
}

// ------------------------------------------------------------------ --list-apps

/// The two lines --list-apps prints per group, in the dock's format.
pub fn list_apps_lines(name: &str, step: &str, key: &str) -> [String; 2] {
    [format!("- name   : {name}   [{step}]"), format!("  key    : {key}")]
}
