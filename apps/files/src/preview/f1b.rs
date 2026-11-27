//! Preview scenes of FB-CHROME (F1b): the tab row under the system caption with three tabs (one busy), the breadcrumb
//! folded at 560 px, and the custom caption (chrome_custom: the only files scene that asks for Custom; while the kit's
//! CUSTOM_CAPTION is false it renders the row layout, the kit's flip commit regenerates its base golden).

use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::nav::NavTimer;
use crate::tab;
use crate::window::FilesScreen;

const ROW_SIZE: SizeL = SizeL { w: 800, h: 360 };
const FOLD_SIZE: SizeL = SizeL { w: 560, h: 360 };
const DEEP: &str = r"C:\made-up\documents\projects\2026\autumn\invoices\scans";

/// The documents folder with two more tabs: one on a folder that never answered (busy past 300 ms), one on another.
fn three_tabs(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    let mut s = fixtures::open(fakes, fmt, ROOT);
    let shared = s.shared().clone();
    let now = shared.services.clock.now_ms();
    let busy = tab::new_tab(&shared, 1, Location::Fs(fixtures::wp(r"C:\made-up\photos-from-the-trip")));
    let quiet = tab::new_tab(&shared, 1, Location::Fs(fixtures::wp(r"C:\made-up\archive")));
    // Each opens right of the active tab, so the one opened last stands second.
    s.tabs_mut().open(quiet, false);
    let busy_id = s.tabs_mut().open(busy, false);
    if let Some(t) = s.tabs_mut().by_id_mut(busy_id) {
        // The listing is on its way: loading, and the 300 ms mark has passed.
        let _ = t.nav.refresh(now);
        let _ = t.nav.on_timer(NavTimer::Busy300, now);
    }
    s
}

fn deep(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(DEEP), fixtures::documents());
    fixtures::open(fakes, fmt, DEEP)
}

fn documents(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

fn row_script(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
}

/// The busy tab's sweep is a tween on the harness clock: part-way along after 600 ms.
fn sweep_script(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    h.advance(600);
    h.render();
}

fn scene(
    name: &'static str,
    size: SizeL,
    cells: Cells,
    caption: Caption,
    build: fn(&Fakes, Fmt) -> FilesScreen,
    script: fn(&mut Harness<FilesScreen>, &Fakes),
) -> Scene {
    Scene { name, size, cells, caption, build, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![
        scene("chrome_row", ROW_SIZE, Cells::Full12, Caption::System, three_tabs, sweep_script),
        scene("breadcrumb_fold", FOLD_SIZE, Cells::Four, Caption::System, deep, row_script),
        scene("chrome_custom", ROW_SIZE, Cells::Four, Caption::Custom, documents, row_script),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_tab_is_busy() {
        let f = Fakes::new();
        let s = three_tabs(&f, Fmt::fixed_en());
        let specs = s.tabs().specs(oled_ui::UiLang::EN);
        let busy: Vec<_> = specs.iter().map(|t| t.busy).collect();
        assert_eq!(busy, [false, true, false], "{:?}", specs.iter().map(|t| t.label.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn only_chrome_custom_requests_custom() {
        let all = super::super::scenes();
        assert!(all.iter().any(|s| s.name == "chrome_custom"), "chrome_custom is always registered");
        for s in &all {
            let want = if s.name == "chrome_custom" { Caption::Custom } else { Caption::System };
            assert_eq!(s.caption, want, "{}", s.name);
        }
    }
}
