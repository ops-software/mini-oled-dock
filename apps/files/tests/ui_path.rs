//! The path field (F4.5, F4.6): completion, typed paths, siblings and the list's typed separators. Plain tests over
//! the fakes through the kit's Harness; the folders completion reads are a TempRoot's.
#![allow(dead_code)]

mod common;

use common::temp::TempRoot;
use files_engine::bg::Lane;
use files_engine::drives::DriveKind;
use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::handoff::RECYCLE_BIN;
use files_engine::listing::WidePath;
use files_engine::options::FilesOptions;
use files_engine::tr::{k, t};
use files_engine::window::FilesScreen;
use files_engine::{complete, pathline};
use oled_trust::frame::files::{Location, ShellReq};
use oled_ui::harness::Harness;
use oled_ui::model::ListModel;
use oled_ui::{SizeL, Tokens, UiLang};

const SIZE: SizeL = SizeL { w: 900, h: 560 };

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a path parses")
}

fn wp_of(p: &std::path::Path) -> WidePath {
    wp(&p.to_string_lossy())
}

struct Win {
    f: Fakes,
    h: Harness<FilesScreen>,
}

impl Win {
    fn new(dir: &WidePath, options: FilesOptions) -> Win {
        let f = Fakes::new();
        f.options.set(options);
        f.enumr.queue(dir, fakes::listing(&[("a.txt", false, 10, FT_2026_11_21)]));
        let shared = f.shared(UiLang::EN);
        let tab = shared.new_tab();
        let s = FilesScreen::new(shared, 1, Location::Fs(dir.clone()), None, tab);
        let mut h = Harness::new(s, SIZE, 96, Tokens::oled());
        h.render();
        f.pump(&mut h, 1);
        h.render();
        Win { f, h }
    }

    fn pump(&mut self) {
        self.f.pump(&mut self.h, 1);
        self.h.render();
    }

    fn at(&self) -> Location {
        self.h.screen().tabs().active().nav.location().clone()
    }

    fn dump(&self) -> String {
        self.h.a11y_dump()
    }

    /// Ctrl+L, the text, no Enter; the answers of the jobs it started are delivered.
    fn type_path(&mut self, text: &str) {
        self.h.key("Ctrl+L");
        self.h.type_text(text);
        self.pump();
    }

    fn enter(&mut self) {
        self.h.key("Enter");
        self.pump();
        self.pump();
    }

    fn rows(&self) -> usize {
        ListModel::len(self.h.screen().tabs().active().table.model())
    }

    fn selected_labels(&self) -> Vec<String> {
        let t = &self.h.screen().tabs().active().table;
        let m = t.model();
        (0..ListModel::len(m))
            .filter(|r| t.selection().contains(ListModel::key(m, *r)))
            .map(|r| m.label(r).to_string())
            .collect()
    }

    /// The a11y dump shows backslashes doubled.
    fn has(&self, text: &str) -> bool {
        self.dump().contains(&text.replace('\\', "\\\\"))
    }

    /// The rows of the open completion list.
    fn popup(&self) -> Vec<String> {
        let Some(m) = self.h.menus().first() else { return Vec::new() };
        m.items
            .iter()
            .map(|i| match i {
                oled_ui::MenuItem::Custom { label, .. } => label.to_string(),
                other => panic!("a list row: {other:?}"),
            })
            .collect()
    }

    fn bg_jobs(&self) -> usize {
        self.f.bg.runs().len()
    }
}

/// A TempRoot with the folders and the file the tests walk.
fn tree() -> TempRoot {
    let r = TempRoot::new("ui_path");
    for d in ["alpha", "alphabet", "beta", "xalpha", "Gamma"] {
        r.dir(d);
    }
    r.file("note.txt", b"x");
    r
}

fn win_at(r: &TempRoot) -> Win {
    Win::new(&wp_of(&r.path().join("alpha")), FilesOptions::default())
}

fn root_text(r: &TempRoot) -> String {
    r.path().to_string_lossy().into_owned()
}

fn queue(w: &Win, dir: &WidePath) {
    w.f.enumr.queue(dir, fakes::listing(&[("note.txt", false, 1, FT_2026_11_21), ("alpha", true, 0, FT_2026_11_21)]));
}

// ---- F4.5 -------------------------------------------------------------------------------------------------------

fn env_for_rows() -> files_engine::parse::Env {
    files_engine::parse::Env {
        vars: vec![("MADEUP_A".into(), "x".into()), ("MADEUP_B".into(), "y".into()), ("OTHER".into(), "z".into())],
        ..Default::default()
    }
}

fn dirs(names: &[&str]) -> Vec<Box<str>> {
    names.iter().map(|n| (*n).into()).collect()
}

#[test]
fn child_folders_prefix_then_substring() {
    let d = dirs(&["alpha", "beta", "pre-alpha", "alphabet", "ALPS"]);
    let r = complete::rows(r"C:\made-up\alp", &d, &env_for_rows(), &[]);
    assert_eq!(r, [r"C:\made-up\alpha", r"C:\made-up\alphabet", r"C:\made-up\ALPS", r"C:\made-up\pre-alpha"]);
    let all = complete::rows(r"C:\made-up\", &d, &env_for_rows(), &[]);
    assert_eq!(all.len(), 5, "an empty name lists every folder");
    // The same through a window: the folders are read off the UI thread, from the typed parent.
    let root = tree();
    let mut w = win_at(&root);
    w.type_path(&format!(r"{}\alp", root_text(&root)));
    let rows = w.popup();
    let r = root_text(&root);
    assert_eq!(
        rows,
        [format!(r"{r}\alpha"), format!(r"{r}\alphabet"), format!(r"{r}\xalpha")],
        "prefix, then substring"
    );
}

#[test]
fn cap_512_and_cache_5s() {
    let root = TempRoot::new("ui_path_cap");
    for i in 0..600 {
        root.dir(&format!("d{i:04}"));
    }
    let listed = complete::list_dirs(&wp_of(root.path()), false);
    assert_eq!(listed.len(), complete::MAX_DIRS, "at most 512 folders of one level");
    let mut w = Win::new(&wp_of(&root.path().join("d0001")), FilesOptions::default());
    let base = w.bg_jobs();
    let text = format!(r"{}\d00", root_text(&root));
    w.type_path(&text);
    let first = w.bg_jobs();
    assert!(first > base, "the typed parent is listed on a Bg job");
    // More typing under the same parent within 5 s reads the cache: no new job.
    w.h.type_text("1");
    w.pump();
    assert_eq!(w.bg_jobs(), first, "cached for 5 s");
    w.f.clock.advance(5_001);
    w.h.type_text("2");
    w.pump();
    assert_eq!(w.bg_jobs(), first + 1, "listed again after 5 s");
}

#[test]
fn variables_and_shell_names() {
    let v = complete::rows("%MADE", &[], &env_for_rows(), &[]);
    assert_eq!(v, ["%MADEUP_A%", "%MADEUP_B%"]);
    let s = complete::rows("shell:d", &[], &env_for_rows(), &[]);
    assert_eq!(s, ["shell:downloads", "shell:documents", "shell:desktop"]);
    let all = complete::rows("sh", &[], &env_for_rows(), &[]);
    assert_eq!(all.len(), complete::MAX_ROWS, "at most 8 rows");
    assert!(all.iter().all(|r| r.starts_with("shell:")));
}

#[test]
fn pins_and_history_fuzzy() {
    let pool = vec![
        r"C:\made-up\projects".to_string(),
        r"D:\made-up\media\project-x".to_string(),
        r"C:\made-up\stuff".to_string(),
        r"C:\made-up\Projects".to_string(),
    ];
    let r = complete::rows("pro", &[], &env_for_rows(), &pool);
    assert_eq!(r, [r"C:\made-up\projects", r"D:\made-up\media\project-x"], "segment starts first, no repeats by case");
    let r = complete::rows("mdp", &[], &env_for_rows(), &pool);
    assert!(r.contains(&r"C:\made-up\projects".to_string()), "characters in order: {r:?}");
    assert!(complete::rows("zzz", &[], &env_for_rows(), &pool).is_empty());
    // Through a window, a pin is offered.
    let f = FilesOptions { pins: vec![r"C:\made-up\pinned-place".into()], ..FilesOptions::default() };
    let root = tree();
    let mut w = Win::new(&wp_of(&root.path().join("alpha")), f);
    w.type_path("pinned");
    assert_eq!(w.popup(), [r"C:\made-up\pinned-place"]);
}

#[test]
fn stale_reply_ignored() {
    let root = tree();
    let mut w = win_at(&root);
    w.h.key("Ctrl+L");
    // Two requests are out before either answers; only the last text's rows show.
    w.h.type_text(&format!(r"{}\x", root_text(&root)));
    w.h.key("Backspace");
    w.h.type_text("G");
    w.pump();
    let rows = w.popup();
    assert_eq!(rows, [format!(r"{}\Gamma", root_text(&root))], "only the last text's rows show: {rows:?}");
}

/// Completion lists a typed parent only on a drive this PC holds as its own: a UNC or WebDAV path, a mapped network
/// drive and a letter the drive cache does not know are never listed while the user types (listing is a request to the
/// server, which authenticates as the user: the NTLM hash would leak before anything was confirmed). The decision is
/// pure; through a window, typing any of them queues no listing on the network lane.
#[test]
fn completion_never_lists_a_remote_parent() {
    let kind = |l: u8| match l {
        b'C' | b'D' => Some(DriveKind::Fixed),
        b'E' => Some(DriveKind::Removable),
        b'Z' => Some(DriveKind::Network),
        b'U' => Some(DriveKind::Unknown),
        _ => None,
    };
    let rows: [(&str, bool); 12] = [
        (r"C:\made-up", true),
        (r"c:\made-up\sub", true),
        (r"D:\", true),
        (r"E:\made-up", true),
        (r"\\?\C:\made-up", true),
        (r"\\server-made-up\share", false),
        (r"\\server-made-up\share\sub", false),
        (r"\\?\UNC\server-made-up\share\sub", false),
        (r"\\host-made-up@SSL\DavWWWRoot\sub", false),
        (r"Z:\made-up", false),
        (r"Q:\made-up", false),
        (r"U:\made-up", false),
    ];
    for (p, want) in rows {
        assert_eq!(complete::may_list(&wp(p), kind), want, "{p}");
    }
    // Through a window (C: and D: fixed, Z: a network drive): nothing remote is queued, so nothing is listed. No pump
    // follows the typing, so even without the rule no job would run and no server would be asked.
    let root = tree();
    let mut w = win_at(&root);
    let network = |w: &Win| w.f.bg.runs().iter().filter(|(_, lane)| *lane == Lane::Network).count();
    let before = network(&w);
    for text in [
        r"\\server-made-up\share\",
        r"\\server-made-up\share\sub\a",
        r"\\?\UNC\server-made-up\share\",
        r"\\host-made-up@SSL\DavWWWRoot\",
        r"Z:\",
        r"Z:\made-up\a",
        r"Q:\",
    ] {
        w.h.key("Ctrl+L");
        w.h.type_text(text);
        assert_eq!(network(&w), before, "typing {text} queues no listing");
        end_edit(&mut w);
    }
}

/// Esc until the field is closed (the first may only close the rows), so the next Ctrl+L starts afresh.
fn end_edit(w: &mut Win) {
    w.h.key("Esc");
    if pathline::editing(w.h.screen()) {
        w.h.key("Esc");
    }
}

/// Completion reads nothing through a link: no folder from the drive's root down to the typed parent may be a junction
/// or a symbolic link (one can lead to a remote share). The folders are asked from the root down, never past the first
/// link, and one that cannot be read refuses too. Through a window: a junction lists nothing, its target does.
#[test]
fn completion_lists_nothing_through_a_link() {
    let asked = std::cell::RefCell::new(Vec::<String>::new());
    let through = complete::no_link_on_the_way(&wp(r"C:\made-up\link\sub"), |d| {
        asked.borrow_mut().push(d.to_string_lossy());
        Some(d.to_string_lossy().ends_with("link"))
    });
    assert!(!through);
    assert_eq!(*asked.borrow(), [r"C:\", r"C:\made-up", r"C:\made-up\link"], "from the root down, never past it");
    assert!(complete::no_link_on_the_way(&wp(r"C:\made-up\plain"), |_| Some(false)));
    assert!(!complete::no_link_on_the_way(&wp(r"C:\made-up\gone"), |_| None), "a folder that cannot be read");
    let root = tree();
    let target = root.dir("real");
    root.dir(r"real\inner");
    root.junction("link", &target);
    let mut w = win_at(&root);
    let r = root_text(&root);
    w.type_path(&format!(r"{r}\link\"));
    assert!(w.popup().iter().all(|row| !row.contains("inner")), "{:?}", w.popup());
    end_edit(&mut w);
    w.type_path(&format!(r"{r}\real\"));
    assert_eq!(w.popup(), [format!(r"{r}\real\inner")]);
}

// ---- F4.6 -------------------------------------------------------------------------------------------------------

#[test]
fn ctrl_l_selects_all() {
    let root = tree();
    let mut w = win_at(&root);
    w.h.key("Ctrl+L");
    assert!(pathline::editing(w.h.screen()));
    let here = root.path().join("alpha").to_string_lossy().into_owned();
    assert!(w.has(&here), "the whole path is in the field: {}", w.dump());
    // Everything is selected: one character replaces it.
    w.h.type_text("Zq");
    w.pump();
    assert!(!w.has(&here), "{}", w.dump());
    w.h.key("Escape");
    assert!(!pathline::editing(w.h.screen()), "Esc restores the breadcrumb");
    w.h.key("Alt+D");
    assert!(pathline::editing(w.h.screen()), "Alt+D too");
}

#[test]
fn enter_folder_file_shell_virtual_command_paths() {
    let root = tree();
    let mut w = win_at(&root);
    let r = root_text(&root);
    // A folder.
    queue(&w, &wp(&format!(r"{r}\beta")));
    w.type_path(&format!(r"{r}\beta"));
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\beta"))));
    // A file: its folder opens with the file selected.
    queue(&w, &wp(&r));
    w.type_path(&format!(r#""{r}\note.txt""#));
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&r)));
    assert_eq!(w.selected_labels(), ["note.txt"]);
    // A known folder through shell:.
    w.h.screen().shared().session.borrow_mut().known.insert("downloads".into(), format!(r"{r}\Gamma"));
    queue(&w, &wp(&format!(r"{r}\Gamma")));
    w.type_path("shell:Downloads");
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\Gamma"))));
    // A relative path.
    queue(&w, &wp(&format!(r"{r}\alpha")));
    w.type_path(r"..\alpha");
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\alpha"))));
    // A location only File Explorer opens: no navigation; one request hands it to File Explorer (F4.13).
    let before = w.f.shell.requests().len();
    w.type_path("shell:RecycleBinFolder");
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\alpha"))));
    let sent = w.f.shell.requests();
    assert_eq!(sent.len(), before + 1, "one hand-off: {sent:?}");
    assert!(matches!(sent.last(), Some(ShellReq::RunTyped { text, .. }) if &**text == RECYCLE_BIN), "{sent:?}");
    let typed = |q: &ShellReq| matches!(q, ShellReq::RunTyped { text, .. } if &**text != RECYCLE_BIN);
    // Text that is not a path runs through the host, in this folder, and only from Enter.
    w.type_path("made-up-command --flag");
    assert!(!w.f.shell.requests().iter().any(typed), "typing never runs");
    w.enter();
    let ran: Vec<_> = w.f.shell.requests().into_iter().filter(|q| typed(q)).collect();
    assert_eq!(ran.len(), 1);
    if let ShellReq::RunTyped { text, dir, .. } = &ran[0] {
        assert_eq!(&**text, "made-up-command --flag");
        assert_eq!(*dir, wp(&format!(r"{r}\alpha")));
    }
    // A bare word that names a folder here goes there instead of running.
    w.h.screen().shared().session.borrow_mut().known.clear();
    queue(&w, &wp(&r));
    w.type_path(r"..");
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&r)));
    queue(&w, &wp(&format!(r"{r}\beta")));
    w.type_path("beta");
    w.enter();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\beta"))));
    assert_eq!(w.f.shell.requests().iter().filter(|q| typed(q)).count(), 1);
}

#[test]
fn not_found_sets_error_and_stays() {
    let root = tree();
    let mut w = win_at(&root);
    let r = root_text(&root);
    let here = w.at();
    w.type_path(&format!(r"{r}\no-such-folder"));
    w.enter();
    assert_eq!(w.at(), here, "the tab stays");
    let want = format!("Can't find {r}\\no-such-folder");
    assert_eq!(pathline::error_of(w.h.screen()), Some(want));
    // The next navigation clears it.
    queue(&w, &wp(&r));
    w.type_path(&r);
    w.enter();
    assert!(pathline::error_of(w.h.screen()).is_none());
    // A path that cannot be one.
    w.type_path("C:x");
    w.enter();
    assert_eq!(pathline::error_of(w.h.screen()), Some(t(k::FILES_STATE_BAD_PATH, UiLang::EN).to_string()));
}

#[test]
fn siblings_menu_lists_and_navigates() {
    let root = tree();
    let mut w = win_at(&root);
    let r = root_text(&root);
    // Tab to the path bar (widget 23), Left to the folder above ours, Down opens its subfolders.
    for _ in 0..40 {
        if w.h.focused().is_some_and(|n| n.owner().0 == 23) {
            break;
        }
        w.h.key("Tab");
    }
    assert!(w.h.focused().is_some_and(|n| n.owner().0 == 23), "the path bar has the focus");
    w.h.key("Left");
    w.h.key("Down");
    w.pump();
    let menus = w.h.menus();
    assert_eq!(menus.len(), 1, "the list opens");
    let names: Vec<String> = menus[0]
        .items
        .iter()
        .map(|i| match i {
            oled_ui::MenuItem::Custom { label, .. } => label.to_string(),
            other => panic!("a list row: {other:?}"),
        })
        .collect();
    assert_eq!(names.len(), 5, "{names:?}");
    assert_eq!(menus[0].highlight, Some(0), "alpha, the folder the path goes through, is highlighted");
    // Down moves, Enter goes.
    queue(&w, &wp(&format!(r"{r}\alphabet")));
    w.h.key("Down");
    w.h.key("Enter");
    w.pump();
    assert_eq!(w.at(), Location::Fs(wp(&format!(r"{r}\alphabet"))));
    assert!(w.h.menus().is_empty());
}

#[test]
fn backslash_in_list_types_into_path() {
    let root = tree();
    let mut w = win_at(&root);
    let here = root.path().join("alpha").to_string_lossy().into_owned();
    assert!(!pathline::editing(w.h.screen()));
    // The list has the focus after the first rows arrived.
    w.h.type_text("\\");
    w.pump();
    assert!(pathline::editing(w.h.screen()), "the character moves the path into the field");
    assert!(w.has(&format!("{here}\\")), "the path and the character: {}", w.dump());
    w.h.key("Escape");
    // Other characters stay in the list (type-ahead).
    w.h.type_text("a");
    assert!(!pathline::editing(w.h.screen()));
}

#[test]
fn esc_restores() {
    let root = tree();
    let mut w = win_at(&root);
    w.type_path(&format!(r"{}\be", root_text(&root)));
    assert!(!w.popup().is_empty(), "completions are open");
    w.h.key("Esc");
    if pathline::editing(w.h.screen()) {
        // The first Esc closed the list, the second ends the edit.
        w.h.key("Esc");
    }
    w.h.render();
    assert!(!pathline::editing(w.h.screen()));
    let _ = pathline::segments(&w.at(), UiLang::EN);
    w.pump();
    assert!(w.dump().contains("alpha"), "the breadcrumb is back");
}
