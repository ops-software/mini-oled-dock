//! The linger (P3-files-app 2.2, F4.12; SPEC-files 5.6.5): when the last main window is asked to close and no
//! operation runs, the window hides instead. Its tabs collapse to one Home tab with no listing, the navigation cache and
//! the per-item icons go, the helpers are told to stop, the heap is compacted and one 10-minute one-shot timer is the
//! only one armed. A start in that time reuses the hidden window; the timer closes it for real, the event loop then
//! ends and the process exits. A process whose exe was updated while it ran never lingers on the old image.

use std::rc::Rc;

use oled_trust::frame::files::{InstanceCmd, Location};
use oled_ui::{Event, EventCx, Reply, Screen, TimerId};
use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};

use crate::listing::collate::utf16_to_wtf8;
use crate::listing::TabId;
use crate::nav::NavEffect;
use crate::tab::{self, Tabs};
use crate::window::{self, FilesScreen, LoopReq, Shed, Show};
use crate::{about, bar, chrome, handoff, live, names, ops_ui, pathline, rename, status};

/// How long the hidden window waits for a start before the process exits.
pub const LINGER_MS: u32 = 10 * 60 * 1000;

/// A window's linger (a field of FilesScreen: the owner of the frozen struct has no other place for it).
#[derive(Debug, Default)]
pub struct Linger {
    /// Hidden, waiting for a start or the timer.
    on: bool,
}

impl Linger {
    /// The window is hidden and lingering.
    pub fn on(&self) -> bool {
        self.on
    }
}

fn linger_timer() -> TimerId {
    window::timer(names::TIMER_LINGER, TabId(0))
}

/// This window is asked to close: when it is the last main window, nothing runs and the exe is current, it hides and
/// lingers (true: not closed). A close while lingering is a real one (false), as is any other case.
pub fn on_last_close(s: &mut FilesScreen, cx: &mut EventCx) -> bool {
    let shared = Rc::clone(&s.shared);
    let last = shared.main_windows().iter().all(|&w| w == s.win);
    if s.linger.on || shared.exe_updated.get() || !last || shared.ops.borrow().running() > 0 {
        return false;
    }
    collapse(s, cx);
    window::shed(s, Shed::Everything);
    // The helpers stop: nothing in this process may wake while it lingers (the next use starts them again).
    shared.services.shell.shutdown();
    shared.services.ops.shutdown();
    if !shared.session.borrow().save(&*shared.services.own) {
        oled_sys::log!("files: {} was not written", names::SESSION_FILE);
    }
    compact_heap();
    window::request_loop(&shared, LoopReq::Show { win: s.win, how: Show::Hide });
    cx.set_timer(linger_timer(), LINGER_MS);
    s.linger.on = true;
    oled_sys::log!("files: the last window is hidden; the process lingers for {} min", LINGER_MS / 60_000);
    true
}

/// Every tab stops (its enumeration, page timers and routes) and one Home tab with no listing takes their place; the
/// notice, the chip's hold and the prespawn go, so the linger's timer is the only one left.
fn collapse(s: &mut FilesScreen, cx: &mut EventCx) {
    live::unwatch(s);
    let now = s.shared.services.clock.now_ms();
    let old: Vec<TabId> = s.tabs.iter().map(|t| t.id).collect();
    for id in old {
        // A refresh hands out the token of a running enumeration: only its Cancel and Disarm effects run.
        if let Some(t) = s.tabs.by_id_mut(id) {
            let fx: Vec<NavEffect> = t
                .nav
                .refresh(now)
                .into_iter()
                .filter(|e| matches!(e, NavEffect::Cancel(_) | NavEffect::Disarm(_)))
                .collect();
            window::run_effects(s, Some(cx), id, fx);
        }
        for kind in names::TIMER_NAV_FIRST..=names::TIMER_NAV_LAST {
            cx.cancel_timer(window::timer(kind, id));
        }
        cx.cancel_timer(window::timer(names::TIMER_PRESPAWN, id));
        s.shared.hub.unroute(id);
    }
    for (_, (_, cancel, _)) in s.fresh.drain() {
        cancel.cancel();
    }
    s.pending.clear();
    s.hits.clear();
    s.deferred.clear();
    s.sort_chosen.clear();
    s.tabs = Tabs::new(tab::new_tab(&s.shared, s.win, Location::Home));
    chrome::sync(s, cx);
    pathline::set_location(s, cx, &Location::Home);
    status::on_notice_timer(s, cx);
    cx.cancel_timer(window::timer(names::TIMER_NOTICE, TabId(0)));
    s.shared.ops.borrow_mut().prune(u64::MAX);
    cx.cancel_timer(window::timer(names::TIMER_CHIP, TabId(0)));
    // What was open over the window goes with its tabs (nothing runs, so no bar or flyout holds a pending choice
    // that matters): a reuse starts clean.
    s.bar = bar::RecycleBar::new();
    s.flyout = ops_ui::Flyout::new();
    s.rename = rename::RenameState::new();
    s.about = about::About::default();
    s.held = handoff::Held::default();
}

/// HeapCompact of the process heap (the one the allocator uses): the freed listings' pages go back to the system.
fn compact_heap() {
    // SAFETY: the process heap's pseudo handle needs no closing; compacting only coalesces and releases free blocks.
    match unsafe { GetProcessHeap() } {
        Ok(heap) => {
            // SAFETY: the process heap's handle; no flags.
            let largest = unsafe { HeapCompact(heap, HEAP_FLAGS(0)) };
            oled_sys::log!("files: heap compacted (largest free block {largest} bytes)");
        }
        Err(e) => oled_sys::log!("files: no process heap ({e})"),
    }
}

/// Timer kind 7: ten minutes without a start. The window closes for real; with it the last window goes, the event
/// loop ends and the process exits.
pub fn on_linger_timer(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    if !s.linger.on {
        return Reply::None;
    }
    oled_sys::log!("files: no start in {} min: the process exits", LINGER_MS / 60_000);
    close_now(s, cx)
}

/// The lingering window closes for real through the window's own close path (a close while lingering is not caught).
pub fn close_now(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    s.event(cx, &Event::CloseRequested)
}

/// A start while lingering (instance::apply asks first): the timer goes and the hidden window is reused, its tab
/// navigated and the window brought to the front; no new window is made. False when not lingering, and for a command
/// that shows nothing (Probe): the linger then goes on.
pub fn on_instance(s: &mut FilesScreen, cx: &mut EventCx, cmd: &InstanceCmd) -> bool {
    if !s.linger.on {
        return false;
    }
    let (to, focus) = match cmd {
        InstanceCmd::NewWindow(at) | InstanceCmd::OpenTab(at) if matches!(at, Location::Fs(_) | Location::Home) => {
            (at.clone(), None)
        }
        InstanceCmd::Select { dir, item } => {
            let mut name = Vec::new();
            if let Some(units) = item.file_name() {
                utf16_to_wtf8(units, &mut name);
            }
            (Location::Fs(dir.clone()), (!name.is_empty()).then(|| name.into_boxed_slice()))
        }
        _ => return false,
    };
    cx.cancel_timer(linger_timer());
    s.linger.on = false;
    if to != Location::Home {
        window::navigate(s, cx, to, focus);
    }
    window::request_loop(&s.shared, LoopReq::Show { win: s.win, how: Show::Front });
    oled_sys::log!("files: a start reuses the lingering window");
    true
}
