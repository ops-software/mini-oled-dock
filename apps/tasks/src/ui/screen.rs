//! The window's screen (F-T5): the layout, the paint, the hook table in its order and the pipeline. It only routes:
//! every callee is the stub or the body of its owner's file, so no group edits this file to plug itself in.

use std::cell::Cell;

use oled_ui::host::AppMsg;
use oled_ui::model::{ListModel, SelectionMode, TableStyle};
use oled_ui::widgets::VirtualTable;
use oled_ui::{
    CommandTable, Event, EventCx, Key, LayoutCx, PaintCx, RectP, Reply, Screen, SizeP, TimerId, Widget, WidgetId,
};

use super::chrome::{Chrome, ChromeOut, SEARCH};
use super::header::{GaugeHeader, HeaderOut};
use super::notice::Notices;
use super::order::PointerGate;
use super::rows::{RowEntry, TasksTable};
use super::view::View;
use super::{actions, commands, grouped, history, keys, list, more_menu, order, rows, search, selection, show, tree};
use crate::app::exec::ActionDone;
use crate::app::presence::{DisplayPower, PresenceMsg};
use crate::app::resolver::{self, NameBook, ResolverDone};
use crate::app::sampling::{self, ControlInputs, Sampling};
use crate::app::state::{self, StateDirty};
use crate::app::updated::{self, UpdState};
use crate::app::Deps;
use crate::engine::frame::Frame;
use crate::model::view::ViewMode;

/// One-shot timers of the window (no periodic timer exists).
pub const TIMER_SNAPSHOT_B: TimerId = TimerId(1);
pub const TIMER_STATE: TimerId = TimerId(2);
pub const TIMER_HOLD: TimerId = TimerId(3);
pub const TIMER_FRESH: TimerId = TimerId(4);
pub const TIMER_CLOSE_CHECK: TimerId = TimerId(5);
pub const TIMER_NOTES: TimerId = TimerId(6);

/// Heights of the bands, logical px: the toolbar, the error band (in the verdict slot), the gauge header and the
/// notice line.
pub const TOOLBAR_H: i32 = 44;
pub const VERDICT_H: i32 = 40;
pub const GAUGE_HEADER_H: i32 = 60;
pub const STATUS_H: i32 = 28;

/// The widgets' ids.
pub const CHROME: WidgetId = WidgetId(1);
pub const HEADER: WidgetId = WidgetId(2);
pub const TABLE: WidgetId = WidgetId(3);
pub const NOTICES: WidgetId = WidgetId(4);

/// AppMsg: the sampler has a new frame.
pub struct FrameReady;
/// AppMsg: a frame for scenes and Harness tests (in-process only).
pub struct InjectFrame(pub Frame);
/// AppMsg: a command as if a menu item had chosen it, for scenes and Harness tests (in-process only; the Harness
/// has no way to pick a menu item).
pub struct InjectCommand(pub oled_ui::CommandId);

/// The state of every part, each a Default struct in its owner's file.
#[derive(Default)]
pub struct Parts {
    pub names: NameBook,
    pub grouped: grouped::GroupedState,
    pub tree: tree::TreeState,
    pub search: search::Matcher,
    pub order: order::OrderGlue,
    pub actions: actions::ActionState,
    pub keys: keys::KeyState,
    pub show: show::ShowState,
    pub history: history::History,
    pub updated: UpdState,
}

pub struct TasksScreen {
    pub(crate) v: View,
    pub(crate) p: Parts,
    pub(crate) table: VirtualTable<TasksTable>,
    pub(crate) chrome: Chrome,
    pub(crate) header: GaugeHeader,
    pub(crate) notices: Notices,
    pub(crate) cmds: CommandTable,
    pub(crate) deps: Deps,
    pub(crate) ctl: ControlInputs,
    first_paint: Cell<Option<u32>>,
    first_paint_done: bool,
    rows: Vec<RowEntry>,
}

impl TasksScreen {
    pub fn new(deps: Deps) -> TasksScreen {
        let cmds = commands::table(deps.lang).unwrap_or_else(|e| {
            oled_sys::log!("tasks: the command table was refused: {e}");
            CommandTable::new()
        });
        let mut v = View::default();
        v.set_mode(grouped::initial_mode(&deps.state));
        let columns = rows::columns(deps.lang);
        let table = VirtualTable::new(TABLE, TasksTable::default(), columns, SelectionMode::Multi, TableStyle::TASKS);
        let ctl = ControlInputs { active: true, mode: v.mode(), ..ControlInputs::default() };
        TasksScreen {
            v,
            p: Parts::default(),
            table,
            chrome: Chrome::default(),
            header: GaugeHeader::default(),
            notices: Notices::default(),
            cmds,
            deps,
            ctl,
            first_paint: Cell::new(None),
            first_paint_done: false,
            rows: Vec::new(),
        }
    }

    /// What the screen runs on (tests read the stores, the sampler's control and the recorders through it).
    pub fn deps(&self) -> &Deps {
        &self.deps
    }

    /// The sampler's inputs as the screen last set them.
    pub fn control_inputs(&self) -> &ControlInputs {
        &self.ctl
    }

    /// True while the notice line shows.
    pub fn notices_shown(&self) -> bool {
        self.notices.line_shown()
    }

    pub fn view(&self) -> &View {
        &self.v
    }

    /// The state of every part (tests read the groups of the last build through it).
    pub fn parts(&self) -> &Parts {
        &self.p
    }

    pub fn chrome(&self) -> &Chrome {
        &self.chrome
    }

    pub fn header(&self) -> &GaugeHeader {
        &self.header
    }

    pub fn table(&self) -> &VirtualTable<TasksTable> {
        &self.table
    }

    // ---- the hook table of F-T5, in its order

    /// The toolbar, the error band while a snapshot fails, the gauge header, the table (the rest), the notice line
    /// while a notice shows; toasts sit above the window's bottom.
    fn layout_parts(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let w = avail.w;
        let mut y = 0;
        let toolbar = cx.px(TOOLBAR_H);
        self.chrome.layout(cx, RectP::wh(0, y, w, toolbar));
        y += toolbar;
        if self.v.read_error().is_some() {
            let band = cx.px(VERDICT_H);
            self.notices.error_band(cx, RectP::wh(0, y, w, band));
            y += band;
        }
        let gauge = cx.px(GAUGE_HEADER_H);
        self.header.layout(cx, RectP::wh(0, y, w, gauge));
        y += gauge;
        let line = if self.notices.line_shown() { cx.px(STATUS_H) } else { 0 };
        let rest = (avail.h - y - line).max(0);
        self.table.layout(cx, RectP::wh(0, y, w, rest));
        if line > 0 {
            self.notices.line(cx, RectP::wh(0, y + rest, w, line));
        }
        cx.set_toast_baseline(avail.h);
        // A modal dialog (About, the key sheet) covers the whole client and is laid out last.
        self.notices.layout_dialog(cx, RectP::wh(0, 0, w, avail.h));
        avail
    }

    fn paint_parts(&self, cx: &mut PaintCx) {
        self.chrome.paint(cx);
        if self.v.read_error().is_some() {
            self.notices.paint_error_band(cx);
        }
        self.header.paint(cx);
        order::on_paint_anim(&self.p.order, cx, self.table.model(), self.deps.options.get().density);
        self.table.paint(cx);
        if self.table.model().len() == 0 && search::query_stands(&self.p.search) {
            search::paint_empty(&self.p.search, cx, self.table.viewport());
        }
        if self.notices.line_shown() {
            self.notices.paint_line(cx);
        }
        order::paint_flash(&self.p.order, cx, &self.table);
        self.notices.paint_dialog(cx);
        // The end of the first paint: the first-frame time.
        if self.first_paint.get().is_none() {
            self.first_paint.set(Some(sampling::ms_since_start()));
        }
    }

    /// The start of every event and data call: the density the options name, then (once, after the first paint) the
    /// first-frame time.
    fn begin_call(&mut self, cx: &mut EventCx) {
        more_menu::apply_density(self, cx);
        if self.first_paint_done {
            return;
        }
        let Some(ms) = self.first_paint.get() else {
            return;
        };
        self.first_paint_done = true;
        sampling::on_first_paint(cx, &self.deps.sampling, ms, self.table.model().len());
        show::on_first_paint(cx, &mut self.p.show, &self.deps.show);
    }

    fn route_event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        let now = cx.now_ms();
        // An open dialog takes every input first and keeps it from the window behind it.
        if self.notices.dialog_open() && (is_input(ev) || matches!(ev, Event::A11y(..))) {
            if let Some(id) = self.notices.dialog_event(cx, ev) {
                commands::dispatch(self, cx, id);
            }
            cx.consume();
            return Reply::None;
        }
        if is_input(ev) {
            sampling::on_input(&self.deps.sampling, &mut self.ctl, now);
        }
        // Whether a menu was open when this event came (the command that opens one arrives as an event too).
        let menu_was_open = order::menu_open(&self.p.order);
        // Commands and timers of the app; keys and text the app takes before the widgets.
        let mut to_widgets = true;
        // Chords and the wheel the widgets would take for themselves (density; keys.rs).
        let own = keys::own_command(ev, table_has_focus(cx), self.table.viewport());
        match ev {
            _ if own.is_some() => {
                cx.consume();
                if let Some(id) = own {
                    commands::dispatch(self, cx, id);
                }
            }
            Event::Command(id) if !id.is_kit() => {
                to_widgets = false;
                commands::dispatch(self, cx, *id);
            }
            Event::Timer(id) if !id.is_kit() => {
                to_widgets = false;
                match *id {
                    TIMER_SNAPSHOT_B => sampling::on_timer(cx, &self.deps.sampling, *id),
                    TIMER_STATE => state::on_timer(&self.deps.state),
                    TIMER_HOLD | TIMER_FRESH => {
                        order::on_timer(cx, &mut self.p.order, *id);
                        self.run_pipeline(cx, now);
                    }
                    TIMER_CLOSE_CHECK | TIMER_NOTES => {
                        actions::on_timer(cx, &mut self.p.actions, *id);
                        self.run_pipeline(cx, now);
                    }
                    show::TIMER_SHOW_FRONT => self.p.show.on_front_timer(),
                    _ => {}
                }
            }
            Event::Key { key, down: true, .. } if *key == Key::ESC => {
                // The Esc cascade: the search, then the multi-selection; then the kit's (esc_closes).
                if search::on_esc(cx, &mut self.p.search, &mut self.chrome) {
                    order::note_query(&mut self.p.order);
                    self.run_pipeline(cx, now);
                    cx.consume();
                } else if selection::on_esc(cx, &mut self.table) {
                    cx.consume();
                }
            }
            Event::Key { key, down, mods, .. } => {
                order::note_mods(&mut self.p.order, *mods);
                let to_table = table_has_focus(cx);
                if keys::on_key(cx, &mut self.p.keys, ev, &mut self.table) {
                    cx.consume();
                } else if *down && to_table && is_navigation(*key) {
                    order::note_list_key(&mut self.p.order, now);
                } else if let Some(out) = keys::space_out(cx, ev, &self.table) {
                    // Space does what Enter does on a section or a group: expand or collapse.
                    if grouped::on_table_out(cx, &mut self.p.grouped, &out, &self.deps.state) {
                        self.run_pipeline(cx, now);
                    }
                    cx.consume();
                }
            }
            Event::Text(c) if table_has_focus(cx) => {
                let before = self.p.search.query().to_string();
                if search::on_list_text(cx, &mut self.p.search, &mut self.chrome, *c) {
                    // A character that started a search filters at once; "/" only moved the focus.
                    if self.p.search.query() != before {
                        order::note_query(&mut self.p.order);
                        self.run_pipeline(cx, now);
                        // The new rows may take the focus back to the table: the field keeps it.
                        self.chrome.focus_field(cx);
                    }
                    cx.consume();
                }
            }
            _ => {}
        }
        if to_widgets && !cx.consumed() {
            // The notice line: its timer, Esc and its action.
            if let Some(id) = self.notices.line_event(cx, ev) {
                commands::dispatch(self, cx, id);
            }
        }
        if to_widgets && !cx.consumed() {
            // A pointer inside the table's viewport passes the click guard first.
            let gate = match ev {
                _ if order::swallows(&mut self.p.order, ev) => PointerGate::Swallow,
                Event::PointerMove { pos, .. } | Event::PointerDown { pos, .. } | Event::PointerUp { pos, .. }
                    if self.table.viewport().contains(*pos) =>
                {
                    order::on_pointer(cx, &mut self.p.order, ev, &self.table, now)
                }
                _ => PointerGate::Pass,
            };
            if gate == PointerGate::Swallow {
                cx.consume();
            } else {
                match cx.target().map(|t| t.owner()) {
                    Some(CHROME | SEARCH) => match self.chrome.event(cx, ev) {
                        Some(ChromeOut::Command(id)) => commands::dispatch(self, cx, id),
                        Some(ChromeOut::Query(q)) => {
                            search::on_query(&mut self.p.search, &q);
                            order::note_query(&mut self.p.order);
                            self.run_pipeline(cx, now);
                        }
                        None => {}
                    },
                    Some(HEADER) => match self.header.event(cx, ev) {
                        Some(HeaderOut::Sort(col)) => {
                            self.v.set_sort(col);
                            self.run_pipeline(cx, now);
                        }
                        Some(HeaderOut::Menu(at)) => grouped::header_menu(cx, at, self.v.mode()),
                        None => {}
                    },
                    Some(NOTICES) => {
                        if self.notices.event(cx, ev).is_some() {
                            commands::dispatch(self, cx, commands::OPEN_WINDOWS_TM);
                        }
                    }
                    Some(TABLE) => {
                        if let Some(out) = self.table.event(cx, ev) {
                            order::on_table_out(&mut self.p.order, &out, now);
                            if grouped::on_table_out(cx, &mut self.p.grouped, &out, &self.deps.state) {
                                self.run_pipeline(cx, now);
                            }
                            actions::on_table_out(cx, &mut self.p.actions, &out, &self.table, &self.v, &self.deps);
                            selection::on_table_out(cx, &out, &mut self.table);
                        }
                    }
                    _ => {}
                }
            }
        }
        match ev {
            Event::WindowActive(on) => {
                self.ctl.active = *on;
                if !*on {
                    order::on_deactivate(&mut self.p.order);
                }
            }
            Event::PointerUp { .. } => order::note_button_up(&mut self.p.order),
            // Every menu ends with a pick or a close.
            Event::Command(_) | Event::ListPicked { .. } | Event::MenuClosed(_) if menu_was_open => {
                order::on_menu_closed(&mut self.p.order)
            }
            Event::CloseRequested => return Reply::Close,
            _ => {}
        }
        Reply::None
    }

    fn route_data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        let now = cx.now_ms();
        let msg = match msg.downcast::<FrameReady>() {
            Ok(_) => {
                if self.v.pull(&self.deps.sampling) {
                    self.run_pipeline(cx, now);
                }
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<InjectFrame>() {
            Ok(f) => {
                self.v.push_frame(f.0);
                self.run_pipeline(cx, now);
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<InjectCommand>() {
            Ok(c) => {
                commands::dispatch(self, cx, c.0);
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<PresenceMsg>() {
            Ok(p) => {
                self.ctl.minimised = p.minimised;
                self.ctl.cloaked = p.cloaked;
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<DisplayPower>() {
            Ok(d) => {
                self.ctl.display_off = !d.0;
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<ResolverDone>() {
            Ok(m) => {
                if resolver::on_done(&mut self.p.names, *m) {
                    self.run_pipeline(cx, now);
                }
                return Reply::None;
            }
            Err(m) => m,
        };
        // The resolver starts once the window has its proxy (app::main, step 13); the screen keeps and stops it.
        let msg = match msg.downcast::<resolver::Started>() {
            Ok(s) => {
                if let Some(old) = self.deps.resolver.replace(s.0) {
                    old.stop();
                }
                return Reply::None;
            }
            Err(m) => m,
        };
        let msg = match msg.downcast::<ActionDone>() {
            Ok(m) => {
                actions::on_done(cx, &mut self.p.actions, *m);
                self.run_pipeline(cx, now);
                return Reply::None;
            }
            Err(m) => m,
        };
        if msg.downcast::<StateDirty>().is_ok() {
            state::on_dirty(cx, &self.deps.state);
        }
        Reply::None
    }

    /// The end of every event and data call: the sampler's control, the toolbar's status, the held look.
    fn end_call(&mut self, cx: &mut EventCx) {
        show::pump(self, cx);
        let now = cx.now_ms();
        self.ctl.paused = self.v.paused();
        self.ctl.mode = self.v.mode();
        self.ctl.machine_cpu_pct = self.v.machine_cpu_pct();
        self.ctl.windows_rows_visible = grouped::windows_rows_visible(&self.p.grouped, &self.v);
        let options = self.deps.options.get();
        if let Some(c) = sampling::sync(&self.deps.sampling, &self.ctl, &options, now) {
            self.chrome.set_status(cx, sampling::status(&c, self.deps.sampling.stats().slow));
        }
        // The table's inline action and context menu act here, with the whole screen; the order holds while a row is
        // in flight.
        actions::end_call(self, cx, now);
        order::sync(cx, &mut self.p.order, now);
        self.header.set_held(cx, order::is_held(&self.p.order, now));
    }

    /// Rows from the view, in this order: merge, build by mode, filter, order, decorate, publish; then the parts that
    /// show the view's numbers.
    pub(crate) fn run_pipeline(&mut self, cx: &mut EventCx, now: u64) {
        self.rows.clear();
        self.v.merge(&self.deps);
        // While a query stands, Grouped and Tree are built open, so the filter finds every process as a row.
        let searching = search::query_stands(&self.p.search);
        self.p.grouped.set_query_open(searching);
        self.p.tree.set_query_open(searching);
        search::prepare(&self.v, &self.p.names, &mut self.p.search);
        match self.v.mode() {
            ViewMode::List => list::build(&self.v, &self.p.names, &mut self.rows),
            ViewMode::Grouped => grouped::build(&self.v, &self.p.names, &mut self.p.grouped, &mut self.rows),
            ViewMode::Tree => tree::build(&self.v, &self.p.names, &mut self.p.tree, &mut self.rows),
        }
        search::filter(&self.v, &mut self.p.search, &mut self.rows);
        self.chrome.set_count(cx, search::count(&self.p.search));
        order::apply(&self.v, &mut self.p.order, &mut self.rows, now);
        self.v.set_ended(order::ghost_names(&self.p.order));
        actions::note_groups(&mut self.p.actions, self.p.grouped.grouping());
        actions::decorate(&self.v, &mut self.p.actions, &mut self.rows, now);
        self.v.publish(cx, &mut self.table, &mut self.rows);
        selection::keep_one_section(cx, &mut self.table);
        self.header.set_gauges(cx, self.v.gauges(), self.v.sort(), self.deps.nf);
        self.notices.set_error(cx, self.v.read_error());
    }
}

impl Screen for TasksScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.layout_parts(cx, avail)
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.paint_parts(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.begin_call(cx);
        let reply = self.route_event(cx, ev);
        self.end_call(cx);
        reply
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        self.begin_call(cx);
        let reply = self.route_data(cx, msg);
        self.end_call(cx);
        reply
    }

    fn copy_data(&mut self, cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
        self.begin_call(cx);
        let taken = show::on_copy_data(cx, &mut self.p.show, &mut self.table, &self.v, tag, bytes);
        self.end_call(cx);
        taken
    }

    fn app_message(&mut self, cx: &mut EventCx, msg: u32, wparam: usize, lparam: isize) -> Reply {
        let _ = lparam;
        self.begin_call(cx);
        let reply = updated::on_message(cx, &mut self.p.updated, &mut self.notices, msg, wparam);
        self.end_call(cx);
        reply
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

impl Drop for TasksScreen {
    /// Stops the sampler and the resolver and writes the state.
    fn drop(&mut self) {
        std::mem::replace(&mut self.deps.sampling, Sampling::inert()).stop();
        if let Some(r) = self.deps.resolver.take() {
            r.stop();
        }
        self.deps.state.flush();
    }
}

/// A pointer, key, text, input method or wheel input (the idle rule's last input).
fn is_input(ev: &Event) -> bool {
    matches!(
        ev,
        Event::PointerMove { .. }
            | Event::PointerDown { .. }
            | Event::PointerUp { .. }
            | Event::Click { .. }
            | Event::DragStart { .. }
            | Event::DragMove { .. }
            | Event::DragEnd { .. }
            | Event::Wheel { .. }
            | Event::Key { .. }
            | Event::Text(_)
            | Event::Ime(_)
    )
}

/// Up, Down, PgUp, PgDn, Home, End.
fn is_navigation(k: Key) -> bool {
    [Key::UP, Key::DOWN, Key::PAGE_UP, Key::PAGE_DOWN, Key::HOME, Key::END].contains(&k)
}

fn table_has_focus(cx: &EventCx) -> bool {
    cx.focused().is_some_and(|f| f.owner() == TABLE)
}
