//! The gallery's goldens (K1a.6): every page x the golden matrix against apps/gallery/tests/golden/base (the set the CI
//! runner makes), or against the set GOLDEN_SET names (the untracked local one, for looking). Plus the a11y snapshot of
//! each page, compared as text.

use std::path::Path;

use gallery::{pages, preview};
use oled_ui::harness::golden;

#[test]
fn every_page_matches_its_goldens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let diff = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden").join("gallery");
    for (name, _) in pages::REGISTRY {
        let cells = preview::cells_for(name);
        for cell in &cells {
            let Some(r) = preview::render(name, cell) else { break };
            golden::check(&root, &diff, &format!("{name}/{}", cell.name()), &r.image);
            if cell.name() == "en-96" {
                golden::check_text(&root, &diff, &format!("{name}/a11y.txt"), &r.dump);
            }
        }
    }
}
