//! Search: the matcher (case- and diacritic-insensitive, ANDed terms, an exact all-digit PID), typing in the list,
//! Esc and the empty state. Filled by TA-T5a (T5.2, T5.3).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T5a

use oled_ui::{CommandId, EventCx, PaintCx, RectP};

use super::chrome::Chrome;
use super::rows::RowEntry;
use super::screen::TasksScreen;
use super::view::View;

/// The query and its compiled terms.
#[derive(Default)]
pub struct Matcher;

/// True while a query stands (the empty state may show). T5.2. Stub: false.
pub fn query_stands(m: &Matcher) -> bool {
    false
}

/// Keeps the rows that match, with their sections and groups. T5.2. Stub: keeps all.
#[allow(clippy::ptr_arg)] // the frozen signature: T5.2 removes the rows that do not match
pub fn filter(v: &View, m: &mut Matcher, rows: &mut Vec<RowEntry>) {}

/// The field's text changed. T5.2. Stub: nothing.
pub fn on_query(m: &mut Matcher, q: &str) {}

/// Esc clears the search first; true when it did. T5.3. Stub: false.
pub fn on_esc(cx: &mut EventCx, m: &mut Matcher, chrome: &mut Chrome) -> bool {
    false
}

/// A printable key or "/" in the list starts a search; true when taken. T5.3. Stub: false.
pub fn on_list_text(cx: &mut EventCx, m: &mut Matcher, chrome: &mut Chrome, c: char) -> bool {
    false
}

/// "Nothing matches" over the empty list. T5.3. Stub: nothing.
pub fn paint_empty(m: &Matcher, cx: &mut PaintCx, viewport: RectP) {}

/// FIND. T5.3. Stub: nothing.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {}
