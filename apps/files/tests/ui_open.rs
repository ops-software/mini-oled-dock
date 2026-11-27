//! Opening, icons in rows and the shell host's lifecycle in the window (P3-files-app F2.1, F2.2, F2.4). Plain tests over
//! the fakes through the kit's Harness: no window, no host, no file system.

use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::listing::{Gen, TabId, WidePath};
use files_engine::model::{ColKind, FilesTable};
use files_engine::msg::{Fwd, Msg};
use files_engine::ops_ui::{self, MutReq};
use files_engine::shell_client::{FallbackWhy, HostState, ShellReply};
use files_engine::status;
use files_engine::window::{FilesScreen, Shared};
use oled_trust::frame::files::{CallKind, ItemRef, Location, Pixels, ReqId, ShellEvt, ShellReq, Verb};
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::{CellOut, ColId, ListModel, TableModel};
use oled_ui::{CommandTable, Event, EventCx, LayoutCx, PaintCx, PointL, Reply, Screen, SizeL, SizeP, Tokens, UiLang};
use std::rc::Rc;

mod common;

const ROOT: &str = r"C:\mod-made-up\reports";

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
}

struct Win {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<FilesScreen>,
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        self.h.screen()
    }

    /// Delivers what the fakes hold back and feeds every message to the window; one more render.
    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
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

    fn rows(&self) -> usize {
        ListModel::len(self.fs().tabs().active().table.model())
    }

    fn execs(&self) -> Vec<(ReqId, Verb, Vec<ItemRef>)> {
        self.f
            .shell
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                ShellReq::Exec { req, verb, items } => Some((req, verb, items)),
                _ => None,
            })
            .collect()
    }

    fn icon_reqs(&self) -> Vec<(ReqId, Gen, Vec<ItemRef>, u16)> {
        self.f
            .shell
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                ShellReq::Icons { req, gen, items, px } => Some((req, gen, items, px)),
                _ => None,
            })
            .collect()
    }

    /// The pixels the row's icon slot holds now; None when the row shows no icon yet.
    fn row_icon(&self, row: usize) -> Option<Vec<u32>> {
        let m: &FilesTable = self.fs().tabs().active().table.model();
        let mut out = CellOut::default();
        m.cell(row, ColId(ColKind::Name as u16), &mut out);
        let icon = out.icon?;
        m.icon(icon, 16).map(|b| b.pixels.clone())
    }
}

fn start_at(f: Fakes, shared: Rc<Shared>, win: u32, tab: TabId, root: &str) -> Win {
    let screen = FilesScreen::new(Rc::clone(&shared), win, Location::Fs(wp(root)), None, tab);
    let mut h = Harness::new(screen, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, win);
    h.render();
    Win { f, shared, h }
}

fn window_over(f: Fakes, root: &str) -> Win {
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    start_at(f, shared, 1, tab, root)
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

/// The folder of reports(), listed, the window shown for a second (the first-frame timers fired).
fn folder_window() -> Win {
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(&reports()));
    f.enumr.queue(&wp(r"C:\mod-made-up\reports\invoices"), fakes::listing(&[("a.txt", false, 1, day(1))]));
    let mut w = window_over(f, ROOT);
    w.h.advance(1_000);
    w.pump();
    w
}

fn path(name: &str) -> ItemRef {
    ItemRef::Path(wp(&format!(r"{ROOT}\{name}")))
}

/// A fake icon of one colour, 16 x 16.
fn pixels(shade: u8) -> Pixels {
    let mut bgra = Vec::with_capacity(16 * 16 * 4);
    for _ in 0..16 * 16 {
        bgra.extend_from_slice(&[shade, 0, 0, 255]);
    }
    Pixels { w: 16, h: 16, bgra: bgra.into_boxed_slice() }
}

fn icon_evt(req: ReqId, gen: Gen, index: u32, shade: u8) -> ShellReply {
    ShellReply::Evt(ShellEvt::Icon { req, gen, index, px: pixels(shade) })
}

// ---- F2.1: opening through the shell host ------------------------------------------------------------------------

#[test]
fn enter_on_file_execs_open() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 1, "{execs:?}");
    assert_eq!((execs[0].1, &execs[0].2), (Verb::Open, &vec![path("notes.txt")]));
    // Several selected files open in one request, in model order; the request is the host's, not the UI's.
    w.h.key("Ctrl+Shift+A").key("Home").key("Down").key("Down").key("Shift+Down").key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 2);
    assert_eq!(execs[1].2, vec![path("notes.txt"), path("photos-raw.zip")]);
}

#[test]
fn enter_on_folder_navigates() {
    let mut w = folder_window();
    w.h.key("Home").key("Enter");
    w.pump();
    match w.fs().tabs().active().nav.location() {
        Location::Fs(p) => assert!(p.to_string_lossy().ends_with("invoices"), "{}", p.to_string_lossy()),
        other => panic!("{other:?}"),
    }
    assert!(w.execs().is_empty(), "a folder is never handed to the host");
}

#[test]
fn more_than_sixteen_opens_only_the_focused_one() {
    let f = Fakes::new();
    let names: Vec<String> = (0..20).map(|i| format!("doc-{i:02}.txt")).collect();
    let rows: Vec<(&str, bool, u64, i64)> = names.iter().map(|n| (n.as_str(), false, 10, day(1))).collect();
    f.enumr.queue(&wp(ROOT), fakes::listing(&rows));
    let mut w = window_over(f, ROOT);
    w.h.key("Home").key("Down").key("Ctrl+A").key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].2.len(), 1, "{:?}", execs[0].2);
}

#[test]
fn properties_on_selection_and_on_background() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Alt+Enter");
    let execs = w.execs();
    assert_eq!((execs[0].1, &execs[0].2), (Verb::Properties, &vec![path("notes.txt")]));
    w.h.key("Ctrl+Shift+A").key("Alt+Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 2);
    assert_eq!((execs[1].1, &execs[1].2), (Verb::Properties, &vec![ItemRef::Path(wp(ROOT))]), "the folder itself");
    w.h.key("Ctrl+Shift+O");
    let execs = w.execs();
    assert_eq!((execs[2].1, &execs[2].2), (Verb::OpenInExplorer, &vec![ItemRef::Path(wp(ROOT))]));
}

#[test]
fn exec_failure_shows_notice() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Enter");
    let req = w.execs()[0].0;
    assert!(w.has_label("5 items"));
    // ERROR_CANCELLED: the user closed the verb's own dialog; nothing to say.
    w.f.shell.reply(ShellReply::Evt(ShellEvt::ExecDone { req, hr: 0x8007_04C7_u32 as i32 }));
    w.pump();
    assert!(w.has_label("5 items"), "a cancelled verb is silent: {}", w.dump());
    w.h.key("Enter");
    let req = w.execs()[1].0;
    w.f.shell.reply(ShellReply::Evt(ShellEvt::ExecDone { req, hr: 0x8007_0002_u32 as i32 }));
    w.pump();
    assert!(!w.has_label("5 items"), "the notice takes the place of the counts: {}", w.dump());
    // A success shows nothing.
    w.h.advance(7_000);
    w.h.key("Enter");
    let req = w.execs()[2].0;
    w.f.shell.reply(ShellReply::Evt(ShellEvt::ExecDone { req, hr: 0 }));
    w.pump();
    assert!(w.has_label("5 items"));
}

#[test]
fn fallback_shows_retry_and_retry_resends() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Enter");
    let req = w.execs()[0].0;
    w.f.shell.reply(ShellReply::Fallback { req, why: FallbackWhy::Deadline });
    w.pump();
    assert!(w.has_label("Windows menu didn't respond"), "{}", w.dump());
    assert!(w.has_button("Retry (F5)"), "{}", w.dump());
    // The button runs the last request again: same verb and items, a new request id.
    for _ in 0..40 {
        if w.h.focused().is_some_and(|n| n.owner() == status::ID_ACTION) {
            break;
        }
        w.h.key("Tab");
    }
    w.h.key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 2, "{execs:?}");
    assert_eq!((&execs[1].1, &execs[1].2), (&execs[0].1, &execs[0].2));
    assert_ne!(execs[1].0, req);
    assert!(!w.has_button("Retry (F5)"));
}

#[test]
fn a_late_fallback_of_an_older_exec_does_not_offer_the_newer_one() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Enter");
    w.h.key("Down").key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 2, "{execs:?}");
    assert_ne!(execs[0].2, execs[1].2, "two different rows");
    w.f.shell.reply(ShellReply::Fallback { req: execs[0].0, why: FallbackWhy::HostDied });
    w.pump();
    assert!(w.has_label("Windows menu didn't respond"), "{}", w.dump());
    assert!(!w.has_button("Retry (F5)"), "the first request is not the remembered one: {}", w.dump());
    w.h.advance(7_000);
    w.f.shell.reply(ShellReply::Fallback { req: execs[1].0, why: FallbackWhy::HostDied });
    w.pump();
    assert!(w.has_button("Retry (F5)"), "the latest request's own fallback offers it: {}", w.dump());
}

#[test]
fn an_older_fallback_after_the_newer_one_withdraws_the_retry() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Down").key("Enter");
    w.h.key("Down").key("Enter");
    let execs = w.execs();
    assert_eq!(execs.len(), 2, "{execs:?}");
    w.f.shell.reply(ShellReply::Fallback { req: execs[1].0, why: FallbackWhy::HostDied });
    w.pump();
    assert!(w.has_button("Retry (F5)"), "{}", w.dump());
    w.f.shell.reply(ShellReply::Fallback { req: execs[0].0, why: FallbackWhy::HostDied });
    w.pump();
    assert!(w.has_label("Windows menu didn't respond"), "{}", w.dump());
    assert!(!w.has_button("Retry (F5)"), "the visible notice offers no Retry: {}", w.dump());
    w.h.key("F5");
    assert_eq!(w.execs().len(), 2, "F5 must not resend");
}

#[test]
fn disabled_fallback_names_the_extension() {
    let mut w = folder_window();
    w.f.shell.disable(CallKind::Exec, "txt");
    w.h.key("Home").key("Down").key("Down").key("Enter");
    let req = w.execs()[0].0;
    w.f.shell.reply(ShellReply::Fallback { req, why: FallbackWhy::Disabled });
    w.pump();
    assert!(w.has_label("Windows menu didn't respond · .txt"), "{}", w.dump());
    assert!(!w.has_button("Retry (F5)"), "a tripped breaker is not retried");
}

#[test]
fn open_never_blocks() {
    let mut w = folder_window();
    // A host that never answers.
    w.h.key("Home").key("Down").key("Down").key("Enter");
    w.h.key("Down");
    w.pump();
    assert_eq!(w.execs().len(), 1);
    w.h.advance(10_000);
    assert!(w.h.is_idle(), "an unanswered request holds no timer and no wake-up");
    assert!(w.has_label("5 items") || w.dump().contains("items"), "{}", w.dump());
}

// ---- F2.2: icons in rows -----------------------------------------------------------------------------------------

#[test]
fn class_icon_pending_then_ready_repaints_rows() {
    let mut w = folder_window();
    let txt_row = 2;
    assert!(w.row_icon(txt_row).is_none(), "the generic slot waits for its icon");
    // The real order: the wake runs while the icon is still pending, then the Bump drain_engines posts.
    w.h.send(Box::new(Msg::EnginesWake));
    w.f.icons.set_icon(b"txt");
    w.h.send(Box::new(Msg::Fwd(Fwd::Bump)));
    w.h.advance(5_000);
    w.h.render();
    assert!(w.row_icon(txt_row).is_some(), "the rows show the icon once it is Ready");
    let noted = w.f.icons.noted();
    assert!(noted.iter().any(|e| &**e == b"txt".as_slice()), "{noted:?}");
}

#[test]
fn icons_only_through_set_icon() {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut files = vec![format!("{root}/src/model.rs")];
    for e in std::fs::read_dir(format!("{root}/src/view")).unwrap() {
        files.push(e.unwrap().path().to_string_lossy().into_owned());
    }
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for bad in ["services.icons", "services.shell", "IconService", "ShellService", "ShellReq", "ClassIcons"] {
            assert!(!text.contains(bad), "{f} names {bad}: the model and the views ask no icon service");
        }
    }
    let model = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(model.matches("icons.put(").count(), 1, "the icon table is written in one place: set_icon");
}

fn exe_window(n: usize) -> Win {
    let f = Fakes::new();
    let names: Vec<String> = (0..n).map(|i| format!("tool-{i:03}.exe")).collect();
    let rows: Vec<(&str, bool, u64, i64)> = names.iter().map(|s| (s.as_str(), false, 10, day(1))).collect();
    f.enumr.queue(&wp(ROOT), fakes::listing(&rows));
    let mut w = window_over(f, ROOT);
    w.h.advance(1_000);
    w.pump();
    w
}

#[test]
fn per_item_requested_for_visible_rows_only() {
    let f = Fakes::new();
    let mut names: Vec<String> =
        vec!["a-tool.exe".into(), "b-link.lnk".into(), "c-notes.txt".into(), "d-site.url".into()];
    // Kinds whose icon the shell builds from text inside the file, a link, and a dehydrated cloud file.
    names.extend(["b2-console.msc".into(), "b3-app.appref-ms".into(), "b4-symlink.exe".into(), "b5-cloud.exe".into()]);
    names.extend((0..80).map(|i| format!("e-fill-{i:02}.txt")));
    names.push("z-late.exe".into());
    names.push("y-folder".into());
    let rows: Vec<(&str, bool, u64, i64)> = names.iter().map(|s| (s.as_str(), s == "y-folder", 10, day(1))).collect();
    let mut batch = fakes::listing(&rows);
    // The last entry is a folder with the system attribute: a customised one.
    if let Some(e) = batch.entries.last_mut() {
        e.attrs = 0x10 | 0x4;
    }
    for (i, n) in names.iter().enumerate() {
        match n.as_str() {
            "b4-symlink.exe" => {
                batch.entries[i].attrs = 0x420;
                batch.entries[i].reparse_tag = 0xA000_000C;
            }
            "b5-cloud.exe" => batch.entries[i].attrs = 0x40_0420,
            _ => {}
        }
    }
    f.enumr.queue(&wp(ROOT), batch);
    let mut w = window_over(f, ROOT);
    w.h.advance(1_000);
    w.pump();
    let reqs = w.icon_reqs();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    let items = &reqs[0].2;
    // The folder sorts first and is customised; the exe, lnk and url rows are visible; the late exe is not.
    assert!(items.contains(&path("a-tool.exe")), "{items:?}");
    assert!(!items.contains(&path("c-notes.txt")) && !items.contains(&path("z-late.exe")), "{items:?}");
    // What names an icon location outside itself (shortcuts, desktop.ini folders) is never sent: the shell would
    // resolve and load that location, which can be a remote one.
    for name in
        ["b-link.lnk", "d-site.url", "y-folder", "b2-console.msc", "b3-app.appref-ms", "b4-symlink.exe", "b5-cloud.exe"]
    {
        assert!(!items.contains(&path(name)), "{name} must keep its class or folder icon: {items:?}");
    }
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(reqs[0].3, 16, "the kit's icon size at 96 dpi");
    // Asking again with nothing new on screen sends nothing.
    w.h.key("Down");
    assert_eq!(w.icon_reqs().len(), 1);
}

#[test]
fn per_item_never_requested_in_a_remote_folder() {
    let root = r"\\server-made-up\share";
    let f = Fakes::new();
    f.enumr.queue(&wp(root), fakes::listing(&[("tool.exe", false, 10, day(1)), ("pic.ico", false, 10, day(1))]));
    let mut w = window_over(f, root);
    w.h.advance(1_000);
    w.pump();
    assert!(w.rows() >= 2, "the listing is shown");
    assert!(w.icon_reqs().is_empty(), "{:?}", w.icon_reqs());
}

#[test]
fn item_icons_wait_for_the_host_warm_up() {
    let f = Fakes::new();
    let rows = [("tool.exe", false, 10, day(1)), ("notes.txt", false, 10, day(1))];
    f.enumr.queue(&wp(ROOT), fakes::listing(&rows));
    let mut w = window_over(f, ROOT);
    w.h.advance(499);
    w.pump();
    w.h.key("Down");
    assert!(w.icon_reqs().is_empty(), "no icon request before FirstFrame + 500 ms: {:?}", w.icon_reqs());
    assert_eq!(w.f.shell.prespawns(), 0);
    w.h.advance(2);
    assert_eq!(w.f.shell.prespawns(), 1);
    assert_eq!(w.icon_reqs().len(), 1, "one request once the host warm-up began");
}

#[test]
fn per_item_reply_sets_the_row_icon() {
    let mut w = exe_window(60);
    let (req, gen, items, _) = w.icon_reqs().remove(0);
    assert!(w.row_icon(0).is_none());
    w.f.shell.reply(icon_evt(req, gen, 0, 200));
    w.f.shell.reply(ShellReply::Evt(ShellEvt::ReqDone { req }));
    w.pump();
    assert_eq!(items.len().min(2), 2);
    let px = w.row_icon(0).expect("the row shows its own icon");
    assert_eq!(px[0], 0xFF00_00C8, "premultiplied 0xAARRGGBB of the reply");
    assert!(w.row_icon(1).is_none(), "the other rows keep waiting");
    // A finished request is not asked for again.
    w.h.key("Down");
    assert_eq!(w.icon_reqs().len(), 1);
}

#[test]
fn scroll_cancels_older_gen() {
    let mut w = exe_window(200);
    let first = w.icon_reqs().remove(0);
    w.h.key("End");
    w.pump();
    let reqs = w.icon_reqs();
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    let second = &reqs[1];
    assert!(second.1 .0 > first.1 .0, "newer generation");
    // The cancel comes just before the new request and names the new generation (the host drops older ones).
    let all = w.f.shell.requests();
    let at = all.iter().position(|r| matches!(r, ShellReq::Icons { gen, .. } if *gen == second.1)).unwrap();
    assert!(matches!(all[at - 1], ShellReq::CancelGen { gen } if gen == second.1), "{all:?}");
    assert!(second.2.iter().all(|i| !first.2.contains(i)), "the new viewport holds other rows");
}

#[test]
fn stale_reply_dropped() {
    let mut w = exe_window(200);
    let first = w.icon_reqs().remove(0);
    w.h.key("End");
    w.pump();
    let second = w.icon_reqs().remove(1);
    // An image of the first request lands after the scroll: dropped.
    w.f.shell.reply(icon_evt(first.0, first.1, 0, 90));
    w.pump();
    assert!(w.row_icon(0).is_none());
    // The current one lands on the last page.
    let last = w.rows() - 1;
    let n = second.2.len();
    w.f.shell.reply(icon_evt(second.0, second.1, (n - 1) as u32, 91));
    w.pump();
    assert!(w.row_icon(last).is_some());
}

#[test]
fn reply_for_a_listing_the_tab_left_is_dropped() {
    let mut w = exe_window(60);
    let (req, gen, _, _) = w.icon_reqs().remove(0);
    w.f.enumr.queue(&wp(r"C:\mod-made-up"), fakes::listing(&[("reports", true, 0, day(1))]));
    w.h.key("Alt+Up");
    w.pump();
    w.f.shell.reply(icon_evt(req, gen, 0, 90));
    w.pump();
    assert!(w.row_icon(0).is_none());
}

#[test]
fn lru_cap() {
    let mut w = exe_window(700);
    // The rows that got an icon, in the order they arrived (the file name carries the row).
    let mut order: Vec<usize> = Vec::new();
    let mut answered = 0;
    for _ in 0..400 {
        if order.len() >= 640 {
            break;
        }
        let reqs = w.icon_reqs();
        if reqs.len() > answered {
            let (req, gen, items, _) = reqs[reqs.len() - 1].clone();
            answered = reqs.len();
            for (i, it) in items.iter().enumerate() {
                let ItemRef::Path(p) = it else { continue };
                let name = p.to_string_lossy();
                let n: usize =
                    name.rsplit_once("tool-").and_then(|(_, r)| r[..3].parse().ok()).expect("a made-up name");
                order.push(n);
                w.f.shell.reply(icon_evt(req, gen, i as u32, (n % 250) as u8 + 1));
            }
            w.f.shell.reply(ShellReply::Evt(ShellEvt::ReqDone { req }));
            w.pump();
        }
        w.h.key("PageDown");
        w.pump();
    }
    assert!(order.len() >= 600, "{}", order.len());
    let (oldest, newest, middle) = (order[0], order[order.len() - 1], order[order.len() - 400]);
    assert!(w.row_icon(oldest).is_none(), "the oldest of the 512 icons the table keeps is gone (row {oldest})");
    assert!(w.row_icon(newest).is_some(), "the newest is there");
    assert!(w.row_icon(middle).is_some());
}

#[test]
fn item_icons_do_not_outlive_their_listing() {
    let mut w = exe_window(60);
    let (req, gen, _, _) = w.icon_reqs().remove(0);
    w.f.shell.reply(icon_evt(req, gen, 0, 200));
    w.f.shell.reply(ShellReply::Evt(ShellEvt::ReqDone { req }));
    w.pump();
    assert!(w.row_icon(0).is_some(), "the exe row shows its own icon");
    // Another folder: its row 0 is a text file and must not show the exe's icon.
    w.f.enumr
        .queue(&wp(r"C:\mod-made-up"), fakes::listing(&[("a.txt", false, 1, day(1)), ("b.txt", false, 1, day(1))]));
    w.h.key("Alt+Up");
    w.pump();
    assert_eq!(w.rows(), 2);
    assert!(w.row_icon(0).is_none(), "an entry id of the old listing must not pick up its icon");
    // Back to the first listing: its icons were dropped with it and are asked for again.
    let before = w.icon_reqs().len();
    let again: Vec<String> = (0..60).map(|i| format!("tool-{i:03}.exe")).collect();
    let again: Vec<(&str, bool, u64, i64)> = again.iter().map(|s| (s.as_str(), false, 10, day(1))).collect();
    w.f.enumr.queue(&wp(ROOT), fakes::listing(&again));
    w.h.key("Alt+Left");
    w.pump();
    assert_eq!(w.rows(), 60);
    assert!(w.row_icon(0).is_none(), "the dropped icon is not shown for a listing that never got it again");
    assert!(w.icon_reqs().len() > before, "the exe rows are asked for again");
}

#[test]
fn a_cancel_does_not_lose_the_other_windows_icons() {
    let names: Vec<String> = (0..200).map(|i| format!("tool-{i:03}.exe")).collect();
    let rows: Vec<(&str, bool, u64, i64)> = names.iter().map(|s| (s.as_str(), false, 10, day(1))).collect();
    let (mut w1, mut w2) = two_windows_over(&rows);
    w1.f.pump(&mut w1.h, 1);
    w1.f.pump(&mut w2.h, 2);
    w1.h.render();
    w2.h.render();
    let icon_reqs = |w: &Win| w.f.shell.requests().into_iter().filter(|r| matches!(r, ShellReq::Icons { .. })).count();
    assert_eq!(icon_reqs(&w1), 2, "each window asked for its visible rows: {:?}", w1.f.shell.requests());
    // Window 1 scrolls while window 2's request is on the wire: the host's cancel is process-wide, so none is sent.
    w1.h.key("End");
    w1.f.pump(&mut w1.h, 1);
    let all = w1.f.shell.requests();
    assert!(all.iter().any(|r| matches!(r, ShellReq::Icons { .. })) && icon_reqs(&w1) == 3, "{all:?}");
    assert!(!all.iter().any(|r| matches!(r, ShellReq::CancelGen { .. })), "{all:?}");
}

/// FilesScreen inside a screen that runs ops_ui::request for every MutReq it receives (request needs an EventCx).
struct Requester {
    s: FilesScreen,
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
                ops_ui::request(&mut self.s, cx, *r);
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
fn retry_resends_only_for_its_own_notice() {
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(ROOT)), None, tab);
    let mut h = Harness::new(Requester { s: screen }, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.advance(1_000);
    f.pump(&mut h, 1);
    h.render();
    h.key("Home").key("Down").key("Down").key("Enter");
    let execs = |f: &Fakes| f.shell.requests().into_iter().filter(|r| matches!(r, ShellReq::Exec { .. })).count();
    let req =
        f.shell.requests().into_iter().find_map(|r| if let ShellReq::Exec { req, .. } = r { Some(req) } else { None });
    f.shell.reply(ShellReply::Fallback { req: req.expect("the open"), why: FallbackWhy::Deadline });
    f.pump(&mut h, 1);
    h.render();
    // The operations helper is gone: its own notice (with its own Retry) replaces the Exec notice.
    let root = common::temp::TempRoot::new("retry_own_notice");
    let file = wp(&root.file("x.txt", b"x").to_string_lossy());
    f.ops.set_fail(Some(files_engine::ops_client::OpsError::Unavailable("gone")));
    h.send(Box::new(MutReq::Recycle(vec![file])));
    h.render();
    let dump = h.a11y_dump();
    assert!(dump.lines().any(|l| l.trim_start().starts_with("Button \"Retry (F5)\"")), "{dump}");
    for _ in 0..40 {
        if h.focused().is_some_and(|n| n.owner() == status::ID_ACTION) {
            break;
        }
        h.key("Tab");
    }
    h.key("Enter");
    assert_eq!(execs(&f), 1, "the other notice's Retry must not open the file again");
}

// ---- F2.4: the shell host's lifecycle ----------------------------------------------------------------------------

#[test]
fn prespawn_after_first_frame_only() {
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(&reports()));
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(wp(ROOT)), None, tab);
    let mut h = Harness::new(screen, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    assert_eq!(f.shell.prespawns(), 0, "not at creation");
    h.advance(2_000);
    assert_eq!(f.shell.prespawns(), 0, "no first frame yet: no timer");
    h.render();
    f.pump(&mut h, 1);
    assert_eq!(f.shell.prespawns(), 0, "the first frame itself spawns nothing");
    h.advance(499);
    assert_eq!(f.shell.prespawns(), 0);
    h.advance(2);
    assert_eq!(f.shell.prespawns(), 1, "one prespawn 500 ms after the first frame");
    h.advance(5_000);
    assert_eq!(f.shell.prespawns(), 1);
}

#[test]
fn right_down_spawns_when_down() {
    let mut w = folder_window();
    let before = w.f.shell.prespawns();
    w.f.shell.set_state(HostState::Down);
    w.h.right_click(PointL { x: 300, y: 200 });
    assert_eq!(w.f.shell.prespawns(), before + 1, "a right button down in the list warms the host");
    w.f.shell.set_state(HostState::Ready { pid: 4242 });
    w.h.right_click(PointL { x: 300, y: 200 });
    assert_eq!(w.f.shell.prespawns(), before + 1, "a host that is up needs nothing");
}

#[test]
fn menu_off_disables_more_options() {
    let w = folder_window();
    assert!(files_engine::open::more_options_enabled(&w.shared));
    w.f.shell.set_state(HostState::MenuOff);
    assert!(!files_engine::open::more_options_enabled(&w.shared), "the breaker of the menu is open");
    w.f.shell.set_state(HostState::Ready { pid: 1 });
    assert!(files_engine::open::more_options_enabled(&w.shared));
}

/// Two windows over one Shared: window 1 first, window 2 listed in its own folder.
fn two_windows() -> (Win, Win) {
    two_windows_over(&reports())
}

fn two_windows_over(rows: &[(&str, bool, u64, i64)]) -> (Win, Win) {
    let f = Fakes::new();
    f.enumr.queue(&wp(ROOT), fakes::listing(rows));
    let shared = f.shared(UiLang::EN);
    let (t1, t2) = (shared.new_tab(), shared.new_tab());
    let mut w1 = start_at(f, Rc::clone(&shared), 1, t1, ROOT);
    // Window 2 shares window 1's fakes; its messages go to its own Collect.
    w1.f.attach_window(2);
    w1.f.enumr.queue(&wp(ROOT), fakes::listing(rows));
    let screen = FilesScreen::new(Rc::clone(&shared), 2, Location::Fs(wp(ROOT)), None, t2);
    let mut h2 = Harness::new(screen, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h2.render();
    w1.f.pump(&mut h2, 2);
    h2.render();
    w1.h.advance(1_000);
    h2.advance(1_000);
    let w2 = Win { f: Fakes::new(), shared, h: h2 };
    (w1, w2)
}

#[test]
fn replies_reach_their_window() {
    let (mut w1, mut w2) = two_windows();
    assert_eq!((w1.rows(), w2.rows()), (5, 5));
    // Window 2 opens a file; the host answers with a failure.
    w2.h.key("Home").key("Down").key("Down").key("Enter");
    let req = {
        let shell = &w1.f.shell;
        let reqs: Vec<_> = shell
            .requests()
            .into_iter()
            .filter_map(|r| if let ShellReq::Exec { req, .. } = r { Some(req) } else { None })
            .collect();
        assert_eq!(reqs.len(), 1);
        reqs[0]
    };
    w1.f.shell.reply(ShellReply::Evt(ShellEvt::ExecDone { req, hr: 0x8007_0002_u32 as i32 }));
    w1.f.pump(&mut w1.h, 1);
    w1.f.pump(&mut w2.h, 2);
    w1.h.render();
    w2.h.render();
    assert!(w1.has_label("5 items"), "window 1 shows nothing of window 2's request: {}", w1.dump());
    assert!(!w2.has_label("5 items"), "window 2 shows the failure: {}", w2.dump());
}

#[test]
fn replies_for_closed_window_dropped() {
    let (mut w1, mut w2) = two_windows();
    w2.h.key("Home").key("Down").key("Down").key("Enter");
    let req =
        w1.f.shell
            .requests()
            .into_iter()
            .find_map(|r| if let ShellReq::Exec { req, .. } = r { Some(req) } else { None })
            .expect("window 2's request");
    // Window 2 closes: its hub entry goes with it.
    w1.shared.hub.detach(2);
    w1.f.shell.reply(ShellReply::Evt(ShellEvt::ExecDone { req, hr: 0x8007_0002_u32 as i32 }));
    w1.f.pump(&mut w1.h, 1);
    w1.h.render();
    assert!(w1.has_label("5 items"), "the reply of a closed window reaches nobody: {}", w1.dump());
    assert!(w1.shared.req_owner.borrow().is_empty() || !w1.shared.req_owner.borrow().contains_key(&req));
}
