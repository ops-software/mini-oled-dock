//! Rename in place and new folder (P3-files-app F3.7, F3.8): plain Harness tests over the fakes. The folder is a real
//! folder under the test root (the gate lets nothing else through); the operations helper is the recorder.

mod common;

use common::temp::TempRoot;
use files_engine::fakes::{self, Fakes, OpsCall};
use files_engine::keys::Cmd;
use files_engine::listing::WidePath;
use files_engine::model::ViewOpts;
use files_engine::rename;
use files_engine::tr::{k, t};
use files_engine::window::{FilesScreen, STARTUP_TAB};
use oled_trust::frame::files::{Location, OpId};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::{Event, EventCx, LayoutCx, PaintCx, Reply, Screen, SizeL, SizeP, Tokens, UiLang};

type Job = Box<dyn FnOnce(&mut FilesScreen, &mut EventCx) -> Reply + Send>;

/// FilesScreen inside a screen that runs the closure a test sends (the calls need an EventCx).
struct Runner {
    s: FilesScreen,
}

impl Screen for Runner {
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
        match msg.downcast::<Job>() {
            Ok(job) => (*job)(&mut self.s, cx),
            Err(m) => self.s.data(cx, m),
        }
    }
    fn commands(&self) -> &oled_ui::CommandTable {
        self.s.commands()
    }
}

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

const FILES: [&str; 3] = ["notes.txt", "report-final-v2.pdf", ".settings"];

struct Win {
    f: Fakes,
    h: Harness<Runner>,
    root: TempRoot,
}

impl Win {
    fn at(&self, name: &str) -> WidePath {
        wp(&self.root.path().join(name).to_string_lossy())
    }

    fn run(&mut self, job: impl FnOnce(&mut FilesScreen, &mut EventCx) -> Reply + Send + 'static) {
        let job: Job = Box::new(job);
        self.h.send(Box::new(job));
        self.h.render();
    }

    fn begin(&mut self, name: &str) {
        let p = self.at(name);
        self.run(move |s, cx| rename::begin_path(s, cx, &p));
    }

    fn renaming(&self) -> bool {
        self.h.screen().s.tabs().active().table.is_renaming()
    }

    fn field(&mut self) -> (String, std::ops::Range<usize>) {
        let f = self.h.screen_mut().s.tabs_mut().active_mut().table.rename_field_mut().expect("a field is open");
        (f.text().to_string(), f.selection())
    }

    /// The line under the open field (the dump shows only that the field is invalid, not why).
    fn line(&mut self) -> Option<String> {
        let tab = self.h.screen_mut().s.tabs_mut().active_mut();
        tab.table.rename_field_mut().and_then(|f| f.error().map(|t| t.to_string()))
    }

    fn dump(&self) -> String {
        self.h.a11y_dump()
    }

    fn calls(&self) -> Vec<OpsCall> {
        self.f.ops.calls()
    }
}

fn win_over(tag: &str, entries: &[(&str, bool)], at_home: bool) -> Win {
    let root = TempRoot::new(tag);
    let rows: Vec<(&str, bool, u64, i64)> = entries
        .iter()
        .map(|&(n, d)| {
            if d {
                root.dir(n);
            } else {
                root.file(n, b"x");
            }
            (n, d, 10, fakes::FT_2026_11_21)
        })
        .collect();
    win_from(root, fakes::listing(&rows), at_home)
}

fn win_from(root: TempRoot, batch: files_engine::listing::types::Batch, at_home: bool) -> Win {
    let dir = wp(&root.path().to_string_lossy());
    let f = Fakes::new();
    f.enumr.queue(&dir, batch);
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let at = if at_home { Location::Home } else { Location::Fs(dir) };
    let s = FilesScreen::new(shared, 1, at, None, if at_home { STARTUP_TAB } else { tab });
    let mut h = Harness::new(Runner { s }, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, h, root }
}

fn win() -> Win {
    let entries: Vec<(&str, bool)> = FILES.iter().map(|n| (*n, false)).chain([("invoices", true)]).collect();
    win_over("ui_rename", &entries, false)
}

// ---- F3.7

#[test]
fn stem_preselected() {
    let mut w = win();
    w.begin("report-final-v2.pdf");
    assert!(w.renaming());
    assert_eq!(w.field(), ("report-final-v2.pdf".to_string(), 0..15));
    w.h.key("Esc");
    assert!(!w.renaming());
    // A folder, and a name that starts with the only dot: the whole name.
    w.begin("invoices");
    assert_eq!(w.field(), ("invoices".to_string(), 0..8));
    w.h.key("Esc");
    w.begin(".settings");
    assert_eq!(w.field(), (".settings".to_string(), 0..9));
}

#[test]
fn f2_cycles() {
    let mut w = win();
    w.begin("report-final-v2.pdf");
    for want in [0..19, 16..19, 0..15, 0..19] {
        w.h.key("F2");
        assert_eq!(w.field().1, want);
    }
    w.h.key("Esc");
    w.begin("invoices");
    w.h.key("F2");
    assert_eq!(w.field().1, 0..8, "no extension: the whole name again");
}

#[test]
fn forbidden_char_refused_with_line() {
    let mut w = win();
    w.begin("notes.txt");
    w.h.type_text("a:b");
    assert_eq!(w.field().0, "ab.txt", "the colon is not typed");
    assert!(w.dump().contains("invalid"), "the field shows its error rim: {}", w.dump());
    assert_eq!(w.line().as_deref(), Some("A name can't contain \\ / : * ? \" < > |"), "and says why");
    w.h.advance(3_100);
    w.h.render();
    assert!(!w.dump().contains("invalid"), "the line goes after 3 s");
    assert!(w.calls().is_empty());
}

#[test]
fn reserved_names_refused() {
    let mut w = win();
    for typed in ["nul", "Com1", "LPT9"] {
        w.begin("notes.txt");
        w.h.type_text(typed).key("Enter");
        assert!(w.renaming(), "{typed} keeps the field open");
        assert!(w.dump().contains("invalid"), "{typed}: the rim");
        assert_eq!(w.line(), Some(format!("{typed}.txt is a name Windows reserves")), "{typed}: the line");
        assert_eq!(w.field().0, format!("{typed}.txt"));
        w.h.key("Esc");
    }
    assert!(w.calls().is_empty());
    // COM10 and CONSOLE are fine names.
    w.begin("notes.txt");
    w.h.type_text("console").key("Enter");
    assert_eq!(w.calls().len(), 1);
}

#[test]
fn clash_keeps_field_open() {
    let mut w = win();
    w.begin("report-final-v2.pdf");
    w.h.key("F2").type_text("NOTES.TXT").key("Enter");
    assert!(w.renaming());
    assert!(w.dump().contains("invalid"), "the rim: {}", w.dump());
    assert_eq!(w.line().as_deref(), Some("There's already an item named NOTES.TXT"), "the line");
    assert!(w.calls().is_empty());
    w.h.key("Esc");
    assert!(!w.renaming());
    assert!(w.calls().is_empty());
}

#[test]
fn trailing_dots_trimmed() {
    let mut w = win();
    w.begin("notes.txt");
    w.h.key("F2").type_text("memo.txt. . ").key("Enter");
    assert_eq!(w.calls(), [OpsCall::Rename(w.at("notes.txt"), "memo.txt".into())]);
}

#[test]
fn unchanged_is_cancel() {
    let mut w = win();
    w.begin("notes.txt");
    w.h.key("Enter");
    assert!(!w.renaming());
    w.begin("notes.txt");
    w.h.key("F2").type_text("  ...").key("Enter");
    assert!(!w.renaming(), "a name of spaces and dots is empty");
    assert!(w.calls().is_empty());
}

#[test]
fn commit_sends_rename_through_gate() {
    let mut w = win();
    w.begin("report-final-v2.pdf");
    w.h.type_text("report-final-v3").key("Enter");
    assert!(!w.renaming());
    assert_eq!(w.calls(), [OpsCall::Rename(w.at("report-final-v2.pdf"), "report-final-v3.pdf".into())]);
    // A folder outside the test roots: the gate refuses, nothing is sent, the window says so.
    // (the neighbour stays closed after a Tab: see tab_after_refused_request_keeps_neighbour_closed)
    let f = Fakes::new();
    let outside = wp(r"C:\mod-files-made-up\projects");
    f.enumr.queue(&outside, fakes::listing(&[("a.txt", false, 1, fakes::FT_2026_11_21)]));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let s = FilesScreen::new(shared, 1, Location::Fs(outside), None, tab);
    let mut h = Harness::new(Runner { s }, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    h.key("Down").key("F2").type_text("b").key("Enter");
    assert!(f.ops.calls().is_empty());
    assert!(h.a11y_dump().contains(&*t(k::FILES_OPS_PREVIEW_GATE, UiLang::EN)), "the gate's notice");
}

#[test]
fn tab_moves_to_next() {
    let mut w = win();
    w.begin("notes.txt");
    w.h.type_text("n2").key("Tab");
    assert_eq!(w.calls(), [OpsCall::Rename(w.at("notes.txt"), "n2.txt".into())]);
    assert!(w.renaming(), "the neighbour is open");
    assert_eq!(w.field().0, "report-final-v2.pdf");
    w.h.key("Esc");
    // A refused commit keeps its own field and does not begin the next.
    w.begin("notes.txt");
    w.h.type_text("aux").key("Tab");
    assert_eq!(w.calls().len(), 1);
    assert_eq!(w.field().0, "aux.txt", "the refused field stays");
}

#[test]
fn tab_after_refused_request_keeps_neighbour_closed() {
    let f = Fakes::new();
    let outside = wp(r"C:\mod-files-made-up\projects");
    f.enumr.queue(
        &outside,
        fakes::listing(&[("a.txt", false, 1, fakes::FT_2026_11_21), ("b.txt", false, 1, fakes::FT_2026_11_21)]),
    );
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let s = FilesScreen::new(shared, 1, Location::Fs(outside), None, tab);
    let mut h = Harness::new(Runner { s }, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    h.key("Down").key("F2").type_text("c").key("Tab");
    assert!(f.ops.calls().is_empty());
    let field = h.screen_mut().s.tabs_mut().active_mut().table.rename_field_mut().expect("the refused field is open");
    assert_eq!(field.text(), "c.txt", "its own row, not the neighbour");
}

#[test]
fn navigation_while_renaming_sends_nothing() {
    let mut w = win();
    w.begin("notes.txt");
    w.h.type_text("x");
    let other = TempRoot::new("ui_rename_other");
    let odir = wp(&other.path().to_string_lossy());
    let names = ["z1.txt", "z2.txt", "z3.txt", "z4.txt", "z5.txt", "z6.txt"];
    let rows: Vec<(&str, bool, u64, i64)> = names.iter().map(|n| (*n, false, 1, fakes::FT_2026_11_21)).collect();
    w.f.enumr.queue(&odir, fakes::listing(&rows));
    w.run(move |s, cx| {
        files_engine::window::navigate(s, cx, Location::Fs(odir), None);
        Reply::Relayout
    });
    w.f.pump(&mut w.h, 1);
    w.h.render();
    w.h.key("Enter");
    assert!(w.calls().is_empty(), "{:?}", w.calls());
}

#[test]
fn commit_without_its_field_sends_nothing() {
    let mut w = win();
    w.run(|s, _| {
        let t = s.tabs_mut().active_mut();
        t.table.model_mut().set_opts(ViewOpts { show_extensions: false, ..ViewOpts::default() });
        Reply::Relayout
    });
    w.begin("report-final-v2.pdf");
    w.h.type_text("x");
    w.h.key("Ctrl+T");
    w.h.key("Ctrl+PgDn");
    w.h.key("Enter");
    for c in w.calls() {
        assert!(matches!(&c, OpsCall::Rename(_, to) if to == "x.pdf"), "the extension stays: {c:?}");
    }
}

#[test]
fn lone_surrogate_name_unchanged_sends_nothing() {
    let root = TempRoot::new("ui_rename_sur");
    let mut b = fakes::listing(&[("a\u{FFFD}b.txt", false, 1, fakes::FT_2026_11_21)]);
    b.names[1..4].copy_from_slice(&[0xED, 0xA0, 0x80]);
    let mut w = win_from(root, b, false);
    w.h.key("Down").key("F2");
    assert!(w.renaming());
    w.h.key("Enter");
    assert!(w.calls().is_empty(), "{:?}", w.calls());
}

#[test]
fn menu_rename_begins() {
    let mut w = win();
    w.begin("nowhere.txt");
    assert!(!w.renaming(), "a path that is not a row of the folder");
    w.begin("report-final-v2.pdf");
    assert!(w.renaming());
    w.h.key("Esc");
    w.h.key("Down");
    w.run(|s, cx| rename::cmd(s, cx, Cmd::Rename));
    assert!(w.renaming(), "the Rename command opens the focused row");
}

#[test]
fn hidden_extension_comes_back() {
    let mut w = win();
    w.run(|s, _| {
        let t = s.tabs_mut().active_mut();
        t.table.model_mut().set_opts(ViewOpts { show_extensions: false, ..ViewOpts::default() });
        Reply::Relayout
    });
    w.begin("report-final-v2.pdf");
    assert_eq!(w.field(), ("report-final-v2".to_string(), 0..15));
    w.h.type_text("x").key("Enter");
    assert_eq!(w.calls(), [OpsCall::Rename(w.at("report-final-v2.pdf"), "x.pdf".into())]);
}

// ---- F3.8

#[test]
fn new_folder_picks_free_name() {
    let mut w = win_over("ui_rename_nf", &[("New folder", true), ("a.txt", false)], false);
    w.h.key("Ctrl+Shift+N");
    assert_eq!(w.calls(), [OpsCall::NewFolder(wp(&w.root.path().to_string_lossy()), "New folder (2)".into())]);
    let mut w = win_over("ui_rename_nf2", &[("a.txt", false)], false);
    w.h.key("Ctrl+Shift+N");
    assert_eq!(w.calls(), [OpsCall::NewFolder(wp(&w.root.path().to_string_lossy()), "New folder".into())]);
}

#[test]
fn created_row_enters_rename() {
    let mut w = win_over("ui_rename_cr", &[("a.txt", false)], false);
    w.h.key("Ctrl+Shift+N");
    // The host made it and the listing shows it: the change reaches the window as live::own_changes passes it.
    let dir = wp(&w.root.path().to_string_lossy());
    w.root.dir("New folder");
    w.f.enumr.queue(
        &dir,
        fakes::listing(&[("New folder", true, 0, fakes::FT_2026_11_21), ("a.txt", false, 1, fakes::FT_2026_11_21)]),
    );
    w.h.key("F5");
    w.f.pump(&mut w.h, 1);
    w.h.render();
    let p = w.at("New folder");
    w.run(move |s, cx| {
        assert!(rename::on_created(s, cx, OpId(1), &p));
        Reply::Redraw
    });
    assert!(w.renaming());
    assert_eq!(w.field(), ("New folder".to_string(), 0..10), "the whole name is selected");
}

#[test]
fn other_ops_created_rows_ignored() {
    let mut w = win_over("ui_rename_ig", &[("New folder", true)], false);
    w.h.key("Ctrl+Shift+N");
    let p = w.at("New folder");
    w.run(move |s, cx| {
        assert!(!rename::on_created(s, cx, OpId(9), &p));
        Reply::None
    });
    assert!(!w.renaming());
}

#[test]
fn disabled_on_home() {
    let mut w = win_over("ui_rename_home", &[], true);
    w.h.key("Ctrl+Shift+N");
    assert!(w.calls().is_empty());
    assert!(!w.renaming());
}
