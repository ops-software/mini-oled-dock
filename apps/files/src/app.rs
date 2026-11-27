//! The window's start (P3-files-app 2.2): the step plan (FZ2, pure) and run, which executes it (FA-WIN, F1a.10).

use std::sync::Arc;
use std::time::{Duration, Instant};

use oled_trust::frame::files::{InstanceCmd, Location};
use oled_ui::host::{EventLoop, LoopCx, WindowRef};
use oled_ui::theme::ThemeSpec;
use oled_ui::{PointP, Screen, UiLang};
use windows::core::PWSTR;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
    FOLDERID_Videos, SHGetKnownFolderPath, SHGetStockIconInfo, KF_FLAG_DEFAULT, SHGSI_ICON, SHSTOCKICONINFO,
    SIID_FOLDER,
};
use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

use crate::bg::Lane;
use crate::cli::{self, Cli};
use crate::format::Fmt;
use crate::hub::Hub;
use crate::instance::{self, Elevation, ForwardError, InstanceNames, Ipc, MutexState};
use crate::listing::{Batch, CancelToken, EnumJob, Gen, WidePath};
use crate::msg::{JobDone, KnownName, Msg};
use crate::services::{self, Services};
use crate::session::{Session, SessionStore};
use crate::types::TypeNames;
use crate::window::{self, FilesScreen, LoopOps, LoopReq, Shared, Show, WinKind, WinRec, STARTUP_TAB};
use crate::{names, notices};

/// One step of a start, in the order run executes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Log,
    HandOff,
    Forward,
    ExitNotForwarded,
    Mutex,
    Services,
    Enumerate,
    StartJobs,
    Config,
    Reopen,
    Identity,
    Loop,
    Ipc,
    WaitFirstBatch,
    OpenWindow,
    Follow,
    Run,
    Preview,
    Selftest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartEnv {
    pub elevation: Elevation,
}

/// A window start: [Log, HandOff] when the token is UAC-elevated (nothing else); else [Log, Forward], then
/// [ExitNotForwarded] alone for --forward-only, else Mutex, Services, Enumerate (a folder or file target only),
/// StartJobs, Config, Reopen, Identity, Loop, Ipc, WaitFirstBatch, OpenWindow, Follow, Run. --preview and --selftest
/// are one step each and never hand off, take the mutex or open a window, whatever the elevation.
pub fn start_plan(cli: &Cli, env: &StartEnv) -> Vec<Step> {
    use Step::*;
    match cli {
        Cli::Preview { .. } => vec![Preview],
        Cli::Selftest { .. } => vec![Selftest],
        Cli::Window { .. } if instance::must_hand_off(env.elevation) => vec![Log, HandOff],
        Cli::Window { forward_only: true, .. } => vec![Log, Forward, ExitNotForwarded],
        Cli::Window { target, select, .. } => {
            let mut plan = vec![Log, Forward, Mutex, Services];
            if target.is_some() || select.is_some() {
                plan.push(Enumerate);
            }
            plan.extend([StartJobs, Config, Reopen, Identity, Loop, Ipc, WaitFirstBatch, OpenWindow, Follow, Run]);
            plan
        }
    }
}

/// What a step tells the executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Next,
    Exit(i32),
}

/// Does the steps; start_plan says which and in what order.
pub trait StepRunner {
    fn run(&mut self, step: Step) -> Flow;
}

/// Runs the plan until a step exits or the plan ends (then EXIT_OK).
pub fn execute(plan: &[Step], runner: &mut dyn StepRunner) -> i32 {
    for step in plan {
        if let Flow::Exit(code) = runner.run(*step) {
            return code;
        }
    }
    names::EXIT_OK
}

/// Executes start_plan (F1a.10).
pub fn run(cli: Cli) -> i32 {
    let plan = start_plan(&cli, &StartEnv { elevation: instance::elevation() });
    let mut real = Real::new(cli);
    let code = execute(&plan, &mut real);
    real.finish();
    code
}

/// How long a start that found the mutex taken keeps trying to hand its job to the other process.
const MUTEX_WAIT: Duration = Duration::from_millis(500);
const MUTEX_STEP: Duration = Duration::from_millis(25);
/// How long the window waits for the first rows of the start-up listing.
const FIRST_BATCH_WAIT: Duration = Duration::from_millis(60);

/// The state the real steps hand to one another.
struct Real {
    cli: Cli,
    /// The process's creation, as an Instant.
    started: Instant,
    log: Option<std::thread::JoinHandle<()>>,
    names: InstanceNames,
    mutex: Option<MutexState>,
    hub: Option<Arc<Hub>>,
    services: Option<Services>,
    first: Option<(Location, Option<WidePath>)>,
    cancel: Option<CancelToken>,
    watch: Option<oled_config::theme::ThemeWatch>,
    lang: UiLang,
    lang_setting: oled_ui::Lang,
    shared: Option<std::rc::Rc<Shared>>,
    reopen: Vec<Vec<Location>>,
    lp: Option<EventLoop>,
    ipc: Option<Ipc>,
    batch: Option<Batch>,
}

impl Real {
    fn new(cli: Cli) -> Real {
        Real {
            cli,
            started: process_start(),
            log: None,
            names: InstanceNames::for_process(),
            mutex: None,
            hub: None,
            services: None,
            first: None,
            cancel: None,
            watch: None,
            lang: UiLang::EN,
            lang_setting: oled_ui::Lang::Auto,
            shared: None,
            reopen: Vec::new(),
            lp: None,
            ipc: None,
            batch: None,
        }
    }

    fn finish(&mut self) {
        oled_sys::logger::stop(self.log.take());
    }

    /// The first location and the item to select, from the command line (Home without a target).
    fn target(&mut self) -> (Location, Option<WidePath>) {
        if let Some(t) = &self.first {
            return t.clone();
        }
        let t = match instance::current_dir().ok_or_else(String::new).and_then(|cwd| cli::to_instance(&self.cli, &cwd))
        {
            Ok(InstanceCmd::NewWindow(l) | InstanceCmd::OpenTab(l)) => (l, None),
            Ok(InstanceCmd::Select { dir, item }) => (Location::Fs(dir), Some(item)),
            Ok(InstanceCmd::Probe { .. }) | Err(_) => (Location::Home, None),
        };
        self.first = Some(t.clone());
        t
    }

    fn forward(&mut self) -> Flow {
        let cmd = match instance::current_dir().ok_or_else(|| "files: the current folder is not a path".to_string()) {
            Ok(cwd) => cli::to_instance(&self.cli, &cwd),
            Err(e) => Err(e),
        };
        let cmd = match cmd {
            Ok(c) => c,
            Err(e) => {
                oled_sys::log!("{e}");
                return Flow::Exit(names::EXIT_USAGE);
            }
        };
        match instance::try_forward(&self.names, &cmd) {
            Ok(ms) => {
                oled_sys::log!("files: handed to the running window in {ms} ms");
                Flow::Exit(names::EXIT_OK)
            }
            Err(ForwardError::NoInstance) => Flow::Next,
            Err(e) => {
                oled_sys::log!("files: the running window did not take the request ({e:?}): an own window opens");
                Flow::Next
            }
        }
    }

    /// Takes the single-instance mutex; when another process holds it, hands the job over again every 25 ms for at most
    /// 500 ms, then goes on with a window of its own.
    fn mutex(&mut self) -> Flow {
        let state = instance::take_mutex(&self.names);
        if matches!(state, MutexState::Busy) {
            let give_up = Instant::now() + MUTEX_WAIT;
            while Instant::now() < give_up {
                std::thread::sleep(MUTEX_STEP);
                if let Flow::Exit(code) = self.forward() {
                    return Flow::Exit(code);
                }
            }
        }
        self.mutex = Some(state);
        Flow::Next
    }

    fn enumerate(&mut self) {
        let (first, _) = self.target();
        let (Location::Fs(dir), Some(hub), Some(services)) = (first, &self.hub, &self.services) else { return };
        hub.route(STARTUP_TAB, 1);
        let cancel = CancelToken::new();
        services.enumr.submit(EnumJob { tab: STARTUP_TAB, gen: Gen(1), dir, cancel: cancel.clone() });
        self.cancel = Some(cancel);
    }

    fn start_jobs(&mut self) {
        let Some(services) = &self.services else { return };
        services.bg.run(0, Lane::Local, Box::new(|| Some(JobDone::StockFolderIcon(stock_folder_icon()))));
        services.bg.run(0, Lane::Local, Box::new(|| Some(JobDone::KnownFolders(known_folders()))));
    }

    fn config(&mut self) -> Flow {
        let _root = oled_sys::datadir::open_existing(false);
        let watch = oled_config::theme::ThemeWatch::open();
        self.lang_setting = oled_ui::theme_lang(watch.current().language);
        self.lang = UiLang::from_lang(self.lang_setting);
        let install_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(std::path::Path::to_path_buf));
        crate::langfile::load_lang_file(install_dir.as_deref(), self.lang.italian);
        let (Some(hub), Some(services)) = (self.hub.clone(), self.services.take()) else {
            oled_sys::log!("files: the services were not started");
            return Flow::Exit(1);
        };
        let session = Session::load(&*services.own);
        // SAFETY: no arguments; reads the user's default UI language.
        let ui_lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        let types = TypeNames::load(&*services.own, ui_lang);
        let shared = Shared::new(hub, services, self.lang, session, types);
        *shared.memory.borrow_mut() = Box::new(window::private_bytes);
        self.watch = Some(watch);
        self.shared = Some(shared);
        Flow::Next
    }

    fn reopen(&mut self) {
        if let Some(shared) = &self.shared {
            self.reopen = notices::take_reopen(&mut shared.session.borrow_mut());
        }
    }

    fn make_loop(&mut self) -> Flow {
        let (Some(shared), Some(watch)) = (&self.shared, &self.watch) else { return Flow::Exit(1) };
        match EventLoop::new(names::APP, self.lang_setting, ThemeSpec::from(watch.current())) {
            Ok(mut lp) => {
                lp.set_placement_store(Box::new(SessionStore::new(std::rc::Rc::clone(shared))));
                self.lp = Some(lp);
                Flow::Next
            }
            Err(e) => {
                oled_sys::log!("files: no event loop ({e:?})");
                Flow::Exit(1)
            }
        }
    }

    fn ipc(&mut self) {
        let Some(hub) = &self.hub else { return };
        match instance::create_ipc(&self.names, Arc::clone(hub)) {
            Ok(ipc) => self.ipc = Some(ipc),
            Err(e) => oled_sys::log!("files: the instance window was not made ({e})"),
        }
    }

    fn wait_first_batch(&mut self) {
        if self.cancel.is_some() {
            if let Some(hub) = &self.hub {
                self.batch = hub.wait_first_batch(self.started + FIRST_BATCH_WAIT);
            }
        }
    }

    fn open_window(&mut self) -> Flow {
        let (mut first, select) = self.target();
        let (Some(shared), Some(lp)) = (self.shared.clone(), self.lp.as_mut()) else { return Flow::Exit(1) };
        let win = shared.new_win();
        // Reopen (F4.14): a start with no target restores the first saved window, every tab, instead of Home; nothing
        // was enumerated for it, so its tab is a fresh one that lists at once.
        let (mut tab, mut rest) = (STARTUP_TAB, Vec::new());
        if first == Location::Home && select.is_none() && !self.reopen.is_empty() {
            let mut saved = self.reopen.remove(0).into_iter();
            if let Some(f) = saved.next() {
                (first, tab, rest) = (f, shared.new_tab(), saved.collect());
            }
        }
        let mut screen = FilesScreen::new(std::rc::Rc::clone(&shared), win, first.clone(), select, tab);
        notices::restore_tabs(&mut screen, rest);
        screen.set_origin(self.started);
        if let Some(cancel) = self.cancel.take() {
            screen.adopt_start(cancel);
        }
        if let Some(batch) = self.batch.take() {
            screen.adopt_first_batch(batch);
        }
        let wref = match lp.open(window::main_spec(&shared), screen) {
            Ok(w) => w,
            Err(e) => {
                oled_sys::log!("files: no window ({e:?})");
                return Flow::Exit(1);
            }
        };
        let hwnd = lp.hwnd(wref).map(|h| h.0 as isize);
        shared.windows.borrow_mut().push(WinRec {
            win,
            kind: WinKind::Main,
            wref: Some(wref),
            hwnd,
            location: Some(first),
        });
        shared.hub.attach(win, Box::new(lp.proxy(wref)));
        for window in std::mem::take(&mut self.reopen) {
            if let Some(first) = window.into_iter().next() {
                window::request_loop(&shared, LoopReq::Main { first, select: None, cascade_from: Some(win) });
            }
        }
        Flow::Next
    }

    fn follow(&mut self) {
        let (Some(shared), Some(lp), Some(watch)) = (self.shared.clone(), self.lp.as_mut(), self.watch.take()) else {
            return;
        };
        let on_theme = {
            let shared = std::rc::Rc::clone(&shared);
            Box::new(move |cx: &mut LoopCx, file: &oled_config::theme::ThemeFile| on_theme_change(&shared, cx, file))
        };
        if let Err(e) = lp.follow_theme_file(watch, on_theme) {
            oled_sys::log!("files: the theme file is not followed ({e:?})");
        }
        let on_loop = {
            let shared = std::rc::Rc::clone(&shared);
            Box::new(move |cx: &mut LoopCx| window::run_loop_requests(&shared, &mut LoopCxOps { cx }))
        };
        if let Err(e) = lp.add_handle(shared.loop_event.handle(), on_loop) {
            oled_sys::log!("files: the loop requests are not followed ({e:?})");
        }
    }

    fn run_loop(&mut self) -> Flow {
        let Some(lp) = self.lp.take() else { return Flow::Exit(1) };
        let code = lp.run();
        if let Some(mutex) = self.mutex.take() {
            instance::release(mutex, self.ipc.take());
        }
        if let Some(shared) = &self.shared {
            shared.services.ops.shutdown();
            shared.services.shell.shutdown();
            // Reopen (F4.14): the new process starts only now, when it can no longer find this one.
            notices::restart_if_due(shared);
        }
        Flow::Exit(code)
    }
}

impl StepRunner for Real {
    fn run(&mut self, step: Step) -> Flow {
        match step {
            Step::Log => {
                self.log = oled_sys::logger::start("", &crate::LOG_UI);
                Flow::Next
            }
            Step::HandOff => Flow::Exit(instance::elevated_handoff(&self.cli)),
            Step::Forward => self.forward(),
            Step::ExitNotForwarded => Flow::Exit(names::EXIT_NOT_FORWARDED),
            Step::Mutex => self.mutex(),
            Step::Services => {
                let hub = Hub::new();
                let exe = std::env::current_exe().unwrap_or_default();
                self.services = Some(services::real(&hub, exe, None));
                self.hub = Some(hub);
                Flow::Next
            }
            Step::Enumerate => {
                self.enumerate();
                Flow::Next
            }
            Step::StartJobs => {
                self.start_jobs();
                Flow::Next
            }
            Step::Config => self.config(),
            Step::Reopen => {
                self.reopen();
                Flow::Next
            }
            Step::Identity => {
                instance::set_identity();
                Flow::Next
            }
            Step::Loop => self.make_loop(),
            Step::Ipc => {
                self.ipc();
                Flow::Next
            }
            Step::WaitFirstBatch => {
                self.wait_first_batch();
                Flow::Next
            }
            Step::OpenWindow => self.open_window(),
            Step::Follow => {
                self.follow();
                Flow::Next
            }
            Step::Run => self.run_loop(),
            Step::Preview => match &self.cli {
                Cli::Preview { dir, scene, lang, dpi } => {
                    Flow::Exit(crate::preview::run(dir, scene.as_deref(), lang.as_deref(), *dpi))
                }
                _ => Flow::Exit(names::EXIT_USAGE),
            },
            Step::Selftest => match &self.cli {
                Cli::Selftest { nonce } => Flow::Exit(crate::selftest::run(nonce)),
                _ => Flow::Exit(names::EXIT_USAGE),
            },
        }
    }
}

/// The theme file changed: the kit repaints by itself (set_theme). A new language is set on the kit's side, in the
/// window's formats and in every main window, which rebuilds what it caches (Msg::Lang).
pub fn on_theme_change(shared: &Shared, cx: &mut LoopCx, file: &oled_config::theme::ThemeFile) {
    let setting = oled_ui::theme_lang(file.language);
    let lang = UiLang::from_lang(setting);
    if lang == shared.lang.get() {
        return;
    }
    cx.set_lang(setting);
    shared.lang.set(lang);
    *shared.fmt.borrow_mut() = std::rc::Rc::new(Fmt::system(lang));
    let windows: Vec<WindowRef> = shared.windows.borrow().iter().filter_map(|w| w.wref).collect();
    for w in windows {
        cx.send(w, Box::new(Msg::Lang));
    }
}

/// What run_loop_requests needs, over the loop callback's LoopCx.
struct LoopCxOps<'a, 'b> {
    cx: &'a mut LoopCx<'b>,
}

impl LoopOps for LoopCxOps<'_, '_> {
    fn open(&mut self, spec: oled_ui::host::WindowSpec, screen: Box<dyn Screen>) -> Option<WindowRef> {
        match self.cx.open(spec, screen) {
            Ok(w) => Some(w),
            Err(e) => {
                oled_sys::log!("files: no window ({e:?})");
                None
            }
        }
    }

    fn proxy(&self, w: WindowRef) -> Option<oled_ui::host::Proxy> {
        self.cx.proxy(w)
    }

    fn hwnd(&self, w: WindowRef) -> Option<isize> {
        self.cx.hwnd(w).map(|h| h.0 as isize)
    }

    fn show(&mut self, w: WindowRef, how: Show) -> bool {
        self.cx.show(w, how)
    }

    fn move_to(&mut self, w: WindowRef, at: PointP) -> bool {
        self.cx.move_to(w, at)
    }

    fn quit(&mut self) {
        self.cx.quit(names::EXIT_OK);
    }
}

/// The process's creation time as an Instant: now minus the time since GetProcessTimes' creation FILETIME.
fn process_start() -> Instant {
    let now = Instant::now();
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo handle of this process and four valid out buffers.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if ok.is_err() {
        return now;
    }
    // SAFETY: no arguments.
    let at = unsafe { GetSystemTimeAsFileTime() };
    let ticks = |f: &FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let since = ticks(&at).saturating_sub(ticks(&created)).min(10 * 10_000_000);
    now.checked_sub(Duration::from_nanos(since * 100)).unwrap_or(now)
}

/// Shell32's own stock folder icon (no extension, no host round trip): (side px, premultiplied BGRA bytes).
fn stock_folder_icon() -> Option<(u16, Arc<[u8]>)> {
    let mut info = SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
    // SAFETY: `info` has cbSize set and lives for the call.
    unsafe { SHGetStockIconInfo(SIID_FOLDER, SHGSI_ICON, &mut info) }.ok()?;
    let bgra = crate::decor::gdi::hicon_to_bgra(info.hIcon);
    // SAFETY: the icon was made for us by the call above and is not used after this line.
    let _ = unsafe { DestroyIcon(info.hIcon) };
    let b = bgra?;
    let side = u16::try_from(b.w).ok().filter(|s| *s > 0 && b.w == b.h)?;
    Some((side, Arc::from(crate::decor::gdi::to_bytes(&b))))
}

/// The seven known folders KnownName names, the ones that exist.
fn known_folders() -> Vec<(KnownName, WidePath)> {
    let ids = [
        (KnownName::Desktop, &FOLDERID_Desktop),
        (KnownName::Downloads, &FOLDERID_Downloads),
        (KnownName::Documents, &FOLDERID_Documents),
        (KnownName::Pictures, &FOLDERID_Pictures),
        (KnownName::Music, &FOLDERID_Music),
        (KnownName::Videos, &FOLDERID_Videos),
        (KnownName::Profile, &FOLDERID_Profile),
    ];
    let mut out = Vec::new();
    for (name, id) in ids {
        // SAFETY: a known folder id and default flags; the returned string is freed below.
        let Ok(p): Result<PWSTR, _> = (unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }) else { continue };
        // SAFETY: `p` is the NUL-terminated string returned by the call above, read once.
        let units: Vec<u16> = unsafe { p.as_wide() }.to_vec();
        // SAFETY: allocated with CoTaskMemAlloc by SHGetKnownFolderPath; not used afterwards.
        unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
        if let Ok(path) = WidePath::new(&units) {
            out.push((name, path));
        }
    }
    out
}
