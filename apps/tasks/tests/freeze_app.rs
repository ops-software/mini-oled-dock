//! The TF0 freeze (P3-tasks-app F-T5): the skeleton used the way its owners will use it. A TasksScreen over
//! Deps::inert in a Harness, the command line, the hook table of screen.rs in its order, every route run once over the
//! stubs, the options, the strings and the manifest rules. No window, no process, no file written.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oled_config::{Kept, Problem};
use oled_trust::frame::tasks::{Page, Select, Show, SortBy};
use oled_ui::harness::Harness;
use oled_ui::model::RowKey;
use oled_ui::verbs::check_table;
use oled_ui::{Chord, CommandId, Key, Lang, Mods, PointL, SizeL, Tokens, UiLang};
use tasks::app::args::{self, Cli, PreviewLang};
use tasks::app::exec::{ActionDone, Outcome, RecordingExecutor};
use tasks::app::launch::RecordingLauncher;
use tasks::app::options::{parse_text, Density, OpenSort, TasksOptions, DEFAULT_REFRESH};
use tasks::app::presence::{DisplayPower, PresenceMsg};
use tasks::app::resolver::ResolverDone;
use tasks::app::sampling::Status;
use tasks::app::state::StateDirty;
use tasks::app::Deps;
use tasks::engine::cadence::Refresh;
use tasks::engine::frame::Frame;
use tasks::model::view::ViewMode;
use tasks::ui::commands;
use tasks::ui::screen::{FrameReady, InjectFrame, TasksScreen};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// A TasksScreen over Deps::inert with recording launcher and executor, 640 x 560 at 96 DPI.
fn harness() -> (Harness<TasksScreen>, Arc<RecordingLauncher>, Arc<RecordingExecutor>) {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let deps = Deps::inert(UiLang::EN, launcher.clone(), exec.clone());
    let h = Harness::new(TasksScreen::new(deps), SizeL { w: 640, h: 560 }, 96, Tokens::oled());
    (h, launcher, exec)
}

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn skeleton_renders_black_with_the_chrome() {
    let (mut h, launcher, exec) = harness();
    let img = h.render();
    assert_eq!((img.w, img.h), (640, 560));
    // T1a draws the toolbar and the gauge header (the first 104 px); with no frame yet, the list area stays black.
    let list_top = 44 + 60;
    assert!(
        img.px.iter().skip(img.w as usize * list_top).all(|&p| p == 0xFF00_0000),
        "an opaque black list area before the first frame"
    );
    let dump = h.a11y_dump();
    assert!(dump.lines().any(|l| l.starts_with("Toolbar")), "the window's toolbar node:\n{dump}");
    h.assert_named();
    assert!(h.is_idle());
    assert!(launcher.reqs.lock().unwrap().is_empty() && exec.batches.lock().unwrap().is_empty());
    let it = Harness::new(
        TasksScreen::new(Deps::inert(
            UiLang::IT,
            Arc::new(RecordingLauncher::default()),
            Arc::new(RecordingExecutor::default()),
        )),
        SizeL { w: 520, h: 400 },
        144,
        Tokens::oled(),
    )
    .lang(Lang::It);
    assert!(it.a11y_dump().contains("Toolbar"));
}

#[test]
fn args_parse_cases() {
    assert_eq!(args::parse(&[]), Ok(Cli::Run(Show::default())));
    let full = a(&["--page", "startup", "--select", "42", "--sort", "memory", "--from-chord"]);
    let show = Show { page: Page::Startup, select: Select::Pid(42), sort: SortBy::Memory, from_chord: true };
    assert_eq!(args::parse(&full), Ok(Cli::Run(show)));
    let nonce = "0123456789abcdef0123456789ABCDEF";
    assert_eq!(args::parse(&a(&["--selftest", "--signal", nonce])), Ok(Cli::Selftest { nonce: nonce.to_string() }));
    assert!(args::parse(&a(&["--selftest"])).is_err(), "--selftest without --signal");
    assert!(args::parse(&a(&["--selftest", "--signal"])).is_err());
    let preview = a(&["--preview", "out", "--scene", "x", "--lang", "it", "--dpi", "144"]);
    let want = Cli::Preview { dir: "out".into(), scene: Some("x".into()), lang: PreviewLang::It, dpi: Some(144) };
    assert_eq!(args::parse(&preview), Ok(want));
    let bare = Cli::Preview { dir: "out".into(), scene: None, lang: PreviewLang::En, dpi: None };
    assert_eq!(args::parse(&a(&["--preview", "out"])), Ok(bare));
    assert_eq!(args::parse(&a(&["--list-apps"])), Ok(Cli::ListApps));
    assert_eq!(args::parse(&a(&["--version"])), Ok(Cli::Version));
    assert!(args::parse(&a(&["--bogus"])).is_err(), "an unknown flag");
}

/// The calls of the F-T5 hook table and of run_pipeline, in the table's order.
const HOOKS: &[&str] = &[
    // layout
    "self.chrome.layout(",
    "self.notices.error_band(",
    "self.header.layout(",
    "self.table.layout(",
    "self.notices.line(",
    "cx.set_toast_baseline(",
    // paint
    "self.chrome.paint(",
    "self.notices.paint_error_band(",
    "self.header.paint(",
    "order::on_paint_anim(",
    "self.table.paint(",
    "search::paint_empty(",
    "self.notices.paint_line(",
    "order::paint_flash(",
    // the end of the first paint
    "self.first_paint.set(Some(sampling::ms_since_start()))",
    // the start of every call, once after the first paint
    "sampling::on_first_paint(",
    "show::on_first_paint(",
    // input
    "sampling::on_input(",
    // Command
    "commands::dispatch(",
    // Timer
    "sampling::on_timer(",
    "state::on_timer(",
    "order::on_timer(",
    "actions::on_timer(",
    // Esc
    "search::on_esc(",
    "selection::on_esc(",
    // other keys
    "keys::on_key(",
    "order::note_list_key(",
    // text
    "search::on_list_text(",
    // a pointer in the table's viewport
    "order::on_pointer(",
    // chrome
    "self.chrome.event(",
    "commands::dispatch(",
    "search::on_query(",
    // header
    "self.header.event(",
    "self.v.set_sort(",
    "grouped::header_menu(",
    // table
    "self.table.event(",
    "order::on_table_out(",
    "grouped::on_table_out(",
    "actions::on_table_out(",
    "selection::on_table_out(",
    // WindowActive, MenuClosed
    "self.ctl.active = ",
    "order::on_menu_closed(",
    // data
    "self.v.pull(",
    "self.v.push_frame(",
    "self.ctl.minimised = ",
    "self.ctl.cloaked = ",
    "self.ctl.display_off = ",
    "resolver::on_done(",
    "actions::on_done(",
    "state::on_dirty(",
    // the end of every call
    "self.ctl.paused = self.v.paused()",
    "self.ctl.mode = self.v.mode()",
    "self.ctl.machine_cpu_pct = self.v.machine_cpu_pct()",
    "grouped::windows_rows_visible(",
    "sampling::sync(",
    "self.chrome.set_status(",
    "sampling::status(",
    "self.header.set_held(",
    "order::is_held(",
    // run_pipeline
    "self.rows.clear()",
    "self.v.merge(",
    "list::build(",
    "grouped::build(",
    "tree::build(",
    "search::filter(",
    "order::apply(",
    "actions::decorate(",
    "self.v.publish(",
];

#[test]
fn hooks_in_order() {
    let src = include_str!("../src/ui/screen.rs");
    let mut at = 0;
    for needle in HOOKS {
        match src[at..].find(needle) {
            Some(i) => at += i + needle.len(),
            None => panic!("screen.rs: {needle:?} is missing or out of the hook table's order"),
        }
    }
    let routes: String =
        src.lines().filter(|l| !l.trim_start().starts_with("pub const")).collect::<Vec<_>>().join("\n");
    for t in ["TIMER_SNAPSHOT_B", "TIMER_STATE", "TIMER_HOLD", "TIMER_FRESH", "TIMER_CLOSE_CHECK", "TIMER_NOTES"] {
        assert_eq!(routes.matches(t).count(), 1, "{t} is routed exactly once");
    }
}

#[test]
fn every_route_runs() {
    let (mut h, launcher, exec) = harness();
    let _ = h.render();
    // Every input kind.
    h.hover(PointL { x: 300, y: 300 });
    h.click(PointL { x: 300, y: 300 });
    h.double_click(PointL { x: 300, y: 300 });
    h.wheel(PointL { x: 300, y: 300 }, 1.0);
    h.drag(PointL { x: 200, y: 200 }, PointL { x: 260, y: 240 });
    h.key("Down");
    h.ime("a", Some("a"));
    h.set_active(false);
    h.set_active(true);
    // A command of each owner module that has a chord: chrome, search, view, more_menu, keysheet, grouped, header,
    // actions.
    for chord in
        ["Ctrl+1", "F10", "Ctrl+F", "F5", "Ctrl+P", "Ctrl+=", "F1", "Ctrl+G", "Ctrl+Shift+1", "Del", "Shift+Del"]
    {
        h.key(chord);
    }
    // Every data message.
    h.send(Box::new(InjectFrame(Frame::default())));
    h.send(Box::new(InjectFrame(Frame::default())));
    h.send(Box::new(FrameReady));
    h.send(Box::new(PresenceMsg { minimised: true, cloaked: false }));
    h.send(Box::new(DisplayPower(false)));
    h.send(Box::new(ResolverDone(Vec::new())));
    h.send(Box::new(ActionDone { row: RowKey(1), outcome: Outcome::Failed(50), refused: 0 }));
    h.send(Box::new(StateDirty));
    // Esc, a letter in the list, a header click and a right click on the list.
    h.key("Esc");
    h.type_text("a");
    h.click(PointL { x: 100, y: 74 });
    h.right_click(PointL { x: 100, y: 300 });
    let _ = h.render();
    // Snapshot B and the state write have run; nothing is left armed.
    h.advance(2_100);
    let s = h.screen();
    assert_eq!(s.view().mode(), ViewMode::Grouped, "grouped::initial_mode: a first start opens Grouped");
    assert!(s.header().held(), "the pointer and the keys held the order a moment ago");
    assert_eq!(s.chrome().status(), Some(Status::Every { period_ms: 1000 }), "sampling::status's stub");
    let c = s.control_inputs();
    assert!(c.minimised && !c.cloaked && c.display_off && c.active && !c.windows_rows_visible);
    let control = s.deps().sampling.last_control().expect("sync set a control");
    assert_eq!(control.refresh, DEFAULT_REFRESH);
    assert_eq!(s.deps().sampling.sample_now_calls(), 1, "snapshot B, once");
    assert!(launcher.reqs.lock().unwrap().is_empty(), "no process started");
    assert!(exec.batches.lock().unwrap().is_empty(), "nothing acted on");
    // The hold ends by its own timer.
    h.advance(6_000);
    assert!(!h.screen().header().held(), "the hold ended");
    assert!(h.is_idle(), "only the named one-shots were armed");
}

#[test]
fn options_roundtrip_in_tf0() {
    let d = TasksOptions::default();
    let want = TasksOptions {
        refresh: Refresh::One,
        density: Density::Standard,
        always_on_top: false,
        open_sorted_by: OpenSort::Auto,
    };
    assert_eq!(d, want);
    assert_eq!(DEFAULT_REFRESH, Refresh::One);
    let (o, errors) = parse_text("// a hand edit\n{\n  \"version\": 1,\n  \"refresh\": \"9\"\n}\n");
    assert_eq!(o.refresh, Refresh::One, "kept the default");
    assert_eq!(errors.len(), 1);
    assert!(matches!(errors[0].problem, Problem::UnknownValue { .. }), "{:?}", errors[0]);
    assert_eq!((errors[0].path.as_str(), errors[0].line, errors[0].kept), ("refresh", 4, Kept::Default));
}

/// (key, text) of a strings file.
fn entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|r| r.split_once('"').map(|(k, rest)| (k.to_string(), rest.to_string())))
        .collect()
}

fn placeholders(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                out.insert(after[..close].to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// The number of keys of the beta (later groups only append, both files at once).
const KEYS: usize = 121;

#[test]
fn strings_complete() {
    let en = entries(&read(&root().join("strings/en.toml")));
    let it = entries(&read(&root().join("strings/it.toml")));
    assert_eq!(en.len(), KEYS, "the key count is pinned");
    assert_eq!(tasks::strings::TABLE.len(), KEYS);
    let en_keys: Vec<&String> = en.iter().map(|(k, _)| k).collect();
    let it_keys: Vec<&String> = it.iter().map(|(k, _)| k).collect();
    assert_eq!(en_keys, it_keys, "the same keys in the same order");
    for ((k, e), (_, i)) in en.iter().zip(&it) {
        assert!(k.starts_with("tasks."), "{k}");
        assert!(e.len() > 4 && i.len() > 4, "{k} is empty");
        assert_eq!(placeholders(e), placeholders(i), "{k}: the placeholders differ");
    }
    assert_eq!(tasks::strings::t(UiLang::EN, tasks::strings::k::TASKS_TAB_PROCESSES), "Processes");
    assert_eq!(tasks::strings::t(UiLang::IT, tasks::strings::k::TASKS_TAB_PROCESSES), "Processi");
    assert_eq!(tasks::strings::app_name(UiLang::EN), "Tasks");
    assert_eq!(tasks::strings::app_name(UiLang::IT), "Attivit\u{e0}");
}

/// The code part of a line: up to a // outside a string literal.
fn code_of(line: &str) -> String {
    let line = line.replace("'\"'", "' '");
    let mut out = String::new();
    let (mut in_str, mut escaped) = (false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if !in_str && c == '/' && chars.peek() == Some(&'/') {
            break;
        }
        if in_str && c == '\\' && !escaped {
            escaped = true;
        } else {
            if c == '"' && !escaped {
                in_str = !in_str;
            }
            escaped = false;
        }
        out.push(c);
    }
    out
}

/// `word` in `s` with no letter, digit or underscore on either side.
fn has_word(s: &str, word: &str) -> bool {
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    s.match_indices(word)
        .any(|(i, _)| !is_word(s[..i].chars().next_back()) && !is_word(s[i + word.len()..].chars().next()))
}

fn files_under(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(it) = std::fs::read_dir(dir) else { return };
    for e in it.flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

#[test]
fn no_display_name_literal() {
    assert!(has_word("x \"Tasks\" y", "Tasks") && !has_word("TasksScreen", "Tasks") && !has_word("MyTasks", "Tasks"));
    assert_eq!(code_of("let a = \"//x\"; // Tasks"), "let a = \"//x\"; ");
    let mut files = Vec::new();
    files_under(&root().join("src"), "rs", &mut files);
    files_under(&root().join("strings"), "toml", &mut files);
    assert!(files.len() > 40, "the sources are walked");
    let mut found = Vec::new();
    for f in files {
        let toml = f.extension().is_some_and(|x| x == "toml");
        for (n, line) in read(&f).lines().enumerate() {
            let code = if toml {
                if line.trim_start().starts_with('#') {
                    String::new()
                } else {
                    line.to_string()
                }
            } else {
                code_of(line)
            };
            if has_word(&code, "Tasks") || has_word(&code, "Attivit\u{e0}") {
                found.push(format!("{}:{}", f.display(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "the display name lives only in the kit's table (kit.app.tasks): {found:#?}");
}

#[test]
fn commands_table_is_valid() {
    for lang in [UiLang::EN, UiLang::IT] {
        let t = commands::table(lang).expect("every chord parses and CommandTable::add accepts every command");
        assert!(check_table(&t, lang).is_ok());
        let ids: Vec<CommandId> = t.iter().map(|c| c.id).collect();
        assert_eq!(ids, commands::ALL, "every command, in table order");
        assert!(t.iter().all(|c| !c.id.is_kit() && !c.label.trim().is_empty()));
    }
    // Every command's chord and alt chord against the plan's table (TF0); the menu-only commands have neither. A chord
    // string Chord::parse cannot read fails table() above, so a typo cannot drop a chord without a word.
    let t = commands::table(UiLang::EN).unwrap();
    let want: [(CommandId, Option<&str>, Option<&str>); 31] = [
        (commands::PAGE_PROCESSES, Some("Ctrl+1"), None),
        (commands::FIND, Some("Ctrl+F"), None),
        (commands::SAMPLE_NOW, Some("F5"), None),
        (commands::PAUSE, Some("Ctrl+P"), None), // alt: the Pause key, checked below
        (commands::DENSITY_STANDARD, Some("Ctrl++"), None),
        (commands::DENSITY_COMPACT, Some("Ctrl+-"), None),
        (commands::MORE, Some("F10"), Some("Alt+M")),
        (commands::KEYS, Some("F1"), None),
        (commands::VIEW_GROUPED, Some("Ctrl+G"), None),
        (commands::VIEW_TREE, Some("Ctrl+T"), None),
        (commands::VIEW_LIST, Some("Ctrl+L"), None),
        (commands::SORT_COL_1, Some("Ctrl+Shift+1"), None),
        (commands::SORT_COL_2, Some("Ctrl+Shift+2"), None),
        (commands::SORT_COL_3, Some("Ctrl+Shift+3"), None),
        (commands::SORT_COL_4, Some("Ctrl+Shift+4"), None),
        (commands::CLOSE_OR_END, Some("Del"), None),
        (commands::END_NOW, Some("Shift+Del"), None),
        (commands::END_TREE, None, None),
        (commands::REOPEN, None, None),
        (commands::OPEN_WINDOWS_TM, None, None),
        (commands::REFRESH_HALF, None, None),
        (commands::REFRESH_ONE, None, None),
        (commands::REFRESH_TWO, None, None),
        (commands::REFRESH_FIVE, None, None),
        (commands::REFRESH_PAUSED, None, None),
        (commands::ALWAYS_ON_TOP, None, None),
        (commands::OPEN_SORTED_AUTO, None, None),
        (commands::OPEN_SORTED_CPU, None, None),
        (commands::OPEN_SORTED_MEMORY, None, None),
        (commands::ABOUT, None, None),
        (commands::RESTART_TO_UPDATE, None, None),
    ];
    assert_eq!(want.map(|w| w.0), commands::ALL, "the expected chords name every command once, in table order");
    let label = |c: Option<Chord>| c.map(|c| c.label(UiLang::EN));
    for (id, chord, alt) in want {
        let c = t.get(id).unwrap_or_else(|| panic!("command {} is in the table", id.0));
        assert_eq!(label(c.chord).as_deref(), chord, "chord of command {}", id.0);
        if id == commands::PAUSE {
            assert_eq!(c.alt_chord, Some(Chord::new(Mods::NONE, Key(0x13))), "PAUSE's alt chord is the Pause key");
        } else {
            assert_eq!(label(c.alt_chord).as_deref(), alt, "alt chord of command {}", id.0);
        }
    }
    // dispatch names every command (the ones without a chord are reached from menus and notices).
    let src = read(&root().join("src/ui/commands.rs"));
    let dispatch = src.split("pub fn dispatch").nth(1).expect("dispatch");
    for line in src.lines().filter(|l| l.starts_with("pub const ") && l.contains(": CommandId = CommandId(")) {
        let name = line.trim_start_matches("pub const ").split(':').next().unwrap_or_default();
        assert!(has_word(dispatch, name), "dispatch does not route {name}");
    }
}

#[test]
fn shellmodel_is_reachable() {
    // TA-SM: the dependency resolves and the identity module is callable from tasks.
    let key = oled_shellmodel::identity::key_from_exe_path("C:\\Program Files\\Orbit\\orbit.exe");
    assert!(!key.is_empty());
}

#[test]
fn examples_are_tested() {
    let manifest = read(&root().join("Cargo.toml"));
    let declared: Vec<(String, bool)> = manifest
        .split("[[example]]")
        .skip(1)
        .map(|block| {
            let block = block.split("\n[").next().unwrap_or_default();
            let name = block.lines().find_map(|l| l.trim().strip_prefix("name = \"")).unwrap_or_default();
            let tested = block.lines().any(|l| l.trim() == "test = true");
            (name.trim_end_matches('"').to_string(), tested)
        })
        .collect();
    let mut found = 0;
    for e in std::fs::read_dir(root().join("examples")).expect("examples/").flatten() {
        let p = e.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        if p.extension().is_some_and(|x| x == "rs") || p.join("main.rs").is_file() {
            found += 1;
            assert!(
                declared.contains(&(stem.clone(), true)),
                "examples/{stem} has no [[example]] entry with test = true"
            );
        }
    }
    assert!(found >= 6, "the engine's three examples and this plan's three");
}
