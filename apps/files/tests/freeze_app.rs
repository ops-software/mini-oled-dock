//! The window's freeze (P3-files-app FZ1-FZ3): strings, frozen items, routing, the key table, the gate and the start
//! plan. Plain and Harness tests over fakes only: no window, no host, no real settings folder.
#![allow(clippy::type_complexity)] // the freeze spells every frozen signature out

use files_engine::tr::{gen, t};
use oled_sys::i18n::key;
use oled_ui::UiLang;

/// The keys whose English and Italian texts are the same on purpose (true cognates and pure placeholders). A key added
/// later with identical texts joins this list in the same commit.
const SAME_IN_BOTH: [&str; 13] = [
    "files.app.title_beta",
    "files.app.beta",
    "files.app.home",
    "files.type.file",
    "files.size.b",
    "files.size.kb",
    "files.size.mb",
    "files.size.gb",
    "files.size.tb",
    "files.ops.chip",
    "files.ops.eta_min",
    "files.ops.eta_s",
    "files.side.drive",
];

/// The key names of a strings file, in file order.
fn keys_of(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix('"'))
        .filter_map(|l| l.split_once('"').map(|(k, _)| k))
        .collect()
}

#[test]
fn strings_are_complete() {
    let en = keys_of(include_str!("../strings/en.toml"));
    let it = keys_of(include_str!("../strings/it.toml"));
    assert_eq!(gen::TABLE.len(), en.len(), "the table has every key of en.toml");
    assert_eq!(gen::TABLE.len(), it.len(), "the table has every key of it.toml");
    let mut same = Vec::new();
    for name in &en {
        assert!(name.starts_with("files."), "{name}: the window's keys start with files.");
        assert!(it.contains(name), "{name} is missing from it.toml");
        let (e, i) = (t(key(name), UiLang::EN), t(key(name), UiLang::IT));
        assert!(!e.is_empty() && !i.is_empty(), "{name} has both texts");
        if e == i {
            same.push(*name);
        }
    }
    same.sort_unstable();
    let mut allowed = SAME_IN_BOTH.to_vec();
    allowed.sort_unstable();
    assert_eq!(same, allowed, "identical EN and IT texts only for the listed cognates");
}

// ---- FZ2: the internal freeze --------------------------------------------------------------------------------------

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use common::temp::TempRoot;
use files_engine::app::{self, StartEnv, Step};
use files_engine::cli::Cli;
use files_engine::fakes::{self, Fakes, OpsCall};
use files_engine::hub::{Collect, Hub, Poster};
use files_engine::instance::{self, Elevation, InstanceNames};
use files_engine::keys::{self, ChordSrc, Cmd, Parity, Slice};
use files_engine::listing::{Batch, Gen, TabId, WidePath};
use files_engine::msg::{Fwd, JobDone, Msg};
use files_engine::ops_ui::{self, MutReq};
use files_engine::services::ReqOwner;
use files_engine::window::{self, FilesScreen, LoopOps, LoopReq, Shared, Show, WinKind, WinRec, STARTUP_TAB};
use files_engine::{
    about, bar, bg, chrome, complete, drives, format, gate, handoff, home, icons, linger, live, menu, model, names,
    nav, navcache, notices, open, opswin, options, parse, pathline, pins, preview, rename, selftest, services, session,
    sidebar, status, tab, toolbar, toolmenus, types, undo, view,
};
use oled_trust::frame::files::{Change, ChangeKind, InstanceCmd, Location, OpEvt, OpId, Pixels, ReqId, ShellEvt};
use oled_ui::harness::Harness;
use oled_ui::host::{AppMsg, Proxy, WindowRef, WindowSpec};
use oled_ui::model::{ColId, RowKey, Selection, SortDir};
use oled_ui::widgets::{PathOut, TabOut};
use oled_ui::{
    Anchor, Chord, CommandId, CommandTable, Event, EventCx, Key, LayoutCx, Mods, PaintCx, PointP, RectP, Reply, Screen,
    SizeL, SizeP, Tokens,
};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Threading::WaitForSingleObject;

type S = FilesScreen;

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
}

/// A made-up folder that never exists: fakes only.
fn folder() -> Location {
    Location::Fs(wp(r"C:\mod-files-made-up\projects"))
}

/// A short name for each message, for assertions.
fn kind(m: &Msg) -> String {
    match m {
        Msg::Batch { tab, .. } => format!("Batch({})", tab.0),
        Msg::EnginesWake => "EnginesWake".into(),
        Msg::Job { win, .. } => format!("Job({win})"),
        Msg::Fwd(Fwd::Shell { owner, .. }) => format!("Shell({owner:?})"),
        Msg::Fwd(Fwd::Watch(_)) => "Watch".into(),
        Msg::Fwd(Fwd::Ops) => "Ops".into(),
        Msg::Fwd(Fwd::Own { op, .. }) => format!("Own({})", op.0),
        Msg::Fwd(Fwd::Bump) => "Bump".into(),
        Msg::Instance(_) => "Instance".into(),
        Msg::Updated { .. } => "Updated".into(),
        Msg::Lang => "Lang".into(),
        Msg::FirstFrame => "FirstFrame".into(),
    }
}

fn kinds(c: &Collect) -> Vec<String> {
    c.take().iter().map(kind).collect()
}

fn screen(f: &Fakes, win: u32, tab: TabId) -> FilesScreen {
    FilesScreen::new(f.shared(oled_ui::UiLang::EN), win, folder(), None, tab)
}

#[test]
fn freeze_items_compile() {
    // 2.1: names, cli, the instance contract's types.
    let _: [&str; 6] = [
        names::WINDOW_CLASS,
        names::OPS_WINDOW_CLASS,
        names::IPC_CLASS,
        names::MUTEX,
        names::TEST_TAG_VAR,
        names::AUMID,
    ];
    let _: [i32; 5] =
        [names::EXIT_NOT_FORWARDED, names::EXIT_OK, names::EXIT_USAGE, names::EXIT_ELEVATED_HANDOFF, names::CASCADE];
    let _: (oled_ui::host::AppId, oled_ui::host::PlacementKey, bool) =
        (names::APP, names::MAIN_PLACEMENT, names::STAGE_BETA);
    let _: [&str; 4] = [names::OPTIONS_FILE, names::SESSION_FILE, names::TYPES_FILE, names::GUARD_FILE];
    let _: [SizeL; 2] = [names::DEFAULT_SIZE, names::MIN_SIZE];
    let _: fn(&[String]) -> Result<Cli, String> = files_engine::cli::parse;
    let _: fn(&Cli, &WidePath) -> Result<InstanceCmd, String> = files_engine::cli::to_instance;
    // tr, msg, hub, bg.
    let _: fn(oled_sys::i18n::S, oled_ui::UiLang) -> oled_ui::node::Text = files_engine::tr::t;
    let _: fn(oled_sys::i18n::S, oled_ui::UiLang, &[(&str, &str)]) -> oled_ui::node::Text = files_engine::tr::tf;
    let _: fn(&Proxy, Msg) -> bool = files_engine::msg::post_msg;
    let _ = files_engine::msg::KnownName::Downloads;
    let _: fn() -> Arc<Hub> = Hub::new;
    let _: fn(&Hub, u32, Box<dyn Poster>) = Hub::attach;
    let _: fn(&Hub, u32) = Hub::detach;
    let _: fn(&Hub, TabId, u32) = Hub::route;
    let _: fn(&Hub, TabId) = Hub::unroute;
    let _: fn(&Hub, u32) = Hub::set_active;
    let _: fn(&Hub) -> Option<u32> = Hub::primary;
    let _: fn(&Hub, Msg) = Hub::post;
    let _: fn(&Hub, u32, Msg) -> bool = Hub::post_to;
    let _: fn(&Hub) = Hub::wake;
    let _: fn(&Hub) = Hub::wake_done;
    let _: fn(&Arc<Hub>) -> Arc<dyn Fn() + Send + Sync> = Hub::notifier;
    let _: fn(&Arc<Hub>) -> files_engine::listing::BatchSink = Hub::sink;
    let _: fn(&Hub, std::time::Instant) -> Option<Batch> = Hub::wait_first_batch;
    let _: fn(Arc<Hub>) -> bg::Bg = bg::Bg::new;
    let _: fn(&bg::Bg, u32, bg::Lane, bg::JobFn) = bg::Bg::run;
    let _: fn(&bg::Bg) -> bg::BgStats = bg::Bg::stats;
    let _: fn(&WidePath, &drives::DriveCache) -> bg::Lane = bg::lane_of;
    // services and fakes.
    let _: fn(&Arc<Hub>, std::path::PathBuf, Option<files_engine::host::TestHooks>) -> services::Services =
        services::real;
    let _: fn(&Arc<Hub>, std::path::PathBuf, Option<files_engine::host::TestHooks>) -> Rc<dyn services::ShellService> =
        services::real_shell;
    let _: fn(&Fakes) -> services::Services = Fakes::services;
    let _: fn(&Fakes, oled_ui::UiLang) -> Rc<Shared> = Fakes::shared;
    let _: fn(&Fakes, u32) -> Arc<Collect> = Fakes::attach_window;
    let _: fn(&Fakes, &mut Harness<FilesScreen>, u32) -> usize = Fakes::pump::<FilesScreen>;
    let _: fn(&[(&str, bool, u64, i64)]) -> Batch = fakes::listing;
    let _: fn(u32, u64) -> Batch = fakes::listing_n;
    // format, model, columns.
    let _: fn(oled_ui::UiLang) -> format::Fmt = format::Fmt::system;
    let _: [fn() -> format::Fmt; 2] = [format::Fmt::fixed_en, format::Fmt::fixed_it];
    let _: fn(&format::Fmt, u64) -> String = format::Fmt::count;
    let _: fn(&format::Fmt, u64, bool) -> String = format::Fmt::size;
    let _: fn(&format::Fmt, i64, format::LocalTime, bool) -> String = format::Fmt::modified;
    let _: fn(&format::Fmt, i64) -> String = format::Fmt::full_date;
    let _: fn(i64) -> Option<format::LocalTime> = format::Fmt::to_local;
    let _: fn(
        Box<dyn model::Listing>,
        Rc<format::Fmt>,
        Rc<types::TypeNames>,
        model::ViewOpts,
        format::LocalTime,
    ) -> model::FilesTable = model::FilesTable::new;
    let _: fn(&mut model::FilesTable, model::IconKey, u16, Arc<[u8]>) -> bool = model::FilesTable::set_icon;
    let _: fn(&model::FilesTable, RowKey) -> Option<WidePath> = model::FilesTable::path_of;
    let _: fn(&model::FilesTable, &Selection) -> Vec<WidePath> = model::FilesTable::selected_paths;
    let _: fn(files_engine::listing::EntryId) -> RowKey = model::key_of;
    let _: fn(RowKey) -> files_engine::listing::EntryId = model::id_of;
    let _: fn(oled_ui::UiLang) -> Vec<oled_ui::model::Column> = view::columns::default_columns;
    let _: fn(model::ColKind) -> Option<files_engine::listing::SortKey> = view::columns::sort_key;
    // nav, tab, navcache.
    let _: fn(TabId, Location) -> nav::NavState = nav::NavState::new;
    let _: fn(&mut nav::NavState, Location, Option<Box<[u8]>>, u64) -> Vec<nav::NavEffect> = nav::NavState::go;
    let _: fn(&mut nav::NavState, &Batch, u64) -> (nav::BatchVerdict, Vec<nav::NavEffect>) = nav::NavState::on_batch;
    let _: fn(&mut nav::NavState, nav::NavTimer, u64) -> Vec<nav::NavEffect> = nav::NavState::on_timer;
    let _: fn(&nav::NavState, bool, usize) -> Vec<(usize, Location)> = nav::NavState::history;
    let _: fn(&Location) -> Option<Location> = nav::parent_of;
    let _: fn(&Shared, u32, Location) -> tab::Tab = tab::new_tab;
    let _: fn(&mut S, &mut EventCx, TabOut) -> Reply = tab::on_out;
    let _: fn(&mut S, &mut EventCx, Location) -> bool = tab::open_background;
    let _: fn(&mut S, &mut EventCx, Location, bool) -> bool = tab::open_here;
    let _: fn(&mut S, &mut EventCx, bool, Anchor) -> bool = tab::history_menu;
    let _: fn(&tab::Tabs, oled_ui::UiLang) -> Vec<oled_ui::widgets::TabSpec> = tab::Tabs::specs;
    let _: fn(&Shared, &Location, navcache::Cached) = navcache::put;
    let _: fn(&Shared, &Location) -> Option<navcache::Cached> = navcache::take;
    let _: fn(&Shared) = navcache::clear;
    // window.
    let _: fn(&Shared, LoopReq) = window::request_loop;
    let _: fn(&Rc<Shared>, &mut dyn LoopOps) = window::run_loop_requests;
    let _: fn(&Shared) -> WindowSpec = window::main_spec;
    let _: fn(&Shared, u32) -> Option<PointP> = window::cascade_pos;
    let _: fn(&Shared) = window::drain_engines;
    let _: fn(u8, TabId) -> oled_ui::TimerId = window::timer;
    let _: fn(&mut S, &mut EventCx, Location, Option<Box<[u8]>>) = window::navigate;
    let _: fn(&mut S, window::Shed) = window::shed;
    let _: fn(&mut S, &mut EventCx, TabId) = window::revalidate;
    let _: fn(&mut S, &mut EventCx, Cmd) -> Reply = window::cmd;
    let _: fn(Rc<Shared>, u32, Location, Option<WidePath>, TabId) -> FilesScreen = FilesScreen::new;
    let _ = (window::Region::List, window::Shed::Everything, WinKind::Ops);
    // keys and preview.
    let _: fn(oled_ui::UiLang) -> CommandTable = keys::command_table;
    let _: fn(CommandId) -> Option<Cmd> = keys::cmd_of;
    let _: fn(&mut S, &mut EventCx, Cmd) -> Reply = keys::run;
    let _: fn() -> Vec<preview::Scene> = preview::scenes;
    let _: fn(preview::Cells) -> Vec<preview::Cell> = preview::cells;
    let _: fn(&preview::Scene, &preview::Cell, &Fakes) -> Harness<FilesScreen> = preview::harness;
    let _: fn(&str, Option<&str>, Option<&str>, Option<u32>) -> i32 = preview::run;
    // app, status, the view, toolbar, chrome.
    let _: fn(&Cli, &StartEnv) -> Vec<Step> = app::start_plan;
    let _: fn(Cli) -> i32 = app::run;
    let _: fn(&mut S, &mut LayoutCx, RectP) = status::layout;
    let _: fn(&S, &mut PaintCx) = status::paint;
    let _: fn(&mut S, &mut EventCx, status::Notice) = status::notice;
    let _: fn(&mut S, &mut EventCx) = status::selection_changed;
    let _: fn(&mut S, &mut EventCx) = status::on_notice_timer;
    let _: fn(&mut S, &mut EventCx, status::NoticeAct) -> Reply = status::on_notice_action;
    let _: fn(&mut S, &mut EventCx, &Event) -> Option<Reply> = status::event;
    let _: fn(&mut S, &mut LayoutCx, RectP) = view::details::layout;
    let _: fn(&S, &mut PaintCx) = view::details::paint;
    let _: fn(&mut S, &mut EventCx, ColId, SortDir) -> Reply = view::details::sort;
    let _: fn(&mut S, &mut EventCx, TabId, Gen, files_engine::listing::SortDone) = view::details::on_sorted;
    let _: fn(&mut S, &mut EventCx) = view::details::columns_changed;
    let _: fn(&S, &nav::Page, RectP, &mut PaintCx) = view::states::paint_state;
    let _: fn(&mut S, &mut EventCx, &Event) -> Option<Reply> = view::states::state_event;
    let _: fn(&mut S, &mut LayoutCx, RectP) = toolbar::layout;
    let _: fn(&mut S, &mut EventCx, &Event) -> Option<Reply> = toolbar::event;
    let _: fn(oled_ui::host::Caption, i32, oled_ui::Scale) -> chrome::ChromeRects = chrome::rects;
    let _: fn(&mut S, &mut LayoutCx, RectP) -> RectP = chrome::layout;
    let _: fn(&S, PointP) -> oled_ui::CaptionHit = chrome::caption_hit;
    let _: fn(&S) -> bool = chrome::has_strip;
    // pathline, parse, complete, sidebar, home, drives, pins.
    let _: fn(&mut S, &mut EventCx, &Location) = pathline::set_location;
    let _: fn(&mut S, &mut EventCx, Option<oled_ui::node::Text>) = pathline::set_error;
    let _: fn(&mut S, &mut EventCx, PathOut) -> Reply = pathline::on_out;
    let _: fn(&mut S, &mut EventCx, char) -> bool = pathline::type_into;
    let _: fn(&mut S, &mut EventCx, u64, Vec<Box<str>>) -> bool = pathline::on_siblings;
    let _: fn(&mut S, &mut EventCx, u64, WidePath, Option<bool>) -> bool = pathline::on_stat;
    let _: fn(&str, &WidePath, &parse::Env) -> parse::Parsed = parse::parse;
    let _: fn(&mut S, &mut EventCx, &str) = complete::request;
    let _: fn(&mut S, &mut EventCx, u64, &WidePath, &[Box<str>], bool) -> bool = complete::on_children;
    let _: fn(&mut S, &mut LayoutCx, RectP) -> RectP = sidebar::layout;
    let _: fn(&mut S, &mut EventCx, u64, &WidePath, &[Box<str>], bool) -> bool = sidebar::on_children;
    let _: fn(&Shared, &Location) = sidebar::on_navigated;
    let _: fn(&mut S, &mut EventCx) = sidebar::on_bump;
    let _: fn(&mut S, &mut LayoutCx, RectP) = home::home_layout;
    let _: fn(&mut S, &mut EventCx, &Event) -> Option<Reply> = home::home_event;
    let _: fn() -> drives::DriveCache = drives::DriveCache::new;
    let _: fn(&Shared) = drives::scan;
    let _: bool = pins::PINS_IMPORT_OK;
    let _: fn(&Shared) = pins::import_if_due;
    let _: fn(&Shared, &files_engine::shell_client::ShellReply) -> bool = pins::on_reply;
    let _: [fn(&Shared, &WidePath) -> bool; 2] = [pins::add, pins::remove];
    let _: fn(&Shared) -> Vec<WidePath> = pins::paths;
    // open, icons, types, menu, toolmenus.
    let _: fn(&mut S, &mut EventCx, &[RowKey]) -> Reply = open::activate_items;
    let _: fn(&mut S, &mut EventCx) -> Reply = open::hand_off_current;
    let _: fn(&Shared) = open::prespawn;
    let _: fn(&Shared) -> Vec<(u32, Fwd)> = open::drain_shell;
    let _: fn(&mut S, &mut EventCx, &files_engine::shell_client::ShellReply) = open::on_exec_reply;
    let _: fn(&mut S, &mut EventCx) = icons::request_visible;
    let _: fn(&mut S, &mut EventCx, ReqOwner, &files_engine::shell_client::ShellReply) = icons::on_icon_reply;
    let _: fn(&mut S) = icons::drop_item_icons;
    let _: [fn(&Shared); 2] = [icons::on_first_frame, icons::on_assoc_changed];
    let _: fn(&[u8], oled_ui::UiLang) -> String = types::TypeNames::fallback;
    let _: fn(&dyn services::OwnFiles, u16) -> types::TypeNames = types::TypeNames::load;
    let _: fn(&types::TypeNames, &dyn services::OwnFiles, u64) = types::TypeNames::save_if_due;
    let _: fn(&mut S, &mut EventCx) = types::request_missing;
    let _: fn(&Shared, &files_engine::shell_client::ShellReply) -> bool = types::on_reply;
    let _: fn(&mut S, &mut EventCx, Option<RowKey>, PointP) -> Reply = menu::open_context;
    let _: fn(&mut S, &mut EventCx, PointP) -> Reply = menu::open_system;
    let _: fn(&mut S, &mut EventCx, CommandId) -> Reply = menu::on_pick;
    let _: [fn(&mut S, &mut EventCx, Anchor) -> Reply; 4] =
        [toolmenus::open_new, toolmenus::open_sort, toolmenus::open_view, toolmenus::open_more];
    // ops_ui, undo, bar, opswin, gate, rename, live.
    let _: fn(&mut S, &mut EventCx, MutReq) -> Option<OpId> = ops_ui::request;
    let _: fn(&mut S, &mut EventCx, OpId, &MutReq) = ops_ui::on_accepted;
    let _: fn(&oled_trust::frame::files::Refusal, oled_ui::UiLang) -> oled_ui::node::Text = ops_ui::refusal_text;
    let _: fn(&mut S, &mut EventCx, Vec<WidePath>) -> Reply = ops_ui::delete_flow;
    let _: fn(&S, &[WidePath]) -> Vec<oled_ui::MenuItem> = ops_ui::f3_items;
    let _: fn(&Shared) -> Vec<(u32, Fwd)> = ops_ui::drain;
    let _: fn(&mut S, &mut LayoutCx, RectP) -> RectP = ops_ui::chip_layout;
    let _: fn(&mut S, &mut EventCx, Option<OpId>) -> Reply = ops_ui::flyout_open;
    let _: fn(&ops_ui::OpsModel, OpId) -> Vec<WidePath> = ops_ui::OpsModel::items_of;
    let _: fn(&mut ops_ui::OpsModel, u32) -> Vec<status::Notice> = ops_ui::OpsModel::take_notices;
    let _: fn(&undo::UndoStack, oled_ui::UiLang) -> Option<oled_ui::node::Text> = undo::UndoStack::label;
    let _: fn(&bar::RecycleBar) -> bool = bar::RecycleBar::is_open;
    let _: fn(&mut S, &mut EventCx, &Event) -> Option<Reply> = bar::bar_event;
    let _: fn(&mut S, &mut EventCx) -> bool = opswin::on_last_close;
    let _: fn(Rc<Shared>, u32) -> opswin::OpsScreen = opswin::OpsScreen::new;
    let _: fn(&Shared) -> WindowSpec = opswin::ops_spec;
    let _: bool = gate::REAL_DATA_MUTATIONS;
    let _: fn(&[&WidePath]) -> bool = gate::may_mutate;
    let _: fn(&mut S, &mut EventCx, RowKey) -> Reply = rename::begin;
    let _: fn(&mut S, &mut EventCx, &WidePath) -> Reply = rename::begin_path;
    let _: fn(&mut S, &mut EventCx, RowKey, String) -> Reply = rename::commit;
    let _: fn(&mut S, &mut EventCx, OpId, &WidePath) -> bool = rename::on_created;
    let _: fn(&mut S, &mut EventCx) = live::watch_active;
    let _: fn(&mut S) = live::unwatch;
    let _: fn(&mut S, &mut EventCx) = live::on_activate;
    let _: fn(&Shared) -> Vec<(u32, Fwd)> = live::drain;
    let _: fn(&mut S, &mut EventCx, TabId, Gen, files_engine::listing::Diff) = live::on_resolved;
    let _: fn(&mut S, &mut EventCx, OpId, &[Change]) = live::own_changes;
    // instance, linger, handoff, notices, about, session, options, selftest.
    let _: fn() -> Elevation = instance::elevation;
    let _: fn(Elevation) -> bool = instance::must_hand_off;
    let _: fn(&Cli) -> i32 = instance::elevated_handoff;
    let _: fn() -> InstanceNames = InstanceNames::for_process;
    let _: fn(&InstanceNames, &InstanceCmd) -> Result<u32, instance::ForwardError> = instance::try_forward;
    let _: fn(&InstanceNames) -> instance::MutexState = instance::take_mutex;
    let _: fn(&InstanceNames, Arc<Hub>) -> Result<instance::Ipc, String> = instance::create_ipc;
    let _: fn(instance::MutexState, Option<instance::Ipc>) = instance::release;
    let _: fn() = instance::set_identity;
    let _: fn(&mut S, &mut EventCx, InstanceCmd) -> Reply = instance::apply;
    #[cfg(debug_assertions)]
    let _: fn(&[String]) -> i32 = instance::test_ipc_role;
    let _: fn(&mut S, &mut EventCx) -> bool = linger::on_last_close;
    let _: fn(&mut S, &mut EventCx) -> Reply = linger::on_linger_timer;
    let _: fn(&mut S, &mut EventCx, &InstanceCmd) -> bool = linger::on_instance;
    let _: fn(&mut S, &mut EventCx, handoff::Target, bool) -> Reply = handoff::hand_off;
    let _: fn(&mut S, &mut EventCx, u32) = notices::on_updated;
    let _: fn(&mut session::Session) -> Vec<Vec<Location>> = notices::take_reopen;
    let _: fn(&mut S, &mut EventCx, Cmd) -> Reply = about::cmd;
    let _: fn(&dyn services::OwnFiles) -> session::Session = session::Session::load;
    let _: fn(&session::Session, &dyn services::OwnFiles) -> bool = session::Session::save;
    let _: fn() -> Option<oled_config::FileSpec> = options::spec;
    let _: fn(&[u8]) -> Result<options::FilesOptions, String> = options::parse_file;
    let _: fn() -> Rc<dyn services::OptionsService> = options::real_service;
    let _: fn(&str) -> i32 = selftest::run;
    // Plain values of the frozen types.
    let o = options::FilesOptions::default();
    assert!(
        o.show_extensions && o.sidebar_visible && o.downloads_by_date && o.new_tab == "home" && o.sidebar_width == 200
    );
    assert_eq!(session::Session::default(), session::Session::default());
    assert_eq!(window::timer(names::TIMER_NOTICE, TabId(0x0123_4567)), oled_ui::TimerId(0x0923_4567));
}

#[test]
fn stubs_return_their_defaults() {
    let f = Fakes::new();
    let shared = f.shared(oled_ui::UiLang::EN);
    assert_eq!(instance::elevated_handoff(&Cli::Selftest { nonce: String::new() }), names::EXIT_ELEVATED_HANDOFF);
    // A tagged name: once FA-INST fills these, the test never meets a running instance or the real names (0.1).
    let names = InstanceNames::tagged(&common::test_tag());
    assert!(matches!(
        instance::try_forward(&names, &InstanceCmd::NewWindow(Location::Home)),
        Err(instance::ForwardError::NoInstance)
    ));
    assert!(matches!(instance::take_mutex(&names), instance::MutexState::Owned(_)));
    // elevation and create_ipc are FA-INST's real bodies now (tests/instance.rs).
    assert_eq!(options::parse_file(b""), Ok(options::FilesOptions::default()));
    assert_eq!(options::parse_file(b"{}"), Ok(options::FilesOptions::default()));
    assert!(navcache::take(&shared, &folder()).is_none());
    assert!(pins::paths(&shared).is_empty());
    assert!(window::cascade_pos(&shared, 1).is_none());
    assert_eq!(types::TypeNames::fallback(b"pdf", oled_ui::UiLang::EN), "PDF File");
    assert_eq!(types::TypeNames::fallback(b"pdf", oled_ui::UiLang::IT), "File PDF");
    assert_eq!(types::TypeNames::fallback(b"", oled_ui::UiLang::EN), "File");
    // parse is FD-PATH's real body now (tests/parse.rs): a bare word is a command, a relative path is resolved.
    assert!(matches!(parse::parse("x", &wp(r"C:\a"), &parse::Env::default()), parse::Parsed::Command(_)));
    assert_eq!(nav::parent_of(&Location::Fs(wp(r"C:\a\b"))), Some(Location::Fs(wp(r"C:\a"))));
    assert_eq!(nav::parent_of(&Location::Fs(wp(r"C:\"))), Some(Location::Home));
    assert_eq!(nav::parent_of(&Location::Fs(wp(r"\\server\share"))), None);
    assert_eq!(nav::parent_of(&Location::Home), None);
    assert!(notices::take_reopen(&mut session::Session::default()).is_empty());
    let spec = window::main_spec(&shared);
    assert_eq!(
        (spec.class, spec.size, spec.placement),
        (names::WINDOW_CLASS, names::DEFAULT_SIZE, Some(names::MAIN_PLACEMENT))
    );
    assert!(matches!(spec.kind, oled_ui::host::WindowKind::Opaque { caption: oled_ui::host::Caption::Custom, .. }));
    assert!(spec.title.ends_with(" Alpha"), "{}", spec.title);
    assert_eq!(opswin::ops_spec(&shared).class, names::OPS_WINDOW_CLASS);
    let mut cache = drives::DriveCache::new();
    cache.set(drives::DriveInfo {
        letter: b'z',
        label: None,
        kind: drives::DriveKind::Network,
        free: None,
        total: None,
        ready: true,
    });
    cache.set(drives::DriveInfo {
        letter: b'C',
        label: None,
        kind: drives::DriveKind::Fixed,
        free: None,
        total: None,
        ready: true,
    });
    assert_eq!(cache.all().iter().map(|d| d.letter).collect::<Vec<_>>(), [b'C', b'Z']);
    assert_eq!(
        (cache.lane(b'c'), cache.lane(b'Z'), cache.lane(b'Q')),
        (Some(bg::Lane::Local), Some(bg::Lane::Network), None)
    );
    assert_eq!(bg::lane_of(&wp(r"C:\a"), &cache), bg::Lane::Local);
    assert_eq!(bg::lane_of(&wp(r"\\server\share\a"), &cache), bg::Lane::Network);
    assert_eq!(bg::lane_of(&wp(r"Q:\a"), &cache), bg::Lane::Network, "an unknown letter counts as Network");
}

/// The rows of SPEC-files 5.4.1 within F1-F4, written out: (command, chords, parity, slice).
fn spec_rows() -> Vec<(Cmd, Vec<Chord>, Parity, Slice)> {
    use Parity::*;
    use Slice::*;
    let c = |s: &str| Chord::parse(s).unwrap_or_else(|| panic!("{s} parses"));
    let code = |m: Mods, k: Key| Chord::new(m, k);
    let mut rows = vec![
        (Cmd::Back, vec![code(Mods::ALT, Key::LEFT), c("Backspace")], Same, F1a),
        (Cmd::Forward, vec![code(Mods::ALT, Key::RIGHT)], Same, F1a),
        (Cmd::Up, vec![code(Mods::ALT, Key::UP)], Same, F1a),
        (Cmd::Home, vec![c("Alt+Home")], Same, F4),
        (Cmd::Refresh, vec![c("F5"), c("Ctrl+R")], Same, F1a),
        (Cmd::EditPath, vec![c("Ctrl+L"), c("Alt+D")], Same, F4),
        (Cmd::EditPathHistory, vec![c("F4")], Same, F4),
        (Cmd::NextRegion, vec![c("F6")], Same, F1a),
        (Cmd::PrevRegion, vec![c("Shift+F6")], Same, F1a),
        (Cmd::ExpandTree, vec![c("Ctrl+Shift+E")], Same, F4),
        (Cmd::FitColumns, vec![code(Mods::CTRL, Key::ADD)], Same, F1a),
        (Cmd::NewTab, vec![c("Ctrl+T")], Same, F1b),
        (Cmd::CloseTab, vec![c("Ctrl+W"), c("Ctrl+F4")], Verify, F1b),
        (Cmd::NextTab, vec![c("Ctrl+Tab"), c("Ctrl+PgDn")], Same, F1b),
        (Cmd::PrevTab, vec![c("Ctrl+Shift+Tab"), c("Ctrl+PgUp")], Same, F1b),
    ];
    let tabs = [Cmd::Tab1, Cmd::Tab2, Cmd::Tab3, Cmd::Tab4, Cmd::Tab5, Cmd::Tab6, Cmd::Tab7, Cmd::Tab8];
    for (i, t) in tabs.into_iter().enumerate() {
        rows.push((t, vec![c(&format!("Ctrl+{}", i + 1))], Verify, F4));
    }
    rows.extend([
        (Cmd::LastTab, vec![c("Ctrl+9")], Verify, F4),
        (Cmd::ReopenTab, vec![c("Ctrl+Shift+T")], Ours, F4),
        (Cmd::NewWindow, vec![c("Ctrl+N")], Same, F1a),
        (Cmd::Open, vec![], Same, F1a),
        (Cmd::OpenNewTab, vec![c("Ctrl+Enter")], Verify, F4),
        (Cmd::SelectAll, vec![c("Ctrl+A")], Same, F1a),
        (Cmd::SelectNone, vec![c("Ctrl+Shift+A")], Ours, F1a),
        (Cmd::Invert, vec![c("Ctrl+I")], Ours, F1a),
        (Cmd::Properties, vec![c("Alt+Enter")], Same, F2),
        (Cmd::Rename, vec![], Same, F3),
        (Cmd::NewFolder, vec![c("Ctrl+Shift+N")], Same, F3),
        (Cmd::CopyPath, vec![c("Ctrl+Shift+C")], Same, F2),
        (Cmd::Recycle, vec![c("Del"), c("Ctrl+D")], Same, F3),
        (Cmd::DeletePermanent, vec![c("Shift+Del")], Ours, F3),
        (Cmd::Undo, vec![c("Ctrl+Z")], Same, F3),
        (Cmd::Redo, vec![c("Ctrl+Y"), c("Ctrl+Shift+Z")], Same, F3),
        (Cmd::OurMenu, vec![code(Mods::NONE, Key::APPS)], Same, F2),
        (Cmd::SystemMenu, vec![c("Shift+F10")], Same, F2),
        (Cmd::HiddenItems, vec![c("Ctrl+H")], Ours, F2),
        (Cmd::Sidebar, vec![c("Ctrl+B")], Ours, F4),
        (Cmd::OpsFlyout, vec![c("Ctrl+J")], Ours, F3),
        (Cmd::OpenInExplorer, vec![c("Ctrl+Shift+O")], Ours, F2),
        (Cmd::Escape, vec![c("Esc")], Same, F1a),
        (Cmd::About, vec![], Ours, F4),
    ]);
    rows
}

#[test]
fn key_table_matches_spec() {
    let want = spec_rows();
    assert_eq!(keys::TABLE.len(), want.len(), "one row per command of F1-F4");
    for (row, (cmd, chords, parity, slice)) in keys::TABLE.iter().zip(&want) {
        assert_eq!(row.cmd, *cmd);
        for src in row.chords {
            assert!(src.chord().is_some(), "{cmd:?}: {src:?} is outside the kit's chord grammar");
            if let ChordSrc::Named(name) = src {
                assert!(Chord::parse(name).is_some(), "{cmd:?}: {name}");
            }
        }
        let got: Vec<Chord> = row.chords.iter().filter_map(ChordSrc::chord).collect();
        assert_eq!(&got, chords, "{cmd:?}: chords");
        assert_eq!((row.parity, row.slice), (*parity, *slice), "{cmd:?}: parity and slice");
        assert!(!files_engine::tr::t_any(row.label, oled_ui::UiLang::EN).is_empty(), "{cmd:?}: label");
        assert!(!files_engine::tr::t_any(row.label, oled_ui::UiLang::IT).is_empty(), "{cmd:?}: Italian label");
    }
    // Not bound in F1-F4: Windows' or the kit's default applies.
    let t = keys::command_table(oled_ui::UiLang::EN);
    for free in [
        "Ctrl+C",
        "Ctrl+X",
        "Ctrl+V",
        "Ctrl+F",
        "Ctrl+E",
        "Ctrl+P",
        "Ctrl+Shift+P",
        "F7",
        "F8",
        "F1",
        "Ctrl+,",
        "Alt+P",
        "F11",
    ] {
        assert!(t.by_chord(Chord::parse(free).unwrap()).is_none(), "{free} stays unbound");
    }
}

#[test]
fn command_table_accepts_every_row() {
    for lang in [oled_ui::UiLang::EN, oled_ui::UiLang::IT] {
        let t = keys::command_table(lang);
        let thirds = keys::TABLE.iter().filter(|r| r.chords.len() > 2).count();
        assert_eq!(t.iter().count(), keys::TABLE.len() + thirds, "every row (and its third chord) is in the table");
        let mut seen: Vec<Chord> = Vec::new();
        for c in t.iter() {
            assert!(!c.id.is_kit(), "{:?} is an app id", c.id);
            assert!(!c.label.is_empty(), "{:?} has a label", c.id);
            let cmd = keys::cmd_of(c.id).unwrap_or_else(|| panic!("{:?} maps back to its command", c.id));
            assert_eq!(c.id.0 % keys::THIRD_CHORD, cmd as u32);
            for ch in [c.chord, c.alt_chord].into_iter().flatten() {
                assert!(!seen.contains(&ch), "{} bound twice", ch.label(oled_ui::UiLang::EN));
                seen.push(ch);
            }
        }
    }
    let it = keys::command_table(oled_ui::UiLang::IT);
    let back = it.get(CommandId(Cmd::Back as u32)).map(|c| c.label.to_string());
    assert_eq!(back.as_deref(), Some("Indietro"));
    let tab3 = it.get(CommandId(Cmd::Tab3 as u32)).map(|c| c.label.to_string());
    assert_eq!(tab3.as_deref(), Some("Scheda 3"));
    assert_eq!(keys::cmd_of(CommandId(0)), None);
    assert_eq!(keys::cmd_of(CommandId(0x7FFF)), None);
}

#[test]
fn fakes_pump_delivers_batches() {
    let f = Fakes::new();
    let mut h = Harness::new(screen(&f, 1, STARTUP_TAB), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    let dir = wp(r"C:\mod-files-made-up\projects");
    f.enumr.queue(
        &dir,
        fakes::listing(&[("invoices", true, 0, fakes::FT_2026_11_21), ("notes.txt", false, 120, fakes::FT_2026_11_21)]),
    );
    let cancel = files_engine::listing::CancelToken::new();
    f.services().enumr.submit(files_engine::listing::EnumJob {
        tab: STARTUP_TAB,
        gen: Gen(3),
        dir: dir.clone(),
        cancel,
    });
    assert_eq!(f.enumr.jobs().len(), 1, "the job is recorded");
    assert_eq!(f.pump(&mut h, 1), 1, "one batch reached window 1's screen");
    assert_eq!(f.pump(&mut h, 1), 0, "nothing more");
    // A job run on the Bg lane posts its result to the window that ran it.
    f.services().bg.run(
        1,
        bg::Lane::Local,
        Box::new(|| Some(JobDone::FreeSpace { root: wp(r"C:\"), free: Some(1), total: Some(2) })),
    );
    assert_eq!(f.bg.queued(), 1);
    assert_eq!(f.pump(&mut h, 1), 1);
    // The batches of a listing reach a FakeListing in natural order, folders first.
    let mut l = f.services().enumr.new_listing(dir, Gen(1));
    l.append(fakes::listing(&[
        ("b.txt", false, 1, 0),
        ("a10.txt", false, 1, 0),
        ("a9.txt", false, 1, 0),
        ("zeta", true, 0, 0),
    ]));
    assert!(l.merge_pending() && l.done());
    let names: Vec<String> =
        l.order().iter().map(|id| String::from_utf8_lossy(l.name(*id).unwrap()).into_owned()).collect();
    assert_eq!(names, ["zeta", "a9.txt", "a10.txt", "b.txt"]);
    let batch = fakes::listing_n(1_000, 7);
    assert_eq!(batch.entries.len(), 1_000);
    let avg = batch.names.len() / 1_000;
    assert!((18..=30).contains(&avg), "average name {avg} bytes");
}

#[test]
fn screen_with_fakes_lays_out() {
    let f = Fakes::new();
    for loc in [folder(), Location::Home] {
        let s = FilesScreen::new(f.shared(oled_ui::UiLang::EN), 1, loc, None, STARTUP_TAB);
        let mut h = Harness::new(s, SizeL { w: 800, h: 500 }, 144, Tokens::oled());
        let img = h.render();
        assert_eq!((img.w, img.h), (1200, 750));
        h.key("Down").key("Ctrl+A").key("F5").key("Esc");
        h.wheel(oled_ui::PointL { x: 200, y: 200 }, -1.0).right_click(oled_ui::PointL { x: 200, y: 200 });
        h.set_active(true);
        let r = h.screen().rects();
        assert!(r.toolbar.h() > 0 && r.status.h() > 0 && r.body.h() > 0, "{r:?}");
        assert_eq!(r.client, RectP::wh(0, 0, 1200, 750));
    }
}

#[test]
fn tab_ids_are_process_unique() {
    let f = Fakes::new();
    let c2 = f.attach_window(2);
    let shared = f.shared(oled_ui::UiLang::EN);
    let s1 = FilesScreen::new(Rc::clone(&shared), shared.new_win(), folder(), None, STARTUP_TAB);
    let s2 = FilesScreen::new(Rc::clone(&shared), shared.new_win(), folder(), None, shared.new_tab());
    assert_eq!((s1.win(), s2.win()), (1, 2));
    let mut ids = vec![(s1.tabs().active().id, 1), (s2.tabs().active().id, 2)];
    for win in [1, 1, 2, 2] {
        ids.push((tab::new_tab(&shared, win, folder()).id, win));
    }
    let mut unique: Vec<u32> = ids.iter().map(|(t, _)| t.0).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 6, "{ids:?}");
    for (tab, win) in &ids {
        f.hub.post(Msg::Batch { tab: *tab, batch: fakes::listing(&[]) });
        let (mine, other) = if *win == 1 { (&f.posted, &c2) } else { (&c2, &f.posted) };
        assert_eq!(kinds(mine), [format!("Batch({})", tab.0)], "tab {} lands in window {win}", tab.0);
        assert!(other.is_empty());
    }
}

#[test]
fn batches_reach_only_their_window() {
    let f = Fakes::new();
    let c2 = f.attach_window(2);
    let shared = f.shared(oled_ui::UiLang::EN);
    let a = tab::new_tab(&shared, 1, folder()).id;
    let b = tab::new_tab(&shared, 2, folder()).id;
    f.hub.post(Msg::Batch { tab: b, batch: fakes::listing(&[]) });
    assert_eq!(kinds(&c2), [format!("Batch({})", b.0)]);
    assert!(f.posted.is_empty());
    f.hub.post(Msg::Batch { tab: TabId(999), batch: fakes::listing(&[]) });
    assert!(f.posted.is_empty() && c2.is_empty(), "an unrouted tab's batch is dropped");
    f.hub.unroute(a);
    f.hub.post(Msg::Batch { tab: a, batch: fakes::listing(&[]) });
    assert!(f.posted.is_empty(), "a closed tab's batch is dropped");
    // Jobs go to the window that ran them (0: the primary); wakes and instance commands as 2.2 says.
    f.hub.post(Msg::Job { win: 2, done: JobDone::Drives(Vec::new()) });
    f.hub.post(Msg::Job { win: 0, done: JobDone::Drives(Vec::new()) });
    f.hub.wake();
    f.hub.wake();
    assert_eq!(kinds(&c2), ["Job(2)"]);
    assert_eq!(kinds(&f.posted), ["Job(0)", "EnginesWake"], "one wake in flight");
    f.hub.set_active(2);
    f.hub.post(Msg::Instance(InstanceCmd::NewWindow(Location::Home)));
    f.hub.post(Msg::Lang);
    assert_eq!(kinds(&c2), ["Instance", "Lang"]);
    assert_eq!(kinds(&f.posted), ["Lang"]);
    // A window not attached yet gets its messages when it attaches; a detached one never.
    let t3 = tab::new_tab(&shared, 3, folder()).id;
    f.hub.post(Msg::Batch { tab: t3, batch: fakes::listing(&[]) });
    let c3 = f.attach_window(3);
    assert_eq!(kinds(&c3), [format!("Batch({})", t3.0)]);
    f.hub.detach(3);
    f.hub.post(Msg::Job { win: 3, done: JobDone::Drives(Vec::new()) });
    assert!(!f.hub.post_to(3, Msg::FirstFrame));
    assert!(c3.is_empty());
}

#[test]
fn a_lost_wake_goes_to_the_next_primary() {
    let hub = Hub::new();
    let (c1, c2) = (Arc::new(Collect::new()), Arc::new(Collect::new()));
    hub.attach(1, Box::new(Arc::clone(&c1)));
    hub.attach(2, Box::new(Arc::clone(&c2)));
    hub.wake();
    assert_eq!(kinds(&c1), ["EnginesWake"]);
    // The primary closes with the wake still in its queue: window 2 gets one.
    hub.detach(1);
    assert_eq!(kinds(&c2), ["EnginesWake"], "the closed primary's wake goes to the next primary");
    hub.wake();
    assert!(c2.is_empty(), "still one wake in flight");
    hub.wake_done();
    hub.wake();
    assert_eq!(kinds(&c2), ["EnginesWake"]);
    hub.wake_done();
    // Not the primary, or no wake in flight: nothing is sent.
    let c3 = Arc::new(Collect::new());
    hub.attach(3, Box::new(Arc::clone(&c3)));
    hub.wake();
    assert_eq!(kinds(&c2), ["EnginesWake"]);
    hub.detach(3);
    assert!(c2.is_empty() && c3.is_empty(), "a closed window that is not the primary takes no wake with it");
    hub.wake_done();
    hub.detach(2);
    assert!(c2.is_empty());
    // The last window closes with a wake in flight: the wake waits for the next window.
    let c4 = Arc::new(Collect::new());
    hub.attach(4, Box::new(Arc::clone(&c4)));
    hub.wake();
    hub.detach(4);
    let c5 = Arc::new(Collect::new());
    hub.attach(5, Box::new(Arc::clone(&c5)));
    assert_eq!(kinds(&c4), ["EnginesWake"]);
    assert_eq!(kinds(&c5), ["EnginesWake"], "the queued wake reaches the next window");
}

/// A window's poster that stops taking messages when its loop ends (Proxy::send returning false).
struct Closing {
    open: std::sync::atomic::AtomicBool,
    got: Collect,
}

impl Poster for Closing {
    fn post(&self, m: AppMsg) -> bool {
        self.open.load(std::sync::atomic::Ordering::Acquire) && self.got.post(m)
    }
}

#[test]
fn a_failed_wake_post_goes_to_the_next_primary() {
    let hub = Hub::new();
    let w1 = Arc::new(Closing { open: false.into(), got: Collect::new() });
    let c2 = Arc::new(Collect::new());
    hub.attach(1, Box::new(Arc::clone(&w1)));
    hub.attach(2, Box::new(Arc::clone(&c2)));
    hub.wake();
    assert_eq!(hub.primary(), Some(2), "the primary whose post failed is detached");
    assert_eq!(kinds(&c2), ["EnginesWake"], "and the wake goes to the next primary");
    // The wake reached the primary, then its loop ended: the next failed post to it moves the wake on.
    let hub = Hub::new();
    let w1 = Arc::new(Closing { open: true.into(), got: Collect::new() });
    let c2 = Arc::new(Collect::new());
    hub.attach(1, Box::new(Arc::clone(&w1)));
    hub.attach(2, Box::new(Arc::clone(&c2)));
    hub.wake();
    assert_eq!(kinds(&w1.got), ["EnginesWake"]);
    w1.open.store(false, std::sync::atomic::Ordering::Release);
    assert!(!hub.post_to(1, Msg::Fwd(Fwd::Bump)));
    assert_eq!(kinds(&c2), ["EnginesWake"]);
    // A queued wake whose window fails on attach waits for the next one.
    let hub = Hub::new();
    hub.wake();
    hub.attach(1, Box::new(Closing { open: false.into(), got: Collect::new() }));
    let c2 = Arc::new(Collect::new());
    hub.attach(2, Box::new(Arc::clone(&c2)));
    assert_eq!(kinds(&c2), ["EnginesWake"]);
}

#[test]
fn start_up_batch_waits_in_its_slot() {
    let hub = Hub::new();
    hub.route(STARTUP_TAB, 1);
    let sink = hub.sink();
    sink(STARTUP_TAB, fakes::listing(&[("a.txt", false, 1, 0)]));
    sink(STARTUP_TAB, fakes::listing(&[("b.txt", false, 1, 0)]));
    let first = hub.wait_first_batch(std::time::Instant::now());
    assert_eq!(first.map(|b| b.entries.len()), Some(1), "the first batch is taken from the slot");
    let c = Arc::new(Collect::new());
    hub.attach(1, Box::new(Arc::clone(&c)));
    assert_eq!(kinds(&c), ["Batch(1)"], "the second one waited for the window");
    let none = Hub::new().wait_first_batch(std::time::Instant::now() + std::time::Duration::from_millis(5));
    assert!(none.is_none());
}

#[test]
fn engine_output_routed_by_owner() {
    let f = Fakes::new();
    let c2 = f.attach_window(2);
    let shared = f.shared(oled_ui::UiLang::EN);
    for win in [1, 2] {
        shared.windows.borrow_mut().push(WinRec { win, kind: WinKind::Main, wref: None, hwnd: None, location: None });
    }
    let tab = tab::new_tab(&shared, 2, folder()).id;
    {
        let mut owners = shared.req_owner.borrow_mut();
        owners.insert(ReqId(1), ReqOwner::Icons { win: 2, tab, gen: Gen(1) });
        owners.insert(ReqId(2), ReqOwner::Exec { win: 1 });
        owners.insert(ReqId(3), ReqOwner::TypeNames);
    }
    shared.watch_owner.borrow_mut().insert(files_engine::watch::WatchId(7), 2);
    let px = Pixels { w: 1, h: 1, bgra: vec![0; 4].into() };
    f.shell.reply(files_engine::shell_client::ShellReply::Evt(ShellEvt::Icon {
        req: ReqId(1),
        gen: Gen(1),
        index: 0,
        px,
    }));
    f.shell.reply(files_engine::shell_client::ShellReply::Evt(ShellEvt::ExecDone { req: ReqId(2), hr: 0 }));
    f.shell.reply(files_engine::shell_client::ShellReply::Evt(ShellEvt::ReqDone { req: ReqId(3) }));
    f.watch.emit(files_engine::watch::WatchEvent::Changes {
        id: files_engine::watch::WatchId(7),
        gen: Gen(1),
        items: Vec::new(),
    });
    let change = Change { kind: ChangeKind::Created, from: None, to: Some(wp(r"C:\mod-files-made-up\projects\new")) };
    f.ops.emit(files_engine::ops_client::OpsEvent::Evt(OpEvt::ItemsDone { op: OpId(5), changes: vec![change] }));
    f.deliver();
    assert_eq!(kinds(&f.posted), ["EnginesWake"], "the engines wake the primary window only");
    assert!(c2.is_empty());
    window::drain_engines(&shared);
    let one = kinds(&f.posted);
    let two = kinds(&c2);
    assert_eq!(one, ["Own(5)", "Shell(Exec { win: 1 })", "Bump"]);
    let icons = format!("Shell(Icons {{ win: 2, tab: TabId({}), gen: Gen(1) }})", tab.0);
    assert_eq!(two, ["Own(5)".to_string(), icons, "Bump".to_string(), "Watch".to_string()]);
    assert!(shared.req_owner.borrow().get(&ReqId(2)).is_none(), "a finished request forgets its owner");
    // A second wake goes out once the first was drained.
    f.ops.emit(files_engine::ops_client::OpsEvent::HostLost);
    f.deliver();
    assert_eq!(kinds(&f.posted), ["EnginesWake"]);
    // Drives and association changes are process-wide.
    f.watch.emit(files_engine::watch::WatchEvent::AssocChanged);
    f.deliver();
    window::drain_engines(&shared);
    assert_eq!(f.icons.assoc_changed_calls(), 1);
    assert!(kinds(&f.posted).contains(&"Bump".to_string()) && kinds(&c2) == ["Bump"]);
}

/// A LoopOps that records every call; it has no proxy (no window exists before the kit's loop).
#[derive(Default)]
struct RecordOps {
    opened: Vec<(&'static str, WindowRef)>,
    shown: Vec<(WindowRef, Show)>,
    moved: Vec<(WindowRef, PointP)>,
}

impl LoopOps for RecordOps {
    fn open(&mut self, spec: WindowSpec, screen: Box<dyn Screen>) -> Option<WindowRef> {
        drop(screen);
        let w = WindowRef(self.opened.len() as u32 + 10);
        self.opened.push((spec.class, w));
        Some(w)
    }
    fn proxy(&self, w: WindowRef) -> Option<Proxy> {
        let _ = w;
        None
    }
    fn hwnd(&self, w: WindowRef) -> Option<isize> {
        Some(w.0 as isize * 100)
    }
    fn show(&mut self, w: WindowRef, how: Show) -> bool {
        self.shown.push((w, how));
        true
    }
    fn move_to(&mut self, w: WindowRef, at: PointP) -> bool {
        self.moved.push((w, at));
        true
    }
}

#[test]
fn loop_requests_run_through_loop_ops() {
    let f = Fakes::new();
    let shared = f.shared(oled_ui::UiLang::EN);
    let first = shared.new_win();
    window::request_loop(&shared, LoopReq::Main { first: folder(), select: None, cascade_from: Some(first) });
    // SAFETY: the event is open; a zero wait only reads its state.
    assert_eq!(unsafe { WaitForSingleObject(shared.loop_event.handle(), 0) }, WAIT_OBJECT_0, "the loop is woken");
    let mut ops = RecordOps::default();
    window::run_loop_requests(&shared, &mut ops);
    assert_eq!(ops.opened, [(names::WINDOW_CLASS, WindowRef(10))], "one window, with main_spec");
    let rec = shared.win(2).expect("the new window's record");
    assert_eq!((rec.kind, rec.wref, rec.hwnd), (WinKind::Main, Some(WindowRef(10)), Some(1000)));
    assert_eq!(rec.location, Some(folder()));
    assert_eq!(ops.shown, [(WindowRef(10), Show::Front)], "shown in front after it opened");
    assert!(ops.moved.is_empty(), "cascade_pos is FA-WIN's (None until then)");
    assert!(shared.loop_requests.borrow().is_empty());
    // The new window's tab is routed to it: its batch waits until window 2 attaches.
    let tab = TabId(shared.next_tab.get() - 1);
    f.hub.post(Msg::Batch { tab, batch: fakes::listing(&[]) });
    let c2 = f.attach_window(2);
    assert_eq!(kinds(&c2), [format!("Batch({})", tab.0)]);
    window::request_loop(&shared, LoopReq::Show { win: 2, how: Show::Hide });
    window::request_loop(&shared, LoopReq::Show { win: 99, how: Show::Front });
    window::request_loop(&shared, LoopReq::Ops);
    window::run_loop_requests(&shared, &mut ops);
    assert_eq!(ops.shown[1], (WindowRef(10), Show::Hide), "Show reaches ops.show; a gone window is ignored");
    assert_eq!(ops.opened[1].0, names::OPS_WINDOW_CLASS);
    assert_eq!(shared.win(3).map(|r| r.kind), Some(WinKind::Ops));
    assert_eq!(ops.shown.len(), 3);
}

#[test]
fn first_frame_posted_once() {
    let f = Fakes::new();
    let mut h = Harness::new(screen(&f, 1, STARTUP_TAB), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    h.render();
    let msgs = f.posted.take();
    assert_eq!(msgs.iter().filter(|m| matches!(m, Msg::FirstFrame)).count(), 1);
    assert_eq!(f.icons.rebuild_calls(), 0, "nothing runs from paint");
    h.send(Box::new(Msg::FirstFrame));
    assert!(!h.is_idle(), "the prespawn timer is armed");
    assert_eq!(f.icons.rebuild_calls(), 1, "the class icons' rebuild once per process");
    h.send(Box::new(Msg::FirstFrame));
    assert_eq!(f.icons.rebuild_calls(), 1);
}

#[test]
fn start_plan_table() {
    use Step::*;
    let window = |target: Option<&str>, forward_only: bool| Cli::Window {
        target: target.map(str::to_string),
        tab: false,
        select: None,
        from_verb: false,
        forward_only,
    };
    let env = |elevation| StartEnv { elevation };
    let tail = [StartJobs, Config, Reopen, Identity, Loop, Ipc, WaitFirstBatch, OpenWindow, Follow, Run];
    for e in [Elevation::Default, Elevation::Limited] {
        let fs = app::start_plan(&window(Some(r"C:\x"), false), &env(e));
        let want: Vec<Step> = [Log, Forward, Mutex, Services, Enumerate].into_iter().chain(tail).collect();
        assert_eq!(fs, want, "{e:?}");
        let home = app::start_plan(&window(None, false), &env(e));
        let want: Vec<Step> = [Log, Forward, Mutex, Services].into_iter().chain(tail).collect();
        assert_eq!(home, want, "{e:?}");
        assert_eq!(app::start_plan(&window(None, true), &env(e)), [Log, Forward, ExitNotForwarded]);
    }
    assert_eq!(app::start_plan(&window(Some(r"C:\x"), false), &env(Elevation::Full)), [Log, HandOff]);
    assert_eq!(app::start_plan(&window(None, true), &env(Elevation::Full)), [Log, HandOff]);
    let preview = Cli::Preview { dir: r"C:\x".into(), scene: None, lang: None, dpi: None };
    let selftest = Cli::Selftest { nonce: "0".repeat(32) };
    for e in [Elevation::Default, Elevation::Full, Elevation::Limited] {
        assert_eq!(app::start_plan(&preview, &env(e)), [Preview]);
        assert_eq!(app::start_plan(&selftest, &env(e)), [Selftest]);
    }
    assert!(instance::must_hand_off(Elevation::Full));
    assert!(!instance::must_hand_off(Elevation::Default) && !instance::must_hand_off(Elevation::Limited));
}

/// The gate's pure core over a made-up machine: TEMP is C:\Users\tester\AppData\Local\Temp, also reachable as the 8.3
/// form C:\Users\TESTER~1\AppData\Local\Temp; attributes come from a table (absent: the path does not exist).
fn gate_check(paths: &[&str], temp: &str, attrs: &[(&str, u32)]) -> bool {
    const LONG: &str = r"C:\Users\tester\AppData\Local\Temp";
    const SHORT: &str = r"C:\Users\TESTER~1\AppData\Local\Temp";
    let attrs: Vec<(String, u32)> = attrs.iter().map(|(p, a)| (p.to_ascii_lowercase(), *a)).collect();
    let attr =
        move |p: &WidePath| attrs.iter().find(|(q, _)| *q == p.to_string_lossy().to_ascii_lowercase()).map(|(_, a)| *a);
    let long = |p: &WidePath| {
        let s = p.to_string_lossy();
        let s = match s.get(..SHORT.len()) {
            Some(head) if head.eq_ignore_ascii_case(SHORT) => format!("{LONG}{}", &s[SHORT.len()..]),
            _ => s,
        };
        WidePath::parse(&s).ok()
    };
    let owned: Vec<WidePath> = paths.iter().map(|p| wp(p)).collect();
    let refs: Vec<&WidePath> = owned.iter().collect();
    gate::may_mutate_with(&refs, &wp(temp), &attr, &long)
}

#[test]
fn gate_table() {
    const T: &str = r"C:\Users\tester\AppData\Local\Temp";
    const SHORT: &str = r"C:\Users\TESTER~1\AppData\Local\Temp";
    let dir = 0x10;
    let junction = 0x10 | 0x400;
    let live = format!(r"{T}\mod-files-live-ab12");
    let test = format!(r"{T}\mod-files-test-99-1");
    let base = [(live.as_str(), dir), (test.as_str(), dir)];
    let rows: Vec<(String, Vec<String>, Vec<(String, u32)>, bool)> = vec![
        ("live root file".into(), vec![format!(r"{live}\report-final-v2.pdf")], vec![], true),
        ("test root, new name".into(), vec![format!(r"{test}\sub\new folder")], vec![], true),
        ("the root itself".into(), vec![live.clone()], vec![], true),
        ("outside".into(), vec![r"C:\Users\tester\Documents\notes.txt".into()], vec![], false),
        ("TEMP itself".into(), vec![format!(r"{T}\notes.txt")], vec![], false),
        ("a prefix with no name".into(), vec![format!(r"{T}\mod-files-live-\a")], vec![], false),
        ("another prefix".into(), vec![format!(r"{T}\mod-files-livex\a")], vec![], false),
        ("a root below a subfolder".into(), vec![format!(r"{T}\other\mod-files-live-ab12\a")], vec![], false),
        (
            "a junction inside the root".into(),
            vec![format!(r"{live}\link\a.txt")],
            vec![(format!(r"{live}\link"), junction)],
            false,
        ),
        (
            "the item is a junction".into(),
            vec![format!(r"{live}\link")],
            vec![(format!(r"{live}\link"), junction)],
            false,
        ),
        (
            "a junction named like a root".into(),
            vec![format!(r"{T}\mod-files-live-x\a.txt")],
            vec![(format!(r"{T}\mod-files-live-x"), junction)],
            false,
        ),
        ("case differs".into(), vec![format!(r"{}\MOD-FILES-LIVE-AB12\X.TXT", T.to_uppercase())], vec![], true),
        ("\\\\?\\ form".into(), vec![format!(r"\\?\{live}\a.txt")], vec![], true),
        ("item in 8.3 form".into(), vec![format!(r"{SHORT}\mod-files-live-ab12\a.txt")], vec![], true),
        ("one of two outside".into(), vec![format!(r"{live}\a.txt"), r"D:\x.txt".into()], vec![], false),
        ("empty list".into(), vec![], vec![], false),
    ];
    for temp in [T, SHORT, &format!(r"\\?\{T}")] {
        for (what, paths, extra, ok) in &rows {
            let mut attrs: Vec<(&str, u32)> = base.to_vec();
            attrs.extend(extra.iter().map(|(p, a)| (p.as_str(), *a)));
            let p: Vec<&str> = paths.iter().map(String::as_str).collect();
            assert_eq!(gate_check(&p, temp, &attrs), *ok, "{what} (TEMP {temp})");
        }
    }
    assert!(WidePath::parse(&format!(r"{live}\..\..\x")).is_err(), "a path with .. never reaches the gate");
    let real = std::hint::black_box(gate::REAL_DATA_MUTATIONS);
    assert!(!real, "real data stays off until FS-3 rows a to c are recorded");
}

/// FilesScreen inside a screen that runs ops_ui::request for every MutReq it receives (request needs an EventCx).
struct Requester {
    s: FilesScreen,
    out: Rc<RefCell<Vec<Option<OpId>>>>,
}

impl Screen for Requester {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.s.layout(cx, avail)
    }
    fn paint(&self, cx: &mut PaintCx) {
        self.s.paint(cx)
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.s.event(cx, ev)
    }
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        match msg.downcast::<MutReq>() {
            Ok(r) => {
                let id = ops_ui::request(&mut self.s, cx, *r);
                self.out.borrow_mut().push(id);
                Reply::None
            }
            Err(m) => self.s.data(cx, m),
        }
    }
    fn commands(&self) -> &CommandTable {
        self.s.commands()
    }
}

#[test]
fn request_is_gated() {
    let f = Fakes::new();
    let out = Rc::new(RefCell::new(Vec::new()));
    let r = Requester { s: screen(&f, 1, STARTUP_TAB), out: Rc::clone(&out) };
    let mut h = Harness::new(r, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    let outside = wp(r"C:\mod-files-made-up\projects\report-final-v2.pdf");
    h.send(Box::new(MutReq::Recycle(vec![outside.clone()])));
    h.send(Box::new(MutReq::Rename { item: outside, to: "x.pdf".into() }));
    h.send(Box::new(MutReq::Recycle(Vec::new())));
    h.send(Box::new(MutReq::ResumeRest(OpId(9))));
    assert_eq!(*out.borrow(), [None, None, None, None]);
    assert!(f.ops.calls().is_empty(), "nothing reached the operations helper");
    let root = TempRoot::new("request_is_gated");
    let file = wp(&root.file("report-final-v3.pdf", b"x").to_string_lossy());
    let dir = wp(&root.path().to_string_lossy());
    h.send(Box::new(MutReq::Recycle(vec![file.clone()])));
    h.send(Box::new(MutReq::NewFolder { parent: dir.clone(), name: "projects".into() }));
    h.send(Box::new(MutReq::Rename { item: file.clone(), to: "report-final-v4.pdf".into() }));
    assert_eq!(out.borrow()[4..], [Some(OpId(1)), Some(OpId(2)), Some(OpId(3))]);
    // A destination that is not one valid name is refused before the gate and the helper, even inside a root.
    for to in ["..", r"a\b", "a/b", ""] {
        h.send(Box::new(MutReq::Rename { item: file.clone(), to: to.into() }));
    }
    h.send(Box::new(MutReq::NewFolder { parent: dir.clone(), name: r"a\b".into() }));
    h.send(Box::new(MutReq::Rename { item: wp(r"C:\"), to: "x".into() }));
    assert_eq!(out.borrow()[7..], [None, None, None, None, None, None]);
    assert_eq!(f.ops.calls().len(), 3, "no further call reached the operations helper");
    assert_eq!(f.ops.calls()[0], OpsCall::Recycle(vec![oled_trust::frame::files::ItemRef::Path(file)]));
    assert_eq!(f.ops.calls()[1], OpsCall::NewFolder(dir, "projects".into()));
    f.ops.set_fail(Some(files_engine::ops_client::OpsError::Unavailable("gone")));
    h.send(Box::new(MutReq::Recycle(vec![wp(&root.path().join("report-final-v3.pdf").to_string_lossy())])));
    assert_eq!(out.borrow().last(), Some(&None), "an unavailable helper is a notice, not an op");
}

#[test]
fn harness_writes_reach_only_fakes() {
    use oled_ui::host::PlacementStore;
    let f = Fakes::new();
    let shared = f.shared(oled_ui::UiLang::EN);
    let mut h = Harness::new(
        FilesScreen::new(Rc::clone(&shared), 1, folder(), None, STARTUP_TAB),
        SizeL { w: 800, h: 500 },
        96,
        Tokens::oled(),
    );
    h.render();
    let mut store = session::SessionStore::new(Rc::clone(&shared));
    let p = oled_ui::host::Placement {
        monitor: "made-up-monitor".into(),
        rect: RectP::wh(10, 10, 800, 500),
        dpi: 96,
        maximized: false,
    };
    store.save(names::MAIN_PLACEMENT, &p);
    assert!(shared.services.options.modify(&mut |o| o.hidden_items = true));
    shared.types.save_if_due(&*shared.services.own, 60_000);
    assert!(shared.session.borrow().save(&*shared.services.own), "session.json goes through Services.own");
    assert_eq!(f.options.modifications().len(), 1, "the option change went to the fake");
    assert!(f.options.modifications()[0].hidden_items);
    for w in f.own.writes() {
        assert!([names::SESSION_FILE, names::TYPES_FILE, names::GUARD_FILE].contains(&w.as_str()), "{w}");
    }
}

// ---- FZ3: command line and fixed names -------------------------------------------------------------------------

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn win(target: Option<&str>, tab: bool, select: Option<&str>, from_verb: bool) -> Cli {
    Cli::Window {
        target: target.map(str::to_string),
        tab,
        select: select.map(str::to_string),
        from_verb,
        forward_only: false,
    }
}

#[test]
fn cli_grammar_table() {
    let nonce = "0123456789abcdef0123456789abcdef";
    let ok: Vec<(Vec<&str>, Cli)> = vec![
        (vec![], win(None, false, None, false)),
        (vec![r"C:\mod-files-made-up\projects"], win(Some(r"C:\mod-files-made-up\projects"), false, None, false)),
        (vec!["projects"], win(Some("projects"), false, None, false)),
        (vec!["--tab", r"C:\a"], win(Some(r"C:\a"), true, None, false)),
        (vec!["--select", r"C:\a\b.txt"], win(None, false, Some(r"C:\a\b.txt"), false)),
        (vec![r"C:\a", "--from-verb"], win(Some(r"C:\a"), false, None, true)),
        (vec!["--from-verb", "--tab", r"C:\a"], win(Some(r"C:\a"), true, None, true)),
        (vec!["--preview", r"C:\out"], Cli::Preview { dir: r"C:\out".into(), scene: None, lang: None, dpi: None }),
        (
            vec!["--preview", r"C:\out", "--scene", "details_basic", "--lang", "it", "--dpi", "144"],
            Cli::Preview {
                dir: r"C:\out".into(),
                scene: Some("details_basic".into()),
                lang: Some("it".into()),
                dpi: Some(144),
            },
        ),
        (vec!["--selftest", "--signal", nonce], Cli::Selftest { nonce: nonce.into() }),
        (vec!["--signal", nonce, "--selftest"], Cli::Selftest { nonce: nonce.into() }),
    ];
    for (a, want) in ok {
        assert_eq!(files_engine::cli::parse(&argv(&a)), Ok(want), "{a:?}");
    }
    let bad: Vec<Vec<&str>> = vec![
        vec!["--frobnicate"],
        vec!["--preview"],
        vec!["--preview", r"C:\out", r"C:\a"],
        vec!["--preview", r"C:\out", "--lang", "fr"],
        vec!["--preview", r"C:\out", "--dpi", "100"],
        vec!["--preview", r"C:\out", "--dpi", "x"],
        vec!["--scene", "details_basic"],
        vec!["--selftest"],
        vec!["--selftest", "--signal", "0123456789ABCDEF0123456789ABCDEF"],
        vec!["--selftest", "--signal", "0123"],
        vec!["--selftest", "--signal", nonce, r"C:\a"],
        vec!["--preview", r"C:\out", "--selftest", "--signal", nonce],
        vec![r"C:\a", r"C:\b"],
        vec![r"C:\a", "--select", r"C:\b.txt"],
        vec!["--tab"],
        vec!["--tab", r"C:\a", "--tab", r"C:\b"],
        vec!["--select"],
        vec!["--from-verb", "--from-verb"],
    ];
    for a in bad {
        let e = files_engine::cli::parse(&argv(&a)).expect_err("refused");
        assert!(e.starts_with("files: ") && e.contains(files_engine::cli::USAGE) && !e.contains('\n'), "{a:?}: {e}");
    }
}

#[test]
fn to_instance_table() {
    let cwd = wp(r"C:\mod-files-made-up\home");
    let stat = |p: &WidePath| -> Option<bool> {
        match p.to_string_lossy().as_str() {
            r"C:\mod-files-made-up\home\projects" | r"C:\mod-files-made-up\docs" | r"C:\" => Some(true),
            r"C:\mod-files-made-up\home\notes.txt" | r"C:\mod-files-made-up\docs\report-final-v2.pdf" => Some(false),
            _ => None,
        }
    };
    let fs = |s: &str| Location::Fs(wp(s));
    let rows: Vec<(Cli, InstanceCmd)> = vec![
        (win(None, false, None, false), InstanceCmd::NewWindow(Location::Home)),
        (win(Some("projects"), false, None, false), InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\home\projects"))),
        (
            win(Some(r".\projects\"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\home\projects")),
        ),
        (win(Some(r"..\docs"), false, None, false), InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs"))),
        (
            win(Some(r"\mod-files-made-up\docs"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs")),
        ),
        (win(Some(r"..\..\..\.."), false, None, false), InstanceCmd::NewWindow(fs(r"C:\"))),
        (
            win(Some("C:/mod-files-made-up/docs"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs")),
        ),
        (
            win(Some(r"\\?\C:\mod-files-made-up\docs"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs")),
        ),
        (
            win(Some("notes.txt"), false, None, false),
            InstanceCmd::Select {
                dir: wp(r"C:\mod-files-made-up\home"),
                item: wp(r"C:\mod-files-made-up\home\notes.txt"),
            },
        ),
        (
            win(None, false, Some(r"C:\mod-files-made-up\docs\report-final-v2.pdf"), false),
            InstanceCmd::Select {
                dir: wp(r"C:\mod-files-made-up\docs"),
                item: wp(r"C:\mod-files-made-up\docs\report-final-v2.pdf"),
            },
        ),
        (win(Some("missing"), false, None, false), InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\home\missing"))),
        (
            win(Some(r"C:\mod-files-made-up\docs"), true, None, false),
            InstanceCmd::OpenTab(fs(r"C:\mod-files-made-up\docs")),
        ),
        (win(Some(r"\\server\share\x"), false, None, true), InstanceCmd::NewWindow(fs(r"\\server\share\x"))),
        // An absolute target with "." or ".." walks from its own root, never from cwd.
        (
            win(Some(r"C:\mod-files-made-up\x\..\docs"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs")),
        ),
        (
            win(Some("C:/mod-files-made-up/./docs/"), false, None, false),
            InstanceCmd::NewWindow(fs(r"C:\mod-files-made-up\docs")),
        ),
        (win(Some(r"\\server\share\a\..\b"), false, None, false), InstanceCmd::NewWindow(fs(r"\\server\share\b"))),
        (win(Some(r"\\server\share\..\..\b"), false, None, false), InstanceCmd::NewWindow(fs(r"\\server\share\b"))),
    ];
    for (c, want) in rows {
        assert_eq!(files_engine::cli::to_instance_with(&c, &cwd, &stat), Ok(want), "{c:?}");
    }
    for refused in [
        Some("C:x"),
        Some("a\u{1}b"),
        Some(r"\\server"),
        Some(r"\\server\..\x"),
        Some(r"\\?\C:\a\..\b"),
        Some(r"\\.\C:\a\..\b"),
        Some(r"1:\a\..\b"),
    ] {
        assert!(
            files_engine::cli::to_instance_with(&win(refused, false, None, false), &cwd, &stat).is_err(),
            "{refused:?}"
        );
    }
    let preview = Cli::Preview { dir: r"C:\out".into(), scene: None, lang: None, dpi: None };
    assert!(files_engine::cli::to_instance(&preview, &cwd).is_err(), "only a window start forwards");
    // The real stat, inside a TempRoot: a folder, a file, nothing.
    let root = TempRoot::new("to_instance_table");
    root.dir("projects");
    root.file("projects/report-final-v3.pdf", b"x");
    let base = wp(&root.path().to_string_lossy());
    let folder = base.join(&"projects".encode_utf16().collect::<Vec<u16>>()).unwrap();
    let file = folder.join(&"report-final-v3.pdf".encode_utf16().collect::<Vec<u16>>()).unwrap();
    let real = |t: &str, tab: bool| files_engine::cli::to_instance(&win(Some(t), tab, None, false), &base);
    assert_eq!(real("projects", false), Ok(InstanceCmd::NewWindow(Location::Fs(folder.clone()))));
    assert_eq!(real("projects", true), Ok(InstanceCmd::OpenTab(Location::Fs(folder.clone()))));
    assert_eq!(
        real(r"projects\report-final-v3.pdf", false),
        Ok(InstanceCmd::Select { dir: folder.clone(), item: file })
    );
    let gone = folder.join(&"gone".encode_utf16().collect::<Vec<u16>>()).unwrap();
    assert_eq!(real(r"projects\gone", false), Ok(InstanceCmd::NewWindow(Location::Fs(gone))));
}

#[test]
fn names_are_pinned() {
    assert_eq!(names::WINDOW_CLASS, "MiniOledFiles");
    assert_eq!(names::OPS_WINDOW_CLASS, "MiniOledFilesProgress");
    assert_eq!(names::IPC_CLASS, "MiniOledFilesIpc");
    assert_eq!(names::MUTEX, r"Local\mini-oled-dock.files");
    assert_eq!(names::TEST_TAG_VAR, "FILES_TEST_TAG");
    assert_eq!(names::EXIT_NOT_FORWARDED, 7);
    assert_eq!(names::AUMID, "MiniOledDock.Files");
    assert_eq!(names::APP, oled_ui::host::AppId("files"));
    assert_eq!(names::MAIN_PLACEMENT, oled_ui::host::PlacementKey("files.main"));
    assert!(std::hint::black_box(names::STAGE_BETA));
    assert_eq!(
        [names::OPTIONS_FILE, names::SESSION_FILE, names::TYPES_FILE, names::GUARD_FILE],
        ["files.json", "session.json", "types.json", "files-guard.json"]
    );
    assert_eq!(names::DEFAULT_SIZE, SizeL { w: 1100, h: 720 });
    assert_eq!(names::MIN_SIZE, SizeL { w: 560, h: 360 });
    assert_eq!(names::CASCADE, 24);
    assert_eq!([names::EXIT_OK, names::EXIT_USAGE, names::EXIT_ELEVATED_HANDOFF], [0, 2, 5]);
    // 3, 4 and 6 stay the engine's; 5 is the same "elevated" exit in both.
    assert_eq!(
        [files_engine::exit::PROTO_MISMATCH, files_engine::exit::PIPE_TAKEN, files_engine::exit::HELLO_FAILED],
        [3, 4, 6]
    );
    assert_eq!(names::EXIT_ELEVATED_HANDOFF, files_engine::exit::ELEVATED);
    assert_eq!(
        [
            names::TIMER_NAV_FIRST,
            names::TIMER_NAV_LAST,
            names::TIMER_CHIP,
            names::TIMER_LINGER,
            names::TIMER_PRESPAWN,
            names::TIMER_NOTICE
        ],
        [1, 5, 6, 7, 8, 9]
    );
    // The operations window is not one of the engine hosts' hidden windows.
    for host in ["MiniOledFilesOps", "MiniOledFilesShell"] {
        assert_ne!(names::OPS_WINDOW_CLASS, host);
        assert_ne!(names::WINDOW_CLASS, host);
        assert_ne!(names::IPC_CLASS, host);
    }
    // The instance names: the constants, or tagged ones for tests.
    let tagged = InstanceNames::tagged("t3st9");
    assert_eq!(
        (tagged.mutex.as_str(), tagged.ipc_class.as_str()),
        (r"Local\mini-oled-dock.files-t3st9", "MiniOledFilesIpc-t3st9")
    );
    if std::env::var_os(names::TEST_TAG_VAR).is_none() {
        let own = InstanceNames::for_process();
        assert_eq!((own.mutex.as_str(), own.ipc_class.as_str()), (names::MUTEX, names::IPC_CLASS));
    }
}

#[test]
fn forward_only_is_debug_only() {
    let a = argv(&["--forward-only", r"C:\mod-files-made-up\projects"]);
    let parsed = files_engine::cli::parse(&a);
    #[cfg(debug_assertions)]
    assert_eq!(
        parsed,
        Ok(Cli::Window {
            target: Some(r"C:\mod-files-made-up\projects".into()),
            tab: false,
            select: None,
            from_verb: false,
            forward_only: true,
        })
    );
    #[cfg(not(debug_assertions))]
    assert!(parsed.is_err(), "a release build refuses --forward-only (exit 2)");
}
