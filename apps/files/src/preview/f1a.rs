//! Preview scenes of the details window (F1a.12): the folder with and without a selection, the 100,000-row list, and
//! the four page states. System caption in every one.

use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::listing::EnumError;
use crate::window::FilesScreen;

const STATE_SIZE: SizeL = SizeL { w: 600, h: 360 };
const WINDOW_SIZE: SizeL = SizeL { w: 800, h: 500 };

fn documents(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

fn selection(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    // Two adjacent rows selected as one plate; the focus ring stays on the second.
    h.key("Home").key("Down").key("Down").key("Shift+Down");
    h.render();
}

const BIG_ROWS: u32 = 100_000;
const BIG_TARGET: usize = 50_000;

fn big(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::many(BIG_ROWS));
    fixtures::open(fakes, fmt, ROOT)
}

fn scrolled(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    let first = |h: &Harness<FilesScreen>| h.screen().tabs().active().table.visible_rows().start;
    // The keys move the focus and the list follows it: page down until the first visible row is the target's.
    h.key("Home");
    loop {
        h.render();
        let at = first(h);
        if at + 16 >= BIG_TARGET {
            break;
        }
        for _ in 0..((BIG_TARGET - at) / 20).max(1) {
            h.key("PageDown");
        }
    }
    h.render();
}

fn empty(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    let dir = format!(r"{ROOT}\none");
    fakes.enumr.queue(&fixtures::wp(&dir), crate::fakes::listing(&[]));
    fixtures::open(fakes, fmt, &dir)
}

fn failing(fakes: &Fakes, fmt: Fmt, err: EnumError) -> FilesScreen {
    let dir = format!(r"{ROOT}\locked");
    fakes.enumr.queue_error(&fixtures::wp(&dir), err);
    fixtures::open(fakes, fmt, &dir)
}

fn denied(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    failing(fakes, fmt, EnumError::AccessDenied)
}

fn offline(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    failing(fakes, fmt, EnumError::Offline { waited_ms: 4_000 })
}

fn connecting(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    // Nothing is queued: the listing never answers, and the page says so once a second has gone.
    fixtures::open(fakes, fmt, r"\\made-up-server\share")
}

fn after_connect_delay(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.advance(1_100);
    h.render();
}

fn scene(
    name: &'static str,
    size: SizeL,
    cells: Cells,
    build: fn(&Fakes, Fmt) -> FilesScreen,
    script: fn(&mut Harness<FilesScreen>, &Fakes),
) -> Scene {
    Scene { name, size, cells, caption: Caption::System, build, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![
        scene("details_basic", WINDOW_SIZE, Cells::Full12, documents, settle),
        scene("details_selection", WINDOW_SIZE, Cells::Full12, documents, selection),
        scene("details_100k", WINDOW_SIZE, Cells::Four, big, scrolled),
        scene("state_empty", STATE_SIZE, Cells::Four, empty, settle),
        scene("state_denied", STATE_SIZE, Cells::Four, denied, settle),
        scene("state_offline", STATE_SIZE, Cells::Four, offline, settle),
        scene("state_connecting", STATE_SIZE, Cells::Four, connecting, after_connect_delay),
    ]
}
