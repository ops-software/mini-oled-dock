//! The window's process life (P3-files-app F4.12-F4.15): the ten-minute linger after the last window, the hand-offs
//! to File Explorer with their persisted loop guard, the notice after an update with its Reopen, and About. Plain
//! tests over the fakes through the kit's Harness: no window, no host, no process started, no file written.

use std::rc::Rc;

use files_engine::about;
use files_engine::fakes::{self, Fakes, OpsCall, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::handoff::{self, Guard, Plan, Target, GUARD_MAX, GUARD_MS};
use files_engine::keys::Cmd;
use files_engine::linger::LINGER_MS;
use files_engine::listing::WidePath;
use files_engine::msg::Msg;
use files_engine::names;
use files_engine::notices;
use files_engine::ops_ui::{OpRow, OpState};
use files_engine::services::ReqOwner;
use files_engine::session::Session;
use files_engine::shell_client::{FallbackWhy, ShellReply};
use files_engine::status::NoticeAct;
use files_engine::tab;
use files_engine::window::{self, FilesScreen, LoopOps, LoopReq, Shared, Show, WinKind, WinRec};
use oled_trust::frame::files::{InstanceCmd, ItemRef, Location, OpId, OpKind, ReqId, ShellReq, Verb};
use oled_ui::harness::Harness;
use oled_ui::host::{AppMsg, Proxy, WindowRef, WindowSpec};
use oled_ui::node::NodeId;
use oled_ui::{CaptionHit, CommandTable, Event, EventCx, LayoutCx, PaintCx, PointP, Reply, Screen, SizeL, SizeP};
use oled_ui::{Tokens, UiLang, WidgetId};

const ROOT: &str = r"C:\mod-made-up\reports";
const OTHER: &str = r"C:\mod-made-up\archive";
const PROFILE: &str = r"C:\mod-made-up\home";

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

/// What a test asks the screen to do from inside an event, where an EventCx exists.
enum Op {
    Event(Event),
    Cmd(Cmd),
    HandOff(Target, bool),
    Go(Location),
    /// What a click on the status notice's button runs.
    Act(NoticeAct),
}

struct Wrap(FilesScreen);

impl Screen for Wrap {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.0.layout(cx, avail)
    }
    fn paint(&self, cx: &mut PaintCx) {
        self.0.paint(cx)
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.0.event(cx, ev)
    }
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        match msg.downcast::<Op>() {
            Ok(op) => match *op {
                Op::Event(ev) => self.0.event(cx, &ev),
                Op::Cmd(c) => files_engine::keys::run(&mut self.0, cx, c),
                Op::HandOff(t, typed) => handoff::hand_off(&mut self.0, cx, t, typed),
                Op::Go(to) => {
                    window::navigate(&mut self.0, cx, to, None);
                    Reply::Relayout
                }
                Op::Act(a) => files_engine::status::on_notice_action(&mut self.0, cx, a),
            },
            Err(msg) => self.0.data(cx, msg),
        }
    }
    fn commands(&self) -> &CommandTable {
        self.0.commands()
    }
    fn caption_hit(&self, p: PointP) -> CaptionHit {
        self.0.caption_hit(p)
    }
}

/// Two folders and three files.
fn rows() -> Vec<(&'static str, bool, u64, i64)> {
    vec![
        ("invoices", true, 0, day(3)),
        ("projects", true, 0, day(9)),
        ("notes.txt", false, 1_200, day(1)),
        ("report-final-v2.pdf", false, 3_400_000, day(5)),
        ("photos-raw.zip", false, 90_000_000, day(40)),
    ]
}

struct Win {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<Wrap>,
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    fn op(&mut self, op: Op) {
        self.h.send(Box::new(op));
        self.pump();
    }

    fn close(&mut self) {
        self.op(Op::Event(Event::CloseRequested));
    }

    fn closed(&self) -> bool {
        self.h.last_outputs().iter().any(|o| o == "CloseWindow")
    }

    fn dump(&self) -> String {
        self.h.a11y_dump()
    }

    fn has_label(&self, text: &str) -> bool {
        self.dump().lines().any(|l| l.trim_start() == format!("Label {text:?}"))
    }

    fn has_button(&self, text: &str) -> bool {
        self.dump().lines().any(|l| l.trim_start().starts_with(&format!("Button {text:?}")))
    }

    /// The hand-offs sent to the shell host, in order: the text run and its folder, or the folder opened.
    fn handoffs(&self) -> Vec<(ReqId, String)> {
        self.f
            .shell
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                ShellReq::RunTyped { req, text, dir } => {
                    Some((req, format!("run {text} in {}", dir.to_string_lossy())))
                }
                ShellReq::Exec { req, verb: Verb::OpenInExplorer, items } => match items.as_slice() {
                    [ItemRef::Path(p)] => Some((req, format!("explorer {}", p.to_string_lossy()))),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn location(&self) -> Location {
        self.fs().tabs().active().nav.location().clone()
    }
}

/// Window 1 of a process over ROOT (its record kept as run_loop_requests would), the listing in, the first-frame
/// timers fired.
fn window() -> Win {
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(&rows()));
    f.enumr.queue(&wp(OTHER), fakes::listing(&[("old-notes.txt", false, 10, day(70))]));
    let shared = f.shared(UiLang::EN);
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    assert_eq!(shared.new_win(), 1);
    let rec = WinRec { win: 1, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Fs(wp(ROOT))) };
    shared.windows.borrow_mut().push(rec);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(ROOT)), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    shared.session.borrow_mut().known.insert("profile".to_string(), PROFILE.to_string());
    let mut w = Win { f, shared, h };
    w.h.advance(1_000);
    w.pump();
    w
}

/// The window lingering: two tabs open, then the last close.
fn lingering() -> Win {
    let mut w = window();
    w.op(Op::Cmd(Cmd::NewTab));
    assert_eq!(w.fs().tabs().len(), 2);
    w.close();
    w
}

// ---- F4.12: the linger ---------------------------------------------------------------------------------------------

#[test]
fn last_close_hides_and_arms_one_timer() {
    let w = lingering();
    assert!(!w.closed(), "{:?}", w.h.last_outputs());
    assert_eq!(*w.shared.loop_requests.borrow(), [LoopReq::Show { win: 1, how: Show::Hide }]);
    assert_eq!(w.fs().tabs().len(), 1, "the tabs collapse to one");
    assert_eq!(w.location(), Location::Home);
    assert_eq!(w.f.shell.shutdowns(), 1, "the shell host is told to stop");
    assert!(w.f.ops.calls().contains(&OpsCall::Shutdown), "the ops client says Bye");
    assert!(w.f.own.writes().iter().any(|n| n == names::SESSION_FILE), "the placements are saved");
    let mut w = w;
    // Exactly one timer: nothing fires in the ten minutes less one millisecond, and after the one that fires at ten
    // minutes nothing is armed any more.
    let n = w.h.last_outputs().len();
    w.h.advance(LINGER_MS - 1);
    assert_eq!(w.h.last_outputs().len(), n, "no other timer fires while lingering: {:?}", &w.h.last_outputs()[n..]);
    assert!(!w.h.is_idle(), "the linger's timer is armed");
    w.h.advance(1);
    assert_eq!(w.h.last_outputs().len(), n + 2, "{:?}", &w.h.last_outputs()[n..]);
    assert!(w.h.is_idle());
}

#[test]
fn timer_exits() {
    let mut w = lingering();
    w.h.advance(LINGER_MS);
    assert!(w.closed(), "{:?}", w.h.last_outputs());
    assert!(w.shared.main_windows().is_empty(), "the window's record went with it");
    assert!(!w.f.hub.post_to(1, Msg::FirstFrame), "the window left the hub");
}

#[test]
fn instance_cmd_reuses_window() {
    let mut w = lingering();
    // A probe shows nothing: the linger goes on.
    w.h.send(Box::new(Msg::Instance(InstanceCmd::Probe { path: wp(ROOT) })));
    assert_eq!(w.shared.loop_requests.borrow().len(), 1);
    w.h.send(Box::new(Msg::Instance(InstanceCmd::NewWindow(Location::Fs(wp(OTHER))))));
    w.pump();
    assert_eq!(
        *w.shared.loop_requests.borrow(),
        [LoopReq::Show { win: 1, how: Show::Hide }, LoopReq::Show { win: 1, how: Show::Front }],
        "the hidden window comes back; no new window"
    );
    assert_eq!(w.location(), Location::Fs(wp(OTHER)));
    assert!(w.has_label("1 item"), "{}", w.dump());
    // The linger's timer went with it, and the next close lingers again.
    w.h.advance(LINGER_MS + 1_000);
    assert!(!w.closed());
    w.close();
    assert!(!w.closed());
    assert_eq!(w.shared.loop_requests.borrow().last(), Some(&LoopReq::Show { win: 1, how: Show::Hide }));
}

#[test]
fn reuse_starts_clean() {
    let mut w = window();
    w.op(Op::Cmd(Cmd::About));
    assert!(w.dump().contains("Dialog"));
    w.close();
    w.h.send(Box::new(Msg::Instance(InstanceCmd::NewWindow(Location::Home))));
    w.pump();
    assert_eq!(w.shared.loop_requests.borrow().last(), Some(&LoopReq::Show { win: 1, how: Show::Front }));
    assert_eq!(w.location(), Location::Home);
    assert!(!w.dump().contains("Dialog"), "what was open over the window went with the linger: {}", w.dump());
}

#[test]
fn select_reuses_window_and_selects() {
    let mut w = lingering();
    let item = wp(r"C:\mod-made-up\reports\notes.txt");
    w.f.enumr.queue(&wp(ROOT), fakes::listing(&rows()));
    w.h.send(Box::new(Msg::Instance(InstanceCmd::Select { dir: wp(ROOT), item })));
    w.pump();
    w.h.advance(1_000);
    w.pump();
    assert_eq!(w.location(), Location::Fs(wp(ROOT)));
    assert!(w.has_label(" · 1 selected · 1.17 KB"), "{}", w.dump());
    assert_eq!(w.shared.loop_requests.borrow().last(), Some(&LoopReq::Show { win: 1, how: Show::Front }));
}

#[test]
fn linger_drops_listings() {
    let mut w = window();
    w.op(Op::Cmd(Cmd::NewTab));
    let bytes = |w: &Win| w.fs().tabs().iter().map(|t| t.table.model().listing().heap_bytes()).sum::<usize>();
    assert!(bytes(&w) > 0);
    w.close();
    for t in w.fs().tabs().iter() {
        assert_eq!(t.table.model().listing().heap_bytes(), 0, "every listing is dropped");
    }
    assert_eq!(w.shared.navcache.borrow().heap_bytes(), 0);
}

#[test]
fn not_with_running_ops() {
    let mut w = window();
    let mut r = OpRow::new(OpId(3), OpKind::DeletePermanent, 4);
    r.state = OpState::Running;
    w.shared.ops.borrow_mut().insert(r);
    w.close();
    assert_eq!(*w.shared.loop_requests.borrow(), [LoopReq::Ops], "the operations window, not the linger");
    assert_eq!(w.fs().tabs().active().nav.location(), &Location::Fs(wp(ROOT)));
}

#[test]
fn second_window_closes_without_lingering() {
    let mut w = window();
    let rec = WinRec { win: 2, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Home) };
    w.shared.windows.borrow_mut().push(rec);
    w.close();
    assert!(w.closed(), "another main window stays: this one closes");
    assert!(w.shared.loop_requests.borrow().is_empty());
}

// ---- F4.13: hand-offs and the loop guard ---------------------------------------------------------------------------

#[test]
fn targets_to_requests() {
    // The plan of each target, pure: text runs only when the user typed it.
    assert_eq!(handoff::plan_of(&Target::RecycleBin, false), Some(Plan::Run(handoff::RECYCLE_BIN.to_string())));
    assert_eq!(handoff::plan_of(&Target::Other("shell:Fonts".into()), true), Some(Plan::Run("shell:Fonts".into())));
    assert_eq!(handoff::plan_of(&Target::Other("shell:Fonts".into()), false), None, "U5: only typed text runs");
    assert_eq!(handoff::plan_of(&Target::Other("  ".into()), true), None);
    // Through the window: each reaches the shell host as this window's request, in the profile folder.
    let mut w = window();
    w.op(Op::HandOff(Target::RecycleBin, false));
    w.op(Op::HandOff(Target::Network, false));
    w.op(Op::HandOff(Target::ThisPc, false));
    w.op(Op::HandOff(Target::Other("shell:Fonts".into()), true));
    w.op(Op::HandOff(Target::Folder(wp(OTHER)), false));
    let sent: Vec<String> = w.handoffs().into_iter().map(|(_, t)| t).collect();
    assert_eq!(
        sent,
        [
            format!("run shell:RecycleBinFolder in {PROFILE}"),
            format!("run shell:NetworkPlacesFolder in {PROFILE}"),
            format!("run shell:MyComputerFolder in {PROFILE}"),
            format!("run shell:Fonts in {PROFILE}"),
            format!("explorer {OTHER}"),
        ]
    );
    for (req, _) in w.handoffs() {
        assert_eq!(w.shared.req_owner.borrow().get(&req), Some(&ReqOwner::Exec { win: 1 }), "replies come back here");
    }
    // Text nobody typed runs nothing: the notice says so.
    w.op(Op::HandOff(Target::Other("shell:Fonts".into()), false));
    assert_eq!(w.handoffs().len(), 5);
    assert!(w.has_label("Not available in this alpha: use File Explorer"), "{}", w.dump());
}

#[test]
fn handoff_fallback_offers_no_stale_retry() {
    let mut w = window();
    // A file opened earlier, then a hand-off the host does not answer: Retry must not open the file again.
    w.h.key("Home").key("Down").key("Down").key("Enter");
    assert!(w.f.shell.requests().iter().any(|r| matches!(r, ShellReq::Exec { verb: Verb::Open, .. })));
    w.op(Op::HandOff(Target::RecycleBin, false));
    let (req, _) = w.handoffs()[0].clone();
    w.f.shell.reply(ShellReply::Fallback { req, why: FallbackWhy::Deadline });
    w.pump();
    assert!(w.has_label("Windows menu didn't respond"), "{}", w.dump());
    assert!(!w.has_button("Retry (F5)"), "{}", w.dump());
}

#[test]
fn guard_blocks_within_3s_and_allows_after() {
    // Pure: the same target within GUARD_MS of the last one is held back, at GUARD_MS it goes; others are not held.
    let Guard::Allow(file) = handoff::guard_check(None, "shell:RecycleBinFolder", 10_000) else { panic!("first") };
    assert_eq!(handoff::guard_check(Some(&file), "shell:RecycleBinFolder", 10_000 + GUARD_MS - 1), Guard::Block);
    assert!(matches!(handoff::guard_check(Some(&file), "shell:RecycleBinFolder", 10_000 + GUARD_MS), Guard::Allow(_)));
    assert!(matches!(handoff::guard_check(Some(&file), "shell:NetworkPlacesFolder", 10_001), Guard::Allow(_)));
    // Through the window: a second click at once shows the notice, and its [Open ↗] hands off after all.
    let mut w = window();
    w.op(Op::HandOff(Target::RecycleBin, false));
    w.op(Op::HandOff(Target::RecycleBin, false));
    assert_eq!(w.handoffs().len(), 1, "held back");
    assert!(w.has_label("This location opens in File Explorer"), "{}", w.dump());
    assert!(w.has_button("Open ↗"), "{}", w.dump());
    w.op(Op::Act(NoticeAct::HandOff));
    assert_eq!(w.handoffs().len(), 2, "the user's own click opens it");
    w.op(Op::Act(NoticeAct::HandOff));
    assert_eq!(w.handoffs().len(), 2, "once");
    // The guard is a file: an entry another process wrote 3.5 s ago does not hold this one back, one just written does.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let w2 = |ms: u64| format!(r#"[{{"target":"shell:NetworkPlacesFolder","ms":{ms}}}]"#);
    w.f.own.put(names::GUARD_FILE, w2(now - 3_500).as_bytes());
    w.op(Op::HandOff(Target::Network, false));
    assert_eq!(w.handoffs().len(), 3);
    w.f.own.put(names::GUARD_FILE, w2(now).as_bytes());
    w.op(Op::HandOff(Target::Network, false));
    assert_eq!(w.handoffs().len(), 3, "another process's hand-off a moment ago");
}

#[test]
fn guard_file_capped_and_malformed_tolerated() {
    let entries = |b: &[u8]| serde_json::from_slice::<Vec<serde_json::Value>>(b).unwrap();
    // At most GUARD_MAX entries, the newest kept.
    let mut file: Option<Vec<u8>> = None;
    for i in 0..(GUARD_MAX as u64 + 6) {
        file = Some(handoff::guard_record(file.as_deref(), &format!("shell:Place{i}"), 1_000 * i));
    }
    let list = entries(file.as_deref().unwrap());
    assert_eq!(list.len(), GUARD_MAX);
    assert_eq!(list[0]["target"], "shell:Place6");
    assert_eq!(list[GUARD_MAX - 1]["target"], format!("shell:Place{}", GUARD_MAX + 5));
    // A long typed text is kept by its first 512 characters.
    let long = "x".repeat(2_000);
    let one = handoff::guard_record(None, &handoff::guard_key(&Plan::Run(long)), 5);
    assert_eq!(entries(&one)[0]["target"].as_str().map(str::len), Some(512));
    // A malformed file, or one of another shape, counts as empty and is written again whole.
    for bad in [&b"{not json"[..], b"{\"target\":1}", b"[{\"target\":\"x\",\"ms\":-4}]", b"\xff\xfe"] {
        let Guard::Allow(again) = handoff::guard_check(Some(bad), "shell:RecycleBinFolder", 9) else { panic!() };
        assert_eq!(entries(&again).len(), 1, "{bad:?}");
    }
    // Through the window: a broken file does not stop the hand-off, and is replaced by a good one.
    let mut w = window();
    w.f.own.put(names::GUARD_FILE, b"garbage");
    w.op(Op::HandOff(Target::ThisPc, false));
    assert_eq!(w.handoffs().len(), 1);
    assert_eq!(entries(&w.f.own.file(names::GUARD_FILE).unwrap()).len(), 1);
    // A file over its cap is not read (OwnFiles returns nothing): it counts as empty.
    w.f.own.put(names::GUARD_FILE, &vec![b' '; handoff::GUARD_CAP + 1]);
    w.op(Op::HandOff(Target::ThisPc, false));
    assert_eq!(w.handoffs().len(), 2);
}

#[test]
fn folder_hand_off_goes_through_the_guard() {
    // Ctrl+Shift+O twice at once: one File Explorer window, then the guard's notice; the folder is in the guard file.
    let mut w = window();
    w.h.key("Ctrl+Shift+O");
    w.h.key("Ctrl+Shift+O");
    w.pump();
    let sent: Vec<String> = w.handoffs().into_iter().map(|(_, t)| t).collect();
    assert_eq!(sent, [format!("explorer {ROOT}")]);
    assert!(w.has_label("This location opens in File Explorer"), "{}", w.dump());
    let file = w.f.own.file(names::GUARD_FILE).unwrap();
    let list = serde_json::from_slice::<Vec<serde_json::Value>>(&file).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["target"], ROOT);
}

// ---- F4.14: the updated notice and Reopen ------------------------------------------------------------------------

fn app_name() -> String {
    UiLang::EN.kit(oled_ui::strings::k::KIT_APP_FILES).to_string()
}

#[test]
fn notice_on_updated() {
    let mut w = window();
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.pump();
    assert!(w.has_label(&format!("{} was updated", app_name())), "{}", w.dump());
    assert!(w.has_button("Reopen"), "{}", w.dump());
    // Never automatic: nothing is asked of the loop, nothing started, and the notice holds a minute.
    assert!(w.shared.loop_requests.borrow().is_empty());
    w.h.advance(notices::UPDATED_HOLD_MS - 1_000);
    w.h.render();
    assert!(w.has_button("Reopen"));
    w.h.advance(2_000);
    w.h.render();
    assert!(!w.has_button("Reopen"));
    assert!(w.has_label("5 items"));
}

#[derive(Default)]
struct RecordOps {
    shown: Vec<(WindowRef, Show)>,
    quits: u32,
}

impl LoopOps for RecordOps {
    fn open(&mut self, spec: WindowSpec, screen: Box<dyn Screen>) -> Option<WindowRef> {
        drop((spec, screen));
        None
    }
    fn proxy(&self, w: WindowRef) -> Option<Proxy> {
        let _ = w;
        None
    }
    fn hwnd(&self, w: WindowRef) -> Option<isize> {
        Some(w.0 as isize)
    }
    fn show(&mut self, w: WindowRef, how: Show) -> bool {
        self.shown.push((w, how));
        true
    }
    fn move_to(&mut self, w: WindowRef, at: PointP) -> bool {
        let _ = (w, at);
        true
    }
    fn quit(&mut self) {
        self.quits += 1;
    }
}

#[test]
fn reopen_saves_and_restarts() {
    let mut w = window();
    // Two tabs here (the second on another folder), and a second window elsewhere.
    w.op(Op::Cmd(Cmd::NewTab));
    w.op(Op::Go(Location::Fs(wp(OTHER))));
    let rec = WinRec { win: 2, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Home) };
    w.shared.windows.borrow_mut().push(rec);
    // No restart before Reopen.
    let mut started: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut spawn = |exe: &std::path::Path, line: &[u16]| {
        started.push((exe.to_path_buf(), String::from_utf16_lossy(line)));
        true
    };
    assert!(!notices::restart_with(&w.shared, &mut spawn));
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.op(Op::Act(NoticeAct::Reopen));
    // The windows' locations are in session.json, this window's tabs first.
    let saved = Session::load(&*w.f.own);
    assert_eq!(saved.reopen, [vec![ROOT.to_string(), OTHER.to_string()], vec![String::new()]]);
    // The loop ends through the loop callback; nothing is shown or opened.
    assert_eq!(*w.shared.loop_requests.borrow(), [LoopReq::Quit]);
    let mut lo = RecordOps::default();
    window::run_loop_requests(&w.shared, &mut lo);
    assert_eq!((lo.quits, lo.shown.len()), (1, 0));
    // After the loop, the installed exe starts with no argument.
    assert!(notices::restart_with(&w.shared, &mut spawn));
    let exe = std::env::current_exe().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, exe);
    assert_eq!(started[0].1.trim_matches('"'), exe.to_string_lossy(), "the exe's path alone: {:?}", started[0].1);
}

#[test]
fn reopen_keeps_where_other_windows_are_now() {
    let mut w = window();
    // A second window of this process, opened on ROOT; its user then opens a folder in it.
    let win = w.shared.new_win();
    let rec = WinRec { win, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Fs(wp(ROOT))) };
    w.shared.windows.borrow_mut().push(rec);
    let _posted = w.f.attach_window(win);
    w.f.enumr.queue(&wp(ROOT), fakes::listing(&rows()));
    let screen = FilesScreen::new(Rc::clone(&w.shared), win, Location::Fs(wp(ROOT)), None, w.shared.new_tab());
    let mut two = Harness::new(Wrap(screen), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    two.render();
    w.f.pump(&mut two, win);
    two.render();
    let invoices = format!(r"{ROOT}\invoices");
    w.f.enumr.queue(&wp(&invoices), fakes::listing(&[]));
    two.key("Home").key("Enter");
    assert_eq!(w.shared.win(win).and_then(|r| r.location), Some(Location::Fs(wp(&invoices))), "the record follows");
    // Reopen in the first window keeps the folder the second one shows now, not the one it opened on.
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.op(Op::Act(NoticeAct::Reopen));
    assert_eq!(Session::load(&*w.f.own).reopen, [vec![ROOT.to_string()], vec![invoices]]);
}

#[test]
fn restore_list_used_once() {
    let mut session = Session {
        reopen: vec![
            vec![ROOT.to_string(), OTHER.to_string(), "not a path".to_string()],
            vec![String::new()],
            vec!["also not a path".to_string()],
        ],
        ..Default::default()
    };
    let windows = notices::take_reopen(&mut session);
    assert_eq!(
        windows,
        [vec![Location::Fs(wp(ROOT)), Location::Fs(wp(OTHER))], vec![Location::Home]],
        "what does not parse is dropped, and a window left empty with it"
    );
    assert!(session.reopen.is_empty());
    assert!(notices::take_reopen(&mut session).is_empty(), "used once");
    // At most 8 windows of 8 tabs.
    session.reopen = vec![vec![ROOT.to_string(); 10]; 10];
    let windows = notices::take_reopen(&mut session);
    assert_eq!((windows.len(), windows[0].len()), (notices::REOPEN_WINDOWS, notices::REOPEN_TABS));
    // A restored window gets its further tabs in their order, the first one shown, each listing.
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(&rows()));
    f.enumr.queue(&wp(OTHER), fakes::listing(&[("old-notes.txt", false, 10, day(70))]));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let mut screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(ROOT)), None, tab);
    notices::restore_tabs(&mut screen, vec![Location::Fs(wp(OTHER)), Location::Home]);
    let at: Vec<Location> = screen.tabs().iter().map(|t| t.nav.location().clone()).collect();
    assert_eq!(at, [Location::Fs(wp(ROOT)), Location::Fs(wp(OTHER)), Location::Home]);
    assert_eq!(screen.tabs().active().nav.location(), &Location::Fs(wp(ROOT)));
    assert!(f.enumr.jobs().iter().any(|j| j.dir == wp(OTHER)), "the restored tab lists at once");
    // Reopen asked no restart in this process: none follows the loop.
    let mut spawn = |_: &std::path::Path, _: &[u16]| -> bool { panic!("no restart") };
    assert!(!notices::restart_with(&shared, &mut spawn));
}

/// The status notice's button, reached by the keyboard and pressed as a user would (the press clears the notice first).
fn press_notice(w: &mut Win) {
    let mut found = false;
    for _ in 0..40 {
        found = w.h.focused().is_some_and(|n| n.owner() == files_engine::status::ID_ACTION);
        if found {
            break;
        }
        w.h.key("Tab");
    }
    assert!(found, "the notice's button takes the focus: {}", w.dump());
    w.h.key("Enter");
    w.pump();
}

#[test]
fn reopen_waits_for_running_ops() {
    let mut w = window();
    let mut r = OpRow::new(OpId(3), OpKind::DeletePermanent, 4);
    r.state = OpState::Running;
    w.shared.ops.borrow_mut().insert(r);
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.pump();
    press_notice(&mut w);
    // The loop goes on (the operations keep their window and the undo), and nothing is kept for a restart.
    assert!(w.shared.loop_requests.borrow().is_empty(), "{:?}", w.shared.loop_requests.borrow());
    assert!(Session::load(&*w.f.own).reopen.is_empty());
    let mut spawn = |_: &std::path::Path, _: &[u16]| -> bool { panic!("no restart") };
    assert!(!notices::restart_with(&w.shared, &mut spawn));
    // The notice is back, for a Reopen once they are done.
    assert!(w.has_button("Reopen"), "{}", w.dump());
}

#[test]
fn updated_process_never_lingers() {
    // Updated, then the last close: the window closes; a later start runs the new exe.
    let mut w = window();
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.close();
    assert!(w.closed(), "{:?}", w.h.last_outputs());
    assert!(w.shared.loop_requests.borrow().is_empty());
    // Lingering, then updated: the hidden window closes at once.
    let mut w = lingering();
    assert!(!w.closed());
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    assert!(w.closed(), "{:?}", w.h.last_outputs());
    assert!(w.shared.main_windows().is_empty());
}

#[test]
fn a_window_opened_after_the_update_never_lingers() {
    let mut w = window();
    // A second window of the same process, opened before the message, that the message does not reach.
    let rec = WinRec { win: 2, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Home) };
    w.shared.windows.borrow_mut().push(rec);
    let tab = w.shared.new_tab();
    let other = FilesScreen::new(Rc::clone(&w.shared), 2, Location::Home, None, tab);
    let mut h2 = Harness::new(Wrap(other), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h2.render();
    w.h.send(Box::new(Msg::Updated { version: 0x01_08_00 }));
    w.pump();
    // The first window goes; the second is the last main window and its own flag was never set.
    w.shared.windows.borrow_mut().retain(|r| r.win != 1);
    h2.send(Box::new(Op::Event(Event::CloseRequested)));
    h2.render();
    assert!(h2.last_outputs().iter().any(|o| o == "CloseWindow"), "{:?}", h2.last_outputs());
    assert!(w.shared.loop_requests.borrow().is_empty(), "no linger");
}

// ---- F4.15: About --------------------------------------------------------------------------------------------------

#[test]
fn about_dialog_named_and_closes_on_esc() {
    let mut w = window();
    w.op(Op::Cmd(Cmd::About));
    let dump = w.dump();
    assert!(dump.contains(&format!("Dialog \"{} Alpha\"", app_name())), "{dump}");
    assert!(w.has_button("Close"), "{dump}");
    w.h.assert_named();
    let close = NodeId::new(WidgetId(about::ID_DIALOG.0 + 1), 0);
    assert_eq!(w.h.focused(), Some(close), "the focus is on Close");
    // While it is open the window's chords do nothing behind it.
    w.h.key("Ctrl+T");
    assert_eq!(w.fs().tabs().len(), 1);
    w.h.key("Esc");
    w.h.render();
    assert!(!w.dump().contains("Dialog"), "{}", w.dump());
    assert_eq!(w.h.focused().map(|n| n.owner()), Some(tab::TABLE_ID), "the list has the focus back");
    // Enter on Close shuts it as well, and the window's chords work again.
    w.op(Op::Cmd(Cmd::About));
    assert!(w.dump().contains("Dialog"));
    w.h.key("Enter");
    w.h.render();
    assert!(!w.dump().contains("Dialog"), "{}", w.dump());
    w.f.enumr.queue(&wp(ROOT), fakes::listing(&rows()));
    w.h.key("Ctrl+T");
    assert_eq!(w.fs().tabs().len(), 2);
}
