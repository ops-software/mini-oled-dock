//! Preview scenes of FC-RENAME (F3.7): the rename field over a name with its stem selected, and the same field kept open
//! with the error rim after a name that is already taken.

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::window::FilesScreen;

const SIZE: SizeL = SizeL { w: 800, h: 360 };

fn documents(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

/// F2 on the fourth row (the long docx name).
fn open_field(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.key("Home").key("Down").key("Down").key("Down").key("F2");
    h.render();
}

fn field_script(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    open_field(h, fakes);
}

/// The whole name replaced by one that is in the folder already: the field stays open with the rim and the line.
fn clash_script(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    open_field(h, fakes);
    h.key("F2").type_text("notes.txt").key("Enter");
    h.render();
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    let scene = |name, script| Scene {
        name,
        size: SIZE,
        cells: Cells::Four,
        caption: Caption::System,
        build: documents,
        script,
    };
    vec![scene("rename_field", field_script), scene("rename_clash", clash_script)]
}
