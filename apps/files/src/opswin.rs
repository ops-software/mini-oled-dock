//! The operations window (P3-files-app 2.2; FC-OPS fills it, F3.5): keeps running operations visible after the
//! last main window closes. It drains the engines when it is the primary window.

use std::rc::Rc;

use oled_ui::host::{AppMsg, Caption, WindowKind, WindowSpec};
use oled_ui::{CommandTable, Event, EventCx, LayoutCx, PaintCx, Reply, Screen, SizeL, SizeP};

use crate::msg::Msg;
use crate::names;
use crate::window::{self, FilesScreen, Shared};

/// Operations are running and this is the last main window: the operations window takes over. Filled by FC-OPS;
/// until then false.
pub fn on_last_close(s: &mut FilesScreen, cx: &mut EventCx) -> bool {
    let _ = (s, cx);
    false
}

pub struct OpsScreen {
    shared: Rc<Shared>,
    win: u32,
    commands: CommandTable,
}

impl OpsScreen {
    pub fn new(shared: Rc<Shared>, win: u32) -> OpsScreen {
        OpsScreen { shared, win, commands: CommandTable::new() }
    }

    pub fn win(&self) -> u32 {
        self.win
    }
}

impl Screen for OpsScreen {
    /// Filled by FC-OPS; until then a black page.
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let _ = cx;
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        let _ = cx;
        match ev {
            Event::CloseRequested => Reply::Close,
            _ => Reply::None,
        }
    }

    /// EnginesWake: drains the engines (this window may be the primary one).
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        let _ = cx;
        if let Ok(m) = msg.downcast::<Msg>() {
            if let Msg::EnginesWake = *m {
                window::drain_engines(&self.shared);
            }
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// Opaque { min 360 x 160, Caption::System }, OPS_WINDOW_CLASS, the app's name, 360 x 200. FC-OPS may change it.
pub fn ops_spec(shared: &Shared) -> WindowSpec {
    let lang = shared.lang.get();
    WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 360, h: 160 }, caption: Caption::System },
        class: names::OPS_WINDOW_CLASS,
        title: lang.kit(oled_ui::strings::k::KIT_APP_FILES),
        size: SizeL { w: 360, h: 200 },
        owner: None,
        esc_closes: false,
        placement: None,
        icon_resource: Some(1),
        opened_by_chord: false,
    }
}
