//! The window's operations (P3-files-app F3.1, F3.2, F3.4, F3.5, F3.6): the entry point and its refusals, Del with the
//! recycle classification and the one bar, the chip, the line and the flyout, the operations window, and the reports
//! of an earlier session. Plain tests over the fakes through the kit's Harness: requests that pass the gate do so
//! inside a TempRoot and reach only FakeOps.

mod common;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use common::temp::TempRoot;
use files_engine::bar;
use files_engine::fakes::{self, Fakes, OpsCall, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::listing::WidePath;
use files_engine::model::key_of;
use files_engine::ops::classify::{NoBin, Recyclability};
use files_engine::ops_client::{OpsError, OpsEvent};
use files_engine::ops_ui::{self, OpRow, OpState, OpsModel};
use files_engine::opswin::{self, OpsScreen};
use files_engine::status::{self, NoticeAct};
use files_engine::window::{self, FilesScreen, LoopOps, LoopReq, Shared, Show, WinKind, WinRec};
use oled_trust::frame::files::{
    Change, ChangeKind, ItemRef, Location, OpEvt, OpId, OpKind, OpResult, Refusal, UndoKind,
};
use oled_ui::harness::Harness;
use oled_ui::host::{AppMsg, Proxy, WindowRef, WindowSpec};
use oled_ui::model::{KeySet, ScrollTo, Selection};
use oled_ui::{
    CaptionHit, CommandTable, Event, EventCx, Key, LayoutCx, Mods, PaintCx, PointL, PointP, RectP, Reply, Screen,
    SizeL, SizeP, Tokens, UiLang,
};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

/// What a test asks the screen to do from inside an event, where an EventCx exists.
enum Op {
    Select(Vec<&'static str>),
    Del(Vec<WidePath>),
    Go(Location),
    Event(Event),
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
                Op::Select(names) => {
                    let t = self.0.tabs_mut().active_mut();
                    let mut keys = KeySet::new();
                    let mut first = None;
                    for n in names {
                        if let Some(id) = t.table.model().listing().find(n.as_bytes()) {
                            keys.insert(key_of(id));
                            first = first.or(Some(key_of(id)));
                        }
                    }
                    t.table.set_selection(cx, Selection::Keys(keys));
                    if let Some(k) = first {
                        t.table.set_focused(cx, k, ScrollTo::Nearest);
                    }
                    self.0.event(cx, &Event::FocusOut);
                    Reply::Relayout
                }
                Op::Del(items) => ops_ui::delete_flow(&mut self.0, cx, items),
                Op::Go(to) => {
                    window::navigate(&mut self.0, cx, to, None);
                    Reply::Relayout
                }
                Op::Event(ev) => self.0.event(cx, &ev),
                Op::Act(a) => status::on_notice_action(&mut self.0, cx, a),
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

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

/// One folder and four files, in the order the list shows them.
fn rows() -> Vec<(&'static str, bool, u64, i64)> {
    vec![
        ("projects", true, 0, day(2)),
        ("a-notes.txt", false, 1_200, day(1)),
        ("b-report-final-v2.pdf", false, 3_400_000, day(5)),
        ("c-photos-raw.zip", false, 90_000_000, day(40)),
        ("d-setup-tool.exe", false, 74_200_000, day(120)),
    ]
}

struct Win {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<Wrap>,
    dir: WidePath,
    _root: Option<TempRoot>,
}

fn main_rec(win: u32, dir: &WidePath) -> WinRec {
    WinRec { win, kind: WinKind::Main, wref: None, hwnd: None, location: Some(Location::Fs(dir.clone())) }
}

fn window_at(dir: WidePath, root: Option<TempRoot>) -> Win {
    let f = Fakes::new();
    f.enumr.queue(&dir, fakes::listing(&rows()));
    let shared = f.shared(UiLang::EN);
    // Machine-independent numbers; window 1 takes its id as run_loop_requests would.
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    assert_eq!(shared.new_win(), 1);
    shared.windows.borrow_mut().push(main_rec(1, &dir));
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir.clone()), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, shared, h, dir, _root: root }
}

/// A window over a TempRoot: the gate lets its requests through to FakeOps.
fn in_root(tag: &str) -> Win {
    let root = TempRoot::new(tag);
    let dir = wp(&root.path().to_string_lossy());
    window_at(dir, Some(root))
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }
    fn path(&self, name: &str) -> WidePath {
        self.dir.join(&name.encode_utf16().collect::<Vec<u16>>()).unwrap()
    }
    fn paths(&self, names: &[&str]) -> Vec<WidePath> {
        names.iter().map(|n| self.path(n)).collect()
    }
    fn refs(&self, names: &[&str]) -> Vec<ItemRef> {
        self.paths(names).into_iter().map(ItemRef::Path).collect()
    }
    fn select(&mut self, names: &[&'static str]) {
        self.h.send(Box::new(Op::Select(names.to_vec())));
        self.h.render();
    }
    fn key(&mut self, k: &str) {
        self.h.key(k);
        self.h.render();
    }
    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }
    /// Del held past the keyboard's repeat delay: a repeated key down.
    fn held_del(&mut self) {
        let ev = Event::Key { key: Key::DELETE, down: true, repeat: true, mods: Mods::NONE };
        self.h.send(Box::new(Op::Event(ev)));
        self.h.render();
    }
    fn emit(&self, e: OpEvt) {
        self.f.ops.emit(OpsEvent::Evt(e));
    }
    fn calls(&self) -> Vec<OpsCall> {
        self.f.ops.calls()
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
    fn bar(&self) -> &bar::RecycleBar {
        bar::bar_of(self.fs())
    }
    fn chip(&self) -> Option<String> {
        ops_ui::flyout_of(self.fs()).chip().map(|(_, t)| t)
    }
    fn click(&mut self, r: RectP) {
        self.h.click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
        self.h.render();
    }
    /// Del answers come from this script, by the item's name.
    fn script(&self, f: fn(&str) -> Recyclability) {
        let facts: ops_ui::FactsFn = Arc::new(move |items: &[(WidePath, u64)]| {
            items.iter().map(|(p, _)| (p.clone(), f(&p.to_string_lossy()))).collect()
        });
        self.shared.ops.borrow_mut().set_facts(facts);
    }
    fn focused_label(&self) -> Option<String> {
        let t = &self.fs().tabs().active().table;
        let row = oled_ui::model::ListModel::row_of(t.model(), t.focused()?)?;
        Some(oled_ui::model::ListModel::label(t.model(), row).to_string())
    }
}

fn started(op: u32, kind: OpKind, items: u32) -> OpEvt {
    OpEvt::Started { op: OpId(op), kind, items, bytes: 0 }
}

fn progress(op: u32, done: u64, total: u64) -> OpEvt {
    OpEvt::Progress { op: OpId(op), done, total, bps: 0, current: "".into() }
}

fn finished(op: u32, items: u32, failed: u32, undo: Option<UndoKind>) -> OpEvt {
    let result = if failed > 0 { OpResult::Failed } else { OpResult::Done };
    OpEvt::Finished { op: OpId(op), result, undo, replaced: 0, items, failed }
}

const DOT: &str = " \u{b7} ";

// ---- F3.1: the entry point --------------------------------------------------------------------------------------

/// The 0-based line range of the body of the first function whose signature holds sig.
fn fn_body(text: &str, sig: &str) -> Option<std::ops::Range<usize>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.contains(sig))?;
    let mut depth = 0i32;
    for (n, l) in lines.iter().enumerate().skip(start) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        if n > start && depth <= 0 {
            return Some(start..n + 1);
        }
    }
    None
}

/// The source files of a module of the window: src/<m>.rs and everything under src/<m>/.
fn sources_of(src: &Path, m: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let file = src.join(format!("{m}.rs"));
    if file.is_file() {
        out.push(file);
    }
    let mut dirs = vec![src.join(m)];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn every_mutation_goes_through_request() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    let window = lib.split("// ---- P3-files-app (window)").nth(1).expect("the window's marker in lib.rs");
    let mods: Vec<&str> =
        window.lines().filter_map(|l| l.trim().strip_prefix("pub mod ")).map(|m| m.trim_end_matches(';')).collect();
    assert!(mods.contains(&"ops_ui") && mods.contains(&"window") && mods.contains(&"rename"), "{mods:?}");
    // OpsService's mutating methods. services.rs and fakes.rs implement the trait (the real adapter calls OpsClient's
    // methods of the same names); every other window module asks through ops_ui::request.
    const CALLS: [&str; 6] = [".recycle(", ".delete_permanent(", ".rename(", ".new_folder(", ".undo(", ".resume_rest("];
    let mut outside = Vec::new();
    let mut inside = Vec::new();
    for m in mods.iter().filter(|m| !["services", "fakes"].contains(m)) {
        for file in sources_of(&src, m) {
            let text = std::fs::read_to_string(&file).unwrap();
            let body = if *m == "ops_ui" { fn_body(&text, "pub fn request(") } else { None };
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                for c in CALLS.iter().filter(|c| code.contains(*c)) {
                    if body.as_ref().is_some_and(|b| b.contains(&n)) {
                        inside.push(*c);
                    } else {
                        outside.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    assert!(outside.is_empty(), "OpsService's mutating methods outside ops_ui::request: {outside:#?}");
    for c in CALLS {
        assert!(inside.contains(&c), "request calls {c}");
    }
}

#[test]
fn refused_shows_notice_and_sends_nothing() {
    // Outside the test and live roots the gate refuses: the notice, and nothing reaches the helper.
    let mut w = window_at(wp(r"C:\made-up\reports"), None);
    w.script(|_| Recyclability::Yes);
    w.select(&["a-notes.txt"]);
    w.key("Shift+Del");
    assert!(w.has_label("Changes are off in this preview build"), "{}", w.dump());
    w.key("Del");
    w.pump();
    assert!(w.calls().is_empty(), "{:?}", w.calls());
    assert!(w.shared.ops.borrow().rows().is_empty());
    // The helper refuses: its reason, and the model holds nothing.
    let mut w = in_root("ops_refused_notice");
    w.f.ops.set_fail(Some(OpsError::Refused(Refusal::Root)));
    w.select(&["a-notes.txt"]);
    w.key("Shift+Del");
    assert!(w.has_label("Not available in this alpha: use File Explorer"), "{}", w.dump());
    assert_eq!(w.calls().len(), 1, "the one call, refused");
    assert!(w.shared.ops.borrow().rows().is_empty());
    // The helper is gone: host_lost with Retry.
    w.f.ops.set_fail(Some(OpsError::Unavailable("gone")));
    w.key("Shift+Del");
    assert!(w.has_label("The operations helper stopped."), "{}", w.dump());
    assert!(w.has_button("Retry (F5)"), "{}", w.dump());
}

#[test]
fn accepted_op_enters_the_model() {
    let mut w = in_root("ops_accepted");
    w.select(&["a-notes.txt", "b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    assert_eq!(w.calls(), [OpsCall::DeletePermanent(w.refs(&["a-notes.txt", "b-report-final-v2.pdf"]))]);
    let m = w.shared.ops.borrow();
    let r = m.row(OpId(1)).expect("the accepted operation");
    assert_eq!((r.kind, r.win, r.count, r.state), (OpKind::DeletePermanent, 1, 2, OpState::Queued));
    assert_eq!(r.items, w.paths(&["a-notes.txt", "b-report-final-v2.pdf"]));
    assert_eq!(r.folder.as_ref(), Some(&w.dir));
    assert_eq!(m.running(), 1);
    assert_eq!(m.items_of(OpId(1)).len(), 2);
}

#[test]
fn refusal_texts_by_reason() {
    let fmt = Fmt::fixed_en();
    let en = UiLang::EN;
    let text = |r: Refusal| ops_ui::refusal_text_with(&r, "notes.txt", 3, en, &fmt).to_string();
    assert_eq!(text(Refusal::Replaced), "Can't undo: 3 items replaced older files");
    assert_eq!(text(Refusal::Changed), "Can't undo: notes.txt changed since");
    assert_eq!(text(Refusal::NotEmpty), "Can't undo: notes.txt isn't empty any more");
    assert_eq!(text(Refusal::OutsideTestRoot), "Changes are off in this preview build");
    assert_eq!(text(Refusal::BadName), "A name can't contain \\ / : * ? \" < > |");
    for r in [Refusal::Root, Refusal::MixedParents, Refusal::TooMany, Refusal::Elevated, Refusal::BadPath] {
        assert_eq!(text(r), "Not available in this alpha: use File Explorer", "{r:?}");
    }
    for r in [Refusal::NoItems, Refusal::UnknownOp, Refusal::OpInUse] {
        assert_eq!(text(r), "The operations helper stopped.", "{r:?}");
    }
    assert_eq!(ops_ui::refusal_text(&Refusal::Root, en), "Not available in this alpha: use File Explorer");
}

// ---- F3.2: Del --------------------------------------------------------------------------------------------------

fn zip_on_network(name: &str) -> Recyclability {
    if name.ends_with(".zip") {
        Recyclability::No(NoBin::Network)
    } else {
        Recyclability::Yes
    }
}

#[test]
fn del_all_recyclable_sends_recycle() {
    let mut w = in_root("ops_del_yes");
    w.script(|_| Recyclability::Yes);
    let jobs = w.f.bg.runs().len();
    w.select(&["a-notes.txt", "b-report-final-v2.pdf"]);
    w.key("Del");
    assert!(w.calls().is_empty(), "nothing goes before the classification");
    assert!(w.bar().is_pending());
    assert_eq!(w.f.bg.runs().len(), jobs + 1, "one classification job");
    assert_eq!(w.f.bg.runs().last().map(|r| r.0), Some(1), "for this window");
    w.pump();
    assert_eq!(w.calls(), [OpsCall::Recycle(w.refs(&["a-notes.txt", "b-report-final-v2.pdf"]))]);
    assert!(!w.bar().is_open() && !w.bar().is_pending());
    // Ctrl+D is the same flow.
    w.select(&["d-setup-tool.exe"]);
    w.key("Ctrl+D");
    w.pump();
    assert_eq!(w.calls().last(), Some(&OpsCall::Recycle(w.refs(&["d-setup-tool.exe"]))));
    // Nothing selected: nothing.
    w.select(&[]);
    w.key("Del");
    w.pump();
    assert_eq!(w.calls().len(), 2);
}

#[test]
fn del_on_network_item_opens_bar() {
    let mut w = in_root("ops_del_bar");
    w.script(zip_on_network);
    w.select(&["a-notes.txt", "b-report-final-v2.pdf", "c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    assert!(w.calls().is_empty(), "the bar asks first: {:?}", w.calls());
    assert!(w.bar().is_open());
    assert_eq!(w.bar().permanent(), w.paths(&["c-photos-raw.zip"]).as_slice());
    assert_eq!(w.bar().recyclable(), w.paths(&["a-notes.txt", "b-report-final-v2.pdf"]).as_slice());
    assert!(w.has_label("This item can't go to the Recycle Bin and will be deleted permanently"), "{}", w.dump());
    assert!(w.has_button("Delete (Enter)") && w.has_button("Cancel (Esc)"), "{}", w.dump());
    w.h.assert_named();
    // Two that cannot: the count.
    let mut w = in_root("ops_del_bar_two");
    w.script(|n| if n.ends_with(".txt") { Recyclability::Yes } else { Recyclability::No(NoBin::Unknown) });
    w.select(&["a-notes.txt", "c-photos-raw.zip", "d-setup-tool.exe"]);
    w.key("Del");
    w.pump();
    assert!(w.has_label("These 2 items can't go to the Recycle Bin and will be deleted permanently"), "{}", w.dump());
}

#[test]
fn bar_enter_sends_both_requests() {
    let mut w = in_root("ops_bar_enter");
    w.script(zip_on_network);
    w.select(&["a-notes.txt", "c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    w.key("Enter");
    assert_eq!(
        w.calls(),
        [OpsCall::Recycle(w.refs(&["a-notes.txt"])), OpsCall::DeletePermanent(w.refs(&["c-photos-raw.zip"]))]
    );
    assert!(!w.bar().is_open());
    // Del on the open bar is not its Enter: pressed again or held, it sends nothing and the bar stays.
    w.select(&["c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    assert!(w.bar().is_open());
    w.key("Del");
    w.held_del();
    assert_eq!(w.calls().len(), 2, "Del never confirms the bar: {:?}", w.calls());
    assert!(w.bar().is_open());
    // Enter does; only the item that cannot go is deleted when nothing else was selected.
    w.key("Enter");
    assert_eq!(w.calls().last(), Some(&OpsCall::DeletePermanent(w.refs(&["c-photos-raw.zip"]))));
    assert_eq!(w.calls().len(), 3, "no Recycle when every item cannot go");
    // The Delete button does the same.
    w.select(&["c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    let (_, delete, _) = w.bar().rects().expect("the bar is open");
    w.click(delete);
    assert_eq!(w.calls().len(), 4);
    assert!(!w.bar().is_open());
}

#[test]
fn bar_esc_sends_nothing() {
    let mut w = in_root("ops_bar_esc");
    w.script(zip_on_network);
    w.select(&["a-notes.txt", "c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    // While open the bar takes the keyboard: Down moves nothing.
    let at = w.focused_label();
    w.key("Down");
    assert_eq!(w.focused_label(), at);
    assert!(w.bar().is_open());
    w.key("Esc");
    assert!(!w.bar().is_open());
    assert!(w.calls().is_empty());
    // Cancel (Esc) as a button.
    w.select(&["a-notes.txt", "c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    let (_, _, cancel) = w.bar().rects().expect("the bar is open");
    w.click(cancel);
    assert!(!w.bar().is_open());
    assert!(w.calls().is_empty());
}

#[test]
fn bar_closes_on_navigation() {
    let mut w = in_root("ops_bar_nav");
    w.script(zip_on_network);
    w.select(&["c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    assert!(w.bar().is_open());
    let sub = w.path("projects");
    w.f.enumr.queue(&sub, fakes::listing(&[("x.txt", false, 1, day(1))]));
    w.h.send(Box::new(Op::Go(Location::Fs(sub))));
    w.pump();
    assert!(!w.bar().is_open());
    assert!(w.calls().is_empty());
    // A selection change closes it too.
    let mut w = in_root("ops_bar_sel");
    w.script(zip_on_network);
    w.select(&["c-photos-raw.zip"]);
    w.key("Del");
    w.pump();
    assert!(w.bar().is_open());
    w.select(&["a-notes.txt"]);
    assert!(!w.bar().is_open());
    assert!(w.calls().is_empty());
    // A Del whose folder changed before its answer is dropped.
    let mut w = in_root("ops_bar_pending_nav");
    w.script(|_| Recyclability::Yes);
    w.select(&["a-notes.txt"]);
    w.key("Del");
    let sub = w.path("projects");
    w.f.enumr.queue(&sub, fakes::listing(&[("x.txt", false, 1, day(1))]));
    w.h.send(Box::new(Op::Go(Location::Fs(sub))));
    w.pump();
    assert!(w.calls().is_empty(), "{:?}", w.calls());
    assert!(!w.bar().is_open() && !w.bar().is_pending());
}

#[test]
fn shift_del_sends_permanent_at_once() {
    let mut w = in_root("ops_shift_del");
    let jobs = w.f.bg.runs().len();
    w.select(&["b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    assert_eq!(w.calls(), [OpsCall::DeletePermanent(w.refs(&["b-report-final-v2.pdf"]))]);
    assert_eq!(w.f.bg.runs().len(), jobs, "no classification for a permanent delete");
    assert!(!w.bar().is_open());
}

/// A small deterministic generator for the property test.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn recycle_never_permanent() {
    const ANSWERS: [Recyclability; 6] = [
        Recyclability::Yes,
        Recyclability::No(NoBin::Network),
        Recyclability::No(NoBin::Removable),
        Recyclability::No(NoBin::OverQuota),
        Recyclability::No(NoBin::Disabled),
        Recyclability::No(NoBin::Unknown),
    ];
    const NAMES: [&str; 4] = ["a-notes.txt", "b-report-final-v2.pdf", "c-photos-raw.zip", "d-setup-tool.exe"];
    let mut g = Lcg(0x5EED);
    let mut w = in_root("ops_never_permanent");
    for case in 0..48 {
        // A random answer per name, a random non-empty selection, and the bar's way out.
        let answers: Vec<Recyclability> = NAMES.iter().map(|_| ANSWERS[(g.next() % 6) as usize]).collect();
        let picked: Vec<&'static str> = NAMES.iter().copied().filter(|_| g.next().is_multiple_of(2)).collect();
        let picked = if picked.is_empty() { vec![NAMES[case % 4]] } else { picked };
        let table: Vec<(String, Recyclability)> = NAMES.iter().map(|n| n.to_string()).zip(answers.clone()).collect();
        let facts: ops_ui::FactsFn = Arc::new(move |items: &[(WidePath, u64)]| {
            items
                .iter()
                .map(|(p, _)| {
                    let s = p.to_string_lossy();
                    let a = table.iter().find(|(n, _)| s.ends_with(n.as_str())).map_or(Recyclability::Yes, |x| x.1);
                    (p.clone(), a)
                })
                .collect()
        });
        w.shared.ops.borrow_mut().set_facts(facts);
        let before = w.calls().len();
        w.select(&picked);
        w.key("Del");
        w.pump();
        let new: Vec<OpsCall> = w.calls()[before..].to_vec();
        let no: Vec<&str> = picked
            .iter()
            .copied()
            .filter(|n| answers[NAMES.iter().position(|x| x == n).unwrap()] != Recyclability::Yes)
            .collect();
        assert!(!new.iter().any(|c| matches!(c, OpsCall::DeletePermanent(_))), "case {case}: {new:?}");
        if no.is_empty() {
            assert_eq!(new, [OpsCall::Recycle(w.refs(&picked))], "case {case}");
            continue;
        }
        assert!(new.is_empty() && w.bar().is_open(), "case {case}: the bar asks: {new:?}");
        // Del again, plain and held, is not the bar's Enter.
        w.key("Del");
        w.held_del();
        assert_eq!(w.calls().len(), before, "case {case}: Del on the open bar sends nothing: {:?}", w.calls());
        assert!(w.bar().is_open(), "case {case}: the bar stays open");
        if g.next().is_multiple_of(2) {
            w.key("Esc");
            assert_eq!(w.calls().len(), before, "case {case}: Esc sends nothing");
        } else {
            w.key("Enter");
            let sent: Vec<OpsCall> = w.calls()[before..].to_vec();
            assert_eq!(
                sent.last(),
                Some(&OpsCall::DeletePermanent(w.refs(&no))),
                "case {case}: only the items that cannot go"
            );
        }
        assert!(!w.bar().is_open());
    }
}

#[test]
fn intercepted_delete_uses_the_del_flow() {
    // FB-MENU hands an intercepted Delete of the system menu to delete_flow: the same classification and bar as Del.
    let mut w = in_root("ops_intercepted");
    w.script(zip_on_network);
    let items = w.paths(&["a-notes.txt", "c-photos-raw.zip"]);
    w.h.send(Box::new(Op::Del(items)));
    assert!(w.calls().is_empty() && w.bar().is_pending());
    w.pump();
    assert!(w.bar().is_open(), "never permanent without the bar");
    w.key("Esc");
    assert!(w.calls().is_empty());
    w.h.send(Box::new(Op::Del(w.paths(&["a-notes.txt"]))));
    w.pump();
    assert_eq!(w.calls(), [OpsCall::Recycle(w.refs(&["a-notes.txt"]))]);
    // The context menu's entries are the window's commands (the same flows as the keys).
    let menu = ops_ui::f3_items(w.fs(), &w.paths(&["a-notes.txt"]));
    assert_eq!(menu.len(), 3, "Rename, Delete, Delete permanently");
    let permanent = oled_ui::CommandId(files_engine::keys::Cmd::DeletePermanent as u32);
    match menu.last() {
        Some(oled_ui::MenuItem::Custom { id, label, chord, enabled, note, danger, .. }) => {
            assert_eq!((*id, &**label, chord.as_deref()), (permanent, "Delete permanently", Some("Shift+Del")));
            assert!(*enabled && *danger, "the danger item");
            assert_eq!(note.as_deref(), Some("Can't be undone"));
        }
        other => panic!("Delete permanently: {other:?}"),
    }
    assert_eq!(ops_ui::f3_items(w.fs(), &w.paths(&["a-notes.txt", "c-photos-raw.zip"])).len(), 2);
    assert!(ops_ui::f3_items(w.fs(), &[]).is_empty());
}

#[test]
fn focus_moves_after_delete() {
    let mut w = in_root("ops_focus_after");
    w.select(&["b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    let removed = |p: WidePath| Change { kind: ChangeKind::Deleted, from: Some(p), to: None };
    w.emit(OpEvt::ItemsDone { op: OpId(1), changes: vec![removed(w.path("b-report-final-v2.pdf"))] });
    w.pump();
    assert_eq!(w.focused_label().as_deref(), Some("c-photos-raw.zip"), "the next row");
    // At the end of the list: the row before.
    w.select(&["d-setup-tool.exe"]);
    w.key("Shift+Del");
    w.emit(OpEvt::ItemsDone { op: OpId(2), changes: vec![removed(w.path("d-setup-tool.exe"))] });
    w.pump();
    assert_eq!(w.focused_label().as_deref(), Some("c-photos-raw.zip"), "the previous row at the end");
}

// ---- F3.4: the chip, the line and the flyout ---------------------------------------------------------------------

fn row(op: u32, kind: OpKind, count: u64, state: OpState) -> OpRow {
    let mut r = OpRow::new(OpId(op), kind, 1);
    r.count = count;
    r.state = state;
    r
}

#[test]
fn chip_texts_by_kind_and_count() {
    let fmt = Fmt::fixed_en();
    let en = UiLang::EN;
    let chip = |m: &OpsModel, top: Option<OpId>| ops_ui::chip_text(m, top, 0, en, &fmt);
    let text = |m: &OpsModel| chip(m, None).map(|c| c.text.to_string());
    let one = |r: OpRow| {
        let mut m = OpsModel::new();
        m.insert(r);
        m
    };
    assert_eq!(text(&OpsModel::new()), None, "nothing runs: no chip");
    let m = one(row(1, OpKind::Recycle, 1, OpState::Running));
    assert_eq!(text(&m).as_deref(), Some(format!("Moving 1 item to the Recycle Bin{DOT}0 %").as_str()));
    let m = one(row(1, OpKind::Recycle, 1_204, OpState::Running));
    assert_eq!(text(&m).as_deref(), Some(format!("Moving 1,204 items to the Recycle Bin{DOT}0 %").as_str()));
    let mut r = row(1, OpKind::DeletePermanent, 14, OpState::Running);
    (r.done, r.total) = (9, 14);
    let m = one(r);
    assert_eq!(text(&m).as_deref(), Some(format!("Deleting 14 items{DOT}64 %").as_str()));
    let mut r = row(1, OpKind::Rename, 1, OpState::Running);
    r.items = vec![wp(r"C:\made-up\notes.txt")];
    r.name = Some("draft.txt".into());
    assert_eq!(text(&one(r)).as_deref(), Some(format!("Renaming notes.txt{DOT}0 %").as_str()));
    let mut r = row(1, OpKind::NewFolder, 1, OpState::Running);
    r.name = Some("plans".into());
    assert_eq!(text(&one(r)).as_deref(), Some(format!("Creating plans{DOT}0 %").as_str()));
    // Several: the count and the aggregate.
    let mut m = OpsModel::new();
    let mut a = row(1, OpKind::Recycle, 3, OpState::Running);
    (a.done, a.total) = (1, 4);
    let mut b = row(2, OpKind::DeletePermanent, 9, OpState::Running);
    (b.done, b.total) = (0, 4);
    m.insert(a);
    m.insert(b);
    assert_eq!(text(&m).as_deref(), Some(format!("2 operations{DOT}12 %").as_str()));
    // Finished: what it did, failures in their own part, Undo only while the stack's top undoes it.
    let mut r = row(5, OpKind::DeletePermanent, 14, OpState::Done);
    r.finished_at = Some(0);
    assert_eq!(text(&one(r.clone())).as_deref(), Some("Deleted 14 items"));
    r.failed = 2;
    let c = chip(&one(r), None).unwrap();
    assert_eq!(c.failed.as_deref(), Some("2 items couldn't be changed"));
    let mut r = row(6, OpKind::Recycle, 3, OpState::Done);
    r.undoable = true;
    assert_eq!(chip(&one(r.clone()), None).unwrap().undo, None);
    let c = chip(&one(r), Some(OpId(6))).unwrap();
    assert_eq!((c.text.as_ref(), c.undo.as_deref()), ("Moved 3 items to the Recycle Bin", Some("Undo")));
    let mut r = row(7, OpKind::Rename, 1, OpState::Done);
    r.name = Some("b.txt".into());
    assert_eq!(text(&one(r)).as_deref(), Some("Renamed to b.txt"));
    let mut r = row(8, OpKind::NewFolder, 1, OpState::Done);
    r.name = Some("x".into());
    assert_eq!(text(&one(r)).as_deref(), Some("Created x"));
    assert_eq!(
        text(&one(row(9, OpKind::Recycle, 1, OpState::Done))).as_deref(),
        Some("Moved 1 item to the Recycle Bin")
    );
    // Italian.
    let mut r = row(1, OpKind::DeletePermanent, 2, OpState::Running);
    (r.done, r.total) = (1, 2);
    let it = ops_ui::chip_text(&one(r), None, 0, UiLang { italian: true, pseudo: false }, &Fmt::fixed_it()).unwrap();
    assert_eq!(it.text, format!("Eliminazione di 2 elementi{DOT}50 %"));
}

#[test]
fn chip_hidden_6s_after_finish_and_no_timer_after() {
    let mut w = in_root("ops_chip_hold");
    w.h.advance(1_000);
    w.select(&["a-notes.txt", "b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    w.emit(started(1, OpKind::DeletePermanent, 2));
    w.emit(progress(1, 1, 2));
    w.pump();
    assert_eq!(w.chip().as_deref(), Some(format!("Deleting 2 items{DOT}50 %").as_str()));
    assert!(w.has_button(&format!("Deleting 2 items{DOT}50 %")), "the chip is a named button: {}", w.dump());
    assert!(ops_ui::flyout_of(w.fs()).line_on(), "the line runs");
    w.emit(finished(1, 2, 0, None));
    w.pump();
    assert_eq!(w.chip().as_deref(), Some("Deleted 2 items"));
    assert!(!ops_ui::flyout_of(w.fs()).line_on());
    assert!(!w.h.is_idle(), "the hold's timer runs");
    w.h.advance(5_900);
    w.h.render();
    assert_eq!(w.chip().as_deref(), Some("Deleted 2 items"), "still within the hold");
    w.h.advance(200);
    w.h.render();
    assert_eq!(w.chip(), None, "gone after 6 s");
    assert!(w.h.is_idle(), "and no timer after it");
    assert!(w.shared.ops.borrow().rows().is_empty());
}

#[test]
fn needs_input_brings_forward() {
    let mut w = in_root("ops_needs_input");
    w.emit(started(3, OpKind::DeletePermanent, 5));
    w.emit(OpEvt::NeedsInput { op: OpId(3), on: true });
    w.pump();
    assert_eq!(w.chip().as_deref(), Some("Needs your input"));
    let (r, _) = ops_ui::flyout_of(w.fs()).chip().unwrap();
    w.click(r);
    assert!(w.calls().contains(&OpsCall::BringForward(OpId(3))), "{:?}", w.calls());
    assert!(!ops_ui::flyout_of(w.fs()).is_open(), "the dialog comes forward; the flyout stays shut");
    w.emit(OpEvt::NeedsInput { op: OpId(3), on: false });
    w.pump();
    assert_eq!(w.chip().as_deref(), Some(format!("Deleting 5 items{DOT}0 %").as_str()));
}

#[test]
fn progress_aggregates() {
    let mut w = in_root("ops_aggregate");
    w.emit(started(1, OpKind::DeletePermanent, 4));
    w.emit(started(2, OpKind::Recycle, 6));
    w.emit(progress(1, 50, 100));
    w.emit(progress(2, 150, 300));
    w.pump();
    assert_eq!(w.shared.ops.borrow().aggregate(), Some(0.5));
    assert_eq!(w.chip().as_deref(), Some(format!("2 operations{DOT}50 %").as_str()));
    assert!(ops_ui::flyout_of(w.fs()).line_on());
    assert!(w.dump().contains("ProgressIndicator"), "{}", w.dump());
    w.emit(progress(1, 100, 100));
    w.emit(progress(2, 300, 300));
    w.pump();
    assert_eq!(w.shared.ops.borrow().aggregate(), Some(1.0));
}

#[test]
fn flyout_rows_and_cancel() {
    let mut w = in_root("ops_flyout_cancel");
    w.select(&["a-notes.txt", "b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    w.emit(started(1, OpKind::DeletePermanent, 2));
    w.emit(progress(1, 1, 2));
    w.pump();
    w.key("Ctrl+J");
    let fl = ops_ui::flyout_of(w.fs());
    assert!(fl.is_open() && fl.panel().is_some());
    let folder = w.dir.file_name().map(String::from_utf16_lossy).unwrap();
    assert_eq!(fl.row_texts(), ["Deleting 2 items".to_string(), folder, "50 %".into()]);
    assert!(w.dump().contains("ListItem \"Deleting 2 items\""), "every row is a node: {}", w.dump());
    w.h.assert_named();
    let cancel = fl.cancel_rect(0).expect("the running row's cancel button");
    w.click(cancel);
    assert!(w.calls().contains(&OpsCall::Cancel(OpId(1))), "{:?}", w.calls());
    // A finished operation's row offers Undo while the stack's top undoes it.
    w.emit(finished(1, 2, 0, None));
    w.pump();
    let fl = ops_ui::flyout_of(w.fs());
    assert_eq!(fl.row_texts(), ["Deleted 2 items"]);
    // Ctrl+J closes it again.
    w.key("Ctrl+J");
    assert!(!ops_ui::flyout_of(w.fs()).is_open());
}

#[test]
fn failures_stay_until_opened() {
    let mut w = in_root("ops_failures");
    w.select(&["a-notes.txt", "b-report-final-v2.pdf"]);
    w.key("Shift+Del");
    w.emit(started(1, OpKind::DeletePermanent, 2));
    let denied = 0x8007_0005_u32 as i32;
    w.emit(OpEvt::ItemFailed { op: OpId(1), item: w.path("a-notes.txt"), hr: denied });
    w.emit(OpEvt::ItemFailed { op: OpId(1), item: w.path("b-report-final-v2.pdf"), hr: denied });
    w.emit(finished(1, 2, 2, None));
    w.pump();
    let chip = w.chip().unwrap();
    assert!(chip.ends_with(&format!("{DOT}2 items couldn't be changed")), "{chip}");
    w.h.advance(7_000);
    w.h.render();
    assert_eq!(w.chip(), None, "the chip's hold ended");
    assert_eq!(w.shared.ops.borrow().rows().len(), 1, "the failure row stays until opened");
    assert!(w.h.is_idle(), "and holds no timer");
    w.key("Ctrl+J");
    let fl = ops_ui::flyout_of(w.fs());
    assert_eq!(fl.row_texts(), [format!("2 items couldn't be changed{DOT}Details \u{203a}")]);
    let details = fl.link_rect(0).expect("the Details link");
    w.click(details);
    let texts = ops_ui::flyout_of(w.fs()).row_texts();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(
        texts[1].starts_with(&format!("a-notes.txt{DOT}")) && texts[2].starts_with("b-report-final-v2.pdf"),
        "{texts:?}"
    );
    w.key("Esc");
    assert!(w.shared.ops.borrow().rows().is_empty(), "opened and closed: the row goes");
}

#[test]
fn flyout_esc_closes() {
    let mut w = in_root("ops_flyout_esc");
    w.emit(started(4, OpKind::Recycle, 3));
    w.pump();
    w.key("Ctrl+J");
    assert!(ops_ui::flyout_of(w.fs()).is_open());
    // While open the flyout takes the keys: Down does not move the list.
    let at = w.focused_label();
    w.key("Down");
    assert_eq!(w.focused_label(), at);
    w.key("Esc");
    assert!(!ops_ui::flyout_of(w.fs()).is_open());
    // A press outside closes it as well.
    w.key("Ctrl+J");
    assert!(ops_ui::flyout_of(w.fs()).is_open());
    w.h.click(PointL { x: 40, y: 120 });
    w.h.render();
    assert!(!ops_ui::flyout_of(w.fs()).is_open());
    // Nothing to show: Ctrl+J opens nothing.
    let mut w = in_root("ops_flyout_empty");
    w.key("Ctrl+J");
    assert!(!ops_ui::flyout_of(w.fs()).is_open());
}

#[test]
fn is_idle_when_nothing_runs() {
    let mut w = in_root("ops_idle");
    w.h.advance(1_000);
    w.h.render();
    assert!(w.h.is_idle());
    assert_eq!(w.chip(), None);
    assert!(!ops_ui::flyout_of(w.fs()).line_on());
    assert!(!w.dump().contains("ProgressIndicator"));
}

#[test]
fn host_lost_stops_running_ops_and_keeps_their_items() {
    let mut w = in_root("ops_host_lost");
    w.select(&["a-notes.txt"]);
    w.key("Shift+Del");
    w.emit(started(1, OpKind::DeletePermanent, 1));
    w.pump();
    w.f.ops.emit(OpsEvent::HostLost);
    w.pump();
    assert!(w.has_label("The operations helper stopped."), "{}", w.dump());
    assert_eq!(w.shared.ops.borrow().row(OpId(1)).map(|r| r.state), Some(OpState::Stopped));
    assert_eq!(w.shared.ops.borrow().running(), 0);
    assert_eq!(w.chip(), None, "a stopped operation claims nothing");
    w.h.advance(6_100);
    w.h.render();
    assert!(w.shared.ops.borrow().row(OpId(1)).is_none(), "the stopped row went after its hold");
    assert_eq!(w.shared.ops.borrow().items_of(OpId(1)), w.paths(&["a-notes.txt"]), "its items stay for a resume");
    // The next start's Pending offers the rest, through the gate with those items.
    w.emit(OpEvt::Pending { of: OpId(1), kind: OpKind::DeletePermanent, done: 0, total: 1, dest: None });
    w.pump();
    w.h.send(Box::new(Op::Act(NoticeAct::Rest(OpId(1)))));
    assert_eq!(w.calls().last(), Some(&OpsCall::ResumeRest(OpId(1))));
}

// ---- F3.5: the operations window ---------------------------------------------------------------------------------

#[derive(Default)]
struct RecordOps {
    opened: Vec<&'static str>,
    shown: Vec<(WindowRef, Show)>,
}

impl LoopOps for RecordOps {
    fn open(&mut self, spec: WindowSpec, screen: Box<dyn Screen>) -> Option<WindowRef> {
        drop(screen);
        self.opened.push(spec.class);
        Some(WindowRef(self.opened.len() as u32 + 20))
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
}

fn closed(w: &Win) -> bool {
    w.h.last_outputs().iter().any(|o| o == "CloseWindow")
}

#[test]
fn last_close_with_running_op_opens_ops_window() {
    let mut w = in_root("ops_last_close");
    w.emit(started(5, OpKind::DeletePermanent, 20_000));
    w.emit(progress(5, 2_000, 20_000));
    w.pump();
    w.h.send(Box::new(Op::Event(Event::CloseRequested)));
    assert!(!closed(&w), "the window waits for the operations window");
    assert_eq!(*w.shared.loop_requests.borrow(), [LoopReq::Ops]);
    assert_eq!(w.shared.ops.borrow().close_after(), Some(1));
    // A second close while it waits asks nothing more.
    w.h.send(Box::new(Op::Event(Event::CloseRequested)));
    assert_eq!(w.shared.loop_requests.borrow().len(), 1);
    // The loop callback opens it with ops_spec; the waiting window then closes.
    let mut lo = RecordOps::default();
    window::run_loop_requests(&w.shared, &mut lo);
    assert_eq!(lo.opened, [files_engine::names::OPS_WINDOW_CLASS]);
    assert_eq!(lo.shown.len(), 1);
    assert!(w.shared.windows.borrow().iter().any(|r| r.kind == WinKind::Ops));
    w.pump();
    assert!(closed(&w), "{:?}", w.h.last_outputs());
    assert!(w.shared.main_windows().is_empty(), "the main window's record went with it");
    assert_eq!(w.shared.ops.borrow().close_after(), None);
    // The operations window shows the flyout's rows.
    let ops_win = w.shared.windows.borrow().iter().find(|r| r.kind == WinKind::Ops).map(|r| r.win).unwrap();
    let mut h =
        Harness::new(OpsScreen::new(Rc::clone(&w.shared), ops_win), SizeL { w: 360, h: 200 }, 96, Tokens::oled());
    h.render();
    assert_eq!(h.screen().row_texts()[0], "Deleting 20,000 items");
    h.assert_named();
    let spec = opswin::ops_spec(&w.shared);
    assert_eq!((spec.size.w, spec.class), (360, files_engine::names::OPS_WINDOW_CLASS));
    assert!((160..=480).contains(&spec.size.h));
}

#[test]
fn closes_6s_after_finish() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    // No main window: the operations window is the primary and drains the engines.
    f.hub.detach(1);
    let _c2 = f.attach_window(2);
    shared.windows.borrow_mut().push(WinRec { win: 2, kind: WinKind::Ops, wref: None, hwnd: None, location: None });
    let mut r = OpRow::new(OpId(4), OpKind::DeletePermanent, 1);
    (r.count, r.state) = (3, OpState::Running);
    shared.ops.borrow_mut().insert(r);
    let mut h = Harness::new(OpsScreen::new(Rc::clone(&shared), 2), SizeL { w: 360, h: 200 }, 96, Tokens::oled());
    h.render();
    assert_eq!(h.screen().row_texts()[0], "Deleting 3 items");
    f.ops.emit(OpsEvent::Evt(finished(4, 3, 0, None)));
    f.pump(&mut h, 2);
    h.render();
    assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"));
    assert_eq!(h.screen().row_texts()[0], "Deleted 3 items");
    h.advance(5_900);
    assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"), "still within the 6 s");
    h.advance(200);
    assert!(h.last_outputs().iter().any(|o| o == "CloseWindow"), "{:?}", h.last_outputs());
    assert!(shared.windows.borrow().is_empty(), "its record went");
}

#[test]
fn second_window_close_does_not() {
    let mut w = in_root("ops_second_close");
    w.shared.windows.borrow_mut().push(main_rec(2, &w.dir));
    w.emit(started(5, OpKind::DeletePermanent, 3));
    w.pump();
    w.h.send(Box::new(Op::Event(Event::CloseRequested)));
    assert!(closed(&w), "another main window stays: this one closes");
    assert!(w.shared.loop_requests.borrow().is_empty());
    // The last window with nothing running does not wait for an operations window: it hides and lingers (F4.12).
    let mut w = in_root("ops_last_idle_close");
    w.h.send(Box::new(Op::Event(Event::CloseRequested)));
    assert!(!closed(&w));
    assert_eq!(*w.shared.loop_requests.borrow(), [LoopReq::Show { win: 1, how: Show::Hide }]);
}

// ---- F3.6: reports of an earlier session -------------------------------------------------------------------------

#[test]
fn done_while_away_notice_then_ack() {
    let mut w = in_root("ops_away");
    w.emit(OpEvt::DoneWhileAway { of: OpId(77), kind: OpKind::DeletePermanent, items: 20_000, failed: 0, dest: None });
    w.pump();
    let app = UiLang::EN.kit(oled_ui::strings::k::KIT_APP_FILES);
    assert!(w.has_label(&format!("Deleted 20,000 items while {app} was closed")), "{}", w.dump());
    assert!(w.calls().contains(&OpsCall::AckDone(OpId(77))), "acknowledged once shown: {:?}", w.calls());
    w.emit(OpEvt::DoneWhileAway { of: OpId(78), kind: OpKind::Recycle, items: 3, failed: 1, dest: None });
    w.pump();
    assert!(
        w.has_label(&format!("Moved 3 items to the Recycle Bin while {app} was closed{DOT}1 item couldn't be changed")),
        "{}",
        w.dump()
    );
    assert!(w.calls().contains(&OpsCall::AckDone(OpId(78))));
}

#[test]
fn pending_offers_the_rest_by_kind() {
    let mut w = in_root("ops_pending");
    w.emit(OpEvt::Pending { of: OpId(70), kind: OpKind::Recycle, done: 5, total: 9, dest: None });
    w.pump();
    assert!(w.has_label("Stopped after 5 of 9 items."), "{}", w.dump());
    assert!(w.has_button("Recycle the rest"), "{}", w.dump());
    w.emit(OpEvt::Pending { of: OpId(71), kind: OpKind::DeletePermanent, done: 1_000, total: 20_000, dest: None });
    w.pump();
    assert!(w.has_label("Stopped after 1,000 of 20,000 items."), "{}", w.dump());
    assert!(w.has_button("Delete the rest"), "{}", w.dump());
    // Any other kind (here an interrupted undo) is not a delete: the notice has no button.
    w.emit(OpEvt::Pending { of: OpId(72), kind: OpKind::Undo, done: 2, total: 3, dest: None });
    w.pump();
    assert!(w.has_label("Stopped after 2 of 3 items."), "{}", w.dump());
    assert!(!w.has_button("Recycle the rest") && !w.has_button("Delete the rest"), "{}", w.dump());
    assert!(!w.calls().iter().any(|c| matches!(c, OpsCall::AckDone(_))), "a pending journal is kept");
}

#[test]
fn resume_goes_through_the_gate() {
    // An operation of an earlier process: no recorded items, so the gate refuses it.
    let mut w = in_root("ops_resume_gate");
    w.emit(OpEvt::Pending { of: OpId(70), kind: OpKind::DeletePermanent, done: 5, total: 9, dest: None });
    w.pump();
    w.h.send(Box::new(Op::Act(NoticeAct::Rest(OpId(70)))));
    w.h.render();
    assert!(w.has_label("Changes are off in this preview build"), "{}", w.dump());
    assert!(!w.calls().iter().any(|c| matches!(c, OpsCall::ResumeRest(_))), "{:?}", w.calls());
    // This process's own operation inside the root: its items pass the gate, and the rest is resumed.
    w.select(&["a-notes.txt"]);
    w.key("Shift+Del");
    w.emit(OpEvt::Pending { of: OpId(1), kind: OpKind::DeletePermanent, done: 0, total: 1, dest: None });
    w.pump();
    assert!(w.has_button("Delete the rest"), "{}", w.dump());
    w.h.send(Box::new(Op::Act(NoticeAct::Rest(OpId(1)))));
    w.h.render();
    assert_eq!(w.calls().last(), Some(&OpsCall::ResumeRest(OpId(1))));
    let m = w.shared.ops.borrow();
    let r = m.row(OpId(2)).expect("the resume is an operation of its own");
    assert_eq!(r.kind, OpKind::DeletePermanent, "shown as the kind it resumes");
}
