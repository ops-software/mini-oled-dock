//! The table100k page (K2a.6): a hundred thousand rows without storage; accepts pages::FocusRow.

use super::table::{columns, Demo, Tables};
use super::{s, Page};
use oled_ui::geom::SizeL;
use oled_ui::model::{SelectionMode, TableStyle};
use oled_ui::node::WidgetId;
use oled_ui::strings::UiLang;
use oled_ui::widgets::VirtualTable;

/// The row count of the page.
pub const ROWS: usize = 100_000;

pub fn page(lang: UiLang) -> Option<Page> {
    let mut demo = Demo::synthetic(ROWS);
    demo.ext_spans = true;
    let mut t = VirtualTable::new(WidgetId(1), demo, columns(lang), SelectionMode::Multi, TableStyle::FILES);
    t.set_label(s(lang, "A hundred thousand files", "Centomila file"));
    let screen = Tables::new(vec![((16, 16, 688, 408), t)]);
    Some(Page { name: "table100k", size: SizeL { w: 720, h: 440 }, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::FocusRow;
    use oled_ui::harness::Harness;
    use oled_ui::node::NodeId;
    use oled_ui::theme::Tokens;

    #[test]
    fn table100k_accepts_focus_row() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        assert!(h.a11y_dump().contains("1 of 100000"));
        h.key("Tab").key("Tab");
        h.send(Box::new(FocusRow(50_000)));
        h.render();
        let dump = h.a11y_dump();
        assert!(dump.contains("\"item 50000.txt\"") && dump.contains("50001 of 100000"), "row 50,000 is in view");
        assert!(!dump.contains("\"item 3.txt\""), "the top is no longer laid out");
        assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 50_001)));
        // Without the focus in the table the row is still brought into view.
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        h.send(Box::new(FocusRow(70_000)));
        assert!(h.a11y_dump().contains("\"item 70000.txt\""));
    }
}
