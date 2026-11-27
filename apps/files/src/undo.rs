//! Undo and redo (P3-files-app 2.2; FC-OPS fills it, F3.3).

use oled_ui::node::Text;
use oled_ui::UiLang;

#[derive(Debug, Default)]
pub struct UndoStack;

impl UndoStack {
    pub fn new() -> UndoStack {
        UndoStack
    }

    /// "Undo <op>". Filled by FC-OPS; until then None.
    pub fn label(&self, lang: UiLang) -> Option<Text> {
        let _ = lang;
        None
    }

    /// Filled by FC-OPS; until then None.
    pub fn redo_label(&self, lang: UiLang) -> Option<Text> {
        let _ = lang;
        None
    }
}
