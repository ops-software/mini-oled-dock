//! The index page and its widget (K1a.6; K4.3): a SidebarList with one entry per page, the number of states the page shows
//! at the right, Up/Down/Home/End, type-ahead and Enter. Every ready page is reachable from the keyboard.

use std::sync::OnceLock;

use super::{Page, REGISTRY};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::model::FgRole;
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{SidebarItem, SidebarList, SidebarOut, SidebarStyle};

/// What the index tells the gallery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexOut {
    /// The page at this position of the names the index was made with.
    Open(usize),
}

/// Logical px: the list starts MARGIN down; an entry is ROW_H high and the plate sits INSET in from the sides.
const MARGIN: i32 = 16;
const ROW_H: i32 = 28;
const INSET: i32 = 8;

/// How many states each registry page shows, by name (the index page itself shows one). Computed once from the pages as
/// they are, English: the count does not depend on the language.
fn states_of(name: &str) -> Option<u32> {
    static COUNTS: OnceLock<Vec<(&'static str, u32)>> = OnceLock::new();
    if name == "index" {
        return Some(1);
    }
    let counts = COUNTS.get_or_init(|| {
        REGISTRY
            .iter()
            .filter(|(n, _)| *n != "index")
            .filter_map(|(n, f)| f(UiLang::EN).map(|p| (*n, p.states)))
            .collect()
    });
    counts.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// The list of pages: a SidebarList of the names; Up and Down move between them, Enter or a click opens one.
pub struct Index {
    list: SidebarList,
    count: usize,
}

impl Index {
    /// Entry i is the page named names[i]; it opens as IndexOut::Open(i). The first entry is the current one: the
    /// gallery opens it on start. The widget ids of the list are id and the ones after it.
    pub fn new(id: WidgetId, names: Vec<&'static str>) -> Index {
        let count = names.len();
        let items = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| SidebarItem {
                key: i as u64,
                label: Text::Borrowed(name),
                glyph: None,
                icon: None,
                depth: 0,
                expandable: false,
                expanded: false,
                section: false,
                trailing: states_of(name).map(|n| (Text::from(n.to_string()), FgRole::Tertiary)),
                disabled: false,
                busy: false,
            })
            .collect();
        let mut list = SidebarList::new(id, SidebarStyle { row_h: ROW_H, inset: INSET }).with_items(items);
        list.set_current(Some(0));
        Index { list, count }
    }

    /// Marks the entry whose page is open.
    pub fn set_current(&mut self, i: Option<usize>) {
        self.list.set_current(i.map(|i| i as u64));
    }
}

impl Widget for Index {
    type Out = IndexOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.list.layout(cx, RectP { t: rect.t + cx.px(MARGIN), ..rect });
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.list.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<IndexOut> {
        match self.list.event(cx, ev)? {
            SidebarOut::Activated(key) => {
                self.set_current(Some(key as usize));
                cx.relayout();
                Some(IndexOut::Open(key as usize))
            }
            _ => None,
        }
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: cx.px(240), h: cx.px(2 * MARGIN + ROW_H * self.count as i32) }
    }
}

/// The gallery's index screen: the list, and the page it was asked to open.
struct IndexScreen {
    index: Index,
    opened: Option<usize>,
    commands: CommandTable,
}

impl Screen for IndexScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.index.layout(cx, RectP::wh(0, 0, avail.w, avail.h));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.index.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Some(IndexOut::Open(i)) = self.index.event(cx, ev) {
            self.opened = Some(i);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// The names of the pages the index shows: every ready page, in the order of the registry (the index page included,
/// which is ready by being this function's caller).
fn listed(lang: UiLang) -> Vec<&'static str> {
    REGISTRY.iter().filter(|(n, f)| *n == "index" || f(lang).is_some()).map(|(n, _)| *n).collect()
}

fn size(entries: usize) -> SizeL {
    SizeL { w: 240, h: 2 * MARGIN + ROW_H * entries as i32 }
}

/// The index of every ready page.
pub fn page(lang: UiLang) -> Option<Page> {
    let names = listed(lang);
    let size = size(names.len());
    let screen = IndexScreen { index: Index::new(WidgetId(1), names), opened: None, commands: CommandTable::new() };
    Some(Page { name: "index", size, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages;
    use oled_ui::geom::PointL;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    fn harness_for(names: Vec<&'static str>) -> Harness<IndexScreen> {
        let size = size(names.len());
        let screen = IndexScreen { index: Index::new(WidgetId(1), names), opened: None, commands: CommandTable::new() };
        Harness::new(screen, size, 96, Tokens::oled())
    }

    fn harness() -> Harness<IndexScreen> {
        harness_for(listed(UiLang::EN))
    }

    /// Pages other groups own that are not merged into this branch yet: they may return None until then. The list only
    /// shrinks; once the whole suite is merged it is empty.
    const NOT_MERGED_YET: [&str; 11] = [
        "caption",
        "dialogs",
        "fields",
        "menus",
        "mock_files",
        "path",
        "progress",
        "split",
        "status",
        "tabs",
        "toolbar",
    ];

    #[test]
    fn index_lists_every_page() {
        for lang in [UiLang::EN, UiLang::IT] {
            let ready: Vec<&str> = pages::all(lang).iter().map(|p| p.name).collect();
            assert_eq!(listed(lang), ready, "the index lists the ready pages, in registry order");
            let h = harness_for(listed(lang));
            let dump = h.a11y_dump();
            assert_eq!(dump.matches("TreeItem").count(), ready.len(), "{dump}");
            for name in &ready {
                assert!(dump.contains(&format!("\"{name}\"")), "{name}");
            }
            h.assert_named();
        }
    }

    #[test]
    fn every_registered_page_returns_some() {
        let missing: Vec<&str> = REGISTRY.iter().filter(|(_, f)| f(UiLang::EN).is_none()).map(|(n, _)| *n).collect();
        let unexpected: Vec<&&str> = missing.iter().filter(|n| !NOT_MERGED_YET.contains(n)).collect();
        assert!(unexpected.is_empty(), "pages that are neither ready nor waiting for their group: {unexpected:?}");
        for (name, f) in REGISTRY {
            if let Some(p) = f(UiLang::EN) {
                assert_eq!(p.name, name, "a page answers to its registry name");
                assert!(p.states >= 1);
            }
        }
    }

    #[test]
    fn each_entry_shows_how_many_states_its_page_has() {
        let rendered = harness().render();
        assert!(!rendered.text_rects.is_empty());
        for (name, _) in REGISTRY {
            let Some(p) = pages::by_name(name, UiLang::EN) else { continue };
            assert_eq!(states_of(name), Some(p.states), "{name}");
        }
        assert_eq!(states_of("no-such-page"), None);
    }

    #[test]
    fn index_keyboard_reaches_every_page() {
        let names = listed(UiLang::EN);
        for i in 0..names.len() {
            let mut h = harness_for(names.clone());
            h.key("Tab");
            for _ in 0..i {
                h.key("Down");
            }
            h.key("Enter");
            assert_eq!(h.screen().opened, Some(i), "{} by arrows", names[i]);
        }
        // Type-ahead: the first letters of a name reach it.
        let mut h = harness_for(names.clone());
        h.key("Tab").type_text("tog").key("Enter");
        assert_eq!(h.screen().opened.map(|i| names[i]), Some("toggles"));
        // End and Home reach the last and the first.
        let mut h = harness_for(names.clone());
        h.key("Tab").key("End").key("Enter");
        assert_eq!(h.screen().opened, Some(names.len() - 1));
        h.key("Home").key("Enter");
        assert_eq!(h.screen().opened, Some(0));
    }

    #[test]
    fn a_click_opens_the_picked_page_and_marks_it() {
        let mut h = harness();
        h.click(PointL { x: 100, y: 16 + 28 * 5 + 10 });
        assert_eq!(h.screen().opened, Some(5));
        let dump = h.a11y_dump();
        let names = listed(UiLang::EN);
        let line = dump.lines().find(|l| l.contains(&format!("\"{}\"", names[5]))).unwrap_or_default();
        assert!(line.contains("selected"), "{line}");
    }

    #[test]
    fn the_arrows_stop_at_the_ends() {
        let mut h = harness();
        h.key("Tab").key("Up");
        assert_eq!(h.focused().map(|n| n.slot()), Some(1), "Up on the first stays");
        for _ in 0..60 {
            h.key("Down");
        }
        assert_eq!(h.focused().map(|n| n.slot() as usize), Some(listed(UiLang::EN).len()));
    }
}
