//! The keyboard sheet (F1): a wide kit Dialog listing the key map, one line per keys.row_<n>. Filled by TA-T5b (T5.4).
#![allow(dead_code, unused_variables)] // stub: removed by TA-T5b

use oled_ui::{CommandId, EventCx};

use super::screen::TasksScreen;

/// KEYS. T5.4. Stub: nothing.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {}
