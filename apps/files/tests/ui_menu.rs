//! Our context menu, the system menu and the toolbar's menus (P3-files-app F2.5, F2.6). Plain tests over the fakes
//! through the kit's Harness: no window, no shell host, no file system (the hang test alone starts the real host, in
//! the CI bench job).

#[cfg(debug_assertions)]
mod common;

use std::rc::Rc;
use std::time::Instant;

use files_engine::fakes::{self, Fakes};
use files_engine::keys::Cmd;
use files_engine::listing::WidePath;
use files_engine::menu::{self, pick, Mapped};
use files_engine::msg::{Fwd, Msg};
use files_engine::preview::fixtures;
use files_engine::services::ReqOwner;
use files_engine::shell_client::{FallbackWhy, ShellReply};
use files_engine::toolmenus;
use files_engine::window::FilesScreen;
use oled_trust::frame::files::{CanonVerb, ItemRef, Location, MenuOutcome, ReqId, ShellEvt, ShellReq};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::ListModel;
use oled_ui::{
    CaptionHit, CommandId, CommandTable, Event, EventCx, LayoutCx, MenuItem, PaintCx, PointL, PointP, Reply, Screen,
    SizeL, SizeP, Tokens, UiLang,
};

const DIR: &str = r"C:\made-up\menu-folder";

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

/// The screen as it is, plus a way to feed it an event from inside its own context (a menu pick).
struct Wrap(FilesScreen);

struct Pick(CommandId);

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
        match msg.downcast::<Pick>() {
            Ok(p) => {
                self.0.event(cx, &Event::Command(p.0));
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

    /// The menu pick as the host would send it.
    fn pick(&mut self, id: u32) {
        self.h.send(Box::new(Pick(CommandId(id))));
        self.pump();
    }

    fn rows(&self) -> usize {
        ListModel::len(self.fs().tabs().active().table.model())
    }

    fn row_of(&self, name: &str) -> usize {
        let m = self.fs().tabs().active().table.model();
        (0..ListModel::len(m)).find(|r| m.label(*r) == name).unwrap_or_else(|| panic!("no row {name}"))
    }

    /// A point inside the row, in logical pixels (the Harness runs at 96 dpi here).
    fn point_of(&self, name: &str) -> PointL {
        let r = self.fs().tabs().active().table.row_rect(self.row_of(name)).expect("the row is in view");
        PointL { x: r.l + 120, y: (r.t + r.b) / 2 }
    }

    /// The requests of the menu host (the type-name lookups of the first rows are not menus).
    fn requests(&self) -> Vec<ShellReq> {
        self.f
            .shell
            .requests()
            .into_iter()
            .filter(|r| matches!(r, ShellReq::Menu { .. } | ShellReq::Exec { .. }))
            .collect()
    }
}

/// The ids of the rows of a menu, separators and headers left out, submenus by their label's position.
fn ids(items: &[MenuItem]) -> Vec<u32> {
    items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Command(id) => Some(id.0),
            MenuItem::Custom { id, .. } => Some(id.0),
            _ => None,
        })
        .collect()
}

fn submenus(items: &[MenuItem]) -> Vec<&Vec<MenuItem>> {
    items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Submenu { items, .. } => Some(items),
            _ => None,
        })
        .collect()
}

fn folder_listing() -> files_engine::listing::Batch {
    fakes::listing(&[
        ("invoices", true, 0, fakes::FT_2026_11_21),
        ("projects", true, 0, fakes::FT_2026_11_21),
        ("notes.txt", false, 1_200, fakes::FT_2026_11_21),
        ("plan.pdf", false, 3_400_000, fakes::FT_2026_11_21),
        ("photos.zip", false, 90_000, fakes::FT_2026_11_21),
    ])
}

fn start(f: Fakes, batch: files_engine::listing::Batch, lang: UiLang) -> Win {
    menu::set_shift_for_tests(Some(false));
    f.enumr.queue(&wp(DIR), batch);
    let shared = f.shared(lang);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(DIR)), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 900, h: 600 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, h }
}

fn window() -> Win {
    start(Fakes::new(), folder_listing(), UiLang::EN)
}

fn is_disabled_with_note(items: &[MenuItem], id: u32) -> bool {
    items.iter().any(|i| matches!(i, MenuItem::Custom { id: x, enabled: false, note: Some(_), .. } if x.0 == id))
}

#[test]
fn menu_items_for_file_folder_background() {
    // A file: Open first, Open with, Pin (disabled until F4.8), Copy as path, Properties, F3's Rename, Delete and
    // Delete permanently (FC-OPS fills ops_ui::f3_items), then Show more options last.
    let mut w = window();
    let at = w.point_of("notes.txt");
    w.h.right_click(at);
    let items = w.h.menus()[0].items.clone();
    let got = ids(&items);
    assert_eq!(got.first(), Some(&(Cmd::Open as u32)), "Open is the default item: {got:?}");
    assert_eq!(
        got,
        [
            Cmd::Open as u32,
            pick::OPEN_WITH,
            pick::PIN,
            Cmd::CopyPath as u32,
            Cmd::Properties as u32,
            Cmd::Rename as u32,
            Cmd::Recycle as u32,
            Cmd::DeletePermanent as u32,
            Cmd::SystemMenu as u32
        ]
    );
    assert!(is_disabled_with_note(&items, pick::PIN));
    assert_eq!(items.last(), Some(&MenuItem::Command(CommandId(Cmd::SystemMenu as u32))));
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 1, "the row was selected first");

    // A folder: Open, Open in new tab (disabled until F4.2), Open in new window, then as files without Open with.
    let mut w = window();
    let at = w.point_of("invoices");
    w.h.right_click(at);
    let items = w.h.menus()[0].items.clone();
    assert_eq!(
        ids(&items),
        [
            Cmd::Open as u32,
            Cmd::OpenNewTab as u32,
            pick::OPEN_NEW_WINDOW,
            pick::PIN,
            Cmd::CopyPath as u32,
            Cmd::Properties as u32,
            Cmd::Rename as u32,
            Cmd::Recycle as u32,
            Cmd::DeletePermanent as u32,
            Cmd::SystemMenu as u32
        ]
    );
    // Open in new tab is live since the tabs landed (F4.2).
    assert!(!is_disabled_with_note(&items, Cmd::OpenNewTab as u32));
    assert!(items.iter().any(|i| matches!(
        i,
        oled_ui::MenuItem::Custom { id, enabled: true, note: None, .. } if id.0 == Cmd::OpenNewTab as u32
    )));

    // Nothing selected: the folder's own menu with its three submenus.
    let mut w = window();
    w.h.right_click(PointL { x: 300, y: 560 });
    let items = w.h.menus()[0].items.clone();
    assert_eq!(
        ids(&items),
        [Cmd::Refresh as u32, Cmd::OpenInExplorer as u32, Cmd::Properties as u32, pick::MORE_BACKGROUND]
    );
    let subs = submenus(&items);
    assert_eq!(subs.len(), 3, "Sort by, View, New");
    assert!(is_disabled_with_note(subs[2], Cmd::NewFolder as u32), "New folder waits for F3");
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 0);
}

#[test]
fn first_level_needs_no_host() {
    let mut w = window();
    let at = w.point_of("plan.pdf");
    w.h.right_click(at);
    assert_eq!(w.h.menus().len(), 1, "our menu is open");
    assert!(w.requests().is_empty(), "no request reached the shell host: {:?}", w.requests());
    // The Menu key opens the same menu on the focused row.
    let mut w = window();
    w.h.key("Home");
    w.h.key("Apps");
    assert_eq!(w.h.menus().len(), 1, "the Menu key opens our menu");
    assert!(w.requests().is_empty());
}

#[test]
fn shift_opens_system_menu() {
    let mut w = window();
    menu::set_shift_for_tests(Some(true));
    let at = w.point_of("notes.txt");
    w.h.right_click(at);
    menu::set_shift_for_tests(Some(false));
    assert!(w.h.menus().is_empty(), "no first level of ours");
    let reqs = w.requests();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    match &reqs[0] {
        ShellReq::Menu { items, extended, .. } => {
            assert!(*extended, "Shift held: the extended verbs");
            assert_eq!(items, &[ItemRef::Path(wp(&format!(r"{DIR}\notes.txt")))]);
        }
        other => panic!("not a menu request: {other:?}"),
    }
    // The menu is open until its answer: a second ask is ignored.
    w.h.key("Shift+F10");
    assert_eq!(w.requests().len(), 1, "one menu at a time");

    // Shift+F10 on the focused row of another window asks for that row.
    let mut w = window();
    w.h.key("Home").key("Shift+F10");
    assert_eq!(w.requests().len(), 1);
    assert!(w.h.menus().is_empty());
}

#[test]
fn intercept_mapping() {
    let dir = wp(DIR);
    let inside = wp(&format!(r"{DIR}\notes.txt"));
    let outside = wp(r"C:\made-up\elsewhere\notes.txt");
    assert_eq!(
        map(CanonVerb::Delete, &[&inside, &outside], &dir),
        Mapped::Delete(vec![inside.clone(), outside.clone()])
    );
    assert_eq!(map(CanonVerb::Rename, &[&inside], &dir), Mapped::Rename(inside.clone()));
    assert_eq!(map(CanonVerb::Rename, &[&outside], &dir), Mapped::Nothing, "only an item of the folder shown");
    assert_eq!(map(CanonVerb::Rename, &[], &dir), Mapped::Nothing);
    for v in [CanonVerb::Cut, CanonVerb::Copy, CanonVerb::Paste, CanonVerb::PasteLink] {
        assert_eq!(map(v, &[&inside], &dir), Mapped::NotInBeta, "{v:?}");
    }

    // Through the reply route: a clipboard verb shows the notice with its button; a dismissal says nothing; a
    // failed or fallen-back menu shows host_down with Retry.
    let mut w = window();
    reply(&mut w, ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Dismissed, items: vec![] });
    reply(&mut w, ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Invoked, items: vec![] });
    assert!(!w.h.a11y_dump().contains("beta") && !w.h.a11y_dump().contains("Windows menu"), "no notice yet");
    reply(
        &mut w,
        ShellEvt::MenuDone {
            req: ReqId(1),
            outcome: MenuOutcome::Intercepted(CanonVerb::Cut),
            items: vec![ItemRef::Path(inside)],
        },
    );
    let dump = w.h.a11y_dump();
    assert!(dump.contains("Not available in this beta"), "{dump}");
    assert!(dump.contains("Open ↗"), "the button: {dump}");

    let mut w = window();
    reply(&mut w, ShellEvt::MenuDone { req: ReqId(2), outcome: MenuOutcome::Failed(-1), items: vec![] });
    let dump = w.h.a11y_dump();
    assert!(dump.contains("Windows menu didn't respond") && dump.contains("Retry"), "{dump}");
    let mut w = window();
    w.h.send(Box::new(Msg::Fwd(Fwd::Shell {
        owner: ReqOwner::Menu { win: 1 },
        reply: ShellReply::Fallback { req: ReqId(3), why: FallbackWhy::Unavailable },
    })));
    w.h.render();
    assert!(w.h.a11y_dump().contains("Windows menu didn't respond"));
}

#[test]
fn intercepted_delete_and_rename_reach_their_flows() {
    // The pure mapping is pinned above; this is the route: an intercepted Delete of the system menu starts the
    // window's own delete flow, an intercepted Rename opens the field.
    let mut w = window();
    let inside = wp(&format!(r"{DIR}\notes.txt"));
    assert!(!w.h.a11y_dump().contains("Changes are off"), "nothing asked yet");
    reply(
        &mut w,
        ShellEvt::MenuDone {
            req: ReqId(1),
            outcome: MenuOutcome::Intercepted(CanonVerb::Delete),
            items: vec![ItemRef::Path(inside.clone())],
        },
    );
    w.pump();
    // Outside the test roots the ops gate refuses with its own line: that line is the proof the request was made.
    let dump = w.h.a11y_dump();
    assert!(dump.contains("Changes are off in this preview build"), "the delete flow ran: {dump}");
    let mut w = window();
    reply(
        &mut w,
        ShellEvt::MenuDone {
            req: ReqId(1),
            outcome: MenuOutcome::Intercepted(CanonVerb::Rename),
            items: vec![ItemRef::Path(inside)],
        },
    );
    assert!(w.fs().tabs().active().table.is_renaming(), "the rename field is open");
}

fn map(v: CanonVerb, paths: &[&WidePath], dir: &WidePath) -> Mapped {
    menu::map_verb(v, paths.iter().map(|p| (*p).clone()).collect(), dir)
}

fn reply(w: &mut Win, e: ShellEvt) {
    w.h.send(Box::new(Msg::Fwd(Fwd::Shell { owner: ReqOwner::Menu { win: 1 }, reply: ShellReply::Evt(e) })));
    w.h.render();
}

#[test]
fn copy_as_path_text() {
    let mut w = window();
    w.h.key("Ctrl+A").key("Ctrl+Shift+C");
    let text = w.h.clipboard().text.clone().expect("the paths were copied");
    let lines: Vec<&str> = text.split("\r\n").collect();
    assert_eq!(lines.len(), 5, "one per line, CRLF: {text:?}");
    assert!(lines.iter().all(|l| l.starts_with('"') && l.ends_with('"')), "quoted: {text:?}");
    assert!(lines.contains(&format!("\"{DIR}\\notes.txt\"").as_str()), "{text:?}");
    assert!(!text.ends_with("\r\n"), "no trailing line break");
    // Nothing selected: the folder itself.
    let mut w = window();
    w.h.key("Ctrl+Shift+C");
    assert_eq!(w.h.clipboard().text.as_deref(), Some(format!("\"{DIR}\"").as_str()));
    // The menu's own pick does the same through the command table.
    let mut w = window();
    let at = w.point_of("plan.pdf");
    w.h.right_click(at);
    w.pick(Cmd::CopyPath as u32);
    assert_eq!(w.h.clipboard().text.as_deref(), Some(format!("\"{DIR}\\plan.pdf\"").as_str()));
}

#[test]
fn sort_menu_items_and_effects() {
    let mut w = window();
    let items = toolmenus::sort_items(w.fs());
    // Name, Date modified, Type, Size, Date created: the columns that sort; then Ascending and Descending.
    assert_eq!(
        ids(&items),
        [
            pick::SORT_COL,
            pick::SORT_COL + 1,
            pick::SORT_COL + 2,
            pick::SORT_COL + 3,
            pick::SORT_COL + 4,
            pick::SORT_ASC,
            pick::SORT_DESC
        ]
    );
    let checked = |items: &[MenuItem]| -> Vec<u32> {
        items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Custom { id, checked: Some(true), .. } => Some(id.0),
                _ => None,
            })
            .collect()
    };
    assert_eq!(checked(&items), [pick::SORT_COL, pick::SORT_ASC]);
    w.pick(pick::SORT_COL + 3);
    w.pick(pick::SORT_DESC);
    assert_eq!(checked(&toolmenus::sort_items(w.fs())), [pick::SORT_COL + 3, pick::SORT_DESC]);
    let m = w.fs().tabs().active().table.model();
    let sizes: Vec<String> = (0..ListModel::len(m)).map(|r| m.label(r).to_string()).collect();
    // Folders stay in front; the files follow from the biggest.
    assert_eq!(sizes[2..], ["plan.pdf", "photos.zip", "notes.txt"], "{sizes:?}");
}

#[test]
fn view_new_and_more_menus() {
    let mut w = window();
    let view = toolmenus::view_items(w.fs());
    assert_eq!(ids(&view), [Cmd::HiddenItems as u32, pick::VIEW_EXTENSIONS, pick::VIEW_COMPACT]);
    let state = |items: &[MenuItem]| -> Vec<Option<bool>> {
        items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Custom { checked, .. } => Some(*checked),
                _ => None,
            })
            .collect()
    };
    assert_eq!(state(&view), [Some(false), Some(true), Some(false)]);
    let tv = toolmenus::toolbar_view_items(w.fs());
    assert_eq!(ids(&tv), [pick::VIEW_DETAILS, Cmd::HiddenItems as u32, pick::VIEW_EXTENSIONS, pick::VIEW_COMPACT]);
    assert_eq!(state(&tv)[0], Some(true), "Details is the checked view");
    w.pick(pick::VIEW_EXTENSIONS);
    w.pick(pick::VIEW_COMPACT);
    assert_eq!(state(&toolmenus::view_items(w.fs())), [Some(false), Some(false), Some(true)]);
    let m = w.fs().tabs().active().table.model();
    assert!(!m.label(w.row_of("notes.txt")).is_empty());
    w.pick(pick::VIEW_EXTENSIONS);
    assert_eq!(state(&toolmenus::view_items(w.fs()))[1], Some(true), "a second pick flips it back");

    let new = toolmenus::new_items(w.fs());
    assert_eq!(ids(&new), [Cmd::NewFolder as u32]);
    assert!(is_disabled_with_note(&new, Cmd::NewFolder as u32));

    let more = toolmenus::more_items(w.fs());
    assert_eq!(
        ids(&more),
        [
            Cmd::Undo as u32,
            Cmd::Redo as u32,
            Cmd::SelectAll as u32,
            Cmd::SelectNone as u32,
            Cmd::Invert as u32,
            Cmd::OpenInExplorer as u32,
            Cmd::About as u32
        ]
    );
    assert!(more.iter().any(|i| matches!(i, MenuItem::Custom { id, enabled: false, .. } if id.0 == Cmd::Undo as u32)));
    // Select all through the menu's id selects every row.
    w.pick(Cmd::SelectAll as u32);
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 5);

    // Select none and Invert through the menu's ids.
    w.pick(Cmd::SelectNone as u32);
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 0);
    w.h.click(w.point_of("notes.txt"));
    w.pick(Cmd::Invert as u32);
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 4);

    // The toolbar's four right buttons open these menus under themselves.
    let mut w = window();
    for (i, want) in [
        toolmenus::new_items(w.fs()),
        toolmenus::sort_items(w.fs()),
        toolmenus::toolbar_view_items(w.fs()),
        toolmenus::more_items(w.fs()),
    ]
    .into_iter()
    .enumerate()
    {
        let r = files_engine::toolbar::right_rects(w.fs())[i];
        w.h.click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
        assert_eq!(w.h.menus().len(), 1, "button {i} opens a menu");
        assert_eq!(w.h.menus()[0].items, want, "button {i}");
        w.h.key("Esc");
        assert!(w.h.menus().is_empty());
    }
}

#[test]
fn background_system_menu_targets_the_folder() {
    let dir = || vec![ItemRef::Path(wp(DIR))];
    let menu_items = |w: &Win| match w.requests().as_slice() {
        [ShellReq::Menu { items, .. }] => items.clone(),
        other => panic!("one menu request expected: {other:?}"),
    };
    // Shift+right-click on the empty space with a file selected: the folder, not the file.
    let mut w = window();
    let file = w.point_of("notes.txt");
    w.h.click(file);
    menu::set_shift_for_tests(Some(true));
    w.h.right_click(PointL { x: 420, y: 450 });
    menu::set_shift_for_tests(Some(false));
    assert_eq!(menu_items(&w), dir());
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 0, "the empty space deselects");

    // The background menu's "Show more options" is the folder's too.
    let mut w = window();
    let file = w.point_of("notes.txt");
    w.h.click(file);
    w.h.right_click(PointL { x: 420, y: 450 });
    assert_eq!(w.h.menus().len(), 1);
    assert!(ids(&w.h.menus()[0].items).contains(&pick::MORE_BACKGROUND));
    w.pick(pick::MORE_BACKGROUND);
    assert_eq!(menu_items(&w), dir());
}

#[test]
fn view_switches_reach_every_later_listing() {
    use files_engine::services::OptionsService;
    let queue = |w: &Win, dir: &str| w.f.enumr.queue(&wp(dir), fixtures::documents());
    let mut w = start(Fakes::new(), fixtures::documents(), UiLang::EN);
    assert_eq!(w.rows(), 9);
    w.h.key("Ctrl+H");
    w.pump();
    assert_eq!(w.rows(), 10);
    // A refresh lists the hidden entry again.
    queue(&w, DIR);
    w.h.key("F5");
    w.pump();
    w.pump();
    assert_eq!(w.rows(), 10, "Refresh keeps the hidden entry");
    // So does a folder entered afterwards.
    let sub = format!(r"{DIR}\invoices");
    queue(&w, &sub);
    let at = w.point_of("invoices");
    w.h.double_click(at);
    w.pump();
    w.pump();
    assert_eq!(w.fs().tabs().active().nav.location(), &Location::Fs(wp(&sub)));
    assert_eq!(w.rows(), 10, "the next folder lists it too");

    // A start with the options on shows them at once.
    let f = Fakes::new();
    let mut o = f.options.get();
    o.hidden_items = true;
    o.compact = true;
    f.options.set(o);
    let mut w = start(f, fixtures::documents(), UiLang::EN);
    w.pump();
    assert_eq!(w.rows(), 10, "hidden items at start");
    let r = w.fs().tabs().active().table.row_rect(0).expect("a row");
    assert_eq!(r.b - r.t, Tokens::oled().size.row_dense, "compact rows at start");
}

#[test]
fn hidden_toggle_changes_visible_rows() {
    let f = Fakes::new();
    let mut w = start(f, fixtures::documents(), UiLang::EN);
    // The folder holds ten entries, one of them hidden.
    let before = w.rows();
    assert_eq!(before, 9, "the hidden item is not listed");
    w.h.key("Ctrl+H");
    w.pump();
    assert_eq!(w.rows(), 10, "Ctrl+H shows it");
    assert!(w.fs().shared().opts.borrow().hidden_items);
    w.pick(Cmd::HiddenItems as u32);
    assert_eq!(w.rows(), 9, "the menu's pick hides it again");
    assert!(!w.fs().shared().opts.borrow().hidden_items);
}

#[test]
fn menus_in_italian_and_every_node_named() {
    let mut w = start(Fakes::new(), folder_listing(), UiLang::IT);
    let at = w.point_of("notes.txt");
    w.h.right_click(at);
    let labels: Vec<String> = w.h.menus()[0]
        .items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Custom { label, .. } => Some(label.to_string()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|l| l == "Apri con…" || l.starts_with("Apri con")), "{labels:?}");
    w.h.assert_named();
}

fn three_files() -> Win {
    let mut w = window();
    let at = w.point_of("notes.txt");
    w.h.click(at);
    w.h.key("Shift+Down").key("Shift+Down");
    w
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_menu_build() {
    let w = three_files();
    assert_eq!(w.fs().tabs().active().table.selection().count(w.rows()), 3);
    let mut us: Vec<f64> = (0..500)
        .map(|_| {
            let t = Instant::now();
            let items = menu::build_context(w.fs());
            let d = t.elapsed().as_secs_f64() * 1_000_000.0;
            assert!(!items.is_empty());
            d
        })
        .collect();
    us.sort_by(f64::total_cmp);
    let p99 = us[us.len() * 99 / 100];
    println!("bench: files menu_build {p99:.1} us");
    assert!(p99 <= 1_000.0 * slack(), "p99 {p99} us over 1,000 us");
}

#[test]
#[ignore = "bench: CI bench job, release, serial"]
fn bench_menu_draw() {
    menu::set_shift_for_tests(Some(false));
    let f = Fakes::new();
    f.enumr.queue(&wp(DIR), folder_listing());
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(DIR)), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 900, h: 600 }, 144, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    let r = h.screen().0.tabs().active().table.row_rect(2).expect("a row");
    let at = PointL { x: r.l + 150, y: (r.t + r.b) / 2 };
    h.click(at);
    h.key("Shift+Down").key("Shift+Down");
    let mut ms: Vec<f64> = (0..100)
        .map(|_| {
            let t = Instant::now();
            h.key("Apps");
            let img = h.menu_image(0).expect("the menu draws");
            let d = t.elapsed().as_secs_f64() * 1000.0;
            assert!(img.w > 0);
            h.key("Esc");
            d
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    let p99 = ms[ms.len() * 99 / 100];
    println!("bench: files menu_draw {p99:.2} ms");
    assert!(p99 <= 16.0 * slack(), "p99 {p99} ms over 16 ms");
}

/// OLED_BENCH_SLACK when it parses as f64 inside 1.0..=4.0, else 1.0.
fn slack() -> f64 {
    let v = std::env::var("OLED_BENCH_SLACK").ok();
    v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| (1.0..=4.0).contains(s)).unwrap_or(1.0)
}

/// A menu request to a host that hangs inside QueryContextMenu: the window keeps painting and answering keys, and the
/// host_down notice shows within the engine's menu deadline (1,500 ms) plus routing. Starts the real shell host (no
/// UI, a test root); the CI bench job runs it in debug, serially.
#[cfg(debug_assertions)]
#[test]
#[ignore = "timing: CI bench job, debug, serial"]
fn timing_menu_hang_keeps_painting() {
    use common::temp::TempRoot;
    use files_engine::services;
    use files_engine::session::Session;
    use files_engine::types::TypeNames;
    use files_engine::window::Shared;
    use std::time::Duration;

    menu::set_shift_for_tests(Some(false));
    let root = TempRoot::new("menuhang");
    let file = root.file("a.txt", b"x");
    let dir = root.path().to_str().expect("a UTF-8 temp path").to_string();
    let mut hooks = common::hooks(&root);
    hooks.env.push(("FILES_FAULT_DLL".into(), common::fault_dll().display().to_string().into()));
    hooks.env.push(("FILES_FAULT_MODE".into(), "hang".into()));
    hooks.env.push(("FILES_FAULT_POINT".into(), "menu".into()));

    let f = Fakes::new();
    let mut svc = f.services();
    svc.shell = services::real_shell(&f.hub, common::files_exe(), Some(hooks));
    let shared = Shared::new(std::sync::Arc::clone(&f.hub), svc, UiLang::EN, Session::default(), TypeNames::empty());
    // The real host ends with the test, passed or failed.
    struct Stop(Rc<Shared>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.services.shell.shutdown();
        }
    }
    let stop = Stop(Rc::clone(&shared));
    f.enumr.queue(&wp(&dir), fakes::listing(&[("a.txt", false, 1, fakes::FT_2026_11_21)]));
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(&dir)), None, tab);
    let mut h = Harness::new(Wrap(screen), SizeL { w: 900, h: 600 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    assert!(file.exists());

    h.key("Home").key("Shift+F10");
    let started = Instant::now();
    let mut notice_at = None;
    while started.elapsed() < Duration::from_millis(2_600) {
        let t = Instant::now();
        h.render();
        let took = t.elapsed();
        assert!(took < Duration::from_millis(100), "a frame took {took:?} while the menu hangs");
        f.pump(&mut h, 1);
        // The window answers a key in between.
        let t = Instant::now();
        h.key("Down");
        assert!(t.elapsed() < Duration::from_millis(100), "a key waited on the host");
        if notice_at.is_none() && h.a11y_dump().contains("Windows menu didn't respond") {
            notice_at = Some(started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let at = notice_at.expect("the host_down notice shows");
    assert!(at < Duration::from_millis(1_600), "the notice came after {at:?}");
    println!("bench: files menu_hang_keeps_painting {} ms", at.as_millis());
    drop(stop);
    assert!(shared.services.shell.host_pid().is_none(), "no host is left");
}
