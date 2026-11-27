//! Preview scenes of FD-INST (F4.15): the About dialog over the details window.

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::about;
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::window::FilesScreen;

const SIZE: SizeL = SizeL { w: 800, h: 480 };

fn about_open(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    let mut s = fixtures::open(fakes, fmt, ROOT);
    about::show(&mut s);
    s
}

fn about_script(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![Scene {
        name: "about",
        size: SIZE,
        cells: Cells::Four,
        caption: Caption::System,
        build: about_open,
        script: about_script,
    }]
}
