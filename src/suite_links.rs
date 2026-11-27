//! The suite apps as the dock sees them (dock-suite 2.5): their paths, names, states, launch parameters and the
//! entries of the menus that list them. The state is re-read by `refresh` (UI thread) and cached; the launches go
//! through the shell thread (`ShellRequest::SuiteLaunch`).
//!
//! Declared as a child module of `ui` (`#[path]`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use oled_trust::frame::tasks::{show_args, Select, Show, MAX_IDENTITY_BYTES};
use oled_trust::manifest::suite::{ComponentId, Stage};
use windows::core::HSTRING;
use windows::Win32::Foundation::RECT;
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, INVALID_FILE_ATTRIBUTES};

use crate::shell_thread::{self, ShellRequest};
use crate::surface::{Font, Surface, TEXT3_ALPHA, WHITE};
use crate::ui::takeover::StageView;
use crate::update::suite::SuiteStatus;

/// A suite app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum App {
    Tasks,
    Files,
}

impl App {
    fn component(self) -> ComponentId {
        match self {
            App::Tasks => ComponentId::Tasks,
            App::Files => ComponentId::Files,
        }
    }
}

/// What the dock knows of one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppState {
    pub installed: bool,
    pub enabled: bool,
    pub stage: StageView,
    pub version: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState { installed: false, enabled: false, stage: StageView::Unknown, version: None }
    }
}

/// The app's file in `dir`. Pure.
pub(crate) fn exe_path_in(dir: &Path, app: App) -> PathBuf {
    dir.join(app.component().file())
}

/// The installed folder's copy of the app's exe (no existence check).
pub(crate) fn exe_path(app: App) -> Option<PathBuf> {
    let dock = crate::autostart::installed_exe().ok()?;
    Some(exe_path_in(dock.parent()?, app))
}

impl App {
    fn slot(self) -> usize {
        match self {
            App::Tasks => 0,
            App::Files => 1,
        }
    }
}

/// The states of Tasks and Folio (by `App::slot`), as `refresh` last read them.
static STATES: Mutex<[AppState; 2]> = Mutex::new([
    AppState { installed: false, enabled: false, stage: StageView::Unknown, version: None },
    AppState { installed: false, enabled: false, stage: StageView::Unknown, version: None },
]);

/// The stage a manifest entry gives; no entry reads as Unknown. Pure.
fn stage_view(stage: Option<Stage>) -> StageView {
    match stage {
        Some(Stage::Stable) => StageView::Stable,
        Some(Stage::Beta) => StageView::Beta,
        None => StageView::Unknown,
    }
}

/// Both states from the config and the suite status; `exists` says whether the app's file is there. Pure.
fn states_of(c: &crate::config::Config, st: &SuiteStatus, exists: impl Fn(App) -> bool) -> [AppState; 2] {
    [App::Tasks, App::Files].map(|app| {
        let entry = st.components.iter().find(|e| e.id == app.component());
        AppState {
            installed: exists(app),
            enabled: match app {
                App::Tasks => c.suite_tasks,
                App::Files => c.suite_files,
            },
            stage: stage_view(entry.and_then(|e| e.stage)),
            version: entry.and_then(|e| e.version.clone()),
        }
    })
}

/// Stores `new` in `slot`; true when it changed. Pure.
fn store(slot: &mut [AppState; 2], new: [AppState; 2]) -> bool {
    let changed = *slot != new;
    *slot = new;
    changed
}

/// A local stat: the path names an existing file (not a folder).
fn file_exists(p: &Path) -> bool {
    let w = HSTRING::from(p.as_os_str());
    // SAFETY: `w` is NUL-terminated and lives for the call.
    let a = unsafe { GetFileAttributesW(&w) };
    a != INVALID_FILE_ATTRIBUTES && a & FILE_ATTRIBUTE_DIRECTORY.0 == 0
}

/// The cached state.
pub(crate) fn state(app: App) -> AppState {
    STATES.lock().unwrap_or_else(|e| e.into_inner())[app.slot()].clone()
}

/// Installed and enabled.
pub(crate) fn usable(app: App) -> bool {
    let s = state(app);
    s.installed && s.enabled
}

/// UI thread: re-reads both states (two stats, the config, the status kept in memory). When the stored state changed
/// the takeover gate is published again, so it follows a component installed by a sync without a config change.
pub(crate) fn refresh() {
    let new = states_of(&crate::config::get(), &crate::update::suite::status(), |app| {
        exe_path(app).is_some_and(|p| file_exists(&p))
    });
    let changed = store(&mut STATES.lock().unwrap_or_else(|e| e.into_inner()), new);
    if changed {
        crate::ui::takeover::refresh_gate();
    }
}

/// The app's display name in the dock's language: the kit table's, never a literal.
pub(crate) fn name(app: App) -> &'static str {
    crate::i18n::kit(match app {
        App::Tasks => oled_ui::strings::k::KIT_APP_TASKS,
        App::Files => oled_ui::strings::k::KIT_APP_FILES,
    })
}

/// Not stable (Unknown counts as beta).
pub(crate) fn beta(app: App) -> bool {
    state(app).stage != StageView::Stable
}

/// The Tasks launch contract's select rule as the dock applies it: an identity key of 1..=512 UTF-8 bytes, no control
/// character, not starting with "--" and not made only of ASCII digits (the parser would read it as a process id).
/// Anything else is left out. Pure.
pub(crate) fn select_of(key: &str) -> Option<Select> {
    let bad = key.is_empty()
        || key.len() > MAX_IDENTITY_BYTES as usize
        || key.chars().any(char::is_control)
        || key.starts_with("--")
        || key.bytes().all(|b| b.is_ascii_digit());
    (!bad).then(|| Select::Identity(key.into()))
}

/// One parameter string that Windows' parser (and `std::env::args`) reads back as exactly `args`: arguments separated
/// by one space; an argument that is empty or holds a space or a tab is put in double quotes; a quote is written \";
/// a run of backslashes is doubled when it precedes a quote or the closing quote, kept as is otherwise. Pure.
pub(crate) fn quote_argv(args: &[String]) -> String {
    let mut line = String::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        let quoted = arg.is_empty() || arg.contains([' ', '\t']);
        if quoted {
            line.push('"');
        }
        let mut backslashes = 0usize;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
            } else {
                if c == '"' {
                    line.extend(std::iter::repeat_n('\\', backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quoted {
            line.extend(std::iter::repeat_n('\\', backslashes));
            line.push('"');
        }
    }
    line
}

/// The parameters of a Tasks launch: the contract's arguments, quoted; None when there is none. Pure.
pub(crate) fn tasks_params(s: &Show) -> Option<String> {
    let args = show_args(s);
    (!args.is_empty()).then(|| quote_argv(&args))
}

/// The shell request that starts `exe` with the parameters of `s`. Pure.
fn launch_request(exe: &Path, s: &Show) -> ShellRequest {
    ShellRequest::SuiteLaunch { target: exe.to_string_lossy().into_owned(), args: tasks_params(s) }
}

/// Starts Tasks through the shell thread. False when it is not usable.
pub(crate) fn open_tasks(s: Show) -> bool {
    let Some(exe) = exe_path(App::Tasks).filter(|_| usable(App::Tasks)) else { return false };
    shell_thread::request(launch_request(&exe, &s));
    true
}

/// Starts Folio with no argument. False when it is not usable.
pub(crate) fn open_files() -> bool {
    let Some(exe) = exe_path(App::Files).filter(|_| usable(App::Files)) else { return false };
    shell_thread::request(ShellRequest::SuiteLaunch { target: exe.to_string_lossy().into_owned(), args: None });
    true
}

/// Windows' own Task Manager, as today's menus open it.
pub(crate) fn open_windows_task_manager() {
    crate::popup::launch("taskmgr.exe");
}

/// A right-aligned tag ("Beta") at the end of a menu row, 12 px from the row's right edge, centred like the row text.
pub(crate) fn paint_tag(s: &mut Surface, rc: RECT, text: &str) {
    let w = s.text_width(text, Font::Small);
    let x = rc.right - s.metrics().px(12) - w;
    let y = crate::popup::center_y(&rc, s.line_height(Font::Small));
    s.text_w(text, w, x, y, Font::Small, WHITE, TEXT3_ALPHA);
}

/// The width the tag takes (the text only).
pub(crate) fn tag_width(s: &Surface, text: &str) -> i32 {
    s.text_width(text, Font::Small)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_trust::frame::tasks::{parse_show, Page, SortBy};
    use oled_ui::strings::UiLang;

    /// What Windows' parser makes of `x.exe <params>`, without the program name.
    fn parsed(params: &str) -> Vec<String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let line: Vec<u16> = format!("x.exe {params}").encode_utf16().chain(std::iter::once(0)).collect();
        let mut n = 0;
        // SAFETY: `line` is NUL-terminated; the array returned holds `n` strings, read once, then freed once.
        unsafe {
            let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut n);
            assert!(!argv.is_null());
            let all: Vec<String> = (1..n as usize).map(|i| (*argv.add(i)).to_string().unwrap_or_default()).collect();
            let _ = LocalFree(Some(HLOCAL(argv.cast())));
            all
        }
    }

    #[test]
    fn exe_path_is_in_the_install_folder() {
        let dir = Path::new(r"C:\Program Files\mini-oled-dock");
        assert_eq!(exe_path_in(dir, App::Tasks), dir.join(ComponentId::Tasks.file()));
        assert_eq!(exe_path_in(dir, App::Files), dir.join(ComponentId::Files.file()));
        assert_eq!(exe_path_in(dir, App::Tasks), dir.join("tasks.exe"));
        assert_eq!(exe_path_in(dir, App::Files), dir.join("files.exe"));
    }

    #[test]
    fn names_come_from_the_table() {
        for italian in [false, true] {
            let lang = if italian { UiLang::IT } else { UiLang::EN };
            crate::i18n::with_italian(italian, || {
                assert_eq!(name(App::Tasks), lang.kit(oled_ui::strings::k::KIT_APP_TASKS));
                assert_eq!(name(App::Files), lang.kit(oled_ui::strings::k::KIT_APP_FILES));
            });
        }
    }

    fn app_state(installed: bool, enabled: bool, stage: StageView, version: Option<&str>) -> AppState {
        AppState { installed, enabled, stage, version: version.map(String::from) }
    }

    #[test]
    fn launch_args_follow_show_args() {
        let back = |s: &Show| {
            let p = tasks_params(s);
            let args = parsed(p.as_deref().unwrap_or(""));
            assert_eq!(parse_show(&args).as_ref(), Ok(s));
            p
        };
        assert_eq!(back(&Show::default()), None);
        assert_eq!(back(&Show { sort: SortBy::Cpu, ..Show::default() }).as_deref(), Some("--sort cpu"));
        let sel = Show { select: select_of(r"c:\apps\contoso.exe").unwrap(), ..Show::default() };
        assert_eq!(back(&sel).as_deref(), Some(r"--select c:\apps\contoso.exe"));
        assert_eq!(back(&Show { from_chord: true, ..Show::default() }).as_deref(), Some("--from-chord"));
    }

    #[test]
    fn open_tasks_builds_a_suite_launch() {
        let exe = Path::new(r"C:\Program Files\mini-oled-dock\tasks.exe");
        let ShellRequest::SuiteLaunch { target, args } =
            launch_request(exe, &Show { sort: SortBy::Cpu, ..Show::default() })
        else {
            panic!("not a SuiteLaunch");
        };
        assert_eq!((target.as_str(), args.as_deref()), (exe.to_str().unwrap(), Some("--sort cpu")));
        let ShellRequest::SuiteLaunch { args, .. } = launch_request(exe, &Show::default()) else { panic!() };
        assert_eq!(args, None);
    }

    #[test]
    fn store_reports_change() {
        let base = [app_state(true, true, StageView::Beta, Some("1.0.0")), AppState::default()];
        let mut slot = base.clone();
        assert!(!store(&mut slot, base.clone()));
        let changed = |f: &dyn Fn(&mut AppState)| {
            let mut new = base.clone();
            f(&mut new[1]);
            let mut s = base.clone();
            store(&mut s, new)
        };
        assert!(changed(&|a| a.installed = true));
        assert!(changed(&|a| a.enabled = true));
        assert!(changed(&|a| a.stage = StageView::Stable));
        assert!(changed(&|a| a.version = Some("2".into())));
        let s = app_state(true, false, StageView::Unknown, None);
        let mut slot = [AppState::default(), AppState::default()];
        assert!(store(&mut slot, [s.clone(), AppState::default()]));
        assert_eq!(slot[0], s, "the new state is kept");
    }

    #[test]
    fn stage_from_status() {
        assert_eq!(stage_view(Some(Stage::Stable)), StageView::Stable);
        assert_eq!(stage_view(Some(Stage::Beta)), StageView::Beta);
        assert_eq!(stage_view(None), StageView::Unknown);
        let st = SuiteStatus {
            version: "1".into(),
            components: vec![crate::update::suite::ComponentStatus {
                id: ComponentId::Tasks,
                installed: true,
                version: Some("0.9.0".into()),
                stage: Some(Stage::Stable),
                enabled: true,
            }],
            last_sync: None,
            last_error: None,
        };
        let c = crate::config::Config::default();
        let s = states_of(&c, &st, |app| app == App::Tasks);
        assert_eq!(s[0].stage, StageView::Stable);
        assert_eq!(s[0].version.as_deref(), Some("0.9.0"));
        assert!(s[0].installed && !s[1].installed);
        assert_eq!(s[1].stage, StageView::Unknown, "an absent component");
        assert_eq!((s[0].enabled, s[1].enabled), (c.suite_tasks, c.suite_files));
    }

    #[test]
    fn usable_needs_both() {
        let both = |i, e| app_state(i, e, StageView::Stable, None);
        let usable_of = |s: &AppState| s.installed && s.enabled;
        assert!(usable_of(&both(true, true)));
        assert!(!usable_of(&both(true, false)) && !usable_of(&both(false, true)) && !usable_of(&both(false, false)));
        // The public readers follow the same rule through the cache.
        let mut slot = STATES.lock().unwrap_or_else(|e| e.into_inner());
        let old = slot.clone();
        *slot = [both(true, true), both(true, false)];
        drop(slot);
        assert!(usable(App::Tasks) && !usable(App::Files));
        *STATES.lock().unwrap_or_else(|e| e.into_inner()) = old;
    }

    #[test]
    fn beta_for_unknown() {
        let mut slot = STATES.lock().unwrap_or_else(|e| e.into_inner());
        let old = slot.clone();
        for (stage, want) in [(StageView::Unknown, true), (StageView::Beta, true), (StageView::Stable, false)] {
            slot[0].stage = stage;
            drop(slot);
            assert_eq!(beta(App::Tasks), want, "{stage:?}");
            slot = STATES.lock().unwrap_or_else(|e| e.into_inner());
        }
        *slot = old;
    }

    #[test]
    fn refresh_republishes_the_gate() {
        let src = include_str!("suite_links.rs").replace("\r\n", "\n");
        let body = &src[src.find("pub(crate) fn refresh()").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let (store_at, gate_at) = (body.find("store(").unwrap(), body.find("takeover::refresh_gate()").unwrap());
        assert!(store_at < gate_at);
        assert!(body[store_at..gate_at].contains("if changed {"), "only inside the branch where store returned true");
        assert_eq!(body.matches("refresh_gate").count(), 1);
    }

    #[test]
    fn select_rule() {
        let id = |k: &str| Some(Select::Identity(k.into()));
        let long = "a".repeat(512);
        assert_eq!(select_of(&long), id(&long));
        assert_eq!(select_of(&"a".repeat(513)), None);
        assert_eq!(select_of(""), None);
        assert_eq!(select_of("a\u{7}b"), None);
        assert_eq!(select_of("a\nb"), None);
        assert_eq!(select_of("--page"), None);
        assert_eq!(select_of("12345"), None);
        assert_eq!(select_of("Contoso.Chat_8wekyb3d8bbwe!App"), id("Contoso.Chat_8wekyb3d8bbwe!App"));
        assert_eq!(select_of(r"c:\apps\contoso.exe"), id(r"c:\apps\contoso.exe"));
    }

    #[test]
    fn quote_argv_round_trips() {
        let fixtures: [&[&str]; 7] = [
            &[
                "--start-icons",
                "32",
                "",
                "a b",
                "tab\there",
                "q\"uote",
                "\"",
                "end\\",
                "end space\\",
                "two\\\\\"q",
                r"\\srv\a b\",
            ],
            &["--install", "--from", r"D:\new kit\", "--quiet"],
            &["--undo-user", "--signal", "0123456789abcdef0123456789abcdef"],
            &["--select", r"c:\program files\contoso\"],
            &["--select", r#"a\"b"#],
            &["--sort", "cpu", "--from-chord"],
            &[],
        ];
        for args in fixtures {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            assert_eq!(parsed(&quote_argv(&args)), args, "{args:?}");
        }
    }

    #[test]
    fn tasks_params_follow_show_args() {
        assert_eq!(tasks_params(&Show::default()), None);
        assert_eq!(tasks_params(&Show { sort: SortBy::Cpu, ..Show::default() }).as_deref(), Some("--sort cpu"));
        let sel = Show { select: Select::Identity(r"c:\program files\contoso\contoso.exe".into()), ..Show::default() };
        assert_eq!(tasks_params(&sel).as_deref(), Some(r#"--select "c:\program files\contoso\contoso.exe""#));
        assert_eq!(tasks_params(&Show { from_chord: true, ..Show::default() }).as_deref(), Some("--from-chord"));
    }

    /// 1,000 seeded launches the dock could write: the parameters read back through Windows' parser and the tool's own
    /// parser give the same Show.
    #[test]
    fn tasks_params_round_trip_through_the_parsers() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet: Vec<char> = "ab Z09-_.!:\\/\"\t'&% é\u{4e2d}".chars().collect();
        for _ in 0..1000 {
            let len = (next() % 40) as usize;
            let key: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let show = Show {
                page: [Page::Processes, Page::Performance, Page::Startup, Page::Services][(next() % 4) as usize],
                select: select_of(&key).unwrap_or(Select::None),
                sort: [SortBy::Auto, SortBy::Cpu, SortBy::Memory][(next() % 3) as usize],
                from_chord: next() % 2 == 0,
            };
            let args = show_args(&show);
            match tasks_params(&show) {
                None => assert!(args.is_empty()),
                Some(p) => {
                    let back = parsed(&p);
                    assert_eq!(back, args, "{key:?}");
                    assert_eq!(parse_show(&back), Ok(show.clone()), "{key:?}");
                }
            }
        }
    }
}
