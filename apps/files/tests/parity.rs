//! Navigation-row parity (P3-files-app F4.16, SPEC-files 5.4.1 within F1-F4): one test per row of keys::TABLE, each
//! pressing the row's chords through the kit's real dispatcher and asserting the row's action through the fakes (the
//! operations recorder, the shell-host recorder, the loop requests, the a11y dump) and its parity class; plus the
//! Verify rows, and a sample of chords outside the table that must reach no command. Plain tests: no window, no host,
//! no process; the folder is a test root under the temp folder, as the other window tests use.

mod common;

use std::rc::Rc;

use common::temp::TempRoot;
use files_engine::drives::{DriveInfo, DriveKind};
use files_engine::fakes::{self, Fakes, OpsCall};
use files_engine::format::Fmt;
use files_engine::keys::{Cmd, Parity, Slice, TABLE};
use files_engine::listing::WidePath;
use files_engine::menu;
use files_engine::ops_client::{OpsEvent, UndoRecord};
use files_engine::ops_ui;
use files_engine::options::FilesOptions;
use files_engine::pathline;
use files_engine::sidebar::{self, RowKind};
use files_engine::undo::UndoEntry;
use files_engine::window::{FilesScreen, LoopReq};
use oled_trust::frame::files::{ItemRef, Location, OpEvt, OpId, OpKind, ShellReq, UndoEntry as Entry, UndoKind, Verb};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::ListModel;
use oled_ui::{
    CaptionHit, Chord, CommandId, CommandTable, Event, EventCx, LayoutCx, PaintCx, PointP, Reply, Screen, SizeL, SizeP,
    Tokens, UiLang,
};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a test path parses")
}

fn day(n: i64) -> i64 {
    fakes::FT_2026_11_21 - n * 864_000_000_000
}

/// What a test asks the screen to do from inside its own event, where an EventCx exists: a command by its id, the
/// way a menu pick or a button reaches the window.
struct Run(Cmd);

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
        match msg.downcast::<Run>() {
            Ok(run) => {
                self.0.event(cx, &Event::Command(CommandId(run.0 as u32)));
                Reply::Relayout
            }
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

/// Folders first, as the list shows them: invoices, projects, notes.txt, report-final-v2.pdf, photos-raw.zip.
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
    h: Harness<Wrap>,
    root: TempRoot,
}

/// A window over a test root (the gate lets its requests through to the recorder), listed, with a second window's
/// worth of state absent: window 1, new tabs open on the current folder.
fn win() -> Win {
    let root = TempRoot::new("parity");
    for (name, dir, ..) in rows() {
        if dir {
            root.dir(name);
        } else {
            root.file(name, b"x");
        }
    }
    let dir = wp(&root.path().to_string_lossy());
    let f = Fakes::new();
    f.options.set(FilesOptions { new_tab: "current".into(), ..FilesOptions::default() });
    f.enumr.queue(&dir, fakes::listing(&rows()));
    f.enumr.queue(&wp(&format!(r"{}\invoices", dir.to_string_lossy())), fakes::listing(&[("a.txt", false, 1, day(1))]));
    let shared = f.shared(UiLang::EN);
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    assert_eq!(shared.new_win(), 1);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 900, h: 600 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, h, root }
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }

    fn dir(&self) -> WidePath {
        wp(&self.root.path().to_string_lossy())
    }

    fn key(&mut self, chord: &str) -> &mut Self {
        self.h.key(chord);
        self.h.render();
        self
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    fn run(&mut self, c: Cmd) {
        self.h.send(Box::new(Run(c)));
        self.h.render();
    }

    fn queue_root(&self) {
        self.f.enumr.queue(&self.dir(), fakes::listing(&rows()));
    }

    fn queue_invoices(&self) {
        let p = wp(&format!(r"{}\invoices", self.dir().to_string_lossy()));
        self.f.enumr.queue(&p, fakes::listing(&[("a.txt", false, 1, day(1))]));
    }

    fn here(&self) -> Location {
        self.fs().tabs().active().nav.location().clone()
    }

    fn here_name(&self) -> String {
        match self.here() {
            Location::Fs(p) => p.to_string_lossy().rsplit('\\').next().unwrap_or_default().to_string(),
            other => format!("{other:?}"),
        }
    }

    fn at_root(&self) -> bool {
        self.here() == Location::Fs(self.dir())
    }

    /// Into the folder "invoices" (the first row) and listed.
    fn enter_invoices(&mut self) {
        self.queue_invoices();
        self.key("Home").key("Enter");
        self.pump();
        assert_eq!(self.here_name(), "invoices");
    }

    fn selected(&self) -> usize {
        let t = self.fs().tabs().active();
        t.table.selection().count(ListModel::len(t.table.model()))
    }

    fn tabs(&self) -> usize {
        self.fs().tabs().len()
    }

    fn active(&self) -> usize {
        let id = self.fs().tabs().active().id;
        self.fs().tabs().iter().position(|t| t.id == id).expect("the active tab is in the list")
    }

    /// Opens tabs on the current folder until there are n.
    fn tabs_to(&mut self, n: usize) {
        while self.tabs() < n {
            self.queue_root();
            self.key("Ctrl+T");
            self.pump();
        }
        assert_eq!(self.tabs(), n);
    }

    fn calls(&self) -> Vec<OpsCall> {
        self.f.ops.calls()
    }

    fn execs(&self) -> Vec<(Verb, Vec<ItemRef>)> {
        self.f
            .shell
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                ShellReq::Exec { verb, items, .. } => Some((verb, items)),
                _ => None,
            })
            .collect()
    }

    fn item(&self, name: &str) -> ItemRef {
        ItemRef::Path(wp(&format!(r"{}\{name}", self.dir().to_string_lossy())))
    }

    /// The focus on notes.txt, the first file (Home, then the two folders).
    fn on_notes(&mut self) {
        self.key("Home").key("Down").key("Down");
    }

    fn has_label(&self, text: &str) -> bool {
        self.h.a11y_dump().lines().any(|l| l.trim_start() == format!("Label {text:?}"))
    }

    fn focused(&self) -> Option<oled_ui::node::NodeId> {
        self.h.focused()
    }
}

/// One test per table row: the row's parity class and chords are the table's, then the body runs once per chord
/// (once with "" for a row without a chord) on a fresh window.
macro_rules! row {
    ($name:ident, $cmd:expr, $parity:expr, [$($chord:expr),*], $body:expr) => {
        #[test]
        fn $name() {
            let chords: &[&str] = &[$($chord),*];
            let r = TABLE.iter().find(|r| r.cmd == $cmd).expect("the command has a row");
            assert_eq!(r.parity, $parity, "{:?}: its parity class", $cmd);
            assert!(r.slice <= Slice::F4, "{:?}: a row of F1-F4", $cmd);
            let table: Vec<Option<Chord>> = r.chords.iter().map(|c| c.chord()).collect();
            let named: Vec<Option<Chord>> = chords.iter().map(|c| Chord::parse(c)).collect();
            assert!(named.iter().all(Option::is_some), "{:?}: every chord parses", $cmd);
            assert_eq!(table, named, "{:?}: the chords of the table", $cmd);
            let run: &dyn Fn(&mut Win, &str) = &$body;
            if chords.is_empty() {
                run(&mut win(), "");
            }
            for chord in chords {
                run(&mut win(), chord);
            }
        }
    };
}

// ---- navigation ------------------------------------------------------------------------------------------------

row!(back, Cmd::Back, Parity::Same, ["Alt+Left", "Backspace"], |w: &mut Win, k: &str| {
    w.enter_invoices();
    w.queue_root();
    w.key(k);
    w.pump();
    assert!(w.at_root(), "{k}: back to the folder we came from");
});

row!(forward, Cmd::Forward, Parity::Same, ["Alt+Right"], |w: &mut Win, k: &str| {
    w.enter_invoices();
    w.queue_root();
    w.key("Alt+Left");
    w.pump();
    w.queue_invoices();
    w.key(k);
    w.pump();
    assert_eq!(w.here_name(), "invoices");
});

row!(up, Cmd::Up, Parity::Same, ["Alt+Up"], |w: &mut Win, k: &str| {
    w.enter_invoices();
    w.queue_root();
    w.key(k);
    w.pump();
    assert!(w.at_root());
});

row!(home, Cmd::Home, Parity::Same, ["Alt+Home"], |w: &mut Win, k: &str| {
    w.key(k);
    w.pump();
    assert_eq!(w.here(), Location::Home);
});

row!(refresh, Cmd::Refresh, Parity::Same, ["F5", "Ctrl+R"], |w: &mut Win, k: &str| {
    let before = w.f.enumr.jobs().len();
    w.queue_root();
    w.key(k);
    w.pump();
    assert!(w.f.enumr.jobs().len() > before, "{k}: the folder is listed again");
});

row!(edit_path, Cmd::EditPath, Parity::Same, ["Ctrl+L", "Alt+D"], |w: &mut Win, k: &str| {
    assert!(!pathline::editing(w.fs()));
    w.key(k);
    assert!(pathline::editing(w.fs()), "{k}: the path field is open");
});

row!(edit_path_history, Cmd::EditPathHistory, Parity::Same, ["F4"], |w: &mut Win, k: &str| {
    w.key(k);
    assert!(pathline::editing(w.fs()), "{k}: the path field is open");
});

row!(next_region, Cmd::NextRegion, Parity::Same, ["F6"], |w: &mut Win, k: &str| {
    w.key("Down");
    let before = w.focused();
    w.key(k);
    assert_ne!(w.focused(), before, "{k}: the focus moves to the next region");
});

row!(prev_region, Cmd::PrevRegion, Parity::Same, ["Shift+F6"], |w: &mut Win, k: &str| {
    w.key("Down");
    let before = w.focused();
    w.key(k);
    assert_ne!(w.focused(), before, "{k}: the focus moves to the previous region");
});

row!(expand_tree, Cmd::ExpandTree, Parity::Same, ["Ctrl+Shift+E"], |w: &mut Win, k: &str| {
    // The drive of the test root is in the tree; the key opens the chain down to the folder shown.
    let path = w.root.path().join("invoices").to_string_lossy().into_owned();
    w.root.dir(r"invoices\deep");
    let letter = path.as_bytes()[0].to_ascii_uppercase();
    let shared = std::rc::Rc::clone(w.fs().shared());
    shared.drives.borrow_mut().set(DriveInfo {
        letter,
        label: None,
        kind: DriveKind::Fixed,
        free: Some(10_000_000_000),
        total: Some(100_000_000_000),
        ready: true,
    });
    w.h.send(Box::new(files_engine::msg::Msg::Fwd(files_engine::msg::Fwd::Bump)));
    w.h.render();
    w.enter_invoices();
    w.key(k);
    for _ in 0..40 {
        w.pump();
    }
    let rows = sidebar::of(w.fs()).rows();
    assert!(rows.iter().any(|r| r.kind == RowKind::Dir), "{k}: the tree shows the folders on the way");
});

row!(fit_columns, Cmd::FitColumns, Parity::Same, ["Ctrl+Add"], |w: &mut Win, k: &str| {
    let width = |w: &Win, i: usize| w.fs().tabs().active().table.columns()[i].width;
    let before = (width(w, 0), width(w, 3));
    w.key(k);
    assert_ne!((width(w, 0), width(w, 3)), before, "{k}: the columns are resized to their cells");
});

// ---- tabs and windows ------------------------------------------------------------------------------------------

row!(new_tab, Cmd::NewTab, Parity::Same, ["Ctrl+T"], |w: &mut Win, k: &str| {
    w.queue_root();
    w.key(k);
    assert_eq!(w.tabs(), 2);
    assert_eq!(w.active(), 1, "{k}: the new tab is shown");
});

row!(close_tab, Cmd::CloseTab, Parity::Verify, ["Ctrl+W", "Ctrl+F4"], |w: &mut Win, k: &str| {
    w.tabs_to(2);
    w.key(k);
    assert_eq!(w.tabs(), 1, "{k}: the shown tab closed");
});

row!(next_tab, Cmd::NextTab, Parity::Same, ["Ctrl+Tab", "Ctrl+PgDn"], |w: &mut Win, k: &str| {
    w.tabs_to(3);
    w.key("Ctrl+1");
    w.key(k);
    assert_eq!(w.active(), 1, "{k}");
});

row!(prev_tab, Cmd::PrevTab, Parity::Same, ["Ctrl+Shift+Tab", "Ctrl+PgUp"], |w: &mut Win, k: &str| {
    w.tabs_to(3);
    w.key(k);
    assert_eq!(w.active(), 1, "{k}: from the last tab to the one before");
});

/// Ctrl+n shows the nth tab.
fn nth_tab(w: &mut Win, k: &str, n: usize) {
    w.tabs_to(8);
    w.key("Ctrl+9");
    assert_eq!(w.active(), 7);
    w.key(k);
    assert_eq!(w.active(), n - 1, "{k}");
}

row!(tab1, Cmd::Tab1, Parity::Verify, ["Ctrl+1"], |w: &mut Win, k: &str| nth_tab(w, k, 1));
row!(tab2, Cmd::Tab2, Parity::Verify, ["Ctrl+2"], |w: &mut Win, k: &str| nth_tab(w, k, 2));
row!(tab3, Cmd::Tab3, Parity::Verify, ["Ctrl+3"], |w: &mut Win, k: &str| nth_tab(w, k, 3));
row!(tab4, Cmd::Tab4, Parity::Verify, ["Ctrl+4"], |w: &mut Win, k: &str| nth_tab(w, k, 4));
row!(tab5, Cmd::Tab5, Parity::Verify, ["Ctrl+5"], |w: &mut Win, k: &str| nth_tab(w, k, 5));
row!(tab6, Cmd::Tab6, Parity::Verify, ["Ctrl+6"], |w: &mut Win, k: &str| nth_tab(w, k, 6));
row!(tab7, Cmd::Tab7, Parity::Verify, ["Ctrl+7"], |w: &mut Win, k: &str| nth_tab(w, k, 7));
row!(tab8, Cmd::Tab8, Parity::Verify, ["Ctrl+8"], |w: &mut Win, k: &str| nth_tab(w, k, 8));

row!(last_tab, Cmd::LastTab, Parity::Verify, ["Ctrl+9"], |w: &mut Win, k: &str| {
    w.tabs_to(4);
    w.key("Ctrl+1");
    assert_eq!(w.active(), 0);
    w.key(k);
    assert_eq!(w.active(), 3, "{k}: the last tab, whatever the count");
});

row!(reopen_tab, Cmd::ReopenTab, Parity::Ours, ["Ctrl+Shift+T"], |w: &mut Win, k: &str| {
    w.tabs_to(2);
    w.key("Ctrl+W");
    assert_eq!(w.tabs(), 1);
    w.queue_root();
    w.key(k);
    assert_eq!(w.tabs(), 2, "{k}: the closed tab comes back");
});

row!(new_window, Cmd::NewWindow, Parity::Same, ["Ctrl+N"], |w: &mut Win, k: &str| {
    w.key(k);
    let q = w.fs().shared().loop_requests.borrow();
    assert_eq!(q.len(), 1, "{k}");
    assert!(matches!(&q[0], LoopReq::Main { cascade_from: Some(1), select: None, .. }));
});

row!(open, Cmd::Open, Parity::Same, [], |w: &mut Win, _k: &str| {
    // A file goes to the shell host; a folder is entered, never handed to it.
    w.on_notes();
    w.run(Cmd::Open);
    assert_eq!(w.execs(), [(Verb::Open, vec![w.item("notes.txt")])]);
    w.queue_invoices();
    w.key("Home");
    w.run(Cmd::Open);
    w.pump();
    assert_eq!(w.here_name(), "invoices");
    assert_eq!(w.execs().len(), 1, "a folder is not an Exec");
});

row!(open_new_tab, Cmd::OpenNewTab, Parity::Verify, ["Ctrl+Enter"], |w: &mut Win, k: &str| {
    w.queue_invoices();
    w.key("Home");
    w.key(k);
    w.pump();
    assert_eq!(w.tabs(), 2, "{k}: the folder opens in a tab of its own");
    assert_eq!(w.active(), 0, "{k}: the shown tab stays shown");
});

// ---- selection -------------------------------------------------------------------------------------------------

row!(select_all, Cmd::SelectAll, Parity::Same, ["Ctrl+A"], |w: &mut Win, k: &str| {
    w.key(k);
    assert_eq!(w.selected(), 5);
});

row!(select_none, Cmd::SelectNone, Parity::Ours, ["Ctrl+Shift+A"], |w: &mut Win, k: &str| {
    w.key("Ctrl+A");
    w.key(k);
    assert_eq!(w.selected(), 0);
});

row!(invert, Cmd::Invert, Parity::Ours, ["Ctrl+I"], |w: &mut Win, k: &str| {
    w.key("Home").key("Shift+Down");
    assert_eq!(w.selected(), 2);
    w.key(k);
    assert_eq!(w.selected(), 3, "{k}: the other rows are selected");
});

row!(escape, Cmd::Escape, Parity::Same, ["Esc"], |w: &mut Win, k: &str| {
    w.key("Ctrl+A");
    w.key(k);
    assert_eq!(w.selected(), 0, "{k}: clears the selection");
});

// ---- items -----------------------------------------------------------------------------------------------------

row!(properties, Cmd::Properties, Parity::Same, ["Alt+Enter"], |w: &mut Win, k: &str| {
    w.on_notes();
    w.key(k);
    assert_eq!(w.execs(), [(Verb::Properties, vec![w.item("notes.txt")])]);
});

row!(rename, Cmd::Rename, Parity::Same, [], |w: &mut Win, _k: &str| {
    w.on_notes();
    w.run(Cmd::Rename);
    assert!(w.fs().tabs().active().table.is_renaming(), "the name is edited in place");
});

row!(new_folder, Cmd::NewFolder, Parity::Same, ["Ctrl+Shift+N"], |w: &mut Win, k: &str| {
    w.key(k);
    assert_eq!(w.calls(), [OpsCall::NewFolder(w.dir(), "New folder".into())]);
});

row!(copy_path, Cmd::CopyPath, Parity::Same, ["Ctrl+Shift+C"], |w: &mut Win, k: &str| {
    w.key(k);
    let want = format!("\"{}\"", w.dir().to_string_lossy());
    assert_eq!(w.h.clipboard().text.as_deref(), Some(want.as_str()), "{k}: the folder, quoted");
});

row!(recycle, Cmd::Recycle, Parity::Same, ["Del", "Ctrl+D"], |w: &mut Win, k: &str| {
    w.on_notes();
    let jobs = w.f.bg.runs().len();
    w.key(k);
    assert!(w.calls().is_empty(), "{k}: nothing goes before the classification");
    assert_eq!(w.f.bg.runs().len(), jobs + 1, "{k}: one classification job");
});

row!(delete_permanent, Cmd::DeletePermanent, Parity::Ours, ["Shift+Del"], |w: &mut Win, k: &str| {
    w.on_notes();
    w.key(k);
    assert_eq!(w.calls(), [OpsCall::DeletePermanent(vec![w.item("notes.txt")])]);
});

row!(undo, Cmd::Undo, Parity::Same, ["Ctrl+Z"], |w: &mut Win, k: &str| {
    w.key(k);
    assert!(w.has_label("Nothing to undo"), "{k}: {}", w.h.a11y_dump());
});

/// A redo entry that makes the folder "x" again.
fn redo_new_folder(w: &Win) {
    let made = wp(&format!(r"{}\x", w.dir().to_string_lossy()));
    let rec = UndoRecord {
        of: OpId(9),
        kind: UndoKind::RecycleNewFolder,
        replaced: 0,
        entries: vec![Entry { from: made.clone(), to: made, size: 1, mtime: 0, dir: false, bin: None }],
    };
    w.fs().shared().undo.borrow_mut().push_redo(UndoEntry { op: OpId(9), kind: OpKind::NewFolder, record: rec });
}

row!(redo, Cmd::Redo, Parity::Same, ["Ctrl+Y", "Ctrl+Shift+Z"], |w: &mut Win, k: &str| {
    redo_new_folder(w);
    w.key(k);
    assert_eq!(w.calls(), [OpsCall::NewFolder(w.dir(), "x".into())], "{k}");
});

row!(our_menu, Cmd::OurMenu, Parity::Same, ["Apps"], |w: &mut Win, k: &str| {
    w.key("Home");
    w.key(k);
    assert_eq!(w.h.menus().len(), 1, "{k}: our menu opens on the focused row");
    let asked = w.f.shell.requests().iter().filter(|r| matches!(r, ShellReq::Menu { .. })).count();
    assert_eq!(asked, 0, "{k}: no shell-host menu request for the first level");
});

row!(system_menu, Cmd::SystemMenu, Parity::Same, ["Shift+F10"], |w: &mut Win, k: &str| {
    menu::set_shift_for_tests(Some(false));
    w.key("Home");
    w.key(k);
    let asked = w.f.shell.requests().iter().filter(|r| matches!(r, ShellReq::Menu { .. })).count();
    assert_eq!(asked, 1, "{k}: the system menu of the focused row is requested");
});

row!(hidden_items, Cmd::HiddenItems, Parity::Ours, ["Ctrl+H"], |w: &mut Win, k: &str| {
    w.key(k);
    assert!(w.f.options.modifications().last().is_some_and(|o| o.hidden_items), "{k}: kept through the service");
});

row!(sidebar, Cmd::Sidebar, Parity::Ours, ["Ctrl+B"], |w: &mut Win, k: &str| {
    let shown = sidebar::of(w.fs()).is_shown();
    w.key(k);
    assert_eq!(sidebar::of(w.fs()).is_shown(), !shown, "{k}: toggles the sidebar");
});

row!(ops_flyout, Cmd::OpsFlyout, Parity::Ours, ["Ctrl+J"], |w: &mut Win, k: &str| {
    // The flyout lists operations: one is running.
    w.on_notes();
    w.key("Shift+Del");
    w.f.ops.emit(OpsEvent::Evt(OpEvt::Started { op: OpId(1), kind: OpKind::DeletePermanent, items: 1, bytes: 0 }));
    w.pump();
    w.key(k);
    assert!(ops_ui::flyout_of(w.fs()).is_open(), "{k}: opens the operations flyout");
    w.key(k);
    assert!(!ops_ui::flyout_of(w.fs()).is_open(), "{k}: and closes it again");
});

row!(open_in_explorer, Cmd::OpenInExplorer, Parity::Ours, ["Ctrl+Shift+O"], |w: &mut Win, k: &str| {
    w.key(k);
    assert_eq!(w.execs(), [(Verb::OpenInExplorer, vec![ItemRef::Path(w.dir())])]);
});

row!(about, Cmd::About, Parity::Ours, [], |w: &mut Win, _k: &str| {
    w.run(Cmd::About);
    assert!(w.h.a11y_dump().contains("Dialog"), "{}", w.h.a11y_dump());
});

// ---- the table as a whole --------------------------------------------------------------------------------------

#[test]
fn every_row_has_its_test() {
    let source = include_str!("parity.rs");
    for r in TABLE {
        let needle = format!("Cmd::{:?},", r.cmd);
        assert!(
            source.lines().any(|l| l.starts_with("row!(") && l.contains(&needle)),
            "{:?} has no parity test",
            r.cmd
        );
    }
}

#[test]
fn verify_rows_are_exactly_the_planned_ones() {
    let mut verify: Vec<String> = TABLE
        .iter()
        .filter(|r| r.parity == Parity::Verify)
        .flat_map(|r| r.chords.iter().filter_map(|c| c.chord()))
        .map(|c| format!("{c:?}"))
        .collect();
    verify.sort();
    // Ctrl+W of CloseTab is the same as File Explorer's; its row is Verify for Ctrl+F4.
    let mut want: Vec<String> = ["Ctrl+W", "Ctrl+F4", "Ctrl+Enter"]
        .into_iter()
        .chain(["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7", "Ctrl+8", "Ctrl+9"])
        .filter_map(Chord::parse)
        .map(|c| format!("{c:?}"))
        .collect();
    want.sort();
    assert_eq!(verify, want);
    let cmds: Vec<Cmd> = TABLE.iter().filter(|r| r.parity == Parity::Verify).map(|r| r.cmd).collect();
    assert_eq!(cmds.len(), 11, "{cmds:?}: CloseTab, Tab1-Tab8, LastTab and OpenNewTab");
}

/// Unbound chords must change nothing. The harness does not hand back the Reply of a key (the screen is driven
/// through `Harness::key`, which drops it), so the Reply::None half of the rule is not asserted here on purpose:
/// this test checks every observable effect instead.
#[test]
fn unbound_chords_reach_no_command() {
    let unbound = [
        "Ctrl+C",
        "Ctrl+X",
        "Ctrl+V",
        "F7",
        "F8",
        "F9",
        "F11",
        "F12",
        "Ctrl+E",
        "Ctrl+G",
        "Ctrl+K",
        "Ctrl+O",
        "Ctrl+P",
        "Ctrl+Q",
        "Ctrl+S",
        "Ctrl+U",
        "Ctrl+Shift+V",
        "Ctrl+Shift+X",
        "Alt+X",
        "Ctrl+Alt+Z",
    ];
    assert_eq!(unbound.len(), 20);
    let mut w = win();
    w.key("Down");
    let before = (w.here(), w.tabs(), w.selected(), w.focused());
    for k in unbound {
        let c = Chord::parse(k).expect("a chord");
        let in_table = TABLE.iter().any(|r| r.chords.iter().any(|x| x.chord() == Some(c)));
        assert!(!in_table, "{k} is in the table");
        w.key(k);
        assert!(w.calls().is_empty(), "{k}: an operation was asked");
        assert!(
            w.f.shell.requests().iter().all(|r| !matches!(r, ShellReq::Exec { .. } | ShellReq::Menu { .. })),
            "{k}"
        );
        assert!(w.fs().shared().loop_requests.borrow().is_empty(), "{k}: a window was asked");
        assert!(w.h.menus().is_empty(), "{k}: a menu opened");
        assert!(w.h.clipboard().text.is_none(), "{k}: the clipboard was written");
        assert_eq!((w.here(), w.tabs(), w.selected(), w.focused()), before, "{k}: the window changed");
    }
}