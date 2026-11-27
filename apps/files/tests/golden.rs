//! The goldens of the details window (F1a.12): every scene x its cells against apps/files/tests/golden/base (the set
//! the CI runner makes), or against the set GOLDEN_SET names (the untracked local one, for looking). Plus the a11y
//! dump of each scene in each language, compared as text.

use std::path::Path;

use files_engine::fakes::Fakes;
use files_engine::preview::{self, Cells};
use oled_ui::harness::golden;
use oled_ui::Lang;

#[test]
fn every_scene_matches_its_goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = Path::new(env!("CARGO_TARGET_TMPDIR")).join("files-golden");
    let scenes = preview::scenes();
    assert!(!scenes.is_empty(), "the details window has scenes");
    for s in &scenes {
        for c in &preview::cells(s.cells) {
            let fakes = Fakes::new();
            let mut h = preview::harness(s, c, &fakes);
            let img = h.render();
            h.assert_named();
            let name = format!("{}/{}", s.name, c.tag);
            golden::check(&root, &diff, &name, &img);
            // The menus open in the scene are popups of their own: their pixels are pinned too.
            for i in 0..h.menus().len() {
                if let Some(menu) = h.menu_image(i) {
                    golden::check(&root, &diff, &format!("{name}-menu{i}"), &menu);
                }
            }
            // One a11y dump per language: English at 96 dpi, Italian at the first Italian cell.
            let it_dpi = if s.cells == Cells::Full12 { 96 } else { 144 };
            let lang = match (c.lang, c.pseudo, c.dpi) {
                (Lang::En, false, 96) if c.tag == "en-96" => Some("en"),
                (Lang::It, false, d) if d == it_dpi && c.tag.starts_with("it-") => Some("it"),
                _ => None,
            };
            if let Some(l) = lang {
                golden::check_text(&root, &diff, &format!("{}/a11y-{l}.txt", s.name), &h.a11y_dump());
            }
        }
    }
}
