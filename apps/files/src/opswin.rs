//! The operations window (P3-files-app 2.2, F3.5): when the last main window is asked to close while operations run,
//! a small window opens with the flyout's rows and the main window closes once it is there. It drains the engines
//! when it is the primary window, closes itself 6 s after the last operation finished, and closing it by hand leaves
//! the operations running in the helper (the next start reports them, F3.6).

use std::rc::Rc;

use oled_ui::host::{AppMsg, Caption, WindowKind, WindowSpec};
use oled_ui::node::{Node, NodeId, Role, WidgetId};
use oled_ui::{CommandTable, Event, EventCx, LayoutCx, PaintCx, RectP, Reply, Screen, SizeL, SizeP};

use crate::listing::TabId;
use crate::msg::{Fwd, Msg};
use crate::names;
use crate::ops_ui::{self, Rows, FINISHED_HOLD_MS};
use crate::tr::{k, t};
use crate::window::{self, FilesScreen, LoopReq, Shared};

const ID_PAGE: WidgetId = WidgetId(360);

/// Logical px.
const PAD: i32 = 12;
const MIN_H: i32 = 160;
const MAX_H: i32 = 480;
const WIDTH: i32 = 360;

/// The last main window is asked to close while operations run: the operations window is requested and this window
/// waits for it (true: not closed now; it closes itself on the Fwd::Ops the operations window sends, ops_ui). Once
/// the operations window is there, or nothing runs, or another main window stays: false.
pub fn on_last_close(s: &mut FilesScreen, cx: &mut EventCx) -> bool {
    let _ = cx;
    let shared = Rc::clone(&s.shared);
    let last = shared.main_windows().iter().all(|&w| w == s.win);
    if !last || shared.ops.borrow().running() == 0 || ops_ui::has_ops_window(&shared) {
        return false;
    }
    if shared.ops.borrow().close_after() == Some(s.win) {
        return true;
    }
    shared.ops.borrow_mut().set_close_after(Some(s.win));
    window::request_loop(&shared, LoopReq::Ops);
    true
}

pub struct OpsScreen {
    shared: Rc<Shared>,
    win: u32,
    commands: CommandTable,
    rows: Rows,
}

impl OpsScreen {
    /// Built by the loop callback before the window opens: the main window that waits hears Fwd::Ops (it closes once
    /// this window is recorded).
    pub fn new(shared: Rc<Shared>, win: u32) -> OpsScreen {
        let waiting = shared.ops.borrow().close_after();
        if let Some(w) = waiting {
            shared.ops.borrow_mut().ops_screen_made();
            shared.hub.post_to(w, Msg::Fwd(Fwd::Ops));
        }
        OpsScreen { shared, win, commands: CommandTable::new(), rows: Rows::new() }
    }

    pub fn win(&self) -> u32 {
        self.win
    }

    /// The rows as text lines (tests).
    pub fn row_texts(&self) -> Vec<String> {
        self.rows.texts()
    }

    fn timer() -> oled_ui::TimerId {
        window::timer(names::TIMER_CHIP, TabId(0))
    }

    /// The window goes: its hub entry and its record.
    fn closing(&mut self) -> Reply {
        self.shared.hub.detach(self.win);
        self.shared.windows.borrow_mut().retain(|w| w.win != self.win);
        Reply::Close
    }

    /// Nothing runs: the window closes 6 s after the last finish; something runs: no timer.
    fn arm(&self, cx: &mut EventCx) {
        let now = cx.now_ms();
        let m = self.shared.ops.borrow();
        if m.running() > 0 {
            cx.cancel_timer(Self::timer());
            return;
        }
        let last = m.rows().iter().filter_map(|r| r.finished_at).max().unwrap_or(now);
        let due = (last + FINISHED_HOLD_MS).saturating_sub(now).max(1);
        cx.set_timer(Self::timer(), u32::try_from(due).unwrap_or(u32::MAX));
    }
}

impl Screen for OpsScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let page = RectP::wh(0, 0, avail.w, avail.h);
        cx.push(Node::new(NodeId::new(ID_PAGE, 0), Role::Pane, page).label(t(k::FILES_CMD_OPS, cx.lang())));
        let fmt = self.shared.fmt.borrow().clone();
        let m = self.shared.ops.borrow();
        let pad = cx.px(PAD);
        self.rows.layout(cx, &m, None, &fmt, page.inset(pad, pad));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.rows.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        match ev {
            Event::CloseRequested => return self.closing(),
            Event::Timer(id) if *id == Self::timer() => {
                let m = self.shared.ops.borrow();
                if m.running() == 0 {
                    drop(m);
                    return self.closing();
                }
                return Reply::None;
            }
            _ => {}
        }
        match self.rows.event(cx, ev) {
            Some(out) => {
                ops_ui::rows_out(&self.shared, out);
                Reply::Relayout
            }
            None => Reply::None,
        }
    }

    /// EnginesWake: drains the engines (this window may be the primary one); Fwd::Ops: the rows and the close timer.
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        let Ok(m) = msg.downcast::<Msg>() else { return Reply::None };
        match *m {
            Msg::EnginesWake => {
                window::drain_engines(&self.shared);
                Reply::None
            }
            Msg::Fwd(Fwd::Ops) => {
                let now = cx.now_ms();
                self.shared.ops.borrow_mut().stamp(now);
                self.arm(cx);
                Reply::Relayout
            }
            _ => Reply::None,
        }
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

/// Opaque { min 360 x 160, Caption::System }, OPS_WINDOW_CLASS, the app's name, 360 wide and as tall as its rows (at
/// most 480).
pub fn ops_spec(shared: &Shared) -> WindowSpec {
    let lang = shared.lang.get();
    let h = (Rows::height_of(&shared.ops.borrow()) + PAD * 2).clamp(MIN_H, MAX_H);
    WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: WIDTH, h: MIN_H }, caption: Caption::System },
        class: names::OPS_WINDOW_CLASS,
        title: lang.kit(oled_ui::strings::k::KIT_APP_FILES),
        size: SizeL { w: WIDTH, h },
        owner: None,
        esc_closes: false,
        placement: None,
        icon_resource: Some(1),
        opened_by_chord: false,
    }
}
