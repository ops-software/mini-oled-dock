//! The sidebar page (K1b.2): the Settings-style nav at 36 px rows at the left, the files-style tree at 28 px rows at the
//! right. Hover and focus need the pointer and the keyboard: the widget tests drive them through the Harness.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{SidebarItem, SidebarList, SidebarStyle};
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 480, h: 320 };

struct Sidebars {
    lists: Vec<(SidebarList, (i32, i32, i32, i32))>,
    commands: CommandTable,
}

impl Screen for Sidebars {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (l, (x, y, w, h)) in &mut self.lists {
            l.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (l, _) in &self.lists {
            l.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (l, _) in &mut self.lists {
            l.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

fn item(key: u64, label: Text, glyph: Option<Glyph>, depth: u16) -> SidebarItem {
    SidebarItem {
        key,
        label,
        glyph,
        icon: None,
        depth,
        expandable: false,
        expanded: false,
        section: false,
        trailing: None,
        disabled: false,
        busy: false,
    }
}

/// The page: a nav with a current entry, and a tree with sections, depth, an expanded node, trailing text, a busy row, a
/// disabled row and a drop target.
pub fn page(lang: UiLang) -> Option<Page> {
    let mut nav = SidebarList::new(WidgetId(1), SidebarStyle { row_h: 36, inset: 8 }).with_items(vec![
        item(1, s(lang, "Display", "Schermo"), Some(Glyph::Monitor), 0),
        item(2, s(lang, "Sound", "Audio"), Some(Glyph::Network), 0),
        item(3, s(lang, "Power", "Alimentazione"), Some(Glyph::Power), 0),
        item(4, s(lang, "Privacy", "Privacy"), Some(Glyph::Lock), 0),
        item(5, s(lang, "About", "Informazioni"), Some(Glyph::Info), 0),
    ]);
    nav.set_current(Some(2));

    let tree = SidebarList::new(WidgetId(2), SidebarStyle { row_h: 28, inset: 8 });
    let mut places = item(10, s(lang, "Favourites", "Preferiti"), None, 0);
    places.section = true;
    let mut docs = item(11, s(lang, "Documents", "Documenti"), Some(Glyph::Folder), 0);
    docs.expandable = true;
    docs.expanded = true;
    let mut pics = item(14, s(lang, "Pictures", "Immagini"), Some(Glyph::Folder), 0);
    pics.expandable = true;
    pics.trailing = Some((Text::from("3"), FgRole::Tertiary));
    let mut dl = item(15, s(lang, "Downloads", "Scaricati"), Some(Glyph::Download), 0);
    dl.trailing = Some((Text::from("12"), FgRole::Attention));
    let mut sync = item(16, s(lang, "Cloud", "Cloud"), Some(Glyph::Cloud), 0);
    sync.busy = true;
    let mut gone = item(17, s(lang, "Archive", "Archivio"), Some(Glyph::Archive), 0);
    gone.disabled = true;
    let mut drive = item(18, s(lang, "Local disk", "Disco locale"), Some(Glyph::Disk), 0);
    drive.trailing = Some((Text::from("212 GB"), FgRole::Tertiary));
    let mut tree = tree.with_items(vec![
        places,
        docs,
        item(12, s(lang, "Reports", "Relazioni"), Some(Glyph::Folder), 1),
        item(13, s(lang, "Notes", "Appunti"), Some(Glyph::Document), 1),
        pics,
        dl,
        sync,
        gone,
        drive,
    ]);
    tree.set_current(Some(12));
    tree.set_drop_target(Some(15));
    let screen =
        Sidebars { lists: vec![(nav, (0, 8, 220, 200)), (tree, (240, 8, 240, 280))], commands: CommandTable::new() };
    Some(Page { name: "sidebar", size: SIZE, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::strings::Lang;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_names_its_nodes_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            let dump = h.a11y_dump();
            assert!(dump.contains("Tree") && dump.contains("TreeItem"), "{dump}");
            assert!(dump.contains("selected") && dump.contains("expanded") && dump.contains("busy"), "{dump}");
        }
    }

    #[test]
    fn the_page_draws_both_lists() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let img = h.render();
        for (x0, x1) in [(0, 230), (240, 480)] {
            let band = img.crop(RectP::wh(x0, 8, x1 - x0, 200));
            assert!(band.px.iter().any(|px| px & 0xFF != 0), "nothing drawn between {x0} and {x1}");
        }
    }
}
