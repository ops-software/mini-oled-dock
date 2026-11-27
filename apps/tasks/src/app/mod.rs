//! The process around the window: the start order of F-T2 (main), the dependencies the screen reads (Deps) and one
//! module per concern, each filled by the group its file names.

pub mod access;
pub mod args;
pub mod dock_names;
pub mod exec;
#[cfg(feature = "fault-inject")]
pub mod fault;
pub mod instance;
pub mod launch;
#[cfg(feature = "preview")]
pub mod list_apps;
pub mod log;
pub mod names_bin;
pub mod options;
pub mod paths;
pub mod presence;
pub mod resolver;
pub mod sampling;
pub mod selftest;
pub mod state;
pub mod updated;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use oled_config::Store;
use oled_sysmon::fmt::NumFmt;
use oled_trust::frame::tasks::{Show, AUMID, WINDOW_CLASS};
use oled_ui::host::{AppId, Caption, EventLoop, PlacementKey, PlacementStore, WindowKind, WindowSpec};
use oled_ui::{SizeL, ThemeSpec, UiLang};
use windows::core::HSTRING;
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;

use crate::model::view::ViewMode;
use crate::strings::{self, k};
use crate::ui::screen::TasksScreen;

/// "Beta" in the title and in About while true; flips in its own commit when Tasks goes stable (Phase 4).
pub const STAGE_BETA: bool = true;
/// The key the main window's placement is stored under.
pub const MAIN_PLACEMENT: PlacementKey = PlacementKey("main");
/// The window's size at its first start and its minimum, logical px.
const WINDOW_SIZE: SizeL = SizeL { w: 640, h: 560 };
const WINDOW_MIN: SizeL = SizeL { w: 520, h: 400 };
/// The system root of Deps::inert: the fixtures' Windows folder (scenes and Harness tests classify against it).
const INERT_SYSTEM_ROOT: &str = "C:\\Windows";

/// What every part of the screen reads; moved into the TasksScreen when the window opens.
pub struct Deps {
    pub lang: UiLang,
    pub nf: NumFmt,
    pub options: Arc<Store<options::TasksOptions>>,
    pub state: state::StateHandle,
    pub sampling: sampling::Sampling,
    pub resolver: Option<resolver::Resolver>,
    pub launcher: Arc<dyn launch::Launcher>,
    pub exec: Arc<dyn exec::Executor>,
    pub instance: Option<instance::Guard>,
    pub system_root: String,
    pub install_dir: Option<PathBuf>,
    pub show: Show,
}

impl Deps {
    /// No thread, no file, no process: Sampling::inert(), an options store that never writes, an empty state, no
    /// resolver. Scenes, Harness tests and --preview run on it, with a recording launcher and executor.
    pub fn inert(lang: UiLang, launcher: Arc<dyn launch::Launcher>, exec: Arc<dyn exec::Executor>) -> Deps {
        Deps {
            lang,
            nf: num_fmt(lang),
            options: Arc::new(Store::new(None, options::on_set)),
            state: state::StateHandle::default(),
            sampling: sampling::Sampling::inert(),
            resolver: None,
            launcher,
            exec,
            instance: None,
            system_root: INERT_SYSTEM_ROOT.to_string(),
            install_dir: None,
            show: Show::default(),
        }
    }
}

/// tasks.exe: the start order of F-T2.
pub fn main() -> ExitCode {
    // (1) Every DLL loaded later comes from System32 only.
    // SAFETY: no pointers; sets this process's default DLL search path.
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
    // (2) The arguments.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let show = match args::parse(&raw) {
        Ok(args::Cli::Run(show)) => show,
        // (3) Handled before anything else: no mutex, no window, no forward.
        Ok(args::Cli::Selftest { nonce }) => return selftest::run(&nonce),
        Ok(args::Cli::Preview { dir, scene, lang, dpi }) => return preview(&dir, scene.as_deref(), lang, dpi),
        Ok(args::Cli::ListApps) => return list_apps(),
        Ok(args::Cli::Version) => {
            console_line(&format!("tasks {}", env!("CARGO_PKG_VERSION")));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            console_line(&format!("tasks: {e}"));
            return ExitCode::from(2);
        }
    };
    // (4) The fault switch of the live chord matrix (never in a shipped build).
    #[cfg(feature = "fault-inject")]
    fault::at_start();
    // (5) Our folders and the log.
    let paths = paths::init();
    let log = log::start(&paths);
    let code = run_window(show, &paths);
    oled_sys::logger::stop(log);
    code
}

/// Steps (6) to (14) of F-T2: the instance, the AUMID, the theme and language, the stores, the sampler, the loop and
/// the window.
fn run_window(show: Show, paths: &paths::Paths) -> ExitCode {
    // (6) One window per user session: forward to a running Tasks, or own the name, or go on unguarded.
    let instance = match instance::acquire(&show) {
        instance::Acquire::Forwarded => return ExitCode::SUCCESS,
        instance::Acquire::NoAnswer => return ExitCode::from(3),
        instance::Acquire::Owner(guard) => Some(guard),
        instance::Acquire::Unguarded => {
            oled_sys::log!("tasks: instance unguarded");
            None
        }
    };
    // (7) The AUMID, before any window.
    // SAFETY: a NUL-terminated string that lives for the call.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID)) } {
        oled_sys::log!("tasks: AUMID not set ({:#x})", e.code().0);
    }
    // (8) The data root (read-only), the theme and the language (Auto: Windows').
    let root = oled_sys::datadir::open_existing(false);
    let watch = oled_config::theme::ThemeWatch::open();
    let lang_setting = oled_ui::theme_lang(watch.current().language);
    oled_sys::i18n::apply(lang_setting);
    let lang = UiLang::from_lang(lang_setting);
    let install_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    updated::load_lang_file(install_dir.as_deref(), lang.italian);
    // (9) The stores, the launcher and the executor.
    let options = options::load(paths);
    let state = state::load(paths);
    let launcher: Arc<dyn launch::Launcher> = Arc::new(launch::Win32Launcher);
    let executor = exec::Win32Executor::new(Arc::clone(&launcher));
    // (10) The sampler starts now, so snapshot A runs while the window is built.
    let first =
        sampling::ControlInputs { active: true, mode: state.view().unwrap_or(ViewMode::Grouped), ..Default::default() };
    let sampling = sampling::start(sampling::control_of(&first, &options.get(), 0));
    let attacher = sampling.attacher();
    // (11) The loop, the placement store and the theme file.
    let mut event_loop = match EventLoop::new(AppId("tasks"), lang_setting, ThemeSpec::from(watch.current())) {
        Ok(l) => l,
        Err(e) => {
            oled_sys::log!("tasks: no event loop ({e:?})");
            return ExitCode::from(1);
        }
    };
    event_loop.set_placement_store(Box::new(state.clone()));
    // A language change applies to the process's strings; the window takes it at its next start (the kit's loop has
    // no per-window language switch yet).
    let on_change = Box::new(|_: &mut oled_ui::host::LoopCx, f: &oled_config::theme::ThemeFile| {
        oled_sys::i18n::apply(oled_ui::theme_lang(f.language));
    });
    if let Err(e) = event_loop.follow_theme_file(watch, on_change) {
        oled_sys::log!("tasks: the theme file is not followed ({e:?})");
    }
    // (12) The window.
    let name = strings::app_name(lang);
    let title_key = if STAGE_BETA { k::TASKS_WINDOW_TITLE_BETA } else { k::TASKS_WINDOW_TITLE };
    let title = oled_sys::i18n::subst(&strings::t(lang, title_key), &[("name", name.as_ref())]);
    let stored = state.load(MAIN_PLACEMENT).is_some();
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: WINDOW_MIN, caption: Caption::System },
        class: WINDOW_CLASS,
        title: title.into(),
        size: WINDOW_SIZE,
        owner: None,
        esc_closes: true,
        placement: Some(MAIN_PLACEMENT),
        icon_resource: Some(1),
        opened_by_chord: show.from_chord || !stored,
    };
    let deps = Deps {
        lang,
        nf: num_fmt(lang),
        options,
        state: state.clone(),
        sampling,
        resolver: None,
        launcher,
        exec: executor.clone(),
        instance,
        system_root: system_root(),
        install_dir,
        show,
    };
    let w = match event_loop.open(spec, TasksScreen::new(deps)) {
        Ok(w) => w,
        Err(e) => {
            oled_sys::log!("tasks: no window ({e:?})");
            return ExitCode::from(1);
        }
    };
    // (13) Everything that posts to the window gets its proxy. The resolver needs the proxy to start, so it starts
    // here and reaches the screen as a message (a frame or a name posted before attach is sent at attach).
    let proxy = event_loop.proxy(w);
    attacher.attach(proxy.clone());
    let poster = proxy.clone();
    state.attach(Box::new(move || poster.send(Box::new(state::StateDirty))));
    executor.attach(proxy.clone());
    let _presence = event_loop.hwnd(w).map(|hwnd| presence::install(proxy.clone(), hwnd));
    let names = resolver::ResolverDeps::new(paths, root.as_ref().map(|r| r.path.as_path()), lang);
    if let Some(r) = resolver::start(proxy.clone(), names) {
        let _ = proxy.send(Box::new(resolver::Started(r)));
    }
    // The suite's Updated message reaches the screen's app_message (TS3).
    // SAFETY: a NUL-terminated string that lives for the call.
    let updated = unsafe { RegisterWindowMessageW(&HSTRING::from(oled_sys::selftest::UPDATED_MESSAGE)) };
    if !event_loop.route_message(w, updated) {
        oled_sys::log!("tasks: the Updated message is not routed");
    }
    // (14) Until the window closes; the screen's Drop stops the sampler and the resolver and flushes the state.
    let code = event_loop.run();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The number format of a language.
fn num_fmt(lang: UiLang) -> NumFmt {
    if lang.italian {
        NumFmt::IT
    } else {
        NumFmt::EN
    }
}

/// The Windows folder (GetSystemWindowsDirectoryW); empty when it cannot be read.
fn system_root() -> String {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is writable for its whole length, which the slice carries.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    buf.get(..n).map(String::from_utf16_lossy).unwrap_or_default()
}

/// One line on the console of the parent, when it has one (--version, bad arguments).
fn console_line(s: &str) {
    // SAFETY: no pointers; fails harmlessly when the parent has no console.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let _ = writeln!(std::io::stdout(), "{s}");
}

#[cfg(feature = "preview")]
fn preview(dir: &Path, scene: Option<&str>, lang: args::PreviewLang, dpi: Option<u32>) -> ExitCode {
    crate::ui::preview::run(dir, scene, lang, dpi)
}

#[cfg(not(feature = "preview"))]
fn preview(_: &Path, _: Option<&str>, _: args::PreviewLang, _: Option<u32>) -> ExitCode {
    console_line("tasks: built without preview");
    ExitCode::from(2)
}

#[cfg(feature = "preview")]
fn list_apps() -> ExitCode {
    list_apps::run()
}

#[cfg(not(feature = "preview"))]
fn list_apps() -> ExitCode {
    console_line("tasks: built without preview");
    ExitCode::from(2)
}
