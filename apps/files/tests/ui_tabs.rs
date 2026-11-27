//! Tabs, their menu and commands, the history menus and the navigation cache (F4.1-F4.3, SPEC-files 5.4.8, 5.2.3, 5.5.2):
//! plain tests over the fakes through the kit's Harness. No window, no file system.
#![allow(dead_code)]

use std::rc::Rc;

use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::listing::{EnumError, Gen, WidePath};
use files_engine::model::{ColKind, Listing};
use files_engine::navcache::{self, Cached, MAX_BYTES, MAX_LISTINGS};
use files_engine::options::FilesOptions;
use files_engine::tab::{self, BACKGROUND_BUDGET};
use files_engine::window::{self, FilesScreen, Shared, Shed};
use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::{ListModel, SortDir};
use oled_ui::widgets::TabOut;
use oled_ui::{
    CaptionHit, CommandId, CommandTable, Event, EventCx, LayoutCx, Mods, MouseButton, PaintCx, PointP, Reply, Screen,
    SizeL, SizeP, Tokens, UiLang,
};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a made-up path parses")
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

const REPORTS: &str = r"C:\mod-made-up\reports";
const INVOICES: &str = r"C:\mod-made-up\reports\invoices";

fn reports() -> Vec<(&'static str, bool, u64, i64)> {
    vec![
        ("invoices", true, 0, day(3)),
        ("projects", true, 0, day(9)),
        ("notes.txt", false, 1_200, day(1)),
        ("report-final-v2.pdf", false, 3_400_000, day(5)),
        ("photos-raw.zip", false, 90_000_000, day(40)),
    ]
}

type Call = Box<dyn FnOnce(&mut FilesScreen, &mut EventCx) + Send>;

/// What a test asks the screen to do from inside its own event, where an EventCx exists.
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
        match msg.downcast::<Call>() {
            Ok(call) => {
                (*call)(&mut self.0, cx);
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
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        &self.h.screen().0
    }

    fn call(&mut self, f: impl FnOnce(&mut FilesScreen, &mut EventCx) + Send + 'static) {
        let call: Call = Box::new(f);
        self.h.send(Box::new(call));
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    /// The location of every tab, in order, as the folder's last name.
    fn order(&self) -> Vec<String> {
        self.fs().tabs().iter().map(|t| name_of(t.nav.location())).collect()
    }

    fn active(&self) -> usize {
        let id = self.fs().tabs().active().id;
        self.fs().tabs().iter().position(|t| t.id == id).expect("the active tab is in the list")
    }

    fn labels(&self) -> Vec<String> {
        let m = self.fs().tabs().active().table.model();
        (0..ListModel::len(m)).map(|r| m.label(r).to_string()).collect()
    }

    fn key_of(&self, i: usize) -> u64 {
        u64::from(self.fs().tabs().iter().nth(i).expect("a tab").id.0)
    }

    fn out(&mut self, out: TabOut) {
        self.call(move |s, cx| {
            tab::on_out(s, cx, out);
        });
    }

    fn open(&mut self, dir: &str, activate: bool) {
        let at = Location::Fs(wp(dir));
        self.call(move |s, cx| {
            tab::open_here(s, cx, at, activate);
        });
    }

    fn pick(&mut self, id: u32) {
        self.call(move |s, cx| {
            s.event(cx, &Event::Command(CommandId(id)));
        });
    }
}

fn name_of(l: &Location) -> String {
    match l {
        Location::Fs(p) => p.to_string_lossy().rsplit('\\').next().unwrap_or_default().to_string(),
        other => format!("{other:?}"),
    }
}

/// A window over REPORTS (listed) whose new tabs open on the current folder.
fn window() -> Win {
    let f = Fakes::new();
    f.options.set(FilesOptions { new_tab: "current".into(), ..FilesOptions::default() });
    f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(REPORTS)), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 900, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, shared, h }
}

/// A new tab at the current folder (its listing queued first), listed.
fn new_tab(w: &mut Win) {
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    w.h.key("Ctrl+T");
    w.pump();
}

// ---- F4.1 -----------------------------------------------------------------------------------------------------------

/// A tab opened over a big folder: its listing holds about 1.2 MiB.
fn big_tab(w: &mut Win, seed: u64) {
    w.f.enumr.queue(&wp(REPORTS), fakes::listing_n(12_000, seed));
    w.h.key("Ctrl+T");
    w.pump();
}

fn heap(w: &Win, i: usize) -> usize {
    w.fs().tabs().iter().nth(i).expect("a tab").table.model().listing().heap_bytes()
}

#[test]
fn background_budget_drops_lru_listing_and_keeps_names() {
    let mut w = window();
    w.h.key("Down").key("Shift+Down");
    let picked: Vec<String> = {
        let t = w.fs().tabs().active();
        let m = t.table.model();
        (0..ListModel::len(m))
            .filter(|r| t.table.selection().contains(ListModel::key(m, *r)))
            .map(|r| m.label(r).to_string())
            .collect()
    };
    assert!(!picked.is_empty());
    // The first tab is small; three big ones follow, each over half the budget.
    for seed in [7, 9, 11] {
        big_tab(&mut w, seed);
    }
    assert_eq!(w.active(), 3);
    assert!(heap(&w, 3) > BACKGROUND_BUDGET / 2, "a big listing: {}", heap(&w, 3));
    let tabs = w.fs().tabs();
    let kept_of = |i: usize| tabs.iter().nth(i).expect("tab").kept.clone();
    // The least recently active listings go first, until the background tabs fit: tab 0, then tab 1.
    let kept = kept_of(0).expect("the least recently active listing was dropped");
    assert_eq!(kept.names.len(), picked.len(), "its selection is saved by name");
    assert_eq!(ListModel::len(tabs.iter().next().expect("tab").table.model()), 0);
    assert!(kept_of(1).is_some(), "the next least recent went too");
    assert!(kept_of(2).is_none() && heap(&w, 2) > 0, "the most recent background tab keeps its rows");
    let held: usize = (0..3).map(|i| heap(&w, i)).sum();
    assert!(held <= BACKGROUND_BUDGET, "{held}");
}

#[test]
fn reactivation_restores_selection_and_anchor() {
    let mut w = window();
    w.h.key("Down").key("Down").key("Shift+Down");
    let picked = selected(&w);
    let focused = focused(&w);
    new_tab(&mut w);
    // The first tab loses its listing.
    w.call(|s, _| window::shed(s, Shed::Background));
    assert!(w.fs().tabs().iter().next().expect("tab").kept.is_some());
    // Coming back enumerates again and puts the selection and the focus back when the listing is done.
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    w.h.key("Ctrl+1");
    w.pump();
    assert_eq!(w.active(), 0);
    assert_eq!(selected(&w), picked, "the selection comes back by name");
    assert_eq!(focused_of(&w), focused);
    assert!(w.fs().tabs().active().kept.is_none());
    // A tab that kept its listing shows it again as it was.
    new_tab(&mut w);
    w.h.key("Ctrl+1");
    assert_eq!(selected(&w), picked);
}

fn selected(w: &Win) -> Vec<String> {
    let t = &w.fs().tabs().active().table;
    let m = t.model();
    (0..ListModel::len(m))
        .filter(|r| t.selection().contains(ListModel::key(m, *r)))
        .map(|r| m.label(r).to_string())
        .collect()
}

fn focused(w: &Win) -> Option<String> {
    focused_of(w)
}

fn focused_of(w: &Win) -> Option<String> {
    let t = &w.fs().tabs().active().table;
    let row = t.model().row_of(t.focused()?)?;
    Some(t.model().label(row).to_string())
}

#[test]
fn revalidation_merges_by_name() {
    let mut w = window();
    let notes = w.fs().tabs().active().table.model().listing().find(b"notes.txt").expect("listed");
    new_tab(&mut w);
    // The folder changed while the first tab was in the background.
    let mut changed = reports();
    changed.retain(|e| e.0 != "photos-raw.zip");
    changed.push(("fresh.txt", false, 5, day(0)));
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&changed));
    w.h.key("Ctrl+1");
    assert!(w.labels().contains(&"photos-raw.zip".to_string()), "the old listing is shown at once");
    w.pump();
    let labels = w.labels();
    assert!(labels.contains(&"fresh.txt".to_string()) && !labels.contains(&"photos-raw.zip".to_string()), "{labels:?}");
    assert_eq!(labels.len(), 5, "no duplicates: {labels:?}");
    let now = w.fs().tabs().active().table.model().listing().find(b"notes.txt");
    assert_eq!(now, Some(notes), "a survivor keeps its id");
}

#[test]
fn background_tab_has_no_watch() {
    let mut w = window();
    new_tab(&mut w);
    new_tab(&mut w);
    let with_watch: Vec<bool> = w.fs().tabs().iter().map(|t| t.watch.is_some()).collect();
    assert_eq!(with_watch, [false, false, true], "only the shown tab is watched");
    w.h.key("Ctrl+1");
    let with_watch: Vec<bool> = w.fs().tabs().iter().map(|t| t.watch.is_some()).collect();
    assert_eq!(with_watch, [true, false, false], "the watch follows the shown tab");
    assert_eq!(w.f.watch.watches().len() - w.f.watch.unwatched().len(), 1, "one live watch in the window");
}

// ---- F4.2 -----------------------------------------------------------------------------------------------------------

#[test]
fn ctrl_digits_select() {
    let mut w = window();
    new_tab(&mut w);
    new_tab(&mut w);
    assert_eq!(w.active(), 2);
    w.h.key("Ctrl+1");
    assert_eq!(w.active(), 0);
    w.h.key("Ctrl+2");
    assert_eq!(w.active(), 1);
    w.h.key("Ctrl+8");
    assert_eq!(w.active(), 1, "no effect past the count");
    w.h.key("Ctrl+9");
    assert_eq!(w.active(), 2, "Ctrl+9 is the last tab");
}

#[test]
fn reopen_stack_of_10() {
    let mut w = window();
    let dirs: Vec<String> = (0..12).map(|i| format!(r"C:\mod-made-up\closed{i:02}")).collect();
    for d in &dirs {
        w.f.enumr.queue(&wp(d), fakes::listing(&reports()));
        w.open(d, false);
    }
    w.pump();
    assert_eq!(w.fs().tabs().len(), 13);
    for i in (1..13).rev() {
        let key = w.key_of(i);
        w.out(TabOut::CloseRequested(key));
    }
    assert_eq!(w.order(), ["reports"]);
    // The last ten closed come back, the newest first; the two oldest were forgotten.
    for (n, want) in (2..12).rev().enumerate() {
        w.f.enumr.queue(&wp(&dirs[want]), fakes::listing(&reports()));
        w.h.key("Ctrl+Shift+T");
        assert_eq!(w.order()[w.active()], format!("closed{want:02}"));
        assert_eq!(w.active(), n + 1, "right of the tab that was shown, and shown");
    }
    let n = w.fs().tabs().len();
    w.h.key("Ctrl+Shift+T");
    assert_eq!(w.fs().tabs().len(), n, "the stack is empty");
}

#[test]
fn tab_menu_items_and_effects() {
    let mut w = window();
    new_tab(&mut w);
    new_tab(&mut w);
    w.h.key("Ctrl+2");
    let key = w.key_of(1);
    w.out(TabOut::ContextMenu(key, PointP { x: 120, y: 12 }));
    let items = w.h.menus().last().expect("the tab menu opened").items.clone();
    let rows: Vec<(String, bool)> = items
        .iter()
        .filter_map(|i| match i {
            oled_ui::MenuItem::Custom { label, enabled, .. } => Some((label.to_string(), *enabled)),
            _ => None,
        })
        .collect();
    let want = ["Duplicate", "Close tab", "Close other tabs", "Close tabs to the right", "Reopen closed tab"];
    assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), want);
    assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), [true, true, true, true, false], "nothing closed yet");
    // Duplicate: a third tab right of the one the menu is for, shown.
    w.pick(tab::pick::DUPLICATE);
    assert_eq!(w.fs().tabs().len(), 4);
    assert_eq!(w.active(), 2);
    // Close tabs to the right of the first one.
    let key = w.key_of(0);
    w.out(TabOut::ContextMenu(key, PointP { x: 20, y: 12 }));
    w.pick(tab::pick::CLOSE_RIGHT);
    assert_eq!(w.fs().tabs().len(), 1);
    // They can be reopened.
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&reports()));
    w.pick(tab::pick::REOPEN);
    assert_eq!(w.fs().tabs().len(), 2);
    // Close other tabs.
    let key = w.key_of(1);
    w.out(TabOut::ContextMenu(key, PointP { x: 120, y: 12 }));
    w.pick(tab::pick::CLOSE_OTHERS);
    assert_eq!(w.fs().tabs().len(), 1);
    assert!(w.fs().tabs().can_reopen());
}

#[test]
fn new_tab_opens_home_by_default() {
    let mut w = window();
    *w.shared.opts.borrow_mut() = FilesOptions::default();
    w.h.key("Ctrl+T");
    assert_eq!(w.fs().tabs().active().nav.location(), &Location::Home);
    assert_eq!(w.order(), ["reports", "Home"]);
}

#[test]
fn drag_reorders() {
    let mut w = window();
    new_tab(&mut w);
    new_tab(&mut w);
    let (a, b, c) = (w.key_of(0), w.key_of(1), w.key_of(2));
    let shown = w.fs().tabs().active().id;
    w.out(TabOut::Moved { key: a, to: 2 });
    let keys: Vec<u64> = (0..3).map(|i| w.key_of(i)).collect();
    assert_eq!(keys, [b, c, a]);
    assert_eq!(w.fs().tabs().active().id, shown, "the shown tab stays shown");
}

#[test]
fn ctrl_enter_opens_background_tab() {
    let mut w = window();
    // The first row is the folder "invoices" (folders first).
    w.f.enumr.queue(&wp(INVOICES), fakes::listing(&[("a.txt", false, 1, day(1))]));
    w.h.key("Ctrl+Enter");
    w.pump();
    assert_eq!(w.order(), ["reports", "invoices"]);
    assert_eq!(w.active(), 0, "the shown tab and its focus stay");
    // A file is no folder: nothing opens.
    w.h.key("Down").key("Down").key("Down");
    assert_eq!(selected(&w), ["photos-raw.zip"]);
    w.h.key("Ctrl+Enter");
    assert_eq!(w.fs().tabs().len(), 2);
}

#[test]
fn middle_click_folder_opens_background_tab() {
    let mut w = window();
    let row = |w: &Win, r: usize| {
        let rect = w.fs().tabs().active().table.row_rect(r).expect("a row");
        PointP { x: rect.l + 40, y: rect.t + 4 }
    };
    w.f.enumr.queue(&wp(INVOICES), fakes::listing(&[("a.txt", false, 1, day(1))]));
    let pos = row(&w, 0);
    w.call(move |s, cx| {
        s.event(cx, &Event::Click { pos, button: MouseButton::Middle, clicks: 1, mods: Mods::NONE });
    });
    w.pump();
    assert_eq!(w.order(), ["reports", "invoices"]);
    assert_eq!(w.active(), 0);
    // A file row: nothing.
    let pos = row(&w, 2);
    w.call(move |s, cx| {
        s.event(cx, &Event::Click { pos, button: MouseButton::Middle, clicks: 1, mods: Mods::NONE });
    });
    assert_eq!(w.fs().tabs().len(), 2);
}

// ---- F4.3 -----------------------------------------------------------------------------------------------------------

/// A listing the cache could hold: `names` in `dir`, finished.
fn cached(dir: &str, names: &[&str]) -> Cached {
    let items: Vec<_> = names.iter().map(|n| (*n, false, 1u64, day(1))).collect();
    let mut l = fakes::FakeListing::new(wp(dir), Gen(0));
    l.append(fakes::listing(&items));
    l.merge_pending();
    Cached { listing: Box::new(l), anchor: None, sort: (ColKind::Name, SortDir::Asc) }
}

fn go(w: &mut Win, dir: &str) {
    w.f.enumr.queue(&wp(dir), fakes::listing(&[("a.txt", false, 1, day(1))]));
    let at = Location::Fs(wp(dir));
    w.call(move |s, cx| window::navigate(s, cx, at, None));
    w.pump();
}

fn deep(w: &mut Win, n: usize) -> Vec<String> {
    let dirs: Vec<String> = (0..n).map(|i| format!(r"C:\mod-made-up\step{i:02}")).collect();
    for d in &dirs {
        go(w, d);
    }
    dirs
}

#[test]
fn history_menu_lists_12() {
    let mut w = window();
    let dirs = deep(&mut w, 14);
    let at = oled_ui::Anchor::Point(PointP { x: 20, y: 60 });
    w.call(move |s, cx| {
        assert!(tab::history_menu(s, cx, true, at));
    });
    let items = w.h.menus().last().expect("the back menu opened").items.clone();
    let rows: Vec<(String, String)> = items
        .iter()
        .filter_map(|i| match i {
            oled_ui::MenuItem::Custom { label, note, .. } => {
                Some((label.to_string(), note.as_ref().map(|n| n.to_string()).unwrap_or_default()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 12, "the last twelve");
    assert_eq!(rows[0].0, "step12", "nearest first");
    assert_eq!(rows[0].1, dirs[12], "the path under the name");
    assert_eq!(rows[11].0, "step01");
    // Nothing to go forward to: no menu.
    let opened = w.h.menus().len();
    w.call(move |s, cx| {
        assert!(!tab::history_menu(s, cx, false, at));
    });
    assert_eq!(w.h.menus().len(), opened);
}

#[test]
fn jump_navigates() {
    let mut w = window();
    let dirs = deep(&mut w, 5);
    w.f.enumr.queue(&wp(&dirs[1]), fakes::listing(&[("a.txt", false, 1, day(1))]));
    let at = oled_ui::Anchor::Point(PointP { x: 20, y: 60 });
    w.call(move |s, cx| {
        tab::history_menu(s, cx, true, at);
    });
    // History: reports, step00..step04 (index 0..5); step01 is index 2.
    w.pick(tab::pick::HISTORY + 2);
    w.pump();
    assert_eq!(name_of(w.fs().tabs().active().nav.location()), "step01");
    assert!(w.fs().tabs().active().nav.can_forward());
}

#[test]
fn back_paints_from_cache_then_revalidates() {
    let mut w = window();
    let notes = w.fs().tabs().active().table.model().listing().find(b"notes.txt").expect("listed");
    go(&mut w, INVOICES);
    assert!(w.shared.navcache.borrow().contains(&Location::Fs(wp(REPORTS))), "the listing the tab left is cached");
    // The folder changed meanwhile; its enumeration has not answered yet.
    let tab = w.fs().tabs().active().id;
    w.h.key("Backspace");
    assert_eq!(w.labels().len(), 5, "painted from the cache in the next frame: {:?}", w.labels());
    assert!(w.fs().hit_state(tab).is_some(), "the cached listing is on show beside the running enumeration");
    let mut changed = reports();
    changed.retain(|e| e.0 != "photos-raw.zip");
    changed.push(("fresh.txt", false, 5, day(0)));
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&changed));
    w.pump();
    let labels = w.labels();
    assert!(labels.contains(&"fresh.txt".to_string()) && !labels.contains(&"photos-raw.zip".to_string()), "{labels:?}");
    assert_eq!(w.fs().tabs().active().table.model().listing().find(b"notes.txt"), Some(notes), "survivors keep ids");
    assert_eq!(w.fs().hit_state(tab), None);
}

#[test]
fn cache_cap_16_and_4mib() {
    let w = window();
    for i in 0..17 {
        let dir = format!(r"C:\mod-made-up\c{i:02}");
        navcache::put(&w.shared, &Location::Fs(wp(&dir)), cached(&dir, &["a.txt"]));
    }
    let n = |w: &Win| w.shared.navcache.borrow().len();
    assert_eq!(n(&w), MAX_LISTINGS);
    assert!(!w.shared.navcache.borrow().contains(&Location::Fs(wp(r"C:\mod-made-up\c00"))), "the least recent went");
    // Taking an entry removes it; putting it again makes it the newest.
    let at = Location::Fs(wp(r"C:\mod-made-up\c05"));
    assert!(navcache::take(&w.shared, &at).is_some());
    assert!(navcache::take(&w.shared, &at).is_none());
    navcache::clear(&w.shared);
    assert_eq!(n(&w), 0);
    // Big ones: the heap stays within 4 MiB, the newest stays.
    for i in 0..6 {
        let dir = format!(r"C:\mod-made-up\big{i}");
        let mut l = fakes::FakeListing::new(wp(&dir), Gen(0));
        l.append(fakes::listing_n(12_000, i + 1));
        l.merge_pending();
        let c = Cached { listing: Box::new(l), anchor: None, sort: (ColKind::Name, SortDir::Asc) };
        navcache::put(&w.shared, &Location::Fs(wp(&dir)), c);
        assert!(w.shared.navcache.borrow().heap_bytes() <= MAX_BYTES);
    }
    assert!(w.shared.navcache.borrow().contains(&Location::Fs(wp(r"C:\mod-made-up\big5"))));
    assert!(w.shared.navcache.borrow().len() < 6, "the oldest went to stay within the heap budget");
}

// ---- the two points carried over from the details window ---------------------------------------------------------------

/// The cache answers for INVOICES, then the enumeration of it fails with `err`.
fn hit_then_failure(err: EnumError, wait_ms: u32) -> Win {
    let mut w = window();
    navcache::put(&w.shared, &Location::Fs(wp(INVOICES)), cached(INVOICES, &["c1.txt", "c2.txt"]));
    w.f.enumr.queue_error(&wp(INVOICES), err);
    let at = Location::Fs(wp(INVOICES));
    w.call(move |s, cx| window::navigate(s, cx, at, None));
    assert_eq!(w.labels(), ["c1.txt", "c2.txt"], "the cached rows are shown at once");
    w.h.advance(wait_ms);
    w.pump();
    w
}

fn dump(w: &Win) -> String {
    w.h.a11y_dump()
}

#[test]
fn access_denied_after_a_hit_shows_no_rows_of_the_folder_the_tab_left() {
    let w = hit_then_failure(EnumError::AccessDenied, 0);
    let tab = w.fs().tabs().active().id;
    assert!(name_of(w.fs().tabs().active().nav.location()) == "invoices");
    assert!(w.labels().is_empty(), "the failed folder's empty listing is on show: {:?}", w.labels());
    assert_eq!(w.fs().hit_state(tab), None);
    assert!(
        dump(&w).contains("denied") || dump(&w).contains("Denied") || dump(&w).contains("permission"),
        "{}",
        dump(&w)
    );
    assert!(
        w.shared.navcache.borrow().contains(&Location::Fs(wp(REPORTS))),
        "the listing the tab left went to the cache"
    );
}

#[test]
fn offline_after_a_hit_shows_no_rows_of_the_folder_the_tab_left() {
    let w = hit_then_failure(EnumError::Offline { waited_ms: 4_100 }, 4_100);
    let tab = w.fs().tabs().active().id;
    assert!(w.labels().is_empty(), "{:?}", w.labels());
    assert_eq!(w.fs().hit_state(tab), None);
    assert!(w.shared.navcache.borrow().contains(&Location::Fs(wp(REPORTS))));
}

#[test]
fn late_not_found_after_a_hit_restores_the_old_folder_and_revalidates_it() {
    let mut w = window();
    navcache::put(&w.shared, &Location::Fs(wp(INVOICES)), cached(INVOICES, &["c1.txt", "c2.txt"]));
    w.f.enumr.queue_error(&wp(INVOICES), EnumError::NotFound);
    let at = Location::Fs(wp(INVOICES));
    w.call(move |s, cx| window::navigate(s, cx, at, None));
    let tab = w.fs().tabs().active().id;
    // Slower than the 60 ms of the blank page.
    w.h.advance(100);
    let before = w.fs().tabs().active().table.model().listing().find(b"notes.txt");
    assert_eq!(before, None);
    w.pump();
    assert_eq!(name_of(w.fs().tabs().active().nav.location()), "reports");
    // The folder we came back to is shown (whatever of its rows the restart has delivered by now).
    let mut changed = reports();
    changed.retain(|e| e.0 != "photos-raw.zip");
    changed.push(("fresh.txt", false, 5, day(0)));
    w.f.enumr.queue(&wp(REPORTS), fakes::listing(&changed));
    w.pump();
    let labels = w.labels();
    assert_eq!(labels.len(), 5, "merged by name, no duplicates: {labels:?}");
    assert!(labels.contains(&"fresh.txt".to_string()) && !labels.contains(&"c1.txt".to_string()), "{labels:?}");
    assert_eq!(w.fs().hit_state(tab), None);
    assert!(!w.shared.navcache.borrow().contains(&Location::Fs(wp(INVOICES))), "the refused folder is not cached");
}

#[test]
fn back_at_the_first_entry_leaves_the_cache_and_the_rows_alone() {
    let mut w = window();
    let copy = cached(REPORTS, &["stale.txt"]);
    navcache::put(&w.shared, &Location::Fs(wp(REPORTS)), copy);
    let tab = w.fs().tabs().active().id;
    let rows = w.labels();
    w.h.key("Down").key("Down");
    let picked = selected(&w);
    w.h.key("Backspace");
    w.h.key("Alt+Left");
    w.h.key("Alt+Right");
    assert_eq!(w.labels(), rows, "the live rows stay");
    assert_eq!(selected(&w), picked);
    assert_eq!(w.fs().hit_state(tab), None);
    assert!(w.shared.navcache.borrow().contains(&Location::Fs(wp(REPORTS))), "the copy was not used up");
}
