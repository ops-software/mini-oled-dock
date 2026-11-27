//! Preview scenes of FD-PATH (F4.4-F4.6): the path field with completions, and a refused path.

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::options::FilesOptions;
use crate::window::FilesScreen;

const SIZE: SizeL = SizeL { w: 900, h: 560 };
/// Made-up pins: the completion rows come from them, whatever machine renders the scene.
const PINS: [&str; 3] = [r"C:\made-up\projects", r"C:\made-up\projects-archive", r"C:\made-up\photos"];

fn build(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.options.set(FilesOptions { pins: PINS.iter().map(|p| p.to_string()).collect(), ..FilesOptions::default() });
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

/// Ctrl+L and "C:\made-up\pro": the pins that match are offered under the field.
fn completion(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    fixtures::settle(h, fakes);
    h.key("Ctrl+L");
    h.type_text(r"C:\made-up\pro");
    fixtures::settle(h, fakes);
}

/// A path that is not there: the reason under the line, the folder unchanged.
fn error(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    fixtures::settle(h, fakes);
    h.key("Ctrl+L");
    h.type_text(r"C:\made-up\no-such-folder");
    h.key("Enter");
    fixtures::settle(h, fakes);
    fixtures::settle(h, fakes);
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    let scene = |name, script| Scene { name, size: SIZE, cells: Cells::Four, caption: Caption::System, build, script };
    vec![scene("path_edit_completion", completion), scene("path_error", error)]
}
