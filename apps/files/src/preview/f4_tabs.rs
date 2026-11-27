//! Preview scenes of FD-TABS (F4.1-F4.3): nine tabs shrinking to the narrowest plate, and the menu of a tab. The menu
//! is drawn by its own popup, which Harness::menu_image renders (saved as <scene>/<cell>-menu0).

use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::{PointL, SizeL};

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::tab;
use crate::window::FilesScreen;

const SIZE: SizeL = SizeL { w: 900, h: 520 };

/// Made-up folder names of the other tabs, short and long.
const OTHERS: [&str; 8] =
    ["projects", "invoices", "a-folder-with-a-rather-long-name", "media", "backups", "notes", "photos-2026", "archive"];

/// The documents window with `extra` more tabs in the background (they list nothing: only their plates are shown).
fn with_tabs(fakes: &Fakes, fmt: Fmt, extra: usize) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    let mut s = fixtures::open(fakes, fmt, ROOT);
    let shared = std::rc::Rc::clone(s.shared());
    for name in OTHERS.iter().take(extra) {
        let at = Location::Fs(fixtures::wp(&format!(r"C:\made-up\{name}")));
        s.tabs_mut().open(tab::new_tab(&shared, 1, at), false);
    }
    s
}

fn many(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    with_tabs(fakes, fmt, OTHERS.len())
}

fn three(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    with_tabs(fakes, fmt, 2)
}

/// A right click on the first plate: the tab's menu.
fn open_menu(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.right_click(PointL { x: 60, y: 18 });
    h.render();
}

fn scene(
    name: &'static str,
    build: fn(&Fakes, Fmt) -> FilesScreen,
    script: fn(&mut Harness<FilesScreen>, &Fakes),
) -> Scene {
    Scene { name, size: SIZE, cells: Cells::Four, caption: Caption::System, build, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![scene("tabs_many", many, settle), scene("tab_menu", three, open_menu)]
}
