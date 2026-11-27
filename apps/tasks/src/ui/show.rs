//! Applying a Show (T6.2): the page, the sort and the selection from the command line or a forwarded WM_COPYDATA, and
//! bringing the window to the front. A foreign frame can only select, sort or page: nothing here builds a Batch.
//!
//! The hooks of F-T5 (on_first_paint, on_copy_data) only record what to do; the screen's end of every call runs
//! pump, which has the view, the groups and the table at hand, so a select whose row is not there yet waits for a
//! later frame.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oled_trust::frame::tasks::{copydata_tag, InstanceCmd, Select, Show, SortBy};
use oled_ui::model::{ListModel, RowKey, ScrollTo, Selection};
use oled_ui::widgets::VirtualTable;
use oled_ui::EventCx;
use oled_ui::FrontResult;
use oled_ui::TimerId;

use super::rows::{group_key, proc_key, TasksTable};
use super::screen::TasksScreen;
use super::view::View;
use crate::app::state;
use crate::model::classify::ImagePath;
use crate::model::group::AppKey;
use crate::model::view::{ColumnId, Section, ViewMode};
use oled_shellmodel::identity::key_from_exe_path;

/// The one-shot timer that makes a call right after the kit applied a front request, so its line is written then.
pub const TIMER_SHOW_FRONT: TimerId = TimerId(16);

/// How long a select waits for its row.
pub const PENDING_MS: u64 = 3_000;

/// Where the time of a front request counts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Since {
    /// The process creation (the start's own Show).
    Start,
    /// The receipt of a forwarded Show, in the window clock.
    Receipt(u64),
}

/// A Show waiting for its row, and the front request.
#[derive(Default)]
pub struct ShowState {
    sort: Option<ColumnId>,
    select: Option<Select>,
    /// The window clock at the first try of the select; it expires PENDING_MS later.
    since: Option<u64>,
    front: Option<Since>,
    /// The front timer fired: the kit has applied the request, the outcome is read at this call and no other.
    front_due: bool,
    /// Timer rounds spent waiting for the outcome.
    front_tries: u8,
}

impl ShowState {
    /// Records what a Show asks. A new select replaces an older one that still waits.
    fn accept(&mut self, show: &Show, with_sort: bool) {
        if with_sort {
            self.sort = match show.sort {
                SortBy::Cpu => Some(ColumnId::Cpu),
                SortBy::Memory => Some(ColumnId::Memory),
                SortBy::Auto => self.sort,
            };
        }
        if show.select != Select::None {
            self.select = Some(show.select.clone());
            self.since = None;
        }
    }

    fn want_front(&mut self, since: Since) {
        self.front = Some(since);
        self.front_due = false;
        self.front_tries = 0;
    }

    /// The timer of a front request fired (the screen's timer arm).
    pub fn on_front_timer(&mut self) {
        self.front_due = true;
    }

    /// A select or a sort still waits.
    pub fn pending(&self) -> bool {
        self.select.is_some() || self.sort.is_some()
    }
}

/// The start's Show, once the first frame is painted. T6.2, T6.3: a chord start comes to the front.
pub fn on_first_paint(cx: &mut EventCx, st: &mut ShowState, show: &Show) {
    #[cfg(feature = "fault-inject")]
    crate::app::fault::after_first_paint();
    st.accept(show, false);
    if show.from_chord && bring_to_front(cx) {
        cx.set_timer(TIMER_SHOW_FRONT, 1);
        st.want_front(Since::Start);
    }
}

/// Asks the kit for the front; a fault-inject build whose script says no-foreground does not (T6b.1).
fn bring_to_front(cx: &mut EventCx) -> bool {
    #[cfg(feature = "fault-inject")]
    if !crate::app::fault::front_allowed() {
        return false;
    }
    cx.bring_to_front();
    true
}

/// A WM_COPYDATA routed by the kit (Screen::copy_data, added by TK1): true when decoded and applied. A bad tag, a
/// frame that does not decode whole or one that is not a Show is refused and changes nothing.
pub fn on_copy_data(
    cx: &mut EventCx,
    st: &mut ShowState,
    table: &mut VirtualTable<TasksTable>,
    v: &View,
    tag: usize,
    bytes: &[u8],
) -> bool {
    let _ = (table, v);
    if tag != copydata_tag() {
        oled_sys::log!("tasks: a frame with another tag was refused");
        return false;
    }
    match oled_trust::frame::decode::<InstanceCmd>(bytes) {
        Ok((InstanceCmd::Show(show), used)) if used == bytes.len() => {
            st.accept(&show, true);
            oled_sys::log!("tasks: show applied");
            if bring_to_front(cx) {
                cx.set_timer(TIMER_SHOW_FRONT, 1);
                st.want_front(Since::Receipt(cx.now_ms()));
            }
            true
        }
        _ => {
            oled_sys::log!("tasks: a frame of {} bytes was refused", bytes.len());
            false
        }
    }
}

/// The end of every call of the screen: the sort, the select that can be applied now, and the front's log line.
pub fn pump(s: &mut TasksScreen, cx: &mut EventCx) {
    log_front(cx, &mut s.p.show);
    if let Some(col) = s.p.show.sort.take() {
        s.v.pin_sort(col, true);
        let now = cx.now_ms();
        s.run_pipeline(cx, now);
    }
    let Some(sel) = s.p.show.select.clone() else { return };
    let now = cx.now_ms();
    let since = *s.p.show.since.get_or_insert(now);
    if apply_select(s, cx, &sel) {
        s.p.show.select = None;
    } else if now.saturating_sub(since) >= PENDING_MS {
        oled_sys::log!("tasks: the selected row did not appear");
        s.p.show.select = None;
    }
}

/// "tasks: front by foreground|topmost|none, <ms> ms", once per request, at the call the request's timer makes (the
/// kit applies the request when the request's call has returned; the calls in between, such as an activation, may
/// still read the previous outcome and are ignored).
fn log_front(cx: &mut EventCx, st: &mut ShowState) {
    let Some(since) = st.front else { return };
    if !std::mem::take(&mut st.front_due) {
        return;
    }
    let Some(result) = cx.front_result() else {
        // Not applied yet: look again shortly, a few times.
        st.front_tries += 1;
        if st.front_tries < 5 {
            cx.set_timer(TIMER_SHOW_FRONT, 10);
        } else {
            st.front = None;
        }
        return;
    };
    st.front = None;
    let ms = match since {
        Since::Start => u64::from(crate::app::sampling::ms_since_start()),
        Since::Receipt(at) => cx.now_ms().saturating_sub(at),
    };
    oled_sys::log!("{}", front_line(result, ms));
}

/// The log line of a front request's outcome.
fn front_line(result: FrontResult, ms: u64) -> String {
    let how = match result {
        FrontResult::Foreground => "foreground",
        FrontResult::Topmost => "topmost",
        FrontResult::None => "none",
    };
    format!("tasks: front by {how}, {ms} ms")
}

/// The row of a select and the section that must be open to show it (Grouped). None: nothing matches yet.
fn target(s: &TasksScreen, sel: &Select) -> Option<(RowKey, Option<Section>)> {
    let grouped = s.v.mode() == ViewMode::Grouped;
    let g = s.p.grouped.grouping();
    let group_row = |gi: usize| {
        let grp = g.groups.get(gi)?;
        let section = grouped.then_some(grp.section);
        match grp.key {
            AppKey::Single(pk) if grp.members.len() == 1 => Some((proc_key(pk), section)),
            ak if grouped => Some((group_key(ak), section)),
            _ => Some((proc_key(*grp.members.first()?), None)),
        }
    };
    match sel {
        Select::None => None,
        Select::Pid(pid) => {
            let procs = s.v.procs();
            let (i, row) = procs.rows.iter().enumerate().find(|(_, r)| r.key.pid == *pid)?;
            if !grouped {
                return Some((proc_key(row.key), None));
            }
            let gi = *g.member_of.get(i)? as usize;
            // A member of a group with others is shown under its group row.
            group_row(gi)
        }
        Select::Identity(id) if !grouped => {
            // The grouping is only built in Grouped mode: resolve from the frame as grouped::build does.
            let frame = s.v.frame();
            let by_window = frame.windows.iter().find_map(|w| {
                let key = match s.p.names.identity(w.hwnd, w.owner.pid) {
                    Some(i) => i.key.clone(),
                    None => match frame.paths.get(w.owner) {
                        Some(ImagePath::Path(p)) => key_from_exe_path(p),
                        _ => return None,
                    },
                };
                key.eq_ignore_ascii_case(id).then_some(w.owner)
            });
            let owner = by_window.or_else(|| {
                frame.paths.entries.iter().find_map(|(k, p)| match p {
                    ImagePath::Path(p) if key_from_exe_path(p).eq_ignore_ascii_case(id) => {
                        s.v.procs().index_of(*k).map(|_| *k)
                    }
                    _ => None,
                })
            })?;
            Some((proc_key(owner), None))
        }
        Select::Identity(id) => {
            let gi = g.groups.iter().position(|grp| match grp.key {
                AppKey::App(x) => s.v.interned(x.0).eq_ignore_ascii_case(id),
                AppKey::Exe(x) => {
                    let path = s.v.interned(x.0);
                    key_from_exe_path(&path).eq_ignore_ascii_case(id)
                }
                AppKey::Image(_) | AppKey::Single(_) => false,
            })?;
            group_row(gi)
        }
    }
}

/// Selects and focuses the row of `sel` when it exists, opening its section first. True when done.
fn apply_select(s: &mut TasksScreen, cx: &mut EventCx, sel: &Select) -> bool {
    let Some((key, section)) = target(s, sel) else { return false };
    if let Some(sec) = section {
        let closed = s.deps.state.collapsed(sec).unwrap_or(sec == Section::Windows);
        if closed {
            s.deps.state.set_collapsed(sec, false);
            state::note_change(cx, &s.deps.state);
            let now = cx.now_ms();
            s.run_pipeline(cx, now);
        }
    }
    if s.table.model().row_of(key).is_none() {
        return false;
    }
    s.table.set_selection(cx, Selection::Single(key));
    s.table.set_focused(cx, key, ScrollTo::Center);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::exec::RecordingExecutor;
    use crate::app::launch::{Launcher, RecordingLauncher};
    use crate::app::Deps;
    use oled_ui::harness::Harness;
    use std::sync::Arc;

    #[test]
    fn front_log_line_once() {
        assert_eq!(front_line(FrontResult::Foreground, 12), "tasks: front by foreground, 12 ms");
        assert_eq!(front_line(FrontResult::Topmost, 0), "tasks: front by topmost, 0 ms");
        assert_eq!(front_line(FrontResult::None, 7), "tasks: front by none, 7 ms");

        let l: Arc<dyn Launcher> = Arc::new(RecordingLauncher::default());
        let mut deps = Deps::inert(oled_ui::UiLang::EN, l, Arc::new(RecordingExecutor::default()));
        deps.show = Show { from_chord: true, ..Show::default() };
        let mut h =
            Harness::new(TasksScreen::new(deps), oled_ui::SizeL { w: 640, h: 560 }, 96, oled_ui::Tokens::oled());
        h.render();
        h.advance(0);
        let ping = || Box::new(crate::app::presence::PresenceMsg { minimised: false, cloaked: false });
        h.send(ping());
        h.send(ping());
        assert!(h.screen().p.show.front.is_some(), "asked; a data call does not log it");
        h.advance(10);
        assert!(h.screen().p.show.front.is_none(), "logged at the timer call");
        h.send(ping());
        assert!(h.screen().p.show.front.is_none(), "once");
    }
}
