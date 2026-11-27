//! Preview scenes of FD-SIDE (F4.8-F4.11): the sidebar with made-up drives and pins, with an expanded tree, and Home.

use std::rc::Rc;

use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::options::FilesOptions;
use crate::sidebar::{days_from_civil, key_of};
use crate::window::FilesScreen;

const SIZE: SizeL = SizeL { w: 900, h: 560 };
const PINS: [&str; 4] = [
    r"C:\made-up\documents",
    r"C:\made-up\projects",
    r"D:\made-up\media",
    r"D:\made-up\a-folder-with-a-rather-long-name",
];

fn window(fakes: &Fakes, fmt: Fmt, at: Location) -> FilesScreen {
    fakes.options.set(FilesOptions { pins: PINS.iter().map(|p| p.to_string()).collect(), ..FilesOptions::default() });
    let shared = fakes.shared(fmt.lang());
    *shared.fmt.borrow_mut() = Rc::new(fmt);
    crate::drives::scan(&shared);
    shared
        .session
        .borrow_mut()
        .network_recent
        .push((r"\\made-up-server\projects".into(), days_from_civil(2026, 11, 21)));
    let tab = shared.new_tab();
    let mut s = FilesScreen::new(shared, 1, at, None, tab);
    for p in PINS {
        s.side.seed_exists(&fixtures::wp(p), true);
    }
    s
}

fn listing(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    window(fakes, fmt, Location::Fs(fixtures::wp(ROOT)))
}

fn tree(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(r"C:\made-up\projects\alpha"), fixtures::documents());
    window(fakes, fmt, Location::Fs(fixtures::wp(r"C:\made-up\projects\alpha")))
}

fn home(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    window(fakes, fmt, Location::Home)
}

fn settle(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    // Pins and drives are the scene's own (no job is queued for them): only the listing arrives.
    fixtures::settle(h, fakes);
}

fn expanded(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    let c = key_of(0, "drive", "C");
    let projects = key_of(c, "dir", "projects");
    let s = h.screen_mut();
    s.side.seed_tree(c, &["apps", "made-up", "tools"]);
    s.side.seed_tree(key_of(c, "dir", "made-up"), &["archive", "documents", "projects"]);
    let _ = projects;
    s.side.seed_tree(key_of(key_of(c, "dir", "made-up"), "dir", "projects"), &["alpha", "beta"]);
    settle(h, fakes);
}

fn scene(
    name: &'static str,
    cells: Cells,
    build: fn(&Fakes, Fmt) -> FilesScreen,
    script: fn(&mut Harness<FilesScreen>, &Fakes),
) -> Scene {
    Scene { name, size: SIZE, cells, caption: Caption::System, build, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![
        scene("sidebar", Cells::Full12, listing, settle),
        scene("sidebar_tree", Cells::Four, tree, expanded),
        scene("home", Cells::Full12, home, settle),
    ]
}
