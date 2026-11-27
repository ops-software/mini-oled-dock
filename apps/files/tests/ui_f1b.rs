//! The chrome of the details window (P3-files-app F1b): the tab strip in its row or in the caption, the breadcrumb and the
//! basic tabs. Plain tests over the fakes through the kit's Harness: no window, no file system.

use files_engine::chrome::{self, hit_of};
use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::listing::{TabId, WidePath};
use files_engine::pathline::segments;
use files_engine::tr::{k, t};
use files_engine::window::{FilesScreen, LoopReq, Show};
use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::model::ListModel;
use oled_ui::{CaptionHit, PointP, Scale, SizeL, Tokens, UiLang};

/// The tab row the harness gives a system-caption window (TAB_ROW_H).
const ROW_H: i32 = 36;
const DIR: &str = r"C:\mod-made-up\reports";

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn day(n: i64) -> i64 {
    FT_2026_11_21 - n * 864_000_000_000
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

struct Win {
    f: Fakes,
    h: Harness<FilesScreen>,
}

impl Win {
    fn fs(&self) -> &FilesScreen {
        self.h.screen()
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    /// Queues the folder's listing once more (each enumeration consumes one) and presses the chord.
    fn key_listing(&mut self, chord: &str) {
        self.f.enumr.queue(&wp(DIR), fakes::listing(&reports()));
        self.h.key(chord);
        self.pump();
    }

    fn ids(&self) -> Vec<TabId> {
        self.fs().tabs().iter().map(|t| t.id).collect()
    }

    fn active(&self) -> TabId {
        self.fs().tabs().active().id
    }

    fn dump(&self) -> String {
        self.h.a11y_dump()
    }
}

fn over(f: Fakes, at: Location, size: SizeL) -> Win {
    // These tests open new tabs on the folder of the tab (the option's other value is Home, tested with the tabs).
    f.options.set(files_engine::options::FilesOptions { new_tab: "current".into(), ..Default::default() });
    let shared = f.shared(UiLang::EN);
    let tab = shared.new_tab();
    let screen = FilesScreen::new(shared, 1, at, None, tab);
    let mut h = Harness::new(screen, size, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    Win { f, h }
}

fn folder_window() -> Win {
    let f = Fakes::new();
    f.enumr.queue(&wp(DIR), fakes::listing(&reports()));
    over(f, Location::Fs(wp(DIR)), SizeL { w: 800, h: 500 })
}

// ---- F1b.1: the strip or the row

#[test]
fn strip_and_row_geometry() {
    for dpi in [96, 120, 144, 192] {
        let sc = Scale { dpi };
        let px = |v: i32| sc.px(v);
        let w = px(900);
        // Custom: a 40 px strip, the buttons 3 x 46 at the right end, the Beta label 12 px left of them, the tabs before.
        let c = chrome::rects(Caption::Custom, w, sc);
        assert_eq!(c.strip, oled_ui::RectP::wh(0, 0, w, px(40)), "{dpi}");
        assert_eq!(c.below, px(40));
        let b = c.buttons.expect("the caption buttons");
        assert_eq!((b.r, b.w(), b.h()), (w, 3 * px(46), px(40)), "{dpi}");
        let beta = c.beta.expect("the Beta label while the stage is beta");
        assert_eq!(beta.r, b.l - px(12), "{dpi}");
        assert!(c.tabs.r <= beta.l && c.tabs.l == 0 && c.tabs.h() == px(40), "{dpi}: {c:?}");
        // System: Windows' caption stays; the row is 36 px, no label, no buttons.
        let s = chrome::rects(Caption::System, w, sc);
        assert_eq!(s.strip, oled_ui::RectP::wh(0, 0, w, px(36)), "{dpi}");
        assert_eq!((s.tabs, s.beta, s.buttons, s.below), (s.strip, None, None, px(36)), "{dpi}");
    }
}

#[test]
fn effective_path_in_harness() {
    // A window that asks for Custom gets whatever the kit's effective caption is: the row while CUSTOM_CAPTION is
    // false (the strip once the kit's flip merged).
    let sc = Scale { dpi: 96 };
    let row = chrome::rects(Caption::System, 800, sc).below;
    let strip = chrome::rects(Caption::Custom, 800, sc).below;
    for asked in [Caption::System, Caption::Custom] {
        let f = Fakes::new();
        f.enumr.queue(&wp(DIR), fakes::listing(&reports()));
        let mut h = over(f, Location::Fs(wp(DIR)), SizeL { w: 800, h: 500 }).h.caption(asked);
        h.render();
        let below = h.screen().rects().chrome.b;
        assert!(below == row || below == strip, "{asked:?}: {below}");
        if asked == Caption::System {
            assert_eq!(below, row);
        }
        let dump = h.a11y_dump();
        assert!(dump.starts_with("TabList"), "the strip comes first: {dump}");
        // The caption buttons exist exactly when the strip does.
        assert_eq!(dump.contains("Button \"Close\""), below == strip, "{dump}");
        h.assert_named();
    }
}

#[test]
fn caption_hit_map() {
    let sc = Scale { dpi: 96 };
    let r = chrome::rects(Caption::Custom, 900, sc);
    let at = |x: i32, y: i32, on_tab: bool| hit_of(&r, PointP { x, y }, on_tab);
    assert_eq!(at(600, 20, false), CaptionHit::Caption, "empty strip space drags and maximises");
    assert_eq!(at(60, 20, true), CaptionHit::Client, "a tab");
    assert_eq!(at(60, 20, false), CaptionHit::Caption, "tab-less space left of the first plate");
    assert_eq!(at(900 - 23, 20, false), CaptionHit::CloseButton);
    assert_eq!(at(900 - 46 - 23, 20, false), CaptionHit::MaxButton);
    assert_eq!(at(900 - 92 - 23, 20, false), CaptionHit::MinButton);
    assert_eq!(at(450, 40, false), CaptionHit::Client, "under the strip");
    // The row layout has no custom caption: the system answers everywhere.
    let row = chrome::rects(Caption::System, 900, sc);
    assert_eq!(hit_of(&row, PointP { x: 450, y: 10 }, false), CaptionHit::Client);
    // The screen under the system caption (the harness's default) answers Client over its whole row.
    let w = folder_window();
    assert_eq!(oled_ui::Screen::caption_hit(w.fs(), PointP { x: 600, y: 10 }), CaptionHit::Client);
}

#[test]
fn beta_label_named() {
    let c = chrome::rects(Caption::Custom, 900, Scale { dpi: 96 });
    assert_eq!(c.beta.is_some(), files_engine::names::STAGE_BETA);
    assert_eq!(t(k::FILES_APP_BETA, UiLang::EN), "Beta");
    assert_eq!(t(k::FILES_APP_BETA, UiLang::IT), "Beta");
    // Under the system caption the title already says Beta: no second label, and every node of the chrome is named.
    let w = folder_window();
    assert!(!w.dump().contains("Label \"Beta\""), "{}", w.dump());
    w.h.assert_named();
}

// ---- F1b.2: the breadcrumb

fn labels(at: &Location) -> Vec<String> {
    segments(at, UiLang::EN).into_iter().map(|s| s.label).collect()
}

#[test]
fn segments_for_drive_unc_home() {
    assert_eq!(labels(&Location::Fs(wp(r"C:\a\b"))), ["C:", "a", "b"]);
    assert_eq!(labels(&Location::Fs(wp(r"C:\"))), ["C:"]);
    assert_eq!(labels(&Location::Fs(wp(r"\\server\share\x\y"))), [r"\\server\share", "x", "y"]);
    assert_eq!(labels(&Location::Fs(wp(r"\\server\share"))), [r"\\server\share"]);
    assert_eq!(labels(&Location::Home), ["Home"]);
    let segs = segments(&Location::Fs(wp(r"C:\a\b")), UiLang::EN);
    assert_eq!(segs[0].at, Location::Fs(wp(r"C:\")));
    assert_eq!(segs[1].at, Location::Fs(wp(r"C:\a")));
    assert_eq!(&*segs[2].name, b"b");
}

/// Tab stops until the focus is in the path bar (widget 23).
fn focus_path(w: &mut Win) {
    for _ in 0..24 {
        if w.h.focused().is_some_and(|n| n.owner().0 == 23) {
            return;
        }
        w.h.key("Tab");
    }
    panic!("the path bar never took the focus: {}", w.dump());
}

#[test]
fn segment_click_navigates_and_focuses_child() {
    let mut w = folder_window();
    w.f.enumr.queue(
        &wp(r"C:\mod-made-up"),
        fakes::listing(&[("archive", true, 0, day(2)), ("reports", true, 0, day(1)), ("zeta.txt", false, 5, day(4))]),
    );
    focus_path(&mut w);
    // The segments in order: C:, mod-made-up, reports; the middle one is the parent.
    w.h.key("Home").key("Right").key("Enter");
    w.pump();
    let here = w.fs().tabs().active().nav.location().clone();
    assert_eq!(here, Location::Fs(wp(r"C:\mod-made-up")));
    let t = &w.fs().tabs().active().table;
    let m = t.model();
    let row = m.row_of(t.focused().expect("a row is focused")).expect("in the list");
    assert_eq!(m.label(row), "reports", "the folder we came from is focused");
    // The breadcrumb follows: it ends in the parent now.
    assert!(!w.dump().contains("Link \"reports\""), "{}", w.dump());
    assert!(w.dump().contains("Link \"mod-made-up\""), "{}", w.dump());
}

#[test]
fn refresh_button_refreshes() {
    let mut w = folder_window();
    w.f.enumr.queue(&wp(DIR), fakes::listing(&reports()[..2]));
    focus_path(&mut w);
    w.h.key("End").key("Enter");
    w.pump();
    assert_eq!(ListModel::len(w.fs().tabs().active().table.model()), 2, "the folder was listed again");
}

#[test]
fn fold_keeps_root_and_last_two() {
    let deep = r"C:\mod-made-up\first-level-folder\second-level-folder\third-level-folder\fourth-level-folder\last-but-one\last";
    let f = Fakes::new();
    f.enumr.queue(&wp(deep), fakes::listing(&reports()));
    let w = over(f, Location::Fs(wp(deep)), SizeL { w: 560, h: 400 });
    let dump = w.dump();
    for kept in ["C:", "last-but-one", "last"] {
        assert!(dump.contains(&format!("Link \"{kept}\"")), "{kept} stays: {dump}");
    }
    for gone in ["first-level-folder", "second-level-folder", "third-level-folder"] {
        assert!(!dump.contains(&format!("Link \"{gone}\"")), "{gone} is folded: {dump}");
    }
}

#[test]
fn offline_marks_segments() {
    // Pixels of the danger colour in the toolbar band: none while the folder lists, some once it is offline.
    let danger = |w: &mut Win| {
        let img = w.h.render();
        let (top, bottom) = (ROW_H, ROW_H + 44);
        let mut n = 0;
        for y in top..bottom {
            for x in 0..img.w as i32 {
                let p = img.px[(y as u32 * img.w + x as u32) as usize];
                let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
                if r > 150 && g < 110 && b < 110 {
                    n += 1;
                }
            }
        }
        n
    };
    let mut ok = folder_window();
    assert_eq!(danger(&mut ok), 0);
    let f = Fakes::new();
    f.enumr.queue_error(&wp(DIR), files_engine::listing::EnumError::Offline { waited_ms: 4_000 });
    let mut off = over(f, Location::Fs(wp(DIR)), SizeL { w: 800, h: 500 });
    off.h.advance(4_100);
    off.pump();
    assert!(danger(&mut off) > 0, "the breadcrumb is drawn in the danger colour");
    assert!(off.fs().tabs().specs(UiLang::EN)[0].error, "and the tab shows the error");
}

// ---- F1b.3: tabs

#[test]
fn ctrl_t_opens_right_and_activates() {
    let mut w = folder_window();
    let a = w.active();
    w.key_listing("Ctrl+T");
    let ids = w.ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], a);
    assert_eq!(w.active(), ids[1], "the new tab is the shown one");
    assert_eq!(w.fs().tabs().active().nav.location(), &Location::Fs(wp(DIR)), "it opens on the current folder");
    // Back to the first tab, then another new tab: it lands right of the first, between the two.
    w.h.key("Ctrl+PageUp");
    assert_eq!(w.active(), a);
    w.key_listing("Ctrl+T");
    let ids2 = w.ids();
    assert_eq!(ids2.len(), 3);
    assert_eq!((ids2[0], ids2[2]), (a, ids[1]));
    assert_eq!(w.active(), ids2[1]);
    w.h.assert_named();
}

#[test]
fn ctrl_w_activates_right_then_left() {
    let mut w = folder_window();
    w.key_listing("Ctrl+T");
    w.key_listing("Ctrl+T");
    let ids = w.ids();
    assert_eq!(ids.len(), 3);
    // Close the middle one: the right neighbour is shown.
    w.h.key("Ctrl+PageUp");
    assert_eq!(w.active(), ids[1]);
    w.h.key("Ctrl+W");
    assert_eq!(w.ids(), [ids[0], ids[2]]);
    assert_eq!(w.active(), ids[2]);
    // Close the last one: the left neighbour is shown.
    w.h.key("Ctrl+W");
    assert_eq!(w.ids(), [ids[0]]);
    assert_eq!(w.active(), ids[0]);
    assert!(!w.h.last_outputs().iter().any(|o| o.contains("CloseWindow")), "{:?}", w.h.last_outputs());
}

#[test]
fn last_tab_closes_window() {
    let mut w = folder_window();
    w.h.key("Ctrl+W");
    // The window is asked to close; as the process's last window it hides and lingers (F4.12).
    let hidden = [LoopReq::Show { win: 1, how: Show::Hide }];
    assert_eq!(*w.fs().shared().loop_requests.borrow(), hidden, "{:?}", w.h.last_outputs());
}

#[test]
fn next_and_prev_wrap() {
    let mut w = folder_window();
    w.key_listing("Ctrl+T");
    w.key_listing("Ctrl+T");
    let ids = w.ids();
    assert_eq!(w.active(), ids[2]);
    w.h.key("Ctrl+Tab");
    assert_eq!(w.active(), ids[0], "Next wraps to the first");
    w.h.key("Ctrl+Shift+Tab");
    assert_eq!(w.active(), ids[2], "Previous wraps to the last");
    w.h.key("Ctrl+PageUp");
    assert_eq!(w.active(), ids[1]);
    w.h.key("Ctrl+PageDown");
    assert_eq!(w.active(), ids[2]);
}

#[test]
fn switching_keeps_each_tab_state() {
    let mut w = folder_window();
    w.h.key("Home").key("Down").key("Shift+Down");
    let sel = |w: &Win| {
        let t = &w.fs().tabs().active().table;
        let m = t.model();
        (0..ListModel::len(m)).filter(|r| t.selection().contains(ListModel::key(m, *r))).collect::<Vec<_>>()
    };
    let first = sel(&w);
    assert_eq!(first.len(), 2);
    w.key_listing("Ctrl+T");
    assert!(sel(&w).is_empty(), "a new tab starts with nothing selected");
    w.h.key("Ctrl+A");
    assert_eq!(sel(&w).len(), 5);
    w.h.key("Ctrl+PageUp");
    assert_eq!(sel(&w), first, "the first tab is as it was left");
    w.h.key("Ctrl+PageDown");
    assert_eq!(sel(&w).len(), 5, "and so is the second");
}

#[test]
fn tab_labels_are_names_not_paths() {
    let w = folder_window();
    let specs = w.fs().tabs().specs(UiLang::EN);
    assert_eq!(specs[0].label, "reports");
    assert_eq!(specs[0].tip.as_deref(), Some(DIR));
    let root = over(Fakes::new(), Location::Fs(wp(r"C:\")), SizeL { w: 800, h: 500 });
    assert_eq!(root.fs().tabs().specs(UiLang::EN)[0].label, "C:");
    let home = over(Fakes::new(), Location::Home, SizeL { w: 800, h: 500 });
    assert_eq!(home.fs().tabs().specs(UiLang::EN)[0].label, "Home");
    assert_eq!(root.fs().tabs().specs(UiLang::IT)[0].label, "C:");
}

#[test]
fn busy_follows_the_listing() {
    // Nothing answers: from 300 ms the tab is busy, and the sweep stops after 10 s.
    let mut w = over(Fakes::new(), Location::Fs(wp(DIR)), SizeL { w: 800, h: 500 });
    assert!(!w.fs().tabs().specs(UiLang::EN)[0].busy);
    w.h.advance(400);
    w.h.render();
    assert!(w.fs().tabs().specs(UiLang::EN)[0].busy);
    w.h.advance(10_000);
    w.h.render();
    assert!(!w.fs().tabs().specs(UiLang::EN)[0].busy);
}
