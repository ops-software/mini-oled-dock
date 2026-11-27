//! The details window (P3-files-app F1a.8 and after): keys, navigation, sorting, the toolbar. Plain tests over the
//! fakes through the kit's Harness: no window, no file system.

use files_engine::app::{self, Flow, StartEnv, Step, StepRunner};
use files_engine::cli::Cli;
use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::instance::Elevation;
use files_engine::keys::Cmd;
use files_engine::listing::{CancelToken, EntryStore, EnumError, EnumJob, Gen, TabId, WidePath};
use files_engine::model::Listing;
use files_engine::msg::{JobDone, Msg};
use files_engine::services::EnumService;
use files_engine::session::Session;
use files_engine::status::{self, Notice, NoticeAct};
use files_engine::tab::TABLE_ID;
use files_engine::types::TypeNames;
use files_engine::view::states;
use files_engine::window::{self, FilesScreen, LoopReq, Shared, Shed, STARTUP_TAB};
use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::{AppMsg, Caption, Placement, WindowKind};
use oled_ui::model::ListModel;
use oled_ui::node::Text;
use oled_ui::CommandId;
use oled_ui::{
    CaptionHit, CommandTable, Event, EventCx, LayoutCx, PaintCx, PointL, PointP, RectP, Reply, Screen, SizeL, SizeP,
    Tokens, UiLang,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// The tab row the harness gives a system-caption window (TAB_ROW_H), above the toolbar.
const CHROME_H: i32 = 36;

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

type Stash = Rc<std::cell::RefCell<Option<files_engine::navcache::Cached>>>;

fn go_cached(w: &mut Win, to: &str, c: files_engine::navcache::Cached) {
    *w.stash.borrow_mut() = Some(c);
    w.do_op(Op::GoCached(Location::Fs(wp(to))));
}

/// What a test asks the screen to do from inside its own event, where an EventCx exists.
enum Op {
    Revalidate(TabId),
    Go(Location),
    /// Goes to the folder with the listing parked in STASH as the cache's answer (a Listing is not Send).
    GoCached(Location),
    Run(Cmd),
    Notice(status::Notice),
    Event(Event),
}

/// The screen as it is, plus the entry points a plain key cannot reach.
struct Wrap(FilesScreen, Stash);

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
            Ok(op) => {
                match *op {
                    Op::Revalidate(tab) => window::revalidate(&mut self.0, cx, tab),
                    Op::Go(to) => window::navigate(&mut self.0, cx, to, None),
                    Op::GoCached(to) => {
                        let c = self.1.borrow_mut().take();
                        window::navigate_with(&mut self.0, cx, to, None, c)
                    }
                    Op::Run(c) => {
                        window::cmd(&mut self.0, cx, c);
                    }
                    Op::Notice(n) => status::notice(&mut self.0, cx, n),
                    Op::Event(ev) => {
                        self.0.event(cx, &ev);
                    }
                }
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

struct Win {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<Wrap>,
    stash: Stash,
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }

    fn do_op(&mut self, op: Op) {
        self.h.send(Box::new(op));
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    /// The labels of the rows, in order.
    fn labels(&self) -> Vec<String> {
        let m = self.fs().tabs().active().table.model();
        (0..ListModel::len(m)).map(|r| m.label(r).to_string()).collect()
    }

    /// The labels of the selected rows, in order.
    fn selected_labels(&self) -> Vec<String> {
        let t = &self.fs().tabs().active().table;
        let m = t.model();
        (0..ListModel::len(m))
            .filter(|r| t.selection().contains(ListModel::key(m, *r)))
            .map(|r| m.label(r).to_string())
            .collect()
    }

    /// The name of the focused row.
    fn focused_label(&self) -> Option<String> {
        let t = &self.fs().tabs().active().table;
        let row = t.model().row_of(t.focused()?)?;
        Some(t.model().label(row).to_string())
    }
}

fn start(f: Fakes, shared: Rc<Shared>, root: &str, tab: TabId) -> Win {
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(root)), None, tab);
    let stash = Stash::default();
    let mut h = Harness::new(Wrap(screen, Rc::clone(&stash)), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, shared, h, stash }
}

fn window_over(f: Fakes, root: &str) -> Win {
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    start(f, shared, root, tab)
}

fn reports() -> Vec<(&'static str, bool, u64, i64)> {
    vec![
        ("invoices", true, 0, day(3)),
        ("projects", true, 0, day(9)),
        ("notes.txt", false, 1_200, day(1)),
        ("report-final-v2.pdf", false, 3_400_000, day(5)),
        ("photos-raw.zip", false, 90_000_000, day(40)),
    ]
}

fn queue_children(f: &Fakes) {
    f.enumr.queue(&wp(r"C:\mod-made-up\reports\invoices"), fakes::listing(&[("a.txt", false, 1, day(1))]));
    f.enumr.queue(&wp(r"C:\mod-made-up\reports\projects"), fakes::listing(&[("b.txt", false, 1, day(1))]));
}

fn folder_window() -> Win {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    queue_children(&f);
    window_over(f, r"C:\mod-made-up\reports")
}

fn here(w: &Win) -> String {
    match w.fs().tabs().active().nav.location() {
        Location::Fs(p) => p.to_string_lossy(),
        other => format!("{other:?}"),
    }
}

fn rows(w: &Win) -> usize {
    ListModel::len(w.fs().tabs().active().table.model())
}

fn selected(w: &Win) -> usize {
    w.fs().tabs().active().table.selection().count(rows(w))
}

#[test]
fn first_listing_arrives_and_every_node_is_named() {
    let w = folder_window();
    assert_eq!(rows(&w), 5);
    w.h.assert_named();
}

#[test]
fn f1a_key_script() {
    let mut w = folder_window();
    w.h.key("Down").key("Down").key("Down");
    let at = w.focused_label().expect("a row is focused after the Down keys");
    w.h.key("Shift+Down");
    assert_eq!(selected(&w), 2, "Shift+Down extends the selection by one row: {:?}", w.selected_labels());
    assert_ne!(w.focused_label(), Some(at), "the focus moved down");
    w.h.assert_named();
    w.h.key("Ctrl+A");
    assert_eq!(selected(&w), 5);
    w.h.key("Ctrl+Shift+A");
    assert_eq!(selected(&w), 0);
    w.h.key("Ctrl+I");
    assert_eq!(selected(&w), 5);
    w.h.key("Ctrl+Shift+A");
    // Enter on a folder navigates, Backspace returns, Alt+Right goes forward again.
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(here(&w).ends_with("invoices"), "{}", here(&w));
    assert_eq!(w.labels(), ["a.txt"]);
    w.h.assert_named();
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    w.h.key("Backspace");
    w.pump();
    assert!(here(&w).ends_with("reports"), "{}", here(&w));
    assert_eq!(rows(&w), 5);
    assert_eq!(w.focused_label().as_deref(), Some("invoices"), "Backspace focuses the folder we came from");
    w.h.assert_named();
    queue_children(&w.f);
    w.h.key("Alt+Right");
    w.pump();
    assert!(here(&w).ends_with("invoices"));
    assert_eq!(w.labels(), ["a.txt"]);
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    w.h.key("Alt+Left");
    w.pump();
    assert!(here(&w).ends_with("reports"));
    assert_eq!(rows(&w), 5);
    w.h.assert_named();
    // The parent arrives in two batches, the folder we came from in the second: it is still focused and selected.
    let mut first = fakes::listing(&[("archive", true, 0, day(2)), ("backups", true, 0, day(3))]);
    first.done = false;
    w.f.enumr.queue(&wp(r"C:\mod-made-up"), first);
    w.f.enumr
        .queue(&wp(r"C:\mod-made-up"), fakes::listing(&[("reports", true, 0, day(1)), ("zeta.txt", false, 5, day(4))]));
    w.h.key("Alt+Up");
    w.pump();
    assert_eq!(here(&w), r"C:\mod-made-up");
    assert_eq!(w.labels(), ["archive", "backups", "reports", "zeta.txt"]);
    assert_eq!(w.focused_label().as_deref(), Some("reports"));
    assert_eq!(w.selected_labels(), ["reports"]);
    w.h.assert_named();
}

#[test]
fn new_window_pushes_one_loop_request() {
    let mut w = folder_window();
    w.h.key("Ctrl+N");
    let q = w.shared.loop_requests.borrow();
    assert_eq!(q.len(), 1);
    assert!(matches!(&q[0], LoopReq::Main { cascade_from: Some(1), select: None, .. }));
}

#[test]
fn sort_click_sorts_with_folders_first() {
    let mut w = folder_window();
    assert_eq!(w.labels(), ["invoices", "projects", "notes.txt", "photos-raw.zip", "report-final-v2.pdf"]);
    // The Size header sits near the right end of the header row, under the toolbar.
    w.h.click(PointL { x: 770, y: CHROME_H + 44 + 16 });
    w.h.render();
    let t = w.fs().tabs().active();
    assert_eq!(t.sort.0, files_engine::model::ColKind::Size);
    let sorted: Vec<_> = t.table.columns().iter().filter(|c| c.sort.is_some()).map(|c| c.id.0).collect();
    assert_eq!(sorted, [files_engine::model::ColKind::Size as u16], "the chevron is on the Size column only");
    // The first click on Size sorts largest first; folders stay on top.
    let l = w.labels();
    assert_eq!(l[2..], ["photos-raw.zip", "report-final-v2.pdf", "notes.txt"]);
    assert!(l[..2].contains(&"invoices".to_string()) && l[..2].contains(&"projects".to_string()), "{l:?}");
    w.h.click(PointL { x: 770, y: CHROME_H + 44 + 16 });
    w.h.render();
    assert_eq!(w.labels()[2..], ["notes.txt", "report-final-v2.pdf", "photos-raw.zip"], "the second click reverses it");
}

#[test]
fn big_sort_runs_off_thread() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\big"), fakes::listing_n(25_000, 7));
    let mut w = window_over(f, r"C:\mod-made-up\big");
    assert_eq!(rows(&w), 25_000);
    let sample = |w: &Win| -> Vec<String> {
        let m = w.fs().tabs().active().table.model();
        (10_000..10_020).map(|r| m.label(r).to_string()).collect()
    };
    let before = sample(&w);
    w.h.click(PointL { x: 770, y: CHROME_H + 44 + 16 });
    assert_eq!(w.f.bg.queued(), 1, "the sort is a background job");
    assert_eq!(sample(&w), before, "the old order stays until Sorted");
    w.pump();
    assert_eq!(w.fs().tabs().active().sort.0, files_engine::model::ColKind::Size);
    assert_ne!(sample(&w), before, "the sorted order was adopted");
}

#[test]
fn navigate_clears_selection() {
    let mut w = folder_window();
    w.h.key("Ctrl+A");
    assert_eq!(selected(&w), 5);
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(here(&w).ends_with("invoices"), "{}", here(&w));
    assert_eq!(selected(&w), 0, "nothing the user did not pick in this folder is selected");
    w.h.key("Ctrl+A");
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    w.h.key("Backspace");
    w.pump();
    assert_eq!(rows(&w), 5);
    assert_eq!(selected(&w), 0);
}

#[test]
fn refresh_keeps_selection_by_name() {
    let mut w = folder_window();
    w.h.key("Down").key("Down").key("Down").key("Shift+Down");
    let picked = w.selected_labels();
    assert_eq!(picked.len(), 2);
    let focus = w.focused_label();
    // The refreshed folder lists its entries in another order: every id differs.
    let mut again = reports();
    again.reverse();
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&again));
    w.h.key("F5");
    w.pump();
    assert_eq!(rows(&w), 5);
    assert_eq!(w.selected_labels(), picked);
    assert_eq!(w.focused_label(), focus);
}

#[test]
fn revalidate_merges_keeping_ids() {
    let mut w = folder_window();
    let key_of = |w: &Win, n: &str| w.fs().tabs().active().table.model().listing().find(n.as_bytes());
    let notes = key_of(&w, "notes.txt").expect("notes.txt is listed");
    let tab = w.fs().tabs().active().id;
    // The folder now has a new file and lost one; the others keep their ids.
    let mut now = reports();
    now.retain(|e| e.0 != "photos-raw.zip");
    now.insert(0, ("zzz-new.txt", false, 7, day(0)));
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&now));
    w.do_op(Op::Revalidate(tab));
    w.pump();
    assert_eq!(rows(&w), 5);
    assert_eq!(key_of(&w, "notes.txt"), Some(notes), "a survivor keeps its id");
    assert!(key_of(&w, "zzz-new.txt").is_some());
    assert!(key_of(&w, "photos-raw.zip").is_none());
}

#[test]
fn revalidate_then_navigate_drops_fresh() {
    let mut w = folder_window();
    let tab = w.fs().tabs().active().id;
    // The revalidation of reports is still running when the tab leaves the folder.
    w.do_op(Op::Revalidate(tab));
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(here(&w).ends_with("invoices"), "{}", here(&w));
    assert_eq!(w.labels(), ["a.txt"]);
    // Its batches arrive late: they are not merged into this folder's listing.
    w.f.enumr.queue(
        &wp(r"C:\mod-made-up\reports"),
        fakes::listing(&[("late-1", false, 1, day(1)), ("late-2", false, 1, day(1))]),
    );
    w.pump();
    assert_eq!(w.labels(), ["a.txt"], "the late batch of the folder we left is not merged into this one");
}

#[test]
fn failed_navigation_restarts_the_old_folder() {
    let mut w = folder_window();
    // projects never finishes on its own: the user leaves it for a folder that does not exist.
    w.h.key("Down").key("Enter");
    w.f.enumr.queue_error(&wp(r"C:\mod-made-up\reports\ghost"), files_engine::listing::EnumError::NotFound);
    w.do_op(Op::Go(Location::Fs(wp(r"C:\mod-made-up\reports\ghost"))));
    let asked = w.fs().tabs().active().nav.gen();
    w.pump();
    assert!(here(&w).ends_with("projects"), "{}", here(&w));
    assert!(w.fs().tabs().active().nav.gen() > asked, "the restart is a new generation");
    assert_eq!(rows(&w), 1, "the restarted listing reached the table: {:?}", w.labels());
    assert_eq!(
        files_engine::pathline::error_of(w.fs()).as_deref(),
        Some(r"Can't find C:\mod-made-up\reports\ghost"),
        "the refused folder is reported under the path line"
    );
}

#[test]
fn refresh_of_a_vanished_folder_names_it() {
    let mut w = folder_window();
    w.f.enumr.queue_error(&wp(r"C:\mod-made-up\reports"), EnumError::NotFound);
    w.h.key("F5");
    w.pump();
    assert_eq!(files_engine::pathline::error_of(w.fs()).as_deref(), Some(r"Can't find C:\mod-made-up\reports"));
}

#[test]
fn x_buttons_navigate() {
    use oled_ui::{Mods, MouseButton};
    let mut w = folder_window();
    let at = PointP { x: 400, y: 200 };
    let click = |w: &mut Win, button| w.do_op(Op::Event(Event::PointerUp { pos: at, button, mods: Mods::NONE }));
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(here(&w).ends_with("invoices"), "{}", here(&w));
    click(&mut w, MouseButton::X1);
    w.pump();
    assert!(here(&w).ends_with("reports"), "X1 goes back: {}", here(&w));
    click(&mut w, MouseButton::X2);
    w.pump();
    assert!(here(&w).ends_with("invoices"), "X2 goes forward: {}", here(&w));
}

#[test]
fn start_up_tab_receives_its_batches() {
    let f = Fakes::new();
    let dir = wp(r"C:\mod-made-up\reports");
    f.enumr.queue(&dir, fakes::listing(&reports()));
    // A listing that, like the real one, ignores the batches of another generation.
    struct Checking(Rc<fakes::FakeEnum>);
    impl EnumService for Checking {
        fn submit(&self, job: EnumJob) {
            self.0.submit(job)
        }
        fn new_listing(&self, dir: WidePath, gen: Gen) -> Box<dyn Listing> {
            Box::new(EntryStore::new(dir, gen))
        }
    }
    let mut services = f.services();
    services.enumr = Rc::new(Checking(Rc::clone(&f.enumr)));
    let shared = Shared::new(Arc::clone(&f.hub), services, UiLang::EN, Session::default(), TypeNames::empty());
    // app.rs submits the start-up job before the window exists, under the first generation.
    f.enumr.submit(EnumJob { tab: STARTUP_TAB, gen: Gen(1), dir: dir.clone(), cancel: CancelToken::new() });
    let w = start(f, shared, r"C:\mod-made-up\reports", STARTUP_TAB);
    assert_eq!(rows(&w), 5, "the start-up window shows its first listing");
}

#[test]
fn downloads_default_sort_is_not_carried_to_other_folders() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\dl"), fakes::listing(&reports()));
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    shared.session.borrow_mut().known.insert("downloads".into(), r"C:\mod-made-up\dl".into());
    let tab = shared.new_tab();
    let mut w = start(f, shared, r"C:\mod-made-up\dl", tab);
    use files_engine::model::ColKind;
    use oled_ui::model::SortDir;
    assert_eq!(w.fs().tabs().active().sort, (ColKind::Modified, SortDir::Desc));
    w.do_op(Op::Go(Location::Fs(wp(r"C:\mod-made-up\reports"))));
    w.pump();
    assert_eq!(w.fs().tabs().active().sort, (ColKind::Name, SortDir::Asc), "another folder sorts by name again");
    // A sort the user picked stays with the tab.
    w.h.click(PointL { x: 770, y: CHROME_H + 44 + 16 });
    w.f.enumr.queue(&wp(r"C:\mod-made-up\dl"), fakes::listing(&reports()));
    w.do_op(Op::Go(Location::Fs(wp(r"C:\mod-made-up\dl"))));
    w.pump();
    assert_eq!(w.fs().tabs().active().sort.0, ColKind::Size);
}

#[test]
fn start_up_tab_on_downloads_lists_by_date_modified() {
    use files_engine::model::ColKind;
    use oled_ui::model::SortDir;
    let f = Fakes::new();
    let dir = wp(r"C:\mod-made-up\dl");
    f.enumr.queue(&dir, fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    shared.session.borrow_mut().known.insert("downloads".into(), r"C:\mod-made-up\dl".into());
    f.enumr.submit(EnumJob { tab: STARTUP_TAB, gen: Gen(1), dir, cancel: CancelToken::new() });
    let w = start(f, shared, r"C:\mod-made-up\dl", STARTUP_TAB);
    let t = w.fs().tabs().active();
    assert_eq!(t.sort, (ColKind::Modified, SortDir::Desc));
    let chevron = t.table.columns().iter().find(|c| c.id.0 == ColKind::Modified as u16).and_then(|c| c.sort);
    assert_eq!(chevron, Some(SortDir::Desc), "the header shows the sort");
    assert_eq!(
        w.labels(),
        ["invoices", "projects", "notes.txt", "report-final-v2.pdf", "photos-raw.zip"],
        "folders first, each group newest first, from the first batch"
    );
}

#[test]
fn shed_everything_drops_the_listing_and_keeps_what_is_needed_to_come_back() {
    let mut w = folder_window();
    w.h.key("Down").key("Shift+Down");
    let picked = w.selected_labels();
    window::shed(&mut w.h.screen_mut().0, Shed::Background);
    assert_eq!(rows(&w), 5, "the shown tab keeps its listing under Background");
    window::shed(&mut w.h.screen_mut().0, Shed::Everything);
    assert_eq!(rows(&w), 0);
    let kept = w.fs().tabs().active().kept.clone().expect("the tab recorded how to come back");
    assert_eq!(kept.names.len(), picked.len());
}

#[test]
fn regions_cycle_with_f6() {
    let mut w = folder_window();
    let in_table = |w: &Win| w.h.focused().is_some_and(|n| n.owner() == TABLE_ID);
    assert!(in_table(&w), "the list has the focus once the first rows are in");
    // Tab walks out of the list (to the toolbar or the sidebar); F6 comes back to it.
    for _ in 0..30 {
        if !in_table(&w) {
            break;
        }
        w.h.key("Tab");
    }
    assert!(!in_table(&w), "the focus left the list");
    // With the sidebar showing, F6 goes to it first, then to the list.
    for _ in 0..2 {
        if !in_table(&w) {
            w.h.key("F6");
        }
    }
    assert!(in_table(&w), "F6 reaches the list again: {:?}", w.h.focused());
}

#[test]
fn fit_columns_sizes_to_the_widest_cell() {
    let mut w = folder_window();
    let width = |w: &Win, i: usize| w.fs().tabs().active().table.columns()[i].width;
    let (name, size) = (width(&w, 0), width(&w, 3));
    w.do_op(Op::Run(Cmd::FitColumns));
    w.h.render();
    assert_ne!((width(&w, 0), width(&w, 3)), (name, size), "the columns were resized");
    assert!(width(&w, 0) >= 200 && width(&w, 3) >= 70, "never under the column's minimum");
}

#[test]
fn right_click_on_back_reaches_the_history_hook_and_nothing_else() {
    let mut w = folder_window();
    w.h.key("Home").key("Enter");
    w.pump();
    let at = here(&w);
    // Back sits at x 8..36, y 8..36 logical; the hook is a stub until the tabs land, so nothing else may happen.
    w.h.right_click(PointL { x: 22, y: CHROME_H + 22 });
    assert_eq!(here(&w), at);
    assert_eq!(selected(&w), 0);
}

// ---- F1a.9: page states and the status line ----------------------------------------------------------------------

fn dump(w: &Win) -> String {
    w.h.a11y_dump()
}

fn has_label(w: &Win, text: &str) -> bool {
    dump(w).lines().any(|l| l.trim_start() == format!("Label {text:?}"))
}

/// A status label that talks about the selection (the sidebar's current row is "selected" too, in its own node).
fn has_selection_part(w: &Win) -> bool {
    dump(w).lines().any(|l| l.trim_start().starts_with("Label") && l.contains("selected"))
}

fn has_button(w: &Win, text: &str) -> bool {
    dump(w).lines().any(|l| l.trim_start().starts_with(&format!("Button {text:?}")))
}

fn window_failing(err: EnumError) -> Win {
    let f = Fakes::new();
    f.enumr.queue_error(&wp(r"C:\mod-made-up\locked"), err);
    window_over(f, r"C:\mod-made-up\locked")
}

/// Tab until the focus is on the widget, then Enter.
fn press(w: &mut Win, id: oled_ui::WidgetId) {
    for _ in 0..40 {
        if w.h.focused().is_some_and(|n| n.owner() == id) {
            break;
        }
        w.h.key("Tab");
    }
    assert!(w.h.focused().is_some_and(|n| n.owner() == id), "the focus reached the button: {:?}", w.h.focused());
    w.h.key("Enter");
}

#[test]
fn state_empty() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\none"), fakes::listing(&[]));
    let w = window_over(f, r"C:\mod-made-up\none");
    assert!(has_label(&w, "This folder is empty"), "{}", dump(&w));
    assert!(has_label(&w, "Ctrl+Shift+N new folder"));
    w.h.assert_named();
}

#[test]
fn state_denied_has_its_two_buttons_and_back_goes_back() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    f.enumr.queue_error(&wp(r"C:\mod-made-up\reports\invoices"), EnumError::AccessDenied);
    let mut w = window_over(f, r"C:\mod-made-up\reports");
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(has_label(&w, "You don't have permission to open this folder."), "{}", dump(&w));
    assert!(has_button(&w, "Open in File Explorer") && has_button(&w, "Back"), "{}", dump(&w));
    w.h.assert_named();
    // Open in File Explorer hands the folder to the shell host (F2.1).
    press(&mut w, states::ID_FIRST);
    w.pump();
    let handed = w.f.shell.requests().into_iter().any(|r| {
        matches!(&r, oled_trust::frame::files::ShellReq::Exec { verb: oled_trust::frame::files::Verb::OpenInExplorer, items, .. }
            if *items == [oled_trust::frame::files::ItemRef::Path(wp(r"C:\mod-made-up\reports\invoices"))])
    });
    assert!(handed, "{:?}", w.f.shell.requests());
    // Back returns to the folder we came from.
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    press(&mut w, states::ID_SECOND);
    w.pump();
    assert!(here(&w).ends_with("reports"), "{}", here(&w));
    assert_eq!(rows(&w), 5);
    assert!(!has_button(&w, "Back"), "the banner is gone with the page");
}

#[test]
fn state_offline_retry_navigates_again() {
    let mut w = window_failing(EnumError::Offline { waited_ms: 4_000 });
    assert!(has_label(&w, r"C:\mod-made-up\locked isn't reachable"), "{}", dump(&w));
    assert!(has_button(&w, "Retry (F5)") && has_button(&w, "Open in File Explorer"), "{}", dump(&w));
    w.h.assert_named();
    w.f.enumr.queue(&wp(r"C:\mod-made-up\locked"), fakes::listing(&reports()));
    press(&mut w, states::ID_FIRST);
    w.pump();
    assert_eq!(rows(&w), 5, "Retry listed the folder again");
    assert!(!has_button(&w, "Retry (F5)"));
}

#[test]
fn state_connecting_still_loading_too_long_unsupported() {
    let f = Fakes::new();
    let w = {
        let mut w = window_over(f, r"\\made-up-server\share");
        w.h.advance(1_100);
        w
    };
    assert!(has_label(&w, r"Connecting to \\made-up-server\share…"), "{}", dump(&w));
    assert!(!has_button(&w, "Retry (F5)"), "Connecting is the text only");
    let mut w = {
        let f = Fakes::new();
        window_over(f, r"C:\mod-made-up\slow")
    };
    w.h.advance(10_100);
    assert!(has_label(&w, "Still loading…"), "{}", dump(&w));
    assert!(has_label(&window_failing(EnumError::TooLong), "This path is too long to open here."));
    assert!(has_label(&window_failing(EnumError::Unsupported), "This location can't be listed here."));
}

#[test]
fn status_counts_and_selection_size() {
    let mut w = folder_window();
    assert!(has_label(&w, "5 items"), "{}", dump(&w));
    assert!(!has_selection_part(&w), "nothing is selected: no selection part");
    // Two folders: counted, no size.
    w.h.key("Home").key("Shift+Down");
    assert!(has_label(&w, " \u{b7} 2 selected"), "{}", dump(&w));
    // Everything: the size sums the files only.
    w.h.key("Ctrl+A");
    let size = w.shared.fmt.borrow().size(1_200 + 3_400_000 + 90_000_000, false);
    assert!(has_label(&w, &format!(" \u{b7} 5 selected \u{b7} {size}")), "{}", dump(&w));
    w.h.key("Ctrl+Shift+A");
    assert!(!has_selection_part(&w));
    // A new arrival changes the count without any key.
    let mut more = reports();
    more.push(("zeta.txt", false, 5, day(1)));
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&more));
    w.h.key("F5");
    w.pump();
    assert!(has_label(&w, "6 items"), "{}", dump(&w));
}

#[test]
fn free_space_requested_once_per_navigation() {
    let mut w = folder_window();
    let free_jobs = |w: &Win| w.f.bg.runs().len();
    assert_eq!(free_jobs(&w), 1, "the first listing asked once: {:?}", w.f.bg.runs());
    w.h.key("Down").key("Up");
    w.h.render();
    assert_eq!(free_jobs(&w), 1, "keys and paints ask nothing");
    // The disk answers.
    let root = wp(r"C:\mod-made-up\reports");
    w.h.send(Box::new(Msg::Job { win: 1, done: JobDone::FreeSpace { root, free: Some(5_000_000_000), total: None } }));
    let free = w.shared.fmt.borrow().size(5_000_000_000, false);
    assert!(has_label(&w, &format!("{free} free")), "{}", dump(&w));
    // An answer for a folder the tab left is not shown.
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    w.h.key("F5");
    w.pump();
    assert_eq!(free_jobs(&w), 2, "one more request for the next navigation");
    w.h.send(Box::new(Msg::Job {
        win: 1,
        done: JobDone::FreeSpace { root: wp(r"D:\elsewhere"), free: Some(1_000_000_000), total: None },
    }));
    let other = w.shared.fmt.borrow().size(1_000_000_000, false);
    assert!(!has_label(&w, &format!("{other} free")));
    // Once the disk answered for this folder and the tab moves on, the old number is gone until the new answer.
    w.h.send(Box::new(Msg::Job {
        win: 1,
        done: JobDone::FreeSpace { root: wp(r"C:\mod-made-up\reports"), free: Some(5_000_000_000), total: None },
    }));
    assert!(has_label(&w, &format!("{free} free")), "{}", dump(&w));
    w.h.key("Home").key("Enter");
    w.pump();
    assert!(here(&w).ends_with("invoices"), "{}", here(&w));
    assert!(!has_label(&w, &format!("{free} free")), "the old volume's space under the new folder: {}", dump(&w));
}

#[test]
fn notice_hides_after_6s_and_on_esc() {
    let mut w = folder_window();
    w.h.advance(1_000);
    let show = |w: &mut Win, text: &'static str| w.do_op(Op::Notice(Notice::new(Text::Borrowed(text))));
    show(&mut w, "Something happened");
    assert!(has_label(&w, "Something happened"), "{}", dump(&w));
    assert!(!has_label(&w, "5 items"), "the notice takes the place of the items");
    w.h.advance(5_900);
    assert!(has_label(&w, "Something happened"), "still held at 5.9 s");
    w.h.advance(200);
    assert!(!has_label(&w, "Something happened"), "gone after 6 s");
    assert!(has_label(&w, "5 items"));
    show(&mut w, "Again");
    assert!(has_label(&w, "Again"));
    w.h.key("Escape");
    assert!(!has_label(&w, "Again"), "Esc dismisses it first");
    assert_eq!(selected(&w), 0);
    // A notice with an action: the button runs it and the notice goes.
    let n =
        Notice { action: Some((Text::Borrowed("Retry now"), NoticeAct::Retry)), ..Notice::new(Text::Borrowed("Lost")) };
    w.do_op(Op::Notice(n));
    assert!(has_button(&w, "Retry now"), "{}", dump(&w));
    w.f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    press(&mut w, status::ID_ACTION);
    w.pump();
    assert!(!has_button(&w, "Retry now") && !has_label(&w, "Lost"));
    assert_eq!(rows(&w), 5);
}

#[test]
fn idle_when_listed() {
    let mut w = folder_window();
    w.h.advance(1_000);
    assert!(w.h.is_idle(), "nothing runs once the listing is done and the first-frame timers have fired");
    w.do_op(Op::Notice(Notice::new(Text::Borrowed("Hello"))));
    assert!(!w.h.is_idle(), "a notice holds a timer");
    w.h.advance(6_100);
    assert!(w.h.is_idle(), "and lets it go");
}

#[test]
fn home_after_a_folder_settles() {
    let mut w = folder_window();
    w.do_op(Op::Go(Location::Home));
    w.pump();
    assert!(w.fs().tabs().active().nav.page() == &files_engine::nav::Page::Home, "the tab is on Home");
    assert!(!states::refresh(w.fs()), "the page state follows the tab to Home, so nothing asks for another layout");
    w.h.advance(1_000);
    assert!(w.h.is_idle(), "an idle Home page runs nothing");
}

#[test]
fn pressure_order_sheds_caches() {
    let mut w = folder_window();
    // A second tab, listed, in the background.
    let shared = std::rc::Rc::clone(&w.shared);
    let id = shared.new_tab();
    let dir = wp(r"C:\mod-made-up\other");
    let mut t = files_engine::tab::tab_with_id(&shared, 1, Location::Fs(dir.clone()), id);
    let mut l = shared.services.enumr.new_listing(dir, Gen(0));
    l.append(fakes::listing(&reports()));
    l.merge_pending();
    t.table.model_mut().replace(l);
    w.h.screen_mut().0.tabs_mut().open(t, false);
    let bg_rows = |w: &Win| ListModel::len(w.fs().tabs().by_id(id).expect("the tab").table.model());
    assert_eq!(bg_rows(&w), 5);
    let reads = Rc::new(Cell::new(0usize));
    let script = |w: &Win, readings: &'static [u64]| {
        reads.set(0);
        let reads = Rc::clone(&reads);
        *w.shared.memory.borrow_mut() = Box::new(move || {
            let i = reads.get();
            reads.set(i + 1);
            readings[i.min(readings.len() - 1)] * 1024 * 1024
        });
    };
    let check = |w: &mut Win| window::check_memory(&mut w.h.screen_mut().0);
    // Under the line: nothing goes.
    script(&w, &[17]);
    assert_eq!(check(&mut w), None);
    assert_eq!((reads.get(), bg_rows(&w)), (1, 5));
    // One step over: the navigation cache only.
    script(&w, &[19, 17]);
    assert_eq!(check(&mut w), Some(Shed::NavCache));
    assert_eq!((reads.get(), bg_rows(&w)), (2, 5), "the background listing is still there");
    // Two steps: the background listings go, the shown one stays.
    script(&w, &[19, 19, 17]);
    assert_eq!(check(&mut w), Some(Shed::Background));
    assert_eq!((reads.get(), bg_rows(&w), rows(&w)), (3, 0, 5));
    // Still over after both: the per-item icons go last.
    script(&w, &[30]);
    assert_eq!(check(&mut w), Some(Shed::ItemIcons));
    assert_eq!(reads.get(), 3);
}

// ---- F1a.10: start path, session, placement ----------------------------------------------------------------------

/// A runner that records the steps it is given and exits where a script says.
struct Recorder {
    seen: Vec<Step>,
    exit_at: Option<(Step, i32)>,
}

impl StepRunner for Recorder {
    fn run(&mut self, step: Step) -> Flow {
        self.seen.push(step);
        match self.exit_at {
            Some((at, code)) if at == step => Flow::Exit(code),
            _ => Flow::Next,
        }
    }
}

fn window_cli(target: Option<&str>) -> Cli {
    Cli::Window { target: target.map(str::to_string), tab: false, select: None, from_verb: false, forward_only: false }
}

fn record(cli: &Cli, elevation: Elevation, exit_at: Option<(Step, i32)>) -> (Vec<Step>, i32) {
    let mut r = Recorder { seen: Vec::new(), exit_at };
    let code = app::execute(&app::start_plan(cli, &StartEnv { elevation }), &mut r);
    (r.seen, code)
}

#[test]
fn start_plan_order() {
    let pos = |seen: &[Step], s: Step| seen.iter().position(|x| *x == s);
    // A folder target: the enumeration is submitted before any window exists, and the window comes after the loop.
    let (seen, code) = record(&window_cli(Some(r"C:\mod-made-up")), Elevation::Default, None);
    assert_eq!(code, 0);
    assert!(pos(&seen, Step::Enumerate) < pos(&seen, Step::OpenWindow), "{seen:?}");
    assert!(pos(&seen, Step::Mutex) < pos(&seen, Step::Services) && pos(&seen, Step::Config) < pos(&seen, Step::Loop));
    assert!(
        pos(&seen, Step::Loop) < pos(&seen, Step::OpenWindow) && pos(&seen, Step::OpenWindow) < pos(&seen, Step::Run)
    );
    assert_eq!(seen.last(), Some(&Step::Run));
    // No target: nothing to enumerate.
    let (seen, _) = record(&window_cli(None), Elevation::Default, None);
    assert!(!seen.contains(&Step::Enumerate), "{seen:?}");
    // A UAC-elevated token: the hand-off ends the start, nothing after it runs, its code is the exit code.
    let (seen, code) = record(&window_cli(None), Elevation::Full, Some((Step::HandOff, 5)));
    assert_eq!(seen, [Step::Log, Step::HandOff]);
    assert_eq!(code, 5);
    // A request the running window took ends the start at Forward.
    let (seen, code) = record(&window_cli(None), Elevation::Default, Some((Step::Forward, 0)));
    assert_eq!((seen, code), (vec![Step::Log, Step::Forward], 0));
    // Preview and selftest take no mutex, hand nothing off and open no window, whatever the elevation.
    let preview = Cli::Preview { dir: "x".into(), scene: None, lang: None, dpi: None };
    let selftest = Cli::Selftest { nonce: "0".repeat(32) };
    for cli in [preview, selftest] {
        let (seen, _) = record(&cli, Elevation::Full, None);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(![Step::Mutex, Step::HandOff, Step::OpenWindow].contains(&seen[0]));
    }
}

fn own_with(bytes: &[u8]) -> Fakes {
    let f = Fakes::new();
    f.own.put(files_engine::names::SESSION_FILE, bytes);
    f
}

#[test]
fn session_round_trip() {
    let f = Fakes::new();
    let mut s = Session::default();
    s.placements.insert(
        "files.main".into(),
        Placement {
            monitor: r"\\.\DISPLAY1".into(),
            rect: RectP { l: 10, t: 20, r: 1110, b: 740 },
            dpi: 144,
            maximized: true,
        },
    );
    s.network_recent.push(("made-up-server".into(), 1_700_000_000));
    s.known.insert("downloads".into(), r"C:\mod-made-up\dl".into());
    s.reopen = vec![vec![r"C:\mod-made-up\a".into(), r"C:\mod-made-up\b".into()], vec![r"C:\mod-made-up\c".into()]];
    assert!(s.save(&*f.own));
    assert_eq!(f.own.writes(), [files_engine::names::SESSION_FILE]);
    assert_eq!(Session::load(&*f.own), s);
}

#[test]
fn session_malformed_gives_defaults() {
    for bad in [&b"{nope"[..], b"[]", b"", b"\xff\xfe\x00", br#"{"placements": 5}"#] {
        assert_eq!(Session::load(&*own_with(bad).own), Session::default(), "{:?}", String::from_utf8_lossy(bad));
    }
    // A placement that cannot be a window is dropped, the rest of the file is kept.
    let text = r#"{"placements":{"a":{"l":0,"t":0,"r":0,"b":0,"dpi":96},"b":{"l":0,"t":0,"r":10,"b":10,"dpi":96}},"known":{"k":"v"}}"#;
    let s = Session::load(&*own_with(text.as_bytes()).own);
    assert_eq!(s.placements.keys().collect::<Vec<_>>(), ["b"]);
    assert_eq!(s.known.len(), 1);
}

#[test]
fn session_cap() {
    // Over 16 KiB the file is not read at all.
    let big = format!(r#"{{"known":{{"k":"{}"}}}}"#, "x".repeat(17 * 1024));
    assert_eq!(Session::load(&*own_with(big.as_bytes()).own), Session::default());
    // Within it, at most 8 windows of 8 tabs are kept.
    let windows: Vec<Vec<String>> = (0..20).map(|w| (0..20).map(|t| format!("C:\\w{w}\\t{t}")).collect()).collect();
    let text = serde_json::json!({ "reopen": windows }).to_string();
    assert!(text.len() < 16 * 1024);
    let s = Session::load(&*own_with(text.as_bytes()).own);
    assert_eq!((s.reopen.len(), s.reopen[0].len()), (8, 8));
}

/// A second main window over the same Shared, with its own harness.
fn second_window(shared: &Rc<Shared>) -> Harness<Wrap> {
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(shared), 2, Location::Fs(wp(r"C:\mod-made-up\reports")), None, tab);
    let mut h = Harness::new(Wrap(screen, Stash::default()), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    h
}

#[test]
fn first_frame_happens_once_per_window_and_the_process_duties_once() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(r"C:\mod-made-up\reports")), None, tab);
    let mut h = Harness::new(Wrap(screen, Stash::default()), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    h.render();
    let first = |f: &Fakes| f.posted.take().iter().filter(|m| matches!(m, Msg::FirstFrame)).count();
    assert_eq!(first(&f), 1, "two paints, one first-frame message");
    assert!(h.is_idle());
    h.send(Box::new(Msg::FirstFrame));
    assert!(!h.is_idle(), "the pre-spawn timer is armed");
    assert_eq!(f.icons.rebuild_calls(), 1);
    // A second window over the same Shared: its own timer, but the process-wide duties are done.
    let mut h2 = second_window(&shared);
    h2.send(Box::new(Msg::FirstFrame));
    assert!(!h2.is_idle());
    assert_eq!(f.icons.rebuild_calls(), 1, "the class icons are rebuilt once per process");
}

#[test]
fn first_frame_painted_before_the_hub_attach_still_does_its_work() {
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    // Window 7 is not attached to the hub: EventLoop::open paints before the attach, so the message is dropped.
    let screen = FilesScreen::new(Rc::clone(&shared), 7, Location::Fs(wp(r"C:\mod-made-up\reports")), None, tab);
    let mut h = Harness::new(Wrap(screen, Stash::default()), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    assert!(h.is_idle());
    assert!(!shared.first_frame_done.get());
    f.hub.attach(7, Box::new(Arc::new(files_engine::hub::Collect::new())));
    h.key("Down");
    assert!(!h.is_idle(), "the pre-spawn timer is armed by the next event");
    assert!(shared.first_frame_done.get());
    assert_eq!(f.icons.rebuild_calls(), 1);
    h.key("Down");
    assert_eq!(f.icons.rebuild_calls(), 1, "once");
}

#[test]
fn first_frame_ms_counts_from_the_process_origin_when_given() {
    use std::time::{Duration, Instant};
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let mut screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(r"C:\mod-made-up\reports")), None, tab);
    let origin = Instant::now();
    let later = origin + Duration::from_millis(250);
    // A window opened later: from its own creation (the fake clock stands at 0 then).
    assert_eq!(screen.first_frame_ms(40, later), 40);
    screen.set_origin(origin);
    assert_eq!(screen.first_frame_ms(40, later), 250, "the process start, not the creation of the screen");
    assert_eq!(screen.first_frame_ms(40, origin), 0);
}

#[test]
fn cascade_clamped() {
    let work = RectP { l: 0, t: 0, r: 1920, b: 1040 };
    let from = RectP { l: 100, t: 100, r: 1100, b: 800 };
    assert_eq!(window::cascade_target(from, &[], work, 24), PointP { x: 124, y: 124 });
    // A window already stands one step down: the next goes one step further.
    let one = [PointP { x: 124, y: 124 }];
    assert_eq!(window::cascade_target(from, &one, work, 24), PointP { x: 148, y: 148 });
    // At the right and bottom edges it is pulled back inside the work area.
    let low = RectP { l: 900, t: 400, r: 1900, b: 1030 };
    assert_eq!(window::cascade_target(low, &[], work, 24), PointP { x: 920, y: 410 });
    // Never above or left of the work area.
    let high = RectP { l: -50, t: -20, r: 950, b: 680 };
    assert_eq!(window::cascade_target(high, &[PointP { x: 0, y: 0 }], work, 24), PointP { x: 0, y: 4 });
}

#[test]
fn main_spec_is_custom_caption() {
    let w = folder_window();
    let spec = window::main_spec(&w.shared);
    assert!(matches!(spec.kind, WindowKind::Opaque { caption: Caption::Custom, .. }), "F1b: the custom caption");
    assert_eq!(spec.placement, Some(files_engine::names::MAIN_PLACEMENT), "window 1 keeps its placement");
    assert!(spec.title.ends_with(" Beta"), "the beta stage shows in the title: {}", spec.title);
    w.shared.windows.borrow_mut().push(window::WinRec {
        win: 1,
        kind: window::WinKind::Main,
        wref: None,
        hwnd: None,
        location: None,
    });
    assert_eq!(window::main_spec(&w.shared).placement, None, "later windows open where the system puts them");
}

#[test]
fn start_takes_its_first_batch_and_selects_the_asked_item() {
    let f = Fakes::new();
    let shared = f.shared(UiLang::EN);
    let dir = wp(r"C:\mod-made-up\reports");
    let item = wp(r"C:\mod-made-up\reports\notes.txt");
    let mut screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir.clone()), Some(item), STARTUP_TAB);
    // app.rs's job for this folder ran under Gen(1); its first batch arrived before the window.
    f.enumr.submit(EnumJob { tab: STARTUP_TAB, gen: Gen(1), dir: dir.clone(), cancel: CancelToken::new() });
    screen.adopt_start(CancelToken::new());
    let mut batch = fakes::listing(&reports());
    batch.gen = Gen(1);
    screen.adopt_first_batch(batch);
    let mut h = Harness::new(Wrap(screen, Stash::default()), SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    let w = Win { f, shared, h, stash: Stash::default() };
    assert_eq!(rows(&w), 5, "the first frame has its rows");
    w.h.assert_named();
    // The first event brings the focus and the selection to the item.
    let mut w = w;
    w.pump();
    assert_eq!(w.selected_labels(), ["notes.txt"]);
    assert_eq!(w.focused_label().as_deref(), Some("notes.txt"));
}

// ---- F1a.11: theme and language live -----------------------------------------------------------------------------

#[test]
fn lang_switch_rebuilds_labels() {
    let mut w = folder_window();
    assert!(dump(&w).contains("\"Name\"") && dump(&w).contains("Back"), "{}", dump(&w));
    assert!(has_label(&w, "5 items"));
    let label = |w: &Win| w.fs().commands().get(CommandId(Cmd::Back as u32)).map(|c| c.label.to_string());
    assert_eq!(label(&w).as_deref(), Some("Back"));
    // The loop callback sets the language and tells every window.
    w.shared.lang.set(UiLang::IT);
    *w.shared.fmt.borrow_mut() = Rc::new(Fmt::system(UiLang::IT));
    w.h.send(Box::new(Msg::Lang));
    w.h.render();
    let d = dump(&w);
    assert!(d.contains("Indietro"), "{d}");
    assert!(d.contains("\"Nome\"") && d.contains("\"Ultima modifica\""), "the column titles follow: {d}");
    assert!(has_label(&w, "5 elementi"), "the status line follows: {d}");
    assert_eq!(label(&w).as_deref(), Some("Indietro"), "the command table is rebuilt");
    // Widths and the sort the user chose stay.
    w.h.click(PointL { x: 770, y: CHROME_H + 44 + 16 });
    let sorted = w.fs().tabs().active().sort;
    w.shared.lang.set(UiLang::EN);
    w.h.send(Box::new(Msg::Lang));
    assert_eq!(w.fs().tabs().active().sort, sorted);
    assert!(dump(&w).contains("\"Name\""));
    w.h.assert_named();
}

#[test]
fn high_contrast_cell_renders() {
    use oled_ui::theme::{HcColors, SysOverlay, ThemeSpec};
    let hc = HcColors {
        window: (0, 0, 0),
        window_text: (255, 255, 255),
        highlight: (26, 235, 255),
        highlight_text: (0, 0, 0),
        gray_text: (63, 242, 63),
    };
    let sys = SysOverlay { high_contrast: Some(hc), reduced_motion: false, text_scale: 1.0 };
    let tokens = Tokens::resolve(&ThemeSpec::oled(), &sys);
    let f = Fakes::new();
    f.enumr.queue(&wp(r"C:\mod-made-up\reports"), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(r"C:\mod-made-up\reports")), None, tab);
    let mut h = Harness::new(Wrap(screen, Stash::default()), SizeL { w: 800, h: 500 }, 144, tokens);
    h.render();
    f.pump(&mut h, 1);
    h.key("Down").key("Shift+Down");
    let img = h.render();
    let highlight = 0xFF00_0000 | (26 << 16) | (235 << 8) | 255;
    assert!(img.px.contains(&highlight), "the selected rows use the system highlight");
    h.assert_named();
}

fn plain_window() -> Win {
    let f = Fakes::new();
    f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    window_over(f, REPORTS)
}

/// A listing the navigation cache could hold: `names` in `dir`, finished.
fn cached(dir: &str, names: &[&str]) -> files_engine::navcache::Cached {
    let items: Vec<_> = names.iter().map(|n| (*n, false, 1u64, day(1))).collect();
    let mut l = fakes::FakeListing::new(wp(dir), Gen(0));
    l.append(fakes::listing(&items));
    l.merge_pending();
    files_engine::navcache::Cached {
        listing: Box::new(l),
        anchor: None,
        sort: (files_engine::model::ColKind::Name, oled_ui::model::SortDir::Asc),
    }
}

fn reports_names() -> Vec<String> {
    let mut n: Vec<String> = reports().iter().map(|e| e.0.to_string()).collect();
    n.sort_by_key(|s| (!matches!(s.as_str(), "invoices" | "projects"), s.to_lowercase()));
    n
}

fn id_of_name(w: &Win, n: &str) -> Option<files_engine::listing::EntryId> {
    w.fs().tabs().active().table.model().listing().find(n.as_bytes())
}

const INVOICES: &str = r"C:\mod-made-up\reports\invoices";
const REPORTS: &str = r"C:\mod-made-up\reports";

#[test]
fn cache_hit_keeps_cached_rows_then_merges_by_name() {
    let mut w = plain_window();
    let tab = w.fs().tabs().active().id;
    let mut first = fakes::listing(&[("c1.txt", false, 1, day(1))]);
    first.done = false;
    w.f.enumr.queue(&wp(INVOICES), first);
    let c = cached(INVOICES, &["c1.txt", "c2.txt"]);
    go_cached(&mut w, INVOICES, c);
    assert_eq!(w.labels(), ["c1.txt", "c2.txt"], "the cached rows are shown at once");
    assert_eq!(
        w.fs().hit_state(tab),
        Some(Some(Location::Fs(wp(REPORTS)))),
        "the listing the tab left is held for the cache"
    );
    let c1 = id_of_name(&w, "c1.txt").expect("c1 is listed");
    w.h.advance(100);
    assert_eq!(w.labels(), ["c1.txt", "c2.txt"], "no blank page at 60 ms");
    w.pump();
    assert_eq!(w.labels(), ["c1.txt", "c2.txt"], "the first batch does not replace the cached rows");
    w.f.enumr.queue(&wp(INVOICES), fakes::listing(&[("c3.txt", false, 1, day(1))]));
    w.pump();
    assert_eq!(w.labels(), ["c1.txt", "c3.txt"], "the done listing is merged by name");
    assert_eq!(id_of_name(&w, "c1.txt"), Some(c1), "a survivor keeps its id");
    assert_eq!(w.fs().hit_state(tab), None, "the navigation is confirmed: the held listing went to the cache");
}

#[test]
fn cache_hit_after_a_busy_page_still_gets_the_new_batches() {
    let mut w = plain_window();
    // projects is still loading (its batches have not come) when the user goes on to a cached folder.
    w.h.key("Down").key("Enter");
    w.h.advance(400);
    w.f.enumr.queue(&wp(INVOICES), fakes::listing(&[("c1.txt", false, 1, day(1)), ("c3.txt", false, 1, day(1))]));
    let c = cached(INVOICES, &["c1.txt", "c2.txt"]);
    go_cached(&mut w, INVOICES, c);
    assert_eq!(w.labels(), ["c1.txt", "c2.txt"]);
    w.pump();
    assert_eq!(w.labels(), ["c1.txt", "c3.txt"], "the new generation's rows reached the table");
}

#[test]
fn refused_folder_after_a_hit_shows_the_folder_it_reverts_to() {
    let mut w = plain_window();
    w.f.enumr.queue_error(&wp(INVOICES), EnumError::NotFound);
    go_cached(&mut w, INVOICES, cached(INVOICES, &["c1.txt", "c2.txt"]));
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    w.pump();
    assert!(here(&w).ends_with("reports"), "{}", here(&w));
    assert_eq!(w.labels(), reports_names(), "the folder we are back in is shown, not the refused one's cache");
    w.pump();
    assert_eq!(w.labels(), reports_names());
}

#[test]
fn going_to_the_current_folder_never_takes_the_cache() {
    let mut w = plain_window();
    let tab = w.fs().tabs().active().id;
    go_cached(&mut w, REPORTS, cached(REPORTS, &["old.txt"]));
    assert_eq!(w.labels(), reports_names());
    assert_eq!(w.fs().hit_state(tab), None);
}
