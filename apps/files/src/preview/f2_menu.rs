//! Preview scenes of FB-MENU (F2.5, F2.6): our context menu on a file and on the empty space, and the More menu of the
//! toolbar. The window behind the menu is the scene's image; the menu itself is drawn by its own popup, which
//! Harness::menu_image renders: preview::run and the golden test save and check it as <scene>/<cell>-menu<i>.

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::{PointL, SizeL};

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::window::FilesScreen;

const WINDOW_SIZE: SizeL = SizeL { w: 800, h: 500 };

fn documents(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

/// The Menu key on a file row: three rows down is a file, so our file menu opens.
fn context_menu_file(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.key("Home").key("Down").key("Down").key("Down").key("Apps");
    h.render();
}

/// A right click on the empty space under the rows: the folder's own menu.
fn context_menu_background(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.right_click(PointL { x: 420, y: 470 });
    h.render();
}

/// The More button, the last of the right cluster: clicked at the centre of its own rect.
fn more_menu(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    let scale = f64::from(h.render().w) / f64::from(WINDOW_SIZE.w);
    let r = crate::toolbar::right_rects(h.screen())[3];
    let logical = |px: i32| (f64::from(px) / scale).round() as i32;
    h.click(PointL { x: logical((r.l + r.r) / 2), y: logical((r.t + r.b) / 2) });
    h.render();
}

fn scene(name: &'static str, script: fn(&mut Harness<FilesScreen>, &Fakes)) -> Scene {
    Scene { name, size: WINDOW_SIZE, cells: Cells::Four, caption: Caption::System, build: documents, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![
        scene("context_menu_file", context_menu_file),
        scene("context_menu_background", context_menu_background),
        scene("more_menu", more_menu),
    ]
}
