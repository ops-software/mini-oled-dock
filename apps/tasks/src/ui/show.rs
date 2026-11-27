//! Applying a Show (T6.2): the page, the sort and the selection from the command line or a forwarded WM_COPYDATA, and
//! bringing the window to the front. A foreign frame can only select, sort or page. Filled by TA-T6a.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T6a

use oled_trust::frame::tasks::Show;
use oled_ui::widgets::VirtualTable;
use oled_ui::EventCx;

use super::rows::TasksTable;
use super::view::View;

/// A Show waiting for its row, and the front request.
#[derive(Default)]
pub struct ShowState;

/// The start's Show, once the first frame is painted. T6.2. Stub: nothing.
pub fn on_first_paint(cx: &mut EventCx, st: &mut ShowState, show: &Show) {}

/// A WM_COPYDATA routed by the kit (Screen::copy_data, added by TK1): true when decoded and applied. T6.2. Stub: false.
pub fn on_copy_data(
    cx: &mut EventCx,
    st: &mut ShowState,
    table: &mut VirtualTable<TasksTable>,
    v: &View,
    tag: usize,
    bytes: &[u8],
) -> bool {
    false
}
