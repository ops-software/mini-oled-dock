//! Toolbar (F-K2b, P2-kit 2.9): the frozen types; filled by K2b.5.

use super::page_tabs::{PageTabs, PageTabsOut};
use super::text_field::{FieldOut, TextField};
use crate::command::CommandId;
use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::glyph::Glyph;
use crate::model::FgRole;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// A toolbar button: a command with its glyph, tip and toggle state.
pub struct ToolbarButton {
    pub command: CommandId,
    pub glyph: Glyph,
    pub tip: Text,
    pub toggled: Option<bool>,
}

/// What a toolbar tells its screen.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolbarOut {
    Command(CommandId),
    Tabs(PageTabsOut),
    Field(FieldOut),
}

/// The toolbar: page tabs, buttons, a field and a status text.
#[allow(dead_code)] // The fields are read by K2b.5.
pub struct Toolbar {
    id: WidgetId,
    tabs: Option<PageTabs>,
    buttons: Vec<ToolbarButton>,
    field: Option<TextField>,
    status: Option<(Text, FgRole)>,
}

impl Toolbar {
    pub fn new(id: WidgetId, tabs: Option<PageTabs>, buttons: Vec<ToolbarButton>, field: Option<TextField>) -> Toolbar {
        Toolbar { id, tabs, buttons, field, status: None }
    }

    pub fn tabs_mut(&mut self) -> Option<&mut PageTabs> {
        self.tabs.as_mut()
    }

    pub fn field_mut(&mut self) -> Option<&mut TextField> {
        self.field.as_mut()
    }

    /// Tasks' "Every 2 s".
    pub fn set_status(&mut self, s: Option<(Text, FgRole)>) {
        self.status = s;
    }
}

impl Widget for Toolbar {
    type Out = ToolbarOut;

    /// Filled by K2b.5; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K2b.5; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K2b.5; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ToolbarOut> {
        let _ = (cx, ev);
        None
    }
}
