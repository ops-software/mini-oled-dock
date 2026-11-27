//! The accessibility, pseudo-locale and display-name sweep (P3-files-app F4.17, ROADMAP 3.1 gates a-c): every scene of
//! the window, in every cell it is rendered in, has no interactive node without a name, no two texts drawn over each
//! other, and its pseudo-locale and high-contrast cells; and the app's display name has one entry in the repository's
//! string tables (the kit's kit.app.files). The literal-string lint is tests/lints.rs, not here.

use std::path::{Path, PathBuf};

use files_engine::fakes::Fakes;
use files_engine::preview::{self, Cells};
use oled_ui::RectP;

/// Rect heights are line boxes: the glyphs sit inside them with leading above and below, so two lines that touch
/// only by their leading (a field's error line under the field, over the column header) do not collide. A real
/// collision shares most of a line: more than a quarter of the shorter rect's height.
fn overlaps(a: &RectP, b: &RectP) -> bool {
    let w = a.r.min(b.r) - a.l.max(b.l);
    let h = a.b.min(b.b) - a.t.max(b.t);
    let shorter = (a.b - a.t).min(b.b - b.t);
    w > 0 && h > shorter / 4
}

/// The pairs of text rects that overlap. The harness logs one rect per drawn text run, deduplicated: two runs of one
/// node never share pixels, so every overlapping pair is two texts drawn over each other.
fn overlapping(rects: &[RectP]) -> Vec<(RectP, RectP)> {
    let mut out = Vec::new();
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            if overlaps(a, b) {
                out.push((*a, *b));
            }
        }
    }
    out
}

#[test]
fn every_scene_is_named_and_its_texts_do_not_overlap() {
    let scenes = preview::scenes();
    assert!(!scenes.is_empty(), "the window has scenes");
    let mut cells_seen = 0;
    for s in &scenes {
        let cells = preview::cells(s.cells);
        for tag in ["pseudo-144", "hc-144"] {
            assert!(cells.iter().any(|c| c.tag == tag), "scene {}: no {tag} cell", s.name);
        }
        assert!(cells.iter().any(|c| c.pseudo) && cells.iter().any(|c| c.tag.starts_with("hc-")), "{}", s.name);
        for c in &cells {
            let fakes = Fakes::new();
            let mut h = preview::harness(s, c, &fakes);
            let img = h.render();
            h.assert_named();
            let mut sets = vec![img.text_rects.clone()];
            if !overlapping(&img.text_rects).is_empty() {
                // An opaque overlay (the operations flyout, the confirm bar) paints over the window and the harness
                // still logs the covered texts: dismiss it, then sweep the overlay's own texts and the window's
                // texts apart. A scene whose overlap survives that is a real one.
                h.key("Esc");
                let without = h.render().text_rects;
                let own: Vec<RectP> = img.text_rects.iter().filter(|r| !without.contains(r)).copied().collect();
                assert!(!own.is_empty(), "scene {} cell {}: texts overlap and no overlay explains it", s.name, c.tag);
                sets = vec![own, without];
            }
            for set in &sets {
                let bad = overlapping(set);
                assert!(bad.is_empty(), "scene {} cell {}: texts overlap: {bad:?}", s.name, c.tag);
            }
            for i in 0..h.menus().len() {
                let menu = h.menu_image(i).expect("an open menu has pixels");
                let bad = overlapping(&menu.text_rects);
                assert!(bad.is_empty(), "scene {} cell {} menu {i}: texts overlap: {bad:?}", s.name, c.tag);
            }
            cells_seen += 1;
        }
    }
    assert!(cells_seen >= scenes.len() * 4, "every scene has at least its four cells");
    // Both cell sets carry the pseudo and high-contrast cells the sweep relies on.
    for set in [Cells::Full12, Cells::Four] {
        assert!(preview::cells(set).iter().any(|c| c.pseudo));
    }
}

/// Every strings folder of the repository: the root's, each app's and each crate's.
fn string_tables() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut dirs = vec![root.join("strings")];
    for group in ["apps", "crates"] {
        let Ok(rd) = std::fs::read_dir(root.join(group)) else { continue };
        dirs.extend(rd.filter_map(Result::ok).map(|e| e.path().join("strings")));
    }
    let mut files = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        files
            .extend(rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")));
    }
    files
}

#[test]
fn display_name_has_one_entry() {
    let kit = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("crates")
            .join("oled-ui")
            .join("strings")
            .join("en.toml"),
    )
    .expect("the kit's English table");
    let name = kit
        .lines()
        .find_map(|l| l.strip_prefix("\"kit.app.files\" = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("the kit holds kit.app.files");
    assert!(name.len() >= 3, "a display name: {name:?}");
    let files = string_tables();
    assert!(files.len() >= 8, "the tables of the root, the apps and the crates were found: {files:?}");
    let lower = name.to_lowercase();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("a table is text");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') || !line.to_lowercase().contains(&lower) {
                continue;
            }
            assert!(
                line.starts_with("\"kit.app.files\" = "),
                "{}:{}: the display name {name:?} appears outside kit.app.files: {line}",
                f.display(),
                n + 1
            );
        }
    }
}

#[test]
fn the_overlap_check_catches_a_planted_collision() {
    let line = RectP { l: 10, t: 100, r: 110, b: 116 };
    let same_line = RectP { l: 60, t: 101, r: 160, b: 117 };
    let under = RectP { l: 10, t: 113, r: 110, b: 129 };
    let beside = RectP { l: 110, t: 100, r: 160, b: 116 };
    assert_eq!(overlapping(&[line, same_line]).len(), 1, "two texts on one line");
    assert!(overlapping(&[line, under]).is_empty(), "lines touching by their leading only");
    assert!(overlapping(&[line, beside]).is_empty(), "side by side");
}