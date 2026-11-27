//! The gallery's pages (P2-kit K0.2): the registry, complete in K0.2; one file per page, filled by its owner (adding a
//! page edits only its own file). A page that returns None is not ready and is skipped.

pub mod buttons;
pub mod caption;
pub mod charts;
pub mod dialogs;
pub mod fields;
pub mod glyphs;
pub mod index;
pub mod menus;
pub mod mock_files;
pub mod mock_tasks;
pub mod path;
pub mod progress;
pub mod properties;
pub mod scroll;
pub mod settings_like;
pub mod sidebar;
pub mod split;
pub mod status;
pub mod table;
pub mod table100k;
pub mod tabs;
pub mod toggles;
pub mod toolbar;
pub mod tooltip;

use std::borrow::Cow;

use oled_ui::geom::SizeL;
use oled_ui::node::Text;
use oled_ui::screen::Screen;
use oled_ui::strings::{pseudo, UiLang};

/// One page: its name, logical size, how many states it shows, and its screen.
pub struct Page {
    pub name: &'static str,
    pub size: SizeL,
    pub states: u32,
    pub screen: Box<dyn Screen>,
}

/// The --focus-row message: a page that supports it accepts it in Screen::data and focuses that row (table100k,
/// K2a.6).
pub struct FocusRow(pub usize);

/// A page's constructor: None while the page is not ready.
pub type PageFn = fn(UiLang) -> Option<Page>;

/// Every page, in the order of the index.
pub const REGISTRY: [(&str, PageFn); 24] = [
    ("buttons", buttons::page),
    ("glyphs", glyphs::page),
    ("settings_like", settings_like::page),
    ("index", index::page),
    ("toggles", toggles::page),
    ("sidebar", sidebar::page),
    ("scroll", scroll::page),
    ("tooltip", tooltip::page),
    ("table", table::page),
    ("table100k", table100k::page),
    ("mock_tasks", mock_tasks::page),
    ("menus", menus::page),
    ("fields", fields::page),
    ("toolbar", toolbar::page),
    ("dialogs", dialogs::page),
    ("status", status::page),
    ("tabs", tabs::page),
    ("split", split::page),
    ("path", path::page),
    ("progress", progress::page),
    ("mock_files", mock_files::page),
    ("caption", caption::page),
    ("charts", charts::page),
    ("properties", properties::page),
];

/// The ready pages in `lang`.
pub fn all(lang: UiLang) -> Vec<Page> {
    REGISTRY.iter().filter_map(|(_, page)| page(lang)).collect()
}

/// The page of that name, if it is ready.
pub fn by_name(name: &str, lang: UiLang) -> Option<Page> {
    REGISTRY.iter().find(|(n, _)| *n == name).and_then(|(_, page)| page(lang))
}

/// A gallery sample text in `lang` (the gallery is never shipped: its samples are not UI strings); pseudo applies.
pub fn s(lang: UiLang, en: &'static str, it: &'static str) -> Text {
    let t = if lang.italian { it } else { en };
    if lang.pseudo {
        Cow::Owned(pseudo(t))
    } else {
        Cow::Borrowed(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_names_are_unique() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|(n, _)| *n).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "a page name is registered twice");
        for lang in [UiLang::EN, UiLang::IT] {
            for p in all(lang) {
                assert!(REGISTRY.iter().any(|(n, _)| *n == p.name), "{} is not its registry name", p.name);
            }
        }
        assert!(by_name("no-such-page", UiLang::EN).is_none());
    }

    #[test]
    fn samples_follow_the_language() {
        assert_eq!(s(UiLang::EN, "Name", "Nome"), "Name");
        assert_eq!(s(UiLang::IT, "Name", "Nome"), "Nome");
        assert!(s(UiLang { italian: false, pseudo: true }, "Name", "Nome").starts_with('['));
    }
}
